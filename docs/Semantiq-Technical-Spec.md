# TECHNICAL SPECIFICATION

# Semantiq
**Deep Dive: Architecture & Implementation**

| | |
|---------|-------------|
| Version | 1.0 |
| Date | January 2026 |
| Author | Nicolas |

---

## Table of Contents
1. [Retrieval Engine](#1-retrieval-engine)
2. [MCP Server Implementation](#2-mcp-server-implementation)
3. [Indexation & Storage](#3-indexation--storage)
4. [Embeddings Pipeline](#4-embeddings-pipeline)
5. [Tree-sitter Integration](#5-tree-sitter-integration)
6. [Daemon Architecture](#6-daemon-architecture)

---

## 1. Retrieval Engine

Le Retrieval Engine est le cœur de Semantiq. Il combine 4 stratégies de recherche et les fusionne intelligemment pour produire les résultats les plus pertinents.

### 1.1 Architecture Multi-Retrieval

```
┌─────────────────────────────────────────────────────────────────┐
│                         USER QUERY                              │
│                "find rate limiting logic"                       │
└─────────────────────────┬───────────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────────┐
│                    QUERY ANALYZER                               │
│  • Extract keywords: ["rate", "limiting", "logic"]              │
│  • Generate embedding: [0.023, -0.156, 0.892, ...]             │
│  • Detect intent: CODE_SEARCH                                   │
│  • Identify scope hints: (none detected)                        │
└─────────────────────────┬───────────────────────────────────────┘
                          │
        ┌─────────────────┼─────────────────┬─────────────────┐
        ▼                 ▼                 ▼                 ▼
┌───────────────┐ ┌───────────────┐ ┌───────────────┐ ┌───────────────┐
│   LEXICAL     │ │   SEMANTIC    │ │     AST       │ │    GRAPH      │
│   (ripgrep)   │ │  (embeddings) │ │ (tree-sitter) │ │ (dependencies)│
│               │ │               │ │               │ │               │
│ "rate_limit"  │ │ cosine_sim()  │ │ fn contains   │ │ imports of    │
│ "rateLimit"   │ │ top_k=50      │ │ "limit" in    │ │ rate_limiter  │
│ "throttle"    │ │               │ │ fn name       │ │ module        │
└───────┬───────┘ └───────┬───────┘ └───────┬───────┘ └───────┬───────┘
        │                 │                 │                 │
        │ score: 0-1      │ score: 0-1      │ score: 0-1      │ score: 0-1
        │                 │                 │                 │
        └─────────────────┴────────┬────────┴─────────────────┘
                                   │
                                   ▼
┌─────────────────────────────────────────────────────────────────┐
│                      FUSION & RANKING                           │
│                                                                 │
│  final_score = w1*lexical + w2*semantic + w3*ast + w4*graph    │
│                + recency_boost + proximity_boost                │
│                                                                 │
│  Weights (learned/tuned):                                       │
│  • w1 (lexical)  = 0.25                                        │
│  • w2 (semantic) = 0.40                                        │
│  • w3 (ast)      = 0.20                                        │
│  • w4 (graph)    = 0.15                                        │
└─────────────────────────┬───────────────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────────────┐
│                     RANKED RESULTS                              │
│  1. src/middleware/rate_limiter.rs     (score: 0.94)           │
│  2. src/api/throttle.rs                (score: 0.87)           │
│  3. src/config/limits.rs               (score: 0.72)           │
│  4. tests/rate_limit_test.rs           (score: 0.68)           │
└─────────────────────────────────────────────────────────────────┘
```

### 1.2 Stratégies de Retrieval

#### 1.2.1 Lexical Search (ripgrep)

Recherche textuelle rapide avec expansion de query. Gère les variations de naming (camelCase, snake_case, kebab-case).

```rust
// Query expansion
fn expand_query(query: &str) -> Vec<String> {
    let mut variants = vec![query.to_string()];

    // camelCase -> snake_case
    variants.push(to_snake_case(query));
    // snake_case -> camelCase
    variants.push(to_camel_case(query));
    // Add common synonyms
    if query.contains("rate") && query.contains("limit") {
        variants.push("throttle".to_string());
        variants.push("quota".to_string());
    }
    variants
}

// Scoring based on match quality
fn lexical_score(match: &Match) -> f32 {
    let mut score = 0.0;

    // Exact match in function/class name
    if match.in_symbol_name { score += 0.5; }
    // Match in code vs comment
    if match.in_code { score += 0.3; } else { score += 0.1; }
    // File path relevance
    if match.path.contains("test") { score *= 0.7; }

    score.min(1.0)
}
```

#### 1.2.2 Semantic Search (Embeddings)

Recherche par similarité vectorielle. Permet de trouver du code conceptuellement similaire même sans correspondance de mots-clés.

```rust
// Semantic search flow
async fn semantic_search(query: &str, index: &VectorIndex) -> Vec<ScoredResult> {
    // 1. Embed the query
    let query_embedding = embed_text(query).await?;

    // 2. Search vector index (sqlite-vss)
    let candidates = index.search_similar(
        &query_embedding,
        top_k: 50,           // Get more candidates for re-ranking
        threshold: 0.3       // Minimum similarity
    ).await?;

    // 3. Score = cosine similarity (already normalized 0-1)
    candidates.into_iter()
        .map(|c| ScoredResult {
            path: c.path,
            score: c.similarity,
            snippet: c.content
        })
        .collect()
}
```

#### 1.2.3 AST Search (Tree-sitter)

Recherche structurelle basée sur l'AST. Permet des queries du type "fonctions async sans error handling".

```rust
// AST-based scoring
fn ast_score(query: &str, symbols: &[Symbol]) -> Vec<ScoredResult> {
    let query_tokens: HashSet<_> = tokenize(query).collect();

    symbols.iter()
        .filter_map(|sym| {
            let name_tokens: HashSet<_> = tokenize(&sym.name).collect();
            let overlap = query_tokens.intersection(&name_tokens).count();

            if overlap == 0 { return None; }

            let score = match sym.kind {
                SymbolKind::Function => 0.4,
                SymbolKind::Class => 0.35,
                SymbolKind::Method => 0.3,
                SymbolKind::Variable => 0.15,
                _ => 0.1,
            } * (overlap as f32 / query_tokens.len() as f32);

            Some(ScoredResult { path: sym.file.clone(), score, .. })
        })
        .collect()
}
```

#### 1.2.4 Graph Search (Dependencies)

Exploite le graphe de dépendances pour trouver des fichiers reliés. Si un fichier est très pertinent, ses importeurs/exporteurs le sont probablement aussi.

```rust
// Graph-based expansion
fn graph_boost(results: &mut [ScoredResult], graph: &DepGraph) {
    let top_files: Vec<_> = results.iter()
        .take(5)
        .map(|r| &r.path)
        .collect();

    for file in &top_files {
        // Boost files that import top results
        for importer in graph.get_importers(file) {
            if let Some(r) = results.iter_mut().find(|r| r.path == importer) {
                r.score += 0.1;  // Boost related files
            }
        }

        // Boost files imported by top results
        for imported in graph.get_imports(file) {
            if let Some(r) = results.iter_mut().find(|r| r.path == imported) {
                r.score += 0.15;  // Slightly higher boost
            }
        }
    }
}
```

### 1.3 Fusion Algorithm (RRF)

On utilise Reciprocal Rank Fusion (RRF) plutôt qu'une simple moyenne pondérée. RRF est plus robuste aux différences d'échelle entre les scores.

```rust
// Reciprocal Rank Fusion
fn fuse_results(
    lexical: Vec<ScoredResult>,
    semantic: Vec<ScoredResult>,
    ast: Vec<ScoredResult>,
    graph: Vec<ScoredResult>,
) -> Vec<ScoredResult> {
    const K: f32 = 60.0;  // RRF constant

    let mut scores: HashMap<PathBuf, f32> = HashMap::new();

    // RRF formula: score = Σ 1/(k + rank)
    for (weight, results) in [
        (0.25, &lexical),
        (0.40, &semantic),
        (0.20, &ast),
        (0.15, &graph),
    ] {
        for (rank, result) in results.iter().enumerate() {
            let rrf_score = weight / (K + rank as f32 + 1.0);
            *scores.entry(result.path.clone()).or_default() += rrf_score;
        }
    }

    // Sort by fused score
    let mut fused: Vec<_> = scores.into_iter()
        .map(|(path, score)| ScoredResult { path, score, .. })
        .collect();
    fused.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

    fused
}
```

### 1.4 Context-Aware Boosting

Des boosters additionnels ajustent le score final selon le contexte:

- **Recency boost**: Fichiers modifiés récemment +10-20%
- **Proximity boost**: Fichiers proches du fichier actif +15%
- **Frequency boost**: Fichiers souvent accédés ensemble +10%
- **Test penalty**: Fichiers de test -30% (sauf si query mentionne "test")

---

## 2. MCP Server Implementation

### 2.1 Protocol Overview

MCP utilise JSON-RPC 2.0 sur stdio (principal) ou HTTP/SSE (remote). Le serveur expose des "tools" que les clients AI peuvent appeler.

```
┌─────────────────────────────────────────────────────────────────┐
│                     MCP PROTOCOL FLOW                           │
└─────────────────────────────────────────────────────────────────┘

AI Tool (Claude Code)                    Semantiq MCP Server
        │                                         │
        │──── initialize ────────────────────────>│
        │<─── capabilities, tools list ───────────│
        │                                         │
        │──── tools/list ────────────────────────>│
        │<─── [semantiq_search, ...] ─────────────│
        │                                         │
        │──── tools/call ────────────────────────>│
        │     {                                   │
        │       "name": "semantiq_search",        │
        │       "arguments": {                    │
        │         "query": "rate limiting"        │
        │       }                                 │
        │     }                                   │
        │<─── result ─────────────────────────────│
        │     {                                   │
        │       "content": [                      │
        │         { "type": "text", ... }         │
        │       ]                                 │
        │     }                                   │
        │                                         │
```

### 2.2 Rust Implementation avec mcp-rs

```toml
# Cargo.toml
[dependencies]
mcp-server = "0.3"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

```rust
// src/main.rs
use mcp_server::{Server, Tool, ToolResult, Content};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_max_results")]
    max_results: usize,
    #[serde(default)]
    include_content: bool,
}

fn default_max_results() -> usize { 10 }

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = RetrievalEngine::new().await?;

    let server = Server::builder()
        .name("semantiq")
        .version(env!("CARGO_PKG_VERSION"))
        .tool(Tool::new(
            "semantiq_search",
            "Semantic and lexical code search",
            SearchArgs::schema(),
            |args: SearchArgs| async {
                let results = engine.search(&args.query, args.max_results).await?;
                Ok(format_results(results, args.include_content))
            }
        ))
        .tool(Tool::new(
            "semantiq_find_refs",
            "Find all references to a symbol",
            FindRefsArgs::schema(),
            |args| async { /* ... */ }
        ))
        .tool(Tool::new(
            "semantiq_deps",
            "Get dependency graph for a file",
            DepsArgs::schema(),
            |args| async { /* ... */ }
        ))
        .tool(Tool::new(
            "semantiq_explain",
            "Get codebase overview",
            ExplainArgs::schema(),
            |args| async { /* ... */ }
        ))
        .build();

    // Run on stdio (default) or HTTP based on args
    if std::env::args().any(|a| a == "--http") {
        server.serve_http("127.0.0.1:3000").await
    } else {
        server.serve_stdio().await
    }
}
```

### 2.3 Tool Schemas (JSON Schema)

```json
// semantiq_search schema
{
  "name": "semantiq_search",
  "description": "Search codebase using semantic and lexical matching",
  "inputSchema": {
    "type": "object",
    "properties": {
      "query": {
        "type": "string",
        "description": "Natural language query or keywords"
      },
      "max_results": {
        "type": "integer",
        "default": 10,
        "minimum": 1,
        "maximum": 50
      },
      "include_content": {
        "type": "boolean",
        "default": false,
        "description": "Include file content snippets in results"
      },
      "scope": {
        "type": "string",
        "enum": ["all", "src", "tests", "docs"],
        "default": "all"
      }
    },
    "required": ["query"]
  }
}

// semantiq_deps schema
{
  "name": "semantiq_deps",
  "description": "Get dependency graph for a file",
  "inputSchema": {
    "type": "object",
    "properties": {
      "file": {
        "type": "string",
        "description": "File path relative to project root"
      },
      "direction": {
        "type": "string",
        "enum": ["imports", "importers", "both"],
        "default": "both"
      },
      "depth": {
        "type": "integer",
        "default": 2,
        "minimum": 1,
        "maximum": 5
      }
    },
    "required": ["file"]
  }
}
```

### 2.4 Error Handling

```rust
// Custom error types mapping to MCP error codes
#[derive(Debug, thiserror::Error)]
enum SemantiqError {
    #[error("Index not ready: {0}")]
    IndexNotReady(String),

    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    #[error("Invalid query: {0}")]
    InvalidQuery(String),

    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl From<SemantiqError> for mcp_server::Error {
    fn from(e: SemantiqError) -> Self {
        match e {
            SemantiqError::IndexNotReady(_) => {
                mcp_server::Error::new(-32002, e.to_string())
            }
            SemantiqError::FileNotFound(_) => {
                mcp_server::Error::new(-32001, e.to_string())
            }
            SemantiqError::InvalidQuery(_) => {
                mcp_server::Error::invalid_params(e.to_string())
            }
            SemantiqError::Internal(_) => {
                mcp_server::Error::internal(e.to_string())
            }
        }
    }
}
```

### 2.5 Response Formatting

```rust
// Format results for AI consumption
fn format_results(results: Vec<SearchResult>, include_content: bool) -> ToolResult {
    let mut content = String::new();

    content.push_str(&format!("Found {} relevant files:\n\n", results.len()));

    for (i, r) in results.iter().enumerate() {
        content.push_str(&format!(
            "{}. **{}** (relevance: {:.0}%)\n",
            i + 1, r.path.display(), r.score * 100.0
        ));

        if !r.symbols.is_empty() {
            content.push_str("   Symbols: ");
            content.push_str(&r.symbols.join(", "));
            content.push_str("\n");
        }

        if include_content {
            content.push_str("   ```\n");
            content.push_str(&r.snippet);
            content.push_str("\n   ```\n");
        }
        content.push_str("\n");
    }

    ToolResult::text(content)
}
```

---

## 3. Indexation & Storage

### 3.1 SQLite Schema

Un seul fichier SQLite stocke tout: métadonnées, symboles, embeddings, et graphe de dépendances. Portable et sans serveur.

```sql
-- Schema principal
CREATE TABLE files (
    id INTEGER PRIMARY KEY,
    path TEXT UNIQUE NOT NULL,
    content_hash TEXT NOT NULL,      -- SHA256 for change detection
    modified_at INTEGER NOT NULL,     -- Unix timestamp
    size_bytes INTEGER NOT NULL,
    language TEXT,
    indexed_at INTEGER NOT NULL
);

CREATE TABLE symbols (
    id INTEGER PRIMARY KEY,
    file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,               -- function, class, method, variable, etc.
    line_start INTEGER NOT NULL,
    line_end INTEGER NOT NULL,
    signature TEXT,                   -- Function signature if applicable
    doc_comment TEXT                  -- Extracted doc comments
);

CREATE TABLE chunks (
    id INTEGER PRIMARY KEY,
    file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    line_start INTEGER NOT NULL,
    line_end INTEGER NOT NULL,
    embedding BLOB                    -- 384-dim float32 vector
);

CREATE TABLE dependencies (
    id INTEGER PRIMARY KEY,
    from_file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
    to_file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,               -- import, export, re-export
    symbol TEXT,                      -- Specific symbol if named import
    UNIQUE(from_file_id, to_file_id, symbol)
);

-- Indexes for fast lookups
CREATE INDEX idx_files_path ON files(path);
CREATE INDEX idx_files_hash ON files(content_hash);
CREATE INDEX idx_symbols_name ON symbols(name);
CREATE INDEX idx_symbols_file ON symbols(file_id);
CREATE INDEX idx_chunks_file ON chunks(file_id);
CREATE INDEX idx_deps_from ON dependencies(from_file_id);
CREATE INDEX idx_deps_to ON dependencies(to_file_id);

-- Vector search with sqlite-vss
CREATE VIRTUAL TABLE vss_chunks USING vss0(embedding(384));
```

### 3.2 Intelligent Chunking

Contrairement au RAG naïf qui découpe arbitrairement, Semantiq chunk par unités sémantiques (fonctions, classes, blocs logiques).

```rust
// Chunking strategy
fn chunk_file(content: &str, tree: &Tree, language: Language) -> Vec<Chunk> {
    let mut chunks = Vec::new();

    // Strategy 1: Symbol-based chunking (preferred)
    let symbols = extract_symbols(tree, language);
    for symbol in symbols {
        if symbol.line_end - symbol.line_start > 3 {  // Skip tiny symbols
            chunks.push(Chunk {
                content: extract_lines(content, symbol.line_start, symbol.line_end),
                line_start: symbol.line_start,
                line_end: symbol.line_end,
                context: format!("{} {}", symbol.kind, symbol.name),
            });
        }
    }

    // Strategy 2: Fill gaps with sliding window
    let covered: HashSet<_> = chunks.iter()
        .flat_map(|c| c.line_start..=c.line_end)
        .collect();

    let lines: Vec<_> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if !covered.contains(&i) {
            // Find contiguous uncovered region
            let start = i;
            while i < lines.len() && !covered.contains(&i) { i += 1; }
            let end = i;

            // Chunk with overlap
            if end - start > 5 {
                for chunk_start in (start..end).step_by(50) {
                    let chunk_end = (chunk_start + 75).min(end);  // 50 lines + 25 overlap
                    chunks.push(Chunk {
                        content: lines[chunk_start..chunk_end].join("\n"),
                        line_start: chunk_start,
                        line_end: chunk_end,
                        context: "code block".to_string(),
                    });
                }
            }
        }
        i += 1;
    }

    chunks
}
```

### 3.3 Incremental Indexing

```rust
// Watch mode for incremental updates
async fn watch_and_index(root: &Path, index: &Index) -> Result<()> {
    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(tx)?;
    watcher.watch(root, RecursiveMode::Recursive)?;

    while let Ok(event) = rx.recv() {
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) => {
                for path in event.paths {
                    if should_index(&path) {
                        reindex_file(&path, index).await?;
                    }
                }
            }
            EventKind::Remove(_) => {
                for path in event.paths {
                    index.remove_file(&path).await?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

async fn reindex_file(path: &Path, index: &Index) -> Result<()> {
    let content = fs::read_to_string(path).await?;
    let hash = sha256(&content);

    // Skip if unchanged
    if index.get_hash(path).await? == Some(hash.clone()) {
        return Ok(());
    }

    // Parse and extract
    let language = detect_language(path);
    let tree = parse(content, language)?;
    let symbols = extract_symbols(&tree, language);
    let chunks = chunk_file(&content, &tree, language);
    let deps = extract_dependencies(&tree, language);

    // Embed chunks (batched for efficiency)
    let embeddings = embed_batch(&chunks).await?;

    // Update index (transaction)
    index.transaction(|tx| {
        tx.upsert_file(path, &hash)?;
        tx.replace_symbols(path, &symbols)?;
        tx.replace_chunks(path, &chunks, &embeddings)?;
        tx.replace_deps(path, &deps)?;
        Ok(())
    }).await
}
```

### 3.4 Memory Management

Pour les gros repos, on utilise plusieurs stratégies:

- **Streaming**: Parse et indexe fichier par fichier, pas tout en mémoire
- **Batching**: Embeddings par batch de 32 pour optimiser GPU/CPU
- **LRU Cache**: Cache des résultats de recherche fréquents
- **Memory-mapped**: SQLite en mode mmap pour gros index

---

## 4. Embeddings Pipeline

### 4.1 Model Selection

| Model | Size | Dims | Speed | Quality |
|-------|------|------|-------|---------|
| **all-MiniLM-L6-v2** ✓ | 80MB | 384 | ~2ms/chunk | Good (general) |
| CodeBERT | 500MB | 768 | ~8ms/chunk | Better (code) |
| Cohere embed-v3 | API | 1024 | ~50ms/chunk | Best |

**Choix:** all-MiniLM-L6-v2 par défaut (local, rapide, gratuit). Option Pro: Cohere API pour meilleure qualité.

### 4.2 ONNX Runtime Integration

```rust
// Rust ONNX embedding
use ort::{Environment, Session, Value};

pub struct Embedder {
    session: Session,
    tokenizer: Tokenizer,
}

impl Embedder {
    pub fn new(model_path: &Path) -> Result<Self> {
        let environment = Environment::builder()
            .with_name("semantiq")
            .with_execution_providers([
                // Try GPU first, fallback to CPU
                CUDAExecutionProvider::default().build(),
                CPUExecutionProvider::default().build(),
            ])
            .build()?;

        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(4)?
            .commit_from_file(model_path)?;

        let tokenizer = Tokenizer::from_pretrained("sentence-transformers/all-MiniLM-L6-v2")?;

        Ok(Self { session, tokenizer })
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // Tokenize
        let encoding = self.tokenizer.encode(text, true)?;
        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();
        let attention_mask: Vec<i64> = encoding.get_attention_mask().iter().map(|&x| x as i64).collect();

        // Create tensors
        let input_ids = Value::from_array(([1, input_ids.len()], input_ids.as_slice()))?;
        let attention_mask = Value::from_array(([1, attention_mask.len()], attention_mask.as_slice()))?;

        // Run inference
        let outputs = self.session.run(vec![input_ids, attention_mask])?;

        // Mean pooling
        let embeddings: Vec<f32> = outputs[0].try_extract()?.view().to_slice().unwrap().to_vec();
        let pooled = mean_pool(&embeddings, &attention_mask);

        // L2 normalize
        Ok(normalize(&pooled))
    }

    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // Batch processing for efficiency
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

fn mean_pool(embeddings: &[f32], mask: &[i64]) -> Vec<f32> {
    // Average embeddings weighted by attention mask
    let dim = 384;
    let seq_len = mask.len();
    let mut pooled = vec![0.0; dim];
    let mut count = 0.0;

    for i in 0..seq_len {
        if mask[i] == 1 {
            for j in 0..dim {
                pooled[j] += embeddings[i * dim + j];
            }
            count += 1.0;
        }
    }

    for v in &mut pooled {
        *v /= count;
    }

    pooled
}
```

### 4.3 Optimizations

- **Quantization INT8**: Réduit la taille du modèle de 50% avec perte de qualité <1%
- **Batch inference**: Traite 32 chunks à la fois pour amortir l'overhead
- **Caching**: Cache les embeddings par content_hash, skip si déjà calculé
- **Lazy loading**: Charge le modèle uniquement quand nécessaire (Tier 2)

---

## 5. Tree-sitter Integration

### 5.1 Multi-Language Support

```rust
// Language detection and parser loading
use tree_sitter::{Parser, Language};

pub struct MultiParser {
    parsers: HashMap<&'static str, Parser>,
}

impl MultiParser {
    pub fn new() -> Self {
        let mut parsers = HashMap::new();

        // Load common languages
        let languages = [
            ("rs", tree_sitter_rust::language()),
            ("ts", tree_sitter_typescript::language_typescript()),
            ("tsx", tree_sitter_typescript::language_tsx()),
            ("js", tree_sitter_javascript::language()),
            ("py", tree_sitter_python::language()),
            ("go", tree_sitter_go::language()),
            ("java", tree_sitter_java::language()),
            ("rb", tree_sitter_ruby::language()),
            ("c", tree_sitter_c::language()),
            ("cpp", tree_sitter_cpp::language()),
        ];

        for (ext, lang) in languages {
            let mut parser = Parser::new();
            parser.set_language(lang).unwrap();
            parsers.insert(ext, parser);
        }

        Self { parsers }
    }

    pub fn parse(&mut self, path: &Path, content: &str) -> Option<Tree> {
        let ext = path.extension()?.to_str()?;
        let parser = self.parsers.get_mut(ext)?;
        parser.parse(content, None)
    }
}
```

### 5.2 Symbol Extraction

```rust
// Extract symbols using tree-sitter queries
const RUST_QUERY: &str = r#"
    (function_item name: (identifier) @function.name) @function.def
    (impl_item type: (type_identifier) @impl.name) @impl.def
    (struct_item name: (type_identifier) @struct.name) @struct.def
    (enum_item name: (type_identifier) @enum.name) @enum.def
    (trait_item name: (type_identifier) @trait.name) @trait.def
    (const_item name: (identifier) @const.name) @const.def
    (static_item name: (identifier) @static.name) @static.def
"#;

const TYPESCRIPT_QUERY: &str = r#"
    (function_declaration name: (identifier) @function.name) @function.def
    (class_declaration name: (type_identifier) @class.name) @class.def
    (interface_declaration name: (type_identifier) @interface.name) @interface.def
    (type_alias_declaration name: (type_identifier) @type.name) @type.def
    (method_definition name: (property_identifier) @method.name) @method.def
    (arrow_function) @arrow.def
    (variable_declarator name: (identifier) @var.name) @var.def
"#;

pub fn extract_symbols(tree: &Tree, language: &str) -> Vec<Symbol> {
    let query_str = match language {
        "rust" => RUST_QUERY,
        "typescript" | "javascript" => TYPESCRIPT_QUERY,
        // ... other languages
        _ => return vec![],
    };

    let query = Query::new(tree.language(), query_str).unwrap();
    let mut cursor = QueryCursor::new();
    let mut symbols = Vec::new();

    for match_ in cursor.matches(&query, tree.root_node(), source.as_bytes()) {
        for capture in match_.captures {
            let node = capture.node;
            let name = &source[node.byte_range()];
            let kind = capture_to_kind(capture.index, &query);

            symbols.push(Symbol {
                name: name.to_string(),
                kind,
                line_start: node.start_position().row,
                line_end: node.end_position().row,
                signature: extract_signature(&node, source),
                doc_comment: extract_doc_comment(&node, source),
            });
        }
    }

    symbols
}
```

### 5.3 Dependency Graph Construction

```rust
// Extract imports/exports for dependency graph
const IMPORT_QUERY_TS: &str = r#"
    (import_statement
        source: (string) @source
        (import_clause
            (named_imports (import_specifier name: (identifier) @symbol))?
            (identifier)? @default
        )?
    ) @import

    (export_statement
        source: (string)? @re_export_source
        declaration: (_)? @export_decl
    ) @export
"#;

pub fn extract_dependencies(tree: &Tree, file_path: &Path) -> Vec<Dependency> {
    let query = Query::new(tree.language(), IMPORT_QUERY_TS).unwrap();
    let mut cursor = QueryCursor::new();
    let mut deps = Vec::new();

    for match_ in cursor.matches(&query, tree.root_node(), source.as_bytes()) {
        let source_capture = match_.captures.iter()
            .find(|c| query.capture_names()[c.index as usize] == "source");

        if let Some(cap) = source_capture {
            let import_path = &source[cap.node.byte_range()];
            let import_path = import_path.trim_matches('"').trim_matches('\'');

            // Resolve relative path
            let resolved = resolve_import(file_path, import_path);

            // Extract imported symbols
            let symbols: Vec<_> = match_.captures.iter()
                .filter(|c| query.capture_names()[c.index as usize] == "symbol")
                .map(|c| source[c.node.byte_range()].to_string())
                .collect();

            deps.push(Dependency {
                from: file_path.to_path_buf(),
                to: resolved,
                kind: DependencyKind::Import,
                symbols,
            });
        }
    }

    deps
}

fn resolve_import(from: &Path, import_path: &str) -> PathBuf {
    if import_path.starts_with('.') {
        // Relative import
        let dir = from.parent().unwrap();
        let resolved = dir.join(import_path);
        // Try common extensions
        for ext in ["ts", "tsx", "js", "jsx", "index.ts", "index.js"] {
            let with_ext = resolved.with_extension(ext);
            if with_ext.exists() {
                return with_ext;
            }
        }
        resolved
    } else {
        // Node module or alias - would need tsconfig/package.json resolution
        PathBuf::from(format!("node_modules/{}", import_path))
    }
}
```

---

## 6. Daemon Architecture

### 6.1 Lifecycle Management

```
┌─────────────────────────────────────────────────────────────────┐
│                    SEMANTIQ DAEMON LIFECYCLE                    │
└─────────────────────────────────────────────────────────────────┘

                        ┌─────────────┐
                        │   STOPPED   │
                        └──────┬──────┘
                               │
                    semantiq serve
                               │
                               ▼
                        ┌─────────────┐
                        │  STARTING   │──── Load config
                        └──────┬──────┘     Detect project root
                               │            Initialize SQLite
                               │
                               ▼
                        ┌─────────────┐
                        │  INDEXING   │──── Parse all files
                        │  (initial)  │     Extract symbols
                        └──────┬──────┘     Generate embeddings
                               │            Build dep graph
                               │
                               ▼
                        ┌─────────────┐
           ┌───────────│    READY    │◄──────────────┐
           │           └──────┬──────┘               │
           │                  │                      │
      MCP request         File change           Reindex
           │                  │                  complete
           ▼                  ▼                      │
    ┌─────────────┐    ┌─────────────┐              │
    │  HANDLING   │    │  UPDATING   │──────────────┘
    │   REQUEST   │    │   INDEX     │
    └─────────────┘    └─────────────┘

                    SIGTERM / SIGINT
                               │
                               ▼
                        ┌─────────────┐
                        │  STOPPING   │──── Flush pending writes
                        └──────┬──────┘     Close connections
                               │
                               ▼
                        ┌─────────────┐
                        │   STOPPED   │
                        └─────────────┘
```

### 6.2 Process Architecture

```rust
// Main daemon structure
pub struct Daemon {
    config: Config,
    index: Arc<Index>,
    embedder: Arc<Embedder>,
    watcher: Option<RecommendedWatcher>,
    mcp_server: McpServer,
    state: Arc<AtomicState>,
}

impl Daemon {
    pub async fn run(self) -> Result<()> {
        // 1. Initial indexing
        self.state.set(State::Indexing);
        self.index_all().await?;
        self.state.set(State::Ready);

        // 2. Start file watcher in background
        let index = self.index.clone();
        let embedder = self.embedder.clone();
        tokio::spawn(async move {
            watch_files(index, embedder).await
        });

        // 3. Run MCP server (blocks)
        self.mcp_server.serve_stdio().await
    }

    async fn index_all(&self) -> Result<()> {
        let files = discover_files(&self.config.root)?;
        let total = files.len();

        // Process in parallel with bounded concurrency
        let semaphore = Arc::new(Semaphore::new(8));
        let mut handles = Vec::new();

        for (i, file) in files.into_iter().enumerate() {
            let permit = semaphore.clone().acquire_owned().await?;
            let index = self.index.clone();
            let embedder = self.embedder.clone();

            handles.push(tokio::spawn(async move {
                let result = index_file(&file, &index, &embedder).await;
                drop(permit);
                result
            }));

            // Progress reporting
            if i % 100 == 0 {
                eprintln!("Indexing: {}/{} files", i, total);
            }
        }

        // Wait for all
        for handle in handles {
            handle.await??;
        }

        Ok(())
    }
}
```

### 6.3 Configuration

```toml
# .semantiq.toml (optional, auto-detected defaults work)
[index]
# Patterns to ignore (in addition to .gitignore)
ignore = [
    "node_modules",
    "target",
    "dist",
    ".git",
    "*.min.js",
    "*.bundle.js"
]

# File size limit for indexing
max_file_size = "1MB"

# Languages to index (empty = all supported)
languages = []

[search]
# Default number of results
default_limit = 10

# Weight tuning (advanced)
[search.weights]
lexical = 0.25
semantic = 0.40
ast = 0.20
graph = 0.15

[embeddings]
# Use local model (default) or cloud API
provider = "local"  # or "cohere", "openai"
# api_key = "..." (if using cloud)

[performance]
# Indexing concurrency
parallel_files = 8

# Embedding batch size
embedding_batch_size = 32

# Memory limit for embeddings cache
cache_size = "100MB"
```

### 6.4 Auto-Update Mechanism

```rust
// Self-update mechanism
pub async fn check_for_updates() -> Result<Option<Release>> {
    let current = env!("CARGO_PKG_VERSION");
    let resp = reqwest::get("https://api.github.com/repos/semantiq/semantiq/releases/latest")
        .await?
        .json::<GithubRelease>()
        .await?;

    if semver::Version::parse(&resp.tag_name)? > semver::Version::parse(current)? {
        Ok(Some(Release {
            version: resp.tag_name,
            download_url: get_asset_url(&resp, current_platform()),
            changelog: resp.body,
        }))
    } else {
        Ok(None)
    }
}

pub async fn self_update(release: &Release) -> Result<()> {
    // Download new binary to temp location
    let temp_path = std::env::temp_dir().join("semantiq-new");
    download_file(&release.download_url, &temp_path).await?;

    // Replace current binary (platform-specific)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o755))?;
        self_replace::self_replace(&temp_path)?;
    }

    #[cfg(windows)]
    {
        self_replace::self_replace(&temp_path)?;
    }

    Ok(())
}
```

### 6.5 Health Monitoring

```rust
// Health metrics exposed via MCP resource
pub struct Health {
    pub state: State,
    pub indexed_files: usize,
    pub indexed_symbols: usize,
    pub index_size_bytes: u64,
    pub memory_usage_bytes: u64,
    pub uptime_seconds: u64,
    pub last_index_duration_ms: u64,
    pub avg_search_latency_ms: f64,
}

// Expose as MCP resource
impl McpServer {
    fn register_resources(&mut self) {
        self.add_resource(Resource::new(
            "semantiq://health",
            "Semantiq daemon health metrics",
            || async {
                let health = get_health_metrics().await;
                ResourceContent::json(&health)
            }
        ));

        self.add_resource(Resource::new(
            "semantiq://index/stats",
            "Index statistics",
            || async {
                let stats = get_index_stats().await;
                ResourceContent::json(&stats)
            }
        ));
    }
}
```

---

## Conclusion

Cette spécification technique couvre les 6 composants clés de Semantiq:

1. **Retrieval Engine**: Fusion multi-stratégie avec RRF pour des résultats pertinents
2. **MCP Server**: Implémentation Rust avec mcp-rs, 4 tools exposés
3. **Indexation**: SQLite single-file avec chunking intelligent et updates incrémentaux
4. **Embeddings**: ONNX Runtime avec MiniLM, batching et caching
5. **Tree-sitter**: Multi-langage, extraction de symboles et graphe de dépendances
6. **Daemon**: Gestion du lifecycle, watch mode, auto-update

**Prochaine étape:** Implémenter le MVP (Phase 1) en commençant par le MCP Server et le Retrieval Engine de base.
