//! eidetic_engine_cli-oskm: handoff CLI integration tests
//!
//! Exercises the real `ee` binary against `ee handoff create/inspect/resume`
//! produced by core/handoff.rs. NO MOCKS — real binary, real filesystem,
//! real workspace.
//!
//! The h0h1 closure landed unit tests inside src/core/handoff.rs but no
//! CLI-layer integration tests; this file fills that gap and documents the
//! determinism gap (capsule_id is UUID v7 and created_at is Utc::now, so
//! two CLI runs produce different content_hashes — the unit test for
//! compute_content_hash is over a fixed string and is not the same
//! contract).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;
const EXIT_STORAGE: i32 = 3;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn stdout_string(output: &Output) -> Result<String, String> {
    String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout was not UTF-8: {error}"))
}

fn stdout_json(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = stdout_string(output)?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn init_workspace() -> Result<(tempfile::TempDir, String), String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let ws = dir.path().to_string_lossy().to_string();
    let init = run_ee(&["--workspace", &ws, "init", "--json"])?;
    ensure(
        init.status.code() == Some(EXIT_SUCCESS),
        format!("init failed: {:?}", init.status.code()),
    )?;
    Ok((dir, ws))
}

fn capsule_path(workspace: &str, name: &str) -> String {
    PathBuf::from(workspace)
        .join(name)
        .to_string_lossy()
        .to_string()
}

#[test]
fn handoff_create_writes_real_capsule_file() -> TestResult {
    let (_dir, ws) = init_workspace()?;
    let cap = capsule_path(&ws, "cap.json");

    let output = run_ee(&[
        "--workspace",
        &ws,
        "handoff",
        "create",
        "--out",
        &cap,
        "--json",
    ])?;
    ensure(
        output.status.code() == Some(EXIT_SUCCESS),
        format!("handoff create failed: {:?}", output.status.code()),
    )?;

    let json = stdout_json(&output)?;
    ensure(
        json.get("schema").and_then(|v| v.as_str()) == Some("ee.handoff.create.v1"),
        "create stdout schema",
    )?;
    let capsule_id = json
        .get("capsule_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing capsule_id".to_string())?;
    ensure(
        capsule_id.starts_with("hcap_"),
        format!("capsule_id should start with hcap_, got {capsule_id}"),
    )?;
    let hash = json
        .get("content_hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing content_hash".to_string())?;
    ensure(
        hash.len() == 16,
        format!("content_hash should be 16 hex chars, got {hash}"),
    )?;
    ensure(
        json.pointer("/swarm_brief_summary/schema")
            .and_then(|v| v.as_str())
            == Some("ee.support_bundle.swarm_brief_summary.v1"),
        "create stdout includes swarm brief summary schema",
    )?;

    ensure(
        PathBuf::from(&cap).exists(),
        "capsule file must exist on disk",
    )?;
    let body = fs::read_to_string(&cap).map_err(|e| e.to_string())?;
    let capsule_json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("capsule file is not valid JSON: {e}"))?;
    ensure(
        capsule_json.get("schema").and_then(|v| v.as_str()) == Some("ee.handoff.capsule.v1"),
        "capsule file schema",
    )?;
    ensure(
        capsule_json.get("capsule_id").and_then(|v| v.as_str()) == Some(capsule_id),
        "capsule_id matches between stdout and file",
    )?;
    ensure(
        capsule_json
            .get("sections")
            .and_then(|v| v.as_array())
            .is_some_and(|sections| {
                sections.iter().any(|section| {
                    section.get("id").and_then(|v| v.as_str()) == Some("swarm_brief_summary")
                })
            }),
        "capsule sections include compact swarm brief summary",
    )?;
    let swarm_summary = capsule_json
        .get("swarm_brief_summary")
        .ok_or_else(|| "capsule missing swarm_brief_summary".to_string())?;
    ensure(
        swarm_summary.get("schema").and_then(|v| v.as_str())
            == Some("ee.support_bundle.swarm_brief_summary.v1"),
        "capsule swarm brief summary schema",
    )?;
    ensure(
        swarm_summary.pointer("/redaction/rawMailBodiesIncluded")
            == Some(&serde_json::json!(false))
            && swarm_summary.pointer("/redaction/rawQueryTextIncluded")
                == Some(&serde_json::json!(false))
            && swarm_summary.pointer("/redaction/fullFileListingsIncluded")
                == Some(&serde_json::json!(false)),
        "capsule swarm brief summary is redaction-safe",
    )?;
    ensure(
        swarm_summary
            .get("reportHash")
            .and_then(|v| v.as_str())
            .is_some_and(|hash| hash.starts_with("blake3:")),
        "capsule swarm brief summary includes report hash",
    )?;
    Ok(())
}

#[test]
fn handoff_create_dry_run_does_not_write_file() -> TestResult {
    let (_dir, ws) = init_workspace()?;
    let cap = capsule_path(&ws, "cap_dry.json");

    let output = run_ee(&[
        "--workspace",
        &ws,
        "handoff",
        "create",
        "--out",
        &cap,
        "--dry-run",
        "--json",
    ])?;
    ensure(
        output.status.code() == Some(EXIT_SUCCESS),
        "dry-run create should succeed",
    )?;
    let json = stdout_json(&output)?;
    ensure(
        json.get("dry_run").and_then(|v| v.as_bool()) == Some(true),
        "dry_run flag should be true in stdout",
    )?;
    ensure(
        !PathBuf::from(&cap).exists(),
        "dry-run must not write capsule file",
    )?;
    Ok(())
}

#[test]
fn handoff_inspect_validates_existing_capsule() -> TestResult {
    let (_dir, ws) = init_workspace()?;
    let cap = capsule_path(&ws, "cap.json");
    let create = run_ee(&[
        "--workspace",
        &ws,
        "handoff",
        "create",
        "--out",
        &cap,
        "--json",
    ])?;
    ensure(create.status.code() == Some(EXIT_SUCCESS), "create failed")?;

    let inspect = run_ee(&["handoff", "inspect", &cap, "--verify-hash", "--json"])?;
    ensure(
        inspect.status.code() == Some(EXIT_SUCCESS),
        "inspect should succeed for valid capsule",
    )?;
    let json = stdout_json(&inspect)?;
    ensure(
        json.get("validation_status").and_then(|v| v.as_str()) == Some("valid"),
        format!(
            "validation_status should be valid, got {:?}",
            json.get("validation_status")
        ),
    )?;
    ensure(
        json.get("hash_actual").and_then(|v| v.as_str()).is_some(),
        "hash_actual should be present when --verify-hash is set",
    )?;
    ensure(
        json.get("section_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            >= 3,
        "capsule should have at least 3 sections (workspace/objective/next_actions)",
    )?;
    Ok(())
}

#[test]
fn handoff_inspect_reports_missing_file_as_invalid() -> TestResult {
    let (_dir, ws) = init_workspace()?;
    let missing = capsule_path(&ws, "no-such-capsule.json");

    let output = run_ee(&["handoff", "inspect", &missing, "--json"])?;
    // inspect intentionally returns 0 even for missing files; it reports
    // validation_status=invalid in the JSON instead of erroring out.
    ensure(
        output.status.code() == Some(EXIT_SUCCESS),
        "inspect of missing file should not exit non-zero (it reports invalid)",
    )?;
    let json = stdout_json(&output)?;
    ensure(
        json.get("validation_status").and_then(|v| v.as_str()) == Some("invalid"),
        "missing capsule should be flagged invalid",
    )?;
    let warnings = json
        .get("warnings")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "warnings should be an array".to_string())?;
    ensure(
        warnings.iter().any(|w| {
            w.as_str()
                .map(|s| s.contains("does not exist"))
                .unwrap_or(false)
        }),
        format!("warnings should mention missing file, got {warnings:?}"),
    )?;
    Ok(())
}

#[test]
fn handoff_inspect_reports_corrupted_capsule_as_storage_error() -> TestResult {
    let (_dir, ws) = init_workspace()?;
    let cap = capsule_path(&ws, "corrupted.json");
    fs::write(&cap, "this is not json {{").map_err(|e| e.to_string())?;

    let output = run_ee(&["handoff", "inspect", &cap, "--json"])?;
    ensure(
        output.status.code() == Some(EXIT_STORAGE),
        format!(
            "corrupted capsule should exit with storage error (3), got {:?}",
            output.status.code()
        ),
    )?;
    let json = stdout_json(&output)?;
    ensure(
        json.get("schema").and_then(|v| v.as_str()) == Some("ee.error.v2"),
        "corrupted inspect should emit ee.error.v2 envelope",
    )?;
    ensure(
        json.pointer("/error/code").and_then(|v| v.as_str()) == Some("storage"),
        "error code should be storage",
    )?;
    Ok(())
}

#[test]
fn handoff_resume_emits_capsule_id_and_objective() -> TestResult {
    let (_dir, ws) = init_workspace()?;
    let cap = capsule_path(&ws, "cap.json");
    let create = run_ee(&[
        "--workspace",
        &ws,
        "handoff",
        "create",
        "--out",
        &cap,
        "--json",
    ])?;
    ensure(create.status.code() == Some(EXIT_SUCCESS), "create failed")?;
    let create_json = stdout_json(&create)?;
    let create_capsule_id = create_json
        .get("capsule_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "create missing capsule_id".to_string())?
        .to_string();

    let resume = run_ee(&["--workspace", &ws, "handoff", "resume", &cap, "--json"])?;
    ensure(
        resume.status.code() == Some(EXIT_SUCCESS),
        format!("resume should succeed, got {:?}", resume.status.code()),
    )?;
    let json = stdout_json(&resume)?;
    ensure(
        json.get("schema").and_then(|v| v.as_str()) == Some("ee.handoff.resume.v1"),
        "resume schema",
    )?;
    ensure(
        json.get("capsule_id").and_then(|v| v.as_str()) == Some(create_capsule_id.as_str()),
        "resume capsule_id matches create capsule_id",
    )?;
    ensure(
        json.get("current_objective")
            .and_then(|v| v.as_str())
            .is_some(),
        "resume should always emit a current_objective",
    )?;
    ensure(
        json.pointer("/swarm_brief_summary/schema")
            .and_then(|v| v.as_str())
            == Some("ee.support_bundle.swarm_brief_summary.v1"),
        "resume includes embedded swarm brief summary",
    )?;
    ensure(
        json.get("artifact_pointers")
            .and_then(|v| v.as_array())
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("id")
                        .and_then(|v| v.as_str())
                        .is_some_and(|id| id.starts_with("swarm_brief_summary:"))
                })
            }),
        "resume exposes swarm brief summary artifact pointer",
    )?;
    Ok(())
}

#[test]
fn handoff_resume_missing_capsule_returns_storage_error() -> TestResult {
    let (_dir, ws) = init_workspace()?;
    let missing = capsule_path(&ws, "no-such-capsule.json");

    let output = run_ee(&["--workspace", &ws, "handoff", "resume", &missing, "--json"])?;
    ensure(
        output.status.code() == Some(EXIT_STORAGE),
        format!(
            "resume of missing capsule must exit with storage error (3), got {:?}",
            output.status.code()
        ),
    )?;
    let json = stdout_json(&output)?;
    ensure(
        json.get("schema").and_then(|v| v.as_str()) == Some("ee.error.v2"),
        "missing-capsule resume should emit ee.error.v2 envelope",
    )?;
    ensure(
        json.pointer("/error/code").and_then(|v| v.as_str()) == Some("storage"),
        "error code should be storage",
    )?;
    Ok(())
}

#[test]
fn handoff_resume_is_structurally_consistent_across_runs() -> TestResult {
    let (_dir, ws) = init_workspace()?;
    let cap = capsule_path(&ws, "cap.json");
    let create = run_ee(&[
        "--workspace",
        &ws,
        "handoff",
        "create",
        "--out",
        &cap,
        "--json",
    ])?;
    ensure(create.status.code() == Some(EXIT_SUCCESS), "create failed")?;

    let mut prior_capsule_id: Option<String> = None;
    let mut prior_objective: Option<String> = None;
    for i in 0..3 {
        let resume = run_ee(&["--workspace", &ws, "handoff", "resume", &cap, "--json"])?;
        ensure(
            resume.status.code() == Some(EXIT_SUCCESS),
            format!("resume run {i} failed"),
        )?;
        let json = stdout_json(&resume)?;
        let capsule_id = json
            .get("capsule_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let objective = json
            .get("current_objective")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(prior) = &prior_capsule_id {
            ensure(
                capsule_id.as_deref() == Some(prior.as_str()),
                format!("resume run {i} capsule_id drifted from {:?}", prior),
            )?;
        }
        if let Some(prior) = &prior_objective {
            ensure(
                objective.as_deref() == Some(prior.as_str()),
                format!("resume run {i} current_objective drifted"),
            )?;
        }
        prior_capsule_id = capsule_id;
        prior_objective = objective;
    }
    Ok(())
}

#[test]
fn handoff_create_redacts_task_frame_secrets_from_capsule_file() -> TestResult {
    let (_dir, ws) = init_workspace()?;

    // Seed a task-frame containing key/value secrets plus bare secret-like
    // tokens. The unit-test suite proves the in-memory redaction path; this
    // test asserts the same is true when invoked through the CLI binary.
    let raw_api_token = format!("{}{}", concat!("sk", "-ant", "-api03", "-"), "A".repeat(52));
    let aws_key = format!("{}{}", concat!("AK", "IA"), "B".repeat(16));
    let jwt = [
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
        "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
        "Rq8IjqberX03cRIZHg7v0Rq8IjqberX03cRIZHg7v0",
    ]
    .join(".");
    let pem_body = concat!("MII", "Handoff", "Capsule", "Body");
    let pem_block = format!("-----BEGIN PRIVATE KEY-----\n{pem_body}\n-----END PRIVATE KEY-----");
    let goal = format!("Continue release with api_key=sk-live-XYZ123 and rotate {raw_api_token}");
    let current_focus = format!("verify handoff redaction for AWS key {aws_key}");
    let blocker = format!("password=hunter2; Authorization: Bearer {jwt}; {pem_block}");
    let frame_create = run_ee(&[
        "--workspace",
        &ws,
        "task-frame",
        "create",
        "--goal",
        &goal,
        "--actor",
        "cc_1_pane6",
        "--current-focus",
        &current_focus,
        "--blocker",
        &blocker,
        "--memory-id",
        "mem_redact_check",
        "--json",
    ])?;
    ensure(
        frame_create.status.code() == Some(EXIT_SUCCESS),
        format!(
            "task-frame create failed: exit={:?} stderr={}",
            frame_create.status.code(),
            String::from_utf8_lossy(&frame_create.stderr)
        ),
    )?;

    let cap = capsule_path(&ws, "cap_redact.json");
    let create = run_ee(&[
        "--workspace",
        &ws,
        "handoff",
        "create",
        "--out",
        &cap,
        "--json",
    ])?;
    ensure(
        create.status.code() == Some(EXIT_SUCCESS),
        format!(
            "create failed under redaction test: {:?}",
            create.status.code()
        ),
    )?;

    let body = fs::read_to_string(&cap).map_err(|e| e.to_string())?;
    ensure(
        !body.contains("sk-live-XYZ123"),
        "capsule file must not contain raw API key after redaction",
    )?;
    ensure(
        !body.contains(&raw_api_token),
        "capsule file must not contain raw bare API token after redaction",
    )?;
    ensure(
        !body.contains(&aws_key),
        "capsule file must not contain raw AWS key after redaction",
    )?;
    ensure(
        !body.contains(&jwt),
        "capsule file must not contain raw bearer/JWT value after redaction",
    )?;
    ensure(
        !body.contains(pem_body),
        "capsule file must not contain raw PEM body after redaction",
    )?;
    ensure(
        !body.contains("hunter2"),
        "capsule file must not contain raw password after redaction",
    )?;
    Ok(())
}

/// Documents the determinism gap: CLI-layer create produces a NEW
/// capsule_id and content_hash on every invocation because both
/// `generate_capsule_id` (UUID v7) and `created_at: Utc::now()` are
/// non-deterministic. The h0h1 acceptance criterion "Same inputs → same
/// capsule data_hash" is NOT met at the CLI layer; only the
/// `compute_content_hash(s)` function itself is deterministic over a
/// fixed string. This test pins the current behavior so that whoever
/// closes the determinism gap (file follow-up bead) sees the test fail
/// and updates the contract intentionally.
#[test]
fn handoff_create_capsule_ids_currently_differ_across_runs() -> TestResult {
    let (_dir, ws) = init_workspace()?;
    let cap1 = capsule_path(&ws, "cap1.json");
    let cap2 = capsule_path(&ws, "cap2.json");

    let a = run_ee(&[
        "--workspace",
        &ws,
        "handoff",
        "create",
        "--out",
        &cap1,
        "--json",
    ])?;
    let b = run_ee(&[
        "--workspace",
        &ws,
        "handoff",
        "create",
        "--out",
        &cap2,
        "--json",
    ])?;
    ensure(a.status.code() == Some(EXIT_SUCCESS), "first create failed")?;
    ensure(
        b.status.code() == Some(EXIT_SUCCESS),
        "second create failed",
    )?;

    let ja = stdout_json(&a)?;
    let jb = stdout_json(&b)?;
    let id_a = ja.get("capsule_id").and_then(|v| v.as_str()).unwrap_or("");
    let id_b = jb.get("capsule_id").and_then(|v| v.as_str()).unwrap_or("");
    ensure(
        id_a != id_b && !id_a.is_empty(),
        format!("expected distinct capsule ids per run (UUID v7), got a={id_a} b={id_b}"),
    )?;
    let hash_a = ja
        .get("content_hash")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let hash_b = jb
        .get("content_hash")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    ensure(
        hash_a != hash_b && !hash_a.is_empty(),
        format!(
            "expected distinct content_hashes (created_at + capsule_id are mixed in), got a={hash_a} b={hash_b}"
        ),
    )?;
    Ok(())
}
