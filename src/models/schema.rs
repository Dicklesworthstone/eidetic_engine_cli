//! Schema validation for JSON contracts (EE-265).
//!
//! Provides validation functions and error types for verifying that incoming
//! JSON documents have valid, known schema versions before processing.

use std::fmt;

use super::{
    ATTENTION_COST_SCHEMA_V1, CASS_EVIDENCE_SPAN_SCHEMA_V1, CASS_SESSION_SCHEMA_V1,
    CLAIM_ENTRY_SCHEMA_V1, CLAIM_MANIFEST_SCHEMA_V1, CLAIMS_FILE_SCHEMA_V1,
    DECISION_PLANE_SCHEMA_V1, DRY_RUN_PREVIEW_SCHEMA_V1, ECONOMY_RECOMMENDATION_SCHEMA_V1,
    ECONOMY_REPORT_SCHEMA_V1, ECONOMY_SCHEMA_CATALOG_V1, EMBEDDING_METADATA_SCHEMA_V1,
    ERROR_SCHEMA_V1, EVAL_FIXTURE_SCHEMA_V1, EXPORT_AGENT_SCHEMA_V1, EXPORT_AUDIT_SCHEMA_V1,
    EXPORT_FOOTER_SCHEMA_V1, EXPORT_HEADER_SCHEMA_V1, EXPORT_LINK_SCHEMA_V1,
    EXPORT_MEMORY_SCHEMA_V1, EXPORT_TAG_SCHEMA_V1, EXPORT_WORKSPACE_SCHEMA_V1,
    GRAPH_MODULE_SCHEMA_V1, IMPORT_CASS_SCHEMA_V1, IMPORT_CURSOR_SCHEMA_V1,
    IMPORT_EIDETIC_LEGACY_SCAN_SCHEMA_V1, IMPORT_LEDGER_CASS_SCHEMA_V1, IMPORT_LEDGER_SCHEMA_V1,
    INDEX_MANIFEST_SCHEMA_V1, MAINTENANCE_DEBT_SCHEMA_V1, MANIFEST_ARTIFACT_SCHEMA_V1,
    MODEL_REGISTRY_SCHEMA_V1, MUTATION_RESPONSE_SCHEMA_V1, PROCEDURE_EXPORT_SCHEMA_V1,
    PROCEDURE_SCHEMA_CATALOG_V1, PROCEDURE_SCHEMA_V1, PROCEDURE_STEP_SCHEMA_V1,
    PROCEDURE_VERIFICATION_SCHEMA_V1, PROGRESS_EVENT_SCHEMA_V1, RECORDER_EVENT_SCHEMA_V1,
    RECORDER_PAYLOAD_SCHEMA_V1, RECORDER_RUN_SCHEMA_V1, RECORDER_SCHEMA_CATALOG_V1,
    REDACTION_STATUS_SCHEMA_V1, RESPONSE_SCHEMA_V1, RISK_RESERVE_SCHEMA_V1,
    SEARCH_DOCUMENT_SCHEMA_V1, SEARCH_MODULE_SCHEMA_V1, SKILL_CAPSULE_SCHEMA_V1,
    UTILITY_VALUE_SCHEMA_V1,
};

/// All known schema identifiers for validation.
pub const KNOWN_SCHEMAS: &[&str] = &[
    RESPONSE_SCHEMA_V1,
    ERROR_SCHEMA_V1,
    IMPORT_CASS_SCHEMA_V1,
    IMPORT_EIDETIC_LEGACY_SCAN_SCHEMA_V1,
    IMPORT_LEDGER_SCHEMA_V1,
    IMPORT_LEDGER_CASS_SCHEMA_V1,
    CASS_SESSION_SCHEMA_V1,
    CASS_EVIDENCE_SPAN_SCHEMA_V1,
    SEARCH_MODULE_SCHEMA_V1,
    SEARCH_DOCUMENT_SCHEMA_V1,
    GRAPH_MODULE_SCHEMA_V1,
    EVAL_FIXTURE_SCHEMA_V1,
    INDEX_MANIFEST_SCHEMA_V1,
    MODEL_REGISTRY_SCHEMA_V1,
    EMBEDDING_METADATA_SCHEMA_V1,
    CLAIMS_FILE_SCHEMA_V1,
    CLAIM_ENTRY_SCHEMA_V1,
    CLAIM_MANIFEST_SCHEMA_V1,
    MANIFEST_ARTIFACT_SCHEMA_V1,
    RECORDER_RUN_SCHEMA_V1,
    RECORDER_EVENT_SCHEMA_V1,
    RECORDER_PAYLOAD_SCHEMA_V1,
    REDACTION_STATUS_SCHEMA_V1,
    IMPORT_CURSOR_SCHEMA_V1,
    RECORDER_SCHEMA_CATALOG_V1,
    // Procedure and skill-capsule schemas (EE-410)
    PROCEDURE_SCHEMA_V1,
    PROCEDURE_STEP_SCHEMA_V1,
    PROCEDURE_VERIFICATION_SCHEMA_V1,
    PROCEDURE_EXPORT_SCHEMA_V1,
    SKILL_CAPSULE_SCHEMA_V1,
    PROCEDURE_SCHEMA_CATALOG_V1,
    // Economy and attention-budget schemas (EE-430)
    UTILITY_VALUE_SCHEMA_V1,
    ATTENTION_COST_SCHEMA_V1,
    RISK_RESERVE_SCHEMA_V1,
    MAINTENANCE_DEBT_SCHEMA_V1,
    ECONOMY_RECOMMENDATION_SCHEMA_V1,
    ECONOMY_REPORT_SCHEMA_V1,
    ECONOMY_SCHEMA_CATALOG_V1,
    // JSONL export schemas (EE-220)
    EXPORT_HEADER_SCHEMA_V1,
    EXPORT_MEMORY_SCHEMA_V1,
    EXPORT_FOOTER_SCHEMA_V1,
    EXPORT_AUDIT_SCHEMA_V1,
    EXPORT_LINK_SCHEMA_V1,
    EXPORT_TAG_SCHEMA_V1,
    EXPORT_AGENT_SCHEMA_V1,
    EXPORT_WORKSPACE_SCHEMA_V1,
    // Decision plane schema (EE-364)
    DECISION_PLANE_SCHEMA_V1,
    // Progress event schema (EE-318)
    PROGRESS_EVENT_SCHEMA_V1,
    // Mutation response schemas (EE-319)
    MUTATION_RESPONSE_SCHEMA_V1,
    DRY_RUN_PREVIEW_SCHEMA_V1,
];

/// Error returned when schema validation fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchemaValidationError {
    /// The document has no `schema` field.
    MissingSchemaField,
    /// The `schema` field is not a string.
    SchemaFieldNotString,
    /// The schema identifier is not recognized.
    UnknownSchema { schema: String },
    /// The schema version is not supported (e.g., "ee.response.v2" when only v1 is known).
    UnsupportedVersion {
        schema: String,
        expected_version: String,
    },
    /// The schema does not match the expected schema for this context.
    SchemaMismatch { expected: String, actual: String },
}

impl fmt::Display for SchemaValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSchemaField => {
                write!(f, "JSON document missing required 'schema' field")
            }
            Self::SchemaFieldNotString => {
                write!(f, "'schema' field must be a string")
            }
            Self::UnknownSchema { schema } => {
                write!(f, "unknown schema identifier: {schema}")
            }
            Self::UnsupportedVersion {
                schema,
                expected_version,
            } => {
                write!(
                    f,
                    "unsupported schema version: {schema} (expected version {expected_version})"
                )
            }
            Self::SchemaMismatch { expected, actual } => {
                write!(f, "schema mismatch: expected {expected}, got {actual}")
            }
        }
    }
}

impl std::error::Error for SchemaValidationError {}

impl SchemaValidationError {
    /// Return a repair suggestion for this error.
    #[must_use]
    pub fn repair(&self) -> &'static str {
        match self {
            Self::MissingSchemaField => {
                "Add a 'schema' field with a valid ee.*.v1 schema identifier."
            }
            Self::SchemaFieldNotString => {
                "Ensure the 'schema' field is a string, not a number or object."
            }
            Self::UnknownSchema { .. } => {
                "Check the schema identifier against known ee.*.v1 schemas."
            }
            Self::UnsupportedVersion { .. } => {
                "Upgrade the document to use a supported schema version."
            }
            Self::SchemaMismatch { .. } => {
                "Ensure the document schema matches the expected schema for this operation."
            }
        }
    }

    /// Return the error code for JSON output.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingSchemaField => "schema_missing",
            Self::SchemaFieldNotString => "schema_not_string",
            Self::UnknownSchema { .. } => "schema_unknown",
            Self::UnsupportedVersion { .. } => "schema_version_unsupported",
            Self::SchemaMismatch { .. } => "schema_mismatch",
        }
    }
}

/// Check if a schema identifier is known.
#[must_use]
pub fn is_known_schema(schema: &str) -> bool {
    KNOWN_SCHEMAS.contains(&schema)
}

/// Extract the base name and version from a schema identifier.
///
/// Returns `None` if the schema doesn't match the `ee.<name>.v<n>` pattern.
#[must_use]
pub fn parse_schema_parts(schema: &str) -> Option<(&str, &str, &str)> {
    let stripped = schema.strip_prefix("ee.")?;
    let dot_v_pos = stripped.rfind(".v")?;
    let name = &stripped[..dot_v_pos];
    let version = &stripped[dot_v_pos + 2..];
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(("ee", name, version))
}

/// Validate that a schema string is known and supported.
///
/// # Errors
///
/// Returns [`SchemaValidationError::UnknownSchema`] if the schema is not in
/// [`KNOWN_SCHEMAS`].
pub fn validate_schema(schema: &str) -> Result<(), SchemaValidationError> {
    if is_known_schema(schema) {
        Ok(())
    } else {
        // Try to provide a better error if it looks like a future version
        if let Some((_, name, version)) = parse_schema_parts(schema) {
            // Check if we have v1 but they're asking for v2+
            let v1_schema = format!("ee.{name}.v1");
            if is_known_schema(&v1_schema) && version != "1" {
                return Err(SchemaValidationError::UnsupportedVersion {
                    schema: schema.to_owned(),
                    expected_version: "v1".to_owned(),
                });
            }
        }
        Err(SchemaValidationError::UnknownSchema {
            schema: schema.to_owned(),
        })
    }
}

/// Validate that a schema matches the expected schema for an operation.
///
/// # Errors
///
/// Returns [`SchemaValidationError::SchemaMismatch`] if the schemas don't match.
pub fn validate_schema_match(expected: &str, actual: &str) -> Result<(), SchemaValidationError> {
    if expected == actual {
        Ok(())
    } else {
        Err(SchemaValidationError::SchemaMismatch {
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn known_schemas_are_valid() -> TestResult {
        for schema in KNOWN_SCHEMAS {
            ensure(
                is_known_schema(schema),
                true,
                &format!("{schema} should be known"),
            )?;
            ensure(
                validate_schema(schema).is_ok(),
                true,
                &format!("{schema} should validate"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn unknown_schema_returns_error() -> TestResult {
        let result = validate_schema("ee.unknown.v1");
        ensure(
            result,
            Err(SchemaValidationError::UnknownSchema {
                schema: "ee.unknown.v1".to_owned(),
            }),
            "unknown schema should fail",
        )
    }

    #[test]
    fn future_version_returns_unsupported_version_error() -> TestResult {
        // ee.response.v2 should fail because we only know v1
        let result = validate_schema("ee.response.v2");
        ensure(
            result,
            Err(SchemaValidationError::UnsupportedVersion {
                schema: "ee.response.v2".to_owned(),
                expected_version: "v1".to_owned(),
            }),
            "future version should return UnsupportedVersion",
        )
    }

    #[test]
    fn future_dotted_recorder_version_returns_unsupported_version_error() -> TestResult {
        let result = validate_schema("ee.recorder.event.v2");
        ensure(
            result,
            Err(SchemaValidationError::UnsupportedVersion {
                schema: "ee.recorder.event.v2".to_owned(),
                expected_version: "v1".to_owned(),
            }),
            "future recorder version should return UnsupportedVersion",
        )
    }

    #[test]
    fn invalid_version_format_returns_unknown_schema() -> TestResult {
        // ee.response.vX is not a valid version
        let result = validate_schema("ee.response.vX");
        ensure(
            result,
            Err(SchemaValidationError::UnknownSchema {
                schema: "ee.response.vX".to_owned(),
            }),
            "invalid version format should return UnknownSchema",
        )
    }

    #[test]
    fn malformed_schema_returns_unknown_schema() -> TestResult {
        let cases = [
            "not.an.ee.schema",
            "ee.missing_version",
            "ee.",
            "",
            "response.v1",
            "ee.v1",
        ];
        for case in cases {
            let result = validate_schema(case);
            ensure(
                matches!(result, Err(SchemaValidationError::UnknownSchema { .. })),
                true,
                &format!("'{case}' should return UnknownSchema"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn schema_mismatch_detection() -> TestResult {
        let result = validate_schema_match(RESPONSE_SCHEMA_V1, ERROR_SCHEMA_V1);
        ensure(
            result,
            Err(SchemaValidationError::SchemaMismatch {
                expected: RESPONSE_SCHEMA_V1.to_owned(),
                actual: ERROR_SCHEMA_V1.to_owned(),
            }),
            "mismatched schemas should fail",
        )
    }

    #[test]
    fn schema_match_succeeds() -> TestResult {
        let result = validate_schema_match(RESPONSE_SCHEMA_V1, RESPONSE_SCHEMA_V1);
        ensure(result.is_ok(), true, "matching schemas should succeed")
    }

    #[test]
    fn parse_schema_parts_extracts_components() -> TestResult {
        let cases = [
            ("ee.response.v1", Some(("ee", "response", "1"))),
            ("ee.import.cass.v1", Some(("ee", "import.cass", "1"))),
            ("ee.response.v2", Some(("ee", "response", "2"))),
            ("ee.response.v123", Some(("ee", "response", "123"))),
            ("not.ee.schema", None),
            ("ee.missing", None),
            ("ee.bad.vX", None),
            ("ee.bad.v", None),
        ];
        for (input, expected) in cases {
            let result = parse_schema_parts(input);
            ensure(result, expected, &format!("parse_schema_parts({input:?})"))?;
        }
        Ok(())
    }

    #[test]
    fn error_codes_are_stable() -> TestResult {
        ensure(
            SchemaValidationError::MissingSchemaField.code(),
            "schema_missing",
            "MissingSchemaField code",
        )?;
        ensure(
            SchemaValidationError::SchemaFieldNotString.code(),
            "schema_not_string",
            "SchemaFieldNotString code",
        )?;
        ensure(
            SchemaValidationError::UnknownSchema {
                schema: "x".to_owned(),
            }
            .code(),
            "schema_unknown",
            "UnknownSchema code",
        )?;
        ensure(
            SchemaValidationError::UnsupportedVersion {
                schema: "x".to_owned(),
                expected_version: "v1".to_owned(),
            }
            .code(),
            "schema_version_unsupported",
            "UnsupportedVersion code",
        )?;
        ensure(
            SchemaValidationError::SchemaMismatch {
                expected: "a".to_owned(),
                actual: "b".to_owned(),
            }
            .code(),
            "schema_mismatch",
            "SchemaMismatch code",
        )?;
        Ok(())
    }

    #[test]
    fn error_display_messages_are_informative() {
        let errors = [
            (
                SchemaValidationError::MissingSchemaField,
                "missing required 'schema' field",
            ),
            (
                SchemaValidationError::SchemaFieldNotString,
                "must be a string",
            ),
            (
                SchemaValidationError::UnknownSchema {
                    schema: "ee.foo.v1".to_owned(),
                },
                "unknown schema identifier: ee.foo.v1",
            ),
            (
                SchemaValidationError::UnsupportedVersion {
                    schema: "ee.response.v2".to_owned(),
                    expected_version: "v1".to_owned(),
                },
                "unsupported schema version: ee.response.v2",
            ),
            (
                SchemaValidationError::SchemaMismatch {
                    expected: "ee.response.v1".to_owned(),
                    actual: "ee.error.v1".to_owned(),
                },
                "schema mismatch: expected ee.response.v1",
            ),
        ];
        for (error, expected_substring) in errors {
            let msg = error.to_string();
            assert!(
                msg.contains(expected_substring),
                "Error message '{}' should contain '{}'",
                msg,
                expected_substring
            );
        }
    }

    #[test]
    fn repair_suggestions_are_provided() {
        let errors = [
            SchemaValidationError::MissingSchemaField,
            SchemaValidationError::SchemaFieldNotString,
            SchemaValidationError::UnknownSchema {
                schema: "x".to_owned(),
            },
            SchemaValidationError::UnsupportedVersion {
                schema: "x".to_owned(),
                expected_version: "v1".to_owned(),
            },
            SchemaValidationError::SchemaMismatch {
                expected: "a".to_owned(),
                actual: "b".to_owned(),
            },
        ];
        for error in errors {
            let repair = error.repair();
            assert!(
                !repair.is_empty(),
                "Repair for {:?} should not be empty",
                error
            );
        }
    }
}
