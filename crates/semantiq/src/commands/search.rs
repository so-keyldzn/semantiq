//! Search the index (for testing)

use anyhow::{Context, Result};
use semantiq_index::IndexStore;
use std::path::PathBuf;
use std::sync::Arc;

const DEFAULT_DB_NAME: &str = ".semantiq.db";

pub async fn search(query: &str, database: Option<PathBuf>, limit: usize) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let db_path = database.unwrap_or_else(|| cwd.join(DEFAULT_DB_NAME));

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

    let results = engine.search(query, limit)?;

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
