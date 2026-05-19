//! Logged e2e harness coverage for `scripts/e2e_overhaul/workspace_hygiene.sh`.
//!
//! The script owns the broad scenario matrix and artifact contracts for
//! bd-1eq3l.8. This test wires its no-build self-test path into the Rust test
//! graph and independently inspects the emitted `ee.test_event.v1` evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

const EXPECTED_SCENARIOS: &[&str] = &[
    "clean",
    "source_and_test",
    "human_source_and_test",
    "human_secret_no_leak",
    "scratch_only",
    "generated_only",
    "scratch_generated_secret",
    "large_binary_scan_skip",
    "active_reservation",
    "agent_mail_empty_snapshot",
    "agent_mail_unavailable",
    "beads_pending_flush",
    "beads_export_only",
    "beads_parse_failure",
];

const REQUIRED_PHASES: &[&str] = &[
    "setup",
    "scenario_plan",
    "schema_validation",
    "scenario",
    "redaction_check",
    "artifact_redaction_check",
    "stdout_stderr_isolation",
    "artifact_reference_contract",
    "mutation_artifact_contract",
    "local_cargo_guard",
    "mutation_check",
    "teardown",
    "negative_contract_check",
    "schema_check",
];

#[test]
fn workspace_hygiene_logged_e2e_self_test_emits_complete_evidence() -> TestResult {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let event_root = temp_dir("ee-wh-logged-events")?;
    let work_root = temp_dir("ee-wh-logged-work")?;
    let script = repo.join("scripts/e2e_overhaul/workspace_hygiene.sh");

    let output = Command::new("bash")
        .arg(&script)
        .arg("--self-test-contracts")
        .current_dir(repo)
        .env("EE_WORKSPACE_HYGIENE_EVENT_DIR", &event_root)
        .env("EE_WORKSPACE_HYGIENE_TMPROOT", &work_root)
        .env("TMPDIR", &work_root)
        .env_remove("EE_BINARY")
        .env_remove("EE_BIN")
        .output()
        .map_err(|error| format!("run {}: {error}", script.display()))?;
    ensure_success(&output, "workspace hygiene logged self-test")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.contains("workspace_hygiene: self-test contracts passed") {
        return Err(format!(
            "self-test did not print success summary; stderr:\n{stderr}"
        ));
    }

    let events_path = event_root.join("events.jsonl");
    let events = read_events(&events_path)?;
    if events.is_empty() {
        return Err(format!("event log was empty: {}", events_path.display()));
    }

    assert_required_phases(&events)?;
    assert_expected_scenario_matrix(&events)?;
    assert_all_events_are_successful(&events)?;
    assert_sanitized_env_shape(&events)?;
    assert_no_local_cargo_commands(&events)?;
    assert_artifacts_exist_inside_event_root(&events, &event_root)?;
    assert_scenario_mutation_artifacts_are_hash_linked(&events)?;
    assert_no_raw_synthetic_secret(&events_path, &events)?;
    Ok(())
}

fn temp_dir(prefix: &str) -> Result<PathBuf, String> {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .map_err(|error| format!("create tempdir: {error}"))
        .map(tempfile::TempDir::keep)
}

fn ensure_success(output: &Output, context: &str) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{context}: exit {:?}; stdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn read_events(path: &Path) -> Result<Vec<Value>, String> {
    let body =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    body.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<Value>(line)
                .map_err(|error| format!("parse event line {}: {error}; line={line}", index + 1))
        })
        .collect()
}

fn assert_required_phases(events: &[Value]) -> TestResult {
    let phases = events
        .iter()
        .filter_map(|event| event.get("phase").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    for expected in REQUIRED_PHASES {
        if !phases.contains(expected) {
            return Err(format!(
                "missing logged e2e phase {expected}; got {phases:?}"
            ));
        }
    }
    Ok(())
}

fn assert_expected_scenario_matrix(events: &[Value]) -> TestResult {
    let scenario_events = events
        .iter()
        .filter(|event| event.get("phase").and_then(Value::as_str) == Some("scenario"))
        .collect::<Vec<_>>();
    let scenarios = scenario_events
        .iter()
        .filter_map(|event| event.get("scenario").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    let expected = EXPECTED_SCENARIOS.iter().copied().collect::<BTreeSet<_>>();
    if scenarios != expected {
        return Err(format!(
            "logged e2e scenario matrix mismatch; expected {expected:?}; got {scenarios:?}"
        ));
    }

    let schema_validation = events
        .iter()
        .filter(|event| event.get("phase").and_then(Value::as_str) == Some("schema_validation"))
        .filter_map(|event| event.get("scenario").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    if schema_validation != expected {
        return Err(format!(
            "schema validation matrix mismatch; expected {expected:?}; got {schema_validation:?}"
        ));
    }
    Ok(())
}

fn assert_all_events_are_successful(events: &[Value]) -> TestResult {
    for event in events {
        if event.get("schema").and_then(Value::as_str) != Some("ee.test_event.v1") {
            return Err(format!("unexpected test-event schema: {event}"));
        }
        if event.get("beadId").and_then(Value::as_str) != Some("bd-1eq3l.8") {
            return Err(format!("unexpected bead id in event: {event}"));
        }
        if event.get("surface").and_then(Value::as_str) != Some("workspace_hygiene") {
            return Err(format!("unexpected surface in event: {event}"));
        }
        if event.get("status").and_then(Value::as_str) != Some("pass") {
            return Err(format!(
                "logged self-test should only leave passing events: {event}"
            ));
        }
        if event.get("exitCode").and_then(Value::as_u64) != Some(0) {
            return Err(format!("passing event must have exitCode 0: {event}"));
        }
        let Some(elapsed_ms) = event.get("elapsedMs").and_then(Value::as_u64) else {
            return Err(format!("event missing integer elapsedMs: {event}"));
        };
        if elapsed_ms > 3_600_000 {
            return Err(format!("event elapsedMs is implausibly large: {event}"));
        }
    }
    Ok(())
}

fn assert_sanitized_env_shape(events: &[Value]) -> TestResult {
    let expected_keys = ["cargoTargetDir", "eeBinary", "tmpRoot", "tmpdir"]
        .into_iter()
        .collect::<BTreeSet<_>>();
    for event in events {
        let env = event
            .get("sanitizedEnv")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("event missing sanitizedEnv object: {event}"))?;
        let keys = env.keys().map(String::as_str).collect::<BTreeSet<_>>();
        if keys != expected_keys {
            return Err(format!(
                "sanitizedEnv keys drifted; expected {expected_keys:?}; got {keys:?}; event={event}"
            ));
        }
        let tmp_root = env
            .get("tmpRoot")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("sanitizedEnv.tmpRoot must be a string: {event}"))?;
        if tmp_root.trim().is_empty() {
            return Err(format!("sanitizedEnv.tmpRoot is empty: {event}"));
        }
        for optional in ["cargoTargetDir", "eeBinary", "tmpdir"] {
            let value = env.get(optional).unwrap_or(&Value::Null);
            if !(value.is_null() || value.is_string()) {
                return Err(format!(
                    "sanitizedEnv.{optional} must be null or string: {event}"
                ));
            }
        }
    }
    Ok(())
}

fn assert_no_local_cargo_commands(events: &[Value]) -> TestResult {
    for event in events {
        let command = event.get("command").and_then(Value::as_str).unwrap_or("");
        if command_bears_local_cargo(command) {
            return Err(format!(
                "logged event contains local Cargo command: {event}"
            ));
        }
    }
    Ok(())
}

fn command_bears_local_cargo(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let tokens = lower
        .split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, ';' | '&' | '|' | '(' | ')'))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    tokens
        .iter()
        .any(|token| matches!(*token, "cargo" | "rustc" | "rustdoc"))
}

fn assert_artifacts_exist_inside_event_root(events: &[Value], event_root: &Path) -> TestResult {
    let event_root = event_root
        .canonicalize()
        .map_err(|error| format!("canonicalize {}: {error}", event_root.display()))?;
    let mut artifact_count = 0usize;
    for event in events {
        for field in [
            "stdoutArtifact",
            "stderrArtifact",
            "beforeMutationArtifact",
            "afterMutationArtifact",
        ] {
            let Some(path) = event.get(field).and_then(Value::as_str) else {
                continue;
            };
            artifact_count += 1;
            let path = Path::new(path);
            let metadata = fs::symlink_metadata(path)
                .map_err(|error| format!("stat {field} artifact {}: {error}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "{field} artifact must be a regular non-symlink file: {}",
                    path.display()
                ));
            }
            let parent = path
                .parent()
                .ok_or_else(|| format!("{field} artifact has no parent: {}", path.display()))?
                .canonicalize()
                .map_err(|error| {
                    format!(
                        "canonicalize {field} artifact parent {}: {error}",
                        path.display()
                    )
                })?;
            if !parent.starts_with(&event_root) {
                return Err(format!(
                    "{field} artifact escaped event root: {} root={}",
                    path.display(),
                    event_root.display()
                ));
            }
        }
    }
    if artifact_count == 0 {
        return Err("logged e2e produced no artifact references".to_owned());
    }
    Ok(())
}

fn assert_scenario_mutation_artifacts_are_hash_linked(events: &[Value]) -> TestResult {
    let mut checked = BTreeMap::new();
    for event in events
        .iter()
        .filter(|event| event.get("phase").and_then(Value::as_str) == Some("scenario"))
    {
        let scenario = event
            .get("scenario")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("scenario event missing scenario name: {event}"))?;
        for (hash_field, artifact_field) in [
            ("beforeMutationHash", "beforeMutationArtifact"),
            ("afterMutationHash", "afterMutationArtifact"),
        ] {
            let expected_hash = event
                .get(hash_field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("scenario event missing {hash_field}: {event}"))?;
            let artifact = event
                .get(artifact_field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("scenario event missing {artifact_field}: {event}"))?;
            let actual_hash = sha256_file(Path::new(artifact))?;
            if actual_hash != expected_hash {
                return Err(format!(
                    "{scenario} {artifact_field} hash mismatch; expected {expected_hash}, got {actual_hash}"
                ));
            }
            let body = fs::read_to_string(artifact)
                .map_err(|error| format!("read mutation artifact {artifact}: {error}"))?;
            if !body.contains("## file fingerprints (path, size_bytes, mtime_seconds, sha256)") {
                return Err(format!(
                    "{scenario} {artifact_field} missing file fingerprint header"
                ));
            }
            if !body.lines().any(|line| line.split('\t').count() == 4) {
                return Err(format!(
                    "{scenario} {artifact_field} missing file fingerprint rows"
                ));
            }
        }
        checked.insert(scenario.to_owned(), true);
    }
    if checked.len() != EXPECTED_SCENARIOS.len() {
        return Err(format!(
            "checked {} scenario mutation artifacts, expected {}",
            checked.len(),
            EXPECTED_SCENARIOS.len()
        ));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let output = Command::new("shasum")
        .arg("-a")
        .arg("256")
        .arg(path)
        .output()
        .map_err(|error| format!("run shasum for {}: {error}", path.display()))?;
    ensure_success(&output, "shasum")?;
    let rendered = String::from_utf8(output.stdout)
        .map_err(|error| format!("shasum stdout was not UTF-8: {error}"))?;
    rendered
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| format!("shasum emitted no digest for {}", path.display()))
}

fn assert_no_raw_synthetic_secret(events_path: &Path, events: &[Value]) -> TestResult {
    let raw_marker = "sk-proj-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    let event_log = fs::read_to_string(events_path)
        .map_err(|error| format!("read {}: {error}", events_path.display()))?;
    if event_log.contains(raw_marker) {
        return Err("event log leaked raw synthetic secret".to_owned());
    }
    for path in event_artifact_paths(events) {
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        if body.contains(raw_marker) {
            return Err(format!(
                "artifact leaked raw synthetic secret: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn event_artifact_paths(events: &[Value]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for event in events {
        for field in [
            "stdoutArtifact",
            "stderrArtifact",
            "beforeMutationArtifact",
            "afterMutationArtifact",
        ] {
            if let Some(path) = event.get(field).and_then(Value::as_str) {
                paths.push(PathBuf::from(path));
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}
