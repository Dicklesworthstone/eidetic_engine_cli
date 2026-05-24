//! bd-1rrz5: real-binary pin test for `ee graph neighborhood --format mermaid`.
//!
//! Mirrors the runtime shape of `graph_neighborhood_smoke.rs` but
//! exercises the new Mermaid renderer surface added for the
//! graph-neighborhood diagram path:
//!
//! * `--format mermaid` on a fresh workspace (no links) emits a
//!   diagram with only the center node and an explicit "empty
//!   neighborhood" comment, preserving the provenance header so an
//!   agent can paste the output without losing context.
//! * `--format mermaid` after seeding a directed `center -> neighbor`
//!   link emits a `graph LR` block with both nodes, the relation as
//!   the edge label, and the directed `-->` arrow.
//! * `--format mermaid` after seeding an undirected link emits `---`
//!   (no arrowhead) so the diagram preserves link directionality.
//! * Mermaid output is deterministic across repeated runs and is
//!   single-document (header, comments, node lines, edge lines) on
//!   stdout with nothing on stderr.
//! * `--format mermaid` with `--limit 1` truncates the edge list
//!   deterministically and emits a "limited output" comment naming
//!   the surviving edge count so an agent knows to rerun.
//! * `--json --format mermaid` and `--robot --format mermaid` keep
//!   the canonical JSON contract instead of accidentally taking the
//!   diagram branch.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use ee::db::{CreateMemoryLinkInput, DbConnection, MemoryLinkRelation, MemoryLinkSource};
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

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-graph-neighborhood-mermaid-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn init_workspace(workspace_arg: &str) -> TestResult {
    let init = run_ee(&["--workspace", workspace_arg, "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )
}

fn remember(workspace_arg: &str, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        content,
    ])?;
    if !output.status.success() {
        return Err(format!(
            "remember failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    parsed["data"]["public_id"]
        .as_str()
        .or_else(|| parsed["data"]["memory_id"].as_str())
        .or_else(|| parsed["data"]["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "remember response missing memory id: {}",
                serde_json::to_string(&parsed).unwrap_or_default()
            )
        })
}

fn insert_link(
    database_path: &std::path::Path,
    link_id: &str,
    src: &str,
    dst: &str,
    directed: bool,
    relation: MemoryLinkRelation,
) -> TestResult {
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            link_id,
            &CreateMemoryLinkInput {
                src_memory_id: src.to_owned(),
                dst_memory_id: dst.to_owned(),
                relation,
                weight: 0.9,
                confidence: 0.85,
                directed,
                evidence_count: 1,
                last_reinforced_at: Some("2026-05-01T00:00:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("e2e-graph-neighborhood-mermaid-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_neighborhood_mermaid(
    workspace_arg: &str,
    center: &str,
    extra: &[&str],
) -> Result<Output, String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--format",
        "mermaid",
        "graph",
        "neighborhood",
        center,
    ];
    args.extend_from_slice(extra);
    run_ee(&args)
}

fn run_neighborhood_mermaid_machine_mode(
    workspace_arg: &str,
    center: &str,
    mode_flag: &str,
) -> Result<(Output, Value), String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        mode_flag,
        "--format",
        "mermaid",
        "graph",
        "neighborhood",
        center,
    ])?;
    let parsed: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        let first_line = first_stdout_line(&output);
        format!(
            "ee graph neighborhood {mode_flag} --format mermaid expected canonical JSON override, \
             but stdout was not JSON: {error}; first output line: {first_line:?}"
        )
    })?;
    Ok((output, parsed))
}

fn first_stdout_line(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(160)
        .collect()
}

fn assert_common_header(stdout: &str, center: &str) -> TestResult {
    ensure(
        stdout.starts_with("%%{init: {\"flowchart\":"),
        format!("mermaid output must start with init directive; got: {stdout:.200?}"),
    )?;
    ensure(
        stdout.contains("\ngraph LR\n"),
        format!("mermaid output must declare a left-right flowchart; got: {stdout:.200?}"),
    )?;
    ensure(
        stdout.contains("schema: ee.graph.neighborhood.v1"),
        format!("mermaid output must preserve the v1 schema comment; got: {stdout:.300?}"),
    )?;
    ensure(
        stdout.contains(&format!("memoryId={center}")),
        format!("mermaid output must echo the center memory id; got: {stdout:.300?}"),
    )?;
    ensure(
        stdout.ends_with('\n'),
        format!("mermaid output must end with a newline; got: {stdout:.50?}"),
    )?;
    Ok(())
}

#[test]
fn graph_neighborhood_machine_modes_override_format_mermaid() -> TestResult {
    let workspace = unique_workspace("machine-override")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let center = remember(
        &workspace_arg,
        "Pin-test graph-neighborhood JSON override for Mermaid requests.",
    )?;

    for mode_flag in ["--json", "--robot"] {
        let (output, parsed) =
            run_neighborhood_mermaid_machine_mode(&workspace_arg, &center, mode_flag)?;
        let first_line = first_stdout_line(&output);
        ensure(
            output.status.success(),
            format!(
                "command=`ee graph neighborhood {mode_flag} --format mermaid`; \
                 expected capability=canonical JSON override; status failed; \
                 first output line={first_line:?}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        ensure(
            output.stderr.is_empty(),
            format!(
                "command=`ee graph neighborhood {mode_flag} --format mermaid`; \
                 expected capability=canonical JSON override; stderr must be empty; \
                 first output line={first_line:?}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        ensure(
            parsed["schema"].as_str() == Some("ee.graph.neighborhood.v1"),
            format!(
                "command=`ee graph neighborhood {mode_flag} --format mermaid`; \
                 expected capability=canonical JSON override; schema drifted; \
                 first output line={first_line:?}; got {parsed}"
            ),
        )?;
        ensure(
            parsed["success"].as_bool() == Some(true),
            format!(
                "command=`ee graph neighborhood {mode_flag} --format mermaid`; \
                 expected success=true; first output line={first_line:?}; got {parsed}"
            ),
        )?;
        ensure(
            parsed["data"]["memoryId"].as_str() == Some(center.as_str()),
            format!(
                "command=`ee graph neighborhood {mode_flag} --format mermaid`; \
                 expected JSON data.memoryId to echo the center id; \
                 first output line={first_line:?}; got {}",
                parsed["data"]
            ),
        )?;
        ensure(
            !first_line.starts_with("%%{init:")
                && !first_line.starts_with("graph LR")
                && !first_line.starts_with("Neighborhood for"),
            format!(
                "command=`ee graph neighborhood {mode_flag} --format mermaid`; \
                 expected JSON override, not Mermaid or human fallback; \
                 first output line={first_line:?}"
            ),
        )?;
    }

    Ok(())
}

#[test]
fn graph_neighborhood_mermaid_renders_empty_neighborhood_marker() -> TestResult {
    let workspace = unique_workspace("empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let center = remember(&workspace_arg, "Pin-test neighborhood center (empty case).")?;

    let output = run_neighborhood_mermaid(&workspace_arg, &center, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph neighborhood --format mermaid (empty) must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "graph neighborhood --format mermaid stderr must stay empty; got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_common_header(&stdout, &center)?;
    ensure(
        stdout.contains(&format!("center: {center}")),
        format!("center node must appear with role label; got: {stdout:.400?}"),
    )?;
    ensure(
        stdout.contains("empty neighborhood"),
        format!("empty neighborhood marker must be present; got: {stdout:.400?}"),
    )?;
    ensure(
        !stdout.contains(" --> ") && !stdout.contains(" --- "),
        format!("empty neighborhood must emit no edge arrows; got: {stdout:.400?}"),
    )?;
    Ok(())
}

#[test]
fn graph_neighborhood_mermaid_renders_directed_edge_with_relation_label() -> TestResult {
    let workspace = unique_workspace("directed-edge")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let center = remember(&workspace_arg, "Pin-test mermaid directed center.")?;
    let neighbor = remember(&workspace_arg, "Pin-test mermaid directed neighbor.")?;
    insert_link(
        &workspace.join(".ee").join("ee.db"),
        "link_00000000000000000000mermaid001",
        &center,
        &neighbor,
        true,
        MemoryLinkRelation::Supports,
    )?;

    let output = run_neighborhood_mermaid(&workspace_arg, &center, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph neighborhood --format mermaid (directed) must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_common_header(&stdout, &center)?;
    ensure(
        stdout.contains(&format!("center: {center}")),
        format!("center role line must appear; got: {stdout:.500?}"),
    )?;
    ensure(
        stdout.contains(&format!("neighbor: {neighbor}")),
        format!("neighbor role line must appear; got: {stdout:.500?}"),
    )?;
    let expected_edge = format!("{center} -->|supports| {neighbor}");
    ensure(
        stdout.contains(&expected_edge),
        format!(
            "directed edge must use --> arrow with supports relation; expected `{expected_edge}` in: {stdout}"
        ),
    )?;
    ensure(
        !stdout.contains("empty neighborhood"),
        format!("non-empty neighborhood must not emit empty marker; got: {stdout:.500?}"),
    )?;
    Ok(())
}

#[test]
fn graph_neighborhood_mermaid_renders_undirected_edge_without_arrowhead() -> TestResult {
    let workspace = unique_workspace("undirected-edge")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let center = remember(&workspace_arg, "Pin-test mermaid undirected center.")?;
    let neighbor = remember(&workspace_arg, "Pin-test mermaid undirected neighbor.")?;
    insert_link(
        &workspace.join(".ee").join("ee.db"),
        "link_00000000000000000000mermaid002",
        &center,
        &neighbor,
        false,
        MemoryLinkRelation::Related,
    )?;

    let output = run_neighborhood_mermaid(&workspace_arg, &center, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph neighborhood --format mermaid (undirected) must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_common_header(&stdout, &center)?;
    let directed_arrow = format!("{center} -->|related| {neighbor}");
    let undirected_line = format!("|related| {neighbor}");
    ensure(
        !stdout.contains(&directed_arrow),
        format!("undirected link must not render as --> arrow; got directed arrow in: {stdout}"),
    )?;
    ensure(
        stdout.contains(" --- ") || stdout.contains("---|"),
        format!("undirected link must use --- arrow style; got: {stdout}"),
    )?;
    ensure(
        stdout.contains(&undirected_line),
        format!(
            "undirected edge must still carry the related relation label; expected `{undirected_line}` in: {stdout}"
        ),
    )?;
    Ok(())
}

#[test]
fn graph_neighborhood_mermaid_output_is_deterministic_across_repeated_runs() -> TestResult {
    let workspace = unique_workspace("determinism")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let center = remember(&workspace_arg, "Pin-test mermaid determinism center.")?;
    let neighbor_a = remember(&workspace_arg, "Pin-test mermaid determinism neighbor A.")?;
    let neighbor_b = remember(&workspace_arg, "Pin-test mermaid determinism neighbor B.")?;
    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(
        &database_path,
        "link_00000000000000000000mermaid003",
        &center,
        &neighbor_a,
        true,
        MemoryLinkRelation::Supports,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000mermaid004",
        &center,
        &neighbor_b,
        true,
        MemoryLinkRelation::Contradicts,
    )?;

    let first = run_neighborhood_mermaid(&workspace_arg, &center, &[])?;
    let second = run_neighborhood_mermaid(&workspace_arg, &center, &[])?;
    ensure(
        first.status.success() && second.status.success(),
        "both runs must succeed".to_string(),
    )?;
    let first_stdout = String::from_utf8_lossy(&first.stdout).into_owned();
    let second_stdout = String::from_utf8_lossy(&second.stdout).into_owned();
    ensure(
        first_stdout == second_stdout,
        format!(
            "mermaid output must be byte-deterministic across runs; first={first_stdout:?} second={second_stdout:?}"
        ),
    )?;
    Ok(())
}

#[test]
fn graph_neighborhood_mermaid_limit_emits_truncation_comment() -> TestResult {
    let workspace = unique_workspace("limit")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let center = remember(&workspace_arg, "Pin-test mermaid limit center.")?;
    let neighbor_a = remember(&workspace_arg, "Pin-test mermaid limit neighbor A.")?;
    let neighbor_b = remember(&workspace_arg, "Pin-test mermaid limit neighbor B.")?;
    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(
        &database_path,
        "link_00000000000000000000mermaid005",
        &center,
        &neighbor_a,
        true,
        MemoryLinkRelation::Supports,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000mermaid006",
        &center,
        &neighbor_b,
        true,
        MemoryLinkRelation::Supports,
    )?;

    let output = run_neighborhood_mermaid(&workspace_arg, &center, &["--limit", "1"])?;
    ensure(
        output.status.success(),
        format!(
            "graph neighborhood --format mermaid --limit 1 must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_common_header(&stdout, &center)?;
    ensure(
        stdout.contains("limited output"),
        format!("--limit truncation must emit a `limited output` comment; got: {stdout}"),
    )?;
    let arrow_lines = stdout
        .lines()
        .filter(|line| line.contains(" --> ") || line.contains(" --- "))
        .count();
    ensure(
        arrow_lines == 1,
        format!("--limit 1 must emit exactly one edge line; got {arrow_lines} in: {stdout}"),
    )?;
    Ok(())
}
