//! Call graph: who calls a function, and what it calls.
//!
//! Built from the `call_edges` table (each AST call site attached to its
//! enclosing function/method at index time). Callees are resolved by name, so
//! each edge carries a confidence; as in `analyze_impact`, only edges that are
//! not `name_only` are followed beyond depth 1. Calls to names the project
//! never defines (standard library, dependencies) are not listed as edges but
//! summarised in `external_callees`.

use super::RetrievalEngine;
use super::impact::{ImpactConfidence, ImpactDefinition};
use super::resolution::{DefLocation, Resolver};
use anyhow::Result;
use std::collections::{BTreeSet, HashSet, VecDeque};
use tracing::info;

pub const DEFAULT_CALL_DEPTH: usize = 1;
pub const MAX_CALL_DEPTH: usize = 3;
pub const DEFAULT_CALL_EDGES: usize = 100;
pub const MAX_CALL_EDGES: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDirection {
    Callers,
    Callees,
    Both,
}

impl CallDirection {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "callers" => Some(Self::Callers),
            "callees" => Some(Self::Callees),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    fn callers(self) -> bool {
        matches!(self, Self::Callers | Self::Both)
    }

    fn callees(self) -> bool {
        matches!(self, Self::Callees | Self::Both)
    }
}

/// One call site: `caller` calls `callee` at `file_path:line`.
#[derive(Debug, Clone)]
pub struct CallSite {
    /// 1 = involves the analysed symbol directly, 2 = one hop further…
    pub depth: usize,
    /// Enclosing function/method of the call; `None` for top-level code.
    pub caller: Option<String>,
    pub callee: String,
    pub file_path: String,
    pub line: usize,
    /// How sure we are that the call resolves to the definition followed.
    pub confidence: ImpactConfidence,
}

#[derive(Debug, Clone)]
pub struct CallGraph {
    pub symbol: String,
    pub definitions: Vec<ImpactDefinition>,
    pub callers: Vec<CallSite>,
    pub callees: Vec<CallSite>,
    /// Names called by the analysed symbol (depth 1) with no definition in
    /// the index: library / standard calls.
    pub external_callees: Vec<String>,
    /// The edge cap was reached; the graph is incomplete.
    pub truncated: bool,
}

struct Frontier {
    name: String,
    /// Definitions of `name` being followed (empty: unknown, match by name).
    locations: Vec<DefLocation>,
    depth: usize,
}

type Visited = HashSet<(String, i64, i64)>;

fn visit_key(name: &str, def: &DefLocation) -> (String, i64, i64) {
    (name.to_string(), def.file_id, def.line)
}

impl RetrievalEngine {
    /// Callers and/or callees of `symbol_name`, up to `max_depth` hops.
    ///
    /// `file_path` restricts the analysis to the definition(s) in that file.
    pub fn call_graph(
        &self,
        symbol_name: &str,
        file_path: Option<&str>,
        direction: CallDirection,
        max_depth: usize,
        max_edges: usize,
    ) -> Result<CallGraph> {
        info!(symbol = %symbol_name, ?file_path, ?direction, max_depth, max_edges, "Building call graph");
        let max_depth = max_depth.clamp(1, MAX_CALL_DEPTH);
        let max_edges = max_edges.clamp(1, MAX_CALL_EDGES);
        let mut resolver = Resolver::new(self);

        let roots: Vec<DefLocation> = resolver
            .definitions(symbol_name)?
            .iter()
            .filter(|d| file_path.is_none_or(|wanted| wanted == d.file_path))
            .cloned()
            .collect();

        let mut graph = CallGraph {
            symbol: symbol_name.to_string(),
            definitions: roots
                .iter()
                .map(|d| ImpactDefinition {
                    file_path: d.file_path.clone(),
                    line: d.line as usize,
                    kind: d.kind.clone(),
                })
                .collect(),
            callers: Vec::new(),
            callees: Vec::new(),
            external_callees: Vec::new(),
            truncated: false,
        };

        if direction.callers() {
            graph.truncated |= self.collect_callers(
                &mut resolver,
                symbol_name,
                &roots,
                max_depth,
                max_edges,
                &mut graph.callers,
            )?;
        }
        if direction.callees() {
            let budget = max_edges.saturating_sub(graph.callers.len()).max(1);
            let mut external = BTreeSet::new();
            graph.truncated |= self.collect_callees(
                &mut resolver,
                symbol_name,
                &roots,
                max_depth,
                budget,
                &mut graph.callees,
                &mut external,
            )?;
            graph.external_callees = external.into_iter().collect();
        }
        Ok(graph)
    }

    /// Breadth-first walk up the call graph. Returns whether the cap was hit.
    fn collect_callers(
        &self,
        resolver: &mut Resolver,
        symbol_name: &str,
        roots: &[DefLocation],
        max_depth: usize,
        max_edges: usize,
        sites: &mut Vec<CallSite>,
    ) -> Result<bool> {
        let mut visited: Visited = roots.iter().map(|d| visit_key(symbol_name, d)).collect();
        let mut queue = VecDeque::from([Frontier {
            name: symbol_name.to_string(),
            locations: roots.to_vec(),
            depth: 1,
        }]);

        while let Some(node) = queue.pop_front() {
            let edges = self
                .store
                .find_callers(&node.name, max_edges.saturating_mul(5))?;
            for edge in edges {
                let confidence = if node.locations.is_empty() {
                    ImpactConfidence::NameOnly
                } else {
                    resolver.confidence(edge.file_id, &node.name, &node.locations)?
                };
                let trusted = confidence > ImpactConfidence::NameOnly;
                // Below the first level an untrusted edge is almost always a
                // homonym: drop it rather than report noise.
                if node.depth > 1 && !trusted {
                    continue;
                }
                if sites.len() >= max_edges {
                    return Ok(true);
                }
                sites.push(CallSite {
                    depth: node.depth,
                    caller: (!edge.caller.is_empty()).then(|| edge.caller.clone()),
                    callee: node.name.clone(),
                    file_path: edge.file_path.clone(),
                    line: edge.line as usize,
                    confidence,
                });

                if trusted && node.depth < max_depth && !edge.caller.is_empty() {
                    let location = DefLocation {
                        file_id: edge.file_id,
                        file_path: edge.file_path,
                        line: edge.caller_line,
                        end_line: edge.caller_line,
                        kind: String::new(),
                        parent: None,
                    };
                    if visited.insert(visit_key(&edge.caller, &location)) {
                        queue.push_back(Frontier {
                            name: edge.caller,
                            locations: vec![location],
                            depth: node.depth + 1,
                        });
                    }
                }
            }
        }
        Ok(false)
    }

    /// Breadth-first walk down the call graph. Returns whether the cap was hit.
    #[allow(clippy::too_many_arguments)]
    fn collect_callees(
        &self,
        resolver: &mut Resolver,
        symbol_name: &str,
        roots: &[DefLocation],
        max_depth: usize,
        max_edges: usize,
        sites: &mut Vec<CallSite>,
        external: &mut BTreeSet<String>,
    ) -> Result<bool> {
        let mut visited: Visited = roots.iter().map(|d| visit_key(symbol_name, d)).collect();
        let mut queue = VecDeque::from([Frontier {
            name: symbol_name.to_string(),
            locations: roots.to_vec(),
            depth: 1,
        }]);

        while let Some(node) = queue.pop_front() {
            let edges = self
                .store
                .find_callees(&node.name, max_edges.saturating_mul(5))?;
            for edge in edges {
                // Only calls made from the definitions being followed, not
                // from homonyms elsewhere.
                if !node.locations.is_empty()
                    && !node
                        .locations
                        .iter()
                        .any(|l| l.file_id == edge.file_id && l.line == edge.caller_line)
                {
                    continue;
                }
                let targets = resolver.definitions(&edge.callee)?.to_vec();
                if targets.is_empty() {
                    if node.depth == 1 {
                        external.insert(edge.callee);
                    }
                    continue;
                }
                let confidence = resolver.confidence(edge.file_id, &edge.callee, &targets)?;
                let trusted = confidence > ImpactConfidence::NameOnly;
                if node.depth > 1 && !trusted {
                    continue;
                }
                if sites.len() >= max_edges {
                    return Ok(true);
                }
                sites.push(CallSite {
                    depth: node.depth,
                    caller: Some(node.name.clone()),
                    callee: edge.callee.clone(),
                    file_path: edge.file_path.clone(),
                    line: edge.line as usize,
                    confidence,
                });

                if trusted && node.depth < max_depth {
                    // Follow the definitions the confidence was based on.
                    let next: Vec<DefLocation> = targets
                        .into_iter()
                        .filter(|t| {
                            confidence != ImpactConfidence::SameFile || t.file_id == edge.file_id
                        })
                        .filter(|t| visited.insert(visit_key(&edge.callee, t)))
                        .collect();
                    if !next.is_empty() {
                        queue.push_back(Frontier {
                            name: edge.callee,
                            locations: next,
                            depth: node.depth + 1,
                        });
                    }
                }
            }
        }
        Ok(false)
    }
}
