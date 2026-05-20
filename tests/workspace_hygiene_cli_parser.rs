//! bd-1eq3l.3 CLI parser coverage for `ee workspace hygiene`.
//!
//! The acceptance criterion is "CLI parser tests cover command selection,
//! workspace, JSON, and human mode." Three inline parser tests inside
//! `src/cli/mod.rs` cover command selection, the per-arg shapes, and human
//! mode. This external file extends that coverage with the combinations and
//! edge cases that would otherwise drift silently as the parser evolves:
//!
//! - JSON-mode flag propagation alongside the hygiene subcommand
//! - Both `--agent-name` and `--agent-mail-snapshot` set together
//! - Bare `ee workspace hygiene` with no per-command flags
//! - `--workspace` plus `--agent-name` combined with `--json`
//! - Unknown flags are rejected before any side effect
//!
//! The tests intentionally use `Cli::try_parse_from` directly so they do not
//! require a built binary and run anywhere the crate compiles.

use std::path::PathBuf;

use clap::Parser;
use clap::error::ErrorKind;
use ee::cli::{Cli, Command, WorkspaceCommand, WorkspaceHygieneArgs, WorkspaceHygieneMode};

type TestResult = Result<(), String>;

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(args.iter().copied())
}

fn ensure_hygiene_command(cli: &Cli, expected: WorkspaceHygieneArgs) -> TestResult {
    let actual = match cli.command.as_ref() {
        Some(Command::Workspace(WorkspaceCommand::Hygiene(args))) => args.clone(),
        other => {
            return Err(format!("expected workspace hygiene command, got {other:?}"));
        }
    };
    if actual != expected {
        return Err(format!("expected {expected:?}, got {actual:?}"));
    }
    Ok(())
}

#[test]
fn json_flag_propagates_alongside_workspace_hygiene() -> TestResult {
    let cli = parse(&["ee", "--json", "workspace", "hygiene"])
        .map_err(|error| format!("parse failed: {:?}", error.kind()))?;
    if !cli.json {
        return Err(format!(
            "--json must propagate to Cli.json for workspace hygiene; got cli={cli:?}"
        ));
    }
    ensure_hygiene_command(&cli, WorkspaceHygieneArgs::default())
}

#[test]
fn workspace_hygiene_accepts_agent_name_and_snapshot_together() -> TestResult {
    let cli = parse(&[
        "ee",
        "--json",
        "workspace",
        "hygiene",
        "--agent-name",
        "GrayForest",
        "--agent-mail-snapshot",
        "tmp/agent-mail.json",
    ])
    .map_err(|error| format!("parse failed: {:?}", error.kind()))?;
    ensure_hygiene_command(
        &cli,
        WorkspaceHygieneArgs {
            agent_name: Some("GrayForest".to_string()),
            agent_mail_snapshot: Some(PathBuf::from("tmp/agent-mail.json")),
            mode: WorkspaceHygieneMode::Report,
            strict_advisory: false,
        },
    )
}

#[test]
fn bare_workspace_hygiene_defaults_optional_args_to_none() -> TestResult {
    let cli = parse(&["ee", "workspace", "hygiene"])
        .map_err(|error| format!("parse failed: {:?}", error.kind()))?;
    if cli.json {
        return Err(format!(
            "default mode must be human (cli.json=false), got {cli:?}"
        ));
    }
    ensure_hygiene_command(&cli, WorkspaceHygieneArgs::default())
}

#[test]
fn workspace_hygiene_combines_with_top_level_workspace_and_json() -> TestResult {
    let cli = parse(&[
        "ee",
        "--json",
        "--workspace",
        "fixtures/hygiene",
        "workspace",
        "hygiene",
        "--agent-name",
        "GrayForest",
    ])
    .map_err(|error| format!("parse failed: {:?}", error.kind()))?;
    if cli.workspace.as_deref() != Some(std::path::Path::new("fixtures/hygiene")) {
        return Err(format!(
            "--workspace must propagate to Cli.workspace for hygiene; got {:?}",
            cli.workspace
        ));
    }
    if !cli.json {
        return Err(format!("--json must propagate; got cli={cli:?}"));
    }
    ensure_hygiene_command(
        &cli,
        WorkspaceHygieneArgs {
            agent_name: Some("GrayForest".to_string()),
            agent_mail_snapshot: None,
            mode: WorkspaceHygieneMode::Report,
            strict_advisory: false,
        },
    )
}

#[test]
fn workspace_hygiene_accepts_precommit_advisory_mode() -> TestResult {
    let cli = parse(&[
        "ee",
        "--json",
        "workspace",
        "hygiene",
        "--mode",
        "precommit",
        "--strict-advisory",
    ])
    .map_err(|error| format!("parse failed: {:?}", error.kind()))?;
    ensure_hygiene_command(
        &cli,
        WorkspaceHygieneArgs {
            agent_name: None,
            agent_mail_snapshot: None,
            mode: WorkspaceHygieneMode::Precommit,
            strict_advisory: true,
        },
    )
}

#[test]
fn workspace_hygiene_rejects_unknown_advisory_mode() {
    let err = parse(&["ee", "workspace", "hygiene", "--mode", "destructive"])
        .expect_err("unknown mode must fail parsing");
    assert!(
        matches!(
            err.kind(),
            ErrorKind::InvalidValue | ErrorKind::ValueValidation
        ),
        "unknown mode should produce a clap value error, got {:?}",
        err.kind()
    );
}

#[test]
fn workspace_hygiene_rejects_unknown_flag() {
    let err = parse(&["ee", "workspace", "hygiene", "--definitely-not-a-real-flag"])
        .expect_err("unknown flag must fail parsing");
    let kind = err.kind();
    assert!(
        matches!(
            kind,
            ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand | ErrorKind::DisplayHelp
        ),
        "unknown flag should produce a clap parse error, got {kind:?}"
    );
}
