//! Tests for the repo map.

use super::*;
use semantiq_parser::{Language, LanguageSupport, ReferenceExtractor, SymbolExtractor};

/// Index Rust sources into an in-memory store (symbols + AST references).
fn index_project(files: &[(String, String)]) -> Arc<IndexStore> {
    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let mut support = LanguageSupport::new().unwrap();
    for (path, source) in files {
        add_file(&store, &mut support, path, source);
    }
    store
}

fn add_file(store: &IndexStore, support: &mut LanguageSupport, path: &str, source: &str) {
    let file_id = store.insert_file(path, Some("rust"), source, 0).unwrap();
    let tree = support.parse(Language::Rust, source).unwrap();
    let symbols = SymbolExtractor::extract(&tree, source, Language::Rust).unwrap();
    store.insert_symbols(file_id, &symbols).unwrap();
    let refs = ReferenceExtractor::extract(&tree, source, Language::Rust);
    store.insert_references(file_id, &refs).unwrap();
}

const POPULAR: usize = 12;
const MODULES: usize = 30;

/// `src/core.rs` defines functions every module calls; `src/leaf.rs` alone
/// calls `src/helper.rs`; each module also has unreferenced functions.
fn sample_project() -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut core = String::new();
    for k in 0..POPULAR {
        core.push_str(&format!(
            "/// Popular helper number {k}, used by every module.\npub fn popular_{k}(x: u32) -> u32 {{ x + {k} }}\n\n"
        ));
    }
    files.push(("src/core.rs".to_string(), core));
    for i in 0..MODULES {
        let calls: Vec<String> = (0..POPULAR).map(|k| format!("popular_{k}(x)")).collect();
        let mut module = format!(
            "/// Worker of module {i}.\npub fn worker_{i}(x: u32) -> u32 {{ {} }}\n",
            calls.join(" + ")
        );
        for j in 0..5 {
            module.push_str(&format!(
                "/// Extra routine {j} of module {i}, not used anywhere else.\npub fn extra_{i}_{j}(value: u32) -> u32 {{ value }}\n"
            ));
        }
        files.push((format!("src/modules/m{i:02}.rs"), module));
    }
    files.push((
        "src/helper.rs".to_string(),
        "/// Helper only the leaf uses.\npub fn helper_routine(x: u32) -> u32 { x }\n".to_string(),
    ));
    files.push((
        "src/leaf.rs".to_string(),
        "pub fn leaf_entry() -> u32 { helper_routine(1) }\n".to_string(),
    ));
    files
}

fn options(max_tokens: usize) -> RepoMapOptions {
    RepoMapOptions {
        max_tokens,
        ..Default::default()
    }
}

fn listed(map: &RepoMap, path: &str) -> bool {
    map.files.iter().any(|f| f.path == path)
}

fn file_rank(map: &RepoMap, path: &str) -> f64 {
    map.files.iter().find(|f| f.path == path).unwrap().rank
}

#[test]
fn test_empty_index() {
    let store = IndexStore::open_in_memory().unwrap();
    let map = build_repo_map(&store, &options(1000)).unwrap();

    assert!(map.files.is_empty());
    assert_eq!(map.total_files, 0);
    assert_eq!(map.shown_symbols, 0);
    assert!(map.text.contains("no source files"));
    assert!(map.estimated_tokens <= map.max_tokens);
}

#[test]
fn test_respects_token_budget() {
    let store = index_project(&sample_project());
    let full = build_repo_map(&store, &options(MAX_REPO_MAP_TOKENS)).unwrap();
    assert!(
        full.estimated_tokens > 2000,
        "fixture too small to test budgets"
    );

    for budget in [MIN_REPO_MAP_TOKENS, 600, 1000, 1500] {
        let map = build_repo_map(&store, &options(budget)).unwrap();
        assert_eq!(map.estimated_tokens, estimate_tokens(&map.text));
        assert!(
            map.estimated_tokens <= budget,
            "{} tokens over a {} budget",
            map.estimated_tokens,
            budget
        );
        assert!(
            map.estimated_tokens as f64 >= budget as f64 * 0.9,
            "{} tokens is under 90% of a {} budget",
            map.estimated_tokens,
            budget
        );
        assert!(map.shown_symbols < map.total_symbols);
    }
}

#[test]
fn test_budget_is_clamped() {
    let store = index_project(&sample_project());
    assert_eq!(
        build_repo_map(&store, &options(10)).unwrap().max_tokens,
        MIN_REPO_MAP_TOKENS
    );
    assert_eq!(
        build_repo_map(&store, &options(1_000_000))
            .unwrap()
            .max_tokens,
        MAX_REPO_MAP_TOKENS
    );
}

#[test]
fn test_deterministic() {
    let files = sample_project();
    let first = build_repo_map(&index_project(&files), &options(800)).unwrap();
    // A fresh store indexed in reverse order assigns different ids.
    let mut reversed = files.clone();
    reversed.reverse();
    let second = build_repo_map(&index_project(&reversed), &options(800)).unwrap();
    let again = build_repo_map(&index_project(&files), &options(800)).unwrap();

    assert_eq!(first.text, second.text);
    assert_eq!(first.files, second.files);
    assert_eq!(first.text, again.text);
}

#[test]
fn test_most_used_code_comes_first() {
    let store = index_project(&sample_project());
    let map = build_repo_map(&store, &options(MIN_REPO_MAP_TOKENS)).unwrap();

    assert!(listed(&map, "src/core.rs"), "{}", map.text);
    assert!(map.text.contains("pub fn popular_0(x: u32) -> u32"));
    assert!(map.text.contains("Popular helper number 0"));
    // Unreferenced routines do not make the cut.
    assert!(!map.text.contains("extra_"), "{}", map.text);
}

#[test]
fn test_focus_brings_related_files_up() {
    let store = index_project(&sample_project());
    let plain = build_repo_map(&store, &options(MIN_REPO_MAP_TOKENS)).unwrap();
    assert!(!listed(&plain, "src/helper.rs"), "{}", plain.text);

    let focused = build_repo_map(
        &store,
        &RepoMapOptions {
            max_tokens: MIN_REPO_MAP_TOKENS,
            focus: vec!["src/leaf.rs".to_string()],
            path_prefix: None,
        },
    )
    .unwrap();
    assert_eq!(focused.focus_files, vec!["src/leaf.rs"]);
    assert!(listed(&focused, "src/leaf.rs"), "{}", focused.text);
    assert!(listed(&focused, "src/helper.rs"), "{}", focused.text);
    assert!(focused.text.contains("(focus: src/leaf.rs)"));

    // With everything listed, the focus reorders the file ranks.
    let all = |focus: Vec<String>| {
        build_repo_map(
            &store,
            &RepoMapOptions {
                max_tokens: MAX_REPO_MAP_TOKENS,
                focus,
                path_prefix: None,
            },
        )
        .unwrap()
    };
    let plain = all(Vec::new());
    let focused = all(vec!["leaf.rs".to_string()]);
    assert!(file_rank(&plain, "src/core.rs") > file_rank(&plain, "src/helper.rs"));
    assert!(file_rank(&focused, "src/helper.rs") > file_rank(&focused, "src/core.rs"));
}

#[test]
fn test_focus_on_symbol_and_unmatched_entries() {
    let store = index_project(&sample_project());
    let map = build_repo_map(
        &store,
        &RepoMapOptions {
            max_tokens: MIN_REPO_MAP_TOKENS,
            focus: vec!["helper_routine".to_string(), "no_such_thing".to_string()],
            path_prefix: None,
        },
    )
    .unwrap();

    assert_eq!(map.focus_symbols, vec!["helper_routine"]);
    assert_eq!(map.unmatched_focus, vec!["no_such_thing"]);
    assert!(listed(&map, "src/helper.rs"), "{}", map.text);
}

#[test]
fn test_focus_on_directory() {
    let store = index_project(&sample_project());
    let map = build_repo_map(
        &store,
        &RepoMapOptions {
            max_tokens: MIN_REPO_MAP_TOKENS,
            focus: vec!["./src/modules/".to_string()],
            path_prefix: None,
        },
    )
    .unwrap();
    assert_eq!(map.focus_files.len(), MODULES);
}

#[test]
fn test_path_prefix_limits_listed_files() {
    let store = index_project(&sample_project());
    let map = build_repo_map(
        &store,
        &RepoMapOptions {
            max_tokens: 600,
            focus: Vec::new(),
            path_prefix: Some("src/modules".to_string()),
        },
    )
    .unwrap();

    assert_eq!(map.total_files, MODULES);
    assert!(!map.files.is_empty());
    assert!(map.files.iter().all(|f| f.path.starts_with("src/modules/")));
    // Each module's worker is referenced by nobody: ranked by file importance.
    assert!(map.text.contains("(under src/modules)"));
}

#[test]
fn test_directory_grouping() {
    let store = index_project(&sample_project());
    let map = build_repo_map(&store, &options(MAX_REPO_MAP_TOKENS)).unwrap();

    // Each directory header appears once, followed by its files.
    assert_eq!(map.text.matches("\nsrc/\n").count(), 1, "{}", map.text);
    assert_eq!(map.text.matches("\nsrc/modules/\n").count(), 1);
    assert!(map.text.contains("\n  core.rs\n    pub fn popular_0"));
}

#[test]
fn test_skips_data_files_and_imports() {
    let store = index_project(&[(
        "src/lib.rs".to_string(),
        "use std::fmt;\npub struct Widget;\n".to_string(),
    )]);
    store
        .insert_file("Cargo.toml", Some("toml"), "[package]", 0)
        .unwrap();

    let map = build_repo_map(&store, &options(1000)).unwrap();
    assert_eq!(map.total_files, 1);
    assert!(map.text.contains("pub struct Widget"));
    assert!(!map.text.contains("use std::fmt"));
    assert!(!map.text.contains("Cargo.toml"));
}

#[test]
fn test_engine_cache_is_invalidated_by_reindex() {
    let store = index_project(&sample_project());
    let engine = RetrievalEngine::with_options(Arc::clone(&store), "/nonexistent", false);

    let first = engine.repo_map(&options(MIN_REPO_MAP_TOKENS)).unwrap();
    let cached = engine.repo_map(&options(MIN_REPO_MAP_TOKENS)).unwrap();
    assert_eq!(first.text, cached.text);
    assert_eq!(
        first.text,
        build_repo_map(&store, &options(MIN_REPO_MAP_TOKENS))
            .unwrap()
            .text
    );

    // Many new callers make helper_routine the most used function.
    let mut support = LanguageSupport::new().unwrap();
    for i in 0..40 {
        add_file(
            &store,
            &mut support,
            &format!("src/users/u{i:02}.rs"),
            "pub fn use_helper() -> u32 { helper_routine(2) }\n",
        );
    }
    let updated = engine.repo_map(&options(MIN_REPO_MAP_TOKENS)).unwrap();
    assert!(listed(&updated, "src/helper.rs"), "{}", updated.text);
}

#[test]
fn test_clean_signature_and_doc() {
    assert_eq!(
        clean_signature(
            Some("pub fn open(path: &Path) -> Result<Self> {"),
            "method",
            "open"
        ),
        "pub fn open(path: &Path) -> Result<Self>"
    );
    assert_eq!(
        clean_signature(Some("pub fn one() -> u32 { 1 }"), "function", "one"),
        "pub fn one() -> u32"
    );
    assert_eq!(
        clean_signature(Some("pub struct Unit;"), "struct", "Unit"),
        "pub struct Unit"
    );
    assert_eq!(clean_signature(None, "struct", "Foo"), "struct Foo");
    assert_eq!(clean_signature(Some("  \n"), "fn", "f"), "fn f");
    let long = format!("fn f({})", "a: u32, ".repeat(40));
    assert_eq!(
        clean_signature(Some(&long), "function", "f")
            .chars()
            .count(),
        MAX_SIGNATURE_CHARS
    );

    assert_eq!(
        clean_doc(Some("/// Opens the store.\n/// More.")),
        Some("Opens the store.".to_string())
    );
    assert_eq!(
        clean_doc(Some("/**\n * Javadoc style.\n */")),
        Some("Javadoc style.".to_string())
    );
    assert_eq!(
        clean_doc(Some("\"\"\"Python docstring.\"\"\"")),
        Some("Python docstring.".to_string())
    );
    assert_eq!(clean_doc(Some("///\n")), None);
    assert_eq!(clean_doc(None), None);
}

#[test]
fn test_pagerank_sums_to_one_and_follows_edges() {
    let edges = vec![(0, 2, 1.0), (1, 2, 1.0), (2, 0, 0.5)];
    let rank = pagerank(3, &edges, &normalized(vec![1.0; 3]));
    assert!((rank.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    assert!(rank[2] > rank[0] && rank[2] > rank[1]);
}
