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
}
