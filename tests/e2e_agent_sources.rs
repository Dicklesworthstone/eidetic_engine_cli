//! bd-2ot4x: real-binary pin tests for `ee agent sources --json`.
//!
//! The agent-source catalog had contract/golden coverage but no
//! `tests/e2e_agent*.rs` route pin. These tests exercise the compiled binary
//! and keep the read-only catalog surface stable for harnesses that discover
//! local agent roots before importing CASS history.

#![cfg(unix)]

use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_agent_sources(extra: &[&str]) -> Result<Value, String> {
    let mut args = vec!["--json", "agent", "sources"];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    ensure(
        output.status.success(),
        format!(
            "ee agent sources {extra:?} must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "ee agent sources {extra:?} must keep stderr empty; got {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("ee agent sources stdout must be JSON: {error}"))
}

fn run_agent_scan(extra: &[&str]) -> Result<Value, String> {
    let mut args = vec!["--json", "agent", "scan"];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    ensure(
        output.status.success(),
        format!(
            "ee agent scan {extra:?} must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "ee agent scan {extra:?} must keep stderr empty; got {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("ee agent scan stdout must be JSON: {error}"))
}

#[test]
fn agent_sources_default_json_is_response_envelope_without_probe_paths() -> TestResult {
    let value = run_agent_sources(&[])?;
    ensure(
        value["schema"].as_str() == Some("ee.response.v2"),
        format!("top-level schema must be ee.response.v2; got {value}"),
    )?;
    ensure(
        value["success"].as_bool() == Some(true),
        format!("agent sources default response must succeed; got {value}"),
    )?;

    let data = &value["data"];
    ensure(
        data["schema"].as_str() == Some("ee.agent.sources.v1"),
        format!("data schema must be ee.agent.sources.v1; got {data}"),
    )?;
    ensure(
        data["command"].as_str() == Some("agent sources"),
        format!("data.command must be agent sources; got {data}"),
    )?;
    ensure(
        data["includePaths"].as_bool() == Some(false),
        format!("includePaths must default false; got {data}"),
    )?;
    ensure(
        data["totalCount"].as_u64().is_some_and(|count| count > 0),
        format!("totalCount must be a nonzero connector count; got {data}"),
    )?;

    let sources = data["sources"]
        .as_array()
        .ok_or_else(|| format!("data.sources must be an array; got {data}"))?;
    ensure(
        !sources.is_empty(),
        format!("default source catalog must be non-empty; got {sources:?}"),
    )?;
    for (index, source) in sources.iter().enumerate() {
        ensure(
            source["slug"].as_str().is_some_and(|slug| !slug.is_empty()),
            format!("sources[{index}].slug must be non-empty; got {source}"),
        )?;
        ensure(
            source.get("probePaths").is_none(),
            format!(
                "sources[{index}] must omit probePaths unless --include-paths is set; got {source}"
            ),
        )?;
    }
    ensure(
        data["originFixtures"].as_array().is_some_and(Vec::is_empty),
        format!("originFixtures must default empty; got {data}"),
    )?;
    ensure(
        data["pathRewrites"].as_array().is_some_and(Vec::is_empty),
        format!("pathRewrites must default empty; got {data}"),
    )
}

#[test]
fn agent_sources_origin_fixture_filter_canonicalizes_codex_alias() -> TestResult {
    let value = run_agent_sources(&[
        "--only",
        "CodexCli",
        "--include-paths",
        "--include-origin-fixtures",
    ])?;
    let data = &value["data"];

    ensure(
        data["schema"].as_str() == Some("ee.agent.sources.v1"),
        format!("data schema must be ee.agent.sources.v1; got {data}"),
    )?;
    ensure(
        data["includePaths"].as_bool() == Some(true),
        format!("includePaths must be true with --include-paths; got {data}"),
    )?;
    ensure(
        data["totalCount"].as_u64() == Some(1),
        format!("CodexCli alias filter must return one source; got {data}"),
    )?;

    let sources = data["sources"]
        .as_array()
        .ok_or_else(|| format!("data.sources must be an array; got {data}"))?;
    let source = sources
        .first()
        .ok_or_else(|| format!("CodexCli filter must return one source; got {sources:?}"))?;
    ensure(
        source["slug"].as_str() == Some("codex"),
        format!("CodexCli must canonicalize to codex; got {source}"),
    )?;
    ensure(
        source["probePaths"]
            .as_array()
            .is_some_and(|paths| !paths.is_empty()),
        format!("--include-paths must expose codex probe paths; got {source}"),
    )?;

    let fixtures = data["originFixtures"]
        .as_array()
        .ok_or_else(|| format!("originFixtures must be an array; got {data}"))?;
    ensure(
        fixtures.len() == 1,
        format!("CodexCli origin fixture filter must return one fixture; got {fixtures:?}"),
    )?;
    let fixture = &fixtures[0];
    ensure(
        fixture["originId"].as_str() == Some("fixture-ssh-csd"),
        format!("origin fixture id must remain stable; got {fixture}"),
    )?;
    ensure(
        fixture["kind"].as_str() == Some("remote_mirror"),
        format!("origin fixture kind must remain remote_mirror; got {fixture}"),
    )?;
    let connector_slugs = fixture["connectorSlugs"]
        .as_array()
        .ok_or_else(|| format!("connectorSlugs must be an array; got {fixture}"))?;
    ensure(
        connector_slugs.len() == 1 && connector_slugs[0].as_str() == Some("codex"),
        format!("origin fixture must be filtered to codex only; got {fixture}"),
    )?;

    let rewrites = data["pathRewrites"]
        .as_array()
        .ok_or_else(|| format!("pathRewrites must be an array; got {data}"))?;
    ensure(
        rewrites.len() == 1,
        format!("CodexCli path rewrite filter must return one rewrite; got {rewrites:?}"),
    )?;
    let rewrite = &rewrites[0];
    ensure(
        rewrite["connectorSlug"].as_str() == Some("codex"),
        format!("path rewrite must be filtered to codex; got {rewrite}"),
    )?;
    ensure(
        rewrite["from"]
            .as_str()
            .is_some_and(|path| path.starts_with("/home/agent/")),
        format!("rewrite.from must stay a bounded remote fixture path; got {rewrite}"),
    )?;
    ensure(
        rewrite["to"]
            .as_str()
            .is_some_and(|path| path.contains("tests/fixtures/agent_detect/remote_mirror")),
        format!("rewrite.to must target the deterministic fixture mirror; got {rewrite}"),
    )
}

#[test]
fn agent_scan_only_canonicalizes_codex_aliases() -> TestResult {
    for alias in ["codex", "codex-cli", "CodexCli"] {
        let value = run_agent_scan(&["--only", alias])?;
        ensure(
            value["schema"].as_str() == Some("ee.agent.scan.v1"),
            format!("agent scan schema must be ee.agent.scan.v1 for {alias}; got {value}"),
        )?;
        ensure(
            value["success"].as_bool() == Some(true),
            format!("agent scan must succeed for {alias}; got {value}"),
        )?;

        let data = &value["data"];
        ensure(
            data["command"].as_str() == Some("agent scan"),
            format!("data.command must be agent scan for {alias}; got {data}"),
        )?;
        let paths = data["paths"]
            .as_array()
            .ok_or_else(|| format!("data.paths must be an array for {alias}; got {data}"))?;
        ensure(
            !paths.is_empty(),
            format!("agent scan --only {alias} must scan codex paths; got {data}"),
        )?;
        ensure(
            data["totalPaths"].as_u64() == Some(paths.len() as u64),
            format!("totalPaths must match paths length for {alias}; got {data}"),
        )?;
        for (index, path) in paths.iter().enumerate() {
            ensure(
                path["slug"].as_str() == Some("codex"),
                format!("paths[{index}] must be canonical codex for {alias}; got {path}"),
            )?;
        }
    }

    Ok(())
}
