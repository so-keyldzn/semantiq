use serde::{Deserialize, Serialize};
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

const GITHUB_API_URL: &str = "https://api.github.com/repos/so-keyldzn/semantiq/releases/latest";
const CACHE_FILE: &str = "version_cache.json";
const DEFAULT_TIMEOUT_MS: u64 = 3000;
const DEFAULT_CACHE_HOURS: u64 = 24;

/// Thread-safe flag to disable update checks without using unsafe env vars
static UPDATE_CHECK_DISABLED: AtomicBool = AtomicBool::new(false);

/// Disable update checks in a thread-safe manner.
/// This should be called at startup before any version checks are performed.
pub fn disable_update_check() {
    UPDATE_CHECK_DISABLED.store(true, Ordering::SeqCst);
}

/// Check if update checks are disabled via the thread-safe flag
fn is_update_check_disabled() -> bool {
    UPDATE_CHECK_DISABLED.load(Ordering::SeqCst)
}

#[derive(Debug, Clone)]
pub struct VersionCheckConfig {
    pub enabled: bool,
    pub cache_duration: Duration,
    pub timeout: Duration,
}

impl Default for VersionCheckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_duration: Duration::from_secs(DEFAULT_CACHE_HOURS * 60 * 60),
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
        }
    }
}

impl VersionCheckConfig {
    pub fn from_env() -> Self {
        // Check both the thread-safe flag and the environment variable
        let enabled = !is_update_check_disabled()
            && std::env::var("SEMANTIQ_UPDATE_CHECK")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true);

        let cache_hours: u64 = std::env::var("SEMANTIQ_UPDATE_CACHE_HOURS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_CACHE_HOURS);

        Self {
            enabled,
            cache_duration: Duration::from_secs(cache_hours * 60 * 60),
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
        }
    }
}

#[derive(Debug)]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct VersionCache {
    latest_version: String,
    checked_at: u64,
}

impl VersionCache {
    fn is_expired(&self, max_age: Duration) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now - self.checked_at > max_age.as_secs()
    }
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

fn get_cache_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("semantiq")
        .join(CACHE_FILE)
}

fn read_cache() -> Option<VersionCache> {
    let path = get_cache_path();
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_cache(cache: &VersionCache) {
    let path = get_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(content) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(&path, content);
    }
}

fn fetch_latest_version(timeout: Duration) -> Option<String> {
    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(timeout))
            .build(),
    );

    let response = agent
        .get(GITHUB_API_URL)
        .header("User-Agent", "semantiq-version-checker")
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .ok()?;

    // Limit response body to 10KB to prevent memory exhaustion
    let mut body = String::new();
    response
        .into_body()
        .as_reader()
        .take(10 * 1024)
        .read_to_string(&mut body)
        .ok()?;
    let release: GitHubRelease = serde_json::from_str(&body).ok()?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    Some(version)
}

fn is_newer(latest: &str, current: &str) -> bool {
    let parse_version = |v: &str| -> Vec<u32> {
        v.trim_start_matches('v')
            .split('.')
            .filter_map(|s| s.parse().ok())
            .collect()
    };

    let latest_parts = parse_version(latest);
    let current_parts = parse_version(current);

    for i in 0..3 {
        let l = latest_parts.get(i).copied().unwrap_or(0);
        let c = current_parts.get(i).copied().unwrap_or(0);
        if l > c {
            return true;
        }
        if l < c {
            return false;
        }
    }
    false
}

/// Check for updates, using cache when available.
/// Returns None if check is disabled or fails.
pub fn check_for_update(current_version: &str, config: &VersionCheckConfig) -> Option<UpdateInfo> {
    if !config.enabled {
        debug!("Version check disabled");
        return None;
    }

    // Check cache first
    if let Some(cache) = read_cache()
        && !cache.is_expired(config.cache_duration)
    {
        debug!("Using cached version info: {}", cache.latest_version);
        return Some(UpdateInfo {
            current_version: current_version.to_string(),
            latest_version: cache.latest_version.clone(),
            update_available: is_newer(&cache.latest_version, current_version),
        });
    }

    // Fetch from GitHub
    let latest = fetch_latest_version(config.timeout)?;

    // Update cache
    let cache = VersionCache {
        latest_version: latest.clone(),
        checked_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    write_cache(&cache);

    Some(UpdateInfo {
        current_version: current_version.to_string(),
        latest_version: latest.clone(),
        update_available: is_newer(&latest, current_version),
    })
}

/// Display update notification via warn! macro
pub fn notify_update(info: &UpdateInfo) {
    if info.update_available {
        warn!(
            "Update available: {} -> {} | Run: npm install -g semantiq-mcp | Or: https://github.com/so-keyldzn/semantiq/releases",
            info.current_version, info.latest_version
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer() {
        assert!(is_newer("0.3.0", "0.2.6"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.2.7", "0.2.6"));
        assert!(!is_newer("0.2.6", "0.2.6"));
        assert!(!is_newer("0.2.5", "0.2.6"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn test_is_newer_with_v_prefix() {
        assert!(is_newer("v0.3.0", "0.2.6"));
        assert!(is_newer("0.3.0", "v0.2.6"));
        assert!(is_newer("v0.3.0", "v0.2.6"));
    }
}
