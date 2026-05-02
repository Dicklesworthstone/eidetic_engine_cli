//! Gate 8 contract tests for the default franken-agent-detection profile.
//!
//! These tests keep `ee agent detect` as a thin, local-first detector wrapper:
//! no connector-backed history readers by default, deterministic root override
//! fixtures, stable aliases, and `ee.response.v1` JSON envelopes.

use ee::core::agent_detect::{
    AgentDetectOptions, AgentDetectRootOverride, AgentSourcesOptions, build_agent_sources_report,
    default_probe_paths_tilde, detect_fixture_agents, detect_installed_agents, fixture_overrides,
    fixtures_path, remote_mirror_fixture_overrides, remote_mirror_path_rewrites,
    rewrite_agent_source_path,
};
use ee::core::status::StatusReport;
use ee::output::{render_agent_detect_json, render_status_json};
use serde_json::Value as JsonValue;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const FORBIDDEN_CRATES: &[&str] = &[
    "tokio",
    "tokio-util",
    "async-std",
    "smol",
    "rusqlite",
    "sqlx",
    "diesel",
    "sea-orm",
    "petgraph",
    "hyper",
    "axum",
    "tower",
    "reqwest",
];

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: T, expected: T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn golden_path(name: &str) -> PathBuf {
    repo_path("tests")
        .join("fixtures")
        .join("golden")
        .join("agent_detect")
        .join(format!("{name}.json.golden"))
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn parse_json(raw: &str, context: &str) -> Result<JsonValue, String> {
    serde_json::from_str(raw).map_err(|error| format!("{context} must be JSON: {error}"))
}

fn normalize_fixture_paths(value: &mut JsonValue) {
    match value {
        JsonValue::String(raw) => {
            let fixture = fixtures_path().to_string_lossy().replace('\\', "/");
            *raw = raw.replace(&fixture, "__FIXTURE_PATH__");
        }
        JsonValue::Array(items) => {
            for item in items {
                normalize_fixture_paths(item);
            }
        }
        JsonValue::Object(fields) => {
            for (key, nested) in fields {
                if key == "generatedAt" && nested.is_string() {
                    *nested = JsonValue::String("TIMESTAMP".to_string());
                } else {
                    normalize_fixture_paths(nested);
                }
            }
        }
        _ => {}
    }
}

fn normalized_agent_detect_json(raw: &str) -> Result<String, String> {
    let mut value = parse_json(raw, "agent detection output")?;
    normalize_fixture_paths(&mut value);
    serde_json::to_string_pretty(&value)
        .map(|json| format!("{json}\n"))
        .map_err(|error| format!("failed to format normalized JSON: {error}"))
}

fn assert_agent_detect_golden(name: &str, raw: &str) -> TestResult {
    let normalized = normalized_agent_detect_json(raw)?;
    let path = golden_path(name);
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    ensure(
        normalized == expected,
        format!(
            "agent-detect golden mismatch for {name}\n--- expected\n{expected}\n+++ actual\n{normalized}"
        ),
    )
}

fn assert_json_stdout(
    output: Output,
    expect_success: bool,
    context: &str,
) -> Result<String, String> {
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{context} stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("{context} stderr was not UTF-8: {error}"))?;
    ensure(
        output.status.success() == expect_success,
        format!(
            "{context} exit status mismatch: got {:?}, stderr: {stderr}",
            output.status.code()
        ),
    )?;
    ensure(
        stderr.is_empty(),
        format!("{context} stderr must be empty, got: {stderr:?}"),
    )?;
    ensure(
        stdout.starts_with('{') && stdout.ends_with('\n'),
        format!("{context} stdout must be newline-terminated JSON, got: {stdout:?}"),
    )?;
    Ok(stdout)
}

#[test]
fn default_feature_profile_keeps_connector_readers_disabled() -> TestResult {
    let manifest_text = fs::read_to_string(repo_path("Cargo.toml"))
        .map_err(|error| format!("failed to read Cargo.toml: {error}"))?;
    let manifest = manifest_text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("Cargo.toml parse error: {error}"))?;
    let dependency = manifest
        .get("dependencies")
        .and_then(toml_edit::Item::as_table)
        .and_then(|dependencies| dependencies.get("franken-agent-detection"))
        .and_then(toml_edit::Item::as_inline_table)
        .ok_or_else(|| {
            "Cargo.toml dependency `franken-agent-detection` must be an inline table".to_string()
        })?;

    ensure_equal(
        dependency
            .get("default-features")
            .and_then(toml_edit::Value::as_bool),
        Some(false),
        "franken-agent-detection default-features",
    )?;
    ensure(
        dependency
            .get("features")
            .and_then(toml_edit::Value::as_array)
            .is_none_or(|features| features.is_empty()),
        "franken-agent-detection must not enable connector-backed features by default",
    )?;
    ensure(
        !manifest_text.contains("franken-agent-detection = { workspace = true"),
        "franken-agent-detection profile must stay explicit in this crate",
    )?;

    let forbidden_hits: Vec<&str> = FORBIDDEN_CRATES
        .iter()
        .copied()
        .filter(|crate_name| {
            manifest_text.contains(&format!("{crate_name} ="))
                || manifest_text.contains(&format!("{crate_name} = {{"))
        })
        .collect();
    ensure(
        forbidden_hits.is_empty(),
        format!("Cargo.toml directly enables forbidden crates: {forbidden_hits:?}"),
    )
}

#[test]
fn root_overrides_detected_report_matches_golden() -> TestResult {
    let report = detect_fixture_agents().map_err(|error| error.to_string())?;
    ensure_equal(&report.format_version, &1, "upstream format version")?;
    ensure_equal(&report.summary.detected_count, &4, "detected fixtures")?;
    ensure_equal(&report.summary.total_count, &4, "total fixtures")?;

    assert_agent_detect_golden("detected", &render_agent_detect_json(&report))
}

#[test]
fn root_overrides_include_undetected_report_matches_golden() -> TestResult {
    let fixtures = fixtures_path();
    let mut overrides = fixture_overrides(&fixtures);
    overrides.push(AgentDetectRootOverride {
        slug: "copilot-cli".to_string(),
        root: fixtures.join("copilot_cli").join("session-state"),
    });

    let report = detect_installed_agents(&AgentDetectOptions {
        only_connectors: Some(vec![
            "claude-code".to_string(),
            "codex-cli".to_string(),
            "copilot-cli".to_string(),
            "gemini-cli".to_string(),
        ]),
        include_undetected: true,
        root_overrides: overrides,
    })
    .map_err(|error| error.to_string())?;

    ensure_equal(&report.summary.detected_count, &3, "detected count")?;
    ensure_equal(&report.summary.total_count, &4, "total count")?;
    ensure(
        report
            .installed_agents
            .iter()
            .any(|agent| agent.slug == "copilot_cli" && !agent.detected),
        "copilot-cli alias must normalize to undetected copilot_cli",
    )?;

    assert_agent_detect_golden("include_undetected", &render_agent_detect_json(&report))
}

#[test]
fn remote_mirror_origin_fixture_detects_and_rewrites_agent_roots() -> TestResult {
    let fixtures = fixtures_path();
    let report = detect_installed_agents(&AgentDetectOptions {
        only_connectors: Some(vec!["claude-code".to_string(), "codex-cli".to_string()]),
        include_undetected: false,
        root_overrides: remote_mirror_fixture_overrides(&fixtures),
    })
    .map_err(|error| error.to_string())?;

    ensure_equal(&report.summary.detected_count, &2, "detected count")?;
    ensure_equal(&report.summary.total_count, &2, "total count")?;

    let rewrites = remote_mirror_path_rewrites(&fixtures);
    let rewritten = rewrite_agent_source_path(
        &rewrites,
        "claude-code",
        "/home/agent/.claude/projects/eidetic_engine_cli/session.jsonl",
    )
    .ok_or_else(|| "remote claude path should rewrite".to_string())?;

    ensure(
        rewritten.ends_with(
            "/remote_mirror/ssh-csd/home/agent/.claude/projects/eidetic_engine_cli/session.jsonl",
        ),
        format!("unexpected rewritten path: {rewritten}"),
    )
}

#[test]
fn agent_sources_origin_fixtures_report_filters_to_requested_connector() -> TestResult {
    let report = build_agent_sources_report(&AgentSourcesOptions {
        only: Some("codex-cli".to_string()),
        include_paths: true,
        include_origin_fixtures: true,
        fixtures_root: Some(fixtures_path()),
    });
    ensure_equal(&report.total_count, &1, "filtered source count")?;
    let source = report
        .sources
        .first()
        .ok_or_else(|| "expected one filtered source".to_string())?;
    ensure_equal(&source.slug.as_str(), &"codex", "source slug")?;
    let expected_connectors = vec!["codex".to_string()];
    let origin_fixture = report
        .origin_fixtures
        .first()
        .ok_or_else(|| "expected one origin fixture".to_string())?;
    ensure_equal(
        &origin_fixture.connector_slugs,
        &expected_connectors,
        "filtered origin fixture connectors",
    )?;
    ensure_equal(&report.path_rewrites.len(), &1, "filtered rewrite count")
}

#[test]
fn agent_sources_cli_json_includes_origin_fixtures() -> TestResult {
    let stdout = assert_json_stdout(
        run_ee(&[
            "--json",
            "agent",
            "sources",
            "--only",
            "codex-cli",
            "--include-paths",
            "--include-origin-fixtures",
        ])?,
        true,
        "ee agent sources origin fixtures",
    )?;
    assert_agent_detect_golden("sources_origin_fixtures", &stdout)
}

#[test]
fn alias_normalization_is_stable_for_common_cli_names() -> TestResult {
    let aliases = [
        ("codex-cli", "codex"),
        ("claude-code", "claude"),
        ("gemini-cli", "gemini"),
        ("copilot-cli", "copilot_cli"),
    ];

    for (alias, canonical) in aliases {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp.path().join(canonical);
        fs::create_dir_all(&root)
            .map_err(|error| format!("failed to create {}: {error}", root.display()))?;
        let report = detect_installed_agents(&AgentDetectOptions {
            only_connectors: Some(vec![alias.to_string()]),
            include_undetected: false,
            root_overrides: vec![AgentDetectRootOverride {
                slug: alias.to_string(),
                root,
            }],
        })
        .map_err(|error| format!("{alias} detection failed: {error}"))?;

        ensure_equal(&report.summary.detected_count, &1, "detected count")?;
        ensure_equal(&report.summary.total_count, &1, "total count")?;
        let entry = report
            .installed_agents
            .first()
            .ok_or_else(|| format!("{alias} did not return an installed agent"))?;
        ensure_equal(&entry.slug.as_str(), &canonical, "canonical alias")?;
        ensure(entry.detected, format!("{alias} should be detected"))?;
    }

    Ok(())
}

#[test]
fn unknown_connector_json_matches_golden() -> TestResult {
    let stdout = assert_json_stdout(
        run_ee(&["--json", "agent", "detect", "--only", "not-a-real-agent"])?,
        false,
        "ee agent detect unknown connector",
    )?;
    assert_agent_detect_golden("unknown_connector", &stdout)
}

#[test]
fn agent_detect_cli_json_uses_response_envelope() -> TestResult {
    let stdout = assert_json_stdout(
        run_ee(&[
            "--json",
            "agent",
            "detect",
            "--only",
            "codex-cli",
            "--include-undetected",
        ])?,
        true,
        "ee agent detect --json",
    )?;
    let value = parse_json(&stdout, "ee agent detect --json stdout")?;

    ensure_equal(
        value.get("schema"),
        Some(&JsonValue::String("ee.response.v1".to_string())),
        "response schema",
    )?;
    ensure_equal(
        value.pointer("/data/command"),
        Some(&JsonValue::String("agent detect".to_string())),
        "command",
    )?;
    ensure_equal(
        value.pointer("/data/formatVersion"),
        Some(&JsonValue::from(1)),
        "upstream format version",
    )?;
    ensure(
        value
            .pointer("/data/generatedAt")
            .is_some_and(JsonValue::is_string),
        "generatedAt should preserve upstream detector timestamp",
    )
}

#[test]
fn status_json_reports_agent_inventory_without_cass_import_requirement() -> TestResult {
    let report = StatusReport::gather();
    let value = parse_json(&render_status_json(&report), "status JSON")?;

    ensure_equal(
        value.pointer("/data/capabilities/agentDetection"),
        Some(&JsonValue::String("ready".to_string())),
        "agent detection capability",
    )?;
    ensure_equal(
        value.pointer("/data/agentInventory/status"),
        Some(&JsonValue::String("not_inspected".to_string())),
        "agent inventory status",
    )?;
    ensure_equal(
        value.pointer("/data/agentInventory/inspectionCommand"),
        Some(&JsonValue::String("ee agent status --json".to_string())),
        "agent inventory inspection command",
    )?;
    ensure(
        value
            .pointer("/data/agentInventory/installedAgents")
            .is_none(),
        "ee status must not expose machine-local agent roots by default",
    )
}

#[test]
fn default_probe_catalog_contains_expected_agent_roots() -> TestResult {
    let probes = default_probe_paths_tilde();
    for (slug, expected) in [
        ("codex", "~/.codex/sessions"),
        ("claude", "~/.claude/projects"),
        ("gemini", "~/.gemini/tmp"),
        ("copilot_cli", "~/.copilot/session-state"),
    ] {
        let paths = probes
            .iter()
            .find_map(|(candidate, paths)| (*candidate == slug).then_some(paths))
            .ok_or_else(|| format!("missing default probe catalog entry for {slug}"))?;
        ensure(
            paths.iter().any(|path| path == expected),
            format!("{slug} probe paths should include {expected}"),
        )?;
    }
    Ok(())
}

#[test]
fn tracked_fixture_roots_exist_for_default_detection_contract() -> TestResult {
    for relative in [
        "codex/sessions/.keep",
        "claude/projects/.keep",
        "gemini/tmp/.keep",
        "cursor/.cursor/.keep",
        "remote_mirror/ssh-csd/home/agent/.codex/sessions/.keep",
        "remote_mirror/ssh-csd/home/agent/.claude/projects/.keep",
    ] {
        let path = fixtures_path().join(Path::new(relative));
        ensure(
            path.is_file(),
            format!("tracked fixture marker missing: {}", path.display()),
        )?;
    }
    Ok(())
}
