use super::{create_test_server, index_test_file, text_of};
use crate::server::RepoMapParams;
use rmcp::handler::server::wrapper::Parameters;

fn index_sample(server: &crate::server::SemantiqServer) {
    index_test_file(
        &server.store,
        "src/store.rs",
        "/// Persistent storage.\npub struct Store;\npub fn open_store() -> Store { Store }\n",
        "rust",
    );
    index_test_file(
        &server.store,
        "src/api.rs",
        "pub fn handle_request() { let _s: Store = open_store(); }\n",
        "rust",
    );
    index_test_file(
        &server.store,
        "src/cli.rs",
        "pub fn run_cli() { let _s: Store = open_store(); }\n",
        "rust",
    );
}

#[tokio::test]
async fn test_repo_map_lists_ranked_symbols() {
    let (server, _temp) = create_test_server();
    index_sample(&server);

    let result = server
        .semantiq_repo_map(Parameters(RepoMapParams::default()))
        .await;
    let structured = result
        .as_ref()
        .ok()
        .and_then(|r| r.structured_content.clone())
        .expect("no structured content");
    let text = text_of(result).expect("repo map failed");

    assert!(text.starts_with("Repo map: "), "{}", text);
    assert!(
        text.contains("    pub struct Store  — Persistent storage.\n"),
        "{}",
        text
    );
    assert!(text.contains("    pub fn open_store() -> Store\n"));
    assert_eq!(structured["max_tokens"], 1500);
    assert_eq!(structured["total_files"], 3);
    assert!(
        structured.get("text").is_none(),
        "text is sent as content only"
    );
    let store = structured["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["file_path"] == "src/store.rs")
        .expect("store.rs not listed");
    assert_eq!(store["symbols"][0]["name"], "Store");
    assert_eq!(store["symbols"][0]["line"], 2);
}

#[tokio::test]
async fn test_repo_map_reports_unmatched_focus() {
    let (server, _temp) = create_test_server();
    index_sample(&server);

    let text = text_of(
        server
            .semantiq_repo_map(Parameters(RepoMapParams {
                focus: Some(vec!["src/api.rs".to_string(), "missing.rs".to_string()]),
                ..Default::default()
            }))
            .await,
    )
    .expect("repo map failed");

    assert!(text.contains("(focus: src/api.rs)"), "{}", text);
    assert!(text.contains("Not found in the index (focus ignored): missing.rs"));
}

#[tokio::test]
async fn test_repo_map_empty_index() {
    let (server, _temp) = create_test_server();
    let text = text_of(
        server
            .semantiq_repo_map(Parameters(RepoMapParams::default()))
            .await,
    )
    .expect("repo map failed");
    assert!(text.contains("no source files"), "{}", text);
}

#[tokio::test]
async fn test_repo_map_validates_input() {
    let (server, _temp) = create_test_server();

    let too_many = server
        .repo_map(RepoMapParams {
            focus: Some(vec!["a.rs".to_string(); 51]),
            ..Default::default()
        })
        .await;
    assert!(too_many.unwrap_err().contains("at most 50"));

    let traversal = server
        .repo_map(RepoMapParams {
            path_prefix: Some("../etc".to_string()),
            ..Default::default()
        })
        .await;
    assert!(traversal.unwrap_err().contains(".."));

    let too_long = server
        .repo_map(RepoMapParams {
            focus: Some(vec!["x".repeat(501)]),
            ..Default::default()
        })
        .await;
    assert!(too_long.is_err());

    // Blank entries are ignored rather than rejected.
    let blank = server
        .repo_map(RepoMapParams {
            focus: Some(vec!["  ".to_string()]),
            path_prefix: Some(" ".to_string()),
            max_tokens: Some(10),
        })
        .await
        .expect("blank focus should be accepted");
    assert_eq!(blank.max_tokens, 256);
}
