//! Threshold calibration algorithms.

use super::config::{Confidence, LanguageThresholds, MIN_SAMPLES_FOR_CALIBRATION, ThresholdConfig};
use super::stats::DistanceStats;
use tracing::{debug, info};

/// Result of a calibration operation.
#[derive(Debug, Clone)]
pub struct CalibrationResult {
    /// The calibrated thresholds.
    pub thresholds: LanguageThresholds,
    /// Language that was calibrated (None for global).
    pub language: Option<String>,
    /// Whether the calibration was successful.
    pub success: bool,
    /// Message describing the result.
    pub message: String,
}

/// Configuration for the calibration algorithm.
#[derive(Debug, Clone)]
pub struct CalibrationConfig {
    /// Minimum number of samples required for calibration.
    pub min_samples: usize,
    /// Percentile to use for max_distance (e.g., 90 = 90th percentile).
    pub distance_percentile: f32,
    /// Percentile to use for min_similarity (e.g., 10 = 10th percentile of distances).
    pub similarity_percentile: f32,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        Self {
            min_samples: MIN_SAMPLES_FOR_CALIBRATION,
            distance_percentile: 90.0,
            similarity_percentile: 10.0,
        }
    }
}

/// Calibrates semantic search thresholds based on observed distances.
pub struct ThresholdCalibrator {
    config: CalibrationConfig,
}

impl ThresholdCalibrator {
    /// Create a new calibrator with default configuration.
    pub fn new() -> Self {
        Self::with_config(CalibrationConfig::default())
    }

    /// Create a new calibrator with custom configuration.
    pub fn with_config(config: CalibrationConfig) -> Self {
        Self { config }
    }

    /// Calibrate thresholds for a specific language.
    ///
    /// Returns calibrated thresholds if enough samples are available.
    pub fn calibrate_language(&self, language: &str, distances: &[f32]) -> CalibrationResult {
        if distances.len() < self.config.min_samples {
            return CalibrationResult {
                thresholds: LanguageThresholds::default(),
                language: Some(language.to_string()),
                success: false,
                message: format!(
                    "Insufficient samples for {}: {} < {} required",
                    language,
                    distances.len(),
                    self.config.min_samples
                ),
            };
        }

        let stats = match DistanceStats::compute(distances) {
            Some(s) => s,
            None => {
                return CalibrationResult {
                    thresholds: LanguageThresholds::default(),
                    language: Some(language.to_string()),
                    success: false,
                    message: format!("Failed to compute statistics for {}", language),
                };
            }
        };

        let thresholds = self.compute_thresholds(&stats, distances.len());

        info!(
            language = language,
            samples = distances.len(),
            max_distance = thresholds.max_distance,
            min_similarity = thresholds.min_similarity,
            confidence = %thresholds.confidence,
            "Calibrated thresholds"
        );

        CalibrationResult {
            thresholds,
            language: Some(language.to_string()),
            success: true,
            message: format!(
                "Calibrated {} with {} samples (confidence: {})",
                language,
                distances.len(),
                Confidence::from_count(distances.len())
            ),
        }
    }

    /// Calibrate global thresholds from all observations.
    pub fn calibrate_global(&self, distances: &[f32]) -> CalibrationResult {
        if distances.len() < self.config.min_samples {
            return CalibrationResult {
                thresholds: LanguageThresholds::default(),
                language: None,
                success: false,
                message: format!(
                    "Insufficient global samples: {} < {} required",
                    distances.len(),
                    self.config.min_samples
                ),
            };
        }

        let stats = match DistanceStats::compute(distances) {
            Some(s) => s,
            None => {
                return CalibrationResult {
                    thresholds: LanguageThresholds::default(),
                    language: None,
                    success: false,
                    message: "Failed to compute global statistics".to_string(),
                };
            }
        };

        let thresholds = self.compute_thresholds(&stats, distances.len());

        info!(
            samples = distances.len(),
            max_distance = thresholds.max_distance,
            min_similarity = thresholds.min_similarity,
            confidence = %thresholds.confidence,
            "Calibrated global thresholds"
        );

        CalibrationResult {
            thresholds,
            language: None,
            success: true,
            message: format!(
                "Calibrated global with {} samples (confidence: {})",
                distances.len(),
                Confidence::from_count(distances.len())
            ),
        }
    }

    /// Compute thresholds from distance statistics.
    fn compute_thresholds(&self, stats: &DistanceStats, sample_count: usize) -> LanguageThresholds {
        // Use percentile-based thresholds
        // max_distance: 90th percentile of distances (includes 90% of results)
        let max_distance =
            self.compute_percentile_threshold(stats, self.config.distance_percentile);

        // min_similarity: derived from 10th percentile of distances
        // Low distance = high similarity, so we want low distances to have high similarity
        let low_distance =
            self.compute_percentile_threshold(stats, self.config.similarity_percentile);
        let min_similarity = DistanceStats::distance_to_similarity(low_distance);

        // Apply sanity bounds
        let max_distance = max_distance.clamp(0.5, 3.0);
        let min_similarity = min_similarity.clamp(0.1, 0.8);

        debug!(
            p90_distance = stats.p90,
            p10_distance = stats.p10,
            computed_max_distance = max_distance,
            computed_min_similarity = min_similarity,
            "Threshold computation"
        );

        LanguageThresholds::calibrated(max_distance, min_similarity, sample_count, stats.clone())
    }

    /// Get a percentile value from stats based on the configured percentile.
    fn compute_percentile_threshold(&self, stats: &DistanceStats, percentile: f32) -> f32 {
        match percentile as i32 {
            0..=15 => stats.p10,
            16..=30 => stats.p25,
            31..=60 => stats.p50,
            61..=80 => stats.p75,
            81..=92 => stats.p90,
            _ => stats.p95,
        }
    }

    /// Create a complete ThresholdConfig from language-grouped observations.
    ///
    /// `observations` is a map from language name to distance values.
    pub fn calibrate_all(
        &self,
        observations: &std::collections::HashMap<String, Vec<f32>>,
    ) -> ThresholdConfig {
        let mut config = ThresholdConfig::new();
        let mut all_distances = Vec::new();

        // Calibrate per-language thresholds
        for (language, distances) in observations {
            all_distances.extend(distances.iter().cloned());

            let result = self.calibrate_language(language, distances);
            if result.success {
                config.set(language.clone(), result.thresholds);
            } else {
                debug!(
                    language = language,
                    message = result.message,
                    "Skipped language calibration"
                );
            }
        }

        // Calibrate global thresholds
        let global_result = self.calibrate_global(&all_distances);
        if global_result.success {
            config.set_global(global_result.thresholds);
        }

        // Set calibration timestamp
        config.calibrated_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        );

        config
    }

    /// Get thresholds with fallback strategy.
    ///
    /// Returns (max_distance, min_similarity) using the cascade:
    /// 1. Calibrated language-specific (if confidence >= Medium)
    /// 2. Calibrated global (if confidence >= Medium)
    /// 3. Defaults
    pub fn get_with_fallback(config: &ThresholdConfig, language: Option<&str>) -> (f32, f32) {
        config.get(language)
    }
}

impl Default for ThresholdCalibrator {
    fn default() -> Self {
        Self::new()
    }
}

/// Format a calibration summary for display.
pub fn format_calibration_summary(config: &ThresholdConfig) -> String {
    let mut lines = Vec::new();

    lines.push("Threshold Calibration Summary".to_string());
    lines.push("=".repeat(40));

    if let Some(ts) = config.calibrated_at {
        lines.push(format!("Calibrated at: {} (Unix timestamp)", ts));
    }

    lines.push(String::new());
    lines.push("Global Thresholds:".to_string());
    lines.push(format!(
        "  max_distance: {:.3} (confidence: {})",
        config.global.max_distance, config.global.confidence
    ));
    lines.push(format!(
        "  min_similarity: {:.3}",
        config.global.min_similarity
    ));
    lines.push(format!("  samples: {}", config.global.sample_count));

    if !config.per_language.is_empty() {
        lines.push(String::new());
        lines.push("Per-Language Thresholds:".to_string());

        let mut languages: Vec<_> = config.per_language.keys().collect();
        languages.sort();

        for lang in languages {
            let t = &config.per_language[lang];
            lines.push(format!(
                "  {}: max_dist={:.3}, min_sim={:.3}, samples={}, conf={}",
                lang, t.max_distance, t.min_similarity, t.sample_count, t.confidence
            ));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threshold::config::{DEFAULT_MAX_DISTANCE, DEFAULT_MIN_SIMILARITY};
    use std::collections::HashMap;

    fn generate_distances(count: usize, mean: f32, spread: f32) -> Vec<f32> {
        (0..count)
            .map(|i| {
                let factor = (i as f32 / count as f32) * 2.0 - 1.0; // -1 to 1
                (mean + factor * spread).max(0.0)
            })
            .collect()
    }

    #[test]
    fn test_calibrate_insufficient_samples() {
        let calibrator = ThresholdCalibrator::new();
        let distances = vec![0.5, 0.6, 0.7]; // Only 3 samples

        let result = calibrator.calibrate_language("rust", &distances);
        assert!(!result.success);
        assert!(result.message.contains("Insufficient"));
    }

    #[test]
    fn test_calibrate_language_success() {
        let calibrator = ThresholdCalibrator::with_config(CalibrationConfig {
            min_samples: 10,
            ..Default::default()
        });

        let distances = generate_distances(100, 0.8, 0.3);
        let result = calibrator.calibrate_language("rust", &distances);

        assert!(result.success);
        assert_eq!(result.language, Some("rust".to_string()));
        assert!(result.thresholds.max_distance > 0.0);
        assert!(result.thresholds.min_similarity > 0.0);
        assert!(result.thresholds.stats.is_some());
    }

    #[test]
    fn test_calibrate_global() {
        let calibrator = ThresholdCalibrator::with_config(CalibrationConfig {
            min_samples: 10,
            ..Default::default()
        });

        let distances = generate_distances(200, 0.9, 0.4);
        let result = calibrator.calibrate_global(&distances);

        assert!(result.success);
        assert!(result.language.is_none());
    }

    #[test]
    fn test_calibrate_all() {
        let calibrator = ThresholdCalibrator::with_config(CalibrationConfig {
            min_samples: 10,
            ..Default::default()
        });

        let mut observations = HashMap::new();
        observations.insert("rust".to_string(), generate_distances(100, 0.7, 0.2));
        observations.insert("python".to_string(), generate_distances(50, 0.9, 0.3));
        observations.insert("go".to_string(), generate_distances(5, 0.8, 0.2)); // Too few

        let config = calibrator.calibrate_all(&observations);

        assert!(config.is_calibrated());
        assert!(config.get_thresholds("rust").is_some());
        assert!(config.get_thresholds("python").is_some());
        assert!(config.get_thresholds("go").is_none()); // Not enough samples
    }

    #[test]
    fn test_threshold_sanity_bounds() {
        let calibrator = ThresholdCalibrator::with_config(CalibrationConfig {
            min_samples: 5,
            ..Default::default()
        });

        // Extremely low distances
        let low_distances: Vec<f32> = (0..100).map(|i| i as f32 * 0.001).collect();
        let result = calibrator.calibrate_language("test", &low_distances);
        assert!(result.thresholds.max_distance >= 0.5);

        // Extremely high distances
        let high_distances: Vec<f32> = (0..100).map(|i| 5.0 + i as f32 * 0.1).collect();
        let result = calibrator.calibrate_language("test", &high_distances);
        assert!(result.thresholds.max_distance <= 3.0);
    }

    #[test]
    fn test_get_with_fallback() {
        let mut config = ThresholdConfig::new();

        // No calibration - should return defaults
        let (max_dist, min_sim) = ThresholdCalibrator::get_with_fallback(&config, Some("rust"));
        assert!((max_dist - DEFAULT_MAX_DISTANCE).abs() < 0.001);
        assert!((min_sim - DEFAULT_MIN_SIMILARITY).abs() < 0.001);

        // Add calibrated rust thresholds
        config.set(
            "rust".to_string(),
            LanguageThresholds {
                max_distance: 1.0,
                min_similarity: 0.4,
                confidence: Confidence::High,
                sample_count: 5000,
                stats: None,
            },
        );

        let (max_dist, _) = ThresholdCalibrator::get_with_fallback(&config, Some("rust"));
        assert!((max_dist - 1.0).abs() < 0.001);

        // Python should still use defaults (or global if set)
        let (max_dist, _) = ThresholdCalibrator::get_with_fallback(&config, Some("python"));
        assert!((max_dist - DEFAULT_MAX_DISTANCE).abs() < 0.001);
    }

    #[test]
    fn test_format_calibration_summary() {
        let mut config = ThresholdConfig::new();
        config.calibrated_at = Some(1700000000);
        config.set_global(LanguageThresholds {
            max_distance: 1.1,
            min_similarity: 0.35,
            confidence: Confidence::High,
            sample_count: 5000,
            stats: None,
        });
        config.set(
            "rust".to_string(),
            LanguageThresholds {
                max_distance: 1.0,
                min_similarity: 0.4,
                confidence: Confidence::Medium,
                sample_count: 1000,
                stats: None,
            },
        );

        let summary = format_calibration_summary(&config);
        assert!(summary.contains("Global Thresholds"));
        assert!(summary.contains("rust"));
        assert!(summary.contains("1.100"));
    }
}
