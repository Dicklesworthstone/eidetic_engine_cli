#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

#[path = "support/fake_tailscale.rs"]
mod fake_tailscale;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut name_hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in name.as_bytes() {
        name_hash ^= u64::from(*byte);
        name_hash = name_hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    std::env::temp_dir().join(format!(
        "eeft-{:08x}-{}-{}",
        name_hash & 0xffff_ffff,
        std::process::id(),
        stamp % 1_000_000_000
    ))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn fake_tailscale_init_creates_scenario_dir_with_default_files() -> TestResult {
    let dir = unique_temp_dir("fake-tailscale-init");
    let scenario = fake_tailscale::FakeTailscaleScenario::builder("init").daemon_state("running");
    scenario.write_to(&dir)?;
    ensure(
        dir.join("tailscale_status.json").is_file(),
        "missing status fixture",
    )?;
    ensure(
        dir.join("control.json").is_file(),
        "missing control fixture",
    )?;
    ensure(dir.join("bin").is_dir(), "missing shim bin directory")?;
    ensure(
        dir.join("responders").is_dir(),
        "missing responder directory",
    )?;
    ensure(dir.join("events").is_dir(), "missing event directory")?;
    Ok(())
}

#[test]
fn fake_tailscale_set_self_writes_authenticated_status_json() -> TestResult {
    let dir = unique_temp_dir("fake-tailscale-self");
    let scenario = fake_tailscale::FakeTailscaleScenario::builder("self")
        .self_node("nodekey:self", "100.64.0.10", "tailnet-alpha", "ee-local")
        .platform("macos_open")
        .authenticated(true);
    scenario.write_to(&dir)?;
    let status: Value = serde_json::from_slice(
        &fs::read(dir.join("tailscale_status.json"))
            .map_err(|error| format!("read status: {error}"))?,
    )
    .map_err(|error| error.to_string())?;
    ensure(
        status["Self"]["ID"] == "nodekey:self",
        format!("bad self id: {status}"),
    )?;
    ensure(
        status["Self"]["Authenticated"] == true,
        format!("self was not authenticated: {status}"),
    )?;
    ensure(
        status["Self"]["Platform"] == "macos_open",
        format!("bad platform: {status}"),
    )?;
    Ok(())
}

#[test]
fn fake_tailscale_add_peer_includes_peer_in_status_json_peers_array() -> TestResult {
    let dir = unique_temp_dir("fake-tailscale-peer");
    let scenario = fake_tailscale::FakeTailscaleScenario::builder("healthy")
        .self_node("nodekey:self", "100.64.0.10", "tailnet-alpha", "ee-local")
        .add_peer("nodekey:b", "100.64.0.12", "peer-b", ["workspace-alpha"])
        .add_peer("nodekey:a", "100.64.0.11", "peer-a", ["workspace-alpha"])
        .add_peer("nodekey:c", "100.64.0.13", "peer-c", ["workspace-beta"])
        .remove_peer("nodekey:c")
        .set_peer_response("nodekey:b", false);
    scenario.write_to(&dir)?;
    let text = fs::read_to_string(dir.join("tailscale_status.json"))
        .map_err(|error| format!("read status: {error}"))?;
    let status: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    ensure(
        status["schema"].is_null(),
        "tailscale status fixture should mimic tailscale, not ee schema",
    )?;
    ensure(
        status["BackendState"] == "Running",
        format!("bad status: {status}"),
    )?;
    let peers = status["Peer"]
        .as_object()
        .ok_or_else(|| format!("missing Peer object: {status}"))?;
    ensure(peers.len() == 2, format!("expected two peers: {status}"))?;
    let keys = peers.keys().cloned().collect::<Vec<_>>();
    ensure(
        keys == vec!["nodekey:a".to_owned(), "nodekey:b".to_owned()],
        format!("peer keys not sorted: {keys:?}"),
    )?;
    ensure(
        peers["nodekey:b"]["Online"] == false,
        format!("peer response state not reflected: {status}"),
    )?;
    Ok(())
}

#[test]
fn fake_tailscale_swap_tailnet_updates_self_tailnet_id_and_display_name() -> TestResult {
    let dir = unique_temp_dir("fake-tailscale-tailnet-swap");
    let scenario = fake_tailscale::FakeTailscaleScenario::builder("swap")
        .swap_tailnet("tailnet-beta", "ee-local-renamed");
    scenario.write_to(&dir)?;
    let status: Value = serde_json::from_slice(
        &fs::read(dir.join("tailscale_status.json"))
            .map_err(|error| format!("read status: {error}"))?,
    )
    .map_err(|error| error.to_string())?;
    ensure(
        status["Self"]["Tailnet"] == "tailnet-beta",
        format!("bad tailnet: {status}"),
    )?;
    ensure(
        status["Self"]["TailnetName"] == "ee-local-renamed",
        format!("bad tailnet display name: {status}"),
    )?;
    Ok(())
}

#[test]
fn fake_tailscale_corrupt_status_json_produces_invalid_utf8_when_kind_is_invalid_utf8() -> TestResult
{
    let dir = unique_temp_dir("fake-tailscale-invalid-utf8");
    fake_tailscale::FakeTailscaleScenario::builder("corrupt").write_to(&dir)?;
    fake_tailscale::FakeTailscaleScenario::corrupt_status_json(&dir, "invalid_utf8")?;
    let bytes = fs::read(dir.join("tailscale_status.json"))
        .map_err(|error| format!("read status: {error}"))?;
    ensure(
        std::str::from_utf8(&bytes).is_err(),
        "invalid_utf8 corruption decoded successfully",
    )?;
    Ok(())
}

#[test]
fn fake_tailscale_with_scenario_returns_test_only_env_overrides() -> TestResult {
    let dir = unique_temp_dir("fake-tailscale-env");
    let scenario = fake_tailscale::FakeTailscaleScenario::builder("env");
    let overrides = scenario.with_scenario(&dir, |env| {
        (
            env.env_value("EE_TAILSCALE_BINARY_OVERRIDE")
                .unwrap_or("")
                .to_owned(),
            env.env_value("EE_TAILSCALE_PROBE_SOCKET_OVERRIDE")
                .unwrap_or("")
                .to_owned(),
        )
    })?;
    ensure(
        overrides.0.ends_with("bin/tailscale"),
        format!("binary override was not a shim path: {overrides:?}"),
    )?;
    ensure(
        overrides.1.ends_with("responders"),
        format!("socket override was not responder path: {overrides:?}"),
    )?;
    Ok(())
}

#[test]
fn fake_tailscale_emit_event_writes_jsonl_with_complete_schema_fields() -> TestResult {
    let dir = unique_temp_dir("fake-tailscale-event");
    let scenario = fake_tailscale::FakeTailscaleScenario::builder("event");
    scenario.with_scenario(&dir, |env| env.emit_event("probe", true, "event coverage"))??;
    scenario.with_scenario(&dir, |env| env.expect_event("probe", true))??;
    let event_text =
        fs::read_to_string(dir.join("events.jsonl")).map_err(|error| format!("{error}"))?;
    let event: Value =
        serde_json::from_str(event_text.trim()).map_err(|error| error.to_string())?;
    for field in [
        "schema",
        "kind",
        "phase",
        "valid",
        "detail",
        "workspace_id",
        "request_id",
        "bead_id",
        "surface",
        "elapsed_ms",
        "artifactHash",
    ] {
        ensure(
            !event[field].is_null(),
            format!("missing event field {field}: {event}"),
        )?;
    }
    ensure(
        event["schema"] == "ee.test_event.v1",
        format!("bad event: {event}"),
    )?;
    Ok(())
}

#[test]
fn fake_tailscale_node_keys_deterministic_across_runs_for_same_scenario_name() -> TestResult {
    let first = fake_tailscale::deterministic_node_key("healthy:self");
    let second = fake_tailscale::deterministic_node_key("healthy:self");
    let different = fake_tailscale::deterministic_node_key("other:self");
    ensure(
        first == second,
        format!("node keys drifted: {first} != {second}"),
    )?;
    ensure(
        first != different,
        "distinct scenario seeds produced same node key",
    )?;
    ensure(
        first.starts_with("nodekey:"),
        format!("bad node key prefix: {first}"),
    )?;
    Ok(())
}

#[test]
fn fake_tailscale_assert_no_invalid_events_exits_non_zero_when_any_event_invalid() -> TestResult {
    let dir = unique_temp_dir("fake-tailscale-invalid-event");
    let script = repo_root()
        .join("scripts")
        .join("e2e_overhaul")
        .join("lib")
        .join("fake_tailscale.sh");
    let output = Command::new("bash")
        .arg("-c")
        .arg(
            r#"
set -euo pipefail
source "$1"
ft_init "$2/scenario" "invalid-event"
FT_EVENT_FILE="$2/bad-events.jsonl"
ft_emit_event "probe" "false" "intentional invalid event" ""
if ft_assert_no_invalid_events "$FT_EVENT_FILE"; then
    exit 9
fi
"#,
        )
        .arg("bash")
        .arg(script)
        .arg(&dir)
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run invalid-event assertion script: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "invalid-event assertion script failed: status={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    Ok(())
}

#[test]
fn fake_tailscale_responder_returns_hello_response_with_configured_workspace_ids() -> TestResult {
    let dir = unique_temp_dir("fake-tailscale-responder");
    let script = repo_root()
        .join("scripts")
        .join("e2e_overhaul")
        .join("lib")
        .join("fake_tailscale.sh");
    let output = Command::new("bash")
        .arg("-c")
        .arg(
            r#"
set -euo pipefail
source "$1"
ft_init "$2/scenario" "responder"
ft_add_peer "nodekey:peer0001" "100.64.0.11" "peer-one" --ee_version=0.2.0 --ee_protocol=1.0 --workspace_ids=w-alpha,w-beta --tag=ee-mesh --respond=true
socket_path="$(ft_run_responder "nodekey:peer0001")"
for _ in 1 2 3 4 5; do
    [ -S "$socket_path" ] && break
    sleep 0.1
done
python3 - "$socket_path" <<'PY'
import json
import socket
import sys
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(sys.argv[1])
sock.sendall(b'{"schema":"ee.mesh.hello.request.v1","workspaceId":"w-alpha"}')
payload = json.loads(sock.recv(65536).decode("utf-8"))
assert payload["schema"] == "ee.mesh.hello.v1", payload
assert payload["accepted"] is True, payload
assert payload["workspaceIds"] == ["w-alpha", "w-beta"], payload
PY
ft_stop_responder "nodekey:peer0001"
"#,
        )
        .arg("bash")
        .arg(script)
        .arg(&dir)
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run responder script: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "responder script failed: status={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    Ok(())
}

#[test]
fn fake_tailscale_shell_self_test_emits_valid_events() -> TestResult {
    let output = Command::new("bash")
        .arg(
            repo_root()
                .join("scripts")
                .join("e2e_overhaul")
                .join("lib")
                .join("test_fake_tailscale.sh"),
        )
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run fake tailscale self-test: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "self-test failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let event_path = stdout
        .lines()
        .last()
        .ok_or_else(|| "self-test did not print event path".to_owned())?;
    let events = fs::read_to_string(event_path)
        .map_err(|error| format!("read self-test event log {event_path}: {error}"))?;
    let mut phases = std::collections::BTreeSet::new();
    for line in events.lines().filter(|line| !line.trim().is_empty()) {
        let event: Value =
            serde_json::from_str(line).map_err(|error| format!("parse event: {error}; {line}"))?;
        ensure(
            event["schema"] == "ee.test_event.v1",
            format!("bad event schema: {event}"),
        )?;
        ensure(event["valid"] == true, format!("invalid event: {event}"))?;
        phases.insert(event["phase"].as_str().unwrap_or("").to_owned());
    }
    for required in [
        "setup",
        "probe",
        "auto_enroll",
        "status",
        "hello",
        "summary",
    ] {
        ensure(
            phases.contains(required),
            format!("missing event phase {required}; phases={phases:?}"),
        )?;
    }
    Ok(())
}
