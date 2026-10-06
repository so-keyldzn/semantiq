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

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RepoMapParams {
    /// Token budget for the map (default 1500, clamped to 256..8000;
    /// estimated as characters / 4)
    pub max_tokens: Option<usize>,
    /// Files, directories or symbol names the current task is about: the map
    /// is then centered on them and on the code they use or are used by
    pub focus: Option<Vec<String>>,
    /// Only list files whose path starts with this prefix, e.g. "src/api/"
    pub path_prefix: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RepoMapOutput {
    pub max_tokens: usize,
    pub estimated_tokens: usize,
    /// Source files considered (after path_prefix)
    pub total_files: usize,
    /// Symbols considered (after path_prefix)
    pub total_symbols: usize,
    pub shown_symbols: usize,
    /// Focus entries resolved to files (including directories expanded)
    pub focus_files: Vec<String>,
    /// Focus entries resolved to symbol names
    pub focus_symbols: Vec<String>,
    /// Focus entries matching no indexed source file or symbol
    pub unmatched_focus: Vec<String>,
    /// Listed files in directory order, each with its most important symbols
    pub files: Vec<RepoMapFileOut>,
    /// Rendered map, sent as the text content
    #[serde(skip)]
    pub text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RepoMapFileOut {
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// PageRank of the file in the reference graph
    pub rank: f64,
    pub symbols: Vec<RepoMapSymbolOut>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RepoMapSymbolOut {
    pub name: String,
    pub kind: String,
    pub line: usize,
    /// Enclosing type or module, for members
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// First line of the declaration
    pub signature: String,
    /// First line of the doc comment, truncated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Importance score (share of rank received through references)
    pub rank: f64,
}

impl RepoMapOutput {
    pub fn render(&self) -> String {
        let mut output = self.text.clone();
        if !self.unmatched_focus.is_empty() {
            output.push_str(&format!(
                "\nNot found in the index (focus ignored): {}\n",
                self.unmatched_focus.join(", ")
            ));
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
    /// definition, call, type, import, reference — or text when the name is
    /// unknown to the AST index and was found by text search
    pub kind: String,
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
                    "📎 {}:{} [{}]\n   {}\n\n",
                    usage.file_path,
                    usage.line,
                    usage.kind,
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

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ImpactParams {
    /// Symbol about to change (function, method, type, constant…)
    pub symbol: String,
    /// Restrict to the definition in this file (relative path), when the name
    /// is defined in several places
    pub file_path: Option<String>,
    /// How many levels of callers to follow (default 2, max 4)
    pub max_depth: Option<usize>,
    /// Maximum number of impact sites (default 200, max 1000)
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImpactOutput {
    pub symbol: String,
    pub definitions: Vec<ImpactDefinitionOut>,
    pub site_count: usize,
    /// Impacted files, closest impact first
    pub files: Vec<ImpactedFile>,
    /// Impacted test files: the tests worth running after the change
    pub test_files: Vec<String>,
    /// The site limit was reached; the analysis is incomplete
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImpactDefinitionOut {
    pub file_path: String,
    pub line: usize,
    pub kind: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImpactedFile {
    pub file_path: String,
    pub is_test: bool,
    /// Smallest depth among this file's sites
    pub depth: usize,
    pub sites: Vec<ImpactSiteOut>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImpactSiteOut {
    pub line: usize,
    /// 1 = uses the symbol directly, 2 = uses a direct user, …
    pub depth: usize,
    /// Name referenced on this line
    pub target: String,
    /// call, type, import or reference
    pub kind: String,
    /// Function/type containing the line, itself impacted at the next depth
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing: Option<String>,
    /// same_file, imports, unique_name, or name_only (possibly a homonym)
    pub confidence: String,
}

impl ImpactOutput {
    pub fn render(&self) -> String {
        let mut output = format!("# Impact of '{}'\n\n", self.symbol);

        if self.definitions.is_empty() {
            output
                .push_str("No definition found in the index; sites below match the name only.\n\n");
        } else {
            for def in &self.definitions {
                output.push_str(&format!(
                    "Defined at {}:{} ({})\n",
                    def.file_path, def.line, def.kind
                ));
            }
            if self.definitions.len() > 1 {
                output.push_str(
                    "Several definitions share this name: pass file_path to analyse one.\n",
                );
            }
            output.push('\n');
        }

        output.push_str(&format!(
            "{} sites in {} files ({} test files){}\n\n",
            self.site_count,
            self.files.len(),
            self.test_files.len(),
            if self.truncated {
                " — limit reached, results incomplete"
            } else {
                ""
            }
        ));

        for file in &self.files {
            output.push_str(&format!(
                "## {}{} (depth {})\n",
                file.file_path,
                if file.is_test { " [test]" } else { "" },
                file.depth
            ));
            for site in &file.sites {
                output.push_str(&format!("  L{} [{}] {}", site.line, site.kind, site.target));
                if let Some(ref enclosing) = site.enclosing {
                    output.push_str(&format!(" in {}", enclosing));
                }
                if site.depth > 1 {
                    output.push_str(&format!(" (depth {})", site.depth));
                }
                if site.confidence == "name_only" {
                    output.push_str(" (name match only)");
                }
                output.push('\n');
            }
            output.push('\n');
        }

        if !self.test_files.is_empty() {
            output.push_str("## Tests to run\n\n");
            for path in &self.test_files {
                output.push_str(&format!("- {}\n", path));
            }
        }

        output
    }
}
