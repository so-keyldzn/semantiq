//! Show index statistics

use anyhow::{Context, Result};
use semantiq_index::IndexStore;
use std::path::PathBuf;

use super::common::resolve_db_path;

pub async fn stats(database: Option<PathBuf>) -> Result<()> {
    let cwd = std::env::current_dir().context("Failed to get current directory")?;
    let db_path = resolve_db_path(database, &cwd);

    if !db_path.exists() {
        anyhow::bail!(
            "Database not found: {:?}. Run 'semantiq index' first.",
            db_path
        );
    }

    let store = IndexStore::open(&db_path)?;
    let stats = store.get_stats()?;

    println!("Semantiq Index Statistics");
    println!("=========================");
    println!("Database: {:?}", db_path);
    println!();
    println!("Index:");
    println!("  Files indexed: {}", stats.file_count);
    println!("  Symbols: {}", stats.symbol_count);
    println!("  Chunks: {}", stats.chunk_count);
    println!("  Dependencies: {}", stats.dependency_count);

    // Show ML calibration info
    let observation_counts = store.get_observation_counts().unwrap_or_default();
    let total_observations: usize = observation_counts.values().sum();
    let calibrations = store.load_all_calibrations().unwrap_or_default();

    const BOOTSTRAP_THRESHOLD: usize = 500;

    println!();
    println!("ML Calibration:");

    // Show bootstrap status
    let bootstrap_progress = (total_observations as f32 / BOOTSTRAP_THRESHOLD as f32 * 100.0).min(100.0) as u8;
    if total_observations < BOOTSTRAP_THRESHOLD {
        println!(
            "  Bootstrap: {}% ({}/{} observations)",
            bootstrap_progress, total_observations, BOOTSTRAP_THRESHOLD
        );
        println!("    Mode: BOOTSTRAP (collecting 100% of observations)");
    } else {
        println!(
            "  Bootstrap: COMPLETE ({} observations)",
            total_observations
        );
        println!("    Mode: PRODUCTION (collecting 10% of observations)");
    }

    println!();
    println!("  Observations by language:");
    if observation_counts.is_empty() {
        println!("    (none yet)");
    } else {
        for (lang, count) in &observation_counts {
            println!("    {}: {}", lang, count);
        }
    }

    println!();
    println!("  Calibrated thresholds: {}", calibrations.len());
    for cal in &calibrations {
        let label = if cal.language == "_global" {
            "global".to_string()
        } else {
            cal.language.clone()
        };
        println!(
            "    {}: max_dist={:.3}, min_sim={:.3} ({})",
            label, cal.max_distance, cal.min_similarity, cal.confidence
        );
    }

    if calibrations.is_empty() {
        println!("    (will auto-calibrate after bootstrap)");
    }

    Ok(())
}
