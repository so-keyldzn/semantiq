use super::create_test_server;
use rmcp::ServerHandler;

#[test]
fn test_get_info_returns_correct_name() {
    let (server, _temp) = create_test_server();
    let info = server.get_info();

    assert_eq!(info.server_info.name, "semantiq");
}

#[test]
fn test_get_info_returns_version() {
    let (server, _temp) = create_test_server();
    let info = server.get_info();

    assert!(!info.server_info.version.is_empty());
}

#[test]
fn test_get_info_has_instructions() {
    let (server, _temp) = create_test_server();
    let info = server.get_info();

    assert!(info.instructions.is_some());
    let instructions = info.instructions.unwrap();
    assert!(instructions.contains("semantiq_search"));
    assert!(instructions.contains("semantiq_find_refs"));
    assert!(instructions.contains("semantiq_deps"));
    assert!(instructions.contains("semantiq_explain"));
}

#[test]
fn test_get_info_enables_tools() {
    let (server, _temp) = create_test_server();
    let info = server.get_info();

    assert!(info.capabilities.tools.is_some());
}
