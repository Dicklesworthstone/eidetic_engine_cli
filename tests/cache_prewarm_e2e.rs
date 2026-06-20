//! Real-binary E2E coverage for `ee cache prewarm`.
//!
//! The in-process CLI contract tests already pin the cache-prewarm planner.
//! This test pins the user-facing route by spawning the compiled `ee` binary
//! and asserting the standard JSON response envelope.

use ee::cache::hotset::{GenerationGate, HotsetBudget, HotsetManifestBuilder};
use ee::obs::test_log::{
    EventKind, LogLevel, TestEvent, excerpt_stderr, hash_bytes, log_event_to,
};
use ee::pack::{PackHotsetEntry, PackHotsetEntryKind, PackSection};
use ee::search::SearchHotsetEntry;
use serde_json::{Value as JsonValue, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;
const TEST_ID: &str = "cache_prewarm_real_binary_e2e";

struct LoggedOutput {
    output: Output,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    elapsed_ms: f64,
    manifest_hash: String,
}

#[derive(Clone, Copy)]
struct HotsetScenario {
    label: &'static str,
    generation: u64,
    entry_count: usize,
    profile: &'static str,
    current_generation: Option<u64>,
    expected_degraded_code: Option<&'static str>,
    expected_search_status: &'static str,
    expected_min_admitted: u64,
}

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

fn run_ee_logged(
    workspace: &Path,
    args: &[String],
    label: &str,
    manifest_path: &Path,
    artifact_dir: &Path,
    event_log: &Path,
) -> Result<LoggedOutput, String> {
    fs::create_dir_all(artifact_dir).map_err(|error| {
        format!(
            "failed to create artifact directory {}: {error}",
            artifact_dir.display()
        )
    })?;
    let manifest_bytes = fs::read(manifest_path)
        .map_err(|error| format!("read manifest {}: {error}", manifest_path.display()))?;
    let manifest_hash = hash_bytes(&manifest_bytes);
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .current_dir(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {:?}: {error}", args))?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stdout_path = artifact_dir.join(format!("{label}.stdout.json"));
    let stderr_path = artifact_dir.join(format!("{label}.stderr.txt"));
    fs::write(&stdout_path, &output.stdout)
        .map_err(|error| format!("write {}: {error}", stdout_path.display()))?;
    fs::write(&stderr_path, &output.stderr)
        .map_err(|error| format!("write {}: {error}", stderr_path.display()))?;

    let mut event = TestEvent::new(TEST_ID, EventKind::CommandEnd)
        .with_field("label", label.to_owned())
        .with_field("workspace", workspace.display().to_string())
        .with_field("source_snapshot_hash", manifest_hash.clone())
        .with_field("manifest_hash", manifest_hash.clone())
        .with_field("stdout_artifact_path", stdout_path.display().to_string())
        .with_field("stderr_artifact_path", stderr_path.display().to_string())
        .with_field(
            "status",
            if output.status.success() {
                "ok".to_owned()
            } else {
                "fail".to_owned()
            },
        )
        .with_field("redaction_status", "query_hashes_and_cache_keys_only")
        .with_field(
            "first_failure_diagnosis",
            if output.status.success() {
                "none".to_owned()
            } else {
                "ee_cache_prewarm_nonzero_exit".to_owned()
            },
        )
        .with_field(
            "latency_sample_summary",
            json!({
                "sample_count": 1,
                "p50_ms": elapsed_ms,
                "p99_ms": elapsed_ms,
            }),
        );
    event.command = Some("ee".to_owned());
    event.args = args.to_vec();
    event.exit_code = output.status.code();
    event.elapsed_ms = Some(elapsed_ms);
    event.stdout_hash = Some(hash_bytes(&output.stdout));
    event.stderr_excerpt = Some(excerpt_stderr(&output.stderr, 512));
    ensure(
        log_event_to(event_log, LogLevel::Verbose, &event),
        "command_end event should be written",
    )?;

    Ok(LoggedOutput {
        output,
        stdout_path,
        stderr_path,
        elapsed_ms,
        manifest_hash,
    })
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

fn write_hotset_manifest(path: &Path, generation: u64, entry_count: usize) -> TestResult {
    let pack_entry = PackHotsetEntry {
        key: "pack:section:real-binary-prewarm".to_string(),
        kind: PackHotsetEntryKind::PackSection,
        section: Some(PackSection::Evidence),
        generation,
        estimated_bytes: 384,
        hit_count: 2,
        redaction_status: "content_not_stored",
    };
    let mut search_entries = Vec::new();
    for index in 0..entry_count {
        search_entries.push(SearchHotsetEntry::memory(
            &format!("mem_real_binary_prewarm_{index:03}"),
            generation,
            3,
        ));
    }
    if entry_count > 0 {
        search_entries.push(
            SearchHotsetEntry::query_shape(
                "cache prewarm e2e raw query must stay hashed",
                generation,
                2,
            )
            .ok_or_else(|| "query shape should normalize".to_string())?,
        );
    }

    let mut builder =
        HotsetManifestBuilder::new("ws_01HQTBINARYPREWARM00000", GenerationGate::new(5, 5))
            .with_profile_tier("standard")
            .with_budget(HotsetBudget::new(128, 8 * 1024 * 1024))
            .search_entries(search_entries);
    if entry_count > 0 {
        builder = builder.pack_entries([pack_entry]);
    }
    let manifest = builder.build();

    fs::write(path, manifest.to_json().to_string())
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn emit_scenario_event(
    event_log: &Path,
    scenario: &HotsetScenario,
    logged: &LoggedOutput,
    response: &JsonValue,
) -> TestResult {
    let data = &response["data"];
    let event = TestEvent::new(TEST_ID, EventKind::AssertOk)
        .with_field("label", scenario.label.to_owned())
        .with_field("command", "ee cache prewarm")
        .with_field(
            "workspace",
            response
                .pointer("/data/fromHotset/workspaceId")
                .and_then(JsonValue::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        )
        .with_field("source_snapshot_hash", logged.manifest_hash.clone())
        .with_field("manifest_hash", logged.manifest_hash.clone())
        .with_field("warmed_search_entries", data["admitted"]["searchEntries"].clone())
        .with_field("warmed_pack_entries", data["admitted"]["packEntries"].clone())
        .with_field("warmed_total_entries", data["admitted"]["totalEntries"].clone())
        .with_field("elapsed_ms", logged.elapsed_ms)
        .with_field(
            "latency_sample_summary",
            json!({
                "sample_count": 1,
                "p50_ms": logged.elapsed_ms,
                "p99_ms": logged.elapsed_ms,
            }),
        )
        .with_field(
            "stdout_artifact_path",
            logged.stdout_path.display().to_string(),
        )
        .with_field(
            "stderr_artifact_path",
            logged.stderr_path.display().to_string(),
        )
        .with_field(
            "redaction_status",
            data["redactionSafety"]["summary"].clone(),
        )
        .with_field("first_failure_diagnosis", "none");
    ensure(
        log_event_to(event_log, LogLevel::Verbose, &event),
        "scenario assert_ok event should be written",
    )
}

#[test]
fn cache_prewarm_real_binary_emits_response_envelope_for_hotset_manifest() -> TestResult {
    let run_dir = unique_run_dir()?;
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create {}: {error}", workspace.display()))?;
    let artifact_dir = run_dir.join("artifacts");
    let event_log = run_dir.join("cache_prewarm_events.jsonl");
    let scenarios = [
        HotsetScenario {
            label: "warm_workspace",
            generation: 5,
            entry_count: 2,
            profile: "standard",
            current_generation: None,
            expected_degraded_code: None,
            expected_search_status: "warm",
            expected_min_admitted: 3,
        },
        HotsetScenario {
            label: "cold_workspace_no_signals",
            generation: 5,
            entry_count: 0,
            profile: "standard",
            current_generation: None,
            expected_degraded_code: Some("hotset_prewarm_no_signals"),
            expected_search_status: "warm",
            expected_min_admitted: 0,
        },
        HotsetScenario {
            label: "stale_source_abstention",
            generation: 5,
            entry_count: 2,
            profile: "standard",
            current_generation: Some(8),
            expected_degraded_code: Some("cache_hotset_stale"),
            expected_search_status: "stale_generation",
            expected_min_admitted: 0,
        },
        HotsetScenario {
            label: "resource_pressure_abstention",
            generation: 5,
            entry_count: 70,
            profile: "lean",
            current_generation: None,
            expected_degraded_code: None,
            expected_search_status: "pressure_fallback",
            expected_min_admitted: 64,
        },
    ];

    let mut warm_response: Option<JsonValue> = None;
    for scenario in scenarios {
        let manifest_path = run_dir.join(format!("{}.hotset.json", scenario.label));
        write_hotset_manifest(&manifest_path, scenario.generation, scenario.entry_count)?;
        let mut args = vec![
            "--workspace".to_owned(),
            workspace.to_string_lossy().into_owned(),
            "--json".to_owned(),
            "cache".to_owned(),
            "prewarm".to_owned(),
            "--from-hotset".to_owned(),
            manifest_path.to_string_lossy().into_owned(),
            "--profile".to_owned(),
            scenario.profile.to_owned(),
        ];
        if let Some(current_generation) = scenario.current_generation {
            args.push("--current-generation".to_owned());
            args.push(current_generation.to_string());
        }

        let logged = run_ee_logged(
            &workspace,
            &args,
            scenario.label,
            &manifest_path,
            &artifact_dir,
            &event_log,
        )?;
        ensure(
            logged.output.status.success(),
            format!(
                "{} cache prewarm failed: status={:?}; stdout={}; stderr={}",
                scenario.label,
                logged.output.status.code(),
                String::from_utf8_lossy(&logged.output.stdout),
                String::from_utf8_lossy(&logged.output.stderr)
            ),
        )?;
        ensure(
            logged.output.stderr.is_empty(),
            format!(
                "{} cache prewarm should not write JSON diagnostics to stderr: {}",
                scenario.label,
                String::from_utf8_lossy(&logged.output.stderr)
            ),
        )?;
        ensure(
            logged.output.stdout.ends_with(b"\n"),
            format!("{} JSON stdout should end with a newline", scenario.label),
        )?;

        let response = parse_stdout_json(&logged.output, scenario.label)?;
        ensure(
            response.pointer("/schema").and_then(JsonValue::as_str) == Some("ee.response.v2"),
            format!("{} top-level response envelope schema", scenario.label),
        )?;
        ensure(
            response.pointer("/success").and_then(JsonValue::as_bool) == Some(true),
            format!("{} top-level response success", scenario.label),
        )?;
        ensure(
            response.pointer("/data/schema").and_then(JsonValue::as_str)
                == Some("ee.cache.prewarm.v1"),
            format!("{} cache prewarm data schema", scenario.label),
        )?;
        ensure(
            response
                .pointer("/data/reports/search/schema")
                .and_then(JsonValue::as_str)
                == Some("ee.search.cache_prewarm.v1"),
            format!("{} search prewarm report schema", scenario.label),
        )?;
        ensure(
            response
                .pointer("/data/reports/pack/schema")
                .and_then(JsonValue::as_str)
                == Some("ee.pack.cache_prewarm.v1"),
            format!("{} pack prewarm report schema", scenario.label),
        )?;
        ensure(
            response
                .pointer("/data/admitted/totalEntries")
                .and_then(JsonValue::as_u64)
                .is_some_and(|count| count >= scenario.expected_min_admitted),
            format!("{} hotset admission count", scenario.label),
        )?;
        ensure(
            response
                .pointer("/data/reports/search/status")
                .and_then(JsonValue::as_str)
                == Some(scenario.expected_search_status),
            format!("{} search prewarm status", scenario.label),
        )?;
        let degraded = response
            .pointer("/data/degraded")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default();
        match scenario.expected_degraded_code {
            Some(code) => ensure(
                degraded
                    .iter()
                    .any(|entry| entry["code"].as_str() == Some(code)),
                format!("{} expected degraded code {code}", scenario.label),
            )?,
            None => ensure(
                degraded.is_empty(),
                format!("{} degraded array should be empty", scenario.label),
            )?,
        }
        ensure(
            response
                .pointer("/data/redactionSafety/summary")
                .and_then(JsonValue::as_str)
                == Some("query_hashes_and_cache_keys_only"),
            format!("{} redaction safety summary", scenario.label),
        )?;
        ensure(
            !response
                .pointer("/data")
                .map_or_else(String::new, JsonValue::to_string)
                .contains("raw query must stay hashed"),
            format!("{} output must not leak raw query text", scenario.label),
        )?;
        emit_scenario_event(&event_log, &scenario, &logged, &response)?;
        if scenario.label == "warm_workspace" {
            warm_response = Some(response);
        }
    }

    let warm_manifest = run_dir.join("warm_workspace.hotset.json");
    let repeat_args = vec![
        "--workspace".to_owned(),
        workspace.to_string_lossy().into_owned(),
        "--json".to_owned(),
        "cache".to_owned(),
        "prewarm".to_owned(),
        "--from-hotset".to_owned(),
        warm_manifest.to_string_lossy().into_owned(),
        "--profile".to_owned(),
        "standard".to_owned(),
    ];
    let repeat = run_ee_logged(
        &workspace,
        &repeat_args,
        "warm_workspace_repeat",
        &warm_manifest,
        &artifact_dir,
        &event_log,
    )?;
    let repeat_response = parse_stdout_json(&repeat.output, "warm_workspace_repeat")?;
    ensure(
        warm_response.as_ref().map(|value| &value["data"]) == Some(&repeat_response["data"]),
        "repeated warm prewarm should preserve output semantics",
    )?;

    let event_log_text = fs::read_to_string(&event_log)
        .map_err(|error| format!("read event log {}: {error}", event_log.display()))?;
    let mut command_end_count = 0_usize;
    let mut assert_ok_count = 0_usize;
    for (line_index, line) in event_log_text.lines().enumerate() {
        let event: JsonValue = serde_json::from_str(line).map_err(|error| {
            format!(
                "event log line {} should be JSON: {error}; line={line}",
                line_index + 1
            )
        })?;
        ensure(
            event.get("schema").and_then(JsonValue::as_str) == Some("ee.test_event.v1"),
            format!("event log line {} schema", line_index + 1),
        )?;
        match event.get("kind").and_then(JsonValue::as_str) {
            Some("command_end") => command_end_count = command_end_count.saturating_add(1),
            Some("assert_ok") => assert_ok_count = assert_ok_count.saturating_add(1),
            other => {
                return Err(format!(
                    "event log line {} unexpected kind {other:?}",
                    line_index + 1
                ));
            }
        }
    }
    ensure(command_end_count >= 5, "event log should record command_end rows")?;
    ensure(assert_ok_count >= 4, "event log should record scenario assertions")
}
