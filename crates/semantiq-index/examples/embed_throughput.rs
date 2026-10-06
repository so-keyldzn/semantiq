//! Embedding throughput by batching strategy, on the chunks of an index.
//!
//! ```sh
//! SEMANTIQ_EMBEDDINGS=onnx cargo run --release -p semantiq-index \
//!     --features semantiq-embeddings/onnx --example embed_throughput -- <db> [chunks] [rounds]
//! ```
//!
//! Strategies (nothing is written to the database):
//! - `per-file`: one `embed_batch` call per file (batches never mix files),
//! - `cross-file`: batches of 32 chunks in id order, across files,
//! - `cross-file-sorted`: windows of 256 chunks sorted by length, then batches
//!   of 32 (what phase 2 does).
//!
//! Rounds alternate the strategies so a CPU load change hits all of them.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use semantiq_embeddings::{EmbeddingModel, create_embedding_model};
use std::time::Instant;

const BATCH: usize = 32;

/// Embeds the chunks, returns the number of forward passes.
type Strategy = fn(&dyn EmbeddingModel, &[(i64, String)]) -> Result<usize>;
const WINDOW: usize = 256;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let db = args
        .next()
        .context("usage: embed_throughput <db> [chunks] [rounds]")?;
    let limit: usize = args.next().map(|a| a.parse()).transpose()?.unwrap_or(1024);
    let rounds: usize = args.next().map(|a| a.parse()).transpose()?.unwrap_or(2);

    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare("SELECT file_id, content FROM chunks ORDER BY id LIMIT ?1")?;
    let chunks: Vec<(i64, String)> = stmt
        .query_map([limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let files = {
        let mut ids: Vec<i64> = chunks.iter().map(|(f, _)| *f).collect();
        ids.dedup();
        ids.len()
    };
    let model = create_embedding_model(None)?;
    anyhow::ensure!(
        !model.is_stub(),
        "stub model selected: set SEMANTIQ_EMBEDDINGS=onnx"
    );
    println!(
        "{} chunks from {} files, {} rounds",
        chunks.len(),
        files,
        rounds
    );

    // Warm-up (session initialisation, allocator).
    let warm: Vec<String> = chunks.iter().take(BATCH).map(|(_, c)| c.clone()).collect();
    model.embed_batch(&warm)?;

    let strategies: [(&str, Strategy); 3] = [
        ("per-file", per_file),
        ("cross-file", cross_file),
        ("cross-file-sorted", cross_file_sorted),
    ];
    let mut totals = [0f64; 3];
    for round in 0..rounds {
        for (i, (name, run)) in strategies.iter().enumerate() {
            let start = Instant::now();
            let passes = run(model.as_ref(), &chunks)?;
            let secs = start.elapsed().as_secs_f64();
            totals[i] += secs;
            println!(
                "round {} {:<18} {:>7.1}s {:>6.1} chunks/s ({} forward passes)",
                round + 1,
                name,
                secs,
                chunks.len() as f64 / secs,
                passes
            );
        }
    }
    for (i, (name, _)) in strategies.iter().enumerate() {
        println!(
            "mean   {:<18} {:>7.1}s {:>6.1} chunks/s",
            name,
            totals[i] / rounds as f64,
            (chunks.len() * rounds) as f64 / totals[i]
        );
    }
    Ok(())
}

fn per_file(model: &dyn EmbeddingModel, chunks: &[(i64, String)]) -> Result<usize> {
    let mut passes = 0;
    for file in chunks.chunk_by(|a, b| a.0 == b.0) {
        let texts: Vec<String> = file.iter().map(|(_, c)| c.clone()).collect();
        model.embed_batch(&texts)?;
        passes += texts.len().div_ceil(BATCH);
    }
    Ok(passes)
}

fn cross_file(model: &dyn EmbeddingModel, chunks: &[(i64, String)]) -> Result<usize> {
    let mut passes = 0;
    for batch in chunks.chunks(BATCH) {
        let texts: Vec<String> = batch.iter().map(|(_, c)| c.clone()).collect();
        model.embed_batch(&texts)?;
        passes += 1;
    }
    Ok(passes)
}

fn cross_file_sorted(model: &dyn EmbeddingModel, chunks: &[(i64, String)]) -> Result<usize> {
    let mut passes = 0;
    for window in chunks.chunks(WINDOW) {
        let mut window: Vec<&String> = window.iter().map(|(_, c)| c).collect();
        window.sort_by_key(|c| c.len());
        for batch in window.chunks(BATCH) {
            let texts: Vec<String> = batch.iter().map(|c| (*c).clone()).collect();
            model.embed_batch(&texts)?;
            passes += 1;
        }
    }
    Ok(passes)
}
