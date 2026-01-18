use crate::schema::{init_schema, ChunkRecord, DependencyRecord, FileRecord, SymbolRecord};
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use semantiq_parser::{CodeChunk, Symbol, PARSER_VERSION};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

pub struct IndexStore {
    conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
}

impl IndexStore {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open database at {:?}", path))?;

        // Enable WAL mode for better concurrency
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;

        init_schema(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: path.to_path_buf(),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        init_schema(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: PathBuf::from(":memory:"),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Helper function to safely acquire the connection lock with proper error handling
    fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.conn.lock().map_err(|e: PoisonError<MutexGuard<Connection>>| {
            anyhow!("Database lock poisoned: {}", e)
        })?;
        f(&conn)
    }

    // File operations

    pub fn insert_file(
        &self,
        path: &str,
        language: Option<&str>,
        content: &str,
        size: i64,
        last_modified: i64,
    ) -> Result<i64> {
        let hash = Self::hash_content(content);
        let indexed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("System time before UNIX epoch")?
            .as_secs() as i64;

        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO files (path, language, hash, size, last_modified, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![path, language, hash, size, last_modified, indexed_at],
            )?;

            let id = conn.last_insert_rowid();
            debug!("Inserted file {} with id {}", path, id);
            Ok(id)
        })
    }

    pub fn get_file_by_path(&self, path: &str) -> Result<Option<FileRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, path, language, hash, size, last_modified, indexed_at
                 FROM files WHERE path = ?1",
            )?;

            let result = stmt
                .query_row([path], |row| {
                    Ok(FileRecord {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        language: row.get(2)?,
                        hash: row.get(3)?,
                        size: row.get(4)?,
                        last_modified: row.get(5)?,
                        indexed_at: row.get(6)?,
                    })
                })
                .optional()?;

            Ok(result)
        })
    }

    pub fn needs_reindex(&self, path: &str, content: &str) -> Result<bool> {
        if let Some(file) = self.get_file_by_path(path)? {
            let current_hash = Self::hash_content(content);
            Ok(file.hash != current_hash)
        } else {
            Ok(true)
        }
    }

    pub fn delete_file(&self, path: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM files WHERE path = ?1", [path])?;
            Ok(())
        })
    }

    pub fn get_file_path_by_id(&self, file_id: i64) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let result = conn
                .query_row(
                    "SELECT path FROM files WHERE id = ?1",
                    [file_id],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(result)
        })
    }

    // Symbol operations

    pub fn insert_symbols(&self, file_id: i64, symbols: &[Symbol]) -> Result<()> {
        let conn = self.conn.lock().map_err(|e: PoisonError<MutexGuard<Connection>>| {
            anyhow!("Database lock poisoned: {}", e)
        })?;

        // Use a transaction for atomicity
        conn.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| -> Result<()> {
            // Delete existing symbols for this file
            conn.execute("DELETE FROM symbols WHERE file_id = ?1", [file_id])?;

            let mut stmt = conn.prepare(
                "INSERT INTO symbols (file_id, name, kind, start_line, end_line, start_byte, end_byte, signature, doc_comment, parent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;

            for symbol in symbols {
                stmt.execute(params![
                    file_id,
                    symbol.name,
                    symbol.kind.as_str(),
                    symbol.start_line as i64,
                    symbol.end_line as i64,
                    symbol.start_byte as i64,
                    symbol.end_byte as i64,
                    symbol.signature,
                    symbol.doc_comment,
                    symbol.parent,
                ])?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
                debug!("Inserted {} symbols for file_id {}", symbols.len(), file_id);
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    pub fn search_symbols(&self, query: &str, limit: usize) -> Result<Vec<SymbolRecord>> {
        // Cap limit to prevent excessive memory usage
        let safe_limit = limit.min(10000);

        self.with_conn(|conn| {
            // Use FTS5 for full-text search
            let mut stmt = conn.prepare(
                "SELECT s.id, s.file_id, s.name, s.kind, s.start_line, s.end_line,
                        s.start_byte, s.end_byte, s.signature, s.doc_comment, s.parent
                 FROM symbols s
                 JOIN symbols_fts ON s.id = symbols_fts.rowid
                 WHERE symbols_fts MATCH ?1
                 LIMIT ?2",
            )?;

            // Escape special FTS5 characters by quoting the query
            // FTS5 treats - as NOT, + as AND, etc.
            let escaped_query = query.replace('"', "\"\"");
            let fts_query = format!("\"{}\"*", escaped_query);
            let results = stmt
                .query_map(params![fts_query, safe_limit as i64], |row| {
                    Ok(SymbolRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        name: row.get(2)?,
                        kind: row.get(3)?,
                        start_line: row.get(4)?,
                        end_line: row.get(5)?,
                        start_byte: row.get(6)?,
                        end_byte: row.get(7)?,
                        signature: row.get(8)?,
                        doc_comment: row.get(9)?,
                        parent: row.get(10)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    pub fn find_symbol_by_name(&self, name: &str) -> Result<Vec<SymbolRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, file_id, name, kind, start_line, end_line,
                        start_byte, end_byte, signature, doc_comment, parent
                 FROM symbols WHERE name = ?1",
            )?;

            let results = stmt
                .query_map([name], |row| {
                    Ok(SymbolRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        name: row.get(2)?,
                        kind: row.get(3)?,
                        start_line: row.get(4)?,
                        end_line: row.get(5)?,
                        start_byte: row.get(6)?,
                        end_byte: row.get(7)?,
                        signature: row.get(8)?,
                        doc_comment: row.get(9)?,
                        parent: row.get(10)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    pub fn get_symbols_by_file(&self, file_id: i64) -> Result<Vec<SymbolRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, file_id, name, kind, start_line, end_line,
                        start_byte, end_byte, signature, doc_comment, parent
                 FROM symbols WHERE file_id = ?1
                 ORDER BY start_line",
            )?;

            let results = stmt
                .query_map([file_id], |row| {
                    Ok(SymbolRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        name: row.get(2)?,
                        kind: row.get(3)?,
                        start_line: row.get(4)?,
                        end_line: row.get(5)?,
                        start_byte: row.get(6)?,
                        end_byte: row.get(7)?,
                        signature: row.get(8)?,
                        doc_comment: row.get(9)?,
                        parent: row.get(10)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    // Chunk operations

    pub fn insert_chunks(&self, file_id: i64, chunks: &[CodeChunk]) -> Result<()> {
        let conn = self.conn.lock().map_err(|e: PoisonError<MutexGuard<Connection>>| {
            anyhow!("Database lock poisoned: {}", e)
        })?;

        // Use a transaction for atomicity
        conn.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| -> Result<()> {
            // Delete existing chunks for this file
            conn.execute("DELETE FROM chunks WHERE file_id = ?1", [file_id])?;

            let mut stmt = conn.prepare(
                "INSERT INTO chunks (file_id, content, start_line, end_line, start_byte, end_byte, symbols_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;

            for chunk in chunks {
                let symbols_json = serde_json::to_string(&chunk.symbols)?;
                stmt.execute(params![
                    file_id,
                    chunk.content,
                    chunk.start_line as i64,
                    chunk.end_line as i64,
                    chunk.start_byte as i64,
                    chunk.end_byte as i64,
                    symbols_json,
                ])?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
                debug!("Inserted {} chunks for file_id {}", chunks.len(), file_id);
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    pub fn update_chunk_embedding(&self, chunk_id: i64, embedding: &[f32]) -> Result<()> {
        self.with_conn(|conn| {
            // Convert f32 slice to bytes
            let embedding_bytes: Vec<u8> = embedding
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();

            conn.execute(
                "UPDATE chunks SET embedding = ?1 WHERE id = ?2",
                params![embedding_bytes, chunk_id],
            )?;

            Ok(())
        })
    }

    pub fn get_chunks_without_embeddings(&self, limit: usize) -> Result<Vec<ChunkRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, file_id, content, start_line, end_line, start_byte, end_byte, symbols_json
                 FROM chunks WHERE embedding IS NULL
                 LIMIT ?1",
            )?;

            let results = stmt
                .query_map([limit as i64], |row| {
                    let symbols_json: String = row.get(7)?;
                    let symbols: Vec<String> = serde_json::from_str(&symbols_json).unwrap_or_default();

                    Ok(ChunkRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        content: row.get(2)?,
                        start_line: row.get(3)?,
                        end_line: row.get(4)?,
                        start_byte: row.get(5)?,
                        end_byte: row.get(6)?,
                        symbols,
                        embedding: None,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    pub fn get_chunks_by_file(&self, file_id: i64) -> Result<Vec<ChunkRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, file_id, content, start_line, end_line, start_byte, end_byte, symbols_json
                 FROM chunks WHERE file_id = ?1",
            )?;

            let results = stmt
                .query_map([file_id], |row| {
                    let symbols_json: String = row.get(7)?;
                    let symbols: Vec<String> = serde_json::from_str(&symbols_json).unwrap_or_default();

                    Ok(ChunkRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        content: row.get(2)?,
                        start_line: row.get(3)?,
                        end_line: row.get(4)?,
                        start_byte: row.get(5)?,
                        end_byte: row.get(6)?,
                        symbols,
                        embedding: None,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    pub fn get_chunks_with_embeddings(&self) -> Result<Vec<(ChunkRecord, Vec<f32>)>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT c.id, c.file_id, c.content, c.start_line, c.end_line, c.start_byte, c.end_byte, c.symbols_json, c.embedding, f.path
                 FROM chunks c
                 JOIN files f ON c.file_id = f.id
                 WHERE c.embedding IS NOT NULL",
            )?;

            let results = stmt
                .query_map([], |row| {
                    let symbols_json: String = row.get(7)?;
                    let symbols: Vec<String> = serde_json::from_str(&symbols_json).unwrap_or_default();
                    let embedding_bytes: Vec<u8> = row.get(8)?;

                    // Convert bytes back to f32
                    let embedding: Vec<f32> = embedding_bytes
                        .chunks(4)
                        .map(|chunk| {
                            let bytes: [u8; 4] = chunk.try_into().unwrap_or([0; 4]);
                            f32::from_le_bytes(bytes)
                        })
                        .collect();

                    let chunk = ChunkRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        content: row.get(2)?,
                        start_line: row.get(3)?,
                        end_line: row.get(4)?,
                        start_byte: row.get(5)?,
                        end_byte: row.get(6)?,
                        symbols,
                        embedding: Some(embedding.clone()),
                    };

                    Ok((chunk, embedding))
                })?
                .filter_map(|r| r.ok())
                .collect();

            Ok(results)
        })
    }

    pub fn get_chunk_file_path(&self, file_id: i64) -> Result<Option<String>> {
        self.get_file_path_by_id(file_id)
    }

    // Dependency operations

    pub fn insert_dependency(
        &self,
        source_file_id: i64,
        target_path: &str,
        import_name: Option<&str>,
        kind: &str,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO dependencies (source_file_id, target_path, import_name, kind)
                 VALUES (?1, ?2, ?3, ?4)",
                params![source_file_id, target_path, import_name, kind],
            )?;

            Ok(())
        })
    }

    pub fn delete_dependencies(&self, file_id: i64) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM dependencies WHERE source_file_id = ?1",
                [file_id],
            )?;
            Ok(())
        })
    }

    pub fn get_dependencies(&self, file_id: i64) -> Result<Vec<DependencyRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, source_file_id, target_path, import_name, kind
                 FROM dependencies WHERE source_file_id = ?1",
            )?;

            let results = stmt
                .query_map([file_id], |row| {
                    Ok(DependencyRecord {
                        id: row.get(0)?,
                        source_file_id: row.get(1)?,
                        target_path: row.get(2)?,
                        import_name: row.get(3)?,
                        kind: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    pub fn get_dependents(&self, target_path: &str) -> Result<Vec<DependencyRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, source_file_id, target_path, import_name, kind
                 FROM dependencies WHERE target_path LIKE ?1 ESCAPE '\\'",
            )?;

            // Escape special LIKE characters to prevent injection
            let escaped_path = target_path
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let pattern = format!("%{}", escaped_path);

            let results = stmt
                .query_map([pattern], |row| {
                    Ok(DependencyRecord {
                        id: row.get(0)?,
                        source_file_id: row.get(1)?,
                        target_path: row.get(2)?,
                        import_name: row.get(3)?,
                        kind: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    // Parser version management

    /// Vérifie si l'index doit être recréé (changement de parser)
    pub fn needs_full_reindex(&self) -> Result<bool> {
        self.with_conn(|conn| {
            Self::needs_full_reindex_impl(conn)
        })
    }

    /// Implémentation interne pour réutilisation dans une transaction
    fn needs_full_reindex_impl(conn: &Connection) -> Result<bool> {
        let stored_version: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'parser_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;

        match stored_version {
            Some(v) => {
                let stored: u32 = match v.parse() {
                    Ok(val) => val,
                    Err(_) => {
                        warn!("Corrupted parser_version in metadata: '{}', forcing reindex", v);
                        0
                    }
                };
                Ok(stored != PARSER_VERSION)
            }
            None => Ok(true), // No version stored = needs reindex
        }
    }

    /// Met à jour la version du parser dans metadata
    pub fn set_parser_version(&self) -> Result<()> {
        self.with_conn(|conn| {
            Self::set_parser_version_impl(conn)
        })
    }

    /// Implémentation interne pour réutilisation dans une transaction
    fn set_parser_version_impl(conn: &Connection) -> Result<()> {
        conn.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES ('parser_version', ?1)",
            [PARSER_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Supprime toutes les données indexées
    pub fn clear_all_data(&self) -> Result<()> {
        self.with_conn(|conn| {
            Self::clear_all_data_impl(conn)
        })
    }

    /// Implémentation interne pour réutilisation dans une transaction
    fn clear_all_data_impl(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             DELETE FROM dependencies;
             DELETE FROM chunks;
             DELETE FROM symbols;
             DELETE FROM files;
             COMMIT;",
        )?;
        debug!("Cleared all indexed data");
        Ok(())
    }

    /// Vérifie et nettoie si nécessaire. Retourne true si reindex nécessaire.
    /// Utilise une transaction unique pour éviter les race conditions.
    pub fn check_and_prepare_for_reindex(&self) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e: PoisonError<MutexGuard<Connection>>| {
            anyhow!("Database lock poisoned: {}", e)
        })?;

        if !Self::needs_full_reindex_impl(&conn)? {
            return Ok(false);
        }

        // Transaction unique pour check + clear + set version
        conn.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| -> Result<()> {
            // Clear all data (sans transaction interne car déjà dans une)
            conn.execute_batch(
                "DELETE FROM dependencies;
                 DELETE FROM chunks;
                 DELETE FROM symbols;
                 DELETE FROM files;",
            )?;
            Self::set_parser_version_impl(&conn)?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
                info!("Parser version changed - index cleared for full reindex");
                Ok(true)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    // Statistics

    pub fn get_stats(&self) -> Result<IndexStats> {
        self.with_conn(|conn| {
            // Single query instead of N+1 queries for better performance
            conn.query_row(
                "SELECT
                    (SELECT COUNT(*) FROM files) as file_count,
                    (SELECT COUNT(*) FROM symbols) as symbol_count,
                    (SELECT COUNT(*) FROM chunks) as chunk_count,
                    (SELECT COUNT(*) FROM dependencies) as dep_count",
                [],
                |row| {
                    Ok(IndexStats {
                        file_count: row.get::<_, i64>(0)? as usize,
                        symbol_count: row.get::<_, i64>(1)? as usize,
                        chunk_count: row.get::<_, i64>(2)? as usize,
                        dependency_count: row.get::<_, i64>(3)? as usize,
                    })
                },
            ).map_err(Into::into)
        })
    }

    // Helper functions

    fn hash_content(content: &str) -> String {
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}

#[derive(Debug, Clone)]
pub struct IndexStats {
    pub file_count: usize,
    pub symbol_count: usize,
    pub chunk_count: usize,
    pub dependency_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use semantiq_parser::SymbolKind;

    #[test]
    fn test_insert_and_get_file() {
        let store = IndexStore::open_in_memory().unwrap();

        let file_id = store
            .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
            .unwrap();

        let file = store.get_file_by_path("test.rs").unwrap().unwrap();
        assert_eq!(file.id, file_id);
        assert_eq!(file.path, "test.rs");
        assert_eq!(file.language, Some("rust".to_string()));
    }

    #[test]
    fn test_insert_and_search_symbols() {
        let store = IndexStore::open_in_memory().unwrap();

        let file_id = store
            .insert_file("test.rs", Some("rust"), "fn hello() {}", 13, 1000)
            .unwrap();

        let symbols = vec![Symbol {
            name: "hello".to_string(),
            kind: SymbolKind::Function,
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 13,
            signature: Some("fn hello()".to_string()),
            doc_comment: None,
            parent: None,
        }];

        store.insert_symbols(file_id, &symbols).unwrap();

        let results = store.find_symbol_by_name("hello").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "hello");
    }

    #[test]
    fn test_needs_full_reindex_no_version() {
        let store = IndexStore::open_in_memory().unwrap();
        // Fresh DB without parser_version should need reindex
        assert!(store.needs_full_reindex().unwrap());
    }

    #[test]
    fn test_needs_full_reindex_same_version() {
        let store = IndexStore::open_in_memory().unwrap();
        store.set_parser_version().unwrap();
        // After setting version, should not need reindex
        assert!(!store.needs_full_reindex().unwrap());
    }

    #[test]
    fn test_needs_full_reindex_different_version() {
        let store = IndexStore::open_in_memory().unwrap();
        // Set a different version manually
        store.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES ('parser_version', '999')",
                [],
            )?;
            Ok(())
        }).unwrap();
        // Should need reindex because version differs
        assert!(store.needs_full_reindex().unwrap());
    }

    #[test]
    fn test_needs_full_reindex_corrupted_version() {
        let store = IndexStore::open_in_memory().unwrap();
        // Set a corrupted version
        store.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES ('parser_version', 'not_a_number')",
                [],
            )?;
            Ok(())
        }).unwrap();
        // Should need reindex because version is corrupted
        assert!(store.needs_full_reindex().unwrap());
    }

    #[test]
    fn test_clear_all_data() {
        let store = IndexStore::open_in_memory().unwrap();

        // Insert some data
        let file_id = store
            .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
            .unwrap();

        let symbols = vec![Symbol {
            name: "main".to_string(),
            kind: SymbolKind::Function,
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 12,
            signature: None,
            doc_comment: None,
            parent: None,
        }];
        store.insert_symbols(file_id, &symbols).unwrap();

        // Verify data exists
        let stats = store.get_stats().unwrap();
        assert_eq!(stats.file_count, 1);
        assert_eq!(stats.symbol_count, 1);

        // Clear all data
        store.clear_all_data().unwrap();

        // Verify data is gone
        let stats = store.get_stats().unwrap();
        assert_eq!(stats.file_count, 0);
        assert_eq!(stats.symbol_count, 0);
    }

    #[test]
    fn test_check_and_prepare_for_reindex() {
        let store = IndexStore::open_in_memory().unwrap();

        // Insert some data
        store
            .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
            .unwrap();

        // First call should return true (needs reindex) and clear data
        let needs_reindex = store.check_and_prepare_for_reindex().unwrap();
        assert!(needs_reindex);

        // Data should be cleared
        let stats = store.get_stats().unwrap();
        assert_eq!(stats.file_count, 0);

        // Second call should return false (version now set)
        let needs_reindex = store.check_and_prepare_for_reindex().unwrap();
        assert!(!needs_reindex);
    }

    #[test]
    fn test_insert_and_get_chunks() {
        use semantiq_parser::CodeChunk;

        let store = IndexStore::open_in_memory().unwrap();

        let file_id = store
            .insert_file("test.rs", Some("rust"), "fn main() {}\nfn foo() {}", 25, 1000)
            .unwrap();

        let chunks = vec![
            CodeChunk {
                content: "fn main() {}".to_string(),
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: 12,
                symbols: vec!["main".to_string()],
            },
            CodeChunk {
                content: "fn foo() {}".to_string(),
                start_line: 2,
                end_line: 2,
                start_byte: 13,
                end_byte: 24,
                symbols: vec!["foo".to_string()],
            },
        ];

        store.insert_chunks(file_id, &chunks).unwrap();

        let retrieved = store.get_chunks_by_file(file_id).unwrap();
        assert_eq!(retrieved.len(), 2);
        assert_eq!(retrieved[0].content, "fn main() {}");
        assert_eq!(retrieved[1].content, "fn foo() {}");
    }

    #[test]
    fn test_chunks_without_embeddings() {
        use semantiq_parser::CodeChunk;

        let store = IndexStore::open_in_memory().unwrap();

        let file_id = store
            .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
            .unwrap();

        let chunks = vec![CodeChunk {
            content: "fn main() {}".to_string(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 12,
            symbols: vec!["main".to_string()],
        }];

        store.insert_chunks(file_id, &chunks).unwrap();

        let without_embeddings = store.get_chunks_without_embeddings(10).unwrap();
        assert_eq!(without_embeddings.len(), 1);
    }

    #[test]
    fn test_update_chunk_embedding() {
        use semantiq_parser::CodeChunk;

        let store = IndexStore::open_in_memory().unwrap();

        let file_id = store
            .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
            .unwrap();

        let chunks = vec![CodeChunk {
            content: "fn main() {}".to_string(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 12,
            symbols: vec!["main".to_string()],
        }];

        store.insert_chunks(file_id, &chunks).unwrap();

        let chunks = store.get_chunks_by_file(file_id).unwrap();
        let chunk_id = chunks[0].id;

        let embedding: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4];
        store.update_chunk_embedding(chunk_id, &embedding).unwrap();

        // After updating, should no longer appear in "without embeddings"
        let without_embeddings = store.get_chunks_without_embeddings(10).unwrap();
        assert!(without_embeddings.is_empty());
    }

    #[test]
    fn test_insert_and_get_dependencies() {
        let store = IndexStore::open_in_memory().unwrap();

        let file_id = store
            .insert_file("src/main.rs", Some("rust"), "use crate::utils;", 17, 1000)
            .unwrap();

        store
            .insert_dependency(file_id, "crate::utils", Some("utils"), "local")
            .unwrap();
        store
            .insert_dependency(file_id, "std::io", Some("io"), "std")
            .unwrap();

        let deps = store.get_dependencies(file_id).unwrap();
        assert_eq!(deps.len(), 2);
        assert!(deps.iter().any(|d| d.target_path == "crate::utils"));
        assert!(deps.iter().any(|d| d.target_path == "std::io"));
    }

    #[test]
    fn test_get_dependents() {
        let store = IndexStore::open_in_memory().unwrap();

        let file_id = store
            .insert_file("src/main.rs", Some("rust"), "use crate::utils;", 17, 1000)
            .unwrap();

        store
            .insert_dependency(file_id, "src/utils.rs", Some("utils"), "local")
            .unwrap();

        let dependents = store.get_dependents("utils.rs").unwrap();
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0].source_file_id, file_id);
    }

    #[test]
    fn test_delete_dependencies() {
        let store = IndexStore::open_in_memory().unwrap();

        let file_id = store
            .insert_file("src/main.rs", Some("rust"), "use crate::utils;", 17, 1000)
            .unwrap();

        store
            .insert_dependency(file_id, "crate::utils", Some("utils"), "local")
            .unwrap();

        let deps = store.get_dependencies(file_id).unwrap();
        assert_eq!(deps.len(), 1);

        store.delete_dependencies(file_id).unwrap();

        let deps = store.get_dependencies(file_id).unwrap();
        assert_eq!(deps.len(), 0);
    }

    #[test]
    fn test_delete_file() {
        let store = IndexStore::open_in_memory().unwrap();

        store
            .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
            .unwrap();

        assert!(store.get_file_by_path("test.rs").unwrap().is_some());

        store.delete_file("test.rs").unwrap();

        assert!(store.get_file_by_path("test.rs").unwrap().is_none());
    }

    #[test]
    fn test_needs_reindex_same_content() {
        let store = IndexStore::open_in_memory().unwrap();
        let content = "fn main() {}";

        store
            .insert_file("test.rs", Some("rust"), content, 12, 1000)
            .unwrap();

        // Same content should not need reindex
        assert!(!store.needs_reindex("test.rs", content).unwrap());
    }

    #[test]
    fn test_needs_reindex_different_content() {
        let store = IndexStore::open_in_memory().unwrap();

        store
            .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
            .unwrap();

        // Different content should need reindex
        assert!(store.needs_reindex("test.rs", "fn main() { println!(\"hello\"); }").unwrap());
    }

    #[test]
    fn test_needs_reindex_new_file() {
        let store = IndexStore::open_in_memory().unwrap();

        // Non-existent file should need indexing
        assert!(store.needs_reindex("new_file.rs", "content").unwrap());
    }

    #[test]
    fn test_get_symbols_by_file() {
        let store = IndexStore::open_in_memory().unwrap();

        let file_id = store
            .insert_file("test.rs", Some("rust"), "fn hello() {}\nfn world() {}", 27, 1000)
            .unwrap();

        let symbols = vec![
            Symbol {
                name: "hello".to_string(),
                kind: SymbolKind::Function,
                start_line: 1,
                end_line: 1,
                start_byte: 0,
                end_byte: 13,
                signature: Some("fn hello()".to_string()),
                doc_comment: None,
                parent: None,
            },
            Symbol {
                name: "world".to_string(),
                kind: SymbolKind::Function,
                start_line: 2,
                end_line: 2,
                start_byte: 14,
                end_byte: 27,
                signature: Some("fn world()".to_string()),
                doc_comment: None,
                parent: None,
            },
        ];

        store.insert_symbols(file_id, &symbols).unwrap();

        let retrieved = store.get_symbols_by_file(file_id).unwrap();
        assert_eq!(retrieved.len(), 2);
        // Should be ordered by start_line
        assert_eq!(retrieved[0].name, "hello");
        assert_eq!(retrieved[1].name, "world");
    }

    #[test]
    fn test_search_symbols_fts() {
        let store = IndexStore::open_in_memory().unwrap();

        let file_id = store
            .insert_file("test.rs", Some("rust"), "fn calculate_total() {}", 23, 1000)
            .unwrap();

        let symbols = vec![Symbol {
            name: "calculate_total".to_string(),
            kind: SymbolKind::Function,
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 23,
            signature: Some("fn calculate_total()".to_string()),
            doc_comment: None,
            parent: None,
        }];

        store.insert_symbols(file_id, &symbols).unwrap();

        // FTS search should find the symbol
        let results = store.search_symbols("calculate", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "calculate_total");
    }

    #[test]
    fn test_get_stats() {
        let store = IndexStore::open_in_memory().unwrap();

        let stats = store.get_stats().unwrap();
        assert_eq!(stats.file_count, 0);
        assert_eq!(stats.symbol_count, 0);
        assert_eq!(stats.chunk_count, 0);
        assert_eq!(stats.dependency_count, 0);

        let file_id = store
            .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
            .unwrap();

        let symbols = vec![Symbol {
            name: "main".to_string(),
            kind: SymbolKind::Function,
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 12,
            signature: None,
            doc_comment: None,
            parent: None,
        }];
        store.insert_symbols(file_id, &symbols).unwrap();

        let stats = store.get_stats().unwrap();
        assert_eq!(stats.file_count, 1);
        assert_eq!(stats.symbol_count, 1);
    }

    #[test]
    fn test_db_path() {
        let store = IndexStore::open_in_memory().unwrap();
        assert_eq!(store.db_path().to_string_lossy(), ":memory:");
    }
}
