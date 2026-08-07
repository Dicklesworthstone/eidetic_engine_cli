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
        let index = self.command_counter.get();
        self.command_counter.set(index + 1);
        let workspace_arg = self.workspace.display().to_string();
        let mut full_args = vec!["--workspace", workspace_arg.as_str(), "--json"];
        full_args.extend_from_slice(args);
        let started = Instant::now();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
        command.current_dir(&self.workspace);
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
            .with_field("workspace", self.workspace.display().to_string())
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
        let output = self.run(label, args)?;
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

/// Number of times `needle` occurs as a JSON string value anywhere in `value`.
fn count_string_values(value: &JsonValue, needle: &str) -> usize {
    match value {
        JsonValue::String(text) => usize::from(text == needle),
        JsonValue::Array(items) => items
            .iter()
            .map(|item| count_string_values(item, needle))
            .sum(),
        JsonValue::Object(map) => map
            .values()
            .map(|item| count_string_values(item, needle))
            .sum(),
        _ => 0,
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

fn audit_action_count(ws: &E2eWorkspace, label: &str, action: &str) -> Result<u64, String> {
    let timeline = ws.run_json(
        label,
        &["audit", "timeline", "--action", action, "--limit", "50"],
    )?;
    json_u64(&timeline, &["data", "pagination", "total_count"]).ok_or_else(|| {
        format!("{label}: audit timeline missing pagination.total_count: {timeline}")
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

    // --- Dry run: plans the candidate but mutates nothing.
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
    let candidates_after_dry_run = ws.run_json(
        "candidates_after_dry_run",
        &["curate", "candidates", "--type", "consolidate", "--all"],
    )?;
    let dry_candidates = json_at(&candidates_after_dry_run, &["data", "candidates"])
        .and_then(JsonValue::as_array)
        .map(Vec::len);
    ws.check(
        "dry_run_no_candidate_rows",
        dry_candidates == Some(0),
        &format!("dry-run must persist no candidates: {candidates_after_dry_run}"),
    )?;
    let after_dry = index_truth(&ws, "index_after_dry_run")?;
    ws.check(
        "dry_run_no_generation_or_index_mutation",
        after_dry.db_generation == baseline.db_generation
            && after_dry.index_generation == baseline.index_generation
            && after_dry.memory_documents == baseline.memory_documents,
        &format!(
            "dry-run must not move generation or index: before db={:?}/idx={:?} after db={:?}/idx={:?}",
            baseline.db_generation,
            baseline.index_generation,
            after_dry.db_generation,
            after_dry.index_generation
        ),
    )?;
    let create_audits_after_dry = audit_action_count(
        &ws,
        "create_audits_after_dry_run",
        "curation_candidate.create",
    )?;
    ws.check(
        "dry_run_no_audit_mutation",
        create_audits_after_dry == create_audits_baseline,
        &format!(
            "dry-run must write no creation audits: before={create_audits_baseline} after={create_audits_after_dry}"
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
            && json_bool(real_result, &["details", "durableMutation"]) == Some(true)
            && json_at(real_result, &["budgetUsed"]).is_some(),
        &format!(
            "real run must insert exactly one candidate with budget accounting: {real_result}"
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
            && count_string_values(&candidates, &duplicate_id) >= 1,
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
    ws.check(
        "apply_changes_name_survivor_and_lineage",
        count_string_values(&apply, &survivor_id) >= 1
            && json_at(&apply, &["data", "application", "changes"])
                .and_then(JsonValue::as_array)
                .is_some_and(|changes| !changes.is_empty()),
        &format!("apply changes must record the survivor and lineage: {apply}"),
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
        count_string_values(&why_duplicate, &candidate_id) >= 1,
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
    ws.check(
        "audit_chain_intact",
        json_bool(&verify, &["data", "integrity_ok"]) == Some(true),
        &format!("audit hash chain must verify after the full loop: {verify}"),
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

    // --- Deduplicated retrieval: survivor once, duplicate gone, controls
    // distinct.
    let search = ws.run_json(
        "search_group_phrase",
        &["search", SEARCH_QUERY, "--limit", "10"],
    )?;
    let results = json_at(&search, &["data", "results"])
        .cloned()
        .unwrap_or(JsonValue::Null);
    ws.check(
        "search_selects_consolidated_once",
        count_string_values(&results, &survivor_id) >= 1
            && count_string_values(&results, &duplicate_id) == 0,
        &format!(
            "search must select the consolidated survivor and never the absorbed duplicate: {search}"
        ),
    )?;
    ws.check(
        "search_keeps_controls_distinct",
        count_string_values(&results, &wording_control_id) >= 1
            && count_string_values(&results, &kind_control_id) >= 1,
        &format!("controls must remain distinct search results: {search}"),
    )?;
    let pack = ws.run_json(
        "pack_group_phrase",
        &["pack", SEARCH_QUERY, "--max-tokens", "2000"],
    )?;
    ws.check(
        "pack_excludes_absorbed_duplicate",
        count_string_values(&pack, &survivor_id) >= 1
            && count_string_values(&pack, &duplicate_id) == 0,
        &format!("pack must include the survivor once and never the duplicate: {pack}"),
    )?;

    // --- Idempotency: steward re-run and apply replay create nothing new.
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

    let done = TestEvent::new(TEST_ID, EventKind::Note)
        .with_field("label", "maintain_loop_closed")
        .with_field("workspace_root", ws.root.display().to_string());
    let _ = log_event_to(&ws.event_log, LogLevel::Verbose, &done);
    Ok(())
}
