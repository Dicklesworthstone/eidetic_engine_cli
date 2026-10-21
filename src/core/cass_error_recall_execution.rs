//! Execution metadata can veto CASS repair proof, never manufacture it.
//!
//! A wrapper may retain exit zero while reporting cancellation, timeout or
//! incomplete output elsewhere. Read both control scopes before discarding
//! the wrapper. Captured stdout and unrelated metadata remain opaque evidence.

use serde_json::Value;

/// Missing controls retain the historical exit-code contract. Present controls
/// must have the declared type and a non-adverse value. Contradictions cannot
/// be resolved by preferring the outer object, a later field, or a green footer.
pub(super) fn refuses_completion(raw: &Value) -> bool {
    [Some(raw), raw.get("metadata")]
        .into_iter()
        .flatten()
        .any(|scope| {
            !scope.is_object()
                || [
                    "is_error",
                    "timed_out",
                    "cancelled",
                    "canceled",
                    "interrupted",
                    "truncated",
                    "output_truncated",
                    "running",
                ]
                .iter()
                .any(|field| {
                    scope
                        .get(*field)
                        .is_some_and(|value| value.as_bool() != Some(false))
                })
                || ["completed", "success"].iter().any(|field| {
                    scope
                        .get(*field)
                        .is_some_and(|value| value.as_bool() != Some(true))
                })
                || scope.get("status").is_some_and(|value| {
                    !matches!(
                        value.as_str(),
                        Some("completed" | "finished" | "exited" | "success" | "succeeded" | "ok")
                    )
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::outcome::codex_output;
    use crate::core::cass_error_recall::test_coverage::{Proof, Requirement};
    use serde_json::json;

    fn in_scope(field: &str, value: Value, nested: bool) -> Value {
        let mut raw = json!({"output": "Finished dev profile", "exit_code": 0, "metadata": {}});
        if nested {
            raw["metadata"][field] = value;
        } else {
            raw[field] = value;
        }
        raw
    }

    fn assert_decoded(raw: &Value, succeeds: bool) {
        for representation in [raw.clone(), Value::String(raw.to_string())] {
            let decoded = codex_output(&representation).expect("retain diagnostic output");
            assert_eq!(decoded.text, raw["output"].as_str().unwrap());
            assert_eq!(decoded.succeeded(), succeeds, "{raw}");
        }
    }

    #[test]
    fn adverse_and_malformed_flags_override_zero_in_both_control_scopes() {
        for field in [
            "is_error",
            "timed_out",
            "cancelled",
            "canceled",
            "interrupted",
            "truncated",
            "output_truncated",
            "running",
        ] {
            for nested in [false, true] {
                for value in [json!(true), Value::Null, json!("false"), json!(0), json!({}), json!([])] {
                    assert_decoded(&in_scope(field, value, nested), false);
                }
                assert_decoded(&in_scope(field, json!(false), nested), true);
            }
        }
    }

    #[test]
    fn completion_booleans_and_status_names_are_checked_not_used_as_exit_authority() {
        for nested in [false, true] {
            for field in ["completed", "success"] {
                for value in [json!(false), Value::Null, json!("true"), json!(1)] {
                    assert_decoded(&in_scope(field, value, nested), false);
                }
                let mut raw = in_scope(field, json!(true), nested);
                assert_decoded(&raw, true);
                raw.as_object_mut().unwrap().remove("exit_code");
                assert_decoded(&raw, false);
            }
            for status in ["completed", "finished", "exited", "success", "succeeded", "ok"] {
                let mut raw = in_scope("status", json!(status), nested);
                assert_decoded(&raw, true);
                raw.as_object_mut().unwrap().remove("exit_code");
                assert_decoded(&raw, false);
            }
            for status in ["running", "pending", "cancelled", "timed_out", "failed", "abstained", "unknown", ""] {
                assert_decoded(&in_scope("status", json!(status), nested), false);
            }
            for status in [Value::Null, json!(true), json!(0), json!([]), json!({})] {
                assert_decoded(&in_scope("status", status, nested), false);
            }
        }
    }

    #[test]
    fn conflicting_scopes_never_prefer_the_optimistic_observation() {
        for field in ["is_error", "timed_out", "cancelled", "truncated"] {
            for nested in [false, true] {
                let mut raw = in_scope(field, json!(true), nested);
                if nested {
                    raw[field] = json!(false);
                } else {
                    raw["metadata"][field] = json!(false);
                }
                assert_decoded(&raw, false);
            }
        }
        for (outer, inner) in [("completed", "running"), ("failed", "succeeded")] {
            let mut raw = in_scope("status", json!(outer), false);
            raw["metadata"]["status"] = json!(inner);
            assert_decoded(&raw, false);
        }
    }

    #[test]
    fn stdout_examples_and_unrelated_metadata_are_not_execution_controls() {
        for output in ["", r#"{"timed_out":true,"status":"running","success":false}"#] {
            let raw = json!({
                "output": output,
                "exit_code": 0,
                "metadata": {
                    "duration_seconds": 0.125,
                    "example": {"is_error": true, "completed": false},
                    "session_id": 42
                }
            });
            assert_decoded(&raw, true);
        }
        let raw = json!({"output": "", "metadata": {"exit_code": 0}});
        assert_decoded(&raw, true);
        let raw = json!({"output": "", "exit_code": 1, "metadata": {"status": "completed"}});
        assert_decoded(&raw, false);
    }

    #[test]
    fn exit_looking_text_cannot_override_rejected_control_metadata() {
        for nested in [false, true] {
            let mut raw = in_scope("timed_out", json!(true), nested);
            raw["output"] = json!("Finished dev profile\nProcess exited with code 0");
            raw.as_object_mut().unwrap().remove("exit_code");
            assert_decoded(&raw, false);
        }
    }

    #[test]
    fn cancelled_passing_test_inventory_cannot_verify_a_failed_subject() {
        let failed_text = "running 1 test\ntest cache::reload ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        let passed_text = "running 1 test\ntest cache::reload ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
        let failed = codex_output(&json!({"output": failed_text, "exit_code": 101})).unwrap();
        let required = Requirement::capture(&failed, true);
        assert!(required.is_guarded());
        let passed = codex_output(&json!({"output": passed_text, "exit_code": 0})).unwrap();
        assert!(required.is_verified_by(&Proof::capture(&passed)));
        for field in ["timed_out", "cancelled", "truncated", "running"] {
            for nested in [false, true] {
                let mut raw = in_scope(field, json!(true), nested);
                raw["output"] = json!(passed_text);
                for representation in [raw.clone(), Value::String(raw.to_string())] {
                    let partial = codex_output(&representation).unwrap();
                    assert!(!required.is_verified_by(&Proof::capture(&partial)), "{field}");
                }
            }
        }
    }

    #[test]
    fn metadata_refusal_retains_real_compiler_diagnostics_without_exposing_metadata() {
        let text = "error[E0308]: mismatched types";
        let raw = json!({
            "output": text,
            "exit_code": 0,
            "metadata": {"is_error": true, "private_diagnostic": "do not copy this metadata"}
        });
        let decoded = codex_output(&raw).unwrap();
        assert_eq!(decoded.text, text);
        assert!(decoded.failed());
        assert!(!decoded.succeeded());
        assert!(!crate::core::cass_error_recall::failure_diagnostics(&decoded).is_empty());
    }
}
