//! Two-phase indexing: phase 1 (`AutoIndexer`) stores the structure and the
//! chunks without embeddings, phase 2 (`embed_pending` / `BackgroundEmbedder`)
//! fills the embeddings, resumably and without orphan `chunks_vec` rows.

use anyhow::Result;
use semantiq_embeddings::{EMBEDDING_DIMENSION, EmbeddingModel, StubEmbeddingModel};
use semantiq_index::{
    AutoIndexer, BackgroundEmbedder, EmbeddingProgress, IndexStore, embed_pending,
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
    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let indexer = AutoIndexer::without_watcher(Arc::clone(&store), root.to_path_buf()).unwrap();
    let result = indexer.initial_index().unwrap();
    assert_eq!(result.errors, 0);
    store
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
