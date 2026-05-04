//! Test suite for the MCP server, split by tool.
//!
//! Sub-modules group tests by the entry point they exercise:
//! `semantiq_search`, `semantiq_find_refs`, `semantiq_deps`, `semantiq_explain`,
//! plus `ServerHandler` metadata and broader edge cases.

use super::SemantiqServer;
use semantiq_index::IndexStore;
use semantiq_retrieval::RetrievalEngine;
use std::sync::Arc;
use tempfile::TempDir;

/// Build a server backed by a temporary on-disk SQLite DB and no background tasks.
pub(super) fn create_test_server() -> (SemantiqServer, TempDir) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let db_path = temp_dir.path().join(".semantiq.db");
    let project_root = temp_dir.path().to_string_lossy().to_string();

    let store = Arc::new(IndexStore::open(&db_path).expect("Failed to open store"));
    let engine = Arc::new(RetrievalEngine::new(Arc::clone(&store), &project_root));

    let server = SemantiqServer {
        engine,
        store,
        auto_indexer: None,
    };

    (server, temp_dir)
}

/// Insert a file into the index and best-effort-extract its symbols.
pub(super) fn index_test_file(
    store: &IndexStore,
    path: &str,
    content: &str,
    language: &str,
) -> i64 {
    let file_id = store
        .insert_file(path, Some(language), content, content.len() as i64, 1000)
        .expect("Failed to insert file");

    let lang = semantiq_parser::Language::from_extension(
        std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or(""),
    );

    if let Some(lang) = lang
        && let Ok(mut support) = semantiq_parser::LanguageSupport::new()
        && let Ok(tree) = support.parse(lang, content)
        && let Ok(symbols) = semantiq_parser::SymbolExtractor::extract(&tree, content, lang)
    {
        let _ = store.insert_symbols(file_id, &symbols);
    }

    file_id
}

mod deps;
mod edge_cases;
mod explain;
mod find_refs;
mod search;
mod server_handler;
