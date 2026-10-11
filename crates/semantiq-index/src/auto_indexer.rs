use crate::IndexStore;
use crate::exclusions::{
    WalkFailures, is_file_too_large, project_walker, project_walker_with, should_exclude_path,
};
use crate::paths::to_relative_string;
use crate::store::DependencyInsert;
use crate::watcher::{FileEvent, FileWatcher};
use anyhow::{Result, bail};
use semantiq_parser::{
    ChunkExtractor, ImportContext, ImportExtractor, ImportKind, Language, LanguageSupport,
    ReferenceExtractor, StructureExtractor, SymbolExtractor, resolve_local_import,
};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;
use tracing::{debug, error, info, warn};

/// Phase 1 of indexing: parse files and store their structure (symbols,
/// references, call edges, type relations, imports) and chunks. Embeddings
/// are phase 2 (`crate::embedder`): chunks are stored without one, and the
/// file hash is stamped as soon as phase 1 is done with the file.
pub struct AutoIndexer {
    store: Arc<IndexStore>,
    watcher: Option<Mutex<FileWatcher>>,
    project_root: PathBuf,
    language_support: Mutex<LanguageSupport>,
    chunk_extractor: ChunkExtractor,
    /// Project facts for import classification (`go.mod` module path).
    import_context: ImportContext,
}

impl AutoIndexer {
    /// An indexer that also watches the project for changes (`process_events`).
    pub fn new(store: Arc<IndexStore>, project_root: PathBuf) -> Result<Self> {
        let mut watcher = FileWatcher::new()?;
        watcher.watch(&project_root)?;
        let indexer = Self::build(store, project_root, Some(watcher))?;
        info!("AutoIndexer initialized for {:?}", indexer.project_root);
        Ok(indexer)
    }

    /// An indexer for one-shot runs (`semantiq index`, query refresh): no
    /// file watcher, `process_events` is a no-op.
    pub fn without_watcher(store: Arc<IndexStore>, project_root: PathBuf) -> Result<Self> {
        Self::build(store, project_root, None)
    }

    fn build(
        store: Arc<IndexStore>,
        project_root: PathBuf,
        watcher: Option<FileWatcher>,
    ) -> Result<Self> {
        Ok(Self {
            store,
            watcher: watcher.map(Mutex::new),
            language_support: Mutex::new(LanguageSupport::new()?),
            chunk_extractor: ChunkExtractor::new(),
            import_context: ImportContext::from_project_root(&project_root),
            project_root,
        })
    }

    /// Stamp `project_root` in the index metadata (read back by the query
    /// commands when only `--database` is given). Called once a walk from
    /// that root proved consistent with the index, never from the
    /// constructor: a wrong root refused by `initial_index` must not be
    /// recorded. Indexing a different directory into a populated index is
    /// allowed (`semantiq index <other> --database <db>`) but worth a
    /// warning when `stale` rows of the previous root are about to be
    /// pruned (none after `--force` or a parser-version clear).
    fn record_project_root(&self, stale: usize) -> Result<()> {
        let canonical = self
            .project_root
            .canonicalize()
            .unwrap_or_else(|_| self.project_root.clone());
        if stale > 0
            && let Some(previous) = self.store.get_project_root()?
            && previous != canonical
        {
            warn!(
                "Index built from {} now indexed from {}: {} files of the previous root will be removed",
                previous.display(),
                canonical.display(),
                stale
            );
        }
        self.store.set_project_root(&canonical)
    }

    /// Perform initial indexing of all files in the project
    /// Only indexes files that are new or have changed since last index
    pub fn initial_index(&self) -> Result<InitialIndexResult> {
        self.initial_index_with(&mut |_| {})
    }

    /// `initial_index`, calling `on_file` after each indexed file (progress
    /// display).
    pub fn initial_index_with(
        &self,
        on_file: &mut dyn FnMut(&InitialIndexResult),
    ) -> Result<InitialIndexResult> {
        info!("Starting initial index of {:?}", self.project_root);

        let mut result = InitialIndexResult::default();
        let mut seen_paths = HashSet::new();
        // Where the walk failed (unreadable directory, I/O error): the files
        // under a failed directory are absent from `seen_paths` without having
        // vanished, and pruning them would throw their rows (and embeddings)
        // away for nothing.
        let mut failures = WalkFailures::default();

        // The project walk: `.gitignore` and excluded names (`exclusions.rs`).
        for entry in project_walker(&self.project_root).build() {
            let entry = match entry {
                Ok(entry) => entry,
                // A `.gitignore` the walker could not fully parse: the walk
                // went on with the rules it could read.
                Err(e) if e.is_partial() => {
                    warn!("Ignore rules partially applied: {}", e);
                    continue;
                }
                Err(e) => {
                    warn!("Walk error, files under it are kept in the index: {}", e);
                    failures.record(&e, &self.project_root);
                    result.errors += 1;
                    continue;
                }
            };

            // Skip directories
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(true) {
                continue;
            }

            let path = entry.path();
            result.scanned += 1;

            // Skip if not a supported language
            if Language::from_path(path).is_none() {
                continue;
            }

            // Get relative path (warns if `path` falls outside project_root).
            let rel_path = to_relative_string(path, &self.project_root);

            // Files above their size limit are never indexed. Drop a row left
            // by an older version or by a file that grew past the limit:
            // the hash check below would otherwise keep it forever.
            if is_file_too_large(path) {
                if self.store.get_file_by_path(&rel_path)?.is_some() {
                    self.store.delete_file(&rel_path)?;
                    debug!("Removed oversized file from index: {}", rel_path);
                    result.removed += 1;
                }
                continue;
            }

            // Read file content to check if needs reindex. A file that can no
            // longer be read (not UTF-8 any more, unreadable, gone between
            // the directory listing and here) cannot be verified: its old row
            // would be stale, so it goes.
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    debug!("Skipping {}: {}", rel_path, e);
                    if self.store.get_file_by_path(&rel_path)?.is_some() {
                        self.store.delete_file(&rel_path)?;
                        debug!("Removed unreadable file from index: {}", rel_path);
                        result.removed += 1;
                    }
                    continue;
                }
            };
            seen_paths.insert(rel_path.clone());

            // Check if file needs to be reindexed
            let needs_reindex = self
                .store
                .needs_reindex(&rel_path, &content)
                .unwrap_or_else(|e| {
                    debug!("Error checking reindex for {}: {}", rel_path, e);
                    // Try to index anyway
                    true
                });
            if !needs_reindex {
                // File already indexed and unchanged
                result.skipped += 1;
                continue;
            }
            // File is new or changed, index it (already checked: forced)
            if let Err(e) = self.index_file(path, true) {
                error!("Failed to index {}: {}", rel_path, e);
                result.errors += 1;
            } else {
                result.indexed += 1;
                on_file(&result);
            }
        }

        // Prune files that vanished (deleted, renamed, branch switch) while the
        // server was not running: the walk above only ever adds or updates rows.
        // Not under a directory the walk could not list: a file not seen there
        // is not necessarily gone, and the next walk that lists it prunes for
        // real. Elsewhere pruning goes on, so that a permanently unreadable
        // directory does not pile stale rows up forever.
        if !failures.is_empty() {
            warn!("Walk incomplete: files under the directories it could not list are kept");
        }
        if failures.covers_root() {
            // The walk failed at the root itself, or without saying where:
            // nothing can be told stale. The root is recorded only when the
            // walk did reach files of it.
            if !seen_paths.is_empty() {
                self.record_project_root(0)?;
            }
        } else {
            let stale: Vec<String> = self
                .store
                .list_file_paths()?
                .into_iter()
                .filter(|path| !seen_paths.contains(path) && !failures.covers(path))
                .collect();

            // Safety net: a walk that found nothing indexable while the index
            // holds files means the root is wrong (a database opened from
            // another directory), not that every file vanished. Pruning would
            // wipe the index; refuse instead.
            if seen_paths.is_empty() && !stale.is_empty() {
                bail!(
                    "no indexable file found under {} while the index holds {} files: wrong \
                     project root? Pass `--project <root>`, or rebuild with \
                     `semantiq index <root> --force` (also the way to go if the project really \
                     has no indexable file left)",
                    self.project_root.display(),
                    stale.len()
                );
            }
            self.record_project_root(stale.len())?;
            for stale in stale {
                match self.store.delete_file(&stale) {
                    Ok(()) => result.removed += 1,
                    Err(e) => {
                        error!("Failed to remove stale {}: {}", stale, e);
                        result.errors += 1;
                    }
                }
            }
        }

        info!(
            "Initial index complete: {} scanned, {} indexed, {} skipped, {} removed, {} errors",
            result.scanned, result.indexed, result.skipped, result.removed, result.errors
        );

        Ok(result)
    }

    /// Process pending file events and reindex changed files
    pub fn process_events(&self) -> Result<ProcessResult> {
        let Some(watcher) = &self.watcher else {
            return Ok(ProcessResult::default());
        };
        let events = {
            let watcher = watcher
                .lock()
                .map_err(|e| anyhow::anyhow!("FileWatcher lock poisoned: {}", e))?;
            watcher.poll_events()
        };

        if events.is_empty() {
            return Ok(ProcessResult::default());
        }

        // Events on files the index could contain wake the embedder
        // (`events`), whether or not they lead to a reindex here.
        let events_on_indexed_files = events.iter().filter(|e| concerns_indexed_file(e)).count();
        let mut result = self.process_file_events(events);
        result.events = events_on_indexed_files;
        Ok(result)
    }

    /// Apply file events as the watcher reports them: created and modified
    /// files are (re)indexed, deleted ones removed. A created or modified
    /// file is indexed only if the project walk would reach it (`.gitignore`,
    /// global excludes, `.git/info/exclude`, excluded names): the watcher sees
    /// every write, including those to generated or ignored files, and the
    /// index must stay the one `initial_index` builds.
    pub fn process_file_events(&self, events: Vec<FileEvent>) -> ProcessResult {
        let mut result = ProcessResult::default();
        let remove = |path: &Path, result: &mut ProcessResult| {
            if let Err(e) = self.remove_file(path) {
                error!("Failed to remove {:?}: {}", path, e);
                result.errors += 1;
            } else {
                result.removed += 1;
            }
        };

        let mut changed: Vec<PathBuf> = Vec::new();
        for event in events {
            match event {
                FileEvent::Created(path) | FileEvent::Modified(path) => {
                    // A rename is reported as Modify(Name) on the OLD path too
                    // (macOS FSEvents, editors' atomic saves): a path that no
                    // longer exists is a removal, not a modification.
                    if !path.exists() {
                        remove(&path, &mut result);
                        continue;
                    }
                    changed.push(path);
                }
                FileEvent::Deleted(path) => remove(&path, &mut result),
            }
        }

        if !changed.is_empty() {
            let (walked, walk_complete) = self.walked_subset(&changed);
            for path in changed {
                let rel_path = to_relative_string(&path, &self.project_root);
                if !walked.contains(&rel_path) {
                    debug!("Not part of the project walk, not indexed: {}", rel_path);
                    // A row from before the file became ignored (or from an
                    // older version) must go; not when the walk itself failed,
                    // since the file may simply have been missed.
                    if walk_complete {
                        match self.store.get_file_by_path(&rel_path) {
                            Ok(Some(_)) => remove(&path, &mut result),
                            Ok(None) => {}
                            Err(e) => {
                                error!("Failed to look up {}: {}", rel_path, e);
                                result.errors += 1;
                            }
                        }
                    }
                    continue;
                }
                match self.index_file(&path, false) {
                    Ok(true) => result.indexed += 1,
                    Ok(false) => {}
                    Err(e) => {
                        error!("Failed to index {:?}: {}", path, e);
                        result.errors += 1;
                    }
                }
            }
        }

        if result.indexed > 0 || result.removed > 0 {
            info!(
                "Auto-indexed: {} files updated, {} files removed, {} errors",
                result.indexed, result.removed, result.errors
            );
        }

        result
    }

    /// The project-relative paths among `candidates` that the project walk
    /// reaches, and whether the walk completed. The walk is restricted to the
    /// ancestors of the candidates and the candidates themselves, so it reads
    /// one directory per path component and the ignore files along the way:
    /// the same rules as `initial_index`, including nested `.gitignore`
    /// files, without a matcher to keep in sync with them.
    fn walked_subset(&self, candidates: &[PathBuf]) -> (HashSet<String>, bool) {
        let root = self.project_root.clone();
        let wanted: HashSet<String> = candidates
            .iter()
            .map(|path| to_relative_string(path, &root))
            .collect();
        let keep_root = root.clone();
        let walker = project_walker_with(&root, move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let rel = to_relative_string(entry.path(), &keep_root);
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                wanted
                    .iter()
                    .any(|wanted| Path::new(wanted).starts_with(&rel))
            } else {
                wanted.contains(&rel)
            }
        })
        .build();

        let mut walked = HashSet::new();
        let mut complete = true;
        for entry in walker {
            match entry {
                Ok(entry) => {
                    if entry.file_type().is_some_and(|ft| ft.is_file()) {
                        walked.insert(to_relative_string(entry.path(), &root));
                    }
                }
                Err(e) if e.is_partial() => warn!("Ignore rules partially applied: {}", e),
                Err(e) => {
                    warn!("Walk error while checking changed files: {}", e);
                    complete = false;
                }
            }
        }
        (walked, complete)
    }

    /// Index a single file (phase 1). Returns `Ok(false)` when the file was
    /// skipped (excluded, unsupported, unreadable or, unless `force`, content
    /// unchanged since last index).
    fn index_file(&self, path: &Path, force: bool) -> Result<bool> {
        // Get relative path (warns if `path` falls outside the project root).
        let rel_path = to_relative_string(path, &self.project_root);

        // Skip excluded paths (.git, node_modules, large files, etc.).
        //
        // The name exclusion (`.git`, `node_modules`, `.semantiq*`, ...) is
        // evaluated against the path RELATIVE to the project root, not the
        // absolute path. Otherwise a project that simply lives under an
        // excluded ancestor (e.g. a CI checkout under `.cache`, a `target`
        // directory) would have every one of its files silently skipped. The
        // file-size check still uses the real absolute `path` since it needs
        // the on-disk metadata.
        if should_exclude_path(Path::new(&rel_path)) || is_file_too_large(path) {
            debug!("Skipping excluded path: {}", rel_path);
            // A file that grew past its size limit must not keep its old rows.
            if self.store.get_file_by_path(&rel_path)?.is_some() {
                self.store.delete_file(&rel_path)?;
            }
            return Ok(false);
        }

        // Check if this is a supported language
        let language = match Language::from_path(path) {
            Some(lang) => lang,
            None => {
                debug!("Skipping unsupported file: {:?}", path);
                return Ok(false);
            }
        };

        // Read file content. A file that can no longer be read (not UTF-8 any
        // more, unreadable) cannot keep a row that no longer describes it.
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                debug!("Skipping {}: {}", rel_path, e);
                if self.store.get_file_by_path(&rel_path)?.is_some() {
                    self.store.delete_file(&rel_path)?;
                    debug!("Removed unreadable file from index: {}", rel_path);
                }
                return Ok(false);
            }
        };

        // Skip files whose content is unchanged since the last index: watchers
        // fire on touch, metadata changes and duplicate events, and re-running
        // the parser (and phase 2's embedding model) for those is pure waste.
        if !force
            && !self
                .store
                .needs_reindex(&rel_path, &content)
                .unwrap_or(true)
        {
            debug!("Unchanged, skipping: {}", rel_path);
            return Ok(false);
        }

        // Get file metadata
        let last_modified = fs::metadata(path)?
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Establish a stable `file_id` for the FK on symbols/chunks/dependencies
        // WITHOUT yet committing the up-to-date content hash.
        //
        // Crash-atomicity: the `files.hash` row is what `needs_reindex` consults.
        // If we stamped the current hash *before* writing children and the
        // process died mid-way, the file would look "up to date" while pointing
        // at partial/missing chunks — and would never be reindexed. So we:
        //   1. reuse the existing `file_id` when the file is already indexed
        //      (its stored hash stays at the OLD value), or
        //   2. for a brand-new file, seed the row with a deliberately
        //      non-matching sentinel hash so `needs_reindex` keeps returning
        //      true until the final stamp below.
        // Only after all children are persisted do we (re)write the row with the
        // real content hash, acting as the commit point.
        let file_id = match self.store.get_file_by_path(&rel_path)? {
            Some(existing) => existing.id,
            None => {
                // Sentinel content whose hash will not match `content` (unless
                // the file genuinely *is* this marker, which carries no chunks).
                const INDEXING_SENTINEL: &str = "\0__semantiq_indexing_in_progress__\0";
                self.store.insert_file(
                    &rel_path,
                    Some(language.name()),
                    INDEXING_SENTINEL,
                    last_modified,
                )?
            }
        };

        // Parse and extract symbols
        let mut language_support = self
            .language_support
            .lock()
            .map_err(|e| anyhow::anyhow!("LanguageSupport lock poisoned: {}", e))?;
        match language_support.parse_path(language, path, &content) {
            Ok(tree) => {
                // Extract symbols
                let symbols = SymbolExtractor::extract(&tree, &content, language)?;
                self.store.insert_symbols(file_id, &symbols)?;

                // Extract identifier occurrences for AST-based find_refs
                let references = ReferenceExtractor::extract(&tree, &content, language);
                self.store.insert_references(file_id, &references)?;

                // Call edges and type relations (semantiq_calls / semantiq_hierarchy)
                let structure =
                    StructureExtractor::extract(&tree, &content, language, &symbols, &references);
                self.store.insert_structure(file_id, &structure)?;

                // Extract chunks. Their embeddings are phase 2: a chunk whose
                // content did not change keeps its embedding, the others wait
                // for the embedder (`crate::embedder`).
                let chunks = self.chunk_extractor.extract(&tree, &content, language)?;
                self.store.insert_chunks(file_id, &chunks)?;

                // Extract imports and store them as the file's dependencies,
                // in one transaction (one commit per file, not per import).
                let imports = ImportExtractor::extract_with_context(
                    &tree,
                    &content,
                    language,
                    &self.import_context,
                )?;
                let resolved: Vec<Option<String>> = imports
                    .iter()
                    .map(|import| {
                        if import.kind == ImportKind::Local {
                            resolve_local_import(
                                &rel_path,
                                &import.path,
                                language,
                                &self.project_root,
                            )
                        } else {
                            None
                        }
                    })
                    .collect();
                let dependencies: Vec<DependencyInsert<'_>> = imports
                    .iter()
                    .zip(&resolved)
                    .map(|(import, resolved)| DependencyInsert {
                        target_path: &import.path,
                        import_name: import.name.as_deref(),
                        kind: import.kind.as_str(),
                        resolved_path: resolved.as_deref(),
                        lines: (import.start_line, import.end_line),
                    })
                    .collect();
                self.store.replace_dependencies(file_id, &dependencies)?;

                debug!(
                    "Auto-indexed {}: {} symbols, {} chunks, {} deps",
                    rel_path,
                    symbols.len(),
                    chunks.len(),
                    imports.len()
                );
            }
            Err(e) => {
                warn!("Failed to parse {}: {}", rel_path, e);
            }
        }

        // Release the parser lock before the final hash stamp so we never hold
        // two locks at once.
        drop(language_support);

        // Commit point: stamp the real content hash LAST, only after all
        // children (symbols/chunks/dependencies) are persisted. Embeddings are
        // not part of it: a chunk without one is phase 2's pending work. A
        // crash before this line leaves the OLD/sentinel hash in place, so
        // `needs_reindex` returns true next time and the file is reindexed
        // rather than silently treated as up-to-date with partial data.
        //
        // This runs on the parse-failure path too: stamping the current hash
        // there is intentional and matches the prior behaviour, preventing an
        // unparseable file from being re-parsed on every cycle.
        self.store
            .insert_file(&rel_path, Some(language.name()), &content, last_modified)?;

        Ok(true)
    }

    /// Remove a file from the index
    fn remove_file(&self, path: &Path) -> Result<()> {
        let rel_path = to_relative_string(path, &self.project_root);

        self.store.delete_file(&rel_path)?;
        debug!("Removed from index: {}", rel_path);

        Ok(())
    }
}

#[derive(Default, Debug)]
pub struct ProcessResult {
    /// File events on files of a supported language, whether or not they led
    /// to a reindex (a file another process already indexed, a CLI refresh,
    /// has `indexed == 0`). Saving a README or a lock file counts for nothing.
    pub events: usize,
    pub indexed: usize,
    pub removed: usize,
    pub errors: usize,
}

/// Whether an event is about a file the index could contain: the embedder is
/// woken for these only.
fn concerns_indexed_file(event: &FileEvent) -> bool {
    let path = match event {
        FileEvent::Created(path) | FileEvent::Modified(path) | FileEvent::Deleted(path) => path,
    };
    Language::from_path(path).is_some()
}

#[derive(Default, Debug)]
pub struct InitialIndexResult {
    pub scanned: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub removed: usize,
    pub errors: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_events_on_supported_languages_count() {
        let events = [
            FileEvent::Modified(PathBuf::from("/p/README.md")),
            FileEvent::Modified(PathBuf::from("/p/Cargo.lock")),
            FileEvent::Created(PathBuf::from("/p/.semantiq.db-wal")),
            FileEvent::Modified(PathBuf::from("/p/src/lib.rs")),
            FileEvent::Deleted(PathBuf::from("/p/src/old.ts")),
            FileEvent::Created(PathBuf::from("/p/config.toml")),
        ];
        let counted = events.iter().filter(|e| concerns_indexed_file(e)).count();
        assert_eq!(counted, 3, "lib.rs, old.ts and config.toml (data language)");
    }
}
