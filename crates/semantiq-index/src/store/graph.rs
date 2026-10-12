//! Bulk, read-only access to the whole code graph (repo map ranking).
//!
//! The per-file accessors would cost one query per file or per name; the repo
//! map needs every file, symbol, resolved import and reference at once.

use super::IndexStore;
use crate::schema::SymbolRecord;
use anyhow::Result;
use rusqlite::params;

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
    /// Ordered by name, file id, then kind. Names defined in more than
    /// [`MAX_NAME_DEFINERS`] files are left out.
    pub ref_counts: Vec<GraphRefCount>,
}

/// A name defined in more files than this (`new` in a monorepo) is left out
/// of the reference counts: matching is by name only, so its references
/// would link every referencing file to every defining file (tens of
/// millions of pairs in a 5k-file monorepo) for a weight already damped to
/// nothing.
pub const MAX_NAME_DEFINERS: usize = 100;

impl IndexStore {
    /// Load files, symbols, resolved imports and reference counts in one go.
    pub fn load_repo_graph(&self) -> Result<RepoGraphData> {
        self.with_conn(|conn| {
            let files = conn
                .prepare_cached("SELECT id, path, language FROM files ORDER BY path")?
                .query_map([], |row| {
                    Ok(GraphFile {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        language: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let symbols = conn
                .prepare_cached(
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
                .prepare_cached(
                    "SELECT DISTINCT d.source_file_id, f.id
                     FROM dependencies d
                     JOIN files f ON f.path = d.resolved_path
                     ORDER BY d.source_file_id, f.id",
                )?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            // Kinds come back as their storage code and are ordered by it
            // (definition, import, call, type, reference), not alphabetically.
            let ref_counts = conn
                .prepare_cached(
                    "SELECT n.name, r.file_id, r.kind, SUM(r.count)
                     FROM refs r
                     JOIN names n ON n.id = r.name_id
                     WHERE r.kind != ?2
                       AND n.name IN (SELECT name FROM symbols
                                      WHERE kind != 'import'
                                      GROUP BY name
                                      HAVING COUNT(DISTINCT file_id) <= ?1)
                     GROUP BY r.name_id, r.file_id, r.kind
                     ORDER BY n.name, r.file_id, r.kind",
                )?
                .query_map(
                    params![
                        MAX_NAME_DEFINERS as i64,
                        super::references::REF_KIND_DEFINITION
                    ],
                    |row| {
                        Ok(GraphRefCount {
                            name: row.get(0)?,
                            file_id: row.get(1)?,
                            kind: super::references::ref_kind_name(row.get(2)?).to_string(),
                            count: row.get::<_, i64>(3)? as usize,
                        })
                    },
                )?
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
    /// added, removed or reindexed. O(1): the `graph_generation` counter is
    /// bumped by triggers on every `files` write (a file is upserted before
    /// its symbols, references and imports are written and deleted with
    /// them), and the symbol / dependency ids come from AUTOINCREMENT, so
    /// re-extracting a file moves their maximum (a `MAX` on a rowid is one
    /// B-tree descent). The previous form summed two `files` columns and
    /// counted `refs` on every unfocused repo map call: 84 ms on a 24k-file
    /// index, cache hit or not (#165).
    ///
    /// A repo map request landing between a file's upsert and its last
    /// structural write may cache that file's partly written graph; the
    /// next write to any file refreshes it.
    pub fn graph_fingerprint(&self) -> Result<String> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT
                    COALESCE((SELECT value FROM metadata WHERE key = 'graph_generation'), '0'),
                    (SELECT COALESCE(MAX(id), 0) FROM files),
                    (SELECT COALESCE(MAX(id), 0) FROM symbols),
                    (SELECT COALESCE(MAX(id), 0) FROM dependencies)",
            )?;
            let fingerprint = stmt.query_row([], |row| {
                Ok(format!(
                    "{}:{}:{}:{}",
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?
                ))
            })?;
            Ok(fingerprint)
        })
    }
}
