//! Change-impact analysis: what may break if a symbol changes.
//!
//! Starting from a symbol's definitions, every AST reference to it is a direct
//! impact site. The symbol enclosing each site (the calling function, the
//! struct using a type…) is impacted in turn, and its own references are
//! followed, breadth-first, up to `max_depth`.
//!
//! References are matched by name, so each site carries a confidence level.
//! Only `SameFile` / `Imports` / `UniqueName` sites propagate further:
//! following name-only matches of common names (`new`, `get`) would drag in
//! most of the codebase. For the same reason, beyond depth 1 a function or
//! method is only followed through calls: a plain reference to its name there
//! is usually a local variable or module that happens to share it.

use super::RetrievalEngine;
use anyhow::Result;
use semantiq_index::{DependencyRecord, SymbolRecord};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use tracing::info;

pub const DEFAULT_IMPACT_DEPTH: usize = 2;
pub const MAX_IMPACT_DEPTH: usize = 4;
pub const DEFAULT_IMPACT_SITES: usize = 200;
pub const MAX_IMPACT_SITES: usize = 1000;

/// How sure we are that a reference points at the analysed definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImpactConfidence {
    /// Only the name matches; may be a homonym defined elsewhere.
    NameOnly,
    /// Only the name matches, but the project defines this name once.
    UniqueName,
    /// The referencing file imports the definition's file, or imports the name.
    Imports,
    /// The reference is in the file that defines the symbol.
    SameFile,
}

impl ImpactConfidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            ImpactConfidence::NameOnly => "name_only",
            ImpactConfidence::UniqueName => "unique_name",
            ImpactConfidence::Imports => "imports",
            ImpactConfidence::SameFile => "same_file",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImpactDefinition {
    pub file_path: String,
    pub line: usize,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct EnclosingSymbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct ImpactSite {
    /// 1 = uses the analysed symbol directly; 2 = uses a symbol from depth 1…
    pub depth: usize,
    /// Name referenced at this site (the analysed symbol at depth 1).
    pub target: String,
    pub file_path: String,
    pub line: usize,
    /// call, type, import or reference
    pub kind: String,
    pub enclosing: Option<EnclosingSymbol>,
    pub confidence: ImpactConfidence,
    pub is_test: bool,
}

#[derive(Debug, Clone)]
pub struct ImpactAnalysis {
    pub symbol: String,
    pub definitions: Vec<ImpactDefinition>,
    pub sites: Vec<ImpactSite>,
    /// The site cap was reached; deeper or later sites were not explored.
    pub truncated: bool,
}

/// Symbol kinds whose users are impacted when the symbol's body changes.
/// Modules and imports span too much to be meaningful callers.
fn propagates(kind: &str) -> bool {
    !matches!(kind, "module" | "import")
}

/// Heuristic test detection from the path and the enclosing symbol name.
pub fn is_test_location(file_path: &str, enclosing: Option<&str>) -> bool {
    let path = file_path.replace('\\', "/").to_lowercase();
    let file_name = path.rsplit('/').next().unwrap_or(&path);
    path.contains("/tests/")
        || path.starts_with("tests/")
        || path.contains("/test/")
        || path.contains("__tests__")
        || file_name.starts_with("test_")
        || file_name.contains("_test.")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
        || file_name == "tests.rs"
        || file_name.ends_with("test.java")
        || file_name.ends_with("tests.cs")
        || enclosing.is_some_and(|name| name.starts_with("test_") || name.starts_with("Test"))
}

/// Last segment of an import path: `a::b::Foo`, `a.b.Foo`, `./a/Foo` → `Foo`.
pub(super) fn last_segment(path: &str) -> &str {
    path.rsplit(|c| [':', '.', '/', '\\'].contains(&c))
        .next()
        .unwrap_or(path)
}

/// A symbol to analyse: its name and the files that define it.
struct Node {
    name: String,
    /// (file_id, file_path) of each definition considered.
    definitions: Vec<(i64, String)>,
    depth: usize,
    /// Function or method: only reachable through calls (see module docs).
    callable: bool,
}

fn is_callable(kind: &str) -> bool {
    matches!(kind, "function" | "method")
}

/// Per-call caches so each file's symbols / imports are loaded once.
#[derive(Default)]
struct Caches {
    symbols: HashMap<i64, Vec<SymbolRecord>>,
    dependencies: HashMap<i64, Vec<DependencyRecord>>,
    definition_counts: HashMap<String, usize>,
}

impl RetrievalEngine {
    /// Analyse what is impacted by a change to `symbol_name`.
    ///
    /// `file_path` restricts the analysis to the definition(s) in that file,
    /// for names defined in several places.
    pub fn analyze_impact(
        &self,
        symbol_name: &str,
        file_path: Option<&str>,
        max_depth: usize,
        max_sites: usize,
    ) -> Result<ImpactAnalysis> {
        info!(symbol = %symbol_name, ?file_path, max_depth, max_sites, "Analyzing impact");
        let max_depth = max_depth.clamp(1, MAX_IMPACT_DEPTH);
        let max_sites = max_sites.clamp(1, MAX_IMPACT_SITES);
        let mut caches = Caches::default();

        let mut definitions: Vec<ImpactDefinition> = Vec::new();
        let mut root_files = Vec::new();
        for symbol in self.store.find_symbol_by_name(symbol_name)? {
            if symbol.kind == "import" {
                continue;
            }
            let path = self.get_file_path(symbol.file_id)?;
            if file_path.is_some_and(|wanted| wanted != path) {
                continue;
            }
            definitions.push(ImpactDefinition {
                file_path: path.clone(),
                line: symbol.start_line as usize,
                kind: symbol.kind.clone(),
            });
            if !root_files.iter().any(|(id, _)| *id == symbol.file_id) {
                root_files.push((symbol.file_id, path));
            }
        }

        let mut sites = Vec::new();
        let mut truncated = false;
        let mut visited: HashSet<(String, i64)> = HashSet::new();
        let callable = !definitions.is_empty() && definitions.iter().all(|d| is_callable(&d.kind));
        let mut queue = VecDeque::from([Node {
            name: symbol_name.to_string(),
            definitions: root_files,
            depth: 1,
            callable,
        }]);

        'bfs: while let Some(node) = queue.pop_front() {
            let unique_name = self.definition_count(&mut caches, &node.name)? <= 1;
            let references = self
                .store
                .find_references_by_name(&node.name, max_sites.saturating_mul(5))?;

            for reference in references {
                if reference.kind == "definition"
                    || (node.callable && node.depth > 1 && reference.kind != "call")
                {
                    continue;
                }

                let mut confidence = self.confidence(&mut caches, &node, &reference)?;
                if confidence == ImpactConfidence::NameOnly && unique_name {
                    confidence = ImpactConfidence::UniqueName;
                }
                let trusted = confidence > ImpactConfidence::NameOnly;
                // Below the first level, an untrusted site is almost always a
                // homonym of the caller name: drop it rather than report noise.
                if node.depth > 1 && !trusted {
                    continue;
                }

                let line = reference.line as usize;
                let enclosing = self
                    .enclosing_symbol(&mut caches, reference.file_id, line)?
                    .filter(|s| s.name != node.name);

                if sites.len() >= max_sites {
                    truncated = true;
                    break 'bfs;
                }

                let is_test = is_test_location(
                    &reference.file_path,
                    enclosing.as_ref().map(|e| e.name.as_str()),
                );
                sites.push(ImpactSite {
                    depth: node.depth,
                    target: node.name.clone(),
                    file_path: reference.file_path.clone(),
                    line,
                    kind: reference.kind.clone(),
                    enclosing: enclosing.clone(),
                    confidence,
                    is_test,
                });

                if let Some(enclosing) = enclosing
                    && trusted
                    && node.depth < max_depth
                    && visited.insert((enclosing.name.clone(), reference.file_id))
                {
                    queue.push_back(Node {
                        callable: is_callable(&enclosing.kind),
                        name: enclosing.name,
                        definitions: vec![(reference.file_id, reference.file_path)],
                        depth: node.depth + 1,
                    });
                }
            }
        }

        Ok(ImpactAnalysis {
            symbol: symbol_name.to_string(),
            definitions,
            sites,
            truncated,
        })
    }

    fn definition_count(&self, caches: &mut Caches, name: &str) -> Result<usize> {
        if let Some(count) = caches.definition_counts.get(name) {
            return Ok(*count);
        }
        let count = self
            .store
            .find_symbol_by_name(name)?
            .iter()
            .filter(|s| s.kind != "import")
            .count();
        caches.definition_counts.insert(name.to_string(), count);
        Ok(count)
    }

    fn confidence(
        &self,
        caches: &mut Caches,
        node: &Node,
        reference: &semantiq_index::ReferenceRecord,
    ) -> Result<ImpactConfidence> {
        if node
            .definitions
            .iter()
            .any(|(id, _)| *id == reference.file_id)
        {
            return Ok(ImpactConfidence::SameFile);
        }

        let deps = match caches.dependencies.entry(reference.file_id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(self.store.get_dependencies(reference.file_id)?),
        };
        let imports = deps.iter().any(|dep| {
            dep.resolved_path
                .as_ref()
                .is_some_and(|resolved| node.definitions.iter().any(|(_, path)| path == resolved))
                || dep.import_name.as_deref() == Some(node.name.as_str())
                || last_segment(&dep.target_path) == node.name
        });

        Ok(if imports {
            ImpactConfidence::Imports
        } else {
            ImpactConfidence::NameOnly
        })
    }

    /// Innermost propagating symbol whose span contains `line`.
    fn enclosing_symbol(
        &self,
        caches: &mut Caches,
        file_id: i64,
        line: usize,
    ) -> Result<Option<EnclosingSymbol>> {
        let symbols = match caches.symbols.entry(file_id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(self.store.get_symbols_by_file(file_id)?),
        };
        let line = line as i64;
        Ok(symbols
            .iter()
            .filter(|s| propagates(&s.kind) && s.start_line <= line && line <= s.end_line)
            .min_by_key(|s| s.end_line - s.start_line)
            .map(|s| EnclosingSymbol {
                name: s.name.clone(),
                kind: s.kind.clone(),
                line: s.start_line as usize,
            }))
    }
}
