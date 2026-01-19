//! Show index statistics

use anyhow::{Context, Result};
use semantiq_index::IndexStore;
use std::path::PathBuf;

const DEFAULT_DB_NAME: &str = ".semantiq.db";

pub async fn stats(database: Option<PathBuf>) -> Result<()> {
    let db_path = match database {
        Some(p) => p,
        None => std::env::current_dir()
            .context("Failed to get current directory")?
            .join(DEFAULT_DB_NAME),
    };

    if !db_path.exists() {
        anyhow::bail!(
            "Database not found: {:?}. Run 'semantiq index' first.",
            db_path
        );
    }

    let store = IndexStore::open(&db_path)?;
    let stats = store.get_stats()?;

    println!("Semantiq Index Statistics");
    println!("========================");
    println!("Database: {:?}", db_path);
    println!("Files indexed: {}", stats.file_count);
    println!("Symbols: {}", stats.symbol_count);
    println!("Chunks: {}", stats.chunk_count);
    println!("Dependencies: {}", stats.dependency_count);

    Ok(())
}
