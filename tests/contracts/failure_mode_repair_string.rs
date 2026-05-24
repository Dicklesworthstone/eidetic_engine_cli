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
