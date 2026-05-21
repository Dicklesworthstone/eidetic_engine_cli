//! Parser-level contract checks for CLI flag precedence.
//!
//! `bd-bife.26` documents how graph, pack, renderer, and maintenance flags
//! compose. These tests pin the current Clap surface without opening a
//! workspace database or executing command handlers.

use clap::Parser;
use clap::error::ErrorKind;
use ee::cli::{
    Cli, Command, CurateCommand, GraphCommand, MaintenanceCommand, OutputFormat, PackCommand,
};
use ee::output::Renderer;

type TestResult = Result<(), String>;

fn parse(args: impl IntoIterator<Item = &'static str>) -> Result<Cli, String> {
    Cli::try_parse_from(args).map_err(|error| format!("unexpected parse error: {:?}", error.kind()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn json_overrides_mermaid_renderer_for_graph_export() -> TestResult {
    let cli = parse([
        "ee",
        "--format",
        "mermaid",
        "--json",
        "graph",
        "export",
        "--snapshot-id",
        "snap_release",
    ])?;

    ensure_equal(&cli.format, &OutputFormat::Mermaid, "requested format")?;
    ensure(cli.json, "json global parsed")?;
    ensure(
        cli.wants_json(),
        "json global forces machine-readable output",
    )?;
    ensure_equal(&cli.renderer(), &Renderer::Json, "effective renderer")
}

#[test]
fn robot_global_forces_json_without_changing_command_shape() -> TestResult {
    let cli = parse(["ee", "--robot", "status", "--skyline"])?;

    ensure(cli.robot, "robot global parsed")?;
    ensure(cli.wants_json(), "robot implies json output")?;
    match cli.command {
        Some(Command::Status(args)) => ensure(args.skyline, "status --skyline parsed"),
        other => Err(format!("expected status command, got {other:?}")),
    }
}

#[test]
fn context_disable_pack_dna_wins_over_explain_output_flag() -> TestResult {
    let cli = parse([
        "ee",
        "context",
        "prepare release",
        "--profile",
        "balanced",
        "--ppr-weight",
        "0",
        "--explain",
        "--no-pack-dna",
    ])?;

    match cli.command {
        Some(Command::Context(args)) => {
            ensure_equal(&args.profile, &"balanced".to_owned(), "context profile")?;
            ensure_equal(&args.ppr_weight, &Some(0.0), "ppr zero weight")?;
            ensure(args.explain, "explain flag parsed")?;
            ensure(args.no_pack_dna, "no-pack-dna suppresses pack DNA output")
        }
        other => Err(format!("expected context command, got {other:?}")),
    }
}

#[test]
fn context_profile_and_positive_ppr_weight_compose() -> TestResult {
    let cli = parse([
        "ee",
        "context",
        "prepare release",
        "--profile",
        "thorough",
        "--ppr-weight",
        "0.7",
        "--candidate-pool",
        "150",
    ])?;

    match cli.command {
        Some(Command::Context(args)) => {
            ensure_equal(&args.profile, &"thorough".to_owned(), "context profile")?;
            ensure_equal(&args.ppr_weight, &Some(0.7), "ppr blend weight")?;
            ensure_equal(&args.candidate_pool, &150, "candidate pool")
        }
        other => Err(format!("expected context command, got {other:?}")),
    }
}

#[test]
fn invalid_ppr_weight_is_rejected_before_command_execution() {
    let error = Cli::try_parse_from(["ee", "context", "prepare release", "--ppr-weight", "1.5"])
        .expect_err("out-of-range ppr-weight should fail");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn optional_boolean_disable_can_be_explicitly_false_for_context() -> TestResult {
    let cli = parse([
        "ee",
        "context",
        "prepare release",
        "--no-coverage-fill=false",
    ])?;

    match cli.command {
        Some(Command::Context(args)) => ensure_equal(
            &args.no_coverage_fill,
            &Some(false),
            "context no-coverage-fill=false",
        ),
        other => Err(format!("expected context command, got {other:?}")),
    }
}

#[test]
fn pack_build_boolean_disables_default_to_true_when_present() -> TestResult {
    let cli = parse([
        "ee",
        "pack",
        "build",
        "--query-file",
        "release.eeq.json",
        "--no-rendered-text",
        "--no-skipped",
        "--no-meta",
    ])?;

    match cli.command {
        Some(Command::Pack(args)) => match args.command {
            Some(PackCommand::Build(build)) => {
                ensure_equal(
                    &build.no_rendered_text,
                    &Some(true),
                    "pack build no-rendered-text",
                )?;
                ensure_equal(&build.no_skipped, &Some(true), "pack build no-skipped")?;
                ensure_equal(&build.no_meta, &Some(true), "pack build no-meta")
            }
            other => Err(format!("expected pack build command, got {other:?}")),
        },
        other => Err(format!("expected pack command, got {other:?}")),
    }
}

#[test]
fn graph_filters_and_require_fresh_parse_together() -> TestResult {
    let cli = parse([
        "ee",
        "graph",
        "centrality",
        "--algorithm",
        "hits-hubs",
        "--memory-id",
        "mem_release_policy",
        "--require-fresh",
        "--limit",
        "25",
    ])?;

    match cli.command {
        Some(Command::Graph(GraphCommand::Centrality(args))) => {
            ensure_equal(&args.algorithm, &"hits-hubs".to_owned(), "algorithm")?;
            ensure_equal(
                &args.memory_id,
                &Some("mem_release_policy".to_owned()),
                "memory id",
            )?;
            ensure(args.require_fresh, "require-fresh parsed")?;
            ensure_equal(&args.limit, &25, "limit")
        }
        other => Err(format!("expected graph centrality command, got {other:?}")),
    }
}

#[test]
fn include_tombstoned_is_read_visibility_for_graph_algorithms() -> TestResult {
    let cli = parse([
        "ee",
        "graph",
        "pagerank",
        "--min-weight",
        "0.2",
        "--min-confidence",
        "0.5",
        "--include-tombstoned",
    ])?;

    match cli.command {
        Some(Command::Graph(GraphCommand::Pagerank(args))) => {
            ensure_equal(&args.min_weight, &Some(0.2), "min weight")?;
            ensure_equal(&args.min_confidence, &Some(0.5), "min confidence")?;
            ensure(args.include_tombstoned, "include tombstoned parsed")
        }
        other => Err(format!("expected graph pagerank command, got {other:?}")),
    }
}

#[test]
fn maintenance_dry_run_and_no_structural_decay_parse_as_independent_controls() -> TestResult {
    let cli = parse([
        "ee",
        "maintenance",
        "run",
        "--job",
        "decay_sweep",
        "--no-structural-decay",
        "--dry-run",
        "--time-limit-ms",
        "500",
        "--item-limit",
        "25",
    ])?;

    match cli.command {
        Some(Command::Maintenance(MaintenanceCommand::Run(args))) => {
            ensure(args.no_structural_decay, "no-structural-decay parsed")?;
            ensure(args.dry_run, "dry-run parsed")?;
            ensure_equal(&args.time_limit_ms, &Some(500), "time limit")?;
            ensure_equal(&args.item_limit, &Some(25), "item limit")
        }
        other => Err(format!("expected maintenance run command, got {other:?}")),
    }
}

#[test]
fn curate_apply_load_bearing_override_is_explicit_and_dry_run_independent() -> TestResult {
    let cli = parse([
        "ee",
        "curate",
        "apply",
        "cand_retract_stale_rule",
        "--allow-tombstone-load-bearing",
        "--dry-run",
    ])?;

    match cli.command {
        Some(Command::Curate(CurateCommand::Apply(args))) => {
            ensure_equal(
                &args.candidate_id,
                &"cand_retract_stale_rule".to_owned(),
                "candidate id",
            )?;
            ensure(
                args.allow_tombstone_load_bearing,
                "load-bearing override parsed",
            )?;
            ensure(args.dry_run, "apply dry-run parsed")
        }
        other => Err(format!("expected curate apply command, got {other:?}")),
    }
}

#[test]
fn curate_tombstone_load_bearing_override_stays_explicit_lifecycle_preview() -> TestResult {
    let cli = parse([
        "ee",
        "curate",
        "tombstone",
        "mem_old_rule",
        "--allow-tombstone-load-bearing",
        "--reason",
        "superseded by validated release rule",
        "--dry-run",
    ])?;

    match cli.command {
        Some(Command::Curate(CurateCommand::Tombstone(args))) => {
            ensure_equal(&args.memory_id, &"mem_old_rule".to_owned(), "memory id")?;
            ensure(args.dry_run, "tombstone dry-run parsed")?;
            ensure(
                args.allow_tombstone_load_bearing,
                "load-bearing tombstone override parsed",
            )?;
            ensure_equal(
                &args.reason,
                &Some("superseded by validated release rule".to_owned()),
                "reason",
            )
        }
        other => Err(format!("expected curate tombstone command, got {other:?}")),
    }
}
