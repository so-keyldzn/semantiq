//! Tests for the call graph, type hierarchy and dead-code analyses.

use super::*;
use semantiq_parser::{
    Language, LanguageSupport, ReferenceExtractor, StructureExtractor, SymbolExtractor,
};

/// Index `files` (path, source) under a temp root, with structure.
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
        let tree = support
            .parse_path(language, std::path::Path::new(path), source)
            .unwrap();
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

/// Six files sorting before `zz.rs`, each with a homonym `new` calling
/// `shared`: more call edges than a `max_edges = 1` lookup examines (5).
fn homonym_project() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = (0..6)
        .map(|i| {
            (
                format!("a{i}.rs"),
                "fn new() {\n    shared();\n}\n".to_string(),
            )
        })
        .collect();
    files.push((
        "zz.rs".to_string(),
        "fn new() {\n    target();\n}\nfn target() {}\npub fn shared() {}\nfn local() {\n    shared();\n}\n"
            .to_string(),
    ));
    files
}

#[test]
fn test_callees_of_a_common_name_are_not_hidden_by_homonyms() {
    // Regression for #42: the store capped the edges in path order before
    // the location filter, so `callees` of `new` in zz.rs came back empty.
    let files = homonym_project();
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let (engine, _root) = index_project(&files);
    let graph = engine
        .call_graph("new", Some("zz.rs"), CallDirection::Callees, 1, 1)
        .unwrap();
    let callees: Vec<&str> = graph.callees.iter().map(|c| c.callee.as_str()).collect();
    assert_eq!(callees, vec!["target"]);
    assert!(graph.external_callees.is_empty());
    assert!(!graph.truncated);
}

#[test]
fn test_callers_of_a_common_name_list_trusted_ones_first_and_flag_the_cut() {
    let files = homonym_project();
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let (engine, _root) = index_project(&files);
    let graph = engine
        .call_graph("shared", None, CallDirection::Callers, 1, 1)
        .unwrap();
    assert_eq!(graph.callers.len(), 1);
    assert_eq!(graph.callers[0].file_path, "zz.rs");
    assert_eq!(graph.callers[0].caller.as_deref(), Some("local"));
    assert_eq!(graph.callers[0].confidence, ImpactConfidence::SameFile);
    assert!(graph.truncated);

    // A budget above the number of edges: complete, not truncated.
    let graph = engine
        .call_graph("shared", None, CallDirection::Callers, 1, 100)
        .unwrap();
    assert_eq!(graph.callers.len(), 7);
    assert!(!graph.truncated);
}

#[test]
fn test_callers_cut_of_name_only_homonyms_beyond_depth_one_is_not_a_truncation() {
    // `local` (two definitions) is called by six homonym `new`s sorting
    // first; at depth 2 those name-only edges are not reported, so cutting
    // them is not a truncation.
    let mut files: Vec<(String, String)> = (0..6)
        .map(|i| {
            (
                format!("a{i}.rs"),
                "fn new() {\n    local();\n}\n".to_string(),
            )
        })
        .collect();
    files.push(("b.rs".to_string(), "pub fn local() {}\n".to_string()));
    files.push((
        "zz.rs".to_string(),
        "pub fn shared() {}\nfn local() {\n    shared();\n}\n".to_string(),
    ));
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let (engine, _root) = index_project(&files);
    let graph = engine
        .call_graph("shared", None, CallDirection::Callers, 2, 1)
        .unwrap();
    let callers: Vec<_> = graph
        .callers
        .iter()
        .map(|c| (c.depth, c.caller.clone().unwrap_or_default()))
        .collect();
    assert_eq!(callers, vec![(1, "local".to_string())]);
    assert!(!graph.truncated);
}

#[test]
fn test_type_hierarchy_flags_a_cut_at_the_edge_cap() {
    // Regression for #42: a lookup limited to `max_edges` could not tell
    // that more relations existed, so the cut went unflagged.
    let (engine, _root) = index_project(&[(
        "model.py",
        "class Base:\n    pass\n\nclass A(Base):\n    pass\n\nclass B(Base):\n    pass\n\nclass C(Base):\n    pass\n",
    )]);
    let h = engine.type_hierarchy("Base", 3, 2).unwrap();
    assert_eq!(h.subtypes.len(), 2);
    assert!(h.truncated);

    let h = engine.type_hierarchy("Base", 3, 3).unwrap();
    assert_eq!(h.subtypes.len(), 3);
    assert!(!h.truncated);
}

#[test]
fn test_type_hierarchy_cut_in_one_direction_does_not_stop_the_other() {
    // Regression: one shared cut flag made the subtype walk stop after its
    // first node once the supertype walk had hit the cap, dropping the
    // subtypes at depth >= 2.
    let (engine, _root) = index_project(&[(
        "model.py",
        "class Mixin1:\n    pass\n\nclass Mixin2:\n    pass\n\nclass Mixin3:\n    pass\n\nclass Model(Mixin1, Mixin2, Mixin3):\n    pass\n\nclass Child(Model):\n    pass\n\nclass GrandChild(Child):\n    pass\n",
    )]);
    let h = engine.type_hierarchy("Model", 3, 2).unwrap();
    assert_eq!(h.supertypes.len(), 2);
    let down: Vec<_> = h
        .subtypes
        .iter()
        .map(|e| (e.depth, e.sub_type.as_str()))
        .collect();
    assert_eq!(down, vec![(1, "Child"), (2, "GrandChild")]);
    assert!(h.truncated);
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
fn test_call_graph_restricted_to_a_file_without_definition_is_empty() {
    // #136: every homonym's edges were listed as name_only.
    let (engine, _root) = index_project(&[
        (
            "util.rs",
            "pub fn shared_name() {}\nfn a() { shared_name(); }\n",
        ),
        (
            "other.rs",
            "pub fn shared_name() {}\nfn b() { shared_name(); }\n",
        ),
    ]);
    let graph = engine
        .call_graph("shared_name", Some("nope.rs"), CallDirection::Both, 2, 100)
        .unwrap();
    assert!(graph.definitions.is_empty());
    assert!(graph.callers.is_empty(), "{:?}", graph.callers);
    assert!(graph.callees.is_empty());
    assert!(!graph.truncated);
}

#[test]
fn test_impact_enclosing_symbol_is_the_function_not_a_local_binding() {
    // #91: `const value = load()` was the enclosing symbol, and the local
    // `value` of `other` a depth-2 site.
    let (engine, _root) = index_project(&[(
        "a.ts",
        "export function load(): number { return 1; }\n\
         export function useLoad(): number {\n  const value = load();\n  return value;\n}\n\
         export function other(): number {\n  const value = 3;\n  return value;\n}\n\
         export function entry(): number { return useLoad(); }\n",
    )]);
    let analysis = engine.analyze_impact("load", None, 3, 100).unwrap();
    let sites: Vec<(usize, &str, usize, Option<&str>)> = analysis
        .sites
        .iter()
        .map(|s| {
            (
                s.depth,
                s.target.as_str(),
                s.line,
                s.enclosing.as_ref().map(|e| e.name.as_str()),
            )
        })
        .collect();
    assert_eq!(
        sites,
        vec![
            (1, "load", 3, Some("useLoad")),
            (2, "useLoad", 10, Some("entry")),
        ]
    );
}

#[test]
fn test_impact_enclosing_symbol_falls_back_to_a_top_level_binding() {
    let (engine, _root) = index_project(&[(
        "a.ts",
        "export function load(): number { return 1; }\nexport const cached = load();\n\
         export function read(): number { return cached; }\n",
    )]);
    let analysis = engine.analyze_impact("load", None, 3, 100).unwrap();
    let sites: Vec<(usize, &str, usize, Option<&str>)> = analysis
        .sites
        .iter()
        .map(|s| {
            (
                s.depth,
                s.target.as_str(),
                s.line,
                s.enclosing.as_ref().map(|e| e.name.as_str()),
            )
        })
        .collect();
    assert_eq!(
        sites,
        vec![
            (1, "load", 2, Some("cached")),
            (2, "cached", 3, Some("read"))
        ]
    );
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

/// #64: a function named only in an attribute string (`#[serde(default =
/// "f")]`) is used by the derive macro and must not be reported as dead.
#[test]
fn test_dead_code_keeps_functions_named_in_attribute_strings() {
    let source = "fn default_true() -> bool {\n    true\n}\n\nfn is_false(b: &bool) -> bool {\n    !b\n}\n\nfn really_unused() -> bool {\n    false\n}\n\n#[derive(serde::Deserialize)]\npub struct Cfg {\n    #[serde(default = \"default_true\", skip_serializing_if = \"is_false\")]\n    pub on: bool,\n}\n";
    let (engine, _root) = index_project(&[("src/lib.rs", source)]);
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
        vec![("really_unused", DeadCodeConfidence::High)],
        "{report:?}"
    );
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

/// TS/JS `export` is public (#63): exported but unreferenced declarations are
/// excluded like Rust `pub`, and reported with low confidence under
/// `include_public`. Private declarations keep the high confidence.
#[test]
fn test_dead_code_typescript_exports_are_public() {
    let tsx = "export default function Panel() { return <Unused />; }\nexport function Unused() { return null; }\nexport const unusedConst = () => 1;\nfunction localDead() { return 1; }\n";
    let barrel = "export { Panel } from './components/Panel';\n";
    // NestJS / Angular: the decorator precedes `export` but is not part of
    // the class signature, so the class stays public.
    let service = "@Injectable()\nexport class UnusedService {\n  run() { return 1; }\n}\n";
    let (engine, _root) = index_project(&[
        ("src/components/Panel.tsx", tsx),
        ("src/index.ts", barrel),
        ("src/service.ts", service),
    ]);
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
        vec![("localDead", DeadCodeConfidence::High)],
        "{report:?}"
    );
    // `Panel` is re-exported by the barrel (a use), `Unused` is rendered by
    // Panel; `unusedConst` and the decorated `UnusedService` are dead but
    // public (`run` is a public method of a class nobody uses).
    assert_eq!(report.excluded.public, 3, "{report:?}");

    let with_public = engine
        .find_dead_code(&DeadCodeOptions {
            include_public: true,
            limit: 100,
            ..Default::default()
        })
        .unwrap();
    let dead: Vec<_> = with_public
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.confidence))
        .collect();
    assert_eq!(
        dead,
        vec![
            ("localDead", DeadCodeConfidence::High),
            ("unusedConst", DeadCodeConfidence::Low),
            ("UnusedService", DeadCodeConfidence::Low),
            ("run", DeadCodeConfidence::Low),
        ],
        "{with_public:?}"
    );
}
