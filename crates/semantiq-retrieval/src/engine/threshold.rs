//! Threshold management and calibration for RetrievalEngine.

use super::RetrievalEngine;
use crate::threshold::{
    CalibrationConfig, Confidence, LanguageThresholds, ThresholdCalibrator, ThresholdConfig,
};
use anyhow::Result;
use semantiq_index::{CalibrationData, IndexStore};
use tracing::{debug, info, warn};

impl RetrievalEngine {
    /// Default minimum similarity threshold for semantic search results.
    pub const SEMANTIC_MIN_SIMILARITY: f32 = 0.3;

    /// Default maximum distance threshold for sqlite-vec (L2 distance).
    pub const SEMANTIC_MAX_DISTANCE: f32 = 1.2;

    /// Load threshold configuration from calibration data in the store.
    pub(crate) fn load_thresholds_from_store(store: &IndexStore) -> ThresholdConfig {
        let mut config = ThresholdConfig::new();

        match store.load_all_calibrations() {
            Ok(calibrations) => {
                for cal in calibrations {
                    let confidence: Confidence = cal.confidence.parse().unwrap_or(Confidence::None);

                    let thresholds = LanguageThresholds {
                        max_distance: cal.max_distance,
                        min_similarity: cal.min_similarity,
                        confidence,
                        sample_count: cal.sample_count,
                        stats: None,
                    };

                    if cal.language == "_global" {
                        config.set_global(thresholds);
                        config.calibrated_at = Some(cal.calibrated_at);
                    } else {
                        config.set(cal.language, thresholds);
                    }
                }

                if config.is_calibrated() {
                    info!(
                        languages = config.per_language.len(),
                        "Loaded calibrated thresholds"
                    );
                }
            }
            Err(e) => {
                warn!("Failed to load calibrations: {}", e);
            }
        }

        config
    }

    /// Reload threshold configuration from the database.
    pub fn reload_thresholds(&self) {
        let new_config = Self::load_thresholds_from_store(&self.store);
        if let Ok(mut config) = self.threshold_config.write() {
            *config = new_config;
        }
    }

    /// Get thresholds for a specific language using the fallback cascade.
    pub(crate) fn get_thresholds(&self, language: Option<&str>) -> (f32, f32) {
        if let Ok(config) = self.threshold_config.read() {
            config.get(language)
        } else {
            // Fallback to defaults if lock is poisoned
            (Self::SEMANTIC_MAX_DISTANCE, Self::SEMANTIC_MIN_SIMILARITY)
        }
    }

    /// Flush collected distance observations to the database.
    pub fn flush_observations(&self) -> Result<usize> {
        let collector = match &self.distance_collector {
            Some(c) => c,
            None => return Ok(0),
        };

        let observations = collector.take_buffer();
        if observations.is_empty() {
            debug!("No observations to flush");
            return Ok(0);
        }

        debug!("Flushing {} observations to database", observations.len());

        let batch: Vec<(String, f32, u64, i64)> = observations
            .into_iter()
            .map(|o| (o.language, o.distance, o.query_hash, o.timestamp))
            .collect();

        let inserted = self.store.insert_distance_observations_batch(&batch)?;
        info!("Flushed {} distance observations to database", inserted);

        Ok(inserted)
    }

    /// Perform automatic calibration based on collected observations.
    pub fn auto_calibrate(&self) -> Result<bool> {
        let all_observations = self.store.get_all_distance_observations()?;

        if all_observations.is_empty() {
            debug!("No observations for auto-calibration");
            return Ok(false);
        }

        let total: usize = all_observations.values().map(|v| v.len()).sum();
        info!(
            total = total,
            languages = all_observations.len(),
            "Starting auto-calibration"
        );

        let calibrator = ThresholdCalibrator::with_config(CalibrationConfig {
            min_samples: 50,
            ..Default::default()
        });

        let config = calibrator.calibrate_all(&all_observations);

        // Save per-language calibrations
        for (language, thresholds) in &config.per_language {
            if thresholds.sample_count >= 50 {
                let stats = thresholds.stats.as_ref();
                let data = CalibrationData {
                    language: language.clone(),
                    max_distance: thresholds.max_distance,
                    min_similarity: thresholds.min_similarity,
                    confidence: thresholds.confidence.to_string(),
                    sample_count: thresholds.sample_count,
                    p50_distance: stats.map(|s| s.p50),
                    p90_distance: stats.map(|s| s.p90),
                    p95_distance: stats.map(|s| s.p95),
                    mean_distance: stats.map(|s| s.mean),
                    std_distance: stats.map(|s| s.std_dev),
                };
                self.store.save_calibration(&data)?;
            }
        }

        // Save global calibration
        if config.global.sample_count >= 50 {
            let stats = config.global.stats.as_ref();
            let data = CalibrationData {
                language: "_global".to_string(),
                max_distance: config.global.max_distance,
                min_similarity: config.global.min_similarity,
                confidence: config.global.confidence.to_string(),
                sample_count: config.global.sample_count,
                p50_distance: stats.map(|s| s.p50),
                p90_distance: stats.map(|s| s.p90),
                p95_distance: stats.map(|s| s.p95),
                mean_distance: stats.map(|s| s.mean),
                std_distance: stats.map(|s| s.std_dev),
            };
            self.store.save_calibration(&data)?;
        }

        self.reload_thresholds();

        info!(
            global_samples = config.global.sample_count,
            languages = config.per_language.len(),
            "Auto-calibration completed"
        );

        Ok(true)
    }

    /// Check if auto-calibration should be triggered and perform it if needed.
    pub(crate) fn maybe_auto_calibrate(&self) {
        let collector = match &self.distance_collector {
            Some(c) => c,
            None => return,
        };

        if collector.should_calibrate() {
            info!("Triggering auto-calibration after bootstrap");
            if let Err(e) = self.auto_calibrate() {
                warn!("Auto-calibration failed: {}", e);
            }
        }
    }

    /// Flush observations to database if buffer is full.
    pub(crate) fn maybe_flush_observations(&self) {
        let collector = match &self.distance_collector {
            Some(c) => c,
            None => return,
        };

        if collector.needs_flush() {
            let _ = self
                .flush_observations()
                .inspect_err(|e| warn!("Failed to flush distance observations: {}", e));
        }

        self.maybe_auto_calibrate();
    }
}
