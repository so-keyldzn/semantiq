//! End-to-end tests of `semantiq update`: the download → checksum → extract →
//! swap flow against a local HTTP server standing in for GitHub (#122).
//!
//! `SEMANTIQ_UPDATE_BASE_URL` (see `semantiq_mcp::self_update::BASE_URL_ENV`)
//! points the binary at the server; the binary under test is a copy of the
//! built one, so the shared build directory is never touched.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use tempfile::TempDir;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const NEW_VERSION: &str = "99.0.0";
const NEW_BINARY: &[u8] = b"#!/bin/sh\necho fake new semantiq\n";

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "semantiq.exe"
    } else {
        "semantiq"
    }
}

/// A minimal HTTP/1.1 server: `routes` maps a path to (status, body). Every
/// other path answers 404. Each request is logged in `hits`.
struct Server {
    base_url: String,
    hits: Arc<Mutex<Vec<String>>>,
}

fn serve(routes: HashMap<String, (u16, Vec<u8>)>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let routes = Arc::new(routes);
    let thread_hits = Arc::clone(&hits);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let routes = Arc::clone(&routes);
            let hits = Arc::clone(&thread_hits);
            thread::spawn(move || handle(stream, &routes, &hits));
        }
    });
    Server {
        base_url: format!("http://127.0.0.1:{port}"),
        hits,
    }
}

fn handle(
    mut stream: TcpStream,
    routes: &HashMap<String, (u16, Vec<u8>)>,
    hits: &Mutex<Vec<String>>,
) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head);
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    hits.lock().unwrap().push(path.clone());
    let (status, body) = routes
        .get(&path)
        .cloned()
        .unwrap_or((404, b"not found".to_vec()));
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The release artifact triple `self_update` derives from the running OS/arch.
fn target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        other => panic!("unsupported test platform {other:?}"),
    }
}

/// A `.tar.gz` holding `semantiq` (or `semantiq.exe`) with `content`, built
/// with the same `tar` the updater extracts with.
fn release_archive(workdir: &Path, content: &[u8]) -> Vec<u8> {
    let payload = workdir.join("payload");
    fs::create_dir_all(&payload).unwrap();
    fs::write(payload.join(binary_name()), content).unwrap();
    let archive = workdir.join("release.tar.gz");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&payload)
        .arg(binary_name())
        .status()
        .expect("tar is installed");
    assert!(status.success());
    fs::read(&archive).unwrap()
}

/// Routes of a fake GitHub publishing `NEW_VERSION` with `archive` and the
/// given `.sha256` sidecar.
fn github_routes(archive: &[u8], sidecar: &str) -> HashMap<String, (u16, Vec<u8>)> {
    let archive_name = format!("semantiq-v{NEW_VERSION}-{}.tar.gz", target_triple());
    let download = format!("/releases/download/v{NEW_VERSION}/{archive_name}");
    HashMap::from([
        (
            "/releases/latest".to_string(),
            (
                200,
                format!(r#"{{"tag_name":"v{NEW_VERSION}","name":"v{NEW_VERSION}"}}"#).into_bytes(),
            ),
        ),
        (download.clone(), (200, archive.to_vec())),
        (
            format!("{download}.sha256"),
            (200, sidecar.as_bytes().to_vec()),
        ),
    ])
}

/// A private copy of the built binary: `update` replaces the executable it
/// runs from, which must never be the one in the shared build directory.
fn installed_copy(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let exe = bin_dir.join(binary_name());
    fs::copy(env!("CARGO_BIN_EXE_semantiq"), &exe).unwrap();
    exe
}

fn run_update(exe: &Path, server: &Server, args: &[&str]) -> Output {
    let mut cmd = Command::new(exe);
    cmd.arg("update")
        .args(args)
        .env("SEMANTIQ_UPDATE_CHECK", "false")
        .env("SEMANTIQ_UPDATE_BASE_URL", &server.base_url);
    // The local server must be reached directly, whatever the shell's proxy.
    for var in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        cmd.env_remove(var);
    }
    cmd.output().expect("failed to run semantiq update")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn update_check_reports_the_newer_release_without_installing() {
    let dir = TempDir::new().unwrap();
    let archive = release_archive(dir.path(), NEW_BINARY);
    let server = serve(github_routes(&archive, &sha256_hex(&archive)));
    let exe = installed_copy(dir.path());
    let before = fs::read(&exe).unwrap();

    let output = run_update(&exe, &server, &["--check"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains(&format!(
            "Update available: v{CURRENT_VERSION} -> v{NEW_VERSION}"
        )),
        "{out}"
    );
    assert_eq!(fs::read(&exe).unwrap(), before, "--check must not install");
    let hits = server.hits.lock().unwrap().clone();
    assert_eq!(
        hits,
        vec!["/releases/latest".to_string()],
        "--check downloads nothing"
    );
}

#[test]
fn update_downloads_verifies_extracts_and_swaps_the_binary() {
    let dir = TempDir::new().unwrap();
    let archive = release_archive(dir.path(), NEW_BINARY);
    // Standard `sha256sum` sidecar format, as release.yml writes it.
    let sidecar = format!(
        "{}  semantiq-v{NEW_VERSION}-{}.tar.gz\n",
        sha256_hex(&archive),
        target_triple()
    );
    let server = serve(github_routes(&archive, &sidecar));
    let exe = installed_copy(dir.path());

    let output = run_update(&exe, &server, &[]);
    assert!(
        output.status.success(),
        "{}\n{}",
        stdout(&output),
        stderr(&output)
    );
    let out = stdout(&output);
    assert!(out.contains("Checksum verified."), "{out}");
    assert!(
        out.contains(&format!(
            "Updated semantiq v{CURRENT_VERSION} -> v{NEW_VERSION}."
        )),
        "{out}"
    );

    // The executable path now holds the archive's binary.
    assert_eq!(fs::read(&exe).unwrap(), NEW_BINARY);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&exe).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "installed binary is executable: {mode:o}"
        );
    }

    // Nothing is left behind next to the binary but (on Windows) the moved-aside image.
    let leftovers: Vec<String> = fs::read_dir(exe.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != binary_name() && !name.ends_with(".old"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    let hits = server.hits.lock().unwrap().clone();
    assert_eq!(hits.len(), 3, "latest, archive, sidecar: {hits:?}");
}

#[test]
fn update_refuses_an_archive_whose_checksum_does_not_match() {
    let dir = TempDir::new().unwrap();
    let archive = release_archive(dir.path(), NEW_BINARY);
    let wrong = sha256_hex(b"something else");
    let server = serve(github_routes(&archive, &wrong));
    let exe = installed_copy(dir.path());
    let before = fs::read(&exe).unwrap();

    let output = run_update(&exe, &server, &[]);
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("checksum mismatch"), "{err}");
    assert_eq!(
        fs::read(&exe).unwrap(),
        before,
        "a mismatching archive is never installed"
    );
}

#[test]
fn update_refuses_an_archive_without_a_checksum_sidecar() {
    let dir = TempDir::new().unwrap();
    let archive = release_archive(dir.path(), NEW_BINARY);
    let mut routes = github_routes(&archive, "unused");
    routes.retain(|path, _| !path.ends_with(".sha256"));
    let server = serve(routes);
    let exe = installed_copy(dir.path());
    let before = fs::read(&exe).unwrap();

    let output = run_update(&exe, &server, &[]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(err.contains("could not verify integrity"), "{err}");
    assert_eq!(fs::read(&exe).unwrap(), before);
}

#[test]
fn update_is_a_no_op_when_already_latest() {
    let dir = TempDir::new().unwrap();
    let routes = HashMap::from([(
        "/releases/latest".to_string(),
        (
            200,
            format!(r#"{{"tag_name":"v{CURRENT_VERSION}"}}"#).into_bytes(),
        ),
    )]);
    let server = serve(routes);
    let exe = installed_copy(dir.path());
    let before = fs::read(&exe).unwrap();

    let output = run_update(&exe, &server, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains(&format!("Already up to date (v{CURRENT_VERSION}).")),
        "{}",
        stdout(&output)
    );
    assert_eq!(fs::read(&exe).unwrap(), before);
    // The answer came from the local server, not from GitHub.
    let hits = server.hits.lock().unwrap().clone();
    assert_eq!(hits, vec!["/releases/latest".to_string()]);
}
