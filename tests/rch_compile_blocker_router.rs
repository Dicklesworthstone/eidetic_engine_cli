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
