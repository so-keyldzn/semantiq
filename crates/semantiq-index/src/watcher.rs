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

    pub fn unwatch(&mut self, path: &Path) -> Result<()> {
        self.watcher.unwatch(path)?;
        self.watched_paths.retain(|p| p != path);
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
            // Skip non-files
            if path.is_dir() {
                continue;
            }

            // Skip excluded paths (hidden dirs, node_modules, etc.). Evaluated on
            // the path relative to the watched root, like `AutoIndexer::index_file`:
            // a project living under e.g. `~/.config/app` or a `.tmpXXXX` dir
            // would otherwise have every event dropped.
            let rel = watched_roots
                .iter()
                .find_map(|root| path.strip_prefix(root).ok())
                .unwrap_or(&path);
            if should_exclude_path(rel) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_event_ignores_excluded_ancestors_of_root() {
        use notify::EventKind;
        use notify::event::{CreateKind, ModifyKind};

        let root = PathBuf::from("/home/u/.config/app");
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

    #[test]
    fn test_watcher_creation() {
        let watcher = FileWatcher::new();
        assert!(watcher.is_ok());
    }
}
