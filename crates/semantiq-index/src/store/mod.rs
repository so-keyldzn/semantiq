//! SQLite-based storage for the code index.
//!
//! This module provides the `IndexStore` type for storing and querying
//! indexed code data including files, symbols, chunks, and dependencies.

mod calibrations;
mod chunks;
mod dependencies;
mod files;
mod graph;
mod observations;
mod references;
mod structure;
mod symbols;

use crate::schema::{init_schema, migrate_schema};
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, ffi::sqlite3_auto_extension};
use sqlite_vec::sqlite3_vec_init;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

// Re-export types
pub use calibrations::{CalibrationData, CalibrationRecord};
pub use chunks::{ChunkHit, EmbeddingCounts, PendingChunk};
pub use dependencies::DependencyInsert;
pub use graph::{GraphFile, GraphRefCount, MAX_NAME_DEFINERS, RepoGraphData};

/// Global initializer for sqlite-vec extension.
///
/// Uses `Once` to ensure the extension is registered exactly once per process,
/// regardless of how many `IndexStore` instances are created.
static SQLITE_VEC_INIT: Once = Once::new();

/// Registers the sqlite-vec extension with SQLite's auto-extension mechanism.
///
/// # Safety
///
/// This function contains an `unsafe` block that is necessary to interface with
/// the SQLite C API. The safety is guaranteed by the following invariants:
///
/// 1. **Function pointer validity**: `sqlite3_vec_init` is a valid C function
///    exported by the `sqlite-vec` crate with the correct signature expected by
///    `sqlite3_auto_extension`. The function signature is:
///    `extern "C" fn(*mut sqlite3, *mut *mut c_char, *const sqlite3_api_routines) -> c_int`
///
/// 2. **Single initialization**: `Once::call_once` guarantees this code runs
///    exactly once per process, preventing double-registration which could cause
///    undefined behavior.
///
/// 3. **Transmute safety**: The `transmute` converts the function pointer to the
///    opaque type expected by `sqlite3_auto_extension`. This is safe because:
///    - The source type (`*const ()` from `sqlite3_vec_init as *const ()`) and
///      target type are both pointer-sized
///    - SQLite will call the function with the correct calling convention
///    - The sqlite-vec crate guarantees ABI compatibility with SQLite
///
/// 4. **Thread safety**: `sqlite3_auto_extension` is documented as thread-safe
///    by SQLite when called before any database connections are opened.
///
/// # Panics
///
/// This function does not panic. If the extension fails to register, SQLite will
/// return an error when attempting to use vec0 virtual tables.
pub(crate) fn init_sqlite_vec() {
    SQLITE_VEC_INIT.call_once(|| {
        // SAFETY: See function-level documentation for safety invariants.
        // The transmute is required because sqlite3_auto_extension expects an
        // Option<unsafe extern "C" fn()> but sqlite3_vec_init is declared without parameters.
        // SQLite's extension loading mechanism handles the parameter passing correctly.
        // This follows the exact pattern from the sqlite-vec crate's own test code.
        unsafe {
            sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> std::os::raw::c_int,
            >(sqlite3_vec_init as *const ())));
        }
        tracing::debug!("sqlite-vec extension registered");
    });
}

/// The main storage interface for the code index.
pub struct IndexStore {
    pub(crate) conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
}

impl IndexStore {
    /// Apply the connection-level PRAGMAs used by every `IndexStore`.
    ///
    /// Kept in one place so the in-memory test fixture and the on-disk
    /// production database share the exact same connection semantics — most
    /// importantly `foreign_keys=ON`, without which FK `ON DELETE CASCADE`
    /// silently does nothing and tests would pass while production cascades.
    /// `journal_mode=WAL` is a harmless no-op on `:memory:` databases.
    fn apply_pragmas(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;
        Ok(())
    }

    /// Open or create an index database at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        init_sqlite_vec();

        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open database at {:?}", path))?;

        Self::apply_pragmas(&conn)?;

        migrate_schema(&conn)
            .and_then(|()| init_schema(&conn))
            .with_context(|| format!("Cannot open the index at {}", path.display()))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: path.to_path_buf(),
        })
    }

    /// Open an in-memory database (useful for testing).
    pub fn open_in_memory() -> Result<Self> {
        init_sqlite_vec();

        let conn = Connection::open_in_memory()?;
        Self::apply_pragmas(&conn)?;
        migrate_schema(&conn)?;
        init_schema(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path: PathBuf::from(":memory:"),
        })
    }

    /// Get the path to the database file.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Helper function to safely acquire the connection lock with proper error handling.
    pub(crate) fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self
            .conn
            .lock()
            .map_err(|e: PoisonError<MutexGuard<Connection>>| {
                anyhow!("Database lock poisoned: {}", e)
            })?;
        f(&conn)
    }

    /// SQLite's `PRAGMA data_version`: changes whenever another connection
    /// commits to this database, never for this connection's own writes. O(1),
    /// no table access: the cheap way to ask "did another process write?".
    pub fn data_version(&self) -> Result<i64> {
        self.with_conn(|conn| {
            conn.query_row("PRAGMA data_version", [], |row| row.get(0))
                .map_err(Into::into)
        })
    }

    /// Get index statistics.
    pub fn get_stats(&self) -> Result<IndexStats> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT
                    (SELECT COUNT(*) FROM files) as file_count,
                    (SELECT COUNT(*) FROM symbols) as symbol_count,
                    (SELECT COUNT(*) FROM chunks) as chunk_count,
                    (SELECT COUNT(*) FROM dependencies) as dep_count,
                    (SELECT COUNT(*) FROM refs) as ref_count,
                    (SELECT COUNT(*) FROM call_edges) as call_edge_count,
                    (SELECT COUNT(*) FROM type_relations) as type_relation_count",
                [],
                |row| {
                    Ok(IndexStats {
                        file_count: row.get::<_, i64>(0)? as usize,
                        symbol_count: row.get::<_, i64>(1)? as usize,
                        chunk_count: row.get::<_, i64>(2)? as usize,
                        dependency_count: row.get::<_, i64>(3)? as usize,
                        ref_count: row.get::<_, i64>(4)? as usize,
                        call_edge_count: row.get::<_, i64>(5)? as usize,
                        type_relation_count: row.get::<_, i64>(6)? as usize,
                    })
                },
            )
            .map_err(Into::into)
        })
    }

    /// Count rows in `chunks_vec` whose `chunk_id` no longer exists in `chunks`.
    ///
    /// `chunks_vec` is a sqlite-vec virtual table and does not honor FK ON DELETE
    /// CASCADE. Orphan rows here mean stale embeddings will surface in KNN
    /// searches and silently degrade semantic ranking. This count must remain 0;
    /// the application code that mutates `chunks` is responsible for purging the
    /// matching `chunks_vec` rows in the same transaction.
    pub fn count_orphan_chunk_vectors(&self) -> Result<i64> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM chunks_vec v
                 LEFT JOIN chunks c ON c.id = v.chunk_id
                 WHERE c.id IS NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })
    }

    /// Count chunks whose `embedding` BLOB is set but which have no `chunks_vec`
    /// row: the opposite direction of [`Self::count_orphan_chunk_vectors`].
    ///
    /// Such a chunk is invisible to both phase 2 (not pending) and to vector
    /// search (no vector), and `insert_chunks` would carry its BLOB over as
    /// "already embedded". Every write pairing the two tables runs in one
    /// transaction so this count must remain 0.
    pub fn count_chunks_missing_vectors(&self) -> Result<i64> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM chunks c
                 LEFT JOIN chunks_vec v ON v.chunk_id = c.id
                 WHERE c.embedding IS NOT NULL AND v.chunk_id IS NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })
    }

    /// Escapes a query string for safe use with FTS5 MATCH.
    ///
    /// FTS5 has several special characters and operators that need handling:
    /// - `"` (double quote): Used for phrase queries, must be escaped by doubling
    /// - `AND`, `OR`, `NOT`: Boolean operators
    /// - `+`, `-`: Required/excluded term prefixes
    /// - `*`: Wildcard suffix for prefix matching
    /// - `^`: Start anchor
    /// - `NEAR`: Proximity operator
    /// - `(`, `)`: Grouping
    ///
    /// This function wraps the query in double quotes for literal matching,
    /// then appends `*` for prefix search.
    pub(crate) fn escape_fts5_query(query: &str) -> String {
        // Strip null bytes and control characters that could cause unexpected FTS5 behavior
        let cleaned: String = query.chars().filter(|c| !c.is_control()).collect();
        let escaped = cleaned.replace('"', "\"\"");
        format!("\"{}\"*", escaped)
    }

    /// Computes a hash of file content for change detection.
    ///
    /// Uses `DefaultHasher` (currently SipHash 1-3) for fast, deterministic hashing.
    /// This is used solely for change detection during incremental indexing.
    pub(crate) fn hash_content(content: &str) -> String {
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}

/// Statistics about the index.
#[derive(Debug, Clone)]
pub struct IndexStats {
    pub file_count: usize,
    pub symbol_count: usize,
    pub chunk_count: usize,
    pub dependency_count: usize,
    /// Rows of `refs` (identifier occurrences behind `find_refs`, `impact`,
    /// `dead_code` and the repo map).
    pub ref_count: usize,
    /// Rows of `call_edges` (behind `calls`).
    pub call_edge_count: usize,
    /// Rows of `type_relations` (behind `hierarchy`).
    pub type_relation_count: usize,
}

#[cfg(test)]
mod tests;
