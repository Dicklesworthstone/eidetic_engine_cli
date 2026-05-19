//! Contract checks for graph-related CLI help surfaces.
//!
//! `bd-bife.14` requires every new graph-facing flag to be discoverable from
//! command help and backed by examples/docs. This contract keeps the current
//! Clap surfaces honest while the broader graph epic is still landing.

use clap::Parser;
use clap::error::ErrorKind;
use ee::cli::Cli;

type TestResult = Result<(), String>;

fn help_for(args: &[&str]) -> Result<String, String> {
    match Cli::try_parse_from(args) {
        Ok(_) => Err(format!("{} did not request help", args.join(" "))),
        Err(error) if error.kind() == ErrorKind::DisplayHelp => Ok(error.to_string()),
        Err(error) => Err(format!(
            "{} returned {:?} instead of help",
            args.join(" "),
            error.kind()
        )),
    }
}

fn assert_contains_all(haystack: &str, needles: &[&str], context: &str) -> TestResult {
    let missing: Vec<&str> = needles
        .iter()
        .copied()
        .filter(|needle| !haystack.contains(needle))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("{context} missing help entries: {missing:?}"))
    }
}

#[test]
fn graph_pack_and_insights_flags_are_help_discoverable() -> TestResult {
    let context_help = help_for(&["ee", "context", "--help"])?;
    assert_contains_all(
        &context_help,
        &[
            "--profile",
            "--ppr-weight",
            "--pack-profile",
            "--resource-profile",
            "--explain",
            "--no-pack-dna",
            "--no-coverage-fill",
            "--no-rendered-text",
            "--no-skipped",
            "--no-meta",
        ],
        "ee context --help",
    )?;

    let pack_help = help_for(&["ee", "pack", "--help"])?;
    assert_contains_all(
        &pack_help,
        &[
            "--profile",
            "--pack-profile",
            "--resource-profile",
            "--coordination-snapshot",
            "--coordination-stale-after-ms",
            "--include-non-affecting-degradations",
            "--include-expired",
            "--include-future",
            "--include-stale",
        ],
        "ee pack --help",
    )?;

    let insights_help = help_for(&["ee", "insights", "--help"])?;
    assert_contains_all(
        &insights_help,
        &["--section", "--explain", "--limit", "--offset"],
        "ee insights --help",
    )?;

    let why_help = help_for(&["ee", "why", "--help"])?;
    assert_contains_all(
        &why_help,
        &["--database", "--confidence-threshold", "--causal-explain"],
        "ee why --help",
    )?;

    let rule_provenance_help = help_for(&["ee", "rule", "provenance", "--help"])?;
    assert_contains_all(
        &rule_provenance_help,
        &["RULE_ID", "--database"],
        "ee rule provenance --help",
    )
}

#[test]
fn graph_command_flags_are_help_discoverable() -> TestResult {
    for command in [
        "pagerank",
        "betweenness",
        "hits",
        "communities",
        "articulation",
    ] {
        let help = help_for(&["ee", "graph", command, "--help"])?;
        assert_contains_all(
            &help,
            &[
                "--database",
                "--min-weight",
                "--min-confidence",
                "--link-limit",
                "--limit",
                "--include-tombstoned",
            ],
            &format!("ee graph {command} --help"),
        )?;
    }

    let louvain_help = help_for(&["ee", "graph", "louvain", "--help"])?;
    assert_contains_all(
        &louvain_help,
        &[
            "--database",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
            "--limit",
            "--resolution",
            "--threshold",
            "--max-level",
            "--seed",
        ],
        "ee graph louvain --help",
    )?;

    let k_core_help = help_for(&["ee", "graph", "k-core", "--help"])?;
    assert_contains_all(
        &k_core_help,
        &[
            "--database",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
            "--k",
        ],
        "ee graph k-core --help",
    )?;

    for command in ["path", "explain-link"] {
        let help = help_for(&["ee", "graph", command, "--help"])?;
        assert_contains_all(
            &help,
            &[
                "--database",
                "--min-weight",
                "--min-confidence",
                "--link-limit",
            ],
            &format!("ee graph {command} --help"),
        )?;
    }

    let export_help = help_for(&["ee", "graph", "export", "--help"])?;
    assert_contains_all(
        &export_help,
        &[
            "--database",
            "--workspace-id",
            "--snapshot-id",
            "--graph-type",
        ],
        "ee graph export --help",
    )?;

    let centrality_help = help_for(&["ee", "graph", "centrality", "--help"])?;
    assert_contains_all(
        &centrality_help,
        &[
            "--database",
            "--algorithm",
            "--limit",
            "--memory-id",
            "--require-fresh",
        ],
        "ee graph centrality --help",
    )?;

    let centrality_refresh_help = help_for(&["ee", "graph", "centrality-refresh", "--help"])?;
    assert_contains_all(
        &centrality_refresh_help,
        &[
            "--database",
            "--dry-run",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
        ],
        "ee graph centrality-refresh --help",
    )?;

    let snapshot_help = help_for(&["ee", "graph", "snapshot", "refresh", "--help"])?;
    assert_contains_all(
        &snapshot_help,
        &[
            "--database",
            "--dry-run",
            "--graph",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
        ],
        "ee graph snapshot refresh --help",
    )?;

    let enrichment_help = help_for(&["ee", "graph", "feature-enrichment", "--help"])?;
    assert_contains_all(
        &enrichment_help,
        &[
            "--database",
            "--dry-run",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
            "--max-features",
            "--min-combined-score",
            "--max-selection-boost",
        ],
        "ee graph feature-enrichment --help",
    )?;

    let neighborhood_help = help_for(&["ee", "graph", "neighborhood", "--help"])?;
    assert_contains_all(
        &neighborhood_help,
        &["--database", "--direction", "--relation", "--limit"],
        "ee graph neighborhood --help",
    )
}

#[test]
fn causal_command_flags_are_help_discoverable() -> TestResult {
    let trace_help = help_for(&["ee", "causal", "trace", "--help"])?;
    assert_contains_all(
        &trace_help,
        &[
            "FAILURE_MEMORY_ID",
            "--memory-id",
            "--run-id",
            "--pack-id",
            "--preflight-id",
            "--tripwire-id",
            "--procedure-id",
            "--agent-id",
            "--database",
            "--database-workspace-id",
            "--limit",
            "--depth",
            "--include-exposures",
            "--include-outcomes",
            "--dry-run",
        ],
        "ee causal trace --help",
    )?;

    let compare_help = help_for(&["ee", "causal", "compare", "--help"])?;
    assert_contains_all(
        &compare_help,
        &[
            "CHAIN_A",
            "CHAIN_B",
            "--fixture-replay-id",
            "--shadow-run-id",
            "--counterfactual-episode-id",
            "--experiment-id",
            "--artifact-id",
            "--decision-id",
            "--method",
            "--dry-run",
        ],
        "ee causal compare --help",
    )?;

    let estimate_help = help_for(&["ee", "causal", "estimate", "--help"])?;
    assert_contains_all(
        &estimate_help,
        &[
            "CHAIN_ID",
            "--artifact-id",
            "--decision-id",
            "--chain-id",
            "--agent-id",
            "--method",
            "--include-confounders",
            "--include-assumptions",
            "--dry-run",
        ],
        "ee causal estimate --help",
    )?;

    let promote_help = help_for(&["ee", "causal", "promote-plan", "--help"])?;
    assert_contains_all(
        &promote_help,
        &[
            "CHAIN_ID",
            "--artifact-id",
            "--decision-id",
            "--estimate-id",
            "--action",
            "--method",
            "--minimum-uplift",
            "--include-revalidation",
            "--include-narrower-routing",
            "--include-experiment-proposals",
            "--dry-run",
        ],
        "ee causal promote-plan --help",
    )
}

#[test]
fn diagnostic_graph_fixture_flags_are_help_discoverable() -> TestResult {
    let causal_edge_help = help_for(&["ee", "diag", "causal-edge", "--help"])?;
    assert_contains_all(
        &causal_edge_help,
        &[
            "--database",
            "--workspace-id",
            "--edge-id",
            "--failure-id",
            "--candidate-cause-id",
            "--contribution-score",
            "--evidence-uri",
            "--computed-at",
            "--method",
        ],
        "ee diag causal-edge --help",
    )?;

    let graph_snapshot_help = help_for(&["ee", "diag", "graph-snapshot", "--help"])?;
    assert_contains_all(
        &graph_snapshot_help,
        &[
            "--database",
            "--status",
            "--metrics-json",
            "--node-count",
            "--edge-count",
            "--source-generation",
        ],
        "ee diag graph-snapshot --help",
    )
}

#[test]
fn proximity_health_and_maintenance_flags_are_help_discoverable() -> TestResult {
    let proximity_help = help_for(&["ee", "proximity", "--help"])?;
    assert_contains_all(
        &proximity_help,
        &[
            "--database",
            "--min-weight",
            "--min-confidence",
            "--link-limit",
            "--include-tombstoned",
        ],
        "ee proximity --help",
    )?;

    let health_help = help_for(&["ee", "health", "--help"])?;
    assert_contains_all(&health_help, &["--robot-insights"], "ee health --help")?;

    let status_help = help_for(&["ee", "status", "--help"])?;
    assert_contains_all(&status_help, &["--skyline"], "ee status --help")?;

    let curate_disposition_help = help_for(&["ee", "curate", "disposition", "--help"])?;
    assert_contains_all(
        &curate_disposition_help,
        &[
            "--database",
            "--actor",
            "--apply",
            "--no-structural-decay",
            "--now",
        ],
        "ee curate disposition --help",
    )?;

    let maintenance_help = help_for(&["ee", "maintenance", "run", "--help"])?;
    assert_contains_all(
        &maintenance_help,
        &[
            "--job",
            "--database",
            "--dry-run",
            "--include-decay",
            "--no-structural-decay",
            "--as-of",
            "--time-limit-ms",
            "--item-limit",
        ],
        "ee maintenance run --help",
    )?;

    let prune_help = help_for(&["ee", "maintenance", "graph-snapshot-prune", "--help"])?;
    assert_contains_all(
        &prune_help,
        &["--database", "--dry-run", "--time-limit-ms", "--item-limit"],
        "ee maintenance graph-snapshot-prune --help",
    )?;

    let witness_prune_help = help_for(&["ee", "maintenance", "graph-witnesses-prune", "--help"])?;
    assert_contains_all(
        &witness_prune_help,
        &[
            "--database",
            "--dry-run",
            "--retention-days",
            "--algorithm-ttl",
        ],
        "ee maintenance graph-witnesses-prune --help",
    )?;

    let wal_checkpoint_help = help_for(&["ee", "maintenance", "wal-checkpoint", "--help"])?;
    assert_contains_all(
        &wal_checkpoint_help,
        &["--database", "--mode", "--dry-run"],
        "ee maintenance wal-checkpoint --help",
    )?;

    let job_run_help = help_for(&["ee", "job", "run", "--help"])?;
    assert_contains_all(
        &job_run_help,
        &[
            "KIND",
            "--database",
            "--dry-run",
            "--time-limit-ms",
            "--item-limit",
        ],
        "ee job run --help",
    )
}

#[test]
fn documented_graph_flag_combinations_parse() -> TestResult {
    for args in [
        &[
            "ee",
            "context",
            "prepare release",
            "--profile",
            "thorough",
            "--ppr-weight",
            "0.5",
            "--explain",
            "--no-pack-dna",
            "--json",
        ][..],
        &[
            "ee",
            "insights",
            "--section",
            "proximityHotspots",
            "--limit",
            "5",
            "--offset",
            "1",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "centrality",
            "--algorithm",
            "pagerank",
            "--limit",
            "10",
            "--require-fresh",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "centrality-refresh",
            "--dry-run",
            "--min-confidence",
            "0.6",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "snapshot",
            "refresh",
            "--graph",
            "memory_links",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "k-core",
            "--k",
            "3",
            "--min-confidence",
            "0.6",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "path",
            "mem_source",
            "mem_target",
            "--min-weight",
            "0.4",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "explain-link",
            "mem_source",
            "mem_target",
            "--link-limit",
            "250",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "feature-enrichment",
            "--dry-run",
            "--min-confidence",
            "0.6",
            "--max-features",
            "25",
            "--min-combined-score",
            "0.2",
            "--max-selection-boost",
            "0.4",
            "--json",
        ][..],
        &[
            "ee",
            "graph",
            "neighborhood",
            "mem_release_policy",
            "--direction",
            "incoming",
            "--relation",
            "supports",
            "--limit",
            "20",
            "--json",
        ][..],
        &[
            "ee",
            "why",
            "mem_failed_release",
            "--causal-explain",
            "--confidence-threshold",
            "0.7",
            "--json",
        ][..],
        &[
            "ee",
            "causal",
            "trace",
            "mem_failed_release",
            "--memory-id",
            "mem_release_policy",
            "--run-id",
            "run_release",
            "--pack-id",
            "pack_release",
            "--preflight-id",
            "preflight_release",
            "--tripwire-id",
            "tripwire_release",
            "--procedure-id",
            "procedure_release",
            "--agent-id",
            "agent_release",
            "--database",
            ".ee/ee.db",
            "--database-workspace-id",
            "ws_release",
            "--limit",
            "5",
            "--depth",
            "3",
            "--include-exposures",
            "--include-outcomes",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "causal",
            "compare",
            "chain_baseline",
            "chain_candidate",
            "--fixture-replay-id",
            "fixture_release",
            "--shadow-run-id",
            "shadow_release",
            "--counterfactual-episode-id",
            "counterfactual_release",
            "--experiment-id",
            "experiment_release",
            "--artifact-id",
            "artifact_release",
            "--decision-id",
            "decision_release",
            "--method",
            "replay",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "causal",
            "estimate",
            "chain_release",
            "--artifact-id",
            "artifact_release",
            "--decision-id",
            "decision_release",
            "--chain-id",
            "chain_release",
            "--agent-id",
            "agent_release",
            "--method",
            "matching",
            "--include-confounders",
            "--include-assumptions",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "causal",
            "promote-plan",
            "chain_release",
            "--artifact-id",
            "artifact_release",
            "--decision-id",
            "decision_release",
            "--estimate-id",
            "estimate_release",
            "--action",
            "promote",
            "--method",
            "replay",
            "--minimum-uplift",
            "0.08",
            "--include-revalidation",
            "--include-narrower-routing",
            "--include-experiment-proposals",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "rule",
            "provenance",
            "rule_release_policy",
            "--database",
            ".ee/ee.db",
            "--json",
        ][..],
        &[
            "ee",
            "maintenance",
            "run",
            "--job",
            "decay_sweep",
            "--no-structural-decay",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "curate",
            "disposition",
            "--no-structural-decay",
            "--now",
            "2026-05-19T00:00:00Z",
            "--json",
        ][..],
        &[
            "ee",
            "job",
            "run",
            "centrality_refresh",
            "--dry-run",
            "--time-limit-ms",
            "500",
            "--item-limit",
            "25",
            "--json",
        ][..],
        &["ee", "status", "--skyline", "--json"][..],
        &[
            "ee",
            "maintenance",
            "graph-witnesses-prune",
            "--dry-run",
            "--retention-days",
            "30",
            "--algorithm-ttl",
            "pagerank=14",
            "--json",
        ][..],
        &[
            "ee",
            "maintenance",
            "wal-checkpoint",
            "--database",
            ".ee/ee.db",
            "--mode",
            "truncate",
            "--dry-run",
            "--json",
        ][..],
        &[
            "ee",
            "diag",
            "causal-edge",
            "--database",
            ".ee/ee.db",
            "--workspace-id",
            "ws_release",
            "--edge-id",
            "edge_release_failure",
            "--failure-id",
            "mem_failed_release",
            "--candidate-cause-id",
            "mem_missing_rch_proof",
            "--contribution-score",
            "0.8",
            "--evidence-uri",
            "file://proof.json",
            "--computed-at",
            "2026-05-19T00:00:00Z",
            "--method",
            "manual",
            "--json",
        ][..],
        &[
            "ee",
            "diag",
            "graph-snapshot",
            "--database",
            ".ee/ee.db",
            "--status",
            "stale",
            "--metrics-json",
            "{\"pagerank\":1}",
            "--node-count",
            "42",
            "--edge-count",
            "77",
            "--source-generation",
            "3",
            "--json",
        ][..],
    ] {
        Cli::try_parse_from(args)
            .map_err(|error| format!("{} failed to parse: {:?}", args.join(" "), error.kind()))?;
    }
    Ok(())
}
