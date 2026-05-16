//! bd-x4hn7: mesh-off ordinary commands stay quiet and do not leave listeners.
//!
//! This is the Cargo-test companion to `scripts/e2e_overhaul/mesh_off_no_network.sh`
//! so remote-only RCH verification can exercise the built `ee` binary directly.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn temp_workspace() -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix("ee-mesh-off-no-network-")
        .tempdir_in("/tmp")
        .map_err(|error| format!("failed to create temp workspace under /tmp: {error}"))
}

fn run_ee(workspace: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .arg("--workspace")
        .arg(workspace)
        .env("EE_MESH_ENABLED", "0")
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn stdout_json(output: &Output, label: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn ensure_success(output: &Output, label: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{label}: ee exited {:?}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim_end(),
        ))
    }
}

fn collect_codes(value: &serde_json::Value, codes: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(code) = map.get("code").and_then(|code| code.as_str()) {
                codes.push(code.to_owned());
            }
            for child in map.values() {
                collect_codes(child, codes);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_codes(child, codes);
            }
        }
        _ => {}
    }
}

fn assert_no_mesh_degradation(json: &serde_json::Value, label: &str) -> TestResult {
    let mut codes = Vec::new();
    collect_codes(json, &mut codes);
    let mesh_codes: Vec<String> = codes
        .into_iter()
        .filter(|code| {
            let lowered = code.to_ascii_lowercase();
            lowered.contains("mesh") || lowered.contains("tailscale") || lowered.contains("peer")
        })
        .collect();
    if mesh_codes.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{label}: unexpected mesh degraded code(s): {mesh_codes:?}"
        ))
    }
}

fn assert_no_mesh_data_key(json: &serde_json::Value, label: &str) -> TestResult {
    let Some(data) = json.get("data").and_then(|data| data.as_object()) else {
        return Ok(());
    };
    let mesh_keys: Vec<&String> = data
        .keys()
        .filter(|key| {
            let lowered = key.to_ascii_lowercase();
            lowered.contains("mesh") || lowered.contains("tailscale") || lowered.contains("peer")
        })
        .collect();
    if mesh_keys.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{label}: unexpected mesh data key(s): {mesh_keys:?}"
        ))
    }
}

fn mesh_or_peer_codes(json: &serde_json::Value, label: &str) -> Vec<String> {
    if label == "status" {
        return Vec::new();
    }
    let mut codes = Vec::new();
    collect_codes(json, &mut codes);
    codes.retain(|code| {
        let lowered = code.to_ascii_lowercase();
        lowered.contains("mesh") || lowered.contains("tailscale") || lowered.contains("peer")
    });
    codes.sort();
    codes.dedup();
    codes
}

fn mesh_or_peer_data_keys(json: &serde_json::Value, label: &str) -> Vec<String> {
    if label == "status" {
        return Vec::new();
    }
    let Some(data) = json.get("data").and_then(|data| data.as_object()) else {
        return Vec::new();
    };
    let mut keys: Vec<String> = data
        .keys()
        .filter(|key| {
            let lowered = key.to_ascii_lowercase();
            lowered.contains("mesh") || lowered.contains("tailscale") || lowered.contains("peer")
        })
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn command_surface(label: &str, json: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "label": label,
        "schema": json.get("schema").and_then(|value| value.as_str()).unwrap_or("<missing>"),
        "success": json.get("success").and_then(|value| value.as_bool()).unwrap_or(false),
        "meshCapability": if json.pointer("/data/capabilities/mesh").is_some() {
            "present"
        } else {
            "absent"
        },
        "meshOrPeerCodes": mesh_or_peer_codes(json, label),
        "meshOrPeerDataKeys": mesh_or_peer_data_keys(json, label),
    })
}

fn assert_golden_surfaces(surfaces: &[serde_json::Value]) -> TestResult {
    let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden/mesh/mesh_off_no_network.commands.json.golden");
    let golden = fs::read_to_string(&golden_path)
        .map_err(|error| format!("failed to read {}: {error}", golden_path.display()))?;
    let expected: serde_json::Value = serde_json::from_str(&golden)
        .map_err(|error| format!("failed to parse {}: {error}", golden_path.display()))?;
    let actual = serde_json::Value::Array(surfaces.to_vec());
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "mesh-off normalized command surface drifted\nexpected: {}\nactual: {}",
            serde_json::to_string_pretty(&expected).unwrap_or_else(|_| expected.to_string()),
            serde_json::to_string_pretty(&actual).unwrap_or_else(|_| actual.to_string()),
        ))
    }
}

fn assert_byte_stable(workspace: &str, label: &str, args: &[&str]) -> TestResult {
    let first = run_ee(workspace, args)?;
    ensure_success(&first, label)?;
    let second = run_ee(workspace, args)?;
    ensure_success(&second, label)?;
    if first.stdout == second.stdout {
        Ok(())
    } else {
        Err(format!("{label}: mesh-off JSON output was not byte-stable"))
    }
}

fn unique_tmp_path(label: &str, extension: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before Unix epoch: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(format!(
        "/tmp/ee-{label}-{}-{nanos}.{extension}",
        std::process::id()
    )))
}

fn assert_mesh_log_phases(log_path: &std::path::Path) -> TestResult {
    let contents = fs::read_to_string(log_path)
        .map_err(|error| format!("failed to read {}: {error}", log_path.display()))?;
    let mut phases = Vec::new();
    let mut saw_schema = false;
    let mut saw_test_end = false;
    let mut saw_assert_fail = false;
    let mut final_assert_fail_count = None;

    for (line_index, line) in contents.lines().enumerate() {
        let event: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "{}:{}: malformed test event JSON: {error}\n{line}",
                log_path.display(),
                line_index + 1
            )
        })?;
        if event.get("schema").and_then(|value| value.as_str()) == Some("ee.test_event.v1") {
            saw_schema = true;
        }
        if event.get("kind").and_then(|value| value.as_str()) == Some("assert_fail") {
            saw_assert_fail = true;
        }
        let fields = event
            .get("fields")
            .and_then(|value| value.as_object())
            .cloned()
            .unwrap_or_default();
        if fields.get("meshScenario").and_then(|value| value.as_str())
            == Some("mesh_off_no_network")
        {
            if let Some(phase) = fields.get("phase").and_then(|value| value.as_str()) {
                phases.push(phase.to_owned());
            }
        }
        if fields
            .get("message")
            .and_then(|value| value.as_str())
            .is_some_and(|message| message == "test_end: mesh_off_no_network")
        {
            saw_test_end = true;
            final_assert_fail_count = fields
                .get("asserts_fail")
                .and_then(|value| value.as_str())
                .map(str::to_owned);
        }
    }

    if !saw_schema {
        return Err("shell driver log did not emit ee.test_event.v1 events".to_owned());
    }
    if saw_assert_fail {
        return Err("shell driver log emitted at least one assert_fail event".to_owned());
    }
    if !saw_test_end {
        return Err("shell driver log did not emit test_end note".to_owned());
    }
    if final_assert_fail_count.as_deref() != Some("0") {
        return Err(format!(
            "shell driver reported non-zero assertion failures: {final_assert_fail_count:?}"
        ));
    }
    for required in ["setup", "action", "assert", "cleanup"] {
        if !phases.iter().any(|phase| phase == required) {
            return Err(format!(
                "shell driver log missing {required:?} phase; phases={phases:?}"
            ));
        }
    }
    let phase_index = |required: &str| {
        phases
            .iter()
            .position(|phase| phase == required)
            .ok_or_else(|| format!("missing phase {required}"))
    };
    let setup = phase_index("setup")?;
    let action = phase_index("action")?;
    let assert = phase_index("assert")?;
    let cleanup = phase_index("cleanup")?;
    if setup <= action && action <= assert && assert <= cleanup {
        Ok(())
    } else {
        Err(format!(
            "shell driver phases out of order; phases={phases:?}"
        ))
    }
}

fn lsof_program() -> Option<&'static str> {
    if Command::new("sh")
        .args(["-c", "command -v lsof >/dev/null 2>&1"])
        .status()
        .ok()
        .is_some_and(|status| status.success())
    {
        Some("lsof")
    } else if std::path::Path::new("/usr/sbin/lsof").is_file() {
        Some("/usr/sbin/lsof")
    } else {
        None
    }
}

fn listener_snapshot() -> Result<Option<BTreeSet<String>>, String> {
    let Some(program) = lsof_program() else {
        return Ok(None);
    };
    let output = Command::new(program)
        .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
        .output()
        .map_err(|error| format!("failed to run {program}: {error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{program} stdout was not UTF-8: {error}"))?;
    let listeners = stdout
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let command = fields.next()?;
            let pid = fields.next()?;
            let address = line.split_whitespace().last()?;
            Some(format!("{command} {pid} {address}"))
        })
        .collect();
    Ok(Some(listeners))
}

fn assert_no_new_mesh_listener(
    before: &Option<BTreeSet<String>>,
    after: &Option<BTreeSet<String>>,
) -> TestResult {
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(());
    };
    let new_mesh: Vec<String> = after
        .difference(before)
        .filter(|line| {
            let lowered = line.to_ascii_lowercase();
            lowered.contains("ee")
                || lowered.contains("eidetic")
                || lowered.contains("mesh")
                || lowered.contains("tailscale")
        })
        .cloned()
        .collect();
    if new_mesh.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "mesh-off status left unexpected listener(s): {new_mesh:?}"
        ))
    }
}

#[test]
fn mesh_off_shell_driver_emits_structured_log() -> TestResult {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = repo_root.join("scripts/e2e_overhaul/mesh_off_no_network.sh");
    let log_path = unique_tmp_path("mesh-off-no-network", "jsonl")?;
    let retention_manifest = unique_tmp_path("mesh-off-no-network-retention", "json")?;
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
        .env("EE_MESH_ENABLED", "0")
        .output()
        .map_err(|error| format!("failed to run {}: {error}", script.display()))?;
    if !output.status.success() {
        return Err(format!(
            "mesh-off shell driver exited {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    assert_mesh_log_phases(&log_path)
}

#[test]
fn mesh_off_status_reports_capability_without_polluting_ordinary_json() -> TestResult {
    let workspace_dir = temp_workspace()?;
    let workspace = workspace_dir.path().to_string_lossy().into_owned();

    let init = run_ee(&workspace, &["init", "--json"])?;
    ensure_success(&init, "init")?;
    let init_json = stdout_json(&init, "init")?;

    let status = run_ee(&workspace, &["status", "--json"])?;
    ensure_success(&status, "status")?;
    let status_json = stdout_json(&status, "status")?;
    assert_byte_stable(&workspace, "status byte stability", &["status", "--json"])?;
    let posture = status_json
        .pointer("/data/capabilities/mesh")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "status: missing data.capabilities.mesh".to_owned())?;
    if !matches!(
        posture,
        "disabled" | "pending" | "unavailable" | "degraded" | "ok"
    ) {
        return Err(format!(
            "status: unexpected mesh capability posture {posture:?}"
        ));
    }

    let mut surfaces = vec![
        command_surface("init", &init_json),
        command_surface("status", &status_json),
    ];

    let cases: [(&str, Vec<&str>); 3] = [
        (
            "remember",
            vec![
                "remember",
                "--level",
                "procedural",
                "--kind",
                "rule",
                "Mesh-off e2e ordinary command fixture.",
                "--json",
            ],
        ),
        (
            "search",
            vec!["search", "mesh-off ordinary command fixture", "--json"],
        ),
        (
            "context",
            vec![
                "context",
                "mesh-off ordinary command fixture",
                "--max-tokens",
                "500",
                "--json",
            ],
        ),
    ];

    let mut memory_id = String::new();
    for (label, args) in cases {
        let output = run_ee(&workspace, &args)?;
        ensure_success(&output, label)?;
        let json = stdout_json(&output, label)?;
        if label == "remember" {
            memory_id = json
                .pointer("/data/memory_id")
                .and_then(|value| value.as_str())
                .ok_or_else(|| "remember: missing data.memory_id".to_owned())?
                .to_owned();
        }
        assert_no_mesh_degradation(&json, label)?;
        assert_no_mesh_data_key(&json, label)?;
        surfaces.push(command_surface(label, &json));
    }

    for (label, args) in [
        (
            "pack",
            vec![
                "pack",
                "mesh-off ordinary command fixture",
                "--max-tokens",
                "500",
                "--json",
            ],
        ),
        ("why", vec!["why", memory_id.as_str(), "--json"]),
    ] {
        let output = run_ee(&workspace, &args)?;
        ensure_success(&output, label)?;
        let json = stdout_json(&output, label)?;
        assert_no_mesh_degradation(&json, label)?;
        assert_no_mesh_data_key(&json, label)?;
        surfaces.push(command_surface(label, &json));
    }
    assert_golden_surfaces(&surfaces)?;

    let before = listener_snapshot()?;
    let status = run_ee(&workspace, &["status", "--json"])?;
    ensure_success(&status, "status listener probe")?;
    let after = listener_snapshot()?;
    assert_no_new_mesh_listener(&before, &after)
}
