use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn run_bash(script: &str) -> Result<String, String> {
    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("run bash helper fixture: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "bash helper fixture exited {:?}; stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("helper stdout was not UTF-8: {error}"))
}

fn parse_json_lines(stdout: &str) -> Result<Vec<serde_json::Value>, String> {
    stdout
        .lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .map_err(|error| format!("line {} must be JSON: {error}; line={line}", index + 1))
        })
        .collect()
}

fn string_at<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
    context: &str,
) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{context} missing string at {pointer}: {value}"))
}

fn assert_test_event_shape(value: &serde_json::Value, context: &str) -> TestResult {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{context} event must be an object: {value}"))?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "schema"
                | "ts"
                | "test_id"
                | "kind"
                | "command"
                | "args"
                | "stdin_hash"
                | "stdout_hash"
                | "stderr_excerpt"
                | "exit_code"
                | "elapsed_ms"
                | "fields"
        ) {
            return Err(format!(
                "{context} event uses non ee.test_event.v1 root field {key}: {value}"
            ));
        }
    }
    if string_at(value, "/schema", context)? != "ee.test_event.v1" {
        return Err(format!("{context} event schema drifted: {value}"));
    }
    string_at(value, "/ts", context)?;
    string_at(value, "/test_id", context)?;
    let kind = string_at(value, "/kind", context)?;
    if kind == "assert_ok" {
        string_at(value, "/fields/label", context)?;
    }
    if kind == "assert_fail" {
        string_at(value, "/fields/label", context)?;
        string_at(value, "/fields/expected", context)?;
        string_at(value, "/fields/actual", context)?;
    }
    for pointer in [
        "/fields/surface",
        "/fields/phase",
        "/fields/scenario",
        "/fields/status",
    ] {
        string_at(value, pointer, context)?;
    }
    Ok(())
}

fn root_mesh_scripts() -> Result<Vec<PathBuf>, String> {
    let scripts_dir = repo_root().join("scripts");
    let mut scripts = Vec::new();
    for entry in fs::read_dir(&scripts_dir)
        .map_err(|error| format!("read_dir {}: {error}", scripts_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read_dir entry: {error}"))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name.starts_with("e2e_mesh_") && name.ends_with(".sh") {
            scripts.push(path);
        }
    }
    scripts.sort();
    Ok(scripts)
}

#[test]
fn mesh_scripts_with_scheduled_events_emit_scenario_outcomes() -> TestResult {
    let mut scheduled_scripts = Vec::new();

    for script in root_mesh_scripts()? {
        let body = read(&script)?;
        let has_bare_scheduled = body.contains(r#""stage":"scheduled""#)
            || body.contains(r#""stage\": \"scheduled\""#)
            || body.contains(r#""message":"scheduled""#)
            || body.contains(r#""message\": \"scheduled\""#)
            || body.contains("mesh_e2e_emit_scheduled");
        if !has_bare_scheduled {
            continue;
        }
        scheduled_scripts.push(script.display().to_string());

        if !body.contains("scripts/lib/mesh_e2e_outcomes.sh")
            && !body.contains("lib/mesh_e2e_outcomes.sh")
        {
            return Err(format!(
                "{} schedules mesh scenarios without the outcome helper",
                script.display()
            ));
        }
        if !body.contains("mesh_e2e_run_with_outcomes") && !body.contains("mesh_e2e_emit_outcomes")
        {
            return Err(format!(
                "{} schedules mesh scenarios without per-scenario outcome emission",
                script.display()
            ));
        }
    }

    if scheduled_scripts.is_empty() {
        return Err("expected at least one scheduled mesh e2e script".to_owned());
    }

    Ok(())
}

#[test]
fn mesh_outcome_helper_emits_schema_valid_event_shape() -> TestResult {
    let stdout = run_bash(
        r#"
set -euo pipefail
source scripts/lib/mesh_e2e_outcomes.sh
export MESH_E2E_EVENT_TS=2026-05-22T00:00:00.000000Z
mesh_e2e_emit_scheduled mesh_replay_convergence event_hash_and_range_summary_are_deterministic 'cargo test --test mesh_replay_convergence'
mesh_e2e_emit_outcome mesh_replay_convergence event_hash_and_range_summary_are_deterministic pass 12.5 ''
mesh_e2e_emit_outcome mesh_replay_convergence missed_ranges_and_out_of_order_batches fail 7.25 'failed assertion'
mesh_e2e_emit_outcome mesh_replay_convergence partition_then_rejoin_converges skipped 0.0 'rch unavailable'
"#,
    )?;
    let events = parse_json_lines(&stdout)?;
    if events.len() != 4 {
        return Err(format!("expected 4 helper events, got {}", events.len()));
    }
    for (index, event) in events.iter().enumerate() {
        assert_test_event_shape(event, &format!("helper event {}", index + 1))?;
    }
    if string_at(&events[0], "/kind", "scheduled")? != "note"
        || string_at(&events[0], "/fields/status", "scheduled")? != "scheduled"
        || string_at(&events[0], "/fields/message", "scheduled")? != "scheduled"
    {
        return Err(format!(
            "scheduled event did not use note/status shape: {}",
            events[0]
        ));
    }
    if string_at(&events[1], "/kind", "pass outcome")? != "assert_ok"
        || string_at(&events[1], "/fields/status", "pass outcome")? != "pass"
    {
        return Err(format!(
            "pass outcome did not use assert_ok shape: {}",
            events[1]
        ));
    }
    if string_at(&events[2], "/kind", "fail outcome")? != "assert_fail"
        || string_at(&events[2], "/fields/status", "fail outcome")? != "fail"
        || string_at(&events[2], "/fields/expected", "fail outcome")? != "pass"
        || string_at(&events[2], "/fields/actual", "fail outcome")? != "fail"
    {
        return Err(format!(
            "fail outcome did not use assert_fail shape: {}",
            events[2]
        ));
    }
    if string_at(&events[3], "/kind", "skipped outcome")? != "note"
        || string_at(&events[3], "/fields/status", "skipped outcome")? != "skipped"
        || string_at(&events[3], "/fields/stderr_tail", "skipped outcome")? != "rch unavailable"
    {
        return Err(format!(
            "skipped outcome did not preserve stderr tail: {}",
            events[3]
        ));
    }
    Ok(())
}

#[test]
fn mesh_replay_outcomes_converge_with_fixed_event_clock() -> TestResult {
    let fixture = r#"
set -euo pipefail
source scripts/lib/mesh_e2e_outcomes.sh
export MESH_E2E_EVENT_TS=2026-05-22T00:00:00.000000Z
export MESH_E2E_DURATION_MS_OVERRIDE=0.0
scenarios=(
  event_hash_and_range_summary_are_deterministic
  missed_ranges_and_out_of_order_batches
  partition_then_rejoin_converges
  conflicting_revisions_are_explicit
  tombstone_and_validity_propagate
  peer_restart_rehydrates_durable_log
)
for scenario in "${scenarios[@]}"; do
  mesh_e2e_emit_scheduled mesh_replay_convergence "$scenario"
done
mesh_e2e_run_with_outcomes mesh_replay_convergence "${scenarios[@]}" -- true
"#;
    let first = run_bash(fixture)?;
    let second = run_bash(fixture)?;
    if first != second {
        return Err(format!(
            "fixed-clock mesh replay outcome JSON drifted\nfirst:\n{first}\nsecond:\n{second}"
        ));
    }
    let events = parse_json_lines(&first)?;
    if events.len() != 12 {
        return Err(format!(
            "expected 6 scheduled + 6 outcome events, got {}",
            events.len()
        ));
    }
    for (index, event) in events.iter().enumerate() {
        assert_test_event_shape(event, &format!("replay convergence event {}", index + 1))?;
    }
    Ok(())
}
