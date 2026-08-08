//! Contract and golden coverage for journal capture (`bd-1pi9m.6`).

use std::path::{Path, PathBuf};
use std::process::Command;

use ee::core::journal::{JOURNAL_DISTILL_SCHEMA_V1, JOURNAL_ENTRY_SCHEMA_V1};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_text(relative: &str) -> Result<String, String> {
    std::fs::read_to_string(repo_path(relative))
        .map_err(|error| format!("read {relative}: {error}"))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let text = read_text(relative)?;
    serde_json::from_str(&text).map_err(|error| format!("parse {relative}: {error}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn assert_json_fixture(relative: &str, actual: Value, label: &str) -> TestResult {
    let expected = read_json(relative)?;
    ensure(
        actual == expected,
        format!(
            "{label} fixture mismatch\nexpected:\n{}\nactual:\n{}",
            serde_json::to_string_pretty(&expected).unwrap_or_else(|_| expected.to_string()),
            serde_json::to_string_pretty(&actual).unwrap_or_else(|_| actual.to_string()),
        ),
    )
}

fn journal_entry_fixture() -> Value {
    json!({
        "schema": JOURNAL_ENTRY_SCHEMA_V1,
        "entryId": "jrn_018ff8dc-3d85-7cc0-a6fd-a2d479e6d401",
        "workspaceId": "wsp_journal_capture_fixture",
        "agentName": "OrangePike",
        "sessionKey": "journal-capture-fixture",
        "kind": "command_failure",
        "source": "hook",
        "body": "cargo test journal capture failed: linker cache missing object after retry zero",
        "structured": {
            "cmd": "cargo test --lib journal_capture",
            "cwd": "/workspace",
            "exitCode": 101,
            "paths": ["src/core/journal.rs"],
            "stderrTail": "error: linker cache missing object"
        },
        "redactionReport": {
            "classesApplied": [],
            "spanCount": 0
        },
        "instructionRisk": "low",
        "createdAt": "2026-06-12T12:00:00Z",
        "distilledAt": null,
        "tombstonedAt": null
    })
}

fn append_golden() -> Value {
    json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "command": "journal append",
            "version": env!("CARGO_PKG_VERSION"),
            "status": "stored",
            "entry": journal_entry_fixture(),
            "truncated": false,
            "redactionApplied": false,
            "degraded": []
        }
    })
}

fn batch_golden() -> Value {
    json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "command": "journal append",
            "version": env!("CARGO_PKG_VERSION"),
            "status": "stored",
            "lineCount": 3,
            "storedCount": 2,
            "failedCount": 1,
            "results": [
                {
                    "line": 1,
                    "status": "stored",
                    "entryId": "jrn_018ff8dc-3d85-7cc0-a6fd-a2d479e6d402",
                    "errorCode": null,
                    "errorMessage": null,
                    "truncated": false,
                    "redactionApplied": false
                },
                {
                    "line": 2,
                    "status": "failed",
                    "entryId": null,
                    "errorCode": "journal_body_required",
                    "errorMessage": "journal entry body must not be empty",
                    "truncated": false,
                    "redactionApplied": false
                },
                {
                    "line": 3,
                    "status": "stored",
                    "entryId": "jrn_018ff8dc-3d85-7cc0-a6fd-a2d479e6d403",
                    "errorCode": null,
                    "errorMessage": null,
                    "truncated": false,
                    "redactionApplied": true
                }
            ],
            "degraded": [
                {
                    "code": "journal_redaction_applied",
                    "severity": "info",
                    "message": "Redaction screen replaced 1 secret-like span(s) [api_key] before persistence; the stored entry contains placeholders only."
                }
            ]
        }
    })
}

fn distill_golden() -> Value {
    json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "schema": JOURNAL_DISTILL_SCHEMA_V1,
            "command": "journal distill",
            "version": env!("CARGO_PKG_VERSION"),
            "status": "ok",
            "workspaceId": "wsp_journal_capture_fixture",
            "scannedCount": 4,
            "scope": {
                "session": "journal-capture-fixture",
                "agent": null,
                "since": null
            },
            "dryRun": true,
            "proposals": [
                {
                    "proposalId": "prop_journal_capture_fixture",
                    "action": "create_candidate",
                    "targetMemoryId": null,
                    "level": "episodic",
                    "kind": "failure",
                    "contentDraft": "Recurring command failure: cargo test --lib journal_capture failed with linker cache missing object.",
                    "typedFields": {
                        "family": "cargo test",
                        "cause": "linker cache missing"
                    },
                    "evidence": [
                        "journal://jrn_018ff8dc-3d85-7cc0-a6fd-a2d479e6d401",
                        "journal://jrn_018ff8dc-3d85-7cc0-a6fd-a2d479e6d402",
                        "journal://jrn_018ff8dc-3d85-7cc0-a6fd-a2d479e6d403"
                    ],
                    "clusterSize": 3,
                    "dedup": {
                        "nearestMemoryId": null,
                        "similarity": null
                    }
                }
            ],
            "abstentions": [
                {
                    "entryId": "jrn_018ff8dc-3d85-7cc0-a6fd-a2d479e6d404",
                    "reason": "instruction_risk_excluded"
                }
            ],
            "applied": null,
            "degraded": []
        }
    })
}

#[test]
fn journal_capture_goldens_pin_append_batch_and_distill_shapes() -> TestResult {
    assert_json_fixture(
        "tests/fixtures/golden/journal_capture/append_command_failure.json.golden",
        append_golden(),
        "journal append",
    )?;
    assert_json_fixture(
        "tests/fixtures/golden/journal_capture/batch_mixed_lines.json.golden",
        batch_golden(),
        "journal batch",
    )?;
    assert_json_fixture(
        "tests/fixtures/golden/journal_capture/distill_dry_run.json.golden",
        distill_golden(),
        "journal distill",
    )
}

#[test]
fn journal_schema_registry_exports_capture_contracts() -> TestResult {
    let exported = ee::output::public_schemas()
        .iter()
        .filter(|schema| {
            schema.id == JOURNAL_ENTRY_SCHEMA_V1 || schema.id == JOURNAL_DISTILL_SCHEMA_V1
        })
        .collect::<Vec<_>>();
    ensure(exported.len() == 2, "journal schemas must be public")?;
    ensure(
        exported.iter().all(|schema| schema.category == "memory"),
        "journal schemas must be categorized as memory",
    )?;

    let entry_schema = read_json("docs/schemas/ee.journal.entry.v1.json")?;
    let distill_schema = read_json("docs/schemas/ee.journal.distill.v1.json")?;
    ensure(
        entry_schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(JOURNAL_ENTRY_SCHEMA_V1),
        "journal entry docs schema must pin ee.journal.entry.v1",
    )?;
    ensure(
        distill_schema
            .pointer("/properties/schema/const")
            .and_then(Value::as_str)
            == Some(JOURNAL_DISTILL_SCHEMA_V1),
        "journal distill docs schema must pin ee.journal.distill.v1",
    )
}

#[test]
fn e2e_journal_capture_script_exercises_full_capture_lifecycle() -> TestResult {
    let script = read_text("scripts/e2e_journal_capture.sh")?;
    for needle in [
        "harness_init \"journal_capture\"",
        "journal append",
        "--stdin --source stdin --json",
        "assert_database_omits_secret",
        "journal distill --session",
        "journal distill --session \"$SESSION\" --apply --json",
        "curate validate",
        "curate apply",
        "pack \"linker cache missing object journal capture\"",
        "outcome trace",
        "harness_summary",
    ] {
        ensure(
            script.contains(needle),
            format!("scripts/e2e_journal_capture.sh must contain `{needle}`"),
        )?;
    }
    Ok(())
}

#[test]
fn e2e_journal_capture_cannot_mask_nonzero_ee_commands() -> TestResult {
    let script = read_text("scripts/e2e_journal_capture.sh")?;
    let helpers = script
        .split_once("ee_json() {")
        .and_then(|(_, rest)| rest.split_once("json_value() {").map(|(body, _)| body))
        .ok_or("journal capture command helpers must remain discoverable")?;

    ensure(
        script.contains("EE_JSON_FAILURES_FILE=\"$LOG_DIR/command-failures.log\""),
        "journal capture must persist failures across command-substitution subshells",
    )?;
    ensure(
        helpers.matches("return \"$rc\"").count() == 2,
        "both journal capture command helpers must propagate the command exit code",
    )?;
    ensure(
        !helpers.contains("|| true"),
        "journal capture command helpers must not swallow nonzero exits",
    )?;
    ensure(
        script.contains("_harness_fail \"logged command failure: $command_failure\""),
        "journal capture must fold durable command failures into harness_summary",
    )
}

#[test]
fn e2e_journal_capture_records_success_shaped_nonzero_results() -> TestResult {
    let script = read_text("scripts/e2e_journal_capture.sh")?;
    let helpers = script
        .split_once("ee_json() {")
        .and_then(|(_, rest)| rest.split_once("json_value() {").map(|(body, _)| body))
        .ok_or("journal capture command helpers must remain discoverable")?;
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let failures = temp.path().join("command-failures.log");
    let mut probe = String::from(
        r#"
set -uo pipefail
EE_BIN=fake-ee
e2e_log_note() { :; }
e2e_log_command() {
    printf '%s\n' '{"schema":"ee.response.v2","success":true}'
    return 7
}
ee_json() {
"#,
    );
    probe.push_str(helpers);
    probe.push_str(
        r#"
json="$(ee_json status --json)"
rc=$?
[ "$rc" -eq 7 ] || exit 20
[ "$json" = '{"schema":"ee.response.v2","success":true}' ] || exit 21
stdin_json="$(ee_json_stdin '{}' journal append --stdin --json)"
stdin_rc=$?
[ "$stdin_rc" -eq 7 ] || exit 22
[ "$stdin_json" = '{"schema":"ee.response.v2","success":true}' ] || exit 23
[ "$(wc -l <"$EE_JSON_FAILURES_FILE")" -eq 2 ] || exit 24
grep -q '^exit=7 command=status --json$' "$EE_JSON_FAILURES_FILE" || exit 25
grep -q '^exit=7 stdin=true command=journal append --stdin --json$' "$EE_JSON_FAILURES_FILE" || exit 26
"#,
    );
    let output = Command::new("bash")
        .arg("-c")
        .arg(probe)
        .env("EE_JSON_FAILURES_FILE", &failures)
        .output()
        .map_err(|error| format!("run journal command-failure probe: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "success-shaped nonzero command probe failed with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[test]
fn journal_capture_contract_fixture_lists_required_evidence() -> TestResult {
    let fixture = read_json("tests/fixtures/contracts/journal_capture/coverage.json")?;
    ensure(
        fixture["schema"].as_str() == Some("ee.journal_capture.contract.v1"),
        "coverage fixture schema",
    )?;
    ensure(
        fixture["bead"].as_str() == Some("bd-1pi9m.6"),
        "coverage fixture bead",
    )?;
    for pointer in [
        "/goldens/0",
        "/goldens/1",
        "/goldens/2",
        "/propertyAxes/0",
        "/propertyAxes/1",
        "/e2eScript",
    ] {
        ensure(
            fixture.pointer(pointer).is_some(),
            format!("coverage fixture missing {pointer}"),
        )?;
    }
    for golden in fixture["goldens"]
        .as_array()
        .ok_or("coverage fixture goldens must be an array")?
    {
        let path = golden.as_str().ok_or("golden path must be a string")?;
        ensure(
            repo_path(path).is_file(),
            format!("golden path missing: {path}"),
        )?;
    }
    Ok(())
}

#[test]
fn pack_ledger_missing_failure_mode_fixture_is_catalogued() -> TestResult {
    let fixture = read_json("tests/fixtures/failure_modes/pack_ledger_missing.json")?;
    ensure(
        fixture["schema"].as_str() == Some("ee.failure_mode_fixture.v1"),
        "fixture schema",
    )?;
    ensure(
        fixture["code"].as_str() == Some("pack_ledger_missing"),
        "fixture code",
    )?;
    ensure(
        fixture["expected_emission"]["message_contains"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.as_str() == Some("no persisted replay ledger"))
            }),
        "fixture must pin the replay-ledger absence message",
    )
}
