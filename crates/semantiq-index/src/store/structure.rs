//! Call graph and type hierarchy operations for IndexStore.

use super::IndexStore;
use crate::schema::{CallEdgeRecord, SymbolRecord, TypeRelationRecord, UnreferencedSymbol};
use anyhow::Result;
use rusqlite::{Row, params};
use semantiq_parser::FileStructure;
use tracing::debug;

/// Symbol kinds considered by dead-code detection: callables and types.
const DEAD_CODE_KINDS: &str =
    "'function', 'method', 'class', 'struct', 'enum', 'interface', 'trait', 'type'";

impl IndexStore {
    /// Maximum number of call edges / relations returned by a single lookup.
    const MAX_STRUCTURE_LIMIT: usize = 10000;

    /// Insert the call edges and type relations of a file (replaces existing ones).
    pub fn insert_structure(&self, file_id: i64, structure: &FileStructure) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", [])?;

            let result = (|| -> Result<()> {
                conn.execute("DELETE FROM call_edges WHERE file_id = ?1", [file_id])?;
                conn.execute("DELETE FROM type_relations WHERE file_id = ?1", [file_id])?;

                let mut stmt = conn.prepare(
                    "INSERT OR IGNORE INTO call_edges (file_id, line, callee, caller, caller_line)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;
                for edge in &structure.calls {
                    stmt.execute(params![
                        file_id,
                        edge.line as i64,
                        edge.callee,
                        edge.caller,
                        edge.caller_line as i64,
                    ])?;
                }

                let mut stmt = conn.prepare(
                    "INSERT OR IGNORE INTO type_relations
                        (file_id, line, type_name, super_name, kind, end_line)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?;
                for relation in &structure.relations {
                    stmt.execute(params![
                        file_id,
                        relation.line as i64,
                        relation.type_name,
                        relation.super_name,
                        relation.kind.as_str(),
                        relation.end_line as i64,
                    ])?;
                }
                Ok(())
            })();

            match result {
                Ok(()) => {
                    conn.execute("COMMIT", [])?;
                    debug!(
                        "Inserted {} call edges and {} type relations for file_id {}",
                        structure.calls.len(),
                        structure.relations.len(),
                        file_id
                    );
                    Ok(())
                }
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", []);
                    Err(e)
                }
            }
        })
    }

    /// Call sites of `callee`, ordered by file path then line.
    pub fn find_callers(&self, callee: &str, limit: usize) -> Result<Vec<CallEdgeRecord>> {
        self.query_call_edges("e.callee = ?1", callee, limit)
    }

    /// Calls made from inside any function/method named `caller`.
    pub fn find_callees(&self, caller: &str, limit: usize) -> Result<Vec<CallEdgeRecord>> {
        self.query_call_edges("e.caller = ?1", caller, limit)
    }

    fn query_call_edges(
        &self,
        filter: &str,
        name: &str,
        limit: usize,
    ) -> Result<Vec<CallEdgeRecord>> {
        let safe_limit = limit.min(Self::MAX_STRUCTURE_LIMIT);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT e.file_id, f.path, e.line, e.callee, e.caller, e.caller_line
                 FROM call_edges e
                 JOIN files f ON f.id = e.file_id
                 WHERE {filter}
                 ORDER BY f.path, e.line
                 LIMIT ?2"
            ))?;
            let records = stmt
                .query_map(params![name, safe_limit as i64], |row| {
                    Ok(CallEdgeRecord {
                        file_id: row.get(0)?,
                        file_path: row.get(1)?,
                        line: row.get(2)?,
                        callee: row.get(3)?,
                        caller: row.get(4)?,
                        caller_line: row.get(5)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(records)
        })
    }

    /// Declarations where `type_name` extends / implements something.
    pub fn find_supertypes(
        &self,
        type_name: &str,
        limit: usize,
    ) -> Result<Vec<TypeRelationRecord>> {
        self.query_type_relations("t.type_name = ?1", type_name, limit)
    }

    /// Declarations where something extends / implements `super_name`.
    pub fn find_subtypes(&self, super_name: &str, limit: usize) -> Result<Vec<TypeRelationRecord>> {
        self.query_type_relations("t.super_name = ?1", super_name, limit)
    }

    /// Every type relation declared in a file, ordered by line.
    pub fn get_type_relations_by_file(&self, file_id: i64) -> Result<Vec<TypeRelationRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT t.file_id, f.path, t.line, t.end_line, t.type_name, t.super_name, t.kind
                 FROM type_relations t
                 JOIN files f ON f.id = t.file_id
                 WHERE t.file_id = ?1
                 ORDER BY t.line",
            )?;
            let records = stmt
                .query_map([file_id], type_relation_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(records)
        })
    }

    fn query_type_relations(
        &self,
        filter: &str,
        name: &str,
        limit: usize,
    ) -> Result<Vec<TypeRelationRecord>> {
        let safe_limit = limit.min(Self::MAX_STRUCTURE_LIMIT);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT t.file_id, f.path, t.line, t.end_line, t.type_name, t.super_name, t.kind
                 FROM type_relations t
                 JOIN files f ON f.id = t.file_id
                 WHERE {filter}
                 ORDER BY f.path, t.line
                 LIMIT ?2"
            ))?;
            let records = stmt
                .query_map(params![name, safe_limit as i64], type_relation_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(records)
        })
    }

    /// Functions, methods and types whose name has no AST reference outside
    /// their own definition span, ordered by file path then line.
    ///
    /// Data files (HTML/JSON/YAML/TOML) are skipped: their keys are not code.
    /// `path_prefix` keeps files whose relative path starts with it;
    /// `language` keeps files of that language (`Language::name()`).
    pub fn find_unreferenced_symbols(
        &self,
        path_prefix: Option<&str>,
        language: Option<&str>,
        limit: usize,
    ) -> Result<Vec<UnreferencedSymbol>> {
        let safe_limit = limit.min(Self::MAX_STRUCTURE_LIMIT);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT s.id, s.file_id, s.name, s.kind, s.start_line, s.end_line,
                        s.start_byte, s.end_byte, s.signature, s.doc_comment, s.parent,
                        f.path, f.language
                 FROM symbols s
                 JOIN files f ON f.id = s.file_id
                 WHERE s.kind IN ({DEAD_CODE_KINDS})
                   AND (f.language IS NULL
                        OR f.language NOT IN ('html', 'json', 'yaml', 'toml'))
                   AND (?1 IS NULL OR substr(f.path, 1, length(?1)) = ?1)
                   AND (?2 IS NULL OR f.language = ?2)
                   AND NOT EXISTS (
                       SELECT 1 FROM refs r
                        WHERE r.name = s.name
                          AND r.kind != 'definition'
                          AND NOT (r.file_id = s.file_id
                                   AND r.line BETWEEN s.start_line AND s.end_line))
                 ORDER BY f.path, s.start_line
                 LIMIT ?3"
            ))?;
            let records = stmt
                .query_map(params![path_prefix, language, safe_limit as i64], |row| {
                    Ok(UnreferencedSymbol {
                        symbol: symbol_from_row(row)?,
                        file_path: row.get(11)?,
                        language: row.get(12)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(records)
        })
    }
}

fn type_relation_from_row(row: &Row) -> rusqlite::Result<TypeRelationRecord> {
    Ok(TypeRelationRecord {
        file_id: row.get(0)?,
        file_path: row.get(1)?,
        line: row.get(2)?,
        end_line: row.get(3)?,
        type_name: row.get(4)?,
        super_name: row.get(5)?,
        kind: row.get(6)?,
    })
}

fn symbol_from_row(row: &Row) -> rusqlite::Result<SymbolRecord> {
    Ok(SymbolRecord {
        id: row.get(0)?,
        file_id: row.get(1)?,
        name: row.get(2)?,
        kind: row.get(3)?,
        start_line: row.get(4)?,
        end_line: row.get(5)?,
        start_byte: row.get(6)?,
        end_byte: row.get(7)?,
        signature: row.get(8)?,
        doc_comment: row.get(9)?,
        parent: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use semantiq_parser::{
        CallEdge, Reference, ReferenceKind, RelationKind, Symbol, SymbolKind, TypeRelation,
    };

    fn sym(name: &str, kind: SymbolKind, start: usize, end: usize) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            start_line: start,
            end_line: end,
            start_byte: 0,
            end_byte: 0,
            signature: None,
            doc_comment: None,
            parent: None,
        }
    }

    fn reference(name: &str, line: usize, kind: ReferenceKind) -> Reference {
        Reference {
            name: name.to_string(),
            line,
            kind,
        }
    }

    #[test]
    fn test_insert_and_query_structure() {
        let store = IndexStore::open_in_memory().unwrap();
        let file_id = store
            .insert_file("src/a.rs", Some("rust"), "x", 1, 0)
            .unwrap();
        let structure = FileStructure {
            calls: vec![CallEdge {
                caller: "run".into(),
                caller_line: 1,
                callee: "helper".into(),
                line: 2,
            }],
            relations: vec![TypeRelation {
                type_name: "Foo".into(),
                super_name: "Bar".into(),
                kind: RelationKind::Implements,
                line: 5,
                end_line: 9,
            }],
        };
        store.insert_structure(file_id, &structure).unwrap();
        // Re-inserting replaces instead of duplicating.
        store.insert_structure(file_id, &structure).unwrap();

        let callers = store.find_callers("helper", 10).unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].caller, "run");
        assert_eq!(callers[0].file_path, "src/a.rs");
        assert_eq!(store.find_callees("run", 10).unwrap().len(), 1);

        let supers = store.find_supertypes("Foo", 10).unwrap();
        assert_eq!(supers.len(), 1);
        assert_eq!(supers[0].super_name, "Bar");
        assert_eq!(supers[0].end_line, 9);
        assert_eq!(store.find_subtypes("Bar", 10).unwrap()[0].type_name, "Foo");

        // Deleting the file cascades.
        store.delete_file("src/a.rs").unwrap();
        assert!(store.find_callers("helper", 10).unwrap().is_empty());
        assert!(store.find_subtypes("Bar", 10).unwrap().is_empty());
    }

    #[test]
    fn test_find_unreferenced_symbols() {
        let store = IndexStore::open_in_memory().unwrap();
        let a = store
            .insert_file("src/a.rs", Some("rust"), "a", 1, 0)
            .unwrap();
        let b = store
            .insert_file("lib/b.rs", Some("rust"), "b", 1, 0)
            .unwrap();
        let data = store
            .insert_file("conf.yaml", Some("yaml"), "c", 1, 0)
            .unwrap();
        store
            .insert_symbols(
                a,
                &[
                    sym("used", SymbolKind::Function, 1, 3),
                    sym("recursive", SymbolKind::Function, 5, 8),
                    sym("CONST_X", SymbolKind::Constant, 10, 10),
                ],
            )
            .unwrap();
        store
            .insert_symbols(b, &[sym("orphan", SymbolKind::Function, 1, 2)])
            .unwrap();
        store
            .insert_symbols(data, &[sym("section", SymbolKind::Struct, 1, 2)])
            .unwrap();
        store
            .insert_references(
                a,
                &[
                    reference("used", 1, ReferenceKind::Definition),
                    reference("recursive", 5, ReferenceKind::Definition),
                    // Self-call inside its own span does not count.
                    reference("recursive", 6, ReferenceKind::Call),
                ],
            )
            .unwrap();
        store
            .insert_references(b, &[reference("used", 2, ReferenceKind::Call)])
            .unwrap();

        let names = |prefix: Option<&str>| -> Vec<String> {
            store
                .find_unreferenced_symbols(prefix, None, 100)
                .unwrap()
                .into_iter()
                .map(|u| u.symbol.name)
                .collect()
        };
        assert_eq!(names(None), vec!["orphan", "recursive"]);
        assert_eq!(names(Some("src/")), vec!["recursive"]);
        assert!(
            store
                .find_unreferenced_symbols(None, Some("python"), 100)
                .unwrap()
                .is_empty()
        );
    }
}
