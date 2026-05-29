//! Index a project directory

use anyhow::Result;
use ignore::WalkBuilder;
use semantiq_embeddings::create_embedding_model;
use semantiq_index::{IndexStore, MAX_FILE_SIZE, paths::to_relative_string, should_exclude_entry};
use semantiq_parser::{
    ChunkExtractor, ImportExtractor, ImportKind, Language, LanguageSupport, SymbolExtractor,
    resolve_local_import,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};
use tracing::{debug, error, info, warn};

use super::common::{resolve_db_path, resolve_project_root};

/// Per-file extraction counts, accumulated into the run totals.
#[derive(Default)]
struct FileStats {
    symbols: usize,
    chunks: usize,
    deps: usize,
}

pub(crate) async fn index(path: &Path, database: Option<PathBuf>, force: bool) -> Result<()> {
    let project_root = resolve_project_root(path)?;
    let db_path = resolve_db_path(database, &project_root);

    info!("Indexing project: {:?}", project_root);
    info!("Database: {:?}", db_path);

    let start = Instant::now();
    let store = IndexStore::open(&db_path)?;

    // Check if parser version changed and prepare for full reindex if needed
    let needs_full_reindex = store.check_and_prepare_for_reindex()?;
    let force = force || needs_full_reindex;

    let mut language_support = LanguageSupport::new()?;
    let chunk_extractor = ChunkExtractor::new();

    // Initialize embedding model
    let embedding_model = match create_embedding_model(None) {
        Ok(model) => {
            info!("Embedding model loaded (dim={})", model.dimension());
            Some(model)
        }
        Err(e) => {
            warn!(
                "Could not load embedding model: {}. Embeddings will not be generated.",
                e
            );
            None
        }
    };

    let mut file_count = 0;
    let mut symbol_count = 0;
    let mut chunk_count = 0;
    let mut dep_count = 0;
    let mut error_count = 0;

    // Walk the directory, excluding hidden dirs and dependency folders
    let walker = WalkBuilder::new(&project_root)
        .hidden(true) // Exclude hidden directories (.git, .claude, etc.)
        .git_ignore(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !should_exclude_entry(&name)
        })
        .build();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        // Check if this is a supported language
        let language = match Language::from_path(path) {
            Some(lang) => lang,
            None => continue,
        };

        // Get relative path (warns if `path` falls outside `project_root`).
        let rel_path = to_relative_string(path, &project_root);

        // Index this file in isolation so a DB/IO/parse error on a single file
        // is logged and counted but does not abort the whole run.
        match index_one_file(
            &store,
            &mut language_support,
            &chunk_extractor,
            embedding_model.as_deref(),
            &project_root,
            path,
            &rel_path,
            language,
            force,
        ) {
            Ok(Some(stats)) => {
                symbol_count += stats.symbols;
                chunk_count += stats.chunks;
                dep_count += stats.deps;
                file_count += 1;

                // Progress update every 100 files
                if file_count % 100 == 0 {
                    info!("Indexed {} files...", file_count);
                }
            }
            Ok(None) => {
                // File skipped (unreadable, unchanged, or too large).
            }
            Err(e) => {
                error!("Failed to index {}: {}", rel_path, e);
                error_count += 1;
            }
        }
    }

    let elapsed = start.elapsed();

    info!("Indexing complete!");
    info!("  Files: {}", file_count);
    info!("  Symbols: {}", symbol_count);
    info!("  Chunks: {}", chunk_count);
    info!("  Dependencies: {}", dep_count);
    info!("  Errors: {}", error_count);
    info!("  Time: {:.2}s", elapsed.as_secs_f64());

    Ok(())
}

/// Index a single file, returning its extraction stats.
///
/// Returns `Ok(None)` when the file is intentionally skipped (unreadable,
/// unchanged, or too large) and `Ok(Some(stats))` when it is indexed. Errors
/// are propagated so the caller can log and count them without aborting the run.
#[allow(clippy::too_many_arguments)]
fn index_one_file(
    store: &IndexStore,
    language_support: &mut LanguageSupport,
    chunk_extractor: &ChunkExtractor,
    embedding_model: Option<&dyn semantiq_embeddings::EmbeddingModel>,
    project_root: &Path,
    path: &Path,
    rel_path: &str,
    language: Language,
    force: bool,
) -> Result<Option<FileStats>> {
    // Read file content
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            debug!("Skipping {}: {}", rel_path, e);
            return Ok(None);
        }
    };

    // Check if we need to reindex
    if !force && !store.needs_reindex(rel_path, &content)? {
        debug!("Skipping {} (unchanged)", rel_path);
        return Ok(None);
    }

    // Get file metadata
    let metadata = fs::metadata(path)?;
    let size = metadata.len() as i64;
    let last_modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0i64);

    // Skip large files
    if size > MAX_FILE_SIZE as i64 {
        debug!("Skipping {} (too large: {} bytes)", rel_path, size);
        return Ok(None);
    }

    // Insert file record
    let file_id = store.insert_file(
        rel_path,
        Some(language.name()),
        &content,
        size,
        last_modified,
    )?;

    let mut stats = FileStats::default();

    // Parse and extract symbols
    match language_support.parse(language, &content) {
        Ok(tree) => {
            // Extract symbols
            let symbols = SymbolExtractor::extract(&tree, &content, language)?;
            store.insert_symbols(file_id, &symbols)?;
            stats.symbols = symbols.len();

            // Extract chunks
            let chunks = chunk_extractor.extract(&tree, &content, language)?;
            store.insert_chunks(file_id, &chunks)?;
            stats.chunks = chunks.len();

            // Generate embeddings for chunks in batch to reduce ONNX overhead.
            if let Some(model) = embedding_model {
                let stored_chunks = store.get_chunks_by_file(file_id)?;
                if !stored_chunks.is_empty() {
                    let texts: Vec<String> =
                        stored_chunks.iter().map(|c| c.content.clone()).collect();
                    match model.embed_batch(&texts) {
                        Ok(embeddings) => {
                            for (chunk, embedding) in stored_chunks.iter().zip(embeddings.iter()) {
                                if let Err(e) = store.update_chunk_embedding(chunk.id, embedding) {
                                    warn!(
                                        "Failed to store embedding for chunk {}: {}",
                                        chunk.id, e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            debug!("Batch embedding failed, falling back to individual: {}", e);
                            // Fallback to individual embedding on batch failure.
                            for chunk in &stored_chunks {
                                match model.embed(&chunk.content) {
                                    Ok(embedding) => {
                                        if let Err(e) =
                                            store.update_chunk_embedding(chunk.id, &embedding)
                                        {
                                            warn!(
                                                "Failed to store embedding for chunk {}: {}",
                                                chunk.id, e
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        debug!(
                                            "Failed to generate embedding for chunk {}: {}",
                                            chunk.id, e
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Extract imports and store as dependencies
            let imports = ImportExtractor::extract(&tree, &content, language)?;
            store.delete_dependencies(file_id)?;
            for import in &imports {
                let resolved = if import.kind == ImportKind::Local {
                    resolve_local_import(rel_path, &import.path, language, project_root)
                } else {
                    None
                };
                store.insert_dependency(
                    file_id,
                    &import.path,
                    import.name.as_deref(),
                    import.kind.as_str(),
                    resolved.as_deref(),
                )?;
            }
            stats.deps = imports.len();

            debug!(
                "Indexed {}: {} symbols, {} chunks, {} deps",
                rel_path, stats.symbols, stats.chunks, stats.deps
            );
        }
        Err(e) => {
            warn!("Failed to parse {}: {}", rel_path, e);
        }
    }

    Ok(Some(stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use semantiq_embeddings::StubEmbeddingModel;
    use std::fs;
    use tempfile::tempdir;

    /// A real source file is indexed and its stats are reported.
    #[test]
    fn test_index_one_file_indexes_real_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let file = root.join("lib.rs");
        fs::write(&file, "pub fn add(a: i32, b: i32) -> i32 { a + b }\n").unwrap();

        let store = IndexStore::open_in_memory().unwrap();
        let mut language_support = LanguageSupport::new().unwrap();
        let chunk_extractor = ChunkExtractor::new();
        let model = StubEmbeddingModel::new();

        let result = index_one_file(
            &store,
            &mut language_support,
            &chunk_extractor,
            Some(&model),
            root,
            &file,
            "lib.rs",
            Language::Rust,
            true,
        )
        .unwrap();

        let stats = result.expect("file should be indexed");
        assert!(stats.symbols >= 1, "expected at least one symbol");

        let db_stats = store.get_stats().unwrap();
        assert_eq!(db_stats.file_count, 1);
        assert_eq!(db_stats.chunk_count, stats.chunks);
    }

    /// An unreadable / missing path is skipped (Ok(None)), not an error, so the
    /// caller keeps going instead of aborting the whole run.
    #[test]
    fn test_index_one_file_skips_unreadable_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let missing = root.join("does_not_exist.rs");

        let store = IndexStore::open_in_memory().unwrap();
        let mut language_support = LanguageSupport::new().unwrap();
        let chunk_extractor = ChunkExtractor::new();

        let result = index_one_file(
            &store,
            &mut language_support,
            &chunk_extractor,
            None,
            root,
            &missing,
            "does_not_exist.rs",
            Language::Rust,
            true,
        )
        .expect("missing file should be a skip, not an error");

        assert!(result.is_none(), "missing file should yield Ok(None)");
        assert_eq!(store.get_stats().unwrap().file_count, 0);
    }

    /// Unchanged files are skipped on a second pass when `force` is false.
    #[test]
    fn test_index_one_file_skips_unchanged() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let file = root.join("main.rs");
        fs::write(&file, "fn main() {}\n").unwrap();

        let store = IndexStore::open_in_memory().unwrap();
        let mut language_support = LanguageSupport::new().unwrap();
        let chunk_extractor = ChunkExtractor::new();

        // First pass indexes the file.
        let first = index_one_file(
            &store,
            &mut language_support,
            &chunk_extractor,
            None,
            root,
            &file,
            "main.rs",
            Language::Rust,
            false,
        )
        .unwrap();
        assert!(first.is_some());

        // Second pass without force sees no change and skips.
        let second = index_one_file(
            &store,
            &mut language_support,
            &chunk_extractor,
            None,
            root,
            &file,
            "main.rs",
            Language::Rust,
            false,
        )
        .unwrap();
        assert!(second.is_none(), "unchanged file should be skipped");
    }

    /// Batch embedding via the stub model populates chunk vectors without error.
    #[test]
    fn test_index_one_file_batch_embeddings() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let file = root.join("util.rs");
        fs::write(
            &file,
            "pub fn one() {}\npub fn two() {}\npub fn three() {}\n",
        )
        .unwrap();

        let store = IndexStore::open_in_memory().unwrap();
        let mut language_support = LanguageSupport::new().unwrap();
        let chunk_extractor = ChunkExtractor::new();
        let model = StubEmbeddingModel::new();

        let stats = index_one_file(
            &store,
            &mut language_support,
            &chunk_extractor,
            Some(&model),
            root,
            &file,
            "util.rs",
            Language::Rust,
            true,
        )
        .unwrap()
        .expect("file should be indexed");

        // Stub embeddings should not leave orphan chunk vectors behind.
        assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
        let _ = stats.chunks;
    }
}
