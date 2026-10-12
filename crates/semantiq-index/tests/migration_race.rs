//! Two processes opening the same old index at once must migrate it once (#88).
//!
//! `migrate_schema_to` read `schema_version` before taking the write lock:
//! the second process waited for the first one's migration, then replayed
//! `ALTER TABLE dependencies ADD COLUMN resolved_path` on the migrated
//! database and failed with `duplicate column name`. The version is now read
//! again under `BEGIN IMMEDIATE`. The scenario is played on a v3 database
//! (the v3 -> v4 step is the non-idempotent one) with a raw connection
//! standing for the first process.

use rusqlite::Connection;
use semantiq_index::schema::migrate_schema_to;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

const V3_SCHEMA: &str = r#"
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
    CREATE TABLE dependencies (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        source_file_id INTEGER NOT NULL,
        target_path TEXT NOT NULL,
        import_name TEXT,
        kind TEXT NOT NULL,
        FOREIGN KEY (source_file_id) REFERENCES files(id) ON DELETE CASCADE
    );
    INSERT INTO metadata (key, value) VALUES ('schema_version', '3');
"#;

fn open(db: &std::path::Path) -> Connection {
    let conn = Connection::open(db).unwrap();
    conn.execute_batch("PRAGMA busy_timeout=5000;").unwrap();
    conn
}

#[test]
fn migration_waiting_for_another_process_does_not_replay_its_steps() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    open(&db).execute_batch(V3_SCHEMA).unwrap();

    // Process A: holds the write lock, then migrates to v4 by hand and commits.
    let barrier = Arc::new(Barrier::new(2));
    let a = {
        let db = db.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            let conn = open(&db);
            conn.execute("BEGIN IMMEDIATE", []).unwrap();
            barrier.wait();
            // Let B read the old version and block on its own BEGIN IMMEDIATE.
            thread::sleep(Duration::from_millis(300));
            conn.execute_batch(
                "ALTER TABLE dependencies ADD COLUMN resolved_path TEXT;
                 CREATE INDEX IF NOT EXISTS idx_deps_resolved ON dependencies(resolved_path);
                 UPDATE metadata SET value = '4' WHERE key = 'schema_version';
                 COMMIT;",
            )
            .unwrap();
        })
    };

    // Process B: opens the index while A holds the lock.
    let conn = open(&db);
    barrier.wait();
    let result = migrate_schema_to(&conn, 4);
    a.join().unwrap();
    result.expect("the second opener must see the migrated version and do nothing");

    let columns: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('dependencies') WHERE name = 'resolved_path'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 1);
    let version: String = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "4");
}
