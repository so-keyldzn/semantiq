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
        self.results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.total_count = self.results.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_result_new() {
        let result = SearchResult::new(
            SearchResultKind::Symbol,
            "test.rs".to_string(),
            10,
            20,
            "fn test() {}".to_string(),
            0.9,
        );

        assert_eq!(result.file_path, "test.rs");
        assert_eq!(result.start_line, 10);
        assert_eq!(result.end_line, 20);
        assert_eq!(result.score, 0.9);
        assert_eq!(result.kind, SearchResultKind::Symbol);
    }

    #[test]
    fn test_search_result_location_single_line() {
        let result = SearchResult::new(
            SearchResultKind::TextMatch,
            "src/main.rs".to_string(),
            42,
            42,
            "let x = 1;".to_string(),
            0.5,
        );

        assert_eq!(result.location(), "src/main.rs:42");
    }

    #[test]
    fn test_search_result_location_multi_line() {
        let result = SearchResult::new(
            SearchResultKind::Symbol,
            "src/lib.rs".to_string(),
            10,
            25,
            "fn foo() { ... }".to_string(),
            0.8,
        );

        assert_eq!(result.location(), "src/lib.rs:10-25");
    }

    #[test]
    fn test_search_result_with_metadata() {
        let result = SearchResult::new(
            SearchResultKind::Symbol,
            "test.rs".to_string(),
            1,
            5,
            "fn hello()".to_string(),
            1.0,
        )
        .with_metadata(SearchResultMetadata {
            symbol_name: Some("hello".to_string()),
            symbol_kind: Some("function".to_string()),
            match_type: Some("definition".to_string()),
            context: Some("/// A greeting function".to_string()),
        });

        assert_eq!(result.metadata.symbol_name, Some("hello".to_string()));
        assert_eq!(result.metadata.symbol_kind, Some("function".to_string()));
    }

    #[test]
    fn test_search_results_new() {
        let results = vec![
            SearchResult::new(
                SearchResultKind::Symbol,
                "a.rs".to_string(),
                1,
                1,
                "fn a()".to_string(),
                0.9,
            ),
            SearchResult::new(
                SearchResultKind::TextMatch,
                "b.rs".to_string(),
                2,
                2,
                "let b".to_string(),
                0.5,
            ),
        ];

        let search_results = SearchResults::new("test".to_string(), results, 100);

        assert_eq!(search_results.query, "test");
        assert_eq!(search_results.total_count, 2);
        assert_eq!(search_results.search_time_ms, 100);
    }

    #[test]
    fn test_search_results_is_empty() {
        let empty = SearchResults::new("test".to_string(), vec![], 10);
        assert!(empty.is_empty());

        let non_empty = SearchResults::new(
            "test".to_string(),
            vec![SearchResult::new(
                SearchResultKind::Symbol,
                "a.rs".to_string(),
                1,
                1,
                "fn a()".to_string(),
                0.9,
            )],
            10,
        );
        assert!(!non_empty.is_empty());
    }

    #[test]
    fn test_search_results_top() {
        let results = vec![
            SearchResult::new(
                SearchResultKind::Symbol,
                "a.rs".to_string(),
                1,
                1,
                "fn a()".to_string(),
                0.9,
            ),
            SearchResult::new(
                SearchResultKind::Symbol,
                "b.rs".to_string(),
                2,
                2,
                "fn b()".to_string(),
                0.8,
            ),
            SearchResult::new(
                SearchResultKind::Symbol,
                "c.rs".to_string(),
                3,
                3,
                "fn c()".to_string(),
                0.7,
            ),
        ];

        let search_results = SearchResults::new("test".to_string(), results, 50);
        let top2 = search_results.top(2);

        assert_eq!(top2.len(), 2);
        assert_eq!(top2[0].file_path, "a.rs");
        assert_eq!(top2[1].file_path, "b.rs");
    }

    #[test]
    fn test_search_results_merge() {
        let results1 = vec![SearchResult::new(
            SearchResultKind::Symbol,
            "a.rs".to_string(),
            1,
            1,
            "fn a()".to_string(),
            0.9,
        )];
        let results2 = vec![SearchResult::new(
            SearchResultKind::Symbol,
            "b.rs".to_string(),
            2,
            2,
            "fn b()".to_string(),
            0.95,
        )];

        let mut search_results1 = SearchResults::new("test".to_string(), results1, 50);
        let search_results2 = SearchResults::new("test".to_string(), results2, 30);

        search_results1.merge(search_results2);

        assert_eq!(search_results1.total_count, 2);
        // After merge, results should be sorted by score (highest first)
        assert_eq!(search_results1.results[0].file_path, "b.rs"); // 0.95
        assert_eq!(search_results1.results[1].file_path, "a.rs"); // 0.9
    }

    #[test]
    fn test_search_result_kind_serialization() {
        assert_eq!(SearchResultKind::Symbol, SearchResultKind::Symbol);
        assert_eq!(SearchResultKind::TextMatch, SearchResultKind::TextMatch);
        assert_eq!(
            SearchResultKind::SemanticMatch,
            SearchResultKind::SemanticMatch
        );
        assert_eq!(SearchResultKind::Reference, SearchResultKind::Reference);
    }
}
