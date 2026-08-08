//! bd-1oep7: real-binary E2E proving consolidation closes the Maintain loop.
//!
//! Drives the compiled `ee` CLI end to end through the public surfaces only:
//! duplicate fixture -> steward `consolidation_pass` (dry-run proof, budget
//! cancellation, real run, dedupe determinism) -> `ee curate validate/apply`
//! (absorb: lineage link, tombstoned duplicate, preserved survivor, audit
//! chain) -> workflow-emitted `index_coalesce` (no manual rebuild) -> truthful
//! generation and per-kind counts -> deduplicated search/pack -> `ee why`
//! provenance -> idempotent re-runs. No mocks: every assertion reads a real
//! database, index, or CLI response envelope.

use ee::obs::test_log::{EventKind, LogLevel, TestEvent, excerpt_stderr, hash_bytes, log_event_to};
use serde_json::Value as JsonValue;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;
const TEST_ID: &str = "consolidation_maintain_e2e";
const GROUP_PHRASE: &str = "Zephyr quill consolidation gate: run cargo fmt --check before release.";
const DUPLICATE_PHRASE: &str =
    "  zephyr   QUILL consolidation gate: run cargo fmt --check before release. ";
const WORDING_CONTROL_PHRASE: &str =
    "Zephyr quill consolidation gate: run cargo fmt --check before publishing.";
const SEARCH_QUERY: &str = "zephyr quill consolidation gate";

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

struct E2eWorkspace {
    root: PathBuf,
    workspace: PathBuf,
    home: PathBuf,
    xdg_data: PathBuf,
    artifact_dir: PathBuf,
    event_log: PathBuf,
    command_counter: std::cell::Cell<u32>,
}

impl E2eWorkspace {
    fn create() -> Result<Self, String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock moved backwards: {error}"))?
            .as_nanos();
        let target_root = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        let root = target_root
            .join("ee-consolidation-e2e")
            .join(format!("{}-{nanos}", std::process::id()));
        let workspace = root.join("workspace");
        let home = root.join("home");
        let xdg_data = root.join("xdg-data");
        let artifact_dir = root.join("artifacts");
        for dir in [&workspace, &home, &xdg_data, &artifact_dir] {
            fs::create_dir_all(dir)
                .map_err(|error| format!("failed to create {}: {error}", dir.display()))?;
        }
        let event_log = root.join("consolidation_maintain_e2e.events.jsonl");
        Ok(Self {
            root,
            workspace,
            home,
            xdg_data,
            artifact_dir,
            event_log,
            command_counter: std::cell::Cell::new(0),
        })
    }

    fn run(&self, label: &str, args: &[&str]) -> Result<Output, String> {
        let workspace = self.workspace.clone();
        self.run_in(&workspace, label, args)
    }

    fn run_in(
        &self,
        workspace: &std::path::Path,
        label: &str,
        args: &[&str],
    ) -> Result<Output, String> {
        let index = self.command_counter.get();
        self.command_counter.set(index + 1);
        let workspace_arg = workspace.display().to_string();
        let mut full_args = vec!["--workspace", workspace_arg.as_str(), "--json"];
        full_args.extend_from_slice(args);
        let started = Instant::now();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
        command.current_dir(workspace);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("EE_") {
                command.env_remove(name);
            }
        }
        command
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.xdg_data)
            .env("EE_EMBED_DOWNLOAD", "off");
        let output = command
            .args(&full_args)
            .output()
            .map_err(|error| format!("failed to run ee {full_args:?}: {error}"))?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let stdout_path = self
            .artifact_dir
            .join(format!("{index:03}-{label}.stdout.json"));
        let stderr_path = self
            .artifact_dir
            .join(format!("{index:03}-{label}.stderr.txt"));
        fs::write(&stdout_path, &output.stdout)
            .map_err(|error| format!("write {}: {error}", stdout_path.display()))?;
        fs::write(&stderr_path, &output.stderr)
            .map_err(|error| format!("write {}: {error}", stderr_path.display()))?;

        let mut event = TestEvent::new(TEST_ID, EventKind::CommandEnd)
            .with_field("label", label.to_owned())
            .with_field("workspace", workspace.display().to_string())
            .with_field("stdout_artifact_path", stdout_path.display().to_string())
            .with_field("stderr_artifact_path", stderr_path.display().to_string())
            .with_field(
                "status",
                if output.status.success() {
                    "ok"
                } else {
                    "fail"
                },
            )
            .with_field("redaction_status", "local_workspace_fixture_content_only")
            .with_field(
                "first_failure_diagnosis",
                if output.status.success() {
                    "none".to_owned()
                } else {
                    format!("ee_{label}_nonzero_exit")
                },
            );
        event.command = Some("ee".to_owned());
        event.args = full_args.iter().map(|arg| (*arg).to_owned()).collect();
        event.exit_code = output.status.code();
        event.elapsed_ms = Some(elapsed_ms);
        event.stdout_hash = Some(hash_bytes(&output.stdout));
        event.stderr_excerpt = Some(excerpt_stderr(&output.stderr, 512));
        ensure(
            log_event_to(&self.event_log, LogLevel::Verbose, &event),
            "command_end event should be written",
        )?;
        Ok(output)
    }

    fn run_json(&self, label: &str, args: &[&str]) -> Result<JsonValue, String> {
        let workspace = self.workspace.clone();
        self.run_json_in(&workspace, label, args)
    }

    fn run_json_in(
        &self,
        workspace: &std::path::Path,
        label: &str,
        args: &[&str],
    ) -> Result<JsonValue, String> {
        let output = self.run_in(workspace, label, args)?;
        ensure(
            output.status.success(),
            format!(
                "{label} should exit 0; code={:?} stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        parse_json(label, &output)
    }

    fn assert_fail(&self, label: &str, detail: &str) -> TestResult {
        let event = TestEvent::new(TEST_ID, EventKind::AssertFail)
            .with_field("label", label.to_owned())
            .with_field("first_failure_diagnosis", detail.to_owned())
            .with_field("redaction_status", "local_workspace_fixture_content_only");
        let _ = log_event_to(&self.event_log, LogLevel::Verbose, &event);
        Err(format!("{label}: {detail}"))
    }

    fn assert_ok(&self, label: &str) -> TestResult {
        let event =
            TestEvent::new(TEST_ID, EventKind::AssertOk).with_field("label", label.to_owned());
        let _ = log_event_to(&self.event_log, LogLevel::Verbose, &event);
        Ok(())
    }

    fn check(&self, label: &str, condition: bool, detail: &str) -> TestResult {
        if condition {
            self.assert_ok(label)
        } else {
            self.assert_fail(label, detail)
        }
    }
}

fn parse_json(label: &str, output: &Output) -> Result<JsonValue, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{label} stdout must be JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn json_str(value: &JsonValue, path: &[&str]) -> Option<String> {
    json_at(value, path)?.as_str().map(str::to_owned)
}

fn json_u64(value: &JsonValue, path: &[&str]) -> Option<u64> {
    json_at(value, path)?.as_u64()
}

fn json_bool(value: &JsonValue, path: &[&str]) -> Option<bool> {
    json_at(value, path)?.as_bool()
}

fn json_at<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a JsonValue> {
    let mut current = value;
    for segment in path {
        current = match segment.parse::<usize>() {
            Ok(index) => current.get(index)?,
            Err(_) => current.get(segment)?,
        };
    }
    Some(current)
}

fn memory_id_from_remember(label: &str, response: &JsonValue) -> Result<String, String> {
    json_str(response, &["data", "memory_id"])
        .or_else(|| json_str(response, &["data", "memoryId"]))
        .ok_or_else(|| format!("{label}: remember response missing memory id: {response}"))
}

/// True when `needle` occurs anywhere inside any string value of `value`.
/// Reserved for the one intentionally JSON-encoded payload (the audit
/// `details` string inside `ee why` history); every structured identity
/// assertion uses typed extraction instead.
fn json_mentions_text(value: &JsonValue, needle: &str) -> bool {
    match value {
        JsonValue::String(text) => text.contains(needle),
        JsonValue::Array(items) => items.iter().any(|item| json_mentions_text(item, needle)),
        JsonValue::Object(map) => map.values().any(|item| json_mentions_text(item, needle)),
        _ => false,
    }
}

fn first_runner_result(report: &JsonValue) -> Option<&JsonValue> {
    report
        .get("data")?
        .get("ticks")?
        .get(0)?
        .get("runner")?
        .get("results")?
        .get(0)
}

struct IndexTruth {
    db_generation: Option<u64>,
    index_generation: Option<u64>,
    memory_documents: Option<u64>,
    health: String,
}

fn index_truth(ws: &E2eWorkspace, label: &str) -> Result<IndexTruth, String> {
    let status = ws.run_json(label, &["index", "status"])?;
    Ok(IndexTruth {
        db_generation: json_u64(&status, &["data", "dbGeneration"]),
        index_generation: json_u64(&status, &["data", "indexGeneration"]),
        memory_documents: json_u64(&status, &["data", "indexDocumentCounts", "memories"]),
        health: json_str(&status, &["data", "health"]).unwrap_or_default(),
    })
}

/// Reject any machine-facing response that is not a canonical ee.response.v2
/// success envelope. Unenveloped (bare-report) output is a contract defect.
fn require_success_envelope(label: &str, response: &JsonValue) -> Result<(), String> {
    if json_str(response, &["schema"]).as_deref() == Some("ee.response.v2")
        && json_bool(response, &["success"]) == Some(true)
        && json_at(response, &["data"]).is_some_and(JsonValue::is_object)
    {
        Ok(())
    } else {
        Err(format!(
            "{label}: expected a canonical ee.response.v2 success envelope, got: {response}"
        ))
    }
}

fn audit_action_count(ws: &E2eWorkspace, label: &str, action: &str) -> Result<u64, String> {
    audit_action_count_in(ws, &ws.workspace.clone(), label, action)
}

fn audit_action_count_in(
    ws: &E2eWorkspace,
    workspace: &std::path::Path,
    label: &str,
    action: &str,
) -> Result<u64, String> {
    let timeline = ws.run_json_in(
        workspace,
        label,
        &["audit", "timeline", "--action", action, "--limit", "50"],
    )?;
    require_success_envelope(label, &timeline)?;
    ensure(
        json_str(&timeline, &["data", "schema"]).as_deref() == Some("ee.audit.timeline.v1"),
        format!("{label}: audit timeline data schema must be ee.audit.timeline.v1: {timeline}"),
    )?;
    json_u64(&timeline, &["data", "pagination", "total_count"]).ok_or_else(|| {
        format!("{label}: audit timeline missing data.pagination.total_count: {timeline}")
    })
}

/// Structured memory ids of an array of result/item objects (each object's
/// `memoryId` field). Missing/typeless fields are a defect surfaced by the
/// caller's exact-count assertions, not silently skipped rows.
fn item_memory_ids(value: &JsonValue, path: &[&str], label: &str) -> Result<Vec<String>, String> {
    let items = json_at(value, path)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{label}: missing array at {path:?}: {value}"))?;
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            item.get("memoryId")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("{label}: item {index} has no string memoryId: {item}"))
        })
        .collect()
}

fn id_count(ids: &[String], id: &str) -> usize {
    ids.iter()
        .filter(|candidate| candidate.as_str() == id)
        .count()
}

/// Full public-surface durable-state snapshot for one workspace: the EXACT
/// memory rows (live + tombstoned, every field), the exact consolidate
/// candidate rows across every status, the exact memory-link and
/// search-index-job rows (via `ee db inspect`), the append-only audit total
/// plus its newest row id/hash (chain head), and both generations with the
/// per-kind index counts and corpus revision. Two equal snapshots prove no
/// durable row or field changed in between.
#[derive(Clone, Debug, PartialEq)]
struct DurableSnapshot {
    memory_rows: usize,
    tombstoned_rows: usize,
    consolidate_candidates: usize,
    audit_total: u64,
    audit_head: JsonValue,
    db_generation: Option<u64>,
    index_generation: Option<u64>,
    index_memory_documents: Option<u64>,
    corpus_revision: Option<String>,
    memory_records: Vec<JsonValue>,
    candidate_records: Vec<JsonValue>,
    link_records: Vec<JsonValue>,
    job_records: Vec<JsonValue>,
}

/// Rows from `ee db inspect <table>` (`data.report.rows[].values`), sorted by
/// their JSON encoding for order-stable comparison.
fn inspect_table_rows(
    ws: &E2eWorkspace,
    workspace: &std::path::Path,
    label: &str,
    table: &str,
) -> Result<Vec<JsonValue>, String> {
    let inspected = ws.run_json_in(
        workspace,
        label,
        &["db", "inspect", table, "--limit", "500"],
    )?;
    require_success_envelope(label, &inspected)?;
    let mut rows = json_at(&inspected, &["data", "report", "rows"])
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{label}: db inspect {table} missing data.report.rows"))?
        .iter()
        .map(|row| row.get("values").cloned().unwrap_or(JsonValue::Null))
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row.to_string());
    Ok(rows)
}

fn sorted_records(value: &JsonValue, path: &[&str], label: &str) -> Result<Vec<JsonValue>, String> {
    let mut records = json_at(value, path)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{label}: missing array at {path:?}"))?
        .clone();
    records.sort_by_key(|record| {
        record
            .get("id")
            .and_then(JsonValue::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| record.to_string())
    });
    Ok(records)
}

fn durable_snapshot(
    ws: &E2eWorkspace,
    workspace: &std::path::Path,
    label: &str,
) -> Result<DurableSnapshot, String> {
    let memories = ws.run_json_in(workspace, &format!("{label}_memories"), &["memory", "list"])?;
    require_success_envelope(label, &memories)?;
    let memory_records = sorted_records(&memories, &["data", "memories"], label)?;
    let tombstoned_rows = memory_records
        .iter()
        .filter(|entry| {
            entry.get("is_tombstoned").and_then(JsonValue::as_bool) == Some(true)
                || entry.get("isTombstoned").and_then(JsonValue::as_bool) == Some(true)
        })
        .count();
    let candidates = ws.run_json_in(
        workspace,
        &format!("{label}_candidates"),
        &["curate", "candidates", "--type", "consolidate", "--all"],
    )?;
    require_success_envelope(label, &candidates)?;
    let candidate_records = sorted_records(&candidates, &["data", "candidates"], label)?;
    let timeline = ws.run_json_in(
        workspace,
        &format!("{label}_audit_head"),
        &["audit", "timeline", "--limit", "1"],
    )?;
    require_success_envelope(label, &timeline)?;
    let audit_total = json_u64(&timeline, &["data", "pagination", "total_count"])
        .ok_or_else(|| format!("{label}: audit timeline missing total_count"))?;
    let audit_head = json_at(&timeline, &["data", "entries", "0"])
        .map(|entry| {
            serde_json::json!({
                "id": entry.get("id").cloned().unwrap_or(JsonValue::Null),
                "this_row_hash": entry.get("this_row_hash").cloned().unwrap_or(JsonValue::Null),
            })
        })
        .unwrap_or(JsonValue::Null);
    let link_records =
        inspect_table_rows(ws, workspace, &format!("{label}_links"), "memory_links")?;
    let job_records =
        inspect_table_rows(ws, workspace, &format!("{label}_jobs"), "search_index_jobs")?;
    let status = ws.run_json_in(
        workspace,
        &format!("{label}_index_status"),
        &["index", "status"],
    )?;
    require_success_envelope(label, &status)?;
    Ok(DurableSnapshot {
        memory_rows: memory_records.len(),
        tombstoned_rows,
        consolidate_candidates: candidate_records.len(),
        audit_total,
        audit_head,
        db_generation: json_u64(&status, &["data", "dbGeneration"]),
        index_generation: json_u64(&status, &["data", "indexGeneration"]),
        index_memory_documents: json_u64(&status, &["data", "indexDocumentCounts", "memories"]),
        corpus_revision: json_str(&status, &["data", "actualCorpusRevision"]),
        memory_records,
        candidate_records,
        link_records,
        job_records,
    })
}

#[test]
fn consolidation_closes_maintain_loop_through_apply_and_retrieval() -> TestResult {
    let ws = E2eWorkspace::create()?;

    // --- Arrange: isolated workspace with duplicates + ineligible controls.
    let init = ws.run_json("init", &["init"])?;
    ws.check(
        "init_envelope",
        json_str(&init, &["schema"]).as_deref() == Some("ee.response.v2")
            && json_bool(&init, &["success"]) == Some(true),
        "init must return a successful ee.response.v2 envelope",
    )?;

    let remember = |label: &str, content: &str, kind: &str, confidence: &str| {
        ws.run_json(
            label,
            &[
                "remember",
                content,
                "--level",
                "semantic",
                "--kind",
                kind,
                "--confidence",
                confidence,
                "--no-propose-candidates",
                "--no-auto-link",
            ],
        )
    };
    let survivor_id = memory_id_from_remember(
        "remember_survivor",
        &remember("remember_survivor", GROUP_PHRASE, "fact", "0.9")?,
    )?;
    let duplicate_id = memory_id_from_remember(
        "remember_duplicate",
        &remember("remember_duplicate", DUPLICATE_PHRASE, "fact", "0.4")?,
    )?;
    let wording_control_id = memory_id_from_remember(
        "remember_wording_control",
        &remember(
            "remember_wording_control",
            WORDING_CONTROL_PHRASE,
            "fact",
            "0.4",
        )?,
    )?;
    let kind_control_id = memory_id_from_remember(
        "remember_kind_control",
        &remember("remember_kind_control", GROUP_PHRASE, "decision", "0.4")?,
    )?;

    // Workflow-emitted indexing only: drain the remember-time index jobs.
    ws.run_json(
        "baseline_index_coalesce",
        &[
            "daemon",
            "--foreground",
            "--once",
            "--job",
            "index_coalesce",
        ],
    )?;
    let baseline = index_truth(&ws, "baseline_index_status")?;
    ws.check(
        "baseline_index_truthful",
        baseline.db_generation.is_some()
            && baseline.db_generation == baseline.index_generation
            && baseline.memory_documents == Some(4)
            && baseline.health == "ready",
        &format!(
            "baseline index must be ready and truthful for 4 memories: db={:?} index={:?} memories={:?} health={}",
            baseline.db_generation,
            baseline.index_generation,
            baseline.memory_documents,
            baseline.health
        ),
    )?;
    let create_audits_baseline =
        audit_action_count(&ws, "baseline_create_audits", "curation_candidate.create")?;

    // --- Dry run: plans the candidate but mutates nothing, proven by a full
    // public durable-state snapshot (memory rows incl. tombstones, candidate
    // rows across every status, total append-only audit count, workspace and
    // index generations, per-kind index counts) taken on either side.
    let workspace_a = ws.workspace.clone();
    let before_dry = durable_snapshot(&ws, &workspace_a, "before_dry_run")?;
    ws.check(
        "dry_run_baseline_shape",
        before_dry.memory_rows == 4
            && before_dry.tombstoned_rows == 0
            && before_dry.consolidate_candidates == 0
            && before_dry.index_memory_documents == Some(4),
        &format!("pre-dry-run fixture must be four live memories: {before_dry:?}"),
    )?;
    let dry_run = ws.run_json(
        "consolidation_dry_run",
        &[
            "daemon",
            "--foreground",
            "--once",
            "--job",
            "consolidation_pass",
            "--dry-run",
        ],
    )?;
    let dry_result = first_runner_result(&dry_run)
        .ok_or_else(|| format!("dry-run report missing runner result: {dry_run}"))?;
    ws.check(
        "dry_run_plans_one_candidate",
        json_bool(dry_result, &["details", "dryRun"]) == Some(true)
            && json_u64(dry_result, &["details", "plannedCandidates"]) == Some(1)
            && json_u64(dry_result, &["details", "insertedCandidates"]) == Some(0)
            && json_bool(dry_result, &["details", "durableMutation"]) == Some(false),
        &format!("dry-run must plan exactly one candidate without mutation: {dry_result}"),
    )?;
    ws.check(
        "dry_run_budget_bounded",
        json_u64(dry_result, &["itemsProcessed"]) == Some(1)
            && json_u64(dry_result, &["details", "selector", "maxCandidates"]) == Some(64)
            && json_u64(dry_result, &["details", "selector", "selectedCandidates"])
                .is_some_and(|selected| selected <= 64),
        &format!(
            "dry-run must report an actual bounded selector budget and item count: {dry_result}"
        ),
    )?;
    let after_dry = durable_snapshot(&ws, &workspace_a, "after_dry_run")?;
    ws.check(
        "dry_run_full_snapshot_unchanged",
        before_dry == after_dry,
        &format!(
            "dry-run must not mutate any durable object: before={before_dry:?} after={after_dry:?}"
        ),
    )?;

    // --- Real-CLI cancellation before mutation: zero item budget cancels.
    let cancelled = ws.run(
        "consolidation_zero_budget",
        &[
            "daemon",
            "--foreground",
            "--once",
            "--job",
            "consolidation_pass",
            "--item-limit",
            "0",
        ],
    )?;
    let cancelled_report = parse_json("consolidation_zero_budget", &cancelled)?;
    let cancelled_result = first_runner_result(&cancelled_report)
        .ok_or_else(|| format!("zero-budget report missing runner result: {cancelled_report}"))?;
    ws.check(
        "zero_budget_cancels_before_mutation",
        json_str(cancelled_result, &["outcome"]).as_deref() == Some("cancelled"),
        &format!("zero item budget must cancel the job: {cancelled_result}"),
    )?;
    let candidates_after_cancel = ws.run_json(
        "candidates_after_cancel",
        &["curate", "candidates", "--type", "consolidate", "--all"],
    )?;
    ws.check(
        "cancel_left_no_candidate_rows",
        json_at(&candidates_after_cancel, &["data", "candidates"])
            .and_then(JsonValue::as_array)
            .map(Vec::len)
            == Some(0),
        &format!("cancelled run must persist nothing: {candidates_after_cancel}"),
    )?;

    // --- Real run: deterministic deduplicated pending Consolidate candidate.
    let real_run = ws.run_json(
        "consolidation_real_run",
        &[
            "daemon",
            "--foreground",
            "--once",
            "--job",
            "consolidation_pass",
        ],
    )?;
    let real_result = first_runner_result(&real_run)
        .ok_or_else(|| format!("real-run report missing runner result: {real_run}"))?;
    let candidate_id = json_str(real_result, &["details", "candidateIds", "0"])
        .ok_or_else(|| format!("real run must emit one candidate id: {real_result}"))?;
    ws.check(
        "real_run_inserts_one_candidate",
        json_u64(real_result, &["details", "insertedCandidates"]) == Some(1)
            && json_u64(real_result, &["details", "plannedCandidates"]) == Some(1)
            && json_bool(real_result, &["details", "durableMutation"]) == Some(true),
        &format!("real run must insert exactly one deduplicated candidate: {real_result}"),
    )?;
    ws.check(
        "real_run_budget_accounted_and_clean",
        json_u64(real_result, &["budgetUsed", "violations"]) == Some(0)
            && json_u64(real_result, &["itemsProcessed"]) == Some(1)
            && json_u64(real_result, &["details", "selector", "maxCandidates"]) == Some(64),
        &format!(
            "real run must report zero budget violations within the bounded selector: {real_result}"
        ),
    )?;

    let rerun = ws.run_json(
        "consolidation_dedupe_rerun",
        &[
            "daemon",
            "--foreground",
            "--once",
            "--job",
            "consolidation_pass",
        ],
    )?;
    let rerun_result = first_runner_result(&rerun)
        .ok_or_else(|| format!("dedupe rerun report missing runner result: {rerun}"))?;
    ws.check(
        "rerun_dedupes_same_candidate",
        json_u64(rerun_result, &["details", "insertedCandidates"]) == Some(0)
            && json_u64(rerun_result, &["details", "alreadyPendingCandidates"]) == Some(1)
            && json_str(rerun_result, &["details", "candidateIds", "0"]).as_deref()
                == Some(candidate_id.as_str()),
        &format!(
            "re-run must plan the same deterministic candidate {candidate_id} and insert nothing: {rerun_result}"
        ),
    )?;

    let candidates = ws.run_json(
        "pending_candidate_row",
        &["curate", "candidates", "--type", "consolidate"],
    )?;
    ws.check(
        "candidate_row_shape",
        json_str(&candidates, &["data", "candidates", "0", "id"]).as_deref()
            == Some(candidate_id.as_str())
            && json_str(&candidates, &["data", "candidates", "0", "targetMemoryId"]).as_deref()
                == Some(duplicate_id.as_str()),
        &format!("pending candidate must target the duplicate {duplicate_id}: {candidates}"),
    )?;
    let create_audits_after_real =
        audit_action_count(&ws, "create_audits_after_real", "curation_candidate.create")?;
    ws.check(
        "creation_audit_written_once",
        create_audits_after_real == create_audits_baseline + 1,
        &format!(
            "real run must write exactly one creation audit: before={create_audits_baseline} after={create_audits_after_real}"
        ),
    )?;

    // --- Workspace isolation: a sibling workspace with its own duplicate
    // pair sees nothing from workspace A's steward runs, and running the
    // pass in B leaves A's full durable snapshot untouched.
    let workspace_b = ws.root.join("workspace-b");
    fs::create_dir_all(&workspace_b)
        .map_err(|error| format!("failed to create workspace-b: {error}"))?;
    let init_b = ws.run_json_in(&workspace_b, "init_workspace_b", &["init"])?;
    require_success_envelope("init_workspace_b", &init_b)?;
    for (label, content) in [
        (
            "remember_b_survivor",
            "Quillmarsh isolation gate: workspace B duplicate one.",
        ),
        (
            "remember_b_duplicate",
            "  quillmarsh   isolation gate: workspace B duplicate one. ",
        ),
    ] {
        let response = ws.run_json_in(
            &workspace_b,
            label,
            &[
                "remember",
                content,
                "--level",
                "semantic",
                "--kind",
                "fact",
                "--confidence",
                "0.7",
                "--no-propose-candidates",
                "--no-auto-link",
            ],
        )?;
        require_success_envelope(label, &response)?;
    }
    let b_candidates_before = ws.run_json_in(
        &workspace_b,
        "workspace_b_candidates_before",
        &["curate", "candidates", "--type", "consolidate", "--all"],
    )?;
    ws.check(
        "workspace_b_untouched_by_a_runs",
        json_at(&b_candidates_before, &["data", "candidates"])
            .and_then(JsonValue::as_array)
            .map(Vec::len)
            == Some(0)
            && audit_action_count_in(
                &ws,
                &workspace_b,
                "workspace_b_create_audits_before",
                "curation_candidate.create",
            )? == 0,
        &format!(
            "workspace A's consolidation runs must not leak candidates or audits into B: {b_candidates_before}"
        ),
    )?;
    let a_before_b_run = durable_snapshot(&ws, &workspace_a, "a_before_b_run")?;
    let b_run = ws.run_json_in(
        &workspace_b,
        "consolidation_pass_workspace_b",
        &[
            "daemon",
            "--foreground",
            "--once",
            "--job",
            "consolidation_pass",
        ],
    )?;
    let b_result = first_runner_result(&b_run)
        .ok_or_else(|| format!("workspace B run missing runner result: {b_run}"))?;
    let b_candidate_id = json_str(b_result, &["details", "candidateIds", "0"])
        .ok_or_else(|| format!("workspace B run must emit its own candidate id: {b_result}"))?;
    ws.check(
        "workspace_b_pass_stays_local",
        json_u64(b_result, &["details", "insertedCandidates"]) == Some(1)
            && b_candidate_id != candidate_id,
        &format!(
            "workspace B must mint its own candidate, distinct from A's {candidate_id}: {b_result}"
        ),
    )?;
    let a_after_b_run = durable_snapshot(&ws, &workspace_a, "a_after_b_run")?;
    ws.check(
        "workspace_a_snapshot_immune_to_b",
        a_before_b_run == a_after_b_run,
        &format!(
            "workspace B's pass must not mutate A's durable state: before={a_before_b_run:?} after={a_after_b_run:?}"
        ),
    )?;

    // --- Validate and apply through the public curation commands.
    let validate = ws.run_json("curate_validate", &["curate", "validate", &candidate_id])?;
    ws.check(
        "validate_approves",
        json_str(&validate, &["data", "mutation", "toStatus"]).as_deref() == Some("approved"),
        &format!("validate must approve the candidate: {validate}"),
    )?;
    ws.check(
        "validate_audit_written",
        audit_action_count(&ws, "validate_audits", "curation_candidate.validate")? == 1,
        "validate must write its audit row",
    )?;

    let apply = ws.run_json("curate_apply", &["curate", "apply", &candidate_id])?;
    ws.check(
        "apply_consolidate_absorb",
        json_str(&apply, &["data", "application", "decision"]).as_deref()
            == Some("consolidate_absorb")
            && json_str(&apply, &["data", "application", "status"]).as_deref() == Some("applied")
            && json_bool(&apply, &["data", "durableMutation"]) == Some(true),
        &format!("apply must run the consolidate-absorb decision: {apply}"),
    )?;
    let apply_changes = json_at(&apply, &["data", "application", "changes"])
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default();
    let change_after = |field: &str| -> Option<String> {
        apply_changes.iter().find_map(|change| {
            (change.get("field").and_then(JsonValue::as_str) == Some(field))
                .then(|| {
                    change
                        .get("after")
                        .and_then(JsonValue::as_str)
                        .map(str::to_owned)
                })
                .flatten()
        })
    };
    ws.check(
        "apply_changes_name_survivor_and_lineage",
        change_after("consolidatedIntoMemoryId").as_deref() == Some(survivor_id.as_str())
            && change_after("tombstoned").as_deref() == Some("true")
            && change_after("derivedFromLinkId").is_some()
            && change_after("searchIndexJobId").is_some(),
        &format!("apply changes must record the survivor and lineage structurally: {apply}"),
    )?;

    // Source preservation + lifecycle posture via a different read surface.
    let memories = ws.run_json("memory_list_after_apply", &["memory", "list"])?;
    let memory_entry = |id: &str| -> Option<&JsonValue> {
        json_at(&memories, &["data", "memories"])?
            .as_array()?
            .iter()
            .find(|entry| entry.get("id").and_then(JsonValue::as_str) == Some(id))
    };
    let duplicate_entry = memory_entry(&duplicate_id);
    let survivor_entry = memory_entry(&survivor_id);
    ws.check(
        "duplicate_preserved_but_tombstoned",
        duplicate_entry.is_some_and(|entry| {
            entry.get("is_tombstoned").and_then(JsonValue::as_bool) == Some(true)
                || entry.get("isTombstoned").and_then(JsonValue::as_bool) == Some(true)
        }),
        &format!(
            "duplicate {duplicate_id} must be preserved as a tombstoned row, not deleted: {memories}"
        ),
    )?;
    ws.check(
        "survivor_untouched",
        survivor_entry.is_some_and(|entry| {
            entry.get("content").and_then(JsonValue::as_str) == Some(GROUP_PHRASE)
                && entry.get("is_tombstoned").and_then(JsonValue::as_bool) != Some(true)
                && entry.get("isTombstoned").and_then(JsonValue::as_bool) != Some(true)
        }),
        &format!("survivor {survivor_id} must keep its content and stay live: {memories}"),
    )?;

    // Lineage + provenance explanation via `ee why`.
    let why_survivor = ws.run_json("why_survivor", &["why", &survivor_id])?;
    let survivor_links = json_at(&why_survivor, &["data", "links"])
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default();
    ws.check(
        "why_survivor_shows_derived_from_lineage",
        survivor_links.iter().any(|link| {
            link.get("relation").and_then(JsonValue::as_str) == Some("derived_from")
                && link.get("linkedMemoryId").and_then(JsonValue::as_str)
                    == Some(duplicate_id.as_str())
        }),
        &format!("why must explain the derived_from lineage to {duplicate_id}: {why_survivor}"),
    )?;
    let why_duplicate = ws.run_json("why_duplicate", &["why", &duplicate_id])?;
    ws.check(
        "why_duplicate_history_names_candidate",
        json_mentions_text(&why_duplicate, &candidate_id),
        &format!(
            "why history for the absorbed duplicate must cite candidate {candidate_id}: {why_duplicate}"
        ),
    )?;

    // Append-only audit chain.
    ws.check(
        "apply_audit_written",
        audit_action_count(&ws, "apply_audits", "curation_candidate.apply")? == 1,
        "apply must write exactly one apply audit row",
    )?;
    let verify = ws.run_json("audit_verify", &["audit", "verify"])?;
    require_success_envelope("audit_verify", &verify)?;
    ws.check(
        "audit_chain_intact",
        json_str(&verify, &["data", "schema"]).as_deref() == Some("ee.audit.verify.v1")
            && json_bool(&verify, &["data", "integrity_ok"]) == Some(true),
        &format!(
            "audit verify must emit the canonical envelope with an intact hash chain: {verify}"
        ),
    )?;

    // --- Index truth: stale is reported honestly, then the workflow-emitted
    // job (and only that job) restores a truthful generation.
    let stale = index_truth(&ws, "index_status_stale_after_apply")?;
    ws.check(
        "apply_reports_honest_staleness",
        match (stale.db_generation, stale.index_generation) {
            (Some(db), Some(index)) => db > index,
            _ => false,
        } && stale.health == "stale",
        &format!(
            "apply must leave an honestly stale index until the workflow job runs: db={:?} index={:?} health={}",
            stale.db_generation, stale.index_generation, stale.health
        ),
    )?;
    ws.run_json(
        "post_apply_index_coalesce",
        &[
            "daemon",
            "--foreground",
            "--once",
            "--job",
            "index_coalesce",
        ],
    )?;
    let truthful = index_truth(&ws, "index_status_truthful")?;
    ws.check(
        "workflow_job_restores_truthful_index",
        truthful.db_generation.is_some()
            && truthful.db_generation == truthful.index_generation
            && truthful.memory_documents == Some(3)
            && truthful.health == "ready",
        &format!(
            "index_coalesce must publish a truthful generation with the duplicate dropped: db={:?} index={:?} memories={:?} health={}",
            truthful.db_generation,
            truthful.index_generation,
            truthful.memory_documents,
            truthful.health
        ),
    )?;

    // --- Deduplicated retrieval, proven on structured identity fields:
    // exactly one survivor result/item, zero absorbed duplicate, each
    // control exactly once.
    let search = ws.run_json(
        "search_group_phrase",
        &["search", SEARCH_QUERY, "--limit", "10"],
    )?;
    require_success_envelope("search_group_phrase", &search)?;
    let search_ids = item_memory_ids(&search, &["data", "results"], "search_group_phrase")?;
    ws.check(
        "search_selects_consolidated_once",
        id_count(&search_ids, &survivor_id) == 1 && id_count(&search_ids, &duplicate_id) == 0,
        &format!(
            "search must return the survivor exactly once and the duplicate never: {search_ids:?}"
        ),
    )?;
    ws.check(
        "search_keeps_controls_distinct",
        id_count(&search_ids, &wording_control_id) == 1
            && id_count(&search_ids, &kind_control_id) == 1,
        &format!("each control must remain exactly one distinct result: {search_ids:?}"),
    )?;
    let pack = ws.run_json(
        "pack_group_phrase",
        &["pack", SEARCH_QUERY, "--max-tokens", "2000"],
    )?;
    require_success_envelope("pack_group_phrase", &pack)?;
    let pack_ids = item_memory_ids(&pack, &["data", "pack", "items"], "pack_group_phrase")?;
    ws.check(
        "pack_includes_survivor_once_never_duplicate",
        id_count(&pack_ids, &survivor_id) == 1 && id_count(&pack_ids, &duplicate_id) == 0,
        &format!(
            "pack must contain the survivor as exactly one item and the duplicate never: {pack_ids:?}"
        ),
    )?;
    ws.check(
        "pack_keeps_controls_distinct",
        id_count(&pack_ids, &wording_control_id) == 1 && id_count(&pack_ids, &kind_control_id) == 1,
        &format!("each control must pack as exactly one distinct item: {pack_ids:?}"),
    )?;

    // --- Idempotency: steward re-run and apply replay create nothing new,
    // proven by a full durable snapshot bracketing both replays.
    let before_replay = durable_snapshot(&ws, &workspace_a, "before_replay")?;
    let idempotent_run = ws.run_json(
        "consolidation_idempotent_rerun",
        &[
            "daemon",
            "--foreground",
            "--once",
            "--job",
            "consolidation_pass",
        ],
    )?;
    let idempotent_result = first_runner_result(&idempotent_run)
        .ok_or_else(|| format!("idempotent rerun missing runner result: {idempotent_run}"))?;
    ws.check(
        "steward_rerun_is_idempotent",
        json_u64(idempotent_result, &["details", "plannedCandidates"]) == Some(0)
            && json_u64(idempotent_result, &["details", "insertedCandidates"]) == Some(0),
        &format!(
            "after absorb the tombstoned duplicate must leave nothing to plan: {idempotent_result}"
        ),
    )?;
    let replay = ws.run_json("curate_apply_replay", &["curate", "apply", &candidate_id])?;
    ws.check(
        "apply_replay_is_idempotent",
        json_str(&replay, &["data", "application", "status"]).as_deref() == Some("already_applied")
            && json_bool(&replay, &["data", "durableMutation"]) == Some(false),
        &format!("re-apply must be an idempotent no-op: {replay}"),
    )?;
    ws.check(
        "no_duplicate_audit_rows_after_replay",
        audit_action_count(&ws, "apply_audits_after_replay", "curation_candidate.apply")? == 1
            && audit_action_count(
                &ws,
                "create_audits_after_replay",
                "curation_candidate.create",
            )? == create_audits_baseline + 1,
        "replay must not append duplicate audit rows",
    )?;
    let final_truth = index_truth(&ws, "final_index_status")?;
    ws.check(
        "final_generation_stable",
        final_truth.db_generation == truthful.db_generation
            && final_truth.index_generation == truthful.index_generation,
        &format!(
            "idempotent re-runs must not move generation: db={:?} index={:?}",
            final_truth.db_generation, final_truth.index_generation
        ),
    )?;
    // Full durable-object census after the replays: every count identical to
    // the pre-replay snapshot, and the absolute values match the closed loop
    // (4 memory rows with exactly the absorbed duplicate tombstoned, one
    // candidate across all statuses, 3 live index documents).
    let after_replay = durable_snapshot(&ws, &workspace_a, "after_replay")?;
    ws.check(
        "replay_full_snapshot_unchanged",
        before_replay == after_replay,
        &format!(
            "replays must not change any durable object: before={before_replay:?} after={after_replay:?}"
        ),
    )?;
    ws.check(
        "replay_absolute_census",
        after_replay.memory_rows == 4
            && after_replay.tombstoned_rows == 1
            && after_replay.consolidate_candidates == 1
            && after_replay.index_memory_documents == Some(3)
            && after_replay.db_generation == after_replay.index_generation,
        &format!("closed-loop census must hold after replay: {after_replay:?}"),
    )?;
    let why_after_replay = ws.run_json("why_survivor_after_replay", &["why", &survivor_id])?;
    let lineage_links_after_replay = json_at(&why_after_replay, &["data", "links"])
        .and_then(JsonValue::as_array)
        .map(|links| {
            links
                .iter()
                .filter(|link| {
                    link.get("relation").and_then(JsonValue::as_str) == Some("derived_from")
                        && link.get("linkedMemoryId").and_then(JsonValue::as_str)
                            == Some(duplicate_id.as_str())
                })
                .count()
        })
        .unwrap_or(0);
    ws.check(
        "replay_single_lineage_link",
        lineage_links_after_replay == 1,
        &format!(
            "exactly one derived_from lineage link must survive the replays: {why_after_replay}"
        ),
    )?;

    // --- Planted negative: a stale/cross-workspace candidate injected
    // through the PUBLIC diag surface — steward-claiming provenance
    // (rule_engine + mem_ source id) over a memory that is not part of this
    // workspace's duplicate group, under a candidate id that is not the
    // steward derivation. Apply must block with typed issues and mutate
    // nothing: no tombstone, no lineage, no jobs, no false Ready.
    let tampered_candidate_id = "curate_00000000000000000000000042";
    let foreign_source_id = "mem_00000000000000000000000099";
    let inject = ws.run_json(
        "inject_tampered_candidate",
        &[
            "diag",
            "curation-candidate",
            "--candidate-id",
            tampered_candidate_id,
            "--candidate-type",
            "consolidate",
            "--source-type",
            "rule_engine",
            "--source-id",
            foreign_source_id,
            "--target-memory-id",
            &wording_control_id,
            "--proposed-content",
            WORDING_CONTROL_PHRASE,
            "--status",
            "pending",
        ],
    )?;
    require_success_envelope("inject_tampered_candidate", &inject)?;
    let tampered_validate = ws.run_json(
        "validate_tampered_candidate",
        &["curate", "validate", tampered_candidate_id],
    )?;
    require_success_envelope("validate_tampered_candidate", &tampered_validate)?;
    let tampered_apply = ws.run_json(
        "apply_tampered_candidate",
        &["curate", "apply", tampered_candidate_id],
    )?;
    require_success_envelope("apply_tampered_candidate", &tampered_apply)?;
    let tampered_error_codes = json_at(&tampered_apply, &["data", "application", "errors"])
        .and_then(JsonValue::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|issue| issue.get("code").and_then(JsonValue::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ws.check(
        "tampered_candidate_apply_blocked",
        json_str(&tampered_apply, &["data", "application", "status"]).as_deref() == Some("blocked")
            && json_bool(&tampered_apply, &["data", "durableMutation"]) == Some(false)
            && tampered_error_codes
                .iter()
                .any(|code| code == "consolidate_absorb_candidate_id_mismatch"),
        &format!(
            "tampered candidate must block with the id-mismatch issue, got codes {tampered_error_codes:?}: {tampered_apply}"
        ),
    )?;
    // Durable rows and index effects are untouched by the blocked apply.
    // (The candidate injection itself legitimately bumps the workspace
    // generation via the candidate-table trigger, so the proof compares the
    // exact memory/link/job rows and index document truth, not generations.)
    let after_tampered = durable_snapshot(&ws, &workspace_a, "after_tampered")?;
    ws.check(
        "tampered_candidate_left_rows_untouched",
        after_tampered.memory_records == after_replay.memory_records
            && after_tampered.link_records == after_replay.link_records
            && after_tampered.job_records == after_replay.job_records
            && after_tampered.tombstoned_rows == 1
            && after_tampered.index_memory_documents == Some(3),
        &format!(
            "blocked tampered apply must not touch memories/links/jobs/index: {after_tampered:?}"
        ),
    )?;
    ws.check(
        "tampered_candidate_row_recorded_not_applied",
        after_tampered.consolidate_candidates == after_replay.consolidate_candidates + 1,
        &format!(
            "the injected candidate row itself must exist exactly once and never transition to applied: before={} after={}",
            after_replay.consolidate_candidates, after_tampered.consolidate_candidates
        ),
    )?;

    // --- Every emitted ee.test_event.v1 line must parse and validate; a
    // deliberately corrupted log must FAIL the same validator (planted
    // negative proving the validator cannot green a broken event stream).
    let event_count = validate_event_log(&ws.event_log)?;
    ws.check(
        "event_log_valid",
        event_count >= 40,
        &format!("expected a substantial validated event stream, got {event_count} lines"),
    )?;
    let corrupted_log = ws.root.join("corrupted-events.jsonl");
    let mut corrupted = fs::read_to_string(&ws.event_log)
        .map_err(|error| format!("read event log for corruption probe: {error}"))?;
    corrupted.push_str(
        "{\"schema\":\"ee.wrong_schema.v1\",\"ts\":\"x\",\"test_id\":\"x\",\"kind\":\"note\"}\n",
    );
    corrupted.push_str("this line is not json at all\n");
    fs::write(&corrupted_log, corrupted)
        .map_err(|error| format!("write corrupted event log: {error}"))?;
    ws.check(
        "broken_event_log_fails_validation",
        validate_event_log(&corrupted_log).is_err(),
        "a corrupted event log must fail the validator, never green",
    )?;

    let done = TestEvent::new(TEST_ID, EventKind::Note)
        .with_field("label", "maintain_loop_closed")
        .with_field("workspace_root", ws.root.display().to_string());
    let _ = log_event_to(&ws.event_log, LogLevel::Verbose, &done);
    Ok(())
}

/// Strict ee.test_event.v1 line validator: every non-empty line must parse
/// as JSON with the exact schema id and non-empty ts/test_id/kind. Returns
/// the validated line count; any defect is an error naming the line.
fn validate_event_log(path: &std::path::Path) -> Result<usize, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("read event log {}: {error}", path.display()))?;
    let mut validated = 0_usize;
    for (line_number, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: JsonValue = serde_json::from_str(line).map_err(|error| {
            format!("event log line {}: invalid JSON: {error}", line_number + 1)
        })?;
        if event.get("schema").and_then(JsonValue::as_str) != Some("ee.test_event.v1") {
            return Err(format!(
                "event log line {}: schema is not ee.test_event.v1: {event}",
                line_number + 1
            ));
        }
        for field in ["ts", "test_id", "kind"] {
            if event
                .get(field)
                .and_then(JsonValue::as_str)
                .is_none_or(str::is_empty)
            {
                return Err(format!(
                    "event log line {}: missing or empty required field {field}: {event}",
                    line_number + 1
                ));
            }
        }
        validated += 1;
    }
    if validated == 0 {
        return Err(format!("event log {} contains no events", path.display()));
    }
    Ok(validated)
}
