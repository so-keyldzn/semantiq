//! Bulk, read-only access to the whole code graph (repo map ranking).
//!
//! The per-file accessors would cost one query per file or per name; the repo
//! map needs every file, symbol, resolved import and reference at once. On a
//! large index that is a million symbols and more reference counts, so the
//! snapshot is compact: names and kinds are interned (an index into one
//! table instead of a `String` per row), and the signatures and doc comments,
//! which only the few listed symbols need in full, are reduced to whatever
//! the caller's `details` closure keeps (their rendered size) and fetched
//! again by id for the selection ([`IndexStore::symbol_texts`]).

use super::IndexStore;
use anyhow::Result;
use rusqlite::{params, params_from_iter};
use std::collections::HashMap;

/// An indexed file, as a node of the code graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphFile {
    pub id: i64,
    pub path: String,
    pub language: Option<String>,
}

/// A symbol definition, as a node of the code graph. `name`, `kind` and
/// `parent` index [`RepoGraphData::names`] / [`RepoGraphData::kinds`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSymbol<T> {
    pub id: i64,
    pub file_id: i64,
    pub name: u32,
    pub kind: u32,
    pub start_line: u32,
    pub parent: Option<u32>,
    /// What the loader's `details` closure kept of the signature and doc.
    pub details: T,
}

/// Non-definition occurrences of `name` in a file, per reference kind, for
/// names that some symbol in the project defines. `name` indexes
/// [`RepoGraphData::names`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphRefCount {
    pub name: u32,
    pub file_id: i64,
    pub calls: u32,
    pub imports: u32,
    pub references: u32,
    pub types: u32,
}

/// Snapshot of the code graph, every list in a deterministic order.
#[derive(Debug, Clone, Default)]
pub struct RepoGraphData<T> {
    /// Interned symbol names, parent paths and referenced names, in order of
    /// first appearance.
    pub names: Vec<String>,
    /// Interned symbol kinds.
    pub kinds: Vec<String>,
    /// Ordered by path.
    pub files: Vec<GraphFile>,
    /// Ordered by file id, then start line. Imports are left out.
    pub symbols: Vec<GraphSymbol<T>>,
    /// `(source_file_id, target_file_id)` for each import resolved to an
    /// indexed file, ordered.
    pub file_edges: Vec<(i64, i64)>,
    /// Ordered by name, then file id. Names defined in more than
    /// [`MAX_NAME_DEFINERS`] files are left out.
    pub ref_counts: Vec<GraphRefCount>,
}

impl<T> RepoGraphData<T> {
    pub fn name(&self, index: u32) -> &str {
        &self.names[index as usize]
    }

    pub fn kind(&self, index: u32) -> &str {
        &self.kinds[index as usize]
    }
}

/// A name defined in more files than this (`new` in a monorepo) is left out
/// of the reference counts: matching is by name only, so its references
/// would link every referencing file to every defining file (tens of
/// millions of pairs in a 5k-file monorepo) for a weight already damped to
/// nothing.
pub const MAX_NAME_DEFINERS: usize = 100;

/// Maximum number of ids in one `symbol_texts` query (SQLite binds).
const TEXT_BATCH: usize = 500;

/// String table built while loading: each distinct string gets one index.
#[derive(Default)]
struct Interner {
    indices: HashMap<String, u32>,
    strings: Vec<String>,
}

impl Interner {
    fn intern(&mut self, value: &str) -> u32 {
        if let Some(&index) = self.indices.get(value) {
            return index;
        }
        let index = self.strings.len() as u32;
        self.indices.insert(value.to_string(), index);
        self.strings.push(value.to_string());
        index
    }
}

impl IndexStore {
    /// Load files, symbols, resolved imports and reference counts in one go.
    ///
    /// `details(name, kind, signature, doc_comment)` is called once per
    /// symbol with the texts borrowed from the row; the snapshot keeps only
    /// what it returns.
    pub fn load_repo_graph<T>(
        &self,
        mut details: impl FnMut(&str, &str, Option<&str>, Option<&str>) -> T,
    ) -> Result<RepoGraphData<T>> {
        self.with_conn(|conn| {
            let mut names = Interner::default();
            let mut kinds = Interner::default();

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

            let mut symbols = Vec::new();
            let mut stmt = conn.prepare_cached(
                "SELECT id, file_id, name, kind, start_line, signature, doc_comment, parent
                 FROM symbols
                 WHERE kind != 'import'
                 ORDER BY file_id, start_line, id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let name = row.get_ref(2)?.as_str()?;
                let kind = row.get_ref(3)?.as_str()?;
                let signature = row.get_ref(5)?.as_str_or_null()?;
                let doc_comment = row.get_ref(6)?.as_str_or_null()?;
                let parent = row.get_ref(7)?.as_str_or_null()?;
                let start_line: i64 = row.get(4)?;
                symbols.push(GraphSymbol {
                    id: row.get(0)?,
                    file_id: row.get(1)?,
                    details: details(name, kind, signature, doc_comment),
                    name: names.intern(name),
                    kind: kinds.intern(kind),
                    start_line: start_line.max(0) as u32,
                    parent: parent.map(|p| names.intern(p)),
                });
            }

            let file_edges = conn
                .prepare_cached(
                    "SELECT DISTINCT d.source_file_id, f.id
                     FROM dependencies d
                     JOIN files f ON f.path = d.resolved_path
                     ORDER BY d.source_file_id, f.id",
                )?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            // Grouped on a prefix of the primary key (name_id, file_id,
            // line): the rows come in that order and the kinds (1-byte
            // codes since schema v11, see `store/references.rs`) are pivoted
            // into columns, so SQLite sums in one pass without a sorter
            // (which took as long as the rest of the load on a 16M-row table).
            let mut ref_counts = Vec::new();
            let mut stmt = conn.prepare_cached(
                "SELECT n.name, r.file_id,
                        SUM(CASE WHEN r.kind = ?2 THEN r.count ELSE 0 END),
                        SUM(CASE WHEN r.kind = ?3 THEN r.count ELSE 0 END),
                        SUM(CASE WHEN r.kind = ?4 THEN r.count ELSE 0 END),
                        SUM(CASE WHEN r.kind = ?5 THEN r.count ELSE 0 END)
                 FROM refs r
                 JOIN names n ON n.id = r.name_id
                 WHERE r.kind != ?6
                   AND n.name IN (SELECT name FROM symbols
                                  WHERE kind != 'import'
                                  GROUP BY name
                                  HAVING COUNT(DISTINCT file_id) <= ?1)
                 GROUP BY r.name_id, r.file_id",
            )?;
            let count = |row: &rusqlite::Row, index: usize| -> rusqlite::Result<u32> {
                Ok(row.get::<_, i64>(index)?.clamp(0, u32::MAX as i64) as u32)
            };
            let mut rows = stmt.query(params![
                MAX_NAME_DEFINERS as i64,
                super::references::ref_kind_code("call"),
                super::references::ref_kind_code("import"),
                super::references::ref_kind_code("reference"),
                super::references::ref_kind_code("type"),
                super::references::REF_KIND_DEFINITION,
            ])?;
            while let Some(row) = rows.next()? {
                ref_counts.push(GraphRefCount {
                    name: names.intern(row.get_ref(0)?.as_str()?),
                    file_id: row.get(1)?,
                    calls: count(row, 2)?,
                    imports: count(row, 3)?,
                    references: count(row, 4)?,
                    types: count(row, 5)?,
                });
            }

            Ok(RepoGraphData {
                names: names.strings,
                kinds: kinds.strings,
                files,
                symbols,
                file_edges,
                ref_counts,
            })
        })
    }

    /// `(signature, doc_comment)` of the given symbols, by id. Ids no longer
    /// in the index are left out.
    #[allow(clippy::type_complexity)]
    pub fn symbol_texts(
        &self,
        ids: &[i64],
    ) -> Result<HashMap<i64, (Option<String>, Option<String>)>> {
        let mut texts = HashMap::with_capacity(ids.len());
        self.with_conn(|conn| {
            for batch in ids.chunks(TEXT_BATCH) {
                let placeholders = vec!["?"; batch.len()].join(", ");
                let mut stmt = conn.prepare(&format!(
                    "SELECT id, signature, doc_comment FROM symbols WHERE id IN ({placeholders})"
                ))?;
                let rows = stmt.query_map(params_from_iter(batch.iter()), |row| {
                    Ok((row.get::<_, i64>(0)?, (row.get(1)?, row.get(2)?)))
                })?;
                for row in rows {
                    let (id, text) = row?;
                    texts.insert(id, text);
                }
            }
            Ok(())
        })?;
        Ok(texts)
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
