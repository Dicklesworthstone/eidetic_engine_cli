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
pub mod perf_artifact;
pub mod posture;
pub mod preflight;
pub mod procedure;
pub mod producer;
pub mod progress;
pub mod provenance;
pub mod query;
pub mod recorder;
pub mod release;
pub mod repro;
pub mod revision;
pub mod rule;
pub mod schema;
pub mod situation;
pub mod timing;
pub mod trust;
pub mod verification;
pub mod why_tag;

pub use backup::{
    BACKUP_CREATE_SCHEMA_V1, BACKUP_INSPECT_SCHEMA_V1, BACKUP_LIST_SCHEMA_V1,
    BACKUP_MANIFEST_SCHEMA_V1, BACKUP_RESTORE_SCHEMA_V1, BACKUP_VERIFY_SCHEMA_V1,
};
pub use causal::{
    CAUSAL_EXPOSURE_SCHEMA_V1, CAUSAL_SCHEMA_CATALOG_V1, CAUSAL_TRACE_SCHEMA_V1,
    CONFOUNDER_SCHEMA_V1, CausalConfounder, CausalDecisionTrace, CausalEvidenceMethod,
    CausalEvidenceStrength, CausalExposure, CausalExposureChannel, CausalFieldSchema,
    CausalObjectSchema, ConfounderKind, DECISION_TRACE_SCHEMA_V1, DecisionTraceOutcome,
    PROMOTION_PLAN_SCHEMA_V1, ParseCausalValueError, PromotionAction, PromotionPlan,
    PromotionPlanStatus, UPLIFT_ESTIMATE_SCHEMA_V1, UpliftDirection, UpliftEstimate,
    causal_schema_catalog_json, causal_schemas,
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
pub use perf_artifact::{
    ARTIFACT_SUMMARY_SCHEMA_V1, ArtifactDegradationSeverity, ArtifactKind, ArtifactSummary,
    DegradedSummary, MetricValue, MetricValueKind, PERF_METRIC_SCHEMA_V1, PERF_SCHEMA_CATALOG_V1,
    ParseArtifactKindError, PerfSchemaCatalog, PerfSchemaEntry, ProfileReference, ProvenanceEntry,
    RedactionPosture, SummaryDegradation, SummaryDegradationCode, perf_schema_catalog,
    perf_schema_catalog_json, perf_schemas,
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
    Procedure, ProcedureExport, ProcedureExportFormat, ProcedureFieldSchema, ProcedureMaturity,
    ProcedureObjectSchema, ProcedureStatus, ProcedureStep, ProcedureVerification,
    ProcedureVerificationStatus, SKILL_CAPSULE_SCHEMA_V1, SkillCapsule, SkillCapsuleInstallMode,
    procedure_schema_catalog_json, procedure_schemas,
};
pub use producer::{
    AgentIdentity, AgentRun, PRODUCER_METADATA_SCHEMA_V1, PRODUCER_SCHEMA_CATALOG_V1,
    ProducerFieldSchema, ProducerIdentityStatus, ProducerMetadata, ProducerObjectSchema,
    ProducerSourceSystem, producer_schema_catalog_json, producer_schemas,
};
pub use progress::{
    PROGRESS_EVENT_SCHEMA_V1, ParseProgressEventTypeError, ProgressEvent, ProgressEventBuilder,
    ProgressEventType, progress_completed, progress_failed, progress_running, progress_started,
};
pub use provenance::{LineSpan, ProvenanceUri, ProvenanceUriError};
pub use query::{
    FilterOperator, FilterPredicate, FilterValue, MemoryScope, MemoryScopeStats, PaginationCursor,
    PaginationCursorError, QueryFilter, QueryFilters, QueryGraphHints, QueryGraphTraversal,
    QueryPagination, QueryTemporalFilters, QueryTemporalValidity, QueryTemporalValidityPosture,
    RedactionFilters, TagFilters, TrustFilters, compute_query_shape_hash, parse_filters,
    parse_pagination, parse_redaction, parse_tags, parse_trust, posture_for_trust_class,
};
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
pub use verification::{
    VERIFICATION_CLOSURE_GUIDANCE_SCHEMA_V1, VERIFICATION_EVIDENCE_SCHEMA_V1,
    VerificationArtifactRef, VerificationClosureGuidance, VerificationEnvironment,
    VerificationEvidenceInput, VerificationEvidenceRecord, VerificationGateAssessment,
    VerificationGateRequirement, VerificationOffload, VerificationOutputSummary,
    VerificationStatus, command_hash, rch_cargo_closure_requirements,
    sample_verification_evidence_records, verification_closure_guidance,
};
pub use why_tag::{ParseWhyTagError, WhyTag};

// ============================================================================
// Public JSON Contract Schema Constants
//
// These constants define the schema identifiers for all public JSON contracts.
// They MUST be used instead of inline string literals to ensure consistency
// and enable schema drift detection.
// ============================================================================

/// Legacy response envelope schema retained for one minor-version cycle.
pub const RESPONSE_SCHEMA_V0: &str = "ee.response.v0";

/// Response envelope schema for successful command output.
pub const RESPONSE_SCHEMA_V1: &str = "ee.response.v1";

/// Current error envelope schema for failed command output.
pub const ERROR_SCHEMA_V2: &str = "ee.error.v2";

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
    UsageWithDetails {
        message: String,
        repair: Option<String>,
        details_json: String,
    },
    UsageCodeWithDetails {
        code: &'static str,
        message: String,
        repair: Option<String>,
        details_json: String,
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
    UnsatisfiedDegradedModeCode {
        code: &'static str,
        message: String,
        repair: Option<String>,
    },
    PolicyDenied {
        message: String,
        repair: Option<String>,
    },
    PolicyDeniedWithDetails {
        message: String,
        repair: Option<String>,
        details_json: String,
    },
    MigrationRequired {
        message: String,
        repair: Option<String>,
    },
    MigrationDrift {
        message: String,
        repair: Option<String>,
    },
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage { message, .. }
            | Self::UsageWithDetails { message, .. }
            | Self::UsageCodeWithDetails { message, .. } => write!(f, "usage error: {message}"),
            Self::Configuration { message, .. } => write!(f, "configuration error: {message}"),
            Self::Storage { message, .. } => write!(f, "storage error: {message}"),
            Self::SearchIndex { message, .. } => write!(f, "search index error: {message}"),
            Self::Graph { message, .. } => write!(f, "graph error: {message}"),
            Self::Import { message, .. } => write!(f, "import error: {message}"),
            Self::NotFound { resource, id, .. } => write!(f, "{resource} not found: {id}"),
            Self::UnsatisfiedDegradedMode { message, .. }
            | Self::UnsatisfiedDegradedModeCode { message, .. } => {
                write!(f, "unsatisfied degraded mode: {message}")
            }
            Self::PolicyDenied { message, .. } | Self::PolicyDeniedWithDetails { message, .. } => {
                write!(f, "policy denied: {message}")
            }
            Self::MigrationRequired { message, .. } => {
                write!(f, "migration required: {message}")
            }
            Self::MigrationDrift { message, .. } => write!(f, "migration drift: {message}"),
        }
    }
}

impl std::error::Error for DomainError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainErrorSeverity {
    Low,
    Medium,
    High,
}

impl DomainErrorSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

// ============================================================================
// Bead bd-17c65.6.1 (F1) — structured error recovery actions
// ============================================================================
//
// Pre-overhaul errors carried only a prose `repair` string ("install cass
// or set [cass.binary] in config"). The 2026-05-10 walkthrough surfaced
// that those hints lie: neither the suggested config-key path nor the
// (only-documented-in-source) EE_CASS_BINARY env var were obvious to a
// caller reading the error. F1 makes `recovery[]` a structured array
// agents can iterate without parsing English prose.

/// Categories of recovery action an agent can take in response to an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryKind {
    /// Set an environment variable.
    Env,
    /// Edit a TOML config file at a specific key.
    Config,
    /// Re-run with an additional CLI flag.
    Flag,
    /// Install a missing tool / binary into a trusted location.
    Install,
    /// Rebuild ee with different features.
    Rebuild,
    /// Fix file or directory permissions.
    Permission,
    /// Run a one-time data migration.
    Migration,
    /// Broaden a query (search-specific).
    Broaden,
    /// Narrow / filter a query.
    Narrow,
    /// Add seed data via `ee remember` or similar.
    Seed,
    /// This error has no recovery path; the caller cannot make progress.
    None,
}

impl RecoveryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Config => "config",
            Self::Flag => "flag",
            Self::Install => "install",
            Self::Rebuild => "rebuild",
            Self::Permission => "permission",
            Self::Migration => "migration",
            Self::Broaden => "broaden",
            Self::Narrow => "narrow",
            Self::Seed => "seed",
            Self::None => "none",
        }
    }
}

/// One concrete recovery action attached to an error envelope.
///
/// Fields are intentionally optional: each `RecoveryKind` populates only
/// the fields meaningful to it (`Env` → `name` + `value_hint`; `Install`
/// → `command` + `results_in`; etc.). Agents inspect `kind` and read the
/// appropriate fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryAction {
    /// Lower number = try first. Ties allowed (agent picks any).
    pub priority: u8,
    pub kind: RecoveryKind,
    /// One-sentence rationale: WHY this option vs others. Distinct from
    /// the outer `repair` prose which describes WHAT to do.
    pub rationale: String,
    /// Env var name (kind == Env).
    pub env_name: Option<String>,
    /// Hint value or shape (kind == Env, Config, Flag).
    pub value_hint: Option<String>,
    /// Config file path (kind == Config).
    pub config_path: Option<String>,
    /// Dotted config key (kind == Config).
    pub config_key: Option<String>,
    /// CLI flag name with leading `--` (kind == Flag).
    pub flag_name: Option<String>,
    /// Concrete shell command (kind == Install, Migration, Rebuild).
    pub command: Option<String>,
    /// What running the command produces (kind == Install).
    pub results_in: Option<String>,
    /// Ready-to-copy example invocation.
    pub example: Option<String>,
}

impl RecoveryAction {
    /// Construct an env-var-set recovery.
    #[must_use]
    pub fn env(
        priority: u8,
        name: impl Into<String>,
        value_hint: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            priority,
            kind: RecoveryKind::Env,
            rationale: rationale.into(),
            env_name: Some(name.into()),
            value_hint: Some(value_hint.into()),
            config_path: None,
            config_key: None,
            flag_name: None,
            command: None,
            results_in: None,
            example: None,
        }
    }

    /// Construct a config-edit recovery.
    #[must_use]
    pub fn config(
        priority: u8,
        path: impl Into<String>,
        key: impl Into<String>,
        value_hint: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            priority,
            kind: RecoveryKind::Config,
            rationale: rationale.into(),
            env_name: None,
            value_hint: Some(value_hint.into()),
            config_path: Some(path.into()),
            config_key: Some(key.into()),
            flag_name: None,
            command: None,
            results_in: None,
            example: None,
        }
    }

    /// Construct an install-binary recovery.
    #[must_use]
    pub fn install(
        priority: u8,
        command: impl Into<String>,
        results_in: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            priority,
            kind: RecoveryKind::Install,
            rationale: rationale.into(),
            env_name: None,
            value_hint: None,
            config_path: None,
            config_key: None,
            flag_name: None,
            command: Some(command.into()),
            results_in: Some(results_in.into()),
            example: None,
        }
    }

    /// Construct a CLI-flag recovery.
    #[must_use]
    pub fn flag(
        priority: u8,
        name: impl Into<String>,
        value_hint: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            priority,
            kind: RecoveryKind::Flag,
            rationale: rationale.into(),
            env_name: None,
            value_hint: Some(value_hint.into()),
            config_path: None,
            config_key: None,
            flag_name: Some(name.into()),
            command: None,
            results_in: None,
            example: None,
        }
    }

    /// Construct a migration-run recovery.
    #[must_use]
    pub fn migration(
        priority: u8,
        command: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            priority,
            kind: RecoveryKind::Migration,
            rationale: rationale.into(),
            env_name: None,
            value_hint: None,
            config_path: None,
            config_key: None,
            flag_name: None,
            command: Some(command.into()),
            results_in: None,
            example: None,
        }
    }

    /// Construct a broaden-query recovery (search-specific).
    #[must_use]
    pub fn broaden(priority: u8, hint: impl Into<String>) -> Self {
        Self {
            priority,
            kind: RecoveryKind::Broaden,
            rationale: hint.into(),
            env_name: None,
            value_hint: None,
            config_path: None,
            config_key: None,
            flag_name: None,
            command: None,
            results_in: None,
            example: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainErrorSituation {
    Usage,
    Configuration,
    Storage,
    SearchIndex,
    Graph,
    Import,
    NotFound,
    UnsatisfiedDegradedMode,
    PolicyDenied,
    MigrationRequired,
}

impl DomainError {
    #[must_use]
    pub fn new(
        _code: impl Into<String>,
        _severity: DomainErrorSeverity,
        situation: DomainErrorSituation,
        message: impl Into<String>,
        repair: impl Into<String>,
    ) -> Self {
        let message = message.into();
        let repair = Some(repair.into());
        match situation {
            DomainErrorSituation::Usage => Self::Usage { message, repair },
            DomainErrorSituation::Configuration => Self::Configuration { message, repair },
            DomainErrorSituation::Storage => Self::Storage { message, repair },
            DomainErrorSituation::SearchIndex => Self::SearchIndex { message, repair },
            DomainErrorSituation::Graph => Self::Graph { message, repair },
            DomainErrorSituation::Import => Self::Import { message, repair },
            DomainErrorSituation::NotFound => Self::Usage { message, repair },
            DomainErrorSituation::UnsatisfiedDegradedMode => {
                Self::UnsatisfiedDegradedMode { message, repair }
            }
            DomainErrorSituation::PolicyDenied => Self::PolicyDenied { message, repair },
            DomainErrorSituation::MigrationRequired => Self::MigrationRequired { message, repair },
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Usage { .. } | Self::UsageWithDetails { .. } => "usage",
            Self::UsageCodeWithDetails { code, .. } => code,
            Self::Configuration { .. } => "configuration",
            Self::Storage { .. } => "storage",
            Self::SearchIndex { .. } => "search_index",
            Self::Graph { .. } => "graph",
            Self::Import { .. } => "import",
            Self::NotFound { .. } => "not_found",
            Self::UnsatisfiedDegradedMode { .. } => "unsatisfied_degraded_mode",
            Self::UnsatisfiedDegradedModeCode { code, .. } => code,
            Self::PolicyDenied { .. } | Self::PolicyDeniedWithDetails { .. } => "policy_denied",
            Self::MigrationRequired { .. } => "migration_required",
            Self::MigrationDrift { .. } => "migration_drift",
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Usage { message, .. }
            | Self::UsageWithDetails { message, .. }
            | Self::UsageCodeWithDetails { message, .. }
            | Self::Configuration { message, .. }
            | Self::Storage { message, .. }
            | Self::SearchIndex { message, .. }
            | Self::Graph { message, .. }
            | Self::Import { message, .. }
            | Self::UnsatisfiedDegradedMode { message, .. }
            | Self::UnsatisfiedDegradedModeCode { message, .. }
            | Self::PolicyDenied { message, .. }
            | Self::PolicyDeniedWithDetails { message, .. }
            | Self::MigrationRequired { message, .. }
            | Self::MigrationDrift { message, .. } => message.clone(),
            Self::NotFound { resource, id, .. } => {
                format!("{resource} not found: {id}")
            }
        }
    }

    #[must_use]
    pub fn repair(&self) -> Option<&str> {
        match self {
            Self::Usage { repair, .. }
            | Self::UsageWithDetails { repair, .. }
            | Self::UsageCodeWithDetails { repair, .. }
            | Self::Configuration { repair, .. }
            | Self::Storage { repair, .. }
            | Self::SearchIndex { repair, .. }
            | Self::Graph { repair, .. }
            | Self::Import { repair, .. }
            | Self::NotFound { repair, .. }
            | Self::UnsatisfiedDegradedMode { repair, .. }
            | Self::UnsatisfiedDegradedModeCode { repair, .. }
            | Self::PolicyDenied { repair, .. }
            | Self::PolicyDeniedWithDetails { repair, .. }
            | Self::MigrationRequired { repair, .. }
            | Self::MigrationDrift { repair, .. } => repair.as_deref(),
        }
    }

    /// Derive structured recovery actions from this error.
    ///
    /// Bead bd-17c65.6.1 (F1). The default returns an empty vector;
    /// specific code/message combinations match heuristically to
    /// well-known recovery paths (cass binary, search index, migration).
    /// Agents iterate the result and pick actions by `priority`.
    ///
    /// This is intentionally heuristic — it does NOT require every error
    /// site to be plumbed with extra fields. Specific error sites that
    /// want richer recovery should add a `recovery_overrides` field in a
    /// follow-up; for now the canonical cases below cover the surfaces
    /// exercised in the 2026-05-10 walkthrough.
    #[must_use]
    pub fn recovery_actions(&self) -> Vec<RecoveryAction> {
        let message = self.message().to_lowercase();
        match self {
            // Cass binary not found in trusted locations.
            Self::Import { .. } if message.contains("cass binary not found") => vec![
                RecoveryAction::env(
                    1,
                    "EE_CASS_BINARY",
                    "<absolute path to executable cass binary>",
                    "Fastest fix when cass is installed under ~/.local/bin or another non-trusted location",
                ),
                RecoveryAction::config(
                    2,
                    ".ee/config.toml",
                    "cass.binary",
                    "<absolute path>",
                    "Persists across sessions; survives shell restart and CI",
                ),
                RecoveryAction::install(
                    3,
                    "brew install cass",
                    "/opt/homebrew/bin/cass (auto-discovered)",
                    "Permanent system-wide solution; preferred for developer workstations",
                ),
            ],
            // Search index missing / corrupt / stale.
            Self::SearchIndex { .. } if message.contains("index") => vec![
                RecoveryAction {
                    priority: 1,
                    kind: RecoveryKind::Migration,
                    rationale: "Rebuild the index from current memory state; idempotent."
                        .to_owned(),
                    env_name: None,
                    value_hint: None,
                    config_path: None,
                    config_key: None,
                    flag_name: None,
                    command: Some("ee index rebuild --workspace .".to_owned()),
                    results_in: None,
                    example: None,
                },
                RecoveryAction {
                    priority: 2,
                    kind: RecoveryKind::Migration,
                    rationale: "Inspect index state before rebuilding (faster diagnosis)."
                        .to_owned(),
                    env_name: None,
                    value_hint: None,
                    config_path: None,
                    config_key: None,
                    flag_name: None,
                    command: Some("ee index status --workspace . --json".to_owned()),
                    results_in: None,
                    example: None,
                },
            ],
            // Migration required.
            Self::MigrationRequired { .. } => vec![RecoveryAction::migration(
                1,
                "ee migrate run --workspace . --to v0.2",
                "Apply outstanding migrations; idempotent and audit-logged.",
            )],
            // Migration drift.
            Self::MigrationDrift { .. } => vec![RecoveryAction {
                priority: 1,
                kind: RecoveryKind::Migration,
                rationale: "Inspect drift details before deciding repair path.".to_owned(),
                env_name: None,
                value_hint: None,
                config_path: None,
                config_key: None,
                flag_name: None,
                command: Some("ee migrate status --workspace . --json".to_owned()),
                results_in: None,
                example: None,
            }],
            // Policy denied: secret-bearing content. Prefer redaction;
            // C2's explicit bypass is surfaced in detailed error metadata.
            Self::PolicyDenied { .. } | Self::PolicyDeniedWithDetails { .. }
                if message.contains("secret") =>
            {
                vec![
                RecoveryAction {
                    priority: 1,
                    kind: RecoveryKind::Broaden,
                    rationale: "Replace the value-bearing substring with a placeholder (e.g. <REDACTED>) before retrying.".to_owned(),
                    env_name: None,
                    value_hint: None,
                    config_path: None,
                    config_key: None,
                    flag_name: None,
                    command: None,
                    results_in: None,
                    example: None,
                },
            ]
            }
            // No workspace found (planned in D7; here we cover the
            // existing usage-error variant for symmetry).
            Self::Usage { .. }
                if message.contains("workspace") && message.contains("not found") =>
            {
                vec![
                    RecoveryAction::flag(
                        1,
                        "--workspace",
                        "<path>",
                        "Point at an explicit workspace; the simplest fix when running from outside an .ee/ directory.",
                    ),
                    RecoveryAction::env(
                        2,
                        "EE_WORKSPACE",
                        "<absolute path>",
                        "Persists for the current shell; useful for scripts that always operate on one workspace.",
                    ),
                    RecoveryAction {
                        priority: 3,
                        kind: RecoveryKind::Seed,
                        rationale: "Create a new workspace at cwd if one doesn't exist yet."
                            .to_owned(),
                        env_name: None,
                        value_hint: None,
                        config_path: None,
                        config_key: None,
                        flag_name: None,
                        command: Some("ee init --workspace .".to_owned()),
                        results_in: None,
                        example: None,
                    },
                ]
            }
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub const fn exit_code(&self) -> ProcessExitCode {
        match self {
            Self::Usage { .. }
            | Self::UsageWithDetails { .. }
            | Self::UsageCodeWithDetails { .. } => ProcessExitCode::Usage,
            Self::Configuration { .. } => ProcessExitCode::Configuration,
            Self::Storage { .. } => ProcessExitCode::Storage,
            Self::SearchIndex { .. } => ProcessExitCode::SearchIndex,
            Self::Graph { .. } => ProcessExitCode::SearchIndex,
            Self::Import { .. } => ProcessExitCode::Import,
            Self::NotFound { .. } => ProcessExitCode::Usage,
            Self::UnsatisfiedDegradedMode { .. } | Self::UnsatisfiedDegradedModeCode { .. } => {
                ProcessExitCode::UnsatisfiedDegradedMode
            }
            Self::PolicyDenied { .. } | Self::PolicyDeniedWithDetails { .. } => {
                ProcessExitCode::PolicyDenied
            }
            Self::MigrationRequired { .. } => ProcessExitCode::MigrationRequired,
            Self::MigrationDrift { .. } => ProcessExitCode::MigrationRequired,
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
    Import = 5,
    UnsatisfiedDegradedMode = 6,
    PolicyDenied = 7,
    MigrationRequired = 8,
    EvalFailure = 9,
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
        assert_eq!(ProcessExitCode::Configuration as u8, 2);
        assert_eq!(ProcessExitCode::Storage as u8, 3);
        assert_eq!(ProcessExitCode::SearchIndex as u8, 4);
        assert_eq!(ProcessExitCode::Import as u8, 5);
        assert_eq!(ProcessExitCode::UnsatisfiedDegradedMode as u8, 6);
        assert_eq!(ProcessExitCode::PolicyDenied as u8, 7);
        assert_eq!(ProcessExitCode::MigrationRequired as u8, 8);
        assert_eq!(ProcessExitCode::EvalFailure as u8, 9);
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
                DomainError::Graph {
                    message: String::new(),
                    repair: None,
                },
                "graph",
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
                DomainError::NotFound {
                    resource: String::new(),
                    id: String::new(),
                    repair: None,
                },
                "not_found",
                ProcessExitCode::Usage,
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
            // Bug: eidetic_engine_cli-wfgr - MigrationDrift must expose its own code
            (
                DomainError::MigrationDrift {
                    message: String::new(),
                    repair: None,
                },
                "migration_drift",
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

    // ========================================================================
    // Bead bd-17c65.6.1 (F1) — RecoveryAction construction + DomainError
    // recovery_actions() heuristic mapping
    // ========================================================================

    #[test]
    fn recovery_kind_as_str_is_stable() {
        // These string forms are the JSON wire enum — changing any of them
        // is a contract change consumers (agents, schemas) depend on.
        assert_eq!(super::RecoveryKind::Env.as_str(), "env");
        assert_eq!(super::RecoveryKind::Config.as_str(), "config");
        assert_eq!(super::RecoveryKind::Flag.as_str(), "flag");
        assert_eq!(super::RecoveryKind::Install.as_str(), "install");
        assert_eq!(super::RecoveryKind::Rebuild.as_str(), "rebuild");
        assert_eq!(super::RecoveryKind::Permission.as_str(), "permission");
        assert_eq!(super::RecoveryKind::Migration.as_str(), "migration");
        assert_eq!(super::RecoveryKind::Broaden.as_str(), "broaden");
        assert_eq!(super::RecoveryKind::Narrow.as_str(), "narrow");
        assert_eq!(super::RecoveryKind::Seed.as_str(), "seed");
        assert_eq!(super::RecoveryKind::None.as_str(), "none");
    }

    #[test]
    fn recovery_action_env_constructor_populates_only_relevant_fields() {
        let action = super::RecoveryAction::env(1, "EE_CASS_BINARY", "/abs/path", "Try this first");
        assert_eq!(action.priority, 1);
        assert_eq!(action.kind, super::RecoveryKind::Env);
        assert_eq!(action.env_name.as_deref(), Some("EE_CASS_BINARY"));
        assert_eq!(action.value_hint.as_deref(), Some("/abs/path"));
        assert_eq!(action.rationale, "Try this first");
        // Non-Env fields stay None
        assert!(action.config_path.is_none());
        assert!(action.flag_name.is_none());
        assert!(action.command.is_none());
    }

    #[test]
    fn recovery_action_config_constructor() {
        let action = super::RecoveryAction::config(
            2,
            ".ee/config.toml",
            "cass.binary",
            "<absolute path>",
            "Persists across sessions",
        );
        assert_eq!(action.kind, super::RecoveryKind::Config);
        assert_eq!(action.config_path.as_deref(), Some(".ee/config.toml"));
        assert_eq!(action.config_key.as_deref(), Some("cass.binary"));
    }

    #[test]
    fn recovery_action_install_constructor() {
        let action = super::RecoveryAction::install(
            3,
            "brew install cass",
            "/opt/homebrew/bin/cass",
            "System-wide solution",
        );
        assert_eq!(action.kind, super::RecoveryKind::Install);
        assert_eq!(action.command.as_deref(), Some("brew install cass"));
        assert_eq!(action.results_in.as_deref(), Some("/opt/homebrew/bin/cass"));
    }

    #[test]
    fn domain_error_recovery_for_cass_binary_emits_three_options() {
        let error = super::DomainError::Import {
            message: "cass binary not found at '/usr/local/bin/cass'".to_owned(),
            repair: Some("install cass".to_owned()),
        };
        let actions = error.recovery_actions();
        assert_eq!(actions.len(), 3, "expected 3 options, got {actions:?}");
        // Priority ascending: env (1), config (2), install (3)
        assert_eq!(actions[0].kind, super::RecoveryKind::Env);
        assert_eq!(actions[0].priority, 1);
        assert_eq!(actions[0].env_name.as_deref(), Some("EE_CASS_BINARY"));
        assert_eq!(actions[1].kind, super::RecoveryKind::Config);
        assert_eq!(actions[1].priority, 2);
        assert_eq!(actions[2].kind, super::RecoveryKind::Install);
        assert_eq!(actions[2].priority, 3);
    }

    #[test]
    fn domain_error_recovery_for_search_index_includes_rebuild() {
        let error = super::DomainError::SearchIndex {
            message: "Search index is stale or missing.".to_owned(),
            repair: Some("ee index rebuild".to_owned()),
        };
        let actions = error.recovery_actions();
        assert!(!actions.is_empty());
        assert!(actions.iter().any(|a| {
            a.command
                .as_deref()
                .is_some_and(|cmd| cmd.contains("ee index rebuild"))
        }));
    }

    #[test]
    fn domain_error_recovery_for_migration_required_emits_migrate_run() {
        let error = super::DomainError::MigrationRequired {
            message: "Workspace is v0.1; current binary expects v0.2.".to_owned(),
            repair: None,
        };
        let actions = error.recovery_actions();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, super::RecoveryKind::Migration);
        assert!(
            actions[0]
                .command
                .as_deref()
                .is_some_and(|cmd| cmd.contains("ee migrate run"))
        );
    }

    #[test]
    fn domain_error_recovery_unmapped_returns_empty() {
        let error = super::DomainError::Graph {
            message: "graph node not in projection".to_owned(),
            repair: None,
        };
        // We haven't mapped a recovery for unrelated graph errors.
        assert!(error.recovery_actions().is_empty());
    }

    #[test]
    fn domain_error_recovery_for_policy_secret_recommends_redact() {
        let error = super::DomainError::PolicyDenied {
            message: "Refusing to persist memory content that contains secrets: openai_sk_prefix."
                .to_owned(),
            repair: None,
        };
        let actions = error.recovery_actions();
        assert!(!actions.is_empty());
        assert_eq!(actions[0].kind, super::RecoveryKind::Broaden);
        assert!(actions[0].rationale.to_lowercase().contains("redact"));
    }
}
