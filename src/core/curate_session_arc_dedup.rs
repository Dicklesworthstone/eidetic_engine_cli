//! Bounded, session-local deduplication of complete source-backed lesson pairs.
//!
//! Repeated descriptions of the same failure and the EXACT same repair need
//! not consume separate review slots. Never synthesize a combined source or
//! increase confidence: keep the first complete pair in canonical source order.
//! Different repair bytes, resources, literals or polarity remain distinct.
//! Exhausted dedup budgets retain proposals unchanged, not partially paired.

use std::collections::{BTreeMap, BTreeSet};

use super::{ReviewSessionCandidate, resource_keys};
use crate::search::simhash::canonicalize_content_for_simhash;

const MAX_FACET_BYTES: usize = 8 * 1024;
const MAX_SHINGLES: usize = 512;
const MIN_NEAR_SHINGLES: usize = 8;
const MAX_REPRESENTATIVES: usize = 1024;
const MAX_PER_REPAIR: usize = 32;
const MAX_RETAINED_BYTES: usize = 4 * 1024 * 1024;

struct Signature {
    failure: String,
    repair: String,
    resources: BTreeSet<String>,
    protected: Vec<String>,
    shingles: BTreeSet<String>,
    retained_bytes: usize,
}

impl Signature {
    fn new(failure: &str, repair: &str) -> Option<Self> {
        if failure.len() > MAX_FACET_BYTES || repair.len() > MAX_FACET_BYTES {
            return None;
        }
        let resources = resource_keys(failure);
        let protected = protected_tokens(failure);
        let failure = canonicalize_content_for_simhash(failure);
        if failure.is_empty() || repair.trim().is_empty() {
            return None;
        }
        let mut shingles = BTreeSet::new();
        let mut words = failure.split_whitespace();
        let mut previous = words.next()?;
        // Bound work by the number of windows, not just distinct shingles:
        // repeated one-letter tokens must not evade the preparation budget.
        for (index, word) in words.enumerate() {
            if index == MAX_SHINGLES {
                return None;
            }
            shingles.insert(format!("{previous} {word}"));
            previous = word;
        }
        let repair = repair.to_owned();
        // Include string capacity, collection entries and conservative node
        // overhead. This is an owned-payload allowance, not an RSS promise.
        let retained_bytes = resources
            .iter()
            .chain(&protected)
            .chain(&shingles)
            .fold(
                failure.capacity() + repair.capacity() + std::mem::size_of::<Self>(),
                |total, text| total.saturating_add(text.capacity() + 128),
            );
        Some(Self {
            failure,
            repair,
            resources,
            protected,
            shingles,
            retained_bytes,
        })
    }

    fn duplicates(&self, earlier: &Self) -> bool {
        if self.repair != earlier.repair
            || self.resources != earlier.resources
            || self.protected != earlier.protected
        {
            return false;
        }
        if self.failure == earlier.failure {
            return true;
        }
        // Short or unanchored failures need an exact normalized match. A high
        // bag-of-words overlap alone cannot equate anonymous error messages.
        if self.resources.is_empty()
            || self.shingles.len() < MIN_NEAR_SHINGLES
            || earlier.shingles.len() < MIN_NEAR_SHINGLES
        {
            return false;
        }
        let intersection = self.shingles.intersection(&earlier.shingles).count();
        let union = self.shingles.len() + earlier.shingles.len() - intersection;
        // Exact, deterministic Jaccard confirmation; no hash-collision merge,
        // approximate score, floating threshold or transitive clustering.
        intersection * 100 >= union * 85
    }
}

/// Preserve ordered technical literals and polarity, in addition to exact
/// case-sensitive resources. Similarity must not erase an error code, a flag,
/// a quoted identifier, a counter, or a negation. This intentionally prefers a
/// missed duplicate over suppressing a materially different observation.
fn protected_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|token| {
            let word = token.trim_matches(|ch: char| ch.is_ascii_punctuation());
            let lower = word.to_ascii_lowercase();
            matches!(
                lower.as_str(),
                "no" | "not" | "never" | "without" | "cannot" | "can't"
                    | "before" | "after" | "only" | "unless" | "except"
                    | "increase" | "decrease" | "enable" | "disable"
                    | "enabled" | "disabled" | "true" | "false" | "null" | "none"
            ) || token.chars().any(|ch| ch.is_ascii_digit())
                || token.contains(['_', '/', '\\', ':', '=', '`', '"', '\'', '$'])
                || token.starts_with('-')
                || word.chars().any(char::is_uppercase)
        })
        .map(str::to_owned)
        .collect()
}

#[derive(Default)]
struct PairIndex {
    // Repair hashes only narrow comparisons. `duplicates` verifies the exact
    // repair bytes even if a hash collision were ever presented.
    repairs: BTreeMap<[u8; 32], Vec<Signature>>,
    representatives: usize,
    retained_bytes: usize,
}

impl PairIndex {
    /// Return true only for a verified duplicate of a retained representative.
    /// Do not index a duplicate: A~B and B~C must never manufacture A~C.
    fn is_duplicate(&mut self, signature: Signature) -> bool {
        let hash = *blake3::hash(signature.repair.as_bytes()).as_bytes();
        if let Some(earlier) = self.repairs.get(&hash) {
            if earlier.iter().any(|row| signature.duplicates(row)) {
                return true;
            }
            if earlier.len() >= MAX_PER_REPAIR {
                return false;
            }
        }
        if self.representatives >= MAX_REPRESENTATIVES
            || self.retained_bytes.saturating_add(signature.retained_bytes) > MAX_RETAINED_BYTES
        {
            return false;
        }
        self.retained_bytes += signature.retained_bytes;
        self.representatives += 1;
        self.repairs.entry(hash).or_default().push(signature);
        false
    }
}

fn pair_signature(left: &ReviewSessionCandidate, right: &ReviewSessionCandidate) -> Option<Signature> {
    let left = left.session_arc.as_ref()?;
    let right = right.session_arc.as_ref()?;
    for (left, right) in [
        (&left.failure_span, &right.failure_span),
        (&left.resolution_span, &right.resolution_span),
    ] {
        if left.evidence_span_id != right.evidence_span_id
            || left.content_hash != right.content_hash
            || left.excerpt != right.excerpt
        {
            return None;
        }
    }
    Signature::new(&left.failure_span.excerpt, &left.resolution_span.excerpt)
}

pub(super) fn retain_distinct_pairs(rows: Vec<ReviewSessionCandidate>) -> Vec<ReviewSessionCandidate> {
    let mut index = PairIndex::default();
    let mut output = Vec::new();
    let mut rows = rows.into_iter();
    while let Some(left) = rows.next() {
        let Some(right) = rows.next() else {
            // The production caller supplies pairs. Do not silently discard an
            // unexpected orphan if another caller violates that precondition.
            output.push(left);
            break;
        };
        if pair_signature(&left, &right).is_some_and(|signature| index.is_duplicate(signature)) {
            continue;
        }
        output.push(left);
        output.push(right);
    }
    output
}

#[cfg(test)]
#[path = "curate_session_arc_dedup_tests.rs"]
mod tests;
