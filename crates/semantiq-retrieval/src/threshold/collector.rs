//! Distance observation collection during search.
//!
//! One observation per search query and language: the median L2 distance
//! of that language's KNN candidates. That is also what the database keeps
//! (`distance_observations` is `UNIQUE(query_hash, language)`), so the
//! counters here, the bootstrap progress and the calibration sample sizes
//! all describe the same thing (#135). Recording every candidate used to
//! count 20 observations per search while the table kept one row.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

/// Stored observations (distinct query × language) collected at 100 %
/// before sampling drops to `sample_rate`; the first auto-calibration runs
/// when it is reached.
pub const BOOTSTRAP_THRESHOLD: usize = 500;

/// After bootstrap, auto-calibration runs again every this many newly
/// stored observations, so a language crossing the applied floor later is
/// calibrated without a manual `semantiq calibrate`.
pub const RECALIBRATE_EVERY: usize = 100;

/// Configuration for the distance collector.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// Maximum number of observations to buffer before flushing to database.
    pub buffer_size: usize,
    /// Sampling rate in production mode (0.0-1.0).
    pub sample_rate: f32,
    /// Maximum age of observations in days before cleanup.
    pub max_age_days: i64,
    /// Number of stored observations before exiting bootstrap mode.
    /// During bootstrap, sample_rate is 100%.
    pub bootstrap_threshold: usize,
    /// Whether to enable bootstrap mode.
    pub enable_bootstrap: bool,
    /// Newly stored observations between two auto-calibrations after
    /// bootstrap (0 = only the one at the end of bootstrap).
    pub recalibrate_every: usize,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            buffer_size: 100,
            sample_rate: 0.1, // 10% sampling in production
            max_age_days: 30,
            bootstrap_threshold: BOOTSTRAP_THRESHOLD,
            enable_bootstrap: true,
            recalibrate_every: RECALIBRATE_EVERY,
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
    /// Observations stored, plus those buffered (reconciled with the
    /// database at each flush: a repeated query is kept once).
    total_observations: AtomicUsize,
    /// Flag indicating calibration should be triggered.
    needs_calibration: AtomicBool,
    /// Stored since the last auto-calibration, after bootstrap.
    stored_since_calibration: AtomicUsize,
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
            stored_since_calibration: AtomicUsize::new(0),
        }
    }

    /// Retention of stored observations, in seconds (`max_age_days`).
    pub fn max_age_secs(&self) -> i64 {
        self.config.max_age_days.saturating_mul(86_400)
    }

    /// Observations needed before leaving bootstrap mode.
    pub fn bootstrap_threshold(&self) -> usize {
        self.config.bootstrap_threshold
    }

    /// Account for `pruned` stored observations deleted from the database, so
    /// the progress reported here keeps matching the table.
    pub fn forget(&self, pruned: usize) {
        let mut total = self.total_observations.load(Ordering::Relaxed);
        while let Err(current) = self.total_observations.compare_exchange(
            total,
            total.saturating_sub(pruned),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            total = current;
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

    /// Record the KNN candidates of one search: one observation per
    /// language present, with the median distance of that language's
    /// candidates (robust to `limit` and to one odd neighbour).
    ///
    /// In bootstrap mode, every query is recorded; in production mode, one
    /// in `1 / sample_rate`. Returns `true` if the query was sampled.
    /// Bootstrap ends at the next flush, once the database holds
    /// `bootstrap_threshold` rows (see [`reconcile`](Self::reconcile)).
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
        let mut by_language: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        for (chunk_id, distance) in results {
            if let Some(language) = language_lookup(*chunk_id) {
                by_language.entry(language).or_default().push(*distance);
            }
        }
        let recorded_count = by_language.len();
        if recorded_count == 0 {
            return true;
        }

        {
            let mut buffer = self.buffer.lock().unwrap_or_else(|e| {
                warn!("DistanceCollector mutex was poisoned, recovering");
                e.into_inner()
            });
            for (language, distances) in by_language {
                buffer.push(DistanceObservation::new(
                    language,
                    median(distances),
                    query_hash,
                ));
            }
        }
        self.total_observations
            .fetch_add(recorded_count, Ordering::Relaxed);
        true
    }

    /// Account for a flush: `flushed` buffered observations were handed to
    /// the database, which kept `inserted` of them (a query already seen is
    /// ignored by its `UNIQUE(query_hash, language)`). The running total
    /// then matches the table; bootstrap ends when it reaches the
    /// threshold, and later every `recalibrate_every` stored observations
    /// request a new auto-calibration.
    pub fn reconcile(&self, flushed: usize, inserted: usize) {
        self.forget(flushed.saturating_sub(inserted));
        let total = self.total_observations.load(Ordering::Relaxed);
        if self.in_bootstrap.load(Ordering::Relaxed) {
            if total >= self.config.bootstrap_threshold {
                self.exit_bootstrap();
            }
            return;
        }
        if self.config.recalibrate_every == 0 {
            return;
        }
        let since = self
            .stored_since_calibration
            .fetch_add(inserted, Ordering::Relaxed)
            + inserted;
        if since >= self.config.recalibrate_every {
            self.stored_since_calibration.store(0, Ordering::Relaxed);
            self.needs_calibration.store(true, Ordering::Release);
        }
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

    /// Check if the buffer needs to be flushed: it is full, or bootstrap is
    /// about to end (the exit is decided from the stored rows at flush time).
    pub fn needs_flush(&self) -> bool {
        let buffer = self.buffer.lock().unwrap_or_else(|e| {
            warn!("DistanceCollector mutex was poisoned, recovering");
            e.into_inner()
        });
        buffer.len() >= self.config.buffer_size
            || (!buffer.is_empty()
                && self.in_bootstrap.load(Ordering::Relaxed)
                && self.total_observations.load(Ordering::Relaxed)
                    >= self.config.bootstrap_threshold)
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

/// Median of a non-empty list (mean of the two middle values when even).
fn median(mut values: Vec<f32>) -> f32 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
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
            ..Default::default()
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
            ..Default::default()
        });

        assert!(collector.is_bootstrap());
        assert_eq!(collector.bootstrap_progress(), 0);

        // In bootstrap, should collect ALL queries
        for i in 0..4 {
            collector.record(&format!("query{}", i), &[(1, 0.5)], |_| {
                Some("rust".to_string())
            });
        }

        assert!(collector.is_bootstrap());
        assert_eq!(collector.total_observations(), 4);
        assert_eq!(collector.buffer_len(), 4);
        assert!(!collector.needs_flush());

        // The fifth reaches the threshold: a flush is due, and bootstrap
        // ends once the database confirms the rows.
        collector.record("query4", &[(1, 0.5)], |_| Some("rust".to_string()));
        assert!(collector.is_bootstrap(), "decided at flush time");
        assert!(collector.needs_flush());
        let flushed = collector.take_buffer().len();
        collector.reconcile(flushed, flushed);

        assert!(!collector.is_bootstrap());
        assert!(collector.should_calibrate()); // Should trigger calibration
        assert!(!collector.should_calibrate()); // Flag should be consumed
    }

    /// #135: one observation per query and language, the median of that
    /// language's candidates, so the count matches the stored rows.
    #[test]
    fn test_record_aggregates_per_language() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            sample_rate: 1.0,
            enable_bootstrap: false,
            ..Default::default()
        });
        let candidates: Vec<(i64, f32)> = (1..=20).map(|i| (i, 0.5 + i as f32 * 0.01)).collect();
        // Chunks 1-15 are rust, 16-20 python.
        collector.record("a query", &candidates, |id| {
            Some(if id <= 15 { "rust" } else { "python" }.to_string())
        });

        assert_eq!(collector.total_observations(), 2);
        let buffer = collector.take_buffer();
        assert_eq!(buffer.len(), 2);
        let rust = buffer.iter().find(|o| o.language == "rust").unwrap();
        let python = buffer.iter().find(|o| o.language == "python").unwrap();
        assert!((rust.distance - 0.58).abs() < 1e-5, "{}", rust.distance);
        assert!((python.distance - 0.68).abs() < 1e-5, "{}", python.distance);
        assert_eq!(rust.query_hash, python.query_hash);
    }

    /// #135: a repeated query is kept once by the database; the running
    /// total follows the table, and bootstrap is not left early.
    #[test]
    fn test_reconcile_follows_the_stored_rows() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            buffer_size: 100,
            bootstrap_threshold: 4,
            enable_bootstrap: true,
            ..Default::default()
        });
        for _ in 0..5 {
            collector.record("same query", &[(1, 0.5)], |_| Some("rust".to_string()));
        }
        assert_eq!(collector.total_observations(), 5);
        assert!(collector.needs_flush(), "threshold apparently reached");
        let flushed = collector.take_buffer().len();
        // The database inserted one row: four were duplicates.
        collector.reconcile(flushed, 1);
        assert_eq!(collector.total_observations(), 1);
        assert!(collector.is_bootstrap());
        assert!(!collector.should_calibrate());
    }

    /// After bootstrap, auto-calibration is requested again every
    /// `recalibrate_every` stored observations.
    #[test]
    fn test_recalibration_after_bootstrap() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            bootstrap_threshold: 10,
            enable_bootstrap: true,
            recalibrate_every: 3,
            ..Default::default()
        })
        .with_existing_count(10);
        assert!(!collector.is_bootstrap());
        collector.reconcile(2, 2);
        assert!(!collector.should_calibrate());
        collector.reconcile(1, 1);
        assert!(collector.should_calibrate());
        assert!(!collector.should_calibrate());
        collector.reconcile(5, 0);
        assert!(!collector.should_calibrate(), "duplicates do not count");
    }

    #[test]
    fn test_median() {
        assert_eq!(median(vec![3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(vec![4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(vec![7.0]), 7.0);
        assert_eq!(median(vec![]), 0.0);
    }

    #[test]
    fn test_production_mode_sampling() {
        let collector = DistanceCollector::with_config(CollectorConfig {
            buffer_size: 100,
            sample_rate: 0.5, // 50% sampling
            max_age_days: 30,
            bootstrap_threshold: 0, // Start in production
            enable_bootstrap: false,
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
