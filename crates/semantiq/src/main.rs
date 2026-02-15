use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

mod commands;
mod http;

#[derive(Parser)]
#[command(name = "semantiq")]
#[command(author, version, about = "Semantic code understanding for AI tools")]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Output logs in JSON format (default for 'serve' command)
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize Semantiq for a project (creates .claude/ config and indexes)
    Init {
        /// Path to the project (default: current directory)
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Initialize Cursor/VS Code configuration for a project
    InitCursor {
        /// Path to the project (default: current directory)
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Start the MCP server (stdio transport) or HTTP API server
    Serve {
        /// Path to the project root (default: current directory)
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Path to the database file (default: .semantiq.db in project root)
        #[arg(short, long)]
        database: Option<PathBuf>,

        /// Disable automatic update check
        #[arg(long)]
        no_update_check: bool,

        /// Start HTTP API server on this port (instead of MCP stdio)
        #[arg(long)]
        http_port: Option<u16>,

        /// CORS allowed origin for HTTP API (e.g., "https://example.com")
        #[arg(long)]
        cors_origin: Option<String>,
    },

    /// Index a project directory
    Index {
        /// Path to the project to index
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Path to the database file
        #[arg(short, long)]
        database: Option<PathBuf>,

        /// Force full reindex (ignore cache)
        #[arg(short, long)]
        force: bool,
    },

    /// Show index statistics
    Stats {
        /// Path to the database file
        #[arg(short, long)]
        database: Option<PathBuf>,
    },

    /// Search the index (for testing)
    Search {
        /// Search query
        query: String,

        /// Path to the database file
        #[arg(short, long)]
        database: Option<PathBuf>,

        /// Maximum results
        #[arg(short, long, default_value = "10")]
        limit: usize,

        /// Minimum score (0.0-1.0, default: 0.35)
        #[arg(long)]
        min_score: Option<f32>,

        /// File extensions to include (comma-separated, e.g., "rs,ts,py")
        #[arg(long)]
        file_type: Option<String>,

        /// Symbol kinds to include (comma-separated, e.g., "function,class")
        #[arg(long)]
        symbol_kind: Option<String>,
    },

    /// Calibrate semantic search thresholds using ML
    Calibrate {
        /// Path to the database file
        #[arg(short, long)]
        database: Option<PathBuf>,

        /// Calibrate only this language (e.g., "rust", "python")
        #[arg(short, long)]
        language: Option<String>,

        /// Show what would be done without saving
        #[arg(long)]
        dry_run: bool,

        /// Minimum samples required for calibration
        #[arg(long, default_value = "100")]
        min_samples: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Setup logging - filter out verbose ONNX Runtime logs
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info,ort=warn")
    };

    // Use JSON logging by default for serve command (MCP server)
    let use_json = cli.json || matches!(cli.command, Commands::Serve { .. });

    if use_json {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    }

    match cli.command {
        Commands::Init { path } => commands::init(&path).await,
        Commands::InitCursor { path } => commands::init_cursor(&path).await,
        Commands::Serve {
            project,
            database,
            no_update_check,
            http_port,
            cors_origin,
        } => commands::serve(project, database, no_update_check, http_port, cors_origin).await,
        Commands::Index {
            path,
            database,
            force,
        } => commands::index(&path, database, force).await,
        Commands::Stats { database } => commands::stats(database).await,
        Commands::Search {
            query,
            database,
            limit,
            min_score,
            file_type,
            symbol_kind,
        } => commands::search(&query, database, limit, min_score, file_type, symbol_kind).await,
        Commands::Calibrate {
            database,
            language,
            dry_run,
            min_samples,
        } => commands::calibrate(database, language, dry_run, min_samples).await,
    }
}
