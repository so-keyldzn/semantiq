//! Search and retrieval engine for semantic code search.
//!
//! This module provides the `RetrievalEngine` which combines multiple search
//! strategies (semantic, symbol, text) into a unified search interface.

mod analysis;
mod calls;
mod dead_code;
mod hierarchy;
mod impact;
mod resolution;
mod search;
mod threshold;

use crate::repo_map::RankedRepo;
use crate::threshold::{CollectorConfig, DistanceCollector, ThresholdConfig};
use semantiq_embeddings::{EmbeddingModel, ModelSlot, RetryPolicy, create_embedding_model};
use semantiq_index::IndexStore;
use std::sync::{Arc, Mutex, RwLock};
use tracing::warn;

/// Accept a project-relative path argument written with either separator
/// (`src/a.rs`, `src\\a.rs`, `./src/a.rs`) and return the form the index
/// stores: `/` separators, no leading `./`, trimmed.
///
/// Every engine entry point taking a file path or a path prefix (`deps`,
/// `impact`, `calls`, `dead_code`, the repo map focus and prefix) goes through
/// this, so the CLI, the MCP tools and the HTTP API accept both styles.
pub fn normalize_input_path(value: &str) -> String {
    let value = value.trim().replace('\\', "/");
    value.trim_start_matches("./").to_string()
}

// Re-export types
pub use analysis::{DependencyInfo, SymbolDefinition, SymbolExplanation};
pub use calls::{
    CallDirection, CallGraph, CallSite, DEFAULT_CALL_DEPTH, DEFAULT_CALL_EDGES, MAX_CALL_DEPTH,
    MAX_CALL_EDGES,
};
pub use dead_code::{
    DEFAULT_DEAD_CODE_LIMIT, DeadCodeConfidence, DeadCodeExclusions, DeadCodeOptions,
    DeadCodeReport, DeadSymbol, MAX_DEAD_CODE_LIMIT,
};
pub use hierarchy::{
    DEFAULT_HIERARCHY_DEPTH, DEFAULT_HIERARCHY_EDGES, MAX_HIERARCHY_DEPTH, MAX_HIERARCHY_EDGES,
    TypeEdge, TypeHierarchy,
};
pub use impact::{
    DEFAULT_IMPACT_DEPTH, DEFAULT_IMPACT_SITES, EnclosingSymbol, ImpactAnalysis, ImpactConfidence,
    ImpactDefinition, ImpactSite, MAX_IMPACT_DEPTH, MAX_IMPACT_SITES, is_test_location,
};

/// The main search and retrieval engine.
pub struct RetrievalEngine {
    pub(crate) store: Arc<IndexStore>,
    pub(crate) root_path: String,
    /// The embedding model, loaded on the first search that needs it and
    /// retried with a backoff after a failure (#76). Shared with the
    /// background embedder by `serve`, so the model is loaded once.
    pub(crate) model_slot: Arc<ModelSlot>,
    /// Adaptive threshold configuration (loaded from calibration).
    pub(crate) threshold_config: Arc<RwLock<ThresholdConfig>>,
    /// Distance collector for ML calibration (optional).
    pub(crate) distance_collector: Option<DistanceCollector>,
    /// Repo map ranking without focus, keyed by the index fingerprint.
    pub(crate) repo_map_cache: Mutex<Option<(String, Arc<RankedRepo>)>>,
}

impl RetrievalEngine {
    /// Create a new RetrievalEngine with distance collection enabled.
    pub fn new(store: Arc<IndexStore>, root_path: &str) -> Self {
        Self::with_options(store, root_path, true)
    }

    /// Create a new RetrievalEngine with optional distance collection.
    ///
    /// When `enable_collection` is true, distance observations are collected
    /// during semantic search for later ML calibration.
    ///
    /// The embedding model is not loaded here: the first `search()` (or
    /// `embedding_model()`) loads it, so a server answers `initialize`
    /// before the first download, and a failed load is retried after a
    /// backoff instead of leaving semantic search off until a restart.
    pub fn with_options(store: Arc<IndexStore>, root_path: &str, enable_collection: bool) -> Self {
        let slot = ModelSlot::new(
            Box::new(|| create_embedding_model(None)),
            RetryPolicy::default(),
        );
        Self::build(store, root_path, Arc::new(slot), enable_collection)
    }

    /// `with_options` with the caller's model slot: `serve` hands the same
    /// slot to the background embedder; tests inject a loader.
    pub fn with_model_slot(
        store: Arc<IndexStore>,
        root_path: &str,
        slot: Arc<ModelSlot>,
        enable_collection: bool,
    ) -> Self {
        Self::build(store, root_path, slot, enable_collection)
    }

    /// Create a RetrievalEngine without an embedding model or distance collection.
    ///
    /// Loading the ONNX model is the dominant startup cost and only `search()`
    /// uses it (its semantic strategy is then skipped). Short-lived callers that
    /// only need references, dependencies, explanations or impact (e.g. one-shot
    /// CLI commands) use this to answer without paying for the model.
    pub fn without_embeddings(store: Arc<IndexStore>, root_path: &str) -> Self {
        Self::build(store, root_path, Arc::new(ModelSlot::disabled()), false)
    }

    fn build(
        store: Arc<IndexStore>,
        root_path: &str,
        model_slot: Arc<ModelSlot>,
        enable_collection: bool,
    ) -> Self {
        // Load calibrated thresholds from database
        let threshold_config = Self::load_thresholds_from_store(&store);

        // Surface a warning if the vector index has lost its referential
        // integrity with the chunks table. Orphan vectors silently dominate
        // the KNN top-k for distant queries; this log is the canary.
        //
        // Note: this acquires the same connection mutex as the AutoIndexer.
        // The engine is constructed before any background indexing task is
        // spawned, so contention here is not expected. If that invariant is
        // ever broken, this call would block on the indexer transaction.
        match store.count_orphan_chunk_vectors() {
            Ok(0) => {}
            Ok(n) => warn!(
                orphan_chunk_vectors = n,
                "chunks_vec contains orphan rows (chunk_id not in chunks); \
                 semantic search may return stale results. Run `semantiq index --force` to repair."
            ),
            Err(e) => warn!("could not count orphan chunk vectors: {e}"),
        }
        // The opposite direction: chunks marked embedded without a vector are
        // skipped by phase 2 and absent from semantic search.
        match store.count_chunks_missing_vectors() {
            Ok(0) => {}
            Ok(n) => warn!(
                chunks_missing_vectors = n,
                "chunks have an embedding but no chunks_vec row; they are missing from \
                 semantic search. Run `semantiq index --force` to repair."
            ),
            Err(e) => warn!("could not count chunks missing vectors: {e}"),
        }

        // Create distance collector if enabled, initialized with existing count
        let distance_collector = if enable_collection {
            let existing_count = store
                .get_observation_counts()
                .map(|counts| counts.values().sum())
                .unwrap_or(0);

            let collector = DistanceCollector::with_config(CollectorConfig {
                buffer_size: 50,
                ..CollectorConfig::default()
            })
            .with_existing_count(existing_count);

            Some(collector)
        } else {
            None
        };

        Self {
            store,
            root_path: root_path.to_string(),
            model_slot,
            threshold_config: Arc::new(RwLock::new(threshold_config)),
            distance_collector,
            repo_map_cache: Mutex::new(None),
        }
    }

    /// The embedding model, loading it on this call if it is not yet (the
    /// first download happens here for a CLI `search`). `None` when the slot
    /// is disabled (`without_embeddings`), while another thread loads it, or
    /// after a failed load until its backoff elapses; the slot's `status()`
    /// says which.
    pub fn embedding_model(&self) -> Option<Arc<dyn EmbeddingModel>> {
        self.model_slot.get_or_load().ok()
    }

    /// The slot holding the embedding model (status, sharing with the
    /// background embedder).
    pub fn model_slot(&self) -> &Arc<ModelSlot> {
        &self.model_slot
    }

    /// Get the current threshold configuration.
    pub fn threshold_config(&self) -> Arc<RwLock<ThresholdConfig>> {
        Arc::clone(&self.threshold_config)
    }

    /// Get the distance collector (if enabled).
    pub fn distance_collector(&self) -> Option<&DistanceCollector> {
        self.distance_collector.as_ref()
    }
}

#[cfg(test)]
mod structure_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod input_path_tests {
    use super::normalize_input_path;

    /// #68: a path argument is accepted with either separator.
    #[test]
    fn accepts_both_separators() {
        assert_eq!(normalize_input_path("src/lib/foo.ts"), "src/lib/foo.ts");
        assert_eq!(normalize_input_path(r"src\lib\foo.ts"), "src/lib/foo.ts");
        assert_eq!(normalize_input_path(r".\src\foo.ts"), "src/foo.ts");
        assert_eq!(normalize_input_path(" ./src/foo.ts "), "src/foo.ts");
        assert_eq!(normalize_input_path(r"crates\"), "crates/");
        assert_eq!(normalize_input_path(""), "");
    }
}
