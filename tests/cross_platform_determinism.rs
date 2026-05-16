//! Cross-platform graph determinism hash table (bd-3usjw.25).
//!
//! This test pins `(target_triple, algorithm, seed) -> output_hash` for
//! deterministic graph wrappers. CI runs it on the supported target matrix so
//! platform-specific floating-point drift fails with a concrete hash mismatch.

#![cfg(feature = "graph")]

use std::collections::BTreeMap;
use std::time::Instant;

use ee::graph::hits::compute_hits;
use ee::graph::minhash_rank::{
    MinHashRankPolicy, MinHashRankResult, compute_minhash_rank_with_policy,
};
use ee::graph::ppr::{
    PersonalizedPageRankPolicy, compute_personalized_pagerank_result_with_policy,
};
use ee::graph::{AttrMap, DiGraph, GraphResult, PageRankResult};
use fnx_runtime::CgseValue;

type TestResult = Result<(), String>;

const TARGET_TRIPLES: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

const EXPECTED_HASHES: &[ExpectedHash] = &[
    ExpectedHash {
        target_triple: "x86_64-unknown-linux-gnu",
        algorithm: "personalized_pagerank",
        seed: "weighted-cycle-v1",
        output_hash: "blake3:01255168f7add48cf43e7a296eff360a5d3248a7e315ceea60985c3410a1e2f0",
    },
    ExpectedHash {
        target_triple: "x86_64-unknown-linux-gnu",
        algorithm: "hits",
        seed: "authority-star-v1",
        output_hash: "blake3:899880f112e11cf015273f6cae10010e01d49f362061bb4cd18932fcbcab1bf9",
    },
    ExpectedHash {
        target_triple: "x86_64-unknown-linux-gnu",
        algorithm: "minhash_rank_centrality",
        seed: "incoming-density-v1",
        output_hash: "blake3:6756bd905f8593b7587ec91fe699e48c4bf2ffff22dd4c94bdeb248cc99a4b53",
    },
    ExpectedHash {
        target_triple: "x86_64-unknown-linux-musl",
        algorithm: "personalized_pagerank",
        seed: "weighted-cycle-v1",
        output_hash: "blake3:01255168f7add48cf43e7a296eff360a5d3248a7e315ceea60985c3410a1e2f0",
    },
    ExpectedHash {
        target_triple: "x86_64-unknown-linux-musl",
        algorithm: "hits",
        seed: "authority-star-v1",
        output_hash: "blake3:899880f112e11cf015273f6cae10010e01d49f362061bb4cd18932fcbcab1bf9",
    },
    ExpectedHash {
        target_triple: "x86_64-unknown-linux-musl",
        algorithm: "minhash_rank_centrality",
        seed: "incoming-density-v1",
        output_hash: "blake3:6756bd905f8593b7587ec91fe699e48c4bf2ffff22dd4c94bdeb248cc99a4b53",
    },
    ExpectedHash {
        target_triple: "aarch64-unknown-linux-gnu",
        algorithm: "personalized_pagerank",
        seed: "weighted-cycle-v1",
        output_hash: "blake3:01255168f7add48cf43e7a296eff360a5d3248a7e315ceea60985c3410a1e2f0",
    },
    ExpectedHash {
        target_triple: "aarch64-unknown-linux-gnu",
        algorithm: "hits",
        seed: "authority-star-v1",
        output_hash: "blake3:899880f112e11cf015273f6cae10010e01d49f362061bb4cd18932fcbcab1bf9",
    },
    ExpectedHash {
        target_triple: "aarch64-unknown-linux-gnu",
        algorithm: "minhash_rank_centrality",
        seed: "incoming-density-v1",
        output_hash: "blake3:6756bd905f8593b7587ec91fe699e48c4bf2ffff22dd4c94bdeb248cc99a4b53",
    },
    ExpectedHash {
        target_triple: "aarch64-apple-darwin",
        algorithm: "personalized_pagerank",
        seed: "weighted-cycle-v1",
        output_hash: "blake3:01255168f7add48cf43e7a296eff360a5d3248a7e315ceea60985c3410a1e2f0",
    },
    ExpectedHash {
        target_triple: "aarch64-apple-darwin",
        algorithm: "hits",
        seed: "authority-star-v1",
        output_hash: "blake3:899880f112e11cf015273f6cae10010e01d49f362061bb4cd18932fcbcab1bf9",
    },
    ExpectedHash {
        target_triple: "aarch64-apple-darwin",
        algorithm: "minhash_rank_centrality",
        seed: "incoming-density-v1",
        output_hash: "blake3:6756bd905f8593b7587ec91fe699e48c4bf2ffff22dd4c94bdeb248cc99a4b53",
    },
    ExpectedHash {
        target_triple: "x86_64-pc-windows-msvc",
        algorithm: "personalized_pagerank",
        seed: "weighted-cycle-v1",
        output_hash: "blake3:01255168f7add48cf43e7a296eff360a5d3248a7e315ceea60985c3410a1e2f0",
    },
    ExpectedHash {
        target_triple: "x86_64-pc-windows-msvc",
        algorithm: "hits",
        seed: "authority-star-v1",
        output_hash: "blake3:899880f112e11cf015273f6cae10010e01d49f362061bb4cd18932fcbcab1bf9",
    },
    ExpectedHash {
        target_triple: "x86_64-pc-windows-msvc",
        algorithm: "minhash_rank_centrality",
        seed: "incoming-density-v1",
        output_hash: "blake3:6756bd905f8593b7587ec91fe699e48c4bf2ffff22dd4c94bdeb248cc99a4b53",
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExpectedHash {
    target_triple: &'static str,
    algorithm: &'static str,
    seed: &'static str,
    output_hash: &'static str,
}

#[derive(Debug)]
struct DivergenceManifest {
    divergences: Vec<Divergence>,
}

#[derive(Debug)]
struct Divergence {
    target_triple: String,
    algorithm: String,
    seed: String,
    reason: String,
}

fn trace_cross_platform_determinism(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "repo",
        request_id = "cross_platform_determinism_contract",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.25"),
        surface = "cross_platform_determinism",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "cross-platform determinism contract checkpoint"
    );
}

#[test]
fn all_supported_targets_have_hash_rows_for_each_algorithm_seed() -> TestResult {
    for target_triple in TARGET_TRIPLES {
        for (algorithm, seed) in [
            ("personalized_pagerank", "weighted-cycle-v1"),
            ("hits", "authority-star-v1"),
            ("minhash_rank_centrality", "incoming-density-v1"),
        ] {
            let present = EXPECTED_HASHES.iter().any(|entry| {
                entry.target_triple == *target_triple
                    && entry.algorithm == algorithm
                    && entry.seed == seed
            });
            ensure(
                present,
                format!("missing expected hash row for {target_triple} {algorithm} {seed}"),
            )?;
        }
    }
    Ok(())
}

#[test]
fn divergence_manifest_is_explicit_and_empty_until_drift_is_approved() -> TestResult {
    let manifest = divergence_manifest()?;
    ensure(
        manifest.divergences.is_empty(),
        format!(
            "cross-platform graph divergences require explicit review; found: {:?}",
            manifest.divergences
        ),
    )
}

#[test]
fn current_target_matches_pinned_graph_algorithm_hashes() -> TestResult {
    let started = Instant::now();
    trace_cross_platform_determinism("dependency_check", 0, &[]);
    let target = current_target_triple();
    ensure(
        TARGET_TRIPLES.contains(&target),
        format!("current target {target} is not listed in TARGET_TRIPLES"),
    )?;
    let manifest = divergence_manifest()?;
    let observed = [
        observed_pagerank_hash()?,
        observed_hits_hash()?,
        observed_minhash_rank_hash()?,
    ];

    for observed in observed {
        let Some(expected) = EXPECTED_HASHES.iter().find(|entry| {
            entry.target_triple == target
                && entry.algorithm == observed.algorithm
                && entry.seed == observed.seed
        }) else {
            return Err(format!(
                "missing expected hash for target={target} algorithm={} seed={} observed={}",
                observed.algorithm, observed.seed, observed.output_hash
            ));
        };

        if expected.output_hash != observed.output_hash {
            let approved_divergence = manifest.divergences.iter().any(|entry| {
                entry.target_triple == target
                    && entry.algorithm == observed.algorithm
                    && entry.seed == observed.seed
                    && !entry.reason.trim().is_empty()
            });
            ensure(
                approved_divergence,
                format!(
                    "cross-platform determinism hash mismatch for target={target} algorithm={} seed={}: expected={} observed={}",
                    observed.algorithm, observed.seed, expected.output_hash, observed.output_hash
                ),
            )?;
        }
    }
    trace_cross_platform_determinism("response", started.elapsed().as_millis() as u64, &[]);
    Ok(())
}

#[derive(Debug)]
struct ObservedHash {
    algorithm: &'static str,
    seed: &'static str,
    output_hash: String,
}

fn observed_pagerank_hash() -> Result<ObservedHash, String> {
    let graph = weighted_cycle_graph()?;
    let seed_weights = BTreeMap::from([("mem_a".to_owned(), 2.0), ("mem_c".to_owned(), 1.0)]);
    let result = graph_result(compute_personalized_pagerank_result_with_policy(
        &graph,
        &seed_weights,
        PersonalizedPageRankPolicy {
            alpha: 0.70,
            max_iterations: 25,
            tolerance: 1.0e-9,
        },
    ))?;
    Ok(ObservedHash {
        algorithm: "personalized_pagerank",
        seed: "weighted-cycle-v1",
        output_hash: hash_pagerank_result(&result),
    })
}

fn observed_hits_hash() -> Result<ObservedHash, String> {
    let graph = authority_star_graph()?;
    let result = graph_result(compute_hits(&graph))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.cross_platform_determinism.hits.v1\0");
    for (node, score) in result.hubs {
        hash_string(&mut hasher, &node);
        hash_f64(&mut hasher, score);
    }
    hasher.update(b"\0authorities\0");
    for (node, score) in result.authorities {
        hash_string(&mut hasher, &node);
        hash_f64(&mut hasher, score);
    }
    Ok(ObservedHash {
        algorithm: "hits",
        seed: "authority-star-v1",
        output_hash: format!("blake3:{}", hasher.finalize().to_hex()),
    })
}

fn observed_minhash_rank_hash() -> Result<ObservedHash, String> {
    let graph = incoming_density_graph()?;
    let result = graph_result(compute_minhash_rank_with_policy(
        &graph,
        MinHashRankPolicy {
            signature_count: 16,
            top_k: 6,
        },
    ))?;
    Ok(ObservedHash {
        algorithm: "minhash_rank_centrality",
        seed: "incoming-density-v1",
        output_hash: hash_minhash_rank_result(&result),
    })
}

fn hash_minhash_rank_result(result: &MinHashRankResult) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.cross_platform_determinism.minhash_rank_centrality.v1\0");
    hasher.update(&(result.policy.signature_count as u64).to_le_bytes());
    hasher.update(&(result.policy.top_k as u64).to_le_bytes());
    hasher.update(&(result.witness.nodes_touched as u64).to_le_bytes());
    hasher.update(&(result.witness.edges_scanned as u64).to_le_bytes());
    hasher.update(&(result.witness.queue_peak as u64).to_le_bytes());
    for score in &result.scores {
        hasher.update(&(score.rank as u64).to_le_bytes());
        hash_string(&mut hasher, &score.node);
        hasher.update(&score.signature_density.to_le_bytes());
        hasher.update(&(score.incoming_edge_count as u64).to_le_bytes());
        hasher.update(&(score.outgoing_edge_count as u64).to_le_bytes());
        for value in &score.signature {
            hasher.update(&value.to_le_bytes());
        }
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_pagerank_result(result: &PageRankResult) -> String {
    let mut scores = result.scores.clone();
    scores.sort_by(|left, right| left.node.cmp(&right.node));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.cross_platform_determinism.personalized_pagerank.v1\0");
    hasher.update(&[u8::from(result.converged)]);
    hasher.update(&result.witness.nodes_touched.to_le_bytes());
    hasher.update(&result.witness.edges_scanned.to_le_bytes());
    hasher.update(&result.witness.queue_peak.to_le_bytes());
    for score in scores {
        hash_string(&mut hasher, &score.node);
        hash_f64(&mut hasher, score.score);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn weighted_cycle_graph() -> Result<DiGraph, String> {
    let mut graph = DiGraph::strict();
    add_weighted_edge(&mut graph, "mem_a", "mem_b", "supports", 1.0, 1.0)?;
    add_weighted_edge(&mut graph, "mem_b", "mem_c", "derived_from", 0.8, 0.9)?;
    add_weighted_edge(&mut graph, "mem_c", "mem_a", "related", 0.6, 0.7)?;
    add_weighted_edge(&mut graph, "mem_a", "mem_d", "co_mention", 0.5, 0.8)?;
    add_weighted_edge(&mut graph, "mem_d", "mem_b", "supports", 0.7, 0.6)?;
    Ok(graph)
}

fn authority_star_graph() -> Result<DiGraph, String> {
    let mut graph = DiGraph::strict();
    for source in ["mem_a", "mem_c", "mem_d", "mem_e"] {
        add_weighted_edge(&mut graph, source, "mem_b", "supports", 1.0, 1.0)?;
    }
    add_weighted_edge(&mut graph, "mem_b", "mem_f", "related", 0.4, 0.9)?;
    add_weighted_edge(&mut graph, "mem_f", "mem_b", "derived_from", 0.6, 0.8)?;
    Ok(graph)
}

fn incoming_density_graph() -> Result<DiGraph, String> {
    let mut graph = DiGraph::strict();
    for (source, target) in [
        ("mem_d", "mem_a"),
        ("mem_e", "mem_a"),
        ("mem_f", "mem_a"),
        ("mem_c", "mem_b"),
        ("mem_e", "mem_b"),
        ("mem_a", "mem_c"),
        ("mem_b", "mem_c"),
        ("mem_a", "mem_d"),
        ("mem_b", "mem_e"),
        ("mem_c", "mem_f"),
    ] {
        add_weighted_edge(&mut graph, source, target, "supports", 1.0, 1.0)?;
    }
    Ok(graph)
}

fn add_weighted_edge(
    graph: &mut DiGraph,
    source: &str,
    target: &str,
    relation: &str,
    weight: f64,
    confidence: f64,
) -> Result<(), String> {
    let mut attrs = AttrMap::new();
    attrs.insert(
        "relation".to_owned(),
        CgseValue::String(relation.to_owned()),
    );
    attrs.insert("weight".to_owned(), CgseValue::Float(weight));
    attrs.insert("confidence".to_owned(), CgseValue::Float(confidence));
    graph
        .add_edge_with_attrs(source.to_owned(), target.to_owned(), attrs)
        .map_err(|error| format!("add edge {source}->{target}: {error:?}"))
}

fn hash_string(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hash_f64(hasher: &mut blake3::Hasher, value: f64) {
    hasher.update(&value.to_bits().to_le_bytes());
}

fn current_target_triple() -> &'static str {
    if cfg!(all(
        target_arch = "x86_64",
        target_os = "linux",
        target_env = "gnu"
    )) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(
        target_arch = "x86_64",
        target_os = "linux",
        target_env = "musl"
    )) {
        "x86_64-unknown-linux-musl"
    } else if cfg!(all(
        target_arch = "aarch64",
        target_os = "linux",
        target_env = "gnu"
    )) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(
        target_arch = "x86_64",
        target_os = "windows",
        target_env = "msvc"
    )) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown-target"
    }
}

fn divergence_manifest() -> Result<DivergenceManifest, String> {
    let manifest = include_str!("cross_platform_determinism/divergences.toml");
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "divergences = []" {
            continue;
        }
        return Err(format!(
            "unsupported cross-platform divergence manifest entry: {trimmed}"
        ));
    }
    Ok(DivergenceManifest {
        divergences: Vec::new(),
    })
}

fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
    result.map_err(|error| error.to_string())
}

fn ensure(condition: bool, message: String) -> TestResult {
    if condition { Ok(()) } else { Err(message) }
}
