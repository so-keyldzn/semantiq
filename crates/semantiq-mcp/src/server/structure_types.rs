//! Parameters and structured outputs of the structural tools:
//! `semantiq_calls`, `semantiq_hierarchy` and `semantiq_dead_code`.

use super::{ImpactDefinitionOut, group_by, location, merge_lines, render_definitions};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CallsParams {
    /// Function or method name
    pub symbol: String,
    /// callers, callees or both (default)
    pub direction: Option<String>,
    /// Definition file, when several share the name
    pub file_path: Option<String>,
    /// Call levels to follow (default 1, max 3)
    pub max_depth: Option<usize>,
    /// Max call edges (default 100)
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CallsOutput {
    pub symbol: String,
    pub direction: String,
    pub definitions: Vec<ImpactDefinitionOut>,
    /// Call sites of the symbol (and of its callers, beyond depth 1)
    pub callers: Vec<CallEdgeOut>,
    /// Calls made by the symbol (and by its callees, beyond depth 1)
    pub callees: Vec<CallEdgeOut>,
    /// Names the symbol calls that the project does not define (libraries)
    pub external_callees: Vec<String>,
    /// The edge limit was reached; the graph is incomplete
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CallEdgeOut {
    /// 1 = involves the symbol directly, 2 = one hop further, …
    pub depth: usize,
    /// Function/method containing the call; absent for top-level code
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    pub callee: String,
    pub file_path: String,
    pub line: usize,
    /// Calls on this line, when more than one (`f(f(x))`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    /// same_file, imports, unique_name, or name_only (possibly a homonym)
    pub confidence: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct HierarchyParams {
    /// Type, class, interface or trait name
    pub symbol: String,
    /// Inheritance levels to follow (default 3, max 5)
    pub max_depth: Option<usize>,
    /// Max relations per direction (default 200)
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HierarchyOutput {
    pub symbol: String,
    pub definitions: Vec<ImpactDefinitionOut>,
    /// What the type extends / implements, transitively
    pub supertypes: Vec<TypeRelationOut>,
    /// What extends / implements the type, transitively
    pub subtypes: Vec<TypeRelationOut>,
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TypeRelationOut {
    /// 1 = direct relation, 2 = one hop further, …
    pub depth: usize,
    pub sub_type: String,
    pub super_type: String,
    /// extends or implements
    pub kind: String,
    /// Where the relation is declared (class header, impl block)
    pub file_path: String,
    pub line: usize,
    /// The type at the far end is defined in the project (false: library type)
    pub resolved: bool,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DeadCodeParams {
    /// Only files under this path prefix, e.g. "src/"
    pub path_prefix: Option<String>,
    /// Only this language, e.g. "rust"
    pub language: Option<String>,
    /// Also report public / exported symbols (default false)
    pub include_public: Option<bool>,
    /// Max symbols (default 100)
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeadCodeOutput {
    /// Unused symbols, most certain first
    pub symbols: Vec<DeadSymbolOut>,
    /// Unreferenced definitions examined before filtering
    pub candidates: usize,
    /// Candidates left out as alive by convention
    pub excluded: DeadCodeExcludedOut,
    pub truncated: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeadSymbolOut {
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// high, medium or low
    pub confidence: String,
    /// What lowers the confidence (empty when high)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeadCodeExcludedOut {
    /// main, constructors, dunder methods
    pub entry_points: usize,
    pub tests: usize,
    /// Public / exported (see include_public)
    pub public: usize,
    /// Trait / interface declarations, trait impls, overrides
    pub trait_members: usize,
}

fn confidence_note(confidence: &str) -> &'static str {
    if confidence == "name_only" {
        " (name match only)"
    } else {
        ""
    }
}

impl CallsOutput {
    pub fn render(&self) -> String {
        let mut output = format!("Calls of '{}'", self.symbol);
        if self.truncated {
            output.push_str(" (limit reached: incomplete, raise limit)");
        }
        output.push('\n');
        render_definitions(&mut output, &self.definitions);

        if self.direction != "callees" {
            self.render_edges(&mut output, "Callers", &self.callers, true);
        }
        if self.direction != "callers" {
            self.render_edges(&mut output, "Callees", &self.callees, false);
            if !self.external_callees.is_empty() {
                output.push_str(&format!(
                    "Library calls: {}\n",
                    self.external_callees.join(", ")
                ));
            }
        }

        output
    }

    /// Edges grouped by file, one line per call: `line caller → callee`. The
    /// queried symbol is implied (callers call it, callees are called by it),
    /// and repeated calls are merged into one line listing their lines.
    fn render_edges(&self, output: &mut String, title: &str, edges: &[CallEdgeOut], callers: bool) {
        if edges.is_empty() {
            output.push_str(&format!("{}: none\n", title));
            return;
        }
        output.push_str(&format!("{} ({}):\n", title, edges.len()));
        for (file, edges) in group_by(edges, |e| &e.file_path) {
            output.push_str(&format!("{}\n", file));
            let described = edges.into_iter().map(|edge| {
                let caller = edge.caller.as_deref().unwrap_or("<top level>");
                let mut text = if callers && edge.callee == self.symbol {
                    caller.to_string()
                } else if !callers && caller == self.symbol {
                    edge.callee.clone()
                } else {
                    format!("{} → {}", caller, edge.callee)
                };
                if let Some(count) = edge.count {
                    text.push_str(&format!(" ×{}", count));
                }
                if edge.depth > 1 {
                    text.push_str(&format!(" (depth {})", edge.depth));
                }
                text.push_str(confidence_note(&edge.confidence));
                (edge.line, text)
            });
            for (lines, text) in merge_lines(described) {
                output.push_str(&format!("  {} {}\n", lines, text));
            }
        }
    }
}

impl HierarchyOutput {
    pub fn render(&self) -> String {
        let mut output = format!("Type hierarchy of '{}'", self.symbol);
        if self.truncated {
            output.push_str(" (limit reached: incomplete, raise limit)");
        }
        output.push('\n');
        render_definitions(&mut output, &self.definitions);

        let sections = [
            ("Supertypes", &self.supertypes),
            ("Subtypes / implementors", &self.subtypes),
        ];
        for (title, edges) in sections {
            if edges.is_empty() {
                output.push_str(&format!("{}: none\n", title));
                continue;
            }
            output.push_str(&format!("{} ({}):\n", title, edges.len()));
            for edge in edges {
                output.push_str(&format!(
                    "{}{} {} {}  {}:{}{}\n",
                    "  ".repeat(edge.depth),
                    edge.sub_type,
                    edge.kind,
                    edge.super_type,
                    edge.file_path,
                    edge.line,
                    if edge.resolved { "" } else { " (library type)" }
                ));
            }
        }
        output
    }
}

impl DeadCodeOutput {
    pub fn render(&self) -> String {
        let mut output = format!(
            "Dead code: {} unreferenced symbols{} ({} candidates; excluded: {} entry points, {} tests, {} public, {} trait members)\n",
            self.symbols.len(),
            if self.truncated {
                ", limit reached: raise limit"
            } else {
                ""
            },
            self.candidates,
            self.excluded.entry_points,
            self.excluded.tests,
            self.excluded.public,
            self.excluded.trait_members
        );
        if self.symbols.is_empty() {
            output.push_str("No unused symbol found.\n");
        }
        for symbol in &self.symbols {
            output.push_str(&format!(
                "[{}] {} {}  {}",
                symbol.confidence,
                symbol.kind,
                symbol.name,
                location(&symbol.file_path, symbol.start_line, symbol.end_line)
            ));
            if !symbol.reasons.is_empty() {
                output.push_str(&format!(" — {}", symbol.reasons.join("; ")));
            }
            output.push('\n');
        }
        output
    }
}
