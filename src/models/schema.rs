//! Schema validation for JSON contracts (EE-265).
//!
//! Provides validation functions and error types for verifying that incoming
//! JSON documents have valid, known schema versions before processing.

use std::fmt;

use super::{ARTIFACT_SUMMARY_SCHEMA_V1, PERF_METRIC_SCHEMA_V1, PERF_SCHEMA_CATALOG_V1};
use super::{
    ATTENTION_BUDGET_SCHEMA_V1, ATTENTION_COST_SCHEMA_V1, BACKUP_CREATE_SCHEMA_V1,
    BACKUP_MANIFEST_SCHEMA_V1, BACKUP_MANIFEST_SCHEMA_V2, BACKUP_RESTORE_SCHEMA_V1,
    BEADS_RETRY_SCHEMA_V1, CASS_EVIDENCE_SPAN_SCHEMA_V1, CASS_SESSION_SCHEMA_V1,
    CAUSAL_EXPOSURE_SCHEMA_V1, CAUSAL_SCHEMA_CATALOG_V1, CAUSAL_TRACE_SCHEMA_V1,
    CLAIM_ENTRY_SCHEMA_V1, CLAIM_MANIFEST_SCHEMA_V1, CLAIMS_FILE_SCHEMA_V1, CONFOUNDER_SCHEMA_V1,
    CONTEXT_PROFILE_SCHEMA_CATALOG_V1, CONTEXT_PROFILE_SCHEMA_V1, DECISION_PLANE_SCHEMA_V1,
    DECISION_TRACE_SCHEMA_V1, DOCTOR_FIX_SUMMARY_SCHEMA_V1, DOCTOR_RUN_DIFF_SCHEMA_V1,
    DOCTOR_UNDO_SUMMARY_SCHEMA_V1, DRY_RUN_PREVIEW_SCHEMA_V1, ECONOMY_RECOMMENDATION_SCHEMA_V1,
    ECONOMY_REPORT_SCHEMA_V1, ECONOMY_SCHEMA_CATALOG_V1, ECONOMY_SIMULATION_SCHEMA_V1,
    EMBEDDING_METADATA_SCHEMA_V1, ERROR_SCHEMA_V2, EVAL_FIXTURE_SCHEMA_V1,
    EXPERIMENT_OUTCOME_SCHEMA_V1, EXPORT_AGENT_SCHEMA_V1, EXPORT_ARTIFACT_SCHEMA_V1,
    EXPORT_AUDIT_SCHEMA_V1, EXPORT_FOOTER_SCHEMA_V1, EXPORT_HEADER_SCHEMA_V1,
    EXPORT_LINK_SCHEMA_V1, EXPORT_MEMORY_SCHEMA_V1, EXPORT_TAG_SCHEMA_V1,
    EXPORT_WORKSPACE_SCHEMA_V1, FAILURE_MODE_FIXTURE_SCHEMA_V1, FEATURE_EVIDENCE_SCHEMA_V1,
    FOCUS_ITEM_SCHEMA_V1, FOCUS_SCHEMA_CATALOG_V1, FOCUS_STATE_SCHEMA_V1, GRAPH_MODULE_SCHEMA_V1,
    IMPORT_CASS_SCHEMA_V1, IMPORT_CURSOR_SCHEMA_V1, IMPORT_EIDETIC_LEGACY_SCAN_SCHEMA_V1,
    IMPORT_JSONL_SCHEMA_V1, IMPORT_LEDGER_CASS_SCHEMA_V1, IMPORT_LEDGER_SCHEMA_V1,
    INDEX_MANIFEST_SCHEMA_V1, LEARNING_EXPERIMENT_SCHEMA_V1, LEARNING_OBSERVATION_SCHEMA_V1,
    LEARNING_QUESTION_SCHEMA_V1, LEARNING_SCHEMA_CATALOG_V1, MAINTENANCE_DEBT_SCHEMA_V1,
    MANIFEST_ARTIFACT_SCHEMA_V1, MESH_EVENT_SCHEMA_V1, MESH_PEER_GROUP_BINDING_SCHEMA_V1,
    MESH_PEER_POLICY_SCHEMA_V1, MESH_POLICY_DECISION_SCHEMA_V1,
    MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1, MESH_STORAGE_STATUS_SCHEMA_V1, MODEL_LIST_SCHEMA_V1,
    MODEL_REGISTRY_SCHEMA_V1, MODEL_STATUS_SCHEMA_V2, MUTATION_RESPONSE_SCHEMA_V1,
    PACK_DNA_SCHEMA_V1, PACK_QUALITY_REPORT_SCHEMA_V1, PACK_SCHEMA_V2, PACK_STREAM_SCHEMA_V1,
    PERF_SCHEMA_V1, PROCEDURE_EXPORT_SCHEMA_V1, PROCEDURE_SCHEMA_CATALOG_V1, PROCEDURE_SCHEMA_V1,
    PROCEDURE_STEP_SCHEMA_V1, PROCEDURE_VERIFICATION_SCHEMA_V1, PROGRESS_EVENT_SCHEMA_V1,
    PROMOTION_PLAN_SCHEMA_V1, PROOF_CHECK_SCHEMA_V1, PROXIMITY_SCHEMA_V1, RECORDER_EVENT_SCHEMA_V1,
    RECORDER_PAYLOAD_SCHEMA_V1, RECORDER_RUN_SCHEMA_V1, RECORDER_SCHEMA_CATALOG_V1,
    REDACTION_STATUS_SCHEMA_V1, RESPONSE_SCHEMA_V0, RESPONSE_SCHEMA_V1, RESPONSE_SCHEMA_V2,
    RISK_RESERVE_SCHEMA_V1, ROUTING_DECISION_SCHEMA_V1, SEARCH_DOCUMENT_SCHEMA_V1,
    SEARCH_MODULE_SCHEMA_V1, SINGLEFLIGHT_KEY_SCHEMA_V1, SINGLEFLIGHT_POSTURE_SCHEMA_V1,
    SITUATION_CLASSIFY_SCHEMA_V1, SITUATION_EXPLAIN_SCHEMA_V1, SITUATION_LINK_SCHEMA_V1,
    SITUATION_SCHEMA_CATALOG_V1, SITUATION_SCHEMA_V1, SITUATION_SHOW_SCHEMA_V1,
    SKILL_CAPSULE_SCHEMA_V1, SYMBOL_EVIDENCE_LINKS_SCHEMA_V1, SYMBOL_SNAPSHOT_SCHEMA_V1,
    TAIL_RISK_RESERVE_RULE_SCHEMA_V1, TASK_SIGNATURE_SCHEMA_V1, TEST_EVENT_SCHEMA_V1,
    UNCERTAINTY_ESTIMATE_SCHEMA_V1, UPLIFT_ESTIMATE_SCHEMA_V1, UTILITY_VALUE_SCHEMA_V1,
};

/// Schema identifier for opt-in agent session budget ledger rows.
pub const SESSION_BUDGET_SCHEMA_V1: &str = "ee.session_budget.v1";
/// Schema identifier for the advisory session-budget planner output (bd-1clqr.3).
pub const SESSION_BUDGET_PLAN_SCHEMA_V1: &str = "ee.session_budget.plan.v1";
/// Schema identifier for the scale-envelope posture contract (bd-ssoco.1).
pub const SCALE_ENVELOPE_SCHEMA_V1: &str = "ee.scale_envelope.v1";
/// Scale-envelope degraded code for cold-but-progressing cache/index posture.
pub const SCALE_POSTURE_WARMING_CODE: &str = "scale_posture_warming";
/// Scale-envelope degraded code for cache/WAL/index churn that exceeds SLOs.
pub const SCALE_POSTURE_THRASHING_CODE: &str = "scale_posture_thrashing";
/// Scale-envelope degraded code for missing deterministic large-corpus fixtures.
pub const SCALE_FIXTURE_UNAVAILABLE_CODE: &str = "scale_fixture_unavailable";
/// Scale-envelope degraded code for bounded probes that stop before full coverage.
pub const SCALE_PROBE_BUDGET_EXCEEDED_CODE: &str = "scale_probe_budget_exceeded";

/// Schema identifier for the group-commit write-intake telemetry contract (bd-d67os.1).
pub const WRITE_GROUP_COMMIT_SCHEMA_V1: &str = "ee.write_group_commit.v1";
/// Group-commit fallback reason: the feature is disabled by config.
pub const WRITE_GROUP_COMMIT_FALLBACK_DISABLED: &str = "disabled";
/// Group-commit fallback reason: a degraded write-owner posture forced the per-write path.
pub const WRITE_GROUP_COMMIT_FALLBACK_DEGRADED: &str = "degraded";
/// Group-commit fallback reason: a single write exceeded the inflight byte ceiling.
pub const WRITE_GROUP_COMMIT_FALLBACK_OVERSIZED: &str = "oversized";
/// Group-commit fallback reason: only one writer was in flight, so no coalescing applied.
pub const WRITE_GROUP_COMMIT_FALLBACK_SINGLE_WRITER: &str = "single_writer";

/// Schema identifier for the incremental index-intake telemetry contract (bd-d67os.5).
pub const INDEX_INTAKE_SCHEMA_V1: &str = "ee.index_intake.v1";
/// Index-intake mode: full rebuild of the entire index from all documents.
pub const INDEX_INTAKE_MODE_FULL_REBUILD: &str = "full_rebuild";
/// Index-intake mode: incremental single-document or small-delta intake.
pub const INDEX_INTAKE_MODE_INCREMENTAL: &str = "incremental";
/// Index-intake mode: periodic segment/WAL merge maintenance.
pub const INDEX_INTAKE_MODE_SEGMENT_MERGE: &str = "segment_merge";
/// Index-intake fallback-to-full reason: no built index was present to update.
pub const INDEX_INTAKE_FALLBACK_INDEX_ABSENT: &str = "index_absent";
/// Index-intake fallback-to-full reason: index vs DB generation skew.
pub const INDEX_INTAKE_FALLBACK_GENERATION_SKEW: &str = "generation_skew";
/// Index-intake fallback-to-full reason: the active index uses different corpus semantics.
pub const INDEX_INTAKE_FALLBACK_CORPUS_REVISION_MISMATCH: &str = "corpus_revision_mismatch";
/// Index-intake fallback-to-full reason: a persisted tier could not be reopened.
pub const INDEX_INTAKE_FALLBACK_TIER_UNAVAILABLE: &str = "tier_unavailable";
/// Index-intake fallback-to-full reason: a forced reindex was requested.
pub const INDEX_INTAKE_FALLBACK_FORCED_REINDEX: &str = "forced_reindex";
/// Index-intake fallback-to-full reason: the delta exceeded the incremental threshold.
pub const INDEX_INTAKE_FALLBACK_DELTA_OVER_THRESHOLD: &str = "delta_over_threshold";

/// Schema identifier for the read-only swarm hotset manifest contract (bd-ty3pl.1).
pub const HOTSET_MANIFEST_SCHEMA_V1: &str = "ee.hotset_manifest.v1";
/// Schema identifier for the bounded production hotset collector (bd-ty3pl.2).
pub const CACHE_HOTSET_COLLECT_SCHEMA_V1: &str = "ee.cache.hotset_collect.v1";

/// Schema identifier for the active embedding posture block (bd-1et0v.1).
pub const EMBEDDING_POSTURE_SCHEMA_V1: &str = "ee.embedding_posture.v1";
/// Embedding posture mode: local neural semantic model is active.
pub const EMBEDDING_POSTURE_MODE_NEURAL_LOCAL: &str = "neural_local";
/// Embedding posture mode: the bundled local neural model is download-capable but not loaded yet.
pub const EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING: &str = "neural_local_pending";
/// Embedding posture mode: deterministic hash fallback is active.
pub const EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH: &str = "deterministic_hash";
/// Embedding posture mode: a neural model exists but download/load policy blocked it.
pub const EMBEDDING_POSTURE_MODE_NEURAL_REMOTE_BLOCKED: &str = "neural_remote_blocked";

/// Schema identifier for the proactive ambient hook profile (bd-2vq2z.10).
pub const AMBIENT_CONTEXT_SCHEMA_V1: &str = "ee.ambient_context.v1";

/// Schema identifier for provenance freshness diagnostics (bd-2vq2z.2).
pub const PROVENANCE_HEALTH_SCHEMA_V1: &str = "ee.provenance_health.v1";

/// Schema identifier for trust calibration diagnostics (bd-2vq2z.3).
pub const TRUST_REPORT_SCHEMA_V1: &str = "ee.trust_report.v1";

/// Schema identifier for the user-global memory store-metadata block (bd-2vq2z.13).
pub const GLOBAL_MEMORY_SCHEMA_V1: &str = "ee.global_memory.v1";

/// Schema identifier for the time-travel memory audit report (bd-2vq2z.16).
pub const TIMELINE_SCHEMA_V1: &str = "ee.timeline.v1";

/// Schema identifier for the public read-only session-orientation bundle.
pub const ORIENT_SCHEMA_V1: &str = "ee.orient.v1";

/// Schema identifier for the bounded read-only Revive-sentinel list.
pub const MEMORY_SENTINEL_REVIVALS_SCHEMA_V1: &str = "ee.memory_sentinel.revivals.v1";

/// Schema identifier for task-specific capture-demand coverage gaps (bd-2vq2z.17).
pub const COVERAGE_GAP_SCHEMA_V1: &str = "ee.coverage_gap.v1";

/// Schema identifier for the authenticated mesh transport frame.
pub const MESH_TAILSCALE_TRANSPORT_FRAME_SCHEMA_V2: &str = "ee.mesh.tailscale_transport_frame.v2";
/// Schema identifier for the initiator's authenticated mesh session open.
pub const MESH_SESSION_OPEN_SCHEMA_V1: &str = "ee.mesh.session_open.v1";
/// Schema identifier for the responder's authenticated mesh session confirmation.
pub const MESH_SESSION_CONFIRM_SCHEMA_V1: &str = "ee.mesh.session_confirm.v1";
/// Schema identifier for the initiator's authenticated mesh session finish.
pub const MESH_SESSION_FINISH_SCHEMA_V1: &str = "ee.mesh.session_finish.v1";
/// Schema identifier for authenticated mesh session capability negotiation.
pub const MESH_SESSION_CAPABILITY_NEGOTIATION_SCHEMA_V1: &str =
    "ee.mesh.session_capability_negotiation.v1";
/// Schema identifier for queryless, workspace-scoped attempt-family retrieval.
pub const SEARCH_FAMILY_SCHEMA_V1: &str = "ee.search.family.v1";

/// All known schema identifiers for validation.
pub const KNOWN_SCHEMAS: &[&str] = &[
    RESPONSE_SCHEMA_V0,
    RESPONSE_SCHEMA_V1,
    RESPONSE_SCHEMA_V2,
    ERROR_SCHEMA_V2,
    PACK_SCHEMA_V2,
    PERF_SCHEMA_V1,
    BEADS_RETRY_SCHEMA_V1,
    DOCTOR_FIX_SUMMARY_SCHEMA_V1,
    DOCTOR_RUN_DIFF_SCHEMA_V1,
    DOCTOR_UNDO_SUMMARY_SCHEMA_V1,
    FAILURE_MODE_FIXTURE_SCHEMA_V1,
    PACK_DNA_SCHEMA_V1,
    PROXIMITY_SCHEMA_V1,
    PROOF_CHECK_SCHEMA_V1,
    PACK_QUALITY_REPORT_SCHEMA_V1,
    PACK_STREAM_SCHEMA_V1,
    TEST_EVENT_SCHEMA_V1,
    MODEL_STATUS_SCHEMA_V2,
    MODEL_LIST_SCHEMA_V1,
    BACKUP_CREATE_SCHEMA_V1,
    BACKUP_RESTORE_SCHEMA_V1,
    BACKUP_MANIFEST_SCHEMA_V1,
    BACKUP_MANIFEST_SCHEMA_V2,
    IMPORT_CASS_SCHEMA_V1,
    IMPORT_EIDETIC_LEGACY_SCAN_SCHEMA_V1,
    IMPORT_JSONL_SCHEMA_V1,
    IMPORT_LEDGER_SCHEMA_V1,
    IMPORT_LEDGER_CASS_SCHEMA_V1,
    CASS_SESSION_SCHEMA_V1,
    CASS_EVIDENCE_SPAN_SCHEMA_V1,
    SEARCH_MODULE_SCHEMA_V1,
    SEARCH_DOCUMENT_SCHEMA_V1,
    SEARCH_FAMILY_SCHEMA_V1,
    "ee.query_assist.v1",
    GRAPH_MODULE_SCHEMA_V1,
    MESH_EVENT_SCHEMA_V1,
    MESH_PEER_GROUP_BINDING_SCHEMA_V1,
    MESH_PEER_POLICY_SCHEMA_V1,
    MESH_POLICY_DECISION_SCHEMA_V1,
    MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1,
    MESH_STORAGE_STATUS_SCHEMA_V1,
    MESH_SESSION_CAPABILITY_NEGOTIATION_SCHEMA_V1,
    MESH_SESSION_CONFIRM_SCHEMA_V1,
    MESH_SESSION_FINISH_SCHEMA_V1,
    MESH_SESSION_OPEN_SCHEMA_V1,
    MESH_TAILSCALE_TRANSPORT_FRAME_SCHEMA_V2,
    CONTEXT_PROFILE_SCHEMA_V1,
    CONTEXT_PROFILE_SCHEMA_CATALOG_V1,
    FOCUS_ITEM_SCHEMA_V1,
    FOCUS_STATE_SCHEMA_V1,
    FOCUS_SCHEMA_CATALOG_V1,
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
    ATTENTION_BUDGET_SCHEMA_V1,
    RISK_RESERVE_SCHEMA_V1,
    TAIL_RISK_RESERVE_RULE_SCHEMA_V1,
    MAINTENANCE_DEBT_SCHEMA_V1,
    ECONOMY_RECOMMENDATION_SCHEMA_V1,
    ECONOMY_REPORT_SCHEMA_V1,
    ECONOMY_SIMULATION_SCHEMA_V1,
    ECONOMY_SCHEMA_CATALOG_V1,
    SESSION_BUDGET_SCHEMA_V1,
    SESSION_BUDGET_PLAN_SCHEMA_V1,
    SCALE_ENVELOPE_SCHEMA_V1,
    WRITE_GROUP_COMMIT_SCHEMA_V1,
    INDEX_INTAKE_SCHEMA_V1,
    HOTSET_MANIFEST_SCHEMA_V1,
    CACHE_HOTSET_COLLECT_SCHEMA_V1,
    EMBEDDING_POSTURE_SCHEMA_V1,
    AMBIENT_CONTEXT_SCHEMA_V1,
    PROVENANCE_HEALTH_SCHEMA_V1,
    TRUST_REPORT_SCHEMA_V1,
    GLOBAL_MEMORY_SCHEMA_V1,
    TIMELINE_SCHEMA_V1,
    ORIENT_SCHEMA_V1,
    MEMORY_SENTINEL_REVIVALS_SCHEMA_V1,
    COVERAGE_GAP_SCHEMA_V1,
    // Active learning agenda and experiment schemas (EE-440)
    LEARNING_QUESTION_SCHEMA_V1,
    UNCERTAINTY_ESTIMATE_SCHEMA_V1,
    LEARNING_EXPERIMENT_SCHEMA_V1,
    LEARNING_OBSERVATION_SCHEMA_V1,
    EXPERIMENT_OUTCOME_SCHEMA_V1,
    LEARNING_SCHEMA_CATALOG_V1,
    // Causal memory credit and uplift schemas (EE-450)
    CAUSAL_EXPOSURE_SCHEMA_V1,
    DECISION_TRACE_SCHEMA_V1,
    UPLIFT_ESTIMATE_SCHEMA_V1,
    CONFOUNDER_SCHEMA_V1,
    PROMOTION_PLAN_SCHEMA_V1,
    CAUSAL_SCHEMA_CATALOG_V1,
    CAUSAL_TRACE_SCHEMA_V1,
    // JSONL export schemas (EE-220)
    EXPORT_HEADER_SCHEMA_V1,
    EXPORT_MEMORY_SCHEMA_V1,
    EXPORT_ARTIFACT_SCHEMA_V1,
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
    // Situation and task-signature schemas (EE-420)
    SITUATION_CLASSIFY_SCHEMA_V1,
    SITUATION_SHOW_SCHEMA_V1,
    SITUATION_EXPLAIN_SCHEMA_V1,
    SITUATION_SCHEMA_V1,
    TASK_SIGNATURE_SCHEMA_V1,
    FEATURE_EVIDENCE_SCHEMA_V1,
    ROUTING_DECISION_SCHEMA_V1,
    SITUATION_LINK_SCHEMA_V1,
    SITUATION_SCHEMA_CATALOG_V1,
    SYMBOL_SNAPSHOT_SCHEMA_V1,
    SYMBOL_EVIDENCE_LINKS_SCHEMA_V1,
    SINGLEFLIGHT_KEY_SCHEMA_V1,
    SINGLEFLIGHT_POSTURE_SCHEMA_V1,
    // Performance forensics artifact summary schemas (mwjq.2)
    ARTIFACT_SUMMARY_SCHEMA_V1,
    PERF_METRIC_SCHEMA_V1,
    PERF_SCHEMA_CATALOG_V1,
    "ee.why.conformal_prediction_set.v1",
    "ee.why.influence.v1",
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
    let name = stripped.get(..dot_v_pos)?;
    let version = stripped.get(dot_v_pos + 2..)?;
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
            // Check for the highest known version we support.
            let v2_schema = format!("ee.{name}.v2");
            let v1_schema = format!("ee.{name}.v1");

            if is_known_schema(&v2_schema) && version != "2" {
                return Err(SchemaValidationError::UnsupportedVersion {
                    schema: schema.to_owned(),
                    expected_version: "v2".to_owned(),
                });
            } else if is_known_schema(&v1_schema) && version != "1" {
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
    fn global_memory_schema_is_registered() -> TestResult {
        ensure(
            is_known_schema(GLOBAL_MEMORY_SCHEMA_V1),
            true,
            "global memory schema should be known",
        )?;
        ensure(
            validate_schema(GLOBAL_MEMORY_SCHEMA_V1),
            Ok(()),
            "global memory schema should validate",
        )
    }

    #[test]
    fn timeline_schema_is_registered() -> TestResult {
        ensure(
            is_known_schema(TIMELINE_SCHEMA_V1),
            true,
            "timeline schema should be known",
        )?;
        ensure(
            validate_schema(TIMELINE_SCHEMA_V1),
            Ok(()),
            "timeline schema should validate",
        )
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
        // ee.response.v999 should fail because we only know up to v2
        let result = validate_schema("ee.response.v999");
        ensure(
            result,
            Err(SchemaValidationError::UnsupportedVersion {
                schema: "ee.response.v999".to_owned(),
                expected_version: "v2".to_owned(),
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
        let result = validate_schema_match(RESPONSE_SCHEMA_V1, ERROR_SCHEMA_V2);
        ensure(
            result,
            Err(SchemaValidationError::SchemaMismatch {
                expected: RESPONSE_SCHEMA_V1.to_owned(),
                actual: ERROR_SCHEMA_V2.to_owned(),
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
                    schema: "ee.response.v999".to_owned(),
                    expected_version: "v2".to_owned(),
                },
                "unsupported schema version: ee.response.v999",
            ),
            (
                SchemaValidationError::SchemaMismatch {
                    expected: "ee.response.v1".to_owned(),
                    actual: "ee.error.v2".to_owned(),
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
