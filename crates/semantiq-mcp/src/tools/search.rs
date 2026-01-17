use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemantiqSearch {
    pub query: String,
    pub limit: Option<usize>,
    pub languages: Option<Vec<String>>,
    pub file_patterns: Option<Vec<String>>,
}

impl SemantiqSearch {
    pub fn new(query: &str) -> Self {
        Self {
            query: query.to_string(),
            limit: None,
            languages: None,
            file_patterns: None,
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_languages(mut self, languages: Vec<String>) -> Self {
        self.languages = Some(languages);
        self
    }

    pub fn with_file_patterns(mut self, patterns: Vec<String>) -> Self {
        self.file_patterns = Some(patterns);
        self
    }
}
