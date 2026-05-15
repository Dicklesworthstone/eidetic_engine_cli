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
            "snapshot",
            "refresh",
            "--graph",
            "memory_links",
            "--dry-run",
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
    ] {
        Cli::try_parse_from(args)
            .map_err(|error| format!("{} failed to parse: {:?}", args.join(" "), error.kind()))?;
    }
    Ok(())
}
