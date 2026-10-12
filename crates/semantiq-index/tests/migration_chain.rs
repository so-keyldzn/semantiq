//! The full migration chain through the production path: a database written
//! by schema v3 (the oldest version `migrate_schema` knows) opened with
//! `IndexStore::open` must land on `SCHEMA_VERSION` and be usable.
//!
//! Each step has its own test (`schema.rs`, `migration_v5.rs`,
//! `migration_v6.rs`), but they stop at the version they introduce
//! (`migrate_schema_to`). This one pins the chain end to end, so a later step
//! that `DROP COLUMN`s or `DELETE FROM`s a table absent from old databases is
//! caught (#122).

use rusqlite::Connection;
use semantiq_index::IndexStore;
use semantiq_index::schema::SCHEMA_VERSION;
use std::path::Path;

/// The DDL of a v3 database (the fixture of `test_migrate_v3_to_v4`):
/// `files.size`, `symbols`/`chunks` byte offsets, `dependencies` without
/// `resolved_path`, no `refs`, `call_edges`, `type_relations`,
/// `distance_observations` or `threshold_calibration` table.
const V3_SCHEMA: &str = r#"
    CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    INSERT INTO metadata (key, value) VALUES ('schema_version', '3');

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
        FOREIGN KEY (source_file_id) REFERENCES files(id) ON DELETE CASCADE
    );

    INSERT INTO files (path, language, hash, size, last_modified, indexed_at)
    VALUES ('src/old.rs', 'rust', 'abc', 10, 1000, 2000);
    INSERT INTO symbols (file_id, name, kind, start_line, end_line, start_byte, end_byte)
    VALUES (1, 'old', 'function', 1, 1, 0, 10);
    INSERT INTO chunks (file_id, content, start_line, end_line, start_byte, end_byte, symbols_json)
    VALUES (1, 'fn old() {}', 1, 1, 0, 11, '[]');
    INSERT INTO dependencies (source_file_id, target_path, import_name, kind)
    VALUES (1, 'crate::utils', 'utils', 'local');
"#;

fn seed_v3_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(V3_SCHEMA).unwrap();
}

fn metadata(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM metadata WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .ok()
}

fn table_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn column_names(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn v3_database_migrates_to_the_current_schema_through_index_store_open() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("old.db");
    seed_v3_db(&db);

    // The production path: migrate_schema() then init_schema().
    let store = IndexStore::open(&db).expect("a v3 database opens");

    // The migrated database is usable through the store API...
    let file_id = store
        .insert_file("src/new.rs", Some("rust"), "fn new() {}", 1000)
        .expect("insert_file works on the migrated database");
    assert!(file_id > 0);
    let counts = store.embedding_counts().unwrap();
    assert_eq!(counts.pending(), counts.total - counts.embedded);
    drop(store);

    // ...and its metadata says it is at the current version, whatever that is.
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        metadata(&conn, "schema_version").as_deref(),
        Some(SCHEMA_VERSION.to_string().as_str())
    );

    // Every table of a fresh database exists (created by init_schema after
    // the migration steps), and the v3-only column is gone.
    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh_db = fresh_dir.path().join("fresh.db");
    drop(IndexStore::open(&fresh_db).unwrap());
    let fresh = Connection::open(&fresh_db).unwrap();
    assert_eq!(
        table_names(&conn),
        table_names(&fresh),
        "tables differ from a fresh database"
    );
    for table in ["files", "symbols", "chunks", "dependencies"] {
        assert_eq!(
            column_names(&conn, table),
            column_names(&fresh, table),
            "columns of `{table}` differ from a fresh database"
        );
    }
    assert!(!column_names(&conn, "files").contains(&"size".to_string()));
}

#[test]
fn migrated_database_reopens_without_changes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("old.db");
    seed_v3_db(&db);

    drop(IndexStore::open(&db).unwrap());
    let conn = Connection::open(&db).unwrap();
    let version_after_first_open = metadata(&conn, "schema_version");
    let tables_after_first_open = table_names(&conn);
    drop(conn);

    // Idempotent: a second open runs no migration and changes nothing.
    let store = IndexStore::open(&db).expect("reopening a migrated database works");
    store
        .insert_file("src/again.rs", Some("rust"), "fn again() {}", 1000)
        .unwrap();
    drop(store);
    let conn = Connection::open(&db).unwrap();
    assert_eq!(metadata(&conn, "schema_version"), version_after_first_open);
    assert_eq!(table_names(&conn), tables_after_first_open);
}
