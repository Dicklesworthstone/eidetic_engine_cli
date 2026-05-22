use std::sync::atomic::{AtomicU64, Ordering};

use fnx_algorithms::{CentralityScore, PageRankResult};

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

#[derive(Clone, Debug, PartialEq)]
pub struct PprPrefetchCacheResultHit {
    pub result: PageRankResult,
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

#[derive(Debug)]
struct PprPrefetchCacheEntry {
    scores: Vec<CentralityScore>,
    result: Option<PageRankResult>,
    result_hash: String,
    /// bd-2lin9: stored atomically so the cache read path
    /// (`PprPrefetchCache::get` / `get_result`) can touch the LRU
    /// timestamp through a shared `&self` and the outer
    /// `RwLock<PprPrefetchCache>` can be acquired via `.read()`
    /// instead of `.write()` for cache hits. Ordering is `Relaxed`
    /// throughout: the LRU eviction policy reads these values under
    /// the outer write lock (so a snapshot is consistent within one
    /// `evict_to_capacity` call), and the determinism contract is
    /// preserved because `next_access_sequence` returns unique
    /// monotonically-increasing values via `fetch_add`.
    last_used_sequence: AtomicU64,
}

impl PprPrefetchCacheEntry {
    fn scores(&self) -> &[CentralityScore] {
        self.result
            .as_ref()
            .map(|result| result.scores.as_slice())
            .unwrap_or(&self.scores)
    }

    fn last_used(&self) -> u64 {
        self.last_used_sequence.load(Ordering::Relaxed)
    }

    fn touch(&self, last_used_sequence: u64) {
        self.last_used_sequence
            .store(last_used_sequence, Ordering::Relaxed);
    }
}

impl Clone for PprPrefetchCacheEntry {
    fn clone(&self) -> Self {
        Self {
            scores: self.scores.clone(),
            result: self.result.clone(),
            result_hash: self.result_hash.clone(),
            last_used_sequence: AtomicU64::new(self.last_used()),
        }
    }
}

#[derive(Clone, Debug)]
struct PprPrefetchCacheSlot {
    key: PprPrefetchCacheKey,
    entry: PprPrefetchCacheEntry,
}

#[derive(Debug)]
struct PprPrefetchCuckooTable {
    buckets: Vec<Option<PprPrefetchCacheSlot>>,
    len: usize,
}

impl PprPrefetchCuckooTable {
    fn new(entry_capacity: usize) -> Self {
        let bucket_count = cuckoo_bucket_count(entry_capacity);
        let mut buckets = Vec::with_capacity(bucket_count);
        buckets.resize_with(bucket_count, || None);
        Self { buckets, len: 0 }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn clear(&mut self) {
        for bucket in &mut self.buckets {
            *bucket = None;
        }
        self.len = 0;
    }

    fn contains_key(&self, key: &PprPrefetchCacheKey) -> bool {
        self.slot_index(key).is_some()
    }

    fn get(&self, key: &PprPrefetchCacheKey) -> Option<&PprPrefetchCacheEntry> {
        let index = self.slot_index(key)?;
        self.buckets[index].as_ref().map(|slot| &slot.entry)
    }

    fn get_mut(&mut self, key: &PprPrefetchCacheKey) -> Option<&mut PprPrefetchCacheEntry> {
        let index = self.slot_index(key)?;
        self.buckets[index].as_mut().map(|slot| &mut slot.entry)
    }

    fn remove(&mut self, key: &PprPrefetchCacheKey) -> Option<PprPrefetchCacheEntry> {
        let index = self.slot_index(key)?;
        let slot = self.buckets[index].take()?;
        self.len = self.len.saturating_sub(1);
        Some(slot.entry)
    }

    fn insert(
        &mut self,
        key: PprPrefetchCacheKey,
        entry: PprPrefetchCacheEntry,
    ) -> Option<(PprPrefetchCacheKey, PprPrefetchCacheEntry)> {
        if self.buckets.is_empty() {
            return Some((key, entry));
        }
        if let Some(existing) = self.get_mut(&key) {
            *existing = entry;
            return None;
        }
        self.insert_new(key, entry)
    }

    fn insert_new(
        &mut self,
        mut key: PprPrefetchCacheKey,
        mut entry: PprPrefetchCacheEntry,
    ) -> Option<(PprPrefetchCacheKey, PprPrefetchCacheEntry)> {
        let mut bucket_index = self.index_one(&key);
        for _ in 0..self.max_displacements() {
            let slot = PprPrefetchCacheSlot { key, entry };
            let Some(displaced) = self.buckets[bucket_index].replace(slot) else {
                self.len += 1;
                return None;
            };
            key = displaced.key;
            entry = displaced.entry;
            bucket_index = self.alternate_index(&key, bucket_index);
        }
        Some((key, entry))
    }

    fn iter(&self) -> impl Iterator<Item = (&PprPrefetchCacheKey, &PprPrefetchCacheEntry)> {
        self.buckets
            .iter()
            .filter_map(|bucket| bucket.as_ref().map(|slot| (&slot.key, &slot.entry)))
    }

    fn slot_index(&self, key: &PprPrefetchCacheKey) -> Option<usize> {
        if self.buckets.is_empty() {
            return None;
        }
        let first = self.index_one(key);
        if self.key_matches_bucket(key, first) {
            return Some(first);
        }
        let second = self.index_two(key);
        if first != second && self.key_matches_bucket(key, second) {
            return Some(second);
        }
        None
    }

    fn key_matches_bucket(&self, key: &PprPrefetchCacheKey, bucket_index: usize) -> bool {
        self.buckets
            .get(bucket_index)
            .and_then(Option::as_ref)
            .is_some_and(|slot| slot.key == *key)
    }

    fn index_one(&self, key: &PprPrefetchCacheKey) -> usize {
        cuckoo_index(
            key,
            b"ee.graph.ppr_prefetch_cache.cuckoo.a.v1",
            self.buckets.len(),
        )
    }

    fn index_two(&self, key: &PprPrefetchCacheKey) -> usize {
        let bucket_count = self.buckets.len();
        let first = self.index_one(key);
        let second = cuckoo_index(
            key,
            b"ee.graph.ppr_prefetch_cache.cuckoo.b.v1",
            bucket_count,
        );
        if bucket_count > 1 && second == first {
            (second + 1) % bucket_count
        } else {
            second
        }
    }

    fn alternate_index(&self, key: &PprPrefetchCacheKey, current_index: usize) -> usize {
        let first = self.index_one(key);
        let second = self.index_two(key);
        if current_index == first {
            second
        } else {
            first
        }
    }

    fn max_displacements(&self) -> usize {
        16 + self.buckets.len().ilog2() as usize * 2
    }
}

fn cuckoo_bucket_count(entry_capacity: usize) -> usize {
    entry_capacity.saturating_mul(4).max(2).next_power_of_two()
}

fn cuckoo_index(key: &PprPrefetchCacheKey, domain: &[u8], bucket_count: usize) -> usize {
    debug_assert!(bucket_count > 0);
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&(key.seed_set_hash.len() as u64).to_le_bytes());
    hasher.update(key.seed_set_hash.as_bytes());
    hasher.update(&key.snapshot_generation.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    (u64::from_le_bytes(bytes) as usize) % bucket_count
}

#[derive(Debug)]
pub struct PprPrefetchCache {
    capacity: usize,
    /// bd-2lin9: atomic so `next_access_sequence` can be called from
    /// the read path (`get` / `get_result` against `&self`).
    /// `fetch_add(1, Relaxed)` guarantees every caller observes a
    /// unique sequence number even under concurrent loads — the LRU
    /// tie-breaker on lexical key order in `lru_victim_key` keeps
    /// eviction deterministic across runs.
    access_sequence: AtomicU64,
    live_generation: Option<u64>,
    entries: PprPrefetchCuckooTable,
}

impl PprPrefetchCache {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            access_sequence: AtomicU64::new(0),
            live_generation: None,
            entries: PprPrefetchCuckooTable::new(capacity),
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
        let result_hash = ppr_prefetch_result_hash(&key, &scores);
        let snapshot_generation = key.snapshot_generation;
        let entry = PprPrefetchCacheEntry {
            scores,
            result: None,
            result_hash: result_hash.clone(),
            last_used_sequence: AtomicU64::new(self.next_access_sequence()),
        };
        if self.capacity == 0 {
            let evicted = self.evict_for_generation_insert(snapshot_generation);
            self.entries.clear();
            return PprPrefetchCacheInsert {
                result_hash,
                evicted,
            };
        }

        let mut evicted = self.evict_for_generation_insert(snapshot_generation);
        self.evict_before_new_key(&key, &mut evicted);
        if let Some((evicted_key, _entry)) = self.entries.insert(key, entry) {
            evicted.push(evicted_key);
        }
        evicted.extend(self.evict_to_capacity());
        PprPrefetchCacheInsert {
            result_hash,
            evicted,
        }
    }

    pub fn insert_result(
        &mut self,
        key: PprPrefetchCacheKey,
        result: PageRankResult,
    ) -> PprPrefetchCacheInsert {
        let result_hash = ppr_prefetch_page_rank_result_hash(&key, &result);
        let snapshot_generation = key.snapshot_generation;
        let entry = PprPrefetchCacheEntry {
            scores: result.scores.clone(),
            result: Some(result),
            result_hash: result_hash.clone(),
            last_used_sequence: AtomicU64::new(self.next_access_sequence()),
        };
        if self.capacity == 0 {
            let evicted = self.evict_for_generation_insert(snapshot_generation);
            self.entries.clear();
            return PprPrefetchCacheInsert {
                result_hash,
                evicted,
            };
        }

        let mut evicted = self.evict_for_generation_insert(snapshot_generation);
        self.evict_before_new_key(&key, &mut evicted);
        if let Some((evicted_key, _entry)) = self.entries.insert(key, entry) {
            evicted.push(evicted_key);
        }
        evicted.extend(self.evict_to_capacity());
        PprPrefetchCacheInsert {
            result_hash,
            evicted,
        }
    }

    /// bd-2lin9: `get` is the cache-read hot path; it takes `&self`
    /// so the surrounding `RwLock<PprPrefetchCache>` can be acquired
    /// via `.read()` and concurrent lookups can run in parallel. The
    /// LRU touch happens through `PprPrefetchCacheEntry::touch`
    /// (atomic store). A hash-mismatched entry is treated as a miss
    /// (returns `None`); the actual removal is deferred until the
    /// next mutating call path (`insert` / `insert_result` /
    /// `invalidate_generations_except`) takes the write lock. This
    /// is safe because:
    ///
    /// 1. Returning `None` makes the caller compute fresh and
    ///    `insert_result`, which evicts via `evict_before_new_key`
    ///    and the cuckoo replace path; the stale slot is reclaimed
    ///    promptly.
    /// 2. A lingering hash-mismatched entry occupies a cache slot
    ///    but its `last_used_sequence` is never updated by a
    ///    subsequent `get` call (because `get` returns early on the
    ///    mismatch), so the LRU eviction in `evict_to_capacity`
    ///    treats it as the oldest entry and evicts it preferentially.
    pub fn get(&self, key: &PprPrefetchCacheKey) -> Option<PprPrefetchCacheHit> {
        if !self.entry_hash_is_valid(key) {
            return None;
        }
        let entry = self.entries.get(key)?;
        entry.touch(self.next_access_sequence());
        Some(PprPrefetchCacheHit {
            scores: entry.scores().to_vec(),
            result_hash: entry.result_hash.clone(),
        })
    }

    /// bd-2lin9: same `&self` read-path contract as `get`; see
    /// `PprPrefetchCache::get` for the hash-mismatch / deferred-
    /// eviction semantics.
    pub fn get_result(&self, key: &PprPrefetchCacheKey) -> Option<PprPrefetchCacheResultHit> {
        if !self.entry_hash_is_valid(key) {
            return None;
        }
        let entry = self.entries.get(key)?;
        let result = entry.result.clone()?;
        entry.touch(self.next_access_sequence());
        Some(PprPrefetchCacheResultHit {
            result,
            result_hash: entry.result_hash.clone(),
        })
    }

    pub fn invalidate_generations_except(
        &mut self,
        snapshot_generation: u64,
    ) -> Vec<PprPrefetchCacheKey> {
        self.live_generation = Some(snapshot_generation);
        self.remove_generations_except(snapshot_generation)
    }

    fn evict_for_generation_insert(
        &mut self,
        snapshot_generation: u64,
    ) -> Vec<PprPrefetchCacheKey> {
        if self.live_generation == Some(snapshot_generation) {
            return Vec::new();
        }
        self.live_generation = Some(snapshot_generation);
        self.remove_generations_except(snapshot_generation)
    }

    fn remove_generations_except(&mut self, snapshot_generation: u64) -> Vec<PprPrefetchCacheKey> {
        let stale = self
            .entries
            .iter()
            .map(|(key, _entry)| key)
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
        let mut dump = self
            .entries
            .iter()
            .map(|(key, entry)| PprPrefetchCacheDebugEntry {
                seed_set_hash: key.seed_set_hash.clone(),
                snapshot_generation: key.snapshot_generation,
                result_hash: entry.result_hash.clone(),
                score_count: entry.scores().len(),
                last_used_sequence: entry.last_used(),
            })
            .collect::<Vec<_>>();
        dump.sort_by(|left, right| {
            left.seed_set_hash
                .cmp(&right.seed_set_hash)
                .then_with(|| left.snapshot_generation.cmp(&right.snapshot_generation))
        });
        dump
    }

    fn next_access_sequence(&self) -> u64 {
        // bd-2lin9: fetch_add returns the OLD value; we add 1 ourselves
        // to preserve the original "first call returns 1" semantics
        // (matches the pre-refactor saturating_add path). Saturating
        // semantics are preserved by clamping at u64::MAX; in practice
        // the counter would need >2^64 cache hits to wrap, so the
        // simpler `+ 1` is sufficient.
        self.access_sequence
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
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

    fn evict_before_new_key(
        &mut self,
        key: &PprPrefetchCacheKey,
        evicted: &mut Vec<PprPrefetchCacheKey>,
    ) {
        if self.entries.contains_key(key) || self.entries.len() < self.capacity {
            return;
        }
        if let Some(victim) = self.lru_victim_key() {
            self.entries.remove(&victim);
            evicted.push(victim);
        }
    }

    fn lru_victim_key(&self) -> Option<PprPrefetchCacheKey> {
        self.entries
            .iter()
            .min_by(|(left_key, left_entry), (right_key, right_entry)| {
                // bd-2lin9: read the atomic last_used_sequence with
                // Relaxed ordering. Eviction always runs under the
                // outer RwLock write lock (insert / insert_result /
                // remove_generations_except / evict_to_capacity all
                // require &mut self), so the snapshot read here is
                // consistent within one eviction pass — no concurrent
                // touches can race with the comparison.
                left_entry
                    .last_used()
                    .cmp(&right_entry.last_used())
                    .then_with(|| left_key.cmp(right_key))
            })
            .map(|(key, _)| key.clone())
    }

    fn entry_hash_is_valid(&self, key: &PprPrefetchCacheKey) -> bool {
        let Some(entry) = self.entries.get(key) else {
            return false;
        };
        let actual_hash = match &entry.result {
            Some(result) => ppr_prefetch_page_rank_result_hash(key, result),
            None => ppr_prefetch_result_hash(key, &entry.scores),
        };
        actual_hash == entry.result_hash
    }

    #[cfg(test)]
    fn corrupt_score_for_test(&mut self, key: &PprPrefetchCacheKey, score: f64) {
        if let Some(entry) = self.entries.get_mut(key) {
            if let Some(result) = &mut entry.result
                && let Some(first) = result.scores.first_mut()
            {
                first.score = score;
                return;
            }
            if let Some(first) = entry.scores.first_mut() {
                first.score = score;
            }
        }
    }

    #[cfg(test)]
    fn corrupt_result_witness_algorithm_for_test(
        &mut self,
        key: &PprPrefetchCacheKey,
        algorithm: &str,
    ) {
        if let Some(entry) = self.entries.get_mut(key)
            && let Some(result) = &mut entry.result
        {
            result.witness.algorithm = algorithm.to_owned();
        }
    }

    #[cfg(test)]
    fn swap_entries_for_test(&mut self, left: &PprPrefetchCacheKey, right: &PprPrefetchCacheKey) {
        let left_entry = self.entries.remove(left).expect("left entry");
        let right_entry = self.entries.remove(right).expect("right entry");
        self.entries.insert(left.clone(), right_entry);
        self.entries.insert(right.clone(), left_entry);
    }
}

#[must_use]
pub fn ppr_prefetch_result_hash(key: &PprPrefetchCacheKey, scores: &[CentralityScore]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.graph.ppr_prefetch_cache.result.v1");
    hasher.update(&(key.seed_set_hash.len() as u64).to_le_bytes());
    hasher.update(key.seed_set_hash.as_bytes());
    hasher.update(&key.snapshot_generation.to_le_bytes());
    hasher.update(&(scores.len() as u64).to_le_bytes());
    for score in scores {
        hasher.update(&(score.node.len() as u64).to_le_bytes());
        hasher.update(score.node.as_bytes());
        hasher.update(&score.score.to_le_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

#[must_use]
pub fn ppr_prefetch_page_rank_result_hash(
    key: &PprPrefetchCacheKey,
    result: &PageRankResult,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.graph.ppr_prefetch_cache.page_rank_result.v1");
    hasher.update(&(key.seed_set_hash.len() as u64).to_le_bytes());
    hasher.update(key.seed_set_hash.as_bytes());
    hasher.update(&key.snapshot_generation.to_le_bytes());
    hasher.update(&[u8::from(result.converged)]);
    update_hash_with_str(&mut hasher, &result.witness.algorithm);
    update_hash_with_str(&mut hasher, &result.witness.complexity_claim);
    hasher.update(&result.witness.nodes_touched.to_le_bytes());
    hasher.update(&result.witness.edges_scanned.to_le_bytes());
    hasher.update(&result.witness.queue_peak.to_le_bytes());
    hasher.update(&(result.scores.len() as u64).to_le_bytes());
    for score in &result.scores {
        hasher.update(&(score.node.len() as u64).to_le_bytes());
        hasher.update(score.node.as_bytes());
        hasher.update(&score.score.to_le_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn update_hash_with_str(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
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

    fn page_rank_result(nodes: &[(&str, f64)]) -> PageRankResult {
        page_rank_result_with_witness(
            nodes,
            "personalized_pagerank_power_iteration",
            "O(k * (|V| + |E|))",
        )
    }

    fn page_rank_result_with_witness(
        nodes: &[(&str, f64)],
        algorithm: &str,
        complexity_claim: &str,
    ) -> PageRankResult {
        PageRankResult {
            scores: scores(nodes),
            converged: true,
            witness: fnx_algorithms::ComplexityWitness {
                algorithm: algorithm.to_owned(),
                complexity_claim: complexity_claim.to_owned(),
                nodes_touched: 3,
                edges_scanned: 2,
                queue_peak: 0,
            },
        }
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
        assert_eq!(
            hit.result_hash,
            ppr_prefetch_result_hash(&key, &expected_scores)
        );
    }

    #[test]
    fn insert_result_then_hit_returns_full_result() {
        let mut cache = PprPrefetchCache::new(2);
        let key = key("seed-a", 1);
        let expected = page_rank_result(&[("mem-a", 0.7), ("mem-b", 0.3)]);
        let insert = cache.insert_result(key.clone(), expected.clone());

        let hit = cache.get_result(&key).expect("cache hit");

        assert_eq!(hit.result, expected);
        assert_eq!(hit.result_hash, insert.result_hash);
        assert_eq!(
            hit.result_hash,
            ppr_prefetch_page_rank_result_hash(&key, &expected)
        );
    }

    #[test]
    fn zero_capacity_insert_returns_hash_without_retaining_entries() {
        let mut cache = PprPrefetchCache::new(0);
        let score_key = key("seed-score", 1);
        let expected_scores = scores(&[("mem-a", 0.7), ("mem-b", 0.3)]);
        let result_key = key("seed-result", 1);
        let expected_result = page_rank_result(&[("mem-c", 1.0)]);

        let score_insert = cache.insert(score_key.clone(), expected_scores.clone());
        let result_insert = cache.insert_result(result_key.clone(), expected_result.clone());

        assert_eq!(
            score_insert.result_hash,
            ppr_prefetch_result_hash(&score_key, &expected_scores)
        );
        assert_eq!(
            result_insert.result_hash,
            ppr_prefetch_page_rank_result_hash(&result_key, &expected_result)
        );
        assert!(score_insert.evicted.is_empty());
        assert!(result_insert.evicted.is_empty());
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.get(&score_key), None);
        assert_eq!(cache.get_result(&result_key), None);
        assert!(cache.debug_dump().is_empty());
    }

    #[test]
    fn full_result_hash_length_prefixes_witness_strings() {
        let key = key("seed-a", 1);
        let left = page_rank_result_with_witness(&[("mem-a", 1.0)], "ab", "c");
        let right = page_rank_result_with_witness(&[("mem-a", 1.0)], "a", "bc");

        assert_ne!(
            ppr_prefetch_page_rank_result_hash(&key, &left),
            ppr_prefetch_page_rank_result_hash(&key, &right)
        );
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

        let removed = cache.invalidate_generations_except(2);

        assert_eq!(removed, vec![old_key.clone()]);
        assert_eq!(cache.get(&old_key), None);
        assert!(cache.is_empty());
        let insert = cache.insert(live_key.clone(), scores(&[("live", 1.0)]));
        assert!(insert.evicted.is_empty());
        assert!(cache.get(&live_key).is_some());
    }

    #[test]
    fn generation_bump_on_insert_evicts_prior_generation() {
        let mut cache = PprPrefetchCache::new(4);
        let old_key = key("seed-a", 1);
        let live_key = key("seed-a", 2);
        cache.insert(old_key.clone(), scores(&[("old", 1.0)]));

        let insert = cache.insert(live_key.clone(), scores(&[("live", 1.0)]));

        assert_eq!(insert.evicted, vec![old_key.clone()]);
        assert_eq!(cache.get(&old_key), None);
        assert!(cache.get(&live_key).is_some());
    }

    #[test]
    fn generation_bump_on_result_insert_evicts_prior_generation() {
        let mut cache = PprPrefetchCache::new(4);
        let old_key = key("seed-a", 1);
        let live_key = key("seed-a", 2);
        cache.insert_result(old_key.clone(), page_rank_result(&[("old", 1.0)]));

        let insert = cache.insert_result(live_key.clone(), page_rank_result(&[("live", 1.0)]));

        assert_eq!(insert.evicted, vec![old_key.clone()]);
        assert_eq!(cache.get_result(&old_key), None);
        assert!(cache.get_result(&live_key).is_some());
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
        cache.insert(key("seed-b", 1), scores(&[("b", 1.0)]));
        cache.insert(key("seed-a", 1), scores(&[("a", 1.0)]));

        let dump = cache.debug_dump();
        let order = dump
            .iter()
            .map(|entry| (entry.seed_set_hash.as_str(), entry.snapshot_generation))
            .collect::<Vec<_>>();

        assert_eq!(
            order,
            vec![
                ("blake3:seed-a", 1),
                ("blake3:seed-b", 1),
                ("blake3:seed-c", 1)
            ]
        );
    }

    // bd-2lin9: after the read-path refactor `get` / `get_result`
    // hold a shared `&self` and CANNOT remove the corrupted entry
    // in place. They return `None` for the mismatch (the safety-
    // critical contract — a corrupted hit never leaks to the
    // caller); the actual eviction is deferred to the next mutating
    // call (next `insert`, `insert_result`, or
    // `invalidate_generations_except`). The two tests below verify
    // both halves of that contract: (a) the read returns None on
    // mismatch, (b) a subsequent insert reclaims the slot via the
    // LRU path so memory does not leak.

    #[test]
    fn hash_mismatch_returns_none_and_lingers_until_next_insert() {
        let mut cache = PprPrefetchCache::new(2);
        let key = key("seed-a", 1);
        cache.insert(key.clone(), scores(&[("mem-a", 1.0)]));
        cache.corrupt_score_for_test(&key, 0.5);

        // Read path returns None for the mismatched entry — corrupted
        // results never leak to callers.
        assert_eq!(cache.get(&key), None);

        // The stale slot lingers until the next mutating call path.
        // A second `insert` for the SAME key replaces it via the
        // cuckoo-table in-place update at line ~131
        // (PprPrefetchCuckooTable::insert), restoring a clean entry.
        cache.insert(key.clone(), scores(&[("mem-a", 1.0)]));
        let hit = cache.get(&key).expect("post-refresh hit");
        assert_eq!(hit.scores.len(), 1);
    }

    #[test]
    fn full_result_hash_mismatch_returns_none_and_lingers_until_next_insert() {
        let mut cache = PprPrefetchCache::new(2);
        let key = key("seed-a", 1);
        cache.insert_result(key.clone(), page_rank_result(&[("mem-a", 1.0)]));
        cache.corrupt_result_witness_algorithm_for_test(&key, "tampered_algorithm");

        // Witness-tampered result never leaks back to the caller.
        assert_eq!(cache.get_result(&key), None);

        // Refresh the entry via the canonical insert path; the stale
        // slot is replaced and subsequent reads succeed.
        cache.insert_result(key.clone(), page_rank_result(&[("mem-a", 1.0)]));
        let hit = cache.get_result(&key).expect("post-refresh hit");
        assert_eq!(hit.result.scores.len(), 1);
    }

    // bd-2lin9: same deferred-eviction semantics as the
    // hash_mismatch_returns_none_and_lingers_until_next_insert
    // test above. Key-score-swap corruption returns None on the
    // read path; both stale slots get reclaimed by subsequent
    // inserts.
    #[test]
    fn hash_mismatch_key_score_swap_returns_none_on_read_path() {
        let mut cache = PprPrefetchCache::new(2);
        let first = key("seed-a", 1);
        let second = key("seed-b", 1);
        cache.insert(first.clone(), scores(&[("mem-a", 1.0)]));
        cache.insert(second.clone(), scores(&[("mem-b", 1.0)]));
        cache.swap_entries_for_test(&first, &second);

        assert_eq!(cache.get(&first), None);
        assert_eq!(cache.get(&second), None);
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

        {
            let mut guard = cache.write().expect("cache lock");
            for index in 0..8 {
                let key = key(&format!("seed-{index}"), 1);
                let expected_scores = scores(&[(&format!("mem-{index}"), index as f64)]);
                let hit = guard.get(&key).expect("inserted entry remains readable");
                assert_eq!(hit.scores, expected_scores);
                assert_eq!(
                    hit.result_hash,
                    ppr_prefetch_result_hash(&key, &expected_scores)
                );
            }
        }

        let guard = cache.read().expect("cache lock");
        let dump = guard.debug_dump();

        assert_eq!(dump.len(), 8);
        assert_eq!(dump[0].seed_set_hash, "blake3:seed-0");
        assert_eq!(dump[7].seed_set_hash, "blake3:seed-7");
    }
}
