//! HTTP API request and response types

use serde::{Deserialize, Serialize};

// ============================================
// Search
// ============================================

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
    pub min_score: Option<f32>,
    pub file_type: Option<String>,
    pub symbol_kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f32,
    pub content: String,
    pub metadata: SearchMetadata,
}

#[derive(Debug, Serialize)]
pub struct SearchMetadata {
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub total_count: usize,
    pub search_time_ms: u64,
}

// ============================================
// Find Refs
// ============================================

#[derive(Debug, Deserialize)]
pub struct FindRefsRequest {
    pub symbol: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct Reference {
    pub file_path: String,
    pub line: u32,
    pub column: Option<u32>,
    pub usage_type: String,
    pub context: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FindRefsResponse {
    pub symbol: String,
    pub definitions: Vec<Reference>,
    pub references: Vec<Reference>,
    pub total_count: usize,
    pub search_time_ms: u64,
}

// ============================================
// Deps
// ============================================

#[derive(Debug, Deserialize)]
pub struct DepsRequest {
    pub file_path: String,
}

#[derive(Debug, Serialize)]
pub struct Dependency {
    pub path: String,
    pub symbols: Option<Vec<String>>,
    pub kind: String,
}

#[derive(Debug, Serialize)]
pub struct DepsResponse {
    pub file_path: String,
    pub imports: Vec<Dependency>,
    pub imported_by: Vec<Dependency>,
    pub search_time_ms: u64,
}

// ============================================
// Explain
// ============================================

#[derive(Debug, Deserialize)]
pub struct ExplainRequest {
    pub symbol: String,
}

#[derive(Debug, Serialize)]
pub struct SymbolDefinition {
    pub file_path: String,
    pub line: u32,
    pub signature: Option<String>,
    pub documentation: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainResponse {
    pub symbol: String,
    pub kind: String,
    pub definitions: Vec<SymbolDefinition>,
    pub related_symbols: Vec<String>,
    pub search_time_ms: u64,
}

// ============================================
// Stats
// ============================================

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub indexed_files: usize,
    pub indexed_symbols: usize,
    pub indexed_chunks: usize,
    pub indexed_dependencies: usize,
}

// ============================================
// Health
// ============================================

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

// ============================================
// Error
// ============================================

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: String,
}
