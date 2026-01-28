//! Calibrate semantic search thresholds based on collected observations.

use anyhow::{Context, Result};
use semantiq_index::IndexStore;
use semantiq_retrieval::{CalibrationConfig, ThresholdCalibrator, format_calibration_summary};
use std::path::PathBuf;

use super::common::resolve_db_path;

/// Run threshold calibration.
pub async fn calibrate(
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

    println!("Semantiq Threshold Calibration");
    println!("==============================");
    println!("Database: {:?}", db_path);
    println!("Min samples: {}", min_samples);
    println!("Dry run: {}", dry_run);
    println!();

    // Get observation counts
    let counts = store.get_observation_counts()?;
    let total: usize = counts.values().sum();

    if counts.is_empty() {
        println!("No distance observations found.");
        println!();
        println!("Distance observations are collected during semantic search.");
        println!("Run more searches to collect data for calibration.");
        return Ok(());
    }

    println!("Distance Observations:");
    println!("  Total: {}", total);
    for (lang, count) in &counts {
        let status = if *count >= min_samples {
            "ready"
        } else {
            "insufficient"
        };
        println!("  {}: {} ({})", lang, count, status);
    }
    println!();

    // Calibrate for specific language or all
    if let Some(ref lang) = language {
        calibrate_language(&store, lang, min_samples, dry_run)?;
    } else {
        calibrate_all(&store, min_samples, dry_run)?;
    }

    Ok(())
}

fn calibrate_language(
    store: &IndexStore,
    language: &str,
    min_samples: usize,
    dry_run: bool,
) -> Result<()> {
    let distances = store.get_distance_observations(language)?;

    if distances.len() < min_samples {
        println!(
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
        println!("Calibration Result for {}:", language);
        println!("  max_distance: {:.4}", result.thresholds.max_distance);
        println!("  min_similarity: {:.4}", result.thresholds.min_similarity);
        println!("  confidence: {}", result.thresholds.confidence);
        println!("  samples: {}", result.thresholds.sample_count);

        if let Some(ref stats) = result.thresholds.stats {
            println!();
            println!("  Statistics:");
            println!("    mean: {:.4}", stats.mean);
            println!("    std_dev: {:.4}", stats.std_dev);
            println!("    p50 (median): {:.4}", stats.p50);
            println!("    p90: {:.4}", stats.p90);
            println!("    p95: {:.4}", stats.p95);
        }

        if !dry_run {
            let stats = result.thresholds.stats.as_ref();
            store.save_calibration(
                language,
                result.thresholds.max_distance,
                result.thresholds.min_similarity,
                &result.thresholds.confidence.to_string(),
                result.thresholds.sample_count,
                stats.map(|s| s.p50),
                stats.map(|s| s.p90),
                stats.map(|s| s.p95),
                stats.map(|s| s.mean),
                stats.map(|s| s.std_dev),
            )?;
            println!();
            println!("Calibration saved to database.");
        } else {
            println!();
            println!("Dry run - not saving to database.");
        }
    } else {
        println!("Calibration failed: {}", result.message);
    }

    Ok(())
}

fn calibrate_all(store: &IndexStore, min_samples: usize, dry_run: bool) -> Result<()> {
    let all_observations = store.get_all_distance_observations()?;

    if all_observations.is_empty() {
        println!("No observations to calibrate.");
        return Ok(());
    }

    let calibrator = ThresholdCalibrator::with_config(CalibrationConfig {
        min_samples,
        ..Default::default()
    });
    let config = calibrator.calibrate_all(&all_observations);

    // Print summary
    println!("{}", format_calibration_summary(&config));

    if !dry_run {
        // Save per-language calibrations
        for (language, thresholds) in &config.per_language {
            if thresholds.sample_count >= min_samples {
                let stats = thresholds.stats.as_ref();
                store.save_calibration(
                    language,
                    thresholds.max_distance,
                    thresholds.min_similarity,
                    &thresholds.confidence.to_string(),
                    thresholds.sample_count,
                    stats.map(|s| s.p50),
                    stats.map(|s| s.p90),
                    stats.map(|s| s.p95),
                    stats.map(|s| s.mean),
                    stats.map(|s| s.std_dev),
                )?;
            }
        }

        // Save global calibration
        if config.global.sample_count >= min_samples {
            let stats = config.global.stats.as_ref();
            store.save_calibration(
                "_global",
                config.global.max_distance,
                config.global.min_similarity,
                &config.global.confidence.to_string(),
                config.global.sample_count,
                stats.map(|s| s.p50),
                stats.map(|s| s.p90),
                stats.map(|s| s.p95),
                stats.map(|s| s.mean),
                stats.map(|s| s.std_dev),
            )?;
        }

        println!();
        println!("Calibration saved to database.");
    } else {
        println!();
        println!("Dry run - not saving to database.");
    }

    Ok(())
}
