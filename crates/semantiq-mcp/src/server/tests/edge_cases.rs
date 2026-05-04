use super::create_test_server;

#[tokio::test]
async fn test_search_with_special_characters() {
    let (server, _temp) = create_test_server();

    let result = server
        .semantiq_search("test*".to_string(), Some(10), None, None, None)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_search_with_unicode() {
    let (server, _temp) = create_test_server();

    let result = server
        .semantiq_search("函数".to_string(), Some(10), None, None, None)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_find_refs_with_special_characters() {
    let (server, _temp) = create_test_server();

    let result = server
        .semantiq_find_refs("operator+".to_string(), Some(10))
        .await;

    assert!(result.is_ok());
}
