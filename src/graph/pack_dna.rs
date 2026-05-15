use std::collections::{BTreeMap, BTreeSet, HashMap};

use asupersync::Cx;
use fnx_classes::{Graph, digraph::DiGraph};
use serde::Serialize;

use crate::graph::algorithms::{DEFAULT_FOREGROUND_BUDGET, current_or_testing_cx, run_with_budget};
use crate::graph::health::detect_louvain_communities;
use crate::graph::{GraphError, GraphResult, MemoryGraphProjection};
use crate::models::MemoryId;
use crate::models::degradation::GRAPH_PACK_DNA_NO_DOMINATOR_CODE;

pub const PACK_DNA_SCHEMA_V1: &str = "ee.context.pack_dna.v1";
pub const DEFAULT_PACK_DNA_EGO_RADIUS: usize = 2;
pub const DEFAULT_PACK_DNA_PPR_NEIGHBOR_LIMIT: usize = 10;

#[derive(Clone, Debug, PartialEq)]
pub struct PackDnaInput {
    pub pack_memory_ids: Vec<MemoryId>,
    pub query_seed_weights: BTreeMap<MemoryId, f64>,
    pub trust_anchor_memory_ids: Vec<MemoryId>,
    pub ego_radius: usize,
    pub ppr_neighbor_limit: usize,
}

impl PackDnaInput {
    #[must_use]
    pub fn new(
        pack_memory_ids: Vec<MemoryId>,
        query_seed_memory_ids: Vec<MemoryId>,
        trust_anchor_memory_ids: Vec<MemoryId>,
    ) -> Self {
        Self {
            pack_memory_ids,
            query_seed_weights: query_seed_memory_ids
                .into_iter()
                .map(|memory_id| (memory_id, 1.0))
                .collect(),
            trust_anchor_memory_ids,
            ego_radius: DEFAULT_PACK_DNA_EGO_RADIUS,
            ppr_neighbor_limit: DEFAULT_PACK_DNA_PPR_NEIGHBOR_LIMIT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackDna {
    pub schema: &'static str,
    pub snapshot_version: u64,
    #[serde(skip_serializing)]
    pub pack_memory_count: usize,
    #[serde(skip_serializing)]
    pub query_seed_count: usize,
    #[serde(skip_serializing)]
    pub trust_anchor_count: usize,
    #[serde(rename = "voronoiDominator")]
    pub dominator: Option<PackDnaDominator>,
    pub community_of_mass: Option<PackDnaCommunity>,
    pub ego_subgraph: Option<PackDnaEgoSubgraph>,
    pub ppr_neighbors: Vec<PackDnaPprNeighbor>,
    pub degraded: Vec<PackDnaDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackDnaDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackDnaDominator {
    pub memory_id: String,
    pub cell_size: usize,
    pub pack_member_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackDnaCommunity {
    pub community_id: String,
    pub size: usize,
    pub pack_member_count: usize,
    pub exemplar_memory_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackDnaEgoSubgraph {
    pub center_memory_id: String,
    pub radius: usize,
    pub node_count: usize,
    pub edge_count: usize,
    pub memory_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackDnaPprNeighbor {
    pub memory_id: String,
    pub score: f64,
}

pub fn compute_pack_dna(
    projection: &MemoryGraphProjection,
    input: &PackDnaInput,
) -> GraphResult<PackDna> {
    let cx = current_or_testing_cx();
    compute_pack_dna_with_cx(&cx, projection, input)
}

pub fn compute_pack_dna_with_cx(
    cx: &Cx,
    projection: &MemoryGraphProjection,
    input: &PackDnaInput,
) -> GraphResult<PackDna> {
    let cx_for_worker = cx.clone();
    let directed = projection.graph.clone();
    let input = input.clone();
    run_with_budget(cx, "pack_dna", DEFAULT_FOREGROUND_BUDGET, move || {
        let undirected = undirected_from_directed(&directed)?;
        compute_pack_dna_unbudgeted(&cx_for_worker, &directed, &undirected, &input)
    })?
}

fn compute_pack_dna_unbudgeted(
    cx: &Cx,
    directed: &DiGraph,
    undirected: &Graph,
    input: &PackDnaInput,
) -> GraphResult<PackDna> {
    let pack_ids = valid_memory_ids(&input.pack_memory_ids, directed);
    let query_seed_weights = valid_seed_weights(&input.query_seed_weights, directed);
    let trust_anchors = pack_dna_trust_anchors(input, directed);
    let dominator = dominant_voronoi_anchor(undirected, &pack_ids, &trust_anchors);
    let community_of_mass = pack_community_of_mass(undirected, &pack_ids);
    let ego_subgraph = dominator
        .as_ref()
        .and_then(|anchor| anchor.memory_id.parse::<MemoryId>().ok())
        .map(|anchor| pack_dna_ego_subgraph(undirected, anchor, input.ego_radius));
    let ppr_neighbors =
        pack_dna_ppr_neighbors(cx, directed, &query_seed_weights, input.ppr_neighbor_limit)?;
    let degraded = pack_dna_degradations(dominator.as_ref());

    Ok(PackDna {
        schema: PACK_DNA_SCHEMA_V1,
        snapshot_version: 0,
        pack_memory_count: pack_ids.len(),
        query_seed_count: query_seed_weights.len(),
        trust_anchor_count: trust_anchors.len(),
        dominator,
        community_of_mass,
        ego_subgraph,
        ppr_neighbors,
        degraded,
    })
}

fn pack_dna_degradations(dominator: Option<&PackDnaDominator>) -> Vec<PackDnaDegradation> {
    if dominator.is_some() {
        return Vec::new();
    }

    vec![PackDnaDegradation {
        code: GRAPH_PACK_DNA_NO_DOMINATOR_CODE.to_owned(),
        severity: "low".to_owned(),
        message: "Pack DNA could not identify a trust anchor dominator for this context pack."
            .to_owned(),
        repair: "Seed a trusted source memory with `trust_class=human_explicit`.".to_owned(),
    }]
}

fn undirected_from_directed(directed: &DiGraph) -> GraphResult<Graph> {
    let mut graph = Graph::strict();
    for node in directed.nodes_ordered() {
        graph.add_node(node.to_owned());
    }
    for edge in directed.edges_ordered() {
        graph
            .add_edge_with_attrs(edge.left, edge.right, edge.attrs)
            .map_err(|error| GraphError::GraphEngine {
                operation: "build pack DNA undirected graph",
                source: error.to_string(),
            })?;
    }
    Ok(graph)
}

fn valid_memory_ids(memory_ids: &[MemoryId], graph: &DiGraph) -> Vec<MemoryId> {
    let mut seen = BTreeSet::new();
    let mut valid = Vec::new();
    for memory_id in memory_ids {
        if seen.insert(*memory_id) && graph.has_node(&memory_id.to_string()) {
            valid.push(*memory_id);
        }
    }
    valid
}

fn valid_seed_weights(
    query_seed_weights: &BTreeMap<MemoryId, f64>,
    graph: &DiGraph,
) -> HashMap<MemoryId, f64> {
    query_seed_weights
        .iter()
        .filter_map(|(memory_id, weight)| {
            (graph.has_node(&memory_id.to_string()) && weight.is_finite() && *weight > 0.0)
                .then_some((*memory_id, *weight))
        })
        .collect()
}

fn pack_dna_trust_anchors(input: &PackDnaInput, graph: &DiGraph) -> Vec<MemoryId> {
    let explicit = valid_memory_ids(&input.trust_anchor_memory_ids, graph);
    if !explicit.is_empty() {
        return explicit;
    }
    valid_memory_ids(
        &input.query_seed_weights.keys().copied().collect::<Vec<_>>(),
        graph,
    )
}

fn dominant_voronoi_anchor(
    graph: &Graph,
    pack_ids: &[MemoryId],
    trust_anchors: &[MemoryId],
) -> Option<PackDnaDominator> {
    let anchor_strings = trust_anchors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let anchor_refs = anchor_strings
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    if anchor_refs.is_empty() {
        return None;
    }

    let pack_strings = pack_ids
        .iter()
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    let cells = fnx_algorithms::voronoi_cells(graph, &anchor_refs);
    let mut candidates = cells
        .into_iter()
        .filter_map(|(anchor, mut cell)| {
            cell.sort();
            let pack_member_count = cell
                .iter()
                .filter(|memory_id| pack_strings.contains(*memory_id))
                .count();
            (pack_member_count > 0).then_some(PackDnaDominator {
                memory_id: anchor,
                cell_size: cell.len(),
                pack_member_count,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .pack_member_count
            .cmp(&left.pack_member_count)
            .then_with(|| right.cell_size.cmp(&left.cell_size))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    candidates.into_iter().next()
}

fn pack_community_of_mass(graph: &Graph, pack_ids: &[MemoryId]) -> Option<PackDnaCommunity> {
    if pack_ids.is_empty() || graph.node_count() == 0 {
        return None;
    }
    let pack_strings = pack_ids
        .iter()
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>();
    let mut communities = detect_louvain_communities(graph)
        .into_iter()
        .map(|mut community| {
            community.sort();
            community
        })
        .collect::<Vec<_>>();
    communities.sort();

    let mut candidates = communities
        .into_iter()
        .enumerate()
        .filter_map(|(index, community)| {
            let pack_member_count = community
                .iter()
                .filter(|memory_id| pack_strings.contains(*memory_id))
                .count();
            if pack_member_count == 0 {
                return None;
            }
            let exemplar_memory_ids = community
                .iter()
                .filter(|memory_id| memory_id.parse::<MemoryId>().is_ok())
                .take(10)
                .cloned()
                .collect::<Vec<_>>();
            Some(PackDnaCommunity {
                community_id: format!("community_{:04}", index + 1),
                size: community.len(),
                pack_member_count,
                exemplar_memory_ids,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .pack_member_count
            .cmp(&left.pack_member_count)
            .then_with(|| right.size.cmp(&left.size))
            .then_with(|| left.community_id.cmp(&right.community_id))
    });
    candidates.into_iter().next()
}

fn pack_dna_ego_subgraph(
    graph: &Graph,
    center_memory_id: MemoryId,
    radius: usize,
) -> PackDnaEgoSubgraph {
    let ego = fnx_algorithms::ego_graph(graph, &center_memory_id.to_string(), radius);
    let mut memory_ids = ego
        .nodes_ordered()
        .into_iter()
        .filter(|memory_id| memory_id.parse::<MemoryId>().is_ok())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    memory_ids.sort();
    PackDnaEgoSubgraph {
        center_memory_id: center_memory_id.to_string(),
        radius,
        node_count: ego.node_count(),
        edge_count: ego.edge_count(),
        memory_ids,
    }
}

fn pack_dna_ppr_neighbors(
    cx: &Cx,
    graph: &DiGraph,
    query_seed_weights: &HashMap<MemoryId, f64>,
    limit: usize,
) -> GraphResult<Vec<PackDnaPprNeighbor>> {
    if query_seed_weights.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let seed_ids = query_seed_weights.keys().copied().collect::<BTreeSet<_>>();
    let scores = crate::graph::ppr::compute_personalized_pagerank_with_cx(
        cx,
        graph,
        query_seed_weights,
        Default::default(),
    )?;
    let mut neighbors = scores
        .into_iter()
        .filter(|(memory_id, score)| !seed_ids.contains(memory_id) && score.is_finite())
        .map(|(memory_id, score)| PackDnaPprNeighbor {
            memory_id: memory_id.to_string(),
            score,
        })
        .collect::<Vec<_>>();
    neighbors.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    neighbors.truncate(limit);
    Ok(neighbors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_classes::AttrMap;
    use fnx_runtime::CgseValue;

    fn mem(raw: u128) -> MemoryId {
        MemoryId::from_uuid(uuid::Uuid::from_u128(raw))
    }

    fn pack_dna_projection() -> MemoryGraphProjection {
        let anchor_a = mem(1).to_string();
        let pack_b = mem(2).to_string();
        let pack_c = mem(3).to_string();
        let anchor_d = mem(4).to_string();
        let pack_e = mem(5).to_string();
        let outside = mem(6).to_string();

        let mut graph = DiGraph::strict();
        add_weighted_edge(&mut graph, &anchor_a, &pack_b);
        add_weighted_edge(&mut graph, &pack_b, &pack_c);
        add_weighted_edge(&mut graph, &anchor_d, &pack_e);
        add_weighted_edge(&mut graph, &pack_c, &outside);
        let node_count = graph.node_count();
        let edge_count = graph.edge_count();
        MemoryGraphProjection {
            graph,
            node_count,
            edge_count,
            build_ms: 1.0,
        }
    }

    fn pack_dna_louvain_projection() -> MemoryGraphProjection {
        let anchor_a = mem(1).to_string();
        let pack_b = mem(2).to_string();
        let pack_c = mem(3).to_string();
        let anchor_d = mem(4).to_string();
        let pack_e = mem(5).to_string();

        let mut graph = DiGraph::strict();
        add_weighted_edge(&mut graph, &anchor_a, &pack_b);
        add_weighted_edge(&mut graph, &anchor_a, &pack_c);
        add_weighted_edge(&mut graph, &pack_b, &pack_c);
        add_weighted_edge(&mut graph, &anchor_d, &pack_e);
        let node_count = graph.node_count();
        let edge_count = graph.edge_count();
        MemoryGraphProjection {
            graph,
            node_count,
            edge_count,
            build_ms: 1.0,
        }
    }

    fn add_weighted_edge(graph: &mut DiGraph, source: &str, target: &str) {
        let mut attrs = AttrMap::new();
        attrs.insert("weight".to_owned(), CgseValue::Float(1.0));
        attrs.insert("confidence".to_owned(), CgseValue::Float(1.0));
        attrs.insert(
            "relation".to_owned(),
            CgseValue::String("supports".to_owned()),
        );
        let result = graph.add_edge_with_attrs(source.to_owned(), target.to_owned(), attrs);
        assert!(
            result.is_ok(),
            "test graph edge should be valid: {result:?}"
        );
    }

    #[test]
    fn pack_dna_selects_voronoi_dominator_for_pack_members() -> Result<(), String> {
        let projection = pack_dna_projection();
        let input = PackDnaInput::new(
            vec![mem(2), mem(3), mem(5)],
            vec![mem(1)],
            vec![mem(1), mem(4)],
        );

        let dna = compute_pack_dna(&projection, &input).map_err(|error| error.to_string())?;

        assert_eq!(dna.schema, PACK_DNA_SCHEMA_V1);
        let serialized = serde_json::to_value(&dna).map_err(|error| error.to_string())?;
        assert_eq!(serialized["schema"], PACK_DNA_SCHEMA_V1);
        assert_eq!(serialized["snapshotVersion"], 0);
        assert!(serialized.get("voronoiDominator").is_some());
        assert!(serialized.get("dominator").is_none());
        assert!(serialized.get("packMemoryCount").is_none());
        assert!(serialized.get("querySeedCount").is_none());
        assert!(serialized.get("trustAnchorCount").is_none());
        assert_eq!(dna.pack_memory_count, 3);
        assert!(dna.degraded.is_empty());
        let dominator = dna
            .dominator
            .ok_or_else(|| "expected pack dominator".to_owned())?;
        assert_eq!(dominator.memory_id, mem(1).to_string());
        assert_eq!(dominator.pack_member_count, 2);
        Ok(())
    }

    #[test]
    fn pack_dna_reports_louvain_community_of_mass() -> Result<(), String> {
        let projection = pack_dna_louvain_projection();
        let input = PackDnaInput::new(
            vec![mem(2), mem(3), mem(5)],
            vec![mem(1)],
            vec![mem(1), mem(4)],
        );

        let dna = compute_pack_dna(&projection, &input).map_err(|error| error.to_string())?;

        let community = dna
            .community_of_mass
            .ok_or_else(|| "expected community of mass".to_owned())?;
        assert!(community.size >= 2);
        assert_eq!(community.pack_member_count, 2);
        assert!(community.exemplar_memory_ids.contains(&mem(2).to_string()));
        assert!(community.exemplar_memory_ids.contains(&mem(3).to_string()));
        Ok(())
    }

    #[test]
    fn pack_dna_ego_subgraph_uses_dominator_radius_two() -> Result<(), String> {
        let projection = pack_dna_projection();
        let input = PackDnaInput::new(
            vec![mem(2), mem(3), mem(5)],
            vec![mem(1)],
            vec![mem(1), mem(4)],
        );

        let dna = compute_pack_dna(&projection, &input).map_err(|error| error.to_string())?;

        let ego = dna
            .ego_subgraph
            .ok_or_else(|| "expected ego subgraph".to_owned())?;
        assert_eq!(ego.center_memory_id, mem(1).to_string());
        assert_eq!(ego.radius, DEFAULT_PACK_DNA_EGO_RADIUS);
        assert!(ego.memory_ids.contains(&mem(1).to_string()));
        assert!(ego.memory_ids.contains(&mem(2).to_string()));
        assert!(ego.memory_ids.contains(&mem(3).to_string()));
        assert!(!ego.memory_ids.contains(&mem(4).to_string()));
        Ok(())
    }

    #[test]
    fn pack_dna_ppr_neighbors_exclude_query_seed_and_sort_by_score() -> Result<(), String> {
        let projection = pack_dna_projection();
        let mut input = PackDnaInput::new(
            vec![mem(2), mem(3), mem(5)],
            vec![mem(1)],
            vec![mem(1), mem(4)],
        );
        input.ppr_neighbor_limit = 2;

        let dna = compute_pack_dna(&projection, &input).map_err(|error| error.to_string())?;

        assert_eq!(dna.ppr_neighbors.len(), 2);
        assert_ne!(dna.ppr_neighbors[0].memory_id, mem(1).to_string());
        assert!(dna.ppr_neighbors[0].score >= dna.ppr_neighbors[1].score);
        assert_eq!(dna.ppr_neighbors[0].memory_id, mem(2).to_string());
        Ok(())
    }

    #[test]
    fn pack_dna_reports_no_dominator_degradation_without_trust_anchor() -> Result<(), String> {
        let projection = pack_dna_projection();
        let input = PackDnaInput::new(vec![mem(2), mem(3)], Vec::new(), Vec::new());

        let dna = compute_pack_dna(&projection, &input).map_err(|error| error.to_string())?;

        assert!(dna.dominator.is_none());
        assert_eq!(dna.degraded.len(), 1);
        let degraded = &dna.degraded[0];
        assert_eq!(degraded.code, GRAPH_PACK_DNA_NO_DOMINATOR_CODE);
        assert_eq!(degraded.severity, "low");
        assert!(degraded.message.contains("trust anchor"));
        assert!(degraded.message.contains("dominator"));
        assert!(degraded.repair.contains("trust_class=human_explicit"));
        Ok(())
    }
}
