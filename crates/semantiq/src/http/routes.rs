//! HTTP API routes and handlers

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use semantiq_mcp::SemantiqServer;
use semantiq_mcp::server::RepoMapParams;
use semantiq_retrieval::SearchOptions;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error};

use super::types::{
    Dependency, DepsRequest, DepsResponse, ErrorResponse, ExplainRequest, ExplainResponse,
    FindRefsRequest, FindRefsResponse, HealthResponse, MapRequest, MapResponse, Reference,
    SearchMetadata, SearchRequest, SearchResponse, SearchResult, StatsResponse, SymbolDefinition,
};

type AppState = Arc<SemantiqServer>;

/// Create the router with all API endpoints
pub(crate) fn create_router(server: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/stats", get(stats))
        .route("/search", post(search))
        .route("/map", post(map))
        .route("/find-refs", post(find_refs))
        .route("/deps", post(deps))
        .route("/explain", post(explain))
        .with_state(server)
}

// ============================================
// Health & Stats
// ============================================

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

async fn stats(
    State(server): State<AppState>,
) -> Result<Json<StatsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let store = Arc::clone(server.store());
    let stats = tokio::task::spawn_blocking(move || store.get_stats())
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("blocking task failed: {}", e)));

    match stats {
        Ok(stats) => Ok(Json(StatsResponse {
            indexed_files: stats.file_count,
            indexed_symbols: stats.symbol_count,
            indexed_chunks: stats.chunk_count,
            indexed_dependencies: stats.dependency_count,
            indexing: server.is_initial_indexing(),
        })),
        Err(e) => {
            error!("Failed to get stats: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get stats".to_string(),
                    code: "STATS_ERROR".to_string(),
                }),
            ))
        }
    }
}

// ============================================
// Search
// ============================================

async fn search(
    State(server): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, (StatusCode, Json<ErrorResponse>)> {
    let start = Instant::now();

    // Validate query
    let query = req.query.trim();
    if query.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Query cannot be empty".to_string(),
                code: "INVALID_QUERY".to_string(),
            }),
        ));
    }
    if query.len() > 500 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Query exceeds maximum length of 500 characters".to_string(),
                code: "QUERY_TOO_LONG".to_string(),
            }),
        ));
    }

    let limit = req.limit.unwrap_or(20).min(100);

    // Build SearchOptions
    let mut options = SearchOptions::new();

    if let Some(score) = req.min_score {
        options = options.with_min_score(score);
    }

    if let Some(ref ft) = req.file_type {
        let types = SearchOptions::parse_csv(ft);
        if !types.is_empty() {
            options = options.with_file_types(types);
        }
    }

    if let Some(ref sk) = req.symbol_kind {
        let kinds = SearchOptions::parse_csv(sk);
        if !kinds.is_empty() {
            options = options.with_symbol_kinds(kinds);
        }
    }

    debug!(query = %query, limit = %limit, "HTTP search request");

    let owned_query = query.to_string();
    let search = server
        .run_blocking(move |engine| engine.search(&owned_query, limit, Some(options)))
        .await;
    match search {
        Ok(results) => {
            let search_time_ms = start.elapsed().as_millis() as u64;

            let response = SearchResponse {
                total_count: results.total_count,
                search_time_ms,
                results: results
                    .results
                    .into_iter()
                    .map(|r| SearchResult {
                        file_path: r.file_path,
                        start_line: r.start_line as u32,
                        end_line: r.end_line as u32,
                        score: r.score,
                        content: r.content,
                        metadata: SearchMetadata {
                            symbol_name: r.metadata.symbol_name,
                            symbol_kind: r.metadata.symbol_kind,
                        },
                    })
                    .collect(),
            };

            Ok(Json(response))
        }
        Err(e) => {
            error!("Search failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Search failed".to_string(),
                    code: "SEARCH_ERROR".to_string(),
                }),
            ))
        }
    }
}

// ============================================
// Repo map
// ============================================

async fn map(
    State(server): State<AppState>,
    Json(req): Json<MapRequest>,
) -> Result<Json<MapResponse>, (StatusCode, Json<ErrorResponse>)> {
    let start = Instant::now();

    debug!(max_tokens = ?req.max_tokens, focus = ?req.focus, "HTTP map request");

    let output = server
        .repo_map(RepoMapParams {
            max_tokens: req.max_tokens,
            focus: req.focus,
            path_prefix: req.path_prefix,
        })
        .await
        .map_err(|e| {
            // Validation errors are user-facing; internal ones are already opaque.
            let internal = e.contains("internal error");
            (
                if internal {
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    StatusCode::BAD_REQUEST
                },
                Json(ErrorResponse {
                    error: e,
                    code: if internal {
                        "MAP_ERROR"
                    } else {
                        "INVALID_MAP_REQUEST"
                    }
                    .to_string(),
                }),
            )
        })?;

    let details = serde_json::to_value(&output).map_err(|e| {
        error!("Failed to serialize repo map: {}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Repo map failed".to_string(),
                code: "MAP_ERROR".to_string(),
            }),
        )
    })?;

    Ok(Json(MapResponse {
        map: output.render(),
        details,
        search_time_ms: start.elapsed().as_millis() as u64,
    }))
}

// ============================================
// Find Refs
// ============================================

async fn find_refs(
    State(server): State<AppState>,
    Json(req): Json<FindRefsRequest>,
) -> Result<Json<FindRefsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let start = Instant::now();

    // Validate symbol
    let symbol = req.symbol.trim();
    if symbol.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Symbol cannot be empty".to_string(),
                code: "INVALID_SYMBOL".to_string(),
            }),
        ));
    }
    if symbol.len() > 500 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Symbol exceeds maximum length of 500 characters".to_string(),
                code: "SYMBOL_TOO_LONG".to_string(),
            }),
        ));
    }

    let limit = req.limit.unwrap_or(50).min(100);

    debug!(symbol = %symbol, limit = %limit, "HTTP find_refs request");

    let owned_symbol = symbol.to_string();
    let refs = server
        .run_blocking(move |engine| engine.find_references(&owned_symbol, limit))
        .await;
    match refs {
        Ok(results) => {
            let search_time_ms = start.elapsed().as_millis() as u64;

            let mut definitions: Vec<Reference> = Vec::new();
            let mut references: Vec<Reference> = Vec::new();

            for r in results.results {
                let is_definition = r.metadata.match_type.as_deref() == Some("definition");

                if is_definition {
                    definitions.push(Reference {
                        file_path: r.file_path,
                        line: r.start_line as u32,
                        column: None,
                        usage_type: "definition".to_string(),
                        context: Some(r.content.lines().next().unwrap_or("").to_string()),
                    });
                } else {
                    let usage_type = r.metadata.match_type.unwrap_or_else(|| "usage".to_string());
                    let context = r.content.trim().to_string();
                    references.push(Reference {
                        file_path: r.file_path,
                        line: r.start_line as u32,
                        column: None,
                        usage_type,
                        context: Some(context),
                    });
                }
            }

            let response = FindRefsResponse {
                symbol: symbol.to_string(),
                total_count: results.total_count,
                search_time_ms,
                definitions,
                references,
            };

            Ok(Json(response))
        }
        Err(e) => {
            error!("Find refs failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Find references failed".to_string(),
                    code: "FIND_REFS_ERROR".to_string(),
                }),
            ))
        }
    }
}

// ============================================
// Deps
// ============================================

async fn deps(
    State(server): State<AppState>,
    Json(req): Json<DepsRequest>,
) -> Result<Json<DepsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let start = Instant::now();

    // Validate file_path
    let file_path = req.file_path.trim();
    if file_path.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "File path cannot be empty".to_string(),
                code: "INVALID_PATH".to_string(),
            }),
        ));
    }
    if file_path.len() > 500 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "File path exceeds maximum length".to_string(),
                code: "PATH_TOO_LONG".to_string(),
            }),
        ));
    }
    if file_path.contains("..") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "File path must not contain '..'".to_string(),
                code: "PATH_TRAVERSAL".to_string(),
            }),
        ));
    }

    debug!(file_path = %file_path, "HTTP deps request");

    let owned_path = file_path.to_string();
    let (dependencies, dependents) = server
        .run_blocking(move |engine| {
            Ok((
                engine.get_dependencies(&owned_path),
                engine.get_dependents(&owned_path),
            ))
        })
        .await
        .map_err(|e| {
            error!("Dependency analysis failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Dependency analysis failed".to_string(),
                    code: "DEPS_ERROR".to_string(),
                }),
            )
        })?;

    let imports = match dependencies {
        Ok(deps) => deps
            .into_iter()
            .map(|d| Dependency {
                path: d.target_path,
                symbols: d.import_name.map(|n| vec![n]),
                kind: d.kind,
            })
            .collect(),
        Err(e) => {
            debug!("Could not get imports: {}", e);
            vec![]
        }
    };

    let imported_by = match dependents {
        Ok(deps) => deps
            .into_iter()
            .map(|d| Dependency {
                path: d.target_path,
                symbols: None,
                kind: "import".to_string(),
            })
            .collect(),
        Err(e) => {
            debug!("Could not get dependents: {}", e);
            vec![]
        }
    };

    let search_time_ms = start.elapsed().as_millis() as u64;

    Ok(Json(DepsResponse {
        file_path: file_path.to_string(),
        imports,
        imported_by,
        search_time_ms,
    }))
}

// ============================================
// Explain
// ============================================

async fn explain(
    State(server): State<AppState>,
    Json(req): Json<ExplainRequest>,
) -> Result<Json<ExplainResponse>, (StatusCode, Json<ErrorResponse>)> {
    let start = Instant::now();

    // Validate symbol
    let symbol = req.symbol.trim();
    if symbol.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Symbol cannot be empty".to_string(),
                code: "INVALID_SYMBOL".to_string(),
            }),
        ));
    }
    if symbol.len() > 500 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Symbol exceeds maximum length".to_string(),
                code: "SYMBOL_TOO_LONG".to_string(),
            }),
        ));
    }

    debug!(symbol = %symbol, "HTTP explain request");

    let owned_symbol = symbol.to_string();
    let explanation = server
        .run_blocking(move |engine| engine.explain_symbol(&owned_symbol))
        .await;
    match explanation {
        Ok(explanation) => {
            let search_time_ms = start.elapsed().as_millis() as u64;

            if !explanation.found {
                return Ok(Json(ExplainResponse {
                    symbol: symbol.to_string(),
                    kind: "unknown".to_string(),
                    definitions: vec![],
                    related_symbols: vec![],
                    search_time_ms,
                }));
            }

            let definitions: Vec<SymbolDefinition> = explanation
                .definitions
                .into_iter()
                .map(|d| SymbolDefinition {
                    file_path: d.file_path,
                    line: d.start_line as u32,
                    signature: d.signature,
                    documentation: d.doc_comment,
                })
                .collect();

            let kind = if definitions.is_empty() {
                "unknown".to_string()
            } else {
                "symbol".to_string()
            };

            Ok(Json(ExplainResponse {
                symbol: explanation.name,
                kind,
                definitions,
                related_symbols: explanation.related_symbols,
                search_time_ms,
            }))
        }
        Err(e) => {
            error!("Explain failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Explain failed".to_string(),
                    code: "EXPLAIN_ERROR".to_string(),
                }),
            ))
        }
    }
}
