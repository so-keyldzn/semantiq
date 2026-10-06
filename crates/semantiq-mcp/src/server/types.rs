//! Input parameters and structured outputs of the MCP tools.
//!
//! Each output type is advertised as the tool's `outputSchema` and returned as
//! `structuredContent`; `render()` produces the markdown sent as text content
//! for clients that only read text.

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Search query: natural language, symbol name, or text pattern
    pub query: String,
    /// Maximum number of results (default 20, max 1000)
    pub limit: Option<usize>,
    /// Minimum score between 0.0 and 1.0 (default 0.3)
    pub min_score: Option<f32>,
    /// Comma-separated file extensions to keep, e.g. "rs,ts,py"
    pub file_type: Option<String>,
    /// Comma-separated symbol kinds to keep: function, method, class, struct,
    /// enum, interface, trait, module, variable, constant, type
    pub symbol_kind: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct FindRefsParams {
    /// Symbol name to look up
    pub symbol: String,
    /// Maximum number of references (default 50, max 1000)
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DepsParams {
    /// File path relative to the project root
    pub file_path: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ExplainParams {
    /// Symbol name to explain
    pub symbol: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchOutput {
    pub query: String,
    pub total_count: usize,
    pub search_time_ms: u64,
    pub results: Vec<SearchHit>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchHit {
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<String>,
    pub content: String,
}

impl SearchOutput {
    pub fn render(&self) -> String {
        let mut output = format!(
            "Found {} results for '{}' ({} ms)\n\n",
            self.total_count, self.query, self.search_time_ms
        );

        for hit in &self.results {
            output.push_str(&format!(
                "📄 {}\n   Lines {}-{} | Score: {:.2}\n",
                hit.file_path, hit.start_line, hit.end_line, hit.score
            ));

            if let Some(ref symbol_name) = hit.symbol_name {
                output.push_str(&format!(
                    "   Symbol: {} ({})\n",
                    symbol_name,
                    hit.symbol_kind.as_deref().unwrap_or("unknown")
                ));
            }

            let snippet: String = hit.content.chars().take(200).collect();
            output.push_str(&format!("   ```\n   {}\n   ```\n\n", snippet.trim()));
        }

        output
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FindRefsOutput {
    pub symbol: String,
    pub total_count: usize,
    pub search_time_ms: u64,
    pub definitions: Vec<Reference>,
    pub usages: Vec<Reference>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Reference {
    pub file_path: String,
    pub line: usize,
    pub content: String,
}

impl FindRefsOutput {
    /// Usages listed in the text output; the rest are only counted.
    const MAX_RENDERED_USAGES: usize = 20;

    pub fn render(&self) -> String {
        let mut output = format!(
            "Found {} references to '{}' ({} ms)\n\n",
            self.total_count, self.symbol, self.search_time_ms
        );

        if !self.definitions.is_empty() {
            output.push_str("## Definitions\n\n");
            for def in &self.definitions {
                output.push_str(&format!(
                    "📍 {}:{}\n   {}\n\n",
                    def.file_path,
                    def.line,
                    def.content.lines().next().unwrap_or("")
                ));
            }
        }

        if !self.usages.is_empty() {
            output.push_str(&format!("## Usages ({} found)\n\n", self.usages.len()));
            for usage in self.usages.iter().take(Self::MAX_RENDERED_USAGES) {
                output.push_str(&format!(
                    "📎 {}:{}\n   {}\n\n",
                    usage.file_path,
                    usage.line,
                    usage.content.trim()
                ));
            }

            if self.usages.len() > Self::MAX_RENDERED_USAGES {
                output.push_str(&format!(
                    "... and {} more usages\n",
                    self.usages.len() - Self::MAX_RENDERED_USAGES
                ));
            }
        }

        output
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DepsOutput {
    pub file_path: String,
    /// What this file imports; `None` if the lookup failed
    pub imports: Option<Vec<Import>>,
    /// Files importing this one; `None` if the lookup failed
    pub imported_by: Option<Vec<String>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Import {
    pub target_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_name: Option<String>,
    pub kind: String,
}

impl DepsOutput {
    pub fn render(&self) -> String {
        let mut output = format!("Dependency analysis for '{}'\n\n", self.file_path);

        match &self.imports {
            Some(imports) => {
                output.push_str(&format!("## Imports ({} dependencies)\n\n", imports.len()));
                for import in imports {
                    output.push_str(&format!("→ {}", import.target_path));
                    if let Some(ref name) = import.import_name {
                        output.push_str(&format!(" (as {})", name));
                    }
                    output.push_str(&format!(" [{}]\n", import.kind));
                }
                output.push('\n');
            }
            None => output.push_str("Could not analyze imports\n\n"),
        }

        match &self.imported_by {
            Some(dependents) => {
                output.push_str(&format!("## Imported by ({} files)\n\n", dependents.len()));
                for path in dependents {
                    output.push_str(&format!("← {}\n", path));
                }
            }
            None => output.push_str("Could not analyze dependents\n"),
        }

        output
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExplainOutput {
    pub symbol: String,
    pub found: bool,
    pub definitions: Vec<Definition>,
    pub usage_count: usize,
    pub related_symbols: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Definition {
    pub file_path: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_comment: Option<String>,
}

impl ExplainOutput {
    pub fn render(&self) -> String {
        if !self.found {
            return format!("Symbol '{}' not found in the index.", self.symbol);
        }

        let mut output = format!("# Symbol: {}\n\n", self.symbol);

        output.push_str(&format!(
            "Found {} definition(s), {} usage(s)\n\n",
            self.definitions.len(),
            self.usage_count
        ));

        for (i, def) in self.definitions.iter().enumerate() {
            output.push_str(&format!("## Definition {} ({})\n", i + 1, def.kind));
            output.push_str(&format!(
                "📄 {}:{}-{}\n\n",
                def.file_path, def.start_line, def.end_line
            ));

            if let Some(ref sig) = def.signature {
                output.push_str(&format!("```\n{}\n```\n\n", sig));
            }

            if let Some(ref doc) = def.doc_comment {
                output.push_str(&format!("**Documentation:**\n{}\n\n", doc));
            }
        }

        if !self.related_symbols.is_empty() {
            output.push_str("## Related Symbols\n\n");
            for related in &self.related_symbols {
                output.push_str(&format!("- {}\n", related));
            }
        }

        output
    }
}
