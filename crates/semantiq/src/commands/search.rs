//! Search the index (for testing)

use anyhow::{Context, Result};
use semantiq_index::IndexStore;
use semantiq_retrieval::SearchOptions;
use std::path::PathBuf;
use std::sync::Arc;

use super::common::resolve_db_path;

pub async fn search(
    query: &str,
    database: Option<PathBuf>,
    limit: usize,
    min_score: Option<f32>,
    file_type: Option<String>,
    symbol_kind: Option<String>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let db_path = resolve_db_path(database, &cwd);

    if !db_path.exists() {
        anyhow::bail!(
            "Database not found: {:?}. Run 'semantiq index' first.",
            db_path
        );
    }

    let store = Arc::new(IndexStore::open(&db_path)?);
    let cwd_str = cwd
        .to_str()
        .context("Current directory path contains invalid UTF-8")?;
    let engine = semantiq_retrieval::RetrievalEngine::new(store, cwd_str);

    // Build SearchOptions
    let mut options = SearchOptions::new();
    if let Some(score) = min_score {
        options = options.with_min_score(score);
    }
    if let Some(ref ft) = file_type {
        let types = SearchOptions::parse_csv(ft);
        if !types.is_empty() {
            options = options.with_file_types(types);
        }
    }
    if let Some(ref sk) = symbol_kind {
        let kinds = SearchOptions::parse_csv(sk);
        if !kinds.is_empty() {
            options = options.with_symbol_kinds(kinds);
        }
    }

    let results = engine.search(query, limit, Some(options))?;

    // Flush distance observations for ML calibration
    if let Err(e) = engine.flush_observations() {
        tracing::debug!("Failed to flush observations: {}", e);
    }

    println!(
        "Search results for '{}' ({} ms)",
        query, results.search_time_ms
    );
    println!("Found {} results\n", results.total_count);

    for result in &results.results {
        println!(
            "📄 {}:{}-{} (score: {:.2})",
            result.file_path, result.start_line, result.end_line, result.score
        );

        if let Some(ref name) = result.metadata.symbol_name {
            println!(
                "   Symbol: {} ({})",
                name,
                result.metadata.symbol_kind.as_deref().unwrap_or("")
            );
        }

        let snippet: String = result.content.chars().take(100).collect();
        println!("   {}", snippet.trim());
        println!();
    }

    Ok(())
}
