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
    ReferenceExtractor, StructureExtractor, SymbolExtractor, resolve_import,
};
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
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
    /// Project facts for import classification and resolution: the `go.mod`
    /// module path and the packages the project's files declare
    /// (`scan_import_context`).
    import_context: ImportContext,
    /// Relative paths whose parse is made to fail (`simulate_parse_failure`),
    /// tree-sitter never failing on its own in a test.
    #[cfg(any(test, feature = "test-support"))]
    parse_failures: Mutex<HashSet<String>>,
}

/// How much of a file `scan_import_context` reads: `package` / `namespace`
/// declarations come before any code.
const PACKAGE_DECLARATION_HEAD: usize = 8 * 1024;

/// The [`ImportContext`] of `project_root`: its `go.mod` module and, from one
/// pass over the project walk, the packages its Java / Kotlin / Scala / C# /
/// PHP files declare and the directories of its Go files (#147). Only the
/// head of those files is read; the other languages cost a directory walk.
fn scan_import_context(project_root: &Path) -> ImportContext {
    let mut context = ImportContext::from_project_root(project_root);
    for entry in project_walker(project_root).build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let Some(language) = Language::from_path(path) else {
            continue;
        };
        if !ImportContext::records_packages(language) {
            continue;
        }
        let rel_path = to_relative_string(path, project_root);
        if should_exclude_path(Path::new(&rel_path)) {
            continue;
        }
        let head = if language == Language::Go {
            String::new()
        } else {
            match read_head(path, PACKAGE_DECLARATION_HEAD) {
                Ok(head) => head,
                Err(e) => {
                    debug!("Package scan skips {}: {}", rel_path, e);
                    continue;
                }
            }
        };
        context.add_source_file(&rel_path, language, &head);
    }
    context
}

/// The first `limit` bytes of `path` as (lossy) text.
fn read_head(path: &Path, limit: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut buffer = Vec::with_capacity(limit.min(4096));
    fs::File::open(path)?
        .take(limit as u64)
        .read_to_end(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// Whether a read error means the file's row no longer describes anything on
/// disk: the file is gone, or is not UTF-8 text any more. Any other error
/// (permissions, interruption, I/O) is transient from the index's point of
/// view: dropping the row would throw its embeddings away and recompute them
/// for nothing once the file is readable again.
pub fn read_error_is_final(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::InvalidData | ErrorKind::NotFound)
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
            import_context: scan_import_context(&project_root),
            project_root,
            #[cfg(any(test, feature = "test-support"))]
            parse_failures: Mutex::new(HashSet::new()),
        })
    }

    /// Make the next parses of `rel_path` fail, as a tree-sitter timeout or
    /// cancellation would. Test support only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn simulate_parse_failure(&self, rel_path: &str) {
        self.parse_failures
            .lock()
            .expect("parse_failures lock poisoned")
            .insert(rel_path.to_string());
    }

    /// Whether `simulate_parse_failure` asked for the parse of `rel_path`
    /// to fail.
    #[cfg(any(test, feature = "test-support"))]
    fn parse_should_fail(&self, rel_path: &str) -> bool {
        self.parse_failures
            .lock()
            .expect("parse_failures lock poisoned")
            .contains(rel_path)
    }

    #[cfg(not(any(test, feature = "test-support")))]
    fn parse_should_fail(&self, _rel_path: &str) -> bool {
        false
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
        let canonical = self.canonical_root();
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

    fn canonical_root(&self) -> PathBuf {
        self.project_root
            .canonicalize()
            .unwrap_or_else(|_| self.project_root.clone())
    }

    /// Whether a walk from this root already proved consistent with the
    /// index (`record_project_root`): the index is this project's, not one
    /// opened from another directory.
    fn root_is_recorded(&self) -> bool {
        matches!(self.store.get_project_root(), Ok(Some(recorded)) if recorded == self.canonical_root())
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

            // Read file content to check if needs reindex. A file that is
            // gone (between the directory listing and here) or not UTF-8 any
            // more has a stale row, so it goes. On any other read error
            // (permissions, interruption) nothing shows the row stale: it is
            // kept, embeddings included, and must not be pruned either.
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) if read_error_is_final(&e) => {
                    debug!("Skipping {}: {}", rel_path, e);
                    if self.store.get_file_by_path(&rel_path)?.is_some() {
                        self.store.delete_file(&rel_path)?;
                        debug!("Removed unreadable file from index: {}", rel_path);
                        result.removed += 1;
                    }
                    continue;
                }
                Err(e) => {
                    warn!("Cannot read {}, its index entry is kept: {}", rel_path, e);
                    seen_paths.insert(rel_path.clone());
                    result.errors += 1;
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
        // A directory event (rename, move-in) or an ignore-file edit carries
        // no file name but may have indexed files whose chunks are pending.
        result.events = events_on_indexed_files.max(result.indexed);
        Ok(result)
    }

    /// Apply file events as the watcher reports them: created and modified
    /// files are (re)indexed, deleted ones removed. A created or modified
    /// file is indexed only if the project walk would reach it (`.gitignore`,
    /// global excludes, `.git/info/exclude`, excluded names): the watcher sees
    /// every write, including those to generated or ignored files, and the
    /// index must stay the one `initial_index` builds.
    ///
    /// A directory rename, move or removal is one event on the directory,
    /// not on its files (FSEvents and inotify alike), and a directory just
    /// created may already hold files: a vanished path loses every row under
    /// it, an existing directory has its subtree walked and pruned
    /// (`index_subtree`). An edited or removed ignore file (`.gitignore`,
    /// `.ignore`, `.git/info/exclude`) is a change to what the walk reaches
    /// below it: its directory is walked again, the whole project for the
    /// root one.
    pub fn process_file_events(&self, events: Vec<FileEvent>) -> ProcessResult {
        let mut result = ProcessResult::default();
        let remove = |path: &Path, result: &mut ProcessResult| match self.remove_file(path) {
            Ok(removed) => result.removed += removed,
            Err(e) => {
                error!("Failed to remove {:?}: {}", path, e);
                result.errors += 1;
            }
        };

        let mut changed: Vec<PathBuf> = Vec::new();
        // Directories whose subtree is walked again.
        let mut subtrees: Vec<PathBuf> = Vec::new();
        for event in events {
            match event {
                FileEvent::Created(path) | FileEvent::Modified(path) => {
                    // A rename is reported as Modify(Name) on the OLD path too
                    // (macOS FSEvents, editors' atomic saves): a path that no
                    // longer exists is a removal, not a modification.
                    if !path.exists() {
                        remove(&path, &mut result);
                        subtrees.extend(self.subtree_lifted_by(&path));
                        continue;
                    }
                    if path.is_dir() {
                        subtrees.push(path);
                        continue;
                    }
                    if let Some(dir) = self.subtree_of_ignore_file(&path) {
                        subtrees.push(dir);
                        continue;
                    }
                    changed.push(path);
                }
                FileEvent::Deleted(path) => {
                    remove(&path, &mut result);
                    subtrees.extend(self.subtree_lifted_by(&path));
                }
            }
        }

        // One walk per subtree, none for a directory under another one.
        subtrees.sort();
        subtrees.dedup();
        let mut walked_dirs: Vec<PathBuf> = Vec::new();
        for dir in subtrees {
            if !walked_dirs.iter().any(|walked| dir.starts_with(walked)) {
                walked_dirs.push(dir);
            }
        }
        if walked_dirs
            .iter()
            .any(|dir| to_relative_string(dir, &self.project_root).is_empty())
        {
            // The project root itself: one walk of the whole project, which
            // covers every other event as well. Through `index_subtree`, not
            // `initial_index`: a root rule that ignores every file (`*`) is
            // a legitimate edit that empties the index, which the wrong-root
            // safety net of `initial_index` would refuse to prune. That net
            // is kept while no walk from this root has proved consistent
            // with the index (the startup walk failed): the index may be
            // another project's.
            if self.root_is_recorded() {
                self.index_subtree(&self.project_root, &mut result);
            } else {
                match self.initial_index() {
                    Ok(walk) => {
                        result.indexed += walk.indexed;
                        result.removed += walk.removed;
                        result.errors += walk.errors;
                    }
                    Err(e) => {
                        error!("Failed to walk the project again: {}", e);
                        result.errors += 1;
                    }
                }
            }
            changed.clear();
        } else {
            for dir in &walked_dirs {
                self.index_subtree(dir, &mut result);
            }
            changed.retain(|path| !walked_dirs.iter().any(|dir| path.starts_with(dir)));
        }

        // The watcher reports several events per save (and per file on a
        // branch switch): one walk and one index pass per file.
        changed.sort();
        changed.dedup();

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

    /// The directory whose walk an edit of the ignore file `path` changes:
    /// the directory of a `.gitignore` or `.ignore`, the project root for
    /// `.git/info/exclude`. `None` for any other file.
    fn subtree_of_ignore_file(&self, path: &Path) -> Option<PathBuf> {
        let name = path.file_name()?.to_str()?;
        if name == ".gitignore" || name == ".ignore" {
            return Some(path.parent().unwrap_or(&self.project_root).to_path_buf());
        }
        if to_relative_string(path, &self.project_root) == ".git/info/exclude" {
            return Some(self.project_root.clone());
        }
        None
    }

    /// The directory to walk again once the ignore file `path` vanished:
    /// the rules it held are lifted, the files they hid are reachable. None
    /// when its directory vanished too (handled as a removal) or for any
    /// other file.
    fn subtree_lifted_by(&self, path: &Path) -> Option<PathBuf> {
        self.subtree_of_ignore_file(path).filter(|dir| dir.is_dir())
    }

    /// Walk the subtree under `dir` (a directory of the project, or the
    /// project root itself: `rel_dir` empty, every row a candidate) with
    /// the project walk, index the files it reaches that are new or changed, and
    /// prune the rows under `dir` of files it did not reach: gone, or ignored
    /// since. Not when the walk failed somewhere, since a file not reached
    /// may simply have been missed. The ignore files above `dir` are read
    /// on the way down, as in `initial_index`.
    fn index_subtree(&self, dir: &Path, result: &mut ProcessResult) {
        let root = self.project_root.clone();
        let rel_dir = to_relative_string(dir, &root);
        let keep_root = root.clone();
        let keep_dir = PathBuf::from(&rel_dir);
        let walker = project_walker_with(&root, move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let rel = PathBuf::from(to_relative_string(entry.path(), &keep_root));
            // The ancestors of `dir`, `dir` itself and everything below.
            keep_dir.starts_with(&rel) || rel.starts_with(&keep_dir)
        })
        .build();

        let mut seen: HashSet<String> = HashSet::new();
        let mut complete = true;
        for entry in walker {
            match entry {
                Ok(entry) => {
                    if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                        continue;
                    }
                    let path = entry.path();
                    let rel_path = to_relative_string(path, &root);
                    if !Path::new(&rel_path).starts_with(&rel_dir) {
                        continue;
                    }
                    seen.insert(rel_path);
                    match self.index_file(path, false) {
                        Ok(true) => result.indexed += 1,
                        Ok(false) => {}
                        Err(e) => {
                            error!("Failed to index {:?}: {}", path, e);
                            result.errors += 1;
                        }
                    }
                }
                Err(e) if e.is_partial() => warn!("Ignore rules partially applied: {}", e),
                Err(e) => {
                    warn!(
                        "Walk error under {}, its rows are kept: {}",
                        if rel_dir.is_empty() { "." } else { &rel_dir },
                        e
                    );
                    complete = false;
                }
            }
        }
        if !complete {
            return;
        }
        let stale = match self.store.list_file_paths_under(&rel_dir) {
            Ok(paths) => paths,
            Err(e) => {
                error!("Failed to list the files under {}: {}", rel_dir, e);
                result.errors += 1;
                return;
            }
        };
        for path in stale.into_iter().filter(|path| !seen.contains(path)) {
            match self.store.delete_file(&path) {
                Ok(()) => result.removed += 1,
                Err(e) => {
                    error!("Failed to remove stale {}: {}", path, e);
                    result.errors += 1;
                }
            }
        }
    }

    /// The project-relative paths among `candidates` that the project walk
    /// reaches, and whether the walk completed. The walk is restricted to the
    /// ancestors of the candidates and the candidates themselves, so it reads
    /// one directory per path component and the ignore files along the way:
    /// the same rules as `initial_index`, including nested `.gitignore`
    /// files, without a matcher to keep in sync with them.
    fn walked_subset(&self, candidates: &[PathBuf]) -> (HashSet<String>, bool) {
        let root = self.project_root.clone();
        // The candidates and every directory above them, so that the filter
        // is one hash lookup per entry: a branch switch hands over thousands
        // of paths in one batch.
        let mut wanted: HashSet<String> = HashSet::with_capacity(candidates.len());
        let mut ancestors: HashSet<String> = HashSet::new();
        for path in candidates {
            let rel = to_relative_string(path, &root);
            let mut dir = Path::new(&rel).parent();
            while let Some(d) = dir.filter(|d| !d.as_os_str().is_empty()) {
                // An ancestor already there brought its own ancestors along.
                if !ancestors.insert(d.to_string_lossy().into_owned()) {
                    break;
                }
                dir = d.parent();
            }
            wanted.insert(rel);
        }
        let keep_root = root.clone();
        let walker = project_walker_with(&root, move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let rel = to_relative_string(entry.path(), &keep_root);
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                ancestors.contains(&rel)
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

        // Read file content. A file that is gone or not UTF-8 any more cannot
        // keep a row that no longer describes it; on any other read error
        // (permissions, interruption) the row stays, see `initial_index_with`.
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if read_error_is_final(&e) => {
                debug!("Skipping {}: {}", rel_path, e);
                if self.store.get_file_by_path(&rel_path)?.is_some() {
                    self.store.delete_file(&rel_path)?;
                    debug!("Removed unreadable file from index: {}", rel_path);
                }
                return Ok(false);
            }
            Err(e) => return Err(anyhow::Error::from(e).context(format!("reading {}", rel_path))),
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
        let parsed = if self.parse_should_fail(&rel_path) {
            Err(anyhow::anyhow!("simulated parse failure"))
        } else {
            language_support.parse_path(language, path, &content)
        };
        let mut parse_error = None;
        match parsed {
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
                // Every import that is not standard-library is looked up in
                // the project: one that names a file of it is `local`,
                // whatever its syntax (`from app.models import User`, #81).
                let resolved: Vec<Option<String>> = imports
                    .iter()
                    .map(|import| {
                        if import.kind == ImportKind::Std {
                            None
                        } else {
                            resolve_import(
                                &rel_path,
                                &import.path,
                                language,
                                &self.project_root,
                                &self.import_context,
                            )
                        }
                    })
                    .collect();
                let dependencies: Vec<DependencyInsert<'_>> = imports
                    .iter()
                    .zip(&resolved)
                    .map(|(import, resolved)| DependencyInsert {
                        target_path: &import.path,
                        import_name: import.name.as_deref(),
                        kind: if resolved.is_some() {
                            ImportKind::Local.as_str()
                        } else {
                            import.kind.as_str()
                        },
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
                // Nothing of this content could be extracted: the rows of the
                // previous version would be served as if they described it
                // (symbols and refs at old lines, stale chunks). Clear them;
                // the hash is stamped below all the same, so that a file
                // tree-sitter cannot parse is not parsed again on every
                // cycle, and the error is reported to the caller.
                self.store.insert_symbols(file_id, &[])?;
                self.store.insert_references(file_id, &[])?;
                self.store.insert_structure(file_id, &Default::default())?;
                self.store.insert_chunks(file_id, &[])?;
                self.store.replace_dependencies(file_id, &[])?;
                parse_error = Some(e);
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
        // This runs on the parse-failure path too, after the children were
        // cleared: the row then says "this content yields nothing" and the
        // file is parsed again when it changes.
        self.store
            .insert_file(&rel_path, Some(language.name()), &content, last_modified)?;

        match parse_error {
            Some(e) => Err(e.context(format!("parsing {}", rel_path))),
            None => Ok(true),
        }
    }

    /// Remove a path from the index: its own row, and the rows under it when
    /// it was a directory (the watcher reports a directory rename or removal
    /// as one event on the directory). Returns how many rows went.
    fn remove_file(&self, path: &Path) -> Result<usize> {
        let rel_path = to_relative_string(path, &self.project_root);

        let removed = self.store.delete_files_under(&rel_path)?;
        if removed > 0 {
            debug!("Removed from index: {} ({} rows)", rel_path, removed);
        }

        Ok(removed)
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
