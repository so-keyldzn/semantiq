//! End-to-end blocking tests for natural-language ("intent") queries.
//!
//! Background — during the diagnostic that produced the chunks_vec garbage-
//! collection fix, we observed that 6 out of 7 natural-language queries
//! returned **zero** results on this very codebase. The root cause was orphan
//! rows in `chunks_vec` dominating the KNN top-k. With the index now kept in
//! sync, this test pins the regression: a small set of natural-language queries
//! must return at least one result and a smaller set of "out-of-scope"
//! baselines must remain empty (so we notice if we accidentally relax the
//! filtering and start returning noise).
//!
//! The test indexes the workspace into an in-memory database and performs real
//! ONNX embedding inference. It is gated behind the `onnx` feature so that
//! the fast `cargo test` flow on a stub build still passes.

#![cfg(feature = "onnx")]

use ignore::WalkBuilder;
use semantiq_index::{IndexStore, MAX_FILE_SIZE, paths::to_relative_string, should_exclude_entry};
use semantiq_parser::{ChunkExtractor, Language, LanguageSupport, SymbolExtractor};
use semantiq_retrieval::RetrievalEngine;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points to crates/semantiq-retrieval; go up two levels.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

/// Build an in-memory IndexStore populated with the workspace's source files.
/// Mirrors the production indexing pipeline but trimmed to what the test needs.
fn build_index() -> Arc<IndexStore> {
    let store = Arc::new(IndexStore::open_in_memory().expect("open in-memory store"));
    let root = workspace_root();
    let mut language_support = LanguageSupport::new().expect("LanguageSupport");
    let chunk_extractor = ChunkExtractor::new();
    let model = semantiq_embeddings::create_embedding_model(None).expect("embedding model");

    let walker = WalkBuilder::new(&root)
        .hidden(true)
        .git_ignore(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !should_exclude_entry(&name)
        })
        .build();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let language = match Language::from_path(path) {
            Some(l) => l,
            None => continue,
        };

        let rel_path = to_relative_string(path, &root);
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size = metadata.len() as i64;
        if size > MAX_FILE_SIZE as i64 {
            continue;
        }

        let file_id = store
            .insert_file(&rel_path, Some(language.name()), &content, size, 0)
            .expect("insert_file");

        if let Ok(tree) = language_support.parse(language, &content) {
            if let Ok(symbols) = SymbolExtractor::extract(&tree, &content, language) {
                store.insert_symbols(file_id, &symbols).ok();
            }
            if let Ok(chunks) = chunk_extractor.extract(&tree, &content, language) {
                store.insert_chunks(file_id, &chunks).ok();
                let stored = store.get_chunks_by_file(file_id).unwrap_or_default();
                for c in stored {
                    if let Ok(emb) = model.embed(&c.content) {
                        let _ = store.update_chunk_embedding(c.id, &emb);
                    }
                }
            }
        }
    }

    store
}

fn search_count(engine: &RetrievalEngine, q: &str) -> usize {
    let res = engine.search(q, 10, None).expect("search");
    res.results.len()
}

#[test]
fn intent_queries_return_results_and_baselines_stay_empty() {
    let store = build_index();
    let root = workspace_root();
    let engine = RetrievalEngine::new(store.clone(), root.to_str().unwrap());

    // The chunks_vec invariant must hold after a full index pass.
    assert_eq!(
        store.count_orphan_chunk_vectors().unwrap(),
        0,
        "indexing pipeline left orphan vectors behind"
    );

    // ───── Positive queries: each must return at least one result ─────
    //
    // These are the same shapes that returned 0 hits during the diagnostic.
    // We don't pin the exact file or score; we only assert that *something*
    // surfaces — the goal is to detect a regression to "all empty", not to
    // freeze the ranker.
    let positive = [
        "panic handling unwrap",
        "how to add a new programming language support",
        "where do we exclude files",
        "tree-sitter parser language detection",
        "vector similarity cosine distance",
    ];
    for q in positive {
        let n = search_count(&engine, q);
        assert!(n >= 1, "expected ≥1 result for positive query {q:?}, got {n}");
    }

    // ───── Negative baselines: nothing in the codebase covers these ─────
    //
    // If these start returning hits, our filtering is too lax (typically
    // because someone widened max_distance / min_similarity without rerunning
    // this test). They serve as the "false positive" canary.
    // Carefully chosen so the literal terms don't appear anywhere in the
    // workspace (including this test file itself — see the obfuscation below).
    let term_a = format!("strip{}", "e");
    let term_b = format!("kubern{}", "etes");
    let negative = [
        format!("{} payment processing checkout", term_a),
        format!("{} pod scheduling helm chart", term_b),
    ];
    for q in &negative {
        let n = search_count(&engine, q);
        assert!(
            n == 0,
            "expected 0 results for out-of-scope baseline {q:?}, got {n}. \
             Filtering may have been relaxed too far."
        );
    }
}
