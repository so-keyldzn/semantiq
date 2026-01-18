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
                    events.extend(Self::convert_event(event));
                }
                Err(e) => {
                    error!("Watch error: {:?}", e);
                }
            }
        }

        events
    }

    fn convert_event(event: Event) -> Vec<FileEvent> {
        use notify::EventKind;

        let mut file_events = Vec::new();

        for path in event.paths {
            // Skip non-files
            if path.is_dir() {
                continue;
            }

            // Skip excluded paths (hidden dirs, node_modules, etc.)
            if should_exclude_path(&path) {
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
    fn test_watcher_creation() {
        let watcher = FileWatcher::new();
        assert!(watcher.is_ok());
    }
}
