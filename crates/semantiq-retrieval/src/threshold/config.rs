//! Configuration types for adaptive thresholds.

use super::stats::DistanceStats;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Default maximum L2 distance threshold for semantic search.
/// Results with distance above this are excluded.
pub const DEFAULT_MAX_DISTANCE: f32 = 1.2;

/// Default minimum similarity score (converted from distance).
/// Score = 1.0 / (1.0 + distance), so 0.3 corresponds to distance ~2.33
pub const DEFAULT_MIN_SIMILARITY: f32 = 0.3;

/// Minimum number of observations required for calibration.
pub const MIN_SAMPLES_FOR_CALIBRATION: usize = 100;

/// Confidence level for calibrated thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    /// No calibration data available, using defaults.
    None,
    /// Few samples (< 500), thresholds may not be reliable.
    Low,
    /// Moderate samples (500-2000), reasonably reliable.
    Medium,
    /// Many samples (> 2000), highly reliable.
    High,
}

impl Confidence {
    /// Determine confidence level from sample count.
    pub fn from_count(count: usize) -> Self {
        if count < MIN_SAMPLES_FOR_CALIBRATION {
            Self::None
        } else if count < 500 {
            Self::Low
        } else if count < 2000 {
            Self::Medium
        } else {
            Self::High
        }
    }

    /// Check if this confidence level is sufficient for using calibrated thresholds.
    pub fn is_sufficient(&self) -> bool {
        matches!(self, Self::Medium | Self::High)
    }
}

impl Default for Confidence {
    fn default() -> Self {
        Self::None
    }
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

impl std::str::FromStr for Confidence {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "none" => Ok(Self::None),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            _ => Err(format!("Unknown confidence level: {}", s)),
        }
    }
}

/// Thresholds for a specific programming language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageThresholds {
    /// Maximum L2 distance threshold. Results with distance above this are excluded.
    pub max_distance: f32,
    /// Minimum similarity score (0-1). Results below this are excluded.
    pub min_similarity: f32,
    /// Confidence level based on sample count.
    pub confidence: Confidence,
    /// Number of samples used for calibration.
    pub sample_count: usize,
    /// Distance statistics from calibration.
    pub stats: Option<DistanceStats>,
}

impl Default for LanguageThresholds {
    fn default() -> Self {
        Self {
            max_distance: DEFAULT_MAX_DISTANCE,
            min_similarity: DEFAULT_MIN_SIMILARITY,
            confidence: Confidence::None,
            sample_count: 0,
            stats: None,
        }
    }
}

impl LanguageThresholds {
    /// Create new thresholds with calibrated values.
    pub fn calibrated(
        max_distance: f32,
        min_similarity: f32,
        sample_count: usize,
        stats: DistanceStats,
    ) -> Self {
        Self {
            max_distance,
            min_similarity,
            confidence: Confidence::from_count(sample_count),
            sample_count,
            stats: Some(stats),
        }
    }

    /// Check if these thresholds should be used (based on confidence).
    pub fn should_use(&self) -> bool {
        self.confidence.is_sufficient()
    }
}

/// Complete threshold configuration for all languages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThresholdConfig {
    /// Per-language threshold settings.
    pub per_language: HashMap<String, LanguageThresholds>,
    /// Global thresholds (aggregated from all languages).
    pub global: LanguageThresholds,
    /// Timestamp of last calibration (Unix timestamp).
    pub calibrated_at: Option<i64>,
}

impl ThresholdConfig {
    /// Create a new empty configuration with default thresholds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get thresholds for a specific language with fallback cascade:
    /// 1. Calibrated language-specific thresholds (if confidence >= Medium)
    /// 2. Calibrated global thresholds (if confidence >= Medium)
    /// 3. Default thresholds
    pub fn get(&self, language: Option<&str>) -> (f32, f32) {
        // Try language-specific thresholds first
        if let Some(thresholds) = language
            .and_then(|lang| self.per_language.get(lang))
            .filter(|t| t.should_use())
        {
            return (thresholds.max_distance, thresholds.min_similarity);
        }

        // Try global thresholds
        if self.global.should_use() {
            return (self.global.max_distance, self.global.min_similarity);
        }

        // Fall back to defaults
        (DEFAULT_MAX_DISTANCE, DEFAULT_MIN_SIMILARITY)
    }

    /// Get the full LanguageThresholds for a language (for inspection/stats).
    pub fn get_thresholds(&self, language: &str) -> Option<&LanguageThresholds> {
        self.per_language.get(language)
    }

    /// Set thresholds for a specific language.
    pub fn set(&mut self, language: String, thresholds: LanguageThresholds) {
        self.per_language.insert(language, thresholds);
    }

    /// Set global thresholds.
    pub fn set_global(&mut self, thresholds: LanguageThresholds) {
        self.global = thresholds;
    }

    /// Get all configured languages.
    pub fn languages(&self) -> impl Iterator<Item = &str> {
        self.per_language.keys().map(|s| s.as_str())
    }

    /// Check if any calibration has been done.
    pub fn is_calibrated(&self) -> bool {
        self.calibrated_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_confidence_from_count() {
        assert_eq!(Confidence::from_count(0), Confidence::None);
        assert_eq!(Confidence::from_count(50), Confidence::None);
        assert_eq!(Confidence::from_count(100), Confidence::Low);
        assert_eq!(Confidence::from_count(499), Confidence::Low);
        assert_eq!(Confidence::from_count(500), Confidence::Medium);
        assert_eq!(Confidence::from_count(1999), Confidence::Medium);
        assert_eq!(Confidence::from_count(2000), Confidence::High);
        assert_eq!(Confidence::from_count(10000), Confidence::High);
    }

    #[test]
    fn test_confidence_is_sufficient() {
        assert!(!Confidence::None.is_sufficient());
        assert!(!Confidence::Low.is_sufficient());
        assert!(Confidence::Medium.is_sufficient());
        assert!(Confidence::High.is_sufficient());
    }

    #[test]
    fn test_confidence_display_and_parse() {
        for conf in [
            Confidence::None,
            Confidence::Low,
            Confidence::Medium,
            Confidence::High,
        ] {
            let s = conf.to_string();
            let parsed: Confidence = s.parse().unwrap();
            assert_eq!(conf, parsed);
        }
    }

    #[test]
    fn test_language_thresholds_default() {
        let t = LanguageThresholds::default();
        assert!((t.max_distance - DEFAULT_MAX_DISTANCE).abs() < 0.001);
        assert!((t.min_similarity - DEFAULT_MIN_SIMILARITY).abs() < 0.001);
        assert_eq!(t.confidence, Confidence::None);
        assert_eq!(t.sample_count, 0);
        assert!(!t.should_use());
    }

    #[test]
    fn test_threshold_config_fallback() {
        let mut config = ThresholdConfig::new();

        // No calibration - should return defaults
        let (max_dist, min_sim) = config.get(Some("rust"));
        assert!((max_dist - DEFAULT_MAX_DISTANCE).abs() < 0.001);
        assert!((min_sim - DEFAULT_MIN_SIMILARITY).abs() < 0.001);

        // Add low confidence calibration - should still return defaults
        config.set(
            "rust".to_string(),
            LanguageThresholds {
                max_distance: 1.0,
                min_similarity: 0.4,
                confidence: Confidence::Low,
                sample_count: 200,
                stats: None,
            },
        );
        let (max_dist, _min_sim) = config.get(Some("rust"));
        assert!((max_dist - DEFAULT_MAX_DISTANCE).abs() < 0.001);

        // Add medium confidence calibration - should use it
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
        let (max_dist, min_sim) = config.get(Some("rust"));
        assert!((max_dist - 1.0).abs() < 0.001);
        assert!((min_sim - 0.4).abs() < 0.001);

        // Unknown language should fall back to global or defaults
        let (max_dist, _) = config.get(Some("python"));
        assert!((max_dist - DEFAULT_MAX_DISTANCE).abs() < 0.001);

        // Set global calibration
        config.set_global(LanguageThresholds {
            max_distance: 1.1,
            min_similarity: 0.35,
            confidence: Confidence::Medium,
            sample_count: 5000,
            stats: None,
        });

        // Unknown language should now use global
        let (max_dist, min_sim) = config.get(Some("python"));
        assert!((max_dist - 1.1).abs() < 0.001);
        assert!((min_sim - 0.35).abs() < 0.001);
    }
}
