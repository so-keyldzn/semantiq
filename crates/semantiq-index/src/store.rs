use crate::schema::{init_schema, ChunkRecord, DependencyRecord, FileRecord, SymbolRecord};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use semantiq_parser::{CodeChunk, Symbol};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

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
             PRAGMA foreign_keys=ON;",
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
            .unwrap()
            .as_secs() as i64;

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO files (path, language, hash, size, last_modified, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![path, language, hash, size, last_modified, indexed_at],
        )?;

        let id = conn.last_insert_rowid();
        debug!("Inserted file {} with id {}", path, id);
        Ok(id)
    }

    pub fn get_file_by_path(&self, path: &str) -> Result<Option<FileRecord>> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM files WHERE path = ?1", [path])?;
        Ok(())
    }

    pub fn get_file_path_by_id(&self, file_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT path FROM files WHERE id = ?1",
                [file_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(result)
    }

    // Symbol operations

    pub fn insert_symbols(&self, file_id: i64, symbols: &[Symbol]) -> Result<()> {
        let conn = self.conn.lock().unwrap();

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

        debug!("Inserted {} symbols for file_id {}", symbols.len(), file_id);
        Ok(())
    }

    pub fn search_symbols(&self, query: &str, limit: usize) -> Result<Vec<SymbolRecord>> {
        let conn = self.conn.lock().unwrap();

        // Use FTS5 for full-text search
        let mut stmt = conn.prepare(
            "SELECT s.id, s.file_id, s.name, s.kind, s.start_line, s.end_line,
                    s.start_byte, s.end_byte, s.signature, s.doc_comment, s.parent
             FROM symbols s
             JOIN symbols_fts ON s.id = symbols_fts.rowid
             WHERE symbols_fts MATCH ?1
             LIMIT ?2",
        )?;

        let fts_query = format!("{}*", query);
        let results = stmt
            .query_map(params![fts_query, limit as i64], |row| {
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
    }

    pub fn find_symbol_by_name(&self, name: &str) -> Result<Vec<SymbolRecord>> {
        let conn = self.conn.lock().unwrap();

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
    }

    pub fn get_symbols_by_file(&self, file_id: i64) -> Result<Vec<SymbolRecord>> {
        let conn = self.conn.lock().unwrap();

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
    }

    // Chunk operations

    pub fn insert_chunks(&self, file_id: i64, chunks: &[CodeChunk]) -> Result<()> {
        let conn = self.conn.lock().unwrap();

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

        debug!("Inserted {} chunks for file_id {}", chunks.len(), file_id);
        Ok(())
    }

    pub fn update_chunk_embedding(&self, chunk_id: i64, embedding: &[f32]) -> Result<()> {
        let conn = self.conn.lock().unwrap();

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
    }

    pub fn get_chunks_without_embeddings(&self, limit: usize) -> Result<Vec<ChunkRecord>> {
        let conn = self.conn.lock().unwrap();

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
    }

    // Dependency operations

    pub fn insert_dependency(
        &self,
        source_file_id: i64,
        target_path: &str,
        import_name: Option<&str>,
        kind: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "INSERT INTO dependencies (source_file_id, target_path, import_name, kind)
             VALUES (?1, ?2, ?3, ?4)",
            params![source_file_id, target_path, import_name, kind],
        )?;

        Ok(())
    }

    pub fn get_dependencies(&self, file_id: i64) -> Result<Vec<DependencyRecord>> {
        let conn = self.conn.lock().unwrap();

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
    }

    pub fn get_dependents(&self, target_path: &str) -> Result<Vec<DependencyRecord>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT id, source_file_id, target_path, import_name, kind
             FROM dependencies WHERE target_path LIKE ?1",
        )?;

        let pattern = format!("%{}", target_path);
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
    }

    // Statistics

    pub fn get_stats(&self) -> Result<IndexStats> {
        let conn = self.conn.lock().unwrap();

        let file_count: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
        let symbol_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
        let chunk_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?;
        let dep_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM dependencies", [], |row| row.get(0))?;

        Ok(IndexStats {
            file_count: file_count as usize,
            symbol_count: symbol_count as usize,
            chunk_count: chunk_count as usize,
            dependency_count: dep_count as usize,
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
}
