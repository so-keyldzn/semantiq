//! Integration tests for `AutoIndexer` against a real on-disk toy project.
//!
//! These exercise the public `AutoIndexer` API (`new`, `initial_index`,
//! `process_events`) end-to-end through a real `IndexStore`, and pin the two
//! invariants that matter most for correctness:
//!   1. after a file is (re)indexed, `chunks_vec` has zero orphan rows, and
//!   2. the stored data reflects the *current* file content.
//!
//! `AutoIndexer` is phase 1 only: chunks are stored without embeddings (phase
//! 2, `embed_pending`, is covered by `two_phase.rs`), and the orphan invariant
//! is checked here on reindex all the same.

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

#[test]
fn initial_index_prunes_files_deleted_while_offline() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::new(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();

    // Simulate a rename that happened while the server was down.
    fs::rename(root.join("src/util.rs"), root.join("src/helpers.rs")).unwrap();

    let result = indexer.initial_index().unwrap();
    assert_eq!(result.removed, 1);
    assert_eq!(result.errors, 0);
    assert!(store.get_file_by_path("src/util.rs").unwrap().is_none());
    assert!(store.get_file_by_path("src/helpers.rs").unwrap().is_some());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

#[test]
fn oversized_data_files_are_skipped_and_stale_rows_removed() {
    use semantiq_index::MAX_DATA_FILE_SIZE;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    write_file(&root, "data/small.json", "{\"name\": \"semantiq\"}\n");

    // A JSON file above the data-file limit, as an older version would have
    // indexed it (simulated by a row inserted directly).
    let mut big = String::from("{\n");
    while big.len() < MAX_DATA_FILE_SIZE as usize + 1024 {
        big.push_str("  \"key_padding_value\": \"0123456789abcdef\",\n");
    }
    big.push_str("  \"end\": 0\n}\n");
    write_file(&root, "data/big.json", &big);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    store
        .insert_file("data/big.json", Some("json"), &big, 0)
        .unwrap();

    let indexer = AutoIndexer::new(Arc::clone(&store), root.clone()).unwrap();
    let result = indexer.initial_index().unwrap();

    assert_eq!(result.errors, 0);
    assert!(
        store.get_file_by_path("data/big.json").unwrap().is_none(),
        "an oversized data file must be dropped from the index"
    );
    assert!(store.get_file_by_path("data/small.json").unwrap().is_some());
    assert!(store.get_file_by_path("src/lib.rs").unwrap().is_some());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

/// #32: the indexer records the project root, and a walk from a directory
/// holding none of the indexed files refuses to prune the index (a database
/// opened from the wrong directory) instead of removing every file.
#[test]
fn initial_index_records_root_and_refuses_to_prune_from_a_wrong_root() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    build_toy_project(&root);
    let store = Arc::new(IndexStore::open_in_memory().unwrap());

    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    assert_eq!(indexer.initial_index().unwrap().indexed, 2);
    assert_eq!(
        store.get_project_root().unwrap(),
        Some(root.canonicalize().unwrap())
    );

    // An empty directory passed as the root: nothing indexable, index populated.
    let wrong = tmp.path().join("elsewhere");
    fs::create_dir_all(&wrong).unwrap();
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), wrong.clone()).unwrap();
    let err = indexer.initial_index().unwrap_err().to_string();
    assert!(err.contains("wrong project root"), "{err}");
    assert_eq!(store.get_stats().unwrap().file_count, 2, "nothing pruned");

    // Back on the real root the index is refreshed in place, and an index with
    // files legitimately removed is still pruned (the walk sees the others).
    fs::remove_file(root.join("src/util.rs")).unwrap();
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.removed, 1);
    assert_eq!(store.get_stats().unwrap().file_count, 1);
    assert_eq!(
        store.get_project_root().unwrap(),
        Some(root.canonicalize().unwrap())
    );
}

// ---------------------------------------------------------------------------
// Walk rules: hidden directories, `.gitignore`, walk errors (#65, #36, #53).
// ---------------------------------------------------------------------------

use semantiq_index::watcher::FileEvent;

/// A project with hidden tooling directories, excluded directories and a
/// `.gitignore`. `.gitignore` is only honoured inside a git repository, so
/// `.git` exists (an empty directory is enough for the `ignore` crate).
fn build_hidden_dirs_project(root: &Path) {
    build_toy_project(root);
    fs::create_dir_all(root.join(".git")).unwrap();
    write_file(root, ".gitignore", "generated/\n*.min.js\n");
    write_file(
        root,
        ".storybook/main.ts",
        "export function storybookMain(): number { return 1; }\n",
    );
    write_file(
        root,
        ".github/workflows/ci.yml",
        "name: ci\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n",
    );
    write_file(root, ".vscode/tasks.json", "{\"version\": \"2.0.0\"}\n");
    write_file(root, ".eslintrc.json", "{\"root\": true}\n");
    write_file(
        root,
        "generated/schema.ts",
        "export function generatedSchema(): number { return 2; }\n",
    );
    write_file(root, "app.min.js", "function minified(){return 3}\n");
    write_file(root, ".idea/workspace.xml", "<project/>\n");
    write_file(
        root,
        ".cache/stale.ts",
        "export function cachedThing(): number { return 4; }\n",
    );
    write_file(root, "node_modules/dep/index.js", "module.exports = 1;\n");
}

#[test]
fn hidden_tooling_dirs_are_indexed_and_excluded_dirs_are_not() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_hidden_dirs_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);

    let mut indexed = store.list_file_paths().unwrap();
    indexed.sort();
    assert_eq!(
        indexed,
        [
            ".eslintrc.json",
            ".github/workflows/ci.yml",
            ".storybook/main.ts",
            ".vscode/tasks.json",
            "src/lib.rs",
            "src/util.rs",
        ],
        "hidden tooling directories are indexed; .gitignore, VCS, cache, IDE \
         and dependency directories are not"
    );
    assert!(
        !store
            .find_symbol_by_name("storybookMain")
            .unwrap()
            .is_empty(),
        "a function in .storybook must be a symbol"
    );
    assert!(
        store
            .find_symbol_by_name("generatedSchema")
            .unwrap()
            .is_empty()
    );
    assert!(store.find_symbol_by_name("cachedThing").unwrap().is_empty());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

#[test]
fn watcher_events_follow_gitignore_and_excluded_names() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_hidden_dirs_project(&root);
    // A nested `.gitignore`, as monorepos have.
    write_file(&root, "packages/.gitignore", "dist-types/\n");
    write_file(
        &root,
        "packages/dist-types/index.ts",
        "export function distTypes(): number { return 5; }\n",
    );

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    let before: Vec<String> = store.list_file_paths().unwrap();

    // Saves of ignored or excluded files, as the watcher reports them.
    let result = indexer.process_file_events(vec![
        FileEvent::Modified(root.join("generated/schema.ts")),
        FileEvent::Created(root.join("app.min.js")),
        FileEvent::Modified(root.join("packages/dist-types/index.ts")),
        FileEvent::Modified(root.join(".cache/stale.ts")),
        FileEvent::Modified(root.join("node_modules/dep/index.js")),
    ]);
    assert_eq!(
        result.indexed, 0,
        "ignored files must not be indexed on save"
    );
    assert_eq!(result.removed, 0);
    assert_eq!(result.errors, 0);
    let mut after = store.list_file_paths().unwrap();
    after.sort();
    let mut expected = before.clone();
    expected.sort();
    assert_eq!(after, expected);
    assert!(
        store
            .find_symbol_by_name("generatedSchema")
            .unwrap()
            .is_empty()
    );
    assert!(store.find_symbol_by_name("distTypes").unwrap().is_empty());

    // Saves of project files, hidden directory included, are indexed.
    write_file(
        &root,
        ".storybook/main.ts",
        "export function storybookRenamed(): number { return 1; }\n",
    );
    write_file(&root, "src/new.rs", "pub fn brand_new() -> i32 { 6 }\n");
    let result = indexer.process_file_events(vec![
        FileEvent::Modified(root.join(".storybook/main.ts")),
        FileEvent::Created(root.join("src/new.rs")),
    ]);
    assert_eq!(result.indexed, 2);
    assert_eq!(result.errors, 0);
    assert!(
        !store
            .find_symbol_by_name("storybookRenamed")
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .find_symbol_by_name("storybookMain")
            .unwrap()
            .is_empty()
    );
    assert!(!store.find_symbol_by_name("brand_new").unwrap().is_empty());

    // A file that became ignored since it was indexed loses its row on save.
    write_file(&root, ".gitignore", "generated/\n*.min.js\nsrc/new.rs\n");
    let result = indexer.process_file_events(vec![FileEvent::Modified(root.join("src/new.rs"))]);
    assert_eq!(result.indexed, 0);
    assert_eq!(result.removed, 1);
    assert!(store.get_file_by_path("src/new.rs").unwrap().is_none());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

#[test]
fn unreadable_file_loses_its_row() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    assert!(store.get_file_by_path("src/util.rs").unwrap().is_some());

    // The file is no longer valid UTF-8: `read_to_string` fails.
    fs::write(root.join("src/util.rs"), b"pub fn helper() {}\n\xff\xfe\n").unwrap();
    let result = indexer.initial_index().unwrap();
    assert_eq!(
        result.removed, 1,
        "the stale row of an unreadable file goes"
    );
    assert_eq!(result.errors, 0);
    assert!(store.get_file_by_path("src/util.rs").unwrap().is_none());
    assert!(store.get_file_by_path("src/lib.rs").unwrap().is_some());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

/// A directory that cannot be listed is a walk error: its files are kept in
/// the index (they did not vanish) and the error is counted. Needs a
/// non-root user: root reads a `chmod 000` directory anyway.
#[cfg(unix)]
#[test]
fn walk_error_keeps_files_under_the_unreadable_dir() {
    use std::os::unix::fs::PermissionsExt;

    /// Restores the directory's permissions on drop so `TempDir` can clean up.
    struct RestorePerms(std::path::PathBuf);
    impl Drop for RestorePerms {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    write_file(
        &root,
        "locked/secret.rs",
        "pub fn locked_away() -> i32 { 7 }\n",
    );

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    assert!(
        store
            .get_file_by_path("locked/secret.rs")
            .unwrap()
            .is_some()
    );

    let locked = root.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestorePerms(locked.clone());
    if fs::read_dir(&locked).is_ok() {
        eprintln!("skipped: the directory is readable despite chmod 000 (running as root?)");
        return;
    }

    // Meanwhile a file really was deleted: it is not pruned either, since the
    // walk cannot tell it apart from the files it could not reach.
    fs::remove_file(root.join("src/util.rs")).unwrap();

    let result = indexer.initial_index().unwrap();
    assert!(
        result.errors >= 1,
        "the unreadable directory is a walk error"
    );
    assert_eq!(
        result.removed, 0,
        "nothing is pruned after an incomplete walk"
    );
    assert!(
        store
            .get_file_by_path("locked/secret.rs")
            .unwrap()
            .is_some(),
        "files under the unreadable directory keep their rows"
    );
    assert!(store.get_file_by_path("src/util.rs").unwrap().is_some());

    // Once readable again, the next walk prunes what really vanished and
    // keeps what is there.
    drop(_restore);
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);
    assert_eq!(result.removed, 1);
    assert!(store.get_file_by_path("src/util.rs").unwrap().is_none());
    assert!(
        store
            .get_file_by_path("locked/secret.rs")
            .unwrap()
            .is_some()
    );
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}
