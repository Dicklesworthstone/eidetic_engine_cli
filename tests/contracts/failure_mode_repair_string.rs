//! J6.1 contract: per-fixture repair-string pinning + regex extraction
//! (`bd-17c65.10.6.1`).
//!
//! Walks `tests/fixtures/failure_modes/*.json` and, for each fixture
//! that has a pinning field set under `expected_emission`
//! (`repair_string` xor `repair_strings`), asserts the J6.1 contract:
//!
//! 1. Exactly one pinning field is populated when either is present
//!    (mutual exclusion).
//! 2. Pinned strings are non-empty.
//! 3. When `repair_command_regex` is present, it compiles under the
//!    Rust `regex-lite` crate and contains exactly one named capture
//!    group named `cmd`.
//! 4. When both a pinning field and `repair_command_regex` are
//!    present, the regex matches at least one of the pinned strings
//!    and the `cmd` group is non-empty.
//! 5. When `repair_contains` is also set, each pinned string contains
//!    the `repair_contains` substring (cross-field consistency).
//!
//! The contract is opt-in per fixture: fixtures with `repair_present:
//! true` but no pinning field are skipped (full backfill is tracked
//! by follow-up bead `bd-17c65.10.6.1.1`). Once a fixture is pinned,
//! the contract test prevents silent drift.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use regex_lite::Regex;
use serde_json::Value;

type TestResult = Result<(), String>;
const SAFETY_REQUIRED_CODES: &[&str] = &[
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

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("failure_modes")
}

fn swarm_work_packet_fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("swarm_work_packet")
        .join(name)
}

fn list_fixture_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| format!("failed to read {}: {error}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    Ok(paths)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn non_empty_string<'a>(value: &'a Value, field: &str, ctx: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{ctx}: repair_safety entry missing non-empty `{field}`"))
}

fn required_bool(value: &Value, field: &str, ctx: &str) -> Result<bool, String> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{ctx}: repair_safety entry missing bool `{field}`"))
}

fn required_nullable_string<'a>(
    value: &'a Value,
    field: &str,
    ctx: &str,
) -> Result<Option<&'a str>, String> {
    match value.get(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.as_str())),
        Some(Value::String(_)) => Err(format!(
            "{ctx}: repair_safety entry `{field}` must not be an empty string"
        )),
        Some(_) => Err(format!(
            "{ctx}: repair_safety entry `{field}` must be string or null"
        )),
        None => Err(format!("{ctx}: repair_safety entry missing `{field}`")),
    }
}

fn required_string_array(value: &Value, field: &str, ctx: &str) -> TestResult {
    let items = value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{ctx}: repair_safety entry missing array `{field}`"))?;
    for (idx, item) in items.iter().enumerate() {
        ensure(
            item.as_str().is_some_and(|s| !s.is_empty()),
            format!("{ctx}: repair_safety entry `{field}[{idx}]` must be a non-empty string"),
        )?;
    }
    Ok(())
}

fn pointer_string_or_null(value: &Value, pointer: &str, ctx: &str) -> TestResult {
    ensure(
        value
            .pointer(pointer)
            .is_some_and(|v| v.is_null() || v.as_str().is_some_and(|s| !s.is_empty())),
        format!("{ctx}: `{pointer}` must be a non-empty string or null"),
    )
}

fn pointer_object_or_null(value: &Value, pointer: &str, ctx: &str) -> TestResult {
    ensure(
        value
            .pointer(pointer)
            .is_some_and(|v| v.is_null() || v.is_object()),
        format!("{ctx}: `{pointer}` must be object or null"),
    )
}

fn string_array_at_pointer(value: &Value, pointer: &str, ctx: &str) -> TestResult {
    let items = value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{ctx}: missing array `{pointer}`"))?;
    for (idx, item) in items.iter().enumerate() {
        ensure(
            item.as_str().is_some_and(|s| !s.is_empty()),
            format!("{ctx}: `{pointer}[{idx}]` must be a non-empty string"),
        )?;
    }
    Ok(())
}

fn manual_text_looks_command_shaped(text: &str) -> bool {
    let text = text.trim_start().to_ascii_lowercase();
    ["am ", "br ", "bv ", "ee ", "git ", "rch ", "cargo "]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn validate_work_packet_fallback_safety(action: &Value, ctx: &str) -> TestResult {
    let _kind = non_empty_string(action, "kind", ctx)?;
    let _summary = non_empty_string(action, "summary", ctx)?;
    pointer_string_or_null(action, "/command", ctx)?;
    pointer_object_or_null(action, "/commandAction", ctx)?;
    pointer_string_or_null(action, "/manualStep", ctx)?;
    let command = action.pointer("/command").and_then(Value::as_str);
    let command_action = action.pointer("/commandAction");
    let manual_step = action.pointer("/manualStep").and_then(Value::as_str);
    let safety = action
        .pointer("/repairSafety")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{ctx}: fallback action missing repairSafety object"))?;
    let safety = Value::Object(safety.clone());
    let risk_class = non_empty_string(&safety, "riskClass", ctx)?;
    ensure(
        RISK_CLASSES.contains(&risk_class),
        format!("{ctx}: repairSafety riskClass `{risk_class}` is not recognized"),
    )?;
    let next_action = non_empty_string(&safety, "nextAction", ctx)?;
    ensure(
        NEXT_ACTIONS.contains(&next_action),
        format!("{ctx}: repairSafety nextAction `{next_action}` is not recognized"),
    )?;
    required_nullable_string(&safety, "preflightCommand", ctx)?;
    let requires_human_approval = required_bool(&safety, "requiresHumanApproval", ctx)?;
    let mutates_external_state = required_bool(&safety, "mutatesExternalState", ctx)?;
    let mutates_tracker_state = required_bool(&safety, "mutatesTrackerState", ctx)?;
    let _privacy_class = non_empty_string(&safety, "privacyClass", ctx)?;
    let rule_id = non_empty_string(&safety, "ruleId", ctx)?;
    let source = non_empty_string(&safety, "source", ctx)?;
    let _reason_code = non_empty_string(&safety, "reasonCode", ctx)?;
    string_array_at_pointer(&safety, "/evidence", ctx)?;
    string_array_at_pointer(&safety, "/preconditions", ctx)?;
    ensure(
        rule_id.starts_with("repair_safety:"),
        format!("{ctx}: repairSafety ruleId `{rule_id}` must use repair_safety: prefix"),
    )?;
    ensure(
        source == "repair_action_safety" || source == "work_packet_manual_fallback",
        format!("{ctx}: repairSafety source `{source}` is not recognized"),
    )?;

    if command.is_some() {
        ensure(
            command_action.is_some_and(Value::is_object),
            format!("{ctx}: command-shaped fallback action must include commandAction"),
        )?;
        ensure(
            source == "repair_action_safety",
            format!("{ctx}: command-shaped fallback action must use repair_action_safety source"),
        )?;
    } else {
        ensure(
            !command_action.is_some_and(Value::is_object),
            format!("{ctx}: commandAction must be null when command is null"),
        )?;
    }

    if let Some(manual_step) = manual_step {
        ensure(
            !manual_text_looks_command_shaped(manual_step),
            format!("{ctx}: manualStep must not be a command-shaped repair hint"),
        )?;
    }

    if risk_class == "unavailable_or_manual_only" {
        ensure(
            command.is_none() && next_action == "manual_only",
            format!("{ctx}: manual-only fallback must not expose a command"),
        )?;
        ensure(
            source == "work_packet_manual_fallback",
            format!("{ctx}: manual-only fallback must use work_packet_manual_fallback source"),
        )?;
    }
    if risk_class == "read_only_probe" {
        ensure(
            !requires_human_approval && !mutates_external_state && !mutates_tracker_state,
            format!("{ctx}: read-only fallback must be non-mutating"),
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

fn pinned_text_has_mutating_external_hint(strings: &[String]) -> bool {
    strings.iter().any(|s| {
        let s = s.to_ascii_lowercase();
        s.contains("am doctor repair")
            || s.contains("br sync")
            || s.contains("rch daemon restart")
            || s.contains("rch workers probe")
            || s.contains("rch workers capabilities --refresh")
    })
}

fn collect_extracted_commands(strings: &[String], regex: Option<&Regex>) -> BTreeSet<String> {
    let mut commands = BTreeSet::new();
    let Some(regex) = regex else {
        return commands;
    };
    for s in strings {
        if let Some(captures) = regex.captures(s)
            && let Some(cmd) = captures.name("cmd")
        {
            commands.insert(cmd.as_str().to_owned());
        }
    }
    commands
}

fn repair_safety_required(
    code: &str,
    pinned: Option<&[String]>,
    commands: &BTreeSet<String>,
) -> bool {
    SAFETY_REQUIRED_CODES.contains(&code)
        || commands
            .iter()
            .any(|command| command_is_mutating_external(command))
        || pinned.is_some_and(pinned_text_has_mutating_external_hint)
}

fn validate_repair_safety_entry(
    entry: &Value,
    commands: &BTreeSet<String>,
    pinned: Option<&[String]>,
    ctx: &str,
) -> TestResult {
    let risk_class = non_empty_string(entry, "riskClass", ctx)?;
    ensure(
        RISK_CLASSES.contains(&risk_class),
        format!("{ctx}: repair_safety riskClass `{risk_class}` is not recognized"),
    )?;
    let next_action = non_empty_string(entry, "nextAction", ctx)?;
    ensure(
        NEXT_ACTIONS.contains(&next_action),
        format!("{ctx}: repair_safety nextAction `{next_action}` is not recognized"),
    )?;
    let command = required_nullable_string(entry, "command", ctx)?;
    let preflight = required_nullable_string(entry, "preflightCommand", ctx)?;
    let requires_human_approval = required_bool(entry, "requiresHumanApproval", ctx)?;
    let mutates_external_state = required_bool(entry, "mutatesExternalState", ctx)?;
    let mutates_tracker_state = required_bool(entry, "mutatesTrackerState", ctx)?;
    let _privacy_class = non_empty_string(entry, "privacyClass", ctx)?;
    let rule_id = non_empty_string(entry, "ruleId", ctx)?;
    let source = non_empty_string(entry, "source", ctx)?;
    let reason_code = non_empty_string(entry, "reasonCode", ctx)?;
    required_string_array(entry, "evidence", ctx)?;
    required_string_array(entry, "preconditions", ctx)?;
    ensure(
        rule_id.starts_with("repair_safety:"),
        format!("{ctx}: repair_safety ruleId `{rule_id}` must use repair_safety: prefix"),
    )?;
    ensure(
        source == "repair_action_safety",
        format!("{ctx}: repair_safety source `{source}` must be repair_action_safety"),
    )?;
    ensure(
        !reason_code.is_empty(),
        format!("{ctx}: repair_safety reasonCode must not be empty"),
    )?;

    if let Some(applies_to) = entry.get("appliesTo").and_then(Value::as_str)
        && let Some(strings) = pinned
    {
        ensure(
            strings.iter().any(|s| s == applies_to),
            format!(
                "{ctx}: repair_safety appliesTo `{applies_to}` does not match any pinned repair string"
            ),
        )?;
    }
    if let Some(command) = command
        && !commands.is_empty()
    {
        ensure(
            commands.contains(command),
            format!(
                "{ctx}: repair_safety command `{command}` was not extracted by repair_command_regex"
            ),
        )?;
    }

    match risk_class {
        "read_only_probe" => {
            ensure(
                preflight.is_none(),
                format!("{ctx}: read_only_probe repair_safety must not require preflight"),
            )?;
            ensure(
                !requires_human_approval && !mutates_external_state && !mutates_tracker_state,
                format!("{ctx}: read_only_probe repair_safety must be non-mutating"),
            )?;
            ensure(
                next_action == "run_directly",
                format!("{ctx}: read_only_probe nextAction must be run_directly"),
            )?;
        }
        "idempotent_refresh" => {
            ensure(
                !requires_human_approval && !mutates_external_state && !mutates_tracker_state,
                format!("{ctx}: idempotent_refresh repair_safety must be non-external"),
            )?;
            ensure(
                next_action == "run_directly" || next_action == "run_preflight_first",
                format!(
                    "{ctx}: idempotent_refresh nextAction must be directly runnable or preflighted"
                ),
            )?;
        }
        "mutating_local_repair" => {
            ensure(
                !mutates_external_state && !mutates_tracker_state,
                format!("{ctx}: mutating_local_repair must not mutate external or tracker state"),
            )?;
            ensure(
                preflight.is_some() || requires_human_approval,
                format!(
                    "{ctx}: mutating_local_repair must carry preflight or require human approval"
                ),
            )?;
        }
        "mutating_external_coordination_repair" => {
            ensure(
                mutates_external_state,
                format!(
                    "{ctx}: mutating_external_coordination_repair must set mutatesExternalState"
                ),
            )?;
            ensure(
                next_action == "coordinate_first" || next_action == "ask_human",
                format!(
                    "{ctx}: mutating_external_coordination_repair nextAction must coordinate or ask a human"
                ),
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
                format!("{ctx}: destructive_or_irreversible_repair must be policy_denied"),
            )?;
        }
        "unavailable_or_manual_only" => {
            ensure(
                command.is_none(),
                format!(
                    "{ctx}: unavailable_or_manual_only repair_safety must not expose a command"
                ),
            )?;
            ensure(
                requires_human_approval && next_action == "manual_only",
                format!(
                    "{ctx}: unavailable_or_manual_only must be manual_only and require approval"
                ),
            )?;
        }
        _ => {}
    }

    if command.is_some_and(command_is_mutating_external) {
        ensure(
            risk_class == "mutating_external_coordination_repair",
            format!(
                "{ctx}: mutating external command must use mutating_external_coordination_repair"
            ),
        )?;
        ensure(
            mutates_external_state,
            format!("{ctx}: mutating external command must set mutatesExternalState"),
        )?;
    }
    if command.is_some_and(|command| command.to_ascii_lowercase().starts_with("br sync")) {
        ensure(
            mutates_tracker_state,
            format!("{ctx}: br sync repair_safety must set mutatesTrackerState"),
        )?;
    }

    Ok(())
}

fn validate_repair_safety_metadata(
    code: &str,
    expected: &Value,
    pinned: Option<&[String]>,
    regex: Option<&Regex>,
    ctx: &str,
) -> TestResult {
    let empty: Vec<String> = Vec::new();
    let strings = pinned.unwrap_or(empty.as_slice());
    let commands = collect_extracted_commands(strings, regex);
    let required = repair_safety_required(code, pinned, &commands);
    let safety = expected.get("repair_safety").filter(|v| !v.is_null());

    if safety.is_none() {
        ensure(
            !required,
            format!("{ctx}: `{code}` repair hint needs repair_safety metadata"),
        )?;
        return Ok(());
    }

    let entries = safety
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{ctx}: repair_safety must be an array"))?;
    ensure(
        !entries.is_empty(),
        format!("{ctx}: repair_safety must not be empty"),
    )?;
    for (idx, entry) in entries.iter().enumerate() {
        validate_repair_safety_entry(
            entry,
            &commands,
            pinned,
            &format!("{ctx} repair_safety[{idx}]"),
        )?;
    }

    if commands
        .iter()
        .any(|command| command_is_mutating_external(command))
    {
        ensure(
            entries.iter().any(|entry| {
                entry.get("riskClass").and_then(Value::as_str)
                    == Some("mutating_external_coordination_repair")
            }),
            format!(
                "{ctx}: mutating external repair command requires mutating_external_coordination_repair metadata"
            ),
        )?;
    }

    Ok(())
}

/// Collect pinned strings from a fixture's expected_emission. Returns
/// None if neither `repair_string` nor `repair_strings` is populated
/// (fixture is unpinned). Returns an error if BOTH are populated
/// (the schema forbids that) or if the values have wrong types.
fn collect_pinned_strings(expected: &Value, ctx: &str) -> Result<Option<Vec<String>>, String> {
    let single = expected.get("repair_string").filter(|v| !v.is_null());
    let array = expected.get("repair_strings").filter(|v| !v.is_null());

    match (single, array) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(format!(
            "{ctx}: both `repair_string` and `repair_strings` are set; \
             pick one. Use `repair_strings` only for codes that emit \
             multiple repair variants from different trigger branches."
        )),
        (Some(s), None) => {
            let value = s
                .as_str()
                .ok_or_else(|| format!("{ctx}: `repair_string` must be a string"))?;
            ensure(
                !value.is_empty(),
                format!("{ctx}: `repair_string` must not be empty"),
            )?;
            Ok(Some(vec![value.to_owned()]))
        }
        (None, Some(arr)) => {
            let items = arr
                .as_array()
                .ok_or_else(|| format!("{ctx}: `repair_strings` must be an array"))?;
            ensure(
                !items.is_empty(),
                format!("{ctx}: `repair_strings` must not be empty when set"),
            )?;
            let mut out = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("{ctx}: `repair_strings[{idx}]` must be a string"))?;
                ensure(
                    !s.is_empty(),
                    format!("{ctx}: `repair_strings[{idx}]` must not be empty"),
                )?;
                out.push(s.to_owned());
            }
            Ok(Some(out))
        }
    }
}

/// Validate the named-capture contract on a repair_command_regex
/// string. Returns the compiled regex on success.
fn compile_repair_command_regex(pattern: &str, ctx: &str) -> Result<Regex, String> {
    let regex = Regex::new(pattern)
        .map_err(|error| format!("{ctx}: `repair_command_regex` failed to compile: {error}"))?;

    // regex-lite exposes capture_names() returning an iterator over
    // Option<&str>; named captures appear as Some(name).
    let names: Vec<&str> = regex.capture_names().flatten().collect();
    ensure(
        names.len() == 1,
        format!(
            "{ctx}: `repair_command_regex` must have exactly one named \
             capture group; found {} named groups ({:?})",
            names.len(),
            names
        ),
    )?;
    ensure(
        names[0] == "cmd",
        format!(
            "{ctx}: `repair_command_regex` named capture must be `cmd`; \
             found `{}`",
            names[0]
        ),
    )?;
    Ok(regex)
}

fn validate_fixture_pinning(path: &Path) -> TestResult {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    let ctx = path.display().to_string();
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{ctx}: missing top-level code"))?;

    let expected = value
        .get("expected_emission")
        .ok_or_else(|| format!("{ctx}: missing expected_emission"))?;

    let pinned = collect_pinned_strings(expected, &ctx)?;
    let regex_str = expected
        .get("repair_command_regex")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_str()
                .ok_or_else(|| format!("{ctx}: `repair_command_regex` must be a string"))
                .map(str::to_owned)
        })
        .transpose()?;

    // Skip entirely-unpinned fixtures (backfill is incremental per
    // bd-17c65.10.6.1.1).
    if pinned.is_none() && regex_str.is_none() {
        return Ok(());
    }

    // Compile the regex if present and validate its name contract.
    let regex = match regex_str.as_deref() {
        Some(pattern) => Some(compile_repair_command_regex(pattern, &ctx)?),
        None => None,
    };

    validate_repair_safety_metadata(code, expected, pinned.as_deref(), regex.as_ref(), &ctx)?;

    // If we have both pinned strings and a regex, assert the regex
    // matches at least one pinned string and the `cmd` capture is
    // non-empty.
    if let (Some(strings), Some(re)) = (pinned.as_deref(), regex.as_ref()) {
        let mut matched = false;
        for s in strings {
            if let Some(caps) = re.captures(s) {
                if let Some(cmd) = caps.name("cmd") {
                    ensure(
                        !cmd.as_str().is_empty(),
                        format!(
                            "{ctx}: `repair_command_regex` matched repair string `{}` but \
                             extracted an empty `cmd` group",
                            s
                        ),
                    )?;
                    matched = true;
                    break;
                }
            }
        }
        ensure(
            matched,
            format!(
                "{ctx}: `repair_command_regex` does not match any of the pinned \
                 repair strings. Pinned: {:?}",
                strings
            ),
        )?;
    }

    // Cross-field consistency: if repair_contains is set, every pinned
    // string must contain it.
    if let Some(strings) = pinned.as_deref() {
        if let Some(contains) = expected
            .get("repair_contains")
            .filter(|v| !v.is_null())
            .and_then(Value::as_str)
        {
            for s in strings {
                ensure(
                    s.contains(contains),
                    format!(
                        "{ctx}: pinned repair string `{}` does not contain the \
                         `repair_contains` substring `{}`; fix one of them",
                        s, contains
                    ),
                )?;
            }
        }
    }

    Ok(())
}

#[test]
fn failure_mode_fixtures_pinned_repairs_are_consistent() -> TestResult {
    let dir = fixtures_dir();
    let fixtures = list_fixture_files(&dir)?;
    let mut errors: Vec<String> = Vec::new();
    for path in &fixtures {
        if let Err(error) = validate_fixture_pinning(path) {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} fixture(s) failed J6.1 repair-pinning validation:\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
}

#[test]
fn failure_mode_fixtures_show_some_pinned_coverage() -> TestResult {
    // Ratcheted floor for the J6.1.1 backfill (bd-17c65.10.6.1.1).
    //
    // The floor only ratchets UP — never down. It is the minimum count
    // of fixtures that MUST carry a pinned `repair_string` or
    // `repair_strings` field. When new fixtures are added with pinned
    // repairs, raise this constant in the same PR. When new fixtures
    // are added without pinning, the floor stays where it is and the
    // unpinned fixtures contribute backfill debt against
    // `bd-17c65.10.6.1.1`.
    //
    // History:
    //   - 2026-05-13 J6.1 seed: 6 pinned (swarm-brief connector codes).
    //   - 2026-05-13 J6.1.1 first backfill pass: 82 pinned via
    //     `scripts/audit_randomness_sources.sh`-pattern grep + jq
    //     patching against verified production literals.
    const PINNED_FLOOR: usize = 80;

    let dir = fixtures_dir();
    let fixtures = list_fixture_files(&dir)?;
    let mut pinned_count = 0usize;
    for path in &fixtures {
        let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let Some(expected) = value.get("expected_emission") else {
            continue;
        };
        let single = expected
            .get("repair_string")
            .filter(|v| !v.is_null())
            .is_some();
        let array = expected
            .get("repair_strings")
            .filter(|v| !v.is_null())
            .is_some();
        if single || array {
            pinned_count += 1;
        }
    }
    ensure(
        pinned_count >= PINNED_FLOOR,
        format!(
            "J6.1.1 floor: at least {PINNED_FLOOR} fixtures must be pinned with \
             `repair_string` or `repair_strings`; found {pinned_count}. The floor \
             ratchets UP only — never down. If you removed a pinned fixture, pin \
             a replacement to keep the floor satisfied OR raise the floor when \
             adding new pinned fixtures."
        ),
    )
}

#[test]
fn work_packet_fallback_actions_have_repair_safety_metadata() -> TestResult {
    let fixtures = [
        "agent_mail_degraded_read_only.json",
        "agent_mail_semantic_readiness_failed.json",
        "beads_command_timeout_no_output.json",
        "crowded_checkout.json",
        "degraded_mail_rch_topology.json",
        "healthy_small.json",
    ];
    let mut errors = Vec::new();
    for fixture_name in fixtures {
        let path = swarm_work_packet_fixture_path(fixture_name);
        let value = match read_json(&path) {
            Ok(value) => value,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let Some(actions) = value
            .pointer("/coordination/agentMail/fallbackActions")
            .and_then(Value::as_array)
        else {
            continue;
        };
        for (index, action) in actions.iter().enumerate() {
            let ctx = format!("{} fallbackActions[{index}]", path.display());
            if let Err(error) = validate_work_packet_fallback_safety(action, &ctx) {
                errors.push(error);
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} work-packet fallback conformance error(s):\n  - {}",
            errors.len(),
            errors.join("\n  - "),
        ))
    }
}

/// Pinned repairs that name a command the CLI does not have, with the bead that owns each.
/// `pinned_repair_commands_parse_under_the_cli` skips these codes, and fails once one of them
/// parses, so the list can only shrink.
const UNBUILT_REPAIR_COMMANDS: &[(&str, &str)] = &[
    (
        "mesh_cursor_repair_required",
        "bd-brj5x: `ee mesh repair` comes only from the uncompiled src/mesh/anti_entropy_model.rs",
    ),
    (
        "mesh_event_quarantined",
        "bd-brj5x: `ee mesh repair` comes only from the uncompiled src/mesh/anti_entropy_model.rs",
    ),
];

/// The `ee ...` command lines a pinned repair tells an agent to run: the whole string when it
/// starts with `ee `, otherwise each backticked span that does.
fn ee_command_lines(pinned: &str) -> Vec<&str> {
    if pinned.starts_with("ee ") {
        return vec![pinned];
    }
    pinned
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("ee "))
        .collect()
}

/// Shell words of one command line, split into separate commands at `;`, `&&` and `||`.
fn shell_commands(line: &str) -> Result<Vec<Vec<String>>, String> {
    fn end_word(word: &mut String, started: &mut bool, command: &mut Vec<String>) {
        if *started {
            command.push(std::mem::take(word));
            *started = false;
        }
    }
    let mut commands = Vec::new();
    let mut command = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(open), _) if ch == open => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(next) = chars.next() {
                    word.push(next);
                    started = true;
                }
            }
            (Some(_), _) => word.push(ch),
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            }
            (None, ';') => {
                end_word(&mut word, &mut started, &mut command);
                commands.push(std::mem::take(&mut command));
            }
            (None, _) if ch.is_whitespace() => end_word(&mut word, &mut started, &mut command),
            (None, _) => {
                word.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(format!("unterminated quote in `{line}`"));
    }
    end_word(&mut word, &mut started, &mut command);
    commands.push(command);
    Ok(commands
        .iter()
        .flat_map(|command| command.split(|word| word == "&&" || word == "||"))
        .filter(|command| !command.is_empty())
        .map(<[String]>::to_vec)
        .collect())
}

/// Parse one `ee` argv with the real CLI. A bare `...` stands for arguments the agent supplies
/// and is dropped. Missing required arguments are accepted, and so are rejected values when the
/// repair leaves a `<memory-id>`-style placeholder to the agent. An unknown flag or subcommand
/// is not.
fn parse_ee_command(argv: &[String]) -> Result<(), String> {
    use clap::Parser;
    use clap::error::ErrorKind;

    let mut argv = argv
        .iter()
        .map(String::as_str)
        .filter(|word| *word != "...")
        .collect::<Vec<_>>();
    // `ee::cli::run` rewrites `outcome quarantine` to `outcome-quarantine` before clap sees it.
    if let Some(index) = argv
        .windows(2)
        .position(|pair| pair == ["outcome", "quarantine"])
    {
        argv[index] = "outcome-quarantine";
        argv.remove(index + 1);
    }
    // A rejected value is the agent's to supply only when the repair shows a `<placeholder>`;
    // otherwise it is a typo in the repair itself, such as an unknown job name.
    let has_placeholder = argv.iter().any(|word| word.contains('<'));
    match ee::cli::Cli::try_parse_from(argv) {
        Ok(_) => Ok(()),
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            | ErrorKind::DisplayVersion
            | ErrorKind::MissingRequiredArgument
            | ErrorKind::MissingSubcommand => Ok(()),
            ErrorKind::InvalidValue | ErrorKind::ValueValidation if has_placeholder => Ok(()),
            kind => Err(format!(
                "{kind:?}: {}",
                error.to_string().lines().next().unwrap_or_default()
            )),
        },
    }
}

/// Every `ee` command a pinned repair names must parse under the real CLI (bd-gfyw3).
///
/// A repair string is what an agent runs after a degraded response. On 2026-11-25 a census found
/// pinned and emitted repairs naming flags and subcommands that do not exist, for example
/// `ee remember --severity`, `ee graph centrality-refresh --algorithm` and `ee symbol snapshot`.
/// An agent that follows one gets a usage error instead of a repair.
#[test]
fn pinned_repair_commands_parse_under_the_cli() -> TestResult {
    let mut parsed = 0_usize;
    let mut failures = Vec::new();
    let mut parsing_exemptions = Vec::new();
    for path in list_fixture_files(&fixtures_dir())? {
        let fixture = read_json(&path)?;
        if fixture.get("retired").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let ctx = path.display().to_string();
        let Some(expected) = fixture.get("expected_emission") else {
            continue;
        };
        let Some(pinned) = collect_pinned_strings(expected, &ctx)? else {
            continue;
        };
        let code = fixture
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut fixture_failures = Vec::new();
        for text in &pinned {
            for line in ee_command_lines(text) {
                for argv in shell_commands(line)? {
                    if argv.first().map(String::as_str) != Some("ee") {
                        continue;
                    }
                    parsed += 1;
                    if let Err(reason) = parse_ee_command(&argv) {
                        fixture_failures.push(format!("{ctx}: `{}`: {reason}", argv.join(" ")));
                    }
                }
            }
        }
        let exempt = UNBUILT_REPAIR_COMMANDS
            .iter()
            .any(|(unbuilt, _)| *unbuilt == code);
        if exempt && fixture_failures.is_empty() {
            parsing_exemptions.push(code.to_owned());
        } else if !exempt {
            failures.extend(fixture_failures);
        }
    }
    ensure(
        parsed >= 100,
        format!("parsed only {parsed} pinned ee commands; the extraction regressed"),
    )?;
    ensure(
        parsing_exemptions.is_empty(),
        format!(
            "these codes now parse; remove them from UNBUILT_REPAIR_COMMANDS: {parsing_exemptions:?}"
        ),
    )?;
    ensure(
        failures.is_empty(),
        format!(
            "{} pinned repair command(s) do not parse under the real CLI:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        ),
    )
}

/// Every `ee` command resource admission tells an agent to run next must parse once its
/// placeholders hold sample values. The fixture guard above forgives any rejected value in a
/// command that shows a `<placeholder>`, and that is how `ee pack <task> --resource-profile
/// constrained` survived: pack profiles are lean, standard and swarm_heavy. The same review found
/// `ee support bundle --include resource-admission` and `ee lab swarm replay --split-workload`,
/// neither of which exists (bd-gfyw3).
#[test]
fn resource_admission_next_commands_parse_under_the_cli() -> TestResult {
    use clap::Parser;
    use ee::shadow::{
        ResourceAdmissionDecision, ResourceAdmissionInput, ResourceBudgetPosture,
        ResourceCostClass, ResourceHostCalibrationPosture, ResourceLanePressurePosture,
        ResourceLocalCargoPosture, ResourceRchPosture, evaluate_resource_profile_budget_admission,
    };

    let base = ResourceAdmissionInput::default;
    let cases = [
        (
            ResourceAdmissionDecision::Abstain,
            ResourceAdmissionInput {
                host_calibration: ResourceHostCalibrationPosture::Missing,
                ..base()
            },
        ),
        (
            ResourceAdmissionDecision::RefuseLocalCargo,
            ResourceAdmissionInput {
                local_cargo: ResourceLocalCargoPosture::Unsafe,
                rch: ResourceRchPosture::RemoteReady,
                ..base()
            },
        ),
        (
            ResourceAdmissionDecision::WaitForRch,
            ResourceAdmissionInput {
                rch: ResourceRchPosture::Blocked,
                ..base()
            },
        ),
        (
            ResourceAdmissionDecision::DegradeToLean,
            ResourceAdmissionInput {
                resource_budget: ResourceBudgetPosture::RecommendDecrease,
                ..base()
            },
        ),
        (
            ResourceAdmissionDecision::SplitWorkload,
            ResourceAdmissionInput {
                estimated_cost_class: ResourceCostClass::SwarmHeavy,
                ..base()
            },
        ),
        (
            ResourceAdmissionDecision::Queue,
            ResourceAdmissionInput {
                lane_pressure: ResourceLanePressurePosture::BackgroundPressure,
                ..base()
            },
        ),
    ];
    let mut parsed = 0_usize;
    let mut failures = Vec::new();
    for (expected, input) in cases {
        let report = evaluate_resource_profile_budget_admission(input);
        ensure(
            report.decision == expected,
            format!("input meant for {expected:?} gave {:?}", report.decision),
        )?;
        for command in &report.next_commands {
            for argv in shell_commands(command)? {
                if argv.first().map(String::as_str) != Some("ee") {
                    continue;
                }
                let mut words = Vec::with_capacity(argv.len());
                for word in &argv {
                    let sample = match word.as_str() {
                        "<task>" => "sample task",
                        "<dir>" => "bundle-dir",
                        other if other.contains('<') => {
                            failures.push(format!(
                                "{expected:?}: `{command}`: no sample value for {other}"
                            ));
                            other
                        }
                        other => other,
                    };
                    words.push(sample);
                }
                parsed += 1;
                if let Err(error) = ee::cli::Cli::try_parse_from(words.iter().copied()) {
                    failures.push(format!(
                        "{expected:?}: `{command}`: {:?}: {}",
                        error.kind(),
                        error.to_string().lines().next().unwrap_or_default()
                    ));
                }
            }
        }
    }
    // Abstain, degrade_to_lean, split_workload and queue each name one `ee` command.
    ensure(
        parsed == 4,
        format!("expected 4 ee next commands across the decisions, parsed {parsed}"),
    )?;
    ensure(
        failures.is_empty(),
        format!(
            "resource-admission next commands do not parse:\n  - {}",
            failures.join("\n  - ")
        ),
    )
}
