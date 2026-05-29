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
