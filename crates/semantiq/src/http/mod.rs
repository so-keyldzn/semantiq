//! HTTP API server for Semantiq
//!
//! Exposes the MCP tools two ways: REST endpoints (used by the interactive
//! demo) and the MCP Streamable HTTP transport at `/mcp`, so remote MCP
//! clients can connect without spawning a stdio process.

mod routes;
#[cfg(test)]
mod tests;
mod types;

pub(crate) use routes::create_router;

use anyhow::Result;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use semantiq_mcp::SemantiqServer;
use std::net::{IpAddr, SocketAddr};
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
    host: IpAddr,
    port: u16,
    cors_origin: Option<String>,
) -> Result<()> {
    // The MCP transport validates the `Host` header against loopback names to
    // block DNS rebinding. When the user explicitly binds beyond loopback, the
    // Host will be this machine's address, so the check has to go.
    let mut mcp_config = StreamableHttpServerConfig::default();
    if !host.is_loopback() {
        mcp_config = mcp_config.disable_allowed_hosts();
    }
    // Open SSE streams would otherwise keep graceful shutdown waiting forever.
    let mcp_shutdown = mcp_config.cancellation_token.clone();
    let mcp_server = server.clone();
    let mcp_service: StreamableHttpService<SemantiqServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(mcp_server.clone()),
            Default::default(),
            mcp_config,
        );

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
        .nest_service("/mcp", mcp_service)
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .layer(ConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS))
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    // The API has no authentication, so binding beyond loopback exposes the
    // indexed source code to anyone who can reach this host.
    let addr = SocketAddr::new(host, port);
    if !host.is_loopback() {
        warn!(
            "HTTP API bound to non-loopback address {}; it is unauthenticated and \
             reachable from the network",
            addr
        );
    }
    info!("Starting HTTP API server on http://{}", addr);
    info!("MCP Streamable HTTP endpoint: http://{}/mcp", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.ok();
            info!("Shutdown signal received, stopping HTTP API server");
            mcp_shutdown.cancel();
        })
        .await?;

    Ok(())
}
