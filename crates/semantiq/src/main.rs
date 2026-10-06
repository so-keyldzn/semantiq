use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

mod commands;
mod http;

use commands::InitOptions;
use commands::query::{self, ImpactArgs, IndexArgs, SearchArgs};

#[derive(Parser)]
#[command(name = "semantiq")]
#[command(author, version, about = "Semantic code understanding for AI tools")]
struct Cli {
    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// JSON output: structured results on stdout for query commands (search,
    /// refs, deps, explain, impact); JSON logs for the others (default for 'serve')
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize Semantiq for a project: installs the Claude Code skill,
    /// registers the MCP server, updates CLAUDE.md, and indexes
    Init {
        /// Path to the project (default: current directory)
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Do not register the MCP server in .mcp.json (skill + CLI only)
        #[arg(long)]
        no_mcp: bool,

        /// Do not install the Claude Code skill (MCP only)
        #[arg(long)]
        no_skill: bool,

        /// Overwrite skill files that differ from the bundled version
        #[arg(short, long)]
        force: bool,

        /// Only install the skill for all projects (~/.claude/skills/semantiq/)
        #[arg(long, conflicts_with_all = ["no_mcp", "no_skill", "no_index"])]
        global: bool,

        /// Skip the initial indexing
        #[arg(long)]
        no_index: bool,
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

        /// Address the HTTP API binds to. Defaults to loopback so the indexed
        /// code is not exposed to the network; use "0.0.0.0" to opt in.
        #[arg(long, default_value = "127.0.0.1")]
        http_host: std::net::IpAddr,
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

    /// Search code by meaning, symbol name or text
    Search {
        /// Search query: natural language, symbol name, or text pattern
        query: String,

        #[command(flatten)]
        index: IndexArgs,

        /// Maximum results
        #[arg(short, long, default_value = "10")]
        limit: usize,

        /// Minimum score (0.0-1.0, default: 0.3)
        #[arg(long)]
        min_score: Option<f32>,

        /// File extensions to include (comma-separated, e.g., "rs,ts,py")
        #[arg(long)]
        file_type: Option<String>,

        /// Symbol kinds to include (comma-separated, e.g., "function,class")
        #[arg(long)]
        symbol_kind: Option<String>,
    },

    /// Find the definitions and usages of a symbol (from the syntax tree)
    Refs {
        /// Symbol name
        symbol: String,

        #[command(flatten)]
        index: IndexArgs,

        /// Maximum references
        #[arg(short, long, default_value = "50")]
        limit: usize,
    },

    /// Show what a file imports and which files import it
    Deps {
        /// File path (relative to the project root)
        file: String,

        #[command(flatten)]
        index: IndexArgs,
    },

    /// Explain a symbol: definitions, signature, docs, usage count
    Explain {
        /// Symbol name
        symbol: String,

        #[command(flatten)]
        index: IndexArgs,
    },

    /// List what may break if a symbol changes, with the tests to run
    Impact {
        /// Symbol about to change
        symbol: String,

        #[command(flatten)]
        index: IndexArgs,

        /// Restrict to the definition in this file, when the name is defined in several places
        #[arg(long)]
        file: Option<String>,

        /// Levels of callers to follow (default 2, max 4)
        #[arg(long)]
        max_depth: Option<usize>,

        /// Maximum impact sites (default 200, max 1000)
        #[arg(short, long)]
        limit: Option<usize>,
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

    /// Update the semantiq binary to the latest release
    Update {
        /// Only check whether an update is available; don't install it
        #[arg(long)]
        check: bool,

        /// Reinstall the latest release even if already up to date
        #[arg(short, long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Query commands print results on stdout, often for an agent to parse:
    // keep stderr to warnings and errors, and `--json` applies to the results.
    let is_query = matches!(
        cli.command,
        Commands::Search { .. }
            | Commands::Refs { .. }
            | Commands::Deps { .. }
            | Commands::Explain { .. }
            | Commands::Impact { .. }
    );

    // Setup logging - filter out verbose ONNX Runtime logs
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else if is_query {
        EnvFilter::new("warn,ort=error")
    } else {
        EnvFilter::new("info,ort=warn")
    };

    // Use JSON logging by default for serve command (MCP server)
    let use_json = !is_query && (cli.json || matches!(cli.command, Commands::Serve { .. }));

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
        Commands::Init {
            path,
            no_mcp,
            no_skill,
            force,
            global,
            no_index,
        } => {
            commands::init(
                &path,
                InitOptions {
                    no_mcp,
                    no_skill,
                    force,
                    global,
                    no_index,
                },
            )
            .await
        }
        Commands::InitCursor { path } => commands::init_cursor(&path).await,
        Commands::Serve {
            project,
            database,
            no_update_check,
            http_port,
            cors_origin,
            http_host,
        } => {
            commands::serve(
                project,
                database,
                no_update_check,
                http_port,
                http_host,
                cors_origin,
            )
            .await
        }
        Commands::Index {
            path,
            database,
            force,
        } => commands::index(&path, database, force).await,
        Commands::Stats { database } => commands::stats(database).await,
        Commands::Search {
            query,
            index,
            limit,
            min_score,
            file_type,
            symbol_kind,
        } => query::search(
            &index,
            SearchArgs {
                query,
                limit,
                min_score,
                file_type,
                symbol_kind,
            },
            cli.json,
        ),
        Commands::Refs {
            symbol,
            index,
            limit,
        } => query::refs(&index, symbol, limit, cli.json),
        Commands::Deps { file, index } => query::deps(&index, &file, cli.json),
        Commands::Explain { symbol, index } => query::explain(&index, symbol, cli.json),
        Commands::Impact {
            symbol,
            index,
            file,
            max_depth,
            limit,
        } => query::impact(
            &index,
            ImpactArgs {
                symbol,
                file,
                max_depth,
                limit,
            },
            cli.json,
        ),
        Commands::Calibrate {
            database,
            language,
            dry_run,
            min_samples,
        } => commands::calibrate(database, language, dry_run, min_samples).await,
        Commands::Update { check, force } => commands::update(check, force).await,
    }
}
