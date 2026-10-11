//! Show index statistics

use anyhow::Result;
use semantiq_index::IndexStore;
use std::path::PathBuf;

use super::query::{IndexArgs, locate_index};

pub(crate) async fn stats(database: Option<PathBuf>) -> Result<()> {
    // Same lookup as the query commands (`--database`, else `.semantiq.db` in
    // the current directory or a parent): from a subdirectory of an indexed
    // project, `stats` must find the index `refs` would use rather than
    // suggest a second, nested one.
    let (db_path, _root) = locate_index(&IndexArgs {
        database,
        ..IndexArgs::default()
    })?;

    let store = IndexStore::open(&db_path)?;
    let stats = store.get_stats()?;
    let embeddings = store.embedding_counts()?;

    println!("Semantiq Index Statistics");
    println!("=========================");
    println!("Database: {:?}", db_path);
    println!();
    println!("Index:");
    println!("  Files indexed: {}", stats.file_count);
    println!("  Symbols: {}", stats.symbol_count);
    println!("  Chunks: {}", stats.chunk_count);
    println!("  Dependencies: {}", stats.dependency_count);
    println!("  References: {}", stats.ref_count);
    println!("  Call edges: {}", stats.call_edge_count);
    println!("  Type relations: {}", stats.type_relation_count);

    println!();
    println!("Embeddings:");
    println!("  Model: {}", semantiq_embeddings::embedding_model_id());
    println!(
        "  Chunks embedded: {}/{} ({}%)",
        embeddings.embedded,
        embeddings.total,
        embeddings.percent()
    );
    if embeddings.pending() > 0 {
        println!(
            "    {} pending: `semantiq index --embeddings-only` (or a running `semantiq serve`) computes them.",
            embeddings.pending()
        );
    }
    match semantiq_embeddings::semantic_search_unavailable_reason() {
        None => println!("  Semantic search: enabled"),
        Some(reason) => {
            println!("  Semantic search: UNAVAILABLE ({})", reason);
            println!("    Symbol and text search still work.");
        }
    }

    // Show ML calibration info
    let observation_counts = store.get_observation_counts().unwrap_or_default();
    let total_observations: usize = observation_counts.values().sum();
    let calibrations = store.load_all_calibrations().unwrap_or_default();

    const BOOTSTRAP_THRESHOLD: usize = 500;

    println!();
    println!("ML Calibration:");

    // Show bootstrap status
    let bootstrap_progress =
        (total_observations as f32 / BOOTSTRAP_THRESHOLD as f32 * 100.0).min(100.0) as u8;
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
