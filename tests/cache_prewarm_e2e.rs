//! Real-binary E2E coverage for `ee cache prewarm`.
//!
//! The in-process CLI contract tests already pin the cache-prewarm planner.
//! This test pins the user-facing route by spawning the compiled `ee` binary
//! and asserting the standard JSON response envelope.

use ee::cache::hotset::{GenerationGate, HotsetBudget, HotsetManifestBuilder};
use ee::pack::{PackHotsetEntry, PackHotsetEntryKind, PackSection};
use ee::search::SearchHotsetEntry;
use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn unique_run_dir() -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let dir = target_root
        .join("ee-cache-prewarm-e2e")
        .join(format!("{}-{now}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
    Ok(dir)
}

fn run_ee(workspace: &Path, args: &[String]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {:?}: {error}", args))
}

fn parse_stdout_json(output: &Output, context: &str) -> Result<JsonValue, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{context} stdout was not JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn write_hotset_manifest(path: &Path) -> TestResult {
    let generation = 5;
    let pack_entry = PackHotsetEntry {
        key: "pack:section:real-binary-prewarm".to_string(),
        kind: PackHotsetEntryKind::PackSection,
        section: Some(PackSection::Evidence),
        generation,
        estimated_bytes: 384,
        hit_count: 2,
        redaction_status: "content_not_stored",
    };
    let manifest =
        HotsetManifestBuilder::new("ws_01HQTBINARYPREWARM00000", GenerationGate::new(5, 5))
            .with_profile_tier("standard")
            .with_budget(HotsetBudget::new(128, 8 * 1024 * 1024))
            .search_entries([
                SearchHotsetEntry::memory("mem_real_binary_prewarm", generation, 3),
                SearchHotsetEntry::query_shape(
                    "cache prewarm e2e raw query must stay hashed",
                    generation,
                    2,
                )
                .ok_or_else(|| "query shape should normalize".to_string())?,
            ])
            .pack_entries([pack_entry])
            .build();

    fs::write(path, manifest.to_json().to_string())
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

#[test]
fn cache_prewarm_real_binary_emits_response_envelope_for_hotset_manifest() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create {}: {error}", workspace.display()))?;
    let manifest_path = run_dir.join("hotset.json");
    write_hotset_manifest(&manifest_path)?;

    let workspace_arg = workspace.to_string_lossy().into_owned();
    let manifest_arg = manifest_path.to_string_lossy().into_owned();
    let args = vec![
        "--workspace".to_owned(),
        workspace_arg,
        "--json".to_owned(),
        "cache".to_owned(),
        "prewarm".to_owned(),
        "--from-hotset".to_owned(),
        manifest_arg,
        "--profile".to_owned(),
        "standard".to_owned(),
    ];
    let output = run_ee(&workspace, &args)?;
    ensure(
        output.status.success(),
        format!(
            "cache prewarm failed: status={:?}; stdout={}; stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "cache prewarm should not write JSON diagnostics to stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stdout.ends_with(b"\n"),
        "cache prewarm JSON stdout should end with a newline",
    )?;

    let response = parse_stdout_json(&output, "cache prewarm")?;
    ensure(
        response.pointer("/schema").and_then(JsonValue::as_str) == Some("ee.response.v2"),
        "top-level response envelope schema",
    )?;
    ensure(
        response.pointer("/success").and_then(JsonValue::as_bool) == Some(true),
        "top-level response success",
    )?;
    ensure(
        response
            .pointer("/degraded")
            .and_then(JsonValue::as_array)
            .is_some_and(Vec::is_empty),
        "top-level degraded array should be empty",
    )?;
    ensure(
        response.pointer("/data/schema").and_then(JsonValue::as_str) == Some("ee.cache.prewarm.v1"),
        "cache prewarm data schema",
    )?;
    ensure(
        response
            .pointer("/data/reports/search/schema")
            .and_then(JsonValue::as_str)
            == Some("ee.search.cache_prewarm.v1"),
        "search prewarm report schema",
    )?;
    ensure(
        response
            .pointer("/data/reports/pack/schema")
            .and_then(JsonValue::as_str)
            == Some("ee.pack.cache_prewarm.v1"),
        "pack prewarm report schema",
    )?;
    ensure(
        response
            .pointer("/data/admitted/totalEntries")
            .and_then(JsonValue::as_u64)
            .is_some_and(|count| count >= 3),
        "hotset entries should be admitted",
    )?;
    ensure(
        response
            .pointer("/data/redactionSafety/summary")
            .and_then(JsonValue::as_str)
            == Some("query_hashes_and_cache_keys_only"),
        "redaction safety summary",
    )?;
    ensure(
        !response
            .pointer("/data")
            .map_or_else(String::new, JsonValue::to_string)
            .contains("raw query must stay hashed"),
        "cache prewarm output must not leak raw query text",
    )
}
