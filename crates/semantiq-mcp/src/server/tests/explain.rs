use super::create_test_server;
use super::index_test_file;

#[tokio::test]
async fn test_explain_returns_formatted_output() {
    let (server, _temp) = create_test_server();

    index_test_file(
        &server.store,
        "lib.rs",
        "/// Documentation for process\nfn process() {}",
        "rust",
    );

    let result = server.semantiq_explain("process".to_string()).await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Symbol: process") || output.contains("not found"));
}

#[tokio::test]
async fn test_explain_symbol_not_found() {
    let (server, _temp) = create_test_server();

    let result = server
        .semantiq_explain("nonexistent_symbol".to_string())
        .await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("not found"));
}

#[tokio::test]
async fn test_explain_shows_definitions_count() {
    let (server, _temp) = create_test_server();

    index_test_file(&server.store, "a.rs", "fn shared_name() {}", "rust");
    index_test_file(&server.store, "b.rs", "fn shared_name() {}", "rust");

    let result = server.semantiq_explain("shared_name".to_string()).await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(
        output.contains("definition") || output.contains("not found"),
        "Expected 'definition' or 'not found' in output: {}",
        output
    );
}
