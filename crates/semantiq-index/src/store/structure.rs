//! Call graph and type hierarchy operations for IndexStore.

use super::IndexStore;
use crate::schema::{CallEdgeRecord, SymbolRecord, TypeRelationRecord, UnreferencedSymbol};
use anyhow::Result;
use rusqlite::{Connection, Row, params};
use semantiq_parser::FileStructure;
use tracing::debug;

/// Symbol kinds considered by dead-code detection: callables and types.
const DEAD_CODE_KINDS: &str =
    "'function', 'method', 'class', 'struct', 'enum', 'interface', 'trait', 'type'";

/// Replace the call edges and type relations of `file_id` inside the
/// caller's transaction.
pub(super) fn insert_structure_in(
    conn: &Connection,
    file_id: i64,
    structure: &FileStructure,
) -> Result<()> {
    conn.prepare_cached("DELETE FROM call_edges WHERE file_id = ?1")?
        .execute([file_id])?;
    conn.prepare_cached("DELETE FROM type_relations WHERE file_id = ?1")?
        .execute([file_id])?;

    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO call_edges
            (file_id, line, callee, caller, caller_line, count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for edge in &structure.calls {
        stmt.execute(params![
            file_id,
            edge.line as i64,
            edge.callee,
            edge.caller,
            edge.caller_line as i64,
            edge.count.max(1) as i64,
        ])?;
    }

    let mut stmt = conn.prepare_cached(
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
}

impl IndexStore {
    /// Maximum number of call edges / relations returned by a single lookup.
    const MAX_STRUCTURE_LIMIT: usize = 10000;

    /// Insert the call edges and type relations of a file (replaces existing
    /// ones), in its own transaction. The indexer writes a file's tables
    /// together with [`IndexStore::store_file_index`](super::IndexStore::store_file_index).
    pub fn insert_structure(&self, file_id: i64, structure: &FileStructure) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", [])?;

            match insert_structure_in(conn, file_id, structure) {
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
        self.find_callers_near(callee, &[], limit)
    }

    /// Call sites of `callee`, the ones most likely to target a definition
    /// in `near_files` first (see `find_references_by_name_near`), so a
    /// capped lookup of a common name never cuts the trusted callers.
    pub fn find_callers_near(
        &self,
        callee: &str,
        near_files: &[i64],
        limit: usize,
    ) -> Result<Vec<CallEdgeRecord>> {
        let safe_limit = limit.min(Self::MAX_STRUCTURE_LIMIT);
        let priority =
            super::references::near_files_priority("e.file_id", callee, safe_limit, near_files);
        self.query_call_edges(
            &priority.with_clause,
            "e.callee = ?1",
            &priority.order_expr,
            priority.params,
        )
    }

    /// Calls made from inside any function/method named `caller`.
    pub fn find_callees(&self, caller: &str, limit: usize) -> Result<Vec<CallEdgeRecord>> {
        self.find_callees_from(caller, &[], limit)
    }

    /// Definition sites per `find_callees_from` query. The sites go into a
    /// row-value `IN (VALUES …)` list, flat however long (an `OR` chain of
    /// `(file_id = ? AND caller_line = ?)` terms nests one level per site and
    /// hits `SQLITE_MAX_EXPR_DEPTH` at 1000: `calls new` failed on any repo
    /// with ≳1000 definitions of a name, #155), so only the parameter limit
    /// (32766, two per site) bounds it; 4000 keeps each statement small and
    /// the batches are merged in Rust.
    pub(super) const CALLEE_SITES_PER_QUERY: usize = 4000;

    /// Calls made from inside the functions/methods named `caller` declared
    /// at the given `(file_id, line)` sites; from any of them when `sites`
    /// is empty. Filtering in SQL keeps a capped lookup of a common name
    /// (`new`) from returning only the homonyms sorting first.
    pub fn find_callees_from(
        &self,
        caller: &str,
        sites: &[(i64, i64)],
        limit: usize,
    ) -> Result<Vec<CallEdgeRecord>> {
        let safe_limit = limit.min(Self::MAX_STRUCTURE_LIMIT);
        if sites.is_empty() {
            let params: Vec<Box<dyn rusqlite::ToSql>> =
                vec![Box::new(caller.to_string()), Box::new(safe_limit as i64)];
            return self.query_call_edges("", "e.caller = ?1", "NULL", params);
        }

        let mut records = Vec::new();
        for batch in sites.chunks(Self::CALLEE_SITES_PER_QUERY) {
            let mut params: Vec<Box<dyn rusqlite::ToSql>> =
                vec![Box::new(caller.to_string()), Box::new(safe_limit as i64)];
            let rows: Vec<String> = batch
                .iter()
                .enumerate()
                .map(|(i, _)| format!("(?{}, ?{})", 2 * i + 3, 2 * i + 4))
                .collect();
            for (file_id, line) in batch {
                params.push(Box::new(*file_id));
                params.push(Box::new(*line));
            }
            let filter = format!(
                "e.caller = ?1 AND (e.file_id, e.caller_line) IN (VALUES {})",
                rows.join(", ")
            );
            records.extend(self.query_call_edges("", &filter, "NULL", params)?);
        }
        if sites.len() > Self::CALLEE_SITES_PER_QUERY {
            // Several batches, each capped on its own: restore the global
            // order and cap.
            records.sort_by(|a, b| {
                (a.file_path.as_str(), a.line, a.callee.as_str()).cmp(&(
                    b.file_path.as_str(),
                    b.line,
                    b.callee.as_str(),
                ))
            });
            records.truncate(safe_limit);
        }
        Ok(records)
    }

    /// `?1` = name, `?2` = limit in `with_clause`, `filter` and `priority`.
    fn query_call_edges(
        &self,
        with_clause: &str,
        filter: &str,
        priority: &str,
        params: Vec<Box<dyn rusqlite::ToSql>>,
    ) -> Result<Vec<CallEdgeRecord>> {
        self.with_conn(|conn| {
            let sql = format!(
                "{with_clause}
                 SELECT e.file_id, f.path, e.line, e.callee, e.caller, e.caller_line, e.count
                 FROM call_edges e
                 JOIN files f ON f.id = e.file_id
                 WHERE {filter}
                 ORDER BY {priority}, f.path, e.line
                 LIMIT ?2"
            );
            // The SQL text varies with the hint and site lists (extra
            // parameters); only the plain `find_callers` / `find_callees`
            // forms are worth a slot in the statement cache.
            let mut cached;
            let mut plain;
            let stmt: &mut rusqlite::Statement<'_> = if params.len() == 2 {
                cached = conn.prepare_cached(&sql)?;
                &mut cached
            } else {
                plain = conn.prepare(&sql)?;
                &mut plain
            };
            let records = stmt
                .query_map(rusqlite::params_from_iter(params), |row| {
                    Ok(CallEdgeRecord {
                        file_id: row.get(0)?,
                        file_path: row.get(1)?,
                        line: row.get(2)?,
                        callee: row.get(3)?,
                        caller: row.get(4)?,
                        caller_line: row.get(5)?,
                        count: row.get::<_, i64>(6)?.max(1) as usize,
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
            let mut stmt = conn.prepare_cached(
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
            let mut stmt = conn.prepare_cached(&format!(
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
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT s.id, s.file_id, s.name, s.kind, s.start_line, s.end_line,
                        s.signature, s.doc_comment, s.parent,
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
                        WHERE r.name_id = (SELECT n.id FROM names n WHERE n.name = s.name)
                          AND r.kind != ?4
                          AND NOT (r.file_id = s.file_id
                                   AND r.line BETWEEN s.start_line AND s.end_line))
                 ORDER BY f.path, s.start_line
                 LIMIT ?3"
            ))?;
            let records = stmt
                .query_map(
                    params![
                        path_prefix,
                        language,
                        safe_limit as i64,
                        super::references::REF_KIND_DEFINITION
                    ],
                    |row| {
                        Ok(UnreferencedSymbol {
                            symbol: symbol_from_row(row)?,
                            file_path: row.get(9)?,
                            language: row.get(10)?,
                        })
                    },
                )?
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
        signature: row.get(6)?,
        doc_comment: row.get(7)?,
        parent: row.get(8)?,
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
            count: 1,
        }
    }

    #[test]
    fn test_insert_and_query_structure() {
        let store = IndexStore::open_in_memory().unwrap();
        let file_id = store.insert_file("src/a.rs", Some("rust"), "x", 0).unwrap();
        let structure = FileStructure {
            calls: vec![CallEdge {
                caller: "run".into(),
                caller_line: 1,
                callee: "helper".into(),
                line: 2,
                count: 2,
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
        assert_eq!(callers[0].count, 2);
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

    fn call(caller: &str, caller_line: usize, callee: &str, line: usize) -> CallEdge {
        CallEdge {
            caller: caller.into(),
            caller_line,
            callee: callee.into(),
            line,
            count: 1,
        }
    }

    #[test]
    fn test_find_callees_from_filters_sites_before_the_limit() {
        // Regression for #42: the limit applied in path order before the
        // location filter, so homonyms sorting first hid the wanted edges.
        let store = IndexStore::open_in_memory().unwrap();
        for i in 0..6 {
            let id = store
                .insert_file(&format!("a{i}.rs"), Some("rust"), "x", 0)
                .unwrap();
            let structure = FileStructure {
                calls: vec![call("new", 1, "shared", 2)],
                relations: vec![],
            };
            store.insert_structure(id, &structure).unwrap();
        }
        let z = store.insert_file("zz.rs", Some("rust"), "x", 0).unwrap();
        let structure = FileStructure {
            calls: vec![call("new", 10, "target", 11), call("new", 20, "other", 21)],
            relations: vec![],
        };
        store.insert_structure(z, &structure).unwrap();

        let edges = store.find_callees_from("new", &[(z, 10)], 5).unwrap();
        let callees: Vec<&str> = edges.iter().map(|e| e.callee.as_str()).collect();
        assert_eq!(callees, vec!["target"]);
        // No sites: any function named `new`, capped.
        assert_eq!(store.find_callees_from("new", &[], 5).unwrap().len(), 5);
    }

    /// Regression for #155: `calls new` on a repository with ≳1000
    /// definitions of `new` failed with `Expression tree is too large
    /// (maximum depth 1000)`: the sites were an `OR` chain.
    #[test]
    fn test_find_callees_from_accepts_1500_definition_sites() {
        let store = IndexStore::open_in_memory().unwrap();
        let file = store.insert_file("big.rs", Some("rust"), "x", 0).unwrap();
        // One `new` per 10 lines, each calling `helper` on the next line.
        let calls: Vec<CallEdge> = (0..1500)
            .map(|i| call("new", 10 * i + 1, "helper", 10 * i + 2))
            .collect();
        store
            .insert_structure(
                file,
                &FileStructure {
                    calls,
                    relations: vec![],
                },
            )
            .unwrap();
        let other = store.insert_file("other.rs", Some("rust"), "x", 0).unwrap();
        store
            .insert_structure(
                other,
                &FileStructure {
                    calls: vec![call("new", 1, "unrelated", 2)],
                    relations: vec![],
                },
            )
            .unwrap();

        let sites: Vec<(i64, i64)> = (0..1500).map(|i| (file, 10 * i + 1)).collect();
        let edges = store.find_callees_from("new", &sites, 10_000).unwrap();
        assert_eq!(edges.len(), 1500);
        assert!(edges.iter().all(|e| e.callee == "helper"));
        // A site that is not in the list is not matched.
        assert_eq!(
            store
                .find_callees_from("new", &sites[..3], 10)
                .unwrap()
                .len(),
            3
        );
    }

    /// More sites than one statement takes: the batches are merged in path
    /// and line order and capped once.
    #[test]
    fn test_find_callees_from_merges_batches_in_order() {
        let store = IndexStore::open_in_memory().unwrap();
        let per_query = IndexStore::CALLEE_SITES_PER_QUERY;
        let total = per_query * 2 + 10;
        // Two files so that path order and batch order disagree: the first
        // batch holds the sites of `b.rs`, the second those of `a.rs`.
        let b = store.insert_file("b.rs", Some("rust"), "x", 0).unwrap();
        let a = store.insert_file("a.rs", Some("rust"), "x", 0).unwrap();
        let mut sites = Vec::with_capacity(total);
        for (file, name) in [(b, "from_b"), (a, "from_a")] {
            let calls: Vec<CallEdge> = (0..total / 2)
                .map(|i| call("new", 2 * i + 1, name, 2 * i + 2))
                .collect();
            sites.extend((0..total / 2).map(|i| (file, 2 * i as i64 + 1)));
            store
                .insert_structure(
                    file,
                    &FileStructure {
                        calls,
                        relations: vec![],
                    },
                )
                .unwrap();
        }
        assert!(sites.len() > per_query);

        let edges = store.find_callees_from("new", &sites, 7).unwrap();
        assert_eq!(edges.len(), 7);
        assert!(edges.iter().all(|e| e.file_path == "a.rs"), "{edges:?}");
        let lines: Vec<i64> = edges.iter().map(|e| e.line).collect();
        assert_eq!(lines, vec![2, 4, 6, 8, 10, 12, 14]);
        // Uncapped, every site is matched exactly once.
        let all = store.find_callees_from("new", &sites, 10_000).unwrap();
        assert_eq!(all.len(), total);
    }

    #[test]
    fn test_find_callers_near_puts_trusted_files_first() {
        let store = IndexStore::open_in_memory().unwrap();
        for i in 0..6 {
            let id = store
                .insert_file(&format!("a{i}.rs"), Some("rust"), "x", 0)
                .unwrap();
            let structure = FileStructure {
                calls: vec![call("run", 1, "shared", 2)],
                relations: vec![],
            };
            store.insert_structure(id, &structure).unwrap();
        }
        let importer = store.insert_file("m.rs", Some("rust"), "x", 0).unwrap();
        store
            .insert_structure(
                importer,
                &FileStructure {
                    calls: vec![call("go", 1, "shared", 3)],
                    relations: vec![],
                },
            )
            .unwrap();
        store
            .insert_dependency(
                importer,
                "crate::zz::shared",
                Some("shared"),
                "local",
                None,
                (1, 1),
            )
            .unwrap();
        let z = store.insert_file("zz.rs", Some("rust"), "x", 0).unwrap();
        store
            .insert_structure(
                z,
                &FileStructure {
                    calls: vec![call("local", 5, "shared", 6)],
                    relations: vec![],
                },
            )
            .unwrap();

        let edges = store.find_callers_near("shared", &[z], 3).unwrap();
        let files: Vec<&str> = edges.iter().map(|e| e.file_path.as_str()).collect();
        assert_eq!(files, vec!["zz.rs", "m.rs", "a0.rs"]);
        // Without a hint, plain path order.
        let edges = store.find_callers("shared", 2).unwrap();
        let files: Vec<&str> = edges.iter().map(|e| e.file_path.as_str()).collect();
        assert_eq!(files, vec!["a0.rs", "a1.rs"]);
    }

    #[test]
    fn test_find_unreferenced_symbols() {
        let store = IndexStore::open_in_memory().unwrap();
        let a = store.insert_file("src/a.rs", Some("rust"), "a", 0).unwrap();
        let b = store.insert_file("lib/b.rs", Some("rust"), "b", 0).unwrap();
        let data = store
            .insert_file("conf.yaml", Some("yaml"), "c", 0)
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
