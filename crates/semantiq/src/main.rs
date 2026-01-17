use anyhow::Result;
use clap::{Parser, Subcommand};
use ignore::WalkBuilder;
use rmcp::ServiceExt;
use semantiq_index::IndexStore;
use semantiq_mcp::SemantiqServer;
use semantiq_parser::{ChunkExtractor, ImportExtractor, Language, LanguageSupport, SymbolExtractor};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_DB_NAME: &str = ".semantiq.db";

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
    /// Start the MCP server (stdio transport)
    Serve {
        /// Path to the project root (default: current directory)
        #[arg(short, long)]
        project: Option<PathBuf>,

        /// Path to the database file (default: .semantiq.db in project root)
        #[arg(short, long)]
        database: Option<PathBuf>,
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
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Setup logging
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Commands::Serve { project, database } => {
            serve(project, database).await
        }
        Commands::Index { path, database, force } => {
            index(&path, database, force).await
        }
        Commands::Stats { database } => {
            stats(database).await
        }
        Commands::Search { query, database, limit } => {
            search(&query, database, limit).await
        }
    }
}

async fn serve(project: Option<PathBuf>, database: Option<PathBuf>) -> Result<()> {
    let project_root = project
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    let db_path = database.unwrap_or_else(|| project_root.join(DEFAULT_DB_NAME));

    info!("Starting Semantiq MCP server");
    info!("Project root: {:?}", project_root);
    info!("Database: {:?}", db_path);

    let server = SemantiqServer::new(&db_path, project_root.to_str().unwrap())?;

    // Start auto-indexer in background
    server.start_auto_indexer();

    // Run MCP server on stdio
    let service = server.serve(rmcp::transport::stdio()).await?;

    // Wait for the service to complete
    service.waiting().await?;

    Ok(())
}

async fn index(path: &Path, database: Option<PathBuf>, force: bool) -> Result<()> {
    let project_root = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    let db_path = database.unwrap_or_else(|| project_root.join(DEFAULT_DB_NAME));

    info!("Indexing project: {:?}", project_root);
    info!("Database: {:?}", db_path);

    let start = Instant::now();
    let store = IndexStore::open(&db_path)?;
    let mut language_support = LanguageSupport::new()?;
    let chunk_extractor = ChunkExtractor::new();

    let mut file_count = 0;
    let mut symbol_count = 0;
    let mut chunk_count = 0;
    let mut dep_count = 0;

    // Walk the directory
    let walker = WalkBuilder::new(&project_root)
        .hidden(false)
        .git_ignore(true)
        .build();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        // Check if this is a supported language
        let language = match Language::from_path(path) {
            Some(lang) => lang,
            None => continue,
        };

        // Get relative path
        let rel_path = path
            .strip_prefix(&project_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Read file content
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                debug!("Skipping {}: {}", rel_path, e);
                continue;
            }
        };

        // Check if we need to reindex
        if !force && !store.needs_reindex(&rel_path, &content)? {
            debug!("Skipping {} (unchanged)", rel_path);
            continue;
        }

        // Get file metadata
        let metadata = fs::metadata(path)?;
        let size = metadata.len() as i64;
        let last_modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Insert file record
        let file_id = store.insert_file(
            &rel_path,
            Some(language.name()),
            &content,
            size,
            last_modified,
        )?;

        // Parse and extract symbols
        match language_support.parse(language, &content) {
            Ok(tree) => {
                // Extract symbols
                let symbols = SymbolExtractor::extract(&tree, &content, language)?;
                store.insert_symbols(file_id, &symbols)?;
                symbol_count += symbols.len();

                // Extract chunks
                let chunks = chunk_extractor.extract(&tree, &content, language)?;
                store.insert_chunks(file_id, &chunks)?;
                chunk_count += chunks.len();

                // Extract imports and store as dependencies
                let imports = ImportExtractor::extract(&tree, &content, language)?;
                store.delete_dependencies(file_id)?;
                for import in &imports {
                    store.insert_dependency(
                        file_id,
                        &import.path,
                        import.name.as_deref(),
                        import.kind.as_str(),
                    )?;
                }
                dep_count += imports.len();

                debug!(
                    "Indexed {}: {} symbols, {} chunks, {} deps",
                    rel_path,
                    symbols.len(),
                    chunks.len(),
                    imports.len()
                );
            }
            Err(e) => {
                warn!("Failed to parse {}: {}", rel_path, e);
            }
        }

        file_count += 1;

        // Progress update every 100 files
        if file_count % 100 == 0 {
            info!("Indexed {} files...", file_count);
        }
    }

    let elapsed = start.elapsed();

    info!("Indexing complete!");
    info!("  Files: {}", file_count);
    info!("  Symbols: {}", symbol_count);
    info!("  Chunks: {}", chunk_count);
    info!("  Dependencies: {}", dep_count);
    info!("  Time: {:.2}s", elapsed.as_secs_f64());

    Ok(())
}

async fn stats(database: Option<PathBuf>) -> Result<()> {
    let db_path = database.unwrap_or_else(|| {
        std::env::current_dir()
            .expect("Failed to get current directory")
            .join(DEFAULT_DB_NAME)
    });

    if !db_path.exists() {
        anyhow::bail!("Database not found: {:?}. Run 'semantiq index' first.", db_path);
    }

    let store = IndexStore::open(&db_path)?;
    let stats = store.get_stats()?;

    println!("Semantiq Index Statistics");
    println!("========================");
    println!("Database: {:?}", db_path);
    println!("Files indexed: {}", stats.file_count);
    println!("Symbols: {}", stats.symbol_count);
    println!("Chunks: {}", stats.chunk_count);
    println!("Dependencies: {}", stats.dependency_count);

    Ok(())
}

async fn search(query: &str, database: Option<PathBuf>, limit: usize) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let db_path = database.unwrap_or_else(|| cwd.join(DEFAULT_DB_NAME));

    if !db_path.exists() {
        anyhow::bail!("Database not found: {:?}. Run 'semantiq index' first.", db_path);
    }

    let store = IndexStore::open(&db_path)?;
    let engine = semantiq_retrieval::RetrievalEngine::new(store, cwd.to_str().unwrap());

    let results = engine.search(query, limit)?;

    println!("Search results for '{}' ({} ms)", query, results.search_time_ms);
    println!("Found {} results\n", results.total_count);

    for result in &results.results {
        println!(
            "📄 {}:{}-{} (score: {:.2})",
            result.file_path, result.start_line, result.end_line, result.score
        );

        if let Some(ref name) = result.metadata.symbol_name {
            println!("   Symbol: {} ({})", name, result.metadata.symbol_kind.as_deref().unwrap_or(""));
        }

        let snippet: String = result.content.chars().take(100).collect();
        println!("   {}", snippet.trim());
        println!();
    }

    Ok(())
}
