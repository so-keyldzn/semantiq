//! Tests for `semantiq_calls`, `semantiq_hierarchy` and `semantiq_dead_code`.

use super::{create_test_server, index_test_file, text_of};
use crate::server::{CallsParams, DeadCodeParams, HierarchyParams};
use rmcp::handler::server::wrapper::Parameters;

const MUTUAL: &str = "fn ping(n: u32) {\n    if n > 0 { pong(n - 1); }\n}\n\nfn pong(n: u32) {\n    if n > 0 { ping(n - 1); }\n}\n\nfn start() {\n    ping(3);\n}\n";

fn calls_params(symbol: &str, direction: Option<&str>, max_depth: Option<usize>) -> CallsParams {
    CallsParams {
        symbol: symbol.to_string(),
        direction: direction.map(str::to_string),
        max_depth,
        ..Default::default()
    }
}

#[tokio::test]
async fn test_calls_mutual_recursion_terminates() {
    let (server, temp) = create_test_server();
    std::fs::write(temp.path().join("lib.rs"), MUTUAL).unwrap();
    index_test_file(&server.store, "lib.rs", MUTUAL, "rust");

    let output = server
        .calls(calls_params("ping", Some("both"), Some(3)))
        .await
        .unwrap();
    let callers: Vec<_> = output
        .callers
        .iter()
        .map(|e| (e.depth, e.caller.clone().unwrap_or_default()))
        .collect();
    assert!(callers.contains(&(1, "pong".to_string())), "{callers:?}");
    assert!(callers.contains(&(1, "start".to_string())), "{callers:?}");
    assert!(output.callers.len() <= 4);
    // ping → pong → ping: the cycle stops at the visited definition.
    let callees: Vec<_> = output.callees.iter().map(|e| e.callee.as_str()).collect();
    assert_eq!(callees, vec!["pong", "ping"]);
    assert!(output.callers.iter().all(|e| e.confidence == "same_file"));

    let text = text_of(
        server
            .semantiq_calls(Parameters(calls_params("ping", None, Some(3))))
            .await,
    )
    .unwrap();
    assert!(text.contains("# Calls of 'ping'"), "{text}");
    assert!(text.contains("start → ping  lib.rs:10"), "{text}");
}

#[tokio::test]
async fn test_calls_unknown_symbol_is_empty() {
    let (server, _temp) = create_test_server();
    let output = server
        .calls(calls_params("does_not_exist", None, None))
        .await
        .unwrap();
    assert!(output.definitions.is_empty());
    assert!(output.callers.is_empty() && output.callees.is_empty());
    assert!(output.render().contains("No call site found"));
}

#[tokio::test]
async fn test_calls_rejects_bad_input() {
    let (server, _temp) = create_test_server();
    assert!(
        server
            .calls(calls_params("ping", Some("sideways"), None))
            .await
            .is_err()
    );
    assert!(server.calls(calls_params("  ", None, None)).await.is_err());
    let traversal = CallsParams {
        symbol: "ping".into(),
        file_path: Some("../etc/passwd".into()),
        ..Default::default()
    };
    assert!(server.calls(traversal).await.is_err());
}

#[tokio::test]
async fn test_hierarchy_rust_traits() {
    let (server, temp) = create_test_server();
    let content = "pub trait Base {}\npub trait EmbeddingModel: Base + Send {\n    fn embed(&self);\n}\npub struct Onnx;\nimpl Base for Onnx {}\nimpl EmbeddingModel for Onnx {\n    fn embed(&self) {}\n}\npub struct Stub;\nimpl EmbeddingModel for Stub {\n    fn embed(&self) {}\n}\n";
    std::fs::write(temp.path().join("model.rs"), content).unwrap();
    index_test_file(&server.store, "model.rs", content, "rust");

    let output = server
        .hierarchy(HierarchyParams {
            symbol: "EmbeddingModel".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let up: Vec<_> = output
        .supertypes
        .iter()
        .map(|e| (e.super_type.as_str(), e.resolved))
        .collect();
    assert_eq!(up, vec![("Base", true), ("Send", false)]);
    let down: Vec<_> = output
        .subtypes
        .iter()
        .map(|e| (e.sub_type.as_str(), e.kind.as_str()))
        .collect();
    assert_eq!(down, vec![("Onnx", "implements"), ("Stub", "implements")]);

    let text = text_of(
        server
            .semantiq_hierarchy(Parameters(HierarchyParams {
                symbol: "EmbeddingModel".into(),
                ..Default::default()
            }))
            .await,
    )
    .unwrap();
    assert!(text.contains("Onnx implements EmbeddingModel"), "{text}");
    assert!(text.contains("Send not defined in the project"), "{text}");
}

#[tokio::test]
async fn test_hierarchy_unknown_type_is_empty() {
    let (server, _temp) = create_test_server();
    let output = server
        .hierarchy(HierarchyParams {
            symbol: "Nothing".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(output.supertypes.is_empty() && output.subtypes.is_empty());
    assert!(output.render().contains("None found"));
}

#[tokio::test]
async fn test_dead_code_reports_unused_private_function() {
    let (server, temp) = create_test_server();
    let content =
        "fn unused_helper() {}\n\nfn used() {}\n\npub fn api() {}\n\nfn main() {\n    used();\n}\n";
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/main.rs"), content).unwrap();
    index_test_file(&server.store, "src/main.rs", content, "rust");

    let output = server
        .dead_code(DeadCodeParams {
            path_prefix: Some("src/".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let names: Vec<_> = output.symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["unused_helper"]);
    assert_eq!(output.symbols[0].confidence, "high");
    assert_eq!(output.excluded.public, 1);

    let text = text_of(
        server
            .semantiq_dead_code(Parameters(DeadCodeParams {
                include_public: Some(true),
                ..Default::default()
            }))
            .await,
    )
    .unwrap();
    assert!(text.contains("[high] function unused_helper"), "{text}");
    assert!(text.contains("[low] function api"), "{text}");
}

#[tokio::test]
async fn test_dead_code_no_result() {
    let (server, _temp) = create_test_server();
    let output = server
        .dead_code(DeadCodeParams {
            language: Some("python".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(output.symbols.is_empty());
    assert!(output.render().contains("No unused symbol found"));
    assert!(
        server
            .dead_code(DeadCodeParams {
                path_prefix: Some("../x".into()),
                ..Default::default()
            })
            .await
            .is_err()
    );
}
