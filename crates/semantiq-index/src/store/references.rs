//! AST reference (identifier occurrence) operations for IndexStore.
//!
//! `refs` stores one row per (name, file, line). Since schema v11 the name
//! is interned in `names` (`refs.name_id`) and the kind is a 1-byte code
//! ([`ref_kind_code`]): the text of both was repeated on every row and again
//! in `idx_refs_file_id` (#162). Callers keep seeing names and kind words.

use super::IndexStore;
use crate::schema::ReferenceRecord;
use anyhow::Result;
use rusqlite::{Connection, params};
use semantiq_parser::Reference;
use tracing::debug;

/// `refs.kind` codes. New kinds get the next number; a code is never reused.
const REF_KINDS: [&str; 5] = ["definition", "import", "call", "type", "reference"];

/// Code of `refs.kind = 'definition'`, the kind most queries exclude.
pub(super) const REF_KIND_DEFINITION: i64 = 0;

/// Storage code of a reference kind (`ReferenceKind::as_str()`). An unknown
/// word is stored as a plain `reference`.
pub(super) fn ref_kind_code(kind: &str) -> i64 {
    REF_KINDS
        .iter()
        .position(|k| *k == kind)
        .unwrap_or(REF_KINDS.len() - 1) as i64
}

/// The kind word of a storage code.
pub(super) fn ref_kind_name(code: i64) -> &'static str {
    usize::try_from(code)
        .ok()
        .and_then(|i| REF_KINDS.get(i))
        .copied()
        .unwrap_or("reference")
}

impl IndexStore {
    /// Maximum number of references returned by a single lookup.
    const MAX_REFERENCE_LIMIT: usize = 10000;

    /// Insert references for a file (replaces existing references for that
    /// file), in its own transaction. The indexer writes a file's tables
    /// together with [`IndexStore::store_file_index`](super::IndexStore::store_file_index).
    pub fn insert_references(&self, file_id: i64, references: &[Reference]) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", [])?;

            match insert_references_in(conn, file_id, references) {
                Ok(()) => {
                    conn.execute("COMMIT", [])?;
                    debug!(
                        "Inserted {} references for file_id {}",
                        references.len(),
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

    /// Occurrences of an exact identifier name, ordered by file path then line.
    pub fn find_references_by_name(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Vec<ReferenceRecord>> {
        self.find_references_by_name_near(name, &[], limit)
    }

    /// Occurrences of an exact identifier name, the ones most likely to
    /// target a definition in `near_files` first: occurrences in those files,
    /// then in files importing one of them (or the name), then the rest; by
    /// file path and line within each group. A capped lookup of a common
    /// name thus never cuts the trusted occurrences.
    pub fn find_references_by_name_near(
        &self,
        name: &str,
        near_files: &[i64],
        limit: usize,
    ) -> Result<Vec<ReferenceRecord>> {
        let safe_limit = limit.min(Self::MAX_REFERENCE_LIMIT);

        self.with_conn(|conn| {
            let priority = near_files_priority("r.file_id", name, safe_limit, near_files);
            let sql = format!(
                "{with}
                 SELECT r.file_id, f.path, r.line, r.kind
                 FROM refs r
                 JOIN files f ON f.id = r.file_id
                 WHERE r.name_id = (SELECT id FROM names WHERE name = ?1)
                 ORDER BY {order}, f.path, r.line
                 LIMIT ?2",
                with = priority.with_clause,
                order = priority.order_expr,
            );
            // The SQL text varies with the number of near files; the no-hint
            // form (`find_references_by_name`) is the one worth caching.
            let mut cached;
            let mut plain;
            let stmt: &mut rusqlite::Statement<'_> = if near_files.is_empty() {
                cached = conn.prepare_cached(&sql)?;
                &mut cached
            } else {
                plain = conn.prepare(&sql)?;
                &mut plain
            };
            let records = stmt
                .query_map(rusqlite::params_from_iter(priority.params), |row| {
                    Ok(ReferenceRecord {
                        file_id: row.get(0)?,
                        file_path: row.get(1)?,
                        line: row.get(2)?,
                        kind: ref_kind_name(row.get(3)?).to_string(),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(records)
        })
    }

    /// Number of non-definition occurrences of an exact identifier name,
    /// counting every occurrence on a line (`refs.count`), not just the lines.
    pub fn count_usages(&self, name: &str) -> Result<usize> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT COALESCE(SUM(count), 0) FROM refs
                 WHERE name_id = (SELECT id FROM names WHERE name = ?1) AND kind != ?2",
            )?;
            let count: i64 =
                stmt.query_row(params![name, REF_KIND_DEFINITION], |row| row.get(0))?;
            Ok(count as usize)
        })
    }
}

/// Replace the references of `file_id` inside the caller's transaction:
/// the names are interned in `names` and the kind stored as its code.
pub(super) fn insert_references_in(
    conn: &Connection,
    file_id: i64,
    references: &[Reference],
) -> Result<()> {
    conn.prepare_cached("DELETE FROM refs WHERE file_id = ?1")?
        .execute([file_id])?;

    let mut intern = conn.prepare_cached("INSERT OR IGNORE INTO names (name) VALUES (?1)")?;
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO refs (name_id, file_id, line, kind, count)
         SELECT id, ?2, ?3, ?4, ?5 FROM names WHERE name = ?1",
    )?;
    for reference in references {
        intern.execute([reference.name.as_str()])?;
        insert.execute(params![
            reference.name,
            file_id,
            reference.line as i64,
            ref_kind_code(reference.kind.as_str()),
            reference.count.max(1) as i64,
        ])?;
    }
    Ok(())
}

/// The pieces of a query ranking rows by their closeness to `near_files`
/// (see [`near_files_priority`]).
pub(super) struct NearFilesPriority {
    /// `WITH …` clause defining the `near` and `importers` file sets, or empty.
    pub with_clause: String,
    /// `ORDER BY` expression: 0 for a near file, 1 for an importer, 2 otherwise.
    pub order_expr: String,
    /// Positional parameters: `?1` = `name`, `?2` = `limit`, then the file ids.
    pub params: Vec<Box<dyn rusqlite::ToSql>>,
}

/// SQL ranking a row of `file_column` by how likely its use of `name`
/// targets a definition in `near_files`: 0 in one of those files, 1 in a file
/// importing one of them (resolved import) or importing the name, 2 otherwise.
///
/// The two file sets are computed once per query as CTEs and probed per row
/// (`IN (SELECT …)` over a non-correlated subquery is an ephemeral index
/// lookup). The previous form evaluated a correlated `EXISTS` over
/// `dependencies` for every occurrence of the name before the `LIMIT`: 0.7 s
/// for the 96k occurrences of `new` on a 24k-file index, once per BFS node
/// of `impact` (#161).
pub(super) fn near_files_priority(
    file_column: &str,
    name: &str,
    limit: usize,
    near_files: &[i64],
) -> NearFilesPriority {
    let mut params: Vec<Box<dyn rusqlite::ToSql>> =
        vec![Box::new(name.to_string()), Box::new(limit as i64)];
    if near_files.is_empty() {
        // A bare integer in ORDER BY would be a column index.
        return NearFilesPriority {
            with_clause: String::new(),
            order_expr: "NULL".to_string(),
            params,
        };
    }
    let rows: Vec<String> = near_files
        .iter()
        .enumerate()
        .map(|(i, _)| format!("(?{})", i + 3))
        .collect();
    params.extend(
        near_files
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::ToSql>),
    );
    // `last_segment(target_path) == name`: the path ends with the name,
    // preceded by a separator or nothing.
    let with_clause = format!(
        "WITH near(file_id) AS MATERIALIZED (VALUES {rows}),
              importers(file_id) AS MATERIALIZED (
                  SELECT DISTINCT d.source_file_id
                  FROM dependencies d
                  LEFT JOIN files df ON df.path = d.resolved_path
                  WHERE df.id IN (SELECT file_id FROM near)
                     OR d.import_name = ?1
                     OR (substr(d.target_path, -length(?1)) = ?1
                         AND (length(d.target_path) = length(?1)
                              OR substr(d.target_path, -length(?1) - 1, 1)
                                 IN (':', '.', '/', '\\'))))",
        rows = rows.join(", "),
    );
    let order_expr = format!(
        "CASE
            WHEN {file_column} IN (SELECT file_id FROM near) THEN 0
            WHEN {file_column} IN (SELECT file_id FROM importers) THEN 1
            ELSE 2
         END"
    );
    NearFilesPriority {
        with_clause,
        order_expr,
        params,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_codes_round_trip_and_never_move() {
        for (code, kind) in REF_KINDS.iter().enumerate() {
            assert_eq!(ref_kind_code(kind), code as i64);
            assert_eq!(ref_kind_name(code as i64), *kind);
        }
        assert_eq!(ref_kind_code("definition"), REF_KIND_DEFINITION);
        // Stored codes are part of the on-disk format.
        assert_eq!(ref_kind_code("import"), 1);
        assert_eq!(ref_kind_code("call"), 2);
        assert_eq!(ref_kind_code("type"), 3);
        assert_eq!(ref_kind_code("reference"), 4);
        assert_eq!(ref_kind_code("something-new"), 4);
        assert_eq!(ref_kind_name(42), "reference");
    }

    /// Regression for #161: the near-file priority must not evaluate a
    /// correlated subquery per reference row.
    #[test]
    fn near_files_priority_is_not_a_correlated_subquery() {
        let store = IndexStore::open_in_memory().unwrap();
        let a = store.insert_file("a.rs", Some("rust"), "x", 0).unwrap();
        let priority = near_files_priority("r.file_id", "foo", 10, &[a]);
        let sql = format!(
            "EXPLAIN QUERY PLAN {with}
             SELECT r.file_id, f.path, r.line, r.kind
             FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.name_id = (SELECT id FROM names WHERE name = ?1)
             ORDER BY {order}, f.path, r.line
             LIMIT ?2",
            with = priority.with_clause,
            order = priority.order_expr,
        );
        let plan: Vec<String> = store
            .with_conn(|conn| {
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(priority.params), |row| {
                        row.get::<_, String>(3)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .unwrap();
        let plan_text = plan.join("\n");
        assert!(
            !plan_text.contains("CORRELATED"),
            "priority is evaluated per reference row:\n{plan_text}"
        );
        assert!(
            plan_text.contains("MATERIALIZE"),
            "file sets are not materialized once:\n{plan_text}"
        );
    }
}
