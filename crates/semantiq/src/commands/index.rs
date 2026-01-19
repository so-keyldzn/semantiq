//! Index a project directory

use anyhow::Result;
use ignore::WalkBuilder;
use semantiq_embeddings::create_embedding_model;
use semantiq_index::{IndexStore, MAX_FILE_SIZE, should_exclude_entry};
use semantiq_parser::{
    ChunkExtractor, ImportExtractor, Language, LanguageSupport, SymbolExtractor,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};
use tracing::{debug, info, warn};

use super::common::{resolve_db_path, resolve_project_root};

pub async fn index(path: &Path, database: Option<PathBuf>, force: bool) -> Result<()> {
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

        // Get relative path
        let rel_path = path
            .strip_prefix(&project_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Read file content
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                debug!("Skipping {}: {}", rel_path, e);
                continue;
            }
        };

        // Check if we need to reindex
        if !force && !store.needs_reindex(&rel_path, &content)? {
            debug!("Skipping {} (unchanged)", rel_path);
            continue;
        }

        // Get file metadata
        let metadata = fs::metadata(path)?;
        let size = metadata.len() as i64;
        let last_modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Skip large files
        if size > MAX_FILE_SIZE as i64 {
            debug!("Skipping {} (too large: {} bytes)", rel_path, size);
            continue;
        }

        // Insert file record
        let file_id = store.insert_file(
            &rel_path,
            Some(language.name()),
            &content,
            size,
            last_modified,
        )?;

        // Parse and extract symbols
        match language_support.parse(language, &content) {
            Ok(tree) => {
                // Extract symbols
                let symbols = SymbolExtractor::extract(&tree, &content, language)?;
                store.insert_symbols(file_id, &symbols)?;
                symbol_count += symbols.len();

                // Extract chunks
                let chunks = chunk_extractor.extract(&tree, &content, language)?;
                store.insert_chunks(file_id, &chunks)?;
                chunk_count += chunks.len();

                // Generate embeddings for chunks
                if let Some(ref model) = embedding_model {
                    let stored_chunks = store.get_chunks_by_file(file_id)?;
                    for chunk in stored_chunks {
                        match model.embed(&chunk.content) {
                            Ok(embedding) => {
                                if let Err(e) = store.update_chunk_embedding(chunk.id, &embedding) {
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

                // Extract imports and store as dependencies
                let imports = ImportExtractor::extract(&tree, &content, language)?;
                store.delete_dependencies(file_id)?;
                for import in &imports {
                    store.insert_dependency(
                        file_id,
                        &import.path,
                        import.name.as_deref(),
                        import.kind.as_str(),
                    )?;
                }
                dep_count += imports.len();

                debug!(
                    "Indexed {}: {} symbols, {} chunks, {} deps",
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

        file_count += 1;

        // Progress update every 100 files
        if file_count % 100 == 0 {
            info!("Indexed {} files...", file_count);
        }
    }

    let elapsed = start.elapsed();

    info!("Indexing complete!");
    info!("  Files: {}", file_count);
    info!("  Symbols: {}", symbol_count);
    info!("  Chunks: {}", chunk_count);
    info!("  Dependencies: {}", dep_count);
    info!("  Time: {:.2}s", elapsed.as_secs_f64());

    Ok(())
}
