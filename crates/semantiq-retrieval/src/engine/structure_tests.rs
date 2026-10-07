//! Tests for the call graph, type hierarchy and dead-code analyses.

use super::*;
use semantiq_parser::{
    Language, LanguageSupport, ReferenceExtractor, StructureExtractor, SymbolExtractor,
};

/// Index Rust `files` (path, source) under a temp root, with structure.
fn index_project(files: &[(&str, &str)]) -> (RetrievalEngine, tempfile::TempDir) {
    let root = tempfile::Builder::new()
        .prefix("semantiq-structure")
        .tempdir()
        .unwrap();
    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let mut support = LanguageSupport::new().unwrap();
    for (path, source) in files {
        let full = root.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, source).unwrap();
        let language = Language::from_path(std::path::Path::new(path)).unwrap();
        let file_id = store
            .insert_file(path, Some(language.name()), source, 0)
            .unwrap();
        let tree = support.parse(language, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, language).unwrap();
        store.insert_symbols(file_id, &symbols).unwrap();
        let refs = ReferenceExtractor::extract(&tree, source, language);
        store.insert_references(file_id, &refs).unwrap();
        let structure = StructureExtractor::extract(&tree, source, language, &symbols, &refs);
        store.insert_structure(file_id, &structure).unwrap();
    }
    let engine = RetrievalEngine::with_options(store, root.path().to_str().unwrap(), false);
    (engine, root)
}

const MUTUAL: &str = "fn ping(n: u32) {\n    if n > 0 { pong(n - 1); }\n}\n\nfn pong(n: u32) {\n    if n > 0 { ping(n - 1); }\n}\n\nfn start() {\n    ping(3);\n    println!(\"done\");\n}\n";

#[test]
fn test_callers_mutual_recursion_terminates() {
    let (engine, _root) = index_project(&[("lib.rs", MUTUAL)]);
    let graph = engine
        .call_graph("ping", None, CallDirection::Callers, 3, 100)
        .unwrap();
    let callers: Vec<_> = graph
        .callers
        .iter()
        .map(|c| (c.depth, c.caller.clone().unwrap_or_default()))
        .collect();
    assert!(callers.contains(&(1, "pong".to_string())), "{callers:?}");
    assert!(callers.contains(&(1, "start".to_string())), "{callers:?}");
    // pong is called by ping (depth 2), which is not walked again.
    assert!(callers.contains(&(2, "ping".to_string())), "{callers:?}");
    assert!(callers.len() <= 4, "{callers:?}");
    assert!(
        graph
            .callers
            .iter()
            .all(|c| c.confidence == ImpactConfidence::SameFile)
    );
}

#[test]
fn test_callees_and_external() {
    let (engine, _root) = index_project(&[("lib.rs", MUTUAL)]);
    let graph = engine
        .call_graph("start", None, CallDirection::Callees, 3, 100)
        .unwrap();
    let callees: Vec<_> = graph
        .callees
        .iter()
        .map(|c| (c.depth, c.callee.as_str()))
        .collect();
    assert_eq!(callees, vec![(1, "ping"), (2, "pong"), (3, "ping")]);
    assert_eq!(graph.external_callees, vec!["println".to_string()]);
    assert!(graph.callers.is_empty());
}

#[test]
fn test_callers_across_files_confidence() {
    let (engine, _root) = index_project(&[
        ("a.rs", "pub fn shared() {}\n"),
        (
            "b.rs",
            "use crate::a::shared;\nfn user() {\n    shared();\n}\n",
        ),
    ]);
    let graph = engine
        .call_graph("shared", None, CallDirection::Both, 1, 100)
        .unwrap();
    assert_eq!(graph.callers.len(), 1);
    assert_eq!(graph.callers[0].confidence, ImpactConfidence::UniqueName);

    let b = engine.store.get_file_by_path("b.rs").unwrap().unwrap().id;
    engine
        .store
        .insert_dependency(
            b,
            "crate::a::shared",
            Some("shared"),
            "local",
            Some("a.rs"),
            (1, 1),
        )
        .unwrap();
    let graph = engine
        .call_graph("shared", None, CallDirection::Callers, 1, 100)
        .unwrap();
    assert_eq!(graph.callers[0].confidence, ImpactConfidence::Imports);
    assert_eq!(graph.callers[0].file_path, "b.rs");
}

#[test]
fn test_call_graph_unknown_symbol() {
    let (engine, _root) = index_project(&[("lib.rs", MUTUAL)]);
    let graph = engine
        .call_graph("nothing_here", None, CallDirection::Both, 2, 100)
        .unwrap();
    assert!(graph.definitions.is_empty());
    assert!(graph.callers.is_empty() && graph.callees.is_empty());
}

#[test]
fn test_type_hierarchy_transitive() {
    let (engine, _root) = index_project(&[(
        "model.ts",
        "interface Base {}\ninterface Model extends Base {}\nclass Onnx implements Model {}\nclass Quantized extends Onnx {}\n",
    )]);
    let h = engine.type_hierarchy("Model", 3, 100).unwrap();
    let up: Vec<_> = h.supertypes.iter().map(|e| e.super_type.as_str()).collect();
    assert_eq!(up, vec!["Base"]);
    let down: Vec<_> = h
        .subtypes
        .iter()
        .map(|e| (e.depth, e.sub_type.as_str(), e.kind.as_str()))
        .collect();
    assert_eq!(
        down,
        vec![(1, "Onnx", "implements"), (2, "Quantized", "extends")]
    );
    assert!(h.subtypes.iter().all(|e| e.resolved));
}

#[test]
fn test_type_hierarchy_cycle_terminates() {
    let (engine, _root) = index_project(&[(
        "cycle.py",
        "class Alpha(Beta):\n    pass\n\nclass Beta(Alpha):\n    pass\n",
    )]);
    let h = engine.type_hierarchy("Alpha", 5, 100).unwrap();
    assert_eq!(h.supertypes.len(), 2);
    assert_eq!(h.subtypes.len(), 2);
}

#[test]
fn test_dead_code_filters() {
    let source = "trait Speak {\n    fn speak(&self);\n}\n\nstruct Dog;\n\nimpl Speak for Dog {\n    fn speak(&self) {}\n}\n\nfn unused_helper() {}\n\npub fn public_api() {}\n\nfn used() {}\n\nfn recursive(n: u32) {\n    recursive(n);\n}\n\n#[inline]\nfn attributed() {}\n\nfn main() {\n    used();\n    let d = Dog;\n    d.speak();\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn checks_things() {}\n}\n";
    let (engine, _root) = index_project(&[("src/main.rs", source)]);
    let report = engine
        .find_dead_code(&DeadCodeOptions {
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    let dead: Vec<_> = report
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.confidence))
        .collect();
    assert_eq!(
        dead,
        vec![
            ("unused_helper", DeadCodeConfidence::High),
            ("recursive", DeadCodeConfidence::High),
            ("attributed", DeadCodeConfidence::Low),
        ],
        "{report:?}"
    );
    assert_eq!(report.excluded.public, 1);
    assert!(report.excluded.tests >= 1);
    assert!(report.excluded.entry_points >= 1);

    let with_public = engine
        .find_dead_code(&DeadCodeOptions {
            include_public: true,
            path_prefix: Some("src/".into()),
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    assert!(
        with_public
            .symbols
            .iter()
            .any(|s| s.name == "public_api" && s.confidence == DeadCodeConfidence::Low)
    );

    let none = engine
        .find_dead_code(&DeadCodeOptions {
            path_prefix: Some("other/".into()),
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    assert!(none.symbols.is_empty());
}
