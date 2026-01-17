use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchResultKind {
    Symbol,
    TextMatch,
    SemanticMatch,
    Reference,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub kind: SearchResultKind,
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub score: f32,
    pub metadata: SearchResultMetadata,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchResultMetadata {
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub match_type: Option<String>,
    pub context: Option<String>,
}

impl SearchResult {
    pub fn new(
        kind: SearchResultKind,
        file_path: String,
        start_line: usize,
        end_line: usize,
        content: String,
        score: f32,
    ) -> Self {
        Self {
            kind,
            file_path,
            start_line,
            end_line,
            content,
            score,
            metadata: SearchResultMetadata::default(),
        }
    }

    pub fn with_metadata(mut self, metadata: SearchResultMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn location(&self) -> String {
        if self.start_line == self.end_line {
            format!("{}:{}", self.file_path, self.start_line)
        } else {
            format!("{}:{}-{}", self.file_path, self.start_line, self.end_line)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResults {
    pub query: String,
    pub results: Vec<SearchResult>,
    pub total_count: usize,
    pub search_time_ms: u64,
}

impl SearchResults {
    pub fn new(query: String, results: Vec<SearchResult>, search_time_ms: u64) -> Self {
        let total_count = results.len();
        Self {
            query,
            results,
            total_count,
            search_time_ms,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    pub fn top(&self, n: usize) -> Vec<&SearchResult> {
        self.results.iter().take(n).collect()
    }

    pub fn merge(&mut self, other: SearchResults) {
        self.results.extend(other.results);
        self.results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        self.total_count = self.results.len();
    }
}
