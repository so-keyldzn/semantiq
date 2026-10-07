//! Bulk, read-only access to the whole code graph (repo map ranking).
//!
//! The per-file accessors would cost one query per file or per name; the repo
//! map needs every file, symbol, resolved import and reference at once.

use super::IndexStore;
use crate::schema::SymbolRecord;
use anyhow::Result;

/// An indexed file, as a node of the code graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphFile {
    pub id: i64,
    pub path: String,
    pub language: Option<String>,
}

/// Number of non-definition occurrences of `name` of a given reference kind
/// (call, type, import, reference) in a file, for names that some symbol in
/// the project defines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRefCount {
    pub name: String,
    pub file_id: i64,
    pub kind: String,
    pub count: usize,
}

/// Snapshot of the code graph, every list in a deterministic order.
#[derive(Debug, Clone, Default)]
pub struct RepoGraphData {
    /// Ordered by path.
    pub files: Vec<GraphFile>,
    /// Ordered by file id, then start line. Imports are left out.
    pub symbols: Vec<SymbolRecord>,
    /// `(source_file_id, target_file_id)` for each import resolved to an
    /// indexed file, ordered.
    pub file_edges: Vec<(i64, i64)>,
    /// Ordered by name, file id, then kind.
    pub ref_counts: Vec<GraphRefCount>,
}

impl IndexStore {
    /// Load files, symbols, resolved imports and reference counts in one go.
    pub fn load_repo_graph(&self) -> Result<RepoGraphData> {
        self.with_conn(|conn| {
            let files = conn
                .prepare("SELECT id, path, language FROM files ORDER BY path")?
                .query_map([], |row| {
                    Ok(GraphFile {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        language: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let symbols = conn
                .prepare(
                    "SELECT id, file_id, name, kind, start_line, end_line,
                            signature, doc_comment, parent
                     FROM symbols
                     WHERE kind != 'import'
                     ORDER BY file_id, start_line, id",
                )?
                .query_map([], |row| {
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
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let file_edges = conn
                .prepare(
                    "SELECT DISTINCT d.source_file_id, f.id
                     FROM dependencies d
                     JOIN files f ON f.path = d.resolved_path
                     ORDER BY d.source_file_id, f.id",
                )?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let ref_counts = conn
                .prepare(
                    "SELECT r.name, r.file_id, r.kind, SUM(r.count)
                     FROM refs r
                     WHERE r.kind != 'definition'
                       AND r.name IN (SELECT name FROM symbols WHERE kind != 'import')
                     GROUP BY r.name, r.file_id, r.kind
                     ORDER BY r.name, r.file_id, r.kind",
                )?
                .query_map([], |row| {
                    Ok(GraphRefCount {
                        name: row.get(0)?,
                        file_id: row.get(1)?,
                        kind: row.get(2)?,
                        count: row.get::<_, i64>(3)? as usize,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            Ok(RepoGraphData {
                files,
                symbols,
                file_edges,
                ref_counts,
            })
        })
    }

    /// Cheap fingerprint of the indexed content: changes whenever a file is
    /// added, removed or reindexed. Symbol and dependency ids come from
    /// AUTOINCREMENT, so re-extracting a file always moves their maximum.
    pub fn graph_fingerprint(&self) -> Result<String> {
        self.with_conn(|conn| {
            let fingerprint = conn.query_row(
                "SELECT
                    (SELECT COUNT(*) FROM files),
                    (SELECT COALESCE(SUM(indexed_at), 0) FROM files),
                    (SELECT COALESCE(SUM(last_modified), 0) FROM files),
                    (SELECT COUNT(*) FROM symbols),
                    (SELECT COALESCE(MAX(id), 0) FROM symbols),
                    (SELECT COUNT(*) FROM dependencies),
                    (SELECT COALESCE(MAX(id), 0) FROM dependencies),
                    (SELECT COUNT(*) FROM refs)",
                [],
                |row| {
                    let mut parts = Vec::with_capacity(8);
                    for i in 0..8 {
                        parts.push(row.get::<_, i64>(i)?.to_string());
                    }
                    Ok(parts.join(":"))
                },
            )?;
            Ok(fingerprint)
        })
    }
}
