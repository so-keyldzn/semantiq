//! Text search over the indexed chunks (`chunks_fts`, a trigram FTS5 index
//! over `chunks.content`), and the file-extension filter shared with the
//! symbol search.

use super::IndexStore;
use anyhow::Result;

/// Which files a search may return, by extension (`accepts_extension` on the
/// engine side), pushed into SQL so it applies before `LIMIT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionFilter {
    /// Only files with one of these extensions (`file_type` set).
    Only(Vec<String>),
    /// Every file except these extensions (the default exclusions).
    Except(Vec<String>),
}

impl ExtensionFilter {
    /// SQL condition on `column` (a file path) and its parameters, in order.
    /// Matching is `LIKE '%.ext'`, case-insensitive for ASCII like the
    /// engine's extension check.
    pub(super) fn sql(&self, column: &str) -> (String, Vec<String>) {
        let (extensions, negate) = match self {
            ExtensionFilter::Only(extensions) => (extensions, false),
            ExtensionFilter::Except(extensions) => (extensions, true),
        };
        if extensions.is_empty() {
            return (if negate { "1" } else { "0" }.to_string(), Vec::new());
        }
        let clauses: Vec<String> = extensions
            .iter()
            .map(|_| format!("{column} LIKE ? ESCAPE '\\'"))
            .collect();
        let params = extensions
            .iter()
            .map(|ext| format!("%.{}", escape_like(ext)))
            .collect();
        let clause = clauses.join(" OR ");
        let sql = if negate {
            format!("NOT ({clause})")
        } else {
            format!("({clause})")
        };
        (sql, params)
    }
}

/// Escape `%`, `_` and `\` for a `LIKE ... ESCAPE '\'` pattern.
fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// A chunk holding a searched text, with its file resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkTextHit {
    pub file_path: String,
    pub language: Option<String>,
    pub content: String,
    /// First line of the chunk in its file, 1-based.
    pub start_line: i64,
}

/// Shortest text the trigram index can look up; shorter needles scan
/// `chunks.content` with `LIKE`.
const TRIGRAM_MIN_CHARS: usize = 3;

impl IndexStore {
    /// Chunks whose content contains `needle` as a substring, ignoring ASCII
    /// case, in files accepted by `files`: a page of `limit` chunks from
    /// `offset`, best FTS rank first. The trigram index answers needles of at
    /// least 3 characters; shorter ones scan the chunk contents in SQLite,
    /// still without touching the file system.
    pub fn search_chunk_text(
        &self,
        needle: &str,
        files: &ExtensionFilter,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ChunkTextHit>> {
        if needle.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let (file_clause, file_params) = files.sql("f.path");
        let trigram = needle.chars().count() >= TRIGRAM_MIN_CHARS;
        let sql = if trigram {
            format!(
                "SELECT f.path, f.language, c.content, c.start_line
                 FROM chunks_fts
                 JOIN chunks c ON c.id = chunks_fts.rowid
                 JOIN files f ON f.id = c.file_id
                 WHERE chunks_fts MATCH ? AND {file_clause}
                 ORDER BY chunks_fts.rank, c.id
                 LIMIT ? OFFSET ?"
            )
        } else {
            format!(
                "SELECT f.path, f.language, c.content, c.start_line
                 FROM chunks c
                 JOIN files f ON f.id = c.file_id
                 WHERE c.content LIKE ? ESCAPE '\\' AND {file_clause}
                 ORDER BY c.id
                 LIMIT ? OFFSET ?"
            )
        };
        let pattern = if trigram {
            fts5_phrase(needle)
        } else {
            format!("%{}%", escape_like(needle))
        };

        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            // Anonymous placeholders, bound in textual order.
            let limit = limit as i64;
            let offset = offset as i64;
            let mut values: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(3 + file_params.len());
            values.push(&pattern);
            values.extend(file_params.iter().map(|p| p as &dyn rusqlite::ToSql));
            values.push(&limit);
            values.push(&offset);
            let hits = stmt
                .query_map(values.as_slice(), |row| {
                    Ok(ChunkTextHit {
                        file_path: row.get(0)?,
                        language: row.get(1)?,
                        content: row.get(2)?,
                        start_line: row.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(hits)
        })
    }

    /// Number of rows of the text index, for tests and `stats`.
    pub fn count_text_indexed_chunks(&self) -> Result<i64> {
        self.with_conn(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM chunks_fts", [], |row| row.get(0))?)
        })
    }
}

/// `needle` as one FTS5 phrase: a substring match for the trigram tokenizer.
/// Control characters are dropped and `"` doubled, like `escape_fts5_query`,
/// without its prefix `*`.
fn fts5_phrase(needle: &str) -> String {
    let cleaned: String = needle.chars().filter(|c| !c.is_control()).collect();
    format!("\"{}\"", cleaned.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use semantiq_parser::CodeChunk;

    fn chunk(content: &str, start_line: usize) -> CodeChunk {
        CodeChunk {
            content: content.to_string(),
            start_line,
            end_line: start_line + content.lines().count().max(1) - 1,
            start_byte: 0,
            end_byte: 0,
            symbols: Vec::new(),
        }
    }

    fn store_with(files: &[(&str, &str, &str)]) -> IndexStore {
        let store = IndexStore::open_in_memory().unwrap();
        for (path, language, content) in files {
            let file_id = store.insert_file(path, Some(language), content, 0).unwrap();
            store.insert_chunks(file_id, &[chunk(content, 1)]).unwrap();
        }
        store
    }

    #[test]
    fn extension_filter_sql() {
        let (sql, params) = ExtensionFilter::Only(vec!["rs".into(), "t_s".into()]).sql("f.path");
        assert_eq!(
            sql,
            "(f.path LIKE ? ESCAPE '\\' OR f.path LIKE ? ESCAPE '\\')"
        );
        assert_eq!(params, vec!["%.rs", "%.t\\_s"]);

        let (sql, params) = ExtensionFilter::Except(vec!["json".into()]).sql("p");
        assert_eq!(sql, "NOT (p LIKE ? ESCAPE '\\')");
        assert_eq!(params, vec!["%.json"]);

        assert_eq!(ExtensionFilter::Only(vec![]).sql("p").0, "0");
        assert_eq!(ExtensionFilter::Except(vec![]).sql("p").0, "1");
    }

    #[test]
    fn search_chunk_text_is_a_case_insensitive_substring_match() {
        let store = store_with(&[
            ("a.rs", "rust", "fn Needle_one() {}\n"),
            ("b.rs", "rust", "fn other() {}\n"),
            ("c.py", "python", "def needle_two(): pass\n"),
        ]);
        let all = ExtensionFilter::Except(vec![]);
        let mut paths: Vec<String> = store
            .search_chunk_text("NEEDLE", &all, 10, 0)
            .unwrap()
            .into_iter()
            .map(|h| h.file_path)
            .collect();
        paths.sort();
        assert_eq!(paths, vec!["a.rs", "c.py"]);

        // `(` and `"` are plain characters of the phrase.
        assert_eq!(
            store
                .search_chunk_text("needle_one()", &all, 10, 0)
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .search_chunk_text("say \"hi\"", &all, 10, 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn search_chunk_text_filters_files_before_the_limit() {
        let mut files: Vec<(String, String)> = (0..20)
            .map(|i| (format!("m{i:02}.rs"), "fn needle() {}\n".to_string()))
            .collect();
        files.push(("z.py".to_string(), "def needle(): pass\n".to_string()));
        let store = IndexStore::open_in_memory().unwrap();
        for (path, content) in &files {
            let language = if path.ends_with(".py") {
                "python"
            } else {
                "rust"
            };
            let file_id = store.insert_file(path, Some(language), content, 0).unwrap();
            store.insert_chunks(file_id, &[chunk(content, 1)]).unwrap();
        }

        let only_py = ExtensionFilter::Only(vec!["py".into()]);
        let hits = store.search_chunk_text("needle", &only_py, 2, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_path, "z.py");
        assert_eq!(hits[0].language.as_deref(), Some("python"));
        assert_eq!(hits[0].start_line, 1);

        let no_rs = ExtensionFilter::Except(vec!["rs".into()]);
        let hits = store.search_chunk_text("needle", &no_rs, 2, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_path, "z.py");

        // Paging walks the whole match set.
        let all = ExtensionFilter::Except(vec![]);
        let page1 = store.search_chunk_text("needle", &all, 15, 0).unwrap();
        let page2 = store.search_chunk_text("needle", &all, 15, 15).unwrap();
        assert_eq!((page1.len(), page2.len()), (15, 6));
    }

    #[test]
    fn search_chunk_text_short_needles_scan_the_chunks() {
        let store = store_with(&[
            ("a.rs", "rust", "let id = 1;\n"),
            ("b.rs", "rust", "let x = 100% + _y;\n"),
        ]);
        let all = ExtensionFilter::Except(vec![]);
        let hits = store.search_chunk_text("ID", &all, 10, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_path, "a.rs");
        // LIKE wildcards in the needle are literal.
        let hits = store.search_chunk_text("%", &all, 10, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file_path, "b.rs");
        let hits = store.search_chunk_text("_y", &all, 10, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(store.search_chunk_text("", &all, 10, 0).unwrap().is_empty());
    }

    #[test]
    fn text_index_follows_chunk_writes() {
        let store = store_with(&[("a.rs", "rust", "fn needle() {}\n")]);
        let all = ExtensionFilter::Except(vec![]);
        assert_eq!(store.count_text_indexed_chunks().unwrap(), 1);

        // Reindexing the file replaces its rows.
        let file_id = store.get_file_by_path("a.rs").unwrap().unwrap().id;
        store
            .insert_chunks(file_id, &[chunk("fn other() {}\n", 1)])
            .unwrap();
        assert!(
            store
                .search_chunk_text("needle", &all, 10, 0)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.search_chunk_text("other", &all, 10, 0).unwrap().len(),
            1
        );
        assert_eq!(store.count_text_indexed_chunks().unwrap(), 1);

        // Deleting the file (FK cascade) and clearing the index empty it.
        store.delete_file("a.rs").unwrap();
        assert_eq!(store.count_text_indexed_chunks().unwrap(), 0);
        let store = store_with(&[("a.rs", "rust", "fn needle() {}\n")]);
        store.clear_all_data().unwrap();
        assert_eq!(store.count_text_indexed_chunks().unwrap(), 0);
    }
}
