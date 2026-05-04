use super::create_test_server;
use super::index_test_file;

#[tokio::test]
async fn test_search_empty_query_returns_error() {
    let (server, _temp) = create_test_server();

    let result = server
        .semantiq_search(String::new(), None, None, None, None)
        .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "Query cannot be empty");
}

#[tokio::test]
async fn test_search_whitespace_only_query_returns_error() {
    let (server, _temp) = create_test_server();

    let result = server
        .semantiq_search("   ".to_string(), None, None, None, None)
        .await;

    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "Query cannot be empty");
}

#[tokio::test]
async fn test_search_query_too_long_returns_error() {
    let (server, _temp) = create_test_server();

    let long_query = "a".repeat(501);
    let result = server
        .semantiq_search(long_query, None, None, None, None)
        .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("maximum length"));
}

#[tokio::test]
async fn test_search_query_at_max_length_succeeds() {
    let (server, _temp) = create_test_server();

    let max_query = "a".repeat(500);
    let result = server
        .semantiq_search(max_query, None, None, None, None)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_search_returns_results_format() {
    let (server, _temp) = create_test_server();

    index_test_file(
        &server.store,
        "test.rs",
        "fn hello_world() { println!(\"Hello\"); }",
        "rust",
    );

    let result = server
        .semantiq_search("hello".to_string(), Some(10), None, None, None)
        .await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("results for 'hello'"));
    assert!(output.contains("ms)"));
}

#[tokio::test]
async fn test_search_with_file_type_filter() {
    let (server, _temp) = create_test_server();

    index_test_file(&server.store, "test.rs", "fn rust_func() {}", "rust");
    index_test_file(
        &server.store,
        "test.py",
        "def python_func(): pass",
        "python",
    );

    let result = server
        .semantiq_search(
            "func".to_string(),
            Some(10),
            None,
            Some("rs".to_string()),
            None,
        )
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_search_with_min_score_filter() {
    let (server, _temp) = create_test_server();

    index_test_file(&server.store, "test.rs", "fn exact_match() {}", "rust");

    let result = server
        .semantiq_search("exact_match".to_string(), Some(10), Some(0.9), None, None)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_search_with_symbol_kind_filter() {
    let (server, _temp) = create_test_server();

    index_test_file(
        &server.store,
        "test.rs",
        "fn my_function() {}\nstruct MyStruct {}",
        "rust",
    );

    let result = server
        .semantiq_search(
            "my".to_string(),
            Some(10),
            None,
            None,
            Some("function".to_string()),
        )
        .await;

    assert!(result.is_ok());
}
