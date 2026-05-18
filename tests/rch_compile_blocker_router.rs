//! bd-1h8ji.5 — compile-blocker owner router contract tests.
//!
//! Exercises the non-mutating router script against synthetic RCH/Rust
//! transcripts and reservation payloads so failed remote verifiers can produce
//! deterministic JSON plus Agent Mail-ready summaries.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

type TestResult = Result<(), String>;
static FIXTURE_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    repo_root()
        .join("scripts")
        .join("rch_compile_blocker_router.py")
}

fn target_tmp_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/rch-compile-blocker-router"))
}

fn write_fixture(name: &str, text: &str) -> Result<PathBuf, String> {
    let index = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = target_tmp_dir().join(format!("router-{}-{index}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let path = dir.join(name);
    fs::write(&path, text).map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(path)
}

fn route(transcript: &str, reservations: Option<&str>) -> Result<Value, String> {
    let transcript_path = write_fixture("transcript.txt", transcript)?;
    let mut command = Command::new("python3");
    command
        .arg(script_path())
        .arg(&transcript_path)
        .arg("--command")
        .arg("cargo test --lib why_toon_matches_json_contract -- --nocapture")
        .arg("--bead-id")
        .arg("bd-test")
        .arg("--agent-name")
        .arg("SilentLark")
        .arg("--now")
        .arg("2026-05-16T05:00:00Z");
    let reservation_path;
    if let Some(payload) = reservations {
        reservation_path = write_fixture("reservations.json", payload)?;
        command.arg("--reservations").arg(&reservation_path);
    }
    let output = command
        .output()
        .map_err(|error| format!("run router: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "router failed with {:?}\nstdout={}\nstderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "parse router JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn preflight(
    dirty_paths: &str,
    reservations: Option<&str>,
    verifier_evidence: Option<&str>,
) -> Result<Value, String> {
    let dirty_path = write_fixture("dirty-paths.json", dirty_paths)?;
    let mut command = Command::new("python3");
    command
        .arg(script_path())
        .arg("--preflight")
        .arg("--dirty-paths")
        .arg(&dirty_path)
        .arg("--command")
        .arg("cargo test --lib audit_lane_property -- --nocapture")
        .arg("--bead-id")
        .arg("bd-preflight")
        .arg("--agent-name")
        .arg("SilentLark")
        .arg("--now")
        .arg("2026-05-16T05:00:00Z")
        .arg("--json");
    let reservation_path;
    if let Some(payload) = reservations {
        reservation_path = write_fixture("reservations.json", payload)?;
        command.arg("--reservations").arg(&reservation_path);
    }
    let evidence_path;
    if let Some(payload) = verifier_evidence {
        evidence_path = write_fixture("verifier-evidence.json", payload)?;
        command.arg("--verifier-evidence").arg(&evidence_path);
    }
    let output = command
        .output()
        .map_err(|error| format!("run preflight: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "preflight failed with {:?}\nstdout={}\nstderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "parse preflight JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn exact_reservation_routes_to_other_agent() -> TestResult {
    let transcript = "error[E0308]: mismatched types\n  --> src/output/streaming.rs:558:31\n   |\n558 |     pack_hash_from_batch(pack)\n";
    let reservations = r#"{
      "conflicts": [{
        "path": "src/output/streaming.rs",
        "holders": [{
          "agent": "CloudyHawk",
          "path_pattern": "src/output/streaming.rs",
          "expires_ts": "2026-05-16T06:00:00Z"
        }]
      }]
    }"#;
    let report = route(transcript, Some(reservations))?;
    if report["schema"] != "ee.rch.compile_blocker_route.v1" {
        return Err(format!("unexpected schema: {report}"));
    }
    if report["routing_decision"] != "reserved_by_other_agent"
        || report["owner_agent"] != "CloudyHawk"
    {
        return Err(format!("expected CloudyHawk reservation route: {report}"));
    }
    if report["first_error"]["file"] != "src/output/streaming.rs"
        || report["first_error"]["line"] != 558
        || report["first_error"]["code"] != "E0308"
    {
        return Err(format!("unexpected first diagnostic: {report}"));
    }
    let summary = report["summary_markdown"].as_str().unwrap_or("");
    if !summary.contains("reserved_by_other_agent") || !summary.contains("CloudyHawk") {
        return Err(format!("summary missing owner route: {summary}"));
    }
    Ok(())
}

#[test]
fn glob_reservation_matches_nested_file() -> TestResult {
    let transcript = "error[E0599]: no method named `stream`\n  --> src/cli/mod.rs:9185:12\n";
    let reservations = r#"[{
      "agent": "TanMoose",
      "path_pattern": "src/cli/*.rs",
      "expires_ts": "2026-05-16T06:00:00Z"
    }]"#;
    let report = route(transcript, Some(reservations))?;
    if report["routing_decision"] != "reserved_by_other_agent"
        || report["owner_agent"] != "TanMoose"
    {
        return Err(format!("glob reservation did not route: {report}"));
    }
    Ok(())
}

#[test]
fn expired_reservation_is_ignored_as_no_owner() -> TestResult {
    let transcript = "error[E0432]: unresolved import\n  --> src/core/search.rs:12:5\n";
    let reservations = r#"[{
      "agent": "StormyCove",
      "path_pattern": "src/core/search.rs",
      "expires_ts": "2026-05-16T04:00:00Z"
    }]"#;
    let report = route(transcript, Some(reservations))?;
    if report["routing_decision"] != "no_owner_found" || !report["owner_agent"].is_null() {
        return Err(format!("expired reservation should be ignored: {report}"));
    }
    Ok(())
}

#[test]
fn current_agent_reservation_is_self_fix_allowed() -> TestResult {
    let transcript =
        "error[E0425]: cannot find value `stderr`\n  --> tests/rch_verify_contract.rs:42:9\n";
    let reservations = r#"[{
      "agent": "SilentLark",
      "path_pattern": "tests/rch_verify_contract.rs",
      "expires_ts": "2026-05-16T06:00:00Z"
    }]"#;
    let report = route(transcript, Some(reservations))?;
    if report["routing_decision"] != "self_fix_allowed" || report["owner_agent"] != "SilentLark" {
        return Err(format!("self reservation should allow self fix: {report}"));
    }
    Ok(())
}

#[test]
fn upstream_dependency_failure_is_not_local_ownership() -> TestResult {
    let transcript =
        "error[E0277]: trait bound failed\n  --> /data/projects/asupersync/src/runtime.rs:44:8\n";
    let report = route(transcript, None)?;
    if report["routing_decision"] != "upstream_dependency_failure" {
        return Err(format!("expected upstream dependency route: {report}"));
    }
    Ok(())
}

#[test]
fn topology_refusal_is_environment_failure() -> TestResult {
    let transcript = "[RCH] local (dependency preflight RCH-E327: Path dependency topology policy failed.)\n[RCH] remote required; refusing local fallback (dependency preflight failed)\n";
    let report = route(transcript, None)?;
    if report["routing_decision"] != "environment_failure" {
        return Err(format!("expected environment failure route: {report}"));
    }
    Ok(())
}

#[test]
fn preflight_clean_snapshot_allows_rch_launch() -> TestResult {
    let report = preflight(r#"[]"#, Some(r#"[]"#), None)?;
    if report["schema"] != "ee.swarm_compile_blockers.v1"
        || report["safeToLaunchRch"] != true
        || report["compileBlockers"] != serde_json::json!([])
    {
        return Err(format!("clean preflight should allow launch: {report}"));
    }
    Ok(())
}

#[test]
fn preflight_dirty_unreserved_rust_file_is_unknown() -> TestResult {
    let dirty = r#"[{"path":"src/core/status.rs","state":"modified"}]"#;
    let report = preflight(dirty, Some(r#"[]"#), None)?;
    if report["safeToLaunchRch"] != "unknown" {
        return Err(format!("dirty unreserved path should be unknown: {report}"));
    }
    let blocker = &report["compileBlockers"][0];
    if blocker["path"] != "src/core/status.rs"
        || blocker["severity"] != "medium"
        || blocker["ownerAgent"] != serde_json::Value::Null
    {
        return Err(format!("unexpected dirty unreserved blocker: {report}"));
    }
    Ok(())
}

#[test]
fn preflight_dirty_reserved_rust_file_blocks_and_templates_owner_mail() -> TestResult {
    let dirty = r#"[{"path":"src/db/mod.rs","state":"modified"}]"#;
    let reservations = r#"[{
      "agent": "CloudyHawk",
      "path_pattern": "src/db/mod.rs",
      "expires_ts": "2026-05-16T06:00:00Z"
    }]"#;
    let report = preflight(dirty, Some(reservations), None)?;
    if report["safeToLaunchRch"] != false {
        return Err(format!("reserved dirty path should block RCH: {report}"));
    }
    let blocker = &report["compileBlockers"][0];
    if blocker["severity"] != "high"
        || blocker["reason"] != "dirty_compile_critical_path_reserved_by_other_agent"
        || blocker["ownerAgent"] != "CloudyHawk"
    {
        return Err(format!("unexpected reserved blocker: {report}"));
    }
    let mail = report["mailTemplate"].as_str().unwrap_or("");
    if !mail.contains("[compile-blocker]") || !mail.contains("src/db/mod.rs") {
        return Err(format!("mail template missing blocker evidence: {report}"));
    }
    Ok(())
}

#[test]
fn preflight_expired_reservation_is_ignored() -> TestResult {
    let dirty = r#"[{"path":"src/db/mod.rs","state":"modified"}]"#;
    let reservations = r#"[{
      "agent": "CloudyHawk",
      "path_pattern": "src/db/mod.rs",
      "expires_ts": "2026-05-16T04:59:59Z"
    }]"#;
    let report = preflight(dirty, Some(reservations), None)?;
    if report["safeToLaunchRch"] != "unknown"
        || report["compileBlockers"][0]["ownerAgent"] != serde_json::Value::Null
    {
        return Err(format!(
            "expired reservation should not own blocker: {report}"
        ));
    }
    Ok(())
}

#[test]
fn preflight_recent_first_error_matching_dirty_path_blocks_rch() -> TestResult {
    let dirty = r#"[{"path":"src/db/mod.rs","state":"modified"}]"#;
    let evidence = r#"[{
      "schema": "ee.rch.verify.v1",
      "status": "remote_failure",
      "command_text": "cargo test --lib ppr_proof -- --nocapture",
      "command_hash": "abc123",
      "first_error_file": "src/db/mod.rs",
      "first_error_line": 431,
      "degraded_codes": ["rch_verify_remote_command_failed"]
    }]"#;
    let report = preflight(dirty, Some(r#"[]"#), Some(evidence))?;
    if report["safeToLaunchRch"] != false {
        return Err(format!("matching first error should block RCH: {report}"));
    }
    let blocker = &report["compileBlockers"][0];
    if blocker["reason"] != "recent_rch_first_error_matches_dirty_path"
        || blocker["recentFirstError"]["line"] != 431
    {
        return Err(format!("unexpected first-error blocker: {report}"));
    }
    Ok(())
}

#[test]
fn preflight_recent_first_error_unrelated_to_dirty_path_does_not_escalate() -> TestResult {
    let dirty = r#"[{"path":"docs/rch_runbook.md","state":"modified"}]"#;
    let evidence = r#"[{
      "schema": "ee.rch.verify.v1",
      "status": "remote_failure",
      "command_text": "cargo test --lib ppr_proof -- --nocapture",
      "command_hash": "abc123",
      "first_error_file": "src/db/mod.rs",
      "first_error_line": 431,
      "degraded_codes": ["rch_verify_remote_command_failed"]
    }]"#;
    let report = preflight(dirty, Some(r#"[]"#), Some(evidence))?;
    if report["safeToLaunchRch"] != true || report["compileBlockers"] != serde_json::json!([]) {
        return Err(format!(
            "unrelated stale first error should not block: {report}"
        ));
    }
    Ok(())
}

#[test]
fn preflight_missing_agent_mail_snapshot_degrades_without_false_safety() -> TestResult {
    let dirty = r#"[{"path":"src/db/mod.rs","state":"modified"}]"#;
    let report = preflight(dirty, None, None)?;
    if report["safeToLaunchRch"] != "unknown" {
        return Err(format!("missing reservations should be unknown: {report}"));
    }
    let degraded = report["degradedCodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !degraded
        .iter()
        .any(|code| code == "agent_mail_reservations_unavailable")
    {
        return Err(format!("missing Agent Mail degraded code absent: {report}"));
    }
    Ok(())
}

#[test]
fn preflight_e2e_temp_repo_logs_and_does_not_mutate_inputs() -> TestResult {
    let index = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = target_tmp_dir().join(format!("preflight-e2e-{}-{index}", std::process::id()));
    let repo_dir = dir.join("repo");
    fs::create_dir_all(repo_dir.join("src/db"))
        .map_err(|error| format!("create {}: {error}", repo_dir.display()))?;
    fs::write(repo_dir.join("src/db/mod.rs"), "pub fn dirty() {}\n")
        .map_err(|error| format!("write temp source: {error}"))?;
    let git_output = Command::new("git")
        .arg("init")
        .arg("-b")
        .arg("main")
        .arg(&repo_dir)
        .output()
        .map_err(|error| format!("git init temp repo: {error}"))?;
    if !git_output.status.success() {
        return Err(format!(
            "git init temp repo failed\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&git_output.stdout),
            String::from_utf8_lossy(&git_output.stderr)
        ));
    }

    let dirty_json =
        r#"{"source_state":{"dirty_paths_sample":[{"path":"src/db/mod.rs","kind":"tracked"}]}}"#;
    let reservation_json = r#"[{
      "agent": "CloudyHawk",
      "path_pattern": "src/db/*.rs",
      "expires_ts": "2026-05-16T06:00:00Z"
    }]"#;
    let evidence_json = r#"[{
      "schema": "ee.rch.verify.v1",
      "status": "remote_failure",
      "command_text": "cargo test --lib ppr_proof -- --nocapture",
      "command_hash": "abc123",
      "first_error_file": "src/db/mod.rs",
      "first_error_line": 431,
      "degraded_codes": ["rch_verify_remote_command_failed"]
    }]"#;
    let dirty_path = dir.join("dirty.json");
    let reservation_path = dir.join("reservations.json");
    let evidence_path = dir.join("evidence.json");
    let event_log = dir.join("preflight-events.jsonl");
    fs::write(&dirty_path, dirty_json).map_err(|error| format!("write dirty json: {error}"))?;
    fs::write(&reservation_path, reservation_json)
        .map_err(|error| format!("write reservation json: {error}"))?;
    fs::write(&evidence_path, evidence_json)
        .map_err(|error| format!("write evidence json: {error}"))?;
    fs::write(
        &event_log,
        serde_json::json!({
            "schema": "ee.test_event.v1",
            "test_id": "bd-3vwx0.8_compile_blocker_preflight",
            "kind": "setup",
            "fields": {"repo": repo_dir.to_string_lossy()}
        })
        .to_string()
            + "\n",
    )
    .map_err(|error| format!("write setup event: {error}"))?;

    let output = Command::new("python3")
        .arg(script_path())
        .arg("--preflight")
        .arg("--dirty-paths")
        .arg(&dirty_path)
        .arg("--reservations")
        .arg(&reservation_path)
        .arg("--verifier-evidence")
        .arg(&evidence_path)
        .arg("--command")
        .arg("cargo test --lib ppr_proof -- --nocapture")
        .arg("--bead-id")
        .arg("bd-3vwx0.8")
        .arg("--agent-name")
        .arg("SilentLark")
        .arg("--now")
        .arg("2026-05-16T05:00:00Z")
        .arg("--json")
        .output()
        .map_err(|error| format!("run preflight e2e: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "preflight e2e failed\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let report: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "parse preflight e2e JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })?;
    fs::OpenOptions::new()
        .append(true)
        .open(&event_log)
        .and_then(|mut file| {
            use std::io::Write;
            writeln!(
                file,
                "{}",
                serde_json::json!({
                    "schema": "ee.test_event.v1",
                    "test_id": "bd-3vwx0.8_compile_blocker_preflight",
                    "kind": "verify",
                    "fields": {
                        "safeToLaunchRch": report["safeToLaunchRch"],
                        "blockerCount": report["compileBlockers"].as_array().map_or(0, Vec::len)
                    }
                })
            )
        })
        .map_err(|error| format!("append verify event: {error}"))?;

    if report["safeToLaunchRch"] != false
        || report["compileBlockers"][0]["ownerAgent"] != "CloudyHawk"
        || report["compileBlockers"][0]["recentFirstError"]["line"] != 431
    {
        return Err(format!("unexpected preflight e2e report: {report}"));
    }
    if fs::read_to_string(&dirty_path).map_err(|error| format!("read dirty json: {error}"))?
        != dirty_json
        || fs::read_to_string(&reservation_path)
            .map_err(|error| format!("read reservations json: {error}"))?
            != reservation_json
        || fs::read_to_string(&evidence_path)
            .map_err(|error| format!("read evidence json: {error}"))?
            != evidence_json
    {
        return Err("preflight e2e mutated one of its input snapshots".to_owned());
    }
    let log_text =
        fs::read_to_string(&event_log).map_err(|error| format!("read event log: {error}"))?;
    if log_text.lines().count() != 2 || !log_text.contains("\"schema\":\"ee.test_event.v1\"") {
        return Err(format!(
            "event log missing ee.test_event.v1 rows: {log_text}"
        ));
    }
    Ok(())
}
