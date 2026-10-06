//! Name-based resolution shared by the call graph, hierarchy and dead-code
//! analyses: where a name is defined, and how sure we are that a site in a
//! given file refers to one of those definitions.

use super::RetrievalEngine;
use super::impact::{ImpactConfidence, last_segment};
use anyhow::Result;
use semantiq_index::{DependencyRecord, SymbolRecord};
use std::collections::HashMap;

/// One definition of a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DefLocation {
    pub file_id: i64,
    pub file_path: String,
    pub line: i64,
    pub end_line: i64,
    pub kind: String,
    /// Enclosing type / module path (`Calculator`, `a::B`)
    pub parent: Option<String>,
}

/// Per-call caches so each name / file is looked up once.
pub(super) struct Resolver<'a> {
    engine: &'a RetrievalEngine,
    definitions: HashMap<String, Vec<DefLocation>>,
    dependencies: HashMap<i64, Vec<DependencyRecord>>,
    file_symbols: HashMap<i64, Vec<SymbolRecord>>,
}

impl<'a> Resolver<'a> {
    pub fn new(engine: &'a RetrievalEngine) -> Self {
        Self {
            engine,
            definitions: HashMap::new(),
            dependencies: HashMap::new(),
            file_symbols: HashMap::new(),
        }
    }

    /// Non-import definitions of `name`.
    pub fn definitions(&mut self, name: &str) -> Result<&[DefLocation]> {
        if !self.definitions.contains_key(name) {
            let mut defs = Vec::new();
            for symbol in self.engine.store.find_symbol_by_name(name)? {
                if symbol.kind == "import" {
                    continue;
                }
                defs.push(DefLocation {
                    file_path: self.engine.get_file_path(symbol.file_id)?,
                    file_id: symbol.file_id,
                    line: symbol.start_line,
                    end_line: symbol.end_line,
                    kind: symbol.kind,
                    parent: symbol.parent,
                });
            }
            defs.sort_by(|a, b| a.file_path.cmp(&b.file_path).then(a.line.cmp(&b.line)));
            self.definitions.insert(name.to_string(), defs);
        }
        Ok(&self.definitions[name])
    }

    /// Symbols of a file, ordered by start line.
    pub fn file_symbols(&mut self, file_id: i64) -> Result<&[SymbolRecord]> {
        if !self.file_symbols.contains_key(&file_id) {
            let symbols = self.engine.store.get_symbols_by_file(file_id)?;
            self.file_symbols.insert(file_id, symbols);
        }
        Ok(&self.file_symbols[&file_id])
    }

    /// How sure we are that a use of `name` in `from_file` targets one of
    /// `targets` (same scale as `semantiq_impact`).
    pub fn confidence(
        &mut self,
        from_file: i64,
        name: &str,
        targets: &[DefLocation],
    ) -> Result<ImpactConfidence> {
        if targets.iter().any(|t| t.file_id == from_file) {
            return Ok(ImpactConfidence::SameFile);
        }
        if !self.dependencies.contains_key(&from_file) {
            let deps = self.engine.store.get_dependencies(from_file)?;
            self.dependencies.insert(from_file, deps);
        }
        let imports = self.dependencies[&from_file].iter().any(|dep| {
            dep.resolved_path
                .as_ref()
                .is_some_and(|resolved| targets.iter().any(|t| &t.file_path == resolved))
                || dep.import_name.as_deref() == Some(name)
                || last_segment(&dep.target_path) == name
        });
        if imports {
            return Ok(ImpactConfidence::Imports);
        }
        Ok(if self.definitions(name)?.len() == 1 {
            ImpactConfidence::UniqueName
        } else {
            ImpactConfidence::NameOnly
        })
    }
}
