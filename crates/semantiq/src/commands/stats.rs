//! Show index statistics

use anyhow::Result;
use semantiq_index::IndexStore;
use semantiq_retrieval::{APPLIED_MIN_SAMPLES, BOOTSTRAP_THRESHOLD, Confidence};
use std::path::PathBuf;

use super::common::{outln, print_stdout};
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

    // One report, printed in one go: `semantiq stats | head` must not panic.
    let mut out = String::new();
    outln!(out, "Semantiq Index Statistics");
    outln!(out, "=========================");
    outln!(out, "Database: {:?}", db_path);
    outln!(out);
    outln!(out, "Index:");
    outln!(out, "  Files indexed: {}", stats.file_count);
    outln!(out, "  Symbols: {}", stats.symbol_count);
    outln!(out, "  Chunks: {}", stats.chunk_count);
    outln!(out, "  Dependencies: {}", stats.dependency_count);
    outln!(out, "  References: {}", stats.ref_count);
    outln!(out, "  Call edges: {}", stats.call_edge_count);
    outln!(out, "  Type relations: {}", stats.type_relation_count);

    outln!(out);
    outln!(out, "Embeddings:");
    outln!(
        out,
        "  Model: {}",
        semantiq_embeddings::embedding_model_id()
    );
    outln!(
        out,
        "  Chunks embedded: {}/{} ({}%)",
        embeddings.embedded,
        embeddings.total,
        embeddings.percent()
    );
    if embeddings.pending() > 0 {
        outln!(
            out,
            "    {} pending: `semantiq index --embeddings-only` (or a running `semantiq serve`) computes them.",
            embeddings.pending()
        );
    }
    match semantiq_embeddings::semantic_search_unavailable_reason() {
        None => outln!(out, "  Semantic search: enabled"),
        Some(reason) => {
            outln!(out, "  Semantic search: UNAVAILABLE ({})", reason);
            outln!(out, "    Symbol and text search still work.");
        }
    }

    // Show ML calibration info
    let observation_counts = store.get_observation_counts().unwrap_or_default();
    let total_observations: usize = observation_counts.values().sum();
    let calibrations = store.load_all_calibrations().unwrap_or_default();

    outln!(out);
    outln!(out, "ML Calibration:");
    outln!(
        out,
        "  (one observation = one search query per language; a calibration is applied from {} observations)",
        APPLIED_MIN_SAMPLES
    );

    // Show bootstrap status
    let bootstrap_progress =
        (total_observations as f32 / BOOTSTRAP_THRESHOLD as f32 * 100.0).min(100.0) as u8;
    if total_observations < BOOTSTRAP_THRESHOLD {
        outln!(
            out,
            "  Bootstrap: {}% ({}/{} observations)",
            bootstrap_progress,
            total_observations,
            BOOTSTRAP_THRESHOLD
        );
        outln!(out, "    Mode: BOOTSTRAP (collecting 100% of observations)");
    } else {
        outln!(
            out,
            "  Bootstrap: COMPLETE ({} observations)",
            total_observations
        );
        outln!(out, "    Mode: PRODUCTION (collecting 10% of observations)");
    }

    outln!(out);
    outln!(out, "  Observations by language:");
    if observation_counts.is_empty() {
        outln!(out, "    (none yet)");
    } else {
        for (lang, count) in &observation_counts {
            outln!(out, "    {}: {}", lang, count);
        }
    }

    outln!(out);
    outln!(out, "  Calibrated thresholds: {}", calibrations.len());
    for cal in &calibrations {
        let label = if cal.language == "_global" {
            "global".to_string()
        } else {
            cal.language.clone()
        };
        let applied = cal
            .confidence
            .parse::<Confidence>()
            .is_ok_and(|c| c.is_sufficient());
        outln!(
            out,
            "    {}: max_dist={:.3}, min_sim={:.3} ({}{})",
            label,
            cal.max_distance,
            cal.min_similarity,
            cal.confidence,
            if applied {
                ""
            } else {
                ", not applied: defaults in force"
            }
        );
    }

    if calibrations.is_empty() {
        outln!(out, "    (will auto-calibrate after bootstrap)");
    }

    print_stdout(out.trim_end())
}
