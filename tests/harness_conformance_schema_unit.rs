//! bd-i0iiw.1 - schema unit test for `ee.harness_conformance.v1` (ADR 0075).
//!
//! Pins the static harness-conformance fixture contract before the simulator
//! and doctor surfaces land. This test intentionally validates the schema JSON
//! and fixture examples directly instead of pulling in a general JSON Schema
//! engine dependency.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_REL: &str = "docs/schemas/ee.harness_conformance.v1.json";
const SCHEMA_NAME: &str = "ee.harness_conformance.v1";
const FIXTURES_REL: &str = "tests/fixtures/harness_conformance";
const FIXTURE_NAMES: [&str; 6] = [
    "codex_session_start",
    "claude_pre_tool_edit",
    "generic_shell_pre_tool_shell",
    "codex_post_tool_success",
    "mcp_client_post_tool_failure",
    "claude_compaction_resume",
];
const REDACTION_CONST: &str = "redacted_bounded_no_secrets";
const HARNESS_IDS: [&str; 4] = ["codex", "claude-code", "generic-shell", "mcp-client"];
const EVENT_NAMES: [&str; 4] = [
    "SessionStart",
    "PreToolUse",
    "PostToolUse",
    "CompactionResume",
];
const FIXTURE_KINDS: [&str; 6] = [
    "session_start",
    "pre_tool_edit",
    "pre_tool_shell",
    "post_tool_success",
    "post_tool_failure",
    "compaction_resume",
];
const ASSERTION_KINDS: [&str; 7] = [
    "command_invoked",
    "json_envelope_valid",
    "output_budget_respected",
    "degraded_handled",
    "secret_redaction",
    "non_zero_exit_policy",
    "no_local_cargo_fallback",
];
const CONFORMANCE_VERDICTS: [&str; 4] = ["pass", "fail", "blocked", "unsupported"];
const ASSERTION_STATUSES: [&str; 3] = ["pass", "fail", "not_applicable"];

fn manifest_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn load_json(relative: &str) -> Result<Value, String> {
    let path = manifest_path(relative);
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn string_set(value: &Value, pointer: &str) -> Result<BTreeSet<String>, String> {
    let node = value
        .pointer(pointer)
        .ok_or_else(|| format!("schema is missing pointer {pointer}"))?;
    let array = node
        .as_array()
        .ok_or_else(|| format!("{pointer} must be a JSON array"))?;
    let mut out = BTreeSet::new();
    for entry in array {
        let name = entry
            .as_str()
            .ok_or_else(|| format!("{pointer} contains a non-string entry: {entry}"))?;
        out.insert(name.to_owned());
    }
    Ok(out)
}

fn expected_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn schema_identity_status_and_required_fields() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        schema.pointer("/title").and_then(Value::as_str) == Some(SCHEMA_NAME),
        "schema title must be the schema name",
    )?;
    ensure(
        schema
            .pointer("/$id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.ends_with(&format!("{SCHEMA_NAME}.json"))),
        "$id must end with the schema file name",
    )?;
    ensure(
        schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(SCHEMA_NAME),
        "schema.const must pin the schema name",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/shipped")
            .and_then(Value::as_bool)
            == Some(false),
        "x-ee-status.shipped must stay false until bd-i0iiw.2 lands the simulator",
    )?;
    ensure(
        schema
            .pointer("/x-ee-status/tracking_bead")
            .and_then(Value::as_str)
            == Some("bd-i0iiw.2"),
        "x-ee-status.tracking_bead must point at the simulator bead",
    )?;

    let required = string_set(&schema, "/required")?;
    let expected = expected_set(&[
        "schema",
        "fixtureVersion",
        "caseId",
        "harness",
        "fixtureKind",
        "eventName",
        "harnessSupport",
        "input",
        "expected",
        "assertions",
        "artifactPolicy",
        "compatibility",
    ]);
    ensure(
        required == expected,
        format!("top-level required set drifted: {required:?}"),
    )
}

#[test]
fn vocabularies_are_pinned_for_future_harness_additions() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    let harness_ids = string_set(&schema, "/$defs/harnessId/enum")?;
    ensure(
        harness_ids == expected_set(&HARNESS_IDS),
        format!("harness id vocabulary drifted: {harness_ids:?}"),
    )?;
    let events = string_set(&schema, "/$defs/eventName/enum")?;
    ensure(
        events == expected_set(&EVENT_NAMES),
        format!("event vocabulary drifted: {events:?}"),
    )?;
    let fixture_kinds = string_set(&schema, "/$defs/fixtureKind/enum")?;
    ensure(
        fixture_kinds == expected_set(&FIXTURE_KINDS),
        format!("fixture kind vocabulary drifted: {fixture_kinds:?}"),
    )?;
    let assertions = string_set(&schema, "/$defs/assertionKind/enum")?;
    ensure(
        assertions == expected_set(&ASSERTION_KINDS),
        format!("assertion vocabulary drifted: {assertions:?}"),
    )?;
    let verdicts = string_set(&schema, "/$defs/conformanceVerdict/enum")?;
    ensure(
        verdicts == expected_set(&CONFORMANCE_VERDICTS),
        format!("conformance verdict vocabulary drifted: {verdicts:?}"),
    )?;
    let statuses = string_set(&schema, "/$defs/assertionStatus/enum")?;
    ensure(
        statuses == expected_set(&ASSERTION_STATUSES),
        format!("assertion status vocabulary drifted: {statuses:?}"),
    )
}

#[test]
fn redaction_and_budget_policy_are_schema_level_constants() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    ensure(
        schema
            .pointer("/$defs/input/properties/redactionStatus/const")
            .and_then(Value::as_str)
            == Some(REDACTION_CONST),
        "redactionStatus const drifted",
    )?;
    ensure(
        schema
            .pointer("/$defs/transcript/properties/lines/items/maxLength")
            .and_then(Value::as_u64)
            == Some(256),
        "transcript line maxLength must stay 256",
    )?;
    ensure(
        schema
            .pointer("/$defs/transcript/properties/byteCount/maximum")
            .and_then(Value::as_u64)
            == Some(8192),
        "transcript byte budget must stay 8192",
    )?;
    ensure(
        schema
            .pointer("/$defs/artifactPolicy/properties/rawTranscriptAllowed/const")
            .and_then(Value::as_bool)
            == Some(false),
        "rawTranscriptAllowed must be const false",
    )?;
    ensure(
        schema
            .pointer("/$defs/artifactPolicy/properties/secretMaterialAllowed/const")
            .and_then(Value::as_bool)
            == Some(false),
        "secretMaterialAllowed must be const false",
    )?;
    ensure(
        schema
            .pointer("/$defs/artifactPolicy/properties/maxArtifactBytes/maximum")
            .and_then(Value::as_u64)
            == Some(65_536),
        "artifact byte budget must stay 65536",
    )?;
    ensure(
        schema
            .pointer("/$defs/expected/properties/localCargoFallbackAllowed/const")
            .and_then(Value::as_bool)
            == Some(false),
        "localCargoFallbackAllowed must be const false",
    )
}

fn validate_fixture(schema: &Value, fixture: &Value, label: &str) -> TestResult {
    let object = fixture
        .as_object()
        .ok_or_else(|| format!("{label}: fixture must be an object"))?;
    for field in string_set(schema, "/required")? {
        ensure(
            object.contains_key(&field),
            format!("{label}: fixture missing required field {field}"),
        )?;
    }
    let allowed = schema
        .pointer("/properties")
        .and_then(Value::as_object)
        .ok_or("schema /properties missing")?;
    for key in object.keys() {
        ensure(
            allowed.contains_key(key),
            format!("{label}: fixture has unknown field {key}"),
        )?;
    }
    ensure(
        fixture.pointer("/schema").and_then(Value::as_str) == Some(SCHEMA_NAME),
        format!("{label}: schema field must be {SCHEMA_NAME}"),
    )?;
    ensure(
        fixture
            .pointer("/fixtureVersion")
            .and_then(Value::as_str)
            .is_some_and(|version| version.starts_with("1.")),
        format!("{label}: fixtureVersion must be major version 1"),
    )?;

    let harness = fixture
        .pointer("/harness")
        .and_then(Value::as_str)
        .unwrap_or("");
    ensure(
        expected_set(&HARNESS_IDS).contains(harness),
        format!("{label}: harness {harness:?} is not in the pinned enum"),
    )?;
    ensure(
        fixture
            .pointer("/harnessSupport/harness")
            .and_then(Value::as_str)
            == Some(harness),
        format!("{label}: harnessSupport.harness must match top-level harness"),
    )?;
    let event = fixture
        .pointer("/eventName")
        .and_then(Value::as_str)
        .unwrap_or("");
    ensure(
        expected_set(&EVENT_NAMES).contains(event),
        format!("{label}: event {event:?} is not in the pinned enum"),
    )?;
    let support_events = fixture
        .pointer("/harnessSupport/events")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label}: harnessSupport.events must be an array"))?;
    ensure(
        support_events
            .iter()
            .any(|entry| entry.as_str() == Some(event)),
        format!("{label}: harnessSupport.events must include eventName {event}"),
    )?;
    let fixture_kind = fixture
        .pointer("/fixtureKind")
        .and_then(Value::as_str)
        .unwrap_or("");
    ensure(
        expected_set(&FIXTURE_KINDS).contains(fixture_kind),
        format!("{label}: fixtureKind {fixture_kind:?} is not in the pinned enum"),
    )?;

    validate_fixture_redaction_and_budgets(fixture, label)?;
    validate_fixture_assertions(fixture, label)
}

fn validate_fixture_redaction_and_budgets(fixture: &Value, label: &str) -> TestResult {
    ensure(
        fixture
            .pointer("/input/redactionStatus")
            .and_then(Value::as_str)
            == Some(REDACTION_CONST),
        format!("{label}: input.redactionStatus must be the pinned const"),
    )?;
    ensure(
        fixture
            .pointer("/expected/localCargoFallbackAllowed")
            .and_then(Value::as_bool)
            == Some(false),
        format!("{label}: local Cargo fallback must be disallowed"),
    )?;
    ensure(
        fixture
            .pointer("/artifactPolicy/rawTranscriptAllowed")
            .and_then(Value::as_bool)
            == Some(false),
        format!("{label}: raw transcript artifacts must be disallowed"),
    )?;
    ensure(
        fixture
            .pointer("/artifactPolicy/secretMaterialAllowed")
            .and_then(Value::as_bool)
            == Some(false),
        format!("{label}: secret material artifacts must be disallowed"),
    )?;
    let output_budget = fixture
        .pointer("/expected/outputBudgetBytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{label}: outputBudgetBytes must be an integer"))?;
    ensure(
        (1..=8192).contains(&output_budget),
        format!("{label}: outputBudgetBytes {output_budget} is outside the schema budget"),
    )?;

    let transcript = &fixture["input"]["transcript"];
    let lines = transcript
        .pointer("/lines")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label}: transcript.lines must be an array"))?;
    let line_count = transcript
        .pointer("/lineCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{label}: transcript.lineCount must be an integer"))?;
    ensure(
        line_count as usize == lines.len(),
        format!("{label}: transcript.lineCount does not match lines length"),
    )?;
    let byte_count = transcript
        .pointer("/byteCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{label}: transcript.byteCount must be an integer"))?;
    ensure(
        byte_count <= 8192,
        format!("{label}: transcript.byteCount exceeds 8192"),
    )?;
    for (index, line) in lines.iter().enumerate() {
        let line = line
            .as_str()
            .ok_or_else(|| format!("{label}: transcript line {index} must be a string"))?;
        ensure(
            line.len() <= 256,
            format!("{label}: transcript line {index} exceeds 256 bytes"),
        )?;
        ensure(
            !contains_forbidden_secret_or_private_path(line),
            format!("{label}: transcript line {index} contains unredacted sensitive content"),
        )?;
    }
    Ok(())
}

fn contains_forbidden_secret_or_private_path(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("sk-")
        || lower.contains("bearer ")
        || lower.contains("begin openssh")
        || lower.contains("begin rsa")
        || lower.contains("/users/")
        || lower.contains("/home/")
}

fn validate_fixture_assertions(fixture: &Value, label: &str) -> TestResult {
    let assertion_kinds = expected_set(&ASSERTION_KINDS);
    let assertion_statuses = expected_set(&ASSERTION_STATUSES);
    let assertions = fixture
        .pointer("/assertions")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label}: assertions must be an array"))?;
    ensure(
        !assertions.is_empty(),
        format!("{label}: assertions must not be empty"),
    )?;
    for (index, assertion) in assertions.iter().enumerate() {
        let kind = assertion
            .pointer("/kind")
            .and_then(Value::as_str)
            .unwrap_or("");
        ensure(
            assertion_kinds.contains(kind),
            format!("{label}: assertions[{index}].kind {kind:?} not in enum"),
        )?;
        let status = assertion
            .pointer("/expectedStatus")
            .and_then(Value::as_str)
            .unwrap_or("");
        ensure(
            assertion_statuses.contains(status),
            format!("{label}: assertions[{index}].expectedStatus {status:?} not in enum"),
        )?;
        ensure(
            assertion
                .pointer("/message")
                .and_then(Value::as_str)
                .is_some(),
            format!("{label}: assertions[{index}] missing message"),
        )?;
    }
    Ok(())
}

#[test]
fn fixtures_round_trip_and_validate() -> TestResult {
    let schema = load_json(SCHEMA_REL)?;
    for name in FIXTURE_NAMES {
        let relative = format!("{FIXTURES_REL}/{name}.json");
        let fixture = load_json(&relative)?;
        validate_fixture(&schema, &fixture, name)?;
        let serialized = serde_json::to_string(&fixture)
            .map_err(|error| format!("{name}: serialize: {error}"))?;
        let reparsed: Value = serde_json::from_str(&serialized)
            .map_err(|error| format!("{name}: reparse: {error}"))?;
        ensure(reparsed == fixture, format!("{name}: round trip drifted"))?;
    }
    Ok(())
}

#[test]
fn fixtures_cover_matrix_events_and_assertions() -> TestResult {
    let mut harnesses = BTreeSet::new();
    let mut events = BTreeSet::new();
    let mut fixture_kinds = BTreeSet::new();
    let mut assertion_kinds = BTreeSet::new();
    let mut event_outcomes = BTreeSet::new();
    let mut saw_local_cargo_denial = false;

    for name in FIXTURE_NAMES {
        let fixture = load_json(&format!("{FIXTURES_REL}/{name}.json"))?;
        harnesses.insert(
            fixture
                .pointer("/harness")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        );
        events.insert(
            fixture
                .pointer("/eventName")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        );
        fixture_kinds.insert(
            fixture
                .pointer("/fixtureKind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        );
        event_outcomes.insert(
            fixture
                .pointer("/expected/eventOutcome")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        );
        if fixture
            .pointer("/input/commandTemplate")
            .and_then(Value::as_str)
            .is_some_and(|command| command.starts_with("cargo "))
        {
            saw_local_cargo_denial = true;
        }
        for assertion in fixture
            .pointer("/assertions")
            .and_then(Value::as_array)
            .unwrap()
        {
            assertion_kinds.insert(
                assertion
                    .pointer("/kind")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            );
        }
    }

    ensure(
        harnesses == expected_set(&HARNESS_IDS),
        format!("fixtures do not cover harness matrix: {harnesses:?}"),
    )?;
    ensure(
        events == expected_set(&EVENT_NAMES),
        format!("fixtures do not cover event names: {events:?}"),
    )?;
    ensure(
        fixture_kinds == expected_set(&FIXTURE_KINDS),
        format!("fixtures do not cover fixture taxonomy: {fixture_kinds:?}"),
    )?;
    ensure(
        assertion_kinds == expected_set(&ASSERTION_KINDS),
        format!("fixtures do not cover assertion kinds: {assertion_kinds:?}"),
    )?;
    ensure(
        event_outcomes.contains("success") && event_outcomes.contains("failure"),
        format!("fixtures must cover both success and failure event outcomes: {event_outcomes:?}"),
    )?;
    ensure(
        saw_local_cargo_denial,
        "fixtures must include a local Cargo fallback denial case",
    )
}

#[test]
fn fixture_versioning_policy_is_additive_and_major_pinned() -> TestResult {
    for name in FIXTURE_NAMES {
        let fixture = load_json(&format!("{FIXTURES_REL}/{name}.json"))?;
        ensure(
            fixture
                .pointer("/compatibility/contractMajor")
                .and_then(Value::as_u64)
                == Some(1),
            format!("{name}: contractMajor must be pinned to 1"),
        )?;
        ensure(
            fixture
                .pointer("/compatibility/fixtureVersionPolicy")
                .and_then(Value::as_str)
                == Some("additive_minor_required"),
            format!("{name}: fixtureVersionPolicy must require additive minor changes"),
        )?;
        ensure(
            fixture
                .pointer("/compatibility/compatibleWith")
                .and_then(Value::as_array)
                .is_some_and(|schemas| {
                    schemas
                        .iter()
                        .any(|schema| schema.as_str() == Some(SCHEMA_NAME))
                }),
            format!("{name}: compatibility must include {SCHEMA_NAME}"),
        )?;
    }
    Ok(())
}
