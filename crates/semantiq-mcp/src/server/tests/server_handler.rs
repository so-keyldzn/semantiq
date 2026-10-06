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
    assert!(instructions.contains("semantiq_impact"));
}

#[test]
fn test_get_info_enables_tools() {
    let (server, _temp) = create_test_server();
    let info = server.get_info();

    assert!(info.capabilities.tools.is_some());
}

#[test]
fn test_tools_are_annotated_read_only() {
    let tools = super::SemantiqServer::tool_router().list_all();

    assert_eq!(tools.len(), 5);
    for tool in &tools {
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no annotations", tool.name));
        assert_eq!(annotations.read_only_hint, Some(true), "{}", tool.name);
        assert_eq!(annotations.destructive_hint, Some(false), "{}", tool.name);
        assert_eq!(annotations.open_world_hint, Some(false), "{}", tool.name);
    }
}

#[test]
fn test_tools_declare_output_schema() {
    let tools = super::SemantiqServer::tool_router().list_all();

    for tool in &tools {
        assert!(
            tool.output_schema.is_some(),
            "{} has no output schema",
            tool.name
        );
    }
}

#[tokio::test]
async fn test_structured_content_matches_text() {
    use super::index_test_file;
    use rmcp::handler::server::wrapper::Parameters;

    let (server, temp) = create_test_server();
    let content = "fn structured_fn() {}";
    std::fs::write(temp.path().join("lib.rs"), content).expect("Failed to write test file");
    index_test_file(&server.store, "lib.rs", content, "rust");

    let result = server
        .semantiq_find_refs(Parameters(crate::server::FindRefsParams {
            symbol: "structured_fn".to_string(),
            limit: None,
        }))
        .await
        .expect("find_refs failed");

    let structured = result.structured_content.expect("no structured content");
    assert_eq!(structured["symbol"], "structured_fn");
    assert_eq!(structured["definitions"][0]["file_path"], "lib.rs");
    assert!(structured["usages"].is_array());
}

#[tokio::test]
async fn test_impact_groups_sites_by_file() {
    use super::index_test_file;
    use rmcp::handler::server::wrapper::Parameters;

    let (server, _temp) = create_test_server();
    index_test_file(
        &server.store,
        "lib.rs",
        "pub fn core_op() {}\nfn helper() { core_op(); }\n",
        "rust",
    );
    index_test_file(
        &server.store,
        "tests.rs",
        "fn test_it() { helper(); }\n",
        "rust",
    );

    let result = server
        .semantiq_impact(Parameters(crate::server::ImpactParams {
            symbol: "core_op".to_string(),
            ..Default::default()
        }))
        .await
        .expect("impact failed");

    let structured = result.structured_content.expect("no structured content");
    assert_eq!(structured["site_count"], 2);
    assert_eq!(structured["files"][0]["file_path"], "lib.rs");
    assert_eq!(structured["files"][0]["sites"][0]["enclosing"], "helper");
    assert_eq!(structured["files"][1]["file_path"], "tests.rs");
    assert_eq!(structured["files"][1]["depth"], 2);
    assert_eq!(structured["test_files"][0], "tests.rs");

    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(text.contains("Tests to run"), "{text}");
}

#[tokio::test]
async fn test_impact_rejects_path_traversal() {
    use rmcp::handler::server::wrapper::Parameters;

    let (server, _temp) = create_test_server();
    let result = server
        .semantiq_impact(Parameters(crate::server::ImpactParams {
            symbol: "x_y".to_string(),
            file_path: Some("../etc/passwd".to_string()),
            ..Default::default()
        }))
        .await;
    assert!(result.is_err());
}
