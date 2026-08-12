//! Golden snapshot tests for ee.error.v2 JSON envelope contracts.
//!
//! These tests ensure all DomainError variants produce stable JSON output
//! conforming to the documented error schema in AGENTS.md:
//!
//! ```json
//! {
//!   "schema": "ee.error.v2",
//!   "error": {
//!     "code": "<error_code>",
//!     "message": "<description>",
//!     "severity": "info|low|warning|medium|high|critical",
//!     "repair": "<optional command>",
//!     "details": { ... }
//!   }
//! }
//! ```

use ee::models::{DomainError, RecoveryAction, RecoveryKind};
use ee::output::error_response_json;
use insta::assert_json_snapshot;
use serde_json::Value;

type TestResult = Result<(), String>;

fn parse_error_json(json: &str) -> Result<Value, String> {
    serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))
}

fn verify_error_envelope(json: &str) -> TestResult {
    let value = parse_error_json(json)?;
    let obj = value.as_object().ok_or("expected object at root")?;

    if obj.get("schema") != Some(&Value::String("ee.error.v2".into())) {
        return Err("missing or incorrect schema field".into());
    }

    let error = obj.get("error").and_then(|e| e.as_object());
    let error = error.ok_or("missing error object")?;

    let required = ["code", "message", "severity", "details"];
    for field in required {
        if !error.contains_key(field) {
            return Err(format!("missing required field: error.{field}"));
        }
    }

    let severity = error.get("severity").and_then(|s| s.as_str());
    match severity {
        Some("info" | "low" | "warning" | "medium" | "high" | "critical") => Ok(()),
        Some(other) => Err(format!("invalid severity: {other}")),
        None => Err("severity must be a string".into()),
    }
}

#[test]
fn error_envelope_usage_with_repair() -> TestResult {
    let error = DomainError::Usage {
        message: "Unknown command 'xyz'.".into(),
        repair: Some("ee --help".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_usage_with_repair", value);
    Ok(())
}

#[test]
fn error_envelope_usage_without_repair() -> TestResult {
    let error = DomainError::Usage {
        message: "Invalid argument for --format.".into(),
        repair: None,
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_usage_without_repair", value);
    Ok(())
}

#[test]
fn error_envelope_configuration() -> TestResult {
    let error = DomainError::Configuration {
        message: "Invalid config file format.".into(),
        repair: Some("ee doctor --fix-plan --json".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_configuration", value);
    Ok(())
}

#[test]
fn error_envelope_storage() -> TestResult {
    let error = DomainError::Storage {
        message: "Database file corrupted.".into(),
        repair: Some("ee doctor --fix-plan --json".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_storage", value);
    Ok(())
}

#[test]
fn error_envelope_search_index() -> TestResult {
    let error = DomainError::SearchIndex {
        message: "Index is stale (generation 9, database generation 12).".into(),
        repair: Some("ee index rebuild".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_search_index", value);
    Ok(())
}

#[test]
fn error_envelope_graph() -> TestResult {
    let error = DomainError::Graph {
        message: "Graph projection outdated.".into(),
        repair: Some("ee graph snapshot refresh".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_graph", value);
    Ok(())
}

#[test]
fn error_envelope_import() -> TestResult {
    let error = DomainError::Import {
        message: "CASS session file not found.".into(),
        repair: Some("ee import cass --dry-run".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_import", value);
    Ok(())
}

#[test]
fn error_envelope_not_found() -> TestResult {
    let error = DomainError::NotFound {
        resource: "memory".into(),
        id: "mem_00000000000000000000000099".into(),
        repair: Some("ee search".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_not_found", value);
    Ok(())
}

#[test]
fn error_envelope_unsatisfied_degraded_mode() -> TestResult {
    let error = DomainError::UnsatisfiedDegradedMode {
        message: "Semantic search unavailable and lexical fallback disabled.".into(),
        repair: Some("ee doctor --json".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_unsatisfied_degraded_mode", value);
    Ok(())
}

#[test]
fn error_envelope_policy_denied() -> TestResult {
    let error = DomainError::PolicyDenied {
        message: "Redaction policy prevents exporting this memory.".into(),
        repair: Some("ee policy show".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_policy_denied", value);
    Ok(())
}

#[test]
fn error_envelope_migration_required() -> TestResult {
    let error = DomainError::MigrationRequired {
        message: "Database schema is at version 5, current is 7.".into(),
        repair: Some("ee migrate run".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_migration_required", value);
    Ok(())
}

#[test]
fn error_envelope_migration_drift() -> TestResult {
    let error = DomainError::MigrationDrift {
        message: "Applied migrations do not match expected checksums.".into(),
        repair: Some("ee doctor --fix-plan --json".into()),
    };
    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let value: Value = parse_error_json(&json)?;
    assert_json_snapshot!("error_envelope_migration_drift", value);
    Ok(())
}

#[test]
fn error_envelope_workspace_store_missing_matches_required_fixture() -> TestResult {
    let mut addressed_workspace = RecoveryAction::flag(
        1,
        "--workspace",
        "/workspace/missing",
        "Re-check the exact addressed workspace before selecting another store.",
    );
    addressed_workspace.example = Some("--workspace /workspace/missing".to_owned());
    let mut addressed_store = RecoveryAction::env(
        2,
        "EE_DATABASE_PATH",
        "/workspace/missing/.ee/ee.db",
        "Re-check the exact addressed database override before initializing a new store.",
    );
    addressed_store.example = Some("EE_DATABASE_PATH=/workspace/missing/.ee/ee.db".to_owned());
    let initialize = RecoveryAction {
        priority: 3,
        kind: RecoveryKind::Seed,
        rationale:
            "Only initialize when this exact addressed workspace was intentionally chosen for a new store."
                .to_owned(),
        env_name: None,
        value_hint: None,
        config_path: None,
        config_key: None,
        flag_name: None,
        command: Some("ee init --workspace /workspace/missing".to_owned()),
        results_in: None,
        example: None,
    };
    let error = DomainError::WorkspaceStoreMissing {
        message: "Database not found at /workspace/missing/.ee/ee.db".to_owned(),
        repair: Some(
            "Re-check --workspace addressing with --workspace /workspace/missing (looked for /workspace/missing/.ee/ee.db). Nearby-store discovery completed and found no populated stores. Only if you intended to create a NEW store here: ee init --workspace /workspace/missing"
                .to_owned(),
        ),
        details_json: serde_json::json!({
            "addressedStorePath": "/workspace/missing/.ee/ee.db",
            "addressedWorkspacePath": "/workspace/missing",
            "storeDiscovery": {
                "outcome": "complete",
                "nearbyStores": [],
            },
        })
        .to_string(),
        recovery_actions: vec![addressed_workspace, addressed_store, initialize],
    };

    let json = error_response_json(&error);
    verify_error_envelope(&json)?;
    let actual = parse_error_json(&json)?;
    let expected = parse_error_json(include_str!(
        "fixtures/golden/error/workspace_store_missing.golden"
    ))?;
    if actual != expected {
        return Err(format!(
            "workspace_store_missing error fixture drifted:\nexpected={expected:#}\nactual={actual:#}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod contract_verification {
    use super::*;

    #[test]
    fn all_error_codes_are_lowercase_snake_case() -> TestResult {
        let errors: Vec<DomainError> = vec![
            DomainError::Usage {
                message: "m".into(),
                repair: None,
            },
            DomainError::Configuration {
                message: "m".into(),
                repair: None,
            },
            DomainError::Storage {
                message: "m".into(),
                repair: None,
            },
            DomainError::SearchIndex {
                message: "m".into(),
                repair: None,
            },
            DomainError::Graph {
                message: "m".into(),
                repair: None,
            },
            DomainError::Import {
                message: "m".into(),
                repair: None,
            },
            DomainError::ImportWithDetails {
                message: "m".into(),
                repair: None,
                details_json: "{}".into(),
            },
            DomainError::NotFound {
                resource: "r".into(),
                id: "i".into(),
                repair: None,
            },
            DomainError::UnsatisfiedDegradedMode {
                message: "m".into(),
                repair: None,
            },
            DomainError::PolicyDenied {
                message: "m".into(),
                repair: None,
            },
            DomainError::MigrationRequired {
                message: "m".into(),
                repair: None,
            },
            DomainError::MigrationDrift {
                message: "m".into(),
                repair: None,
            },
        ];

        for error in errors {
            let json = error_response_json(&error);
            let value: Value = parse_error_json(&json)?;
            let code = value["error"]["code"].as_str().ok_or("missing code")?;

            if !code.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                return Err(format!("code '{code}' is not lowercase_snake_case"));
            }
        }
        Ok(())
    }

    #[test]
    fn severity_matches_documented_exit_codes() -> TestResult {
        let test_cases = [
            (
                DomainError::Usage {
                    message: "m".into(),
                    repair: None,
                },
                "low",
            ),
            (
                DomainError::Configuration {
                    message: "m".into(),
                    repair: None,
                },
                "medium",
            ),
            (
                DomainError::Storage {
                    message: "m".into(),
                    repair: None,
                },
                "high",
            ),
            (
                DomainError::SearchIndex {
                    message: "m".into(),
                    repair: None,
                },
                "medium",
            ),
            (
                DomainError::Graph {
                    message: "m".into(),
                    repair: None,
                },
                "medium",
            ),
            (
                DomainError::Import {
                    message: "m".into(),
                    repair: None,
                },
                "medium",
            ),
            (
                DomainError::NotFound {
                    resource: "r".into(),
                    id: "i".into(),
                    repair: None,
                },
                "low",
            ),
            (
                DomainError::UnsatisfiedDegradedMode {
                    message: "m".into(),
                    repair: None,
                },
                "medium",
            ),
            (
                DomainError::PolicyDenied {
                    message: "m".into(),
                    repair: None,
                },
                "medium",
            ),
            (
                DomainError::MigrationRequired {
                    message: "m".into(),
                    repair: None,
                },
                "medium",
            ),
            (
                DomainError::MigrationDrift {
                    message: "m".into(),
                    repair: None,
                },
                "high",
            ),
        ];

        for (error, expected_severity) in test_cases {
            let json = error_response_json(&error);
            let value: Value = parse_error_json(&json)?;
            let actual = value["error"]["severity"]
                .as_str()
                .ok_or("missing severity")?;
            if actual != expected_severity {
                let code = value["error"]["code"].as_str().unwrap_or("?");
                return Err(format!(
                    "severity mismatch for {code}: expected {expected_severity}, got {actual}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn not_found_includes_resource_and_id_in_details() -> TestResult {
        let error = DomainError::NotFound {
            resource: "memory".into(),
            id: "mem_test123".into(),
            repair: None,
        };
        let json = error_response_json(&error);
        let value: Value = parse_error_json(&json)?;
        let details = &value["error"]["details"];

        if details["resource"].as_str() != Some("memory") {
            return Err("NotFound details missing resource field".into());
        }
        if details["id"].as_str() != Some("mem_test123") {
            return Err("NotFound details missing id field".into());
        }
        Ok(())
    }
}
