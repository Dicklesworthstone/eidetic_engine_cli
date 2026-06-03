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
