//! File operations for IndexStore.

use super::IndexStore;
use crate::schema::FileRecord;
use anyhow::{Context, Result, anyhow};
use rusqlite::Connection;
use rusqlite::{OptionalExtension, params};
use semantiq_parser::PARSER_VERSION;
use std::sync::{MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

impl IndexStore {
    /// Insert or update a file record.
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
            // UPSERT keyed on the UNIQUE `path` column. Using ON CONFLICT ... DO
            // UPDATE (instead of INSERT OR REPLACE) keeps the existing `id` stable
            // across re-indexing: INSERT OR REPLACE deletes the old row and inserts
            // a new one, which both reassigns a fresh rowid AND fires the FK
            // ON DELETE CASCADE on `chunks`/`symbols` — that cascade also orphans
            // `chunks_vec` rows (sqlite-vec ignores FK cascade). With a stable id,
            // the file_id no longer changes on re-index, so the in-transaction
            // purge in `insert_chunks(file_id)` is sufficient to avoid orphans.
            conn.execute(
                "INSERT INTO files (path, language, hash, size, last_modified, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(path) DO UPDATE SET
                     language = excluded.language,
                     hash = excluded.hash,
                     size = excluded.size,
                     last_modified = excluded.last_modified,
                     indexed_at = excluded.indexed_at",
                params![path, language, hash, size, last_modified, indexed_at],
            )?;

            // last_insert_rowid() is only meaningful for the INSERT branch; on the
            // UPDATE branch it is not set to the conflicting row's id. Always read
            // the stable id back by path.
            let id: i64 =
                conn.query_row("SELECT id FROM files WHERE path = ?1", [path], |row| {
                    row.get(0)
                })?;
            debug!("Upserted file {} with id {}", path, id);
            Ok(id)
        })
    }

    /// Get a file record by its path.
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

    /// Check if a file needs to be re-indexed based on content hash.
    pub fn needs_reindex(&self, path: &str, content: &str) -> Result<bool> {
        if let Some(file) = self.get_file_by_path(path)? {
            let current_hash = Self::hash_content(content);
            Ok(file.hash != current_hash)
        } else {
            Ok(true)
        }
    }

    /// Delete a file and its associated data (cascades to symbols, chunks, deps).
    /// Also purges the corresponding `chunks_vec` rows: sqlite-vec virtual tables
    /// don't honor FK / ON DELETE CASCADE, so without this purge old embeddings
    /// linger as orphans and pollute KNN results.
    pub fn delete_file(&self, path: &str) -> Result<()> {
        self.with_conn(|conn| {
            // Purge vec0 rows BEFORE the FK cascade fires — once `files` is
            // deleted the dependent `chunks` rows are gone and the subquery
            // would see nothing.
            conn.execute(
                "DELETE FROM chunks_vec
                 WHERE chunk_id IN (
                     SELECT c.id FROM chunks c
                     JOIN files f ON f.id = c.file_id
                     WHERE f.path = ?1
                 )",
                [path],
            )?;

            conn.execute("DELETE FROM files WHERE path = ?1", [path])?;
            Ok(())
        })
    }

    /// Get a file path by its ID.
    pub fn get_file_path_by_id(&self, file_id: i64) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let result = conn
                .query_row("SELECT path FROM files WHERE id = ?1", [file_id], |row| {
                    row.get(0)
                })
                .optional()?;
            Ok(result)
        })
    }

    /// Get the language associated with a file by its ID.
    pub fn get_file_language(&self, file_id: i64) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let result = conn
                .query_row(
                    "SELECT language FROM files WHERE id = ?1",
                    [file_id],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(result)
        })
    }

    // Parser version management

    /// Check if a full re-index is needed (parser version changed).
    pub fn needs_full_reindex(&self) -> Result<bool> {
        self.with_conn(Self::needs_full_reindex_impl)
    }

    /// Internal implementation for use within a transaction.
    pub(crate) fn needs_full_reindex_impl(conn: &Connection) -> Result<bool> {
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
                        warn!(
                            "Corrupted parser_version in metadata: '{}', forcing reindex",
                            v
                        );
                        0
                    }
                };
                Ok(stored != PARSER_VERSION)
            }
            None => Ok(true), // No version stored = needs reindex
        }
    }

    /// Update the parser version in metadata.
    pub fn set_parser_version(&self) -> Result<()> {
        self.with_conn(Self::set_parser_version_impl)
    }

    /// Internal implementation for use within a transaction.
    pub(crate) fn set_parser_version_impl(conn: &Connection) -> Result<()> {
        conn.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES ('parser_version', ?1)",
            [PARSER_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Clear all indexed data (files, symbols, chunks, dependencies).
    pub fn clear_all_data(&self) -> Result<()> {
        self.with_conn(Self::clear_all_data_impl)
    }

    /// Internal implementation for use within a transaction.
    pub(crate) fn clear_all_data_impl(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             DELETE FROM chunks_vec;
             DELETE FROM dependencies;
             DELETE FROM chunks;
             DELETE FROM symbols;
             DELETE FROM files;
             COMMIT;",
        )?;
        debug!("Cleared all indexed data");
        Ok(())
    }

    /// Check if re-index is needed and prepare (clear data, set version).
    /// Returns true if a full re-index is required.
    pub fn check_and_prepare_for_reindex(&self) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e: PoisonError<MutexGuard<Connection>>| {
                anyhow!("Database lock poisoned: {}", e)
            })?;

        if !Self::needs_full_reindex_impl(&conn)? {
            return Ok(false);
        }

        // Single transaction for check + clear + set version
        conn.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| -> Result<()> {
            conn.execute_batch(
                "DELETE FROM chunks_vec;
                 DELETE FROM dependencies;
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
}
