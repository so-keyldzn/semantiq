//! Start the MCP server (stdio transport) or HTTP API server

use anyhow::{Context, Result};
use rmcp::ServiceExt;
use semantiq_mcp::{SemantiqServer, disable_update_check};
use std::path::PathBuf;
use tracing::info;

use super::common::resolve_db_path;

pub async fn serve(
    project: Option<PathBuf>,
    database: Option<PathBuf>,
    no_update_check: bool,
    http_port: Option<u16>,
    cors_origin: Option<String>,
) -> Result<()> {
    // Disable update check if flag is set (thread-safe, no unsafe needed)
    if no_update_check {
        disable_update_check();
    }

    let project_root = match project {
        Some(p) => p,
        None => std::env::current_dir().context("Failed to get current directory")?,
    };

    let db_path = resolve_db_path(database, &project_root);

    let project_root_str = project_root
        .to_str()
        .context("Project root path contains invalid UTF-8")?;
    let server = SemantiqServer::new(&db_path, project_root_str)?;

    // Start auto-indexer in background
    server.start_auto_indexer();

    if let Some(port) = http_port {
        // HTTP API mode
        info!("Starting Semantiq HTTP API server");
        info!("Project root: {:?}", project_root);
        info!("Database: {:?}", db_path);

        crate::http::serve_http(server, port, cors_origin).await
    } else {
        // MCP stdio mode
        info!("Starting Semantiq MCP server");
        info!("Project root: {:?}", project_root);
        info!("Database: {:?}", db_path);

        let service = server.serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;

        Ok(())
    }
}
