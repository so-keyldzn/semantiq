use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use semantiq_mcp::SemantiqServer;
use std::sync::Arc;
use tower::ServiceExt;

use crate::http::create_router;
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
