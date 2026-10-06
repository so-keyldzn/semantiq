//! Parameters and structured outputs of the structural tools:
//! `semantiq_calls`, `semantiq_hierarchy` and `semantiq_dead_code`.

use super::ImpactDefinitionOut;
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CallsParams {
    /// Function or method name
    pub symbol: String,
    /// callers (who calls it), callees (what it calls) or both (default both)
    pub direction: Option<String>,
    /// Restrict to the definition in this file (relative path), when the name
    /// is defined in several places
    pub file_path: Option<String>,
    /// How many call levels to follow (default 1, max 3)
    pub max_depth: Option<usize>,
    /// Maximum number of call edges (default 100, max 1000)
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
    /// same_file, imports, unique_name, or name_only (possibly a homonym)
    pub confidence: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct HierarchyParams {
    /// Type, class, interface or trait name
    pub symbol: String,
    /// How many inheritance levels to follow (default 3, max 5)
    pub max_depth: Option<usize>,
    /// Maximum number of relations per direction (default 200, max 1000)
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
    /// Only files whose relative path starts with this prefix, e.g. "src/"
    pub path_prefix: Option<String>,
    /// Only this language: rust, typescript, python, go, java, …
    pub language: Option<String>,
    /// Also report public / exported symbols (default false)
    pub include_public: Option<bool>,
    /// Maximum number of symbols (default 100, max 1000)
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
    /// Why it is reported, and what lowers the confidence
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

fn render_definitions(output: &mut String, definitions: &[ImpactDefinitionOut]) {
    if definitions.is_empty() {
        output.push_str("No definition found in the index; matching by name only.\n\n");
        return;
    }
    for def in definitions {
        output.push_str(&format!(
            "Defined at {}:{} ({})\n",
            def.file_path, def.line, def.kind
        ));
    }
    output.push('\n');
}

impl CallsOutput {
    pub fn render(&self) -> String {
        let mut output = format!("# Calls of '{}' ({})\n\n", self.symbol, self.direction);
        render_definitions(&mut output, &self.definitions);

        if self.direction != "callees" {
            output.push_str(&format!("## Callers ({})\n\n", self.callers.len()));
            if self.callers.is_empty() {
                output.push_str("No call site found.\n");
            }
            for edge in &self.callers {
                output.push_str(&format!(
                    "{}{} → {}  {}:{}{}\n",
                    "  ".repeat(edge.depth - 1),
                    edge.caller.as_deref().unwrap_or("<top level>"),
                    edge.callee,
                    edge.file_path,
                    edge.line,
                    confidence_note(&edge.confidence)
                ));
            }
            output.push('\n');
        }

        if self.direction != "callers" {
            output.push_str(&format!("## Callees ({})\n\n", self.callees.len()));
            if self.callees.is_empty() {
                output.push_str("No call to a project symbol.\n");
            }
            for edge in &self.callees {
                output.push_str(&format!(
                    "{}{} → {}  {}:{}{}\n",
                    "  ".repeat(edge.depth - 1),
                    edge.caller.as_deref().unwrap_or("<top level>"),
                    edge.callee,
                    edge.file_path,
                    edge.line,
                    confidence_note(&edge.confidence)
                ));
            }
            if !self.external_callees.is_empty() {
                output.push_str(&format!(
                    "\nLibrary calls: {}\n",
                    self.external_callees.join(", ")
                ));
            }
            output.push('\n');
        }

        if self.truncated {
            output.push_str("Limit reached: the graph is incomplete.\n");
        }
        output
    }
}

impl HierarchyOutput {
    pub fn render(&self) -> String {
        let mut output = format!("# Type hierarchy of '{}'\n\n", self.symbol);
        render_definitions(&mut output, &self.definitions);

        let sections = [
            ("Supertypes", &self.supertypes, true),
            ("Subtypes / implementors", &self.subtypes, false),
        ];
        for (title, edges, up) in sections {
            output.push_str(&format!("## {} ({})\n\n", title, edges.len()));
            if edges.is_empty() {
                output.push_str("None found.\n");
            }
            for edge in edges {
                let far = if up { &edge.super_type } else { &edge.sub_type };
                output.push_str(&format!(
                    "{}{} {} {}  {}:{}{}\n",
                    "  ".repeat(edge.depth - 1),
                    edge.sub_type,
                    edge.kind,
                    edge.super_type,
                    edge.file_path,
                    edge.line,
                    if edge.resolved {
                        String::new()
                    } else {
                        format!(" ({} not defined in the project)", far)
                    }
                ));
            }
            output.push('\n');
        }
        if self.truncated {
            output.push_str("Limit reached: the hierarchy is incomplete.\n");
        }
        output
    }
}

impl DeadCodeOutput {
    pub fn render(&self) -> String {
        let mut output = format!(
            "# Dead code: {} symbols ({} unreferenced candidates; excluded: {} entry points, {} tests, {} public, {} trait members)\n\n",
            self.symbols.len(),
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
                "- [{}] {} {} — {}:{}-{}\n    {}\n",
                symbol.confidence,
                symbol.kind,
                symbol.name,
                symbol.file_path,
                symbol.start_line,
                symbol.end_line,
                symbol.reasons.join("; ")
            ));
        }
        if self.truncated {
            output.push_str("\nLimit reached: more candidates exist.\n");
        }
        output
    }
}
