use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::str::FromStr;
use std::sync::{OnceLock, RwLock};

use asupersync::Cx;
use fnx_algorithms::{CentralityScore, ComplexityWitness, PageRankResult};
use fnx_classes::digraph::DiGraph;

use crate::config::env_registry::{self, EnvVar};
use crate::db::DbConnection;
use crate::graph::algorithms::{
    AlgorithmResultCacheRun, AlgorithmResultCacheSpec, DEFAULT_FOREGROUND_BUDGET,
    current_or_testing_cx, run_with_budget, run_with_result_cache,
};
use crate::graph::ppr_prefetch_cache::{
    PprPrefetchCache, PprPrefetchCacheKey, PprPrefetchCacheResultHit,
};
use crate::graph::{
    ComplexityWitnessCounters, GraphResult, emit_complexity_witness, graph_algorithm_params_hash,
};
use crate::models::MemoryId;

pub const DEFAULT_PERSONALIZED_PAGERANK_ALPHA: f64 = 0.85;
pub const DEFAULT_PERSONALIZED_PAGERANK_MAX_ITERATIONS: usize = 50;
pub const DEFAULT_PERSONALIZED_PAGERANK_TOLERANCE: f64 = 1.0e-3;
pub const DEFAULT_PPR_PREFETCH_CACHE_ENTRIES: usize = 4096;

const RELATION_WEIGHT_SUPPORTS: f64 = 1.0;
const RELATION_WEIGHT_DERIVED_FROM: f64 = 0.8;
const RELATION_WEIGHT_RELATED: f64 = 0.6;
const RELATION_WEIGHT_CO_TAG: f64 = 0.4;
const RELATION_WEIGHT_CO_MENTION: f64 = 0.3;
const RELATION_WEIGHT_ZERO: f64 = 0.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PersonalizedPageRankPolicy {
    pub alpha: f64,
    pub max_iterations: usize,
    pub tolerance: f64,
}

impl Default for PersonalizedPageRankPolicy {
    fn default() -> Self {
        Self {
            alpha: DEFAULT_PERSONALIZED_PAGERANK_ALPHA,
            max_iterations: DEFAULT_PERSONALIZED_PAGERANK_MAX_ITERATIONS,
            tolerance: DEFAULT_PERSONALIZED_PAGERANK_TOLERANCE,
        }
    }
}

pub struct PersonalizedPageRankWitnessSpec<'a> {
    pub conn: &'a DbConnection,
    pub workspace_id: &'a str,
    pub snapshot_id: &'a str,
    pub snapshot_version: u64,
    pub params: &'a serde_json::Value,
    pub elapsed_ms: u64,
}

#[must_use]
pub fn personalized_pagerank_cache_params(
    policy: PersonalizedPageRankPolicy,
    seed_map: &BTreeMap<String, f64>,
) -> serde_json::Value {
    serde_json::json!({
        "alpha": policy.alpha,
        "maxIterations": policy.max_iterations,
        "seedCount": seed_map.len(),
        "seedWeightEncoding": "f64.to_bits.hex.v1",
        "seedWeights": personalized_pagerank_cache_seed_signature(seed_map),
        "tolerance": policy.tolerance,
    })
}

#[must_use]
pub fn personalized_pagerank_cache_seed_signature(
    seed_map: &BTreeMap<String, f64>,
) -> BTreeMap<String, String> {
    seed_map
        .iter()
        .map(|(memory_id, weight)| (memory_id.clone(), format!("0x{:016x}", weight.to_bits())))
        .collect()
}

pub fn compute_personalized_pagerank(
    graph: &DiGraph,
    seed_map: &HashMap<MemoryId, f64>,
) -> GraphResult<HashMap<MemoryId, f64>> {
    let cx = current_or_testing_cx();
    compute_personalized_pagerank_with_cx(
        &cx,
        graph,
        seed_map,
        PersonalizedPageRankPolicy::default(),
    )
}

pub fn compute_personalized_pagerank_with_policy(
    graph: &DiGraph,
    seed_map: &HashMap<MemoryId, f64>,
    policy: PersonalizedPageRankPolicy,
) -> GraphResult<HashMap<MemoryId, f64>> {
    let cx = current_or_testing_cx();
    compute_personalized_pagerank_with_cx(&cx, graph, seed_map, policy)
}

pub fn compute_personalized_pagerank_with_cx(
    cx: &Cx,
    graph: &DiGraph,
    seed_map: &HashMap<MemoryId, f64>,
    policy: PersonalizedPageRankPolicy,
) -> GraphResult<HashMap<MemoryId, f64>> {
    let seed_weights = seed_map
        .iter()
        .map(|(memory_id, weight)| (memory_id.to_string(), *weight))
        .collect::<BTreeMap<_, _>>();
    let result = compute_personalized_pagerank_result_with_cx(cx, graph, &seed_weights, policy)?;
    Ok(result
        .scores
        .into_iter()
        .filter_map(|score| {
            MemoryId::from_str(&score.node)
                .ok()
                .map(|memory_id| (memory_id, score.score))
        })
        .collect())
}

pub fn compute_personalized_pagerank_result(
    graph: &DiGraph,
    seed_map: &BTreeMap<String, f64>,
) -> GraphResult<PageRankResult> {
    let cx = current_or_testing_cx();
    compute_personalized_pagerank_result_with_cx(
        &cx,
        graph,
        seed_map,
        PersonalizedPageRankPolicy::default(),
    )
}

pub fn compute_personalized_pagerank_result_with_policy(
    graph: &DiGraph,
    seed_map: &BTreeMap<String, f64>,
    policy: PersonalizedPageRankPolicy,
) -> GraphResult<PageRankResult> {
    let cx = current_or_testing_cx();
    compute_personalized_pagerank_result_with_cx(&cx, graph, seed_map, policy)
}

pub fn compute_personalized_pagerank_result_with_cx(
    cx: &Cx,
    graph: &DiGraph,
    seed_map: &BTreeMap<String, f64>,
    policy: PersonalizedPageRankPolicy,
) -> GraphResult<PageRankResult> {
    let graph = graph.clone();
    let seed_map = seed_map.clone();
    run_with_budget(
        cx,
        "personalized_pagerank",
        DEFAULT_FOREGROUND_BUDGET,
        move || compute_personalized_pagerank_result_unbudgeted(&graph, &seed_map, policy),
    )
}

pub fn compute_personalized_pagerank_result_cached(
    spec: &AlgorithmResultCacheSpec<'_>,
    graph: &DiGraph,
    seed_map: &BTreeMap<String, f64>,
    policy: PersonalizedPageRankPolicy,
) -> GraphResult<AlgorithmResultCacheRun<PageRankResult>> {
    let cx = current_or_testing_cx();
    compute_personalized_pagerank_result_cached_with_cx(&cx, spec, graph, seed_map, policy)
}

pub fn compute_personalized_pagerank_result_cached_with_cx(
    cx: &Cx,
    spec: &AlgorithmResultCacheSpec<'_>,
    graph: &DiGraph,
    seed_map: &BTreeMap<String, f64>,
    policy: PersonalizedPageRankPolicy,
) -> GraphResult<AlgorithmResultCacheRun<PageRankResult>> {
    run_with_result_cache(spec, || {
        compute_personalized_pagerank_result_with_cx(cx, graph, seed_map, policy)
    })
}

pub fn compute_personalized_pagerank_result_cached_with_graph<F>(
    spec: &AlgorithmResultCacheSpec<'_>,
    seed_map: &BTreeMap<String, f64>,
    policy: PersonalizedPageRankPolicy,
    build_graph: F,
) -> GraphResult<AlgorithmResultCacheRun<PageRankResult>>
where
    F: FnOnce() -> GraphResult<DiGraph>,
{
    let cx = current_or_testing_cx();
    let prefetch_key = ppr_prefetch_cache_key(spec)?;
    run_with_result_cache(spec, || {
        if let Some(hit) = load_ppr_prefetch_result(&prefetch_key) {
            return Ok(hit.result);
        }
        let graph = build_graph()?;
        let result = compute_personalized_pagerank_result_with_cx(&cx, &graph, seed_map, policy)?;
        store_ppr_prefetch_result(prefetch_key, &result);
        Ok(result)
    })
}

fn ppr_prefetch_cache_key(spec: &AlgorithmResultCacheSpec<'_>) -> GraphResult<PprPrefetchCacheKey> {
    let seed_set_hash =
        graph_algorithm_params_hash(spec.algorithm, spec.snapshot_content_hash, spec.params)?;
    Ok(PprPrefetchCacheKey::new(
        seed_set_hash,
        ppr_prefetch_snapshot_generation(spec.snapshot_content_hash),
    ))
}

fn ppr_prefetch_snapshot_generation(snapshot_content_hash: &str) -> u64 {
    let digest = blake3::hash(snapshot_content_hash.as_bytes());
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

fn ppr_prefetch_cache() -> &'static RwLock<PprPrefetchCache> {
    static CACHE: OnceLock<RwLock<PprPrefetchCache>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(PprPrefetchCache::new(ppr_prefetch_cache_capacity())))
}

fn ppr_prefetch_cache_capacity() -> usize {
    env_registry::read_or_default(EnvVar::PprCacheEntries)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_PPR_PREFETCH_CACHE_ENTRIES)
}

fn load_ppr_prefetch_result(key: &PprPrefetchCacheKey) -> Option<PprPrefetchCacheResultHit> {
    let mut cache = ppr_prefetch_cache()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let hit = cache.get_result(key);
    tracing::debug!(
        target: "ee::graph",
        surface = "ppr_cache",
        seed_set_hash = %key.seed_set_hash,
        snapshot_generation = key.snapshot_generation,
        cache_hit = hit.is_some(),
        cache_size = cache.len(),
        eviction_count = 0_usize,
        result_hash = hit.as_ref().map(|hit| hit.result_hash.as_str()).unwrap_or(""),
        "personalized PageRank prefetch cache lookup"
    );
    hit
}

fn store_ppr_prefetch_result(key: PprPrefetchCacheKey, result: &PageRankResult) {
    let mut cache = ppr_prefetch_cache()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let insert = cache.insert_result(key.clone(), result.clone());
    tracing::debug!(
        target: "ee::graph",
        surface = "ppr_cache",
        seed_set_hash = %key.seed_set_hash,
        snapshot_generation = key.snapshot_generation,
        cache_hit = false,
        cache_size = cache.len(),
        eviction_count = insert.evicted.len(),
        result_hash = %insert.result_hash,
        "personalized PageRank prefetch cache insert"
    );
}

fn compute_personalized_pagerank_result_unbudgeted(
    graph: &DiGraph,
    seed_map: &BTreeMap<String, f64>,
    policy: PersonalizedPageRankPolicy,
) -> PageRankResult {
    let mut nodes = graph.nodes_ordered();
    nodes.sort_unstable();
    let node_count = nodes.len();
    if node_count == 0 {
        return personalized_pagerank_result(Vec::new(), true, 0, 0, 0);
    }

    let personalization = normalized_seed_weights(graph, seed_map);
    if personalization.is_empty() {
        let scores = nodes
            .iter()
            .map(|node| CentralityScore {
                node: (*node).to_owned(),
                score: 0.0,
            })
            .collect();
        return personalized_pagerank_result(scores, true, 0, 0, 0);
    }

    let node_index = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (*node, index))
        .collect::<BTreeMap<&str, usize>>();
    let outgoing = weighted_outgoing_edges(graph, &nodes, &node_index);
    let alpha = policy.alpha.clamp(0.0, 1.0);
    let teleport_scale = 1.0 - alpha;
    let max_pushes = policy.max_iterations.saturating_mul(node_count).max(1);
    let mut estimates = vec![0.0; node_count];
    let mut residuals = vec![0.0; node_count];
    let mut active = BTreeSet::new();
    for (node, seed_weight) in &personalization {
        if let Some(index) = node_index.get(node.as_str()) {
            residuals[*index] += *seed_weight;
            maybe_activate_node(
                *index,
                residuals[*index],
                outgoing[*index].len(),
                &mut active,
                policy,
            );
        }
    }

    let mut pushes = 0_usize;
    let mut edges_scanned = 0_usize;
    let mut queue_peak = active.len();

    while let Some(source_index) = active.pop_first() {
        if pushes >= max_pushes {
            break;
        }
        let residual = residuals[source_index];
        residuals[source_index] = 0.0;
        if !should_push_residual(residual, outgoing[source_index].len(), policy) {
            continue;
        }

        pushes = pushes.saturating_add(1);
        estimates[source_index] += teleport_scale * residual;
        let push = alpha * residual;
        if push <= 0.0 {
            continue;
        }

        if outgoing[source_index].is_empty() {
            for (node, seed_weight) in &personalization {
                if let Some(target_index) = node_index.get(node.as_str()) {
                    residuals[*target_index] += push * seed_weight;
                    maybe_activate_node(
                        *target_index,
                        residuals[*target_index],
                        outgoing[*target_index].len(),
                        &mut active,
                        policy,
                    );
                }
            }
        } else {
            edges_scanned = edges_scanned.saturating_add(outgoing[source_index].len());
            for (target_index, weight_share) in &outgoing[source_index] {
                residuals[*target_index] += push * weight_share;
                maybe_activate_node(
                    *target_index,
                    residuals[*target_index],
                    outgoing[*target_index].len(),
                    &mut active,
                    policy,
                );
            }
        }
        queue_peak = queue_peak.max(active.len());
    }

    let converged = active.is_empty();

    let scores = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| CentralityScore {
            node: (*node).to_owned(),
            score: estimates[index],
        })
        .collect();
    personalized_pagerank_result(scores, converged, pushes, edges_scanned, queue_peak)
}

fn should_push_residual(
    residual: f64,
    outgoing_edge_count: usize,
    policy: PersonalizedPageRankPolicy,
) -> bool {
    let degree_scale = outgoing_edge_count.max(1) as f64;
    residual > policy.tolerance.max(0.0) * degree_scale
}

fn maybe_activate_node(
    index: usize,
    residual: f64,
    outgoing_edge_count: usize,
    active: &mut BTreeSet<usize>,
    policy: PersonalizedPageRankPolicy,
) {
    if should_push_residual(residual, outgoing_edge_count, policy) {
        active.insert(index);
    }
}

pub fn emit_personalized_pagerank_witness(
    spec: &PersonalizedPageRankWitnessSpec<'_>,
    result: &PageRankResult,
) -> GraphResult<()> {
    let counters = ComplexityWitnessCounters::strict_with_fnx_counters(
        spec.elapsed_ms,
        "exact",
        personalized_pagerank_decision_path_hash(spec.params, result),
        result.witness.nodes_touched,
        result.witness.edges_scanned,
        result.witness.queue_peak,
    );
    emit_complexity_witness(
        spec.conn,
        spec.workspace_id,
        spec.snapshot_id,
        "personalized_pagerank",
        spec.snapshot_version,
        spec.params,
        &counters,
    )
}

fn weighted_outgoing_edges(
    graph: &DiGraph,
    nodes: &[&str],
    node_index: &BTreeMap<&str, usize>,
) -> Vec<Vec<(usize, f64)>> {
    nodes
        .iter()
        .map(|source| {
            let mut edges = graph
                .neighbors_iter(source)
                .map(|neighbors| {
                    neighbors
                        .filter_map(|target| {
                            let target_index = node_index.get(target).copied()?;
                            let weight = edge_weight(graph, source, target);
                            (weight > 0.0).then_some((target_index, weight))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            edges.sort_unstable_by_key(|(target_index, _)| *target_index);
            let total = edges.iter().map(|(_, weight)| *weight).sum::<f64>();
            if total > 0.0 {
                for (_, weight) in &mut edges {
                    *weight /= total;
                }
            }
            edges
        })
        .collect()
}

fn normalized_seed_weights(
    graph: &DiGraph,
    seed_map: &BTreeMap<String, f64>,
) -> BTreeMap<String, f64> {
    let mut seeds = seed_map
        .iter()
        .filter_map(|(node, weight)| {
            (graph.has_node(node) && weight.is_finite() && *weight > 0.0)
                .then_some((node.clone(), *weight))
        })
        .collect::<BTreeMap<_, _>>();
    let total = seeds.values().sum::<f64>();
    if total <= 0.0 {
        return BTreeMap::new();
    }
    for weight in seeds.values_mut() {
        *weight /= total;
    }
    seeds
}

fn edge_weight(graph: &DiGraph, source: &str, target: &str) -> f64 {
    let Some(attrs) = graph.edge_attrs(source, target) else {
        return 0.0;
    };
    let relation = attrs
        .get("relation")
        .map(fnx_runtime::CgseValue::as_str)
        .unwrap_or_else(|| "related".to_owned());
    let stored_weight = attrs
        .get("weight")
        .and_then(fnx_runtime::CgseValue::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(1.0);
    let confidence = attrs
        .get("confidence")
        .and_then(fnx_runtime::CgseValue::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(1.0);
    relation_weight(&relation) * stored_weight * confidence
}

fn relation_weight(relation: &str) -> f64 {
    match relation {
        "supports" => RELATION_WEIGHT_SUPPORTS,
        "derived_from" => RELATION_WEIGHT_DERIVED_FROM,
        "related" => RELATION_WEIGHT_RELATED,
        "co_tag" => RELATION_WEIGHT_CO_TAG,
        "co_mention" => RELATION_WEIGHT_CO_MENTION,
        "contradicts" | "supersedes" => RELATION_WEIGHT_ZERO,
        _ => RELATION_WEIGHT_RELATED,
    }
}

fn personalized_pagerank_result(
    scores: Vec<CentralityScore>,
    converged: bool,
    nodes_touched: usize,
    edges_scanned: usize,
    queue_peak: usize,
) -> PageRankResult {
    PageRankResult {
        scores,
        converged,
        witness: ComplexityWitness {
            algorithm: "personalized_pagerank_acl_push".to_owned(),
            complexity_claim: "O(1/epsilon * 1/(1-alpha)) local push".to_owned(),
            nodes_touched,
            edges_scanned,
            queue_peak,
        },
    }
}

fn personalized_pagerank_decision_path_hash(
    params: &serde_json::Value,
    result: &PageRankResult,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.graph.personalized_pagerank.decision.v1");
    hash_json_value(&mut hasher, params);
    update_hash_with_len_prefixed_str(&mut hasher, &result.witness.algorithm);
    update_hash_with_len_prefixed_str(&mut hasher, &result.witness.complexity_claim);
    hasher.update(&result.witness.nodes_touched.to_le_bytes());
    hasher.update(&result.witness.edges_scanned.to_le_bytes());
    hasher.update(&result.witness.queue_peak.to_le_bytes());
    hasher.update(&[u8::from(result.converged)]);
    let mut scores = result.scores.clone();
    scores.sort_unstable_by(|left, right| left.node.cmp(&right.node));
    hasher.update(&(scores.len() as u64).to_le_bytes());
    for score in scores {
        update_hash_with_len_prefixed_str(&mut hasher, &score.node);
        hasher.update(&score.score.to_le_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn update_hash_with_len_prefixed_str(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_json_value(hasher: &mut blake3::Hasher, value: &serde_json::Value) {
    match value {
        serde_json::Value::Null => {
            hasher.update(b"n");
        }
        serde_json::Value::Bool(value) => {
            hasher.update(if *value { b"t" } else { b"f" });
        }
        serde_json::Value::Number(value) => {
            hasher.update(b"#");
            hasher.update(value.to_string().as_bytes());
        }
        serde_json::Value::String(value) => {
            hasher.update(b"s");
            update_hash_with_len_prefixed_str(hasher, value);
        }
        serde_json::Value::Array(items) => {
            hasher.update(b"[");
            hasher.update(&(items.len() as u64).to_le_bytes());
            for item in items {
                hash_json_value(hasher, item);
            }
            hasher.update(b"]");
        }
        serde_json::Value::Object(fields) => {
            hasher.update(b"{");
            hasher.update(&(fields.len() as u64).to_le_bytes());
            let mut keys = fields.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                update_hash_with_len_prefixed_str(hasher, key);
                if let Some(value) = fields.get(key) {
                    hash_json_value(hasher, value);
                }
            }
            hasher.update(b"}");
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateGraphSnapshotInput, CreateWorkspaceInput, GraphSnapshotType};
    use fnx_classes::AttrMap;
    use fnx_runtime::CgseValue;
    use uuid::Uuid;

    type TestResult<T = ()> = Result<T, String>;
    const WORKSPACE_ID: &str = "wsp_01234567890123456789012345";

    fn graph_result<T>(result: GraphResult<T>) -> TestResult<T> {
        result.map_err(|error| error.to_string())
    }

    fn memory_id(seed: u128) -> MemoryId {
        MemoryId::from_uuid(Uuid::from_u128(seed))
    }

    fn edge_attrs(relation: &str, weight: f64, confidence: f64) -> AttrMap {
        let mut attrs = AttrMap::new();
        attrs.insert(
            "relation".to_owned(),
            CgseValue::String(relation.to_owned()),
        );
        attrs.insert("weight".to_owned(), CgseValue::Float(weight));
        attrs.insert("confidence".to_owned(), CgseValue::Float(confidence));
        attrs
    }

    fn assert_pagerank_results_equivalent(left: &PageRankResult, right: &PageRankResult) {
        assert_eq!(left.converged, right.converged);
        assert_eq!(left.witness, right.witness);
        assert_eq!(left.scores.len(), right.scores.len());
        for (left_score, right_score) in left.scores.iter().zip(&right.scores) {
            assert_eq!(left_score.node, right_score.node);
            assert!((left_score.score - right_score.score).abs() < 1.0e-12);
        }
    }

    #[test]
    fn personalized_pagerank_single_seed_walks_outward() -> TestResult {
        let mut graph = DiGraph::strict();
        let a = memory_id(1);
        let b = memory_id(2);
        let c = memory_id(3);
        graph
            .add_edge_with_attrs(
                a.to_string(),
                b.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        graph
            .add_edge_with_attrs(
                b.to_string(),
                c.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        let seeds = HashMap::from([(a, 1.0)]);

        let result = graph_result(compute_personalized_pagerank(&graph, &seeds))?;

        assert_eq!(result.len(), 3);
        assert!(result.get(&a).copied().unwrap_or(0.0) > 0.0);
        assert!(result.get(&b).copied().unwrap_or(0.0) > 0.0);
        assert!(result.get(&c).copied().unwrap_or(0.0) > 0.0);
        Ok(())
    }

    #[test]
    fn personalized_pagerank_normalizes_multi_seed_weights() -> TestResult {
        let mut graph = DiGraph::strict();
        let a = memory_id(11);
        let b = memory_id(12);
        let c = memory_id(13);
        graph
            .add_edge_with_attrs(
                a.to_string(),
                b.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        graph
            .add_edge_with_attrs(
                c.to_string(),
                b.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        let seeds = HashMap::from([(a, 3.0), (c, 1.0)]);

        let result = graph_result(compute_personalized_pagerank_with_policy(
            &graph,
            &seeds,
            PersonalizedPageRankPolicy {
                alpha: 0.0,
                ..PersonalizedPageRankPolicy::default()
            },
        ))?;

        assert!((result.get(&a).copied().unwrap_or(0.0) - 0.75).abs() < 1.0e-12);
        assert!((result.get(&c).copied().unwrap_or(0.0) - 0.25).abs() < 1.0e-12);
        assert!((result.get(&b).copied().unwrap_or(0.0)).abs() < 1.0e-12);
        Ok(())
    }

    #[test]
    fn personalized_pagerank_uses_relation_weighted_edges() -> TestResult {
        let mut graph = DiGraph::strict();
        let seed = memory_id(21);
        let strong = memory_id(22);
        let weak = memory_id(23);
        let zero = memory_id(24);
        graph
            .add_edge_with_attrs(
                seed.to_string(),
                strong.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        graph
            .add_edge_with_attrs(
                seed.to_string(),
                weak.to_string(),
                edge_attrs("co_mention", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        graph
            .add_edge_with_attrs(
                seed.to_string(),
                zero.to_string(),
                edge_attrs("contradicts", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        let seeds = HashMap::from([(seed, 1.0)]);

        let result = graph_result(compute_personalized_pagerank(&graph, &seeds))?;

        assert!(
            result.get(&strong).copied().unwrap_or(0.0) > result.get(&weak).copied().unwrap_or(0.0)
        );
        assert!((result.get(&zero).copied().unwrap_or(0.0)).abs() < 1.0e-12);
        Ok(())
    }

    #[test]
    fn personalized_pagerank_is_deterministic_across_runs() -> TestResult {
        let mut graph = DiGraph::strict();
        let a = memory_id(31);
        let b = memory_id(32);
        let c = memory_id(33);
        graph
            .add_edge_with_attrs(
                a.to_string(),
                b.to_string(),
                edge_attrs("supports", 0.8, 0.9),
            )
            .map_err(|error| error.to_string())?;
        graph
            .add_edge_with_attrs(
                b.to_string(),
                c.to_string(),
                edge_attrs("derived_from", 0.6, 0.7),
            )
            .map_err(|error| error.to_string())?;
        graph
            .add_edge_with_attrs(
                c.to_string(),
                a.to_string(),
                edge_attrs("related", 0.4, 0.5),
            )
            .map_err(|error| error.to_string())?;
        let seeds = HashMap::from([(a, 2.0), (b, 1.0)]);

        let first = graph_result(compute_personalized_pagerank(&graph, &seeds))?;
        let second = graph_result(compute_personalized_pagerank(&graph, &seeds))?;
        let third = graph_result(compute_personalized_pagerank(&graph, &seeds))?;
        let raw_seeds = seeds
            .iter()
            .map(|(memory_id, weight)| (memory_id.to_string(), *weight))
            .collect::<BTreeMap<_, _>>();
        let raw = graph_result(compute_personalized_pagerank_result(&graph, &raw_seeds))?;

        assert_eq!(first, second);
        assert_eq!(second, third);
        assert_eq!(raw.witness.algorithm, "personalized_pagerank_acl_push");
        Ok(())
    }

    #[test]
    fn personalized_pagerank_uses_local_acl_frontier() -> TestResult {
        let mut graph = DiGraph::strict();
        let ids = (100..200).map(memory_id).collect::<Vec<_>>();
        for pair in ids.windows(2) {
            graph
                .add_edge_with_attrs(
                    pair[0].to_string(),
                    pair[1].to_string(),
                    edge_attrs("supports", 1.0, 1.0),
                )
                .map_err(|error| error.to_string())?;
        }
        let seeds = BTreeMap::from([(ids[0].to_string(), 1.0)]);

        let result = graph_result(compute_personalized_pagerank_result(&graph, &seeds))?;

        assert_eq!(result.witness.algorithm, "personalized_pagerank_acl_push");
        assert!(
            result.witness.nodes_touched < ids.len(),
            "local push should not scan the full chain: {:?}",
            result.witness
        );
        assert!(
            result.witness.edges_scanned < ids.len(),
            "local push should not scan every chain edge: {:?}",
            result.witness
        );
        assert_eq!(result.witness.queue_peak, 1);
        assert!(
            result
                .scores
                .iter()
                .find(|score| score.node == ids[0].to_string())
                .is_some_and(|score| score.score > 0.0)
        );
        assert!(
            result
                .scores
                .iter()
                .find(|score| score.node == ids[99].to_string())
                .is_some_and(|score| score.score == 0.0)
        );
        Ok(())
    }

    #[test]
    fn personalized_pagerank_witness_emits_deterministic_decision_hash() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let snapshot_id = "gsnap_0000000000000000000000322";
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-ppr-witness".to_owned(),
                    name: Some("ppr witness".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                snapshot_id,
                &CreateGraphSnapshotInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    snapshot_version: 9,
                    schema_version: "ee.graph.snapshot.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 2,
                    edge_count: 1,
                    metrics_json: r#"{"nodes":[],"edges":[]}"#.to_owned(),
                    content_hash: "blake3:ppr-witness".to_owned(),
                    source_generation: 9,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let mut graph = DiGraph::strict();
        let a = memory_id(41);
        let b = memory_id(42);
        graph
            .add_edge_with_attrs(
                a.to_string(),
                b.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        let seeds = BTreeMap::from([(a.to_string(), 1.0)]);
        let result = graph_result(compute_personalized_pagerank_result(&graph, &seeds))?;
        let params = serde_json::json!({
            "alpha": DEFAULT_PERSONALIZED_PAGERANK_ALPHA,
            "seedCount": 1
        });

        emit_personalized_pagerank_witness(
            &PersonalizedPageRankWitnessSpec {
                conn: &connection,
                workspace_id: WORKSPACE_ID,
                snapshot_id,
                snapshot_version: 9,
                params: &params,
                elapsed_ms: 17,
            },
            &result,
        )
        .map_err(|error| error.to_string())?;

        let rows = connection
            .list_graph_algorithm_witnesses(
                WORKSPACE_ID,
                snapshot_id,
                Some("personalized_pagerank"),
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(rows.len(), 1);
        let witness: serde_json::Value =
            serde_json::from_str(&rows[0].witness_json).map_err(|error| error.to_string())?;
        assert_eq!(witness["elapsed_ms"], 17);
        assert_eq!(witness["sampling_choice"], "exact");
        assert_eq!(witness["snapshot_id"], snapshot_id);
        assert_eq!(witness["snapshot_version"], 9);
        assert_eq!(witness["snapshot_content_hash"], "blake3:ppr-witness");
        assert_eq!(witness["params"], params);
        assert_eq!(witness["compatibility_mode"], "strict");
        assert!(
            witness["decision_path_hash"]
                .as_str()
                .is_some_and(|value| value.starts_with("blake3:"))
        );
        assert_eq!(
            witness["observed_counters"]["nodes_touched"],
            result.witness.nodes_touched
        );
        assert_eq!(
            witness["observed_counters"]["edges_scanned"],
            result.witness.edges_scanned
        );
        assert_eq!(
            witness["observed_counters"]["queue_peak"],
            result.witness.queue_peak
        );

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn personalized_pagerank_decision_hash_length_prefixes_witness_strings() {
        let params = serde_json::json!({"alpha": 0.85, "seedCount": 1});
        let left = PageRankResult {
            scores: vec![CentralityScore {
                node: "mem-a".to_owned(),
                score: 1.0,
            }],
            converged: true,
            witness: ComplexityWitness {
                algorithm: "ab".to_owned(),
                complexity_claim: "c".to_owned(),
                nodes_touched: 1,
                edges_scanned: 0,
                queue_peak: 1,
            },
        };
        let right = PageRankResult {
            witness: ComplexityWitness {
                algorithm: "a".to_owned(),
                complexity_claim: "bc".to_owned(),
                ..left.witness.clone()
            },
            ..left.clone()
        };

        assert_ne!(
            personalized_pagerank_decision_path_hash(&params, &left),
            personalized_pagerank_decision_path_hash(&params, &right),
            "decision hash must not let adjacent witness strings collide by concatenation"
        );
    }

    #[test]
    fn personalized_pagerank_cache_params_are_seed_order_stable() -> TestResult {
        let policy = PersonalizedPageRankPolicy::default();
        let a = memory_id(43);
        let b = memory_id(44);
        let first = BTreeMap::from([(a.to_string(), 0.75), (b.to_string(), 0.25)]);
        let second = BTreeMap::from([(b.to_string(), 0.25), (a.to_string(), 0.75)]);

        let first_params = personalized_pagerank_cache_params(policy, &first);
        let second_params = personalized_pagerank_cache_params(policy, &second);
        let first_hash = crate::graph::graph_algorithm_params_hash(
            "personalized_pagerank",
            "blake3:ppr-cache-seed-order",
            &first_params,
        )
        .map_err(|error| error.to_string())?;
        let second_hash = crate::graph::graph_algorithm_params_hash(
            "personalized_pagerank",
            "blake3:ppr-cache-seed-order",
            &second_params,
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(first_params, second_params);
        assert_eq!(first_hash, second_hash);
        assert_eq!(first_params["seedWeightEncoding"], "f64.to_bits.hex.v1");
        assert_eq!(first_params["seedCount"], 2);
        Ok(())
    }

    #[test]
    fn personalized_pagerank_cache_params_include_seed_identity_and_weight() -> TestResult {
        let policy = PersonalizedPageRankPolicy::default();
        let a = memory_id(45);
        let b = memory_id(46);
        let seed_a = BTreeMap::from([(a.to_string(), 1.0)]);
        let seed_b = BTreeMap::from([(b.to_string(), 1.0)]);
        let seed_a_half = BTreeMap::from([(a.to_string(), 0.5)]);
        let params_a = personalized_pagerank_cache_params(policy, &seed_a);
        let params_b = personalized_pagerank_cache_params(policy, &seed_b);
        let params_a_half = personalized_pagerank_cache_params(policy, &seed_a_half);
        let snapshot_hash = "blake3:ppr-cache-seed-identity";

        let hash_a = crate::graph::graph_algorithm_params_hash(
            "personalized_pagerank",
            snapshot_hash,
            &params_a,
        )
        .map_err(|error| error.to_string())?;
        let hash_b = crate::graph::graph_algorithm_params_hash(
            "personalized_pagerank",
            snapshot_hash,
            &params_b,
        )
        .map_err(|error| error.to_string())?;
        let hash_a_half = crate::graph::graph_algorithm_params_hash(
            "personalized_pagerank",
            snapshot_hash,
            &params_a_half,
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(params_a["seedCount"], params_b["seedCount"]);
        assert_ne!(params_a["seedWeights"], params_b["seedWeights"]);
        assert_ne!(hash_a, hash_b);
        assert_ne!(hash_a, hash_a_half);
        let a_key = a.to_string();
        assert_eq!(
            params_a["seedWeights"][a_key.as_str()],
            "0x3ff0000000000000"
        );
        assert_eq!(
            params_a_half["seedWeights"][a_key.as_str()],
            "0x3fe0000000000000"
        );
        Ok(())
    }

    #[test]
    fn personalized_pagerank_cached_result_reuses_snapshot_result() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let snapshot_id = "gsnap_0000000000000000000000422";
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-ppr-cache".to_owned(),
                    name: Some("ppr cache".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                snapshot_id,
                &CreateGraphSnapshotInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    snapshot_version: 10,
                    schema_version: "ee.graph.snapshot.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 2,
                    edge_count: 1,
                    metrics_json: r#"{"nodes":[],"edges":[]}"#.to_owned(),
                    content_hash: "blake3:ppr-cache".to_owned(),
                    source_generation: 10,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let mut graph = DiGraph::strict();
        let a = memory_id(51);
        let b = memory_id(52);
        graph
            .add_edge_with_attrs(
                a.to_string(),
                b.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        let seeds = BTreeMap::from([(a.to_string(), 1.0)]);
        let params =
            personalized_pagerank_cache_params(PersonalizedPageRankPolicy::default(), &seeds);
        let spec = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id: WORKSPACE_ID,
            snapshot_id,
            snapshot_content_hash: "blake3:ppr-cache",
            algorithm: "personalized_pagerank",
            params: &params,
            ttl_seconds: 300,
        };

        let first = graph_result(compute_personalized_pagerank_result_cached(
            &spec,
            &graph,
            &seeds,
            PersonalizedPageRankPolicy::default(),
        ))?;

        let empty_graph = DiGraph::strict();
        let second = graph_result(compute_personalized_pagerank_result_cached(
            &spec,
            &empty_graph,
            &seeds,
            PersonalizedPageRankPolicy::default(),
        ))?;

        assert!(!first.cache_hit);
        assert!(second.cache_hit);
        assert_eq!(first.params_hash, second.params_hash);
        assert_pagerank_results_equivalent(&first.result, &second.result);

        let rows = connection
            .list_graph_algorithm_results(WORKSPACE_ID, snapshot_id, Some("personalized_pagerank"))
            .map_err(|error| error.to_string())?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].params_hash, first.params_hash);

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn personalized_pagerank_cached_result_separates_same_count_seed_sets() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let snapshot_id = "gsnap_0000000000000000000000472";
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-ppr-cache-seed-sets".to_owned(),
                    name: Some("ppr cache seed sets".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                snapshot_id,
                &CreateGraphSnapshotInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    snapshot_version: 12,
                    schema_version: "ee.graph.snapshot.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 3,
                    edge_count: 2,
                    metrics_json: r#"{"nodes":[],"edges":[]}"#.to_owned(),
                    content_hash: "blake3:ppr-cache-seed-sets".to_owned(),
                    source_generation: 12,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let mut graph = DiGraph::strict();
        let a = memory_id(71);
        let b = memory_id(72);
        let c = memory_id(73);
        graph
            .add_edge_with_attrs(
                a.to_string(),
                c.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        graph
            .add_edge_with_attrs(
                b.to_string(),
                c.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        let seed_a = BTreeMap::from([(a.to_string(), 1.0)]);
        let seed_b = BTreeMap::from([(b.to_string(), 1.0)]);
        let params_a =
            personalized_pagerank_cache_params(PersonalizedPageRankPolicy::default(), &seed_a);
        let params_b =
            personalized_pagerank_cache_params(PersonalizedPageRankPolicy::default(), &seed_b);
        let spec_a = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id: WORKSPACE_ID,
            snapshot_id,
            snapshot_content_hash: "blake3:ppr-cache-seed-sets",
            algorithm: "personalized_pagerank",
            params: &params_a,
            ttl_seconds: 300,
        };
        let spec_b = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id: WORKSPACE_ID,
            snapshot_id,
            snapshot_content_hash: "blake3:ppr-cache-seed-sets",
            algorithm: "personalized_pagerank",
            params: &params_b,
            ttl_seconds: 300,
        };

        let first = graph_result(compute_personalized_pagerank_result_cached(
            &spec_a,
            &graph,
            &seed_a,
            PersonalizedPageRankPolicy::default(),
        ))?;
        let second = graph_result(compute_personalized_pagerank_result_cached(
            &spec_b,
            &graph,
            &seed_b,
            PersonalizedPageRankPolicy::default(),
        ))?;
        let cache_hit = graph_result(compute_personalized_pagerank_result_cached(
            &spec_a,
            &DiGraph::strict(),
            &seed_a,
            PersonalizedPageRankPolicy::default(),
        ))?;

        assert!(!first.cache_hit);
        assert!(!second.cache_hit);
        assert!(cache_hit.cache_hit);
        assert_ne!(first.params_hash, second.params_hash);
        assert_pagerank_results_equivalent(&first.result, &cache_hit.result);
        assert_ne!(
            first
                .result
                .scores
                .iter()
                .find(|score| score.node == a.to_string())
                .map(|score| score.score),
            second
                .result
                .scores
                .iter()
                .find(|score| score.node == a.to_string())
                .map(|score| score.score)
        );
        let rows = connection
            .list_graph_algorithm_results(WORKSPACE_ID, snapshot_id, Some("personalized_pagerank"))
            .map_err(|error| error.to_string())?;
        assert_eq!(rows.len(), 2);

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn personalized_pagerank_cached_with_graph_skips_projection_on_cache_hit() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let snapshot_id = "gsnap_0000000000000000000000522";
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-ppr-cache-lazy-graph".to_owned(),
                    name: Some("ppr cache lazy graph".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                snapshot_id,
                &CreateGraphSnapshotInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    snapshot_version: 11,
                    schema_version: "ee.graph.snapshot.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 2,
                    edge_count: 1,
                    metrics_json: r#"{"nodes":[],"edges":[]}"#.to_owned(),
                    content_hash: "blake3:ppr-cache-lazy-graph".to_owned(),
                    source_generation: 11,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let mut graph = DiGraph::strict();
        let a = memory_id(61);
        let b = memory_id(62);
        graph
            .add_edge_with_attrs(
                a.to_string(),
                b.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        let seeds = BTreeMap::from([(a.to_string(), 1.0)]);
        let params =
            personalized_pagerank_cache_params(PersonalizedPageRankPolicy::default(), &seeds);
        let spec = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id: WORKSPACE_ID,
            snapshot_id,
            snapshot_content_hash: "blake3:ppr-cache-lazy-graph",
            algorithm: "personalized_pagerank",
            params: &params,
            ttl_seconds: 300,
        };
        let build_count = std::cell::Cell::new(0_usize);

        let first = graph_result(compute_personalized_pagerank_result_cached_with_graph(
            &spec,
            &seeds,
            PersonalizedPageRankPolicy::default(),
            || {
                build_count.set(build_count.get() + 1);
                Ok(graph.clone())
            },
        ))?;
        let second = graph_result(compute_personalized_pagerank_result_cached_with_graph(
            &spec,
            &seeds,
            PersonalizedPageRankPolicy::default(),
            || {
                build_count.set(build_count.get() + 1);
                Ok(DiGraph::strict())
            },
        ))?;

        assert!(!first.cache_hit);
        assert!(second.cache_hit);
        assert_eq!(build_count.get(), 1);
        assert_pagerank_results_equivalent(&first.result, &second.result);

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn personalized_pagerank_prefetch_skips_projection_across_snapshot_cache_rows() -> TestResult {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let snapshot_a = "gsnap_0000000000000000000000523";
        let snapshot_b = "gsnap_0000000000000000000000524";
        let content_hash = "blake3:ppr-prefetch-lazy-graph";
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-ppr-prefetch-lazy-graph".to_owned(),
                    name: Some("ppr prefetch lazy graph".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        for snapshot_id in [snapshot_a, snapshot_b] {
            connection
                .insert_graph_snapshot(
                    snapshot_id,
                    &CreateGraphSnapshotInput {
                        workspace_id: WORKSPACE_ID.to_owned(),
                        snapshot_version: 14,
                        schema_version: "ee.graph.snapshot.v1".to_owned(),
                        graph_type: GraphSnapshotType::MemoryLinks,
                        node_count: 2,
                        edge_count: 1,
                        metrics_json: r#"{"nodes":[],"edges":[]}"#.to_owned(),
                        content_hash: content_hash.to_owned(),
                        source_generation: 14,
                        expires_at: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        let mut graph = DiGraph::strict();
        let a = memory_id(81);
        let b = memory_id(82);
        graph
            .add_edge_with_attrs(
                a.to_string(),
                b.to_string(),
                edge_attrs("supports", 1.0, 1.0),
            )
            .map_err(|error| error.to_string())?;
        let seeds = BTreeMap::from([(a.to_string(), 1.0)]);
        let params =
            personalized_pagerank_cache_params(PersonalizedPageRankPolicy::default(), &seeds);
        let spec_a = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id: WORKSPACE_ID,
            snapshot_id: snapshot_a,
            snapshot_content_hash: content_hash,
            algorithm: "personalized_pagerank",
            params: &params,
            ttl_seconds: 300,
        };
        let spec_b = AlgorithmResultCacheSpec {
            conn: &connection,
            workspace_id: WORKSPACE_ID,
            snapshot_id: snapshot_b,
            snapshot_content_hash: content_hash,
            algorithm: "personalized_pagerank",
            params: &params,
            ttl_seconds: 300,
        };
        let build_count = std::cell::Cell::new(0_usize);

        let first = graph_result(compute_personalized_pagerank_result_cached_with_graph(
            &spec_a,
            &seeds,
            PersonalizedPageRankPolicy::default(),
            || {
                build_count.set(build_count.get() + 1);
                Ok(graph.clone())
            },
        ))?;
        let second = graph_result(compute_personalized_pagerank_result_cached_with_graph(
            &spec_b,
            &seeds,
            PersonalizedPageRankPolicy::default(),
            || {
                build_count.set(build_count.get() + 1);
                Ok(DiGraph::strict())
            },
        ))?;

        assert!(!first.cache_hit);
        assert!(!second.cache_hit);
        assert_eq!(build_count.get(), 1);
        assert_pagerank_results_equivalent(&first.result, &second.result);

        connection.close().map_err(|error| error.to_string())
    }
}
