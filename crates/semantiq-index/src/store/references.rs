//! AST reference (identifier occurrence) operations for IndexStore.

use super::IndexStore;
use crate::schema::ReferenceRecord;
use anyhow::Result;
use rusqlite::params;
use semantiq_parser::Reference;
use tracing::debug;

impl IndexStore {
    /// Maximum number of references returned by a single lookup.
    const MAX_REFERENCE_LIMIT: usize = 10000;

    /// Insert references for a file (replaces existing references for that file).
    pub fn insert_references(&self, file_id: i64, references: &[Reference]) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", [])?;

            let result = (|| -> Result<()> {
                conn.execute("DELETE FROM refs WHERE file_id = ?1", [file_id])?;

                let mut stmt = conn.prepare(
                    "INSERT OR IGNORE INTO refs (name, file_id, line, kind) VALUES (?1, ?2, ?3, ?4)",
                )?;
                for reference in references {
                    stmt.execute(params![
                        reference.name,
                        file_id,
                        reference.line as i64,
                        reference.kind.as_str(),
                    ])?;
                }
                Ok(())
            })();

            match result {
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
        let safe_limit = limit.min(Self::MAX_REFERENCE_LIMIT);

        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT f.path, r.line, r.kind
                 FROM refs r
                 JOIN files f ON f.id = r.file_id
                 WHERE r.name = ?1
                 ORDER BY f.path, r.line
                 LIMIT ?2",
            )?;
            let records = stmt
                .query_map(params![name, safe_limit as i64], |row| {
                    Ok(ReferenceRecord {
                        file_path: row.get(0)?,
                        line: row.get(1)?,
                        kind: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(records)
        })
    }

    /// Number of non-definition occurrences of an exact identifier name.
    pub fn count_usages(&self, name: &str) -> Result<usize> {
        self.with_conn(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM refs WHERE name = ?1 AND kind != 'definition'",
                [name],
                |row| row.get(0),
            )?;
            Ok(count as usize)
        })
    }
}
