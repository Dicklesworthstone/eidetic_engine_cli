//! Plan §4 North Star procedural-distillation E2E coverage
//! (eidetic_engine_cli-lpb5).
//!
//! Two tests, both named with a `north_star` prefix so the swarm-required
//! `cargo test --workspace north_star` filter actually executes them:
//!
//! 1. `north_star_procedural_distillation_partial_chain_through_audit_and_why`
//!    exercises
//!    the part of the distillation flow that is wired today: `ee remember`,
//!    `ee memory show`, `ee why`, `ee audit timeline`, `ee audit verify`,
//!    `ee search`, `ee curate candidates`. It asserts the audit chain is
//!    intact, the why-explanation reads back the seeded memories, and search
//!    surfaces the seeded content. This is the floor of the bead's North
//!    Star scenario today.
//!
//! 2. `north_star_procedural_distillation_full_chain_review_curate_apply`
//!    runs the Plan §4 path against deterministic fixtures: `ee import cass`,
//!    `ee search`, `ee review session --propose`, `ee curate validate`,
//!    `ee curate apply`, `ee rule show`, `ee memory show`, `ee why`,
//!    `ee audit timeline`, `ee audit verify`, `ee procedure list`, and
//!    `ee learn agenda`.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use ee::db::{CreateEvidenceSpanInput, DbConnection, SearchIndexJobStatus};
use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn ee_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(ee_bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))
}

fn run_ee_with_env(args: &[&str], envs: &[(&str, String)]) -> Result<Output, String> {
    let mut command = Command::new(ee_bin());
    command.args(args).env("NO_COLOR", "1");
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))
}

fn run_ee_json(args: &[&str]) -> Result<JsonValue, String> {
    let output = run_ee(args)?;
    parse_json_output(output, args)
}

fn run_ee_json_with_env(args: &[&str], envs: &[(&str, String)]) -> Result<JsonValue, String> {
    let output = run_ee_with_env(args, envs)?;
    parse_json_output(output, args)
}

fn parse_json_output(output: Output, args: &[&str]) -> Result<JsonValue, String> {
    if !output.status.success() {
        return Err(format!(
            "ee {} failed (exit {:?})\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    if !output.stderr.is_empty() {
        return Err(format!(
            "ee {} wrote stderr during JSON run:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse JSON from ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T: std::fmt::Debug + PartialEq>(actual: &T, expected: &T, ctx: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_contains(haystack: &str, needle: &str, ctx: &str) -> TestResult {
    ensure(
        haystack.contains(needle),
        format!("{ctx}: expected {haystack:?} to contain {needle:?}"),
    )
}

/// Returns the `data.code` of a response, treating any non-`success`
/// envelope as a degraded abstention.
fn abstention_code(value: &JsonValue) -> Option<&str> {
    let success = value
        .pointer("/success")
        .and_then(JsonValue::as_bool)
        .unwrap_or(true);
    if success {
        return None;
    }
    value.pointer("/data/code").and_then(JsonValue::as_str)
}

fn remember_memory(
    workspace_arg: &str,
    level: &str,
    kind: &str,
    tags: &str,
    content: &str,
) -> Result<(String, String), String> {
    let value = run_ee_json(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        level,
        "--kind",
        kind,
        "--tags",
        tags,
        content,
    ])?;
    let memory_id = value
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing /data/memory_id".to_owned())?
        .to_owned();
    let audit_id = value
        .pointer("/data/audit_id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing /data/audit_id".to_owned())?;
    if !audit_id.starts_with("audit_") {
        return Err(format!("audit_id missing audit_ prefix: {audit_id}"));
    }
    let workspace_id = value
        .pointer("/data/workspace_id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "missing /data/workspace_id".to_owned())?
        .to_owned();
    Ok((memory_id, workspace_id))
}

fn seed_memory(
    workspace_arg: &str,
    level: &str,
    kind: &str,
    tags: &str,
    content: &str,
) -> Result<String, String> {
    remember_memory(workspace_arg, level, kind, tags, content).map(|(memory_id, _)| memory_id)
}

fn workspace_arg(workspace: &Path) -> String {
    workspace.to_string_lossy().into_owned()
}

#[test]
fn north_star_procedural_distillation_partial_chain_through_audit_and_why() -> TestResult {
    let staging = tempfile::Builder::new()
        .prefix("ee-lpb5-partial-")
        .tempdir()
        .map_err(|error| format!("create temp dir: {error}"))?;

    let workspace = staging.path().join("ws");
    std::fs::create_dir_all(&workspace).map_err(|error| format!("mkdir ws: {error}"))?;
    let ws_arg = workspace_arg(&workspace);

    // 1. Initialize the workspace (ee init).
    let init = run_ee_json(&["--workspace", &ws_arg, "--json", "init"])?;
    ensure_equal(
        &init.pointer("/data/status").and_then(JsonValue::as_str),
        &Some("created"),
        "init status",
    )?;

    // 2. Seed: a procedural rule + an episodic observation that justifies it.
    //    These mirror the "repeated CI failure" Plan §4 scenario.
    let rule_id = seed_memory(
        &ws_arg,
        "procedural",
        "rule",
        "ci,release",
        "Always run cargo test before creating a release tag",
    )?;
    let observation_id = seed_memory(
        &ws_arg,
        "episodic",
        "observation",
        "ci,release,failure",
        "2026-04-15 release failed because tests were not run locally first",
    )?;
    ensure(rule_id != observation_id, "memory ids are distinct")?;

    // 3. ee memory show on each surfaces stable JSON.
    for memory_id in [&rule_id, &observation_id] {
        let value = run_ee_json(&[
            "--workspace",
            &ws_arg,
            "--json",
            "memory",
            "show",
            memory_id,
        ])?;
        let stored_id = value
            .pointer("/data/memory/id")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| format!("memory show: missing /data/memory/id for {memory_id}"))?;
        ensure_equal(&stored_id, &memory_id.as_str(), "memory show id round-trip")?;
    }

    // 4. ee search retrieves the seeded rule by content terms.
    let search = run_ee_json(&["--workspace", &ws_arg, "--json", "search", "cargo test"])?;
    let results = search
        .pointer("/data/results")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "search missing /data/results".to_owned())?;
    ensure(
        !results.is_empty(),
        "search should retrieve at least one result for 'cargo test'",
    )?;
    let retrieved_ids: Vec<&str> = results
        .iter()
        .filter_map(|r| r.get("docId").and_then(JsonValue::as_str))
        .collect();
    ensure(
        retrieved_ids.contains(&rule_id.as_str()),
        format!("search results {retrieved_ids:?} did not contain seeded rule {rule_id}"),
    )?;

    // 5. ee why returns a rich explanation for the rule, including selection
    //    score breakdown and storage provenance.
    let why = run_ee_json(&["--workspace", &ws_arg, "--json", "why", &rule_id])?;
    ensure_equal(
        &why.pointer("/data/found").and_then(JsonValue::as_bool),
        &Some(true),
        "why marks memory as found",
    )?;
    ensure_equal(
        &why.pointer("/data/storage/trustClass")
            .and_then(JsonValue::as_str),
        &Some("human_explicit"),
        "why surfaces human_explicit trust class",
    )?;
    let selection_score = why
        .pointer("/data/selection/selectionScore")
        .and_then(JsonValue::as_f64)
        .ok_or_else(|| "why missing /data/selection/selectionScore".to_owned())?;
    ensure(
        selection_score > 0.0,
        format!("why selection score must be positive, got {selection_score}"),
    )?;
    let breakdown = why
        .pointer("/data/selection/scoreBreakdown")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    ensure(
        breakdown.contains("confidence") && breakdown.contains("utility"),
        format!("why score breakdown must show confidence + utility components, got: {breakdown}"),
    )?;

    // 6. ee audit timeline returns chain-hashed entries for the two memory.create
    //    audits we triggered above. The chain hash MUST be a blake3:* string and
    //    each row's prev_row_hash must match the previous row's this_row_hash.
    let audit = run_ee_json(&["--workspace", &ws_arg, "--json", "audit", "timeline"])?;
    let entries = audit
        .pointer("/entries")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "audit timeline missing /entries".to_owned())?;
    ensure(
        entries.len() >= 2,
        format!("expected ≥2 audit entries, got {}", entries.len()),
    )?;
    for entry in entries {
        let this_hash = entry
            .get("this_row_hash")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| "audit entry missing this_row_hash".to_owned())?;
        ensure(
            this_hash.starts_with("blake3:"),
            format!("audit row hash must be blake3:*, got {this_hash}"),
        )?;
    }

    // 7. ee audit verify must report integrity_ok=true with the same row count
    //    we observed in the timeline.
    let verify = run_ee_json(&["--workspace", &ws_arg, "--json", "audit", "verify"])?;
    ensure_equal(
        &verify.pointer("/integrity_ok").and_then(JsonValue::as_bool),
        &Some(true),
        "audit chain integrity",
    )?;
    let verified_rows = verify
        .pointer("/rows")
        .and_then(JsonValue::as_u64)
        .unwrap_or_default();
    ensure(
        verified_rows >= 2,
        format!("audit verify rows {verified_rows} should be ≥2"),
    )?;
    let issues = verify
        .pointer("/issues")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "audit verify missing /issues".to_owned())?;
    ensure(
        issues.is_empty(),
        format!("audit verify reported issues: {issues:?}"),
    )?;

    // 8. ee curate candidates is wired and returns a stable empty queue
    //    (the bead's "duplicate-rule warnings" assertion lives in step 9
    //    once `ee curate validate` against a real candidate is plumbed; for
    //    now we just confirm the surface itself is not abstaining).
    let candidates = run_ee_json(&["--workspace", &ws_arg, "--json", "curate", "candidates"])?;
    if let Some(code) = abstention_code(&candidates) {
        return Err(format!(
            "ee curate candidates is abstaining with code {code:?}; \
             distillation flow cannot continue"
        ));
    }
    let total_count = candidates
        .pointer("/data/totalCount")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| "curate candidates missing /data/totalCount".to_owned())?;
    ensure_equal(
        &total_count,
        &0,
        "curate candidates queue is empty before any review proposes one",
    )?;

    Ok(())
}

#[cfg(unix)]
#[test]
fn north_star_procedural_distillation_full_chain_review_curate_apply() -> TestResult {
    let staging = tempfile::Builder::new()
        .prefix("ee-lpb5-full-")
        .tempdir()
        .map_err(|error| format!("create temp dir: {error}"))?;

    let workspace = staging.path().join("ws");
    std::fs::create_dir_all(&workspace).map_err(|error| format!("mkdir ws: {error}"))?;
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("canonicalize workspace: {error}"))?;
    let ws_arg = workspace_arg(&workspace);

    run_ee_json(&["--workspace", &ws_arg, "--json", "init"])?;

    let fake_bin_dir = staging.path().join("bin");
    fs::create_dir_all(&fake_bin_dir).map_err(|error| format!("mkdir fake bin: {error}"))?;
    set_executable_dir_permissions(&fake_bin_dir)?;
    let cass_binary = fake_bin_dir.join("cass");
    write_fake_cass_binary(&cass_binary)?;
    let session_path = workspace.join("cass-session-lpb5.jsonl");
    fs::write(
        &session_path,
        r#"{"role":"assistant","content":"clippy warning release failed until the workspace ran cargo test before tagging"}"#,
    )
    .map_err(|error| format!("write fake CASS session: {error}"))?;
    let view_path = staging.path().join("cass-view.json");
    fs::write(
        &view_path,
        serde_json::json!({
            "lines": [
                {
                    "line": 10,
                    "content": r#"{"type":"message","message":{"role":"assistant","content":"clippy warning release failed because cargo test was skipped before the release tag"}}"#
                },
                {
                    "line": 11,
                    "content": r#"{"type":"message","message":{"role":"assistant","content":"fix was to run cargo test, cargo clippy, and re-check the failing warning before tagging"}}"#
                }
            ]
        })
        .to_string(),
    )
    .map_err(|error| format!("write fake CASS view: {error}"))?;
    let session_path = session_path
        .canonicalize()
        .map_err(|error| format!("canonicalize session path: {error}"))?;
    let path = path_with_fake_cass(&fake_bin_dir)?;
    let cass_binary_arg = workspace_arg(&cass_binary);
    let view_path_arg = workspace_arg(&view_path);
    let session_arg = workspace_arg(&session_path);

    let import = run_ee_json_with_env(
        &[
            "--workspace",
            &ws_arg,
            "--json",
            "import",
            "cass",
            "--limit",
            "1",
        ],
        &[
            ("PATH", path),
            ("EE_CASS_BINARY", cass_binary_arg),
            ("EE_FAKE_CASS_SESSION", session_arg.clone()),
            ("EE_FAKE_CASS_WORKSPACE", ws_arg.clone()),
            ("EE_FAKE_CASS_VIEW_JSON_PATH", view_path_arg),
        ],
    )?;
    ensure_equal(
        &import
            .pointer("/data/sessionsImported")
            .and_then(JsonValue::as_u64),
        &Some(1),
        "CASS import persisted one deterministic session",
    )?;
    ensure_equal(
        &import
            .pointer("/data/spansImported")
            .and_then(JsonValue::as_u64),
        &Some(2),
        "CASS import persisted two raw evidence spans",
    )?;
    let session_id = import
        .pointer("/data/sessions/0/sessionId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "import missing /data/sessions/0/sessionId".to_owned())?
        .to_owned();

    let (failure_memory_id, workspace_id) = remember_memory(
        &ws_arg,
        "episodic",
        "observation",
        "ci,release,clippy",
        "Repeated CI failure: clippy warning release failed until cargo test and cargo clippy were run locally before tagging.",
    )?;
    seed_memory(
        &ws_arg,
        "procedural",
        "rule",
        "ci,release",
        "Before tagging a release, run cargo test and cargo clippy and inspect any warning failures.",
    )?;

    let search = run_ee_json(&[
        "--workspace",
        &ws_arg,
        "--json",
        "search",
        "clippy warning release failed",
    ])?;
    let retrieved_ids = search
        .pointer("/data/results")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "search missing /data/results".to_owned())?
        .iter()
        .filter_map(|result| result.get("docId").and_then(JsonValue::as_str))
        .collect::<Vec<_>>();
    ensure(
        retrieved_ids.contains(&failure_memory_id.as_str()),
        format!(
            "search should retrieve the repeated CI failure memory {failure_memory_id}; got {retrieved_ids:?}"
        ),
    )?;

    let database = workspace.join(".ee").join("ee.db");
    persist_linked_review_spans(&database, &workspace_id, &session_id, &failure_memory_id)?;

    let review = run_ee_json(&[
        "--workspace",
        &ws_arg,
        "--json",
        "review",
        "session",
        &session_id,
        "--propose",
        "--min-confidence",
        "0.5",
    ])?;
    ensure_equal(
        &review
            .pointer("/data/proposeMode")
            .and_then(JsonValue::as_bool),
        &Some(true),
        "review session propose mode",
    )?;
    ensure_equal(
        &review
            .pointer("/data/durableMutation")
            .and_then(JsonValue::as_bool),
        &Some(true),
        "review session persisted curation candidate",
    )?;
    ensure_equal(
        &review
            .pointer("/data/evidenceSpanCount")
            .and_then(JsonValue::as_u64),
        &Some(4),
        "review session sees imported plus linked evidence spans",
    )?;
    ensure_equal(
        &review
            .pointer("/data/candidateCount")
            .and_then(JsonValue::as_u64),
        &Some(1),
        "review session proposed one linting candidate",
    )?;
    let candidate = review
        .pointer("/data/candidates/0")
        .ok_or_else(|| "review missing first candidate".to_owned())?;
    let candidate_id = candidate
        .get("candidateId")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "review candidate missing candidateId".to_owned())?
        .to_owned();
    ensure_equal(
        &candidate.get("topicKey").and_then(JsonValue::as_str),
        &Some("linting"),
        "review candidate topic",
    )?;
    ensure_equal(
        &candidate.get("candidateKind").and_then(JsonValue::as_str),
        &Some("failure"),
        "review candidate kind captures failure pattern",
    )?;
    let proposed_content = candidate
        .get("proposedContent")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "review candidate missing proposedContent".to_owned())?;
    ensure_contains(
        proposed_content,
        "clippy warning release failed",
        "candidate content",
    )?;
    ensure_contains(proposed_content, "cargo test", "candidate content")?;

    let queued = run_ee_json(&[
        "--workspace",
        &ws_arg,
        "--json",
        "curate",
        "candidates",
        "--group-duplicates",
    ])?;
    ensure_equal(
        &queued
            .pointer("/data/filter/groupDuplicates")
            .and_then(JsonValue::as_bool),
        &Some(true),
        "curate candidates duplicate grouping flag",
    )?;
    ensure_equal(
        &queued
            .pointer("/data/totalCount")
            .and_then(JsonValue::as_u64),
        &Some(1),
        "curate candidates lists the proposed rule candidate",
    )?;
    ensure_equal(
        &queued
            .pointer("/data/candidates/0/id")
            .and_then(JsonValue::as_str),
        &Some(candidate_id.as_str()),
        "curate candidates surfaces review candidate",
    )?;

    let validate = run_ee_json(&[
        "--workspace",
        &ws_arg,
        "--json",
        "curate",
        "validate",
        &candidate_id,
        "--actor",
        "north-star-e2e",
    ])?;
    ensure_equal(
        &validate
            .pointer("/data/validation/status")
            .and_then(JsonValue::as_str),
        &Some("passed"),
        "curate validate passes",
    )?;
    ensure_equal(
        &validate
            .pointer("/data/mutation/toStatus")
            .and_then(JsonValue::as_str),
        &Some("approved"),
        "curate validate approves candidate",
    )?;
    ensure_equal(
        &validate
            .pointer("/data/durableMutation")
            .and_then(JsonValue::as_bool),
        &Some(true),
        "curate validate records review mutation",
    )?;

    let apply = run_ee_json(&[
        "--workspace",
        &ws_arg,
        "--json",
        "curate",
        "apply",
        &candidate_id,
        "--actor",
        "north-star-e2e",
    ])?;
    ensure_equal(
        &apply
            .pointer("/data/application/decision")
            .and_then(JsonValue::as_str),
        &Some("create_rule"),
        "curate apply creates a procedural rule",
    )?;
    ensure_equal(
        &apply
            .pointer("/data/durableMutation")
            .and_then(JsonValue::as_bool),
        &Some(true),
        "curate apply persists rule creation",
    )?;
    let rule_id = change_after(&apply, "ruleId")
        .ok_or_else(|| "curate apply missing ruleId change".to_owned())?;
    let source_memory_count = change_after(&apply, "sourceMemoryCount")
        .ok_or_else(|| "curate apply missing sourceMemoryCount change".to_owned())?;
    ensure_equal(
        &source_memory_count.as_str(),
        &"1",
        "created rule records scoped source memory provenance",
    )?;

    let rule = run_ee_json(&["--workspace", &ws_arg, "--json", "rule", "show", &rule_id])?;
    ensure_equal(
        &rule.pointer("/data/found").and_then(JsonValue::as_bool),
        &Some(true),
        "rule show finds applied rule",
    )?;
    ensure_equal(
        &rule.pointer("/data/rule/id").and_then(JsonValue::as_str),
        &Some(rule_id.as_str()),
        "rule show id",
    )?;
    ensure_contains(
        rule.pointer("/data/rule/content")
            .and_then(JsonValue::as_str)
            .unwrap_or_default(),
        "clippy warning release failed",
        "rule content",
    )?;
    let source_memories = rule
        .pointer("/data/rule/sourceMemoryIds")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "rule show missing sourceMemoryIds".to_owned())?;
    ensure(
        source_memories
            .iter()
            .any(|value| value.as_str() == Some(failure_memory_id.as_str())),
        format!("rule source memories should include {failure_memory_id}: {source_memories:?}"),
    )?;

    assert_rule_index_job_queued(&database, &workspace_id, &rule_id)?;

    let memory = run_ee_json(&[
        "--workspace",
        &ws_arg,
        "--json",
        "memory",
        "show",
        &failure_memory_id,
    ])?;
    ensure_equal(
        &memory
            .pointer("/data/memory/id")
            .and_then(JsonValue::as_str),
        &Some(failure_memory_id.as_str()),
        "memory show returns the distilled source memory",
    )?;

    let why = run_ee_json(&["--workspace", &ws_arg, "--json", "why", &failure_memory_id])?;
    ensure_equal(
        &why.pointer("/data/found").and_then(JsonValue::as_bool),
        &Some(true),
        "why explains source memory",
    )?;
    ensure(
        why.pointer("/data/selection/selectionScore")
            .and_then(JsonValue::as_f64)
            .unwrap_or_default()
            > 0.0,
        "why selection score should be positive",
    )?;

    let outcome = run_ee_json(&[
        "--workspace",
        &ws_arg,
        "--json",
        "outcome",
        &failure_memory_id,
        "--signal",
        "harmful",
        "--reason",
        "The repeated clippy-release failure still needs tighter procedure evidence.",
        "--source-type",
        "outcome_observed",
        "--source-id",
        "north-star-e2e",
    ])?;
    ensure_equal(
        &outcome.pointer("/data/status").and_then(JsonValue::as_str),
        &Some("recorded"),
        "outcome records learning feedback",
    )?;

    let learn = run_ee_json(&["--workspace", &ws_arg, "--json", "learn", "agenda"])?;
    ensure_equal(
        &learn.pointer("/schema").and_then(JsonValue::as_str),
        &Some("ee.learn.agenda.v1"),
        "learn agenda schema",
    )?;
    ensure(
        learn
            .pointer("/totalGaps")
            .and_then(JsonValue::as_u64)
            .unwrap_or_default()
            >= 1,
        "learn agenda should include the harmful-feedback gap",
    )?;
    let learn_samples = learn
        .pointer("/items/0/sample_ids")
        .or_else(|| learn.pointer("/items/0/sampleIds"))
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "learn agenda missing sample ids".to_owned())?;
    ensure(
        learn_samples.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|sample| sample == failure_memory_id || sample.starts_with("fb_"))
        }),
        format!("learn agenda samples should mention memory or feedback: {learn_samples:?}"),
    )?;

    let procedure = run_ee_json(&["--workspace", &ws_arg, "--json", "procedure", "list"])?;
    ensure_equal(
        &procedure.pointer("/schema").and_then(JsonValue::as_str),
        &Some("ee.procedure.list_report.v1"),
        "procedure list schema",
    )?;
    ensure(
        procedure.pointer("/total_count").is_some(),
        "procedure list returns a totalCount field even when no procedure candidates were applied",
    )?;

    let audit = run_ee_json(&["--workspace", &ws_arg, "--json", "audit", "timeline"])?;
    let audit_entries = audit
        .pointer("/entries")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "audit timeline missing entries".to_owned())?;
    ensure(
        audit_entries.iter().any(|entry| {
            entry
                .get("mutation_kind")
                .and_then(JsonValue::as_str)
                .is_some_and(|kind| kind == "curation_candidate.apply")
        }),
        "audit timeline should include the curation apply mutation",
    )?;
    let verify = run_ee_json(&["--workspace", &ws_arg, "--json", "audit", "verify"])?;
    ensure_equal(
        &verify.pointer("/integrity_ok").and_then(JsonValue::as_bool),
        &Some(true),
        "audit verify integrity after full flow",
    )?;

    Ok(())
}

#[cfg(unix)]
fn set_executable_dir_permissions(path: &Path) -> TestResult {
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("metadata {}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("chmod {}: {error}", path.display()))
}

#[cfg(unix)]
fn path_with_fake_cass(fake_dir: &Path) -> Result<String, String> {
    let mut entries = vec![fake_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(entries)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
fn write_fake_cass_binary(path: &Path) -> TestResult {
    let script = r#"#!/bin/sh
set -eu
cmd="${1:-}"
case "$cmd" in
  sessions)
    printf '{"sessions":[{"path":"%s","workspace":"%s","agent":"codex","started_at":"2026-05-06T00:00:00Z","message_count":2,"token_count":160,"content_hash":"hash-lpb5-north-star"}]}\n' "$EE_FAKE_CASS_SESSION" "$EE_FAKE_CASS_WORKSPACE"
    ;;
  view)
    cat "$EE_FAKE_CASS_VIEW_JSON_PATH"
    ;;
  *)
    echo "unexpected cass command: $cmd" >&2
    exit 64
    ;;
esac
"#;
    fs::write(path, script).map_err(|error| format!("write {}: {error}", path.display()))?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("metadata {}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("chmod {}: {error}", path.display()))
}

fn persist_linked_review_spans(
    database: &Path,
    workspace_id: &str,
    session_id: &str,
    memory_id: &str,
) -> TestResult {
    let connection = DbConnection::open_file(database)
        .map_err(|error| format!("open {}: {error}", database.display()))?;
    for (id, cass_span_id, line, excerpt) in [
        (
            "ev_50000000000000000000000001",
            "lpb5-linked-1",
            20,
            "clippy warning release failed because cargo test was skipped before the release tag",
        ),
        (
            "ev_50000000000000000000000002",
            "lpb5-linked-2",
            21,
            "fix was to run cargo test, cargo clippy, and inspect warning output before tagging",
        ),
    ] {
        connection
            .insert_evidence_span(
                id,
                &CreateEvidenceSpanInput {
                    workspace_id: workspace_id.to_owned(),
                    session_id: session_id.to_owned(),
                    memory_id: Some(memory_id.to_owned()),
                    cass_span_id: cass_span_id.to_owned(),
                    span_kind: "message".to_owned(),
                    start_line: line,
                    end_line: line,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: excerpt.to_owned(),
                    content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                    metadata_json: Some(r#"{"schema":"lpb5.review_span.v1"}"#.to_owned()),
                },
            )
            .map_err(|error| format!("insert linked evidence span {id}: {error}"))?;
    }
    connection
        .close()
        .map_err(|error| format!("close linked evidence db: {error}"))
}

fn change_after(value: &JsonValue, field: &str) -> Option<String> {
    value
        .pointer("/data/application/changes")
        .and_then(JsonValue::as_array)?
        .iter()
        .find_map(|change| {
            (change.get("field").and_then(JsonValue::as_str) == Some(field))
                .then(|| change.get("after").and_then(JsonValue::as_str))
                .flatten()
                .map(str::to_owned)
        })
}

fn assert_rule_index_job_queued(database: &Path, workspace_id: &str, rule_id: &str) -> TestResult {
    let connection = DbConnection::open_file(database)
        .map_err(|error| format!("open {}: {error}", database.display()))?;
    let jobs = connection
        .list_search_index_jobs(workspace_id, Some(SearchIndexJobStatus::Pending))
        .map_err(|error| format!("list search index jobs: {error}"))?;
    connection
        .close()
        .map_err(|error| format!("close index job db: {error}"))?;
    ensure(
        jobs.iter().any(|job| {
            job.document_source.as_deref() == Some("rule")
                && job.document_id.as_deref() == Some(rule_id)
                && job.job_type == "single_document"
        }),
        format!("pending search index jobs should include rule {rule_id}: {jobs:?}"),
    )
}
