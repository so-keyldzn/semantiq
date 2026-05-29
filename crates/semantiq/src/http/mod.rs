//! HTTP API server for Semantiq demo
//!
//! Exposes the MCP tools via HTTP REST endpoints for the interactive demo.

mod routes;
#[cfg(test)]
mod tests;
mod types;

pub(crate) use routes::create_router;

use anyhow::Result;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use semantiq_mcp::SemantiqServer;
use std::net::SocketAddr;
use std::sync::Arc;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

/// Maximum request body size (1 MB). Prevents OOM from oversized payloads.
const MAX_BODY_SIZE: usize = 1024 * 1024;

/// Maximum concurrent requests. Prevents resource exhaustion from abuse.
const MAX_CONCURRENT_REQUESTS: usize = 50;

/// Start the HTTP API server
pub(crate) async fn serve_http(
    server: SemantiqServer,
    port: u16,
    cors_origin: Option<String>,
) -> Result<()> {
    let server = Arc::new(server);

    // Build CORS layer.
    //
    // Default (no --cors-origin): restrictive. We do NOT allow any cross-origin
    // requests — `CorsLayer::new()` with no `allow_origin` configured emits no
    // `Access-Control-Allow-Origin` header, so browsers block cross-site reads.
    // Same-origin requests are unaffected. This is the safe default for a local
    // tool. Pass --cors-origin <ORIGIN> to explicitly opt into a known origin.
    let cors = if let Some(origin) = cors_origin {
        CorsLayer::new()
            .allow_origin(origin.parse::<axum::http::HeaderValue>()?)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        warn!(
            "No CORS origin specified; cross-origin requests are blocked. \
             Pass --cors-origin <ORIGIN> to allow a specific origin."
        );
        CorsLayer::new()
    };

    let app: Router = create_router(server)
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .layer(ConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS))
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("Starting HTTP API server on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
            info!("Shutdown signal received, stopping HTTP API server");
        })
        .await?;

    Ok(())
}
