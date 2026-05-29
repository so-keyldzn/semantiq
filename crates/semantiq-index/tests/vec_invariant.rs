//! Integration tests guarding the `chunks` ↔ `chunks_vec` integrity invariant.
//!
//! `chunks_vec` is a sqlite-vec virtual table and does not honor FK ON DELETE
//! CASCADE. Every code path that removes rows from `chunks` must purge the
//! matching `chunks_vec` rows in the same transaction. These tests pin that
//! contract: any regression that lets orphan vectors accumulate must trip here.

use semantiq_index::IndexStore;
use semantiq_parser::CodeChunk;

fn make_chunk(content: &str, start_line: usize, end_line: usize) -> CodeChunk {
    CodeChunk {
        content: content.to_string(),
        start_line,
        end_line,
        start_byte: 0,
        end_byte: content.len(),
        symbols: Vec::new(),
    }
}

fn make_embedding(seed: f32) -> Vec<f32> {
    // Distinct but valid 384-d vectors; values matter little for the invariant.
    (0..384).map(|i| seed + (i as f32) * 0.0001).collect()
}

fn assert_no_orphans(store: &IndexStore, ctx: &str) {
    let n = store.count_orphan_chunk_vectors().expect("count orphans");
    assert_eq!(n, 0, "{ctx}: found {n} orphan rows in chunks_vec");
}

#[test]
fn fresh_store_has_no_orphans() {
    let store = IndexStore::open_in_memory().unwrap();
    assert_no_orphans(&store, "fresh store");
}

#[test]
fn reindex_same_file_does_not_leak_vectors() {
    let store = IndexStore::open_in_memory().unwrap();
    let file_id = store
        .insert_file("foo.rs", Some("rust"), "fn old() {}", 11, 1000)
        .unwrap();

    // First indexing pass: 3 chunks, each with an embedding.
    let v1 = vec![
        make_chunk("fn a() {}", 1, 1),
        make_chunk("fn b() {}", 2, 2),
        make_chunk("fn c() {}", 3, 3),
    ];
    store.insert_chunks(file_id, &v1).unwrap();
    let stored = store.get_chunks_by_file(file_id).unwrap();
    assert_eq!(stored.len(), 3);
    for (i, c) in stored.iter().enumerate() {
        store
            .update_chunk_embedding(c.id, &make_embedding(i as f32))
            .unwrap();
    }

    // Reindex: brand-new chunks for the same file. Old `chunks` rows are wiped
    // by `insert_chunks`; the corresponding `chunks_vec` rows must go too.
    let v2 = vec![make_chunk("fn x() {}", 1, 1), make_chunk("fn y() {}", 2, 2)];
    store.insert_chunks(file_id, &v2).unwrap();
    let stored2 = store.get_chunks_by_file(file_id).unwrap();
    assert_eq!(stored2.len(), 2);
    for (i, c) in stored2.iter().enumerate() {
        store
            .update_chunk_embedding(c.id, &make_embedding(10.0 + i as f32))
            .unwrap();
    }

    assert_no_orphans(&store, "after reindex of same file");
}

#[test]
fn delete_file_purges_vectors() {
    let store = IndexStore::open_in_memory().unwrap();
    let file_id = store
        .insert_file("bar.rs", Some("rust"), "fn b() {}", 9, 1000)
        .unwrap();
    let chunks = vec![make_chunk("fn a() {}", 1, 1), make_chunk("fn b() {}", 2, 2)];
    store.insert_chunks(file_id, &chunks).unwrap();
    let stored = store.get_chunks_by_file(file_id).unwrap();
    for c in &stored {
        store
            .update_chunk_embedding(c.id, &make_embedding(0.5))
            .unwrap();
    }

    store.delete_file("bar.rs").unwrap();

    assert_no_orphans(&store, "after delete_file");
}

#[test]
fn clear_all_data_purges_vectors() {
    let store = IndexStore::open_in_memory().unwrap();
    let file_id = store
        .insert_file("baz.rs", Some("rust"), "fn b() {}", 9, 1000)
        .unwrap();
    let chunks = vec![make_chunk("fn a() {}", 1, 1)];
    store.insert_chunks(file_id, &chunks).unwrap();
    for c in store.get_chunks_by_file(file_id).unwrap() {
        store
            .update_chunk_embedding(c.id, &make_embedding(0.7))
            .unwrap();
    }

    store.clear_all_data().unwrap();

    assert_no_orphans(&store, "after clear_all_data");
}

/// Regression for the real production leak path (HIGH-1 / HIGH-2).
///
/// `AutoIndexer::index_file` calls `insert_file(path, ...)` on every reindex,
/// then `insert_chunks(file_id, ...)`, then `update_chunk_embedding(...)`.
///
/// The original bug: `insert_file` used `INSERT OR REPLACE`. Re-inserting the
/// same path deleted the old `files` row (FK CASCADE wiped its `chunks`, but
/// NOT the sqlite-vec `chunks_vec` rows) and minted a brand-new `files.id`.
/// `insert_chunks(new_id, ...)` then purged `chunks_vec` only for the *new*
/// file_id, leaving every prior embedding orphaned in `chunks_vec`.
///
/// After the HIGH-1 fix `insert_file` keeps a STABLE id on conflict, so the
/// subsequent `insert_chunks(file_id, ...)` purges the correct vectors and no
/// orphans accumulate. This test walks that exact call sequence twice (V1 then
/// V2 of the same path) and asserts both the stable-id contract and a clean
/// `chunks_vec`.
#[test]
fn prod_reindex_path_does_not_leak_vectors() {
    let store = IndexStore::open_in_memory().unwrap();

    // --- Pass 1: index "app.rs" with content V1 (hash V1). ---
    let id_v1 = store
        .insert_file(
            "app.rs",
            Some("rust"),
            "fn v1_a() {} fn v1_b() {}",
            24,
            1000,
        )
        .unwrap();

    let chunks_v1 = vec![
        make_chunk("fn v1_a() {}", 1, 1),
        make_chunk("fn v1_b() {}", 2, 2),
        make_chunk("fn v1_c() {}", 3, 3),
    ];
    store.insert_chunks(id_v1, &chunks_v1).unwrap();
    for (i, c) in store.get_chunks_by_file(id_v1).unwrap().iter().enumerate() {
        store
            .update_chunk_embedding(c.id, &make_embedding(i as f32))
            .unwrap();
    }
    assert_no_orphans(&store, "after V1 index pass");

    // --- Pass 2: same path, new content V2 (hash V2). This is the exact call
    // order AutoIndexer uses on a reindex: insert_file -> insert_chunks ->
    // update_chunk_embedding. ---
    let id_v2 = store
        .insert_file("app.rs", Some("rust"), "fn v2_x() {}", 12, 2000)
        .unwrap();

    // Stable-id contract (HIGH-1): re-inserting the same path must reuse the
    // existing row id. If this regresses to a fresh id, the old chunks_vec
    // rows below become orphans again.
    assert_eq!(
        id_v1, id_v2,
        "insert_file must return the stable existing id for the same path"
    );

    let chunks_v2 = vec![make_chunk("fn v2_x() {}", 1, 1)];
    store.insert_chunks(id_v2, &chunks_v2).unwrap();
    for c in store.get_chunks_by_file(id_v2).unwrap() {
        store
            .update_chunk_embedding(c.id, &make_embedding(100.0))
            .unwrap();
    }

    // The single remaining chunk must have an embedding and there must be no
    // orphan vectors left behind from V1.
    let final_chunks = store.get_chunks_by_file(id_v2).unwrap();
    assert_eq!(final_chunks.len(), 1, "V2 should leave exactly one chunk");
    assert_no_orphans(&store, "after V2 reindex via prod call path");
}

/// Stress: simulate the real-world pattern where the same file is reindexed
/// many times (e.g. while a user types). The original bug surfaced after
/// dozens of reindex cycles, so a single reindex assertion would not have
/// caught it. Locking this down with 50 cycles makes the regression test
/// realistic without adding meaningful runtime.
#[test]
fn many_reindexes_keep_invariant() {
    let store = IndexStore::open_in_memory().unwrap();
    let file_id = store
        .insert_file("hot.rs", Some("rust"), "v0", 2, 1000)
        .unwrap();

    for i in 0..50 {
        // Vary chunk count and content each cycle so chunk_ids actually
        // cycle (otherwise INSERT OR REPLACE would mask the leak).
        let n = (i % 4) + 1;
        let chunks: Vec<_> = (0..n)
            .map(|k| make_chunk(&format!("fn f{i}_{k}() {{}}"), k + 1, k + 1))
            .collect();
        store.insert_chunks(file_id, &chunks).unwrap();
        for c in store.get_chunks_by_file(file_id).unwrap() {
            store
                .update_chunk_embedding(c.id, &make_embedding(i as f32))
                .unwrap();
        }
    }

    assert_no_orphans(&store, "after 50 reindex cycles");
}
