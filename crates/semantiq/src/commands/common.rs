//! Common utilities and constants for CLI commands

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Default database filename
pub const DEFAULT_DB_NAME: &str = ".semantiq.db";

/// Resolves a path to an absolute project root path.
/// If the path is relative, it's joined with the current directory.
pub fn resolve_project_root(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// Returns the database path, using the provided path or defaulting to
/// `DEFAULT_DB_NAME` in the project root.
pub fn resolve_db_path(database: Option<PathBuf>, project_root: &Path) -> PathBuf {
    database.unwrap_or_else(|| project_root.join(DEFAULT_DB_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_resolve_project_root_absolute() {
        let path = Path::new("/tmp/test");
        let result = resolve_project_root(path).unwrap();
        assert_eq!(result, PathBuf::from("/tmp/test"));
    }

    #[test]
    fn test_resolve_project_root_relative() {
        let path = Path::new(".");
        let result = resolve_project_root(path).unwrap();
        assert_eq!(result, env::current_dir().unwrap());
    }

    #[test]
    fn test_resolve_db_path_with_provided() {
        let db = Some(PathBuf::from("/custom/path.db"));
        let project = Path::new("/project");
        let result = resolve_db_path(db, project);
        assert_eq!(result, PathBuf::from("/custom/path.db"));
    }

    #[test]
    fn test_resolve_db_path_default() {
        let project = Path::new("/project");
        let result = resolve_db_path(None, project);
        assert_eq!(result, PathBuf::from("/project/.semantiq.db"));
    }
}
