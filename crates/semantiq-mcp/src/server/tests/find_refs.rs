use super::create_test_server;
use super::index_test_file;

#[tokio::test]
async fn test_find_refs_returns_formatted_output() {
    let (server, temp) = create_test_server();

    let content = "fn my_symbol() {}";
    let file_path = temp.path().join("test.rs");
    std::fs::write(&file_path, content).expect("Failed to write test file");

    index_test_file(&server.store, "test.rs", content, "rust");

    let result = server
        .semantiq_find_refs("my_symbol".to_string(), Some(10))
        .await;

    assert!(result.is_ok(), "Expected Ok but got: {:?}", result);
    let output = result.unwrap();
    assert!(output.contains("references to 'my_symbol'"));
}

#[tokio::test]
async fn test_find_refs_with_definitions() {
    let (server, temp) = create_test_server();

    let content = "fn calculate() {}";
    let file_path = temp.path().join("lib.rs");
    std::fs::write(&file_path, content).expect("Failed to write test file");

    index_test_file(&server.store, "lib.rs", content, "rust");

    let result = server
        .semantiq_find_refs("calculate".to_string(), Some(50))
        .await;

    assert!(result.is_ok(), "Expected Ok but got: {:?}", result);
    let output = result.unwrap();
    assert!(output.contains("references to 'calculate'"));
}

#[tokio::test]
async fn test_find_refs_default_limit() {
    let (server, _temp) = create_test_server();

    let result = server
        .semantiq_find_refs("nonexistent".to_string(), None)
        .await;

    assert!(result.is_ok());
}
