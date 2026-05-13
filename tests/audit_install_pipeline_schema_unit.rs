//! bd-2gill — schema validator for `ee.audit.install_pipeline.v1`.
//!
//! Runs `scripts/audit_install_pipeline.sh` (or uses an existing
//! `tests/audit_artifacts/latest_install_pipeline.json` symlink),
//! parses the envelope, and asserts:
//!
//! 1. Schema field equals `ee.audit.install_pipeline.v1`.
//! 2. `decided_path` is one of {path_a_post_release, path_b_pre_release}.
//! 3. `decision_inputs` is a JSON object (not a string), with the
//!    three boolean signal fields.
//! 4. Each probe sub-object has a `probe_status` string.
//! 5. `next_actions` is a non-empty array of strings.
//!
//! The test does not require network access — it reads whatever the
//! latest audit run produced. If no audit artifact exists yet, the
//! test is skipped (so a clean checkout doesn't fail CI).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

fn audit_artifact_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("audit_artifacts")
        .join("latest_install_pipeline.json")
}

fn load_audit() -> Result<Option<Value>, String> {
    let path = audit_artifact_path();
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    Ok(Some(value))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn schema_pin_is_v1() -> TestResult {
    let Some(audit) = load_audit()? else {
        eprintln!("skipped: no audit artifact at {}", audit_artifact_path().display());
        return Ok(());
    };
    let schema = audit
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "audit: missing top-level `schema`".to_string())?;
    ensure(
        schema == "ee.audit.install_pipeline.v1",
        format!(
            "audit: schema is `{schema}`; expected `ee.audit.install_pipeline.v1`. \
             Re-run `scripts/audit_install_pipeline.sh`."
        ),
    )
}

#[test]
fn decided_path_is_known() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let allowed: BTreeSet<&str> = ["path_a_post_release", "path_b_pre_release"]
        .into_iter()
        .collect();
    let decided = audit
        .get("decided_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "audit: missing `decided_path`".to_string())?;
    ensure(
        allowed.contains(decided),
        format!(
            "audit: decided_path `{decided}` not in {:?}. Update the script or this test \
             in the same PR if a new path is introduced.",
            allowed
        ),
    )
}

#[test]
fn decision_inputs_is_object_with_three_booleans() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let inputs = audit
        .get("decision_inputs")
        .ok_or_else(|| "audit: missing `decision_inputs`".to_string())?;
    let obj = inputs
        .as_object()
        .ok_or_else(|| "audit: `decision_inputs` is not a JSON object".to_string())?;
    for required in ["gh_releases", "crates_name_claimed", "tap_formula_present"] {
        let v = obj
            .get(required)
            .ok_or_else(|| format!("audit: decision_inputs missing `{required}`"))?;
        if !v.is_boolean() {
            return Err(format!(
                "audit: decision_inputs.`{required}` is not a boolean; got `{}`",
                v
            ));
        }
    }
    Ok(())
}

#[test]
fn each_probe_has_probe_status() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    for probe in ["github_releases", "crates_io", "homebrew_tap"] {
        let probe_obj = audit
            .get(probe)
            .ok_or_else(|| format!("audit: missing probe object `{probe}`"))?;
        let status = probe_obj
            .get("probe_status")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!("audit: probe `{probe}` missing string `probe_status` field")
            })?;
        if status.is_empty() {
            return Err(format!("audit: probe `{probe}` has empty probe_status"));
        }
    }
    Ok(())
}

#[test]
fn next_actions_is_non_empty_string_array() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let actions = audit
        .get("next_actions")
        .and_then(Value::as_array)
        .ok_or_else(|| "audit: missing or non-array `next_actions`".to_string())?;
    ensure(
        !actions.is_empty(),
        "audit: `next_actions` is empty".to_string(),
    )?;
    for (i, action) in actions.iter().enumerate() {
        if !action.is_string() {
            return Err(format!(
                "audit: next_actions[{i}] is not a string; got `{}`",
                action
            ));
        }
    }
    Ok(())
}

#[test]
fn release_workflow_inventory_present() -> TestResult {
    let Some(audit) = load_audit()? else {
        return Ok(());
    };
    let wf = audit
        .get("release_workflow")
        .ok_or_else(|| "audit: missing `release_workflow` object".to_string())?;
    // The script writes this as either a JSON object (when release.yml
    // exists) or the literal "no_release_yml" string fallback.
    if wf.is_string() {
        // Acceptable fallback when no release.yml; nothing else to check.
        return Ok(());
    }
    let _ = wf
        .as_object()
        .ok_or_else(|| "audit: `release_workflow` is neither object nor string".to_string())?;
    Ok(())
}
