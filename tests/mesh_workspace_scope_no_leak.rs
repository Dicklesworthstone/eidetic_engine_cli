//! bd-2jb3s.4: two-workspace mesh scope no-leak shell proof.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult = Result<(), String>;

fn unique_tmp_path(label: &str, extension: &str) -> PathBuf {
    PathBuf::from(format!(
        "/tmp/ee-{label}-{}-{extension}",
        std::process::id()
    ))
}

fn event_fields(event: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    event
        .get("fields")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn assert_mesh_scope_log(log_path: &Path) -> TestResult {
    let contents = fs::read_to_string(log_path)
        .map_err(|error| format!("failed to read {}: {error}", log_path.display()))?;
    let mut saw_schema = false;
    let mut saw_test_end = false;
    let mut saw_assert_fail = false;
    let mut final_assert_fail_count = None;
    let mut workspace_a_allowed = false;
    let mut workspace_b_denied = false;
    let mut checked_no_leak_labels = 0usize;

    for (line_index, line) in contents.lines().enumerate() {
        let event: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "{}:{}: malformed test event JSON: {error}\n{line}",
                log_path.display(),
                line_index + 1
            )
        })?;
        if event.get("schema").and_then(serde_json::Value::as_str) == Some("ee.test_event.v1") {
            saw_schema = true;
        }
        if event.get("kind").and_then(serde_json::Value::as_str) == Some("assert_fail") {
            saw_assert_fail = true;
        }
        let fields = event_fields(&event);
        if event.get("kind").and_then(serde_json::Value::as_str) == Some("mesh_scope_decision") {
            for required in [
                "workspace_scope_decision",
                "workspace_id",
                "origin_workspace_id",
                "peer_group_id",
                "producer_peer_id",
                "material_lane",
                "allowed",
                "reason",
            ] {
                if fields
                    .get(required)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .is_empty()
                {
                    return Err(format!("mesh_scope_decision missing {required}: {event}"));
                }
            }
            let workspace_id = fields
                .get("workspace_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let decision = fields
                .get("workspace_scope_decision")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let allowed = fields
                .get("allowed")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let lane = fields
                .get("material_lane")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if workspace_id == "wsp_local_release_a_001" && decision == "allow" && allowed == "true"
            {
                workspace_a_allowed = true;
            }
            if workspace_id == "wsp_local_release_b_001"
                && lane == "body"
                && decision == "deny"
                && allowed == "false"
            {
                workspace_b_denied = true;
            }
        }
        if event.get("kind").and_then(serde_json::Value::as_str) == Some("assert_ok") {
            if fields
                .get("label")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|label| {
                    label.starts_with("mesh_scope_workspace_b_") && label.ends_with("_absent")
                })
            {
                checked_no_leak_labels += 1;
            }
        }
        if fields
            .get("message")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message == "test_end: mesh_workspace_scope_no_leak")
        {
            saw_test_end = true;
            final_assert_fail_count = fields
                .get("asserts_fail")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
        }
    }

    if !saw_schema {
        return Err("shell driver log did not emit ee.test_event.v1 events".to_owned());
    }
    if saw_assert_fail {
        return Err("shell driver emitted at least one assert_fail event".to_owned());
    }
    if !saw_test_end {
        return Err("shell driver log did not emit test_end note".to_owned());
    }
    if final_assert_fail_count.as_deref() != Some("0") {
        return Err(format!(
            "shell driver reported non-zero assertion failures: {final_assert_fail_count:?}"
        ));
    }
    if !workspace_a_allowed {
        return Err("missing workspace A allowed mesh-scope decision event".to_owned());
    }
    if !workspace_b_denied {
        return Err("missing workspace B denied body mesh-scope decision event".to_owned());
    }
    if checked_no_leak_labels < 10 {
        return Err(format!(
            "expected broad workspace B no-leak assertions, saw {checked_no_leak_labels}"
        ));
    }
    Ok(())
}

#[test]
fn mesh_workspace_scope_shell_driver_proves_no_leak() -> TestResult {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = repo_root.join("scripts/e2e_overhaul/mesh_workspace_scope_no_leak.sh");
    let log_path = unique_tmp_path("mesh-workspace-scope-no-leak", "jsonl");
    let retention_manifest = unique_tmp_path("mesh-workspace-scope-no-leak-retention", "json");
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(repo_root)
        .env("EE_BINARY", env!("CARGO_BIN_EXE_ee"))
        .env("EE_TEST_LOG_PATH", &log_path)
        .env("EE_TEST_LOG_LEVEL", "normal")
        .env("EE_E2E_KEEP_WORKSPACE", "1")
        .env("EE_E2E_KEEP_ARTIFACTS", "1")
        .env("EE_E2E_TMPDIR", "/tmp")
        .env("EE_E2E_ARTIFACT_TMPDIR", "/tmp")
        .env("EE_E2E_RETENTION_MANIFEST", &retention_manifest)
        .env("TMPDIR", "/tmp")
        .env("EE_MESH_ENABLED", "1")
        .env("EE_MESH_MODE", "cache")
        .output()
        .map_err(|error| format!("failed to run {}: {error}", script.display()))?;
    if !output.status.success() {
        return Err(format!(
            "mesh workspace scope shell driver exited {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    assert_mesh_scope_log(&log_path)
}
