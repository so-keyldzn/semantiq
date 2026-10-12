//! File operations for IndexStore.

use super::DependencyInsert;
use super::IndexStore;
use crate::schema::FileRecord;
use anyhow::{Context, Result, anyhow};
use rusqlite::Connection;
use rusqlite::{OptionalExtension, params};
use semantiq_parser::{CodeChunk, FileStructure, PARSER_VERSION, Reference, Symbol};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// Everything phase 1 extracted from one file, for
/// [`IndexStore::store_file_index`].
#[derive(Debug, Clone, Copy)]
pub struct FileIndexWrite<'a> {
    /// Project-relative path (`/` separators).
    pub path: &'a str,
    /// `Language::name()`.
    pub language: Option<&'a str>,
    /// The content that was parsed; only its hash is stored.
    pub content: &'a str,
    /// The file's mtime (seconds since the epoch) when it was read.
    pub last_modified: i64,
    /// When the content was read (seconds since the epoch).
    pub indexed_at: i64,
    pub symbols: &'a [Symbol],
    pub references: &'a [Reference],
    pub structure: &'a FileStructure,
    pub chunks: &'a [CodeChunk],
    pub dependencies: &'a [DependencyInsert<'a>],
}

/// What the indexer's walk needs of a `files` row to tell whether the file
/// changed without reading it (see [`IndexStore::load_file_stamps`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStamp {
    pub id: i64,
    /// Content hash, the ground truth.
    pub hash: String,
    /// The file's mtime (seconds) when its content was last hashed.
    pub last_modified: i64,
    /// When the content was last hashed (seconds).
    pub indexed_at: i64,
}

/// How long (seconds) a write after the hash can keep a file's mtime: the
/// coarsest mtime granularity in common use (FAT and exFAT round to 2 s;
/// SMB and some NFS mounts do the same, and a server clock a second behind
/// looks alike). Ext4, APFS and NTFS keep sub-second times, where 1 s would
/// do; the wider window only costs one extra hash of the files modified in
/// the second before an index run.
pub const RACY_WINDOW_SECS: i64 = 2;

impl FileStamp {
    /// Whether a file with this mtime (seconds) can be trusted unchanged
    /// without hashing it. Not when the mtime moved, and not when the file
    /// was modified within [`RACY_WINDOW_SECS`] before its content was
    /// hashed (or later, by a clock skew): a write landing in that window
    /// after the hash can leave the mtime where it is, so such a row is
    /// hashed again, as git's "racy" index entries are.
    pub fn trusts(&self, last_modified: i64) -> bool {
        self.last_modified == last_modified && !self.is_racy()
    }

    /// Whether the mtime of the file is within [`RACY_WINDOW_SECS`] before
    /// the second its content was hashed, or after it.
    pub fn is_racy(&self) -> bool {
        self.last_modified + RACY_WINDOW_SECS > self.indexed_at
    }
}

/// A new stamp for a file whose content was hashed and found unchanged
/// ([`IndexStore::refresh_file_stamps`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStampRefresh {
    pub path: String,
    pub last_modified: i64,
    pub indexed_at: i64,
}

/// Seconds since the UNIX epoch.
pub(crate) fn now_secs() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System time before UNIX epoch")?
        .as_secs() as i64)
}

/// UPSERT keyed on the UNIQUE `path` column, returning the row's id in the
/// same statement.
///
/// `ON CONFLICT ... DO UPDATE` (instead of `INSERT OR REPLACE`) keeps the
/// existing `id` stable across re-indexing: `INSERT OR REPLACE` deletes the
/// old row and inserts a new one, which both reassigns a fresh rowid AND
/// fires the FK `ON DELETE CASCADE` on `chunks`/`symbols` (that cascade also
/// orphans `chunks_vec` rows, sqlite-vec ignoring FK cascades). With a stable
/// id the in-transaction purge of `insert_chunks` is enough to avoid orphans.
///
/// `RETURNING id` makes the upsert and the id lookup one statement:
/// `last_insert_rowid()` is only meaningful on the INSERT branch, and a
/// separate `SELECT` could find the row deleted by another process in
/// between (a concurrent prune or `--force`).
fn upsert_file(
    conn: &Connection,
    path: &str,
    language: Option<&str>,
    hash: &str,
    last_modified: i64,
    indexed_at: i64,
) -> Result<i64> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO files (path, language, hash, last_modified, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(path) DO UPDATE SET
             language = excluded.language,
             hash = excluded.hash,
             last_modified = excluded.last_modified,
             indexed_at = excluded.indexed_at
         RETURNING id",
    )?;
    let id: i64 = stmt.query_row(
        params![path, language, hash, last_modified, indexed_at],
        |row| row.get(0),
    )?;
    debug!("Upserted file {} with id {}", path, id);
    Ok(id)
}

impl IndexStore {
    /// Insert or update a file record, stamped as indexed now.
    pub fn insert_file(
        &self,
        path: &str,
        language: Option<&str>,
        content: &str,
        last_modified: i64,
    ) -> Result<i64> {
        self.insert_file_at(path, language, content, last_modified, now_secs()?)
    }

    /// Insert or update a file record with an explicit `indexed_at`: the
    /// indexer passes the time it read the content (see
    /// [`FileStamp::is_racy`]), which may be well before the write when
    /// files are parsed in parallel.
    pub fn insert_file_at(
        &self,
        path: &str,
        language: Option<&str>,
        content: &str,
        last_modified: i64,
        indexed_at: i64,
    ) -> Result<i64> {
        let hash = Self::hash_content(content);
        self.with_conn(|conn| upsert_file(conn, path, language, &hash, last_modified, indexed_at))
    }

    /// Store everything phase 1 extracted from a file in one transaction:
    /// the `files` row (with the content hash) and its symbols, references,
    /// call edges, type relations, chunks and dependencies. Returns the
    /// stable `file_id`.
    ///
    /// All or nothing: a crash or an error anywhere (a concurrent prune of
    /// the row, `SQLITE_BUSY`, a constraint) rolls the whole file back, so
    /// the row never carries a hash that does not describe its children,
    /// and the previous rows stay in place. `needs_reindex` then sees the
    /// old hash and the file is indexed again. One commit per file instead
    /// of six, with the per-file statements kept in the statement cache.
    pub fn store_file_index(&self, write: &FileIndexWrite<'_>) -> Result<i64> {
        let hash = Self::hash_content(write.content);
        self.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", [])?;
            let result = (|| -> Result<i64> {
                let file_id = upsert_file(
                    conn,
                    write.path,
                    write.language,
                    &hash,
                    write.last_modified,
                    write.indexed_at,
                )?;
                super::symbols::insert_symbols_in(conn, file_id, write.symbols)?;
                super::references::insert_references_in(conn, file_id, write.references)?;
                super::structure::insert_structure_in(conn, file_id, write.structure)?;
                super::chunks::insert_chunks_in(conn, file_id, write.chunks)?;
                super::dependencies::replace_dependencies_in(conn, file_id, write.dependencies)?;
                Ok(file_id)
            })();
            match result {
                Ok(file_id) => {
                    conn.execute("COMMIT", [])?;
                    debug!(
                        "Stored {}: {} symbols, {} refs, {} chunks, {} deps",
                        write.path,
                        write.symbols.len(),
                        write.references.len(),
                        write.chunks.len(),
                        write.dependencies.len()
                    );
                    Ok(file_id)
                }
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", []);
                    Err(e)
                }
            }
        })
    }

    /// The change-detection stamp of every indexed file, keyed by path, in
    /// one query: the indexer's walk looks files up here instead of running
    /// one `SELECT` per file.
    pub fn load_file_stamps(&self) -> Result<HashMap<String, FileStamp>> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT id, path, hash, last_modified, indexed_at FROM files")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    FileStamp {
                        id: row.get(0)?,
                        hash: row.get(2)?,
                        last_modified: row.get(3)?,
                        indexed_at: row.get(4)?,
                    },
                ))
            })?;
            let mut stamps = HashMap::new();
            for row in rows {
                let (path, stamp) = row?;
                stamps.insert(path, stamp);
            }
            Ok(stamps)
        })
    }

    /// Re-stamp files whose content was hashed and found unchanged although
    /// their stamp could not tell (`last_modified` moved, or the row was
    /// racy): the next walk trusts their mtime again. One transaction for
    /// the whole batch; a path that is gone meanwhile is left alone.
    pub fn refresh_file_stamps(&self, stamps: &[FileStampRefresh]) -> Result<()> {
        if stamps.is_empty() {
            return Ok(());
        }
        self.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", [])?;
            let result = (|| -> Result<()> {
                let mut stmt = conn.prepare_cached(
                    "UPDATE files SET last_modified = ?2, indexed_at = ?3 WHERE path = ?1",
                )?;
                for stamp in stamps {
                    stmt.execute(params![stamp.path, stamp.last_modified, stamp.indexed_at])?;
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    conn.execute("COMMIT", [])?;
                    Ok(())
                }
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", []);
                    Err(e)
                }
            }
        })
    }

    /// Get a file record by its path.
    pub fn get_file_by_path(&self, path: &str) -> Result<Option<FileRecord>> {
        self.with_conn(|conn| {
            // Called once per file by the refresh walk: cached statement.
            let mut stmt = conn.prepare_cached(
                "SELECT id, path, language, hash, last_modified, indexed_at
                 FROM files WHERE path = ?1",
            )?;

            let result = stmt
                .query_row([path], |row| {
                    Ok(FileRecord {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        language: row.get(2)?,
                        hash: row.get(3)?,
                        last_modified: row.get(4)?,
                        indexed_at: row.get(5)?,
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
    ///
    /// Both statements run in one transaction. Committing the purge alone (a
    /// crash or `SQLITE_BUSY` before the second statement) would leave chunks
    /// whose `embedding` BLOB is set but whose vector is gone: neither pending
    /// for phase 2 nor searchable, and carried over as "already embedded" by
    /// `insert_chunks` for unchanged content.
    pub fn delete_file(&self, path: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", [])?;
            let result = (|| -> Result<()> {
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
            })();
            match result {
                Ok(()) => {
                    conn.execute("COMMIT", [])?;
                    Ok(())
                }
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", []);
                    Err(e)
                }
            }
        })
    }

    /// Delete the file at `path` and every file under the directory `path`
    /// (`path/...`), with their data, and return how many rows went. The
    /// watcher reports a directory rename or removal as one event on the
    /// directory, so the rows of its files are only reachable by prefix.
    /// Same transaction shape as [`Self::delete_file`]: the `chunks_vec` rows
    /// are purged first, in the same transaction.
    pub fn delete_files_under(&self, path: &str) -> Result<usize> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        self.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", [])?;
            let result = (|| -> Result<usize> {
                // `substr` rather than `LIKE`: `_` and `%` in a path are
                // literal characters here.
                conn.execute(
                    "DELETE FROM chunks_vec
                     WHERE chunk_id IN (
                         SELECT c.id FROM chunks c
                         JOIN files f ON f.id = c.file_id
                         WHERE f.path = ?1 OR substr(f.path, 1, length(?2)) = ?2
                     )",
                    params![path, prefix],
                )?;
                let removed = conn.execute(
                    "DELETE FROM files WHERE path = ?1 OR substr(path, 1, length(?2)) = ?2",
                    params![path, prefix],
                )?;
                Ok(removed)
            })();
            match result {
                Ok(removed) => {
                    conn.execute("COMMIT", [])?;
                    Ok(removed)
                }
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", []);
                    Err(e)
                }
            }
        })
    }

    /// The relative paths of the indexed files under the directory `dir`
    /// (`dir/...`), or every path when `dir` is empty.
    pub fn list_file_paths_under(&self, dir: &str) -> Result<Vec<String>> {
        if dir.is_empty() {
            return self.list_file_paths();
        }
        let prefix = format!("{}/", dir.trim_end_matches('/'));
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT path FROM files WHERE substr(path, 1, length(?1)) = ?1")?;
            let paths = stmt
                .query_map([&prefix], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok(paths)
        })
    }

    /// List the relative paths of every indexed file.
    pub fn list_file_paths(&self) -> Result<Vec<String>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT path FROM files")?;
            let paths = stmt
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok(paths)
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

    // Project root

    /// Record the project root this index is built from (`metadata` key
    /// `project_root`, absolute and canonical when the directory exists).
    /// The query commands derive the root from it when only `--database` is
    /// given, so a refresh never walks the wrong directory.
    pub fn set_project_root(&self, root: &Path) -> Result<()> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES ('project_root', ?1)",
                [root.to_string_lossy().as_ref()],
            )?;
            Ok(())
        })
    }

    /// The project root recorded by [`Self::set_project_root`], if any (an
    /// index built by an older version has none).
    pub fn get_project_root(&self) -> Result<Option<PathBuf>> {
        self.with_conn(|conn| {
            let root: Option<String> = conn
                .query_row(
                    "SELECT value FROM metadata WHERE key = 'project_root'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(root.map(PathBuf::from))
        })
    }

    // Parser version management

    /// Check if a full re-index is needed (parser version changed). Test
    /// support only: production goes through `check_and_prepare_for_reindex`.
    #[cfg(any(test, feature = "test-support"))]
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

    /// Update the parser version in metadata. Test support only: production
    /// stamps it inside `check_and_prepare_for_reindex`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_parser_version(&self) -> Result<()> {
        self.with_conn(Self::set_parser_version_impl)
    }

    /// Overwrite one `metadata` row. Test support only: lets a test play an
    /// index left by an older parser (`parser_version`) or by another
    /// embedding model (`embedding_model`, `embedding_dim`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
            Ok(())
        })
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

    /// Internal implementation: one transaction, rolled back on error so a
    /// failing `DELETE` (I/O error, `SQLITE_FULL`, a busy checkpoint) never
    /// leaves the shared connection inside an open transaction, where every
    /// later `BEGIN IMMEDIATE` would fail until restart (#139).
    pub(crate) fn clear_all_data_impl(conn: &Connection) -> Result<()> {
        conn.execute("BEGIN IMMEDIATE", [])?;
        let result = conn.execute_batch(
            "DELETE FROM chunks_vec;
             DELETE FROM refs;
             DELETE FROM names;
             DELETE FROM call_edges;
             DELETE FROM type_relations;
             DELETE FROM dependencies;
             DELETE FROM chunks;
             DELETE FROM symbols;
             DELETE FROM files;",
        );
        match result {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
                debug!("Cleared all indexed data");
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e.into())
            }
        }
    }

    /// Check whether the parser version changed and, if so, prepare the full
    /// re-index phase 1 is about to run: the structural tables (symbols,
    /// references, call edges, type relations, imports) are cleared and every
    /// file's hash is reset so the next walk re-extracts it; the version is
    /// stamped. Returns true when that happened.
    ///
    /// Files and chunks stay, embeddings included: `insert_chunks` carries
    /// the embedding of a chunk whose content did not change over to the
    /// re-extracted chunk, so a parser bump no longer redoes phase 2 (the
    /// most expensive part) for code that did not move (#86). Files that
    /// vanished meanwhile are pruned by the walk as usual.
    pub fn check_and_prepare_for_reindex(&self) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e: PoisonError<MutexGuard<Connection>>| {
                anyhow!("Database lock poisoned: {}", e)
            })?;

        // Single transaction for check + clear + set version. The version is
        // read under the lock: two processes opening the same index together
        // (`serve` and a CLI command) prepare it once, not twice.
        conn.execute("BEGIN IMMEDIATE", [])?;

        let result = (|| -> Result<bool> {
            if !Self::needs_full_reindex_impl(&conn)? {
                return Ok(false);
            }
            conn.execute_batch(
                "DELETE FROM refs;
                 DELETE FROM names;
                 DELETE FROM call_edges;
                 DELETE FROM type_relations;
                 DELETE FROM dependencies;
                 DELETE FROM symbols;
                 UPDATE files SET hash = '';",
            )?;
            Self::set_parser_version_impl(&conn)?;
            Ok(true)
        })();

        match result {
            Ok(prepared) => {
                conn.execute("COMMIT", [])?;
                if prepared {
                    info!(
                        "Parser version changed - structure cleared for a full reindex (embeddings kept)"
                    );
                }
                Ok(prepared)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `delete_files_under` removes the path itself and the rows under it
    /// as a directory, nothing with the same prefix as a name, and `_` or
    /// `%` in a path are literal.
    #[test]
    fn delete_files_under_removes_the_directory_rows_only() {
        let store = IndexStore::open_in_memory().unwrap();
        for path in [
            "src/old",
            "src/old/a.rs",
            "src/old/deep/b.rs",
            "src/older/c.rs",
            "src/old_x/d.rs",
            "src/main.rs",
        ] {
            store.insert_file(path, Some("rust"), path, 0).unwrap();
        }
        assert_eq!(store.delete_files_under("src/old").unwrap(), 3);
        let mut left = store.list_file_paths().unwrap();
        left.sort();
        assert_eq!(left, ["src/main.rs", "src/old_x/d.rs", "src/older/c.rs"]);
        assert_eq!(store.delete_files_under("src/old").unwrap(), 0);
        assert_eq!(store.delete_files_under("src/main.rs").unwrap(), 1);

        let mut under = store.list_file_paths_under("src").unwrap();
        under.sort();
        assert_eq!(under, ["src/old_x/d.rs", "src/older/c.rs"]);
        assert!(store.list_file_paths_under("src/old").unwrap().is_empty());
    }
}
