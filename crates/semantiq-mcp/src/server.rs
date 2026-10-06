use anyhow::Result;
use rmcp::{
    ServerHandler,
    handler::server::{tool::schema_for_output, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    service::{NotificationContext, Peer, RoleServer},
    tool, tool_handler, tool_router,
};
use semantiq_index::{AutoIndexer, IndexStore};
use semantiq_retrieval::{
    DEFAULT_IMPACT_DEPTH, DEFAULT_IMPACT_SITES, RetrievalEngine, SearchOptions,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

use crate::version_check::{VersionCheckConfig, check_for_update};

mod types;
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

/// Trim and bound a required string argument.
fn validate_input(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{} cannot be empty", label));
    }
    if value.len() > MAX_INPUT_LEN {
        return Err(format!(
            "{} exceeds maximum length of {} characters",
            label, MAX_INPUT_LEN
        ));
    }
    Ok(value.to_string())
}

const MAX_INPUT_LEN: usize = 500;

impl SemantiqServer {
    pub async fn search(&self, params: SearchParams) -> Result<SearchOutput, String> {
        debug!(
            query = %params.query,
            limit = ?params.limit,
            file_type = ?params.file_type,
            symbol_kind = ?params.symbol_kind,
            "semantiq_search called"
        );

        let query = validate_input(&params.query, "Query")?;
        let limit = params.limit.unwrap_or(20).min(1000);

        let mut options = SearchOptions::new();

        if let Some(score) = params.min_score {
            options = options.with_min_score(score);
        }

        if let Some(ref ft) = params.file_type {
            let types = SearchOptions::parse_csv(ft);
            if !types.is_empty() {
                options = options.with_file_types(types);
            }
        }

        if let Some(ref sk) = params.symbol_kind {
            let kinds = SearchOptions::parse_csv(sk);
            if !kinds.is_empty() {
                options = options.with_symbol_kinds(kinds);
            }
        }

        let owned_query = query.clone();
        let results = self
            .run_blocking(move |engine| engine.search(&owned_query, limit, Some(options)))
            .await
            .map_err(|e| {
                error!("Search failed: {}", e);
                "Search failed: an internal error occurred".to_string()
            })?;

        Ok(SearchOutput {
            query,
            total_count: results.total_count,
            search_time_ms: results.search_time_ms,
            results: results
                .results
                .into_iter()
                .map(|r| SearchHit {
                    file_path: r.file_path,
                    start_line: r.start_line,
                    end_line: r.end_line,
                    score: r.score,
                    symbol_name: r.metadata.symbol_name,
                    symbol_kind: r.metadata.symbol_kind,
                    content: r.content,
                })
                .collect(),
        })
    }

    pub async fn find_refs(&self, params: FindRefsParams) -> Result<FindRefsOutput, String> {
        debug!(symbol = %params.symbol, limit = ?params.limit, "semantiq_find_refs called");

        let symbol = validate_input(&params.symbol, "Symbol name")?;
        let limit = params.limit.unwrap_or(50).min(1000);

        let owned_symbol = symbol.clone();
        let results = self
            .run_blocking(move |engine| engine.find_references(&owned_symbol, limit))
            .await
            .map_err(|e| {
                error!("Find references failed: {}", e);
                "Find references failed: an internal error occurred".to_string()
            })?;

        let mut definitions = Vec::new();
        let mut usages = Vec::new();
        for r in results.results {
            let kind = r
                .metadata
                .match_type
                .unwrap_or_else(|| "reference".to_string());
            let is_definition = kind == "definition";
            let reference = Reference {
                file_path: r.file_path,
                line: r.start_line,
                kind,
                content: r.content,
            };
            if is_definition {
                definitions.push(reference);
            } else {
                usages.push(reference);
            }
        }

        Ok(FindRefsOutput {
            symbol,
            total_count: results.total_count,
            search_time_ms: results.search_time_ms,
            definitions,
            usages,
        })
    }

    pub async fn deps(&self, params: DepsParams) -> Result<DepsOutput, String> {
        debug!(file = %params.file_path, "semantiq_deps called");

        let file_path = validate_input(&params.file_path, "File path")?;
        // Reject path traversal attempts
        if file_path.contains("..") {
            return Err("File path must not contain '..'".to_string());
        }

        let owned_path = file_path.clone();
        let (dependencies, dependents) = self
            .run_blocking(move |engine| {
                Ok((
                    engine.get_dependencies(&owned_path),
                    engine.get_dependents(&owned_path),
                ))
            })
            .await
            .map_err(|e| {
                error!("Dependency analysis failed: {}", e);
                "Dependency analysis failed: an internal error occurred".to_string()
            })?;

        let imports = dependencies
            .inspect_err(|e| error!("Could not analyze imports: {}", e))
            .ok()
            .map(|deps| {
                deps.into_iter()
                    .map(|d| Import {
                        target_path: d.target_path,
                        import_name: d.import_name,
                        kind: d.kind,
                    })
                    .collect()
            });
        let imported_by = dependents
            .inspect_err(|e| error!("Could not analyze dependents: {}", e))
            .ok()
            .map(|deps| deps.into_iter().map(|d| d.target_path).collect());

        Ok(DepsOutput {
            file_path,
            imports,
            imported_by,
        })
    }

    pub async fn impact(&self, params: ImpactParams) -> Result<ImpactOutput, String> {
        debug!(symbol = %params.symbol, file = ?params.file_path, "semantiq_impact called");

        let symbol = validate_input(&params.symbol, "Symbol name")?;
        let file_path = match params.file_path {
            Some(ref path) => {
                let path = validate_input(path, "File path")?;
                if path.contains("..") {
                    return Err("File path must not contain '..'".to_string());
                }
                Some(path)
            }
            None => None,
        };
        let max_depth = params.max_depth.unwrap_or(DEFAULT_IMPACT_DEPTH);
        let limit = params.limit.unwrap_or(DEFAULT_IMPACT_SITES);

        let owned_symbol = symbol.clone();
        let analysis = self
            .run_blocking(move |engine| {
                engine.analyze_impact(&owned_symbol, file_path.as_deref(), max_depth, limit)
            })
            .await
            .map_err(|e| {
                error!("Impact analysis failed: {}", e);
                "Impact analysis failed: an internal error occurred".to_string()
            })?;

        // Group sites by file, closest impact first.
        let mut files: Vec<ImpactedFile> = Vec::new();
        for site in &analysis.sites {
            let out = ImpactSiteOut {
                line: site.line,
                depth: site.depth,
                target: site.target.clone(),
                kind: site.kind.clone(),
                enclosing: site.enclosing.as_ref().map(|e| e.name.clone()),
                confidence: site.confidence.as_str().to_string(),
            };
            match files.iter_mut().find(|f| f.file_path == site.file_path) {
                Some(file) => {
                    file.depth = file.depth.min(site.depth);
                    file.is_test |= site.is_test;
                    file.sites.push(out);
                }
                None => files.push(ImpactedFile {
                    file_path: site.file_path.clone(),
                    is_test: site.is_test,
                    depth: site.depth,
                    sites: vec![out],
                }),
            }
        }
        files.sort_by(|a, b| {
            a.depth
                .cmp(&b.depth)
                .then_with(|| a.file_path.cmp(&b.file_path))
        });
        for file in &mut files {
            file.sites
                .sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.depth.cmp(&b.depth)));
        }
        let test_files = files
            .iter()
            .filter(|f| f.is_test)
            .map(|f| f.file_path.clone())
            .collect();

        Ok(ImpactOutput {
            symbol: analysis.symbol,
            definitions: analysis
                .definitions
                .into_iter()
                .map(|d| ImpactDefinitionOut {
                    file_path: d.file_path,
                    line: d.line,
                    kind: d.kind,
                })
                .collect(),
            site_count: analysis.sites.len(),
            files,
            test_files,
            truncated: analysis.truncated,
        })
    }

    pub async fn explain(&self, params: ExplainParams) -> Result<ExplainOutput, String> {
        debug!(symbol = %params.symbol, "semantiq_explain called");

        let symbol = validate_input(&params.symbol, "Symbol name")?;

        let owned_symbol = symbol.clone();
        let explanation = self
            .run_blocking(move |engine| engine.explain_symbol(&owned_symbol))
            .await
            .map_err(|e| {
                error!("Explain failed: {}", e);
                "Explain failed: an internal error occurred".to_string()
            })?;

        if !explanation.found {
            return Ok(ExplainOutput {
                symbol,
                found: false,
                definitions: Vec::new(),
                usage_count: 0,
                related_symbols: Vec::new(),
            });
        }

        // The engine collects related symbols in a HashSet; sort so the
        // truncated list is stable across calls.
        let mut related_symbols = explanation.related_symbols;
        related_symbols.sort();
        related_symbols.truncate(10);

        Ok(ExplainOutput {
            symbol: explanation.name,
            found: true,
            definitions: explanation
                .definitions
                .into_iter()
                .map(|d| Definition {
                    file_path: d.file_path,
                    kind: d.kind,
                    start_line: d.start_line,
                    end_line: d.end_line,
                    signature: d.signature,
                    doc_comment: d.doc_comment,
                })
                .collect(),
            usage_count: explanation.usage_count,
            related_symbols,
        })
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
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("semantiq", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Semantiq indexes this project (symbols, chunks, embeddings, imports) for \
                 semantic code understanding. Use semantiq_search for natural-language or fuzzy \
                 code search, semantiq_find_refs to trace symbol usage, semantiq_deps to see a \
                 file's imports and dependents, semantiq_impact before changing a symbol, and semantiq_explain for a symbol's definition \
                 and documentation. Plain grep remains better for exact string matches.",
            )
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        // Now that we have a peer connection, spawn the version check
        Self::spawn_version_check(context.peer);
    }
}

#[cfg(test)]
mod tests;
