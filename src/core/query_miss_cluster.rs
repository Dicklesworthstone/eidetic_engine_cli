//! bd-1n0np.6.4 — query-miss clustering into knowledge-gap candidates.
//!
//! Clusters low-utility/missed searches (from the query-miss ledger,
//! bd-1n0np.6.3) into knowledge-gap candidates: a tight cluster of repeated
//! misses on the same topic is an honest signal that the store lacks a memory
//! the swarm keeps reaching for. The result is REPORTING/ADVISORY only — tight
//! clusters become `knowledge_gap` curation candidates routed through the curate
//! pipeline and surfaced in `ee swarm brief`; nothing here writes memory.
//!
//! This core is pure and deterministic: misses are grouped by a normalized,
//! order-independent token-set key (so paraphrases collapse), and output is
//! sorted. A richer similarity clustering (fnx Louvain / DBSCAN over miss
//! embeddings) is a future enrichment over this reporting-first core; the
//! caller loads ledger rows and routes the candidates into `curate`.

use std::collections::{BTreeMap, BTreeSet};

/// Default minimum total misses in a cluster before it is proposed as a
/// knowledge-gap candidate (a one-off miss is noise, not a gap).
pub const KNOWLEDGE_GAP_MIN_CLUSTER_MISSES: u32 = 3;

/// One observed missed/low-utility query from the ledger (bd-1n0np.6.3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryMissObservation {
    pub query: String,
    pub miss_count: u32,
}

/// A proposed knowledge-gap candidate: a tight cluster of repeated misses.
/// Advisory only — the caller routes it into the curate pipeline for review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeGapCandidate {
    /// Normalized cluster key (order-independent token set).
    pub cluster_key: String,
    /// A stable representative query (lexicographically smallest member).
    pub representative_query: String,
    /// Total misses across the cluster.
    pub miss_count: u32,
    /// Distinct member queries, sorted.
    pub member_queries: Vec<String>,
}

/// Order-independent normalized key for a query: lowercased, whitespace-split,
/// deduplicated, sorted token set. Paraphrases that differ only by word order or
/// casing collapse to the same key. Empty for blank/whitespace-only queries.
#[must_use]
pub fn query_cluster_key(query: &str) -> String {
    let mut tokens: Vec<String> = query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens.join(" ")
}

/// Cluster query misses into knowledge-gap candidates (bd-1n0np.6.4).
/// Deterministic: misses are grouped by [`query_cluster_key`], blank queries are
/// dropped, clusters below `min_cluster_misses` are excluded, and both the
/// candidate list and each member list are sorted.
#[must_use]
pub fn cluster_query_misses(
    observations: &[QueryMissObservation],
    min_cluster_misses: u32,
) -> Vec<KnowledgeGapCandidate> {
    // cluster key -> (total miss count, distinct member queries)
    let mut clusters: BTreeMap<String, (u32, BTreeSet<String>)> = BTreeMap::new();
    for observation in observations {
        let query = observation.query.trim();
        if query.is_empty() {
            continue;
        }
        let key = query_cluster_key(query);
        if key.is_empty() {
            continue;
        }
        let entry = clusters.entry(key).or_insert((0, BTreeSet::new()));
        entry.0 = entry.0.saturating_add(observation.miss_count);
        entry.1.insert(query.to_string());
    }

    clusters
        .into_iter()
        .filter(|(_, (miss_count, _))| *miss_count >= min_cluster_misses)
        .map(|(cluster_key, (miss_count, members))| {
            let member_queries: Vec<String> = members.into_iter().collect();
            let representative_query = member_queries
                .first()
                .cloned()
                .unwrap_or_else(|| cluster_key.clone());
            KnowledgeGapCandidate {
                cluster_key,
                representative_query,
                miss_count,
                member_queries,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        KNOWLEDGE_GAP_MIN_CLUSTER_MISSES, QueryMissObservation, cluster_query_misses,
        query_cluster_key,
    };

    fn miss(query: &str, count: u32) -> QueryMissObservation {
        QueryMissObservation {
            query: query.to_string(),
            miss_count: count,
        }
    }

    #[test]
    fn cluster_key_is_order_and_case_independent() {
        assert_eq!(
            query_cluster_key("Fix flaky Socket timeout"),
            query_cluster_key("socket TIMEOUT flaky fix")
        );
        assert_eq!(query_cluster_key("   "), "");
    }

    #[test]
    fn paraphrased_misses_cluster_into_one_candidate() {
        let observations = vec![
            miss("fix flaky socket timeout", 2),
            miss("socket timeout flaky fix", 3),
            miss("unrelated cargo build error", 1),
        ];
        let candidates = cluster_query_misses(&observations, KNOWLEDGE_GAP_MIN_CLUSTER_MISSES);
        // The socket-timeout paraphrases (2+3=5 >= 3) cluster; the lone cargo
        // miss (1 < 3) is below threshold.
        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert_eq!(candidate.miss_count, 5);
        assert_eq!(candidate.member_queries.len(), 2);
        assert_eq!(candidate.representative_query, "fix flaky socket timeout");
    }

    #[test]
    fn below_threshold_and_blank_are_excluded() {
        let observations = vec![miss("one off miss", 1), miss("   ", 9)];
        let candidates = cluster_query_misses(&observations, 3);
        assert!(candidates.is_empty());
    }

    #[test]
    fn clustering_is_deterministic_and_order_independent() {
        let forward = vec![
            miss("disk pressure cleanup", 2),
            miss("cleanup disk pressure", 2),
            miss("rch worker offline", 4),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();

        let first = cluster_query_misses(&forward, 3);
        let second = cluster_query_misses(&reversed, 3);
        assert_eq!(first, second, "clustering is independent of input order");
        assert_eq!(first.len(), 2); // disk-pressure cluster (4) + rch cluster (4)
    }
}
