//! #135: the distance collector counts what `distance_observations` stores
//! (one row per query and language), bootstrap ends on stored rows, and
//! auto-calibration only saves thresholds search will apply.

use semantiq_index::IndexStore;
use semantiq_retrieval::{APPLIED_MIN_SAMPLES, RetrievalEngine};
use std::sync::Arc;

fn engine() -> (RetrievalEngine, Arc<IndexStore>) {
    let store = Arc::new(IndexStore::open_in_memory().unwrap());
    let engine = RetrievalEngine::new(Arc::clone(&store), "/nonexistent");
    (engine, store)
}

/// Twenty KNN candidates per search, as `search_semantic` records them.
fn candidates() -> Vec<(i64, f32)> {
    (1..=20).map(|i| (i, 0.6 + i as f32 * 0.02)).collect()
}

fn synthetic_rows(language: &str, from: u64, count: usize) -> Vec<(String, f32, u64, i64)> {
    (0..count as u64)
        .map(|i| {
            (
                language.to_string(),
                0.5 + ((from + i) % 50) as f32 * 0.01,
                from + i,
                1_700_000_000 + i as i64,
            )
        })
        .collect()
}

/// 25 searches × 20 candidates used to count 500 observations, end the
/// bootstrap and trigger a one-shot calibration over 25 stored rows.
#[test]
fn collector_counts_one_observation_per_query_and_language() {
    let (engine, store) = engine();
    let collector = engine.distance_collector().unwrap();
    assert!(collector.is_bootstrap());

    for i in 0..25 {
        collector.record(&format!("query {i}"), &candidates(), |_| {
            Some("rust".to_string())
        });
    }
    assert_eq!(collector.total_observations(), 25);
    assert!(collector.is_bootstrap());
    assert_eq!(engine.flush_observations().unwrap(), 25);
    assert_eq!(store.get_observation_counts().unwrap()["rust"], 25);
    assert_eq!(collector.total_observations(), 25);

    // The same queries again: the table keeps one row per query, and so
    // does the count.
    for i in 0..25 {
        collector.record(&format!("query {i}"), &candidates(), |_| {
            Some("rust".to_string())
        });
    }
    assert_eq!(engine.flush_observations().unwrap(), 0, "all duplicates");
    assert_eq!(collector.total_observations(), 25);
    assert!(collector.is_bootstrap());
    assert!(!collector.should_calibrate());
}

/// A calibration below `APPLIED_MIN_SAMPLES` is not saved (search would not
/// use it); past the floor it is saved and applied.
#[test]
fn auto_calibrate_saves_only_what_search_applies() {
    let (engine, store) = engine();
    let defaults = engine.threshold_config().read().unwrap().get(Some("rust"));

    store
        .insert_distance_observations_batch(&synthetic_rows("rust", 0, 150))
        .unwrap();
    assert!(!engine.auto_calibrate().unwrap());
    assert!(store.load_all_calibrations().unwrap().is_empty());
    assert_eq!(
        engine.threshold_config().read().unwrap().get(Some("rust")),
        defaults
    );

    store
        .insert_distance_observations_batch(&synthetic_rows("rust", 150, APPLIED_MIN_SAMPLES - 150))
        .unwrap();
    assert!(engine.auto_calibrate().unwrap());
    let saved = store.load_all_calibrations().unwrap();
    let rust = saved.iter().find(|c| c.language == "rust").unwrap();
    assert_eq!(rust.confidence, "medium");
    assert_eq!(rust.sample_count, APPLIED_MIN_SAMPLES);
    let applied = engine.threshold_config().read().unwrap().get(Some("rust"));
    assert_ne!(applied, defaults);
    assert!((applied.0 - rust.max_distance).abs() < 1e-6);
}
