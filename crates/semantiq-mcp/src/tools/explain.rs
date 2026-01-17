use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemantiqExplain {
    pub symbol: String,
    pub include_source: bool,
    pub include_docs: bool,
    pub include_related: bool,
}

impl SemantiqExplain {
    pub fn new(symbol: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            include_source: true,
            include_docs: true,
            include_related: true,
        }
    }

    pub fn minimal(mut self) -> Self {
        self.include_source = false;
        self.include_related = false;
        self
    }

    pub fn with_source(mut self, include: bool) -> Self {
        self.include_source = include;
        self
    }

    pub fn with_docs(mut self, include: bool) -> Self {
        self.include_docs = include;
        self
    }

    pub fn with_related(mut self, include: bool) -> Self {
        self.include_related = include;
        self
    }
}
