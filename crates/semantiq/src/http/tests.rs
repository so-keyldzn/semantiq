use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use semantiq_mcp::SemantiqServer;
use std::sync::Arc;
use tower::ServiceExt;

use crate::http::create_router;
use crate::http::host::{AllowedHosts, require_allowed_host};
use crate::http::types::*;

/// Create a test router with an in-memory database
fn test_router() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let server = SemantiqServer::new(&db_path, dir.path().to_str().unwrap()).unwrap();
    // Leak the tempdir so it stays alive for the duration of the test
    std::mem::forget(dir);
    create_router(Arc::new(server))
}

/// The test router behind the `Host` guard, as `serve --http-port` builds it
/// when bound to the loopback address `bind`.
fn router_bound_to(bind: &str) -> axum::Router {
    let allowed = AllowedHosts::for_bind_address(bind.parse().unwrap());
    test_router().layer(axum::middleware::from_fn_with_state(
        allowed,
        require_allowed_host,
    ))
}

/// Bound to the default `127.0.0.1`.
fn guarded_router() -> axum::Router {
    router_bound_to("127.0.0.1")
}

async fn response_body(response: axum::http::Response<Body>) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

// ============================================
// Health endpoint
// ============================================

#[tokio::test]
async fn test_health_returns_ok() {
    let app = test_router();

    let response = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_body(response).await;
    let health: HealthResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(health.status, "ok");
    assert!(!health.version.is_empty());
}

// ============================================
// Stats endpoint
// ============================================

#[tokio::test]
async fn test_stats_returns_ok() {
    let app = test_router();

    let response = app
        .oneshot(Request::get("/stats").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_body(response).await;
    let stats: StatsResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(stats.indexed_files, 0);
    assert_eq!(stats.indexed_symbols, 0);
    assert_eq!(stats.indexed_refs, 0);
    assert_eq!(stats.indexed_call_edges, 0);
    assert_eq!(stats.indexed_type_relations, 0);
    assert!(!stats.indexing);
    // `SemantiqServer::new` watches the temporary project.
    assert!(stats.watching);
    // Tests run with the stub model (`test-stub` feature).
    assert_eq!(stats.embedding_model, "stub");
    assert!(!stats.semantic_search);
    assert!(stats.semantic_search_unavailable_reason.is_some());
}

// ============================================
// Search validation
// ============================================

#[tokio::test]
async fn test_search_empty_query() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/search")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query": ""}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_body(response).await;
    let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, "INVALID_QUERY");
}

#[tokio::test]
async fn test_search_query_too_long() {
    let app = test_router();
    let long_query = "a".repeat(501);

    let response = app
        .oneshot(
            Request::post("/search")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"query": "{}"}}"#, long_query)))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_body(response).await;
    let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, "QUERY_TOO_LONG");
}

#[tokio::test]
async fn test_search_valid_query_empty_index() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/search")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query": "test function"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_body(response).await;
    let search: SearchResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(search.total_count, 0);
    assert!(search.results.is_empty());
}

#[tokio::test]
async fn test_search_missing_body() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/search")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    // Missing required field "query" should return 422
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

// ============================================
// Find refs validation
// ============================================

#[tokio::test]
async fn test_find_refs_empty_symbol() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/find-refs")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"symbol": ""}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_body(response).await;
    let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, "INVALID_SYMBOL");
}

#[tokio::test]
async fn test_find_refs_symbol_too_long() {
    let app = test_router();
    let long_symbol = "a".repeat(501);

    let response = app
        .oneshot(
            Request::post("/find-refs")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"symbol": "{}"}}"#, long_symbol)))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_body(response).await;
    let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, "SYMBOL_TOO_LONG");
}

#[tokio::test]
async fn test_find_refs_valid_symbol_empty_index() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/find-refs")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"symbol": "test_fn"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_body(response).await;
    let refs: FindRefsResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(refs.symbol, "test_fn");
    assert!(refs.definitions.is_empty());
    assert!(refs.references.is_empty());
}

// ============================================
// Deps validation
// ============================================

#[tokio::test]
async fn test_deps_empty_path() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/deps")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"file_path": ""}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_body(response).await;
    let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, "INVALID_PATH");
}

#[tokio::test]
async fn test_deps_path_traversal() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/deps")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"file_path": "../etc/passwd"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_body(response).await;
    let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, "PATH_TRAVERSAL");
}

#[tokio::test]
async fn test_deps_valid_path_empty_index() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/deps")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"file_path": "src/main.rs"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_body(response).await;
    let deps: DepsResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(deps.file_path, "src/main.rs");
    assert!(deps.imports.is_empty());
    assert!(deps.imported_by.is_empty());
}

// ============================================
// Explain validation
// ============================================

#[tokio::test]
async fn test_explain_empty_symbol() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/explain")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"symbol": ""}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_body(response).await;
    let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, "INVALID_SYMBOL");
}

#[tokio::test]
async fn test_explain_valid_symbol_empty_index() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/explain")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"symbol": "MyStruct"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_body(response).await;
    let explain: ExplainResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(explain.symbol, "MyStruct");
    assert_eq!(explain.kind, "unknown");
}

// ============================================
// 404 for unknown routes
// ============================================

#[tokio::test]
async fn test_unknown_route_returns_404() {
    let app = test_router();

    let response = app
        .oneshot(Request::get("/nonexistent").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ============================================
// Wrong HTTP method
// ============================================

#[tokio::test]
async fn test_search_get_method_not_allowed() {
    let app = test_router();

    let response = app
        .oneshot(Request::get("/search").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

// ============================================
// Repo map endpoint
// ============================================

#[tokio::test]
async fn test_map_empty_index() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/map")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"max_tokens": 500}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_body(response).await;
    let map: MapResponse = serde_json::from_slice(&body).unwrap();
    assert!(map.map.contains("no source files"));
    assert_eq!(map.details["max_tokens"], 500);
    assert_eq!(map.details["total_files"], 0);
    assert!(map.details["files"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_map_rejects_path_traversal() {
    let app = test_router();

    let response = app
        .oneshot(
            Request::post("/map")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"path_prefix": "../etc"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_body(response).await;
    let error: ErrorResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(error.code, "INVALID_MAP_REQUEST");
}

// ============================================
// Structure: calls, hierarchy, dead code
// ============================================

async fn post_json(path: &str, body: &str) -> (StatusCode, serde_json::Value) {
    let response = test_router()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_body(response).await;
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn test_calls_validation() {
    let (status, body) = post_json("/calls", r#"{"symbol": " "}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "INVALID_SYMBOL");

    let (status, body) = post_json("/calls", r#"{"symbol": "f", "direction": "up"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "INVALID_DIRECTION");

    let (status, body) = post_json("/calls", r#"{"symbol": "f", "file_path": "../secret"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "PATH_TRAVERSAL");
}

#[tokio::test]
async fn test_calls_empty_index() {
    let (status, body) = post_json(
        "/calls",
        r#"{"symbol": "index_file", "direction": "callers"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["symbol"], "index_file");
    assert_eq!(body["callers"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_hierarchy_endpoint() {
    let (status, body) = post_json("/hierarchy", r#"{"symbol": ""}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "INVALID_SYMBOL");

    let (status, body) = post_json("/hierarchy", r#"{"symbol": "EmbeddingModel"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["subtypes"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_dead_code_endpoint() {
    let (status, body) = post_json("/dead-code", r#"{"path_prefix": "../etc"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "PATH_TRAVERSAL");

    let (status, body) =
        post_json("/dead-code", r#"{"path_prefix": "crates/", "limit": 5000}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["symbols"].as_array().unwrap().is_empty());
}

// ============================================
// Loopback Host guard (DNS rebinding)
// ============================================

#[tokio::test]
async fn test_foreign_host_is_rejected_on_rest_endpoints() {
    // A page at http://evil.example:3000 whose name re-resolves to 127.0.0.1
    // sends same-origin requests carrying its own Host: no CORS involved.
    for (path, body) in [
        ("/search", Some(r#"{"query": "compute"}"#)),
        ("/stats", None),
        ("/health", None),
    ] {
        let request = match body {
            Some(body) => Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body)),
            None => Request::get(path).body(Body::empty()),
        };
        let response = guarded_router()
            .oneshot(request.unwrap().tap_host("evil.example:3000"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        let body = response_body(response).await;
        assert_eq!(
            String::from_utf8_lossy(&body),
            "Forbidden: Host header is not allowed"
        );
    }
}

#[tokio::test]
async fn test_loopback_hosts_are_accepted() {
    for host in [
        "localhost:3000",
        "localhost",
        "LocalHost",
        "127.0.0.1:3000",
        "[::1]:3000",
    ] {
        let response = guarded_router()
            .oneshot(
                Request::get("/health")
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn test_missing_host_is_rejected_unless_the_uri_carries_it() {
    let response = guarded_router()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // HTTP/2 puts the host in `:authority`, which hyper exposes on the URI.
    let response = guarded_router()
        .oneshot(
            Request::get("http://localhost:3000/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = guarded_router()
        .oneshot(
            Request::get("http://evil.example/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_bound_loopback_address_is_accepted() {
    // `--http-host 127.0.0.2` is loopback too: its clients send its address.
    for (host, status) in [
        ("127.0.0.2:3000", StatusCode::OK),
        ("127.0.0.2", StatusCode::OK),
        ("localhost:3000", StatusCode::OK),
        ("127.0.0.3:3000", StatusCode::FORBIDDEN),
        ("evil.example:3000", StatusCode::FORBIDDEN),
    ] {
        let response = router_bound_to("127.0.0.2")
            .oneshot(
                Request::get("/health")
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{host}");
    }
}

#[tokio::test]
async fn test_router_without_the_guard_ignores_host() {
    // `--http-host 0.0.0.0` disables the guard: the Host is then this
    // machine's address, whatever it is.
    let response = test_router()
        .oneshot(
            Request::get("/health")
                .header("host", "build-box.lan:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// Set the `Host` header on a built request.
trait TapHost {
    fn tap_host(self, host: &str) -> Self;
}

impl TapHost for Request<Body> {
    fn tap_host(mut self, host: &str) -> Self {
        self.headers_mut()
            .insert("host", axum::http::HeaderValue::from_str(host).unwrap());
        self
    }
}
