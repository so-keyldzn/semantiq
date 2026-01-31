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

            let mut all_results = Vec::new();
            let mut seen_ids: HashSet<i64> = HashSet::new();

            for pattern in patterns {
                let mut stmt = conn.prepare(
                    "SELECT id, source_file_id, target_path, import_name, kind
                     FROM dependencies WHERE target_path LIKE ?1 ESCAPE '\\'",
                )?;

                let results = stmt
                    .query_map([&pattern], |row| {
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
                        // Additional validation
                        let import = &r.target_path;
                        let import_lower = import.to_lowercase();
                        let basename_lower = basename.to_lowercase();
                        import.ends_with(basename)
                            || import.ends_with(filename)
                            || import.ends_with(&format!("{}.ts", basename))
                            || import.ends_with(&format!("{}.tsx", basename))
                            || import.ends_with(&format!("{}.js", basename))
                            || import.ends_with(&format!("{}.jsx", basename))
                            || import.ends_with(&format!("{}.rs", basename))
                            || import_lower.ends_with(&basename_lower)
                    });

                for record in results {
                    if seen_ids.insert(record.id) {
                        all_results.push(record);
                    }
                }
            }

            Ok(all_results)
        })
    }
}
