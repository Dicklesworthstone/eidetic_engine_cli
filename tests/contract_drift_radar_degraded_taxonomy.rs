//! Contract drift radar — degraded-code taxonomy and recovery cross-check (bd-31nul.4).
//!
//! Adds structural and policy checks that complement the existing fixture
//! <-> doc / fixture <-> taxonomy gates (`tests/degraded_codes_doc_coverage.rs`,
//! `tests/degraded_code_taxonomy_consistency_test.rs`). Those gates prove the
//! set of codes is consistent across surfaces. This file proves each
//! individual `tests/fixtures/failure_modes/<code>.json` carries the agent-
//! actionable fields the contract requires:
//!
//! * canonical severity vocabulary — agents key automation off severity, so a
//!   typo like `medium` -> `med` would silently break alert routing;
//! * required structural fields — every non-retired fixture must carry
//!   `schema`, `code`, `surfaces[]`, `severity`, `trigger.invocation`, and a
//!   non-empty `expected_emission.message_contains[]` so an agent harness can
//!   reproduce and recognize the failure;
//! * top-level/emission parity — `expected_emission.code` must equal
//!   `.code` and `expected_emission.severity` must equal `.severity` (these
//!   diverge silently when a fixture is renamed without re-running its
//!   harness);
//! * repair-shape parity — `repair_present: true` requires at least one of
//!   `expected_emission.repair_contains` or `expected_emission.repair_strings`;
//!   `repair_present: false` requires neither (prevents prose-only recovery
//!   masquerading as structured repair).
//!
//! Inline negative fixtures drive the same validators with deliberately-
//! broken JSON to prove the radar fails closed instead of silently passing.
//!
//! On any violation the test emits one `ee.test_event.v1` JSONL row per
//! finding under the workspace tmp dir, so failure diagnostics survive
//! beyond the panic message.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

const BEAD_ID: &str = "bd-31nul.4";
const TEST_EVENT_SCHEMA: &str = "ee.test_event.v1";
const CANONICAL_SEVERITIES: &[&str] = &["info", "low", "warning", "medium", "high", "critical"];

/// Temporary repair-shape exemptions for legacy fixtures. Keep this empty
/// unless a follow-up bead cites why a fixture cannot carry structured repair
/// expectations yet.
const REPAIR_SHAPE_ALLOWLIST: &[&str] = &[];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures_dir() -> PathBuf {
    repo_root().join("tests/fixtures/failure_modes")
}

fn generated_degraded_codes_doc_path() -> PathBuf {
    repo_root().join("docs/degraded_codes.md")
}

fn degraded_codes_doc_generator_path() -> PathBuf {
    repo_root().join("scripts/build_degraded_codes_doc.sh")
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[derive(Debug, Clone)]
struct FixtureValidation {
    code: String,
    path: PathBuf,
    findings: Vec<Finding>,
}

#[derive(Debug, Clone)]
struct Finding {
    kind: &'static str,
    detail: String,
}

fn validate_failure_mode_fixture(path: &Path, value: &Value) -> FixtureValidation {
    let mut findings = Vec::new();
    let code_at_root = value.get("code").and_then(Value::as_str).map(str::to_owned);
    let code_label = code_at_root.clone().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_owned()
    });

    if value.get("schema").and_then(Value::as_str) != Some("ee.failure_mode_fixture.v1") {
        findings.push(Finding {
            kind: "schema_mismatch",
            detail: format!(
                "expected `schema` == \"ee.failure_mode_fixture.v1\"; got {:?}",
                value.get("schema")
            ),
        });
    }

    if code_at_root.is_none() {
        findings.push(Finding {
            kind: "missing_code",
            detail: "fixture missing required string field `.code`".to_owned(),
        });
    }

    let surfaces_array = value.get("surfaces").and_then(Value::as_array);
    match surfaces_array {
        None => findings.push(Finding {
            kind: "missing_surfaces",
            detail: "fixture missing required array field `.surfaces`".to_owned(),
        }),
        Some(surfaces) if surfaces.is_empty() => findings.push(Finding {
            kind: "empty_surfaces",
            detail: "fixture `.surfaces[]` must list at least one agent-facing surface".to_owned(),
        }),
        Some(surfaces) => {
            for (index, surface) in surfaces.iter().enumerate() {
                let valid = surface.as_str().is_some_and(|s| !s.trim().is_empty());
                if !valid {
                    findings.push(Finding {
                        kind: "invalid_surface_entry",
                        detail: format!(
                            "`.surfaces[{index}]` must be a non-empty string; got {surface:?}"
                        ),
                    });
                }
            }
        }
    }

    let top_severity = value.get("severity").and_then(Value::as_str);
    match top_severity {
        None => findings.push(Finding {
            kind: "missing_severity",
            detail: "fixture missing required string field `.severity`".to_owned(),
        }),
        Some(sev) if !CANONICAL_SEVERITIES.contains(&sev) => findings.push(Finding {
            kind: "severity_vocab_violation",
            detail: format!(
                "`.severity` = {sev:?} must be one of {CANONICAL_SEVERITIES:?}; agents key automation off severity, typos silently break alert routing"
            ),
        }),
        Some(_) => {}
    }

    let trigger = value.get("trigger");
    let invocation = trigger
        .and_then(|t| t.get("invocation"))
        .and_then(Value::as_str);
    if invocation.is_none() || invocation.is_some_and(|s| s.trim().is_empty()) {
        findings.push(Finding {
            kind: "missing_trigger_invocation",
            detail:
                "fixture `.trigger.invocation` must be a non-empty string so harnesses can reproduce"
                    .to_owned(),
        });
    }

    let emission = value.get("expected_emission");
    let emission_obj = emission.and_then(Value::as_object);
    if emission_obj.is_none() {
        findings.push(Finding {
            kind: "missing_expected_emission",
            detail: "fixture missing required object `.expected_emission`".to_owned(),
        });
    }

    if let (Some(obj), Some(root_code)) = (emission_obj, code_at_root.as_deref()) {
        let emission_code = obj.get("code").and_then(Value::as_str);
        if emission_code != Some(root_code) {
            findings.push(Finding {
                kind: "expected_emission_code_mismatch",
                detail: format!(
                    "`.expected_emission.code` = {emission_code:?} does not match `.code` = {root_code:?}"
                ),
            });
        }
        if let Some(top) = top_severity {
            let emission_sev = obj.get("severity").and_then(Value::as_str);
            if emission_sev != Some(top) {
                findings.push(Finding {
                    kind: "expected_emission_severity_mismatch",
                    detail: format!(
                        "`.expected_emission.severity` = {emission_sev:?} does not match `.severity` = {top:?}"
                    ),
                });
            }
        }
        let message_contains_ok = obj
            .get("message_contains")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                !items.is_empty()
                    && items
                        .iter()
                        .all(|item| item.as_str().is_some_and(|s| !s.trim().is_empty()))
            });
        if !message_contains_ok {
            findings.push(Finding {
                kind: "missing_message_contains",
                detail: "`.expected_emission.message_contains[]` must be a non-empty array of non-empty strings so agents can match the emission".to_owned(),
            });
        }

        let repair_present = value
            .get("repair_present")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let has_repair_contains = obj
            .get("repair_contains")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        let has_repair_strings = obj
            .get("repair_strings")
            .and_then(Value::as_array)
            .is_some_and(|arr| {
                !arr.is_empty()
                    && arr
                        .iter()
                        .all(|item| item.as_str().is_some_and(|s| !s.trim().is_empty()))
            });
        let allowlisted_for_repair_shape = code_at_root
            .as_deref()
            .is_some_and(|c| REPAIR_SHAPE_ALLOWLIST.contains(&c));
        if repair_present
            && !(has_repair_contains || has_repair_strings)
            && !allowlisted_for_repair_shape
        {
            findings.push(Finding {
                kind: "repair_shape_missing_structured_field",
                detail: "`.repair_present` = true but neither `.expected_emission.repair_contains` nor `.expected_emission.repair_strings` is present; prose-only recovery breaks agent automation".to_owned(),
            });
        }
        if !repair_present && (has_repair_contains || has_repair_strings) {
            findings.push(Finding {
                kind: "repair_shape_contradicts_flag",
                detail: "`.repair_present` = false but the fixture carries `repair_contains`/`repair_strings`; either flip the flag or remove the strings".to_owned(),
            });
        }
    }

    FixtureValidation {
        code: code_label,
        path: path.to_path_buf(),
        findings,
    }
}

fn collect_fixture_validations() -> Result<Vec<FixtureValidation>, String> {
    let dir = fixtures_dir();
    let mut results = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("parse {} as JSON: {e}", path.display()))?;
        if value
            .get("retired")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            // Retired fixtures are kept for history and intentionally allowed to lag
            // the structural contract.
            continue;
        }
        results.push(validate_failure_mode_fixture(&path, &value));
    }
    if results.is_empty() {
        return Err(format!(
            "no failure-mode fixtures parsed under {}",
            dir.display(),
        ));
    }
    Ok(results)
}

fn event_log_path() -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target"));
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = base
        .join("ee-contract-drift-radar")
        .join(format!("bd-31nul-4-{}-{nonce}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    dir.join("findings.jsonl")
}

fn emit_event(
    log_path: &Path,
    scenario: &str,
    phase: &str,
    status: &str,
    fixture_code: &str,
    fixture_path: &Path,
    finding_kind: &str,
    detail: &str,
) -> TestResult {
    let event = json!({
        "schema": TEST_EVENT_SCHEMA,
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "bead_id": BEAD_ID,
        "scenario": scenario,
        "phase": phase,
        "status": status,
        "fixtureCode": fixture_code,
        "fixturePath": fixture_path.display().to_string(),
        "findingKind": finding_kind,
        "detail": detail,
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("open jsonl log {}: {e}", log_path.display()))?;
    serde_json::to_writer(&mut file, &event).map_err(|e| format!("serialize test event: {e}"))?;
    file.write_all(b"\n").map_err(|e| format!("newline: {e}"))?;
    Ok(())
}

fn emit_findings(
    log_path: &Path,
    scenario: &str,
    validations: &[&FixtureValidation],
) -> TestResult {
    for v in validations {
        for finding in &v.findings {
            emit_event(
                log_path,
                scenario,
                "violation",
                "fail",
                &v.code,
                &v.path,
                finding.kind,
                &finding.detail,
            )?;
        }
    }
    Ok(())
}

fn report_violations(
    scenario: &str,
    validations: &[&FixtureValidation],
    kinds: &[&'static str],
) -> TestResult {
    let log = event_log_path();
    emit_findings(&log, scenario, validations)?;

    let mut grouped: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for v in validations {
        for finding in &v.findings {
            if kinds.contains(&finding.kind) {
                grouped.entry(finding.kind).or_default().push(format!(
                    "{} ({}): {}",
                    v.code,
                    v.path.display(),
                    finding.detail
                ));
            }
        }
    }

    if grouped.is_empty() {
        return Ok(());
    }

    let mut buf = format!(
        "[{scenario}] degraded-code taxonomy drift: {} fixture(s) violate the contract. \
         ee.test_event.v1 breadcrumbs at {}.\n",
        grouped.values().map(Vec::len).sum::<usize>(),
        log.display(),
    );
    for (kind, items) in grouped {
        buf.push_str(&format!("\n  [{}] x{}\n", kind, items.len()));
        for item in items.iter().take(10) {
            buf.push_str(&format!("    - {item}\n"));
        }
        if items.len() > 10 {
            buf.push_str(&format!("    ... ({} more)\n", items.len() - 10));
        }
    }
    Err(buf)
}

// --- positive tests over the live fixture set ---

#[test]
fn every_fixture_severity_is_canonical_vocab() -> TestResult {
    let validations = collect_fixture_validations()?;
    let refs: Vec<&FixtureValidation> = validations.iter().collect();
    report_violations(
        "every_fixture_severity_is_canonical_vocab",
        &refs,
        &["severity_vocab_violation", "missing_severity"],
    )
}

#[test]
fn every_fixture_has_required_structural_fields() -> TestResult {
    let validations = collect_fixture_validations()?;
    let refs: Vec<&FixtureValidation> = validations.iter().collect();
    report_violations(
        "every_fixture_has_required_structural_fields",
        &refs,
        &[
            "schema_mismatch",
            "missing_code",
            "missing_surfaces",
            "empty_surfaces",
            "invalid_surface_entry",
            "missing_trigger_invocation",
            "missing_expected_emission",
            "missing_message_contains",
        ],
    )
}

#[test]
fn every_fixture_emission_matches_top_level() -> TestResult {
    let validations = collect_fixture_validations()?;
    let refs: Vec<&FixtureValidation> = validations.iter().collect();
    report_violations(
        "every_fixture_emission_matches_top_level",
        &refs,
        &[
            "expected_emission_code_mismatch",
            "expected_emission_severity_mismatch",
        ],
    )
}

#[test]
fn every_fixture_repair_shape_matches_repair_present() -> TestResult {
    let validations = collect_fixture_validations()?;
    let refs: Vec<&FixtureValidation> = validations.iter().collect();
    report_violations(
        "every_fixture_repair_shape_matches_repair_present",
        &refs,
        &[
            "repair_shape_missing_structured_field",
            "repair_shape_contradicts_flag",
        ],
    )
}

#[test]
fn generated_degraded_codes_doc_uses_taxonomy_categories() -> TestResult {
    let doc_path = generated_degraded_codes_doc_path();
    let generator_path = degraded_codes_doc_generator_path();
    let doc = fs::read_to_string(&doc_path)
        .map_err(|error| format!("read {}: {error}", doc_path.display()))?;
    let generator = fs::read_to_string(&generator_path)
        .map_err(|error| format!("read {}: {error}", generator_path.display()))?;

    for (label, path, content) in [
        ("generated doc", doc_path.as_path(), doc.as_str()),
        ("generator", generator_path.as_path(), generator.as_str()),
    ] {
        ensure(
            !content.contains("affects_this_response"),
            format!(
                "{label} {} references obsolete taxonomy category `affects_this_response`; use `response_time`, `mixed`, or `build_time`",
                path.display()
            ),
        )?;
        ensure(
            content.contains("`response_time` codes")
                && content.contains("response-time half of `mixed`"),
            format!(
                "{label} {} must explain that default degraded[] contains `response_time` codes plus the response-time half of `mixed`",
                path.display()
            ),
        )?;
    }
    Ok(())
}

// --- negative fixtures: prove the radar fails closed ---

fn run_validator_inline(value: Value) -> FixtureValidation {
    let path = PathBuf::from("<inline-negative-fixture>");
    validate_failure_mode_fixture(&path, &value)
}

#[test]
fn contract_drift_radar_pins_json_example_count_semantics() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("ee-contract-radar-counts-")
        .tempdir_in("/tmp")
        .or_else(|_| tempfile::tempdir())
        .map_err(|error| format!("create tempdir: {error}"))?;
    let root = temp.path();
    let scripts_dir = root.join("scripts");
    let schemas_dir = root.join("docs").join("schemas");
    fs::create_dir_all(&scripts_dir).map_err(|error| format!("create scripts dir: {error}"))?;
    fs::create_dir_all(&schemas_dir).map_err(|error| format!("create schemas dir: {error}"))?;
    fs::create_dir_all(root.join("docs")).map_err(|error| format!("create docs dir: {error}"))?;

    fs::copy(
        repo_root().join("scripts").join("contract-drift-radar.sh"),
        scripts_dir.join("contract-drift-radar.sh"),
    )
    .map_err(|error| format!("copy contract-drift-radar.sh: {error}"))?;
    fs::write(schemas_dir.join("ee.response.v2.json"), "{}\n")
        .map_err(|error| format!("write schema inventory fixture: {error}"))?;
    fs::write(
        root.join("docs").join("contract-drift-radar.md"),
        r#"# Canned Contract Examples

```json
{ "schema": "ee.response.v2", "success": true }
```

```jsonc
{ "schema": "ee.unknown_contract.v1", "success": true }
```

<!-- legacy-example -->
```json
{ "schema": "ee.legacy_allowed.v1", "success": true }
```
"#,
    )
    .map_err(|error| format!("write canned docs fixture: {error}"))?;

    let output = Command::new("bash")
        .arg(root.join("scripts").join("contract-drift-radar.sh"))
        .arg("--json")
        .arg("--quiet")
        .output()
        .map_err(|error| format!("run contract-drift-radar.sh: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "contract-drift-radar.sh failed: status={:?}\nstderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("radar stdout was not UTF-8: {error}"))?;
    let report: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("radar stdout was not JSON: {error}\nstdout={stdout}"))?;
    let summary = report
        .get("summary")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("summary object missing from report: {report}"))?;

    ensure(
        summary
            .get("envelopeExamplesScanned")
            .and_then(Value::as_u64)
            == Some(3),
        format!("expected 3 scanned examples; summary={summary:?}"),
    )?;
    ensure(
        summary.get("schemaIdViolations").and_then(Value::as_u64) == Some(1),
        format!("expected 1 unknown-schema violation; summary={summary:?}"),
    )?;
    ensure(
        summary.get("skippedLegacyExamples").and_then(Value::as_u64) == Some(1),
        format!("expected 1 skipped legacy example; summary={summary:?}"),
    )?;
    ensure(
        report
            .pointer("/violations/jsonExampleCheck")
            .and_then(Value::as_array)
            .is_some_and(|violations| violations.len() == 1),
        format!("expected exactly one json example violation: {report}"),
    )
}

#[test]
fn negative_fixture_with_nonstandard_severity_is_flagged() -> TestResult {
    let value = json!({
        "schema": "ee.failure_mode_fixture.v1",
        "code": "test_negative_severity",
        "surfaces": ["test"],
        "severity": "moderate",
        "repair_present": false,
        "trigger": { "invocation": "ee diag stub --json" },
        "expected_emission": {
            "code": "test_negative_severity",
            "severity": "moderate",
            "message_contains": ["stub"],
        }
    });
    let v = run_validator_inline(value);
    let kinds: BTreeSet<&str> = v.findings.iter().map(|f| f.kind).collect();
    ensure(
        kinds.contains("severity_vocab_violation"),
        format!("expected severity_vocab_violation; got {kinds:?}"),
    )?;
    // Top-level + emission both use the bogus severity so we expect the
    // emission-mismatch check to NOT fire (they agree on `moderate`).
    ensure(
        !kinds.contains("expected_emission_severity_mismatch"),
        format!(
            "non-standard severity that matches between root and emission should NOT trip the mismatch check; got {kinds:?}",
        ),
    )
}

#[test]
fn negative_fixture_with_emission_code_mismatch_is_flagged() -> TestResult {
    let value = json!({
        "schema": "ee.failure_mode_fixture.v1",
        "code": "renamed_root_only",
        "surfaces": ["test"],
        "severity": "warning",
        "repair_present": false,
        "trigger": { "invocation": "ee diag stub --json" },
        "expected_emission": {
            "code": "old_name_in_emission",
            "severity": "warning",
            "message_contains": ["stub"],
        }
    });
    let v = run_validator_inline(value);
    let kinds: BTreeSet<&str> = v.findings.iter().map(|f| f.kind).collect();
    ensure(
        kinds.contains("expected_emission_code_mismatch"),
        format!("expected expected_emission_code_mismatch; got {kinds:?}"),
    )
}

#[test]
fn negative_fixture_with_prose_only_repair_is_flagged() -> TestResult {
    let value = json!({
        "schema": "ee.failure_mode_fixture.v1",
        "code": "prose_only_repair_case",
        "surfaces": ["test"],
        "severity": "warning",
        "repair_present": true,
        "trigger": { "invocation": "ee diag stub --json" },
        "expected_emission": {
            "code": "prose_only_repair_case",
            "severity": "warning",
            "message_contains": ["stub"],
            // Prose-only "repair" embedded in the message is not structured;
            // repair_contains / repair_strings are absent on purpose.
        }
    });
    let v = run_validator_inline(value);
    let kinds: BTreeSet<&str> = v.findings.iter().map(|f| f.kind).collect();
    ensure(
        kinds.contains("repair_shape_missing_structured_field"),
        format!("expected repair_shape_missing_structured_field; got {kinds:?}"),
    )
}

#[test]
fn negative_fixture_with_repair_strings_but_flag_false_is_flagged() -> TestResult {
    let value = json!({
        "schema": "ee.failure_mode_fixture.v1",
        "code": "flag_strings_contradiction",
        "surfaces": ["test"],
        "severity": "warning",
        "repair_present": false,
        "trigger": { "invocation": "ee diag stub --json" },
        "expected_emission": {
            "code": "flag_strings_contradiction",
            "severity": "warning",
            "message_contains": ["stub"],
            "repair_contains": "ee repair --json",
        }
    });
    let v = run_validator_inline(value);
    let kinds: BTreeSet<&str> = v.findings.iter().map(|f| f.kind).collect();
    ensure(
        kinds.contains("repair_shape_contradicts_flag"),
        format!("expected repair_shape_contradicts_flag; got {kinds:?}"),
    )
}

#[test]
fn negative_fixture_with_empty_message_contains_is_flagged() -> TestResult {
    let value = json!({
        "schema": "ee.failure_mode_fixture.v1",
        "code": "empty_message_contains_case",
        "surfaces": ["test"],
        "severity": "warning",
        "repair_present": false,
        "trigger": { "invocation": "ee diag stub --json" },
        "expected_emission": {
            "code": "empty_message_contains_case",
            "severity": "warning",
            "message_contains": [],
        }
    });
    let v = run_validator_inline(value);
    let kinds: BTreeSet<&str> = v.findings.iter().map(|f| f.kind).collect();
    ensure(
        kinds.contains("missing_message_contains"),
        format!("expected missing_message_contains; got {kinds:?}"),
    )
}

#[test]
fn negative_fixture_with_missing_invocation_is_flagged() -> TestResult {
    let value = json!({
        "schema": "ee.failure_mode_fixture.v1",
        "code": "no_invocation_case",
        "surfaces": ["test"],
        "severity": "warning",
        "repair_present": false,
        "trigger": { "setup_commands": ["ee init"] },
        "expected_emission": {
            "code": "no_invocation_case",
            "severity": "warning",
            "message_contains": ["stub"],
        }
    });
    let v = run_validator_inline(value);
    let kinds: BTreeSet<&str> = v.findings.iter().map(|f| f.kind).collect();
    ensure(
        kinds.contains("missing_trigger_invocation"),
        format!("expected missing_trigger_invocation; got {kinds:?}"),
    )
}
