use std::collections::BTreeMap;

use fnx_algorithms::CentralityScore;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PprPrefetchCacheKey {
    pub seed_set_hash: String,
    pub snapshot_generation: u64,
}

impl PprPrefetchCacheKey {
    #[must_use]
    pub fn new(seed_set_hash: impl Into<String>, snapshot_generation: u64) -> Self {
        Self {
            seed_set_hash: seed_set_hash.into(),
            snapshot_generation,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PprPrefetchCacheHit {
    pub scores: Vec<CentralityScore>,
    pub result_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PprPrefetchCacheInsert {
    pub result_hash: String,
    pub evicted: Vec<PprPrefetchCacheKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PprPrefetchCacheDebugEntry {
    pub seed_set_hash: String,
    pub snapshot_generation: u64,
    pub result_hash: String,
    pub score_count: usize,
    pub last_used_sequence: u64,
}

#[derive(Clone, Debug)]
struct PprPrefetchCacheEntry {
    scores: Vec<CentralityScore>,
    result_hash: String,
    last_used_sequence: u64,
}

#[derive(Debug)]
pub struct PprPrefetchCache {
    capacity: usize,
    access_sequence: u64,
    entries: BTreeMap<PprPrefetchCacheKey, PprPrefetchCacheEntry>,
}

impl PprPrefetchCache {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            access_sequence: 0,
            entries: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert(
        &mut self,
        key: PprPrefetchCacheKey,
        scores: Vec<CentralityScore>,
    ) -> PprPrefetchCacheInsert {
        let result_hash = ppr_prefetch_result_hash(&scores);
        if self.capacity == 0 {
            self.entries.clear();
            return PprPrefetchCacheInsert {
                result_hash,
                evicted: Vec::new(),
            };
        }

        let last_used_sequence = self.next_access_sequence();
        self.entries.insert(
            key,
            PprPrefetchCacheEntry {
                scores,
                result_hash: result_hash.clone(),
                last_used_sequence,
            },
        );
        let evicted = self.evict_to_capacity();
        PprPrefetchCacheInsert {
            result_hash,
            evicted,
        }
    }

    pub fn get(&mut self, key: &PprPrefetchCacheKey) -> Option<PprPrefetchCacheHit> {
        let entry = self.entries.get(key)?;
        let actual_hash = ppr_prefetch_result_hash(&entry.scores);
        if actual_hash != entry.result_hash {
            self.entries.remove(key);
            return None;
        }

        let last_used_sequence = self.next_access_sequence();
        let entry = self
            .entries
            .get_mut(key)
            .expect("entry exists after hash validation");
        entry.last_used_sequence = last_used_sequence;
        Some(PprPrefetchCacheHit {
            scores: entry.scores.clone(),
            result_hash: entry.result_hash.clone(),
        })
    }

    pub fn invalidate_generations_except(
        &mut self,
        snapshot_generation: u64,
    ) -> Vec<PprPrefetchCacheKey> {
        let stale = self
            .entries
            .keys()
            .filter(|key| key.snapshot_generation != snapshot_generation)
            .cloned()
            .collect::<Vec<_>>();
        for key in &stale {
            self.entries.remove(key);
        }
        stale
    }

    #[must_use]
    pub fn debug_dump(&self) -> Vec<PprPrefetchCacheDebugEntry> {
        self.entries
            .iter()
            .map(|(key, entry)| PprPrefetchCacheDebugEntry {
                seed_set_hash: key.seed_set_hash.clone(),
                snapshot_generation: key.snapshot_generation,
                result_hash: entry.result_hash.clone(),
                score_count: entry.scores.len(),
                last_used_sequence: entry.last_used_sequence,
            })
            .collect()
    }

    fn next_access_sequence(&mut self) -> u64 {
        self.access_sequence = self.access_sequence.saturating_add(1);
        self.access_sequence
    }

    fn evict_to_capacity(&mut self) -> Vec<PprPrefetchCacheKey> {
        let mut evicted = Vec::new();
        while self.entries.len() > self.capacity {
            let Some(victim) = self.lru_victim_key() else {
                break;
            };
            self.entries.remove(&victim);
            evicted.push(victim);
        }
        evicted
    }

    fn lru_victim_key(&self) -> Option<PprPrefetchCacheKey> {
        self.entries
            .iter()
            .min_by(|(left_key, left_entry), (right_key, right_entry)| {
                left_entry
                    .last_used_sequence
                    .cmp(&right_entry.last_used_sequence)
                    .then_with(|| left_key.cmp(right_key))
            })
            .map(|(key, _)| key.clone())
    }

    #[cfg(test)]
    fn corrupt_score_for_test(&mut self, key: &PprPrefetchCacheKey, score: f64) {
        if let Some(entry) = self.entries.get_mut(key)
            && let Some(first) = entry.scores.first_mut()
        {
            first.score = score;
        }
    }
}

#[must_use]
pub fn ppr_prefetch_result_hash(scores: &[CentralityScore]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.graph.ppr_prefetch_cache.result.v1");
    hasher.update(&(scores.len() as u64).to_le_bytes());
    for score in scores {
        hasher.update(&(score.node.len() as u64).to_le_bytes());
        hasher.update(score.node.as_bytes());
        hasher.update(&score.score.to_le_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};
    use std::thread;

    use super::*;

    fn key(seed: &str, generation: u64) -> PprPrefetchCacheKey {
        PprPrefetchCacheKey::new(format!("blake3:{seed}"), generation)
    }

    fn scores(nodes: &[(&str, f64)]) -> Vec<CentralityScore> {
        nodes
            .iter()
            .map(|(node, score)| CentralityScore {
                node: (*node).to_owned(),
                score: *score,
            })
            .collect()
    }

    #[test]
    fn empty_cache_misses() {
        let mut cache = PprPrefetchCache::new(2);

        assert_eq!(cache.get(&key("seed-a", 1)), None);
        assert!(cache.is_empty());
    }

    #[test]
    fn insert_then_hit_returns_scores_and_hash() {
        let mut cache = PprPrefetchCache::new(2);
        let key = key("seed-a", 1);
        let expected_scores = scores(&[("mem-a", 0.7), ("mem-b", 0.3)]);
        let insert = cache.insert(key.clone(), expected_scores.clone());

        let hit = cache.get(&key).expect("cache hit");

        assert_eq!(hit.scores, expected_scores);
        assert_eq!(hit.result_hash, insert.result_hash);
        assert_eq!(hit.result_hash, ppr_prefetch_result_hash(&expected_scores));
    }

    #[test]
    fn generation_mismatch_misses_without_removing_exact_generation() {
        let mut cache = PprPrefetchCache::new(4);
        let old_key = key("seed-a", 1);
        let old_scores = scores(&[("mem-a", 1.0)]);
        cache.insert(old_key.clone(), old_scores.clone());

        assert_eq!(cache.get(&key("seed-a", 2)), None);
        assert_eq!(
            cache.get(&old_key).expect("old generation hit").scores,
            old_scores
        );
    }

    #[test]
    fn generation_invalidation_removes_incompatible_entries() {
        let mut cache = PprPrefetchCache::new(4);
        let old_key = key("seed-a", 1);
        let live_key = key("seed-a", 2);
        cache.insert(old_key.clone(), scores(&[("old", 1.0)]));
        cache.insert(live_key.clone(), scores(&[("live", 1.0)]));

        let removed = cache.invalidate_generations_except(2);

        assert_eq!(removed, vec![old_key.clone()]);
        assert_eq!(cache.get(&old_key), None);
        assert!(cache.get(&live_key).is_some());
    }

    #[test]
    fn lru_eviction_removes_oldest_accessed_entry() {
        let mut cache = PprPrefetchCache::new(2);
        let first = key("first", 1);
        let second = key("second", 1);
        let third = key("third", 1);
        cache.insert(first.clone(), scores(&[("first", 1.0)]));
        cache.insert(second.clone(), scores(&[("second", 1.0)]));
        assert!(cache.get(&first).is_some());

        let insert = cache.insert(third.clone(), scores(&[("third", 1.0)]));

        assert_eq!(insert.evicted, vec![second.clone()]);
        assert!(cache.get(&first).is_some());
        assert_eq!(cache.get(&second), None);
        assert!(cache.get(&third).is_some());
    }

    #[test]
    fn insert_after_eviction_reuses_capacity() {
        let mut cache = PprPrefetchCache::new(1);
        let first = key("first", 1);
        let second = key("second", 1);
        let third = key("third", 1);

        cache.insert(first.clone(), scores(&[("first", 1.0)]));
        cache.insert(second.clone(), scores(&[("second", 1.0)]));
        let insert = cache.insert(third.clone(), scores(&[("third", 1.0)]));

        assert_eq!(insert.evicted, vec![second]);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(&first), None);
        assert!(cache.get(&third).is_some());
    }

    #[test]
    fn debug_dump_is_sorted_by_key() {
        let mut cache = PprPrefetchCache::new(4);
        cache.insert(key("seed-c", 1), scores(&[("c", 1.0)]));
        cache.insert(key("seed-a", 2), scores(&[("a", 1.0)]));
        cache.insert(key("seed-a", 1), scores(&[("a-old", 1.0)]));

        let dump = cache.debug_dump();
        let order = dump
            .iter()
            .map(|entry| (entry.seed_set_hash.as_str(), entry.snapshot_generation))
            .collect::<Vec<_>>();

        assert_eq!(
            order,
            vec![
                ("blake3:seed-a", 1),
                ("blake3:seed-a", 2),
                ("blake3:seed-c", 1)
            ]
        );
    }

    #[test]
    fn hash_mismatch_evicts_corrupted_entry() {
        let mut cache = PprPrefetchCache::new(2);
        let key = key("seed-a", 1);
        cache.insert(key.clone(), scores(&[("mem-a", 1.0)]));
        cache.corrupt_score_for_test(&key, 0.5);

        assert_eq!(cache.get(&key), None);
        assert!(cache.is_empty());
    }

    #[test]
    fn shared_lock_concurrent_insert_smoke() {
        let cache = Arc::new(RwLock::new(PprPrefetchCache::new(8)));
        let mut handles = Vec::new();
        for index in 0..8 {
            let cache = Arc::clone(&cache);
            handles.push(thread::spawn(move || {
                let key = key(&format!("seed-{index}"), 1);
                let mut guard = cache.write().expect("cache lock");
                guard.insert(key, scores(&[(&format!("mem-{index}"), index as f64)]));
            }));
        }
        for handle in handles {
            handle.join().expect("thread joins");
        }

        let guard = cache.read().expect("cache lock");
        let dump = guard.debug_dump();

        assert_eq!(dump.len(), 8);
        assert_eq!(dump[0].seed_set_hash, "blake3:seed-0");
        assert_eq!(dump[7].seed_set_hash, "blake3:seed-7");
    }
}
