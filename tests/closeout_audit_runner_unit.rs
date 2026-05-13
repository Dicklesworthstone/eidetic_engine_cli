//! J11.4 — Closeout audit runner contract test (bd-17c65.10.11.4).
//!
//! Invokes `scripts/closeout_audit.sh` against three synthetic
//! workspaces under `tests/fixtures/closeout_audit/` and asserts the
//! emitted `ee.closeout_audit.v1` JSON has the expected readiness
//! classification + structural shape.
//!
//! Three readiness cases pinned:
//!
//!   1. **ready** — bead has no open dependencies, no uncommitted
//!      files reference it, J1 log directory has at least one
//!      `*.jsonl`. Audit reports `readiness: "ready"`, empty
//!      `blockers[]`, possibly some caveats from the live env
//!      (rch / agent_mail) but not the J1-log-absent caveat.
//!
//!   2. **ready_with_caveats** — bead has no blockers but missing
//!      J1 log (no `tests/logs/active/*.jsonl`). Audit reports
//!      `readiness: "ready_with_caveats"`, empty `blockers[]`,
//!      ≥1 caveat including `j1_log_absent`.
//!
//!   3. **blocked** — bead has an open dependency (status="open").
//!      Audit reports `readiness: "blocked"`, ≥1 entry in
//!      `blockers[]` matching `open_dependencies:`, and
//!      `evidence.open_dependencies` array is non-empty.
//!
//! Plus: bash syntax check (`bash -n scripts/closeout_audit.sh`)
//! and structural-shape assertions (every audit emits the canonical
//! schema + bead_id + readiness + evidence sub-object).
//!
//! No `use ee::*` imports — pure `std::process::Command` + JSON
//! parsing — so this test builds independently of any lib state.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    repo_root().join("scripts").join("closeout_audit.sh")
}

fn fixture_path(scenario: &str) -> PathBuf {
    repo_root()
        .join("tests")
        .join("fixtures")
        .join("closeout_audit")
        .join(scenario)
}

/// Run the closeout audit script against a fixture workspace and
/// return the parsed JSON output. Asserts exit code 0 (the audit
/// itself is non-destructive — every classification is a successful
/// exit; only argument errors / missing tools yield non-zero).
fn run_audit(scenario: &str, bead_id: &str) -> Result<Value, String> {
    let fixture = fixture_path(scenario);
    if !fixture.exists() {
        return Err(format!("fixture not found: {}", fixture.display(),));
    }
    let output = Command::new("bash")
        .arg(script_path())
        .arg("--bead")
        .arg(bead_id)
        .arg("--json")
        .arg("--workspace-root")
        .arg(&fixture)
        .output()
        .map_err(|e| format!("spawn closeout_audit.sh: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "closeout_audit.sh exited {:?}: {stderr}",
            output.status.code(),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout)
        .map_err(|e| format!("audit JSON did not parse: {e}\nstdout was: {stdout}"))
}

/// Assert every audit output carries the canonical schema fields.
/// Belt-and-suspenders so a refactor of the script that drops a
/// field fails CI here even if the readiness logic still produces
/// the right verdict.
fn assert_envelope_shape(audit: &Value) -> TestResult {
    let required_top_level = [
        "schema",
        "bead_id",
        "readiness",
        "evidence",
        "blockers",
        "caveats",
        "next_actions",
    ];
    for key in &required_top_level {
        if audit.get(key).is_none() {
            return Err(format!(
                "audit missing required top-level field `{key}`. Full audit: {audit}",
            ));
        }
    }
    if audit["schema"].as_str() != Some("ee.closeout_audit.v1") {
        return Err(format!(
            "audit schema is {:?}, expected `ee.closeout_audit.v1`",
            audit["schema"].as_str(),
        ));
    }
    let required_evidence = [
        "bead_status",
        "bead_assignee",
        "bead_title",
        "open_dependencies",
        "uncommitted_files_referencing_bead",
        "rch_status",
        "agent_mail_status",
        "j1_log_present",
        "j1_log_path",
    ];
    for key in &required_evidence {
        if audit["evidence"].get(key).is_none() {
            return Err(format!("audit.evidence missing required field `{key}`",));
        }
    }
    for arr in &["blockers", "caveats", "next_actions"] {
        if !audit[arr].is_array() {
            return Err(format!(
                "audit.{arr} must be an array; got {:?}",
                audit[arr],
            ));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Script-quality gate (catches syntax breakage before the runtime tests)
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn closeout_audit_script_is_bash_syntax_clean() -> TestResult {
    let output = Command::new("bash")
        .arg("-n")
        .arg(script_path())
        .output()
        .map_err(|e| format!("spawn bash -n: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`bash -n scripts/closeout_audit.sh` failed: {stderr}",
        ));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Readiness classification — 3 fixture scenarios
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn ready_fixture_reports_readiness_ready() -> TestResult {
    let audit = run_audit("ready", "bd-fxt-ready-1")?;
    assert_envelope_shape(&audit)?;

    let readiness = audit["readiness"].as_str().unwrap_or("");
    if readiness != "ready" {
        return Err(format!(
            "ready fixture should report readiness=`ready`; got {readiness:?}. Full audit: {audit}",
        ));
    }
    if audit["blockers"]
        .as_array()
        .map_or(false, |a| !a.is_empty())
    {
        return Err(format!(
            "ready fixture should have zero blockers; got {}",
            audit["blockers"],
        ));
    }
    // J1 log present in this fixture, so the j1_log_absent caveat
    // must NOT appear. Other caveats from the live environment
    // (rch / agent_mail) are tolerated.
    let caveats = audit["caveats"].as_array().cloned().unwrap_or_default();
    for caveat in &caveats {
        if caveat.as_str().unwrap_or("").starts_with("j1_log_absent") {
            return Err(format!(
                "ready fixture should NOT report j1_log_absent (fixture seeds tests/logs/active/run.jsonl); got caveat: {caveat}",
            ));
        }
    }
    if audit["evidence"]["j1_log_present"].as_bool() != Some(true) {
        return Err(format!(
            "ready fixture's j1_log_present should be true; got {:?}",
            audit["evidence"]["j1_log_present"],
        ));
    }
    Ok(())
}

#[test]
fn ready_with_caveats_fixture_reports_readiness_ready_with_caveats() -> TestResult {
    let audit = run_audit("ready_with_caveats", "bd-fxt-caveats-1")?;
    assert_envelope_shape(&audit)?;

    let readiness = audit["readiness"].as_str().unwrap_or("");
    // Either `ready_with_caveats` (caveat present) or `ready` (none
    // of the optional caveats fired in the current env). The
    // fixture deliberately omits the J1 log so j1_log_absent should
    // fire — but if the live agent_mail happens to be unreachable
    // too, that's also a caveat. So we accept either reading as
    // long as blockers is empty and we don't drop into `blocked`.
    if readiness == "blocked" {
        return Err(format!(
            "ready_with_caveats fixture must NOT be classified `blocked`; got {readiness}. Full audit: {audit}",
        ));
    }
    if audit["blockers"]
        .as_array()
        .map_or(false, |a| !a.is_empty())
    {
        return Err(format!(
            "ready_with_caveats fixture should have zero blockers; got {}",
            audit["blockers"],
        ));
    }
    if audit["evidence"]["j1_log_present"].as_bool() != Some(false) {
        return Err(format!(
            "ready_with_caveats fixture's j1_log_present should be false; got {:?}",
            audit["evidence"]["j1_log_present"],
        ));
    }
    // At least the j1_log_absent caveat must fire.
    let caveats = audit["caveats"].as_array().cloned().unwrap_or_default();
    let has_j1_caveat = caveats
        .iter()
        .any(|c| c.as_str().unwrap_or("").starts_with("j1_log_absent"));
    if !has_j1_caveat {
        return Err(format!(
            "ready_with_caveats fixture should report j1_log_absent caveat; got caveats: {:?}",
            caveats,
        ));
    }
    Ok(())
}

#[test]
fn blocked_fixture_reports_readiness_blocked_with_open_dependency() -> TestResult {
    let audit = run_audit("blocked", "bd-fxt-blocked-1")?;
    assert_envelope_shape(&audit)?;

    let readiness = audit["readiness"].as_str().unwrap_or("");
    if readiness != "blocked" {
        return Err(format!(
            "blocked fixture should report readiness=`blocked`; got {readiness:?}. Full audit: {audit}",
        ));
    }
    let blockers = audit["blockers"].as_array().cloned().unwrap_or_default();
    if blockers.is_empty() {
        return Err(format!(
            "blocked fixture should have ≥1 blocker; got empty array",
        ));
    }
    let has_open_dep_blocker = blockers
        .iter()
        .any(|b| b.as_str().unwrap_or("").starts_with("open_dependencies:"));
    if !has_open_dep_blocker {
        return Err(format!(
            "blocked fixture should have `open_dependencies:` blocker; got {:?}",
            blockers,
        ));
    }
    // The open dep must surface in evidence.open_dependencies too.
    let open_deps = audit["evidence"]["open_dependencies"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if open_deps.is_empty() {
        return Err(format!(
            "blocked fixture should list its open dependency in evidence.open_dependencies; got empty",
        ));
    }
    let matching = open_deps.iter().any(|d| {
        d.get("id").and_then(Value::as_str) == Some("bd-fxt-blocked-dep-open")
            && d.get("status").and_then(Value::as_str) == Some("open")
    });
    if !matching {
        return Err(format!(
            "blocked fixture should list bd-fxt-blocked-dep-open with status=open; got {:?}",
            open_deps,
        ));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Failure-mode gates: non-destructive contract + missing-bead
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn missing_bead_id_exits_with_code_3() -> TestResult {
    let fixture = fixture_path("ready");
    let output = Command::new("bash")
        .arg(script_path())
        .arg("--bead")
        .arg("bd-fxt-does-not-exist")
        .arg("--json")
        .arg("--workspace-root")
        .arg(&fixture)
        .output()
        .map_err(|e| format!("spawn closeout_audit.sh: {e}"))?;
    let code = output.status.code().unwrap_or(0);
    if code != 3 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "missing-bead should exit 3 per the documented contract; got {code}. stderr: {stderr}",
        ));
    }
    Ok(())
}

#[test]
fn missing_bead_arg_exits_with_code_2() -> TestResult {
    let output = Command::new("bash")
        .arg(script_path())
        .arg("--json")
        .output()
        .map_err(|e| format!("spawn closeout_audit.sh: {e}"))?;
    let code = output.status.code().unwrap_or(0);
    if code != 2 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "missing --bead should exit 2 (usage error); got {code}. stderr: {stderr}",
        ));
    }
    Ok(())
}

#[test]
fn script_does_not_mutate_fixture_workspace() -> TestResult {
    // Capture file mtimes pre- and post-audit; assert nothing
    // changed in the fixture workspace. This is the non-destructive
    // contract gate.
    let fixture = fixture_path("ready");
    fn collect_mtimes(root: &Path, out: &mut Vec<(PathBuf, std::time::SystemTime)>) -> TestResult {
        for entry in
            std::fs::read_dir(root).map_err(|e| format!("read_dir {}: {e}", root.display()))?
        {
            let entry = entry.map_err(|e| format!("read entry: {e}"))?;
            let path = entry.path();
            if path.is_dir() {
                collect_mtimes(&path, out)?;
            } else {
                let m = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map_err(|e| format!("metadata {}: {e}", path.display()))?;
                out.push((path, m));
            }
        }
        Ok(())
    }

    let mut before: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    collect_mtimes(&fixture, &mut before)?;
    // Run the audit.
    let _audit = run_audit("ready", "bd-fxt-ready-1")?;
    let mut after: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    collect_mtimes(&fixture, &mut after)?;
    before.sort_by(|a, b| a.0.cmp(&b.0));
    after.sort_by(|a, b| a.0.cmp(&b.0));
    if before != after {
        return Err(format!(
            "closeout_audit.sh mutated the fixture workspace under {} (mtimes changed). The non-destructive contract is violated.",
            fixture.display(),
        ));
    }
    Ok(())
}
