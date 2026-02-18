//! Dependency operations for IndexStore.

use super::IndexStore;
use crate::schema::DependencyRecord;
use anyhow::Result;
use rusqlite::params;
use std::collections::HashSet;

impl IndexStore {
    /// Insert a dependency record.
    pub fn insert_dependency(
        &self,
        source_file_id: i64,
        target_path: &str,
        import_name: Option<&str>,
        kind: &str,
        resolved_path: Option<&str>,
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO dependencies (source_file_id, target_path, import_name, kind, resolved_path)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![source_file_id, target_path, import_name, kind, resolved_path],
            )?;

            Ok(())
        })
    }

    /// Delete all dependencies for a file.
    pub fn delete_dependencies(&self, file_id: i64) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM dependencies WHERE source_file_id = ?1",
                [file_id],
            )?;
            Ok(())
        })
    }

    /// Get all dependencies for a file (what it imports).
    pub fn get_dependencies(&self, file_id: i64) -> Result<Vec<DependencyRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, source_file_id, target_path, import_name, kind, resolved_path
                 FROM dependencies WHERE source_file_id = ?1",
            )?;

            let results = stmt
                .query_map([file_id], |row| {
                    Ok(DependencyRecord {
                        id: row.get(0)?,
                        source_file_id: row.get(1)?,
                        target_path: row.get(2)?,
                        import_name: row.get(3)?,
                        kind: row.get(4)?,
                        resolved_path: row.get(5)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Get all files that depend on the given target path (reverse dependencies).
    ///
    /// Uses a two-phase approach:
    /// 1. **Exact match** via `resolved_path` — fast, no false positives.
    /// 2. **Fallback LIKE** for imports without a `resolved_path` — uses heuristic
    ///    pattern matching for backwards compatibility.
    ///
    /// # SQL Safety Invariant
    ///
    /// The dynamic SQL in the fallback phase is safe from injection because:
    /// 1. The number of `?N` placeholders is deterministic (5 or 6), derived from
    ///    the fixed set of pattern templates below — never from user input.
    /// 2. All actual values are passed as parameterized bind values via
    ///    `params.as_slice()`, never interpolated into the SQL string.
    /// 3. Special LIKE characters (`%`, `_`, `\`) in path components are escaped
    ///    via `escape_like()` before being used as bind values.
    pub fn get_dependents(&self, target_path: &str) -> Result<Vec<DependencyRecord>> {
        self.with_conn(|conn| {
            let mut seen_ids: HashSet<i64> = HashSet::new();
            let mut all_results: Vec<DependencyRecord> = Vec::new();

            // Phase 1: Exact match via resolved_path (fast, precise)
            {
                let mut stmt = conn.prepare(
                    "SELECT id, source_file_id, target_path, import_name, kind, resolved_path
                     FROM dependencies WHERE resolved_path = ?1",
                )?;

                let exact_results: Vec<DependencyRecord> = stmt
                    .query_map([target_path], |row| {
                        Ok(DependencyRecord {
                            id: row.get(0)?,
                            source_file_id: row.get(1)?,
                            target_path: row.get(2)?,
                            import_name: row.get(3)?,
                            kind: row.get(4)?,
                            resolved_path: row.get(5)?,
                        })
                    })?
                    .filter_map(|r| r.ok())
                    .filter(|r| seen_ids.insert(r.id))
                    .collect();

                all_results.extend(exact_results);
            }

            // Phase 2: Fallback LIKE for imports without resolved_path
            {
                let patterns = Self::build_dependent_patterns(target_path);

                // Safety: placeholder count is deterministic (patterns.len() is 5 or 6).
                let conditions: Vec<String> = (1..=patterns.len())
                    .map(|i| format!("target_path LIKE ?{} ESCAPE '\\'", i))
                    .collect();
                let query = format!(
                    "SELECT id, source_file_id, target_path, import_name, kind, resolved_path
                     FROM dependencies WHERE resolved_path IS NULL AND ({})",
                    conditions.join(" OR ")
                );

                let path = std::path::Path::new(target_path);
                let basename = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(target_path);
                let filename = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(target_path);

                let mut stmt = conn.prepare(&query)?;
                let params: Vec<&dyn rusqlite::ToSql> =
                    patterns.iter().map(|p| p as &dyn rusqlite::ToSql).collect();

                let basename_lower = basename.to_lowercase();

                let fallback_results: Vec<DependencyRecord> = stmt
                    .query_map(params.as_slice(), |row| {
                        Ok(DependencyRecord {
                            id: row.get(0)?,
                            source_file_id: row.get(1)?,
                            target_path: row.get(2)?,
                            import_name: row.get(3)?,
                            kind: row.get(4)?,
                            resolved_path: row.get(5)?,
                        })
                    })?
                    .filter_map(|r| r.ok())
                    .filter(|r| {
                        let import = &r.target_path;
                        let import_lower = import.to_lowercase();
                        import.ends_with(basename)
                            || import.ends_with(filename)
                            || import.ends_with(&format!("{}.ts", basename))
                            || import.ends_with(&format!("{}.tsx", basename))
                            || import.ends_with(&format!("{}.js", basename))
                            || import.ends_with(&format!("{}.jsx", basename))
                            || import.ends_with(&format!("{}.rs", basename))
                            || import_lower.ends_with(&basename_lower)
                    })
                    .filter(|r| seen_ids.insert(r.id))
                    .collect();

                all_results.extend(fallback_results);
            }

            Ok(all_results)
        })
    }

    /// Build LIKE patterns for reverse dependency matching.
    ///
    /// Returns 5 or 6 patterns depending on whether the path has a parent
    /// directory component. All path components are LIKE-escaped so that
    /// `%`, `_`, and `\` in filenames are treated literally.
    fn build_dependent_patterns(target_path: &str) -> Vec<String> {
        let path = std::path::Path::new(target_path);
        let basename = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(target_path);
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(target_path);
        let parent_and_name = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .map(|parent| format!("{}/{}", parent, basename));

        let mut patterns = vec![
            format!("%{}", escape_like(filename)),
            format!("%/{}", escape_like(basename)),
            format!("./{}", escape_like(basename)),
            format!("../{}", escape_like(basename)),
            format!("%{}", escape_like(basename)),
        ];

        if let Some(ref parent_name) = parent_and_name {
            patterns.push(format!("%{}", escape_like(parent_name)));
        }

        patterns
    }
}

/// Escape special LIKE characters so they are matched literally.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
