//! Repair-safety conformance gate for agent-facing remediation hints
//! (`bd-3g4r4.5`).
//!
//! This is the regression net over the shared safety vocabulary. It
//! keeps work-packet fallback actions and high-risk failure-mode repair
//! hints machine-branchable instead of prose-only.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

type TestResult = Result<(), String>;

const HIGH_RISK_FAILURE_MODE_CODES: &[&str] = &[
    "agent_mail_semantic_readiness_failed",
    "agent_mail_unavailable",
    "beads_tracker_stale",
    "index_stale",
    "preflight_patterns_unavailable",
    "quarantine_database_unreadable",
    "rch_unavailable",
    "rch_worker_topology_blocked",
    "workspace_hygiene_agent_mail_timeout",
    "workspace_hygiene_agent_mail_unavailable",
];

const RISK_CLASSES: &[&str] = &[
    "read_only_probe",
    "idempotent_refresh",
    "mutating_local_repair",
    "mutating_external_coordination_repair",
    "approval_required_repair",
    "destructive_or_irreversible_repair",
    "unavailable_or_manual_only",
];

const NEXT_ACTIONS: &[&str] = &[
    "run_directly",
    "run_preflight_first",
    "coordinate_first",
    "ask_human",
    "manual_only",
    "policy_denied",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn tracked_paths(pattern: &str) -> Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .arg("ls-files")
        .arg(pattern)
        .current_dir(repo_root())
        .output()
        .map_err(|error| format!("spawn git ls-files {pattern}: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "git ls-files {pattern} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let mut paths = String::from_utf8(output.stdout)
        .map_err(|error| format!("decode git ls-files {pattern}: {error}"))?
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| repo_root().join(line))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn string_field<'a>(value: &'a Value, field: &str, ctx: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{ctx}: missing non-empty `{field}`"))
}

fn bool_field(value: &Value, field: &str, ctx: &str) -> Result<bool, String> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{ctx}: missing bool `{field}`"))
}

fn nullable_string_field<'a>(
    value: &'a Value,
    field: &str,
    ctx: &str,
) -> Result<Option<&'a str>, String> {
    match value.get(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.as_str())),
        Some(Value::String(_)) => Err(format!("{ctx}: `{field}` must not be empty")),
        Some(_) => Err(format!("{ctx}: `{field}` must be string or null")),
        None => Err(format!("{ctx}: missing `{field}`")),
    }
}

fn string_array_field(value: &Value, field: &str, ctx: &str) -> TestResult {
    let items = value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{ctx}: missing array `{field}`"))?;
    for (index, item) in items.iter().enumerate() {
        ensure(
            item.as_str().is_some_and(|value| !value.is_empty()),
            format!("{ctx}: `{field}[{index}]` must be a non-empty string"),
        )?;
    }
    Ok(())
}

fn command_is_mutating_external(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    command.contains("am doctor repair")
        || command.starts_with("br sync")
        || command.starts_with("br update")
        || command.starts_with("br close")
        || command.starts_with("br reopen")
        || command.starts_with("br comments add")
        || command.starts_with("rch daemon restart")
        || command.starts_with("rch workers probe")
        || command.starts_with("rch workers capabilities --refresh")
}

fn validate_repair_safety(
    safety: &Value,
    command: Option<&str>,
    manual_step: Option<&str>,
    ctx: &str,
) -> TestResult {
    let risk_class = string_field(safety, "riskClass", ctx)?;
    ensure(
        RISK_CLASSES.contains(&risk_class),
        format!("{ctx}: unknown riskClass `{risk_class}`"),
    )?;
    let preflight_command = nullable_string_field(safety, "preflightCommand", ctx)?;
    let requires_human_approval = bool_field(safety, "requiresHumanApproval", ctx)?;
    let mutates_external_state = bool_field(safety, "mutatesExternalState", ctx)?;
    let mutates_tracker_state = bool_field(safety, "mutatesTrackerState", ctx)?;
    let _privacy_class = string_field(safety, "privacyClass", ctx)?;
    let next_action = string_field(safety, "nextAction", ctx)?;
    ensure(
        NEXT_ACTIONS.contains(&next_action),
        format!("{ctx}: unknown nextAction `{next_action}`"),
    )?;
    let rule_id = string_field(safety, "ruleId", ctx)?;
    let source = string_field(safety, "source", ctx)?;
    let _reason_code = string_field(safety, "reasonCode", ctx)?;
    string_array_field(safety, "evidence", ctx)?;
    string_array_field(safety, "preconditions", ctx)?;
    ensure(
        rule_id.starts_with("repair_safety:"),
        format!("{ctx}: ruleId `{rule_id}` must start with repair_safety:"),
    )?;
    ensure(
        matches!(
            source,
            "repair_action_safety" | "work_packet_manual_fallback"
        ),
        format!("{ctx}: source `{source}` is not a repair-safety source"),
    )?;

    match risk_class {
        "read_only_probe" => {
            ensure(
                preflight_command.is_none(),
                format!("{ctx}: read_only_probe must not require preflight"),
            )?;
            ensure(
                !requires_human_approval && !mutates_external_state && !mutates_tracker_state,
                format!("{ctx}: read_only_probe must be non-mutating"),
            )?;
            ensure(
                next_action == "run_directly",
                format!("{ctx}: read_only_probe nextAction must be run_directly"),
            )?;
        }
        "idempotent_refresh" => {
            ensure(
                !requires_human_approval && !mutates_external_state && !mutates_tracker_state,
                format!("{ctx}: idempotent_refresh must not mutate external state"),
            )?;
            ensure(
                matches!(next_action, "run_directly" | "run_preflight_first"),
                format!("{ctx}: idempotent_refresh nextAction is too risky"),
            )?;
        }
        "mutating_local_repair" => {
            ensure(
                !mutates_external_state && !mutates_tracker_state,
                format!("{ctx}: mutating_local_repair must stay local"),
            )?;
            ensure(
                preflight_command.is_some() || requires_human_approval,
                format!("{ctx}: mutating_local_repair needs preflight or human approval"),
            )?;
        }
        "mutating_external_coordination_repair" => {
            ensure(
                mutates_external_state,
                format!("{ctx}: mutating_external_coordination_repair must flag external state"),
            )?;
            ensure(
                matches!(next_action, "coordinate_first" | "ask_human"),
                format!("{ctx}: external coordination repair must coordinate or ask a human"),
            )?;
        }
        "approval_required_repair" => {
            ensure(
                requires_human_approval && next_action == "ask_human",
                format!("{ctx}: approval_required_repair must ask a human"),
            )?;
        }
        "destructive_or_irreversible_repair" => {
            ensure(
                requires_human_approval && next_action == "policy_denied",
                format!("{ctx}: destructive repair must be policy_denied"),
            )?;
        }
        "unavailable_or_manual_only" => {
            ensure(
                command.is_none()
                    || safety
                        .get("command")
                        .is_some_and(|value| value.as_str().is_none()),
                format!("{ctx}: manual-only safety must not expose a runnable command"),
            )?;
            ensure(
                manual_step.is_some() || command.is_none(),
                format!("{ctx}: manual-only safety needs a manual step or null command"),
            )?;
            ensure(
                next_action == "manual_only",
                format!("{ctx}: manual-only safety must emit manual_only"),
            )?;
            ensure(
                !mutates_external_state && !mutates_tracker_state,
                format!("{ctx}: manual-only safety must not claim mutation"),
            )?;
        }
        _ => {}
    }

    if let Some(command) = command {
        if command_is_mutating_external(command) {
            ensure(
                risk_class == "mutating_external_coordination_repair",
                format!("{ctx}: mutating external command `{command}` lacks external risk class"),
            )?;
            ensure(
                mutates_external_state,
                format!(
                    "{ctx}: mutating external command `{command}` must set mutatesExternalState"
                ),
            )?;
        }
        if command.to_ascii_lowercase().starts_with("br ") {
            ensure(
                mutates_tracker_state || risk_class == "read_only_probe",
                format!("{ctx}: Beads command `{command}` must classify tracker mutation"),
            )?;
        }
    }

    Ok(())
}

fn walk_fallback_actions(value: &Value, path: &str, errors: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(actions) = map.get("fallbackActions").and_then(Value::as_array) {
                for (index, action) in actions.iter().enumerate() {
                    let ctx = format!("{path}/fallbackActions[{index}]");
                    let command = action.get("command").and_then(Value::as_str);
                    let manual_step = action.get("manualStep").and_then(Value::as_str);
                    if command.is_some() || manual_step.is_some() {
                        match action.get("repairSafety") {
                            Some(safety) => {
                                if let Err(error) =
                                    validate_repair_safety(safety, command, manual_step, &ctx)
                                {
                                    errors.push(error);
                                }
                            }
                            None => errors.push(format!(
                                "{ctx}: fallback action names command/manualStep without repairSafety"
                            )),
                        }
                    }
                }
            }
            for (key, child) in map {
                walk_fallback_actions(child, &format!("{path}/{key}"), errors);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                walk_fallback_actions(child, &format!("{path}[{index}]"), errors);
            }
        }
        _ => {}
    }
}

fn walk_source_run_recovery_actions(value: &Value, path: &str, errors: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            let is_source_run_evidence = map.get("schema").and_then(Value::as_str)
                == Some("ee.source_run_evidence.v1")
                || map.get("const").and_then(Value::as_str) == Some("ee.source_run_evidence.v1");
            if is_source_run_evidence
                && let Some(actions) = map.get("recovery").and_then(Value::as_array)
            {
                for (index, action) in actions.iter().enumerate() {
                    let ctx = format!("{path}/recovery[{index}]");
                    let command = action.get("command").and_then(Value::as_str);
                    let manual_step = action
                        .get("manualStep")
                        .and_then(Value::as_str)
                        .or_else(|| action.get("message").and_then(Value::as_str));
                    if command.is_some() || manual_step.is_some() {
                        match action.get("repairSafety") {
                            Some(safety) => {
                                if let Err(error) =
                                    validate_repair_safety(safety, command, manual_step, &ctx)
                                {
                                    errors.push(error);
                                }
                            }
                            None => errors.push(format!(
                                "{ctx}: source-run recovery names command/message without repairSafety"
                            )),
                        }
                    }
                }
            }
            for (key, child) in map {
                walk_source_run_recovery_actions(child, &format!("{path}/{key}"), errors);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                walk_source_run_recovery_actions(child, &format!("{path}[{index}]"), errors);
            }
        }
        _ => {}
    }
}

fn walk_swarm_incident_recovery_actions(value: &Value, path: &str, errors: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            let is_swarm_incident = map.get("schema").and_then(Value::as_str)
                == Some("ee.swarm_incident.v1")
                || map.get("const").and_then(Value::as_str) == Some("ee.swarm_incident.v1");
            if is_swarm_incident
                && let Some(actions) = map.get("expectedRecoveryActions").and_then(Value::as_array)
            {
                for (index, action) in actions.iter().enumerate() {
                    let ctx = format!("{path}/expectedRecoveryActions[{index}]");
                    let command = action.get("command").and_then(Value::as_str);
                    let manual_step = action.get("manualStep").and_then(Value::as_str);
                    if command.is_some() || manual_step.is_some() {
                        match action.get("repairSafety") {
                            Some(safety) => {
                                if let Err(error) =
                                    validate_repair_safety(safety, command, manual_step, &ctx)
                                {
                                    errors.push(error);
                                }
                            }
                            None => errors.push(format!(
                                "{ctx}: swarm incident recovery action names command/manualStep without repairSafety"
                            )),
                        }
                    }
                }
            }
            for (key, child) in map {
                walk_swarm_incident_recovery_actions(child, &format!("{path}/{key}"), errors);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                walk_swarm_incident_recovery_actions(child, &format!("{path}[{index}]"), errors);
            }
        }
        _ => {}
    }
}

fn collect_repair_safety_risk_classes(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(safety) = map.get("repairSafety").or_else(|| map.get("repair_safety")) {
                match safety {
                    Value::Object(object) => {
                        if let Some(risk_class) = object.get("riskClass").and_then(Value::as_str) {
                            out.insert(risk_class.to_owned());
                        }
                    }
                    Value::Array(items) => {
                        for item in items {
                            if let Some(risk_class) = item.get("riskClass").and_then(Value::as_str)
                            {
                                out.insert(risk_class.to_owned());
                            }
                        }
                    }
                    _ => {}
                }
            }
            for child in map.values() {
                collect_repair_safety_risk_classes(child, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_repair_safety_risk_classes(child, out);
            }
        }
        _ => {}
    }
}

fn validate_high_risk_failure_fixture(path: &Path) -> TestResult {
    let value = read_json(path)?;
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{}: missing code", path.display()))?;
    if !HIGH_RISK_FAILURE_MODE_CODES.contains(&code) {
        return Ok(());
    }
    let expected = value
        .get("expected_emission")
        .ok_or_else(|| format!("{}: missing expected_emission", path.display()))?;
    let safety = expected
        .get("repair_safety")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "{}: high-risk fixture `{code}` lacks repair_safety",
                path.display()
            )
        })?;
    ensure(
        !safety.is_empty(),
        format!(
            "{}: high-risk fixture `{code}` has empty repair_safety",
            path.display()
        ),
    )?;
    for (index, entry) in safety.iter().enumerate() {
        let command = entry.get("command").and_then(Value::as_str);
        let manual_step = entry.get("manualStep").and_then(Value::as_str);
        validate_repair_safety(
            entry,
            command,
            manual_step.or(Some("failure-mode repair metadata")),
            &format!("{} repair_safety[{index}]", path.display()),
        )?;
    }
    Ok(())
}

#[test]
fn work_packet_fallback_actions_have_repair_safety_metadata() -> TestResult {
    let mut errors = Vec::new();
    for path in tracked_paths("tests/fixtures/swarm_work_packet/*.json")? {
        let value = read_json(&path)?;
        walk_fallback_actions(&value, &path.display().to_string(), &mut errors);
    }
    let schema_path = repo_root().join("docs/schemas/swarm/ee.swarm.work_packet.v1.json");
    let schema = read_json(&schema_path)?;
    walk_fallback_actions(&schema, &schema_path.display().to_string(), &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} fallback repair-safety conformance error(s):\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
}

#[test]
fn high_risk_failure_mode_repairs_have_safety_metadata() -> TestResult {
    let mut errors = Vec::new();
    for path in tracked_paths("tests/fixtures/failure_modes/*.json")? {
        if let Err(error) = validate_high_risk_failure_fixture(&path) {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} high-risk failure-mode repair-safety error(s):\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
}

#[test]
fn source_run_recovery_actions_have_repair_safety_metadata() -> TestResult {
    let mut errors = Vec::new();
    let schema_path = repo_root().join("docs/schemas/swarm/ee.source_run_evidence.v1.json");
    let schema = read_json(&schema_path)?;
    walk_source_run_recovery_actions(&schema, &schema_path.display().to_string(), &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} source-run recovery repair-safety conformance error(s):\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
}

#[test]
fn swarm_incident_recovery_actions_have_repair_safety_metadata() -> TestResult {
    let mut errors = Vec::new();
    let schema_path = repo_root().join("docs/schemas/swarm/ee.swarm_incident.v1.json");
    let schema = read_json(&schema_path)?;
    let required = schema
        .pointer("/$defs/recoveryAction/required")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "{}: missing /$defs/recoveryAction/required",
                schema_path.display()
            )
        })?;
    ensure(
        required
            .iter()
            .any(|field| field.as_str() == Some("repairSafety")),
        format!(
            "{}: swarm incident recovery actions must require repairSafety",
            schema_path.display()
        ),
    )?;
    walk_swarm_incident_recovery_actions(&schema, &schema_path.display().to_string(), &mut errors);
    for path in tracked_paths("tests/fixtures/swarm_incidents/*.json")? {
        let value = read_json(&path)?;
        walk_swarm_incident_recovery_actions(&value, &path.display().to_string(), &mut errors);
    }
    let all_examples_path = repo_root().join("tests/fixtures/swarm_schemas/all_examples.json");
    walk_swarm_incident_recovery_actions(
        &read_json(&all_examples_path)?,
        &all_examples_path.display().to_string(),
        &mut errors,
    );

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} swarm incident recovery repair-safety conformance error(s):\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
}

#[test]
fn repair_safety_matrix_covers_agent_decisions() -> TestResult {
    let mut risk_classes = BTreeSet::new();
    for pattern in [
        "tests/fixtures/failure_modes/*.json",
        "tests/fixtures/swarm_work_packet/*.json",
        "tests/fixtures/swarm_incidents/*.json",
    ] {
        for path in tracked_paths(pattern)? {
            collect_repair_safety_risk_classes(&read_json(&path)?, &mut risk_classes);
        }
    }
    collect_repair_safety_risk_classes(
        &read_json(&repo_root().join("docs/schemas/swarm/ee.swarm.work_packet.v1.json"))?,
        &mut risk_classes,
    );
    collect_repair_safety_risk_classes(
        &read_json(&repo_root().join("docs/schemas/swarm/ee.source_run_evidence.v1.json"))?,
        &mut risk_classes,
    );

    for required in [
        "read_only_probe",
        "idempotent_refresh",
        "mutating_external_coordination_repair",
        "unavailable_or_manual_only",
    ] {
        ensure(
            risk_classes.contains(required),
            format!("repair-safety fixture matrix lacks `{required}` coverage"),
        )?;
    }

    let preflight_schema = read_json(&repo_root().join("docs/schemas/ee.preflight.guard.v1.json"))?;
    let schema_text = preflight_schema.to_string();
    ensure(
        schema_text.contains("destructive_or_irreversible_repair")
            && schema_text.contains("policy_denied"),
        "preflight guard schema must retain destructive policy-denied vocabulary",
    )?;
    Ok(())
}
