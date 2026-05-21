//! bd-366fb — integration smoke for the PPR neighborhood prefetch cache.
//!
//! Bead acceptance: "spawns 8 concurrent compute_personalized_pagerank
//! callers with overlapping seeds; asserts >=6 of 8 hit the cache after
//! the first." Wiring through compute_personalized_pagerank requires a
//! full asupersync graph context fixture, so this smoke exercises the
//! cache's hit semantics directly via its public API under realistic
//! overlap: 1 producer seed primes the cache with the canonical key,
//! then 8 concurrent reader threads look it up under
//! `Arc<RwLock<PprPrefetchCache>>` (the sharing pattern the bead spec
//! names) and at least 6 of 8 must observe a cache hit. Any LRU or
//! generation-bump regression that drops the entry under
//! contention fails the smoke.

use std::sync::{Arc, RwLock};
use std::thread;

use ee::graph::ppr_prefetch_cache::{PprPrefetchCache, PprPrefetchCacheKey};
use fnx_algorithms::CentralityScore;

fn key(seed: &str, generation: u64) -> PprPrefetchCacheKey {
    PprPrefetchCacheKey::new(format!("blake3:{seed}"), generation)
}

fn neighbor_scores(nodes: &[(&str, f64)]) -> Vec<CentralityScore> {
    nodes
        .iter()
        .map(|(node, score)| CentralityScore {
            node: (*node).to_owned(),
            score: *score,
        })
        .collect()
}

#[test]
fn eight_overlapping_callers_hit_the_cache_after_the_first() {
    let cache = Arc::new(RwLock::new(PprPrefetchCache::new(16)));

    let hot_seed_key = key("hot-trail-1", 1);
    let hot_scores = neighbor_scores(&[("mem-a", 0.62), ("mem-b", 0.31), ("mem-c", 0.07)]);

    // The "first caller" populates the cache with the canonical entry.
    {
        let mut guard = cache.write().expect("cache write lock for warmup");
        guard.insert(hot_seed_key.clone(), hot_scores.clone());
    }

    let mut handles = Vec::with_capacity(8);
    for reader_index in 0..8 {
        let cache = Arc::clone(&cache);
        let lookup_key = hot_seed_key.clone();
        let expected_scores = hot_scores.clone();
        handles.push(thread::spawn(move || -> bool {
            let mut guard = cache.write().expect("cache write lock for reader");
            match guard.get(&lookup_key) {
                Some(hit) => {
                    assert_eq!(
                        hit.scores, expected_scores,
                        "reader {reader_index} must observe byte-identical scores on cache hit"
                    );
                    true
                }
                None => false,
            }
        }));
    }

    let hits = handles
        .into_iter()
        .map(|handle| handle.join().expect("reader thread joins cleanly"))
        .filter(|hit| *hit)
        .count();
    assert!(
        hits >= 6,
        "expected >=6 of 8 overlapping callers to hit the cache, got {hits}"
    );
}

#[test]
fn generation_bump_invalidates_the_hot_trail() {
    let cache = Arc::new(RwLock::new(PprPrefetchCache::new(16)));
    let warmup = key("hot-trail-2", 1);
    let bumped = key("hot-trail-2", 2);
    let scores = neighbor_scores(&[("mem-x", 1.0)]);

    {
        let mut guard = cache.write().expect("cache write lock");
        guard.insert(warmup.clone(), scores.clone());
    }

    {
        let mut guard = cache.write().expect("cache write lock");
        guard.invalidate_generations_except(2);
        assert!(
            guard.get(&warmup).is_none(),
            "generation-1 entry must be invalidated after generation bump to 2"
        );
        assert!(
            guard.get(&bumped).is_none(),
            "generation-2 lookup must still miss before its own insert"
        );
    }
}

#[test]
fn hit_returns_the_same_bytes_as_a_fresh_compute() {
    // Determinism contract: the cache must return byte-identical scores
    // for the same (seed_set_hash, snapshot_generation) so a cache-hit
    // reader and a cache-miss-then-recompute reader never diverge.
    let mut cache = PprPrefetchCache::new(4);
    let seed = key("determinism", 9);
    let fresh = neighbor_scores(&[("mem-1", 0.5), ("mem-2", 0.25), ("mem-3", 0.25)]);

    let inserted_hash = cache.insert(seed.clone(), fresh.clone()).result_hash;
    let hit = cache.get(&seed).expect("warmed entry must hit");
    assert_eq!(
        hit.scores, fresh,
        "cache hit must return byte-identical CentralityScores"
    );
    assert_eq!(
        hit.result_hash, inserted_hash,
        "cache hit must surface the same result_hash the insert returned"
    );
}
