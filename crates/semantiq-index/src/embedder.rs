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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Texts per forward pass (the ONNX model's own batch size).
pub const DEFAULT_BATCH_SIZE: usize = 32;

/// Pending chunks read per query.
const WINDOW_SIZE: usize = 8 * DEFAULT_BATCH_SIZE;

/// Live progress of phase 2, shared with the readers that report it.
///
/// `embedded` and `total` are packed into one atomic so a reader never sees
/// a `total` from one pass next to an `embedded` from another (which made
/// `pending()` underflow).
#[derive(Debug, Default)]
pub struct EmbeddingProgress {
    /// `total << 32 | embedded`.
    counts: AtomicU64,
    running: AtomicBool,
    /// Why the embedding model could not be loaded, until a load succeeds.
    model_error: Mutex<Option<String>>,
}

fn pack_counts(counts: EmbeddingCounts) -> u64 {
    let clamp = |n: usize| u64::from(u32::try_from(n).unwrap_or(u32::MAX));
    (clamp(counts.total) << 32) | clamp(counts.embedded)
}

/// `embedded` is clamped to `total`: `add` may push the low half past it
/// when chunks were inserted after the pass counted them.
fn unpack_counts(packed: u64) -> EmbeddingCounts {
    let total = (packed >> 32) as usize;
    EmbeddingCounts {
        embedded: ((packed & u64::from(u32::MAX)) as usize).min(total),
        total,
    }
}

impl EmbeddingProgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Chunks with an embedding / all chunks, as of the last update.
    pub fn counts(&self) -> EmbeddingCounts {
        unpack_counts(self.counts.load(Ordering::Relaxed))
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

    /// Why the embedding model is unavailable (last load attempt failed),
    /// `None` once it is loaded or before the first attempt.
    pub fn model_error(&self) -> Option<String> {
        self.model_error.lock().ok().and_then(|e| e.clone())
    }

    fn set_model_error(&self, error: Option<String>) {
        if let Ok(mut slot) = self.model_error.lock() {
            *slot = error;
        }
    }

    fn start(&self, counts: EmbeddingCounts) {
        self.counts.store(pack_counts(counts), Ordering::Relaxed);
        self.running.store(true, Ordering::Relaxed);
    }

    /// One atomic add on the low half; `unpack_counts` clamps it to `total`.
    fn add(&self, embedded: usize) {
        let embedded = u64::from(u32::try_from(embedded).unwrap_or(u32::MAX));
        self.counts.fetch_add(embedded, Ordering::Relaxed);
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
/// Called again, after a backoff, when a load fails (download, disk, digest);
/// the thread owns it and calls it sequentially.
pub type ModelLoader = Box<dyn FnMut() -> Result<Box<dyn EmbeddingModel>> + Send>;

/// Timing of the background thread.
#[derive(Debug, Clone, Copy)]
pub struct EmbedderConfig {
    /// Without a notification, how often the thread checks the database for
    /// chunks left pending by another process (`semantiq index
    /// --no-embeddings`, a query command's refresh) and retries a failed
    /// model load once its backoff has elapsed.
    pub poll_interval: Duration,
    /// Wait before the first retry of a failed model load; doubles on each
    /// failure up to `max_retry_delay`.
    pub initial_retry_delay: Duration,
    pub max_retry_delay: Duration,
}

impl Default for EmbedderConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(30),
            initial_retry_delay: Duration::from_secs(30),
            max_retry_delay: Duration::from_secs(15 * 60),
        }
    }
}

/// Runs phase 2 on a dedicated thread, woken by [`BackgroundEmbedder::notify`]
/// after each phase 1 write and on its own every `poll_interval` (chunks
/// another process left pending, model load retry). Stops when dropped.
pub struct BackgroundEmbedder {
    shared: Arc<Shared>,
}

struct Shared {
    /// Set by `notify`, cleared when a pass starts.
    dirty: Mutex<bool>,
    wake: Condvar,
    shutdown: AtomicBool,
    progress: Arc<EmbeddingProgress>,
    config: EmbedderConfig,
}

impl BackgroundEmbedder {
    /// Start the thread with the default timing. The model is loaded by
    /// `load_model` on the first pass, so neither startup nor phase 1 waits
    /// for it.
    pub fn spawn(store: Arc<IndexStore>, load_model: ModelLoader) -> Result<Self> {
        Self::spawn_with(store, load_model, EmbedderConfig::default())
    }

    /// `spawn` with explicit timing (tests, tuning).
    pub fn spawn_with(
        store: Arc<IndexStore>,
        load_model: ModelLoader,
        config: EmbedderConfig,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            dirty: Mutex::new(false),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            progress: Arc::new(EmbeddingProgress::new()),
            config,
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

/// Backoff of the model load: after a failure, the next attempt waits
/// `delay`, which doubles on every failure up to the configured maximum.
struct ModelRetry {
    next_attempt: Option<Instant>,
    delay: Duration,
    config: EmbedderConfig,
}

impl ModelRetry {
    fn new(config: EmbedderConfig) -> Self {
        Self {
            next_attempt: None,
            delay: config.initial_retry_delay,
            config,
        }
    }

    fn due(&self) -> bool {
        self.next_attempt.is_none_or(|at| Instant::now() >= at)
    }

    /// Record a failure; returns the wait before the next attempt.
    fn failed(&mut self) -> Duration {
        let wait = self.delay;
        self.next_attempt = Some(Instant::now() + wait);
        self.delay = (self.delay * 2).min(self.config.max_retry_delay);
        wait
    }

    fn succeeded(&mut self) {
        self.next_attempt = None;
        self.delay = self.config.initial_retry_delay;
    }
}

/// Why the thread left its wait.
enum Wake {
    /// `notify` was called: a pass is due whatever the database says.
    Requested,
    /// `poll_interval` elapsed with no request.
    Poll,
    Shutdown,
}

impl Shared {
    /// Sleep until a pass is requested, the poll interval elapses or the owner
    /// is dropped. On `Requested`, the pass is marked running before the request
    /// is released so `wait_idle` never sees an idle gap between the two.
    fn wait(&self) -> Wake {
        let Ok(mut dirty) = self.dirty.lock() else {
            return Wake::Shutdown;
        };
        let deadline = Instant::now() + self.config.poll_interval;
        while !*dirty && !self.shutdown.load(Ordering::Relaxed) {
            let now = Instant::now();
            if now >= deadline {
                return Wake::Poll;
            }
            dirty = match self.wake.wait_timeout(dirty, deadline - now) {
                Ok((guard, _)) => guard,
                Err(_) => return Wake::Shutdown,
            };
        }
        if self.shutdown.load(Ordering::Relaxed) {
            return Wake::Shutdown;
        }
        *dirty = false;
        self.progress.running.store(true, Ordering::Relaxed);
        Wake::Requested
    }

    fn run(&self, store: &IndexStore, mut load_model: ModelLoader) {
        let mut model: Option<Box<dyn EmbeddingModel>> = None;
        let mut retry = ModelRetry::new(self.config);
        // `PRAGMA data_version` read before the chunks were last counted or
        // embedded. It changes only when another connection commits (CLI
        // refresh, `semantiq index --no-embeddings`): this process's own
        // writes come with a `notify`. A poll that finds it unchanged touches
        // no table, so an idle server never scans `chunks` under the lock.
        let mut seen_version: Option<i64> = None;

        loop {
            // Read before the pass: a commit landing during the pass changes
            // the version again and is counted by the next poll.
            let version = match self.wait() {
                Wake::Shutdown => return,
                Wake::Requested => store.data_version().ok(),
                Wake::Poll => {
                    let version = match store.data_version() {
                        Ok(version) => version,
                        Err(e) => {
                            debug!("Embedding poll skipped: {}", e);
                            continue;
                        }
                    };
                    if seen_version == Some(version) {
                        continue;
                    }
                    match store.embedding_counts() {
                        Ok(counts) if counts.pending() > 0 => {
                            debug!("Embedding poll: {} chunks pending", counts.pending());
                        }
                        Ok(_) => {
                            seen_version = Some(version);
                            continue;
                        }
                        Err(e) => {
                            debug!("Embedding poll skipped: {}", e);
                            continue;
                        }
                    }
                    self.progress.running.store(true, Ordering::Relaxed);
                    Some(version)
                }
            };

            if model.is_none() {
                if !retry.due() {
                    self.progress.finish();
                    continue;
                }
                match load_model() {
                    Ok(loaded) => {
                        model = Some(loaded);
                        retry.succeeded();
                        self.progress.set_model_error(None);
                    }
                    Err(e) => {
                        let wait = retry.failed();
                        warn!(
                            "Embedding model unavailable, semantic index not built \
                             (next attempt in {}s): {}",
                            wait.as_secs(),
                            e
                        );
                        self.progress.set_model_error(Some(e.to_string()));
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
            seen_version = version;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_counts_are_read_as_one_unit() {
        let progress = EmbeddingProgress::new();
        progress.start(EmbeddingCounts {
            embedded: 3,
            total: 10,
        });
        assert_eq!(progress.counts().pending(), 7);
        progress.add(5);
        assert_eq!(progress.counts().embedded, 8);
        // Never past the total, even if a batch over-reports.
        progress.add(50);
        assert_eq!(progress.counts().embedded, 10);
        assert_eq!(progress.counts().pending(), 0);
        assert!(progress.in_progress().is_none());
    }

    /// Readers between two `start` calls used to see `total` from one pass and
    /// `embedded` from the other; `pending()` underflowed. The packed counter
    /// can only be observed in a state some writer stored.
    #[test]
    fn progress_never_shows_more_embedded_than_total_under_concurrent_updates() {
        let progress = Arc::new(EmbeddingProgress::new());
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let progress = Arc::clone(&progress);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                let mut i = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    // Alternate a big index with a tiny one: with two
                    // separate atomics, `embedded = 900` next to `total = 5`
                    // was observable.
                    if i.is_multiple_of(2) {
                        progress.start(EmbeddingCounts {
                            embedded: 900,
                            total: 1000,
                        });
                    } else {
                        progress.start(EmbeddingCounts {
                            embedded: 0,
                            total: 5,
                        });
                    }
                    progress.add(1);
                    i += 1;
                }
            })
        };
        for _ in 0..200_000 {
            let counts = progress.counts();
            assert!(
                counts.embedded <= counts.total,
                "embedded {} > total {}",
                counts.embedded,
                counts.total
            );
            assert!(counts.percent() <= 100);
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }

    #[test]
    fn pack_round_trips_and_clamps() {
        let counts = EmbeddingCounts {
            embedded: 123_456,
            total: 7_890_123,
        };
        let back = unpack_counts(pack_counts(counts));
        assert_eq!(back.embedded, counts.embedded);
        assert_eq!(back.total, counts.total);
        let huge = unpack_counts(pack_counts(EmbeddingCounts {
            embedded: usize::MAX,
            total: usize::MAX,
        }));
        assert_eq!(huge.embedded, u32::MAX as usize);
        assert_eq!(huge.total, u32::MAX as usize);
    }

    #[test]
    fn model_retry_backs_off_and_resets() {
        let config = EmbedderConfig {
            poll_interval: Duration::from_millis(10),
            initial_retry_delay: Duration::from_millis(100),
            max_retry_delay: Duration::from_millis(300),
        };
        let mut retry = ModelRetry::new(config);
        assert!(retry.due());
        assert_eq!(retry.failed(), Duration::from_millis(100));
        assert!(!retry.due());
        assert_eq!(retry.failed(), Duration::from_millis(200));
        assert_eq!(retry.failed(), Duration::from_millis(300));
        assert_eq!(retry.failed(), Duration::from_millis(300), "capped");
        retry.succeeded();
        assert!(retry.due());
        assert_eq!(retry.failed(), Duration::from_millis(100), "reset");
    }
}
