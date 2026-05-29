//! Integration tests for `AutoIndexer` against a real on-disk toy project.
//!
//! These exercise the public `AutoIndexer` API (`new`, `initial_index`,
//! `process_events`) end-to-end through a real `IndexStore`, and pin the two
//! invariants that matter most for correctness:
//!   1. after a file is (re)indexed, `chunks_vec` has zero orphan rows, and
//!   2. the stored data reflects the *current* file content.
//!
//! Embeddings use the default stub model (the `onnx` feature is off in tests),
//! which still writes a `chunks_vec` row per chunk — so the orphan invariant is
//! genuinely exercised on reindex even without a real model.

use semantiq_index::{AutoIndexer, IndexStore};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

/// Write a file, creating parent dirs as needed.
fn write_file(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn build_toy_project(root: &Path) {
    write_file(
        root,
        "src/lib.rs",
        "pub fn alpha() -> i32 { 1 }\n\npub fn beta() -> i32 { 2 }\n",
    );
    write_file(
        root,
        "src/util.rs",
        "pub fn helper(x: i32) -> i32 { x + 1 }\n",
    );
    // A non-source file that must be ignored by the language filter.
    write_file(root, "README.md", "# toy\n");
}

#[test]
fn initial_index_populates_store_without_orphans() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::new(Arc::clone(&store), root.clone()).unwrap();

    let result = indexer.initial_index().unwrap();

    // Both .rs files indexed; README.md scanned but skipped (unsupported lang).
    assert!(result.scanned >= 2, "should scan the project files");
    assert_eq!(result.indexed, 2, "both Rust files should be indexed");
    assert_eq!(result.errors, 0);

    // Files are stored under RELATIVE paths (never absolute).
    assert!(store.get_file_by_path("src/lib.rs").unwrap().is_some());
    assert!(store.get_file_by_path("src/util.rs").unwrap().is_some());
    assert!(
        store.get_file_by_path("README.md").unwrap().is_none(),
        "unsupported files must not be indexed"
    );

    // Symbols reflect the current content.
    assert!(!store.find_symbol_by_name("alpha").unwrap().is_empty());
    assert!(!store.find_symbol_by_name("beta").unwrap().is_empty());
    assert!(!store.find_symbol_by_name("helper").unwrap().is_empty());

    // The core invariant: no orphan vectors.
    assert_eq!(
        store.count_orphan_chunk_vectors().unwrap(),
        0,
        "no orphan chunk vectors after initial index"
    );
}

#[test]
fn reindex_after_modification_updates_content_and_keeps_invariant() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::new(Arc::clone(&store), root.clone()).unwrap();

    // First pass.
    indexer.initial_index().unwrap();
    let file_id = store
        .get_file_by_path("src/lib.rs")
        .unwrap()
        .expect("lib.rs indexed")
        .id;
    assert!(!store.find_symbol_by_name("alpha").unwrap().is_empty());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);

    // Modify the file: rename `alpha` -> `gamma`, drop `beta`.
    write_file(&root, "src/lib.rs", "pub fn gamma() -> i32 { 42 }\n");

    // A fresh `initial_index` detects the content change (hash differs) and
    // reindexes only the modified file. This is the same `index_file` path the
    // watcher drives, but deterministic (no OS-event timing).
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.indexed, 1, "only the modified file is reindexed");
    assert_eq!(result.skipped, 1, "unchanged util.rs is skipped");
    assert_eq!(result.errors, 0);

    // The file row id is STABLE across reindex (HIGH-1 contract): re-inserting
    // the same path must reuse the existing row rather than mint a new one.
    let file_id_after = store
        .get_file_by_path("src/lib.rs")
        .unwrap()
        .expect("lib.rs still indexed")
        .id;
    assert_eq!(
        file_id, file_id_after,
        "reindex must keep a stable file id for the same path"
    );

    // Content is up to date: new symbol present, old ones gone.
    assert!(
        !store.find_symbol_by_name("gamma").unwrap().is_empty(),
        "new symbol `gamma` must be indexed"
    );
    assert!(
        store.find_symbol_by_name("alpha").unwrap().is_empty(),
        "renamed-away symbol `alpha` must be purged"
    );
    assert!(
        store.find_symbol_by_name("beta").unwrap().is_empty(),
        "removed symbol `beta` must be purged"
    );

    // Chunks now reflect the new content only.
    let chunks = store.get_chunks_by_file(file_id_after).unwrap();
    assert!(
        chunks.iter().any(|c| c.content.contains("gamma")),
        "chunk content should reflect the new source"
    );
    assert!(
        chunks.iter().all(|c| !c.content.contains("beta")),
        "no chunk should retain the removed `beta`"
    );

    // The invariant must still hold after the reindex churn.
    assert_eq!(
        store.count_orphan_chunk_vectors().unwrap(),
        0,
        "no orphan chunk vectors after reindex"
    );
}

#[test]
fn second_initial_index_is_a_full_noop_when_unchanged() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::new(Arc::clone(&store), root.clone()).unwrap();

    indexer.initial_index().unwrap();

    // Running again with no file changes must reindex nothing.
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.indexed, 0, "nothing changed -> nothing reindexed");
    assert_eq!(result.skipped, 2, "both files unchanged -> skipped");
    assert_eq!(result.errors, 0);
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

#[test]
fn process_events_with_no_pending_events_is_empty_and_clean() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::new(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();

    // With no buffered watcher events, `process_events` is a clean no-op.
    // (We avoid asserting on real OS file-watch delivery, which is timing- and
    // platform-dependent; this pins the public contract deterministically.)
    let result = indexer.process_events().unwrap();
    assert_eq!(result.indexed, 0);
    assert_eq!(result.removed, 0);
    assert_eq!(result.errors, 0);
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}
