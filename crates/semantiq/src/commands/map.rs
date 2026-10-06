//! Print a ranked map of the repository

use anyhow::Result;
use semantiq_index::IndexStore;
use semantiq_retrieval::{RepoMapOptions, build_repo_map};
use std::path::PathBuf;

use super::common::resolve_db_path;

pub(crate) async fn map(
    database: Option<PathBuf>,
    max_tokens: usize,
    focus: Vec<String>,
    path_prefix: Option<String>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let db_path = resolve_db_path(database, &cwd);

    if !db_path.exists() {
        anyhow::bail!(
            "Database not found: {:?}. Run 'semantiq index' first.",
            db_path
        );
    }

    // The map only reads the index: no engine, so no embedding model to load.
    let store = IndexStore::open(&db_path)?;
    let options = RepoMapOptions {
        max_tokens,
        focus,
        path_prefix,
    };
    let map = tokio::task::spawn_blocking(move || build_repo_map(&store, &options)).await??;

    print!("{}", map.text);
    if !map.unmatched_focus.is_empty() {
        eprintln!(
            "No indexed file or symbol matches: {}",
            map.unmatched_focus.join(", ")
        );
    }
    tracing::debug!(
        tokens = map.estimated_tokens,
        build_time_ms = map.build_time_ms,
        "repo map built"
    );

    Ok(())
}
