//! Public `ee workspace hygiene` degraded-emission coverage.
//!
//! bd-1eq3l.11.1 requires proof that cataloged workspace-hygiene
//! degraded codes are visible through the CLI surface, not just listed
//! in fixtures. This test keeps the scenario intentionally small: an
//! isolated git repo with one dirty file exercises the real binary and
//! asserts the stable JSON and human summaries expose the codes that the
//! current public collector can deterministically emit.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

const OVERSIZED_BEADS_JSONL_LEN: usize = 8 * 1024 * 1024 + 1024;
const SYNTHETIC_RAW_SECRET: &str = "sk-proj-WORKSPACEHYGIENEPUBLICSYNTHETIC000000000000";
const SYNTHETIC_BENIGN_LOOKALIKE: &str = "sk-proj-example-placeholder";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoverageKind {
    PublicTriggered,
    FixtureOnly { rationale: &'static str },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CoverageEntry {
    code: &'static str,
    kind: CoverageKind,
}

const WORKSPACE_HYGIENE_COVERAGE: &[CoverageEntry] = &[
    CoverageEntry {
        code: "workspace_hygiene_agent_mail_timeout",
        kind: CoverageKind::FixtureOnly {
            rationale: "the current public CLI hardcodes Agent Mail as unavailable; timeout requires an injectable slow Agent Mail provider",
        },
    },
    CoverageEntry {
        code: "workspace_hygiene_agent_mail_unavailable",
        kind: CoverageKind::PublicTriggered,
    },
    CoverageEntry {
        code: "workspace_hygiene_beads_content_not_provided",
        kind: CoverageKind::FixtureOnly {
            rationale: "public triggering requires unreadable .beads/issues.jsonl; permission-sensitive temp files are kept out of this no-cleanup test suite",
        },
    },
    CoverageEntry {
        code: "workspace_hygiene_beads_db_divergence_unknown",
        kind: CoverageKind::PublicTriggered,
    },
    CoverageEntry {
        code: "workspace_hygiene_beads_jsonl_truncated",
        kind: CoverageKind::PublicTriggered,
    },
    CoverageEntry {
        code: "workspace_hygiene_beads_parse_error",
        kind: CoverageKind::PublicTriggered,
    },
    CoverageEntry {
        code: "workspace_hygiene_beads_reserved",
        kind: CoverageKind::FixtureOnly {
            rationale: "the current public CLI passes no Beads reservation snapshot; reservation-driven emission is pure-input covered until a public snapshot input exists",
        },
    },
    CoverageEntry {
        code: "workspace_hygiene_beads_self_reservation",
        kind: CoverageKind::FixtureOnly {
            rationale: "the current public CLI passes no self-reservation snapshot; self-reservation emission is pure-input covered until a public snapshot input exists",
        },
    },
    CoverageEntry {
        code: "workspace_hygiene_beads_unavailable",
        kind: CoverageKind::FixtureOnly {
            rationale: "the current public CLI reads only bounded JSONL content and does not yet collect Beads DB/command availability",
        },
    },
    CoverageEntry {
        code: "workspace_hygiene_config_invalid",
        kind: CoverageKind::FixtureOnly {
            rationale: "invalid classifier config is cataloged but the public hygiene command currently returns configuration errors rather than degraded success",
        },
    },
    CoverageEntry {
        code: "workspace_hygiene_output_truncated",
        kind: CoverageKind::FixtureOnly {
            rationale: "the public path exists but needs a giant dirty fixture; deterministic trigger coverage lives in the core workspace-hygiene truncation unit test",
        },
    },
    CoverageEntry {
        code: "workspace_hygiene_partial_metadata",
        kind: CoverageKind::PublicTriggered,
    },
    CoverageEntry {
        code: "workspace_hygiene_secret_scan_skipped",
        kind: CoverageKind::PublicTriggered,
    },
];

fn run_command(command: &mut Command, context: &str) -> Result<Output, String> {
    command
        .output()
        .map_err(|error| format!("{context}: failed to run command: {error}"))
}

fn ensure_success(output: &Output, context: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{context}: exit {:?}; stdout: {}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).trim_end(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        ))
    }
}

fn workspace_dir() -> Result<PathBuf, String> {
    let mut root = std::env::var("EE_E2E_TMPDIR")
        .or_else(|_| std::env::var("TMPDIR"))
        .unwrap_or_else(|_| "/tmp".to_owned());
    if root.starts_with("/Volumes/") {
        root = "/tmp".to_owned();
    }
    let temp = tempfile::Builder::new()
        .prefix("ee-workspace-hygiene-public-")
        .tempdir_in(root)
        .map_err(|error| format!("tempdir: {error}"))?;
    Ok(temp.keep())
}

fn init_dirty_git_workspace() -> Result<PathBuf, String> {
    let workspace = workspace_dir()?;
    fs::write(workspace.join("notes.md"), "workspace hygiene fixture\n")
        .map_err(|error| format!("write fixture file: {error}"))?;
    ensure_success(
        &run_command(
            Command::new("git")
                .arg("init")
                .arg("-b")
                .arg("main")
                .current_dir(&workspace),
            "git init",
        )?,
        "git init",
    )?;
    Ok(workspace)
}

fn write_oversized_beads_jsonl(workspace: &Path) -> TestResult {
    let beads_dir = workspace.join(".beads");
    fs::create_dir_all(&beads_dir).map_err(|error| format!("create .beads dir: {error}"))?;
    let mut body = Vec::with_capacity(OVERSIZED_BEADS_JSONL_LEN);
    body.extend_from_slice(b"{\"id\":\"bd-public\",\"title\":\"public emission fixture\"}\n");
    body.resize(OVERSIZED_BEADS_JSONL_LEN, b'\n');
    fs::write(beads_dir.join("issues.jsonl"), body)
        .map_err(|error| format!("write .beads/issues.jsonl: {error}"))?;
    Ok(())
}

fn write_invalid_beads_jsonl(workspace: &Path) -> TestResult {
    let beads_dir = workspace.join(".beads");
    fs::create_dir_all(&beads_dir).map_err(|error| format!("create .beads dir: {error}"))?;
    fs::write(
        beads_dir.join("issues.jsonl"),
        "{\"id\":\"bd-public\"}\n{not valid json\n",
    )
    .map_err(|error| format!("write invalid .beads/issues.jsonl: {error}"))?;
    Ok(())
}

fn write_synthetic_secret_fixture(workspace: &Path) -> TestResult {
    fs::write(
        workspace.join(".env.local"),
        format!(
            "OPENAI_API_KEY={SYNTHETIC_RAW_SECRET}\n# benign lookalike: {SYNTHETIC_BENIGN_LOOKALIKE}\n"
        ),
    )
    .map_err(|error| format!("write synthetic secret fixture: {error}"))?;
    Ok(())
}

fn run_ee(args: &[&str], workspace: &Path, context: &str) -> Result<Output, String> {
    run_command(
        Command::new(env!("CARGO_BIN_EXE_ee"))
            .args(args)
            .arg("--workspace")
            .arg(workspace)
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY"),
        context,
    )
}

fn parse_json(output: &Output, context: &str) -> Result<Value, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{context}: stdout must be JSON: {error}; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn degraded_codes(value: &Value) -> Vec<&str> {
    value
        .pointer("/data/degraded")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn string_array_at<'a>(value: &'a Value, pointer: &str) -> Vec<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

#[test]
fn workspace_hygiene_degraded_code_coverage_matrix_is_explicit() -> TestResult {
    let mut matrix_codes = std::collections::BTreeSet::new();
    for entry in WORKSPACE_HYGIENE_COVERAGE {
        if !matrix_codes.insert(entry.code) {
            return Err(format!(
                "duplicate workspace hygiene coverage row: {}",
                entry.code
            ));
        }
        if let CoverageKind::FixtureOnly { rationale } = entry.kind {
            if rationale.trim().is_empty() {
                return Err(format!(
                    "fixture-only row for {} needs rationale",
                    entry.code
                ));
            }
        }

        let fixture = workspace_hygiene_fixture(entry.code)?;
        if fixture.pointer("/code").and_then(Value::as_str) != Some(entry.code) {
            return Err(format!(
                "fixture code mismatch for {}; fixture: {fixture}",
                entry.code
            ));
        }
        if fixture
            .pointer("/trigger/invocation")
            .and_then(Value::as_str)
            != Some("ee workspace hygiene --workspace . --json")
        {
            return Err(format!(
                "fixture for {} must pin the public workspace hygiene invocation",
                entry.code
            ));
        }
    }

    let fixture_codes = workspace_hygiene_fixture_codes()?;
    if matrix_codes != fixture_codes {
        return Err(format!(
            "workspace hygiene coverage matrix mismatch; matrix={matrix_codes:?}; fixtures={fixture_codes:?}"
        ));
    }
    Ok(())
}

fn workspace_hygiene_fixture(code: &str) -> Result<Value, String> {
    let path = workspace_hygiene_fixture_dir().join(format!("{code}.json"));
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn workspace_hygiene_fixture_codes() -> Result<std::collections::BTreeSet<&'static str>, String> {
    let mut codes = std::collections::BTreeSet::new();
    for entry in WORKSPACE_HYGIENE_COVERAGE {
        codes.insert(entry.code);
    }

    let mut fixture_codes = std::collections::BTreeSet::new();
    for entry in fs::read_dir(workspace_hygiene_fixture_dir())
        .map_err(|error| format!("read failure-mode fixtures: {error}"))?
    {
        let path = entry
            .map_err(|error| format!("read failure-mode fixture entry: {error}"))?
            .path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(code) = file_name.strip_suffix(".json") else {
            continue;
        };
        if !code.starts_with("workspace_hygiene_") {
            continue;
        }
        let Some(matrix_code) = codes.get(code) else {
            return Err(format!(
                "workspace hygiene fixture {file_name} needs a coverage matrix row"
            ));
        };
        fixture_codes.insert(*matrix_code);
    }
    Ok(fixture_codes)
}

fn workspace_hygiene_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("failure_modes")
}

#[test]
fn workspace_hygiene_json_public_surface_emits_degraded_codes() -> TestResult {
    let workspace = init_dirty_git_workspace()?;
    let output = run_ee(
        &[
            "--json",
            "workspace",
            "hygiene",
            "--agent-name",
            "IvoryCondor",
        ],
        &workspace,
        "workspace hygiene json",
    )?;
    ensure_success(&output, "workspace hygiene json")?;
    let value = parse_json(&output, "workspace hygiene json")?;

    if value["schema"] != "ee.response.v1" {
        return Err(format!("unexpected response schema: {}", value["schema"]));
    }
    if value["success"] != true {
        return Err(format!(
            "workspace hygiene response was not success: {value}"
        ));
    }
    if value.pointer("/data/schema").and_then(Value::as_str) != Some("ee.workspace_hygiene.v1") {
        return Err(format!("missing workspace hygiene schema in data: {value}"));
    }
    if value.pointer("/data/command").and_then(Value::as_str) != Some("workspace hygiene") {
        return Err(format!(
            "missing command in workspace hygiene data: {value}"
        ));
    }

    let codes = degraded_codes(&value);
    for expected in [
        "workspace_hygiene_agent_mail_unavailable",
        "workspace_hygiene_partial_metadata",
    ] {
        if !codes.contains(&expected) {
            return Err(format!(
                "workspace hygiene JSON missing degraded code {expected}; got {codes:?}"
            ));
        }
    }

    let coordination_codes = string_array_at(&value, "/data/coordinationState/degradedCodes");
    if !coordination_codes.contains(&"workspace_hygiene_agent_mail_unavailable") {
        return Err(format!(
            "coordinationState missing Agent Mail degraded code; got {coordination_codes:?}; response: {value}"
        ));
    }
    if value
        .pointer("/data/coordinationState/agentMailAvailable")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err(format!(
            "coordinationState must report Agent Mail unavailable: {value}"
        ));
    }

    let next_actions = string_array_at(&value, "/data/nextActions");
    if !next_actions
        .iter()
        .any(|action| action.contains("Refresh Agent Mail reservations"))
    {
        return Err(format!(
            "workspace hygiene JSON missing Agent Mail recovery next action; got {next_actions:?}"
        ));
    }
    Ok(())
}

#[test]
fn workspace_hygiene_json_public_surface_emits_beads_degraded_codes() -> TestResult {
    let workspace = init_dirty_git_workspace()?;
    write_oversized_beads_jsonl(&workspace)?;
    let output = run_ee(
        &[
            "--json",
            "workspace",
            "hygiene",
            "--agent-name",
            "IvoryCondor",
        ],
        &workspace,
        "workspace hygiene json beads degraded",
    )?;
    ensure_success(&output, "workspace hygiene json beads degraded")?;
    let value = parse_json(&output, "workspace hygiene json beads degraded")?;

    let codes = degraded_codes(&value);
    for expected in [
        "workspace_hygiene_beads_db_divergence_unknown",
        "workspace_hygiene_beads_jsonl_truncated",
        "workspace_hygiene_secret_scan_skipped",
    ] {
        if !codes.contains(&expected) {
            return Err(format!(
                "workspace hygiene JSON missing Beads degraded code {expected}; got {codes:?}; response: {value}"
            ));
        }
    }
    if value
        .pointer("/data/secretScan/skippedContentScanCount")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        == 0
    {
        return Err(format!(
            "secretScan must report skipped content scan count for oversized dirty files: {value}"
        ));
    }
    if value
        .pointer("/data/secretScan/maxFileBytes")
        .and_then(Value::as_u64)
        != Some(64 * 1024)
    {
        return Err(format!(
            "secretScan must expose the default per-file cap: {value}"
        ));
    }

    let beads_codes = string_array_at(&value, "/data/beadsState/degradedCodes");
    for expected in [
        "workspace_hygiene_beads_db_divergence_unknown",
        "workspace_hygiene_beads_jsonl_truncated",
    ] {
        if !beads_codes.contains(&expected) {
            return Err(format!(
                "beadsState missing degraded code {expected}; got {beads_codes:?}; response: {value}"
            ));
        }
    }

    if value
        .pointer("/data/beadsState/jsonlPosture/untracked")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(format!(
            "beadsState must report untracked .beads/issues.jsonl posture: {value}"
        ));
    }
    Ok(())
}

#[test]
fn workspace_hygiene_json_public_surface_emits_beads_parse_error() -> TestResult {
    let workspace = init_dirty_git_workspace()?;
    write_invalid_beads_jsonl(&workspace)?;
    let output = run_ee(
        &[
            "--json",
            "workspace",
            "hygiene",
            "--agent-name",
            "IvoryCondor",
        ],
        &workspace,
        "workspace hygiene json beads parse error",
    )?;
    ensure_success(&output, "workspace hygiene json beads parse error")?;
    let value = parse_json(&output, "workspace hygiene json beads parse error")?;

    let codes = degraded_codes(&value);
    if !codes.contains(&"workspace_hygiene_beads_parse_error") {
        return Err(format!(
            "workspace hygiene JSON missing Beads parse-error degraded code; got {codes:?}; response: {value}"
        ));
    }

    let beads_codes = string_array_at(&value, "/data/beadsState/degradedCodes");
    if !beads_codes.contains(&"workspace_hygiene_beads_parse_error") {
        return Err(format!(
            "beadsState missing parse-error degraded code; got {beads_codes:?}; response: {value}"
        ));
    }
    if value
        .pointer("/data/beadsState/parseErrorLine")
        .and_then(Value::as_u64)
        != Some(2)
    {
        return Err(format!(
            "beadsState must report the invalid JSONL line number: {value}"
        ));
    }
    Ok(())
}

#[test]
fn workspace_hygiene_public_surfaces_do_not_leak_secret_file_content() -> TestResult {
    let workspace = init_dirty_git_workspace()?;
    write_synthetic_secret_fixture(&workspace)?;

    let json_output = run_ee(
        &[
            "--json",
            "workspace",
            "hygiene",
            "--agent-name",
            "IvoryCondor",
        ],
        &workspace,
        "workspace hygiene json secret no-leak",
    )?;
    ensure_success(&json_output, "workspace hygiene json secret no-leak")?;
    let json_stdout = String::from_utf8(json_output.stdout)
        .map_err(|error| format!("workspace hygiene JSON stdout UTF-8: {error}"))?;
    assert_no_synthetic_secret("workspace hygiene JSON", &json_stdout)?;
    let value: Value = serde_json::from_str(&json_stdout)
        .map_err(|error| format!("workspace hygiene JSON parse: {error}"))?;

    let secret_row = value
        .pointer("/data/pathClassifications")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.pointer("/path").and_then(Value::as_str) == Some(".env.local"))
        })
        .ok_or_else(|| "workspace hygiene JSON missing .env.local classification row".to_owned())?;
    if secret_row.pointer("/kind").and_then(Value::as_str) != Some("secret_risk") {
        return Err(format!(
            ".env.local should be classified as secret_risk: {secret_row}"
        ));
    }
    if secret_row.pointer("/bucket").and_then(Value::as_str) != Some("do_not_commit") {
        return Err(format!(
            ".env.local should be do_not_commit while untracked: {secret_row}"
        ));
    }
    if !string_array_at(secret_row, "/reasons").contains(&"secret_path_pattern") {
        return Err(format!(
            ".env.local should include secret_path_pattern reason: {secret_row}"
        ));
    }

    let human_output = run_ee(
        &["workspace", "hygiene", "--agent-name", "IvoryCondor"],
        &workspace,
        "workspace hygiene human secret no-leak",
    )?;
    ensure_success(&human_output, "workspace hygiene human secret no-leak")?;
    let human_stdout = String::from_utf8(human_output.stdout)
        .map_err(|error| format!("workspace hygiene human stdout UTF-8: {error}"))?;
    assert_no_synthetic_secret("workspace hygiene human", &human_stdout)
}

fn assert_no_synthetic_secret(surface: &str, rendered: &str) -> TestResult {
    if rendered.contains(SYNTHETIC_RAW_SECRET) {
        return Err(format!("{surface} leaked the raw synthetic secret"));
    }
    if rendered.contains(SYNTHETIC_BENIGN_LOOKALIKE) {
        return Err(format!("{surface} leaked benign lookalike fixture content"));
    }
    Ok(())
}

#[test]
fn workspace_hygiene_human_public_surface_summarizes_degraded_state() -> TestResult {
    let workspace = init_dirty_git_workspace()?;
    let output = run_ee(
        &["workspace", "hygiene", "--agent-name", "IvoryCondor"],
        &workspace,
        "workspace hygiene human",
    )?;
    ensure_success(&output, "workspace hygiene human")?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("workspace hygiene human stdout UTF-8: {error}"))?;

    for expected in [
        "Workspace hygiene:",
        "Degraded:",
        "workspace_hygiene_agent_mail_unavailable",
        "workspace_hygiene_partial_metadata",
    ] {
        if !stdout.contains(expected) {
            return Err(format!(
                "workspace hygiene human output missing {expected:?}; stdout:\n{stdout}"
            ));
        }
    }
    Ok(())
}
