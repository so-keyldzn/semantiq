//! Search and retrieval engine for semantic code search.
//!
//! This module provides the `RetrievalEngine` which combines multiple search
//! strategies (semantic, symbol, text) into a unified search interface.

mod analysis;
mod search;
mod threshold;

use crate::threshold::{CollectorConfig, DistanceCollector, ThresholdConfig};
use semantiq_embeddings::{EmbeddingModel, create_embedding_model};
use semantiq_index::IndexStore;
use std::sync::{Arc, RwLock};
use tracing::debug;

// Re-export types
pub use analysis::{DependencyInfo, SymbolDefinition, SymbolExplanation};

/// The main search and retrieval engine.
pub struct RetrievalEngine {
    pub(crate) store: Arc<IndexStore>,
    pub(crate) root_path: String,
    pub(crate) embedding_model: Option<Box<dyn EmbeddingModel>>,
    /// Adaptive threshold configuration (loaded from calibration).
    pub(crate) threshold_config: Arc<RwLock<ThresholdConfig>>,
    /// Distance collector for ML calibration (optional).
    pub(crate) distance_collector: Option<DistanceCollector>,
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
    pub fn with_options(store: Arc<IndexStore>, root_path: &str, enable_collection: bool) -> Self {
        // Try to load embedding model
        let embedding_model = match create_embedding_model(None) {
            Ok(model) => {
                debug!("Embedding model loaded (dim={})", model.dimension());
                Some(model)
            }
            Err(e) => {
                debug!("Failed to load embedding model: {}", e);
                None
            }
        };

        // Load calibrated thresholds from database
        let threshold_config = Self::load_thresholds_from_store(&store);

        // Create distance collector if enabled, initialized with existing count
        let distance_collector = if enable_collection {
            let existing_count = store
                .get_observation_counts()
                .map(|counts| counts.values().sum())
                .unwrap_or(0);

            let collector = DistanceCollector::with_config(CollectorConfig {
                buffer_size: 50,
                sample_rate: 0.1,
                max_age_days: 30,
                bootstrap_threshold: 500,
                enable_bootstrap: true,
            })
            .with_existing_count(existing_count);

            Some(collector)
        } else {
            None
        };

        Self {
            store,
            root_path: root_path.to_string(),
            embedding_model,
            threshold_config: Arc::new(RwLock::new(threshold_config)),
            distance_collector,
        }
    }

    /// Get the current threshold configuration.
    pub fn threshold_config(&self) -> Arc<RwLock<ThresholdConfig>> {
        Arc::clone(&self.threshold_config)
    }

    /// Get the distance collector (if enabled).
    pub fn distance_collector(&self) -> Option<&DistanceCollector> {
        self.distance_collector.as_ref()
    }

    /// Get bootstrap status information.
    pub fn bootstrap_status(&self) -> Option<(bool, u8, usize)> {
        self.distance_collector.as_ref().map(|c| {
            (
                c.is_bootstrap(),
                c.bootstrap_progress(),
                c.total_observations(),
            )
        })
    }
}

#[cfg(test)]
mod tests;
