use std::process::ExitCode;

pub mod backup;
pub mod causal;
pub mod certificate;
pub mod claims;
pub mod context_profile;
pub mod decision;
pub mod degradation;
pub mod demo;
pub mod economy;
pub mod episode;
pub mod error_codes;
pub mod focus;
pub mod id;
pub mod install;
pub mod jsonl;
pub mod learn;
pub mod memory;
pub mod model_registry;
pub mod mutation;
pub mod posture;
pub mod preflight;
pub mod procedure;
pub mod progress;
pub mod provenance;
pub mod recorder;
pub mod release;
pub mod repro;
pub mod revision;
pub mod rule;
pub mod schema;
pub mod situation;
pub mod timing;
pub mod trust;

pub use backup::{
    BACKUP_CREATE_SCHEMA_V1, BACKUP_INSPECT_SCHEMA_V1, BACKUP_LIST_SCHEMA_V1,
    BACKUP_MANIFEST_SCHEMA_V1, BACKUP_RESTORE_SCHEMA_V1, BACKUP_VERIFY_SCHEMA_V1,
};
pub use causal::{
    CAUSAL_EXPOSURE_SCHEMA_V1, CAUSAL_SCHEMA_CATALOG_V1, CAUSAL_TRACE_SCHEMA_V1,
    CONFOUNDER_SCHEMA_V1, CausalConfounder, CausalDecisionTrace, CausalEvidenceStrength,
    CausalExposure, CausalExposureChannel, CausalFieldSchema, CausalObjectSchema, ConfounderKind,
    DECISION_TRACE_SCHEMA_V1, DecisionTraceOutcome, PROMOTION_PLAN_SCHEMA_V1,
    ParseCausalValueError, PromotionAction, PromotionPlan, PromotionPlanStatus,
    UPLIFT_ESTIMATE_SCHEMA_V1, UpliftDirection, UpliftEstimate, causal_schema_catalog_json,
    causal_schemas,
};
pub use certificate::{
    CERTIFICATE_SCHEMA_V1, Certificate, CertificateKind, CertificateStatus, CurationCertificate,
    LifecycleCertificate, LifecycleEvent, PackCertificate, ParseCertificateKindError,
    ParseCertificateStatusError, ParseLifecycleEventError, PrivacyBudgetCertificate,
    TailRiskCertificate,
};
pub use claims::{
    ArtifactType, BLAKE3_HEX_LEN, CLAIM_ENTRY_SCHEMA_V1, CLAIM_MANIFEST_SCHEMA_V1,
    CLAIMS_FILE_SCHEMA_V1, ClaimEntry, ClaimManifest, ClaimStatus, ClaimsFile,
    MANIFEST_ARTIFACT_SCHEMA_V1, ManifestArtifact, ManifestValidationError,
    ManifestValidationErrorKind, ManifestVerificationStatus, ParseArtifactTypeError,
    ParseClaimStatusError, ParseManifestVerificationStatusError, ParseVerificationFrequencyError,
    VerificationFrequency, is_valid_artifact_path, is_valid_blake3_hex, validate_artifact_entry,
    validate_manifest_structure,
};
pub use context_profile::{
    CONTEXT_PROFILE_SCHEMA_CATALOG_V1, CONTEXT_PROFILE_SCHEMA_V1, ContextProfile,
    ContextProfileFieldSchema, ContextProfileName, ContextProfileObjectSchema,
    ContextProfileObjective, ContextProfileSection, ContextProfileSectionMix,
    ContextProfileValidationError, context_profile_schema_catalog_json, context_profile_schemas,
};
pub use decision::{
    DECISION_PLANE_SCHEMA_V1, DecisionPlane, DecisionPlaneMetadata, DecisionRecord,
    DecisionRecordBuilder, ParseDecisionPlaneError,
};
pub use degradation::{
    ALL_DEGRADATION_CODES, ActiveDegradation, DegradationCode, DegradationSeverity,
    DegradedSubsystem,
};
pub use demo::{
    DEMO_ARTIFACT_OUTPUT_SCHEMA_V1, DEMO_COMMAND_SCHEMA_V1, DEMO_ENTRY_SCHEMA_V1,
    DEMO_FILE_SCHEMA_V1, DEMO_RUN_RESULT_SCHEMA_V1, DemoArtifactOutput, DemoCommand,
    DemoCommandResult, DemoEntry, DemoFile, DemoParseError, DemoRunResult, DemoStatus,
    DemoValidationError, DemoValidationErrorKind, OutputVerification, ParseDemoStatusError,
    ParseOutputVerificationError, is_valid_demo_artifact_path, parse_demo_file_yaml,
    validate_demo_file,
};
pub use economy::{
    ATTENTION_BUDGET_SCHEMA_V1, ATTENTION_COST_SCHEMA_V1, AggregateUtility,
    AttentionBudgetAllocation, AttentionBudgetRequest, AttentionCost, ContextAttentionProfile,
    DebtLevel, ECONOMY_RECOMMENDATION_SCHEMA_V1, ECONOMY_REPORT_SCHEMA_V1,
    ECONOMY_SCHEMA_CATALOG_V1, ECONOMY_SIMULATION_SCHEMA_V1, EconomyFieldSchema,
    EconomyObjectSchema, EconomyRecommendation, EconomyReport, EconomyRiskCategory, Effort, Impact,
    MAINTENANCE_DEBT_SCHEMA_V1, MaintenanceDebt, RISK_RESERVE_SCHEMA_V1, RecommendationType,
    RiskReserve, SituationAttentionProfile, TAIL_RISK_RESERVE_RULE_SCHEMA_V1, TailRiskArtifactKind,
    TailRiskDemotionAction, TailRiskReserveRule, TailRiskSeverity, UTILITY_VALUE_SCHEMA_V1,
    UtilityValue, economy_schema_catalog_json, economy_schemas,
};
pub use episode::{
    ActionType, COUNTERFACTUAL_CLAIM_ID_PREFIX, COUNTERFACTUAL_CLAIM_SCHEMA_V1,
    COUNTERFACTUAL_RUN_ID_PREFIX, COUNTERFACTUAL_RUN_SCHEMA_V1, CounterfactualClaim,
    CounterfactualClaimType, CounterfactualMethod, CounterfactualRun, EPISODE_ID_PREFIX,
    EpisodeAction, EpisodeOutcome, INTERVENTION_ID_PREFIX, INTERVENTION_SCHEMA_V1, Intervention,
    InterventionType, ParseActionTypeError, ParseCounterfactualClaimTypeError,
    ParseCounterfactualMethodError, ParseEpisodeOutcomeError, ParseInterventionTypeError,
    ParseRegretCategoryError, REGRET_DELTA_SCHEMA_V1, REGRET_ENTRY_ID_PREFIX,
    REGRET_ENTRY_SCHEMA_V1, REGRET_LEDGER_SCHEMA_V1, RegretCategory, RegretDelta, RegretEntry,
    RegretLedger, RegretSummary, TASK_EPISODE_SCHEMA_V1, TaskEpisode,
};
pub use focus::{
    FOCUS_ITEM_SCHEMA_V1, FOCUS_SCHEMA_CATALOG_V1, FOCUS_STATE_SCHEMA_V1, FocusCapacityStatus,
    FocusFieldSchema, FocusItem, FocusObjectSchema, FocusState, FocusValidationError,
    focus_schema_catalog_json, focus_schemas,
};
pub use id::{
    AuditId, BackupId, CandidateId, ClaimId, DemoId, EXECUTABLE_ID_SCHEMA_V1, EvidenceId,
    ExecutableIdKind, Id, IdJsonSchema, IdKind, MemoryId, MemoryLinkId, ModelId, PackId,
    ParseExecutableIdKindError, ParseIdError, PolicyId, RuleId, SessionId, TraceId, WorkspaceId,
    executable_id_schema_catalog_json, executable_id_schemas,
};
pub use install::{
    CurrentBinary, INSTALL_CHECK_SCHEMA_V1, INSTALL_PLAN_SCHEMA_V1, InstallArtifactSelection,
    InstallCheckReport, InstallFinding, InstallFindingCode, InstallFindingSeverity,
    InstallOperation, InstallPathAnalysis, InstallPathStatus, InstallPermissionCheck,
    InstallPermissionStatus, InstallPlanReport, InstallPlanStatus, InstallTarget,
    InstallVerificationPlan, PathBinary, PlannedInstallOperation, UPDATE_PLAN_SCHEMA_V1,
    UpdateSourcePosture, compare_versions, findings_status, is_safe_install_path,
};
pub use jsonl::{
    ALL_EXPORT_SCHEMAS, EXPORT_AGENT_SCHEMA_V1, EXPORT_ARTIFACT_SCHEMA_V1, EXPORT_AUDIT_SCHEMA_V1,
    EXPORT_FOOTER_SCHEMA_V1, EXPORT_FORMAT_VERSION, EXPORT_HEADER_SCHEMA_V1, EXPORT_LINK_SCHEMA_V1,
    EXPORT_MEMORY_SCHEMA_V1, EXPORT_TAG_SCHEMA_V1, EXPORT_WORKSPACE_SCHEMA_V1, ExportAgentRecord,
    ExportAgentRecordBuilder, ExportArtifactRecord, ExportArtifactRecordBuilder, ExportAuditRecord,
    ExportAuditRecordBuilder, ExportFooter, ExportFooterBuilder, ExportHeader, ExportHeaderBuilder,
    ExportLinkRecord, ExportLinkRecordBuilder, ExportMemoryRecord, ExportMemoryRecordBuilder,
    ExportRecord, ExportRecordType, ExportScope, ExportTagRecord, ExportWorkspaceRecord,
    ExportWorkspaceRecordBuilder, ImportSource, ParseExportRecordTypeError, ParseExportScopeError,
    ParseImportSourceError, ParseRedactionLevelError, ParseTrustLevelError, RedactionLevel,
    TrustLevel,
};
pub use learn::{
    EXPERIMENT_OUTCOME_SCHEMA_V1, ExperimentOutcome, ExperimentOutcomeStatus,
    ExperimentSafetyBoundary, LEARNING_EXPERIMENT_SCHEMA_V1, LEARNING_OBSERVATION_SCHEMA_V1,
    LEARNING_QUESTION_SCHEMA_V1, LEARNING_SCHEMA_CATALOG_V1, LearningExperiment,
    LearningExperimentStatus, LearningFieldSchema, LearningObjectSchema, LearningObservation,
    LearningObservationSignal, LearningQuestion, LearningQuestionStatus, LearningTargetKind,
    ParseLearningValueError, UNCERTAINTY_ESTIMATE_SCHEMA_V1, UncertaintyEstimate,
    learning_schema_catalog_json, learning_schemas,
};
pub use memory::{
    Confidence, Importance, KNOWN_MEMORY_KINDS, MAX_CONTENT_BYTES, MAX_TAG_BYTES, MemoryContent,
    MemoryKind, MemoryLevel, MemoryValidationError, Tag, UnitScore, Utility,
};
pub use model_registry::{
    EMBEDDING_METADATA_SCHEMA_V1, EmbeddingMetadataFieldSchema, EmbeddingMetadataObjectSchema,
    EmbeddingMetadataRecord, EmbeddingMetadataValidationError, EmbeddingPooling,
    EmbeddingVectorDtype, MODEL_REGISTRY_SCHEMA_V1, ModelDistanceMetric, ModelProvider,
    ModelPurpose, ModelRegistryStatus, ParseModelRegistryValueError,
    embedding_metadata_schema_catalog_json, embedding_metadata_schemas,
};
pub use mutation::{
    DRY_RUN_PREVIEW_SCHEMA_V1, DryRunPreview, DryRunSummary, IdempotencyClass,
    MUTATION_RESPONSE_SCHEMA_V1, MutationActionStatus, MutationActionType, MutationResponse,
    MutationSummary, ParseIdempotencyClassError, ParseMutationActionStatusError,
    ParseMutationActionTypeError, PlannedAction,
};
pub use posture::{ActionCategory, Posture, PostureSummary, SuggestedAction};
pub use preflight::{
    PREFLIGHT_RUN_ID_PREFIX, PREFLIGHT_RUN_SCHEMA_V1, ParsePreflightStatusError,
    ParseRiskCategoryError, ParseRiskLevelError, ParseTripwireActionError,
    ParseTripwireEventTypeError, ParseTripwireStateError, ParseTripwireTypeError, PreflightRun,
    PreflightStatus, RISK_BRIEF_ID_PREFIX, RISK_BRIEF_SCHEMA_V1, RiskBrief, RiskCategory, RiskItem,
    RiskLevel, TRIPWIRE_EVENT_ID_PREFIX, TRIPWIRE_EVENT_SCHEMA_V1, TRIPWIRE_ID_PREFIX,
    TRIPWIRE_SCHEMA_V1, Tripwire, TripwireAction, TripwireEvent, TripwireEventType, TripwireState,
    TripwireType,
};
pub use procedure::{
    PROCEDURE_EXPORT_SCHEMA_V1, PROCEDURE_SCHEMA_CATALOG_V1, PROCEDURE_SCHEMA_V1,
    PROCEDURE_STEP_SCHEMA_V1, PROCEDURE_VERIFICATION_SCHEMA_V1, ParseProcedureValueError,
    Procedure, ProcedureExport, ProcedureExportFormat, ProcedureFieldSchema, ProcedureObjectSchema,
    ProcedureStatus, ProcedureStep, ProcedureVerification, ProcedureVerificationStatus,
    SKILL_CAPSULE_SCHEMA_V1, SkillCapsule, SkillCapsuleInstallMode, procedure_schema_catalog_json,
    procedure_schemas,
};
pub use progress::{
    PROGRESS_EVENT_SCHEMA_V1, ParseProgressEventTypeError, ProgressEvent, ProgressEventBuilder,
    ProgressEventType, progress_completed, progress_failed, progress_running, progress_started,
};
pub use provenance::{LineSpan, ProvenanceUri, ProvenanceUriError};
pub use recorder::{
    IMPORT_CURSOR_SCHEMA_V1, ImportCursor, ImportSourceType, ParseImportSourceTypeError,
    ParsePayloadContentTypeError, ParseRationaleTraceKindError, ParseRationaleTracePostureError,
    ParseRationaleTraceVisibilityError, ParseRecorderEventTypeError, ParseRecorderRunStatusError,
    ParseRedactionStatusError, PayloadContentType, RATIONALE_TRACE_SCHEMA_V1,
    RECORDER_EVENT_SCHEMA_V1, RECORDER_IMPORT_PLAN_SCHEMA_V1, RECORDER_PAYLOAD_SCHEMA_V1,
    RECORDER_RUN_SCHEMA_V1, RECORDER_SCHEMA_CATALOG_V1, REDACTION_STATUS_SCHEMA_V1, RationaleTrace,
    RationaleTraceKind, RationaleTracePosture, RationaleTraceValidationError,
    RationaleTraceValidationErrorKind, RationaleTraceVisibility, RecorderEvent,
    RecorderEventChainStatus, RecorderEventType, RecorderFieldSchema, RecorderObjectSchema,
    RecorderPayload, RecorderRunMeta, RecorderRunStatus, RedactionStatus, RedactionStatusSnapshot,
    recorder_schema_catalog_json, recorder_schemas, validate_rationale_summary,
};
pub use release::{
    RELEASE_ARTIFACT_SCHEMA_V1, RELEASE_BINARY_NAME, RELEASE_MANIFEST_SCHEMA_V1,
    RELEASE_MANIFEST_VERIFICATION_SCHEMA_V1, RELEASE_SCHEMA_CATALOG_V1, ReleaseArchiveFormat,
    ReleaseArtifact, ReleaseChecksum, ReleaseChecksumAlgorithm, ReleaseInstallLayout,
    ReleaseManifest, ReleaseProvenance, ReleaseSignature, ReleaseVerificationCode,
    ReleaseVerificationFinding, ReleaseVerificationReport, ReleaseVerificationSeverity,
    ReleaseVerificationStatus, compatibility_notes_for_target, default_archive_format,
    default_install_path, is_allowed_package_member_path, is_safe_release_artifact_path,
    is_supported_release_target, minimum_os_assumptions, release_artifact_file_name,
    release_artifact_id, release_executable_name, release_tag, sha256_hex,
    verify_release_manifest_json,
};
pub use repro::{
    DependencyCategory, ParseDependencyCategoryError, ParseProvenanceEventTypeError,
    ProvenanceEvent, ProvenanceEventType, ProvenanceSource, ProvenanceVerification,
    REPRO_ENV_SCHEMA_V1, REPRO_LOCK_SCHEMA_V1, REPRO_MANIFEST_SCHEMA_V1, REPRO_PACK_SCHEMA_V1,
    REPRO_PROVENANCE_SCHEMA_V1, ReproArtifact, ReproDependency, ReproEnv, ReproLock, ReproManifest,
    ReproProvenance,
};
pub use revision::{
    IdempotencyKey, IdempotencyKeyError, LEGAL_HOLD_ID_LEN, LEGAL_HOLD_PREFIX, LegalHold,
    LegalHoldId, REVISION_GROUP_ID_LEN, REVISION_GROUP_PREFIX, RevisionGroupId, RevisionIdError,
    RevisionMeta, SupersessionLink, SupersessionReason,
};
pub use rule::{
    ParseRuleLifecycleActionError, ParseRuleLifecycleTriggerError, ParseRuleMaturityError,
    ParseRuleScopeError, RuleLifecycleAction, RuleLifecycleEvidence, RuleLifecycleTransition,
    RuleLifecycleTrigger, RuleMaturity, RuleScope,
};
pub use schema::{
    KNOWN_SCHEMAS, SchemaValidationError, is_known_schema, parse_schema_parts, validate_schema,
    validate_schema_match,
};
pub use situation::{
    FEATURE_EVIDENCE_SCHEMA_V1, FeatureEvidence, ParseSituationValueError,
    ROUTING_DECISION_SCHEMA_V1, RoutingDecision, SITUATION_CLASSIFY_SCHEMA_V1,
    SITUATION_EXPLAIN_SCHEMA_V1, SITUATION_LINK_SCHEMA_V1, SITUATION_SCHEMA_CATALOG_V1,
    SITUATION_SCHEMA_V1, SITUATION_SHOW_SCHEMA_V1, Situation, SituationCategory,
    SituationConfidence, SituationFeatureType, SituationFieldSchema, SituationLink,
    SituationLinkRelation, SituationObjectSchema, SituationReplayPolicy, SituationRoutingSurface,
    TASK_SIGNATURE_SCHEMA_V1, TaskSignature, situation_schema_catalog_json, situation_schemas,
};
pub use timing::{DiagnosticTiming, TimingCapture, TimingPhase};
pub use trust::{ParseTrustClassError, TrustClass};

// ============================================================================
// Public JSON Contract Schema Constants
//
// These constants define the schema identifiers for all public JSON contracts.
// They MUST be used instead of inline string literals to ensure consistency
// and enable schema drift detection.
// ============================================================================

/// Response envelope schema for successful command output.
pub const RESPONSE_SCHEMA_V1: &str = "ee.response.v1";

/// Error envelope schema for failed command output.
pub const ERROR_SCHEMA_V1: &str = "ee.error.v1";

/// Schema for query request documents (`--query-file`).
pub const QUERY_SCHEMA_V1: &str = "ee.query.v1";

/// Schema for CASS import reports (`ee import cass`).
pub const IMPORT_CASS_SCHEMA_V1: &str = "ee.import.cass.v1";

/// Schema for read-only legacy Eidetic import scans.
pub const IMPORT_EIDETIC_LEGACY_SCAN_SCHEMA_V1: &str = "ee.import.eidetic_legacy.scan.v1";

/// Schema for JSONL import reports (`ee import jsonl`).
pub const IMPORT_JSONL_SCHEMA_V1: &str = "ee.import.jsonl.v1";

/// Schema for review session reports (`ee review session --propose`).
pub const REVIEW_SESSION_SCHEMA_V1: &str = "ee.review.session.v1";

/// Schema for import ledger entries.
pub const IMPORT_LEDGER_SCHEMA_V1: &str = "ee.import_ledger.v1";

/// Schema for CASS-specific import ledger entries.
pub const IMPORT_LEDGER_CASS_SCHEMA_V1: &str = "ee.import_ledger.cass.v1";

/// Schema for imported CASS session metadata.
pub const CASS_SESSION_SCHEMA_V1: &str = "ee.cass_session.v1";

/// Schema for CASS evidence span entries.
pub const CASS_EVIDENCE_SPAN_SCHEMA_V1: &str = "ee.cass_evidence_span.v1";

/// Schema for search module readiness.
pub const SEARCH_MODULE_SCHEMA_V1: &str = "ee.search.module.v1";

/// Schema for canonical search documents.
pub const SEARCH_DOCUMENT_SCHEMA_V1: &str = "ee.search.document.v1";

/// Schema for graph module readiness.
pub const GRAPH_MODULE_SCHEMA_V1: &str = "ee.graph.module.v1";

/// Schema for evaluation fixtures.
pub const EVAL_FIXTURE_SCHEMA_V1: &str = "ee.eval_fixture.v1";

/// Schema for release gate checks (EE-348).
pub const RELEASE_GATE_SCHEMA_V1: &str = "ee.eval.release_gate.v1";

/// Schema for tail budget configuration (EE-348).
pub const TAIL_BUDGET_CONFIG_SCHEMA_V1: &str = "ee.eval.tail_budget_config.v1";

/// Schema for index manifest (tracking index state and staleness).
pub const INDEX_MANIFEST_SCHEMA_V1: &str = "ee.index_manifest.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainError {
    Usage {
        message: String,
        repair: Option<String>,
    },
    Configuration {
        message: String,
        repair: Option<String>,
    },
    Storage {
        message: String,
        repair: Option<String>,
    },
    SearchIndex {
        message: String,
        repair: Option<String>,
    },
    Graph {
        message: String,
        repair: Option<String>,
    },
    Import {
        message: String,
        repair: Option<String>,
    },
    NotFound {
        resource: String,
        id: String,
        repair: Option<String>,
    },
    UnsatisfiedDegradedMode {
        message: String,
        repair: Option<String>,
    },
    PolicyDenied {
        message: String,
        repair: Option<String>,
    },
    MigrationRequired {
        message: String,
        repair: Option<String>,
    },
}

impl DomainError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Usage { .. } => "usage",
            Self::Configuration { .. } => "configuration",
            Self::Storage { .. } => "storage",
            Self::SearchIndex { .. } => "search_index",
            Self::Graph { .. } => "graph",
            Self::Import { .. } => "import",
            Self::NotFound { .. } => "not_found",
            Self::UnsatisfiedDegradedMode { .. } => "unsatisfied_degraded_mode",
            Self::PolicyDenied { .. } => "policy_denied",
            Self::MigrationRequired { .. } => "migration_required",
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Usage { message, .. }
            | Self::Configuration { message, .. }
            | Self::Storage { message, .. }
            | Self::SearchIndex { message, .. }
            | Self::Graph { message, .. }
            | Self::Import { message, .. }
            | Self::UnsatisfiedDegradedMode { message, .. }
            | Self::PolicyDenied { message, .. }
            | Self::MigrationRequired { message, .. } => message.clone(),
            Self::NotFound { resource, id, .. } => {
                format!("{resource} not found: {id}")
            }
        }
    }

    #[must_use]
    pub fn repair(&self) -> Option<&str> {
        match self {
            Self::Usage { repair, .. }
            | Self::Configuration { repair, .. }
            | Self::Storage { repair, .. }
            | Self::SearchIndex { repair, .. }
            | Self::Graph { repair, .. }
            | Self::Import { repair, .. }
            | Self::NotFound { repair, .. }
            | Self::UnsatisfiedDegradedMode { repair, .. }
            | Self::PolicyDenied { repair, .. }
            | Self::MigrationRequired { repair, .. } => repair.as_deref(),
        }
    }

    #[must_use]
    pub const fn exit_code(&self) -> ProcessExitCode {
        match self {
            Self::Usage { .. } => ProcessExitCode::Usage,
            Self::Configuration { .. } => ProcessExitCode::Configuration,
            Self::Storage { .. } => ProcessExitCode::Storage,
            Self::SearchIndex { .. } => ProcessExitCode::SearchIndex,
            Self::Graph { .. } => ProcessExitCode::Graph,
            Self::Import { .. } => ProcessExitCode::Import,
            Self::NotFound { .. } => ProcessExitCode::NotFound,
            Self::UnsatisfiedDegradedMode { .. } => ProcessExitCode::UnsatisfiedDegradedMode,
            Self::PolicyDenied { .. } => ProcessExitCode::PolicyDenied,
            Self::MigrationRequired { .. } => ProcessExitCode::MigrationRequired,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProcessExitCode {
    Success = 0,
    Usage = 1,
    Configuration = 2,
    Storage = 3,
    SearchIndex = 4,
    Graph = 5,
    Import = 6,
    UnsatisfiedDegradedMode = 7,
    PolicyDenied = 8,
    MigrationRequired = 9,
    NotFound = 10,
}

impl From<ProcessExitCode> for ExitCode {
    fn from(value: ProcessExitCode) -> Self {
        Self::from(value as u8)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityStatus {
    Ready,
    Pending,
    Degraded,
    Unimplemented,
}

impl CapabilityStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Pending => "pending",
            Self::Degraded => "degraded",
            Self::Unimplemented => "unimplemented",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CapabilityStatus, DomainError, ProcessExitCode};

    type TestResult = Result<(), String>;

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        ctx: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn exit_codes_match_project_contract() {
        assert_eq!(ProcessExitCode::Success as u8, 0);
        assert_eq!(ProcessExitCode::Usage as u8, 1);
        assert_eq!(ProcessExitCode::MigrationRequired as u8, 9);
    }

    #[test]
    fn capability_status_strings_are_stable() {
        assert_eq!(CapabilityStatus::Ready.as_str(), "ready");
        assert_eq!(CapabilityStatus::Pending.as_str(), "pending");
        assert_eq!(CapabilityStatus::Degraded.as_str(), "degraded");
        assert_eq!(CapabilityStatus::Unimplemented.as_str(), "unimplemented");
    }

    #[test]
    fn domain_error_codes_are_stable() -> TestResult {
        let cases = [
            (
                DomainError::Usage {
                    message: String::new(),
                    repair: None,
                },
                "usage",
                ProcessExitCode::Usage,
            ),
            (
                DomainError::Configuration {
                    message: String::new(),
                    repair: None,
                },
                "configuration",
                ProcessExitCode::Configuration,
            ),
            (
                DomainError::Storage {
                    message: String::new(),
                    repair: None,
                },
                "storage",
                ProcessExitCode::Storage,
            ),
            (
                DomainError::SearchIndex {
                    message: String::new(),
                    repair: None,
                },
                "search_index",
                ProcessExitCode::SearchIndex,
            ),
            (
                DomainError::Import {
                    message: String::new(),
                    repair: None,
                },
                "import",
                ProcessExitCode::Import,
            ),
            (
                DomainError::UnsatisfiedDegradedMode {
                    message: String::new(),
                    repair: None,
                },
                "unsatisfied_degraded_mode",
                ProcessExitCode::UnsatisfiedDegradedMode,
            ),
            (
                DomainError::PolicyDenied {
                    message: String::new(),
                    repair: None,
                },
                "policy_denied",
                ProcessExitCode::PolicyDenied,
            ),
            (
                DomainError::MigrationRequired {
                    message: String::new(),
                    repair: None,
                },
                "migration_required",
                ProcessExitCode::MigrationRequired,
            ),
        ];
        for (error, expected_code, expected_exit) in cases {
            ensure_equal(&error.code(), &expected_code, "code")?;
            ensure_equal(&error.exit_code(), &expected_exit, "exit_code")?;
        }
        Ok(())
    }

    #[test]
    fn domain_error_message_and_repair_accessors() -> TestResult {
        let err = DomainError::Storage {
            message: "Database locked".to_string(),
            repair: Some("ee doctor --fix-plan --json".to_string()),
        };
        ensure_equal(&err.message(), &"Database locked".to_string(), "message")?;
        ensure_equal(
            &err.repair(),
            &Some("ee doctor --fix-plan --json"),
            "repair",
        )
    }

    #[test]
    fn query_schema_version_is_stable() -> TestResult {
        ensure_equal(
            &super::QUERY_SCHEMA_V1,
            &"ee.query.v1",
            "query schema version",
        )
    }

    #[test]
    fn release_gate_and_tail_budget_schema_versions_are_stable() -> TestResult {
        ensure_equal(
            &super::RELEASE_GATE_SCHEMA_V1,
            &"ee.eval.release_gate.v1",
            "release gate schema",
        )?;
        ensure_equal(
            &super::TAIL_BUDGET_CONFIG_SCHEMA_V1,
            &"ee.eval.tail_budget_config.v1",
            "tail budget config schema",
        )
    }
}
