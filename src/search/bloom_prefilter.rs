use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const DEFAULT_HASH_COUNT: usize = 7;
const DEFAULT_FALSE_POSITIVE_RATE: f64 = 0.01;

/// Counting Bloom filter used by the SRR4 negation prefilter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CountingBloomPrefilter {
    counters: Vec<u8>,
    hash_count: usize,
    inserted: usize,
}

impl CountingBloomPrefilter {
    #[must_use]
    pub fn with_capacity(expected_items: usize) -> Self {
        let bit_count = optimal_bit_count(expected_items, DEFAULT_FALSE_POSITIVE_RATE);
        Self {
            counters: vec![0; bit_count.max(1)],
            hash_count: DEFAULT_HASH_COUNT,
            inserted: 0,
        }
    }

    pub fn insert(&mut self, value: &str) {
        let indexes = self.indexes(value);
        for index in indexes {
            let counter = &mut self.counters[index];
            *counter = counter.saturating_add(1);
        }
        self.inserted = self.inserted.saturating_add(1);
    }

    pub fn remove(&mut self, value: &str) {
        let indexes = self.indexes(value);
        for index in indexes {
            let counter = &mut self.counters[index];
            *counter = counter.saturating_sub(1);
        }
        self.inserted = self.inserted.saturating_sub(1);
    }

    #[must_use]
    pub fn might_contain(&self, value: &str) -> bool {
        self.indexes(value)
            .into_iter()
            .all(|index| self.counters[index] > 0)
    }

    #[must_use]
    pub fn definitely_absent(&self, value: &str) -> bool {
        !self.might_contain(value)
    }

    #[must_use]
    pub fn estimated_false_positive_rate(&self) -> f64 {
        if self.counters.is_empty() {
            return 0.0;
        }
        let m = self.counters.len() as f64;
        let k = self.hash_count as f64;
        let n = self.inserted as f64;
        (1.0 - (-k * n / m).exp()).powf(k)
    }

    fn indexes(&self, value: &str) -> Vec<usize> {
        let first = hash_with_seed(value, 0);
        let second = hash_with_seed(value, 1).max(1);
        (0..self.hash_count)
            .map(|i| {
                let combined = first.wrapping_add((i as u64).wrapping_mul(second));
                combined as usize % self.counters.len()
            })
            .collect()
    }
}

fn optimal_bit_count(expected_items: usize, false_positive_rate: f64) -> usize {
    if expected_items == 0 {
        return DEFAULT_HASH_COUNT * 8;
    }
    let n = expected_items as f64;
    let m = -(n * false_positive_rate.ln()) / std::f64::consts::LN_2.powi(2);
    m.ceil() as usize
}

fn hash_with_seed(value: &str, seed: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::CountingBloomPrefilter;

    #[test]
    fn empty_filter_reports_absent() {
        let filter = CountingBloomPrefilter::with_capacity(16);
        assert!(filter.definitely_absent("rust"));
    }

    #[test]
    fn inserted_values_may_be_present() {
        let mut filter = CountingBloomPrefilter::with_capacity(16);
        filter.insert("archived");
        assert!(filter.might_contain("archived"));
    }

    #[test]
    fn removed_values_become_absent_when_no_other_insert_remains() {
        let mut filter = CountingBloomPrefilter::with_capacity(16);
        filter.insert("archived");
        filter.remove("archived");
        assert!(filter.definitely_absent("archived"));
    }

    #[test]
    fn bloom_false_positive_rate_within_budget() {
        let mut filter = CountingBloomPrefilter::with_capacity(128);
        for index in 0..128 {
            filter.insert(&format!("tag-{index}"));
        }
        assert!(filter.estimated_false_positive_rate() <= 0.015);
    }
}
