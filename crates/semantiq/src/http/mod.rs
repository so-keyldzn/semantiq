//! HTTP API server for Semantiq demo
//!
//! Exposes the MCP tools via HTTP REST endpoints for the interactive demo.

mod routes;
mod types;

pub use routes::create_router;

use anyhow::Result;
use axum::extract::DefaultBodyLimit;
use axum::Router;
use semantiq_mcp::SemantiqServer;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

/// Maximum request body size (1 MB). Prevents OOM from oversized payloads.
const MAX_BODY_SIZE: usize = 1024 * 1024;

/// Start the HTTP API server
pub async fn serve_http(server: SemantiqServer, port: u16, cors_origin: Option<String>) -> Result<()> {
    let server = Arc::new(server);

    // Build CORS layer
    let cors = if let Some(origin) = cors_origin {
        CorsLayer::new()
            .allow_origin(origin.parse::<axum::http::HeaderValue>()?)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        warn!("No CORS origin specified, allowing all origins. Set --cors-origin in production.");
        CorsLayer::very_permissive()
    };

    let app: Router = create_router(server)
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("Starting HTTP API server on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
