//! Adaptive threshold management for semantic search.
//!
//! This module provides ML-based adaptive thresholds that automatically calibrate
//! based on observed distance distributions per programming language.

mod calibrator;
mod collector;
mod config;
mod stats;

pub use calibrator::{
    CalibrationConfig, CalibrationResult, ThresholdCalibrator, format_calibration_summary,
};
pub use collector::{
    BOOTSTRAP_THRESHOLD, CollectorConfig, DistanceCollector, DistanceObservation, RECALIBRATE_EVERY,
};
pub use config::{APPLIED_MIN_SAMPLES, Confidence, LanguageThresholds, ThresholdConfig};
pub use stats::DistanceStats;
