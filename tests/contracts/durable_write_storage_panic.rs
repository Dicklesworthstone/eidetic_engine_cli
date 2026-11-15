//! Contract coverage for recovered storage panics on durable write commands.

use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};
use ee::models::{DomainError, ERROR_SCHEMA_V2, ProcessExitCode};
use ee::output::error_response_json;
use serde_json::Value;

type TestResult = Result<(), String>;

fn audited_durable_write_paths() -> Vec<&'static str> {
    let manifest = EffectManifest::build();
    let mut paths = manifest
        .mutating_commands()
        .into_iter()
        .filter(|effect| {
            effect.default_effect == EffectClass::DurableMemoryWrite
                && effect.mutation_contract.side_effect_class == SideEffectClass::AuditedMutation
        })
        .map(|effect| effect.command_path)
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths
}

#[test]
fn durable_write_storage_panic_contract_has_expected_surface() -> TestResult {
    let paths = audited_durable_write_paths();
    if paths.is_empty() {
        return Err("durable write contract surface must not be empty".to_owned());
    }

    for expected in [
        "remember",
        "memory revise",
        "curate apply",
        "workflow close",
    ] {
        if !paths.contains(&expected) {
            return Err(format!(
                "durable write contract surface is missing expected command `{expected}`; paths={paths:?}"
            ));
        }
    }

    if paths.contains(&"pack build") {
        return Err(
            "append-only `pack build` must not be classified as audited durable_write".into(),
        );
    }

    Ok(())
}

#[test]
fn durable_write_storage_panics_render_storage_exit_and_error_v2() -> TestResult {
    let paths = audited_durable_write_paths();
    let mut failures = Vec::new();

    for path in paths {
        let error = DomainError::Storage {
            message: format!(
                "{path} durable_write recovered StoragePanic from FrankenSQLite row assembly"
            ),
            repair: Some("Run ee doctor --json for storage diagnostics.".to_owned()),
        };

        if error.exit_code() != ProcessExitCode::Storage {
            failures.push(format!(
                "{path}: expected storage exit 3, got {:?}",
                error.exit_code()
            ));
            continue;
        }

        let rendered = error_response_json(&error);
        let parsed: Value = serde_json::from_str(&rendered)
            .map_err(|parse_error| format!("{path}: error JSON parses: {parse_error}"))?;

        if parsed.get("schema").and_then(Value::as_str) != Some(ERROR_SCHEMA_V2) {
            failures.push(format!(
                "{path}: expected top-level schema {ERROR_SCHEMA_V2}"
            ));
        }
        if parsed.pointer("/error/code").and_then(Value::as_str) != Some("storage") {
            failures.push(format!("{path}: expected /error/code storage"));
        }
        if parsed.pointer("/error/severity").and_then(Value::as_str) != Some("high") {
            failures.push(format!("{path}: expected /error/severity high"));
        }
        let message = parsed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !message.contains(path) || !message.contains("recovered StoragePanic") {
            failures.push(format!(
                "{path}: error message must identify command and recovered StoragePanic, got {message:?}"
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "durable_write StoragePanic mapping contract failed:\n  {}",
            failures.join("\n  ")
        ))
    }
}

#[test]
fn durable_write_handlers_convert_injected_storage_panic_to_storage_exit_and_error_v2() -> TestResult
{
    let temp = tempfile::tempdir().map_err(|err| format!("tempdir: {err}"))?;
    let workspace = temp
        .path()
        .canonicalize()
        .map_err(|err| format!("canonicalize workspace: {err}"))?;
    let ws = workspace.to_string_lossy().into_owned();

    let mut init_stdout = Vec::new();
    let mut init_stderr = Vec::new();
    let init_exit = ee::cli::run(
        ["ee", "init", "--workspace", ws.as_str(), "--json"].map(std::ffi::OsString::from),
        &mut init_stdout,
        &mut init_stderr,
    );
    if init_exit != ProcessExitCode::Success {
        return Err(format!(
            "ee init failed with {init_exit:?}: stdout={} stderr={}",
            String::from_utf8_lossy(&init_stdout),
            String::from_utf8_lossy(&init_stderr),
        ));
    }

    let panic_payload = "range end index 27 out of range for slice of length 26";
    let cases: &[(&str, &[&str])] = &[
        (
            "remember (direct DomainError::Storage mapping)",
            &[
                "ee",
                "remember",
                "storage panic probe",
                "--workspace",
                ws.as_str(),
                "--json",
            ],
        ),
        (
            "workflow close (direct DomainError::Storage mapping)",
            &[
                "ee",
                "workflow",
                "close",
                "wf_01234567890123456789012345",
                "--workspace",
                ws.as_str(),
                "--json",
            ],
        ),
        (
            "curate apply (db_failure_or_storage_panic over MigrationRequired)",
            &[
                "ee",
                "curate",
                "apply",
                "curate_01234567890123456789012345",
                "--workspace",
                ws.as_str(),
                "--json",
            ],
        ),
        (
            "journal append (db_failure_or_storage_panic over MigrationRequired)",
            &[
                "ee",
                "journal",
                "append",
                "storage panic probe",
                "--workspace",
                ws.as_str(),
                "--json",
            ],
        ),
        (
            "curate doctor (db_failure_or_storage_panic over MigrationRequired)",
            &[
                "ee",
                "curate",
                "doctor",
                "--workspace",
                ws.as_str(),
                "--json",
            ],
        ),
        (
            "causal promote-plan (db_failure_or_storage_panic over MigrationRequired)",
            &[
                "ee",
                "causal",
                "promote-plan",
                "chn_01234567890123456789012345",
                "--workspace",
                ws.as_str(),
                "--json",
            ],
        ),
    ];

    for (label, argv) in cases {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = ee::db::with_injected_storage_panic(None, panic_payload, || {
            ee::cli::run(
                argv.iter().copied().map(std::ffi::OsString::from),
                &mut stdout,
                &mut stderr,
            )
        });
        let raw_stdout = String::from_utf8_lossy(&stdout);
        let raw_stderr = String::from_utf8_lossy(&stderr);
        if exit != ProcessExitCode::Storage {
            return Err(format!(
                "{label}: expected ProcessExitCode::Storage (exit 3), got {exit:?}\nstdout: {raw_stdout}\nstderr: {raw_stderr}"
            ));
        }

        let parsed: Value = serde_json::from_str(&raw_stdout).map_err(|err| {
            format!(
                "{label}: expected valid JSON error envelope on stdout: {err}\nstdout: {raw_stdout}"
            )
        })?;
        if parsed.get("schema").and_then(Value::as_str) != Some(ERROR_SCHEMA_V2) {
            return Err(format!(
                "{label}: expected top-level schema {ERROR_SCHEMA_V2}, got {parsed}"
            ));
        }
        if parsed.pointer("/error/code").and_then(Value::as_str) != Some("storage") {
            return Err(format!(
                "{label}: expected /error/code == \"storage\", got {parsed}"
            ));
        }
        if parsed.pointer("/error/severity").and_then(Value::as_str) != Some("high") {
            return Err(format!(
                "{label}: expected /error/severity == \"high\", got {parsed}"
            ));
        }
        let message = parsed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if (!message.contains("recovered from panic") && !message.contains("storage panic"))
            || !message.contains(panic_payload)
        {
            return Err(format!(
                "{label}: expected /error/message to carry recovered StoragePanic payload, got {message:?}"
            ));
        }
    }

    // Negative/recovery twin: after the one-shot injected panics above, a normal
    // durable_write succeeds and proves neither the thread-local injection state
    // nor the workspace write-owner lock was poisoned or orphaned.
    let mut ok_stdout = Vec::new();
    let mut ok_stderr = Vec::new();
    let ok_exit = ee::cli::run(
        [
            "ee",
            "remember",
            "post-panic healthy write",
            "--workspace",
            ws.as_str(),
            "--json",
        ]
        .map(std::ffi::OsString::from),
        &mut ok_stdout,
        &mut ok_stderr,
    );
    if ok_exit != ProcessExitCode::Success {
        return Err(format!(
            "post-panic healthy remember failed with {ok_exit:?}: stdout={} stderr={}",
            String::from_utf8_lossy(&ok_stdout),
            String::from_utf8_lossy(&ok_stderr),
        ));
    }
    let ok_json: Value = serde_json::from_slice(&ok_stdout)
        .map_err(|err| format!("post-panic remember JSON parse: {err}"))?;
    if ok_json.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "expected post-panic remember success=true, got {ok_json}"
        ));
    }

    Ok(())
}
