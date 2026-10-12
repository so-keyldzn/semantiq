//! One embedding model shared by everything in a process, loaded on first
//! use and retried with a backoff when the load fails.
//!
//! `serve` used to hold two copies: the search engine loaded one eagerly at
//! startup (download included, before the MCP transport was open) and never
//! retried, while the background embedder loaded its own and retried. A
//! [`ModelSlot`] holds a single `Arc<dyn EmbeddingModel>`: the embedder
//! thread fills it after phase 1 (`load_now`), a search picks it up
//! (`get_or_load`), and whichever side loads it first serves both (#76,
//! #97). The ONNX session is already behind a mutex, so sharing is safe; a
//! query waits at most for one batch of the embedder.

use crate::model::{EmbeddingModel, public_load_reason};
use anyhow::Result;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Builds the model. Called on the thread that needs it, never twice at
/// once; may block (first download). Errors are shown to clients through
/// [`public_load_reason`], so returning a [`crate::ModelLoadError`] gives a
/// precise message and any other error a generic one.
pub type ModelLoaderFn = Box<dyn Fn() -> Result<Box<dyn EmbeddingModel>> + Send + Sync>;

/// Backoff between load attempts: `initial_delay` after the first failure,
/// doubling up to `max_delay`, reset by a success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    /// The same timing as the background embedder.
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(30),
            max_delay: Duration::from_secs(15 * 60),
        }
    }
}

/// What a slot holds right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStatus {
    /// This slot never loads a model (engines built `without_embeddings`).
    Disabled,
    /// No attempt yet: the first `get_or_load` makes one.
    NotLoaded,
    /// A load is under way on some thread.
    Loading,
    Loaded,
    /// The last attempt failed. `reason` is safe to show to a client; the
    /// next attempt is accepted after `retry_in`.
    Failed {
        reason: String,
        retry_in: Duration,
    },
}

/// Why [`ModelSlot::get_or_load`] returned no model. `Display` is safe to
/// show to a client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelUnavailable {
    #[error("no embedding model is loaded in this context")]
    Disabled,
    #[error("the embedding model is being loaded")]
    Loading,
    /// The last attempt failed with `reason`; `get_or_load` waits for the
    /// backoff before trying again.
    #[error("{reason}")]
    Failed { reason: String },
}

struct State {
    model: Option<Arc<dyn EmbeddingModel>>,
    loading: bool,
    last_error: Option<String>,
    next_attempt: Option<Instant>,
    delay: Duration,
}

/// A lazily loaded, shared, retryable embedding model. See the module doc.
pub struct ModelSlot {
    loader: Option<ModelLoaderFn>,
    policy: RetryPolicy,
    state: Mutex<State>,
    /// Signalled when a load finishes, for `load_now` callers waiting on
    /// another thread's attempt.
    settled: Condvar,
}

impl std::fmt::Debug for ModelSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelSlot")
            .field("status", &self.status())
            .finish()
    }
}

impl ModelSlot {
    /// A slot filled by `loader` on first use.
    pub fn new(loader: ModelLoaderFn, policy: RetryPolicy) -> Self {
        Self::build(Some(loader), policy)
    }

    /// A slot that never loads anything: `get_or_load` is
    /// [`ModelUnavailable::Disabled`] and nothing is logged.
    pub fn disabled() -> Self {
        Self::build(None, RetryPolicy::default())
    }

    /// A slot already holding `model` (tests, callers that built their own).
    pub fn preloaded(model: Box<dyn EmbeddingModel>) -> Self {
        let slot = Self::build(None, RetryPolicy::default());
        if let Ok(mut state) = slot.state.lock() {
            state.model = Some(Arc::from(model));
        }
        slot
    }

    fn build(loader: Option<ModelLoaderFn>, policy: RetryPolicy) -> Self {
        Self {
            loader,
            policy,
            state: Mutex::new(State {
                model: None,
                loading: false,
                last_error: None,
                next_attempt: None,
                delay: policy.initial_delay,
            }),
            settled: Condvar::new(),
        }
    }

    /// The model if it is loaded; never triggers a load.
    pub fn model(&self) -> Option<Arc<dyn EmbeddingModel>> {
        self.state.lock().ok().and_then(|state| state.model.clone())
    }

    pub fn status(&self) -> ModelStatus {
        let Ok(state) = self.state.lock() else {
            return ModelStatus::Disabled;
        };
        if state.model.is_some() {
            ModelStatus::Loaded
        } else if state.loading {
            ModelStatus::Loading
        } else if let Some(reason) = &state.last_error {
            ModelStatus::Failed {
                reason: reason.clone(),
                retry_in: state
                    .next_attempt
                    .map(|at| at.saturating_duration_since(Instant::now()))
                    .unwrap_or_default(),
            }
        } else if self.loader.is_none() {
            ModelStatus::Disabled
        } else {
            ModelStatus::NotLoaded
        }
    }

    /// The model, loading it on this thread if no attempt is in flight and
    /// the backoff of the last failure has elapsed. Does not wait for a load
    /// running on another thread: a search answers without its semantic
    /// strategy rather than block behind a download.
    pub fn get_or_load(&self) -> std::result::Result<Arc<dyn EmbeddingModel>, ModelUnavailable> {
        match self.claim(false)? {
            Claim::Ready(model) => Ok(model),
            Claim::Go => self.attempt(),
        }
    }

    /// The model, loading it now whatever the backoff says (the embedder
    /// thread's own schedule), or waiting for the attempt another thread is
    /// making and taking its outcome.
    pub fn load_now(&self) -> std::result::Result<Arc<dyn EmbeddingModel>, ModelUnavailable> {
        match self.claim(true)? {
            Claim::Ready(model) => Ok(model),
            Claim::Go => self.attempt(),
        }
    }

    /// Decide, under the lock, whether the caller gets the model, must load
    /// it, or leaves empty-handed.
    fn claim(&self, force: bool) -> std::result::Result<Claim, ModelUnavailable> {
        if self.loader.is_none() {
            return match self.model() {
                Some(model) => Ok(Claim::Ready(model)),
                None => Err(ModelUnavailable::Disabled),
            };
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(model) = &state.model {
                return Ok(Claim::Ready(Arc::clone(model)));
            }
            if state.loading {
                if !force {
                    return Err(ModelUnavailable::Loading);
                }
                state = self.settled.wait(state).unwrap_or_else(|e| e.into_inner());
                continue;
            }
            let due = state.next_attempt.is_none_or(|at| Instant::now() >= at);
            if !force && !due {
                return Err(ModelUnavailable::Failed {
                    reason: state.last_error.clone().unwrap_or_default(),
                });
            }
            state.loading = true;
            return Ok(Claim::Go);
        }
    }

    /// Run the loader outside the lock and record the outcome. A panic in
    /// the loader (ort, tokenizers) counts as a failure: the slot stays
    /// usable and the next attempt is scheduled like any other.
    fn attempt(&self) -> std::result::Result<Arc<dyn EmbeddingModel>, ModelUnavailable> {
        let loader = self.loader.as_ref().expect("claim() checked the loader");
        let outcome = match catch_unwind(AssertUnwindSafe(loader)) {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("the embedding model loader panicked")),
        };

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.loading = false;
        let result = match outcome {
            Ok(model) => {
                let model: Arc<dyn EmbeddingModel> = Arc::from(model);
                info!("Embedding model loaded (dim={})", model.dimension());
                state.model = Some(Arc::clone(&model));
                state.last_error = None;
                state.next_attempt = None;
                state.delay = self.policy.initial_delay;
                Ok(model)
            }
            Err(e) => {
                let wait = state.delay;
                state.next_attempt = Some(Instant::now() + wait);
                state.delay = (state.delay * 2).min(self.policy.max_delay);
                let reason = public_load_reason(&e);
                warn!(
                    "Embedding model unavailable, semantic search is off (next attempt in {}s): {:#}",
                    wait.as_secs(),
                    e
                );
                state.last_error = Some(reason.clone());
                Err(ModelUnavailable::Failed { reason })
            }
        };
        drop(state);
        self.settled.notify_all();
        result
    }
}

enum Claim {
    Ready(Arc<dyn EmbeddingModel>),
    Go,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StubEmbeddingModel;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn quick() -> RetryPolicy {
        RetryPolicy {
            initial_delay: Duration::from_millis(40),
            max_delay: Duration::from_millis(100),
        }
    }

    /// A loader that fails `failures` times, then returns the stub.
    fn flaky(failures: usize) -> (ModelLoaderFn, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let loader: ModelLoaderFn = Box::new(move || {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            if n < failures {
                Err(crate::ModelLoadError::Download(anyhow::anyhow!(
                    "https://example.invalid/model.onnx: attempt {n} timed out"
                ))
                .into())
            } else {
                Ok(Box::new(StubEmbeddingModel::new()) as Box<dyn EmbeddingModel>)
            }
        });
        (loader, calls)
    }

    #[test]
    fn loads_on_first_use_and_caches() {
        let (loader, calls) = flaky(0);
        let slot = ModelSlot::new(loader, quick());
        assert_eq!(slot.status(), ModelStatus::NotLoaded);
        assert!(slot.model().is_none(), "model() never loads");
        let a = slot.get_or_load().expect("loaded");
        let b = slot.get_or_load().expect("loaded");
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(slot.status(), ModelStatus::Loaded);
    }

    /// #76: a failed load is retried after the backoff, without a restart,
    /// and the reason exposed meanwhile carries no URL (#100). The waits read
    /// the slot's own `retry_in` instead of assuming how long a `sleep` takes,
    /// and the delays leave a slow runner (macOS CI) room between two
    /// consecutive calls: a 60 ms sleep against an 80 ms backoff overshot.
    #[test]
    fn retries_after_backoff_and_recovers() {
        let (loader, calls) = flaky(2);
        let policy = RetryPolicy {
            initial_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(1),
        };
        let slot = ModelSlot::new(loader, policy);

        let err = slot.get_or_load().err().expect("unavailable");
        let ModelUnavailable::Failed { reason } = &err else {
            panic!("{err:?}");
        };
        assert!(reason.contains("download failed"), "{reason}");
        assert!(!reason.contains("example.invalid"), "{reason}");
        let ModelStatus::Failed {
            retry_in: first_wait,
            ..
        } = slot.status()
        else {
            panic!("{:?}", slot.status());
        };
        assert!(first_wait <= Duration::from_millis(200), "{first_wait:?}");

        // Within the backoff: no new attempt, same reason.
        assert_eq!(slot.get_or_load().err().expect("unavailable"), err);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        std::thread::sleep(first_wait);
        assert!(slot.get_or_load().is_err(), "second failure");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // The delay doubled (400 ms): the next attempt is further away than
        // the first one was, and not due yet.
        let ModelStatus::Failed {
            retry_in: second_wait,
            ..
        } = slot.status()
        else {
            panic!("{:?}", slot.status());
        };
        assert!(
            second_wait > first_wait,
            "{second_wait:?} <= {first_wait:?}"
        );
        assert!(slot.get_or_load().is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        std::thread::sleep(second_wait);

        assert!(slot.get_or_load().is_ok(), "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(slot.status(), ModelStatus::Loaded);
    }

    /// `load_now` ignores the backoff (the embedder's schedule) and fills
    /// the slot for `get_or_load` callers.
    #[test]
    fn load_now_ignores_the_backoff() {
        let (loader, calls) = flaky(1);
        let slot = ModelSlot::new(loader, RetryPolicy::default());
        assert!(slot.get_or_load().is_err());
        assert!(slot.get_or_load().is_err(), "30 s backoff");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(slot.load_now().is_ok());
        assert!(slot.get_or_load().is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// While one thread loads, `get_or_load` does not block (`Loading`) and
    /// `load_now` waits for that attempt instead of starting a second one.
    #[test]
    fn a_load_in_flight_is_not_duplicated() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let slot = Arc::new(ModelSlot::new(
            Box::new(move || {
                seen.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(150));
                Ok(Box::new(StubEmbeddingModel::new()) as Box<dyn EmbeddingModel>)
            }),
            quick(),
        ));
        let first = {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || slot.load_now().is_ok())
        };
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(slot.status(), ModelStatus::Loading);
        assert_eq!(
            slot.get_or_load().err().expect("unavailable"),
            ModelUnavailable::Loading
        );
        assert!(slot.load_now().is_ok(), "waited for the first attempt");
        assert!(first.join().unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn panicking_loader_is_a_failure_not_a_poison() {
        let slot = ModelSlot::new(Box::new(|| panic!("ort exploded")), quick());
        let err = slot.get_or_load().err().expect("unavailable");
        assert!(matches!(err, ModelUnavailable::Failed { .. }), "{err:?}");
        assert!(matches!(slot.status(), ModelStatus::Failed { .. }));
        // Still usable: the next call is a regular (retried) failure.
        std::thread::sleep(Duration::from_millis(50));
        assert!(matches!(
            slot.get_or_load(),
            Err(ModelUnavailable::Failed { .. })
        ));
    }

    #[test]
    fn disabled_and_preloaded_slots() {
        let disabled = ModelSlot::disabled();
        assert_eq!(disabled.status(), ModelStatus::Disabled);
        assert_eq!(
            disabled.get_or_load().err().expect("unavailable"),
            ModelUnavailable::Disabled
        );
        assert_eq!(
            disabled.load_now().err().expect("unavailable"),
            ModelUnavailable::Disabled
        );

        let preloaded = ModelSlot::preloaded(Box::new(StubEmbeddingModel::new()));
        assert_eq!(preloaded.status(), ModelStatus::Loaded);
        assert!(preloaded.get_or_load().expect("loaded").is_stub());
    }
}
