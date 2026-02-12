use crate::IndexStore;
use crate::exclusions::{should_exclude, should_exclude_entry};
use crate::watcher::{FileEvent, FileWatcher};
use anyhow::Result;
use ignore::WalkBuilder;
use semantiq_embeddings::{EmbeddingModel, create_embedding_model};
use semantiq_parser::{
    ChunkExtractor, ImportExtractor, Language, LanguageSupport, SymbolExtractor,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;
use tracing::{debug, error, info, warn};

pub struct AutoIndexer {
    store: Arc<IndexStore>,
    watcher: Mutex<FileWatcher>,
    project_root: PathBuf,
    language_support: Mutex<LanguageSupport>,
    chunk_extractor: ChunkExtractor,
    embedding_model: Box<dyn EmbeddingModel>,
}

impl AutoIndexer {
    pub fn new(store: Arc<IndexStore>, project_root: PathBuf) -> Result<Self> {
        let mut watcher = FileWatcher::new()?;
        watcher.watch(&project_root)?;

        let language_support = LanguageSupport::new()?;
        let chunk_extractor = ChunkExtractor::new();

        // Initialize embedding model (downloads if needed)
        let embedding_model = create_embedding_model(None)?;
        info!(
            "Embedding model initialized (dim={})",
            embedding_model.dimension()
        );

        info!("AutoIndexer initialized for {:?}", project_root);

        Ok(Self {
            store,
            watcher: Mutex::new(watcher),
            project_root,
            language_support: Mutex::new(language_support),
            chunk_extractor,
            embedding_model,
        })
    }

    /// Perform initial indexing of all files in the project
    /// Only indexes files that are new or have changed since last index
    pub fn initial_index(&self) -> Result<InitialIndexResult> {
        info!("Starting initial index of {:?}", self.project_root);

        let mut result = InitialIndexResult::default();

        // Use ignore crate to walk directory respecting .gitignore
        let walker = WalkBuilder::new(&self.project_root)
            .hidden(true) // Skip hidden files by default
            .git_ignore(true) // Respect .gitignore
            .git_global(true) // Respect global gitignore
            .git_exclude(true) // Respect .git/info/exclude
            .filter_entry(|entry| {
                // Skip excluded directories
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    let name = entry.file_name().to_string_lossy();
                    return !should_exclude_entry(&name);
                }
                true
            })
            .build();

        for entry in walker.flatten() {
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

            // Get relative path
            let rel_path = path
                .strip_prefix(&self.project_root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();

            // Read file content to check if needs reindex
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    debug!("Skipping {}: {}", rel_path, e);
                    continue;
                }
            };

            // Check if file needs to be reindexed
            match self.store.needs_reindex(&rel_path, &content) {
                Ok(true) => {
                    // File is new or changed, index it
                    if let Err(e) = self.index_file(path) {
                        error!("Failed to index {}: {}", rel_path, e);
                        result.errors += 1;
                    } else {
                        result.indexed += 1;
                    }
                }
                Ok(false) => {
                    // File already indexed and unchanged
                    result.skipped += 1;
                }
                Err(e) => {
                    debug!("Error checking reindex for {}: {}", rel_path, e);
                    // Try to index anyway
                    if let Err(e) = self.index_file(path) {
                        error!("Failed to index {}: {}", rel_path, e);
                        result.errors += 1;
                    } else {
                        result.indexed += 1;
                    }
                }
            }
        }

        info!(
            "Initial index complete: {} scanned, {} indexed, {} skipped, {} errors",
            result.scanned, result.indexed, result.skipped, result.errors
        );

        Ok(result)
    }

    /// Process pending file events and reindex changed files
    pub fn process_events(&self) -> Result<ProcessResult> {
        let events = {
            let watcher = self
                .watcher
                .lock()
                .map_err(|e| anyhow::anyhow!("FileWatcher lock poisoned: {}", e))?;
            watcher.poll_events()
        };

        if events.is_empty() {
            return Ok(ProcessResult::default());
        }

        let mut result = ProcessResult::default();

        for event in events {
            match event {
                FileEvent::Created(path) | FileEvent::Modified(path) => {
                    if let Err(e) = self.index_file(&path) {
                        error!("Failed to index {:?}: {}", path, e);
                        result.errors += 1;
                    } else {
                        result.indexed += 1;
                    }
                }
                FileEvent::Deleted(path) => {
                    if let Err(e) = self.remove_file(&path) {
                        error!("Failed to remove {:?}: {}", path, e);
                        result.errors += 1;
                    } else {
                        result.removed += 1;
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

        Ok(result)
    }

    /// Index a single file
    fn index_file(&self, path: &Path) -> Result<()> {
        // Skip excluded paths (hidden dirs, node_modules, large files, etc.)
        if should_exclude(path) {
            debug!("Skipping excluded path: {:?}", path);
            return Ok(());
        }

        // Check if this is a supported language
        let language = match Language::from_path(path) {
            Some(lang) => lang,
            None => {
                debug!("Skipping unsupported file: {:?}", path);
                return Ok(());
            }
        };

        // Get relative path
        let rel_path = path
            .strip_prefix(&self.project_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Read file content
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                debug!("Skipping {}: {}", rel_path, e);
                return Ok(());
            }
        };

        // Get file metadata
        let metadata = fs::metadata(path)?;
        let size = metadata.len() as i64;
        let last_modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Insert file record
        let file_id = self.store.insert_file(
            &rel_path,
            Some(language.name()),
            &content,
            size,
            last_modified,
        )?;

        // Parse and extract symbols
        let mut language_support = self
            .language_support
            .lock()
            .map_err(|e| anyhow::anyhow!("LanguageSupport lock poisoned: {}", e))?;
        match language_support.parse(language, &content) {
            Ok(tree) => {
                // Extract symbols
                let symbols = SymbolExtractor::extract(&tree, &content, language)?;
                self.store.insert_symbols(file_id, &symbols)?;

                // Extract chunks and generate embeddings
                let chunks = self.chunk_extractor.extract(&tree, &content, language)?;
                self.store.insert_chunks(file_id, &chunks)?;

                // Generate embeddings for chunks in batch to reduce ONNX overhead
                let chunks_to_embed = self.store.get_chunks_by_file(file_id)?;
                if !chunks_to_embed.is_empty() {
                    let texts: Vec<String> =
                        chunks_to_embed.iter().map(|c| c.content.clone()).collect();
                    match self.embedding_model.embed_batch(&texts) {
                        Ok(embeddings) => {
                            for (chunk, embedding) in chunks_to_embed.iter().zip(embeddings.iter())
                            {
                                if let Err(e) =
                                    self.store.update_chunk_embedding(chunk.id, embedding)
                                {
                                    debug!(
                                        "Failed to store embedding for chunk {}: {}",
                                        chunk.id, e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            debug!("Batch embedding failed, falling back to individual: {}", e);
                            // Fallback to individual embedding on batch failure
                            for chunk in &chunks_to_embed {
                                match self.embedding_model.embed(&chunk.content) {
                                    Ok(embedding) => {
                                        if let Err(e) =
                                            self.store.update_chunk_embedding(chunk.id, &embedding)
                                        {
                                            debug!(
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

                // Extract imports and store as dependencies
                let imports = ImportExtractor::extract(&tree, &content, language)?;
                self.store.delete_dependencies(file_id)?;
                for import in &imports {
                    self.store.insert_dependency(
                        file_id,
                        &import.path,
                        import.name.as_deref(),
                        import.kind.as_str(),
                    )?;
                }

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

        Ok(())
    }

    /// Remove a file from the index
    fn remove_file(&self, path: &Path) -> Result<()> {
        let rel_path = path
            .strip_prefix(&self.project_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        self.store.delete_file(&rel_path)?;
        debug!("Removed from index: {}", rel_path);

        Ok(())
    }
}

#[derive(Default, Debug)]
pub struct ProcessResult {
    pub indexed: usize,
    pub removed: usize,
    pub errors: usize,
}

#[derive(Default, Debug)]
pub struct InitialIndexResult {
    pub scanned: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub errors: usize,
}
