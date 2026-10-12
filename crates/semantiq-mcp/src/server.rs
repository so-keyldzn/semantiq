use anyhow::Result;
use rmcp::{
    ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    service::{NotificationContext, Peer, RoleServer},
    tool, tool_handler, tool_router,
};
use semantiq_embeddings::ModelStatus;
use semantiq_index::{AutoIndexer, BackgroundEmbedder, EmbeddingCounts, IndexStore};
use semantiq_retrieval::RetrievalEngine;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::version_check::{VersionCheckConfig, check_for_update};

mod outputs;
mod schema;
mod structure;
mod structure_types;
mod types;
pub use outputs::*;
pub use schema::{input_schema, output_schema};
pub use structure::*;
pub use structure_types::*;
pub use types::*;

#[derive(Clone)]
pub struct SemantiqServer {
    engine: Arc<RetrievalEngine>,
    store: Arc<IndexStore>,
    auto_indexer: Option<Arc<Mutex<AutoIndexer>>>,
    /// False when the file watcher could not be created (e.g. inotify watch
    /// limit): the initial index and the embedder still run, but edits made
    /// after startup are not reindexed.
    watching: bool,
    /// True while the initial index pass (phase 1: structure) started by
    /// `start_auto_indexer` runs.
    initial_indexing: Arc<AtomicBool>,
    /// Phase 2 (embeddings) runner, started by `start_auto_indexer`.
    embedder: Arc<OnceLock<BackgroundEmbedder>>,
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
        // Once per server, not per CLI command (#164).
        store.warn_on_vector_integrity_loss();

        let engine = Arc::new(RetrievalEngine::new(Arc::clone(&store), project_root));

        // Initialize auto-indexer with the same shared store
        let project_root = PathBuf::from(project_root);
        let (auto_indexer, watching) =
            match AutoIndexer::new(Arc::clone(&store), project_root.clone()) {
                Ok(indexer) => {
                    info!("Auto-indexing enabled");
                    (Some(Arc::new(Mutex::new(indexer))), true)
                }
                Err(e) => Self::indexer_without_watcher(&store, project_root, &e),
            };

        Ok(Self {
            engine,
            store,
            auto_indexer,
            watching,
            initial_indexing: Arc::new(AtomicBool::new(false)),
            embedder: Arc::new(OnceLock::new()),
        })
    }

    /// Fallback when the file watcher cannot be created (inotify watch limit
    /// on a large tree, unsupported filesystem): index without one so the
    /// initial pass and the embedder still run, and say so.
    fn indexer_without_watcher(
        store: &Arc<IndexStore>,
        project_root: PathBuf,
        cause: &anyhow::Error,
    ) -> (Option<Arc<Mutex<AutoIndexer>>>, bool) {
        match AutoIndexer::without_watcher(Arc::clone(store), project_root) {
            Ok(indexer) => {
                warn!(
                    "File watcher unavailable, live reindexing is off (restart the server or \
                     run `semantiq index` after editing files): {}",
                    cause
                );
                (Some(Arc::new(Mutex::new(indexer))), false)
            }
            Err(e) => {
                error!(
                    "Auto-indexing disabled: file watcher unavailable ({}) and indexer \
                     initialization failed ({})",
                    cause, e
                );
                (None, false)
            }
        }
    }

    /// Whether the file watcher reindexes edits made while the server runs.
    pub fn is_watching(&self) -> bool {
        self.watching
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

    /// Embedded / total chunks while the background embedder (phase 2) still
    /// has chunks to embed; `None` once the semantic index is complete.
    pub fn embedding_in_progress(&self) -> Option<EmbeddingCounts> {
        self.embedder.get()?.progress().in_progress()
    }

    /// Why the background embedder's last pass failed (model load, or a
    /// pass that stopped unexpectedly; both retried with a backoff), `None`
    /// otherwise. Short text, safe for clients.
    pub fn embedding_model_error(&self) -> Option<String> {
        self.embedder.get()?.progress().model_error()
    }

    /// Why semantic search is off right now, most permanent first: the stub
    /// backend; the shared model slot (loading, or its last load failed and
    /// is retried after a backoff, by the embedder thread and by the next
    /// search); the embedder's last pass failed. `None` = available. Never
    /// loads the model itself.
    pub fn semantic_search_unavailable_reason(&self) -> Option<String> {
        unavailable_reason(
            semantiq_embeddings::semantic_search_unavailable_reason(),
            self.engine.model_slot().status(),
            self.embedding_model_error(),
        )
    }

    /// The runtime part of `semantic_search_unavailable_reason` (everything
    /// but the backend), for the search notice.
    fn runtime_unavailable_reason(&self) -> Option<String> {
        unavailable_reason(
            None,
            self.engine.model_slot().status(),
            self.embedding_model_error(),
        )
    }

    /// Like `with_indexing_notice`, plus the share of the semantic index that
    /// is ready while phase 2 runs: only `semantiq_search` uses embeddings.
    fn with_search_notice(&self, output: String) -> String {
        if self.is_initial_indexing() {
            return self.with_indexing_notice(output);
        }
        search_notice(
            semantiq_embeddings::semantic_search_unavailable_reason(),
            self.runtime_unavailable_reason(),
            self.embedding_in_progress(),
            output,
        )
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

    /// Start the auto-indexing background task.
    ///
    /// Phase 1 (structure) runs first over the whole project; then the
    /// background embedder (phase 2) is woken and fills the chunk embeddings on
    /// its own thread while queries and the watcher go on. Watcher events run
    /// phase 1 right away and queue their new chunks for the embedder. Without
    /// a watcher (`is_watching() == false`) only the initial pass runs; the
    /// embedder still polls for chunks left pending by another process.
    pub fn start_auto_indexer(&self) {
        if let Some(ref auto_indexer) = self.auto_indexer {
            let indexer = Arc::clone(auto_indexer);
            let initial_indexing = Arc::clone(&self.initial_indexing);
            let watching = self.watching;
            initial_indexing.store(true, Ordering::Relaxed);

            // The embedder loads the engine's own model slot: one copy of
            // the model, loaded after phase 1 (off the transport path), and
            // a load by either side serves both (#76, #97). `load_now`
            // follows the embedder's schedule, not the slot's backoff, and
            // returns the short public reason on failure (#100).
            let slot = Arc::clone(self.engine.model_slot());
            let embedder = match BackgroundEmbedder::spawn(
                Arc::clone(&self.store),
                Box::new(move || slot.load_now().map_err(anyhow::Error::new)),
            ) {
                Ok(embedder) => {
                    let _ = self.embedder.set(embedder);
                    Some(Arc::clone(&self.embedder))
                }
                Err(e) => {
                    error!("Background embedder not started: {}", e);
                    None
                }
            };
            let notify_embedder = move || {
                if let Some(embedder) = embedder.as_ref().and_then(|e| e.get()) {
                    embedder.notify();
                }
            };

            tokio::spawn(async move {
                // Perform initial indexing in a blocking task
                let indexer_clone = Arc::clone(&indexer);
                let initial_result = tokio::task::spawn_blocking(move || {
                    let indexer = indexer_clone.blocking_lock();
                    indexer.initial_index()
                })
                .await;
                initial_indexing.store(false, Ordering::Relaxed);
                // Phase 2 also picks up chunks left pending by an earlier run
                // (`semantiq index --no-embeddings`, query refresh, a crash).
                notify_embedder();

                match initial_result {
                    Ok(Ok(result)) => {
                        if result.indexed > 0 {
                            info!(
                                "Initial indexing complete (structure ready): {} files indexed, {} skipped",
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

                if !watching {
                    return;
                }

                // Then start watching for changes
                let mut interval = tokio::time::interval(Duration::from_secs(2));

                loop {
                    interval.tick().await;

                    // process_events() parses files and writes to SQLite (all
                    // blocking). Run it on the blocking pool so it does not stall
                    // the async runtime. Clone the Arc and move it into the closure.
                    let indexer_clone = Arc::clone(&indexer);
                    let result = tokio::task::spawn_blocking(move || {
                        let indexer = indexer_clone.blocking_lock();
                        indexer.process_events()
                    })
                    .await;

                    match result {
                        Ok(Ok(result)) => {
                            // Any event, not only a reindex: a file another
                            // process just indexed (CLI refresh) has
                            // `indexed == 0` but its chunks are pending.
                            if result.events > 0 {
                                notify_embedder();
                            }
                        }
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

/// Reason semantic search is unavailable, most permanent first; `None` when
/// the backend is ONNX, the shared model is loaded (or not asked for yet:
/// the next search loads it) and the embedder's last pass succeeded.
fn unavailable_reason(
    backend_reason: Option<&str>,
    model_status: ModelStatus,
    model_error: Option<String>,
) -> Option<String> {
    if let Some(reason) = backend_reason {
        return Some(reason.to_string());
    }
    match model_status {
        ModelStatus::Failed { reason, .. } => Some(model_error_reason(reason)),
        ModelStatus::Loading => Some(
            "the embedding model is being loaded; semantic matches are included once it is ready"
                .to_string(),
        ),
        ModelStatus::Disabled => Some("no embedding model is loaded in this context".to_string()),
        ModelStatus::NotLoaded | ModelStatus::Loaded => model_error.map(model_error_reason),
    }
}

/// Reason shown by the search notice and `GET /stats` when the embedding
/// model failed to load. `error` is the short classified reason, never the
/// raw error text (#100).
fn model_error_reason(error: String) -> String {
    format!("{error}; loading is retried in the background")
}

/// Notice prepended to a `semantiq_search` output. With the stub backend
/// (`backend_reason`) the instructions already say semantic search is off, so
/// nothing is added; `unavailable_reason` (model load failure) warns that
/// only symbol and text matches are returned; `in_progress` says how much of
/// the semantic index is ready.
fn search_notice(
    backend_reason: Option<&str>,
    unavailable_reason: Option<String>,
    in_progress: Option<EmbeddingCounts>,
    output: String,
) -> String {
    if backend_reason.is_some() {
        return output;
    }
    if let Some(reason) = unavailable_reason {
        return format!(
            "⚠️ Semantic index unavailable: {reason}. Only symbol and text matches are \
             returned.\n\n{output}"
        );
    }
    match in_progress {
        Some(counts) => format!(
            "⏳ Semantic index {}% ready ({}/{} chunks embedded): some meaning-based \
             matches may be missing; symbol and text matches are complete.\n\n{}",
            counts.percent(),
            counts.embedded,
            counts.total,
            output
        ),
        None => output,
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

    pub async fn calls(&self, params: CallsParams) -> Result<CallsOutput, String> {
        self.run_tool("Call graph", params, calls_output).await
    }

    pub async fn hierarchy(&self, params: HierarchyParams) -> Result<HierarchyOutput, String> {
        self.run_tool("Type hierarchy", params, hierarchy_output)
            .await
    }

    pub async fn dead_code(&self, params: DeadCodeParams) -> Result<DeadCodeOutput, String> {
        self.run_tool("Dead code analysis", params, dead_code_output)
            .await
    }
}

#[tool_router]
impl SemantiqServer {
    #[tool(
        name = "semantiq_search",
        description = "Find code by meaning or approximate symbol name when there is no exact string to grep (\"where is auth handled?\"). Returns path:lines, symbol and one preview line per hit; snippets=true adds the code.",
        input_schema = input_schema::<SearchParams>(),
        output_schema = output_schema::<SearchOutput>(),
        annotations(title = "Search code", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.search(params).await?;
        structured_result(&output, self.with_search_notice(output.render()))
    }

    #[tool(
        name = "semantiq_repo_map",
        description = "Ranked overview of the repository: key files and symbol signatures, by how much the code uses them. Call it first on an unfamiliar repo; focus centers it on the task's files or symbols.",
        input_schema = input_schema::<RepoMapParams>(),
        output_schema = output_schema::<RepoMapOutput>(),
        annotations(title = "Repository map", read_only_hint = true, open_world_hint = false)
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
        description = "Definitions and usages of a symbol from the syntax tree, tagged call/type/import/reference. Unlike grep, never matches comments, strings or longer names.",
        input_schema = input_schema::<FindRefsParams>(),
        output_schema = output_schema::<FindRefsOutput>(),
        annotations(title = "Find references", read_only_hint = true, open_world_hint = false)
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
        description = "What a file imports and which files import it.",
        input_schema = input_schema::<DepsParams>(),
        output_schema = output_schema::<DepsOutput>(),
        annotations(title = "File dependencies", read_only_hint = true, open_world_hint = false)
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
        description = "Definitions of a symbol with signature and docs, its usage count and the symbols defined next to it.",
        input_schema = input_schema::<ExplainParams>(),
        output_schema = output_schema::<ExplainOutput>(),
        annotations(title = "Explain symbol", read_only_hint = true, open_world_hint = false)
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
        description = "Before changing a symbol: every place that may break, transitively up to max_depth, grouped by file, with the tests to run.",
        input_schema = input_schema::<ImpactParams>(),
        output_schema = output_schema::<ImpactOutput>(),
        annotations(title = "Change impact", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_impact(
        &self,
        Parameters(params): Parameters<ImpactParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.impact(params).await?;
        structured_result(&output, self.with_indexing_notice(output.render()))
    }

    #[tool(
        name = "semantiq_calls",
        description = "Callers and callees of a function, transitively up to max_depth; each edge names the enclosing function, which grep cannot.",
        input_schema = input_schema::<CallsParams>(),
        output_schema = output_schema::<CallsOutput>(),
        annotations(title = "Call graph", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_calls(
        &self,
        Parameters(params): Parameters<CallsParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.calls(params).await?;
        structured_result(&output, self.with_indexing_notice(output.render()))
    }

    #[tool(
        name = "semantiq_hierarchy",
        description = "What a type extends or implements and every subtype or implementor, transitively (not Go interfaces). Replaces grepping for \"impl X for\" / \"extends X\".",
        input_schema = input_schema::<HierarchyParams>(),
        output_schema = output_schema::<HierarchyOutput>(),
        annotations(title = "Type hierarchy", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_hierarchy(
        &self,
        Parameters(params): Parameters<HierarchyParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.hierarchy(params).await?;
        structured_result(&output, self.with_indexing_notice(output.render()))
    }

    #[tool(
        name = "semantiq_dead_code",
        description = "Functions, methods and types referenced nowhere, excluding entry points, tests, trait members and public symbols (unless include_public).",
        input_schema = input_schema::<DeadCodeParams>(),
        output_schema = output_schema::<DeadCodeOutput>(),
        annotations(title = "Dead code", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn semantiq_dead_code(
        &self,
        Parameters(params): Parameters<DeadCodeParams>,
    ) -> Result<CallToolResult, String> {
        let output = self.dead_code(params).await?;
        structured_result(&output, self.with_indexing_notice(output.render()))
    }
}

#[tool_handler]
impl ServerHandler for SemantiqServer {
    fn get_info(&self) -> ServerConfig {
        let mut instructions = String::from(
            "Semantiq answers structural questions about this project from a syntax-aware \
             index. Start an unfamiliar repo with semantiq_repo_map. Prefer semantiq_find_refs, \
             semantiq_calls, semantiq_hierarchy and semantiq_impact over grep for usages, call \
             graphs, subtypes and change impact; semantiq_search for code you cannot name; \
             semantiq_deps, semantiq_explain and semantiq_dead_code for imports, symbol docs \
             and unused code. Grep stays better for exact strings.",
        );
        if let Some(reason) = semantiq_embeddings::semantic_search_unavailable_reason() {
            instructions.push_str(&format!(
                " Note: semantic (embedding) search is unavailable because {reason}; \
                 semantiq_search only matches symbol names and text."
            ));
        } else if self.is_initial_indexing() || self.embedding_in_progress().is_some() {
            instructions.push_str(
                " The index is being built: structural tools answer as soon as files are \
                 parsed, while embeddings are computed in the background; until they are \
                 complete, semantiq_search says how much of the semantic index is ready.",
            );
        }
        if self.auto_indexer.is_none() {
            instructions.push_str(
                " Note: auto-indexing is off in this session (the indexer could not be \
                 created); the index is updated only by `semantiq index`, so answers may be \
                 stale.",
            );
        } else if !self.watching {
            instructions.push_str(
                " Note: live file watching is off in this session (the watcher could not be \
                 created); files edited after startup are reindexed only by `semantiq index` \
                 or a server restart.",
            );
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
