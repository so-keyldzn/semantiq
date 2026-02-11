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
    ) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO dependencies (source_file_id, target_path, import_name, kind)
                 VALUES (?1, ?2, ?3, ?4)",
                params![source_file_id, target_path, import_name, kind],
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
                "SELECT id, source_file_id, target_path, import_name, kind
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
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;

            Ok(results)
        })
    }

    /// Get all files that depend on the given target path (reverse dependencies).
    ///
    /// Uses a single SQL query with OR conditions instead of multiple separate queries.
    pub fn get_dependents(&self, target_path: &str) -> Result<Vec<DependencyRecord>> {
        self.with_conn(|conn| {
            // Extract the file basename without extension for flexible matching
            let path = std::path::Path::new(target_path);
            let basename = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(target_path);

            // Get filename with extension if present
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(target_path);

            // Also get the parent path components for more precise matching
            let parent_and_name = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .map(|parent| format!("{}/{}", parent, basename));

            // Escape special LIKE characters
            fn escape_like(s: &str) -> String {
                s.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            }

            // Build patterns to match various import styles
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

            // Build a single query with OR conditions instead of multiple queries
            let conditions: Vec<String> = (1..=patterns.len())
                .map(|i| format!("target_path LIKE ?{} ESCAPE '\\'", i))
                .collect();
            let query = format!(
                "SELECT id, source_file_id, target_path, import_name, kind
                 FROM dependencies WHERE {}",
                conditions.join(" OR ")
            );

            let mut stmt = conn.prepare(&query)?;
            let params: Vec<&dyn rusqlite::ToSql> =
                patterns.iter().map(|p| p as &dyn rusqlite::ToSql).collect();

            let mut seen_ids: HashSet<i64> = HashSet::new();
            let basename_lower = basename.to_lowercase();

            let all_results = stmt
                .query_map(params.as_slice(), |row| {
                    Ok(DependencyRecord {
                        id: row.get(0)?,
                        source_file_id: row.get(1)?,
                        target_path: row.get(2)?,
                        import_name: row.get(3)?,
                        kind: row.get(4)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .filter(|r| {
                    // Additional validation to reduce false positives
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

            Ok(all_results)
        })
    }
}
