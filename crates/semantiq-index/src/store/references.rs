//! AST reference (identifier occurrence) operations for IndexStore.

use super::IndexStore;
use crate::schema::ReferenceRecord;
use anyhow::Result;
use rusqlite::{Connection, params};
use semantiq_parser::Reference;
use tracing::debug;

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
            let (priority, params) = near_files_priority("r.file_id", name, safe_limit, near_files);
            let mut stmt = conn.prepare(&format!(
                "SELECT r.file_id, f.path, r.line, r.kind
                 FROM refs r
                 JOIN files f ON f.id = r.file_id
                 WHERE r.name = ?1
                 ORDER BY {priority}, f.path, r.line
                 LIMIT ?2"
            ))?;
            let records = stmt
                .query_map(rusqlite::params_from_iter(params), |row| {
                    Ok(ReferenceRecord {
                        file_id: row.get(0)?,
                        file_path: row.get(1)?,
                        line: row.get(2)?,
                        kind: row.get(3)?,
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
            let count: i64 = conn.query_row(
                "SELECT COALESCE(SUM(count), 0) FROM refs WHERE name = ?1 AND kind != 'definition'",
                [name],
                |row| row.get(0),
            )?;
            Ok(count as usize)
        })
    }
}

/// Replace the references of `file_id` inside the caller's transaction.
pub(super) fn insert_references_in(
    conn: &Connection,
    file_id: i64,
    references: &[Reference],
) -> Result<()> {
    conn.prepare_cached("DELETE FROM refs WHERE file_id = ?1")?
        .execute([file_id])?;

    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO refs (name, file_id, line, kind, count)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for reference in references {
        stmt.execute(params![
            reference.name,
            file_id,
            reference.line as i64,
            reference.kind.as_str(),
            reference.count.max(1) as i64,
        ])?;
    }
    Ok(())
}

/// SQL expression ranking a row of `file_column` by how likely its use of
/// `name` targets a definition in `near_files`: 0 in one of those files, 1
/// in a file importing one of them (resolved import) or importing the name,
/// 2 otherwise. Returns the expression and the positional parameters of the
/// query: `?1` = `name`, `?2` = `limit`, then the file ids.
pub(super) fn near_files_priority(
    file_column: &str,
    name: &str,
    limit: usize,
    near_files: &[i64],
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut params: Vec<Box<dyn rusqlite::ToSql>> =
        vec![Box::new(name.to_string()), Box::new(limit as i64)];
    if near_files.is_empty() {
        // A bare integer in ORDER BY would be a column index.
        return ("NULL".to_string(), params);
    }
    let placeholders: Vec<String> = near_files
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 3))
        .collect();
    params.extend(
        near_files
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::ToSql>),
    );
    let ids = placeholders.join(", ");
    // `last_segment(target_path) == name`: the path ends with the name,
    // preceded by a separator or nothing.
    let priority = format!(
        "CASE
            WHEN {file_column} IN ({ids}) THEN 0
            WHEN EXISTS (
                SELECT 1 FROM dependencies d
                LEFT JOIN files df ON df.path = d.resolved_path
                WHERE d.source_file_id = {file_column}
                  AND (df.id IN ({ids})
                       OR d.import_name = ?1
                       OR (substr(d.target_path, -length(?1)) = ?1
                           AND (length(d.target_path) = length(?1)
                                OR substr(d.target_path, -length(?1) - 1, 1)
                                   IN (':', '.', '/', '\\'))))) THEN 1
            ELSE 2
         END"
    );
    (priority, params)
}
