//! Tests for IndexStore.

use super::*;
use semantiq_parser::{CodeChunk, Symbol, SymbolKind};

#[test]
fn test_insert_and_get_file() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
        .unwrap();

    let file = store.get_file_by_path("test.rs").unwrap().unwrap();
    assert_eq!(file.id, file_id);
    assert_eq!(file.path, "test.rs");
    assert_eq!(file.language, Some("rust".to_string()));
}

#[test]
fn test_insert_and_search_symbols() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("test.rs", Some("rust"), "fn hello() {}", 13, 1000)
        .unwrap();

    let symbols = vec![Symbol {
        name: "hello".to_string(),
        kind: SymbolKind::Function,
        start_line: 1,
        end_line: 1,
        start_byte: 0,
        end_byte: 13,
        signature: Some("fn hello()".to_string()),
        doc_comment: None,
        parent: None,
    }];

    store.insert_symbols(file_id, &symbols).unwrap();

    let results = store.find_symbol_by_name("hello").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "hello");
}

#[test]
fn test_needs_full_reindex_no_version() {
    let store = IndexStore::open_in_memory().unwrap();
    assert!(store.needs_full_reindex().unwrap());
}

#[test]
fn test_needs_full_reindex_same_version() {
    let store = IndexStore::open_in_memory().unwrap();
    store.set_parser_version().unwrap();
    assert!(!store.needs_full_reindex().unwrap());
}

#[test]
fn test_needs_full_reindex_different_version() {
    let store = IndexStore::open_in_memory().unwrap();
    store
        .with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES ('parser_version', '999')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(store.needs_full_reindex().unwrap());
}

#[test]
fn test_needs_full_reindex_corrupted_version() {
    let store = IndexStore::open_in_memory().unwrap();
    store
        .with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES ('parser_version', 'not_a_number')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(store.needs_full_reindex().unwrap());
}

#[test]
fn test_clear_all_data() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
        .unwrap();

    let symbols = vec![Symbol {
        name: "main".to_string(),
        kind: SymbolKind::Function,
        start_line: 1,
        end_line: 1,
        start_byte: 0,
        end_byte: 12,
        signature: None,
        doc_comment: None,
        parent: None,
    }];
    store.insert_symbols(file_id, &symbols).unwrap();

    let stats = store.get_stats().unwrap();
    assert_eq!(stats.file_count, 1);
    assert_eq!(stats.symbol_count, 1);

    store.clear_all_data().unwrap();

    let stats = store.get_stats().unwrap();
    assert_eq!(stats.file_count, 0);
    assert_eq!(stats.symbol_count, 0);
}

#[test]
fn test_check_and_prepare_for_reindex() {
    let store = IndexStore::open_in_memory().unwrap();

    store
        .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
        .unwrap();

    let needs_reindex = store.check_and_prepare_for_reindex().unwrap();
    assert!(needs_reindex);

    let stats = store.get_stats().unwrap();
    assert_eq!(stats.file_count, 0);

    let needs_reindex = store.check_and_prepare_for_reindex().unwrap();
    assert!(!needs_reindex);
}

#[test]
fn test_insert_and_get_chunks() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file(
            "test.rs",
            Some("rust"),
            "fn main() {}\nfn foo() {}",
            25,
            1000,
        )
        .unwrap();

    let chunks = vec![
        CodeChunk {
            content: "fn main() {}".to_string(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 12,
            symbols: vec!["main".to_string()],
        },
        CodeChunk {
            content: "fn foo() {}".to_string(),
            start_line: 2,
            end_line: 2,
            start_byte: 13,
            end_byte: 24,
            symbols: vec!["foo".to_string()],
        },
    ];

    store.insert_chunks(file_id, &chunks).unwrap();

    let retrieved = store.get_chunks_by_file(file_id).unwrap();
    assert_eq!(retrieved.len(), 2);
    assert_eq!(retrieved[0].content, "fn main() {}");
    assert_eq!(retrieved[1].content, "fn foo() {}");
}

#[test]
fn test_chunks_without_embeddings() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
        .unwrap();

    let chunks = vec![CodeChunk {
        content: "fn main() {}".to_string(),
        start_line: 1,
        end_line: 1,
        start_byte: 0,
        end_byte: 12,
        symbols: vec!["main".to_string()],
    }];

    store.insert_chunks(file_id, &chunks).unwrap();

    let without_embeddings = store.get_chunks_without_embeddings(10).unwrap();
    assert_eq!(without_embeddings.len(), 1);
}

#[test]
fn test_update_chunk_embedding() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
        .unwrap();

    let chunks = vec![CodeChunk {
        content: "fn main() {}".to_string(),
        start_line: 1,
        end_line: 1,
        start_byte: 0,
        end_byte: 12,
        symbols: vec!["main".to_string()],
    }];

    store.insert_chunks(file_id, &chunks).unwrap();

    let chunks = store.get_chunks_by_file(file_id).unwrap();
    let chunk_id = chunks[0].id;

    let embedding: Vec<f32> = (0..384).map(|i| i as f32 * 0.001).collect();
    store.update_chunk_embedding(chunk_id, &embedding).unwrap();

    let without_embeddings = store.get_chunks_without_embeddings(10).unwrap();
    assert!(without_embeddings.is_empty());
}

#[test]
fn test_vector_search() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("src/main.rs", Some("rust"), "fn main() {}", 12, 1000)
        .unwrap();

    let chunks = vec![
        CodeChunk {
            content: "fn hello() {}".to_string(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 13,
            symbols: vec!["hello".to_string()],
        },
        CodeChunk {
            content: "fn world() {}".to_string(),
            start_line: 2,
            end_line: 2,
            start_byte: 14,
            end_byte: 27,
            symbols: vec!["world".to_string()],
        },
        CodeChunk {
            content: "fn foo() {}".to_string(),
            start_line: 3,
            end_line: 3,
            start_byte: 28,
            end_byte: 39,
            symbols: vec!["foo".to_string()],
        },
    ];

    store.insert_chunks(file_id, &chunks).unwrap();
    let stored_chunks = store.get_chunks_by_file(file_id).unwrap();

    let embedding1: Vec<f32> = (0..384).map(|i| i as f32 * 0.001).collect();
    let embedding2: Vec<f32> = (0..384).map(|i| i as f32 * 0.002).collect();
    let embedding3: Vec<f32> = (0..384).map(|i| i as f32 * 0.003).collect();

    store
        .update_chunk_embedding(stored_chunks[0].id, &embedding1)
        .unwrap();
    store
        .update_chunk_embedding(stored_chunks[1].id, &embedding2)
        .unwrap();
    store
        .update_chunk_embedding(stored_chunks[2].id, &embedding3)
        .unwrap();

    let query: Vec<f32> = (0..384).map(|i| i as f32 * 0.0011).collect();
    let results = store.search_similar_chunks(&query, 2).unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, stored_chunks[0].id);

    let chunk_ids: Vec<i64> = results.iter().map(|(id, _)| *id).collect();
    let found_chunks = store.get_chunks_by_ids(&chunk_ids).unwrap();
    assert_eq!(found_chunks.len(), 2);
}

#[test]
fn test_insert_and_get_dependencies() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("src/main.rs", Some("rust"), "use crate::utils;", 17, 1000)
        .unwrap();

    store
        .insert_dependency(file_id, "crate::utils", Some("utils"), "local")
        .unwrap();
    store
        .insert_dependency(file_id, "std::io", Some("io"), "std")
        .unwrap();

    let deps = store.get_dependencies(file_id).unwrap();
    assert_eq!(deps.len(), 2);
    assert!(deps.iter().any(|d| d.target_path == "crate::utils"));
    assert!(deps.iter().any(|d| d.target_path == "std::io"));
}

#[test]
fn test_get_dependents() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("src/main.rs", Some("rust"), "use crate::utils;", 17, 1000)
        .unwrap();

    store
        .insert_dependency(file_id, "src/utils.rs", Some("utils"), "local")
        .unwrap();

    let dependents = store.get_dependents("utils.rs").unwrap();
    assert_eq!(dependents.len(), 1);
    assert_eq!(dependents[0].source_file_id, file_id);
}

#[test]
fn test_get_dependents_deduplicates() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_a = store
        .insert_file("src/app.rs", Some("rust"), "use crate::lib;", 15, 1000)
        .unwrap();

    // Insert a dependency that would match multiple LIKE patterns
    store
        .insert_dependency(file_a, "./lib", Some("lib"), "local")
        .unwrap();

    // Should return exactly one result even though "./lib" matches
    // multiple LIKE patterns (e.g., "%/lib" and "./lib")
    let dependents = store.get_dependents("src/lib.rs").unwrap();
    assert!(
        dependents.len() <= 1,
        "Expected at most 1 dependent, got {}",
        dependents.len()
    );
}

#[test]
fn test_get_dependents_multiple_importers() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_a = store
        .insert_file("src/a.rs", Some("rust"), "use crate::shared;", 18, 1000)
        .unwrap();
    let file_b = store
        .insert_file("src/b.rs", Some("rust"), "use crate::shared;", 18, 1000)
        .unwrap();

    store
        .insert_dependency(file_a, "crate::shared", Some("shared"), "local")
        .unwrap();
    store
        .insert_dependency(file_b, "./shared", Some("shared"), "local")
        .unwrap();

    let dependents = store.get_dependents("src/shared.rs").unwrap();
    assert_eq!(
        dependents.len(),
        2,
        "Expected 2 dependents, got {}",
        dependents.len()
    );
}

#[test]
fn test_get_dependents_no_false_positives() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file(
            "src/main.rs",
            Some("rust"),
            "use crate::something;",
            21,
            1000,
        )
        .unwrap();

    store
        .insert_dependency(
            file_id,
            "crate::something_else",
            Some("something_else"),
            "local",
        )
        .unwrap();

    // "utils.rs" should not match "something_else"
    let dependents = store.get_dependents("utils.rs").unwrap();
    assert_eq!(dependents.len(), 0);
}

#[test]
fn test_delete_dependencies() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("src/main.rs", Some("rust"), "use crate::utils;", 17, 1000)
        .unwrap();

    store
        .insert_dependency(file_id, "crate::utils", Some("utils"), "local")
        .unwrap();

    let deps = store.get_dependencies(file_id).unwrap();
    assert_eq!(deps.len(), 1);

    store.delete_dependencies(file_id).unwrap();

    let deps = store.get_dependencies(file_id).unwrap();
    assert_eq!(deps.len(), 0);
}

#[test]
fn test_delete_file() {
    let store = IndexStore::open_in_memory().unwrap();

    store
        .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
        .unwrap();

    assert!(store.get_file_by_path("test.rs").unwrap().is_some());

    store.delete_file("test.rs").unwrap();

    assert!(store.get_file_by_path("test.rs").unwrap().is_none());
}

#[test]
fn test_needs_reindex_same_content() {
    let store = IndexStore::open_in_memory().unwrap();
    let content = "fn main() {}";

    store
        .insert_file("test.rs", Some("rust"), content, 12, 1000)
        .unwrap();

    assert!(!store.needs_reindex("test.rs", content).unwrap());
}

#[test]
fn test_needs_reindex_different_content() {
    let store = IndexStore::open_in_memory().unwrap();

    store
        .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
        .unwrap();

    assert!(
        store
            .needs_reindex("test.rs", "fn main() { println!(\"hello\"); }")
            .unwrap()
    );
}

#[test]
fn test_needs_reindex_new_file() {
    let store = IndexStore::open_in_memory().unwrap();
    assert!(store.needs_reindex("new_file.rs", "content").unwrap());
}

#[test]
fn test_get_symbols_by_file() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file(
            "test.rs",
            Some("rust"),
            "fn hello() {}\nfn world() {}",
            27,
            1000,
        )
        .unwrap();

    let symbols = vec![
        Symbol {
            name: "hello".to_string(),
            kind: SymbolKind::Function,
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 13,
            signature: Some("fn hello()".to_string()),
            doc_comment: None,
            parent: None,
        },
        Symbol {
            name: "world".to_string(),
            kind: SymbolKind::Function,
            start_line: 2,
            end_line: 2,
            start_byte: 14,
            end_byte: 27,
            signature: Some("fn world()".to_string()),
            doc_comment: None,
            parent: None,
        },
    ];

    store.insert_symbols(file_id, &symbols).unwrap();

    let retrieved = store.get_symbols_by_file(file_id).unwrap();
    assert_eq!(retrieved.len(), 2);
    assert_eq!(retrieved[0].name, "hello");
    assert_eq!(retrieved[1].name, "world");
}

#[test]
fn test_search_symbols_fts() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("test.rs", Some("rust"), "fn calculate_total() {}", 23, 1000)
        .unwrap();

    let symbols = vec![Symbol {
        name: "calculate_total".to_string(),
        kind: SymbolKind::Function,
        start_line: 1,
        end_line: 1,
        start_byte: 0,
        end_byte: 23,
        signature: Some("fn calculate_total()".to_string()),
        doc_comment: None,
        parent: None,
    }];

    store.insert_symbols(file_id, &symbols).unwrap();

    let results = store.search_symbols("calculate", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "calculate_total");
}

#[test]
fn test_get_stats() {
    let store = IndexStore::open_in_memory().unwrap();

    let stats = store.get_stats().unwrap();
    assert_eq!(stats.file_count, 0);
    assert_eq!(stats.symbol_count, 0);
    assert_eq!(stats.chunk_count, 0);
    assert_eq!(stats.dependency_count, 0);

    let file_id = store
        .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
        .unwrap();

    let symbols = vec![Symbol {
        name: "main".to_string(),
        kind: SymbolKind::Function,
        start_line: 1,
        end_line: 1,
        start_byte: 0,
        end_byte: 12,
        signature: None,
        doc_comment: None,
        parent: None,
    }];
    store.insert_symbols(file_id, &symbols).unwrap();

    let stats = store.get_stats().unwrap();
    assert_eq!(stats.file_count, 1);
    assert_eq!(stats.symbol_count, 1);
}

#[test]
fn test_db_path() {
    let store = IndexStore::open_in_memory().unwrap();
    assert_eq!(store.db_path().to_string_lossy(), ":memory:");
}

// Distance observations and calibration tests

#[test]
fn test_insert_distance_observation() {
    let store = IndexStore::open_in_memory().unwrap();

    let inserted = store
        .insert_distance_observation("rust", 0.5, 12345, 1000000)
        .unwrap();
    assert!(inserted);

    // Duplicate should be ignored (UNIQUE constraint)
    let inserted = store
        .insert_distance_observation("rust", 0.6, 12345, 1000001)
        .unwrap();
    assert!(!inserted);

    // Same query hash, different language should work
    let inserted = store
        .insert_distance_observation("python", 0.7, 12345, 1000002)
        .unwrap();
    assert!(inserted);
}

#[test]
fn test_insert_distance_observations_batch() {
    let store = IndexStore::open_in_memory().unwrap();

    let observations = vec![
        ("rust".to_string(), 0.5, 1, 1000000),
        ("rust".to_string(), 0.6, 2, 1000001),
        ("python".to_string(), 0.7, 3, 1000002),
    ];

    let inserted = store
        .insert_distance_observations_batch(&observations)
        .unwrap();
    assert_eq!(inserted, 3);
}

#[test]
fn test_get_distance_observations() {
    let store = IndexStore::open_in_memory().unwrap();

    store
        .insert_distance_observation("rust", 0.5, 1, 1000000)
        .unwrap();
    store
        .insert_distance_observation("rust", 0.6, 2, 1000001)
        .unwrap();
    store
        .insert_distance_observation("python", 0.7, 3, 1000002)
        .unwrap();

    let rust_obs = store.get_distance_observations("rust").unwrap();
    assert_eq!(rust_obs.len(), 2);
    assert!(rust_obs.contains(&0.5));
    assert!(rust_obs.contains(&0.6));

    let python_obs = store.get_distance_observations("python").unwrap();
    assert_eq!(python_obs.len(), 1);
}

#[test]
fn test_get_all_distance_observations() {
    let store = IndexStore::open_in_memory().unwrap();

    store
        .insert_distance_observation("rust", 0.5, 1, 1000000)
        .unwrap();
    store
        .insert_distance_observation("rust", 0.6, 2, 1000001)
        .unwrap();
    store
        .insert_distance_observation("python", 0.7, 3, 1000002)
        .unwrap();

    let all_obs = store.get_all_distance_observations().unwrap();
    assert_eq!(all_obs.len(), 2);
    assert_eq!(all_obs.get("rust").map(|v| v.len()), Some(2));
    assert_eq!(all_obs.get("python").map(|v| v.len()), Some(1));
}

#[test]
fn test_get_observation_counts() {
    let store = IndexStore::open_in_memory().unwrap();

    store
        .insert_distance_observation("rust", 0.5, 1, 1000000)
        .unwrap();
    store
        .insert_distance_observation("rust", 0.6, 2, 1000001)
        .unwrap();
    store
        .insert_distance_observation("python", 0.7, 3, 1000002)
        .unwrap();

    let counts = store.get_observation_counts().unwrap();
    assert_eq!(counts.get("rust"), Some(&2));
    assert_eq!(counts.get("python"), Some(&1));
}

#[test]
fn test_cleanup_old_observations() {
    let store = IndexStore::open_in_memory().unwrap();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    store
        .insert_distance_observation("rust", 0.5, 1, now - 100000)
        .unwrap();
    store
        .insert_distance_observation("rust", 0.6, 2, now - 10)
        .unwrap();

    let deleted = store.cleanup_old_observations(86400).unwrap();
    assert_eq!(deleted, 1);

    let remaining = store.get_distance_observations("rust").unwrap();
    assert_eq!(remaining.len(), 1);
    assert!((remaining[0] - 0.6).abs() < 0.001);
}

#[test]
fn test_save_and_load_calibration() {
    let store = IndexStore::open_in_memory().unwrap();

    store
        .save_calibration(&CalibrationData {
            language: "rust".to_string(),
            max_distance: 1.0,
            min_similarity: 0.4,
            confidence: "medium".to_string(),
            sample_count: 1000,
            p50_distance: Some(0.8),
            p90_distance: Some(1.1),
            p95_distance: Some(1.2),
            mean_distance: Some(0.85),
            std_distance: Some(0.15),
        })
        .unwrap();

    let calibration = store.load_calibration("rust").unwrap().unwrap();
    assert_eq!(calibration.language, "rust");
    assert!((calibration.max_distance - 1.0).abs() < 0.001);
    assert!((calibration.min_similarity - 0.4).abs() < 0.001);
    assert_eq!(calibration.confidence, "medium");
    assert_eq!(calibration.sample_count, 1000);
    assert!(calibration.p50_distance.is_some());
}

#[test]
fn test_load_all_calibrations() {
    let store = IndexStore::open_in_memory().unwrap();

    store
        .save_calibration(&CalibrationData {
            language: "rust".to_string(),
            max_distance: 1.0,
            min_similarity: 0.4,
            confidence: "medium".to_string(),
            sample_count: 1000,
            p50_distance: None,
            p90_distance: None,
            p95_distance: None,
            mean_distance: None,
            std_distance: None,
        })
        .unwrap();
    store
        .save_calibration(&CalibrationData {
            language: "python".to_string(),
            max_distance: 1.1,
            min_similarity: 0.35,
            confidence: "high".to_string(),
            sample_count: 5000,
            p50_distance: None,
            p90_distance: None,
            p95_distance: None,
            mean_distance: None,
            std_distance: None,
        })
        .unwrap();

    let calibrations = store.load_all_calibrations().unwrap();
    assert_eq!(calibrations.len(), 2);
}

#[test]
fn test_clear_calibrations() {
    let store = IndexStore::open_in_memory().unwrap();

    store
        .save_calibration(&CalibrationData {
            language: "rust".to_string(),
            max_distance: 1.0,
            min_similarity: 0.4,
            confidence: "medium".to_string(),
            sample_count: 1000,
            p50_distance: None,
            p90_distance: None,
            p95_distance: None,
            mean_distance: None,
            std_distance: None,
        })
        .unwrap();

    let before = store.load_all_calibrations().unwrap();
    assert_eq!(before.len(), 1);

    store.clear_calibrations().unwrap();

    let after = store.load_all_calibrations().unwrap();
    assert_eq!(after.len(), 0);
}

#[test]
fn test_get_file_language() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("test.rs", Some("rust"), "fn main() {}", 12, 1000)
        .unwrap();

    let language = store.get_file_language(file_id).unwrap();
    assert_eq!(language, Some("rust".to_string()));

    let no_lang = store.get_file_language(999).unwrap();
    assert!(no_lang.is_none());
}

#[test]
fn test_get_chunk_language() {
    let store = IndexStore::open_in_memory().unwrap();

    let file_id = store
        .insert_file("test.py", Some("python"), "def main(): pass", 16, 1000)
        .unwrap();

    let chunks = vec![CodeChunk {
        content: "def main(): pass".to_string(),
        start_line: 1,
        end_line: 1,
        start_byte: 0,
        end_byte: 16,
        symbols: vec!["main".to_string()],
    }];

    store.insert_chunks(file_id, &chunks).unwrap();

    let stored_chunks = store.get_chunks_by_file(file_id).unwrap();
    let chunk_id = stored_chunks[0].id;

    let language = store.get_chunk_language(chunk_id).unwrap();
    assert_eq!(language, Some("python".to_string()));
}
