use crate::watcher::{FileEvent, FileWatcher};
use crate::IndexStore;
use anyhow::Result;
use semantiq_parser::{ChunkExtractor, ImportExtractor, Language, LanguageSupport, SymbolExtractor};
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
}

impl AutoIndexer {
    pub fn new(store: Arc<IndexStore>, project_root: PathBuf) -> Result<Self> {
        let mut watcher = FileWatcher::new()?;
        watcher.watch(&project_root)?;

        let language_support = LanguageSupport::new()?;
        let chunk_extractor = ChunkExtractor::new();

        info!("AutoIndexer initialized for {:?}", project_root);

        Ok(Self {
            store,
            watcher: Mutex::new(watcher),
            project_root,
            language_support: Mutex::new(language_support),
            chunk_extractor,
        })
    }

    /// Process pending file events and reindex changed files
    pub fn process_events(&self) -> Result<ProcessResult> {
        let events = {
            let watcher = self.watcher.lock().unwrap();
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
        let mut language_support = self.language_support.lock().unwrap();
        match language_support.parse(language, &content) {
            Ok(tree) => {
                // Extract symbols
                let symbols = SymbolExtractor::extract(&tree, &content, language)?;
                self.store.insert_symbols(file_id, &symbols)?;

                // Extract chunks
                let chunks = self.chunk_extractor.extract(&tree, &content, language)?;
                self.store.insert_chunks(file_id, &chunks)?;

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
