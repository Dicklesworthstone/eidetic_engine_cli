//! Issue #80: native CLI reservation evidence, identity binding, and restoration.
//! The corrected 50-reservation report used a different project; the direct
//! regression is one active src/** reservation in the matching workspace.

use std::{fs, path::Path, process::Command};

use chrono::{Duration, Utc};
use serde_json::{Value, json};

#[path = "agent_mail_fixture/snapshot_v1.rs"]
mod fixture;

type TestResult = Result<(), String>;

fn call(workspace: &str, args: &[&str]) -> Result<Value, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["--workspace", workspace, "--json"])
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| error.to_string())?;
    // bd-w5bza: print all THREE facts. `output.status` Display renders
    // "signal: 9" where `.code()` is None, and `.code()` is what the drift
    // guard credits, so both are spelled. Comment sits ABOVE the `if`: the
    // guard's assertion_window caps a site at 14 lines from the condition and
    // a comment inside the block would push the tokens out of scope.
    if !output.status.success() {
        return Err(format!(
            "ee {args:?} failed: {} (code {:?})\nstdout: {}\nstderr: {}",
            output.status,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())
}

fn workspace() -> Result<(tempfile::TempDir, String), String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = fs::canonicalize(dir.path()).map_err(|error| error.to_string())?;
    let name = path.to_str().ok_or("non-UTF-8 workspace")?.to_owned();
    call(&name, &["init"])?;
    Ok((dir, name))
}

fn snapshot(project: &str, active: bool) -> Result<Value, String> {
    let text = fixture::declared_snapshot_v1(
        Path::new(project),
        "CaptureAgent",
        &[fixture::ReservationFixture {
            path_pattern: "src/**",
            holder: "OtherAgent",
            exclusive: true,
        }],
    )?;
    let mut value: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    value["file_reservations"][0]["expires_ts"] =
        json!((Utc::now() + Duration::seconds(if active { 3600 } else { -1 })).to_rfc3339());
    Ok(value)
}

fn write_snapshot(workspace: &str, value: &Value) -> Result<String, String> {
    let path = format!("{workspace}/.ee/agent-mail-snapshot.json");
    fs::write(&path, value.to_string()).map_err(|error| error.to_string())?;
    Ok(path)
}

fn create(workspace: &str, extra: &[&str]) -> Result<(String, Value), String> {
    let path = format!("{workspace}/handoff.json");
    let mut args = vec![
        "handoff",
        "create",
        "--out",
        &path,
        "--profile",
        "compact",
        "--command-timeout-ms",
        "50",
    ];
    args.extend_from_slice(extra);
    call(workspace, &args)?;
    let capsule: Value =
        serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let section = capsule["sections"]
        .as_array()
        .ok_or("missing sections")?
        .iter()
        .find(|section| section["id"] == "agent_mail_reservations")
        .ok_or("missing reservation section")?;
    let evidence = serde_json::from_str(section["content"].as_str().ok_or("section content")?)
        .map_err(|error| error.to_string())?;
    Ok((path, evidence))
}

fn resume(workspace: &str, path: &str, extra: &[&str]) -> Result<Value, String> {
    let mut args = vec!["handoff", "resume", path];
    args.extend_from_slice(extra);
    let report = call(workspace, &args)?;
    report["next_actions"]
        .as_array()
        .ok_or("missing next actions")?
        .iter()
        .filter_map(|action| action["reason"].as_str())
        .filter_map(|reason| serde_json::from_str::<Value>(reason).ok())
        .find(|value| value.get("currentReservationCount").is_some())
        .ok_or_else(|| "missing current reservation evidence at resume".to_owned())
}

fn verified(value: &Value, count: u64) {
    assert_eq!(value["authoritativeAtObservation"], true, "{value}");
    assert_eq!(value["currentReservationCount"], count, "{value}");
}

fn unknown(value: &Value) {
    assert_eq!(value["authoritativeAtObservation"], false, "{value}");
    assert!(value["currentReservationCount"].is_null(), "{value}");
}

#[test]
fn matching_default_reservation_survives_preview_capture_and_hmac_restore() -> TestResult {
    let (_dir, ws) = workspace()?;
    let input = write_snapshot(&ws, &snapshot(&ws, true)?)?;
    let original = fs::read(&input).map_err(|error| error.to_string())?;
    let preview = call(&ws, &["handoff", "preview", "--command-timeout-ms", "50"])?;
    assert_eq!(
        preview["swarm_brief_summary"]["counts"]["activeReservationCount"],
        1
    );
    let (path, evidence) = create(&ws, &[])?;
    verified(&evidence, 1);
    assert_eq!(evidence["historical"], true);
    let current = resume(&ws, &path, &[])?;
    verified(&current, 1);
    assert_eq!(current["historical"], false);
    call(&ws, &["handoff", "inspect", &path, "--verify-hash"])?;
    assert_eq!(
        fs::read(input).map_err(|error| error.to_string())?,
        original
    );
    Ok(())
}

#[test]
fn explicit_snapshot_beats_invalid_default_for_all_three_operations() -> TestResult {
    let (_dir, ws) = workspace()?;
    write_snapshot(&ws, &json!({"invalid": true}))?;
    let selected = format!("{ws}/selected.json");
    fs::write(&selected, snapshot(&ws, true)?.to_string()).map_err(|error| error.to_string())?;
    let extra = ["--agent-mail-snapshot", selected.as_str()];
    let preview = call(
        &ws,
        &[
            "handoff",
            "preview",
            "--command-timeout-ms",
            "50",
            extra[0],
            extra[1],
        ],
    )?;
    assert_eq!(
        preview["swarm_brief_summary"]["counts"]["activeReservationCount"],
        1
    );
    let (path, evidence) = create(&ws, &extra)?;
    verified(&evidence, 1);
    assert_eq!(evidence["snapshotInput"], "explicit");
    verified(&resume(&ws, &path, &extra)?, 1);
    Ok(())
}

#[test]
fn distinct_project_requires_independent_binding_at_capture_and_resume() -> TestResult {
    let (_checkout, ws) = workspace()?;
    let (_project, project) = workspace()?;
    write_snapshot(&ws, &snapshot(&project, true)?)?;
    unknown(&create(&ws, &[])?.1);
    let key = fixture::project_key_for_workspace(Path::new(&project))?;
    let extra = ["--agent-mail-project-key", key.as_str()];
    let (path, evidence) = create(&ws, &extra)?;
    verified(&evidence, 1);
    assert_eq!(evidence["binding"], "explicit_project_key");
    assert_eq!(evidence["expectedProjectKey"], key);
    unknown(&resume(&ws, &path, &[])?);
    verified(&resume(&ws, &path, &extra)?, 1);
    let wrong = fixture::project_key_for_workspace(Path::new(&ws))?;
    unknown(&create(&ws, &[extra[0], &wrong])?.1);
    unknown(&create(&ws, &[extra[0], "invalid-key"])?.1);
    Ok(())
}

#[test]
fn expired_reservations_are_a_verified_empty_active_set() -> TestResult {
    let (_dir, ws) = workspace()?;
    write_snapshot(&ws, &snapshot(&ws, false)?)?;
    let (path, evidence) = create(&ws, &[])?;
    verified(&evidence, 0);
    verified(&resume(&ws, &path, &[])?, 0);
    Ok(())
}

#[test]
fn missing_explicit_snapshot_does_not_fall_back_to_default_or_capsule() -> TestResult {
    let (_dir, ws) = workspace()?;
    unknown(&create(&ws, &[])?.1);
    write_snapshot(&ws, &snapshot(&ws, true)?)?;
    let (path, evidence) = create(&ws, &[])?;
    verified(&evidence, 1);
    let missing = format!("{ws}/missing.json");
    let extra = ["--agent-mail-snapshot", missing.as_str()];
    unknown(&resume(&ws, &path, &extra)?);
    unknown(&create(&ws, &extra)?.1);
    Ok(())
}

#[test]
fn malformed_declared_snapshots_are_not_salvaged_or_leaked() -> TestResult {
    let (_dir, ws) = workspace()?;
    let good = snapshot(&ws, true)?;
    for field in [
        "project_key",
        "generated_at",
        "command_statuses",
        "redaction_status",
    ] {
        let mut bad = good.clone();
        bad.as_object_mut().ok_or("fixture object")?.remove(field);
        write_snapshot(&ws, &bad)?;
        unknown(&create(&ws, &[])?.1);
    }
    let mut bad = good;
    bad["summary"]["file_reservation_count"] = json!(50);
    write_snapshot(&ws, &bad)?;
    unknown(&create(&ws, &[])?.1);
    bad["raw_mail_body"] = json!("secret-body-must-not-leak");
    write_snapshot(&ws, &bad)?;
    let (path, evidence) = create(&ws, &[])?;
    unknown(&evidence);
    assert!(
        !fs::read_to_string(&path)
            .map_err(|error| error.to_string())?
            .contains("secret-body-must-not-leak")
    );
    fs::write(format!("{ws}/.ee/agent-mail-snapshot.json"), "{not-json")
        .map_err(|error| error.to_string())?;
    unknown(&resume(&ws, &path, &[])?);
    Ok(())
}

#[test]
fn stale_future_and_undeclared_snapshots_never_authorize_reservations() -> TestResult {
    let (_dir, ws) = workspace()?;
    let mut value = snapshot(&ws, true)?;
    value["generated_at"] = json!((Utc::now() - Duration::minutes(10)).to_rfc3339());
    write_snapshot(&ws, &value)?;
    let (path, evidence) = create(&ws, &[])?;
    unknown(&evidence);
    assert_eq!(evidence["freshness"]["state"], "stale");
    value["generated_at"] = json!((Utc::now() + Duration::minutes(10)).to_rfc3339());
    write_snapshot(&ws, &value)?;
    unknown(&resume(&ws, &path, &[])?);
    write_snapshot(&ws, &json!({"file_reservations": []}))?;
    unknown(&create(&ws, &[])?.1);
    Ok(())
}

#[test]
fn resume_rechecks_expiry_instead_of_replaying_captured_count() -> TestResult {
    let (_dir, ws) = workspace()?;
    write_snapshot(&ws, &snapshot(&ws, true)?)?;
    let (path, captured) = create(&ws, &[])?;
    verified(&captured, 1);
    write_snapshot(&ws, &snapshot(&ws, false)?)?;
    verified(&resume(&ws, &path, &[])?, 0);
    Ok(())
}

#[test]
fn reservation_details_are_bounded_redacted_and_carry_observation_time() -> TestResult {
    let (_dir, ws) = workspace()?;
    let mut value = snapshot(&ws, true)?;
    let row = value["file_reservations"][0].clone();
    let rows = (0..270)
        .map(|index| {
            let mut item = row.clone();
            item["path_pattern"] = json!(format!("private-{index}/**"));
            item
        })
        .collect::<Vec<_>>();
    value["file_reservations"] = json!(rows);
    value["summary"]["file_reservation_count"] = json!(270);
    write_snapshot(&ws, &value)?;
    let (_, evidence) = create(&ws, &[])?;
    verified(&evidence, 270);
    assert_eq!(
        evidence["reservations"]
            .as_array()
            .ok_or("reservation details")?
            .len(),
        256
    );
    assert_eq!(evidence["reservationDetailsTruncated"], true);
    assert!(evidence["freshness"]["observedAt"].is_string());
    for private in ["OtherAgent", "private-", ws.as_str()] {
        assert!(!evidence.to_string().contains(private));
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_explicit_snapshot_is_rejected_despite_valid_default() -> TestResult {
    let (_dir, ws) = workspace()?;
    let input = write_snapshot(&ws, &snapshot(&ws, true)?)?;
    let link = format!("{ws}/snapshot-link.json");
    std::os::unix::fs::symlink(input, &link).map_err(|error| error.to_string())?;
    unknown(&create(&ws, &["--agent-mail-snapshot", &link])?.1);
    Ok(())
}

#[test]
fn fresh_but_degraded_empty_observation_is_not_authoritative() -> TestResult {
    let (_dir, ws) = workspace()?;
    let mut value = snapshot(&ws, false)?;
    value["health_level"] = json!("yellow");
    value["fallback_active"] = json!(true);
    value["producer_status"] = json!("degraded");
    write_snapshot(&ws, &value)?;
    let (path, evidence) = create(&ws, &[])?;
    assert_eq!(evidence["freshness"]["state"], "current");
    unknown(&evidence);
    unknown(&resume(&ws, &path, &[])?);
    Ok(())
}
