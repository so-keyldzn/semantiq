use crate::exclusions::should_exclude_path;
use anyhow::Result;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;
use tracing::{debug, error, info};

pub enum FileEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}

pub struct FileWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<Result<Event, notify::Error>>,
    watched_paths: Vec<PathBuf>,
}

impl FileWatcher {
    pub fn new() -> Result<Self> {
        let (tx, rx) = channel();

        let watcher = RecommendedWatcher::new(
            move |res| {
                let _ = tx.send(res);
            },
            Config::default().with_poll_interval(Duration::from_secs(2)),
        )?;

        Ok(Self {
            watcher,
            receiver: rx,
            watched_paths: Vec::new(),
        })
    }

    pub fn watch(&mut self, path: &Path) -> Result<()> {
        info!("Watching directory: {:?}", path);
        self.watcher.watch(path, RecursiveMode::Recursive)?;
        self.watched_paths.push(path.to_path_buf());
        Ok(())
    }

    pub fn poll_events(&self) -> Vec<FileEvent> {
        let mut events = Vec::new();

        while let Ok(result) = self.receiver.try_recv() {
            match result {
                Ok(event) => {
                    debug!("File event: {:?}", event);
                    events.extend(Self::convert_event(event, &self.watched_paths));
                }
                Err(e) => {
                    error!("Watch error: {:?}", e);
                }
            }
        }

        events
    }

    fn convert_event(event: Event, watched_roots: &[PathBuf]) -> Vec<FileEvent> {
        use notify::EventKind;

        let mut file_events = Vec::new();

        for path in event.paths {
            // Directories are reported too: a rename or removal of a
            // directory is one event on it, not on its files, and the indexer
            // walks or prunes its subtree (`AutoIndexer::process_file_events`).

            // Skip excluded names (.git, node_modules, .semantiq*, etc.) early,
            // before the event reaches the indexer, which then also applies the
            // project's ignore rules. Evaluated on the path relative to the
            // watched root, like `AutoIndexer::index_file`: a project living
            // under e.g. `~/.cache/app` would otherwise have every event dropped.
            // `.git/info/exclude` is the one path under `.git` the indexer
            // wants to hear about: it changes which files the walk reaches.
            let rel = watched_roots
                .iter()
                .find_map(|root| path.strip_prefix(root).ok())
                .unwrap_or(&path);
            if should_exclude_path(rel) && !is_git_exclude_file(rel) {
                debug!("Skipping excluded path event: {:?}", path);
                continue;
            }

            match event.kind {
                EventKind::Create(_) => {
                    file_events.push(FileEvent::Created(path));
                }
                EventKind::Modify(_) => {
                    file_events.push(FileEvent::Modified(path));
                }
                EventKind::Remove(_) => {
                    file_events.push(FileEvent::Deleted(path));
                }
                _ => {}
            }
        }

        file_events
    }

    pub fn watched_paths(&self) -> &[PathBuf] {
        &self.watched_paths
    }
}

/// Whether `rel` (relative to the watched root) is `.git/info/exclude`.
fn is_git_exclude_file(rel: &Path) -> bool {
    rel == Path::new(".git/info/exclude")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_event_ignores_excluded_ancestors_of_root() {
        use notify::EventKind;
        use notify::event::{CreateKind, ModifyKind};

        // `.cache` is an excluded name; `.config` is not (it holds project
        // configuration), so it would not exercise the rule.
        let root = PathBuf::from("/home/u/.cache/app");
        let roots = vec![root.clone()];

        let event =
            Event::new(EventKind::Create(CreateKind::File)).add_path(root.join("src/main.rs"));
        let events = FileWatcher::convert_event(event, &roots);
        assert!(matches!(events.as_slice(), [FileEvent::Created(_)]));

        // Exclusions inside the project still apply.
        let event = Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(root.join("node_modules/x/index.js"));
        assert!(FileWatcher::convert_event(event, &roots).is_empty());
    }

    /// Directory events reach the indexer (#85), and so does a change to
    /// `.git/info/exclude` despite `.git` being excluded (#138).
    #[test]
    fn test_convert_event_keeps_directories_and_git_exclude() {
        use notify::EventKind;
        use notify::event::{CreateKind, ModifyKind, RemoveKind};

        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("src/new")).unwrap();
        let roots = vec![root.clone()];

        let event =
            Event::new(EventKind::Create(CreateKind::Folder)).add_path(root.join("src/new"));
        assert!(matches!(
            FileWatcher::convert_event(event, &roots).as_slice(),
            [FileEvent::Created(p)] if p == &root.join("src/new")
        ));
        let event =
            Event::new(EventKind::Remove(RemoveKind::Folder)).add_path(root.join("src/old"));
        assert!(matches!(
            FileWatcher::convert_event(event, &roots).as_slice(),
            [FileEvent::Deleted(_)]
        ));

        let event =
            Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join(".git/info/exclude"));
        assert!(matches!(
            FileWatcher::convert_event(event, &roots).as_slice(),
            [FileEvent::Modified(_)]
        ));
        let event =
            Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join(".git/index"));
        assert!(FileWatcher::convert_event(event, &roots).is_empty());
    }

    #[test]
    fn test_watcher_creation() {
        let watcher = FileWatcher::new();
        assert!(watcher.is_ok());
    }
}
