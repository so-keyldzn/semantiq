//! `semantiq serve --http-port` speaks MCP Streamable HTTP at `/mcp`: the
//! `initialize` handshake, the session header and `tools/list` over a raw
//! HTTP/1.1 connection to the running binary (#122). The REST routes have
//! in-process tests in `src/http/tests.rs`; this is the only test of the
//! transport the binary actually mounts.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// A `serve --http-port` process, killed on drop.
struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start_server(project: &Path) -> Server {
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_semantiq"))
        .args(["serve", "--project"])
        .arg(project)
        .args(["--http-port", &port.to_string()])
        .env("SEMANTIQ_EMBEDDINGS", "stub")
        .env("SEMANTIQ_UPDATE_CHECK", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn semantiq serve");
    let mut server = Server { child, port };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return server;
        }
        if let Some(status) = server.child.try_wait().unwrap() {
            panic!("serve exited early with {status}");
        }
        assert!(Instant::now() < deadline, "serve did not listen on {port}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A decoded HTTP response: status, lowercase header names, body.
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The JSON-RPC messages of the body: SSE `data:` lines, or the body
    /// itself when the server answered with `application/json`.
    fn messages(&self) -> Vec<Value> {
        if self
            .header("content-type")
            .is_some_and(|ct| ct.starts_with("text/event-stream"))
        {
            self.body
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .filter(|data| !data.is_empty())
                .map(|data| serde_json::from_str(data).unwrap_or_else(|e| panic!("{e}: {data}")))
                .collect()
        } else if self.body.trim().is_empty() {
            Vec::new()
        } else {
            vec![serde_json::from_str(&self.body).unwrap()]
        }
    }
}

/// Send one request and read the response. An SSE response is read until a
/// message with `until_id` arrives (the stream may stay open afterwards);
/// any other response until the server closes the connection.
fn request(
    port: u16,
    method: &str,
    extra_headers: &[(&str, &str)],
    body: Option<&Value>,
    until_id: Option<u64>,
) -> Response {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    // `extra_headers` may override the `Host` the request is sent with.
    let host = extra_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| format!("127.0.0.1:{port}"));
    let mut head = format!(
        "{method} /mcp HTTP/1.1\r\nHost: {host}\r\nAccept: application/json, text/event-stream\r\nConnection: close\r\n"
    );
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    for (k, v) in extra_headers
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("host"))
    {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("bad status line {status_line:?}"));
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':').unwrap();
        headers.push((k.trim().to_lowercase(), v.trim().to_string()));
    }
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.contains("chunked"));
    let content_length: Option<usize> = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok());

    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            reader.read_line(&mut size_line).unwrap();
            let size = usize::from_str_radix(size_line.trim(), 16)
                .unwrap_or_else(|_| panic!("bad chunk size {size_line:?}"));
            if size == 0 {
                break;
            }
            let mut chunk = vec![0u8; size + 2];
            reader.read_exact(&mut chunk).unwrap();
            body.extend_from_slice(&chunk[..size]);
            if let Some(id) = until_id
                && String::from_utf8_lossy(&body)
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
                    .any(|m| m["id"] == id)
            {
                break;
            }
        }
    } else if let Some(len) = content_length {
        body.resize(len, 0);
        reader.read_exact(&mut body).unwrap();
    } else {
        reader.read_to_end(&mut body).unwrap();
    }
    Response {
        status,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "http_mcp test", "version": "0"}
        }
    })
}

#[test]
fn mcp_endpoint_initializes_and_lists_the_tools() {
    let project = project();
    let server = start_server(project.path());
    let port = server.port;

    // initialize: a JSON-RPC result over SSE, with a session id.
    let response = request(port, "POST", &[], Some(&initialize_request()), Some(1));
    assert_eq!(response.status, 200, "{}", response.body);
    let session = response
        .header("mcp-session-id")
        .expect("initialize answers with Mcp-Session-Id")
        .to_string();
    let messages = response.messages();
    let init = messages
        .iter()
        .find(|m| m["id"] == 1)
        .unwrap_or_else(|| panic!("no initialize result: {}", response.body));
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(init["result"]["serverInfo"]["name"], "semantiq");
    assert_eq!(
        init["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(init["result"]["capabilities"]["tools"].is_object());

    // The handshake ends with the initialized notification (202, no body).
    let session_headers = [
        ("Mcp-Session-Id", session.as_str()),
        ("MCP-Protocol-Version", "2025-06-18"),
    ];
    let response = request(
        port,
        "POST",
        &session_headers,
        Some(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})),
        None,
    );
    assert_eq!(response.status, 202, "{}", response.body);

    // tools/list in the session: the nine read-only tools.
    let response = request(
        port,
        "POST",
        &session_headers,
        Some(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})),
        Some(2),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let messages = response.messages();
    let list = messages
        .iter()
        .find(|m| m["id"] == 2)
        .unwrap_or_else(|| panic!("no tools/list result: {}", response.body));
    let mut names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "semantiq_calls",
            "semantiq_dead_code",
            "semantiq_deps",
            "semantiq_explain",
            "semantiq_find_refs",
            "semantiq_hierarchy",
            "semantiq_impact",
            "semantiq_repo_map",
            "semantiq_search",
        ]
    );
    for tool in list["result"]["tools"].as_array().unwrap() {
        assert!(tool["inputSchema"].is_object(), "{tool}");
        assert!(tool["outputSchema"].is_object(), "{tool}");
    }

    // A request outside any session is rejected, not served.
    let response = request(
        port,
        "POST",
        &[],
        Some(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"})),
        None,
    );
    assert!(
        response.status >= 400,
        "tools/list without a session: {} {}",
        response.status,
        response.body
    );

    // DELETE ends the session; the id is no longer accepted.
    let response = request(port, "DELETE", &session_headers, None, None);
    assert!(
        response.status < 300,
        "DELETE /mcp: {} {}",
        response.status,
        response.body
    );
    let response = request(
        port,
        "POST",
        &session_headers,
        Some(&json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"})),
        None,
    );
    assert!(
        response.status >= 400,
        "tools/list after DELETE: {} {}",
        response.status,
        response.body
    );
}

#[test]
fn mcp_endpoint_rejects_a_foreign_host_header() {
    let project = project();
    let server = start_server(project.path());

    // DNS rebinding guard (#38): the same `Host` check as the REST routes.
    let response = request(
        server.port,
        "POST",
        &[("Host", "evil.example")],
        Some(&initialize_request()),
        None,
    );
    assert_eq!(response.status, 403, "{}", response.body);
}
