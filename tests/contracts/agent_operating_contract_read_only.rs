//! AOP5 contract coverage for the read-only agent operating contract builder.
//!
//! The public CLI surface for this report is not wired yet, so this test keeps
//! the no-mock invariant at the core boundary: a real retained Git workspace,
//! real AGENTS.md/README.md files, Beads metadata, and fixture readiness
//! evidence go through `extract_agent_operating_contract`. The extractor must
//! produce byte-stable JSON and leave the workspace unchanged.

use ee::core::preflight::{
    AGENT_OPERATING_CONTRACT_SCHEMA_V1, AgentOperatingContractOptions,
    AgentOperatingContractReadinessEvidence, AgentOperatingContractReadinessMetric,
    AgentOperatingContractReport, AgentReadinessEvidenceInput, AgentReadinessSourceInput,
    AgentReadinessStatus, extract_agent_operating_contract,
};
use ee::output::ResponseEnvelope;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

#[derive(Debug)]
struct WorkspaceState {
    status: String,
    files: Vec<(String, String)>,
    hash: String,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq + ?Sized,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn retained_artifact_root(test_id: &str) -> Result<PathBuf, String> {
    let target_root = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let root = target_root
        .join("aop5-agent-contract")
        .join(format!("{test_id}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&root).map_err(|error| format!("create {}: {error}", root.display()))?;
    Ok(root)
}

fn write_file(path: &Path, content: &str) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create parent {}: {error}", parent.display()))?;
    }
    fs::write(path, content).map_err(|error| format!("write {}: {error}", path.display()))
}

fn append_file(path: &Path, content: &str) -> TestResult {
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| format!("open append {}: {error}", path.display()))?;
    file.write_all(content.as_bytes())
        .map_err(|error| format!("append {}: {error}", path.display()))
}

fn run_git(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .map_err(|error| format!("spawn git {args:?}: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed with status {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        ));
    }
    Ok(stdout)
}

fn init_git_workspace(workspace: &Path) -> TestResult {
    fs::create_dir_all(workspace)
        .map_err(|error| format!("create workspace {}: {error}", workspace.display()))?;
    run_git(workspace, &["init", "-q", "--initial-branch=main"])?;
    run_git(
        workspace,
        &["config", "user.email", "aop5-fixture@example.invalid"],
    )?;
    run_git(workspace, &["config", "user.name", "AOP5 Fixture"])?;
    Ok(())
}

fn commit_baseline(workspace: &Path) -> TestResult {
    run_git(
        workspace,
        &[
            "add",
            "AGENTS.md",
            "README.md",
            ".beads/issues.jsonl",
            "coordination/agent_mail_archive/file_reservations.jsonl",
            "coordination/agent_mail_archive/messages.jsonl",
            "coordination/rch_state/status.json",
            "src/lib.rs",
        ],
    )?;
    run_git(workspace, &["commit", "-q", "-m", "fixture baseline"])?;
    Ok(())
}

fn write_full_fixture_workspace(workspace: &Path) -> TestResult {
    init_git_workspace(workspace)?;
    write_file(
        &workspace.join("AGENTS.md"),
        r#"# Agent Rules

## RULE NUMBER 1: NO FILE DELETION

YOU ARE NEVER ALLOWED TO DELETE A FILE WITHOUT EXPRESS PERMISSION.

## RULE NUMBER 2: NO WORKTREES. EVER. NO EXCEPTIONS.

Never run `git worktree add`.

## Git Branch: ONLY Use `main`, NEVER `master`

The default branch is `main`.

## Irreversible Git & Filesystem Actions

Absolutely forbidden commands include `git reset --hard`.
Never run `git stash`.
Never run `git checkout <other-ref>`.

## Compiler Checks

All cargo builds and tests and other CPU intensive operations MUST be done using $rch.

## Local Dev Environment: External Build Drive

Preserve CARGO_TARGET_DIR on the external USB-NVMe drive at /Volumes/USBNVME16TB.
"#,
    )?;
    write_file(
        &workspace.join("README.md"),
        r#"# Eidetic Engine

## Hard Requirements

- Runtime is `/dp/asupersync`. **No Tokio.** Anywhere. Ever.
- Database is `/dp/frankensqlite` through `/dp/sqlmodel_rust`. **No `rusqlite`, no SQLx, no Diesel, no SeaORM.**
- Graph is `/dp/franken_networkx`. **No `petgraph`.**
- Every machine-facing command supports stable JSON output.
- Every generated context includes provenance and score explanation.
"#,
    )?;
    write_file(
        &workspace.join("src/lib.rs"),
        "pub fn fixture() -> &'static str { \"ok\" }\n",
    )?;
    write_file(
        &workspace.join(".beads/issues.jsonl"),
        "{\"id\":\"bd-aop5-fixture\",\"status\":\"open\",\"title\":\"baseline\"}\n",
    )?;
    write_file(
        &workspace.join("coordination/agent_mail_archive/messages.jsonl"),
        "{\"id\":1,\"thread\":\"br-bd-3d6ko.5\",\"subject\":\"AOP5 fixture evidence\"}\n",
    )?;
    write_file(
        &workspace.join("coordination/agent_mail_archive/file_reservations.jsonl"),
        "{\"path\":\"tests/contracts/agent_operating_contract_read_only.rs\",\"exclusive\":true}\n",
    )?;
    write_file(
        &workspace.join("coordination/rch_state/status.json"),
        "{\"remote_required\":true,\"workers_healthy\":5,\"slots_available\":28}\n",
    )?;
    commit_baseline(workspace)?;
    append_file(
        &workspace.join(".beads/issues.jsonl"),
        "{\"id\":\"bd-aop5-dirty\",\"status\":\"in_progress\",\"title\":\"dirty tracker fixture\"}\n",
    )?;
    Ok(())
}

fn write_missing_readme_workspace(workspace: &Path) -> TestResult {
    init_git_workspace(workspace)?;
    write_file(
        &workspace.join("AGENTS.md"),
        "# Agent Rules\n\nRULE NUMBER 2: NO WORKTREES. EVER.\n",
    )?;
    write_file(&workspace.join("src/lib.rs"), "pub fn fixture() {}\n")?;
    write_file(
        &workspace.join(".beads/issues.jsonl"),
        "{\"id\":\"bd-aop5-missing-readme\",\"status\":\"open\"}\n",
    )?;
    run_git(
        workspace,
        &["add", "AGENTS.md", ".beads/issues.jsonl", "src/lib.rs"],
    )?;
    run_git(
        workspace,
        &["commit", "-q", "-m", "missing readme baseline"],
    )?;
    Ok(())
}

fn file_state_digest(workspace: &Path) -> Result<Vec<(String, String)>, String> {
    fn visit(root: &Path, path: &Path, out: &mut Vec<(String, String)>) -> Result<(), String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("metadata {}: {error}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("strip {}: {error}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == ".git" || relative.starts_with(".git/") {
            return Ok(());
        }
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)
                .map_err(|error| format!("read_dir {}: {error}", path.display()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("read_dir entry {}: {error}", path.display()))?;
            children.sort_by_key(|entry| entry.path());
            for child in children {
                visit(root, &child.path(), out)?;
            }
            return Ok(());
        }
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(path)
                .map_err(|error| format!("read_link {}: {error}", path.display()))?;
            out.push((relative, format!("symlink:{}", target.to_string_lossy())));
            return Ok(());
        }
        if metadata.is_file() {
            let bytes =
                fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
            out.push((relative, format!("file:{}", blake3::hash(&bytes).to_hex())));
        }
        Ok(())
    }

    let mut out = Vec::new();
    visit(workspace, workspace, &mut out)?;
    out.sort();
    Ok(out)
}

fn mutation_hash(status: &str, files: &[(String, String)]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(status.as_bytes());
    for (path, state) in files {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(state.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

fn capture_workspace_state(workspace: &Path) -> Result<WorkspaceState, String> {
    let status = run_git(
        workspace,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
        ],
    )?;
    let files = file_state_digest(workspace)?;
    let hash = mutation_hash(&status, &files);
    Ok(WorkspaceState {
        status,
        files,
        hash,
    })
}

fn readiness_source(
    status: AgentReadinessStatus,
    summary: &str,
    evidence_refs: &[&str],
    metrics: &[(&str, &str)],
) -> AgentReadinessSourceInput {
    let mut source = AgentReadinessSourceInput::new(status, summary);
    source.evidence_refs = evidence_refs
        .iter()
        .map(|value| (*value).to_owned())
        .collect();
    source.metrics = metrics
        .iter()
        .map(|(name, value)| AgentOperatingContractReadinessMetric {
            name: (*name).to_owned(),
            value: (*value).to_owned(),
        })
        .collect();
    source
}

fn fixture_readiness() -> AgentReadinessEvidenceInput {
    AgentReadinessEvidenceInput {
        agent_mail: Some(readiness_source(
            AgentReadinessStatus::Ok,
            "Agent Mail fixture has no unread or ack-required messages.",
            &[
                "agent_mail:fixture:inbox-empty",
                "agent_mail:fixture:reservations-empty",
            ],
            &[
                ("ack_required_count", "0"),
                ("active_reservation_count", "0"),
            ],
        )),
        beads: Some(readiness_source(
            AgentReadinessStatus::Ok,
            "Beads fixture was collected from the retained temp workspace.",
            &["beads:fixture:issues-jsonl"],
            &[("ready_count", "1"), ("stale_count", "0")],
        )),
        bv: Some(readiness_source(
            AgentReadinessStatus::Ok,
            "BV fixture selected the AOP5 contract slice.",
            &["bv:fixture:top-pick"],
            &[("top_pick_count", "1")],
        )),
        tracker: Some(readiness_source(
            AgentReadinessStatus::Dirty,
            ".beads/issues.jsonl is intentionally dirty in the fixture.",
            &["git:fixture:dirty-beads"],
            &[("dirty_tracker_paths", "1")],
        )),
        rch: Some(readiness_source(
            AgentReadinessStatus::Blocked,
            "RCH fixture is remote-required but topology-blocked.",
            &["rch:fixture:workspace-inheritance-blocked"],
            &[("workers_healthy", "5"), ("slots_available", "28")],
        )),
    }
}

fn readiness_by_service(
    readiness: &[AgentOperatingContractReadinessEvidence],
) -> BTreeMap<&str, &AgentOperatingContractReadinessEvidence> {
    readiness
        .iter()
        .map(|entry| (entry.service.as_str(), entry))
        .collect()
}

fn blake3_prefixed(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn rendered_stdout_envelope(report: &AgentOperatingContractReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.to_json())
        .degraded_array(&report.degraded, |obj, degraded| {
            obj.field_str("code", degraded.code.as_str());
            obj.field_str("severity", degraded.severity.as_str());
            obj.field_str("message", degraded.message.as_str());
        })
        .finish()
}

fn assert_stdout_is_single_response_envelope(stdout: &str, stderr: &str) -> TestResult {
    ensure(
        stderr.is_empty(),
        format!("stderr should be empty, got {stderr:?}"),
    )?;
    ensure(
        stdout.ends_with('\n'),
        "stdout should terminate with exactly one newline",
    )?;
    ensure(
        stdout.lines().count() == 1,
        format!("stdout should contain one JSON envelope line, got {stdout:?}"),
    )?;
    let parsed: Value = serde_json::from_str(stdout.trim_end())
        .map_err(|error| format!("parse stdout: {error}"))?;
    ensure_equal(
        parsed
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "ee.response.v2",
        "stdout response envelope schema",
    )?;
    ensure_equal(
        &parsed.get("success").and_then(Value::as_bool),
        &Some(true),
        "stdout response envelope success",
    )?;
    ensure_equal(
        parsed
            .pointer("/data/schema")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        AGENT_OPERATING_CONTRACT_SCHEMA_V1,
        "stdout data schema",
    )?;
    ensure(
        parsed.get("degraded").and_then(Value::as_array).is_some(),
        "stdout envelope should include degraded array",
    )
}

fn assert_workspace_file_unchanged(
    before: &WorkspaceState,
    after: &WorkspaceState,
    path: &str,
) -> TestResult {
    let before_state = before
        .files
        .iter()
        .find(|(candidate, _)| candidate == path)
        .map(|(_, state)| state);
    let after_state = after
        .files
        .iter()
        .find(|(candidate, _)| candidate == path)
        .map(|(_, state)| state);
    ensure_equal(
        &before_state,
        &after_state,
        &format!("{path} mutation sentinel"),
    )
}

fn write_event_log(
    artifact_root: &Path,
    test_id: &str,
    started: Instant,
    output_json: &str,
    before: &WorkspaceState,
    after: &WorkspaceState,
    degraded_codes: &[String],
) -> Result<Value, String> {
    let event_log = artifact_root.join("events.jsonl");
    let output_hash = blake3_prefixed(output_json.as_bytes());
    let event = json!({
        "schema": "ee.test_event.v1",
        "ts": "2026-05-19T00:00:00Z",
        "test_id": test_id,
        "kind": "command_end",
        "command": "extract_agent_operating_contract",
        "args": ["core-api", "retained-temp-git-workspace"],
        "stdout_hash": output_hash,
        "exit_code": 0,
        "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
        "fields": {
            "workspace_path_hash": blake3_prefixed(artifact_root.join("workspace").display().to_string().as_bytes()),
            "output_artifact_hash": output_hash,
            "before_mutation_hash": before.hash.as_str(),
            "after_mutation_hash": after.hash.as_str(),
            "before_status_bytes": before.status.len().to_string(),
            "after_status_bytes": after.status.len().to_string(),
            "before_file_count": before.files.len().to_string(),
            "after_file_count": after.files.len().to_string(),
            "degraded_codes": degraded_codes,
            "first_failure_diagnosis": if before.hash == after.hash { Value::Null } else { json!("workspace mutated during agent operating contract extraction") }
        }
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&event_log)
        .map_err(|error| format!("open event log {}: {error}", event_log.display()))?;
    serde_json::to_writer(&mut file, &event)
        .map_err(|error| format!("write event JSON: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("write event newline: {error}"))?;
    Ok(event)
}

#[test]
fn agent_operating_contract_core_e2e_is_deterministic_and_read_only() -> TestResult {
    let artifact_root = retained_artifact_root("core-e2e-read-only")?;
    let workspace = artifact_root.join("workspace");
    write_full_fixture_workspace(&workspace)?;
    let before = capture_workspace_state(&workspace)?;

    let options = AgentOperatingContractOptions {
        workspace: workspace.clone(),
        readiness: fixture_readiness(),
    };

    let started = Instant::now();
    let first = extract_agent_operating_contract(&options).map_err(|error| error.message())?;
    let first_json = first.to_json();
    let first_stdout = format!("{}\n", rendered_stdout_envelope(&first));
    let second = extract_agent_operating_contract(&options).map_err(|error| error.message())?;
    let second_json = second.to_json();
    let second_stdout = format!("{}\n", rendered_stdout_envelope(&second));
    let after = capture_workspace_state(&workspace)?;

    ensure_equal(&first_json, &second_json, "byte-stable report JSON")?;
    ensure_equal(
        &first_stdout,
        &second_stdout,
        "byte-stable stdout envelope JSON",
    )?;
    assert_stdout_is_single_response_envelope(&first_stdout, "")?;
    ensure_equal(
        &before.hash,
        &after.hash,
        "workspace mutation hash must not change",
    )?;
    for sentinel in [
        ".beads/issues.jsonl",
        "coordination/agent_mail_archive/file_reservations.jsonl",
        "coordination/agent_mail_archive/messages.jsonl",
        "coordination/rch_state/status.json",
        "src/lib.rs",
    ] {
        assert_workspace_file_unchanged(&before, &after, sentinel)?;
    }
    ensure_equal(
        first.schema.as_str(),
        AGENT_OPERATING_CONTRACT_SCHEMA_V1,
        "agent contract schema",
    )?;
    ensure(
        first.degraded.is_empty(),
        format!("full fixture should not degrade: {:?}", first.degraded),
    )?;

    let rule_ids = first
        .rules
        .iter()
        .map(|rule| rule.id.as_str())
        .collect::<Vec<_>>();
    for required in [
        "agent.no_file_deletion",
        "agent.no_worktrees",
        "agent.no_git_reset_hard",
        "agent.no_git_stash",
        "agent.no_git_checkout_other_ref",
        "agent.main_branch_only",
        "agent.rch_remote_verification",
        "agent.external_build_drive",
        "agent.no_tokio_runtime",
        "agent.no_rusqlite_sqlx_diesel",
        "agent.no_petgraph",
        "agent.stable_json",
        "agent.context_provenance",
    ] {
        ensure(
            rule_ids.contains(&required),
            format!("missing extracted operating rule `{required}` in {rule_ids:?}"),
        )?;
    }

    let readiness = readiness_by_service(&first.readiness_evidence);
    ensure_equal(
        readiness
            .get("agent_mail")
            .ok_or_else(|| "missing agent_mail readiness".to_owned())?
            .status
            .as_str(),
        "ok",
        "agent mail readiness",
    )?;
    let tracker = readiness
        .get("tracker")
        .ok_or_else(|| "missing tracker readiness".to_owned())?;
    ensure_equal(tracker.status.as_str(), "dirty", "tracker readiness")?;
    ensure(
        tracker
            .degraded_codes
            .contains(&"workspace_hygiene_beads_db_divergence_unknown".to_owned()),
        "dirty tracker readiness should carry divergence degraded code",
    )?;
    let rch = readiness
        .get("rch")
        .ok_or_else(|| "missing rch readiness".to_owned())?;
    ensure_equal(rch.status.as_str(), "blocked", "rch readiness")?;
    ensure(
        rch.degraded_codes
            .contains(&"rch_worker_topology_blocked".to_owned()),
        "blocked RCH readiness should carry topology degraded code",
    )?;

    let event = write_event_log(
        &artifact_root,
        "agent_operating_contract_core_e2e_is_deterministic_and_read_only",
        started,
        &first_stdout,
        &before,
        &after,
        &Vec::new(),
    )?;
    ensure_equal(
        event["schema"].as_str().unwrap_or_default(),
        "ee.test_event.v1",
        "event schema",
    )?;
    ensure_equal(
        event["kind"].as_str().unwrap_or_default(),
        "command_end",
        "event kind",
    )?;
    ensure_equal(
        &event["stdout_hash"].as_str(),
        &event
            .pointer("/fields/output_artifact_hash")
            .and_then(Value::as_str),
        "event output artifact hash mirrors stdout hash",
    )
}

#[test]
fn agent_operating_contract_missing_readme_degrades_without_mutation() -> TestResult {
    let artifact_root = retained_artifact_root("missing-readme-read-only")?;
    let workspace = artifact_root.join("workspace");
    write_missing_readme_workspace(&workspace)?;
    let before = capture_workspace_state(&workspace)?;

    let started = Instant::now();
    let report = extract_agent_operating_contract(&AgentOperatingContractOptions {
        workspace: workspace.clone(),
        readiness: AgentReadinessEvidenceInput::default(),
    })
    .map_err(|error| error.message())?;
    let stdout = format!("{}\n", rendered_stdout_envelope(&report));
    let after = capture_workspace_state(&workspace)?;

    assert_stdout_is_single_response_envelope(&stdout, "")?;
    ensure_equal(
        &before.hash,
        &after.hash,
        "missing-docs extraction must still be read-only",
    )?;
    ensure(
        report
            .degraded
            .iter()
            .any(|entry| entry.code == "agent_contract_source_unavailable"),
        format!(
            "missing README should emit source-unavailable degradation: {:?}",
            report.degraded
        ),
    )?;
    let readiness = readiness_by_service(&report.readiness_evidence);
    let agent_mail = readiness
        .get("agent_mail")
        .ok_or_else(|| "missing default agent_mail readiness".to_owned())?;
    ensure_equal(
        agent_mail.status.as_str(),
        "not_collected",
        "absent Agent Mail readiness",
    )?;
    ensure(
        agent_mail.degraded_codes.is_empty(),
        "absent Agent Mail readiness should not invent degraded codes",
    )?;
    ensure(
        agent_mail.next_action.is_none(),
        "absent Agent Mail readiness should not invent repair action",
    )?;

    let degraded_codes = report
        .degraded
        .iter()
        .map(|entry| entry.code.clone())
        .collect::<Vec<_>>();
    let event = write_event_log(
        &artifact_root,
        "agent_operating_contract_missing_readme_degrades_without_mutation",
        started,
        &stdout,
        &before,
        &after,
        &degraded_codes,
    )?;
    ensure(
        event
            .pointer("/fields/degraded_codes")
            .and_then(Value::as_array)
            .is_some_and(|codes| {
                codes
                    .iter()
                    .any(|code| code == "agent_contract_source_unavailable")
            }),
        "event should record missing-docs degraded code",
    )
}
