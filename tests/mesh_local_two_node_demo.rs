//! bd-ghey6: local two-node mesh demo fixture without real Tailscale.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path() -> PathBuf {
    repo_root().join("scripts/e2e_overhaul/mesh_local_two_node_demo.sh")
}

fn golden_path() -> PathBuf {
    repo_root().join("tests/fixtures/golden/mesh/local_two_node_demo.json")
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

fn read_json(path: &Path) -> Result<Value, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn collect_mesh_phases(log_path: &Path) -> Result<(Vec<String>, bool, Option<String>), String> {
    let contents = fs::read_to_string(log_path)
        .map_err(|error| format!("read {}: {error}", log_path.display()))?;
    let mut phases = Vec::new();
    let mut saw_assert_fail = false;
    let mut final_fail_count = None;

    for (line_index, line) in contents.lines().enumerate() {
        let event: Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "{}:{}: malformed ee.test_event.v1 row: {error}\n{line}",
                log_path.display(),
                line_index + 1
            )
        })?;
        ensure(
            event.get("schema").and_then(Value::as_str) == Some("ee.test_event.v1"),
            format!(
                "{}:{}: missing ee.test_event.v1 schema",
                log_path.display(),
                line_index + 1
            ),
        )?;
        if event.get("kind").and_then(Value::as_str) == Some("assert_fail") {
            saw_assert_fail = true;
        }
        let fields = event.get("fields").and_then(Value::as_object);
        if fields
            .and_then(|fields| fields.get("meshScenario"))
            .and_then(Value::as_str)
            == Some("mesh_local_two_node_demo")
        {
            if let Some(phase) = fields
                .and_then(|fields| fields.get("phase"))
                .and_then(Value::as_str)
            {
                phases.push(phase.to_owned());
            }
        }
        if fields
            .and_then(|fields| fields.get("message"))
            .and_then(Value::as_str)
            == Some("test_end: mesh_local_two_node_demo")
        {
            final_fail_count = fields
                .and_then(|fields| fields.get("asserts_fail"))
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
    }

    Ok((phases, saw_assert_fail, final_fail_count))
}

#[test]
fn local_two_node_demo_script_is_non_networked_and_structured() -> TestResult {
    let script = script_path();
    let metadata = fs::metadata(&script)
        .map_err(|error| format!("expected {} to exist: {error}", script.display()))?;
    ensure(metadata.is_file(), "demo script must be a regular file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure(
            metadata.permissions().mode() & 0o111 != 0,
            format!(
                "demo script must be executable: mode={:o}",
                metadata.permissions().mode()
            ),
        )?;
    }

    let body = fs::read_to_string(&script)
        .map_err(|error| format!("read {}: {error}", script.display()))?;
    ensure(
        body.lines()
            .next()
            .is_some_and(|line| line.contains("bash")),
        "demo script must use a bash shebang",
    )?;
    ensure(
        body.contains("set -euo pipefail"),
        "demo script must enable strict mode",
    )?;
    for required in [
        "bd-ghey6",
        "mesh_scenario_setup \"$SCENARIO\" 2",
        "local_file",
        "tier1DoesNotContactNetwork",
        "lazyBodyRequiresPolicyGrant",
        "peerUnavailableKeepsForegroundUsable",
        "e2e_log_golden_compare",
    ] {
        ensure(
            body.contains(required),
            format!("demo script missing required marker {required:?}"),
        )?;
    }
    for forbidden in [
        "tailscale ",
        "curl ",
        "nc ",
        "ssh ",
        "http://",
        "https://",
        "cargo ",
        "git reset",
        "git clean",
        "rm -rf",
        "--no-verify",
        "--force",
    ] {
        ensure(
            !body.contains(forbidden),
            format!("demo script contains forbidden token {forbidden:?}"),
        )?;
    }

    Ok(())
}

#[test]
fn local_two_node_demo_golden_pins_mesh_semantics() -> TestResult {
    let golden = read_json(&golden_path())?;
    ensure(
        golden.get("schema").and_then(Value::as_str) == Some("ee.mesh.local_two_node_demo.v1"),
        "golden must declare the local two-node demo schema",
    )?;
    ensure(
        golden.pointer("/transport/externalNetworkRequired") == Some(&Value::Bool(false)),
        "golden must prove no external network requirement",
    )?;
    ensure(
        golden.pointer("/transport/tailscaleAccountRequired") == Some(&Value::Bool(false)),
        "golden must prove no Tailscale account requirement",
    )?;
    let steps = golden
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| "golden missing steps array".to_owned())?;
    let phases: Vec<&str> = steps
        .iter()
        .filter_map(|step| step.get("phase").and_then(Value::as_str))
        .collect();
    for required in [
        "remember",
        "sync_metadata",
        "tier1_search",
        "lazy_body_fetch",
        "revision_available",
        "peer_unavailable",
    ] {
        ensure(
            phases.contains(&required),
            format!("golden missing phase {required:?}; phases={phases:?}"),
        )?;
    }
    for invariant in [
        "/invariants/tier1DoesNotContactNetwork",
        "/invariants/eagerSyncIsMetadataOnly",
        "/invariants/lazyBodyRequiresPolicyGrant",
        "/invariants/fresherPeerRevisionIsNoticeOnly",
        "/invariants/peerUnavailableKeepsForegroundUsable",
    ] {
        ensure(
            golden.pointer(invariant) == Some(&Value::Bool(true)),
            format!("golden invariant {invariant} must be true"),
        )?;
    }
    ensure(
        golden.pointer("/structuredLog/emitsRawMemoryBodies") == Some(&Value::Bool(false)),
        "structured log contract must forbid raw memory bodies",
    )?;
    ensure(
        golden.pointer("/structuredLog/emitsPeerSecrets") == Some(&Value::Bool(false)),
        "structured log contract must forbid peer secrets",
    )
}

#[test]
fn local_two_node_demo_shell_driver_matches_golden_and_logs_phases() -> TestResult {
    let script = script_path();
    let log_path = unique_tmp_path("mesh-local-two-node-demo", "jsonl")?;
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(repo_root())
        .env("EE_TEST_LOG_PATH", &log_path)
        .env("EE_TEST_LOG_LEVEL", "normal")
        .env("EE_E2E_TMPDIR", "/tmp")
        .env("TMPDIR", "/tmp")
        .env("EE_BINARY", "/bin/true")
        .output()
        .map_err(|error| format!("failed to run {}: {error}", script.display()))?;
    if !output.status.success() {
        return Err(format!(
            "demo script exited {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let actual: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("demo stdout was not JSON: {error}"))?;
    let expected = read_json(&golden_path())?;
    ensure(
        actual == expected,
        format!(
            "demo stdout drifted from golden\nexpected:\n{}\nactual:\n{}",
            serde_json::to_string_pretty(&expected).unwrap_or_else(|_| expected.to_string()),
            serde_json::to_string_pretty(&actual).unwrap_or_else(|_| actual.to_string()),
        ),
    )?;

    let (phases, saw_assert_fail, final_fail_count) = collect_mesh_phases(&log_path)?;
    ensure(!saw_assert_fail, "demo log emitted assert_fail")?;
    ensure(
        final_fail_count.as_deref() == Some("0"),
        format!("demo log reported non-zero assertion failures: {final_fail_count:?}"),
    )?;
    for required in ["setup", "action", "assert", "cleanup"] {
        ensure(
            phases.iter().any(|phase| phase == required),
            format!("demo log missing phase {required:?}; phases={phases:?}"),
        )?;
    }

    Ok(())
}
