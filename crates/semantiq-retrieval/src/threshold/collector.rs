//! Distance observation collection during search.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// A single distance observation recorded during search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistanceObservation {
    /// Programming language of the matched chunk.
    pub language: String,
    /// L2 distance from query to chunk.
    pub distance: f32,
    /// Hash of the query (for deduplication).
    pub query_hash: u64,
    /// Unix timestamp when the observation was recorded.
    pub timestamp: i64,
}

impl DistanceObservation {
    /// Create a new observation.
    pub fn new(language: String, distance: f32, query_hash: u64) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Self {
            language,
            distance,
            query_hash,
            timestamp,
        }
    }

    /// Compute a hash for a query string.
    pub fn hash_query(query: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        query.hash(&mut hasher);
        hasher.finish()
    }
}

/// Configuration for the distance collector.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// Maximum number of observations to buffer before flushing to database.
    pub buffer_size: usize,
    /// Sampling rate in production mode (0.0-1.0).
    pub sample_rate: f32,
    /// Maximum age of observations in days before cleanup.
    pub max_age_days: i64,
    /// Number of observations to collect before exiting bootstrap mode.
    /// During bootstrap, sample_rate is 100%.
    pub bootstrap_threshold: usize,
    /// Whether to enable bootstrap mode.
    pub enable_bootstrap: bool,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            buffer_size: 100,
            sample_rate: 0.1, // 10% sampling in production
            max_age_days: 30,
            bootstrap_threshold: 500, // Collect 500 observations before switching to production
            enable_bootstrap: true,
        }
    }
}

/// Collects distance observations during semantic search.
///
/// The collector supports a "bootstrap" mode where it collects 100% of observations
/// until a threshold is reached, then switches to production mode with lower sampling.
pub struct DistanceCollector {
    /// Buffered observations not yet flushed.
    buffer: Mutex<Vec<DistanceObservation>>,
    /// Configuration.
    config: CollectorConfig,
    /// Counter for sampling in production mode.
    sample_counter: Mutex<u64>,
    /// Whether we're still in bootstrap mode.
    in_bootstrap: AtomicBool,
    /// Total observations collected (for bootstrap threshold).
    total_observations: AtomicUsize,
    /// Flag indicating calibration should be triggered.
    needs_calibration: AtomicBool,
}

impl DistanceCollector {
    /// Create a new collector with default configuration.
    pub fn new() -> Self {
        Self::with_config(CollectorConfig::default())
    }

    /// Create a new collector with custom configuration.
    pub fn with_config(config: CollectorConfig) -> Self {
        let in_bootstrap = config.enable_bootstrap;
        Self {
            buffer: Mutex::new(Vec::with_capacity(config.buffer_size)),
            config,
            sample_counter: Mutex::new(0),
            in_bootstrap: AtomicBool::new(in_bootstrap),
            total_observations: AtomicUsize::new(0),
            needs_calibration: AtomicBool::new(false),
        }
    }

    /// Create a collector that starts in production mode (no bootstrap).
    pub fn production(config: CollectorConfig) -> Self {
        Self {
            buffer: Mutex::new(Vec::with_capacity(config.buffer_size)),
            in_bootstrap: AtomicBool::new(false),
            total_observations: AtomicUsize::new(config.bootstrap_threshold + 1),
            needs_calibration: AtomicBool::new(false),
            sample_counter: Mutex::new(0),
            config,
        }
    }

    /// Initialize the collector with existing observation count from database.
    pub fn with_existing_count(mut self, count: usize) -> Self {
        self.total_observations = AtomicUsize::new(count);
        if count >= self.config.bootstrap_threshold {
            self.in_bootstrap = AtomicBool::new(false);
            info!(
                count = count,
                threshold = self.config.bootstrap_threshold,
                "Starting in production mode (enough observations)"
            );
        } else {
            info!(
                count = count,
                threshold = self.config.bootstrap_threshold,
                remaining = self.config.bootstrap_threshold - count,
                "Starting in bootstrap mode"
            );
        }
        self
    }

    /// Record distance observations from a search result.
    ///
    /// In bootstrap mode, collects all observations.
    /// In production mode, samples based on configured rate.
    /// Returns `true` if observations were actually recorded.
    pub fn record(
        &self,
        query: &str,
        results: &[(i64, f32)],
        language_lookup: impl Fn(i64) -> Option<String>,
    ) -> bool {
        // Apply sampling (100% in bootstrap, configured rate in production)
        if !self.should_sample() {
            return false;
        }

        let query_hash = DistanceObservation::hash_query(query);
        let mut recorded_count = 0;

        {
            let mut buffer = self.buffer.lock().unwrap_or_else(|e| {
                warn!("DistanceCollector mutex was poisoned, recovering");
                e.into_inner()
            });

            for (chunk_id, distance) in results {
                if let Some(language) = language_lookup(*chunk_id) {
                    buffer.push(DistanceObservation::new(language, *distance, query_hash));
                    recorded_count += 1;
                }
            }
        }

        // Update total count and check bootstrap threshold
        if recorded_count > 0 {
            let new_total = self
                .total_observations
                .fetch_add(recorded_count, Ordering::Relaxed)
                + recorded_count;

            // Check if we should exit bootstrap mode
            if self.in_bootstrap.load(Ordering::Relaxed)
                && new_total >= self.config.bootstrap_threshold
            {
                self.exit_bootstrap();
            }
        }

        true
    }

    /// Exit bootstrap mode and switch to production sampling.
    fn exit_bootstrap(&self) {
        if self
            .in_bootstrap
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            info!(
                total = self.total_observations.load(Ordering::Relaxed),
                "Exiting bootstrap mode, switching to {}% sampling",
                (self.config.sample_rate * 100.0) as u32
            );
            // Signal that calibration should be triggered
            self.needs_calibration.store(true, Ordering::Release);
        }
    }

    /// Check if calibration should be triggered and reset the flag.
    pub fn should_calibrate(&self) -> bool {
        self.needs_calibration
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
    }

    /// Check if we're in bootstrap mode.
    pub fn is_bootstrap(&self) -> bool {
        self.in_bootstrap.load(Ordering::Relaxed)
    }

    /// Get the total number of observations collected.
    pub fn total_observations(&self) -> usize {
        self.total_observations.load(Ordering::Relaxed)
    }

    /// Get bootstrap progress as a percentage (0-100).
    pub fn bootstrap_progress(&self) -> u8 {
        if !self.config.enable_bootstrap {
            return 100;
        }
        let total = self.total_observations.load(Ordering::Relaxed);
        let progress = (total as f32 / self.config.bootstrap_threshold as f32 * 100.0) as u8;
        progress.min(100)
    }

    /// Record a single observation directly (useful for testing or manual collection).
    pub fn record_single(&self, observation: DistanceObservation) {
        let mut buffer = self.buffer.lock().unwrap_or_else(|e| {
            warn!("DistanceCollector mutex was poisoned, recovering");
            e.into_inner()
        });
        buffer.push(observation);
        self.total_observations.fetch_add(1, Ordering::Relaxed);
    }

    /// Check if the buffer needs to be flushed.
    pub fn needs_flush(&self) -> bool {
        let buffer = self.buffer.lock().unwrap_or_else(|e| {
            warn!("DistanceCollector mutex was poisoned, recovering");
            e.into_inner()
        });
        buffer.len() >= self.config.buffer_size
    }

    /// Take all buffered observations (clears the buffer).
    pub fn take_buffer(&self) -> Vec<DistanceObservation> {
        let mut buffer = self.buffer.lock().unwrap_or_else(|e| {
            warn!("DistanceCollector mutex was poisoned, recovering");
            e.into_inner()
        });
        std::mem::take(&mut *buffer)
    }

    /// Get the current buffer size.
    pub fn buffer_len(&self) -> usize {
        let buffer = self.buffer.lock().unwrap_or_else(|e| {
            warn!("DistanceCollector mutex was poisoned, recovering");
            e.into_inner()
        });
        buffer.len()
    }

    /// Get the configuration.
    pub fn config(&self) -> &CollectorConfig {
        &self.config
    }

    /// Determine if this observation should be sampled.
    fn should_sample(&self) -> bool {
        // Always sample 100% in bootstrap mode
        if self.in_bootstrap.load(Ordering::Relaxed) {
            return true;
        }

        // Production mode: use configured sample rate
        if self.config.sample_rate >= 1.0 {
            return true;
        }
        if self.config.sample_rate <= 0.0 {
            return false;
        }

        let mut counter = self.sample_counter.lock().unwrap_or_else(|e| {
            warn!("DistanceCollector mutex was poisoned, recovering");
            e.into_inner()
        });
        *counter = counter.wrapping_add(1);

        // Sample every N observations where N = 1/sample_rate
        let n = (1.0 / self.config.sample_rate) as u64;
        (*counter).is_multiple_of(n)
    }
}

impl Default for DistanceCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observation_creation() {
        let obs = DistanceObservation::new("rust".to_string(), 0.5, 12345);
        assert_eq!(obs.language, "rust");
        assert!((obs.distance - 0.5).abs() < 0.001);
        assert_eq!(obs.query_hash, 12345);
        assert!(obs.timestamp > 0);
    }

    #[test]
    fn test_query_hash() {
        let hash1 = DistanceObservation::hash_query("hello");
        let hash2 = DistanceObservation::hash_query("hello");
        let hash3 = DistanceObservation::hash_query("world");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_collector_basic() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            buffer_size: 10,
            sample_rate: 1.0,
            max_age_days: 30,
            bootstrap_threshold: 100,
            enable_bootstrap: false, // Disable bootstrap for this test
        });

        collector.record_single(DistanceObservation::new("rust".to_string(), 0.5, 1));
        collector.record_single(DistanceObservation::new("python".to_string(), 0.6, 2));

        assert_eq!(collector.buffer_len(), 2);

        let buffer = collector.take_buffer();
        assert_eq!(buffer.len(), 2);
        assert_eq!(collector.buffer_len(), 0);
    }

    #[test]
    fn test_bootstrap_mode() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            buffer_size: 100,
            sample_rate: 0.1, // Would be 10% in production
            max_age_days: 30,
            bootstrap_threshold: 5,
            enable_bootstrap: true,
        });

        assert!(collector.is_bootstrap());
        assert_eq!(collector.bootstrap_progress(), 0);

        // In bootstrap, should collect ALL observations
        for i in 0..4 {
            collector.record(&format!("query{}", i), &[(1, 0.5)], |_| {
                Some("rust".to_string())
            });
        }

        assert!(collector.is_bootstrap());
        assert_eq!(collector.total_observations(), 4);
        assert_eq!(collector.buffer_len(), 4);

        // One more should trigger exit from bootstrap
        collector.record("query4", &[(1, 0.5)], |_| Some("rust".to_string()));

        assert!(!collector.is_bootstrap());
        assert!(collector.should_calibrate()); // Should trigger calibration
        assert!(!collector.should_calibrate()); // Flag should be consumed
    }

    #[test]
    fn test_production_mode_sampling() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            buffer_size: 100,
            sample_rate: 0.5, // 50% sampling
            max_age_days: 30,
            bootstrap_threshold: 0, // Start in production
            enable_bootstrap: false,
        });

        assert!(!collector.is_bootstrap());

        // Record 10 queries, should get ~5 with 50% sampling
        for i in 0..10 {
            collector.record(&format!("query{}", i), &[(1, 0.5)], |_| {
                Some("rust".to_string())
            });
        }

        let buffer = collector.take_buffer();
        assert_eq!(buffer.len(), 5);
    }

    #[test]
    fn test_with_existing_count() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            bootstrap_threshold: 100,
            enable_bootstrap: true,
            ..Default::default()
        })
        .with_existing_count(150);

        // Should start in production mode since we have enough observations
        assert!(!collector.is_bootstrap());
        assert_eq!(collector.total_observations(), 150);
    }

    #[test]
    fn test_with_existing_count_still_bootstrap() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            bootstrap_threshold: 100,
            enable_bootstrap: true,
            ..Default::default()
        })
        .with_existing_count(50);

        // Should still be in bootstrap mode
        assert!(collector.is_bootstrap());
        assert_eq!(collector.bootstrap_progress(), 50);
    }

    #[test]
    fn test_collector_needs_flush() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            buffer_size: 3,
            sample_rate: 1.0,
            max_age_days: 30,
            bootstrap_threshold: 100,
            enable_bootstrap: false,
        });

        assert!(!collector.needs_flush());

        collector.record_single(DistanceObservation::new("rust".to_string(), 0.5, 1));
        collector.record_single(DistanceObservation::new("rust".to_string(), 0.6, 2));
        assert!(!collector.needs_flush());

        collector.record_single(DistanceObservation::new("rust".to_string(), 0.7, 3));
        assert!(collector.needs_flush());
    }

    #[test]
    fn test_record_with_language_lookup() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            buffer_size: 100,
            sample_rate: 1.0,
            max_age_days: 30,
            bootstrap_threshold: 100,
            enable_bootstrap: false,
        });

        let results = vec![(1, 0.5), (2, 0.6), (3, 0.7)];

        collector.record("test query", &results, |chunk_id| match chunk_id {
            1 => Some("rust".to_string()),
            2 => Some("python".to_string()),
            3 => None, // Unknown language
            _ => None,
        });

        let buffer = collector.take_buffer();
        assert_eq!(buffer.len(), 2); // Only 2 had known languages

        assert!(buffer.iter().any(|o| o.language == "rust"));
        assert!(buffer.iter().any(|o| o.language == "python"));
    }
}
