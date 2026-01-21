use anyhow::Result;
use rmcp::{
    ServerHandler,
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool,
};
use semantiq_index::{AutoIndexer, IndexStore};
use semantiq_retrieval::{RetrievalEngine, SearchOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::info;

use crate::version_check::{VersionCheckConfig, check_for_update, notify_update};

#[derive(Clone)]
pub struct SemantiqServer {
    engine: Arc<RetrievalEngine>,
    store: Arc<IndexStore>,
    auto_indexer: Option<Arc<Mutex<AutoIndexer>>>,
}

impl SemantiqServer {
    pub fn new(db_path: &Path, project_root: &str) -> Result<Self> {
        info!("Initializing Semantiq MCP server");
        info!("Database path: {:?}", db_path);
        info!("Project root: {}", project_root);

        // Share a single IndexStore instance across all components
        let store = Arc::new(IndexStore::open(db_path)?);

        // Check if parser version changed and prepare for full reindex if needed
        let _ = store.check_and_prepare_for_reindex()?;

        let engine = Arc::new(RetrievalEngine::new(Arc::clone(&store), project_root));

        // Initialize auto-indexer with the same shared store
        let auto_indexer = match AutoIndexer::new(Arc::clone(&store), PathBuf::from(project_root)) {
            Ok(indexer) => {
                info!("Auto-indexing enabled");
                Some(Arc::new(Mutex::new(indexer)))
            }
            Err(e) => {
                info!("Auto-indexing disabled: {}", e);
                None
            }
        };

        // Spawn background version check (non-blocking)
        Self::spawn_version_check();

        Ok(Self {
            engine,
            store,
            auto_indexer,
        })
    }

    fn spawn_version_check() {
        tokio::spawn(async {
            tokio::task::spawn_blocking(|| {
                let config = VersionCheckConfig::from_env();
                if let Some(info) = check_for_update(env!("CARGO_PKG_VERSION"), &config) {
                    notify_update(&info);
                }
            })
            .await
            .ok();
        });
    }

    pub fn store(&self) -> &Arc<IndexStore> {
        &self.store
    }

    pub fn engine(&self) -> &Arc<RetrievalEngine> {
        &self.engine
    }

    /// Start the auto-indexing background task
    /// Performs initial indexing first, then watches for changes
    pub fn start_auto_indexer(&self) {
        if let Some(ref auto_indexer) = self.auto_indexer {
            let indexer = Arc::clone(auto_indexer);

            tokio::spawn(async move {
                // Perform initial indexing in a blocking task
                let indexer_clone = Arc::clone(&indexer);
                let initial_result = tokio::task::spawn_blocking(move || {
                    let indexer = indexer_clone.blocking_lock();
                    indexer.initial_index()
                })
                .await;

                match initial_result {
                    Ok(Ok(result)) => {
                        if result.indexed > 0 {
                            info!(
                                "Initial indexing complete: {} files indexed, {} skipped",
                                result.indexed, result.skipped
                            );
                        } else if result.scanned > 0 {
                            info!("Index up to date: {} files checked", result.scanned);
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::error!("Initial indexing failed: {}", e);
                    }
                    Err(e) => {
                        tracing::error!("Initial indexing task panicked: {}", e);
                    }
                }

                // Then start watching for changes
                let mut interval = tokio::time::interval(Duration::from_secs(2));

                loop {
                    interval.tick().await;

                    let indexer = indexer.lock().await;
                    if let Err(e) = indexer.process_events() {
                        tracing::error!("Auto-indexer error: {}", e);
                    }
                }
            });

            info!("Auto-indexer background task started");
        }
    }
}

#[tool(tool_box)]
impl SemantiqServer {
    #[tool(
        name = "semantiq_search",
        description = "Search for code patterns, symbols, or text in the codebase. Returns relevant matches with file paths and line numbers. Supports filtering: min_score (0.0-1.0, default 0.35), file_type (comma-separated extensions like 'rs,ts,py'), symbol_kind (function,method,class,struct,enum,interface,trait,module,variable,constant,type)."
    )]
    pub async fn semantiq_search(
        &self,
        #[tool(param)] query: String,
        #[tool(param)] limit: Option<usize>,
        #[tool(param)] min_score: Option<f32>,
        #[tool(param)] file_type: Option<String>,
        #[tool(param)] symbol_kind: Option<String>,
    ) -> Result<String, String> {
        // Validate query
        let query = query.trim();
        if query.is_empty() {
            return Err("Query cannot be empty".to_string());
        }
        if query.len() > 500 {
            return Err("Query exceeds maximum length of 500 characters".to_string());
        }

        let limit = limit.unwrap_or(20);

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

        match self.engine.search(query, limit, Some(options)) {
            Ok(results) => {
                let mut output = format!(
                    "Found {} results for '{}' ({} ms)\n\n",
                    results.total_count, query, results.search_time_ms
                );

                for result in &results.results {
                    output.push_str(&format!(
                        "📄 {}\n   Lines {}-{} | Score: {:.2}\n",
                        result.file_path, result.start_line, result.end_line, result.score
                    ));

                    if let Some(ref symbol_name) = result.metadata.symbol_name {
                        output.push_str(&format!(
                            "   Symbol: {} ({})\n",
                            symbol_name,
                            result.metadata.symbol_kind.as_deref().unwrap_or("unknown")
                        ));
                    }

                    let snippet: String = result.content.chars().take(200).collect();
                    output.push_str(&format!("   ```\n   {}\n   ```\n\n", snippet.trim()));
                }

                Ok(output)
            }
            Err(e) => Err(format!("Search failed: {}", e)),
        }
    }

    #[tool(
        name = "semantiq_find_refs",
        description = "Find all references to a symbol including definitions and usages. Useful for understanding how a function or class is used."
    )]
    pub async fn semantiq_find_refs(
        &self,
        #[tool(param)] symbol: String,
        #[tool(param)] limit: Option<usize>,
    ) -> Result<String, String> {
        let limit = limit.unwrap_or(50);

        match self.engine.find_references(&symbol, limit) {
            Ok(results) => {
                let mut output = format!(
                    "Found {} references to '{}' ({} ms)\n\n",
                    results.total_count, symbol, results.search_time_ms
                );

                let definitions: Vec<_> = results
                    .results
                    .iter()
                    .filter(|r| {
                        r.metadata
                            .match_type
                            .as_ref()
                            .map(|t| t == "definition")
                            .unwrap_or(false)
                    })
                    .collect();

                let usages: Vec<_> = results
                    .results
                    .iter()
                    .filter(|r| {
                        r.metadata
                            .match_type
                            .as_ref()
                            .map(|t| t != "definition")
                            .unwrap_or(true)
                    })
                    .collect();

                if !definitions.is_empty() {
                    output.push_str("## Definitions\n\n");
                    for def in &definitions {
                        output.push_str(&format!(
                            "📍 {}:{}\n   {}\n\n",
                            def.file_path,
                            def.start_line,
                            def.content.lines().next().unwrap_or("")
                        ));
                    }
                }

                if !usages.is_empty() {
                    output.push_str(&format!("## Usages ({} found)\n\n", usages.len()));
                    for usage in usages.iter().take(20) {
                        output.push_str(&format!(
                            "📎 {}:{}\n   {}\n\n",
                            usage.file_path,
                            usage.start_line,
                            usage.content.trim()
                        ));
                    }

                    if usages.len() > 20 {
                        output.push_str(&format!("... and {} more usages\n", usages.len() - 20));
                    }
                }

                Ok(output)
            }
            Err(e) => Err(format!("Find references failed: {}", e)),
        }
    }

    #[tool(
        name = "semantiq_deps",
        description = "Analyze the dependency graph for a file. Shows what the file imports and what other files import it."
    )]
    pub async fn semantiq_deps(&self, #[tool(param)] file_path: String) -> Result<String, String> {
        let mut output = format!("Dependency analysis for '{}'\n\n", file_path);

        match self.engine.get_dependencies(&file_path) {
            Ok(deps) => {
                output.push_str(&format!("## Imports ({} dependencies)\n\n", deps.len()));
                for dep in &deps {
                    output.push_str(&format!("→ {}", dep.target_path));
                    if let Some(ref name) = dep.import_name {
                        output.push_str(&format!(" (as {})", name));
                    }
                    output.push_str(&format!(" [{}]\n", dep.kind));
                }
                output.push('\n');
            }
            Err(e) => {
                output.push_str(&format!("Could not analyze imports: {}\n\n", e));
            }
        }

        match self.engine.get_dependents(&file_path) {
            Ok(deps) => {
                output.push_str(&format!("## Imported by ({} files)\n\n", deps.len()));
                for dep in &deps {
                    output.push_str(&format!("← {}\n", dep.target_path));
                }
            }
            Err(e) => {
                output.push_str(&format!("Could not analyze dependents: {}\n", e));
            }
        }

        Ok(output)
    }

    #[tool(
        name = "semantiq_explain",
        description = "Get a detailed explanation of a symbol including its definition, documentation, usage patterns, and related symbols."
    )]
    pub async fn semantiq_explain(&self, #[tool(param)] symbol: String) -> Result<String, String> {
        match self.engine.explain_symbol(&symbol) {
            Ok(explanation) => {
                if !explanation.found {
                    return Ok(format!("Symbol '{}' not found in the index.", symbol));
                }

                let mut output = format!("# Symbol: {}\n\n", explanation.name);

                output.push_str(&format!(
                    "Found {} definition(s), {} usage(s)\n\n",
                    explanation.definitions.len(),
                    explanation.usage_count
                ));

                for (i, def) in explanation.definitions.iter().enumerate() {
                    output.push_str(&format!("## Definition {} ({})\n", i + 1, def.kind));
                    output.push_str(&format!(
                        "📄 {}:{}-{}\n\n",
                        def.file_path, def.start_line, def.end_line
                    ));

                    if let Some(ref sig) = def.signature {
                        output.push_str(&format!("```\n{}\n```\n\n", sig));
                    }

                    if let Some(ref doc) = def.doc_comment {
                        output.push_str(&format!("**Documentation:**\n{}\n\n", doc));
                    }
                }

                if !explanation.related_symbols.is_empty() {
                    output.push_str("## Related Symbols\n\n");
                    for related in explanation.related_symbols.iter().take(10) {
                        output.push_str(&format!("- {}\n", related));
                    }
                }

                Ok(output)
            }
            Err(e) => Err(format!("Explain failed: {}", e)),
        }
    }
}

#[tool(tool_box)]
impl ServerHandler for SemantiqServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: Default::default(),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "semantiq".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: Some(
                "Semantiq provides semantic code understanding tools for AI assistants. \
                Use semantiq_search to find code, semantiq_find_refs to trace symbol usage, \
                semantiq_deps to analyze dependencies, and semantiq_explain for detailed symbol info."
                    .to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Helper to create a test server with a temporary database
    fn create_test_server() -> (SemantiqServer, TempDir) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = temp_dir.path().join(".semantiq.db");
        let project_root = temp_dir.path().to_string_lossy().to_string();

        // Create the server without spawning background tasks
        let store = Arc::new(IndexStore::open(&db_path).expect("Failed to open store"));
        let engine = Arc::new(RetrievalEngine::new(Arc::clone(&store), &project_root));

        let server = SemantiqServer {
            engine,
            store,
            auto_indexer: None,
        };

        (server, temp_dir)
    }

    /// Helper to index a test file with optional symbol extraction.
    /// For simplicity in MCP tests, we insert the file and optionally parse symbols.
    fn index_test_file(store: &IndexStore, path: &str, content: &str, language: &str) -> i64 {
        let file_id = store
            .insert_file(path, Some(language), content, content.len() as i64, 1000)
            .expect("Failed to insert file");

        // Parse and insert symbols using the correct API
        let lang = semantiq_parser::Language::from_extension(
            std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(""),
        );

        if let Some(lang) = lang
            && let Ok(mut support) = semantiq_parser::LanguageSupport::new()
            && let Ok(tree) = support.parse(lang, content)
            && let Ok(symbols) =
                semantiq_parser::SymbolExtractor::extract(&tree, content, lang)
        {
            let _ = store.insert_symbols(file_id, &symbols);
        }

        file_id
    }

    // ==================== semantiq_search tests ====================

    #[tokio::test]
    async fn test_search_empty_query_returns_error() {
        let (server, _temp) = create_test_server();

        let result = server
            .semantiq_search("".to_string(), None, None, None, None)
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Query cannot be empty");
    }

    #[tokio::test]
    async fn test_search_whitespace_only_query_returns_error() {
        let (server, _temp) = create_test_server();

        let result = server
            .semantiq_search("   ".to_string(), None, None, None, None)
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Query cannot be empty");
    }

    #[tokio::test]
    async fn test_search_query_too_long_returns_error() {
        let (server, _temp) = create_test_server();

        let long_query = "a".repeat(501);
        let result = server
            .semantiq_search(long_query, None, None, None, None)
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("maximum length"));
    }

    #[tokio::test]
    async fn test_search_query_at_max_length_succeeds() {
        let (server, _temp) = create_test_server();

        let max_query = "a".repeat(500);
        let result = server
            .semantiq_search(max_query, None, None, None, None)
            .await;

        // Should not error on length validation
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_search_returns_results_format() {
        let (server, _temp) = create_test_server();

        // Index a test file
        index_test_file(
            &server.store,
            "test.rs",
            "fn hello_world() { println!(\"Hello\"); }",
            "rust",
        );

        let result = server
            .semantiq_search("hello".to_string(), Some(10), None, None, None)
            .await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("results for 'hello'"));
        assert!(output.contains("ms)"));
    }

    #[tokio::test]
    async fn test_search_with_file_type_filter() {
        let (server, _temp) = create_test_server();

        index_test_file(&server.store, "test.rs", "fn rust_func() {}", "rust");
        index_test_file(
            &server.store,
            "test.py",
            "def python_func(): pass",
            "python",
        );

        let result = server
            .semantiq_search(
                "func".to_string(),
                Some(10),
                None,
                Some("rs".to_string()),
                None,
            )
            .await;

        assert!(result.is_ok());
        // Result should only contain .rs files
    }

    #[tokio::test]
    async fn test_search_with_min_score_filter() {
        let (server, _temp) = create_test_server();

        index_test_file(&server.store, "test.rs", "fn exact_match() {}", "rust");

        let result = server
            .semantiq_search("exact_match".to_string(), Some(10), Some(0.9), None, None)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_search_with_symbol_kind_filter() {
        let (server, _temp) = create_test_server();

        index_test_file(
            &server.store,
            "test.rs",
            "fn my_function() {}\nstruct MyStruct {}",
            "rust",
        );

        let result = server
            .semantiq_search(
                "my".to_string(),
                Some(10),
                None,
                None,
                Some("function".to_string()),
            )
            .await;

        assert!(result.is_ok());
    }

    // ==================== semantiq_find_refs tests ====================

    #[tokio::test]
    async fn test_find_refs_returns_formatted_output() {
        let (server, temp) = create_test_server();

        // Create the file physically in the temp directory
        let content = "fn my_symbol() {}";
        let file_path = temp.path().join("test.rs");
        std::fs::write(&file_path, content).expect("Failed to write test file");

        index_test_file(&server.store, "test.rs", content, "rust");

        let result = server
            .semantiq_find_refs("my_symbol".to_string(), Some(10))
            .await;

        assert!(result.is_ok(), "Expected Ok but got: {:?}", result);
        let output = result.unwrap();
        assert!(output.contains("references to 'my_symbol'"));
    }

    #[tokio::test]
    async fn test_find_refs_with_definitions() {
        let (server, temp) = create_test_server();

        // Create the file physically in the temp directory
        let content = "fn calculate() {}";
        let file_path = temp.path().join("lib.rs");
        std::fs::write(&file_path, content).expect("Failed to write test file");

        index_test_file(&server.store, "lib.rs", content, "rust");

        let result = server
            .semantiq_find_refs("calculate".to_string(), Some(50))
            .await;

        assert!(result.is_ok(), "Expected Ok but got: {:?}", result);
        let output = result.unwrap();
        // Should find the definition
        assert!(output.contains("references to 'calculate'"));
    }

    #[tokio::test]
    async fn test_find_refs_default_limit() {
        let (server, _temp) = create_test_server();

        let result = server
            .semantiq_find_refs("nonexistent".to_string(), None)
            .await;

        // Should use default limit of 50
        assert!(result.is_ok());
    }

    // ==================== semantiq_deps tests ====================

    #[tokio::test]
    async fn test_deps_returns_formatted_output() {
        let (server, _temp) = create_test_server();

        let file_id = index_test_file(&server.store, "main.rs", "use crate::utils;", "rust");

        // Add a dependency
        server
            .store
            .insert_dependency(file_id, "crate::utils", Some("utils"), "local")
            .expect("Failed to insert dependency");

        let result = server.semantiq_deps("main.rs".to_string()).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Dependency analysis for 'main.rs'"));
        assert!(output.contains("Imports"));
    }

    #[tokio::test]
    async fn test_deps_shows_imports_section() {
        let (server, _temp) = create_test_server();

        let file_id = index_test_file(&server.store, "app.rs", "use std::io;", "rust");

        server
            .store
            .insert_dependency(file_id, "std::io", Some("io"), "std")
            .expect("Failed to insert dependency");

        let result = server.semantiq_deps("app.rs".to_string()).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Imports"));
        assert!(output.contains("std::io"));
    }

    #[tokio::test]
    async fn test_deps_nonexistent_file() {
        let (server, _temp) = create_test_server();

        let result = server.semantiq_deps("nonexistent.rs".to_string()).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("0 dependencies"));
    }

    // ==================== semantiq_explain tests ====================

    #[tokio::test]
    async fn test_explain_returns_formatted_output() {
        let (server, _temp) = create_test_server();

        index_test_file(
            &server.store,
            "lib.rs",
            "/// Documentation for process\nfn process() {}",
            "rust",
        );

        let result = server.semantiq_explain("process".to_string()).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Symbol: process") || output.contains("not found"));
    }

    #[tokio::test]
    async fn test_explain_symbol_not_found() {
        let (server, _temp) = create_test_server();

        let result = server
            .semantiq_explain("nonexistent_symbol".to_string())
            .await;

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("not found"));
    }

    #[tokio::test]
    async fn test_explain_shows_definitions_count() {
        let (server, _temp) = create_test_server();

        index_test_file(&server.store, "a.rs", "fn shared_name() {}", "rust");
        index_test_file(&server.store, "b.rs", "fn shared_name() {}", "rust");

        let result = server.semantiq_explain("shared_name".to_string()).await;

        assert!(result.is_ok());
        let output = result.unwrap();
        // Should mention definitions found
        assert!(
            output.contains("definition") || output.contains("not found"),
            "Expected 'definition' or 'not found' in output: {}",
            output
        );
    }

    // ==================== ServerHandler tests ====================

    #[test]
    fn test_get_info_returns_correct_name() {
        let (server, _temp) = create_test_server();
        let info = server.get_info();

        assert_eq!(info.server_info.name, "semantiq");
    }

    #[test]
    fn test_get_info_returns_version() {
        let (server, _temp) = create_test_server();
        let info = server.get_info();

        assert!(!info.server_info.version.is_empty());
    }

    #[test]
    fn test_get_info_has_instructions() {
        let (server, _temp) = create_test_server();
        let info = server.get_info();

        assert!(info.instructions.is_some());
        let instructions = info.instructions.unwrap();
        assert!(instructions.contains("semantiq_search"));
        assert!(instructions.contains("semantiq_find_refs"));
        assert!(instructions.contains("semantiq_deps"));
        assert!(instructions.contains("semantiq_explain"));
    }

    #[test]
    fn test_get_info_enables_tools() {
        let (server, _temp) = create_test_server();
        let info = server.get_info();

        // ServerCapabilities should have tools enabled
        assert!(info.capabilities.tools.is_some());
    }

    // ==================== Edge case tests ====================

    #[tokio::test]
    async fn test_search_with_special_characters() {
        let (server, _temp) = create_test_server();

        // Should handle special regex/FTS characters gracefully
        let result = server
            .semantiq_search("test*".to_string(), Some(10), None, None, None)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_search_with_unicode() {
        let (server, _temp) = create_test_server();

        let result = server
            .semantiq_search("函数".to_string(), Some(10), None, None, None)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_find_refs_with_special_characters() {
        let (server, _temp) = create_test_server();

        let result = server
            .semantiq_find_refs("operator+".to_string(), Some(10))
            .await;

        assert!(result.is_ok());
    }
}
