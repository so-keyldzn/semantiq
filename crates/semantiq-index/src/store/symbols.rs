//! Symbol operations for IndexStore.

use super::IndexStore;
use crate::schema::SymbolRecord;
use anyhow::{Result, anyhow};
use rusqlite::Connection;
use rusqlite::params;
use semantiq_parser::Symbol;
use std::collections::HashMap;
use std::sync::{MutexGuard, PoisonError};
use tracing::debug;

impl IndexStore {
    /// Maximum limit for symbol search results to prevent excessive memory usage.
    const MAX_SYMBOL_SEARCH_LIMIT: usize = 10000;

    /// Insert symbols for a file (replaces existing symbols for that file).
    pub fn insert_symbols(&self, file_id: i64, symbols: &[Symbol]) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e: PoisonError<MutexGuard<Connection>>| {
                anyhow!("Database lock poisoned: {}", e)
            })?;

        // Use a transaction for atomicity
        conn.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| -> Result<()> {
            // Delete existing symbols for this file
            conn.execute("DELETE FROM symbols WHERE file_id = ?1", [file_id])?;

            let mut stmt = conn.prepare(
                "INSERT INTO symbols (file_id, name, kind, start_line, end_line, signature, doc_comment, parent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;

            for symbol in symbols {
                stmt.execute(params![
                    file_id,
                    symbol.name,
                    symbol.kind.as_str(),
                    symbol.start_line as i64,
                    symbol.end_line as i64,
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

    /// Search symbols using FTS5 full-text search.
    pub fn search_symbols(&self, query: &str, limit: usize) -> Result<Vec<SymbolRecord>> {
        // Cap limit to prevent excessive memory usage
        let safe_limit = limit.min(Self::MAX_SYMBOL_SEARCH_LIMIT);

        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT s.id, s.file_id, s.name, s.kind, s.start_line, s.end_line,
                        s.signature, s.doc_comment, s.parent
                 FROM symbols s
                 JOIN symbols_fts ON s.id = symbols_fts.rowid
                 WHERE symbols_fts MATCH ?1
                 ORDER BY symbols_fts.rank
                 LIMIT ?2",
            )?;

            let fts_query = Self::escape_fts5_query(query);
            let results = stmt
                .query_map(params![fts_query, safe_limit as i64], |row| {
                    Ok(SymbolRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        name: row.get(2)?,
                        kind: row.get(3)?,
                        start_line: row.get(4)?,
                        end_line: row.get(5)?,
                        signature: row.get(6)?,
                        doc_comment: row.get(7)?,
                        parent: row.get(8)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Find symbols by exact name match.
    pub fn find_symbol_by_name(&self, name: &str) -> Result<Vec<SymbolRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, file_id, name, kind, start_line, end_line,
                        signature, doc_comment, parent
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
                        signature: row.get(6)?,
                        doc_comment: row.get(7)?,
                        parent: row.get(8)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Get all symbols for a file, ordered by start line.
    pub fn get_symbols_by_file(&self, file_id: i64) -> Result<Vec<SymbolRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, file_id, name, kind, start_line, end_line,
                        signature, doc_comment, parent
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
                        signature: row.get(6)?,
                        doc_comment: row.get(7)?,
                        parent: row.get(8)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }
}

impl IndexStore {
    /// `name -> kind` of the symbols of each of `file_ids`, in one query
    /// (per 900 ids). On duplicate names in a file the last one by line wins:
    /// enough to label a chunk by its first symbol.
    pub fn get_symbol_kinds_by_files(
        &self,
        file_ids: &[i64],
    ) -> Result<HashMap<i64, HashMap<String, String>>> {
        let mut kinds: HashMap<i64, HashMap<String, String>> = HashMap::new();
        for batch in file_ids.chunks(super::chunks::SQL_IN_BATCH) {
            let placeholders: Vec<&str> = batch.iter().map(|_| "?").collect();
            let query = format!(
                "SELECT file_id, name, kind FROM symbols
                 WHERE file_id IN ({})
                 ORDER BY file_id, start_line",
                placeholders.join(",")
            );
            self.with_conn(|conn| {
                let mut stmt = conn.prepare(&query)?;
                let rows = stmt.query_map(rusqlite::params_from_iter(batch.iter()), |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
                for row in rows {
                    let (file_id, name, kind) = row?;
                    kinds.entry(file_id).or_default().insert(name, kind);
                }
                Ok(())
            })?;
        }
        Ok(kinds)
    }
}
