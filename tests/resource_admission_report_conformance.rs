//! bd-u7f9q.1: serialized resource-admission report schema conformance + golden matrix.
//!
//! `tests/e2e_diag_resource_admission.rs` (bd-19bq8.2) pins the safety contract
//! for the `wait_for_rch` decision with hand-coded field checks, and
//! `tests/contracts/shadow_run.rs` covers the in-memory decision vocabulary. What
//! neither pins is that the *serialized* `ee diag resource-admission --json`
//! report validates field-for-field against the authoritative wire schema
//! `docs/schemas/ee.resource_admission.v1.json` across decisions, including the
//! nested `subject`, `sourcePosture`, structured `nextCommands`, `evidence`, and
//! default-deny `redactionPosture` objects. This test closes that gap so future
//! agents do not mistake schema registration plus decision-unit tests for full
//! wire conformance: it would fail if the rendered report drifts from the
//! documented contract (renamed field, dropped required field, out-of-enum
//! value, or a violated abstain/admit/refuse conditional).
//!
//! No source changes accompany this test: the render in
//! `diag_resource_admission_value` (src/cli/mod.rs) already emits the full wire
//! shape. The conformance matrix at the bottom maps every schema MUST-field to
//! the assertion that proves it.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn schema_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
        .join("ee.resource_admission.v1.json")
}

fn load_schema() -> Result<Value, String> {
    let path = schema_path();
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

/// Run `ee diag resource-admission --json <args>` and return the report object
/// (the `data` block of the `ee.response.v2` envelope).
fn run_report(extra: &[&str]) -> Result<Value, String> {
    let mut args = vec!["--json", "diag", "resource-admission"];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    ensure(
        output.status.success(),
        format!(
            "ee {} must exit 0; stderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "machine route must keep stderr empty; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout must be JSON: {error}"))?;
    ensure(
        parsed.pointer("/schema") == Some(&Value::String("ee.response.v2".to_owned())),
        format!("unexpected response envelope: {parsed}"),
    )?;
    parsed
        .pointer("/data")
        .cloned()
        .ok_or_else(|| format!("response missing /data: {parsed}"))
}

// ---------------------------------------------------------------------------
// Focused JSON-Schema validator (Draft 2020-12 subset the schema actually uses:
// $ref/$defs, type, const, enum, required, properties, additionalProperties=false,
// oneOf, items, minItems/maxItems, contains, minimum, min/maxLength, allOf if/then).
// `pattern` is intentionally not enforced (no regex dependency in the test crate);
// the live render emits null hashes and omits patterned source schemas, so it does
// not exercise pattern, and the schema's own examples are validated below to keep
// the validator honest.
// ---------------------------------------------------------------------------

fn resolve_ref<'a>(node: &'a Value, root: &'a Value) -> &'a Value {
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        if let Some(name) = reference.strip_prefix("#/$defs/") {
            if let Some(def) = root.pointer(&format!("/$defs/{name}")) {
                return def;
            }
        }
    }
    node
}

fn single_type_matches(value: &Value, ty: &str) -> bool {
    match ty {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn type_matches(value: &Value, ty: &Value) -> bool {
    match ty {
        Value::String(name) => single_type_matches(value, name),
        Value::Array(names) => names.iter().any(|name| {
            name.as_str()
                .is_some_and(|name| single_type_matches(value, name))
        }),
        _ => true,
    }
}

fn validate(value: &Value, schema: &Value, root: &Value, path: &str, errs: &mut Vec<String>) {
    let schema = resolve_ref(schema, root);

    if let Some(constant) = schema.get("const") {
        if value != constant {
            errs.push(format!("{path}: expected const {constant}, got {value}"));
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.iter().any(|entry| entry == value) {
            errs.push(format!("{path}: value {value} not in enum {allowed:?}"));
        }
    }
    if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
        let matched = variants.iter().any(|variant| {
            let mut sub = Vec::new();
            validate(value, variant, root, path, &mut sub);
            sub.is_empty()
        });
        if !matched {
            errs.push(format!("{path}: value {value} matched none of oneOf"));
        }
        return;
    }
    if let Some(ty) = schema.get("type") {
        if !type_matches(value, ty) {
            errs.push(format!("{path}: type mismatch, want {ty}, got {value}"));
            return;
        }
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(field) {
                    errs.push(format!("{path}: missing required field '{field}'"));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            if let Some(properties) = properties {
                for key in object.keys() {
                    if !properties.contains_key(key) {
                        errs.push(format!("{path}: unexpected additional property '{key}'"));
                    }
                }
            }
        }
        if let Some(properties) = properties {
            for (key, child) in object {
                if let Some(child_schema) = properties.get(key) {
                    validate(child, child_schema, root, &format!("{path}/{key}"), errs);
                }
            }
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(items) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate(item, items, root, &format!("{path}[{index}]"), errs);
            }
        }
        if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64) {
            if array.len() as u64 > max_items {
                errs.push(format!(
                    "{path}: {} items exceeds maxItems {max_items}",
                    array.len()
                ));
            }
        }
        if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64) {
            if (array.len() as u64) < min_items {
                errs.push(format!(
                    "{path}: {} items below minItems {min_items}",
                    array.len()
                ));
            }
        }
        if let Some(contains) = schema.get("contains") {
            let satisfied = array.iter().any(|item| {
                let mut sub = Vec::new();
                validate(item, contains, root, path, &mut sub);
                sub.is_empty()
            });
            if !satisfied {
                errs.push(format!("{path}: no array item satisfies 'contains'"));
            }
        }
    }

    if let Some(text) = value.as_str() {
        let len = text.chars().count() as u64;
        if let Some(max_len) = schema.get("maxLength").and_then(Value::as_u64) {
            if len > max_len {
                errs.push(format!("{path}: length {len} exceeds maxLength {max_len}"));
            }
        }
        if let Some(min_len) = schema.get("minLength").and_then(Value::as_u64) {
            if len < min_len {
                errs.push(format!("{path}: length {len} below minLength {min_len}"));
            }
        }
    }

    if let Some(number) = value.as_i64() {
        if let Some(minimum) = schema.get("minimum").and_then(Value::as_i64) {
            if number < minimum {
                errs.push(format!("{path}: {number} below minimum {minimum}"));
            }
        }
    }

    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        for clause in all_of {
            match (clause.get("if"), clause.get("then")) {
                (Some(condition), Some(consequence)) => {
                    let mut condition_errs = Vec::new();
                    validate(value, condition, root, path, &mut condition_errs);
                    if condition_errs.is_empty() {
                        validate(value, consequence, root, path, errs);
                    }
                }
                _ => validate(value, clause, root, path, errs),
            }
        }
    }
}

fn validate_report(report: &Value, schema: &Value) -> TestResult {
    let mut errs = Vec::new();
    validate(report, schema, schema, "", &mut errs);
    ensure(
        errs.is_empty(),
        format!(
            "report does not conform to ee.resource_admission.v1:\n  - {}\nreport: {report}",
            errs.join("\n  - ")
        ),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The validator and the schema agree: every documented `examples[]` entry
/// validates. This keeps the focused validator honest against the authoritative
/// reference fixtures (which include full `wait_for_rch`, `admit`, `queue`,
/// `degrade_to_lean`, and `abstain` reports with `queuePressure`).
#[test]
fn schema_examples_validate_against_their_own_schema() -> TestResult {
    let schema = load_schema()?;
    let examples = schema
        .get("examples")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema must carry examples[]".to_owned())?;
    ensure(
        examples.len() >= 5,
        format!("expected >=5 reference examples, got {}", examples.len()),
    )?;
    let mut saw_wait = false;
    let mut saw_abstain = false;
    for (index, example) in examples.iter().enumerate() {
        let mut errs = Vec::new();
        validate(
            example,
            &schema,
            &schema,
            &format!("examples[{index}]"),
            &mut errs,
        );
        ensure(
            errs.is_empty(),
            format!(
                "schema example {index} is non-conformant:\n  - {}",
                errs.join("\n  - ")
            ),
        )?;
        match example.pointer("/decision").and_then(Value::as_str) {
            Some("wait_for_rch") => saw_wait = true,
            Some("abstain") => saw_abstain = true,
            _ => {}
        }
    }
    ensure(
        saw_wait,
        "schema examples must include a wait_for_rch report",
    )?;
    ensure(
        saw_abstain,
        "schema examples must include an abstain report",
    )
}

/// Each decision the policy can emit renders a report that validates against the
/// full wire schema. Args are chosen to deterministically force each decision
/// (see `evaluate_resource_profile_budget_admission` precedence:
/// abstain > refuse_local_cargo > wait_for_rch > degrade_to_lean > split_workload
/// > queue > admit).
#[test]
fn rendered_reports_validate_for_every_decision() -> TestResult {
    let schema = load_schema()?;

    // Every diag arg has a clap default (surface=diagnostics, host=fresh,
    // budget=within-budget, rch=not-required, local=clean, lane=clear, ...), and
    // that default set evaluates to `admit`. Each decision is forced by the
    // single posture flag that trips its branch in
    // `evaluate_resource_profile_budget_admission`, so the arg lists carry no
    // duplicates and rely on no last-wins behavior.
    let cases: &[(&str, &str, &[&str])] = &[
        ("admit", "admit", &[]),
        (
            "wait_for_rch",
            "wait_for_rch",
            &["--rch", "active-project-exclusion"],
        ),
        ("abstain", "abstain", &["--host-calibration", "missing"]),
        (
            "degrade_to_lean",
            "degrade_to_lean",
            &["--resource-budget", "recommend-decrease"],
        ),
        (
            "refuse_local_cargo",
            "refuse_local_cargo",
            &["--local-cargo", "unsafe"],
        ),
        (
            "split_workload",
            "split_workload",
            &["--estimated-cost-class", "swarm-heavy"],
        ),
        (
            "queue",
            "queue",
            &["--lane-pressure", "background-pressure"],
        ),
    ];

    for (label, expected_decision, args) in cases {
        let report = run_report(args)?;
        ensure(
            report.pointer("/decision") == Some(&Value::String((*expected_decision).to_owned())),
            format!(
                "case {label}: expected decision {expected_decision}, got {:?}",
                report.pointer("/decision")
            ),
        )?;
        validate_report(&report, &schema).map_err(|error| format!("case {label}: {error}"))?;
    }
    Ok(())
}

/// Conformance matrix: prove every schema MUST-field is present and well-typed in
/// the two decisions the bead calls out explicitly (`wait_for_rch`, `abstain`),
/// including the structured `nextCommand`/`evidenceRef` shapes — not only the
/// decision enum and reason strings.
#[test]
fn required_field_matrix_for_wait_and_abstain() -> TestResult {
    let schema = load_schema()?;

    let wait = run_report(&[
        "--surface",
        "verification",
        "--command-class",
        "verification",
        "--requested-profile",
        "swarm",
        "--effective-profile",
        "swarm",
        "--estimated-cost-class",
        "swarm-heavy",
        "--rch",
        "active-project-exclusion",
        "--local-cargo",
        "refused",
        "--lane-pressure",
        "verification-pressure",
        "--daemon",
        "not-required",
        "--replay",
        "not-required",
    ])?;
    let abstain = run_report(&["--host-calibration", "missing"])?;

    let top_required = [
        "schema",
        "sideEffectFree",
        "advisoryOnly",
        "mutationPolicy",
        "policyDomain",
        "policyId",
        "decision",
        "subject",
        "sourcePosture",
        "reasonCodes",
        "abstentionReasons",
        "nextCommands",
        "evidence",
        "redactionPosture",
        "degraded",
    ];
    let subject_required = [
        "surface",
        "commandClass",
        "requestedProfile",
        "effectiveProfile",
        "estimatedCostClass",
    ];
    let source_posture_required = [
        "hostCalibration",
        "resourceBudget",
        "rch",
        "localCargo",
        "lanePressure",
        "workloadPressure",
        "daemon",
        "replay",
    ];
    let redaction_required = [
        "rawMemoryBodyPresent",
        "rawMailBodyPresent",
        "rawCommandOutputPresent",
        "absoluteHostPathPresent",
        "fullEnvironmentPresent",
        "secretsPresent",
    ];
    let next_command_required = ["priority", "kind", "command", "destructive", "rationale"];
    let evidence_required = ["kind", "freshness", "confidence", "hash", "boundedPreview"];

    for (label, report) in [("wait_for_rch", &wait), ("abstain", &abstain)] {
        for field in top_required {
            ensure(
                report.get(field).is_some(),
                format!("{label}: missing top-level required field '{field}'"),
            )?;
        }
        let subject = report
            .get("subject")
            .and_then(Value::as_object)
            .ok_or("subject object")?;
        for field in subject_required {
            ensure(
                subject.contains_key(field),
                format!("{label}: subject missing '{field}'"),
            )?;
        }
        let posture = report
            .get("sourcePosture")
            .and_then(Value::as_object)
            .ok_or("sourcePosture object")?;
        for field in source_posture_required {
            ensure(
                posture.contains_key(field),
                format!("{label}: sourcePosture missing '{field}'"),
            )?;
        }
        // redactionPosture is default-deny: every flag must be const false.
        let redaction = report
            .get("redactionPosture")
            .and_then(Value::as_object)
            .ok_or("redactionPosture object")?;
        for field in redaction_required {
            ensure(
                redaction.get(field) == Some(&Value::Bool(false)),
                format!("{label}: redactionPosture/{field} must be false"),
            )?;
        }
        // Structured nextCommands (not bare strings).
        let next_commands = report
            .get("nextCommands")
            .and_then(Value::as_array)
            .ok_or("nextCommands array")?;
        for (index, command) in next_commands.iter().enumerate() {
            for field in next_command_required {
                ensure(
                    command.get(field).is_some(),
                    format!("{label}: nextCommands[{index}] missing structured field '{field}'"),
                )?;
            }
            ensure(
                command.get("destructive") == Some(&Value::Bool(false)),
                format!("{label}: nextCommands[{index}] must be non-destructive"),
            )?;
        }
        // Structured evidence refs (kind + freshness + confidence + hash + boundedPreview).
        let evidence = report
            .get("evidence")
            .and_then(Value::as_array)
            .ok_or("evidence array")?;
        for (index, item) in evidence.iter().enumerate() {
            for field in evidence_required {
                ensure(
                    item.get(field).is_some(),
                    format!("{label}: evidence[{index}] missing structured field '{field}'"),
                )?;
            }
        }
        // And the whole thing must validate against the schema.
        validate_report(report, &schema).map_err(|error| format!("{label}: {error}"))?;
    }
    Ok(())
}

/// The schema's decision-conditional invariants hold on rendered reports:
/// abstain carries non-empty abstentionReasons + nextCommands; admit carries an
/// empty abstentionReasons; refuse_local_cargo carries the `local_cargo_refused`
/// reason code. These are the safety-relevant `allOf`/if-then rules.
#[test]
fn decision_conditionals_hold_on_rendered_reports() -> TestResult {
    let abstain = run_report(&["--host-calibration", "missing"])?;
    ensure(
        abstain
            .pointer("/abstentionReasons")
            .and_then(Value::as_array)
            .is_some_and(|reasons| !reasons.is_empty()),
        format!("abstain must carry >=1 abstentionReasons: {abstain}"),
    )?;
    ensure(
        abstain
            .pointer("/nextCommands")
            .and_then(Value::as_array)
            .is_some_and(|commands| !commands.is_empty()),
        format!("abstain must carry >=1 nextCommands: {abstain}"),
    )?;

    // Default postures (all clear) evaluate to `admit`.
    let admit = run_report(&[])?;
    ensure(
        admit.pointer("/decision") == Some(&Value::String("admit".to_owned())),
        format!("expected admit decision: {admit}"),
    )?;
    ensure(
        admit
            .pointer("/abstentionReasons")
            .and_then(Value::as_array)
            .is_some_and(|reasons| reasons.is_empty()),
        format!("admit must carry empty abstentionReasons: {admit}"),
    )?;

    let refuse = run_report(&["--local-cargo", "unsafe"])?;
    ensure(
        refuse.pointer("/decision") == Some(&Value::String("refuse_local_cargo".to_owned())),
        format!("expected refuse_local_cargo: {refuse}"),
    )?;
    ensure(
        refuse
            .pointer("/reasonCodes")
            .and_then(Value::as_array)
            .is_some_and(|codes| {
                codes
                    .iter()
                    .any(|code| code == &json!("local_cargo_refused"))
            }),
        format!("refuse_local_cargo must list local_cargo_refused: {refuse}"),
    )
}
