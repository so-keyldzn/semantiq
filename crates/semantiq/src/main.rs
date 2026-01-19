use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

mod commands;

#[derive(Parser)]
#[command(name = "semantiq")]
#[command(author, version, about = "Semantic code understanding for AI tools")]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

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

    /// Start the MCP server (stdio transport)
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

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Commands::Init { path } => commands::init(&path).await,
        Commands::InitCursor { path } => commands::init_cursor(&path).await,
        Commands::Serve {
            project,
            database,
            no_update_check,
        } => commands::serve(project, database, no_update_check).await,
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
    }
}
