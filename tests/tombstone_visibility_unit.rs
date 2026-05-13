use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn ee_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn artifact_root() -> PathBuf {
    option_env!("CARGO_TARGET_TMPDIR").map_or_else(
        || std::env::temp_dir().join("ee-tombstone-visibility-unit"),
        PathBuf::from,
    )
}

fn unique_dir(name: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before epoch: {error}"))?
        .as_nanos();
    let dir = artifact_root().join(format!("{name}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("mkdir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn run_ee(workspace: &Path, args: &[&str]) -> Result<JsonValue, String> {
    let output = Command::new(ee_bin())
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .args(args)
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!(
            "ee {} failed (exit {:?})\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse JSON from ee {}: {error}", args.join(" ")))
}

fn json_str<'a>(value: &'a JsonValue, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("{context}: missing string at {pointer}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn degraded_codes(value: &JsonValue) -> Vec<&str> {
    value
        .pointer("/data/degraded")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flat_map(|items| items.iter())
        .filter_map(|item| item.get("code").and_then(JsonValue::as_str))
        .collect()
}

fn json_array(value: &JsonValue) -> &[JsonValue] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn records_path(report: &JsonValue) -> Result<PathBuf, String> {
    json_str(report, "/data/recordsPath", "export report").map(PathBuf::from)
}

fn read_jsonl_records(path: &Path) -> Result<Vec<JsonValue>, String> {
    fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<JsonValue>(line)
                .map_err(|error| format!("parse {} line {}: {error}", path.display(), index + 1))
        })
        .collect()
}

#[test]
fn tombstone_visibility_surfaces_are_explicit_and_roundtrip_safe() -> TestResult {
    let root = unique_dir("tombstone-visibility")?;
    let source_workspace = root.join("source");
    let imported_workspace = root.join("imported");
    let export_dir = root.join("export");
    fs::create_dir_all(&source_workspace)
        .map_err(|error| format!("mkdir source workspace: {error}"))?;
    fs::create_dir_all(&imported_workspace)
        .map_err(|error| format!("mkdir imported workspace: {error}"))?;

    run_ee(&source_workspace, &["init"])?;
    let query = "b8 tombstone visibility alpha marker";
    let reason = "superseded by B8 lifecycle fixture";
    let tombstoned_content = format!("{query} tombstoned rule");
    let tombstoned = run_ee(
        &source_workspace,
        &[
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "b8,tombstone",
            "--no-propose-candidates",
            &tombstoned_content,
        ],
    )?;
    let active = run_ee(
        &source_workspace,
        &[
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "b8,tombstone",
            "--no-propose-candidates",
            "b8 tombstone visibility beta active companion",
        ],
    )?;
    let tombstoned_id = json_str(&tombstoned, "/data/memory_id", "tombstoned remember")?;
    let active_id = json_str(&active, "/data/memory_id", "active remember")?;

    run_ee(
        &source_workspace,
        &[
            "memory",
            "link",
            tombstoned_id,
            active_id,
            "--relation",
            "supports",
            "--actor",
            "tombstone_visibility_unit",
        ],
    )?;
    run_ee(
        &source_workspace,
        &[
            "curate",
            "tombstone",
            tombstoned_id,
            "--reason",
            reason,
            "--actor",
            "tombstone_visibility_unit",
        ],
    )?;

    let search_default = run_ee(
        &source_workspace,
        &["search", query, "--relevance-floor", "0.0"],
    )?;
    ensure(
        !json_array(&search_default["data"]["results"])
            .iter()
            .any(|result| result["docId"].as_str() == Some(tombstoned_id)),
        "default search should exclude tombstoned memory",
    )?;
    ensure(
        degraded_codes(&search_default).contains(&"tombstoned_filtered"),
        "default search should emit tombstoned_filtered",
    )?;

    let search_include = run_ee(
        &source_workspace,
        &[
            "search",
            query,
            "--include-tombstoned",
            "--relevance-floor",
            "0.0",
        ],
    )?;
    let included_search_result = json_array(&search_include["data"]["results"])
        .iter()
        .find(|result| result["docId"].as_str() == Some(tombstoned_id))
        .ok_or_else(|| "include-tombstoned search should return tombstoned memory".to_owned())?;
    ensure(
        included_search_result["tombstoned"].as_bool() == Some(true)
            && included_search_result["tombstonedAt"].as_str().is_some()
            && included_search_result["metadata"]["tombstoned"].as_bool() == Some(true),
        "include-tombstoned search result should carry tombstone markers",
    )?;
    ensure(
        degraded_codes(&search_include).contains(&"tombstoned_in_results"),
        "include-tombstoned search should emit tombstoned_in_results",
    )?;

    let context_default = run_ee(&source_workspace, &["context", query])?;
    ensure(
        !json_array(&context_default["data"]["pack"]["items"])
            .iter()
            .any(|item| item["memoryId"].as_str() == Some(tombstoned_id)),
        "default context should exclude tombstoned memory",
    )?;
    let context_include = run_ee(
        &source_workspace,
        &["context", query, "--include-tombstoned"],
    )?;
    ensure(
        json_array(&context_include["data"]["pack"]["items"])
            .iter()
            .any(|item| {
                item["memoryId"].as_str() == Some(tombstoned_id)
                    && item["lifecycle"]["status"].as_str() == Some("tombstoned")
                    && item["lifecycle"]["tombstonedAt"].as_str().is_some()
            }),
        "include-tombstoned context should include lifecycle metadata",
    )?;
    ensure(
        degraded_codes(&context_include).contains(&"tombstoned_in_results"),
        "include-tombstoned context should emit tombstoned_in_results",
    )?;

    let why = run_ee(&source_workspace, &["why", tombstoned_id])?;
    ensure(
        why["data"]["lifecycle"]["status"].as_str() == Some("tombstoned"),
        "why should include tombstoned lifecycle status",
    )?;
    ensure(
        why["data"]["lifecycle"]["tombstoned_reason"].as_str() == Some(reason),
        "why should include tombstone reason",
    )?;

    let memory_list = run_ee(&source_workspace, &["memory", "list"])?;
    ensure(
        json_array(&memory_list["data"]["memories"])
            .iter()
            .any(|memory| {
                memory["id"].as_str() == Some(tombstoned_id)
                    && memory["is_tombstoned"].as_bool() == Some(true)
            }),
        "memory list should include tombstoned memories by default",
    )?;
    let memory_list_without = run_ee(&source_workspace, &["memory", "list", "--no-tombstoned"])?;
    ensure(
        !json_array(&memory_list_without["data"]["memories"])
            .iter()
            .any(|memory| memory["id"].as_str() == Some(tombstoned_id)),
        "memory list --no-tombstoned should exclude tombstoned memories",
    )?;

    let graph_default = run_ee(&source_workspace, &["graph", "pagerank"])?;
    ensure(
        json_array(&graph_default["data"]["graph"]["excludedNodes"])
            .iter()
            .any(|node| node.as_str() == Some(tombstoned_id)),
        "graph pagerank should report excluded tombstoned nodes by default",
    )?;
    let graph_include = run_ee(
        &source_workspace,
        &["graph", "pagerank", "--include-tombstoned"],
    )?;
    ensure(
        !json_array(&graph_include["data"]["graph"]["excludedNodes"])
            .iter()
            .any(|node| node.as_str() == Some(tombstoned_id)),
        "graph pagerank --include-tombstoned should include tombstoned node in compute",
    )?;

    let export_dir_arg = export_dir.to_string_lossy().into_owned();
    let export = run_ee(
        &source_workspace,
        &[
            "export",
            "--output-dir",
            &export_dir_arg,
            "--redaction",
            "none",
            "--label",
            "b8-tombstone",
        ],
    )?;
    let export_records = read_jsonl_records(&records_path(&export)?)?;
    ensure(
        export_records.iter().any(|record| {
            record["schema"].as_str() == Some("ee.export.memory.v1")
                && record["memory_id"].as_str() == Some(tombstoned_id)
                && record["tombstoned_at"].as_str().is_some()
                && record["tombstoned_reason"].as_str() == Some(reason)
        }),
        "export should include tombstone metadata on the memory record",
    )?;

    run_ee(&imported_workspace, &["init"])?;
    let records_path_arg = records_path(&export)?.to_string_lossy().into_owned();
    run_ee(
        &imported_workspace,
        &["import", "jsonl", "--source", &records_path_arg],
    )?;
    let imported_why = run_ee(&imported_workspace, &["why", tombstoned_id])?;
    ensure(
        imported_why["data"]["lifecycle"]["status"].as_str() == Some("tombstoned"),
        "imported why should preserve tombstoned lifecycle status",
    )?;
    ensure(
        imported_why["data"]["lifecycle"]["tombstoned_reason"].as_str() == Some(reason),
        "imported why should preserve tombstone reason",
    )
}

#[test]
fn tombstoned_in_results_failure_mode_fixture_is_catalog_shaped() -> TestResult {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/failure_modes/tombstoned_in_results.json");
    let fixture = fs::read_to_string(&path).map_err(|error| format!("read fixture: {error}"))?;
    let fixture: JsonValue =
        serde_json::from_str(&fixture).map_err(|error| format!("parse fixture: {error}"))?;

    ensure(
        fixture["schema"].as_str() == Some("ee.failure_mode_fixture.v1"),
        "fixture schema",
    )?;
    ensure(
        fixture["code"].as_str() == Some("tombstoned_in_results"),
        "fixture code",
    )?;
    ensure(
        fixture["introduced_by"]["bead"].as_str() == Some("bd-17c65.2.8"),
        "fixture bead",
    )?;
    ensure(
        json_array(&fixture["expected_emission"]["message_contains"])
            .iter()
            .any(|item| item.as_str() == Some("--include-tombstoned")),
        "fixture documents include flag trigger",
    )
}
