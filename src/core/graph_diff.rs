//! bd-3a1op.5 — `ee graph diff` temporal structural diff (ADR 0066).
//!
//! Diffs two PERSISTED graph snapshots of one family: node/edge add/remove
//! sets (content-hash keyed, deterministically ordered — `diff(A,B)` sets are
//! exact complements of `diff(B,A)`), community deltas via stable
//! best-overlap fingerprint matching (Louvain labels are not stable across
//! runs, so communities are matched by maximum member-set Jaccard with a
//! ≥ 0.5 same-community threshold; below threshold reports birth/death), and
//! top-N centrality movers ranked by |Δpagerank| from the centrality values
//! PERSISTED inside each snapshot — never recomputed inline; nodes missing a
//! persisted value on either side are honestly omitted and counted.
//!
//! Pure over parsed snapshot views: the CLI layer resolves snapshots and
//! feeds `metrics_json`; everything here is deterministic.

use std::collections::{BTreeMap, BTreeSet};

use fnx_classes::Graph;
use fnx_runtime::CompatibilityMode;
use serde::Serialize;

/// Wire schema id for the diff report.
pub const GRAPH_DIFF_SCHEMA_V1: &str = "ee.graph.diff.v1";
/// Governor truncation point for every detail array (declared in the report).
pub const GRAPH_DIFF_DETAIL_CAP: usize = 64;
/// Top-N centrality movers reported.
pub const GRAPH_DIFF_MOVERS_CAP: usize = 10;
/// Minimum member-set Jaccard for two communities to count as the SAME
/// community across snapshots (ADR 0066).
pub const COMMUNITY_MATCH_JACCARD_THRESHOLD: f64 = 0.5;

/// One edge as persisted in a snapshot's metrics payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffEdge {
    /// Content-hash key (blake3 over the canonical `src|relation|dst|directed`
    /// string; undirected edges canonicalize endpoint order first).
    pub key: String,
    pub source: String,
    pub target: String,
    pub relation: String,
    pub directed: bool,
}

/// Parsed, diff-ready view of one persisted snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotGraphView {
    pub snapshot_id: String,
    pub snapshot_version: u32,
    pub created_at: String,
    /// Node id → persisted pagerank (None when the snapshot has no persisted
    /// value for that node — honestly omitted from movers, never recomputed).
    pub nodes: BTreeMap<String, Option<f64>>,
    pub edges: BTreeMap<String, DiffEdge>,
}

/// Content-hash key for an edge (deterministic, orientation-canonical for
/// undirected edges).
#[must_use]
pub fn edge_key(source: &str, target: &str, relation: &str, directed: bool) -> String {
    let (a, b) = if directed || source <= target {
        (source, target)
    } else {
        (target, source)
    };
    let canonical = format!("{a}|{relation}|{b}|{directed}");
    let hash = blake3::hash(canonical.as_bytes()).to_hex();
    format!("edge_{}", &hash.as_str()[..26])
}

/// Parse persisted flat or legacy nested topology without inventing an empty
/// graph from missing or malformed rows. Explicit empty arrays remain valid.
/// Typed snapshot families store relation metadata under edge attributes.
pub fn parse_snapshot_graph(
    snapshot_id: &str,
    snapshot_version: u32,
    created_at: &str,
    metrics_json: &str,
) -> Result<SnapshotGraphView, String> {
    let metrics: serde_json::Value = serde_json::from_str(metrics_json)
        .map_err(|error| format!("snapshot {snapshot_id} metrics_json did not parse: {error}"))?;
    if !metrics.is_object() {
        return Err("snapshot metrics must be a JSON object".to_owned());
    }
    if let Some(schema) = metrics.get("schema")
        && schema.as_str() != Some("ee.graph.snapshot.metrics.v1")
    {
        return Err("snapshot metrics schema is unsupported".to_owned());
    }
    let graph_type = metrics
        .get("graphType")
        .map(|value| snapshot_text(Some(value), "graphType"))
        .transpose()?;
    let node_values = snapshot_array(&metrics, "nodes")?;
    let edge_values = snapshot_array(&metrics, "edges")?;
    let snapshot_pagerank_observed = snapshot_pagerank_is_observed(
        metrics
            .get("centrality")
            .and_then(|centrality| centrality.get("algorithmStatus")),
    )?;
    let mut nodes = BTreeMap::new();
    for node in node_values {
        let id = snapshot_text(
            consistent_alias(node.get("id"), node.get("memoryId"), "node identity")?,
            "node identity",
        )?;
        let pagerank = match node.get("pagerank") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(
                value
                    .as_f64()
                    .filter(|score| score.is_finite() && *score >= 0.0)
                    .ok_or_else(|| "snapshot pagerank must be nonnegative and finite".to_owned())?,
            ),
        };
        let node_pagerank_observed = snapshot_pagerank_is_observed(node.get("metricStatus"))?;
        // Cooperative refresh persists zero placeholders for unavailable metrics.
        // Either producer status can withhold authority, even when they disagree.
        let pagerank = if snapshot_pagerank_observed && node_pagerank_observed {
            pagerank
        } else {
            None
        };
        if nodes.insert(id.to_owned(), pagerank).is_some() {
            return Err("snapshot contains duplicate node identities".to_owned());
        }
    }

    let mut edges = BTreeMap::new();
    for edge in edge_values {
        let source = snapshot_text(
            consistent_alias(
                edge.get("source"),
                edge.get("sourceMemoryId"),
                "edge source",
            )?,
            "edge source",
        )?;
        let target = snapshot_text(
            consistent_alias(
                edge.get("target"),
                edge.get("targetMemoryId"),
                "edge target",
            )?,
            "edge target",
        )?;
        if !nodes.contains_key(source) || !nodes.contains_key(target) {
            return Err("snapshot edge references an undeclared node".to_owned());
        }
        let attrs = match edge.get("attrs") {
            None => None,
            Some(value) => Some(
                value
                    .as_object()
                    .ok_or_else(|| "snapshot edge attrs must be an object".to_owned())?,
            ),
        };
        let relation = consistent_alias(
            edge.get("relation"),
            attrs.and_then(|attrs| attrs.get("relation")),
            "edge relation",
        )?;
        let typed_relation = match graph_type {
            Some("revision_dag") => attrs.and_then(|attrs| attrs.get("transition_kind")),
            Some("rule_provenance") => attrs.and_then(|attrs| attrs.get("rule_role")),
            _ => None,
        };
        let relation = match consistent_alias(relation, typed_relation, "edge relation")? {
            Some(value) => snapshot_text(Some(value), "edge relation")?,
            None => match graph_type {
                Some("causal_evidence") => "causal_evidence",
                None if attrs.is_none() => "related",
                _ => return Err("snapshot edge relation is missing".to_owned()),
            },
        };
        let directed = match consistent_alias(
            edge.get("directed"),
            attrs.and_then(|attrs| attrs.get("directed")),
            "edge directedness",
        )? {
            Some(value) => value
                .as_bool()
                .ok_or_else(|| "snapshot edge directedness must be boolean".to_owned())?,
            None => match graph_type {
                Some("causal_evidence" | "revision_dag" | "contradiction_subgraph") => true,
                Some("rule_provenance") => false,
                None if attrs.is_none() => false,
                _ => return Err("snapshot edge directedness is missing".to_owned()),
            },
        };
        let (source, target) = if directed || source <= target {
            (source, target)
        } else {
            (target, source)
        };
        let key = edge_key(source, target, relation, directed);
        edges.insert(
            key.clone(),
            DiffEdge {
                key,
                source: source.to_owned(),
                target: target.to_owned(),
                relation: relation.to_owned(),
                directed,
            },
        );
    }

    Ok(SnapshotGraphView {
        snapshot_id: snapshot_id.to_owned(),
        snapshot_version,
        created_at: created_at.to_owned(),
        nodes,
        edges,
    })
}

fn snapshot_array<'a>(
    metrics: &'a serde_json::Value,
    field: &str,
) -> Result<&'a [serde_json::Value], String> {
    metrics
        .get(field)
        .or_else(|| metrics.get("graph").and_then(|graph| graph.get(field)))
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("snapshot {field} must be an explicit array"))
}

fn consistent_alias<'a>(
    first: Option<&'a serde_json::Value>,
    second: Option<&'a serde_json::Value>,
    field: &str,
) -> Result<Option<&'a serde_json::Value>, String> {
    match (first, second) {
        (Some(left), Some(right)) if left != right => {
            Err(format!("snapshot {field} aliases disagree"))
        }
        (Some(value), _) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn snapshot_text<'a>(value: Option<&'a serde_json::Value>, field: &str) -> Result<&'a str, String> {
    value
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| format!("snapshot {field} must be a nonempty string"))
}

fn snapshot_pagerank_is_observed(
    metric_status: Option<&serde_json::Value>,
) -> Result<bool, String> {
    let Some(metric_status) = metric_status else {
        return Ok(true);
    };
    let metric_status = metric_status
        .as_object()
        .ok_or_else(|| "snapshot metric status must be an object".to_owned())?;
    let Some(pagerank_status) = metric_status.get("pagerank") else {
        return Ok(true);
    };
    let status = pagerank_status
        .as_str()
        .and_then(crate::graph::CentralityAlgorithmStatus::from_str)
        .ok_or_else(|| "snapshot pagerank status is unsupported".to_owned())?;
    Ok(matches!(
        status,
        crate::graph::CentralityAlgorithmStatus::Computed
    ))
}

/// Select by the represented instant, not RFC3339 text ordering. Keep the
/// first row on a timestamp tie so the caller's snapshot-version order wins.
pub(crate) fn snapshot_at_or_before<'a>(
    timestamps: impl IntoIterator<Item = &'a str>,
    since: &chrono::DateTime<chrono::Utc>,
) -> Result<Option<usize>, String> {
    let mut nearest: Option<(usize, chrono::DateTime<chrono::Utc>)> = None;
    for (index, raw) in timestamps.into_iter().enumerate() {
        let timestamp = chrono::DateTime::parse_from_rfc3339(raw)
            .map(|at| at.with_timezone(&chrono::Utc))
            .map_err(|_| "persisted graph snapshot timestamp must be RFC3339".to_owned())?;
        if timestamp <= *since
            && nearest
                .as_ref()
                .is_none_or(|(_, current)| timestamp > *current)
        {
            nearest = Some((index, timestamp));
        }
    }
    Ok(nearest.map(|(index, _)| index))
}

/// One matched community pair with its membership churn.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityMatch {
    /// Deterministic fingerprint of the FROM-side community (lexically
    /// smallest member — stable across Louvain label permutations).
    pub from_fingerprint: String,
    pub to_fingerprint: String,
    pub jaccard: f64,
    pub joined: Vec<String>,
    pub left: Vec<String>,
}

/// A community present on only one side after fingerprint matching.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityEndpoint {
    pub fingerprint: String,
    pub size: usize,
    pub members: Vec<String>,
}

/// Community-level delta between two snapshots.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityDelta {
    pub matched: Vec<CommunityMatch>,
    pub births: Vec<CommunityEndpoint>,
    pub deaths: Vec<CommunityEndpoint>,
    /// Matched pairs with zero membership churn (not listed in `matched`).
    pub unchanged: usize,
}

fn louvain_from_edges(edges: &BTreeMap<String, DiffEdge>) -> Vec<BTreeSet<String>> {
    let mut graph = Graph::new(CompatibilityMode::Strict);
    for edge in edges.values() {
        let _ = graph.extend_edges_unrecorded([(edge.source.as_str(), edge.target.as_str())]);
    }
    crate::graph::health::detect_louvain_communities(&graph)
        .into_iter()
        .map(|members| members.into_iter().collect::<BTreeSet<String>>())
        .filter(|members| !members.is_empty())
        .collect()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let intersection = a.intersection(b).count();
    let union = a.len() + b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn fingerprint(members: &BTreeSet<String>) -> String {
    members
        .iter()
        .next()
        .cloned()
        .unwrap_or_else(|| "<empty>".to_owned())
}

fn endpoint(members: &BTreeSet<String>) -> CommunityEndpoint {
    let mut listed: Vec<String> = members.iter().cloned().collect();
    listed.truncate(GRAPH_DIFF_DETAIL_CAP);
    CommunityEndpoint {
        fingerprint: fingerprint(members),
        size: members.len(),
        members: listed,
    }
}

/// Match communities across two snapshots by maximum member-set Jaccard
/// (greedy, deterministic: overlap desc, then from/to fingerprints).
/// Pairs at or above [`COMMUNITY_MATCH_JACCARD_THRESHOLD`] are the SAME
/// community (membership churn reported); the rest are births/deaths.
#[must_use]
pub fn diff_communities(
    from_edges: &BTreeMap<String, DiffEdge>,
    to_edges: &BTreeMap<String, DiffEdge>,
) -> CommunityDelta {
    let from = louvain_from_edges(from_edges);
    let to = louvain_from_edges(to_edges);

    let mut candidates: Vec<(usize, usize, f64)> = Vec::new();
    for (i, a) in from.iter().enumerate() {
        for (j, b) in to.iter().enumerate() {
            let overlap = jaccard(a, b);
            if overlap >= COMMUNITY_MATCH_JACCARD_THRESHOLD {
                candidates.push((i, j, overlap));
            }
        }
    }
    candidates.sort_by(|x, y| {
        y.2.partial_cmp(&x.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| fingerprint(&from[x.0]).cmp(&fingerprint(&from[y.0])))
            .then_with(|| fingerprint(&to[x.1]).cmp(&fingerprint(&to[y.1])))
    });

    let mut from_used = vec![false; from.len()];
    let mut to_used = vec![false; to.len()];
    let mut matched = Vec::new();
    let mut unchanged = 0usize;
    for (i, j, overlap) in candidates {
        if from_used[i] || to_used[j] {
            continue;
        }
        from_used[i] = true;
        to_used[j] = true;
        let joined: Vec<String> = to[j].difference(&from[i]).cloned().collect();
        let left: Vec<String> = from[i].difference(&to[j]).cloned().collect();
        if joined.is_empty() && left.is_empty() {
            unchanged += 1;
            continue;
        }
        let mut joined = joined;
        let mut left = left;
        joined.truncate(GRAPH_DIFF_DETAIL_CAP);
        left.truncate(GRAPH_DIFF_DETAIL_CAP);
        matched.push(CommunityMatch {
            from_fingerprint: fingerprint(&from[i]),
            to_fingerprint: fingerprint(&to[j]),
            jaccard: overlap,
            joined,
            left,
        });
    }

    let mut deaths: Vec<CommunityEndpoint> = from
        .iter()
        .enumerate()
        .filter(|(i, _)| !from_used[*i])
        .map(|(_, members)| endpoint(members))
        .collect();
    let mut births: Vec<CommunityEndpoint> = to
        .iter()
        .enumerate()
        .filter(|(j, _)| !to_used[*j])
        .map(|(_, members)| endpoint(members))
        .collect();
    deaths.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    births.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    deaths.truncate(GRAPH_DIFF_DETAIL_CAP);
    births.truncate(GRAPH_DIFF_DETAIL_CAP);
    matched.sort_by(|a, b| a.from_fingerprint.cmp(&b.from_fingerprint));

    CommunityDelta {
        matched,
        births,
        deaths,
        unchanged,
    }
}

/// One centrality mover: pagerank persisted on BOTH sides, ranked by |delta|.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CentralityMover {
    pub memory_id: String,
    pub from_pagerank: f64,
    pub to_pagerank: f64,
    pub delta: f64,
}

/// Which detail arrays hit the governor cap.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffTruncation {
    pub nodes_added: bool,
    pub nodes_removed: bool,
    pub edges_added: bool,
    pub edges_removed: bool,
}

/// Reference to one side of the diff.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRef {
    pub snapshot_id: String,
    pub snapshot_version: u32,
    pub created_at: String,
    pub node_count: usize,
    pub edge_count: usize,
}

/// Summary counts (full, pre-truncation).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub nodes_added: usize,
    pub nodes_removed: usize,
    pub edges_added: usize,
    pub edges_removed: usize,
    pub communities_matched: usize,
    pub community_births: usize,
    pub community_deaths: usize,
    pub centrality_movers: usize,
    /// Nodes present on both sides but missing a persisted pagerank on at
    /// least one side — omitted from movers, never recomputed.
    pub centrality_omitted: usize,
}

/// The full `ee.graph.diff.v1` report payload.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphDiffReport {
    pub schema: &'static str,
    pub graph_type: String,
    pub from: SnapshotRef,
    pub to: SnapshotRef,
    pub summary: DiffSummary,
    pub nodes_added: Vec<String>,
    pub nodes_removed: Vec<String>,
    pub edges_added: Vec<DiffEdge>,
    pub edges_removed: Vec<DiffEdge>,
    pub communities: CommunityDelta,
    pub movers: Vec<CentralityMover>,
    /// Governor truncation point for every detail array.
    pub detail_cap: usize,
    pub truncated: DiffTruncation,
}

fn snapshot_ref(view: &SnapshotGraphView) -> SnapshotRef {
    SnapshotRef {
        snapshot_id: view.snapshot_id.clone(),
        snapshot_version: view.snapshot_version,
        created_at: view.created_at.clone(),
        node_count: view.nodes.len(),
        edge_count: view.edges.len(),
    }
}

/// Compute the structural diff between two parsed snapshot views.
#[must_use]
pub fn diff_snapshots(
    graph_type: &str,
    from: &SnapshotGraphView,
    to: &SnapshotGraphView,
) -> GraphDiffReport {
    let nodes_added_full: Vec<String> = to
        .nodes
        .keys()
        .filter(|id| !from.nodes.contains_key(*id))
        .cloned()
        .collect();
    let nodes_removed_full: Vec<String> = from
        .nodes
        .keys()
        .filter(|id| !to.nodes.contains_key(*id))
        .cloned()
        .collect();
    let edges_added_full: Vec<DiffEdge> = to
        .edges
        .iter()
        .filter(|(key, _)| !from.edges.contains_key(*key))
        .map(|(_, edge)| edge.clone())
        .collect();
    let edges_removed_full: Vec<DiffEdge> = from
        .edges
        .iter()
        .filter(|(key, _)| !to.edges.contains_key(*key))
        .map(|(_, edge)| edge.clone())
        .collect();

    let mut movers: Vec<CentralityMover> = Vec::new();
    let mut centrality_omitted = 0usize;
    for (id, from_rank) in &from.nodes {
        let Some(to_rank) = to.nodes.get(id) else {
            continue;
        };
        match (from_rank, to_rank) {
            (Some(from_rank), Some(to_rank)) => {
                let delta = to_rank - from_rank;
                if delta != 0.0 {
                    movers.push(CentralityMover {
                        memory_id: id.clone(),
                        from_pagerank: *from_rank,
                        to_pagerank: *to_rank,
                        delta,
                    });
                }
            }
            _ => centrality_omitted += 1,
        }
    }
    movers.sort_by(|a, b| {
        b.delta
            .abs()
            .partial_cmp(&a.delta.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.memory_id.cmp(&b.memory_id))
    });
    movers.truncate(GRAPH_DIFF_MOVERS_CAP);

    let communities = diff_communities(&from.edges, &to.edges);

    let summary = DiffSummary {
        nodes_added: nodes_added_full.len(),
        nodes_removed: nodes_removed_full.len(),
        edges_added: edges_added_full.len(),
        edges_removed: edges_removed_full.len(),
        communities_matched: communities.matched.len() + communities.unchanged,
        community_births: communities.births.len(),
        community_deaths: communities.deaths.len(),
        centrality_movers: movers.len(),
        centrality_omitted,
    };
    let truncated = DiffTruncation {
        nodes_added: nodes_added_full.len() > GRAPH_DIFF_DETAIL_CAP,
        nodes_removed: nodes_removed_full.len() > GRAPH_DIFF_DETAIL_CAP,
        edges_added: edges_added_full.len() > GRAPH_DIFF_DETAIL_CAP,
        edges_removed: edges_removed_full.len() > GRAPH_DIFF_DETAIL_CAP,
    };

    let mut nodes_added = nodes_added_full;
    let mut nodes_removed = nodes_removed_full;
    let mut edges_added = edges_added_full;
    let mut edges_removed = edges_removed_full;
    nodes_added.truncate(GRAPH_DIFF_DETAIL_CAP);
    nodes_removed.truncate(GRAPH_DIFF_DETAIL_CAP);
    edges_added.truncate(GRAPH_DIFF_DETAIL_CAP);
    edges_removed.truncate(GRAPH_DIFF_DETAIL_CAP);

    GraphDiffReport {
        schema: GRAPH_DIFF_SCHEMA_V1,
        graph_type: graph_type.to_owned(),
        from: snapshot_ref(from),
        to: snapshot_ref(to),
        summary,
        nodes_added,
        nodes_removed,
        edges_added,
        edges_removed,
        communities,
        movers,
        detail_cap: GRAPH_DIFF_DETAIL_CAP,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{
        COMMUNITY_MATCH_JACCARD_THRESHOLD, DiffEdge, GRAPH_DIFF_DETAIL_CAP, SnapshotGraphView,
        diff_communities, diff_snapshots, edge_key, parse_snapshot_graph, snapshot_at_or_before,
    };
    use std::collections::BTreeMap;

    fn view(
        id: &str,
        version: u32,
        nodes: &[(&str, Option<f64>)],
        edges: &[(&str, &str)],
    ) -> SnapshotGraphView {
        let mut node_map = BTreeMap::new();
        for (node, rank) in nodes {
            node_map.insert((*node).to_owned(), *rank);
        }
        let mut edge_map = BTreeMap::new();
        for (source, target) in edges {
            let key = edge_key(source, target, "related", false);
            edge_map.insert(
                key.clone(),
                DiffEdge {
                    key,
                    source: (*source).to_owned(),
                    target: (*target).to_owned(),
                    relation: "related".to_owned(),
                    directed: false,
                },
            );
        }
        SnapshotGraphView {
            snapshot_id: id.to_owned(),
            snapshot_version: version,
            created_at: format!("2026-08-0{version}T00:00:00Z"),
            nodes: node_map,
            edges: edge_map,
        }
    }

    #[test]
    fn edge_key_is_orientation_canonical_for_undirected_edges() {
        assert_eq!(
            edge_key("mem_b", "mem_a", "related", false),
            edge_key("mem_a", "mem_b", "related", false)
        );
        assert_ne!(
            edge_key("mem_b", "mem_a", "related", true),
            edge_key("mem_a", "mem_b", "related", true)
        );
        assert_ne!(
            edge_key("mem_a", "mem_b", "related", false),
            edge_key("mem_a", "mem_b", "contradicts", false)
        );
    }

    #[test]
    fn add_remove_sets_are_exact_complements() {
        let a = view(
            "gsnap_a",
            1,
            &[("m1", Some(0.4)), ("m2", Some(0.3)), ("m3", Some(0.3))],
            &[("m1", "m2"), ("m2", "m3")],
        );
        let b = view(
            "gsnap_b",
            2,
            &[("m1", Some(0.5)), ("m2", Some(0.2)), ("m4", Some(0.3))],
            &[("m1", "m2"), ("m1", "m4")],
        );
        let ab = diff_snapshots("memory_links", &a, &b);
        let ba = diff_snapshots("memory_links", &b, &a);
        assert_eq!(ab.nodes_added, vec!["m4".to_owned()]);
        assert_eq!(ab.nodes_removed, vec!["m3".to_owned()]);
        assert_eq!(ab.nodes_added, ba.nodes_removed);
        assert_eq!(ab.nodes_removed, ba.nodes_added);
        assert_eq!(ab.edges_added, ba.edges_removed);
        assert_eq!(ab.edges_removed, ba.edges_added);
        assert_eq!(ab.summary.nodes_added, 1);
        assert_eq!(ab.summary.edges_added, 1);
        assert_eq!(ab.summary.edges_removed, 1);
    }

    #[test]
    fn identical_structure_yields_empty_community_delta() {
        // Two triangles — same structure fed in different insertion orders
        // (label permutation): matching must report zero churn.
        let edges_one = view(
            "gsnap_a",
            1,
            &[],
            &[
                ("m1", "m2"),
                ("m2", "m3"),
                ("m1", "m3"),
                ("m7", "m8"),
                ("m8", "m9"),
                ("m7", "m9"),
            ],
        )
        .edges;
        let edges_two = view(
            "gsnap_b",
            2,
            &[],
            &[
                ("m8", "m9"),
                ("m7", "m9"),
                ("m7", "m8"),
                ("m1", "m3"),
                ("m2", "m3"),
                ("m1", "m2"),
            ],
        )
        .edges;
        let delta = diff_communities(&edges_one, &edges_two);
        assert!(delta.matched.is_empty(), "no churn to report: {delta:?}");
        assert!(delta.births.is_empty(), "no births: {delta:?}");
        assert!(delta.deaths.is_empty(), "no deaths: {delta:?}");
        assert!(delta.unchanged >= 1, "communities matched silently");
    }

    #[test]
    fn community_birth_death_and_membership_churn_are_reported() {
        let from = view(
            "gsnap_a",
            1,
            &[],
            &[("m1", "m2"), ("m2", "m3"), ("m1", "m3")],
        );
        // The triangle keeps m1-m3 and gains m4 (churn ≥ 0.5 Jaccard); a
        // brand-new pair community is born.
        let to = view(
            "gsnap_b",
            2,
            &[],
            &[
                ("m1", "m2"),
                ("m2", "m3"),
                ("m1", "m3"),
                ("m3", "m4"),
                ("m8", "m9"),
            ],
        );
        let delta = diff_communities(&from.edges, &to.edges);
        assert!(
            delta
                .matched
                .iter()
                .any(|matched| matched.joined.contains(&"m4".to_owned())),
            "m4 joins the surviving community: {delta:?}"
        );
        assert!(
            delta
                .births
                .iter()
                .any(|birth| birth.members.contains(&"m8".to_owned())),
            "the m8-m9 community is a birth: {delta:?}"
        );
        assert!(delta.deaths.is_empty(), "nothing died: {delta:?}");
    }

    #[test]
    fn movers_rank_by_abs_delta_with_id_tie_break_and_omit_missing_sides() {
        let from = view(
            "gsnap_a",
            1,
            &[
                ("m1", Some(0.10)),
                ("m2", Some(0.75)),
                ("m3", Some(0.25)),
                ("m4", None),
                ("m5", Some(0.20)),
            ],
            &[],
        );
        let to = view(
            "gsnap_b",
            2,
            &[
                ("m1", Some(0.40)), // |Δ| = 0.30
                ("m2", Some(0.50)), // |Δ| = 0.25 exactly (dyadic — ties in f64)
                ("m3", Some(0.50)), // |Δ| = 0.25 exactly — ties with m2, id breaks
                ("m4", Some(0.50)), // omitted: no persisted from-side value
                ("m5", Some(0.20)), // zero delta: not a mover
            ],
            &[],
        );
        let report = diff_snapshots("memory_links", &from, &to);
        let ids: Vec<&str> = report
            .movers
            .iter()
            .map(|mover| mover.memory_id.as_str())
            .collect();
        assert_eq!(ids, vec!["m1", "m2", "m3"], "rank + tie-break drifted");
        assert_eq!(report.summary.centrality_omitted, 1);
        assert!((report.movers[0].delta - 0.30).abs() < 1e-9);
    }

    #[test]
    fn detail_arrays_truncate_at_the_declared_governor_cap() {
        let mut nodes: Vec<(String, Option<f64>)> = Vec::new();
        for index in 0..(GRAPH_DIFF_DETAIL_CAP + 8) {
            nodes.push((format!("mem_{index:04}"), None));
        }
        let node_refs: Vec<(&str, Option<f64>)> = nodes
            .iter()
            .map(|(id, rank)| (id.as_str(), *rank))
            .collect();
        let from = view("gsnap_a", 1, &[], &[]);
        let to = view("gsnap_b", 2, &node_refs, &[]);
        let report = diff_snapshots("memory_links", &from, &to);
        assert_eq!(report.summary.nodes_added, GRAPH_DIFF_DETAIL_CAP + 8);
        assert_eq!(report.nodes_added.len(), GRAPH_DIFF_DETAIL_CAP);
        assert!(report.truncated.nodes_added);
        assert_eq!(report.detail_cap, GRAPH_DIFF_DETAIL_CAP);
    }

    #[test]
    fn unavailable_pagerank_preserves_topology_without_fabricated_movers() {
        let baseline = serde_json::json!({
            "schema": "ee.graph.snapshot.metrics.v1",
            "graphType": "memory_links",
            "nodes": [{"id": "m1", "pagerank": 0.0}],
            "edges": [],
        });
        let updated = serde_json::json!({
            "schema": "ee.graph.snapshot.metrics.v1",
            "graphType": "memory_links",
            "centrality": {"algorithmStatus": {"pagerank": "computed"}},
            "nodes": [
                {"id": "m1", "pagerank": 0.4, "metricStatus": {"pagerank": "computed"}},
                {"id": "m2", "pagerank": 0.6, "metricStatus": {"pagerank": "computed"}},
            ],
            "edges": [{"source": "m1", "target": "m2", "relation": "supports", "directed": true}],
        });
        let to = parse_snapshot_graph("to", 2, "t", &updated.to_string()).unwrap();
        let with_statuses = |node_status: Option<&str>, snapshot_status: Option<&str>| {
            let mut metrics = baseline.clone();
            if let Some(status) = node_status {
                metrics["nodes"][0]["metricStatus"] = serde_json::json!({"pagerank": status});
            }
            if let Some(status) = snapshot_status {
                metrics["centrality"] =
                    serde_json::json!({"algorithmStatus": {"pagerank": status}});
            }
            metrics
        };

        for unavailable in ["skipped", "timed_out", "cancelled", "failed"] {
            for (node_status, snapshot_status) in [
                (Some(unavailable), None),
                (None, Some(unavailable)),
                (Some(unavailable), Some("computed")),
                (Some("computed"), Some(unavailable)),
            ] {
                let metrics = with_statuses(node_status, snapshot_status);
                let from = parse_snapshot_graph("from", 1, "t", &metrics.to_string()).unwrap();
                assert_eq!(from.nodes.len(), 1);
                assert_eq!(from.nodes["m1"], None);
                let report = diff_snapshots("memory_links", &from, &to);
                assert!(
                    report.movers.is_empty(),
                    "unavailable metric became a mover: {metrics}"
                );
                assert_eq!(report.summary.centrality_omitted, 1);
                assert_eq!(report.nodes_added, vec!["m2".to_owned()]);
                assert_eq!(report.summary.edges_added, 1);
            }
        }

        // An observed zero and legacy values without status remain real measurements.
        for (node_status, snapshot_status) in [
            (None, None),
            (Some("computed"), None),
            (None, Some("computed")),
            (Some("computed"), Some("computed")),
        ] {
            let metrics = with_statuses(node_status, snapshot_status);
            let from = parse_snapshot_graph("from", 1, "t", &metrics.to_string()).unwrap();
            assert_eq!(from.nodes["m1"], Some(0.0));
            let report = diff_snapshots("memory_links", &from, &to);
            assert_eq!(report.summary.centrality_omitted, 0);
            assert_eq!(report.movers.len(), 1);
            assert_eq!(report.movers[0].memory_id, "m1");
            assert_eq!(report.movers[0].delta, 0.4);
        }

        for invalid_status in [
            serde_json::json!("unknown"),
            serde_json::Value::Null,
            serde_json::json!(false),
        ] {
            let mut metrics = baseline.clone();
            metrics["nodes"][0]["metricStatus"] = serde_json::json!({"pagerank": invalid_status});
            assert!(parse_snapshot_graph("invalid", 1, "t", &metrics.to_string()).is_err());
        }
    }

    #[test]
    fn typed_snapshot_edges_preserve_producer_relations_and_directions() {
        for (family, attrs, relation, directed) in [
            (
                "revision_dag",
                serde_json::json!({"transition_kind": "supersedes"}),
                "supersedes",
                true,
            ),
            (
                "rule_provenance",
                serde_json::json!({"rule_role": "supporting"}),
                "supporting",
                false,
            ),
            (
                "contradiction_subgraph",
                serde_json::json!({"relation": "contradicts", "directed": false}),
                "contradicts",
                false,
            ),
            (
                "causal_evidence",
                serde_json::json!({"method": "temporal", "contribution_score": 0.7}),
                "causal_evidence",
                true,
            ),
        ] {
            let metrics = serde_json::json!({
                "schema": "ee.graph.snapshot.metrics.v1",
                "graphType": family,
                "nodes": [{"id": "m1"}, {"id": "m2"}],
                "edges": [{"source": "m1", "target": "m2", "attrs": attrs}],
            });
            let from = parse_snapshot_graph("from", 1, "t", &metrics.to_string()).unwrap();
            let edge = from.edges.values().next().unwrap();
            assert_eq!(edge.relation, relation);
            assert_eq!(edge.directed, directed);
            let mut reversed = metrics;
            reversed["edges"][0]["source"] = serde_json::json!("m2");
            reversed["edges"][0]["target"] = serde_json::json!("m1");
            let to = parse_snapshot_graph("to", 2, "t", &reversed.to_string()).unwrap();
            let diff = diff_snapshots(family, &from, &to);
            assert_eq!(diff.summary.edges_added, if directed { 1 } else { 0 });
            assert_eq!(diff.summary.edges_removed, if directed { 1 } else { 0 });
            if !directed {
                assert_eq!(from.edges, to.edges);
            }
        }
    }

    #[test]
    fn revision_relation_changes_are_real_structural_changes() {
        let metrics = |relation| {
            serde_json::json!({
                "schema": "ee.graph.snapshot.metrics.v1",
                "graphType": "revision_dag",
                "nodes": [{"id": "m1"}, {"id": "m2"}],
                "edges": [{
                    "source": "m1", "target": "m2",
                    "attrs": {"transition_kind": relation}
                }],
            })
            .to_string()
        };
        let from = parse_snapshot_graph("from", 1, "t", &metrics("supersedes")).unwrap();
        let to = parse_snapshot_graph("to", 2, "t", &metrics("derived_from")).unwrap();
        let diff = diff_snapshots("revision_dag", &from, &to);
        assert_eq!(diff.summary.edges_added, 1);
        assert_eq!(diff.summary.edges_removed, 1);
        assert_eq!(diff.edges_added[0].relation, "derived_from");
        assert_eq!(diff.edges_removed[0].relation, "supersedes");
    }

    #[test]
    fn explicit_empty_and_legacy_nested_snapshots_remain_valid() {
        for metrics in [
            serde_json::json!({"nodes": [], "edges": []}),
            serde_json::json!({"graph": {"nodes": [], "edges": []}}),
            serde_json::json!({
                "schema": "ee.graph.snapshot.metrics.v1",
                "graphType": "revision_dag",
                "nodes": [], "edges": []
            }),
        ] {
            let parsed = parse_snapshot_graph("empty", 1, "t", &metrics.to_string()).unwrap();
            assert!(parsed.nodes.is_empty());
            assert!(parsed.edges.is_empty());
        }
        let legacy = serde_json::json!({
            "graph": {
                "nodes": [{"memoryId": "m1"}, {"memoryId": "m2"}],
                "edges": [{"sourceMemoryId": "m2", "targetMemoryId": "m1"}],
            }
        });
        let parsed = parse_snapshot_graph("legacy", 1, "t", &legacy.to_string()).unwrap();
        let edge = parsed.edges.values().next().unwrap();
        assert_eq!(edge.source, "m1");
        assert_eq!(edge.target, "m2");
        assert_eq!(edge.relation, "related");
        assert!(!edge.directed);
    }

    #[test]
    fn malformed_snapshot_rows_never_become_empty_or_partial_success() {
        for metrics in [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!({"nodes": [], "edges": {}}),
            serde_json::json!({"nodes": null, "edges": []}),
            serde_json::json!({
                "nodes": null, "edges": [],
                "graph": {"nodes": [], "edges": []}
            }),
            serde_json::json!({"schema": "unsupported", "nodes": [], "edges": []}),
            serde_json::json!({"nodes": [{}], "edges": []}),
            serde_json::json!({"nodes": [{"id": ""}], "edges": []}),
            serde_json::json!({"nodes": [{"id": "m1", "memoryId": "m2"}], "edges": []}),
            serde_json::json!({"nodes": [{"id": "m1"}, {"id": "m1"}], "edges": []}),
            serde_json::json!({"nodes": [{"id": "m1", "pagerank": "unknown"}], "edges": []}),
            serde_json::json!({"nodes": [{"id": "m1", "pagerank": -1}], "edges": []}),
            serde_json::json!({"nodes": [{"id": "m1"}], "edges": [{}]}),
            serde_json::json!({
                "nodes": [{"id": "m1"}],
                "edges": [{"source": "m1", "target": "missing"}]
            }),
            serde_json::json!({
                "nodes": [{"id": "m1"}],
                "edges": [{"source": "m1", "sourceMemoryId": "m2", "target": "m1"}]
            }),
            serde_json::json!({
                "nodes": [{"id": "m1"}],
                "edges": [{"source": "m1", "target": "m1", "directed": "false"}]
            }),
            serde_json::json!({
                "nodes": [{"id": "m1"}],
                "edges": [{"source": "m1", "target": "m1", "attrs": []}]
            }),
            serde_json::json!({
                "nodes": [{"id": "m1"}],
                "edges": [{
                    "source": "m1", "target": "m1",
                    "relation": "supports", "attrs": {"relation": "contradicts"}
                }]
            }),
            serde_json::json!({
                "nodes": [{"id": "m1"}],
                "edges": [{
                    "source": "m1", "target": "m1",
                    "directed": true, "attrs": {"directed": false}
                }]
            }),
            serde_json::json!({
                "graphType": "revision_dag",
                "nodes": [{"id": "m1"}],
                "edges": [{"source": "m1", "target": "m1", "attrs": {}}]
            }),
        ] {
            assert!(
                parse_snapshot_graph("malformed", 1, "t", &metrics.to_string()).is_err(),
                "malformed topology was accepted: {metrics}"
            );
        }
    }

    #[test]
    fn since_selection_uses_instants_and_preserves_fractional_precision() {
        let history = [
            "2026-08-01T01:00:00+02:00",
            "2026-08-01T00:00:00.123456789Z",
            "2026-08-01T00:00:00Z",
        ];
        let cutoff = |text| {
            chrono::DateTime::parse_from_rfc3339(text)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        assert_eq!(
            snapshot_at_or_before(history, &cutoff("2026-08-01T02:00:00.2+02:00")).unwrap(),
            Some(1)
        );
        assert_eq!(
            snapshot_at_or_before(history, &cutoff("2026-08-01T00:00:00.123456788Z")).unwrap(),
            Some(2)
        );
        assert_eq!(
            snapshot_at_or_before(history, &cutoff("2026-07-31T23:00:00Z")).unwrap(),
            Some(0)
        );
        assert_eq!(
            snapshot_at_or_before(history, &cutoff("2026-07-31T22:59:59Z")).unwrap(),
            None
        );
        assert_eq!(
            snapshot_at_or_before(
                ["2026-08-01T00:00:00Z", "2026-08-01T02:00:00+02:00"],
                &cutoff("2026-08-01T00:00:00Z")
            )
            .unwrap(),
            Some(0)
        );
        assert!(
            snapshot_at_or_before(
                ["2026-08-01T00:00:00Z", "not a timestamp"],
                &cutoff("2026-08-01T00:00:00Z")
            )
            .is_err()
        );
    }

    #[test]
    fn parse_snapshot_graph_reads_persisted_metrics_shape() {
        let metrics = serde_json::json!({
            "schema": "ee.graph.snapshot.metrics.v1",
            "nodes": [
                {"id": "m1", "pagerank": 0.5},
                {"id": "m2", "pagerank": null},
            ],
            "edges": [
                {"source": "m1", "target": "m2", "relation": "supports", "directed": true},
            ],
        })
        .to_string();
        let parsed = parse_snapshot_graph("gsnap_x", 3, "2026-08-09T00:00:00Z", &metrics).unwrap();
        assert_eq!(parsed.nodes.len(), 2);
        assert_eq!(parsed.nodes["m1"], Some(0.5));
        assert_eq!(parsed.nodes["m2"], None);
        assert_eq!(parsed.edges.len(), 1);
        let edge = parsed.edges.values().next().unwrap();
        assert_eq!(edge.relation, "supports");
        assert!(edge.directed);
        assert!(parse_snapshot_graph("gsnap_y", 4, "t", "not json").is_err());
        let _ = COMMUNITY_MATCH_JACCARD_THRESHOLD;
    }
}
