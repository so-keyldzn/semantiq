//! Integration tests for the v4 -> v5 schema migration.
//!
//! v5 introduced a cleanup pass that fixes residue left by two older bugs:
//!   (a) orphan rows in the sqlite-vec `chunks_vec` table (no FK CASCADE), and
//!   (b) ghost `files` rows stored under an *absolute* path (the pre-`paths.rs`
//!       `strip_prefix(root).unwrap_or(path)` fallthrough).
//!
//! These tests seed a hand-built v4 database containing both kinds of residue,
//! run `migrate_schema`, and assert the cleanup + version bump + idempotence.
//!
//! Why a raw `rusqlite::Connection` instead of `IndexStore`: the public store
//! API always opens at the *current* schema version, so it can't represent a
//! genuine v4-on-disk state with pre-existing orphans. We build the v4 schema
//! by hand. `chunks_vec` (a vec0 virtual table) needs the sqlite-vec extension
//! registered first; constructing any `IndexStore` registers it process-wide
//! via SQLite's auto-extension mechanism, which then applies to every
//! subsequently opened connection in this process.

use rusqlite::Connection;
use semantiq_index::IndexStore;
use semantiq_index::schema::{SCHEMA_VERSION, migrate_schema};

/// The DDL of a v4 database. v4 == v3 + `dependencies.resolved_path`, so the
/// `dependencies` table here already has that column. `chunks_vec` is the vec0
/// virtual table that holds embeddings.
const V4_SCHEMA: &str = r#"
    CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);

    CREATE TABLE files (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        path TEXT NOT NULL UNIQUE,
        language TEXT,
        hash TEXT NOT NULL,
        size INTEGER NOT NULL,
        last_modified INTEGER NOT NULL,
        indexed_at INTEGER NOT NULL
    );

    CREATE TABLE symbols (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        file_id INTEGER NOT NULL,
        name TEXT NOT NULL,
        kind TEXT NOT NULL,
        start_line INTEGER NOT NULL,
        end_line INTEGER NOT NULL,
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        signature TEXT,
        doc_comment TEXT,
        parent TEXT,
        FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
    );

    CREATE TABLE chunks (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        file_id INTEGER NOT NULL,
        content TEXT NOT NULL,
        start_line INTEGER NOT NULL,
        end_line INTEGER NOT NULL,
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        symbols_json TEXT,
        embedding BLOB,
        FOREIGN KEY (file_id) REFERENCES files(id) ON DELETE CASCADE
    );

    CREATE TABLE dependencies (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        source_file_id INTEGER NOT NULL,
        target_path TEXT NOT NULL,
        import_name TEXT,
        kind TEXT NOT NULL,
        resolved_path TEXT,
        FOREIGN KEY (source_file_id) REFERENCES files(id) ON DELETE CASCADE
    );
"#;

/// Bootstrap the sqlite-vec extension once, process-wide, by constructing any
/// `IndexStore`. After this returns, every freshly opened connection in this
/// process can create / use vec0 virtual tables.
fn register_sqlite_vec() {
    let _ = IndexStore::open_in_memory().expect("bootstrap IndexStore registers sqlite-vec");
}

/// A 384-d embedding blob (little-endian f32) for the vec0 table.
fn embedding_blob(seed: f32) -> Vec<u8> {
    (0..384)
        .flat_map(|i| (seed + i as f32 * 0.001).to_le_bytes())
        .collect()
}

/// Build a v4 database in memory with:
///   - a "good" relative-path file with one chunk + its vec row,
///   - a ghost absolute-path file with one chunk + its vec row,
///   - one pure orphan vec row (chunk_id that never existed in `chunks`),
///   - schema_version == '4'.
fn seed_v4_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    conn.execute_batch(V4_SCHEMA).unwrap();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE chunks_vec USING vec0(
            chunk_id INTEGER PRIMARY KEY,
            embedding float[384]
        );",
    )
    .unwrap();

    conn.execute(
        "INSERT INTO metadata (key, value) VALUES ('schema_version', '4')",
        [],
    )
    .unwrap();

    // Good file (relative path) -> file_id 1, chunk_id 1.
    conn.execute(
        "INSERT INTO files (path, language, hash, size, last_modified, indexed_at)
         VALUES ('src/good.rs', 'rust', 'h1', 10, 1000, 2000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chunks (file_id, content, start_line, end_line, start_byte, end_byte, symbols_json)
         VALUES (1, 'fn good() {}', 1, 1, 0, 12, '[]')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chunks_vec(chunk_id, embedding) VALUES (1, ?1)",
        [embedding_blob(0.1)],
    )
    .unwrap();

    // Ghost file with an ABSOLUTE path -> file_id 2, chunk_id 2.
    conn.execute(
        "INSERT INTO files (path, language, hash, size, last_modified, indexed_at)
         VALUES ('/abs/ghost.rs', 'rust', 'h2', 10, 1000, 2000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chunks (file_id, content, start_line, end_line, start_byte, end_byte, symbols_json)
         VALUES (2, 'fn ghost() {}', 1, 1, 0, 13, '[]')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chunks_vec(chunk_id, embedding) VALUES (2, ?1)",
        [embedding_blob(0.2)],
    )
    .unwrap();

    // A pure orphan vec row: chunk_id 999 has no matching `chunks` row.
    conn.execute(
        "INSERT INTO chunks_vec(chunk_id, embedding) VALUES (999, ?1)",
        [embedding_blob(0.3)],
    )
    .unwrap();

    conn
}

fn stored_version(conn: &Connection) -> String {
    conn.query_row(
        "SELECT value FROM metadata WHERE key = 'schema_version'",
        [],
        |row| row.get::<_, String>(0),
    )
    .unwrap()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap()
}

fn orphan_vec_count(conn: &Connection) -> i64 {
    count(
        conn,
        "SELECT COUNT(*) FROM chunks_vec v
         LEFT JOIN chunks c ON c.id = v.chunk_id
         WHERE c.id IS NULL",
    )
}

#[test]
fn migrate_v4_to_v5_purges_orphans_and_ghost_files() {
    register_sqlite_vec();
    let conn = seed_v4_db();

    // Pre-conditions: 2 files (one absolute), 2 chunks, 3 vec rows (1 pure
    // orphan + 1 that belongs to the ghost file).
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 2);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM chunks"), 2);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM chunks_vec"), 3);
    assert!(
        orphan_vec_count(&conn) >= 1,
        "seed should have an orphan vec row"
    );

    migrate_schema(&conn).unwrap();

    // The absolute-path ghost file is gone; only the relative file remains.
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM files"),
        1,
        "ghost absolute-path file must be deleted"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM files WHERE path = 'src/good.rs'"
        ),
        1,
        "the good relative-path file must survive"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM files WHERE substr(path,1,1) = '/'"
        ),
        0,
        "no absolute paths may remain"
    );

    // FK CASCADE removed the ghost file's chunk; only the good chunk remains.
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM chunks"), 1);

    // Every orphan vec row (pure + ghost-derived) is purged. Only chunk_id 1's
    // vector survives.
    assert_eq!(orphan_vec_count(&conn), 0, "all orphan vec rows purged");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM chunks_vec"), 1);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunks_vec WHERE chunk_id = 1"),
        1,
        "the good chunk's vector must survive"
    );

    // Version bumped to current.
    assert_eq!(stored_version(&conn), SCHEMA_VERSION.to_string());
}

#[test]
fn migrate_v4_to_v5_is_idempotent() {
    register_sqlite_vec();
    let conn = seed_v4_db();

    migrate_schema(&conn).unwrap();
    let files_after_first = count(&conn, "SELECT COUNT(*) FROM files");
    let chunks_after_first = count(&conn, "SELECT COUNT(*) FROM chunks");
    let vecs_after_first = count(&conn, "SELECT COUNT(*) FROM chunks_vec");

    // Re-running migrate on an already-v5 database is a no-op (stored >= target
    // short-circuits) and must not corrupt or further mutate any rows.
    migrate_schema(&conn).unwrap();
    migrate_schema(&conn).unwrap();

    assert_eq!(stored_version(&conn), SCHEMA_VERSION.to_string());
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM files"),
        files_after_first
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunks"),
        chunks_after_first
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM chunks_vec"),
        vecs_after_first
    );
    assert_eq!(orphan_vec_count(&conn), 0);
}
