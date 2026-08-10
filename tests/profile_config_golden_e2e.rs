//! Scrubbed golden e2e coverage for `ee profile config plan`.
//!
//! The command probes the real host and a real initialized workspace, then this
//! harness canonicalizes host-specific values before comparing the public JSON
//! shape to a checked-in golden artifact.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use tempfile::{Builder as TempDirBuilder, TempDir};

type TestResult = Result<(), String>;

fn ee_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn worker_local_tempdir(prefix: &str) -> Result<TempDir, String> {
    let tmp_root = Path::new("/tmp");
    if tmp_root.is_dir() {
        TempDirBuilder::new()
            .prefix(prefix)
            .tempdir_in(tmp_root)
            .map_err(|error| format!("tempdir: {error}"))
    } else {
        TempDirBuilder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| format!("tempdir: {error}"))
    }
}

fn run_ee(workspace: &Path, args: &[&str]) -> Result<Output, String> {
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| format!("workspace path is not UTF-8: {}", workspace.display()))?;
    let mut full_args = vec!["--workspace", workspace_arg, "--json"];
    full_args.extend_from_slice(args);

    Command::new(ee_bin())
        .args(full_args)
        .env_remove("EE_WORKSPACE")
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))
}

fn parse_success_json(output: Output, label: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{label} stdout UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("{label} stderr UTF-8: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{label} failed with status {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        ));
    }
    if !stderr.is_empty() {
        return Err(format!("{label} JSON mode wrote stderr:\n{stderr}"));
    }
    serde_json::from_str(&stdout).map_err(|error| format!("{label} stdout JSON: {error}\n{stdout}"))
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(canonical_json).collect()),
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let mut canonical = Map::new();
            for (key, value) in entries {
                canonical.insert(key, canonical_json(value));
            }
            Value::Object(canonical)
        }
        scalar => scalar,
    }
}

fn pointer_clone(value: &Value, pointer: &str) -> Result<Value, String> {
    value
        .pointer(pointer)
        .cloned()
        .ok_or_else(|| format!("missing JSON pointer {pointer}"))
}

fn scrub_profile_config_plan(mut value: Value) -> Result<Value, String> {
    if let Some(data) = value.get_mut("data").and_then(Value::as_object_mut) {
        data.insert(
            "configPath".to_string(),
            json!("[WORKSPACE]/.ee/config.toml"),
        );

        if let Some(profile) = data.get_mut("profile").and_then(Value::as_object_mut) {
            profile.insert("recommended".to_string(), json!("[HOST_PROFILE]"));
            profile.insert("confidence".to_string(), json!("[HOST_CONFIDENCE]"));
            profile.insert("reasons".to_string(), json!(["[HOST_REASON]"]));
        }

        let planned_toml = data
            .get("plannedToml")
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| "profile plan missing plannedToml".to_string())?;
        if planned_toml.parse::<toml_edit::DocumentMut>().is_err() {
            return Err("profile plan plannedToml should remain valid TOML".to_string());
        }

        scrub_probe(data.get_mut("probe").ok_or("profile plan missing probe")?);
    }

    Ok(canonical_json(value))
}

fn scrub_probe(probe: &mut Value) {
    if let Some(object) = probe.as_object_mut() {
        object.insert("complete".to_string(), json!("[BOOL]"));
        object.insert("degraded".to_string(), json!("[HOST_PROBE_DEGRADATIONS]"));

        if let Some(cpu) = object.get_mut("cpu").and_then(Value::as_object_mut) {
            cpu.insert("logicalCores".to_string(), json!("[CPU]"));
            cpu.insert("physicalCores".to_string(), json!("[CPU_OR_NULL]"));
        }

        if let Some(memory) = object.get_mut("memory").and_then(Value::as_object_mut) {
            memory.insert("totalBytes".to_string(), json!("[BYTES_OR_NULL]"));
            memory.insert("availableBytes".to_string(), json!("[BYTES_OR_NULL]"));
            memory.insert("cgroupLimitBytes".to_string(), json!("[BYTES_OR_NULL]"));
            memory.insert("source".to_string(), json!("[MEMORY_SOURCE]"));
        }

        if let Some(paths) = object.get_mut("paths").and_then(Value::as_array_mut) {
            for path in paths {
                if let Some(path) = path.as_object_mut() {
                    path.insert("exists".to_string(), json!("[BOOL_OR_NULL]"));
                    path.insert(
                        "nearestExistingAncestor".to_string(),
                        json!("[BOOL_OR_NULL]"),
                    );
                    path.insert("probeStatus".to_string(), json!("[PROBE_STATUS]"));
                    path.insert(
                        "sameFilesystemAsWorkspace".to_string(),
                        json!("[BOOL_OR_NULL]"),
                    );
                    path.insert("totalBytes".to_string(), json!("[BYTES_OR_NULL]"));
                    path.insert("availableBytes".to_string(), json!("[BYTES_OR_NULL]"));
                }
            }
        }

        if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools {
                if let Some(tool) = tool.as_object_mut() {
                    tool.insert("available".to_string(), json!("[BOOL]"));
                }
            }
        }

        if let Some(environment) = object.get_mut("environment").and_then(Value::as_object_mut) {
            environment.insert("tmpdirConfigured".to_string(), json!("[BOOL]"));
            environment.insert("cargoTargetDirConfigured".to_string(), json!("[BOOL]"));
            environment.insert("rchHintConfigured".to_string(), json!("[BOOL]"));
        }

        if let Some(workspace) = object.get_mut("workspace").and_then(Value::as_object_mut) {
            workspace.insert("initialized".to_string(), json!("[BOOL]"));
        }

        if let Some(rch) = object
            .get_mut("topology")
            .and_then(Value::as_object_mut)
            .and_then(|topology| topology.get_mut("rch"))
            .and_then(Value::as_object_mut)
        {
            rch.insert("available".to_string(), json!("[BOOL]"));
            rch.insert("status".to_string(), json!("[RCH_STATUS]"));
            rch.insert("posture".to_string(), json!("[RCH_POSTURE]"));
            rch.insert("message".to_string(), json!("[RCH_MESSAGE]"));
            rch.insert("repair".to_string(), json!("[RCH_REPAIR_OR_NULL]"));
        }
    }
}

fn assert_golden(actual: &Value) -> TestResult {
    let expected_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("profile")
        .join("profile_config_plan_scrubbed.json.golden");
    let expected_text = fs::read_to_string(&expected_path)
        .map_err(|error| format!("read {}: {error}", expected_path.display()))?;
    let expected: Value = serde_json::from_str(&expected_text)
        .map_err(|error| format!("parse {}: {error}", expected_path.display()))?;
    if &expected != actual {
        let pretty = serde_json::to_string_pretty(actual)
            .map_err(|error| format!("format actual golden: {error}"))?;
        return Err(format!(
            "scrubbed profile config plan golden changed; actual:\n{pretty}\n"
        ));
    }
    Ok(())
}

#[test]
fn profile_config_plan_portable_matches_scrubbed_golden() -> TestResult {
    let root = worker_local_tempdir("ee-profile-config-golden-")?;
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| format!("create workspace: {error}"))?;

    let init = parse_success_json(run_ee(&workspace, &["init"])?, "ee init")?;
    if pointer_clone(&init, "/success")? != json!(true) {
        return Err(format!("ee init did not report success: {init}"));
    }

    let plan = parse_success_json(
        run_ee(
            &workspace,
            &["profile", "config", "plan", "--profile", "portable"],
        )?,
        "ee profile config plan --profile portable",
    )?;
    if pointer_clone(&plan, "/success")? != json!(true) {
        return Err(format!(
            "profile config plan did not report success: {plan}"
        ));
    }

    let scrubbed = scrub_profile_config_plan(plan)?;
    eprintln!(
        "{}",
        json!({
            "schema": "ee.test_event.v1",
            "kind": "profile_config_golden_e2e",
            "phase": "golden_compare",
            "workspace": workspace.display().to_string(),
            "artifactNonce": SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| format!("clock before UNIX_EPOCH: {error}"))?
                .as_nanos()
                .to_string(),
        })
    );
    assert_golden(&scrubbed)
}
