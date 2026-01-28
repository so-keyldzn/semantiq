//! Threshold calibration storage operations.

use super::IndexStore;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use tracing::debug;

/// Record of calibrated thresholds loaded from the database.
#[derive(Debug, Clone)]
pub struct CalibrationRecord {
    pub language: String,
    pub max_distance: f32,
    pub min_similarity: f32,
    pub confidence: String,
    pub sample_count: usize,
    pub p50_distance: Option<f32>,
    pub p90_distance: Option<f32>,
    pub p95_distance: Option<f32>,
    pub mean_distance: Option<f32>,
    pub std_distance: Option<f32>,
    pub calibrated_at: i64,
}

/// Data for saving a calibration (reduces function arguments).
#[derive(Debug, Clone)]
pub struct CalibrationData {
    pub language: String,
    pub max_distance: f32,
    pub min_similarity: f32,
    pub confidence: String,
    pub sample_count: usize,
    pub p50_distance: Option<f32>,
    pub p90_distance: Option<f32>,
    pub p95_distance: Option<f32>,
    pub mean_distance: Option<f32>,
    pub std_distance: Option<f32>,
}

impl IndexStore {
    /// Save calibrated thresholds for a language.
    pub fn save_calibration(&self, data: &CalibrationData) -> Result<()> {
        let calibrated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO threshold_calibration
                 (language, max_distance, min_similarity, confidence, sample_count,
                  p50_distance, p90_distance, p95_distance, mean_distance, std_distance, calibrated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    data.language,
                    data.max_distance,
                    data.min_similarity,
                    data.confidence,
                    data.sample_count as i64,
                    data.p50_distance,
                    data.p90_distance,
                    data.p95_distance,
                    data.mean_distance,
                    data.std_distance,
                    calibrated_at
                ],
            )?;

            debug!(
                "Saved calibration for {}: max_dist={:.3}, min_sim={:.3}, samples={}",
                data.language, data.max_distance, data.min_similarity, data.sample_count
            );

            Ok(())
        })
    }

    /// Load all calibrated thresholds.
    pub fn load_all_calibrations(&self) -> Result<Vec<CalibrationRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT language, max_distance, min_similarity, confidence, sample_count,
                        p50_distance, p90_distance, p95_distance, mean_distance, std_distance, calibrated_at
                 FROM threshold_calibration",
            )?;

            let results = stmt
                .query_map([], |row| {
                    Ok(CalibrationRecord {
                        language: row.get(0)?,
                        max_distance: row.get(1)?,
                        min_similarity: row.get(2)?,
                        confidence: row.get(3)?,
                        sample_count: row.get::<_, i64>(4)? as usize,
                        p50_distance: row.get(5)?,
                        p90_distance: row.get(6)?,
                        p95_distance: row.get(7)?,
                        mean_distance: row.get(8)?,
                        std_distance: row.get(9)?,
                        calibrated_at: row.get(10)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Load calibration for a specific language.
    pub fn load_calibration(&self, language: &str) -> Result<Option<CalibrationRecord>> {
        self.with_conn(|conn| {
            let result = conn
                .query_row(
                    "SELECT language, max_distance, min_similarity, confidence, sample_count,
                            p50_distance, p90_distance, p95_distance, mean_distance, std_distance, calibrated_at
                     FROM threshold_calibration WHERE language = ?1",
                    [language],
                    |row| {
                        Ok(CalibrationRecord {
                            language: row.get(0)?,
                            max_distance: row.get(1)?,
                            min_similarity: row.get(2)?,
                            confidence: row.get(3)?,
                            sample_count: row.get::<_, i64>(4)? as usize,
                            p50_distance: row.get(5)?,
                            p90_distance: row.get(6)?,
                            p95_distance: row.get(7)?,
                            mean_distance: row.get(8)?,
                            std_distance: row.get(9)?,
                            calibrated_at: row.get(10)?,
                        })
                    },
                )
                .optional()?;

            Ok(result)
        })
    }

    /// Delete all calibration data.
    pub fn clear_calibrations(&self) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM threshold_calibration", [])?;
            Ok(())
        })
    }
}
