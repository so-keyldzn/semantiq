//! `semantiq serve` on stdio: the MCP handshake and `tools/list` through the
//! binary's stdin/stdout, and the "stdout is reserved for protocol messages"
//! rule — every byte the server writes to stdout is a JSON-RPC message, the
//! logs go to stderr (#122).

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn project() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn compute(a: u32) -> u32 {\n    a + 1\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn serve_speaks_json_rpc_on_stdout_and_logs_on_stderr() {
    let project = project();
    let mut child = Command::new(env!("CARGO_BIN_EXE_semantiq"))
        .args(["serve", "--project"])
        .arg(project.path())
        .env("SEMANTIQ_EMBEDDINGS", "stub")
        .env("SEMANTIQ_UPDATE_CHECK", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn semantiq serve");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    // Drain stderr on its own thread so the server never blocks on a full pipe.
    let mut stderr = child.stderr.take().unwrap();
    let stderr_thread = std::thread::spawn(move || {
        let mut logs = String::new();
        let _ = stderr.read_to_string(&mut logs);
        logs
    });

    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "serve_stdio test", "version": "0"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ];
    for request in &requests {
        writeln!(stdin, "{request}").unwrap();
    }
    stdin.flush().unwrap();

    // Read stdout line by line until the tools/list response (id 2).
    let mut lines = Vec::new();
    let mut reader = BufReader::new(stdout);
    let mut tools: Option<Value> = None;
    while tools.is_none() {
        let mut line = String::new();
        let read = reader.read_line(&mut line).unwrap();
        assert!(
            read > 0,
            "stdout closed before the tools/list response: {lines:?}"
        );
        let message: Value = serde_json::from_str(line.trim_end()).unwrap_or_else(|e| {
            panic!("stdout line is not JSON-RPC ({e}): {line:?}\nearlier lines: {lines:?}")
        });
        assert_eq!(message["jsonrpc"], "2.0", "{line}");
        if message["id"] == 2 {
            tools = Some(message.clone());
        }
        lines.push(message);
    }

    // Closing stdin ends the session and the process.
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("serve did not exit after stdin closed");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "serve exited with {status}");

    // Nothing but the two responses reached stdout.
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();
    for line in rest.lines().filter(|l| !l.trim().is_empty()) {
        let message: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout line is not JSON-RPC ({e}): {line:?}"));
        assert_eq!(message["jsonrpc"], "2.0", "{line}");
    }
    let init = lines
        .iter()
        .find(|m| m["id"] == 1)
        .expect("initialize result");
    assert_eq!(init["result"]["serverInfo"]["name"], "semantiq");
    let tools = tools.unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 9, "{names:?}");
    assert!(names.contains(&"semantiq_search"), "{names:?}");

    // The logs went to stderr, as JSON lines in serve mode.
    let logs = stderr_thread.join().unwrap();
    assert!(logs.contains("Starting Semantiq MCP server"), "{logs}");
    let first = logs.lines().next().unwrap_or_default();
    assert!(
        serde_json::from_str::<Value>(first).is_ok(),
        "stderr log lines are JSON in serve mode: {first:?}"
    );
}
