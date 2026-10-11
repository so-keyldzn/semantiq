use super::create_test_server;
use super::index_test_file;

#[tokio::test]
async fn test_deps_returns_formatted_output() {
    let (server, _temp) = create_test_server();

    let file_id = index_test_file(&server.store, "main.rs", "use crate::utils;", "rust");

    server
        .store
        .insert_dependency(
            file_id,
            "crate::utils",
            Some("utils"),
            "local",
            None,
            (1, 1),
        )
        .expect("Failed to insert dependency");

    let result = server.call_deps("main.rs".to_string()).await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.starts_with("main.rs\n"), "{output}");
    assert!(output.contains("Imports"));
}

#[tokio::test]
async fn test_deps_shows_imports_section() {
    let (server, _temp) = create_test_server();

    let file_id = index_test_file(&server.store, "app.rs", "use std::io;", "rust");

    server
        .store
        .insert_dependency(file_id, "std::io", Some("io"), "std", None, (1, 1))
        .expect("Failed to insert dependency");

    let result = server.call_deps("app.rs".to_string()).await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Imports"));
    assert!(output.contains("std::io"));
}

#[tokio::test]
async fn test_deps_nonexistent_file() {
    let (server, _temp) = create_test_server();

    let result = server.call_deps("nonexistent.rs".to_string()).await;

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Imports (0):"), "{output}");
}

#[tokio::test]
async fn test_deps_shows_reverse_dependencies() {
    let (server, _temp) = create_test_server();

    index_test_file(&server.store, "utils.rs", "pub fn helper() {}", "rust");
    let main_id = index_test_file(&server.store, "main.rs", "use crate::utils;", "rust");

    server
        .store
        .insert_dependency(
            main_id,
            "crate::utils",
            Some("utils"),
            "local",
            None,
            (1, 1),
        )
        .expect("Failed to insert dependency");

    let result = server.call_deps("utils.rs".to_string()).await;

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

/// #68: a path given with backslashes is looked up and echoed in the stored
/// form, as README and REFERENCE.md promise.
#[tokio::test]
async fn test_deps_echoes_normalized_file_path() {
    use crate::server::{DeadCodeParams, DepsParams, dead_code_output, deps_output};

    let (server, _temp) = create_test_server();
    let file_id = index_test_file(
        &server.store,
        "src/lib/main.rs",
        "use crate::utils;",
        "rust",
    );
    server
        .store
        .insert_dependency(
            file_id,
            "crate::utils",
            Some("utils"),
            "local",
            None,
            (1, 1),
        )
        .expect("Failed to insert dependency");

    let output = deps_output(
        &server.engine,
        DepsParams {
            file_path: r"src\lib\main.rs".to_string(),
        },
    )
    .unwrap();
    assert_eq!(output.file_path, "src/lib/main.rs");
    assert_eq!(output.imports.as_ref().map(Vec::len), Some(1), "{output:?}");

    let text = server
        .call_deps(r".\src\lib\main.rs".to_string())
        .await
        .unwrap();
    assert!(text.starts_with("src/lib/main.rs\n"), "{text}");

    // `DeadCodeOutput` does not echo the prefix: the filter must behave the
    // same with either separator.
    let dead_code = |prefix: &str| {
        dead_code_output(
            &server.engine,
            DeadCodeParams {
                path_prefix: Some(prefix.to_string()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let backslash = dead_code(r"src\lib\");
    let slash = dead_code("src/lib/");
    assert_eq!(backslash.candidates, slash.candidates);
    assert_eq!(backslash.symbols.len(), slash.symbols.len());
    assert_eq!(dead_code(r"other\").candidates, 0);
}
