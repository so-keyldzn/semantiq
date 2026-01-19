//! Start the MCP server (stdio transport)

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

    info!("Starting Semantiq MCP server");
    info!("Project root: {:?}", project_root);
    info!("Database: {:?}", db_path);

    let project_root_str = project_root
        .to_str()
        .context("Project root path contains invalid UTF-8")?;
    let server = SemantiqServer::new(&db_path, project_root_str)?;

    // Start auto-indexer in background
    server.start_auto_indexer();

    // Run MCP server on stdio
    let service = server.serve(rmcp::transport::stdio()).await?;

    // Wait for the service to complete
    service.waiting().await?;

    Ok(())
}
