//! Input parameters and structured outputs of the MCP tools.
//!
//! Each output type is advertised as the tool's `outputSchema` and returned as
//! `structuredContent`; `render()` produces the compact text sent as text
//! content (and printed by the CLI): one line per result, paths written once.

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Longest line kept in previews and usage lines, in characters.
pub(crate) const MAX_LINE_CHARS: usize = 120;

/// Trim a source line and cut it to `MAX_LINE_CHARS` characters.
pub(crate) fn clip_line(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() <= MAX_LINE_CHARS {
        line.to_string()
    } else {
        let mut out: String = line.chars().take(MAX_LINE_CHARS - 1).collect();
        out.push('…');
        out
    }
}

/// `1 definition`, `3 definitions`.
pub(crate) fn count(n: usize, noun: &str) -> String {
    format!("{} {}{}", n, noun, if n == 1 { "" } else { "s" })
}

/// `path:start-end`, or `path:line` for a single line.
pub(crate) fn location(file_path: &str, start_line: usize, end_line: usize) -> String {
    if end_line > start_line {
        format!("{}:{}-{}", file_path, start_line, end_line)
    } else {
        format!("{}:{}", file_path, start_line)
    }
}

/// Merge entries with the same description into one `l1,l2,l3 description`
/// line, in order of first appearance.
pub(crate) fn merge_lines(
    entries: impl IntoIterator<Item = (usize, String)>,
) -> Vec<(String, String)> {
    let mut merged: Vec<(Vec<String>, String)> = Vec::new();
    for (line, text) in entries {
        match merged.iter_mut().find(|(_, t)| *t == text) {
            Some((lines, _)) => lines.push(line.to_string()),
            None => merged.push((vec![line.to_string()], text)),
        }
    }
    merged
        .into_iter()
        .map(|(lines, text)| (lines.join(","), text))
        .collect()
}

/// `a::X, a::Y, b` → `a::{X, Y}, b`: Rust-style paths sharing a module are
/// written once.
pub(crate) fn join_paths(paths: &[String]) -> String {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for path in paths {
        let (prefix, name) = match path.rsplit_once("::") {
            Some((prefix, name)) if !name.starts_with('{') => (prefix, name),
            _ => ("", path.as_str()),
        };
        match groups
            .iter_mut()
            .find(|(p, _)| !p.is_empty() && *p == prefix)
        {
            Some((_, names)) => names.push(name),
            None => groups.push((prefix, vec![name])),
        }
    }
    groups
        .into_iter()
        .map(|(prefix, names)| match (prefix, names.as_slice()) {
            ("", _) => names.join(", "),
            (_, [name]) => format!("{}::{}", prefix, name),
            _ => format!("{}::{{{}}}", prefix, names.join(", ")),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Group items by a key (file path, import kind…), keeping the order of
/// first appearance.
pub(crate) fn group_by<'a, T>(
    items: impl IntoIterator<Item = &'a T>,
    key: impl Fn(&T) -> &str,
) -> Vec<(&'a str, Vec<&'a T>)>
where
    T: 'a,
{
    let mut groups: Vec<(&str, Vec<&T>)> = Vec::new();
    for item in items {
        let file = key(item);
        match groups.iter_mut().find(|(f, _)| *f == file) {
            Some((_, list)) => list.push(item),
            None => groups.push((file, vec![item])),
        }
    }
    groups
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// What the code does, or a symbol name
    pub query: String,
    /// Max results (default 10)
    pub limit: Option<usize>,
    /// Min score 0-1 (default 0.3)
    pub min_score: Option<f32>,
    /// File extensions, e.g. "rs,ts"
    pub file_type: Option<String>,
    /// Symbol kinds, e.g. "function,struct"
    pub symbol_kind: Option<String>,
    /// Include each hit's code
    pub snippets: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct FindRefsParams {
    /// Symbol name
    pub symbol: String,
    /// Max references (default 30)
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DepsParams {
    /// File path relative to the project root
    pub file_path: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ExplainParams {
    /// Symbol name
    pub symbol: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchOutput {
    pub query: String,
    pub results: Vec<SearchHit>,
    /// More results exist beyond `limit`
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchHit {
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    /// Relative to this query, rounded to 2 decimals (f64 so that JSON
    /// prints 0.95, not the f32 widening 0.949999988)
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<String>,
    /// The most relevant line of the hit, trimmed and cut to 120 characters
    pub preview: String,
    /// Code of the hit, only with `snippets`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl SearchOutput {
    pub fn render(&self) -> String {
        if self.results.is_empty() {
            return format!(
                "No results for '{}'. Rephrase as what the code does, or use grep for exact text.\n",
                self.query
            );
        }

        let mut output = format!(
            "{} for '{}'",
            count(self.results.len(), "result"),
            self.query
        );
        if self.truncated {
            output.push_str(" (more exist: raise limit)");
        }
        output.push('\n');

        for hit in &self.results {
            output.push_str(&location(&hit.file_path, hit.start_line, hit.end_line));
            if let Some(ref name) = hit.symbol_name {
                output.push_str(&format!(
                    " {} {}",
                    hit.symbol_kind.as_deref().unwrap_or("symbol"),
                    name
                ));
            }
            output.push_str(&format!(" ({:.2})\n", hit.score));
            match hit.content {
                Some(ref content) => {
                    output.push_str(&format!("```\n{}\n```\n", content.trim_end()));
                }
                None if !hit.preview.is_empty() => {
                    output.push_str(&format!("  {}\n", hit.preview));
                }
                None => {}
            }
        }

        output
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RepoMapParams {
    /// Token budget (default 1500, 256-8000)
    pub max_tokens: Option<usize>,
    /// Files, directories or symbols to center the map on
    pub focus: Option<Vec<String>>,
    /// Only files under this path prefix, e.g. "src/api/"
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
    /// PageRank of the file in the reference graph (4 significant digits)
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
    pub definitions: Vec<Reference>,
    pub usages: Vec<Reference>,
    /// The limit was reached: more references exist
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Reference {
    pub file_path: String,
    pub line: usize,
    /// Usages: call, type, import, reference, or text (found by text search).
    /// Definitions: the symbol kind (function, struct…) or definition
    pub kind: String,
    /// The line, trimmed and cut to 120 characters
    pub content: String,
}

impl FindRefsOutput {
    pub fn render(&self) -> String {
        if self.definitions.is_empty() && self.usages.is_empty() {
            return format!("No references to '{}'.\n", self.symbol);
        }

        let mut output = format!(
            "'{}': {}, {}",
            self.symbol,
            count(self.definitions.len(), "definition"),
            count(self.usages.len(), "usage")
        );
        if self.truncated {
            output.push_str(" (limit reached: raise limit)");
        }
        output.push('\n');

        if !self.definitions.is_empty() {
            output.push_str("Definitions:\n");
            for def in &self.definitions {
                output.push_str(&format!(
                    "  {}:{} {}  {}\n",
                    def.file_path, def.line, def.kind, def.content
                ));
            }
        }

        if !self.usages.is_empty() {
            output.push_str("Usages:\n");
            for (file, usages) in group_by(&self.usages, |u| &u.file_path) {
                output.push_str(&format!("{}\n", file));
                for usage in usages {
                    output.push_str(&format!(
                        "  {} {}  {}\n",
                        usage.line, usage.kind, usage.content
                    ));
                }
            }
        }

        output
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DepsOutput {
    pub file_path: String,
    /// What this file imports; absent if the lookup failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imports: Option<Vec<Import>>,
    /// Files importing this one, sorted, without duplicates; absent if the
    /// lookup failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imported_by: Option<Vec<String>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Import {
    pub target_path: String,
    /// Imported name, omitted when `target_path` already ends with it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_name: Option<String>,
    /// local, external or std
    pub kind: String,
    /// First and last line of the import statement (1-based)
    pub line: usize,
    pub end_line: usize,
}

impl DepsOutput {
    pub fn render(&self) -> String {
        let mut output = format!("{}\n", self.file_path);

        match &self.imports {
            Some(imports) => {
                output.push_str(&format!("Imports ({}):\n", imports.len()));
                // One line per kind; within it, the imports of each statement
                // line are grouped, tagged with that line and separated by
                // `;` so the tag reads for the whole group:
                // `local: a, b (L3); c (L5)`.
                for (kind, group) in group_by(imports, |i| &i.kind) {
                    let mut by_line: BTreeMap<(usize, usize), Vec<String>> = BTreeMap::new();
                    for i in group {
                        let target = match i.import_name {
                            Some(ref name) => format!("{} as {}", i.target_path, name),
                            None => i.target_path.clone(),
                        };
                        by_line
                            .entry((i.line, i.end_line.max(i.line)))
                            .or_default()
                            .push(target);
                    }
                    let described: Vec<String> = by_line
                        .iter()
                        .map(|((line, end_line), targets)| {
                            if end_line > line {
                                format!("{} (L{}-{})", join_paths(targets), line, end_line)
                            } else {
                                format!("{} (L{})", join_paths(targets), line)
                            }
                        })
                        .collect();
                    output.push_str(&format!("  {}: {}\n", kind, described.join("; ")));
                }
            }
            None => output.push_str("Imports: lookup failed\n"),
        }

        match &self.imported_by {
            Some(dependents) if dependents.is_empty() => output.push_str("Imported by: none\n"),
            Some(dependents) => output.push_str(&format!(
                "Imported by ({}): {}\n",
                dependents.len(),
                dependents.join(", ")
            )),
            None => output.push_str("Imported by: lookup failed\n"),
        }

        output
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExplainOutput {
    pub symbol: String,
    pub found: bool,
    /// Definitions, import statements excluded
    pub definitions: Vec<Definition>,
    /// Import statements of the name, as `path:line`
    pub imported_in: Vec<String>,
    /// Non-definition occurrences of the name (a line using it twice counts 2)
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
            return format!(
                "Symbol '{}' not found in the index: try semantiq_search.\n",
                self.symbol
            );
        }

        let mut output = format!(
            "{}: {}, {}, {}\n",
            self.symbol,
            count(self.definitions.len(), "definition"),
            count(self.imported_in.len(), "import"),
            count(self.usage_count, "usage")
        );

        for def in &self.definitions {
            output.push_str(&format!(
                "{} {}\n",
                def.kind,
                location(&def.file_path, def.start_line, def.end_line)
            ));
            if let Some(ref doc) = def.doc_comment {
                for line in doc.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    output.push_str(&format!("  {}\n", line));
                }
            }
            if let Some(ref sig) = def.signature {
                output.push_str(&format!("  {}\n", sig.trim()));
            }
        }

        if !self.imported_in.is_empty() {
            output.push_str(&format!(
                "Imported in ({}): {}\n",
                self.imported_in.len(),
                self.imported_in.join(", ")
            ));
        }

        if !self.related_symbols.is_empty() {
            output.push_str(&format!("Related: {}\n", self.related_symbols.join(", ")));
        }

        output
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ImpactParams {
    /// Symbol about to change
    pub symbol: String,
    /// Definition file, when several share the name
    pub file_path: Option<String>,
    /// Levels of users to follow (default 2, max 4)
    pub max_depth: Option<usize>,
    /// Max impact sites (default 200)
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

/// One line per definition: `kind path:line`.
pub(crate) fn render_definitions(output: &mut String, definitions: &[ImpactDefinitionOut]) {
    if definitions.is_empty() {
        output.push_str("No definition in the index: matched by name only.\n");
        return;
    }
    let defs: Vec<String> = definitions
        .iter()
        .map(|d| format!("{} {}:{}", d.kind, d.file_path, d.line))
        .collect();
    output.push_str(&format!("Defined: {}\n", defs.join(", ")));
    if definitions.len() > 1 {
        output.push_str("Several definitions share this name: pass file_path to pick one.\n");
    }
}

impl ImpactOutput {
    pub fn render(&self) -> String {
        let mut output = format!(
            "Impact of '{}': {} in {} ({}){}\n",
            self.symbol,
            count(self.site_count, "site"),
            count(self.files.len(), "file"),
            count(self.test_files.len(), "test file"),
            if self.truncated {
                " (limit reached: incomplete, raise limit)"
            } else {
                ""
            }
        );
        render_definitions(&mut output, &self.definitions);

        for file in &self.files {
            output.push_str(&format!(
                "{}{} (depth {})\n",
                file.file_path,
                if file.is_test { " [test]" } else { "" },
                file.depth
            ));
            let described = file.sites.iter().map(|site| {
                let mut text = site.kind.clone();
                if site.target != self.symbol {
                    text.push_str(&format!(" {}", site.target));
                }
                if let Some(ref enclosing) = site.enclosing {
                    text.push_str(&format!(" in {}", enclosing));
                }
                if site.depth > file.depth {
                    text.push_str(&format!(" (depth {})", site.depth));
                }
                if site.confidence == "name_only" {
                    text.push_str(" (name match only)");
                }
                (site.line, text)
            });
            for (lines, text) in merge_lines(described) {
                output.push_str(&format!("  {} {}\n", lines, text));
            }
        }

        if !self.test_files.is_empty() {
            output.push_str(&format!("Tests to run: {}\n", self.test_files.join(", ")));
        }

        output
    }
}
