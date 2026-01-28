//! Tests for RetrievalEngine.

use super::*;

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
