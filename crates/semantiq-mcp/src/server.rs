use anyhow::Result;
use rmcp::{
    model::{ServerCapabilities, Implementation, ServerInfo},
    tool,
    ServerHandler,
};
use semantiq_index::{AutoIndexer, IndexStore};
use semantiq_retrieval::RetrievalEngine;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::info;

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
        let engine = Arc::new(RetrievalEngine::new(
            Arc::clone(&store),
            project_root,
        ));

        // Initialize auto-indexer with the same shared store
        let auto_indexer = match AutoIndexer::new(
            Arc::clone(&store),
            PathBuf::from(project_root),
        ) {
            Ok(indexer) => {
                info!("Auto-indexing enabled");
                Some(Arc::new(Mutex::new(indexer)))
            }
            Err(e) => {
                info!("Auto-indexing disabled: {}", e);
                None
            }
        };

        Ok(Self { engine, store, auto_indexer })
    }

    pub fn store(&self) -> &Arc<IndexStore> {
        &self.store
    }

    pub fn engine(&self) -> &Arc<RetrievalEngine> {
        &self.engine
    }

    /// Start the auto-indexing background task
    pub fn start_auto_indexer(&self) {
        if let Some(ref auto_indexer) = self.auto_indexer {
            let indexer = Arc::clone(auto_indexer);

            tokio::spawn(async move {
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
        description = "Search for code patterns, symbols, or text in the codebase. Returns relevant matches with file paths and line numbers."
    )]
    pub async fn semantiq_search(
        &self,
        #[tool(param)] query: String,
        #[tool(param)] limit: Option<usize>,
    ) -> Result<String, String> {
        let limit = limit.unwrap_or(20);

        match self.engine.search(&query, limit) {
            Ok(results) => {
                let mut output = format!(
                    "Found {} results for '{}' ({} ms)\n\n",
                    results.total_count, query, results.search_time_ms
                );

                for result in &results.results {
                    output.push_str(&format!(
                        "📄 {}\n   Lines {}-{} | Score: {:.2}\n",
                        result.file_path,
                        result.start_line,
                        result.end_line,
                        result.score
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
                        output.push_str(&format!(
                            "... and {} more usages\n",
                            usages.len() - 20
                        ));
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
    pub async fn semantiq_deps(
        &self,
        #[tool(param)] file_path: String,
    ) -> Result<String, String> {
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
    pub async fn semantiq_explain(
        &self,
        #[tool(param)] symbol: String,
    ) -> Result<String, String> {
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
                    output.push_str(&format!(
                        "## Definition {} ({})\n",
                        i + 1,
                        def.kind
                    ));
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
