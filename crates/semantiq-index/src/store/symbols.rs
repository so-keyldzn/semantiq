//! Symbol operations for IndexStore.

use super::IndexStore;
use super::text::ExtensionFilter;
use crate::schema::SymbolRecord;
use anyhow::{Result, anyhow};
use rusqlite::Connection;
use rusqlite::params;
use semantiq_parser::Symbol;
use std::collections::HashMap;
use std::sync::{MutexGuard, PoisonError};
use tracing::debug;

impl IndexStore {
    /// Maximum limit for symbol search results to prevent excessive memory usage.
    const MAX_SYMBOL_SEARCH_LIMIT: usize = 10000;

    /// Insert symbols for a file (replaces existing symbols for that file),
    /// in its own transaction. The indexer writes a file's tables together
    /// with [`IndexStore::store_file_index`](super::IndexStore::store_file_index).
    pub fn insert_symbols(&self, file_id: i64, symbols: &[Symbol]) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e: PoisonError<MutexGuard<Connection>>| {
                anyhow!("Database lock poisoned: {}", e)
            })?;

        // Use a transaction for atomicity
        conn.execute("BEGIN IMMEDIATE", [])?;

        match insert_symbols_in(&conn, file_id, symbols) {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
                debug!("Inserted {} symbols for file_id {}", symbols.len(), file_id);
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Search symbols using FTS5 full-text search: every kind but `import`,
    /// in every file. See [`IndexStore::search_symbols_filtered`].
    pub fn search_symbols(&self, query: &str, limit: usize) -> Result<Vec<SymbolRecord>> {
        Ok(self
            .search_symbols_filtered(query, None, &ExtensionFilter::Except(Vec::new()), limit)?
            .into_iter()
            .map(|(symbol, _)| symbol)
            .collect())
    }

    /// Search symbols using FTS5 full-text search, each with its file path,
    /// best rank first. `kinds` keeps only those symbol kinds (`import` rows
    /// are left out unless asked for: an import is not a definition) and
    /// `files` those file extensions; both apply in SQL, before `LIMIT`, so
    /// a small limit never comes back empty while matching symbols of the
    /// wanted kind or file exist further down the ranking.
    pub fn search_symbols_filtered(
        &self,
        query: &str,
        kinds: Option<&[String]>,
        files: &ExtensionFilter,
        limit: usize,
    ) -> Result<Vec<(SymbolRecord, String)>> {
        // Cap limit to prevent excessive memory usage
        let safe_limit = limit.min(Self::MAX_SYMBOL_SEARCH_LIMIT);

        let (kind_clause, kind_params): (String, Vec<String>) = match kinds {
            None => ("s.kind != 'import'".to_string(), Vec::new()),
            Some([]) => ("0".to_string(), Vec::new()),
            Some(kinds) => {
                let placeholders = vec!["?"; kinds.len()].join(",");
                (
                    format!("lower(s.kind) IN ({placeholders})"),
                    kinds.iter().map(|k| k.to_lowercase()).collect(),
                )
            }
        };
        let (file_clause, file_params) = files.sql("f.path");
        let sql = format!(
            "SELECT s.id, s.file_id, s.name, s.kind, s.start_line, s.end_line,
                    s.signature, s.doc_comment, s.parent, f.path
             FROM symbols s
             JOIN symbols_fts ON s.id = symbols_fts.rowid
             JOIN files f ON f.id = s.file_id
             WHERE symbols_fts MATCH ? AND {kind_clause} AND {file_clause}
             ORDER BY symbols_fts.rank
             LIMIT ?"
        );

        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql)?;

            // Anonymous placeholders, bound in textual order.
            let fts_query = Self::escape_fts5_query(query);
            let limit = safe_limit as i64;
            let mut values: Vec<&dyn rusqlite::ToSql> = vec![&fts_query];
            values.extend(kind_params.iter().map(|k| k as &dyn rusqlite::ToSql));
            values.extend(file_params.iter().map(|p| p as &dyn rusqlite::ToSql));
            values.push(&limit);
            let results = stmt
                .query_map(values.as_slice(), |row| {
                    Ok((
                        SymbolRecord {
                            id: row.get(0)?,
                            file_id: row.get(1)?,
                            name: row.get(2)?,
                            kind: row.get(3)?,
                            start_line: row.get(4)?,
                            end_line: row.get(5)?,
                            signature: row.get(6)?,
                            doc_comment: row.get(7)?,
                            parent: row.get(8)?,
                        },
                        row.get(9)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// The first `limit` definitions of `name` with their file path, by path
    /// then line: every symbol of that exact name except `import` rows (an
    /// import statement is a usage, listed by the `refs` table). Bounded in
    /// SQL, so a common name (`new`) costs `limit` rows, not one per
    /// definition in the project.
    pub fn find_definitions_by_name(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Vec<(SymbolRecord, String)>> {
        let safe_limit = limit.min(Self::MAX_SYMBOL_SEARCH_LIMIT);
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT s.id, s.file_id, s.name, s.kind, s.start_line, s.end_line,
                        s.signature, s.doc_comment, s.parent, f.path
                 FROM symbols s
                 JOIN files f ON f.id = s.file_id
                 WHERE s.name = ?1 AND s.kind != 'import'
                 ORDER BY f.path, s.start_line, s.id
                 LIMIT ?2",
            )?;

            let results = stmt
                .query_map(params![name, safe_limit as i64], |row| {
                    Ok((
                        SymbolRecord {
                            id: row.get(0)?,
                            file_id: row.get(1)?,
                            name: row.get(2)?,
                            kind: row.get(3)?,
                            start_line: row.get(4)?,
                            end_line: row.get(5)?,
                            signature: row.get(6)?,
                            doc_comment: row.get(7)?,
                            parent: row.get(8)?,
                        },
                        row.get(9)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Find symbols by exact name match, import rows included (`explain`
    /// counts them as `imported_in`).
    pub fn find_symbol_by_name(&self, name: &str) -> Result<Vec<SymbolRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, file_id, name, kind, start_line, end_line,
                        signature, doc_comment, parent
                 FROM symbols WHERE name = ?1",
            )?;

            let results = stmt
                .query_map([name], |row| {
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
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Get all symbols for a file, ordered by start line.
    pub fn get_symbols_by_file(&self, file_id: i64) -> Result<Vec<SymbolRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, file_id, name, kind, start_line, end_line,
                        signature, doc_comment, parent
                 FROM symbols WHERE file_id = ?1
                 ORDER BY start_line",
            )?;

            let results = stmt
                .query_map([file_id], |row| {
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
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }
}

/// Replace the symbols of `file_id` inside the caller's transaction.
pub(super) fn insert_symbols_in(conn: &Connection, file_id: i64, symbols: &[Symbol]) -> Result<()> {
    conn.prepare_cached("DELETE FROM symbols WHERE file_id = ?1")?
        .execute([file_id])?;

    let mut stmt = conn.prepare_cached(
        "INSERT INTO symbols (file_id, name, kind, start_line, end_line, signature, doc_comment, parent)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for symbol in symbols {
        stmt.execute(params![
            file_id,
            symbol.name,
            symbol.kind.as_str(),
            symbol.start_line as i64,
            symbol.end_line as i64,
            symbol.signature,
            symbol.doc_comment,
            symbol.parent,
        ])?;
    }
    Ok(())
}

impl IndexStore {
    /// `name -> kind` of the symbols of each of `file_ids`, in one query
    /// (per 900 ids). On duplicate names in a file the last one by line wins:
    /// enough to label a chunk by its first symbol.
    pub fn get_symbol_kinds_by_files(
        &self,
        file_ids: &[i64],
    ) -> Result<HashMap<i64, HashMap<String, String>>> {
        let mut kinds: HashMap<i64, HashMap<String, String>> = HashMap::new();
        for batch in file_ids.chunks(super::chunks::SQL_IN_BATCH) {
            let placeholders: Vec<&str> = batch.iter().map(|_| "?").collect();
            let query = format!(
                "SELECT file_id, name, kind FROM symbols
                 WHERE file_id IN ({})
                 ORDER BY file_id, start_line",
                placeholders.join(",")
            );
            self.with_conn(|conn| {
                let mut stmt = conn.prepare(&query)?;
                let rows = stmt.query_map(rusqlite::params_from_iter(batch.iter()), |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
                for row in rows {
                    let (file_id, name, kind) = row?;
                    kinds.entry(file_id).or_default().insert(name, kind);
                }
                Ok(())
            })?;
        }
        Ok(kinds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semantiq_parser::SymbolKind;

    fn symbol(name: &str, kind: SymbolKind, line: usize, signature: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            start_line: line,
            end_line: line,
            start_byte: 0,
            end_byte: 0,
            signature: Some(signature.to_string()),
            doc_comment: None,
            parent: None,
        }
    }

    /// The proj3 probe of #134: 20 files `a/mNN.rs` defining `fn config`,
    /// one struct `Config` with a longer signature (lower bm25 rank) and one
    /// TypeScript `config` function.
    fn probe_store() -> IndexStore {
        let store = IndexStore::open_in_memory().unwrap();
        for i in 0..20 {
            let path = format!("a/m{i:02}.rs");
            let file_id = store.insert_file(&path, Some("rust"), "", 0).unwrap();
            store
                .insert_symbols(
                    file_id,
                    &[symbol("config", SymbolKind::Function, 1, "fn config()")],
                )
                .unwrap();
        }
        let file_id = store
            .insert_file("z_types.rs", Some("rust"), "", 0)
            .unwrap();
        store
            .insert_symbols(
                file_id,
                &[symbol(
                    "Config",
                    SymbolKind::Struct,
                    2,
                    "pub struct Config<T: Clone + Send + Sync + 'static> { inner: T }",
                )],
            )
            .unwrap();
        let file_id = store
            .insert_file("web/z.ts", Some("typescript"), "", 0)
            .unwrap();
        store
            .insert_symbols(
                file_id,
                &[
                    symbol(
                        "config",
                        SymbolKind::Function,
                        1,
                        "export function config(options: Options): Config",
                    ),
                    symbol(
                        "config",
                        SymbolKind::Import,
                        3,
                        "import { config } from './x'",
                    ),
                ],
            )
            .unwrap();
        store
    }

    #[test]
    fn search_symbols_filtered_applies_the_kind_filter_before_the_limit() {
        let store = probe_store();
        let all = ExtensionFilter::Except(Vec::new());
        let structs = ["struct".to_string()];
        let hits = store
            .search_symbols_filtered("config", Some(&structs), &all, 5)
            .unwrap();
        let found: Vec<(&str, &str, &str)> = hits
            .iter()
            .map(|(s, p)| (s.name.as_str(), s.kind.as_str(), p.as_str()))
            .collect();
        assert_eq!(found, vec![("Config", "struct", "z_types.rs")]);
    }

    #[test]
    fn search_symbols_filtered_applies_the_extension_filter_before_the_limit() {
        let store = probe_store();
        let ts = ExtensionFilter::Only(vec!["ts".to_string()]);
        let hits = store
            .search_symbols_filtered("config", None, &ts, 5)
            .unwrap();
        let found: Vec<(&str, &str, &str)> = hits
            .iter()
            .map(|(s, p)| (s.name.as_str(), s.kind.as_str(), p.as_str()))
            .collect();
        assert_eq!(found, vec![("config", "function", "web/z.ts")]);

        let not_rs = ExtensionFilter::Except(vec!["rs".to_string()]);
        let hits = store
            .search_symbols_filtered("config", None, &not_rs, 5)
            .unwrap();
        assert!(hits.iter().all(|(_, p)| p == "web/z.ts"), "{hits:?}");
    }

    #[test]
    fn search_symbols_leaves_imports_out_unless_asked() {
        let store = probe_store();
        let all = ExtensionFilter::Except(Vec::new());
        let hits = store
            .search_symbols_filtered("config", None, &all, 100)
            .unwrap();
        assert_eq!(hits.len(), 22);
        assert!(hits.iter().all(|(s, _)| s.kind != "import"));
        assert!(
            store
                .search_symbols("config", 100)
                .unwrap()
                .iter()
                .all(|s| s.kind != "import")
        );

        let imports = ["Import".to_string()];
        let hits = store
            .search_symbols_filtered("config", Some(&imports), &all, 100)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.kind, "import");
        assert!(
            store
                .search_symbols_filtered("config", Some(&[]), &all, 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn find_definitions_by_name_is_bounded_and_skips_imports() {
        let store = probe_store();
        let definitions = store.find_definitions_by_name("config", 100).unwrap();
        assert_eq!(definitions.len(), 21);
        assert!(definitions.iter().all(|(s, _)| s.kind != "import"));
        // By path: the TypeScript definition comes last.
        assert_eq!(definitions[0].1, "a/m00.rs");
        assert_eq!(definitions[20].1, "web/z.ts");

        let definitions = store.find_definitions_by_name("config", 3).unwrap();
        let paths: Vec<&str> = definitions.iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!(paths, vec!["a/m00.rs", "a/m01.rs", "a/m02.rs"]);

        // The unbounded lookup still sees the import row (`explain`).
        assert_eq!(store.find_symbol_by_name("config").unwrap().len(), 22);
        assert!(
            store
                .find_definitions_by_name("nothing", 10)
                .unwrap()
                .is_empty()
        );
    }
}
