//! Tests for RetrievalEngine.

use super::search::{WEIGHT_SEMANTIC, WEIGHT_SYMBOL, WEIGHT_TEXT, weight_strategy};
use super::*;
use crate::query::SearchOptions;
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
fn test_weight_strategy_two_hits_keep_their_strength() {
    // Regression for #34: min-max normalization sent the weakest hit of a
    // strategy to 0.0 (`parse_configuration` below every grep line).
    let mut results = vec![mk_result(1.0), mk_result(0.8)];
    weight_strategy(&mut results, WEIGHT_SYMBOL);

    assert!((results[0].score - WEIGHT_SYMBOL).abs() < 1e-6);
    assert!(
        (results[1].score - 0.8 * WEIGHT_SYMBOL).abs() < 1e-6,
        "the weaker hit keeps its raw score: {}",
        results[1].score
    );
    assert!(
        results[1].score > 0.5 * WEIGHT_TEXT,
        "a real symbol hit outranks weak grep lines"
    );
}

#[test]
fn test_weight_strategy_single_hit_is_not_inflated() {
    // Regression for #34: a lone weak hit used to map to full weight and
    // outrank the best semantic chunk.
    let mut results = vec![mk_result(0.45)];
    weight_strategy(&mut results, WEIGHT_SYMBOL);
    assert!((results[0].score - 0.45 * WEIGHT_SYMBOL).abs() < 1e-6);

    let mut semantic = vec![mk_result(0.95)];
    weight_strategy(&mut semantic, WEIGHT_SEMANTIC);
    assert!(semantic[0].score > results[0].score);
}

#[test]
fn test_weight_strategy_applies_weight() {
    let mut results = vec![mk_result(0.2), mk_result(0.9)];
    weight_strategy(&mut results, WEIGHT_TEXT);
    assert!((results[1].score - 0.9 * WEIGHT_TEXT).abs() < 1e-6);
    assert!((results[0].score - 0.2 * WEIGHT_TEXT).abs() < 1e-6);
}

#[test]
fn test_weight_strategy_empty_is_noop() {
    let mut results: Vec<SearchResult> = Vec::new();
    weight_strategy(&mut results, WEIGHT_SYMBOL);
    assert!(results.is_empty());
}

#[test]
fn test_exact_symbol_match_outranks_boosted_prefix_match() {
    // Regression for #34: the boosted scores were clamped to 1.0, so an exact
    // function match tied with a prefix match of a short function name.
    use semantiq_parser::{Symbol, SymbolKind};

    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let file_id = store.insert_file("cfg.rs", Some("rust"), "", 0).unwrap();
    let mk = |name: &str, line: usize| Symbol {
        name: name.to_string(),
        kind: SymbolKind::Function,
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
            &[mk("parse_configuration", 1), mk("parse_config", 2)],
        )
        .unwrap();
    let root = tempfile::TempDir::new().unwrap();
    let engine = RetrievalEngine::new(store, root.path().to_str().unwrap());

    let results = engine.search("parse_config", 10, None).unwrap().results;
    let scored: Vec<(&str, f32)> = results
        .iter()
        .map(|r| (r.metadata.symbol_name.as_deref().unwrap_or(""), r.score))
        .collect();
    assert_eq!(scored[0].0, "parse_config", "{scored:?}");
    assert_eq!(scored[1].0, "parse_configuration", "{scored:?}");
    assert!(scored[0].1 > scored[1].1, "{scored:?}");
    assert!(
        scored.iter().all(|(_, s)| *s <= 1.0 && *s > 0.0),
        "{scored:?}"
    );
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
        line: 3,
        end_line: 3,
    };

    assert_eq!(dep.target_path, "src/utils.rs");
    assert_eq!(dep.line, 3);
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
    let file_id = store.insert_file("cfg.rs", Some("rust"), "", 0).unwrap();
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

/// Index `files` (path, Rust source) under a temp root with symbols, chunks
/// and AST references.
fn index_rust_project(files: &[(&str, &str)]) -> (RetrievalEngine, tempfile::TempDir) {
    index_rust_project_with(files, RetrievalEngine::new)
}

fn index_rust_project_with(
    files: &[(&str, &str)],
    build: fn(Arc<IndexStore>, &str) -> RetrievalEngine,
) -> (RetrievalEngine, tempfile::TempDir) {
    index_project_with(files, build)
}

/// Index `files` (path, source) of any supported language, chosen by the
/// extension of each path, under a temp root with symbols, chunks and AST
/// references.
fn index_project(files: &[(&str, &str)]) -> (RetrievalEngine, tempfile::TempDir) {
    index_project_with(files, RetrievalEngine::new)
}

fn index_project_with(
    files: &[(&str, &str)],
    build: fn(Arc<IndexStore>, &str) -> RetrievalEngine,
) -> (RetrievalEngine, tempfile::TempDir) {
    use semantiq_parser::{
        ChunkExtractor, Language, LanguageSupport, ReferenceExtractor, SymbolExtractor,
    };

    let root = tempfile::Builder::new()
        .prefix("semantiq-refs")
        .tempdir()
        .unwrap();
    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let mut support = LanguageSupport::new().unwrap();
    let chunker = ChunkExtractor::new();
    for (path, source) in files {
        let full = root.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, source).unwrap();
        let language = Language::from_path(std::path::Path::new(path)).unwrap();
        let file_id = store
            .insert_file(path, Some(language.name()), source, 0)
            .unwrap();
        let tree = support.parse(language, source).unwrap();
        let symbols = SymbolExtractor::extract(&tree, source, language).unwrap();
        store.insert_symbols(file_id, &symbols).unwrap();
        let chunks = chunker.extract(&tree, source, language).unwrap();
        store.insert_chunks(file_id, &chunks).unwrap();
        let refs = ReferenceExtractor::extract(&tree, source, language);
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
fn test_text_fallback_matches_whole_identifiers_case_sensitively() {
    // Regression for #43: the fallback expanded case variants and matched
    // substrings, so `id` hit `uuid`, `width`, `provider` and `IDENTITY`.
    let (engine, _root) = index_rust_project(&[(
        "main.rs",
        "fn main() {\n    let a = \"id\";\n    let b = \"uuid\";\n    let c = \"width\";\n    let d = \"ID\";\n    let e = \"user_id\";\n    let f = \"x.id\";\n}\n",
    )]);

    let results = engine.find_references("id", 50).unwrap();
    let hits: Vec<_> = results
        .results
        .iter()
        .map(|r| (r.start_line, r.metadata.match_type.as_deref().unwrap_or("")))
        .collect();
    assert_eq!(hits, vec![(2, "text"), (7, "text")], "{hits:?}");
}

/// The text strategy walks the project like the indexer: a symlink to a file
/// outside the project is not read through (#109), and a credential store
/// is not read even under an explicit `file_type` filter (#113).
#[cfg(unix)]
#[test]
fn test_text_search_skips_symlinks_and_credential_files() {
    use crate::query::SearchOptions;
    use std::os::unix::fs::symlink;

    let tmp = tempfile::Builder::new()
        .prefix("semantiq-links")
        .tempdir()
        .unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("private.js"), "const needle_outside = 1;\n").unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("app.js"), "const needle_inside = 1;\n").unwrap();
    std::fs::write(root.join("secrets.env"), "NEEDLE_TOKEN=needle_secret\n").unwrap();
    symlink(outside.join("private.js"), root.join("linked.js")).unwrap();
    symlink("app.js", root.join("alias.js")).unwrap();
    // Text search answers from the index, so what the indexer's walker
    // leaves out (symlinks, credential files) is never text-searched.
    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    semantiq_index::AutoIndexer::without_watcher(store.clone(), root.clone())
        .unwrap()
        .initial_index()
        .unwrap();
    let engine = RetrievalEngine::new(store, root.to_str().unwrap());

    let results = engine.search("needle", 10, None).unwrap().results;
    let mut files: Vec<&str> = results.iter().map(|r| r.file_path.as_str()).collect();
    files.sort();
    files.dedup();
    assert_eq!(files, ["app.js"], "{results:?}");

    let options = SearchOptions::new().with_file_types(vec!["env".to_string()]);
    let results = engine.search("needle", 10, Some(options)).unwrap().results;
    assert!(results.is_empty(), "{results:?}");
}

#[test]
fn test_text_search_skips_minified_files_and_cuts_long_lines() {
    // Regression for #44: a minified bundle returned its whole line as the
    // hit's content.
    let minified = format!("!function(){{var needle=1;{}}}();\n", "a=1;".repeat(2000));
    // A normal file with one long generated line: the hit is cut, not dropped.
    let long_line = format!(
        "const TABLE = [{}needle{}];",
        "0,".repeat(1000),
        ",0".repeat(1000)
    );
    let filler = "function helper_number_x(value) { return value + 1; }\n".repeat(20);
    let table = format!("{filler}{long_line}\n{filler}");
    let (engine, _root) = index_project(&[
        ("vendor.min.js", minified.as_str()),
        ("app.js", "function run() {\n  return needle();\n}\n"),
        ("table.js", table.as_str()),
    ]);

    let results = engine.search("needle", 10, None).unwrap().results;
    // `var needle` is a symbol of the bundle; the text strategy skips it.
    let files: Vec<&str> = text_hits(&results).into_iter().map(|h| h.0).collect();
    assert!(files.contains(&"app.js"), "{files:?}");
    assert!(files.contains(&"table.js"), "{files:?}");
    assert!(!files.contains(&"vendor.min.js"), "{files:?}");
    let cut = results
        .iter()
        .find(|r| r.file_path == "table.js" && r.kind == SearchResultKind::TextMatch)
        .unwrap();
    assert!(cut.content.chars().count() <= crate::text_searcher::MAX_LINE_CHARS + 2);
    assert!(cut.content.contains("needle"));
}

/// Lines of the text hits, `(file, line, content)`.
fn text_hits(results: &[SearchResult]) -> Vec<(&str, usize, &str)> {
    results
        .iter()
        .filter(|r| r.kind == SearchResultKind::TextMatch)
        .map(|r| (r.file_path.as_str(), r.start_line, r.content.as_str()))
        .collect()
}

#[test]
fn test_text_search_answers_from_the_index_with_file_lines() {
    // Regression for #160: the text strategy read every file of the
    // project. It now comes from the chunks, with the line numbers of the
    // file (a chunk starts anywhere) and the long-file windows deduplicated.
    let body = (0..120)
        .map(|i| format!("fn filler_{i}() {{\n    let v = {i};\n}}\n"))
        .collect::<String>();
    let source = format!("{body}fn target() {{\n    needle_here();\n}}\n");
    let (engine, _root) =
        index_project(&[("big.rs", source.as_str()), ("other.rs", "fn x() {}\n")]);
    let chunks = engine.store.get_chunks_by_file(1).unwrap();
    assert!(chunks.len() > 1, "the file must span several chunks");

    let results = engine.search("needle_here", 10, None).unwrap().results;
    let needle_line = source
        .lines()
        .position(|l| l.contains("needle_here"))
        .unwrap()
        + 1;
    assert_eq!(
        text_hits(&results),
        vec![("big.rs", needle_line, "needle_here();")]
    );

    // The project files are not read: a file edited on disk but not
    // reindexed keeps answering from the index.
    std::fs::write(_root.path().join("other.rs"), "fn x() { needle_here(); }\n").unwrap();
    let results = engine.search("needle_here", 10, None).unwrap().results;
    assert_eq!(text_hits(&results).len(), 1);
}

#[test]
fn test_text_search_keeps_attributes_and_directives() {
    // Regression for #94: every line starting with `#` was skipped as a
    // comment, so `#[derive(Serialize)]` and `#define` were invisible.
    let (engine, _root) = index_project(&[
        (
            "model.rs",
            "use serde::Serialize;\n\n#[derive(Serialize)]\npub struct Model {\n    id: u32,\n}\n",
        ),
        ("buf.c", "#define MAX_BUF 10\nint buf[MAX_BUF];\n"),
        ("conf.py", "# MAX_BUF is the limit\nMAX_BUF = 10\n"),
    ]);

    let results = engine
        .search("derive(Serialize)", 10, None)
        .unwrap()
        .results;
    assert_eq!(
        text_hits(&results),
        vec![("model.rs", 3, "#[derive(Serialize)]")]
    );

    let results = engine.search("MAX_BUF", 10, None).unwrap().results;
    let mut hits = text_hits(&results);
    hits.sort();
    assert_eq!(
        hits,
        vec![
            ("buf.c", 1, "#define MAX_BUF 10"),
            ("buf.c", 2, "int buf[MAX_BUF];"),
        ],
        "the C directive is code, the Python comment is not"
    );
}

#[test]
fn test_punctuation_only_query_terms_do_not_match_every_line() {
    // Regression for #92: `-` expanded to `_` and ``, and the empty regex
    // matched every line of the first file walked.
    let (engine, _root) = index_project(&[(
        "main.rs",
        "use std::fmt;\n\nfn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}\n",
    )]);

    for query in ["zzqq - yyxx", "zzqq -", "_"] {
        let results = engine.search(query, 10, None).unwrap().results;
        assert!(
            results.is_empty(),
            "{query:?} matched {:?}",
            text_hits(&results)
        );
    }
}

#[test]
fn test_symbol_kind_filter_applies_to_every_strategy() {
    // Regression for #93: text hits (no kind) came back for `--symbol-kind
    // struct`, and for #134: the filter ran after the FTS `LIMIT`, so a
    // small limit returned nothing although a matching struct exists.
    let mut files: Vec<(String, String)> = (0..20)
        .map(|i| (format!("a/m{i:02}.rs"), "fn config() {}\n".to_string()))
        .collect();
    files.push((
        "z_types.rs".to_string(),
        "pub struct Config<T: Clone + Send + Sync + 'static> {\n    inner: T,\n}\n".to_string(),
    ));
    files.push((
        "web/z.ts".to_string(),
        "export function config(options: Options): Config {\n  return options;\n}\n".to_string(),
    ));
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let (engine, _root) = index_project(&borrowed);
    let hits = |results: Vec<SearchResult>| -> Vec<(String, String, String)> {
        results
            .into_iter()
            .map(|r| {
                (
                    r.metadata.symbol_name.unwrap_or_default(),
                    r.metadata.symbol_kind.unwrap_or_default(),
                    r.file_path,
                )
            })
            .collect()
    };

    let structs = SearchOptions::new().with_symbol_kinds(vec!["struct".into()]);
    let results = engine.search("config", 5, Some(structs)).unwrap().results;
    assert_eq!(
        hits(results),
        vec![(
            "Config".to_string(),
            "struct".to_string(),
            "z_types.rs".to_string()
        )]
    );

    let helpers = SearchOptions::new().with_symbol_kinds(vec!["struct".into()]);
    let results = engine.search("inner", 5, Some(helpers)).unwrap().results;
    assert!(
        results.is_empty(),
        "no text hits under a kind filter: {:?}",
        hits(results)
    );

    let ts = SearchOptions::new().with_file_types(vec!["ts".into()]);
    let results = engine.search("config", 5, Some(ts)).unwrap().results;
    let found = hits(results);
    assert!(
        found.contains(&(
            "config".to_string(),
            "function".to_string(),
            "web/z.ts".to_string()
        )),
        "{found:?}"
    );
    assert!(found.iter().all(|(_, _, p)| p == "web/z.ts"), "{found:?}");
}

#[test]
fn test_imports_are_usages_not_definitions() {
    // Regression for #77: import statements (symbols of kind `import`) were
    // listed as definitions by `refs` and scored as symbol hits by `search`.
    let (engine, _root) = index_project(&[
        (
            "lib/secrets.ts",
            "export function useTypedSecrets() {\n  return 1;\n}\n",
        ),
        (
            "a.ts",
            "import { useTypedSecrets } from './lib/secrets';\nuseTypedSecrets();\n",
        ),
        ("b.ts", "import { useTypedSecrets } from './lib/secrets';\n"),
    ]);
    assert_eq!(
        engine
            .store
            .find_symbol_by_name("useTypedSecrets")
            .unwrap()
            .len(),
        3,
        "the import rows are still symbols (explain counts them)"
    );

    let results = engine.find_references("useTypedSecrets", 50).unwrap();
    let hits: Vec<(&str, usize, &str)> = results
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
    assert_eq!(
        hits,
        vec![
            ("lib/secrets.ts", 1, "definition"),
            ("a.ts", 1, "import"),
            ("a.ts", 2, "call"),
            ("b.ts", 1, "import"),
        ],
        "{hits:?}"
    );

    let results = engine.search("useTypedSecrets", 10, None).unwrap().results;
    let symbol_hits: Vec<(&str, &str)> = results
        .iter()
        .filter(|r| r.kind == SearchResultKind::Symbol)
        .map(|r| {
            (
                r.file_path.as_str(),
                r.metadata.symbol_kind.as_deref().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(symbol_hits, vec![("lib/secrets.ts", "function")]);

    // An explicit `import` kind filter opts back in.
    let imports = SearchOptions::new().with_symbol_kinds(vec!["import".into()]);
    let results = engine
        .search("useTypedSecrets", 10, Some(imports))
        .unwrap()
        .results;
    let mut files: Vec<&str> = results.iter().map(|r| r.file_path.as_str()).collect();
    files.sort();
    assert_eq!(files, vec!["a.ts", "b.ts"]);
}

#[test]
fn test_find_references_reads_each_file_once_and_bounds_definitions() {
    // Regression for #159: every definition of a common name was read from
    // disk before `limit` applied. The definitions are bounded in SQL.
    let source = (0..30)
        .map(|i| format!("impl T{i} {{\n    pub fn new() -> Self {{ Self }}\n}}\n"))
        .collect::<String>();
    let (engine, _root) = index_project(&[("many.rs", source.as_str())]);

    let results = engine.find_references("new", 5).unwrap().results;
    assert_eq!(results.len(), 5);
    assert!(
        results
            .iter()
            .all(|r| r.metadata.match_type.as_deref() == Some("definition")),
        "{results:?}"
    );
    assert_eq!(results[0].content, "    pub fn new() -> Self { Self }");
    assert_eq!(results[0].start_line, 2);
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
fn test_impact_mutual_recursion_reports_each_site_once() {
    // Regression for #46: the analysed symbol was never marked visited, so
    // `ping` <-> `pong` re-walked every reference to `ping` at depth 3 and reported
    // the depth-1 sites again.
    let (engine, _root) = index_rust_project(&[
        (
            "lib.rs",
            "pub fn ping(n: u32) { if n > 0 { pong(n - 1); } }\npub fn pong(n: u32) { if n > 0 { ping(n - 1); } }\n",
        ),
        ("main.rs", "fn main() { ping(3); }\n"),
    ]);

    let analysis = engine.analyze_impact("ping", None, 3, 100).unwrap();
    assert_eq!(
        impact_sites(&analysis),
        vec![
            (1, "lib.rs", 2, "same_file"),
            (1, "main.rs", 1, "unique_name"),
            (2, "lib.rs", 1, "same_file"),
        ],
        "{:?}",
        analysis.sites
    );
    assert!(!analysis.truncated);

    // With a cap of 3 the duplicates used to set `truncated`.
    let capped = engine.analyze_impact("ping", None, 3, 3).unwrap();
    assert_eq!(capped.sites.len(), 3);
    assert!(!capped.truncated);
}

#[test]
fn test_find_references_tolerates_a_missing_definition_file() {
    let (engine, root) = index_rust_project(&[
        ("lib.rs", "pub fn compute() -> u32 { 1 }\n"),
        ("main.rs", "fn main() { compute(); }\n"),
    ]);
    std::fs::remove_file(root.path().join("lib.rs")).unwrap();

    let results = engine.find_references("compute", 50).unwrap();
    let hits: Vec<_> = results
        .results
        .iter()
        .map(|r| (r.file_path.as_str(), r.content.as_str()))
        .collect();
    assert_eq!(
        hits,
        vec![("lib.rs", ""), ("main.rs", "fn main() { compute(); }")]
    );
}

#[test]
fn test_impact_depth_two_through_a_common_name_is_not_cut_by_homonyms() {
    // Regression for #42: with `max_sites = 2` the lookup of `mid` examined
    // 10 occurrences in path order, all homonyms in files sorting before
    // zz.rs, and the trusted depth-2 site in zz.rs was never reached.
    let mut files: Vec<(String, String)> = (0..12)
        .map(|i| {
            (
                format!("b{i:02}.rs"),
                "fn mid() {}\nfn x() {\n    mid();\n}\n".to_string(),
            )
        })
        .collect();
    files.push((
        "zz.rs".to_string(),
        "fn base() {}\nfn mid() {\n    base();\n}\nfn top() {\n    mid();\n}\n".to_string(),
    ));
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let (engine, _root) = index_rust_project(&files);

    let analysis = engine.analyze_impact("base", None, 2, 2).unwrap();
    assert_eq!(
        impact_sites(&analysis),
        vec![(1, "zz.rs", 3, "same_file"), (2, "zz.rs", 6, "same_file")]
    );
    // The occurrences of `mid` beyond the cap are name-only homonyms that
    // depth 2 would not report: not a truncation.
    assert!(!analysis.truncated);
}

#[test]
fn test_impact_flags_a_cut_of_trusted_occurrences_only() {
    // 12 same-file non-call references to `mid` after its one real caller:
    // the cap (10) cuts trusted rows, so the analysis is truncated even
    // though no reported site was lost.
    let refs = "    let f = mid;\n".repeat(12);
    let source = format!(
        "fn base() {{}}\nfn mid() {{\n    base();\n}}\nfn top() {{\n    mid();\n}}\nfn aliases() {{\n{refs}}}\n"
    );
    let (engine, _root) = index_rust_project(&[("zz.rs", source.as_str())]);

    let analysis = engine.analyze_impact("base", None, 2, 2).unwrap();
    assert_eq!(
        impact_sites(&analysis),
        vec![(1, "zz.rs", 3, "same_file"), (2, "zz.rs", 6, "same_file")]
    );
    assert!(analysis.truncated);
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
