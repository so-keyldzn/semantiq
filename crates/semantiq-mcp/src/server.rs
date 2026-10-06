use anyhow::Result;
use rmcp::{
    ServerHandler,
    handler::server::{tool::schema_for_output, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    service::{NotificationContext, Peer, RoleServer},
    tool, tool_handler, tool_router,
};
use semantiq_index::{AutoIndexer, IndexStore};
use semantiq_retrieval::RetrievalEngine;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::version_check::{VersionCheckConfig, check_for_update};

mod outputs;
mod types;
pub use outputs::*;
pub use types::*;

#[derive(Clone)]
pub struct SemantiqServer {
    engine: Arc<RetrievalEngine>,
    store: Arc<IndexStore>,
    auto_indexer: Option<Arc<Mutex<AutoIndexer>>>,
    /// True while the initial index pass started by `start_auto_indexer` runs.
    initial_indexing: Arc<AtomicBool>,
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

        Ok(Self {
            engine,
            store,
            auto_indexer,
            initial_indexing: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Spawn a background version check that notifies the MCP client if an update is available.
    ///
    /// MCP logging is deprecated (SEP-2577) but remains the only channel clients
    /// surface to the user without a tool call.
    #[allow(deprecated)]
    fn spawn_version_check(peer: Peer<RoleServer>) {
        tokio::spawn(async move {
            let info = tokio::task::spawn_blocking(|| {
                let config = VersionCheckConfig::from_env();
                check_for_update(env!("CARGO_PKG_VERSION"), &config)
            })
            .await
            .ok()
            .flatten();

            if let Some(info) = info
                && info.update_available
            {
                let message = format!(
                    "Update available: {} -> {} | Run: semantiq update | Or: https://github.com/so-keyldzn/semantiq/releases",
                    info.current_version, info.latest_version
                );
                let _ = peer
                    .notify_logging_message(
                        rmcp::model::LoggingMessageNotificationParam::new(
                            rmcp::model::LoggingLevel::Warning,
                            serde_json::json!(message),
                        )
                        .with_logger("semantiq"),
                    )
                    .await;
            }
        });
    }

    /// Run a retrieval call on the blocking pool. Engine calls do SQLite I/O
    /// behind a std mutex (held for long stretches during reindexing), ONNX
    /// inference and filesystem walks; running them inline on an async worker
    /// would let a few concurrent requests park the whole runtime.
    pub async fn run_blocking<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&RetrievalEngine) -> Result<T> + Send + 'static,
    {
        let engine = Arc::clone(&self.engine);
        tokio::task::spawn_blocking(move || f(&engine))
            .await
            .map_err(|e| anyhow::anyhow!("blocking task failed: {}", e))?
    }

    pub fn store(&self) -> &Arc<IndexStore> {
        &self.store
    }

    pub fn engine(&self) -> &Arc<RetrievalEngine> {
        &self.engine
    }

    /// Whether the initial index pass is still running (results may be incomplete).
    pub fn is_initial_indexing(&self) -> bool {
        self.initial_indexing.load(Ordering::Relaxed)
    }

    /// Prepend a warning to a tool response while the initial index is incomplete.
    fn with_indexing_notice(&self, output: String) -> String {
        if self.is_initial_indexing() {
            format!(
                "⏳ Initial indexing in progress: results may be incomplete.\n\n{}",
                output
            )
        } else {
            output
        }
    }

    /// Start the auto-indexing background task
    /// Performs initial indexing first, then watches for changes
    pub fn start_auto_indexer(&self) {
        if let Some(ref auto_indexer) = self.auto_indexer {
            let indexer = Arc::clone(auto_indexer);
            let initial_indexing = Arc::clone(&self.initial_indexing);
            initial_indexing.store(true, Ordering::Relaxed);

            tokio::spawn(async move {
                // Perform initial indexing in a blocking task
                let indexer_clone = Arc::clone(&indexer);
                let initial_result = tokio::task::spawn_blocking(move || {
                    let indexer = indexer_clone.blocking_lock();
                    indexer.initial_index()
                })
                .await;
                initial_indexing.store(false, Ordering::Relaxed);

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

                    // process_events() parses files, runs ONNX, and writes to SQLite
                    // (all blocking). Run it on the blocking pool so it does not stall
                    // the async runtime. Clone the Arc and move it into the closure.
                    let indexer_clone = Arc::clone(&indexer);
                    let result = tokio::task::spawn_blocking(move || {
                        let indexer = indexer_clone.blocking_lock();
                        indexer.process_events()
                    })
                    .await;

                    match result {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => {
                            tracing::error!("Auto-indexer error: {}", e);
                        }
                        Err(e) => {
                            tracing::error!("Auto-indexer task panicked: {}", e);
                        }
                    }
                }
            });

            info!("Auto-indexer background task started");
        }
    }
}

/// Wrap a tool output as both markdown text (for clients that only read
/// text) and `structuredContent` matching the tool's `outputSchema`.
fn structured_result<T: Serialize>(output: &T, text: String) -> Result<CallToolResult, String> {
    let value = serde_json::to_value(output).map_err(|e| {
        error!("Failed to serialize tool output: {}", e);
        "an internal error occurred".to_string()
    })?;
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(value);
    Ok(result)
}

impl SemantiqServer {
    /// Run a shared output builder on the blocking pool. A failed join maps to
    /// the tool's opaque error, like any other internal error.
    async fn run_tool<P, T>(
        &self,
        label: &'static str,
        params: P,
        f: fn(&RetrievalEngine, P) -> Result<T, String>,
    ) -> Result<T, String>
    where
        P: Send + 'static,
        T: Send + 'static,
    {
        self.run_blocking(move |engine| Ok(f(engine, params)))
            .await
            .map_err(|e| {
                error!("{} failed: {}", label, e);
                format!("{} failed: an internal error occurred", label)
            })?
    }

    pub async fn search(&self, params: SearchParams) -> Result<SearchOutput, String> {
        self.run_tool("Search", params, search_output).await
    }

    pub async fn repo_map(&self, params: RepoMapParams) -> Result<RepoMapOutput, String> {
        self.run_tool("Repo map", params, repo_map_output).await
    }

    pub async fn find_refs(&self, params: FindRefsParams) -> Result<FindRefsOutput, String> {
        self.run_tool("Find references", params, find_refs_output)
            .await
    }

    pub async fn deps(&self, params: DepsParams) -> Result<DepsOutput, String> {
        self.run_tool("Dependency analysis", params, deps_output)
            .await
    }

    pub async fn impact(&self, params: ImpactParams) -> Result<ImpactOutput, String> {
        self.run_tool("Impact analysis", params, impact_output)
            .await
    }

    pub async fn explain(&self, params: ExplainParams) -> Result<ExplainOutput, String> {
        self.run_tool("Explain", params, explain_output).await
    }
}

#[tool_router]
impl SemantiqServer {
    #[tool(
        name = "semantiq_search",
        description = "Search the indexed codebase by meaning, symbol name, or text. Prefer this over grep for natural-language questions (\"where is auth handled?\") and fuzzy symbol lookups. Returns file paths, line ranges, scores and snippets.",
        output_schema = schema_for_output::<SearchOutput>(),
        annotations(title = "Search code", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.search(params).await?;
        structured_result(&output, self.with_indexing_notice(output.render()))
    }

    #[tool(
        name = "semantiq_repo_map",
        description = "Get a compact map of the repository: its most important files and, for each, the signatures of its key symbols, ranked by how much the rest of the code uses them (PageRank over references and imports). Call it first when starting a task in an unfamiliar repository, before searching or reading files. Pass focus (files, directories or symbol names) to center the map on the code a task touches, and max_tokens to size it.",
        output_schema = schema_for_output::<RepoMapOutput>(),
        annotations(title = "Repository map", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_repo_map(
        &self,
        Parameters(params): Parameters<RepoMapParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.repo_map(params).await?;
        structured_result(&output, self.with_indexing_notice(output.render()))
    }

    #[tool(
        name = "semantiq_find_refs",
        description = "Find the definitions and usages of a symbol across the codebase, from the syntax tree: comments, strings and longer names containing it are never matched (unlike grep). Each usage is tagged call, type, import or reference. Use it before renaming or changing a function, type or method.",
        output_schema = schema_for_output::<FindRefsOutput>(),
        annotations(title = "Find references", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_find_refs(
        &self,
        Parameters(params): Parameters<FindRefsParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.find_refs(params).await?;
        structured_result(&output, self.with_indexing_notice(output.render()))
    }

    #[tool(
        name = "semantiq_deps",
        description = "Show the dependency graph of a file: what it imports and which files import it. Use it to estimate the impact of changing a file.",
        output_schema = schema_for_output::<DepsOutput>(),
        annotations(title = "File dependencies", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_deps(
        &self,
        Parameters(params): Parameters<DepsParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.deps(params).await?;
        structured_result(&output, self.with_indexing_notice(output.render()))
    }

    #[tool(
        name = "semantiq_explain",
        description = "Explain a symbol: its definitions with signature and documentation, usage count, and other symbols defined alongside it.",
        output_schema = schema_for_output::<ExplainOutput>(),
        annotations(title = "Explain symbol", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_explain(
        &self,
        Parameters(params): Parameters<ExplainParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.explain(params).await?;
        structured_result(&output, self.with_indexing_notice(output.render()))
    }
    #[tool(
        name = "semantiq_impact",
        description = "Before changing a function, method or type, list what may break: every place that uses it, then the users of those places (up to max_depth), grouped by file, with the test files to run. Each site has a confidence (same_file, imports, unique_name, name_only) since matching is by name.",
        output_schema = schema_for_output::<ImpactOutput>(),
        annotations(title = "Change impact", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_impact(
        &self,
        Parameters(params): Parameters<ImpactParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.impact(params).await?;
        structured_result(&output, self.with_indexing_notice(output.render()))
    }
}

#[tool_handler]
impl ServerHandler for SemantiqServer {
    fn get_info(&self) -> ServerConfig {
        let mut instructions = String::from(
            "Semantiq indexes this project (symbols, chunks, embeddings, imports) for \
             semantic code understanding. On an unfamiliar repository, start with \
             semantiq_repo_map for a ranked overview of its key files and symbols (pass \
             focus to center it on the files of the task). Use semantiq_search for natural-language or fuzzy \
             code search, semantiq_find_refs to trace symbol usage, semantiq_deps to see a \
             file's imports and dependents, semantiq_impact before changing a symbol, and semantiq_explain for a symbol's definition \
             and documentation. Plain grep remains better for exact string matches.",
        );
        if let Some(reason) = semantiq_embeddings::semantic_search_unavailable_reason() {
            instructions.push_str(&format!(
                " Note: semantic (embedding) search is unavailable because {reason}; \
                 semantiq_search only matches symbol names and text."
            ));
        }
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("semantiq", env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions)
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        // Now that we have a peer connection, spawn the version check
        Self::spawn_version_check(context.peer);
    }
}

#[cfg(test)]
mod tests;
