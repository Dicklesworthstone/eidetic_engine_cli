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

/// One missed search read back from the query-miss audit log (bd-1n0np.6.3).
///
/// 6.3's privacy redaction stores `queryHash` only — the raw query text and the
/// query vector are deliberately NOT persisted (`queryTextStored: false`,
/// `queryVectorStored: false`). So paraphrase/vector clustering (the original
/// 6.4 ambition) is impossible on this data; the only honest signal is **exact
/// repeated misses of the same query hash**, which this models.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissAuditObservation {
    /// Opaque blake3 query hash (the only surviving query identity).
    pub query_hash: String,
    /// Why the search was recorded as a miss (e.g. `no_relevant_results`).
    pub reason: String,
}

/// A knowledge-gap candidate from repeated identical misses, hash-clustered.
/// Advisory only — a query the swarm keeps issuing with no useful result is an
/// honest signal the store lacks a memory, even when the text is redacted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepeatedMissGap {
    pub query_hash: String,
    pub miss_count: u32,
    /// Distinct miss reasons observed for this hash, sorted.
    pub reasons: Vec<String>,
}

/// Cluster query-miss audit observations by exact query hash into knowledge-gap
/// candidates (bd-1n0np.6.4). Deterministic: grouped by hash, blank hashes
/// dropped, hashes seen fewer than `min_misses` times excluded, output sorted by
/// descending miss count then hash so the most-reached-for gaps surface first.
#[must_use]
pub fn cluster_repeated_misses(
    observations: &[MissAuditObservation],
    min_misses: u32,
) -> Vec<RepeatedMissGap> {
    // query hash -> (miss count, distinct reasons)
    let mut by_hash: BTreeMap<String, (u32, BTreeSet<String>)> = BTreeMap::new();
    for observation in observations {
        let hash = observation.query_hash.trim();
        if hash.is_empty() {
            continue;
        }
        let entry = by_hash
            .entry(hash.to_string())
            .or_insert((0, BTreeSet::new()));
        entry.0 = entry.0.saturating_add(1);
        let reason = observation.reason.trim();
        if !reason.is_empty() {
            entry.1.insert(reason.to_string());
        }
    }

    let mut gaps: Vec<RepeatedMissGap> = by_hash
        .into_iter()
        .filter(|(_, (miss_count, _))| *miss_count >= min_misses)
        .map(|(query_hash, (miss_count, reasons))| RepeatedMissGap {
            query_hash,
            miss_count,
            reasons: reasons.into_iter().collect(),
        })
        .collect();
    gaps.sort_by(|left, right| {
        right
            .miss_count
            .cmp(&left.miss_count)
            .then_with(|| left.query_hash.cmp(&right.query_hash))
    });
    gaps
}

#[cfg(test)]
mod tests {
    use super::{
        KNOWLEDGE_GAP_MIN_CLUSTER_MISSES, MissAuditObservation, QueryMissObservation,
        cluster_query_misses, cluster_repeated_misses, query_cluster_key,
    };

    fn miss_audit(query_hash: &str, reason: &str) -> MissAuditObservation {
        MissAuditObservation {
            query_hash: query_hash.to_string(),
            reason: reason.to_string(),
        }
    }

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

    #[test]
    fn repeated_misses_below_threshold_are_excluded() {
        let observations = vec![
            miss_audit("blake3:aaa", "no_relevant_results"),
            miss_audit("blake3:aaa", "no_relevant_results"),
            miss_audit("blake3:aaa", "weak_query_recall"),
            miss_audit("blake3:bbb", "no_relevant_results"),
            miss_audit("   ", "no_relevant_results"),
        ];
        let gaps = cluster_repeated_misses(&observations, KNOWLEDGE_GAP_MIN_CLUSTER_MISSES);
        // aaa repeated 3x (>=3) is a gap; bbb (1) and the blank hash are excluded.
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].query_hash, "blake3:aaa");
        assert_eq!(gaps[0].miss_count, 3);
        // Distinct reasons are collected and sorted.
        assert_eq!(
            gaps[0].reasons,
            vec![
                "no_relevant_results".to_string(),
                "weak_query_recall".to_string()
            ]
        );
    }

    #[test]
    fn repeated_misses_rank_by_count_then_hash_deterministically() {
        let observations = vec![
            miss_audit("blake3:zzz", "no_relevant_results"),
            miss_audit("blake3:zzz", "no_relevant_results"),
            miss_audit("blake3:zzz", "no_relevant_results"),
            miss_audit("blake3:aaa", "no_relevant_results"),
            miss_audit("blake3:aaa", "no_relevant_results"),
            miss_audit("blake3:aaa", "no_relevant_results"),
            miss_audit("blake3:aaa", "no_relevant_results"),
        ];
        let mut reversed = observations.clone();
        reversed.reverse();
        let first = cluster_repeated_misses(&observations, 3);
        let second = cluster_repeated_misses(&reversed, 3);
        assert_eq!(
            first, second,
            "hash clustering is independent of input order"
        );
        // aaa (4 misses) outranks zzz (3 misses) despite the lexical order.
        assert_eq!(first[0].query_hash, "blake3:aaa");
        assert_eq!(first[0].miss_count, 4);
        assert_eq!(first[1].query_hash, "blake3:zzz");
    }
}
