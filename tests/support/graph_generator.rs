#![allow(dead_code)]

use fnx_classes::Graph;
use fnx_classes::digraph::DiGraph;

pub fn deterministic_graph(node_count: usize, density: f64, seed: u64) -> Result<Graph, String> {
    let mut graph = Graph::strict();
    add_nodes(&mut graph, node_count);

    let density = normalized_density(density);
    for left in 0..node_count {
        for right in (left + 1)..node_count {
            if edge_sample(seed, left, right, 0x4752_4150_485f_554e) < density {
                graph
                    .add_edge(node_name(left), node_name(right))
                    .map_err(|error| error.to_string())?;
            }
        }
    }

    Ok(graph)
}

pub fn deterministic_digraph(
    node_count: usize,
    density: f64,
    seed: u64,
) -> Result<DiGraph, String> {
    let mut graph = DiGraph::strict();
    add_digraph_nodes(&mut graph, node_count);

    let density = normalized_density(density);
    for source in 0..node_count {
        for target in 0..node_count {
            if source != target
                && edge_sample(seed, source, target, 0x4449_4752_4150_485f) < density
            {
                graph
                    .add_edge(node_name(source), node_name(target))
                    .map_err(|error| error.to_string())?;
            }
        }
    }

    Ok(graph)
}

fn add_nodes(graph: &mut Graph, node_count: usize) {
    for node in 0..node_count {
        let _ = graph.add_node(node_name(node));
    }
}

fn add_digraph_nodes(graph: &mut DiGraph, node_count: usize) {
    for node in 0..node_count {
        let _ = graph.add_node(node_name(node));
    }
}

fn normalized_density(density: f64) -> f64 {
    if !density.is_finite() || density <= 0.0 {
        0.0
    } else if density >= 1.0 {
        1.0
    } else {
        density
    }
}

fn node_name(index: usize) -> String {
    index.to_string()
}

fn edge_sample(seed: u64, left: usize, right: usize, tag: u64) -> f64 {
    let mixed = splitmix64(
        seed ^ tag
            ^ (left as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ (right as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9),
    );
    let fraction_bits = mixed >> 11;
    (fraction_bits as f64) * (1.0 / ((1_u64 << 53) as f64))
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = value;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
