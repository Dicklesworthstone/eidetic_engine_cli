//! Criterion benchmark and budget guard for worst-case `ee why --format toon`.
//!
//! Group name: `ee_why_toon_token_count`
//!
//! bd-bife.20 pins the compact TOON budget for graph-accreted `ee why`
//! output. The worst-case fixture includes the expanded graph badges that made
//! the surface risky: bayesPosterior, loadBearing, HITS, revision lineage, and
//! causal explanation.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use serde_json::{Value, json};

const GROUP_NAME: &str = "ee_why_toon_token_count";
const PRE_EPIC_BASELINE_TOKENS: u32 = 800;
const MAX_COMPACT_TOON_TOKENS: u32 = PRE_EPIC_BASELINE_TOKENS * 3 / 2;

fn worst_case_why_value(verbose: bool) -> Value {
    let verbose_tail = if verbose {
        " This deliberately verbose explanatory clause repeats enough detail to exceed the compact TOON token budget and prove the bench catches expansion."
            .repeat(80)
    } else {
        String::new()
    };

    json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "command": "why",
            "memoryId": "mem_graph_release_guardrail",
            "found": true,
            "storage": {
                "origin": "remember",
                "trustClass": "human_explicit",
                "trustSubclass": "project_rule",
                "provenanceUri": "file://AGENTS.md#L1-L90",
                "workflowId": "wf_release_graph",
                "createdAt": "2026-05-21T00:00:00Z",
                "validFrom": "2026-05-21T00:00:00Z",
                "validTo": null,
                "validityStatus": "active",
                "validityWindowKind": "open_ended"
            },
            "retrieval": {
                "confidence": 0.9123,
                "utility": 0.8444,
                "importance": 0.7812,
                "tags": ["release", "rch", "graph", "toon"],
                "level": "procedural",
                "kind": "rule"
            },
            "selection": {
                "selectionScore": 0.8877,
                "aboveConfidenceThreshold": true,
                "isActive": true,
                "scoreBreakdown": "confidence * utility + graph authority boost",
                "latestPackSelection": {
                    "packId": "pack_graph_release",
                    "query": "prepare graph release verification",
                    "profile": "grounding",
                    "rank": 1,
                    "section": "procedural_rules",
                    "estimatedTokens": 32,
                    "relevance": 0.93,
                    "utility": 0.84,
                    "why": "graph release verification needs RCH-only build discipline",
                    "packHash": "blake3:packgraphrelease",
                    "selectedAt": "2026-05-21T00:01:00Z"
                }
            },
            "bayesPosterior": {
                "alpha": 9.5,
                "beta": 1.0,
                "mean": 0.9047619048,
                "effectiveSampleSize": 10.5,
                "credibleInterval90": { "lo": 0.712, "hi": 0.989 },
                "credibleInterval50": { "lo": 0.861, "hi": 0.951 }
            },
            "graphRetrieval": {
                "status": "available",
                "source": {
                    "kind": "graph_snapshot",
                    "workspaceId": "wsp_graph_release",
                    "graphType": "memory_links",
                    "snapshot": {
                        "id": "graph_snapshot_release",
                        "schemaVersion": "ee.graph.snapshot.v1",
                        "snapshotVersion": 7,
                        "sourceGeneration": 42,
                        "status": "fresh",
                        "contentHash": "blake3:graphsnapshot",
                        "createdAt": "2026-05-21T00:00:30Z"
                    }
                },
                "centralityScore": 0.7712,
                "authorityScore": 0.9134,
                "hubScore": 0.4521,
                "hits": {
                    "schema": "ee.graph.hits.v1",
                    "authority": {
                        "raw": 0.9134,
                        "normalized": 1.0,
                        "rank": 1,
                        "percentile": 1.0
                    },
                    "hub": {
                        "raw": 0.4521,
                        "normalized": 0.552,
                        "rank": 4,
                        "percentile": 0.76
                    },
                    "roleLabel": "authority",
                    "roleRationale": "rules cite this memory as a grounding source"
                },
                "pagerank": {
                    "raw": 0.041,
                    "normalized": 0.88,
                    "rank": 2,
                    "weight": 0.35,
                    "contribution": 0.308,
                    "formula": "normalized_pagerank * 0.35"
                },
                "betweenness": {
                    "raw": 0.019,
                    "normalized": 0.41,
                    "rank": 9,
                    "weight": 0.20,
                    "contribution": 0.082,
                    "formula": "normalized_betweenness * 0.20"
                },
                "communityId": "community_release",
                "distanceToQuerySeed": 1,
                "sameClusterAsTopResult": true,
                "evidenceSupportCount": 12,
                "contradictionCount": 0,
                "orphanPenalty": 0.0,
                "staleBridgePenalty": 0.0,
                "labels": ["load_bearing", "authority", "fresh"],
                "reasons": ["high authority score", "recent pack selection", "fresh provenance"],
                "centralityFormula": "0.35*pagerank + 0.20*betweenness + 0.45*authority",
                "orphanPenaltyFormula": "0 when degree > 0",
                "staleBridgePenaltyFormula": "0 when validity active",
                "degraded": []
            },
            "loadBearing": {
                "isLoadBearing": true,
                "loadBearingScore": 0.8732,
                "authorityRank": 1,
                "citingRuleCount": 3,
                "interpretation": "load_bearing",
                "evidence": {
                    "schema": "ee.load_bearing.v1",
                    "projection": "rule_source_bipartite",
                    "algorithm": "eigenvector_authority",
                    "snapshotId": "graph_snapshot_release"
                },
                "citingRules": [
                    { "ruleId": "rule_rch_remote_only", "relation": "cites" },
                    { "ruleId": "rule_no_reset", "relation": "cites" },
                    { "ruleId": "rule_agent_mail_reserve", "relation": "cites" }
                ],
                "rationale": "multiple operational rules rely on this memory as source evidence"
            },
            "revisionLineage": {
                "schema": "ee.memory.revision_lineage.v1",
                "memoryId": "mem_graph_release_guardrail",
                "generation": 4,
                "ancestors": ["mem_graph_release_guardrail_v1", "mem_graph_release_guardrail_v2"],
                "descendants": ["mem_graph_release_guardrail_v4"],
                "immediateDominator": "mem_graph_release_guardrail_v2",
                "dominanceFrontier": ["mem_release_policy", "mem_rch_policy"],
                "impact": {
                    "affectedMemoryCount": 7,
                    "affectedPackCount": 2,
                    "risk": "medium"
                }
            },
            "causalExplanation": {
                "schema": "ee.why.causal.v1",
                "memoryId": "mem_graph_release_guardrail",
                "status": "available",
                "chains": [{
                    "chainId": "causal_release_rch",
                    "cost": 0.22,
                    "supportingEvidenceCount": 5,
                    "path": [
                        { "memoryId": "mem_rch_remote_only", "relation": "prevents" },
                        { "memoryId": "mem_graph_release_guardrail", "relation": "supports" }
                    ]
                }],
                "summary": format!("RCH-only verification prevents local fallback drift.{verbose_tail}")
            },
            "agentProfile": {
                "schema": "ee.agent_context_profile.v1",
                "agentName": "CalmBay",
                "agentNameHash": "blake3:calmbay",
                "helpfulCount": 18,
                "harmfulCount": 1,
                "ignoredCount": 4,
                "observedOutcomes": 23,
                "bias": 0.02734375,
                "maxBiasMagnitude": 0.125,
                "coldStart": false,
                "coldStartThreshold": 5,
                "lastSeenAt": "2026-05-21T00:02:00Z"
            },
            "lifecycle": {
                "status": "active",
                "tombstoned_at": null,
                "tombstoned_reason": null
            },
            "degraded": []
        },
        "degraded": []
    })
}

fn render_worst_case_toon(verbose: bool) -> String {
    let json = worst_case_why_value(verbose).to_string();
    ee::output::render_toon_from_json(&json)
}

fn estimated_tokens(text: &str) -> u32 {
    ee::pack::estimate_tokens_default(text)
}

fn assert_compact_toon_budget() {
    let compact_toon = render_worst_case_toon(false);
    let compact_tokens = estimated_tokens(&compact_toon);
    assert!(
        compact_tokens <= MAX_COMPACT_TOON_TOKENS,
        "{GROUP_NAME}: compact worst-case TOON used {compact_tokens} tokens; budget is {MAX_COMPACT_TOON_TOKENS} tokens (1.5x pre-epic baseline {PRE_EPIC_BASELINE_TOKENS})"
    );

    let verbose_toon = render_worst_case_toon(true);
    let verbose_tokens = estimated_tokens(&verbose_toon);
    assert!(
        verbose_tokens > MAX_COMPACT_TOON_TOKENS,
        "{GROUP_NAME}: deliberate verbose expansion used {verbose_tokens} tokens; expected it to exceed the compact budget {MAX_COMPACT_TOON_TOKENS}"
    );
}

fn bench_why_toon_token_count(c: &mut Criterion) {
    assert_compact_toon_budget();

    let json = worst_case_why_value(false).to_string();
    c.bench_function(GROUP_NAME, |b| {
        b.iter(|| {
            let toon = ee::output::render_toon_from_json(black_box(&json));
            let tokens = estimated_tokens(&toon);
            assert!(
                tokens <= MAX_COMPACT_TOON_TOKENS,
                "{GROUP_NAME}: compact worst-case TOON used {tokens} tokens; budget is {MAX_COMPACT_TOON_TOKENS}"
            );
            black_box((toon, tokens));
        });
    });
}

criterion_group!(benches, bench_why_toon_token_count);
criterion_main!(benches);
