//! Tests for RetrievalEngine.

use super::search::{WEIGHT_SEMANTIC, WEIGHT_SYMBOL, WEIGHT_TEXT, normalize_and_weight};
use super::*;
use crate::results::{SearchResult, SearchResultKind};

fn mk_result(score: f32) -> SearchResult {
    SearchResult::new(
        SearchResultKind::Symbol,
        "f.rs".to_string(),
        1,
        1,
        "x".to_string(),
        score,
    )
}

#[test]
fn test_normalize_and_weight_min_max() {
    let mut results = vec![mk_result(0.5), mk_result(1.0), mk_result(0.75)];
    normalize_and_weight(&mut results, 1.0);

    // Min-max: lowest -> 0.0, highest -> 1.0, middle in between.
    assert!((results[0].score - 0.0).abs() < 1e-6, "min should map to 0");
    assert!((results[1].score - 1.0).abs() < 1e-6, "max should map to 1");
    assert!(results[2].score > 0.0 && results[2].score < 1.0);

    // Relative order within the strategy is preserved.
    assert!(results[1].score > results[2].score);
    assert!(results[2].score > results[0].score);
}

#[test]
fn test_normalize_and_weight_applies_weight() {
    let mut results = vec![mk_result(0.2), mk_result(0.9)];
    normalize_and_weight(&mut results, WEIGHT_TEXT);

    // Top result maps to 1.0 * weight; bottom maps to 0.0 * weight.
    assert!((results[1].score - WEIGHT_TEXT).abs() < 1e-6);
    assert!((results[0].score - 0.0).abs() < 1e-6);
}

#[test]
fn test_normalize_and_weight_single_result_keeps_full_strength() {
    // A lone result has no spread to normalize against; it must keep full
    // weight (mapped to 1.0 * weight) rather than collapse to 0.
    let mut results = vec![mk_result(0.42)];
    normalize_and_weight(&mut results, WEIGHT_SYMBOL);
    assert!((results[0].score - WEIGHT_SYMBOL).abs() < 1e-6);
}

#[test]
fn test_normalize_and_weight_equal_scores_keep_full_strength() {
    // All-equal scores (span == 0) must not collapse to 0.
    let mut results = vec![mk_result(0.6), mk_result(0.6), mk_result(0.6)];
    normalize_and_weight(&mut results, WEIGHT_SEMANTIC);
    for r in &results {
        assert!((r.score - WEIGHT_SEMANTIC).abs() < 1e-6);
    }
}

#[test]
fn test_normalize_and_weight_empty_is_noop() {
    let mut results: Vec<SearchResult> = Vec::new();
    normalize_and_weight(&mut results, WEIGHT_SYMBOL);
    assert!(results.is_empty());
}

#[test]
fn test_strategy_weights_ordering() {
    // Documented intent: symbol >= semantic > text so an exact symbol hit
    // outranks a fuzzy semantic hit which outranks a plain grep hit, all else
    // equal (i.e. each at full normalized strength 1.0).
    //
    // These are compile-time constants, so we enforce the ordering in a `const`
    // block: this fails the *build* (not just the test) if the weights are ever
    // reordered, and avoids clippy's `assertions_on_constants` lint that fires
    // on a runtime `assert!` over constant operands.
    const {
        assert!(WEIGHT_SYMBOL >= WEIGHT_SEMANTIC);
        assert!(WEIGHT_SEMANTIC > WEIGHT_TEXT);
    }
}

#[test]
fn test_min_score_aligned_with_semantic_floor() {
    // Dead-zone alignment: the post-merge global floor and the semantic
    // similarity floor must be the same single value (no silent gap).
    use crate::query::SearchOptions;
    assert!(
        (SearchOptions::DEFAULT_MIN_SCORE - RetrievalEngine::SEMANTIC_MIN_SIMILARITY).abs() < 1e-6
    );
}

/// Calculate cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a * norm_b)
}

#[test]
fn test_cosine_similarity() {
    let a = vec![1.0, 0.0, 0.0];
    let b = vec![1.0, 0.0, 0.0];
    assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.0001);

    let c = vec![1.0, 0.0, 0.0];
    let d = vec![0.0, 1.0, 0.0];
    assert!((cosine_similarity(&c, &d)).abs() < 0.0001);
}

#[test]
fn test_cosine_similarity_opposite_vectors() {
    let a = vec![1.0, 0.0, 0.0];
    let b = vec![-1.0, 0.0, 0.0];
    assert!((cosine_similarity(&a, &b) + 1.0).abs() < 0.0001);
}

#[test]
fn test_cosine_similarity_same_direction() {
    let a = vec![1.0, 2.0, 3.0];
    let b = vec![2.0, 4.0, 6.0];
    assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.0001);
}

#[test]
fn test_cosine_similarity_empty_vectors() {
    let a: Vec<f32> = vec![];
    let b: Vec<f32> = vec![];
    assert_eq!(cosine_similarity(&a, &b), 0.0);
}

#[test]
fn test_cosine_similarity_different_lengths() {
    let a = vec![1.0, 0.0];
    let b = vec![1.0, 0.0, 0.0];
    assert_eq!(cosine_similarity(&a, &b), 0.0);
}

#[test]
fn test_cosine_similarity_zero_vector() {
    let a = vec![0.0, 0.0, 0.0];
    let b = vec![1.0, 2.0, 3.0];
    assert_eq!(cosine_similarity(&a, &b), 0.0);
}

#[test]
fn test_dependency_info_struct() {
    let dep = DependencyInfo {
        target_path: "src/utils.rs".to_string(),
        import_name: Some("utils".to_string()),
        kind: "local".to_string(),
    };

    assert_eq!(dep.target_path, "src/utils.rs");
    assert_eq!(dep.import_name, Some("utils".to_string()));
    assert_eq!(dep.kind, "local");
}

#[test]
fn test_symbol_definition_struct() {
    let def = SymbolDefinition {
        file_path: "src/lib.rs".to_string(),
        kind: "function".to_string(),
        start_line: 10,
        end_line: 20,
        signature: Some("fn process_data()".to_string()),
        doc_comment: Some("/// Process data".to_string()),
    };

    assert_eq!(def.file_path, "src/lib.rs");
    assert_eq!(def.kind, "function");
    assert_eq!(def.start_line, 10);
    assert_eq!(def.end_line, 20);
}

#[test]
fn test_symbol_explanation_not_found() {
    let explanation = SymbolExplanation {
        name: "unknown_symbol".to_string(),
        found: false,
        definitions: Vec::new(),
        usage_count: 0,
        related_symbols: Vec::new(),
    };

    assert!(!explanation.found);
    assert!(explanation.definitions.is_empty());
    assert_eq!(explanation.usage_count, 0);
}

#[test]
fn test_symbol_explanation_found() {
    let explanation = SymbolExplanation {
        name: "process_data".to_string(),
        found: true,
        definitions: vec![SymbolDefinition {
            file_path: "src/lib.rs".to_string(),
            kind: "function".to_string(),
            start_line: 10,
            end_line: 20,
            signature: Some("fn process_data()".to_string()),
            doc_comment: None,
        }],
        usage_count: 5,
        related_symbols: vec!["helper".to_string(), "utils".to_string()],
    };

    assert!(explanation.found);
    assert_eq!(explanation.definitions.len(), 1);
    assert_eq!(explanation.usage_count, 5);
    assert_eq!(explanation.related_symbols.len(), 2);
}

#[test]
fn test_min_score_does_not_drop_weakest_hit_of_a_strategy() {
    use semantiq_parser::{Symbol, SymbolKind};

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let file_id = store.insert_file("cfg.rs", Some("rust"), "", 0, 0).unwrap();
    let mk = |name: &str, line: usize| Symbol {
        name: name.to_string(),
        // Variable (not Function): the function kind boost caps both scores
        // at 1.0, which would hide the min-max collapse.
        kind: SymbolKind::Variable,
        start_line: line,
        end_line: line,
        start_byte: 0,
        end_byte: 0,
        signature: None,
        doc_comment: None,
        parent: None,
    };
    store
        .insert_symbols(
            file_id,
            &[mk("parse_config", 1), mk("parse_config_file", 2)],
        )
        .unwrap();

    // Empty root: no text-search hits, only the two symbol hits.
    let root = tempfile::TempDir::new().unwrap();
    let engine = RetrievalEngine::new(store, root.path().to_str().unwrap());

    let results = engine.search("parse_config", 10, None).unwrap();
    let names: Vec<_> = results
        .results
        .iter()
        .filter_map(|r| r.metadata.symbol_name.as_deref())
        .collect();
    assert!(names.contains(&"parse_config"), "{names:?}");
    assert!(
        names.contains(&"parse_config_file"),
        "prefix match must survive the default min_score: {names:?}"
    );
}

/// Index `files` (path, Rust source) under a temp root with symbols and AST references.
fn index_rust_project(files: &[(&str, &str)]) -> (RetrievalEngine, tempfile::TempDir) {
    index_rust_project_with(files, RetrievalEngine::new)
}

fn index_rust_project_with(
    files: &[(&str, &str)],
    build: fn(Arc<IndexStore>, &str) -> RetrievalEngine,
) -> (RetrievalEngine, tempfile::TempDir) {
    use semantiq_parser::{Language, LanguageSupport, ReferenceExtractor, SymbolExtractor};

    // Text search skips hidden paths, so avoid the default `.tmpXXXX` name.
    let root = tempfile::Builder::new()
        .prefix("semantiq-refs")
        .tempdir()
        .unwrap();
    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let mut support = LanguageSupport::new().unwrap();
    for (path, source) in files {
        std::fs::write(root.path().join(path), source).unwrap();
        let file_id = store
            .insert_file(path, Some("rust"), source, source.len() as i64, 0)
            .unwrap();
        let tree = support.parse(Language::Rust, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, Language::Rust).unwrap();
        store.insert_symbols(file_id, &symbols).unwrap();
        let refs = ReferenceExtractor::extract(&tree, source, Language::Rust);
        store.insert_references(file_id, &refs).unwrap();
    }
    let engine = build(store, root.path().to_str().unwrap());
    (engine, root)
}

#[test]
fn test_without_embeddings_answers_non_search_queries() {
    let (engine, _root) = index_rust_project_with(
        &[
            (
                "lib.rs",
                "pub fn core_op() -> u32 { 1 }
",
            ),
            ("main.rs", "fn main() {\n    core_op();\n}\n"),
        ],
        RetrievalEngine::without_embeddings,
    );
    assert!(engine.embedding_model.is_none());
    assert!(engine.distance_collector().is_none());

    let refs = engine.find_references("core_op", 50).unwrap();
    assert_eq!(refs.results.len(), 2);
    assert!(engine.explain_symbol("core_op").unwrap().found);
    let impact = engine.analyze_impact("core_op", None, 2, 100).unwrap();
    assert_eq!(impact.sites.len(), 1);
    assert!(engine.get_dependencies("main.rs").is_ok());
    assert!(engine.get_dependents("lib.rs").is_ok());
    // Search still works, without its semantic strategy.
    assert!(engine.search("core_op", 10, None).is_ok());
}

#[test]
fn test_find_references_uses_ast_not_text() {
    let (engine, _root) = index_rust_project(&[
        ("lib.rs", "pub fn compute() -> u32 { 1 }\n"),
        (
            "main.rs",
            "// compute is documented here\nfn main() {\n    let s = \"compute\";\n    let compute_all = 2;\n    compute();\n}\n",
        ),
    ]);

    let results = engine.find_references("compute", 50).unwrap();
    let hits: Vec<_> = results
        .results
        .iter()
        .map(|r| {
            (
                r.file_path.as_str(),
                r.start_line,
                r.metadata.match_type.as_deref().unwrap_or(""),
            )
        })
        .collect();

    // Comment (line 1), string (line 3) and `compute_all` (line 4) are excluded.
    assert_eq!(
        hits,
        vec![("lib.rs", 1, "definition"), ("main.rs", 5, "call")],
        "{hits:?}"
    );
    assert_eq!(results.results[1].content, "compute();");
}

#[test]
fn test_find_references_falls_back_to_text_for_unknown_names() {
    // A name that only appears inside a string has no AST reference.
    let (engine, _root) = index_rust_project(&[(
        "main.rs",
        "fn main() {\n    let key = \"only_in_string\";\n}\n",
    )]);

    let results = engine.find_references("only_in_string", 50).unwrap();
    assert_eq!(results.results.len(), 1);
    assert_eq!(
        results.results[0].metadata.match_type.as_deref(),
        Some("text")
    );
}

#[test]
fn test_explain_usage_count_from_ast() {
    let (engine, _root) = index_rust_project(&[(
        "lib.rs",
        "fn helper() {}\n// helper helper helper\nfn a() { helper(); }\nfn b() { helper(); }\n",
    )]);

    let explanation = engine.explain_symbol("helper").unwrap();
    assert!(explanation.found);
    assert_eq!(explanation.usage_count, 2);
}

fn impact_sites(analysis: &super::ImpactAnalysis) -> Vec<(usize, &str, usize, &str)> {
    analysis
        .sites
        .iter()
        .map(|s| (s.depth, s.file_path.as_str(), s.line, s.confidence.as_str()))
        .collect()
}

#[test]
fn test_impact_follows_callers_and_flags_tests() {
    let (engine, _root) = index_rust_project(&[
        (
            "lib.rs",
            "pub fn core_op() -> u32 { 1 }\npub fn helper() -> u32 { core_op() }\n",
        ),
        (
            "app.rs",
            "fn run() { helper(); }\nfn other() { let helper = 2; }\n",
        ),
        ("test_app.rs", "fn test_run() { run(); }\n"),
    ]);

    let analysis = engine.analyze_impact("core_op", None, 3, 100).unwrap();
    assert_eq!(analysis.definitions.len(), 1);
    assert_eq!(
        impact_sites(&analysis),
        vec![
            (1, "lib.rs", 2, "same_file"),
            // `let helper = 2` is a local variable, not a call: not followed.
            (2, "app.rs", 1, "unique_name"),
            (3, "test_app.rs", 1, "unique_name"),
        ]
    );
    assert_eq!(
        analysis.sites[1]
            .enclosing
            .as_ref()
            .map(|e| e.name.as_str()),
        Some("run")
    );
    assert!(analysis.sites[2].is_test);
    assert!(!analysis.truncated);
}

#[test]
fn test_impact_does_not_propagate_through_homonyms() {
    let (engine, _root) = index_rust_project(&[
        ("a.rs", "pub fn process() {}\n"),
        ("b.rs", "pub fn process() {}\n"),
        ("c.rs", "fn go() { process(); }\n"),
        ("d.rs", "fn main() { go(); }\n"),
    ]);

    let analysis = engine
        .analyze_impact("process", Some("a.rs"), 3, 100)
        .unwrap();
    assert_eq!(analysis.definitions.len(), 1);
    // c.rs might call b.rs's `process`: reported, but `go` is not followed.
    assert_eq!(impact_sites(&analysis), vec![(1, "c.rs", 1, "name_only")]);
}

#[test]
fn test_impact_respects_depth_and_site_limits() {
    let (engine, _root) = index_rust_project(&[(
        "lib.rs",
        "fn base() {}\nfn one() { base(); }\nfn two() { one(); }\nfn three() { two(); }\n",
    )]);

    let shallow = engine.analyze_impact("base", None, 1, 100).unwrap();
    assert_eq!(shallow.sites.len(), 1);

    let capped = engine.analyze_impact("base", None, 4, 2).unwrap();
    assert_eq!(capped.sites.len(), 2);
    assert!(capped.truncated);
}

#[test]
fn test_is_test_location() {
    use super::is_test_location;
    assert!(is_test_location("crates/x/tests/it.rs", None));
    assert!(is_test_location("src/store/tests.rs", None));
    assert!(is_test_location("web/app.spec.ts", None));
    assert!(is_test_location("pkg/handler_test.go", None));
    assert!(is_test_location("src/lib.rs", Some("test_parse")));
    assert!(!is_test_location("src/lib.rs", Some("parse")));
    assert!(!is_test_location("src/contest.rs", None));
}
