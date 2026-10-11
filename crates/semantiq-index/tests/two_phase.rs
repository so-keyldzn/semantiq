//! Two-phase indexing: phase 1 (`AutoIndexer`) stores the structure and the
//! chunks without embeddings, phase 2 (`embed_pending` / `BackgroundEmbedder`)
//! fills the embeddings, resumably and without orphan `chunks_vec` rows.

use anyhow::Result;
use semantiq_embeddings::{EMBEDDING_DIMENSION, EmbeddingModel, StubEmbeddingModel};
use semantiq_index::{
    AutoIndexer, BackgroundEmbedder, EmbedderConfig, EmbeddingProgress, IndexStore, embed_pending,
};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tempfile::TempDir;

/// A model that returns distinct non-zero vectors and counts the texts it embeds.
#[derive(Default)]
struct CountingModel {
    texts: AtomicUsize,
}

impl EmbeddingModel for CountingModel {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.texts.fetch_add(1, Ordering::Relaxed);
        let seed = (text.len() % 97) as f32 + 1.0;
        Ok((0..EMBEDDING_DIMENSION)
            .map(|i| seed + i as f32 * 1e-4)
            .collect())
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
    fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.embed(query)
    }
    fn dimension(&self) -> usize {
        EMBEDDING_DIMENSION
    }
}

fn write_file(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

/// Enough functions over several files for many cross-file batches.
fn build_project(root: &Path, files: usize, functions: usize) {
    for f in 0..files {
        let mut src = String::new();
        for i in 0..functions {
            src.push_str(&format!(
                "/// Function {i} of file {f}.\npub fn f{f}_{i}(x: i32) -> i32 {{\n    let y = x * {i};\n    helper(y) + {f}\n}}\n\n"
            ));
        }
        if f == 0 {
            src.push_str("pub fn helper(x: i32) -> i32 { x + 1 }\n");
        }
        write_file(root, &format!("src/m{f}.rs"), &src);
    }
}

fn no_stop() -> bool {
    false
}

fn phase1(root: &Path) -> Arc<IndexStore> {
    phase1_into(root, Arc::new(IndexStore::open_in_memory().unwrap()))
}

fn phase1_into(root: &Path, store: Arc<IndexStore>) -> Arc<IndexStore> {
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.to_path_buf()).unwrap();
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);
    store
}

/// The server's store and a second connection to the same file, standing for
/// another process (CLI refresh, `semantiq index --no-embeddings`).
fn on_disk_stores(tmp: &TempDir) -> (Arc<IndexStore>, Arc<IndexStore>) {
    let db = tmp.path().join("index.db");
    (
        Arc::new(IndexStore::open(&db).unwrap()),
        Arc::new(IndexStore::open(&db).unwrap()),
    )
}

#[test]
fn phase1_stores_structure_without_any_embedding() {
    let tmp = TempDir::new().unwrap();
    build_project(tmp.path(), 3, 5);
    let store = phase1(tmp.path());

    // References and call edges are there...
    let refs = store.find_references_by_name("helper", 100).unwrap();
    assert!(refs.len() > 1, "helper: definition + usages, got {refs:?}");
    let callers = store.find_callers("helper", 100).unwrap();
    assert_eq!(callers.len(), 15, "every f*_* calls helper");
    // ...and no chunk has an embedding yet, so no vector either.
    let counts = store.embedding_counts().unwrap();
    assert!(counts.total >= 3);
    assert_eq!(counts.embedded, 0);
    assert_eq!(counts.percent(), 0);
    // The file hash is stamped: an unchanged file is not reindexed.
    let indexer =
        AutoIndexer::without_watcher(Arc::clone(&store), tmp.path().to_path_buf()).unwrap();
    let again = indexer.initial_index().unwrap();
    assert_eq!(again.indexed, 0);
    assert_eq!(again.skipped, 3);
}

#[test]
fn phase2_embeds_every_chunk_across_files() {
    let tmp = TempDir::new().unwrap();
    build_project(tmp.path(), 80, 2);
    let store = phase1(tmp.path());
    let total = store.embedding_counts().unwrap().total;
    assert!(total > 32, "needs several batches, got {total} chunks");

    let model = CountingModel::default();
    let progress = EmbeddingProgress::new();
    let mut batches = 0;
    let result = embed_pending(&store, &model, 32, &progress, &no_stop, &mut |_| {
        batches += 1
    })
    .unwrap();

    assert_eq!(result.embedded, total);
    assert_eq!(result.failed, 0);
    assert_eq!(batches, total.div_ceil(32));
    assert_eq!(model.texts.load(Ordering::Relaxed), total);
    let counts = store.embedding_counts().unwrap();
    assert_eq!(counts.pending(), 0);
    assert_eq!(counts.percent(), 100);
    assert_eq!(progress.counts(), counts);
    assert!(progress.in_progress().is_none());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
    // Every chunk is now searchable by vector.
    let query = vec![1.0; EMBEDDING_DIMENSION];
    assert!(!store.search_similar_chunks(&query, 5).unwrap().is_empty());

    // Nothing left: a second pass embeds nothing.
    let again = embed_pending(&store, &model, 32, &progress, &no_stop, &mut |_| {}).unwrap();
    assert_eq!(again.embedded, 0);
    assert_eq!(model.texts.load(Ordering::Relaxed), total);
}

#[test]
fn phase2_resumes_after_interruption() {
    let tmp = TempDir::new().unwrap();
    build_project(tmp.path(), 80, 2);
    let store = phase1(tmp.path());
    let total = store.embedding_counts().unwrap().total;

    // Stop after two batches, as a crash or Ctrl-C would.
    let model = CountingModel::default();
    let checks = AtomicUsize::new(0);
    let stop_after_two = || checks.fetch_add(1, Ordering::Relaxed) >= 2;
    let progress = EmbeddingProgress::new();
    let first = embed_pending(&store, &model, 16, &progress, &stop_after_two, &mut |_| {}).unwrap();
    assert!(first.stopped);
    assert_eq!(first.embedded, 32);
    let partial = store.embedding_counts().unwrap();
    assert_eq!(partial.embedded, 32);
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);

    // The next pass only embeds what is left.
    let second = embed_pending(&store, &model, 16, &progress, &no_stop, &mut |_| {}).unwrap();
    assert_eq!(second.embedded, total - 32);
    assert_eq!(model.texts.load(Ordering::Relaxed), total);
    assert_eq!(store.embedding_counts().unwrap().pending(), 0);
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

/// A model that reindexes the project while a batch is being embedded, as the
/// watcher would while the background embedder computes vectors.
struct ReindexingModel {
    store: Arc<IndexStore>,
    root: std::path::PathBuf,
    fired: AtomicUsize,
}

impl EmbeddingModel for ReindexingModel {
    fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.5; EMBEDDING_DIMENSION])
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if self.fired.fetch_add(1, Ordering::Relaxed) == 0 {
            // Every file changes: all chunks read so far are deleted.
            for f in 0..2 {
                fs::write(
                    self.root.join(format!("src/m{f}.rs")),
                    format!("pub fn changed_{f}() {{}}\n"),
                )
                .unwrap();
            }
            AutoIndexer::without_watcher(Arc::clone(&self.store), self.root.clone())
                .unwrap()
                .initial_index()
                .unwrap();
        }
        texts.iter().map(|t| self.embed(t)).collect()
    }
    fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.embed(query)
    }
    fn dimension(&self) -> usize {
        EMBEDDING_DIMENSION
    }
}

#[test]
fn phase2_skips_chunks_deleted_meanwhile_without_orphans() {
    let tmp = TempDir::new().unwrap();
    build_project(tmp.path(), 2, 3);
    let store = phase1(tmp.path());

    let model = ReindexingModel {
        store: Arc::clone(&store),
        root: tmp.path().to_path_buf(),
        fired: AtomicUsize::new(0),
    };
    let progress = EmbeddingProgress::new();
    let result = embed_pending(&store, &model, 64, &progress, &no_stop, &mut |_| {}).unwrap();

    // The first batch's chunks vanished: nothing written for them...
    assert!(result.vanished > 0);
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
    // ...and the reindexed chunks (newer ids) were picked up by the same pass.
    assert_eq!(store.embedding_counts().unwrap().pending(), 0);
}

#[test]
fn reindex_keeps_embeddings_of_unchanged_chunks() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_file(
        root,
        "src/lib.rs",
        "pub fn one() -> i32 {\n    1\n}\n\npub fn two() -> i32 {\n    2\n}\n\npub fn three() -> i32 {\n    3\n}\n",
    );
    let store = phase1(root);
    let model = CountingModel::default();
    let progress = EmbeddingProgress::new();
    embed_pending(&store, &model, 32, &progress, &no_stop, &mut |_| {}).unwrap();
    let total = store.embedding_counts().unwrap().total;
    assert_eq!(model.texts.load(Ordering::Relaxed), total);

    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.to_path_buf()).unwrap();

    // Same content rewritten (a touch, an editor save): skipped, nothing to embed.
    let content = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    fs::write(root.join("src/lib.rs"), &content).unwrap();
    assert_eq!(indexer.initial_index().unwrap().indexed, 0);
    assert_eq!(store.embedding_counts().unwrap().pending(), 0);

    // One function edited: only its chunk waits for an embedding.
    fs::write(
        root.join("src/lib.rs"),
        content.replace("    2\n", "    22\n"),
    )
    .unwrap();
    assert_eq!(indexer.initial_index().unwrap().indexed, 1);
    let counts = store.embedding_counts().unwrap();
    assert_eq!(counts.total, total);
    assert_eq!(counts.pending(), 1, "only the edited chunk is re-embedded");
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);

    let result = embed_pending(&store, &model, 32, &progress, &no_stop, &mut |_| {}).unwrap();
    assert_eq!(result.embedded, 1);
    assert_eq!(model.texts.load(Ordering::Relaxed), total + 1);
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

/// #62: a function larger than the chunk size is stored as several windowed
/// chunks, and editing its tail only re-embeds the windows whose content
/// changed (the carry-over is keyed by content).
#[test]
fn oversized_function_is_stored_as_windows_and_keeps_unchanged_embeddings() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let mut src = String::from("pub fn huge(x: i32) -> i32 {\n    let mut acc = x;\n");
    for i in 0..400 {
        src.push_str(&format!("    acc = step_{i}(acc);\n"));
    }
    src.push_str("    acc\n}\n");
    write_file(root, "src/lib.rs", &src);
    let store = phase1(root);
    let model = CountingModel::default();
    let progress = EmbeddingProgress::new();
    embed_pending(&store, &model, 32, &progress, &no_stop, &mut |_| {}).unwrap();
    let total = store.embedding_counts().unwrap().total;
    assert!(total > 4, "{total} chunks for a 400-line function");

    // Edit the last line of the body: only the last window(s) change.
    fs::write(
        root.join("src/lib.rs"),
        src.replace("    acc\n}\n", "    acc + 1\n}\n"),
    )
    .unwrap();
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.to_path_buf()).unwrap();
    assert_eq!(indexer.initial_index().unwrap().indexed, 1);
    let counts = store.embedding_counts().unwrap();
    assert_eq!(counts.total, total);
    assert!(
        counts.pending() <= 2,
        "{} of {} windows re-embedded after editing the tail",
        counts.pending(),
        total
    );
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

#[test]
fn background_embedder_fills_pending_chunks_when_notified() {
    let tmp = TempDir::new().unwrap();
    build_project(tmp.path(), 3, 10);
    let store = phase1(tmp.path());
    assert!(store.embedding_counts().unwrap().pending() > 0);

    let embedder = BackgroundEmbedder::spawn(
        Arc::clone(&store),
        Box::new(|| Ok(Box::new(StubEmbeddingModel::new()) as Box<dyn EmbeddingModel>)),
    )
    .unwrap();
    // Nothing runs before the first notification (phase 1 owns the CPU).
    std::thread::sleep(Duration::from_millis(50));
    assert!(store.embedding_counts().unwrap().pending() > 0);

    embedder.notify();
    assert!(embedder.wait_idle(Duration::from_secs(30)));
    assert_eq!(store.embedding_counts().unwrap().pending(), 0);
    assert!(embedder.progress().in_progress().is_none());
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);

    // New chunks after a reindex are embedded on the next notification.
    write_file(tmp.path(), "src/new.rs", "pub fn fresh() -> i32 { 7 }\n");
    AutoIndexer::without_watcher(Arc::clone(&store), tmp.path().to_path_buf())
        .unwrap()
        .initial_index()
        .unwrap();
    assert!(store.embedding_counts().unwrap().pending() > 0);
    embedder.notify();
    assert!(embedder.wait_idle(Duration::from_secs(30)));
    assert_eq!(store.embedding_counts().unwrap().pending(), 0);
}

/// Fast timing for the background thread's own checks.
fn quick_config() -> EmbedderConfig {
    EmbedderConfig {
        poll_interval: Duration::from_millis(50),
        initial_retry_delay: Duration::from_millis(200),
        max_retry_delay: Duration::from_secs(1),
    }
}

/// Poll `cond` until it holds or `timeout` elapses.
fn wait_until(timeout: Duration, cond: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while !cond() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// #37: chunks left pending by another process (`semantiq index
/// --no-embeddings`, a query command's refresh) are embedded without any
/// `notify`: the thread polls `PRAGMA data_version` on its own and counts the
/// chunks when another connection has committed.
#[test]
fn background_embedder_polls_for_chunks_pending_without_a_notification() {
    let tmp = TempDir::new().unwrap();
    build_project(tmp.path(), 2, 8);
    let (store, other_process) = on_disk_stores(&tmp);
    phase1_into(tmp.path(), Arc::clone(&other_process));
    assert!(store.embedding_counts().unwrap().pending() > 0);

    let embedder = BackgroundEmbedder::spawn_with(
        Arc::clone(&store),
        Box::new(|| Ok(Box::new(StubEmbeddingModel::new()) as Box<dyn EmbeddingModel>)),
        quick_config(),
    )
    .unwrap();
    // No notify: the startup backlog is picked up by the first poll.
    assert!(wait_until(Duration::from_secs(10), || {
        store.embedding_counts().unwrap().pending() == 0
    }));
    assert!(embedder.progress().in_progress().is_none());

    // Phase 1 run by another process (the CLI refresh) while the server is
    // up: still no notify, still embedded.
    write_file(tmp.path(), "src/late.rs", "pub fn late() -> i32 { 7 }\n");
    phase1_into(tmp.path(), Arc::clone(&other_process));
    assert!(store.embedding_counts().unwrap().pending() > 0);
    assert!(wait_until(Duration::from_secs(10), || {
        store.embedding_counts().unwrap().pending() == 0
    }));
    assert_eq!(store.count_orphan_chunk_vectors().unwrap(), 0);
}

/// `PRAGMA data_version` moves only on another connection's commit.
#[test]
fn data_version_changes_only_when_another_connection_commits() {
    let tmp = TempDir::new().unwrap();
    build_project(tmp.path(), 1, 2);
    let (store, other_process) = on_disk_stores(&tmp);
    let before = store.data_version().unwrap();
    assert_eq!(store.data_version().unwrap(), before, "stable when idle");

    phase1_into(tmp.path(), Arc::clone(&store));
    assert_eq!(
        store.data_version().unwrap(),
        before,
        "own writes do not move it"
    );

    write_file(tmp.path(), "src/more.rs", "pub fn more() -> i32 { 1 }\n");
    phase1_into(tmp.path(), Arc::clone(&other_process));
    assert_ne!(
        store.data_version().unwrap(),
        before,
        "another connection's commit moves it"
    );
}

/// #52: a failed model load is reported through the progress and retried
/// after a backoff; the first successful load clears the error and embeds the
/// backlog.
#[test]
fn background_embedder_retries_a_failed_model_load_with_backoff() {
    let tmp = TempDir::new().unwrap();
    build_project(tmp.path(), 1, 5);
    let store = phase1(tmp.path());
    let total = store.embedding_counts().unwrap().pending();
    assert!(total > 0);

    let attempts = Arc::new(AtomicUsize::new(0));
    let loader_attempts = Arc::clone(&attempts);
    let embedder = BackgroundEmbedder::spawn_with(
        Arc::clone(&store),
        Box::new(move || {
            // The first two loads fail (flaky download), the third succeeds.
            if loader_attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                anyhow::bail!("download failed: connection reset")
            }
            Ok(Box::new(StubEmbeddingModel::new()) as Box<dyn EmbeddingModel>)
        }),
        EmbedderConfig {
            poll_interval: Duration::from_millis(50),
            initial_retry_delay: Duration::from_millis(300),
            max_retry_delay: Duration::from_secs(1),
        },
    )
    .unwrap();

    embedder.notify();
    assert!(embedder.wait_idle(Duration::from_secs(10)));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    let error = embedder
        .progress()
        .model_error()
        .expect("load failure exposed");
    assert!(error.contains("connection reset"), "{error}");
    assert!(embedder.progress().in_progress().is_none());
    assert_eq!(store.embedding_counts().unwrap().pending(), total);

    // Within the backoff, a notification does not hammer the loader.
    embedder.notify();
    assert!(embedder.wait_idle(Duration::from_secs(10)));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "retry waits for the backoff"
    );

    // Once the backoff elapses the polls retry (second failure, then
    // success) and the pending chunks are embedded.
    assert!(wait_until(Duration::from_secs(15), || {
        store.embedding_counts().unwrap().pending() == 0
    }));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert!(wait_until(Duration::from_secs(5), || {
        embedder.progress().model_error().is_none()
    }));
}

/// A poll does not retry the chunks the model keeps failing on while nobody
/// wrote to the database; a commit from another connection (CLI reindex that
/// may have replaced them) or a notification does.
#[test]
fn background_embedder_poll_retries_failing_chunks_only_after_another_connection_writes() {
    #[derive(Default)]
    struct FailingModel {
        calls: AtomicUsize,
    }
    impl EmbeddingModel for FailingModel {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("bad chunk")
        }
        fn embed_batch(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            anyhow::bail!("bad batch")
        }
        fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
            self.embed(query)
        }
        fn dimension(&self) -> usize {
            EMBEDDING_DIMENSION
        }
    }

    let tmp = TempDir::new().unwrap();
    build_project(tmp.path(), 1, 3);
    let (store, other_process) = on_disk_stores(&tmp);
    phase1_into(tmp.path(), Arc::clone(&other_process));
    let pending = store.embedding_counts().unwrap().pending();
    assert!(pending > 0);

    let model = Arc::new(FailingModel::default());
    let shared = Arc::clone(&model);
    let embedder = BackgroundEmbedder::spawn_with(
        Arc::clone(&store),
        Box::new(move || Ok(Box::new(ArcModel(Arc::clone(&shared))) as Box<dyn EmbeddingModel>)),
        quick_config(),
    )
    .unwrap();

    // First poll: one attempt per chunk, all left pending.
    assert!(wait_until(Duration::from_secs(10), || {
        model.calls.load(Ordering::SeqCst) >= pending
    }));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        pending,
        "later polls do not touch an unchanged database"
    );

    // Another process reindexes: the pending set may have changed, so the
    // next poll runs a pass (the old chunks fail again, the new one too).
    write_file(tmp.path(), "src/extra.rs", "pub fn extra() -> i32 { 1 }\n");
    phase1_into(tmp.path(), Arc::clone(&other_process));
    let pending_now = store.embedding_counts().unwrap().pending();
    assert!(pending_now > pending);
    assert!(wait_until(Duration::from_secs(10), || {
        model.calls.load(Ordering::SeqCst) >= pending + pending_now
    }));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(model.calls.load(Ordering::SeqCst), pending + pending_now);

    // A notification always runs a pass.
    embedder.notify();
    assert!(embedder.wait_idle(Duration::from_secs(10)));
    assert_eq!(
        model.calls.load(Ordering::SeqCst),
        pending + 2 * pending_now
    );
}

/// Shares one model between the test and the loader.
struct ArcModel<M: EmbeddingModel>(Arc<M>);

impl<M: EmbeddingModel> EmbeddingModel for ArcModel<M> {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.0.embed(text)
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.0.embed_batch(texts)
    }
    fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.0.embed_query(query)
    }
    fn dimension(&self) -> usize {
        self.0.dimension()
    }
}
