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

/// Value deserialization keeps only the last occurrence of a key. Before using
/// an encoded wrapper as proof, inspect its control scopes without constructing
/// a second Value tree. Unknown diagnostic objects and stdout are not controls.
/// Object-valued inputs have already lost wire multiplicity at their caller;
/// this check deliberately applies only where the encoded bytes still exist.
pub(super) fn unique_control_fields(text: &str) -> bool {
    use serde::de::DeserializeSeed;

    let mut decoder = serde_json::Deserializer::from_str(text);
    ControlScope { outer: true }
        .deserialize(&mut decoder)
        .and_then(|()| decoder.end())
        .is_ok()
}

struct ControlScope {
    outer: bool,
}

impl<'de> serde::de::DeserializeSeed<'de> for ControlScope {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for ControlScope {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an execution control object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<(), A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        use serde::de::{Error, IgnoredAny};

        // Fixed-size bookkeeping, independent of stdout or unknown key count.
        let mut seen = 0_u16;
        while let Some(key) = map.next_key::<String>()? {
            let bit = match key.as_str() {
                "output" if self.outer => Some(0),
                "metadata" if self.outer => Some(1),
                "exit_code" => Some(2),
                "is_error" => Some(3),
                "timed_out" => Some(4),
                "cancelled" => Some(5),
                "canceled" => Some(6),
                "interrupted" => Some(7),
                "truncated" => Some(8),
                "output_truncated" => Some(9),
                "running" => Some(10),
                "completed" => Some(11),
                "success" => Some(12),
                "status" => Some(13),
                _ => None,
            };
            if let Some(bit) = bit {
                let mask = 1_u16 << bit;
                if seen & mask != 0 {
                    return Err(A::Error::custom("duplicate execution control field"));
                }
                seen |= mask;
            }
            if self.outer && key == "metadata" {
                map.next_value_seed(ControlScope { outer: false })?;
            } else {
                let _ = map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
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
                for value in [
                    json!(true),
                    Value::Null,
                    json!("false"),
                    json!(0),
                    json!({}),
                    json!([]),
                ] {
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
            for status in [
                "completed",
                "finished",
                "exited",
                "success",
                "succeeded",
                "ok",
            ] {
                let mut raw = in_scope("status", json!(status), nested);
                assert_decoded(&raw, true);
                raw.as_object_mut().unwrap().remove("exit_code");
                assert_decoded(&raw, false);
            }
            for status in [
                "running",
                "pending",
                "cancelled",
                "timed_out",
                "failed",
                "abstained",
                "unknown",
                "",
            ] {
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
        for output in [
            "",
            r#"{"timed_out":true,"status":"running","success":false}"#,
        ] {
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
                    assert!(
                        !required.is_verified_by(&Proof::capture(&partial)),
                        "{field}"
                    );
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

#[cfg(test)]
mod encoded_tests {
    use super::*;
    use crate::core::cass_error_recall::outcome::codex_output;
    use serde_json::json;

    #[test]
    fn duplicated_controls_cannot_hide_an_earlier_failure_in_encoded_wrappers() {
        for (field, bad, good) in [
            ("exit_code", "1", "0"),
            ("is_error", "true", "false"),
            ("timed_out", "true", "false"),
            ("cancelled", "true", "false"),
            ("canceled", "true", "false"),
            ("interrupted", "true", "false"),
            ("truncated", "true", "false"),
            ("output_truncated", "true", "false"),
            ("running", "true", "false"),
            ("completed", "false", "true"),
            ("success", "false", "true"),
            ("status", "\"running\"", "\"completed\""),
        ] {
            for (first, last) in [(bad, good), (good, bad), (good, good)] {
                for nested in [false, true] {
                    let controls = format!("\"{field}\":{first},\"{field}\":{last}");
                    let text = if nested {
                        format!(
                            r#"{{"output":"Finished dev profile","exit_code":0,"metadata":{{{controls}}}}}"#
                        )
                    } else {
                        let exit = if field == "exit_code" {
                            ""
                        } else {
                            ",\"exit_code\":0"
                        };
                        format!(r#"{{"output":"Finished dev profile",{controls}{exit}}}"#)
                    };
                    assert!(!unique_control_fields(&text));
                    let decoded = codex_output(&Value::String(text)).expect("diagnostic text");
                    assert_eq!(decoded.text, "Finished dev profile");
                    assert!(!decoded.succeeded(), "{field}, nested={nested}");
                }
            }
        }
    }

    #[test]
    fn duplicate_output_metadata_and_escaped_aliases_also_withhold_proof() {
        for text in [
            r#"{"output":"failure","output":"Finished dev profile","exit_code":0}"#,
            r#"{"output":"Finished dev profile","metadata":{"exit_code":1},"metadata":{"exit_code":0}}"#,
            r#"{"output":"Finished dev profile","metadata":{},"metadata":{},"exit_code":0}"#,
            r#"{"output":"Finished dev profile","exit_code":1,"exit\u005fcode":0}"#,
            r#"{"output":"Finished dev profile","exit_code":0,"metadata":{"is_error":true,"is\u005ferror":false}}"#,
        ] {
            assert!(!unique_control_fields(text));
            let decoded = codex_output(&Value::String(text.to_owned())).unwrap();
            assert!(!decoded.succeeded());
        }
    }

    #[test]
    fn unique_encoded_controls_preserve_wire_order_and_unknown_metadata_compatibility() {
        for raw in [
            json!({"output":"", "exit_code":0}),
            json!({"output":"Finished dev profile", "metadata":{"exit_code":0, "duration_seconds":0.125}}),
            json!({"output":"Finished dev profile", "exit_code":0, "metadata":{"exit_code":0, "completed":true}}),
        ] {
            for encoded in [raw.to_string(), serde_json::to_string_pretty(&raw).unwrap()] {
                assert!(unique_control_fields(&encoded));
                assert!(codex_output(&Value::String(encoded)).unwrap().succeeded());
            }
        }
        // The same field in different scopes is not a duplicate; the existing
        // semantic check, not wire order, rejects a conflicting value.
        let conflict =
            r#"{"metadata":{"exit_code":1},"output":"Finished dev profile","exit_code":0}"#;
        assert!(unique_control_fields(conflict));
        assert!(
            !codex_output(&Value::String(conflict.to_owned()))
                .unwrap()
                .succeeded()
        );
        let unknown = r#"{"output":"","exit_code":0,"metadata":{"example":{"is_error":true,"is_error":false}},"note":1,"note":2}"#;
        assert!(unique_control_fields(unknown));
        assert!(
            codex_output(&Value::String(unknown.to_owned()))
                .unwrap()
                .succeeded()
        );
    }

    #[test]
    fn captured_json_is_not_reparsed_as_wrapper_control_authority() {
        let stdout = r#"{"exit_code":1,"exit_code":0,"timed_out":true}"#;
        let raw = json!({"output":stdout, "metadata":{"exit_code":0}});
        let text = raw.to_string();
        assert!(unique_control_fields(&text));
        let decoded = codex_output(&Value::String(text)).unwrap();
        assert_eq!(decoded.text, stdout);
        assert!(decoded.succeeded());
        for bad in ["[]", "null", "{", "{} {}", r#"{"metadata":false}"#] {
            assert!(!unique_control_fields(bad));
        }
    }
}
