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

use semantiq_embeddings::StubEmbeddingModel;
use semantiq_index::{AutoIndexer, EmbeddingProgress, IndexStore, embed_pending};
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

/// A parser version bump (#86): `check_and_prepare_for_reindex` keeps the
/// files and chunks but every file must be read and re-extracted by the
/// next walk, mtime fast path (#157) included, and the embeddings of the
/// unchanged chunks are carried over.
#[test]
fn parser_bump_reextracts_every_file_despite_unchanged_mtimes() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::new(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    embed_pending(
        &store,
        &StubEmbeddingModel::new(),
        8,
        &EmbeddingProgress::new(),
        &|| false,
        &mut |_| {},
    )
    .unwrap();
    let before = store.get_stats().unwrap();
    assert!(before.symbol_count > 0 && before.ref_count > 0);
    let counts = store.embedding_counts().unwrap();
    assert!(counts.total > 0 && counts.pending() == 0);

    // An index left by an older parser.
    store.set_metadata("parser_version", "0").unwrap();
    assert!(store.check_and_prepare_for_reindex().unwrap());
    assert_eq!(
        store.get_stats().unwrap().symbol_count,
        0,
        "structure cleared"
    );
    assert_eq!(store.get_stats().unwrap().file_count, 2, "files kept");

    // Nothing touched the files: their mtimes are the stored ones.
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.indexed, 2, "every file is re-extracted");
    assert_eq!(result.skipped, 0);
    let after = store.get_stats().unwrap();
    assert_eq!(after.symbol_count, before.symbol_count);
    assert_eq!(after.ref_count, before.ref_count);
    let counts = store.embedding_counts().unwrap();
    assert_eq!(counts.pending(), 0, "embeddings carried over");
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
    assert!(!store.needs_full_reindex().unwrap());
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
    // Credential files and directories, hidden like the tooling directories
    // but never indexed.
    write_file(root, ".env.json", "{\"envJsonToken\": \"s3cr3t\"}\n");
    write_file(
        root,
        ".docker/config.json",
        "{\"dockerConfigAuth\": \"s3cr3t\"}\n",
    );
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
        "hidden tooling directories are indexed; .gitignore, VCS, cache, IDE, \
         dependency and credential directories and credential files are not"
    );
    assert!(store.get_file_by_path(".env.json").unwrap().is_none());
    assert!(
        store
            .get_file_by_path(".docker/config.json")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .find_symbol_by_name("envJsonToken")
            .unwrap()
            .is_empty(),
        "a key of a credential file must not be a symbol"
    );
    assert!(
        store
            .find_symbol_by_name("dockerConfigAuth")
            .unwrap()
            .is_empty()
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
    // Editors report several events per save: a file is indexed once.
    let result = indexer.process_file_events(vec![
        FileEvent::Modified(root.join(".storybook/main.ts")),
        FileEvent::Created(root.join("src/new.rs")),
        FileEvent::Modified(root.join("src/new.rs")),
        FileEvent::Modified(root.join(".storybook/main.ts")),
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

/// A file that cannot be read for a transient reason (permissions here) is
/// an error, not a removal: its row and embeddings stay, in the walk and on a
/// watcher event. Needs a non-root user: root reads a `chmod 000` file.
#[cfg(unix)]
#[test]
fn unreadable_file_keeps_its_row_on_permission_error() {
    use std::os::unix::fs::PermissionsExt;

    struct RestorePerms(std::path::PathBuf);
    impl Drop for RestorePerms {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o644));
        }
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    let before = store.get_file_by_path("src/util.rs").unwrap().unwrap();

    let util = root.join("src/util.rs");
    fs::set_permissions(&util, fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestorePerms(util.clone());
    if fs::read_to_string(&util).is_ok() {
        eprintln!("skipped: the file is readable despite chmod 000 (running as root?)");
        return;
    }

    let result = indexer.initial_index().unwrap();
    assert_eq!(result.removed, 0, "a transient read error is not a removal");
    assert_eq!(result.errors, 1);
    let kept = store.get_file_by_path("src/util.rs").unwrap().unwrap();
    assert_eq!(kept.hash, before.hash, "the row is untouched");

    let result = indexer.process_file_events(vec![FileEvent::Modified(util.clone())]);
    assert_eq!(result.indexed, 0);
    assert_eq!(result.removed, 0);
    assert_eq!(result.errors, 1);
    assert!(store.get_file_by_path("src/util.rs").unwrap().is_some());

    // Readable again and unchanged: nothing to do.
    drop(_restore);
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);
    assert_eq!(result.indexed, 0);
    assert_eq!(result.removed, 0);
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

/// A directory that cannot be listed is a walk error: its files are kept in
/// the index (they did not vanish), the error is counted, and pruning goes
/// on outside that directory. Needs a non-root user: root reads a
/// `chmod 000` directory anyway.
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

    // Meanwhile a file really was deleted outside the locked directory: the
    // walk did list its directory, so it is pruned as usual.
    fs::remove_file(root.join("src/util.rs")).unwrap();

    let result = indexer.initial_index().unwrap();
    assert!(
        result.errors >= 1,
        "the unreadable directory is a walk error"
    );
    assert_eq!(
        result.removed, 1,
        "a file that vanished outside the unlisted directory is pruned"
    );
    assert!(store.get_file_by_path("src/util.rs").unwrap().is_none());
    assert!(
        store
            .get_file_by_path("locked/secret.rs")
            .unwrap()
            .is_some(),
        "files under the unreadable directory keep their rows"
    );

    // Once readable again, the next walk keeps what is there...
    drop(_restore);
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);
    assert_eq!(result.removed, 0);
    assert!(
        store
            .get_file_by_path("locked/secret.rs")
            .unwrap()
            .is_some()
    );

    // ...and prunes under it what really vanished.
    fs::remove_file(root.join("locked/secret.rs")).unwrap();
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);
    assert_eq!(result.removed, 1);
    assert!(
        store
            .get_file_by_path("locked/secret.rs")
            .unwrap()
            .is_none()
    );
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

/// The project root itself cannot be listed: the walk fails at depth 0 and
/// sees nothing, which must read as "could not look", not as "every file
/// vanished" nor as a wrong project root. Nothing is pruned, the index is
/// untouched and `initial_index` returns `Ok`. Needs a non-root user.
#[cfg(unix)]
#[test]
fn unreadable_root_keeps_the_index() {
    use std::os::unix::fs::PermissionsExt;

    struct RestorePerms(std::path::PathBuf);
    impl Drop for RestorePerms {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("project");
    build_toy_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    assert_eq!(indexer.initial_index().unwrap().indexed, 2);
    assert_eq!(store.get_stats().unwrap().file_count, 2);

    fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestorePerms(root.clone());
    if fs::read_dir(&root).is_ok() {
        eprintln!("skipped: the root is readable despite chmod 000 (running as root?)");
        return;
    }

    let result = indexer
        .initial_index()
        .expect("an unlistable root is not a wrong root");
    assert!(result.errors >= 1, "the unlistable root is a walk error");
    assert_eq!(result.removed, 0, "nothing is pruned");
    assert_eq!(result.indexed, 0);
    assert_eq!(store.get_stats().unwrap().file_count, 2);
    assert_eq!(
        store.get_project_root().unwrap(),
        Some(root.canonicalize().unwrap()),
        "the root recorded by the first walk is left alone"
    );

    // Readable again: everything is still there and nothing is reindexed.
    drop(_restore);
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);
    assert_eq!(result.removed, 0);
    assert_eq!(result.indexed, 0);
    assert_eq!(store.get_stats().unwrap().file_count, 2);
}

/// Sorted relative paths of the indexed files.
fn indexed_paths(store: &IndexStore) -> Vec<String> {
    let mut paths = store.list_file_paths().unwrap();
    paths.sort();
    paths
}

/// A directory rename, a directory created with files in it and a directory
/// removal are each one event on the directory (#85): the rows under the old
/// name go, the files under the new one are indexed.
#[test]
fn directory_events_walk_and_prune_the_subtree() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    write_file(&root, "src/old/a.rs", "pub fn old_a() -> i32 { 1 }\n");
    write_file(&root, "src/old/b.rs", "pub fn old_b() -> i32 { 2 }\n");

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    assert_eq!(
        indexed_paths(&store),
        ["src/lib.rs", "src/old/a.rs", "src/old/b.rs", "src/util.rs"]
    );

    // `mv src/old src/new`, as FSEvents reports it: one Modify(Name) on each
    // directory path, nothing on the files.
    fs::rename(root.join("src/old"), root.join("src/new")).unwrap();
    let result = indexer.process_file_events(vec![
        FileEvent::Modified(root.join("src/old")),
        FileEvent::Modified(root.join("src/new")),
    ]);
    assert_eq!(result.removed, 2, "the rows under the old name go");
    assert_eq!(
        result.indexed, 2,
        "the files under the new name are indexed"
    );
    assert_eq!(result.errors, 0);
    assert_eq!(
        indexed_paths(&store),
        ["src/lib.rs", "src/new/a.rs", "src/new/b.rs", "src/util.rs"]
    );
    let old_a = store.find_symbol_by_name("old_a").unwrap();
    assert_eq!(old_a.len(), 1, "defined once, under the new path");
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);

    // A directory that appears with files already in it (`mv` from outside
    // the project, `git checkout`): one Create on the directory.
    write_file(&root, "src/extra/c.rs", "pub fn extra_c() -> i32 { 3 }\n");
    let result = indexer.process_file_events(vec![FileEvent::Created(root.join("src/extra"))]);
    assert_eq!(result.indexed, 1);
    assert_eq!(result.removed, 0);
    assert!(!store.find_symbol_by_name("extra_c").unwrap().is_empty());

    // A file event under a walked directory is covered by the walk, and a
    // second event on an unchanged subtree is a no-op.
    let result = indexer.process_file_events(vec![
        FileEvent::Modified(root.join("src/new")),
        FileEvent::Modified(root.join("src/new/a.rs")),
    ]);
    assert_eq!(result.indexed, 0);
    assert_eq!(result.removed, 0);
    assert_eq!(result.errors, 0);

    // `rm -rf src/new`: one Remove on the directory.
    fs::remove_dir_all(root.join("src/new")).unwrap();
    let result = indexer.process_file_events(vec![FileEvent::Deleted(root.join("src/new"))]);
    assert_eq!(result.removed, 2);
    assert_eq!(
        indexed_paths(&store),
        ["src/extra/c.rs", "src/lib.rs", "src/util.rs"]
    );
    assert!(store.find_symbol_by_name("old_a").unwrap().is_empty());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);

    // A vanished directory whose rows are already gone: nothing removed, no
    // error (the watcher reports the old path of a rename twice at times).
    let result = indexer.process_file_events(vec![FileEvent::Deleted(root.join("src/new"))]);
    assert_eq!(result.removed, 0);
    assert_eq!(result.errors, 0);
}

/// Editing `.gitignore` re-applies the rules (#138): files that became
/// ignored lose their rows, files that became un-ignored are indexed. A
/// nested `.gitignore` only touches its own subtree.
#[test]
fn ignore_file_edits_reapply_the_rules() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    write_file(
        &root,
        "generated/schema.ts",
        "export function generatedSchema(): number { return 1; }\n",
    );
    write_file(
        &root,
        "packages/gen/index.ts",
        "export function distIndex(): number { return 2; }\n",
    );
    write_file(
        &root,
        "packages/src/index.ts",
        "export function srcIndex(): number { return 3; }\n",
    );

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    assert_eq!(
        indexed_paths(&store),
        [
            "generated/schema.ts",
            "packages/gen/index.ts",
            "packages/src/index.ts",
            "src/lib.rs",
            "src/util.rs",
        ]
    );

    // The root `.gitignore` starts ignoring `generated/`.
    write_file(&root, ".gitignore", "generated/\n");
    let result = indexer.process_file_events(vec![FileEvent::Modified(root.join(".gitignore"))]);
    assert_eq!(result.removed, 1, "{result:?}");
    assert_eq!(result.indexed, 0);
    assert_eq!(result.errors, 0);
    assert!(
        store
            .get_file_by_path("generated/schema.ts")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .find_symbol_by_name("generatedSchema")
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);

    // A nested `.gitignore` ignores `gen/` under `packages/` only.
    write_file(&root, "packages/.gitignore", "gen/\n");
    let result =
        indexer.process_file_events(vec![FileEvent::Created(root.join("packages/.gitignore"))]);
    assert_eq!(result.removed, 1, "{result:?}");
    assert_eq!(result.indexed, 0);
    assert_eq!(
        indexed_paths(&store),
        ["packages/src/index.ts", "src/lib.rs", "src/util.rs"]
    );

    // The rule is lifted: the files are indexed again.
    write_file(&root, ".gitignore", "");
    fs::remove_file(root.join("packages/.gitignore")).unwrap();
    let result = indexer.process_file_events(vec![
        FileEvent::Modified(root.join(".gitignore")),
        FileEvent::Deleted(root.join("packages/.gitignore")),
    ]);
    assert_eq!(result.indexed, 2, "{result:?}");
    assert_eq!(result.removed, 0);
    assert_eq!(result.errors, 0);
    assert!(
        !store
            .find_symbol_by_name("generatedSchema")
            .unwrap()
            .is_empty()
    );
    assert!(!store.find_symbol_by_name("distIndex").unwrap().is_empty());
}

/// A root ignore rule that ignores every file (`*`, a mistaken edit) is a
/// legitimate edit that empties the index: the walk from this root was
/// validated at startup, so the wrong-root safety net of `initial_index`
/// must not refuse to prune (Codex review of #138). Lifting the rule brings
/// everything back.
#[test]
fn root_ignore_rule_may_ignore_every_file() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    assert_eq!(indexer.initial_index().unwrap().indexed, 2);

    write_file(&root, ".gitignore", "*\n");
    let result = indexer.process_file_events(vec![FileEvent::Modified(root.join(".gitignore"))]);
    assert_eq!(result.errors, 0, "{result:?}");
    assert_eq!(result.removed, 2);
    assert_eq!(result.indexed, 0);
    assert!(indexed_paths(&store).is_empty());
    assert!(store.find_symbol_by_name("alpha").unwrap().is_empty());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);

    write_file(&root, ".gitignore", "");
    let result = indexer.process_file_events(vec![FileEvent::Modified(root.join(".gitignore"))]);
    assert_eq!(result.indexed, 2, "{result:?}");
    assert_eq!(result.removed, 0);
    assert_eq!(result.errors, 0);
    assert_eq!(indexed_paths(&store), ["src/lib.rs", "src/util.rs"]);
}

/// Removing an ignore file lifts its rules (Codex review of #138): the
/// files it hid are indexed, under its own directory for a nested one and
/// the whole project for the root one. A rename reported on the old path
/// counts as a removal too.
#[test]
fn ignore_file_removal_lifts_the_rules() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    write_file(
        &root,
        "generated/schema.ts",
        "export function generatedSchema(): number { return 1; }\n",
    );
    write_file(
        &root,
        "packages/gen/index.ts",
        "export function distIndex(): number { return 2; }\n",
    );
    write_file(
        &root,
        "packages/src/index.ts",
        "export function srcIndex(): number { return 3; }\n",
    );
    write_file(&root, ".gitignore", "generated/\n");
    write_file(&root, "packages/.gitignore", "gen/\n");

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    assert_eq!(
        indexed_paths(&store),
        ["packages/src/index.ts", "src/lib.rs", "src/util.rs"]
    );

    // The nested one is deleted: its directory is walked again, not the
    // rest of the project (`generated/` stays ignored).
    fs::remove_file(root.join("packages/.gitignore")).unwrap();
    let result =
        indexer.process_file_events(vec![FileEvent::Deleted(root.join("packages/.gitignore"))]);
    assert_eq!(result.indexed, 1, "{result:?}");
    assert_eq!(result.removed, 0);
    assert_eq!(result.errors, 0);
    assert_eq!(
        indexed_paths(&store),
        [
            "packages/gen/index.ts",
            "packages/src/index.ts",
            "src/lib.rs",
            "src/util.rs",
        ]
    );

    // The root one is renamed away, reported on the old path.
    fs::rename(root.join(".gitignore"), root.join(".gitignore.bak")).unwrap();
    let result = indexer.process_file_events(vec![FileEvent::Modified(root.join(".gitignore"))]);
    assert_eq!(result.indexed, 1, "{result:?}");
    assert_eq!(result.removed, 0);
    assert_eq!(result.errors, 0);
    assert!(
        !store
            .find_symbol_by_name("generatedSchema")
            .unwrap()
            .is_empty()
    );

    // An ignore file vanishing with its directory is a plain removal.
    fs::remove_dir_all(root.join("packages")).unwrap();
    let result = indexer.process_file_events(vec![
        FileEvent::Deleted(root.join("packages/.gitignore")),
        FileEvent::Deleted(root.join("packages")),
    ]);
    assert_eq!(result.removed, 2, "{result:?}");
    assert_eq!(result.errors, 0);
    assert_eq!(
        indexed_paths(&store),
        ["generated/schema.ts", "src/lib.rs", "src/util.rs"]
    );
}

/// A parse failure on a modified file (#90) must not leave the symbols,
/// references, chunks and imports of the previous version in the index:
/// they are cleared, the error is reported, and the file is not parsed
/// again until it changes.
#[test]
fn parse_failure_clears_the_previous_rows_and_is_an_error() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    write_file(
        &root,
        "src/lib.rs",
        "use crate::util;\n\npub fn alpha() -> i32 { util::helper(1) }\n",
    );

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    let file_id = store.get_file_by_path("src/lib.rs").unwrap().unwrap().id;
    assert!(!store.find_symbol_by_name("alpha").unwrap().is_empty());
    assert!(!store.get_chunks_by_file(file_id).unwrap().is_empty());
    assert!(!store.get_dependencies(file_id).unwrap().is_empty());
    assert!(store.count_usages("helper").unwrap() > 0);

    indexer.simulate_parse_failure("src/lib.rs");
    let changed = "pub fn gamma() -> i32 { 42 }\n";
    write_file(&root, "src/lib.rs", changed);
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 1, "{result:?}");
    assert_eq!(result.indexed, 0);

    // Nothing of the old version is served...
    assert!(store.find_symbol_by_name("alpha").unwrap().is_empty());
    assert!(store.find_symbol_by_name("gamma").unwrap().is_empty());
    assert!(store.get_chunks_by_file(file_id).unwrap().is_empty());
    assert!(store.get_dependencies(file_id).unwrap().is_empty());
    assert_eq!(store.count_usages("helper").unwrap(), 0);
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
    // ...and the row is stamped: the next walk does not parse it again.
    assert!(!store.needs_reindex("src/lib.rs", changed).unwrap());
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);
    assert_eq!(result.indexed, 0);

    // A watcher event on the same file reports the error too.
    write_file(&root, "src/lib.rs", "pub fn delta() -> i32 { 43 }\n");
    let result = indexer.process_file_events(vec![FileEvent::Modified(root.join("src/lib.rs"))]);
    assert_eq!(result.errors, 1);
    assert_eq!(result.indexed, 0);
    assert!(store.find_symbol_by_name("delta").unwrap().is_empty());
}

/// Symlinks are not indexed (#109): a link to a file or directory outside
/// the project is not read through, a link inside it does not index the
/// same file twice, and a watcher event on a link indexes nothing.
#[cfg(unix)]
#[test]
fn symlinks_are_not_indexed() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let outside = tmp.path().join("outside");
    write_file(&outside, "private.rs", "pub fn outside_fn_marker() {}\n");
    write_file(&outside, "dir/leaked.rs", "pub fn leaked_marker() {}\n");
    write_file(&outside, "private.yaml", "token: SHOULD_NOT_BE_INDEXED\n");
    let root = tmp.path().join("project");
    build_toy_project(&root);
    symlink(outside.join("private.rs"), root.join("src/linked.rs")).unwrap();
    symlink(outside.join("private.yaml"), root.join("linked.yaml")).unwrap();
    symlink(outside.join("dir"), root.join("vendored")).unwrap();
    symlink("util.rs", root.join("src/link.rs")).unwrap();

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);
    assert_eq!(indexed_paths(&store), ["src/lib.rs", "src/util.rs"]);
    assert!(
        store
            .find_symbol_by_name("outside_fn_marker")
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .find_symbol_by_name("leaked_marker")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.find_symbol_by_name("helper").unwrap().len(),
        1,
        "an in-project link does not define the symbol twice"
    );

    // Watcher events on the links (a save through the link, a new link)
    // index nothing either.
    let result = indexer.process_file_events(vec![
        FileEvent::Modified(root.join("src/linked.rs")),
        FileEvent::Created(root.join("src/link.rs")),
        FileEvent::Modified(root.join("vendored")),
    ]);
    assert_eq!(result.indexed, 0);
    assert_eq!(result.errors, 0);
    assert_eq!(indexed_paths(&store), ["src/lib.rs", "src/util.rs"]);

    // A row an older version stored for a link is pruned by the walk.
    store
        .insert_file("src/linked.rs", Some("rust"), "stale", 0)
        .unwrap();
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.removed, 1);
    assert_eq!(indexed_paths(&store), ["src/lib.rs", "src/util.rs"]);
}

/// `.gitignore` is honoured outside a git repository too (#113): an exported
/// archive or a build directory has no `.git` and no reason to index what
/// its `.gitignore` names.
#[test]
fn gitignore_is_honoured_without_a_git_directory() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    write_file(&root, ".gitignore", "secrets.yaml\ngenerated/\n");
    write_file(
        &root,
        "secrets.yaml",
        "token: SHOULD_BE_IGNORED_BY_GITIGNORE\n",
    );
    write_file(&root, "generated/out.rs", "pub fn generated_out() {}\n");
    write_file(&root, "config/credentials.json", "{\"key\": \"x\"}\n");
    assert!(!root.join(".git").exists());

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    assert_eq!(indexed_paths(&store), ["src/lib.rs", "src/util.rs"]);
}

/// #81 / #147 / #143 / #142: the stored dependencies classify an import of a
/// file of the project as `local` with its `resolved_path`, whatever the
/// syntax (Python absolute import, Java package, Rust `mod x;` and
/// `super::`, C `#include "x.h"`), and the project's own package comes from
/// the walk done when the indexer is built.
#[test]
fn project_imports_are_local_and_resolved() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write_file(&root, "app/__init__.py", "");
    write_file(&root, "app/models.py", "class User:\n    pass\n");
    write_file(
        &root,
        "app/views.py",
        "import os\nimport requests\nfrom app.models import User\nfrom .models import User as U\n",
    );
    write_file(
        &root,
        "src/main/java/com/acme/models/User.java",
        "package com.acme.models;\npublic class User {}\n",
    );
    write_file(
        &root,
        "src/main/java/com/acme/api/Server.java",
        "package com.acme.api;\nimport java.util.List;\nimport com.acme.models.User;\n\
         import com.google.common.base.Strings;\npublic class Server {}\n",
    );
    write_file(
        &root,
        "rs/src/lib.rs",
        "mod util;\npub mod store;\nuse anyhow::Result;\n",
    );
    write_file(&root, "rs/src/util.rs", "pub fn helper() {}\n");
    write_file(
        &root,
        "rs/src/store/mod.rs",
        "use super::util::helper;\n#[cfg(test)]\nmod tests {\n    use super::*;\n}\n",
    );
    write_file(
        &root,
        "c/src/main.c",
        "#include <stdio.h>\n#include \"util.h\"\n",
    );
    write_file(&root, "c/src/util.h", "int f(void);\n");

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);

    let deps = |path: &str| -> Vec<(String, String, Option<String>)> {
        let file = store.get_file_by_path(path).unwrap().unwrap();
        store
            .get_dependencies(file.id)
            .unwrap()
            .into_iter()
            .map(|d| (d.target_path, d.kind, d.resolved_path))
            .collect()
    };
    let dep =
        |t: &str, k: &str, r: Option<&str>| (t.to_string(), k.to_string(), r.map(String::from));

    assert_eq!(
        deps("app/views.py"),
        vec![
            dep("os", "std", None),
            dep("requests", "external", None),
            dep("app.models", "local", Some("app/models.py")),
            dep(".models", "local", Some("app/models.py")),
        ]
    );
    assert_eq!(
        deps("src/main/java/com/acme/api/Server.java"),
        vec![
            dep("java.util.List", "std", None),
            dep(
                "com.acme.models.User",
                "local",
                Some("src/main/java/com/acme/models/User.java")
            ),
            dep("com.google.common.base.Strings", "external", None),
        ]
    );
    assert_eq!(
        deps("rs/src/lib.rs"),
        vec![
            dep("util", "local", Some("rs/src/util.rs")),
            dep("store", "local", Some("rs/src/store/mod.rs")),
            dep("anyhow::Result", "external", None),
        ]
    );
    assert_eq!(
        deps("rs/src/store/mod.rs"),
        vec![
            dep("super::util::helper", "local", Some("rs/src/util.rs")),
            dep("self", "local", None),
        ]
    );
    assert_eq!(
        deps("c/src/main.c"),
        vec![
            dep("stdio.h", "std", None),
            dep("util.h", "local", Some("c/src/util.h")),
        ]
    );

    // `imported_by` of the resolved files.
    let dependents = |path: &str| -> Vec<String> {
        store
            .get_dependents(path)
            .unwrap()
            .into_iter()
            .map(|d| d.target_path)
            .collect()
    };
    assert_eq!(dependents("app/models.py").len(), 2);
    assert_eq!(dependents("rs/src/util.rs").len(), 2);
}

/// Set a file's mtime.
fn set_mtime(path: &Path, time: std::time::SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

fn hour_ago() -> std::time::SystemTime {
    std::time::SystemTime::now() - std::time::Duration::from_secs(3600)
}

/// #157: an indexed file whose mtime did not move is trusted without being
/// read. Its content is rewritten behind the index's back (mtime restored):
/// the walk does not notice, by design, and does once the mtime moves.
#[test]
fn unchanged_mtime_is_trusted_without_reading_the_file() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    let util = root.join("src/util.rs");
    // Older than the index: not a racy stamp.
    set_mtime(&util, hour_ago());

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    assert_eq!(indexer.initial_index().unwrap().indexed, 2);
    let before = store.get_file_by_path("src/util.rs").unwrap().unwrap();

    fs::write(&util, "pub fn gamma(x: i32) -> i32 { x * 3 }\n").unwrap();
    set_mtime(&util, hour_ago());
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.indexed, 0, "same mtime: the file is not read");
    assert_eq!(result.skipped, 2);
    let kept = store.get_file_by_path("src/util.rs").unwrap().unwrap();
    assert_eq!(kept.hash, before.hash);
    assert!(store.find_symbol_by_name("gamma").unwrap().is_empty());

    // The mtime moves: hashed, changed, reindexed.
    set_mtime(&util, std::time::SystemTime::now());
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.indexed, 1);
    assert_eq!(store.find_symbol_by_name("gamma").unwrap().len(), 1);
    assert!(store.find_symbol_by_name("helper").unwrap().is_empty());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

/// A file whose mtime moved but whose content did not is hashed once: its
/// stamp is refreshed so that the next walk trusts the new mtime.
#[test]
fn unchanged_content_with_a_moved_mtime_is_restamped() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    let util = root.join("src/util.rs");

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();

    let past = hour_ago();
    set_mtime(&util, past);
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.indexed, 0);
    assert_eq!(result.skipped, 2);
    let stamp = store.get_file_by_path("src/util.rs").unwrap().unwrap();
    let past_secs = past
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert_eq!(
        stamp.last_modified, past_secs,
        "the stamp follows the mtime"
    );

    // Trusted now: a rewrite that keeps the mtime is invisible.
    fs::write(&util, "pub fn gamma(x: i32) -> i32 { x * 3 }\n").unwrap();
    set_mtime(&util, past);
    assert_eq!(indexer.initial_index().unwrap().indexed, 0);
}

/// A file modified in the same second its content was hashed (or later: a
/// future mtime) keeps a "racy" stamp, hashed again on every walk until it
/// settles, so a write landing right after the hash is never missed.
#[test]
fn racy_stamp_is_hashed_again() {
    use semantiq_index::RACY_WINDOW_SECS;
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    let util = root.join("src/util.rs");
    let soon = std::time::SystemTime::now() + std::time::Duration::from_secs(30);
    set_mtime(&util, soon);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    indexer.initial_index().unwrap();
    let stamp = store.get_file_by_path("src/util.rs").unwrap().unwrap();
    assert!(
        stamp.last_modified + RACY_WINDOW_SECS > stamp.indexed_at,
        "racy"
    );

    // Same mtime, new content: the stamp cannot be trusted, the hash tells.
    fs::write(&util, "pub fn gamma(x: i32) -> i32 { x * 3 }\n").unwrap();
    set_mtime(&util, soon);
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.indexed, 1);
    assert_eq!(store.find_symbol_by_name("gamma").unwrap().len(), 1);
}

/// On a filesystem with coarse mtimes (FAT: 2 s), a write right after the
/// hash can land on the mtime of the second *before* it. Such a stamp is
/// racy too, so the file is hashed again; one `RACY_WINDOW_SECS` older,
/// the stamp is trusted.
#[test]
fn coarse_mtime_in_the_window_before_the_hash_is_hashed_again() {
    use semantiq_index::RACY_WINDOW_SECS;
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    let util = root.join("src/util.rs");
    let old_mtime = hour_ago();
    set_mtime(&util, old_mtime);

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    assert_eq!(indexer.initial_index().unwrap().indexed, 2);
    let stamp = store.get_file_by_path("src/util.rs").unwrap().unwrap();
    let content = fs::read_to_string(&util).unwrap();

    // The hash happened one second after the (rounded) mtime: a FAT write
    // after it can keep that mtime. Rewrite the content, keep the mtime.
    store
        .insert_file_at(
            "src/util.rs",
            Some("rust"),
            &content,
            stamp.last_modified,
            stamp.last_modified + RACY_WINDOW_SECS - 1,
        )
        .unwrap();
    fs::write(&util, "pub fn gamma(x: i32) -> i32 { x * 3 }\n").unwrap();
    set_mtime(&util, old_mtime);
    assert_eq!(
        indexer.initial_index().unwrap().indexed,
        1,
        "a stamp within the window before the hash is hashed again"
    );
    assert_eq!(store.find_symbol_by_name("gamma").unwrap().len(), 1);

    // Hashed a full window after the mtime: trusted, a same-mtime rewrite
    // is not seen (by design, as on any filesystem).
    let stamp = store.get_file_by_path("src/util.rs").unwrap().unwrap();
    let content = fs::read_to_string(&util).unwrap();
    store
        .insert_file_at(
            "src/util.rs",
            Some("rust"),
            &content,
            stamp.last_modified,
            stamp.last_modified + RACY_WINDOW_SECS,
        )
        .unwrap();
    fs::write(&util, "pub fn delta(x: i32) -> i32 { x * 4 }\n").unwrap();
    set_mtime(&util, old_mtime);
    assert_eq!(indexer.initial_index().unwrap().indexed, 0);
    assert!(store.find_symbol_by_name("delta").unwrap().is_empty());
}

#[test]
fn file_stamp_trust_rules() {
    use semantiq_index::{FileStamp, RACY_WINDOW_SECS};
    let stamp = FileStamp {
        id: 1,
        hash: "h".into(),
        last_modified: 100,
        indexed_at: 200,
    };
    assert!(stamp.trusts(100));
    assert!(!stamp.trusts(101), "mtime moved");
    assert!(!stamp.trusts(99), "mtime moved back (cp -p, checkout)");
    let racy = FileStamp {
        last_modified: 200,
        ..stamp.clone()
    };
    assert!(
        racy.is_racy() && !racy.trusts(200),
        "hashed in the mtime's second"
    );
    // A filesystem with 2 s mtimes (FAT, SMB) can give a write after the
    // hash the mtime of the second before it.
    let coarse = FileStamp {
        last_modified: 200 - RACY_WINDOW_SECS + 1,
        ..stamp.clone()
    };
    assert!(
        coarse.is_racy() && !coarse.trusts(coarse.last_modified),
        "mtime within the racy window before the hash"
    );
    let settled = FileStamp {
        last_modified: 200 - RACY_WINDOW_SECS,
        ..stamp.clone()
    };
    assert!(
        !settled.is_racy() && settled.trusts(settled.last_modified),
        "mtime older than the racy window"
    );
    let future = FileStamp {
        last_modified: 300,
        ..stamp
    };
    assert!(
        future.is_racy() && !future.trusts(300),
        "mtime after the hash"
    );
}

/// #89 / #158: a file's row and tables are written in one transaction. A
/// write that fails halfway (here a trigger aborting the chunk insert)
/// leaves the previous symbols and hash in place, so the file is indexed
/// again next time instead of carrying new symbols under an old hash.
#[test]
fn file_index_write_is_all_or_nothing() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    build_toy_project(&root);
    let db = root.join("index.db");

    let store = Arc::new(IndexStore::open(&db).unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone()).unwrap();
    assert_eq!(indexer.initial_index().unwrap().indexed, 2);
    let before = store.get_file_by_path("src/util.rs").unwrap().unwrap();

    let other = rusqlite::Connection::open(&db).unwrap();
    other
        .execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON chunks
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();

    fs::write(
        root.join("src/util.rs"),
        "pub fn gamma(x: i32) -> i32 { x * 3 }\n",
    )
    .unwrap();
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 1);
    assert_eq!(result.indexed, 0);
    let kept = store.get_file_by_path("src/util.rs").unwrap().unwrap();
    assert_eq!(kept.hash, before.hash, "the old hash stays");
    assert_eq!(kept.id, before.id);
    assert_eq!(store.find_symbol_by_name("helper").unwrap().len(), 1);
    assert!(store.find_symbol_by_name("gamma").unwrap().is_empty());

    other.execute_batch("DROP TRIGGER boom;").unwrap();
    assert_eq!(indexer.initial_index().unwrap().indexed, 1);
    assert_eq!(store.find_symbol_by_name("gamma").unwrap().len(), 1);
    assert!(store.find_symbol_by_name("helper").unwrap().is_empty());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

/// #158: the parser pool indexes the same project as a single thread does,
/// file for file.
#[test]
fn parser_pool_matches_single_threaded_indexing() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    for i in 0..40 {
        write_file(
            &root,
            &format!("src/m{i}.rs"),
            &format!(
                "use crate::m{prev}::f{prev};\n\npub struct S{i};\n\nimpl S{i} {{\n    pub fn get(&self) -> i32 {{ f{i}() }}\n}}\n\npub fn f{i}() -> i32 {{ f{prev}() + {i} }}\n",
                prev = (i + 39) % 40
            ),
        );
    }
    write_file(
        &root,
        "data/config.json",
        "{\"name\": \"toy\", \"threads\": 4}\n",
    );

    let index_with = |threads: usize| {
        let store = Arc::new(IndexStore::open_in_memory().unwrap());
        let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.clone())
            .unwrap()
            .with_threads(threads);
        assert_eq!(indexer.threads(), threads);
        let result = indexer.initial_index().unwrap();
        assert_eq!(result.indexed, 41);
        assert_eq!(result.errors, 0);
        let stats = store.get_stats().unwrap();
        let mut refs: Vec<_> = store
            .find_references_by_name("f3", 100)
            .unwrap()
            .into_iter()
            .map(|r| (r.file_path, r.line, r.kind))
            .collect();
        refs.sort();
        let callers = store.find_callers("f7", 100).unwrap().len();
        (
            stats.file_count,
            stats.symbol_count,
            stats.ref_count,
            stats.call_edge_count,
            stats.chunk_count,
            stats.dependency_count,
            refs,
            callers,
        )
    };
    let single = index_with(1);
    let pooled = index_with(4);
    assert_eq!(single, pooled);
    assert_eq!(single.0, 41);
    assert!(single.7 >= 2, "{} callers of f7", single.7);
}
