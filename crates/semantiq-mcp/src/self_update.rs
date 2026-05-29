//! In-place self-update of the `semantiq` binary.
//!
//! Mirrors the install logic of `npm/scripts/install.js`: download the release
//! archive for the current platform from GitHub, verify its published SHA256
//! checksum, extract the binary, and atomically replace the running executable.
//!
//! The integrity check is mandatory — an archive whose checksum cannot be
//! fetched or does not match is never extracted or installed.

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::debug;

use crate::version_check::{self, REPO};

/// Network timeout for downloading the release archive. Generous because the
/// archive can be tens of megabytes on a slow connection.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Hard cap on archive size to avoid unbounded memory use (256 MiB).
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
/// Cap on the tiny `.sha256` checksum file.
const MAX_CHECKSUM_BYTES: u64 = 1024;
/// Timeout for the fast version/checksum metadata requests.
const META_TIMEOUT: Duration = Duration::from_secs(10);

/// Options controlling an update run.
#[derive(Debug, Clone, Copy)]
pub struct UpdateOptions {
    /// Only report whether an update is available; do not download or install.
    pub check_only: bool,
    /// Reinstall the latest release even if it matches the current version.
    pub force: bool,
}

/// Outcome of an update run, for callers that want to react programmatically.
#[derive(Debug)]
pub enum UpdateOutcome {
    /// Already running the latest version (and `force` was not set).
    AlreadyLatest { version: String },
    /// A newer version is available; `check_only` prevented installing it.
    UpdateAvailable { current: String, latest: String },
    /// The binary was replaced in place.
    Updated { from: String, to: String },
}

/// Map the compile-time target OS/arch to the release artifact triple used in
/// the GitHub release asset names. Returns `None` on unsupported platforms.
fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "semantiq.exe"
    } else {
        "semantiq"
    }
}

/// Build a configured ureq agent. ureq follows redirects by default, which is
/// required since GitHub release downloads redirect to a CDN.
fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(timeout))
            // Reject plaintext HTTP and any redirect that downgrades to it:
            // integrity rests entirely on the TLS channel.
            .https_only(true)
            .build(),
    )
}

/// Build an unpredictable, process-unique scratch name to avoid symlink/TOCTOU
/// races on shared temp directories.
fn unique_scratch_name(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{}-{}", std::process::id(), nanos)
}

fn download_bytes(url: &str, timeout: Duration, max_bytes: u64) -> Result<Vec<u8>> {
    let response = agent(timeout)
        .get(url)
        .header("User-Agent", "semantiq-self-update")
        .call()
        .with_context(|| format!("request failed: {url}"))?;

    let mut buf = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(max_bytes)
        .read_to_end(&mut buf)
        .context("failed to read response body")?;
    Ok(buf)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// The published `.sha256` may be a bare hex digest or the standard
/// `sha256sum` format ("<hex>  <filename>"). Extract the leading hex token.
fn parse_expected_sha256(raw: &str) -> Result<String> {
    let hex = raw.split_whitespace().next().unwrap_or("").to_lowercase();
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(hex)
    } else {
        bail!("malformed checksum file");
    }
}

/// Extract `archive` (a `.tar.gz`) into `dest_dir` by shelling out to `tar`,
/// matching the npm installer. `tar` is present on macOS, Linux, and Windows 10+.
fn extract_tar_gz(archive: &Path, dest_dir: &Path) -> Result<()> {
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest_dir)
        .status()
        .context("failed to spawn `tar` (is it installed and on PATH?)")?;
    if !status.success() {
        bail!("`tar` exited with status {status}");
    }
    Ok(())
}

/// Atomically replace the currently running executable with `new_bin`.
///
/// On Unix, `rename` over a running executable works and is atomic when both
/// paths are on the same filesystem. On Windows the running image cannot be
/// overwritten, so the current exe is first moved aside to `<exe>.old`.
fn replace_current_exe(new_bin: &Path) -> Result<()> {
    let current = std::env::current_exe().context("cannot locate current executable")?;
    let current = std::fs::canonicalize(&current).unwrap_or(current);
    let dir = current
        .parent()
        .ok_or_else(|| anyhow!("executable has no parent directory"))?;

    // Stage the new binary next to the target so the final rename stays on the
    // same filesystem (cross-device rename would fail).
    let staged = dir.join(unique_scratch_name(".semantiq-update"));
    std::fs::copy(new_bin, &staged).context("failed to stage new binary")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .context("failed to set executable permissions")?;
    }

    let result = (|| -> Result<()> {
        #[cfg(windows)]
        {
            let backup = current.with_extension("old");
            let _ = std::fs::remove_file(&backup);
            std::fs::rename(&current, &backup)
                .context("failed to move running executable aside")?;
            std::fs::rename(&staged, &current).context("failed to install new binary")?;
            // Best-effort cleanup; the old image may still be locked while running.
            let _ = std::fs::remove_file(&backup);
            Ok(())
        }
        #[cfg(not(windows))]
        {
            std::fs::rename(&staged, &current).context("failed to install new binary")?;
            Ok(())
        }
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// Check for and (unless `check_only`) install the latest release.
///
/// `current_version` is the running binary's version (typically
/// `env!("CARGO_PKG_VERSION")`). Progress is printed to stdout; this is a
/// foreground CLI operation, not the MCP server, so stdout is fine.
pub fn run(current_version: &str, opts: UpdateOptions) -> Result<UpdateOutcome> {
    println!("Checking for updates...");

    let latest = version_check::fetch_latest_uncached(META_TIMEOUT)
        .ok_or_else(|| anyhow!("could not reach GitHub to check the latest version"))?;

    let newer = version_check::is_version_newer(&latest, current_version);
    debug!(current = current_version, latest = %latest, newer, "update check");

    if !newer && !opts.force {
        println!("Already up to date (v{current_version}).");
        return Ok(UpdateOutcome::AlreadyLatest {
            version: current_version.to_string(),
        });
    }

    if opts.check_only {
        println!("Update available: v{current_version} -> v{latest}");
        println!("Run `semantiq update` to install it.");
        return Ok(UpdateOutcome::UpdateAvailable {
            current: current_version.to_string(),
            latest,
        });
    }

    let target = target_triple().ok_or_else(|| {
        anyhow!(
            "unsupported platform: {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;

    let archive_name = format!("semantiq-v{latest}-{target}.tar.gz");
    let base = format!("https://github.com/{REPO}/releases/download/v{latest}/{archive_name}");
    let checksum_url = format!("{base}.sha256");

    println!("Downloading {archive_name}...");
    let archive_bytes = download_bytes(&base, DOWNLOAD_TIMEOUT, MAX_ARCHIVE_BYTES)
        .with_context(|| format!("failed to download {archive_name}"))?;

    // Integrity verification: never install an unverified binary.
    let checksum_raw = download_bytes(&checksum_url, META_TIMEOUT, MAX_CHECKSUM_BYTES)
        .map_err(|e| anyhow!("could not verify integrity (missing {archive_name}.sha256): {e}"))
        .and_then(|b| String::from_utf8(b).context("checksum file is not valid UTF-8"))?;
    let expected = parse_expected_sha256(&checksum_raw)?;
    let actual = sha256_hex(&archive_bytes);
    if actual != expected {
        bail!("checksum mismatch for {archive_name}\n  expected: {expected}\n  actual:   {actual}");
    }
    println!("Checksum verified.");

    // Extract into a unique temp dir, then atomically swap the binary in.
    let tmp_dir = std::env::temp_dir().join(unique_scratch_name("semantiq-update"));
    // create_dir (not create_dir_all) fails if the path already exists, defeating
    // a pre-created symlink on a shared temp dir.
    std::fs::create_dir(&tmp_dir).context("failed to create temp directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp_dir, std::fs::Permissions::from_mode(0o700))
            .context("failed to restrict temp directory permissions")?;
    }
    let cleanup = TmpDir(tmp_dir.clone());

    let archive_path = tmp_dir.join(&archive_name);
    std::fs::write(&archive_path, &archive_bytes).context("failed to write archive to disk")?;
    extract_tar_gz(&archive_path, &tmp_dir)?;

    let extracted = locate_binary(&tmp_dir)
        .ok_or_else(|| anyhow!("extracted archive did not contain a `{}`", binary_name()))?;
    replace_current_exe(&extracted)?;
    drop(cleanup);

    println!("Updated semantiq v{current_version} -> v{latest}.");
    Ok(UpdateOutcome::Updated {
        from: current_version.to_string(),
        to: latest,
    })
}

/// Find the extracted binary, allowing for archives that nest it in a folder.
fn locate_binary(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join(binary_name());
    if direct.is_file() {
        return Some(direct);
    }
    // Fall back to a shallow scan of immediate subdirectories.
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let candidate = entry.path().join(binary_name());
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// RAII guard that removes the temp extraction directory on drop.
struct TmpDir(PathBuf);

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_checksum() {
        let hex = "a".repeat(64);
        assert_eq!(parse_expected_sha256(&hex).unwrap(), hex);
    }

    #[test]
    fn parse_sha256sum_format() {
        let hex = "b".repeat(64);
        let raw = format!("{hex}  semantiq-v1.0.0-x86_64-apple-darwin.tar.gz");
        assert_eq!(parse_expected_sha256(&raw).unwrap(), hex);
    }

    #[test]
    fn parse_uppercase_normalized() {
        let raw = "C".repeat(64);
        assert_eq!(parse_expected_sha256(&raw).unwrap(), "c".repeat(64));
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(parse_expected_sha256("not-a-hash").is_err());
        assert!(parse_expected_sha256("").is_err());
        assert!(parse_expected_sha256(&"z".repeat(64)).is_err());
    }

    #[test]
    fn sha256_matches_known_vector() {
        // SHA256("") = e3b0c442...
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn current_platform_is_supported() {
        // The build targets we ship for should all resolve.
        if cfg!(any(
            target_os = "macos",
            target_os = "linux",
            target_os = "windows"
        )) {
            assert!(target_triple().is_some());
        }
    }
}
