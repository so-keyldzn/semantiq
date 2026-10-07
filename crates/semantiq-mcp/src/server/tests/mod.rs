//! Test suite for the MCP server, split by tool.
//!
//! Sub-modules group tests by the entry point they exercise:
//! `semantiq_search`, `semantiq_repo_map`, `semantiq_find_refs`, `semantiq_deps`,
//! `semantiq_explain`, the structural tools (`semantiq_calls`, `semantiq_hierarchy`,
//! `semantiq_dead_code`), plus `ServerHandler` metadata and broader edge cases.

use super::{DepsParams, ExplainParams, FindRefsParams, SearchParams, SemantiqServer};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
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
        initial_indexing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        embedder: Arc::new(std::sync::OnceLock::new()),
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
        let references = semantiq_parser::ReferenceExtractor::extract(&tree, content, lang);
        let _ = store.insert_references(file_id, &references);
        let structure = semantiq_parser::StructureExtractor::extract(
            &tree,
            content,
            lang,
            &symbols,
            &references,
        );
        let _ = store.insert_structure(file_id, &structure);
    }

    file_id
}

/// Text content of a successful tool call.
pub(super) fn text_of(result: Result<CallToolResult, String>) -> Result<String, String> {
    let result = result?;
    assert_eq!(result.is_error, Some(false));
    assert!(
        result.structured_content.is_some(),
        "tool result must carry structuredContent"
    );
    Ok(result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect())
}

/// Positional-argument wrappers around the MCP tools, returning their text output.
impl SemantiqServer {
    async fn call_search(
        &self,
        query: String,
        limit: Option<usize>,
        min_score: Option<f32>,
        file_type: Option<String>,
        symbol_kind: Option<String>,
    ) -> Result<String, String> {
        text_of(
            self.semantiq_search(Parameters(SearchParams {
                query,
                limit,
                min_score,
                file_type,
                symbol_kind,
                snippets: None,
            }))
            .await,
        )
    }

    async fn call_find_refs(&self, symbol: String, limit: Option<usize>) -> Result<String, String> {
        text_of(
            self.semantiq_find_refs(Parameters(FindRefsParams { symbol, limit }))
                .await,
        )
    }

    async fn call_deps(&self, file_path: String) -> Result<String, String> {
        text_of(
            self.semantiq_deps(Parameters(DepsParams { file_path }))
                .await,
        )
    }

    async fn call_explain(&self, symbol: String) -> Result<String, String> {
        text_of(
            self.semantiq_explain(Parameters(ExplainParams { symbol }))
                .await,
        )
    }
}

mod deps;
mod edge_cases;
mod explain;
mod find_refs;
mod repo_map;
mod schemas;
mod search;
mod server_handler;
mod structure;
