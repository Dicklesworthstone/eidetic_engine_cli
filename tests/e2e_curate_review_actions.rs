//! bd-cilvv: real-binary pin test for `ee curate accept`/`reject`/
//! `snooze`/`apply` unique error paths.
//!
//! Three distinct validators across the review-action family had no
//! end-to-end coverage. They join the broader curate-family pin
//! tests (bd-29acc candidates, bd-1gka6 validate, bd-kxm0c
//! propose-derived) so the whole queue surface is locked:
//!
//! * `parse_snoozed_until` is unique to snooze. Pinned shape:
//!   `--until garbage_timestamp` -> Usage `"invalid --until
//!   timestamp"` + repair `"ee curate snooze <candidate-id>
//!   --until <RFC3339> --json"`
//! * `validate_curate_review_reason` is shared across review
//!   actions but its 4 KiB cap had no end-to-end check.
//!   Pinned shape: `accept <id> --reason <5KB-text>` -> Usage
//!   `"curate review --reason must be <= 4096 bytes"` + repair
//!   `"Store long rationale in an external note and pass a short
//!   reason pointer."`
//! * `validate_curate_candidate_id` is reused by apply but bd-1gka6
//!   only pinned it via validate. Pinned shape: `apply bad-id` ->
//!   Usage `"invalid curation candidate ID"` + repair
//!   `"ee curate candidates --json"`
//! * NotFound on a valid-format but missing candidate via reject.
//!   Pinned shape: `reject <valid-format-but-missing>` -> NotFound
//!   `"curation candidate"` + `"ee curate candidates --json"`
//! * NotFound on a valid-format but missing candidate via retire
//!   dry-run. Pinned shape: `retire <valid-format-but-missing>
//!   --dry-run` -> NotFound before any retirement plan is reported.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

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
        .join("ee-curate-review-actions-pin")
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

fn run_curate(workspace_arg: &str, action_args: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "curate"];
    args.extend_from_slice(action_args);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("curate stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_error_with_repair(
    parsed: &Value,
    message_needles: &[&str],
    repair_needles: &[&str],
) -> TestResult {
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    for needle in message_needles {
        ensure(
            message.contains(needle),
            format!("message must contain {needle:?}; got {message}"),
        )?;
    }
    let repair = error["repair"].as_str().unwrap_or_default();
    for needle in repair_needles {
        ensure(
            repair.contains(needle),
            format!("repair must contain {needle:?}; got {repair}"),
        )?;
    }
    Ok(())
}

#[test]
fn curate_snooze_rejects_invalid_until_timestamp_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("snooze-bad-until")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    // Valid candidate id format so we reach parse_snoozed_until
    // before any DB lookup. The actual candidate doesn't need to
    // exist — the snoozed_until parser runs first.
    let valid_id = "curate_00000000000000000000000123";
    let (output, parsed) = run_curate(
        &workspace_arg,
        &["snooze", valid_id, "--until", "garbage_timestamp"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate snooze --until garbage_timestamp must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["invalid --until timestamp"],
        &["ee curate snooze", "<RFC3339>"],
    )
}

#[test]
fn curate_accept_rejects_reason_over_4kib_with_size_repair() -> TestResult {
    let workspace = unique_workspace("accept-reason-too-long")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    // validate_curate_review_reason enforces a 4 KiB cap. Generate
    // a 5 KiB rationale to trip the guard. The candidate id is in
    // valid format so the reason validator runs before the lookup.
    let valid_id = "curate_00000000000000000000000124";
    let big_reason = "x".repeat(5 * 1024);
    let (output, parsed) = run_curate(
        &workspace_arg,
        &["accept", valid_id, "--reason", big_reason.as_str()],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate accept --reason <5KB> must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["curate review --reason must be <= 4096 bytes"],
        &["Store long rationale in an external note"],
    )
}

#[test]
fn curate_apply_rejects_invalid_candidate_id_format_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("apply-bad-id")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_curate(&workspace_arg, &["apply", "bad-id"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate apply bad-id must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["invalid curation candidate ID", "bad-id"],
        &["ee curate candidates --json"],
    )
}

#[test]
fn curate_reject_returns_not_found_for_valid_format_but_missing_candidate() -> TestResult {
    let workspace = unique_workspace("reject-not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let valid_id = "curate_00000000000000000000000077";
    let (output, parsed) = run_curate(&workspace_arg, &["reject", valid_id])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate reject on unknown candidate must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    // `DomainError::NotFound` renders its resource and id under `error.details`
    // (src/output/mod.rs:15542-15545), not at the top level, so the previous
    // `error["id"]` read was always empty and the check rested entirely on a
    // prose needle in `message`.
    //
    // `id` is the field that answers "WHICH candidate could not be found", and
    // it is the machine-stable one. `resource` is human prose: all seven
    // construction sites spell it "curation candidate" with a space
    // (src/core/curate.rs:5266 and six others). The `curation_candidate` code
    // is a real vocabulary, but it belongs to `targetType`/`DocumentSource`,
    // not to `resource` -- asserting it here pinned a spelling that is not
    // emitted on this path, which is why this check failed while its own
    // message printed the value it demanded. Require the resource to be named
    // without pinning how it is worded.
    let error_resource = error["details"]["resource"].as_str().unwrap_or_default();
    let error_id = error["details"]["id"].as_str().unwrap_or_default();
    ensure(
        error_id == valid_id && !error_resource.is_empty(),
        format!(
            "not-found error must name the curation candidate it could not find; got message={message}, resource={error_resource}, id={error_id}"
        ),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee curate candidates --json"),
        format!("not-found repair must point at `ee curate candidates --json`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn curate_retire_dry_run_returns_not_found_for_valid_format_but_missing_candidate() -> TestResult {
    let workspace = unique_workspace("retire-dry-run-not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let valid_id = "curate_00000000000000000000000999";
    let (output, parsed) = run_curate(&workspace_arg, &["retire", valid_id, "--dry-run"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate retire --dry-run on unknown candidate must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let code = error["code"].as_str().unwrap_or_default();
    ensure(
        code == "not_found",
        format!("retire dry-run must return not_found for missing candidate; got {code}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    // `DomainError::NotFound` renders its resource and id under `error.details`
    // (src/output/mod.rs:15542-15545), not at the top level, so the previous
    // `error["id"]` read was always empty and the check rested entirely on a
    // prose needle in `message`.
    //
    // `id` is the field that answers "WHICH candidate could not be found", and
    // it is the machine-stable one. `resource` is human prose: all seven
    // construction sites spell it "curation candidate" with a space
    // (src/core/curate.rs:5266 and six others). The `curation_candidate` code
    // is a real vocabulary, but it belongs to `targetType`/`DocumentSource`,
    // not to `resource` -- asserting it here pinned a spelling that is not
    // emitted on this path, which is why this check failed while its own
    // message printed the value it demanded. Require the resource to be named
    // without pinning how it is worded.
    let error_resource = error["details"]["resource"].as_str().unwrap_or_default();
    let error_id = error["details"]["id"].as_str().unwrap_or_default();
    ensure(
        error_id == valid_id && !error_resource.is_empty(),
        format!(
            "not-found error must name the curation candidate it could not find; got message={message}, resource={error_resource}, id={error_id}"
        ),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee curate candidates --json"),
        format!("not-found repair must point at `ee curate candidates --json`; got {repair}"),
    )?;
    Ok(())
}

struct ConflictProposalFixture {
    workspace: PathBuf,
    memory_a: String,
    memory_b: String,
}

impl ConflictProposalFixture {
    fn new(name: &str) -> Result<Self, String> {
        let workspace = unique_workspace(name)?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let mut fixture = Self {
            workspace,
            memory_a: String::new(),
            memory_b: String::new(),
        };
        fixture.success(&["init"])?;
        for (first, content) in [
            (true, "Production deployments require serial execution."),
            (
                false,
                "Development deployments permit concurrent execution.",
            ),
        ] {
            let response = fixture.success(&[
                "remember",
                content,
                "--level",
                "semantic",
                "--kind",
                "fact",
                "--no-auto-link",
                "--no-propose-candidates",
            ])?;
            let id = response["data"]["memoryId"]
                .as_str()
                .ok_or_else(|| format!("remember must return memoryId: {response}"))?
                .to_owned();
            if first {
                fixture.memory_a = id;
            } else {
                fixture.memory_b = id;
            }
        }
        fixture.success(&[
            "memory",
            "link",
            &fixture.memory_a,
            &fixture.memory_b,
            "--relation",
            "contradicts",
        ])?;
        Ok(fixture)
    }

    fn run(&self, args: &[&str]) -> Result<(Output, Value), String> {
        let output =
            crate::isolated_ee::isolated_ee_command(&self.workspace.join("isolated-runtime"))?
                .arg("--workspace")
                .arg(&self.workspace)
                .arg("--json")
                .args(args)
                .output()
                .map_err(|error| format!("run ee {args:?}: {error}"))?;
        let parsed = serde_json::from_slice(&output.stdout).map_err(|error| {
            format!(
                "ee {args:?} returned invalid JSON: {error}; status={:?}; stdout={}; stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            )
        })?;
        Ok((output, parsed))
    }

    fn success(&self, args: &[&str]) -> Result<Value, String> {
        let (output, parsed) = self.run(args)?;
        ensure(
            output.status.success() && parsed["success"] == true,
            format!(
                "ee {args:?} must succeed; status={:?}; stdout={parsed}; stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr),
            ),
        )?;
        Ok(parsed)
    }

    fn database(&self) -> Result<ee::db::DbConnection, String> {
        ee::db::DbConnection::open_file_read_only(self.workspace.join(".ee/ee.db"))
            .map_err(|error| error.to_string())
    }

    fn propose(&self, verb: &str, extra: &[&str]) -> Result<Value, String> {
        let mut args = vec![
            "conflict",
            "resolve",
            self.memory_a.as_str(),
            self.memory_b.as_str(),
            "--verb",
            verb,
            "--propose",
            "--reason",
            "Each deployment statement has an explicit environment context.",
        ];
        args.extend_from_slice(extra);
        self.success(&args)
    }
}

#[test]
fn queued_conflict_scope_split_preserves_both_memories_until_reviewed_apply() -> TestResult {
    let fixture = ConflictProposalFixture::new("queued-conflict-scope-split")?;
    let before = {
        let db = fixture.database()?;
        [&fixture.memory_a, &fixture.memory_b]
            .into_iter()
            .map(|id| {
                db.get_memory(id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("missing remembered source {id}"))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let extra = [
        "--scope-a-tags",
        "production",
        "--scope-b-tags",
        "development",
    ];
    let proposed = fixture.propose("scope-split", &extra)?;
    ensure(
        proposed["data"]["status"] == "proposed"
            && proposed["data"]["persisted"] == true
            && proposed["data"]["dryRun"] == false,
        format!("proposal must report a persisted queue entry: {proposed}"),
    )?;
    let candidate_id = proposed["data"]["candidateId"]
        .as_str()
        .ok_or_else(|| format!("proposal must return its candidateId: {proposed}"))?;
    let replay = fixture.propose("scope-split", &extra)?;
    ensure(
        replay["data"]["candidateId"].as_str() == Some(candidate_id),
        format!("repeating one proposal must reuse its candidate: {replay}"),
    )?;
    let (pending_output, pending_apply) = fixture.run(&["curate", "apply", candidate_id])?;
    ensure(
        pending_output.status.success(),
        "blocked pending apply must return its structured report",
    )?;
    assert_blocked_conflict_apply(&pending_apply, "candidate_not_approved")?;
    {
        let db = fixture.database()?;
        for original in &before {
            let current = db
                .get_memory(&original.id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("proposal lost source {}", original.id))?;
            ensure(
                current.content == original.content && current.tombstoned_at.is_none(),
                "proposing a scope split must preserve both original memory bodies",
            )?;
            ensure(
                db.get_memory_tags(&original.id)
                    .map_err(|error| error.to_string())?
                    .is_empty(),
                "scope tags must not be applied before review",
            )?;
        }
        let candidate = db
            .get_curation_candidate(&before[0].workspace_id, candidate_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "proposal was not durably queued".to_owned())?;
        ensure(
            candidate.status == "pending",
            "new proposal must be pending",
        )?;
    }
    let shown = fixture.success(&["curate", "show", candidate_id])?;
    let receipt_text = shown["data"]["candidate"]["proposedContent"]
        .as_str()
        .ok_or_else(|| format!("show must preserve the complete review receipt: {shown}"))?;
    let receipt: Value = serde_json::from_str(receipt_text)
        .map_err(|error| format!("review receipt must be structured JSON: {error}"))?;
    ensure(
        receipt["plan"] == proposed["data"]["plan"],
        format!("show must expose the exact resolution actions proposed for review: {shown}"),
    )?;
    fixture.success(&["curate", "validate", candidate_id, "--dry-run"])?;
    fixture.success(&["curate", "accept", candidate_id])?;
    let applied = fixture.success(&["curate", "apply", candidate_id])?;
    ensure(
        applied["data"]["mutation"]["persisted"] == true
            && applied["data"]["mutation"]["toStatus"] == "applied",
        format!("approved scope split must complete its queue entry: {applied}"),
    )?;
    let db = fixture.database()?;
    for (original, scope, other_scope) in [
        (&before[0], "production", "development"),
        (&before[1], "development", "production"),
    ] {
        let current = db
            .get_memory(&original.id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("apply lost source {}", original.id))?;
        let tags = db
            .get_memory_tags(&original.id)
            .map_err(|error| error.to_string())?;
        ensure(
            current.content == original.content
                && current.tombstoned_at.is_none()
                && tags.iter().any(|tag| tag == scope)
                && !tags.iter().any(|tag| tag == other_scope),
            format!(
                "scope split must preserve and correctly scope {}: {tags:?}",
                original.id
            ),
        )?;
    }
    let candidate = db
        .get_curation_candidate(&before[0].workspace_id, candidate_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "applied candidate disappeared".to_owned())?;
    ensure(
        candidate.status == "applied",
        "candidate must remain applied",
    )
}

fn assert_blocked_conflict_apply(response: &Value, code: &str) -> TestResult {
    ensure(
        response["success"] == false
            && response["data"]["application"]["status"] == "blocked"
            && response["data"]["mutation"]["persisted"] == false
            && response["data"]["durableMutation"] == false
            && response["data"]["application"]["errors"]
                .as_array()
                .is_some_and(|errors| errors.iter().any(|issue| issue["code"] == code)),
        format!("queued conflict apply must refuse with {code}: {response}"),
    )
}

#[test]
fn queued_conflict_apply_refuses_resolution_completed_after_approval() -> TestResult {
    let fixture = ConflictProposalFixture::new("queued-conflict-stale")?;
    let proposed = fixture.propose("supersede", &["--keep", &fixture.memory_a])?;
    let candidate_id = proposed["data"]["candidateId"]
        .as_str()
        .ok_or_else(|| format!("proposal must return its candidateId: {proposed}"))?;
    fixture.success(&["curate", "accept", candidate_id])?;
    fixture.success(&[
        "conflict",
        "resolve",
        &fixture.memory_a,
        &fixture.memory_b,
        "--verb",
        "both-valid",
        "--apply",
    ])?;
    let before = {
        let db = fixture.database()?;
        (
            db.count_table_rows("memories")
                .map_err(|error| error.to_string())?,
            db.count_table_rows("audit_log")
                .map_err(|error| error.to_string())?,
        )
    };
    let (output, response) = fixture.run(&["curate", "apply", candidate_id])?;
    ensure(
        output.status.success(),
        "blocked stale apply must return its structured report",
    )?;
    assert_blocked_conflict_apply(&response, "conflict_proposal_stale")?;
    let db = fixture.database()?;
    ensure(
        db.count_table_rows("memories")
            .map_err(|error| error.to_string())?
            == before.0
            && db
                .count_table_rows("audit_log")
                .map_err(|error| error.to_string())?
                == before.1,
        "a stale proposal must not create another decision or audit a partial resolution",
    )?;
    for id in [&fixture.memory_a, &fixture.memory_b] {
        let memory = db
            .get_memory(id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("stale application lost source {id}"))?;
        ensure(
            memory.tombstoned_at.is_none(),
            "the stale supersede proposal must not override the later both-valid resolution",
        )?;
    }
    Ok(())
}
