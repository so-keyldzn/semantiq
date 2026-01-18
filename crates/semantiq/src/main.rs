use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ignore::WalkBuilder;
use rmcp::ServiceExt;
use semantiq_embeddings::create_embedding_model;
use semantiq_index::{IndexStore, should_exclude_entry, MAX_FILE_SIZE};
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
    /// Initialize Semantiq for a project (creates .claude/ config and indexes)
    Init {
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
        Commands::Init { path } => {
            init(&path).await
        }
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

async fn init(path: &Path) -> Result<()> {
    let project_root = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    println!("Initializing Semantiq for {:?}", project_root);

    // 1. Create .claude directory
    let claude_dir = project_root.join(".claude");
    if !claude_dir.exists() {
        fs::create_dir_all(&claude_dir)?;
        println!("Created .claude/");
    }

    // 2. Create .claude/settings.json with MCP config
    let settings_path = claude_dir.join("settings.json");
    let settings_content = r#"{
  "mcpServers": {
    "semantiq": {
      "command": "semantiq",
      "args": ["serve"]
    }
  }
}
"#;
    fs::write(&settings_path, settings_content)?;
    println!("Created .claude/settings.json");

    // 3. Create CLAUDE.md with instructions
    let claude_md_path = project_root.join("CLAUDE.md");
    let claude_md_content = r#"# Project Intelligence

This project uses Semantiq for semantic code understanding.

## Important: Use Semantiq Tools First

**Always use Semantiq MCP tools instead of grep/find/Glob for code search.**

| Instead of... | Use... |
|---------------|--------|
| `Grep`, `grep`, `rg` | `semantiq_search` |
| `Glob`, `find`, `ls` | `semantiq_search` |
| Manual symbol tracing | `semantiq_find_refs` |
| Reading imports manually | `semantiq_deps` |

Semantiq provides faster, more accurate results with semantic understanding.

## Available MCP Tools

When working with this codebase, you have access to these powerful tools:

### `semantiq_search`
Search for code patterns, symbols, or text semantically.
```
Example: "authentication handler", "database connection", "error handling"
```

### `semantiq_find_refs`
Find all references to a symbol (definitions and usages).
```
Example: Find where a function is called, or where a class is used.
```

### `semantiq_deps`
Analyze the dependency graph for a file.
```
Example: What does this file import? What imports this file?
```

### `semantiq_explain`
Get detailed explanation of a symbol including definition, docs, and usage patterns.
```
Example: Understand what a function does, its signature, and how it's used.
```

## Best Practices

1. **Use `semantiq_search` first** to find relevant code before making changes
2. **Use `semantiq_find_refs`** to understand impact before refactoring
3. **Use `semantiq_deps`** to understand module relationships
4. **Use `semantiq_explain`** for unfamiliar symbols

## Auto-Indexing

The index updates automatically when files change. No manual reindexing needed.
"#;

    if !claude_md_path.exists() {
        fs::write(&claude_md_path, claude_md_content)?;
        println!("Created CLAUDE.md");
    } else {
        println!("CLAUDE.md already exists, skipping");
    }

    // 4. Update .gitignore
    let gitignore_path = project_root.join(".gitignore");
    let gitignore_entry = ".semantiq.db";

    if gitignore_path.exists() {
        let content = fs::read_to_string(&gitignore_path)?;
        if !content.contains(gitignore_entry) {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(&gitignore_path)?;
            use std::io::Write;
            writeln!(file, "\n# Semantiq\n{}", gitignore_entry)?;
            println!("Added .semantiq.db to .gitignore");
        }
    } else {
        fs::write(&gitignore_path, format!("# Semantiq\n{}\n", gitignore_entry))?;
        println!("Created .gitignore");
    }

    // 5. Index the project
    println!("\nIndexing project...");
    index(path, None, false).await?;

    println!("\n✓ Semantiq initialized successfully!");
    println!("\nNext steps:");
    println!("  1. Restart Claude Code to load the MCP server");
    println!("  2. The semantiq tools will be available automatically");

    Ok(())
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

    // Initialize embedding model
    let embedding_model = match create_embedding_model(None) {
        Ok(model) => {
            info!("Embedding model loaded (dim={})", model.dimension());
            Some(model)
        }
        Err(e) => {
            warn!("Could not load embedding model: {}. Embeddings will not be generated.", e);
            None
        }
    };

    let mut file_count = 0;
    let mut symbol_count = 0;
    let mut chunk_count = 0;
    let mut dep_count = 0;

    // Walk the directory, excluding hidden dirs and dependency folders
    let walker = WalkBuilder::new(&project_root)
        .hidden(true) // Exclude hidden directories (.git, .claude, etc.)
        .git_ignore(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !should_exclude_entry(&name)
        })
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

        // Skip large files
        if size > MAX_FILE_SIZE as i64 {
            debug!("Skipping {} (too large: {} bytes)", rel_path, size);
            continue;
        }

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

                // Generate embeddings for chunks
                if let Some(ref model) = embedding_model {
                    let stored_chunks = store.get_chunks_by_file(file_id)?;
                    for chunk in stored_chunks {
                        if let Ok(embedding) = model.embed(&chunk.content) {
                            let _ = store.update_chunk_embedding(chunk.id, &embedding);
                        }
                    }
                }

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

    let store = std::sync::Arc::new(IndexStore::open(&db_path)?);
    let cwd_str = cwd.to_str().context("Current directory path contains invalid UTF-8")?;
    let engine = semantiq_retrieval::RetrievalEngine::new(store, cwd_str);

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
