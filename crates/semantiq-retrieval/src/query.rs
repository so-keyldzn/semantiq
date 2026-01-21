use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub expanded_terms: Vec<String>,
    pub filters: QueryFilters,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryFilters {
    pub languages: Vec<String>,
    pub file_patterns: Vec<String>,
    pub symbol_kinds: Vec<String>,
    pub include_tests: bool,
}

impl Query {
    pub fn new(text: &str) -> Self {
        let expander = QueryExpander::new();
        let expanded_terms = expander.expand(text);

        Self {
            text: text.to_string(),
            expanded_terms,
            filters: QueryFilters::default(),
        }
    }

    pub fn with_filters(mut self, filters: QueryFilters) -> Self {
        self.filters = filters;
        self
    }

    pub fn all_terms(&self) -> Vec<&str> {
        let mut terms: Vec<&str> = vec![&self.text];
        terms.extend(self.expanded_terms.iter().map(|s| s.as_str()));
        terms
    }
}

pub struct QueryExpander;

impl QueryExpander {
    pub fn new() -> Self {
        Self
    }

    pub fn expand(&self, text: &str) -> Vec<String> {
        let mut expanded = Vec::new();

        // Split on whitespace and process each term
        for term in text.split_whitespace() {
            // Add case variations
            expanded.extend(self.case_variations(term));
        }

        // Remove duplicates while preserving order
        let mut seen = std::collections::HashSet::new();
        expanded.retain(|x| {
            let normalized = x.to_lowercase();
            if seen.contains(&normalized) || normalized == text.to_lowercase() {
                false
            } else {
                seen.insert(normalized);
                true
            }
        });

        expanded
    }

    fn case_variations(&self, term: &str) -> Vec<String> {
        let mut variations = Vec::new();

        // snake_case to camelCase
        if term.contains('_') {
            variations.push(self.snake_to_camel(term));
            variations.push(self.snake_to_pascal(term));
        }

        // camelCase to snake_case
        if self.is_camel_case(term) {
            variations.push(self.camel_to_snake(term));
        }

        // PascalCase to snake_case
        if self.is_pascal_case(term) {
            variations.push(self.camel_to_snake(term));
            variations.push(self.pascal_to_camel(term));
        }

        // kebab-case variations
        if term.contains('-') {
            variations.push(term.replace('-', "_"));
            variations.push(self.kebab_to_camel(term));
        }

        variations
    }

    fn snake_to_camel(&self, s: &str) -> String {
        let mut result = String::new();
        let mut capitalize_next = false;

        for c in s.chars() {
            if c == '_' {
                capitalize_next = true;
            } else if capitalize_next {
                result.push(c.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                result.push(c);
            }
        }

        result
    }

    fn snake_to_pascal(&self, s: &str) -> String {
        let camel = self.snake_to_camel(s);
        let mut chars = camel.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    }

    fn camel_to_snake(&self, s: &str) -> String {
        let mut result = String::new();

        for (i, c) in s.chars().enumerate() {
            if c.is_uppercase() {
                if i > 0 {
                    result.push('_');
                }
                result.push(c.to_ascii_lowercase());
            } else {
                result.push(c);
            }
        }

        result
    }

    fn pascal_to_camel(&self, s: &str) -> String {
        let mut chars = s.chars();
        match chars.next() {
            Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    }

    fn kebab_to_camel(&self, s: &str) -> String {
        self.snake_to_camel(&s.replace('-', "_"))
    }

    fn is_camel_case(&self, s: &str) -> bool {
        let mut chars = s.chars();
        if let Some(first) = chars.next() {
            if first.is_lowercase() {
                return chars.any(|c| c.is_uppercase());
            }
        }
        false
    }

    fn is_pascal_case(&self, s: &str) -> bool {
        let mut chars = s.chars();
        if let Some(first) = chars.next() {
            if first.is_uppercase() {
                return chars.any(|c| c.is_uppercase() || c.is_lowercase());
            }
        }
        false
    }
}

impl Default for QueryExpander {
    fn default() -> Self {
        Self::new()
    }
}

/// Options for filtering and configuring search behavior
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Minimum score threshold (0.0-1.0). Results below this score are excluded.
    pub min_score: Option<f32>,
    /// File extensions to include (e.g., ["rs", "ts"]). If set, only these extensions are searched.
    pub file_types: Option<Vec<String>>,
    /// Symbol kinds to include (e.g., ["function", "class"]). If set, only these symbol types are returned.
    pub symbol_kinds: Option<Vec<String>>,
}

impl SearchOptions {
    /// Default minimum score threshold
    pub const DEFAULT_MIN_SCORE: f32 = 0.35;

    /// Extensions excluded by default when no file_types filter is set
    pub const EXCLUDED_EXTENSIONS: &'static [&'static str] = &[
        "json",
        "lock",
        "yaml",
        "yml",
        "md",
        "txt",
        "toml",
        "xml",
        "csv",
        "log",
        "env",
        "gitignore",
        "dockerignore",
        "editorconfig",
        "prettierrc",
        "eslintrc",
    ];

    /// Valid symbol kinds for filtering
    pub const VALID_SYMBOL_KINDS: &'static [&'static str] = &[
        "function",
        "method",
        "class",
        "struct",
        "enum",
        "interface",
        "trait",
        "module",
        "variable",
        "constant",
        "type",
    ];

    /// Create new SearchOptions with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Create SearchOptions with a minimum score
    pub fn with_min_score(mut self, min_score: f32) -> Self {
        self.min_score = Some(min_score.clamp(0.0, 1.0));
        self
    }

    /// Create SearchOptions with file type filter
    pub fn with_file_types(mut self, file_types: Vec<String>) -> Self {
        self.file_types = Some(file_types);
        self
    }

    /// Create SearchOptions with symbol kind filter
    pub fn with_symbol_kinds(mut self, symbol_kinds: Vec<String>) -> Self {
        self.symbol_kinds = Some(symbol_kinds);
        self
    }

    /// Get the effective minimum score (uses default if not set)
    pub fn effective_min_score(&self) -> f32 {
        self.min_score.unwrap_or(Self::DEFAULT_MIN_SCORE)
    }

    /// Check if a file extension is accepted by these options
    pub fn accepts_extension(&self, ext: &str) -> bool {
        let ext_lower = ext.to_lowercase();

        if let Some(ref file_types) = self.file_types {
            // If file_types is set, only accept those extensions
            file_types.iter().any(|ft| ft.to_lowercase() == ext_lower)
        } else {
            // Otherwise, exclude the default excluded extensions
            !Self::EXCLUDED_EXTENSIONS.contains(&ext_lower.as_str())
        }
    }

    /// Check if a symbol kind is accepted by these options
    pub fn accepts_symbol_kind(&self, kind: &str) -> bool {
        if let Some(ref symbol_kinds) = self.symbol_kinds {
            let kind_lower = kind.to_lowercase();
            symbol_kinds
                .iter()
                .any(|sk| sk.to_lowercase() == kind_lower)
        } else {
            // Accept all symbol kinds if no filter is set
            true
        }
    }

    /// Parse a comma-separated string into a vector of trimmed strings
    pub fn parse_csv(input: &str) -> Vec<String> {
        input
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snake_to_camel() {
        let expander = QueryExpander::new();
        assert_eq!(expander.snake_to_camel("hello_world"), "helloWorld");
        assert_eq!(expander.snake_to_camel("get_user_by_id"), "getUserById");
    }

    #[test]
    fn test_camel_to_snake() {
        let expander = QueryExpander::new();
        assert_eq!(expander.camel_to_snake("helloWorld"), "hello_world");
        assert_eq!(expander.camel_to_snake("getUserById"), "get_user_by_id");
    }

    #[test]
    fn test_query_expansion() {
        let query = Query::new("get_user");
        assert!(query.expanded_terms.contains(&"getUser".to_string()));
    }

    #[test]
    fn test_snake_to_pascal() {
        let expander = QueryExpander::new();
        assert_eq!(expander.snake_to_pascal("hello_world"), "HelloWorld");
        assert_eq!(expander.snake_to_pascal("get_user"), "GetUser");
    }

    #[test]
    fn test_pascal_to_camel() {
        let expander = QueryExpander::new();
        assert_eq!(expander.pascal_to_camel("HelloWorld"), "helloWorld");
        assert_eq!(expander.pascal_to_camel("GetUser"), "getUser");
    }

    #[test]
    fn test_kebab_to_camel() {
        let expander = QueryExpander::new();
        assert_eq!(expander.kebab_to_camel("hello-world"), "helloWorld");
        assert_eq!(expander.kebab_to_camel("get-user-by-id"), "getUserById");
    }

    #[test]
    fn test_is_camel_case() {
        let expander = QueryExpander::new();
        assert!(expander.is_camel_case("helloWorld"));
        assert!(expander.is_camel_case("getUser"));
        assert!(!expander.is_camel_case("HelloWorld")); // PascalCase
        assert!(!expander.is_camel_case("hello")); // All lowercase
        assert!(!expander.is_camel_case("HELLO")); // All uppercase
    }

    #[test]
    fn test_is_pascal_case() {
        let expander = QueryExpander::new();
        assert!(expander.is_pascal_case("HelloWorld"));
        assert!(expander.is_pascal_case("GetUser"));
        assert!(!expander.is_pascal_case("helloWorld")); // camelCase
    }

    #[test]
    fn test_query_new() {
        let query = Query::new("search_term");
        assert_eq!(query.text, "search_term");
        assert!(!query.filters.include_tests);
        assert!(query.filters.languages.is_empty());
    }

    #[test]
    fn test_query_with_filters() {
        let filters = QueryFilters {
            languages: vec!["rust".to_string(), "python".to_string()],
            file_patterns: vec!["*.rs".to_string()],
            symbol_kinds: vec!["function".to_string()],
            include_tests: true,
        };

        let query = Query::new("test").with_filters(filters);
        assert_eq!(query.filters.languages.len(), 2);
        assert!(query.filters.include_tests);
    }

    #[test]
    fn test_query_all_terms() {
        let query = Query::new("get_user");
        let terms = query.all_terms();

        // Should include original term
        assert!(terms.contains(&"get_user"));
        // Should include expanded terms
        assert!(terms.len() >= 1);
    }

    #[test]
    fn test_case_variations_snake_case() {
        let expander = QueryExpander::new();
        let variations = expander.case_variations("hello_world");

        assert!(variations.contains(&"helloWorld".to_string()));
        assert!(variations.contains(&"HelloWorld".to_string()));
    }

    #[test]
    fn test_case_variations_camel_case() {
        let expander = QueryExpander::new();
        let variations = expander.case_variations("helloWorld");

        assert!(variations.contains(&"hello_world".to_string()));
    }

    #[test]
    fn test_case_variations_pascal_case() {
        let expander = QueryExpander::new();
        let variations = expander.case_variations("HelloWorld");

        assert!(variations.contains(&"hello_world".to_string()));
        assert!(variations.contains(&"helloWorld".to_string()));
    }

    #[test]
    fn test_case_variations_kebab_case() {
        let expander = QueryExpander::new();
        let variations = expander.case_variations("hello-world");

        assert!(variations.contains(&"hello_world".to_string()));
        assert!(variations.contains(&"helloWorld".to_string()));
    }

    #[test]
    fn test_expand_removes_duplicates() {
        let expander = QueryExpander::new();
        let expanded = expander.expand("test");

        // Check no duplicates
        let mut seen = std::collections::HashSet::new();
        for term in &expanded {
            assert!(
                seen.insert(term.to_lowercase()),
                "Duplicate found: {}",
                term
            );
        }
    }

    #[test]
    fn test_expand_does_not_include_original() {
        let expander = QueryExpander::new();
        let expanded = expander.expand("get_user");

        // Should not include the original term itself
        assert!(!expanded.iter().any(|t| t.to_lowercase() == "get_user"));
    }

    #[test]
    fn test_query_filters_default() {
        let filters = QueryFilters::default();
        assert!(filters.languages.is_empty());
        assert!(filters.file_patterns.is_empty());
        assert!(filters.symbol_kinds.is_empty());
        assert!(!filters.include_tests);
    }

    #[test]
    fn test_query_expander_default() {
        let expander = QueryExpander::default();
        // Should work the same as new()
        assert_eq!(expander.snake_to_camel("test_case"), "testCase");
    }

    // SearchOptions tests

    #[test]
    fn test_search_options_default() {
        let options = SearchOptions::default();
        assert!(options.min_score.is_none());
        assert!(options.file_types.is_none());
        assert!(options.symbol_kinds.is_none());
    }

    #[test]
    fn test_search_options_default_min_score() {
        let options = SearchOptions::default();
        assert!((options.effective_min_score() - SearchOptions::DEFAULT_MIN_SCORE).abs() < 0.001);
    }

    #[test]
    fn test_search_options_with_min_score() {
        let options = SearchOptions::new().with_min_score(0.5);
        assert!((options.effective_min_score() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_search_options_min_score_clamped() {
        let options_high = SearchOptions::new().with_min_score(1.5);
        assert!((options_high.effective_min_score() - 1.0).abs() < 0.001);

        let options_low = SearchOptions::new().with_min_score(-0.5);
        assert!((options_low.effective_min_score() - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_accepts_extension_default_excludes_json() {
        let options = SearchOptions::default();
        assert!(!options.accepts_extension("json"));
        assert!(!options.accepts_extension("JSON"));
        assert!(!options.accepts_extension("lock"));
        assert!(!options.accepts_extension("yaml"));
        assert!(!options.accepts_extension("yml"));
        assert!(!options.accepts_extension("md"));
        assert!(!options.accepts_extension("toml"));
    }

    #[test]
    fn test_accepts_extension_default_includes_code() {
        let options = SearchOptions::default();
        assert!(options.accepts_extension("rs"));
        assert!(options.accepts_extension("ts"));
        assert!(options.accepts_extension("py"));
        assert!(options.accepts_extension("go"));
        assert!(options.accepts_extension("js"));
    }

    #[test]
    fn test_accepts_extension_custom_filter() {
        let options =
            SearchOptions::new().with_file_types(vec!["rs".to_string(), "ts".to_string()]);
        assert!(options.accepts_extension("rs"));
        assert!(options.accepts_extension("RS"));
        assert!(options.accepts_extension("ts"));
        assert!(!options.accepts_extension("py"));
        assert!(!options.accepts_extension("js"));
        // When custom filter is set, excluded extensions are allowed if in filter
        // but json is not in our filter, so still excluded
        assert!(!options.accepts_extension("json"));
    }

    #[test]
    fn test_accepts_symbol_kind_default() {
        let options = SearchOptions::default();
        assert!(options.accepts_symbol_kind("function"));
        assert!(options.accepts_symbol_kind("class"));
        assert!(options.accepts_symbol_kind("method"));
        assert!(options.accepts_symbol_kind("anything")); // accepts all when no filter
    }

    #[test]
    fn test_accepts_symbol_kind_with_filter() {
        let options = SearchOptions::new()
            .with_symbol_kinds(vec!["function".to_string(), "class".to_string()]);
        assert!(options.accepts_symbol_kind("function"));
        assert!(options.accepts_symbol_kind("FUNCTION")); // case insensitive
        assert!(options.accepts_symbol_kind("class"));
        assert!(!options.accepts_symbol_kind("method"));
        assert!(!options.accepts_symbol_kind("variable"));
    }

    #[test]
    fn test_parse_csv() {
        let result = SearchOptions::parse_csv("rs, ts, py");
        assert_eq!(result, vec!["rs", "ts", "py"]);

        let result_with_spaces = SearchOptions::parse_csv("  function ,  class  ");
        assert_eq!(result_with_spaces, vec!["function", "class"]);

        let result_empty = SearchOptions::parse_csv("");
        assert!(result_empty.is_empty());

        let result_single = SearchOptions::parse_csv("rs");
        assert_eq!(result_single, vec!["rs"]);
    }

    #[test]
    fn test_search_options_builder_chain() {
        let options = SearchOptions::new()
            .with_min_score(0.6)
            .with_file_types(vec!["rs".to_string()])
            .with_symbol_kinds(vec!["function".to_string()]);

        assert!((options.effective_min_score() - 0.6).abs() < 0.001);
        assert!(options.accepts_extension("rs"));
        assert!(!options.accepts_extension("ts"));
        assert!(options.accepts_symbol_kind("function"));
        assert!(!options.accepts_symbol_kind("class"));
    }
}
