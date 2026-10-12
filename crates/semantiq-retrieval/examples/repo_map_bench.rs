//! Time the repo map stages on an existing index, in one process so the
//! engine cache is exercised: the first unfocused call loads the graph, the
//! focused calls re-rank it, the last unfocused call is served from cache.
//!
//! ```sh
//! SEMANTIQ_EMBEDDINGS=stub cargo run -p semantiq-retrieval --example repo_map_bench -- <db> [focus-path]
//! ```

use semantiq_index::IndexStore;
use semantiq_retrieval::{RepoMapOptions, RetrievalEngine};
use std::sync::Arc;
use std::time::Instant;

fn rss_mb() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
        / 1024
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    let db = args
        .next()
        .expect("usage: repo_map_bench <db> [focus-path]");
    let focus = args.next();

    let store = Arc::new(IndexStore::open(std::path::Path::new(&db))?);
    let engine = RetrievalEngine::without_embeddings(store, ".");

    let mut runs = vec![("unfocused, cold", Vec::new())];
    if let Some(focus) = &focus {
        runs.push(("focused, graph cached", vec![focus.clone()]));
        runs.push(("focused again", vec![focus.clone()]));
    }
    runs.push(("unfocused, ranking cached", Vec::new()));

    for (label, focus) in runs {
        let start = Instant::now();
        let map = engine.repo_map(&RepoMapOptions {
            max_tokens: 1500,
            focus,
            path_prefix: None,
        })?;
        println!(
            "{label}: {} ms, rss {} MB ({} of {} symbols, {} tokens)",
            start.elapsed().as_millis(),
            rss_mb(),
            map.shown_symbols,
            map.total_symbols,
            map.estimated_tokens
        );
    }
    Ok(())
}
