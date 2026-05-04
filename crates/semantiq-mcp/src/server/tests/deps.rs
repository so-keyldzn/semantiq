use super::create_test_server;
use super::index_test_file;

#[tokio::test]
async fn test_deps_returns_formatted_output() {
    let (server, _temp) = create_test_server();

    let file_id = index_test_file(&server.store, "main.rs", "use crate::utils;", "rust");

    server
        .store
        .insert_dependency(file_id, "crate::utils", Some("utils"), "local", None)
        .expect("Failed to insert dependency");

    let result = server.semantiq_deps("main.rs".to_string()).await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Dependency analysis for 'main.rs'"));
    assert!(output.contains("Imports"));
}

#[tokio::test]
async fn test_deps_shows_imports_section() {
    let (server, _temp) = create_test_server();

    let file_id = index_test_file(&server.store, "app.rs", "use std::io;", "rust");

    server
        .store
        .insert_dependency(file_id, "std::io", Some("io"), "std", None)
        .expect("Failed to insert dependency");

    let result = server.semantiq_deps("app.rs".to_string()).await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Imports"));
    assert!(output.contains("std::io"));
}

#[tokio::test]
async fn test_deps_nonexistent_file() {
    let (server, _temp) = create_test_server();

    let result = server.semantiq_deps("nonexistent.rs".to_string()).await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("0 dependencies"));
}

#[tokio::test]
async fn test_deps_shows_reverse_dependencies() {
    let (server, _temp) = create_test_server();

    index_test_file(&server.store, "utils.rs", "pub fn helper() {}", "rust");
    let main_id = index_test_file(&server.store, "main.rs", "use crate::utils;", "rust");

    server
        .store
        .insert_dependency(main_id, "crate::utils", Some("utils"), "local", None)
        .expect("Failed to insert dependency");

    let result = server.semantiq_deps("utils.rs".to_string()).await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(
        output.contains("Imported by"),
        "Expected 'Imported by' section in output: {}",
        output
    );
    assert!(
        output.contains("main.rs"),
        "Expected 'main.rs' as reverse dependency in output: {}",
        output
    );
}
