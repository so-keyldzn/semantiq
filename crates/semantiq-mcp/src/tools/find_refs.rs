use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemantiqFindRefs {
    pub symbol: String,
    pub limit: Option<usize>,
    pub include_definitions: bool,
    pub include_usages: bool,
}

impl SemantiqFindRefs {
    pub fn new(symbol: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            limit: None,
            include_definitions: true,
            include_usages: true,
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn definitions_only(mut self) -> Self {
        self.include_usages = false;
        self
    }

    pub fn usages_only(mut self) -> Self {
        self.include_definitions = false;
        self
    }
}
