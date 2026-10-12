//! HTTP API server for Semantiq
//!
//! Exposes the MCP tools two ways: REST endpoints (used by the interactive
//! demo) and the MCP Streamable HTTP transport at `/mcp`, so remote MCP
//! clients can connect without spawning a stdio process.

mod cors;
mod host;
mod limits;
mod routes;
#[cfg(test)]
mod tests;
mod types;

pub(crate) use cors::CorsOrigins;
pub(crate) use limits::RequestLimits;
pub(crate) use routes::create_router;

use host::{AllowedHosts, require_allowed_host};

use anyhow::Result;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use semantiq_mcp::SemantiqServer;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

/// Maximum request body size (1 MB). Prevents OOM from oversized payloads.
const MAX_BODY_SIZE: usize = 1024 * 1024;

/// Start the HTTP API server. `cors` is the validated `--cors-origin`
/// (`CorsOrigins::parse`); the query endpoints run under `RequestLimits`.
pub(crate) async fn serve_http(
    server: SemantiqServer,
    host: IpAddr,
    port: u16,
    cors: Option<CorsOrigins>,
) -> Result<()> {
    // The MCP transport validates the `Host` header against loopback names to
    // block DNS rebinding; `require_allowed_host` below does the same for the
    // REST endpoints, both with the same list (loopback names plus the bound
    // address). When the user explicitly binds beyond loopback, the Host will
    // be this machine's address, so both checks have to go.
    let allowed_hosts = host
        .is_loopback()
        .then(|| AllowedHosts::for_bind_address(host));
    let mut mcp_config = StreamableHttpServerConfig::default();
    mcp_config = match &allowed_hosts {
        Some(allowed) => mcp_config.with_allowed_hosts(allowed.names().iter().cloned()),
        None => mcp_config.disable_allowed_hosts(),
    };
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
    // tool. Pass --cors-origin <ORIGIN> to explicitly opt into known origins;
    // `*` is refused by `CorsOrigins::parse`.
    let cors = match cors {
        Some(origins) => {
            info!(
                "CORS: allowing origins {}",
                origins.origins().collect::<Vec<_>>().join(", ")
            );
            origins.layer()
        }
        None => {
            warn!(
                "No CORS origin specified; cross-origin requests are blocked. \
                 Pass --cors-origin <ORIGIN> to allow a specific origin."
            );
            CorsLayer::new()
        }
    };

    // The request limits (timeout, concurrency, load shedding) are applied by
    // `create_router` to the query endpoints only: `/health` must answer while
    // the server is busy and `/mcp` streams must not be cut by the timeout.
    let mut app: Router = create_router(server, RequestLimits::default())
        .nest_service("/mcp", mcp_service)
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .layer(TraceLayer::new_for_http())
        .layer(cors);
    if let Some(allowed) = allowed_hosts {
        app = app.layer(middleware::from_fn_with_state(
            allowed,
            require_allowed_host,
        ));
    }

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
