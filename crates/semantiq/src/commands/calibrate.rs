//! Calibrate semantic search thresholds based on collected observations.

use anyhow::{Context, Result};
use semantiq_index::{CalibrationData, IndexStore};
use semantiq_retrieval::{CalibrationConfig, ThresholdCalibrator, format_calibration_summary};
use std::path::{Path, PathBuf};

use super::common::{outln, print_stdout, resolve_db_path};

/// Run threshold calibration.
pub(crate) async fn calibrate(
    database: Option<PathBuf>,
    language: Option<String>,
    dry_run: bool,
    min_samples: usize,
) -> Result<()> {
    let cwd = std::env::current_dir().context("Failed to get current directory")?;
    let db_path = resolve_db_path(database, &cwd);

    if !db_path.exists() {
        anyhow::bail!(
            "Database not found: {:?}. Run 'semantiq index' first.",
            db_path
        );
    }

    let store = IndexStore::open(&db_path)?;

    // One report, printed in one go (also what was written before an error):
    // `semantiq calibrate | head` must not panic.
    let mut out = String::new();
    let result = run(&store, &db_path, language, dry_run, min_samples, &mut out);
    print_stdout(out.trim_end())?;
    result
}

fn run(
    store: &IndexStore,
    db_path: &Path,
    language: Option<String>,
    dry_run: bool,
    min_samples: usize,
    out: &mut String,
) -> Result<()> {
    outln!(out, "Semantiq Threshold Calibration");
    outln!(out, "==============================");
    outln!(out, "Database: {:?}", db_path);
    outln!(out, "Min samples: {}", min_samples);
    outln!(out, "Dry run: {}", dry_run);
    outln!(out);

    // Get observation counts
    let counts = store.get_observation_counts()?;
    let total: usize = counts.values().sum();

    if counts.is_empty() {
        outln!(out, "No distance observations found.");
        outln!(out);
        outln!(
            out,
            "Distance observations are collected during semantic search."
        );
        outln!(out, "Run more searches to collect data for calibration.");
        return Ok(());
    }

    outln!(out, "Distance Observations:");
    outln!(out, "  Total: {}", total);
    for (lang, count) in &counts {
        let status = if *count >= min_samples {
            "ready"
        } else {
            "insufficient"
        };
        outln!(out, "  {}: {} ({})", lang, count, status);
    }
    outln!(out);

    // Calibrate for specific language or all
    if let Some(ref lang) = language {
        calibrate_language(store, lang, min_samples, dry_run, out)?;
    } else {
        calibrate_all(store, min_samples, dry_run, out)?;
    }

    Ok(())
}

fn calibrate_language(
    store: &IndexStore,
    language: &str,
    min_samples: usize,
    dry_run: bool,
    out: &mut String,
) -> Result<()> {
    let distances = store.get_distance_observations(language)?;

    if distances.len() < min_samples {
        outln!(
            out,
            "Insufficient samples for {}: {} < {} required",
            language,
            distances.len(),
            min_samples
        );
        return Ok(());
    }

    let calibrator = ThresholdCalibrator::new();
    let result = calibrator.calibrate_language(language, &distances);

    if result.success {
        outln!(out, "Calibration Result for {}:", language);
        outln!(out, "  max_distance: {:.4}", result.thresholds.max_distance);
        outln!(
            out,
            "  min_similarity: {:.4}",
            result.thresholds.min_similarity
        );
        outln!(out, "  confidence: {}", result.thresholds.confidence);
        outln!(out, "  samples: {}", result.thresholds.sample_count);

        if let Some(ref stats) = result.thresholds.stats {
            outln!(out);
            outln!(out, "  Statistics:");
            outln!(out, "    mean: {:.4}", stats.mean);
            outln!(out, "    std_dev: {:.4}", stats.std_dev);
            outln!(out, "    p50 (median): {:.4}", stats.p50);
            outln!(out, "    p90: {:.4}", stats.p90);
            outln!(out, "    p95: {:.4}", stats.p95);
        }

        if !dry_run {
            let stats = result.thresholds.stats.as_ref();
            let data = CalibrationData {
                language: language.to_string(),
                max_distance: result.thresholds.max_distance,
                min_similarity: result.thresholds.min_similarity,
                confidence: result.thresholds.confidence.to_string(),
                sample_count: result.thresholds.sample_count,
                p50_distance: stats.map(|s| s.p50),
                p90_distance: stats.map(|s| s.p90),
                p95_distance: stats.map(|s| s.p95),
                mean_distance: stats.map(|s| s.mean),
                std_distance: stats.map(|s| s.std_dev),
            };
            store.save_calibration(&data)?;
            outln!(out);
            outln!(out, "Calibration saved to database.");
        } else {
            outln!(out);
            outln!(out, "Dry run - not saving to database.");
        }
    } else {
        outln!(out, "Calibration failed: {}", result.message);
    }

    Ok(())
}

fn calibrate_all(
    store: &IndexStore,
    min_samples: usize,
    dry_run: bool,
    out: &mut String,
) -> Result<()> {
    let all_observations = store.get_all_distance_observations()?;

    if all_observations.is_empty() {
        outln!(out, "No observations to calibrate.");
        return Ok(());
    }

    let calibrator = ThresholdCalibrator::with_config(CalibrationConfig {
        min_samples,
        ..Default::default()
    });
    let config = calibrator.calibrate_all(&all_observations);

    // Print summary
    outln!(out, "{}", format_calibration_summary(&config));

    if !dry_run {
        // Save per-language calibrations
        for (language, thresholds) in &config.per_language {
            if thresholds.sample_count >= min_samples {
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
                store.save_calibration(&data)?;
            }
        }

        // Save global calibration
        if config.global.sample_count >= min_samples {
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
            store.save_calibration(&data)?;
        }

        outln!(out);
        outln!(out, "Calibration saved to database.");
    } else {
        outln!(out);
        outln!(out, "Dry run - not saving to database.");
    }

    Ok(())
}
