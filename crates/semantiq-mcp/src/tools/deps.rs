use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemantiqDeps {
    pub file_path: String,
    pub direction: DependencyDirection,
    pub depth: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DependencyDirection {
    /// What this file imports
    Imports,
    /// What imports this file
    ImportedBy,
    /// Both directions
    #[default]
    Both,
}

impl SemantiqDeps {
    pub fn new(file_path: &str) -> Self {
        Self {
            file_path: file_path.to_string(),
            direction: DependencyDirection::default(),
            depth: None,
        }
    }

    pub fn imports_only(mut self) -> Self {
        self.direction = DependencyDirection::Imports;
        self
    }

    pub fn imported_by_only(mut self) -> Self {
        self.direction = DependencyDirection::ImportedBy;
        self
    }

    pub fn with_depth(mut self, depth: usize) -> Self {
        self.depth = Some(depth);
        self
    }
}
