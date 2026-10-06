//! Phase 2 of indexing: embeddings for the chunks phase 1 stored without one.
//!
//! Phase 1 (`AutoIndexer`) writes symbols, references, call edges, type
//! relations, imports and chunks, and stamps the file hash: every structural
//! query works as soon as it is done. Phase 2 then fills `chunks.embedding` /
//! `chunks_vec`. A chunk whose `embedding` is NULL is still to do, so phase 2
//! resumes after a crash or a stop with no extra bookkeeping.
//!
//! Chunks are embedded across files, `batch_size` at a time in id order (so a
//! forward pass is full even on a project of small files; sorting windows by
//! length to cut padding measured no faster, see `examples/embed_throughput.rs`).
//! Vectors are computed without holding the database lock and written one
//! batch per transaction.

use crate::IndexStore;
use crate::store::{EmbeddingCounts, PendingChunk};
use anyhow::Result;
use semantiq_embeddings::EmbeddingModel;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Texts per forward pass (the ONNX model's own batch size).
pub const DEFAULT_BATCH_SIZE: usize = 32;

/// Pending chunks read per query.
const WINDOW_SIZE: usize = 8 * DEFAULT_BATCH_SIZE;

/// Live progress of phase 2, shared with the readers that report it.
#[derive(Debug, Default)]
pub struct EmbeddingProgress {
    embedded: AtomicUsize,
    total: AtomicUsize,
    running: AtomicBool,
}

impl EmbeddingProgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Chunks with an embedding / all chunks, as of the last update.
    pub fn counts(&self) -> EmbeddingCounts {
        EmbeddingCounts {
            embedded: self.embedded.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
        }
    }

    /// Whether a phase 2 pass is under way.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// The counts while a pass still has chunks to embed, `None` otherwise.
    pub fn in_progress(&self) -> Option<EmbeddingCounts> {
        let counts = self.counts();
        (self.is_running() && counts.pending() > 0).then_some(counts)
    }

    fn start(&self, counts: EmbeddingCounts) {
        self.embedded.store(counts.embedded, Ordering::Relaxed);
        self.total.store(counts.total, Ordering::Relaxed);
        self.running.store(true, Ordering::Relaxed);
    }

    fn add(&self, embedded: usize) {
        let total = self.total.load(Ordering::Relaxed);
        let done = (self.embedded.load(Ordering::Relaxed) + embedded).min(total);
        self.embedded.store(done, Ordering::Relaxed);
    }

    fn finish(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Outcome of one phase 2 pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmbedResult {
    /// Chunks given an embedding.
    pub embedded: usize,
    /// Chunks the model failed on (left pending, retried by the next pass).
    pub failed: usize,
    /// Chunks deleted while their vector was computed (file reindexed).
    pub vanished: usize,
    /// Whether the pass stopped early on request.
    pub stopped: bool,
}

/// Embed every pending chunk, `batch_size` texts per forward pass.
///
/// `progress` is reset from the database at the start and advanced after
/// every batch; `on_batch` runs after each batch (progress display);
/// `should_stop` is checked between batches.
pub fn embed_pending(
    store: &IndexStore,
    model: &dyn EmbeddingModel,
    batch_size: usize,
    progress: &EmbeddingProgress,
    should_stop: &dyn Fn() -> bool,
    on_batch: &mut dyn FnMut(&EmbeddingProgress),
) -> Result<EmbedResult> {
    progress.start(store.embedding_counts()?);
    let result = embed_pending_inner(store, model, batch_size, progress, should_stop, on_batch);
    progress.finish();
    result
}

fn embed_pending_inner(
    store: &IndexStore,
    model: &dyn EmbeddingModel,
    batch_size: usize,
    progress: &EmbeddingProgress,
    should_stop: &dyn Fn() -> bool,
    on_batch: &mut dyn FnMut(&EmbeddingProgress),
) -> Result<EmbedResult> {
    let batch_size = batch_size.max(1);
    let mut result = EmbedResult::default();
    let mut after_id = 0;

    loop {
        let window = store.pending_embedding_chunks(after_id, WINDOW_SIZE.max(batch_size))?;
        let Some(last) = window.last() else {
            break;
        };
        after_id = last.id;

        for batch in window.chunks(batch_size) {
            if should_stop() {
                result.stopped = true;
                return Ok(result);
            }
            let vectors = embed_batch(model, batch);
            let pairs: Vec<(i64, &[f32])> = batch
                .iter()
                .zip(&vectors)
                .filter_map(|(chunk, vector)| Some((chunk.id, vector.as_deref()?)))
                .collect();
            let failed = batch.len() - pairs.len();
            let written = store.store_chunk_embeddings(&pairs)?;
            result.embedded += written;
            result.failed += failed;
            result.vanished += pairs.len() - written;
            progress.add(written);
            on_batch(progress);
        }
    }

    Ok(result)
}

/// Embed one batch; on a batch error, fall back to one text at a time so a
/// single bad chunk does not hold back its neighbours. `None` = failed.
fn embed_batch(model: &dyn EmbeddingModel, batch: &[PendingChunk]) -> Vec<Option<Vec<f32>>> {
    let texts: Vec<String> = batch.iter().map(|chunk| chunk.content.clone()).collect();
    match model.embed_batch(&texts) {
        Ok(vectors) if vectors.len() == batch.len() => vectors.into_iter().map(Some).collect(),
        outcome => {
            if let Err(e) = outcome {
                debug!("Batch embedding failed, falling back to individual: {}", e);
            }
            batch
                .iter()
                .map(|chunk| match model.embed(&chunk.content) {
                    Ok(vector) => Some(vector),
                    Err(e) => {
                        debug!("Failed to embed chunk {}: {}", chunk.id, e);
                        None
                    }
                })
                .collect()
        }
    }
}

/// Loads the embedding model on the background thread, at its first pass.
pub type ModelLoader = Box<dyn FnOnce() -> Result<Box<dyn EmbeddingModel>> + Send>;

/// Runs phase 2 on a dedicated thread, woken by [`BackgroundEmbedder::notify`]
/// after each phase 1 write. Stops when dropped.
pub struct BackgroundEmbedder {
    shared: Arc<Shared>,
}

struct Shared {
    /// Set by `notify`, cleared when a pass starts.
    dirty: Mutex<bool>,
    wake: Condvar,
    shutdown: AtomicBool,
    progress: Arc<EmbeddingProgress>,
}

impl BackgroundEmbedder {
    /// Start the thread. The model is loaded by `load_model` on the first pass,
    /// so neither startup nor phase 1 waits for it.
    pub fn spawn(store: Arc<IndexStore>, load_model: ModelLoader) -> Result<Self> {
        let shared = Arc::new(Shared {
            dirty: Mutex::new(false),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            progress: Arc::new(EmbeddingProgress::new()),
        });
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name("semantiq-embedder".to_string())
            .spawn(move || worker.run(&store, load_model))?;
        Ok(Self { shared })
    }

    /// Ask for a pass: new chunks may be waiting for an embedding.
    pub fn notify(&self) {
        if let Ok(mut dirty) = self.shared.dirty.lock() {
            *dirty = true;
            self.shared.wake.notify_one();
        }
    }

    /// Live progress of the current pass.
    pub fn progress(&self) -> &Arc<EmbeddingProgress> {
        &self.shared.progress
    }

    /// Wait until no pass is running or requested (tests, shutdown).
    /// Returns false on timeout.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let requested = self.shared.dirty.lock().map(|d| *d).unwrap_or(false);
            if !requested && !self.shared.progress.is_running() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for BackgroundEmbedder {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
        self.shared.wake.notify_one();
    }
}

impl Shared {
    fn run(&self, store: &IndexStore, load_model: ModelLoader) {
        let mut load_model = Some(load_model);
        let mut model: Option<Box<dyn EmbeddingModel>> = None;

        loop {
            // Sleep until a pass is requested or the owner is dropped.
            {
                let Ok(mut dirty) = self.dirty.lock() else {
                    return;
                };
                while !*dirty && !self.shutdown.load(Ordering::Relaxed) {
                    dirty = match self.wake.wait(dirty) {
                        Ok(guard) => guard,
                        Err(_) => return,
                    };
                }
                if self.shutdown.load(Ordering::Relaxed) {
                    return;
                }
                *dirty = false;
                // Mark the pass as running before releasing the request, so
                // `wait_idle` never sees an idle gap between the two.
                self.progress.running.store(true, Ordering::Relaxed);
            }

            if model.is_none() {
                let Some(load) = load_model.take() else {
                    self.progress.finish();
                    continue;
                };
                match load() {
                    Ok(loaded) => model = Some(loaded),
                    Err(e) => {
                        warn!(
                            "Embedding model unavailable, semantic index not built: {}",
                            e
                        );
                        self.progress.finish();
                        continue;
                    }
                }
            }
            let Some(model) = model.as_deref() else {
                continue;
            };

            let start = Instant::now();
            let should_stop = || self.shutdown.load(Ordering::Relaxed);
            match embed_pending(
                store,
                model,
                DEFAULT_BATCH_SIZE,
                &self.progress,
                &should_stop,
                &mut |_| {},
            ) {
                Ok(result) if result.embedded > 0 || result.failed > 0 => info!(
                    "Embeddings: {} chunks embedded in {:.1}s ({} failed)",
                    result.embedded,
                    start.elapsed().as_secs_f64(),
                    result.failed
                ),
                Ok(_) => {}
                Err(e) => warn!("Embedding pass failed: {}", e),
            }
        }
    }
}
