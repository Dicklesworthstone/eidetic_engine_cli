use std::process::ExitCode;

pub mod attestation;
pub mod backup;
pub mod bead_affinity;
pub mod bead_affinity_loader;
pub mod causal;
pub mod certificate;
pub mod claims;
pub mod contention;
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
pub mod memory_anchor;
pub mod memory_seal;
pub mod memory_sentinel;
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
pub mod regression_causality;
pub mod release;
pub mod repro;
pub mod revision;
pub mod rule;
pub mod schema;
pub mod singleflight;
pub mod situation;
pub mod symbol;
pub mod task_lens;
pub mod timing;
pub mod trust;
pub mod verification;
pub mod why_tag;

pub use attestation::{
    ATTESTATION_BUNDLE_SCHEMA_V1, ATTESTATION_HASH_ALGORITHM, ATTESTATION_LOCAL_TRUTH_STATEMENT,
    AttestationBundle, AttestationEvidenceManifest, AttestationEvidenceRef, AttestationHashEntry,
    AttestationHashManifest, AttestationOmission, AttestationRedactionEntry,
    AttestationRedactionManifest, AttestationSubject, AttestationSubjectKind,
    AttestationTrustStatement, ParseAttestationSubjectKindError,
};
pub use backup::{
    BACKUP_CREATE_SCHEMA_V1, BACKUP_INSPECT_SCHEMA_V1, BACKUP_LIST_SCHEMA_V1,
    BACKUP_MANIFEST_SCHEMA_V1, BACKUP_MANIFEST_SCHEMA_V2, BACKUP_RESTORE_SCHEMA_V1,
    BACKUP_VERIFY_SCHEMA_V1,
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
    AGENT_CONTEXT_PROFILE_SCHEMA_V1, AGENT_PROFILE_BIAS_CAP, AGENT_PROFILE_COLD_START_CODE,
    AGENT_PROFILE_COLD_START_OUTCOMES, AgentContextProfileBias, AgentContextProfileCounts,
    AgentContextProfileDecayedCounts, CONTEXT_PROFILE_SCHEMA_CATALOG_V1, CONTEXT_PROFILE_SCHEMA_V1,
    ContextProfile, ContextProfileFieldSchema, ContextProfileName, ContextProfileObjectSchema,
    ContextProfileObjective, ContextProfileSection, ContextProfileSectionMix,
    ContextProfileValidationError, context_profile_schema_catalog_json, context_profile_schemas,
    decay_factor,
};
pub use decision::{
    DECISION_PLANE_SCHEMA_V1, DecisionPlane, DecisionPlaneMetadata, DecisionRecord,
    DecisionRecordBuilder, ParseDecisionPlaneError,
};
pub use degradation::{
    ALL_DEGRADATION_CODES, ActiveDegradation, DegradationCode, DegradationSeverity,
    DegradedSubsystem, ParseDegradationSeverityError,
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
    executable_id_schema_catalog_json, executable_id_schemas, public_attempt_family_alias,
    public_audit_id, public_memory_id, public_memory_link_id, public_pack_id, public_workspace_id,
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
    ExportAgentRecordBuilder, ExportArtifactRecord, ExportArtifactRecordBuilder,
    ExportAttemptFamilyRecord, ExportAuditRecord, ExportAuditRecordBuilder, ExportFooter,
    ExportFooterBuilder, ExportHeader, ExportHeaderBuilder, ExportLinkRecord,
    ExportLinkRecordBuilder, ExportMemoryRecord, ExportMemoryRecordBuilder, ExportRecord,
    ExportRecordType, ExportScope, ExportTagRecord, ExportWorkspaceRecord,
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
    Confidence, Importance, KNOWN_MEMORY_KINDS, MAX_CONTENT_BYTES, MAX_TAG_BYTES,
    MAX_TYPED_MEMORY_FIELD_LIST_ITEMS, MAX_TYPED_MEMORY_FIELD_VALUE_BYTES, MAX_TYPED_MEMORY_FIELDS,
    MAX_TYPED_MEMORY_FIELDS_JSON_BYTES, MemoryContent, MemoryKind, MemoryLevel,
    MemoryValidationError, TYPED_MEMORY_FIELDS_SCHEMA_V1, Tag, UnitScore, Utility,
    canonicalize_typed_memory_fields_json, canonicalize_typed_memory_fields_json_with_redactor,
};
pub use memory_anchor::{
    CreateMemoryAnchorInput, ExtractedAnchorSurface, MEMORY_ANCHOR_SCHEMA_V1,
    MemoryAnchorFreshnessState, MemoryAnchorKind, MemoryAnchorSource, StoredMemoryAnchor,
    extract_memory_anchor_surfaces, extract_precision_memory_anchors, memory_anchor_value_hash,
};
pub use memory_seal::{
    MEMORY_SEAL_COMMITMENT_SCHEMA_V1, MEMORY_SEAL_PLACEHOLDER_CONTENT, MEMORY_SEAL_SCHEMA_V1,
    MemorySeal, MemorySealValidationError, memory_seal_commitment, seal_commitment_for_content,
    validate_memory_seal_commitment,
};
pub use memory_sentinel::{
    CreateMemorySentinelSpecInput, MAX_MEMORY_SENTINEL_EVIDENCE_BYTES,
    MAX_MEMORY_SENTINEL_PREDICATE_BYTES, MAX_MEMORY_SENTINEL_PROVENANCE_BYTES,
    MAX_MEMORY_SENTINEL_TARGET_BYTES, MEMORY_SENTINEL_RESULT_HASH_SCHEMA_V1,
    MEMORY_SENTINEL_RESULT_SCHEMA_V1, MEMORY_SENTINEL_SPEC_HASH_SCHEMA_V1,
    MEMORY_SENTINEL_SPEC_SCHEMA_V1, MemorySentinelKind, MemorySentinelPolarity,
    MemorySentinelResult, MemorySentinelResultInput, MemorySentinelResultStatus,
    MemorySentinelSafetyClass, MemorySentinelSpec, MemorySentinelValidationError,
    ParsedMemorySentinelSpec, SentinelObservation, StoredMemorySentinelResult,
    StoredMemorySentinelSpec, memory_sentinel_spec_repair_hint, parse_memory_sentinel_spec,
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
    FilterOperator, FilterPredicate, FilterValue, GLOBAL_MEMORY_SCOPE_TAG,
    HOUSE_RULE_MEMORY_SCOPE_TAG, MemoryScope, MemoryScopeStats, PaginationCursor,
    PaginationCursorError, QueryFilter, QueryFilters, QueryGraphHints, QueryGraphTraversal,
    QueryPagination, QueryTemporalFilters, QueryTemporalValidity, QueryTemporalValidityPosture,
    RedactionFilters, TagFilters, TrustFilters, compute_query_shape_hash,
    memory_tags_include_global_scope, parse_filters, parse_pagination, parse_redaction, parse_tags,
    parse_trust, posture_for_trust_class,
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
pub use regression_causality::{
    NormalizedRegressionEvidenceRow, REGRESSION_CAUSALITY_SCHEMA_V1,
    REGRESSION_EVIDENCE_NORMALIZATION_SCHEMA_V1, RegressionCapsuleEvidenceSource,
    RegressionCausalitySeverity, RegressionEvidenceInput, RegressionEvidenceKind,
    RegressionEvidenceNormalizationReport, RegressionEvidenceProvenance, RegressionEvidenceStatus,
    RegressionNormalizationDegradation, RegressionRedactionStatus, RegressionSourceMaterialization,
    normalize_regression_evidence_inputs,
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
    CorpusRevision, IdempotencyKey, IdempotencyKeyError, LEGAL_HOLD_ID_LEN, LEGAL_HOLD_PREFIX,
    LegalHold, LegalHoldId, REVISION_GROUP_ID_LEN, REVISION_GROUP_PREFIX, RevisionGroupId,
    RevisionIdError, RevisionMeta, SupersessionLink, SupersessionReason,
};
pub use rule::{
    ParseRuleLifecycleActionError, ParseRuleLifecycleTriggerError, ParseRuleMaturityError,
    ParseRuleScopeError, RuleLifecycleAction, RuleLifecycleEvidence, RuleLifecycleTransition,
    RuleLifecycleTrigger, RuleMaturity, RuleScope,
};
pub use schema::{
    AMBIENT_CONTEXT_SCHEMA_V1, CACHE_HOTSET_COLLECT_SCHEMA_V1, COVERAGE_GAP_SCHEMA_V1,
    EMBEDDING_POSTURE_MODE_DETERMINISTIC_HASH, EMBEDDING_POSTURE_MODE_NEURAL_LOCAL,
    EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING, EMBEDDING_POSTURE_MODE_NEURAL_REMOTE_BLOCKED,
    EMBEDDING_POSTURE_SCHEMA_V1, GLOBAL_MEMORY_SCHEMA_V1, HOTSET_MANIFEST_SCHEMA_V1,
    INDEX_INTAKE_FALLBACK_CORPUS_REVISION_MISMATCH, INDEX_INTAKE_FALLBACK_DELTA_OVER_THRESHOLD,
    INDEX_INTAKE_FALLBACK_FORCED_REINDEX, INDEX_INTAKE_FALLBACK_GENERATION_SKEW,
    INDEX_INTAKE_FALLBACK_INDEX_ABSENT, INDEX_INTAKE_FALLBACK_TIER_UNAVAILABLE,
    INDEX_INTAKE_MODE_FULL_REBUILD, INDEX_INTAKE_MODE_INCREMENTAL, INDEX_INTAKE_MODE_SEGMENT_MERGE,
    INDEX_INTAKE_SCHEMA_V1, KNOWN_SCHEMAS, PROVENANCE_HEALTH_SCHEMA_V1, SCALE_ENVELOPE_SCHEMA_V1,
    SCALE_FIXTURE_UNAVAILABLE_CODE, SCALE_POSTURE_THRASHING_CODE, SCALE_POSTURE_WARMING_CODE,
    SCALE_PROBE_BUDGET_EXCEEDED_CODE, SESSION_BUDGET_SCHEMA_V1, SchemaValidationError,
    TRUST_REPORT_SCHEMA_V1, WRITE_GROUP_COMMIT_FALLBACK_DEGRADED,
    WRITE_GROUP_COMMIT_FALLBACK_DISABLED, WRITE_GROUP_COMMIT_FALLBACK_OVERSIZED,
    WRITE_GROUP_COMMIT_FALLBACK_SINGLE_WRITER, WRITE_GROUP_COMMIT_SCHEMA_V1, is_known_schema,
    parse_schema_parts, validate_schema, validate_schema_match,
};
pub use singleflight::{
    SINGLEFLIGHT_KEY_CANONICAL_VERSION, SINGLEFLIGHT_KEY_SCHEMA_V1, SINGLEFLIGHT_POSTURE_SCHEMA_V1,
    SingleFlightKey, SingleFlightKeyInput, SingleFlightLastKeyPosture, SingleFlightPostureReport,
    SingleFlightSurface, SingleFlightSurfaceCounters, SingleFlightSurfacePosture, query_shape_hash,
    sample_singleflight_keys,
};
pub use situation::{
    FEATURE_EVIDENCE_SCHEMA_V1, FeatureEvidence, ParseSituationValueError,
    ROUTING_DECISION_SCHEMA_V1, RoutingDecision, SITUATION_ADOPT_SCHEMA_V1,
    SITUATION_CLASSIFY_SCHEMA_V1, SITUATION_EXPLAIN_SCHEMA_V1, SITUATION_LINK_SCHEMA_V1,
    SITUATION_SCHEMA_CATALOG_V1, SITUATION_SCHEMA_V1, SITUATION_SHOW_SCHEMA_V1, Situation,
    SituationCategory, SituationConfidence, SituationFeatureType, SituationFieldSchema,
    SituationLink, SituationLinkRelation, SituationObjectSchema, SituationReplayPolicy,
    SituationRoutingSurface, TASK_SIGNATURE_SCHEMA_V1, TaskSignature,
    situation_schema_catalog_json, situation_schemas,
};
pub use symbol::{
    SYMBOL_EVIDENCE_LINK_ID_PREFIX, SYMBOL_EVIDENCE_LINKS_SCHEMA_V1, SYMBOL_ID_PREFIX,
    SYMBOL_SNAPSHOT_SCHEMA_V1, SymbolEvidenceLink, SymbolEvidenceLinkDegradation,
    SymbolEvidenceLinkDegradationCode, SymbolEvidenceLinkSet, SymbolEvidenceReasonCode,
    SymbolEvidenceResolution, SymbolEvidenceSourceKind, SymbolGraphDegradation,
    SymbolGraphDegradationCode, SymbolGraphDegradationSeverity, SymbolKind, SymbolParserKind,
    SymbolRecord, SymbolSnapshot, SymbolSourceFile, SymbolSourceLanguage, SymbolSourceRange,
    SymbolVisibility,
};
pub use task_lens::{
    BUILTIN_TASK_LENS_IDS, MAX_TASK_LENS_CANDIDATE_POOL, MAX_TASK_LENS_DESCRIPTION_BYTES,
    MAX_TASK_LENS_FACETS, MAX_TASK_LENS_ID_BYTES, MAX_TASK_LENS_KINDS, MAX_TASK_LENS_RESULTS,
    MAX_TASK_LENS_TOKENS, MAX_WORKSPACE_TASK_LENSES, TASK_LENS_SCHEMA_V1, TASK_LENS_VERSION,
    TaskLens, TaskLensCatalog, TaskLensInput, TaskLensOverlay, TaskLensValidationError,
    builtin_task_lens, builtin_task_lenses,
};
pub use timing::{DiagnosticTiming, TimingCapture, TimingPhase};
pub use trust::{
    AttemptFamilyMultiplicity, AttemptFamilyPromotionPosture, ParseTrustClassError, TrustClass,
};
pub use verification::{
    CompileBlockerCacheEntry, CompileBlockerCacheInput, CompileBlockerCacheStatus,
    CompileBlockerLookup, CompileBlockerLookupRequest, GITHUB_ACTIONS_CHECK_RUN_SCHEMA_V1,
    GithubActionsVerificationEvidenceParseError, PROOF_BROKER_SCHEMA_V1,
    ProofBrokerAdmissionDecision, ProofBrokerAdmissionVerdict, ProofBrokerEvidenceRef,
    ProofBrokerFingerprint, ProofBrokerFingerprintInput, ProofBrokerLedgerRecord,
    ProofBrokerLedgerRecordInput, ProofBrokerLedgerState, ProofBrokerOwnerRef,
    RCH_SELECTOR_ADMISSION_PROBE_SCHEMA_V1, RCH_VERIFY_SCHEMA_V1,
    RchVerificationEvidenceParseError, VERIFICATION_BROKER_VIEW_SCHEMA_V1,
    VERIFICATION_CLOSEOUT_CAPSULE_SCHEMA_V1, VERIFICATION_CLOSURE_GUIDANCE_SCHEMA_V1,
    VERIFICATION_COMPILE_BLOCKER_CACHE_SCHEMA_V1, VERIFICATION_COMPILE_BLOCKER_LOOKUP_SCHEMA_V1,
    VERIFICATION_EVIDENCE_SCHEMA_V1, VERIFICATION_REUSE_ADVISORY_SCHEMA_V1,
    VERIFICATION_RUN_SCHEMA_V1, VerificationArtifactRef, VerificationBrokerRchMetadata,
    VerificationBrokerStatus, VerificationBrokerView, VerificationBrokerViewRequest,
    VerificationCloseoutCapsule, VerificationCloseoutCapsuleRequest,
    VerificationCloseoutSupportBundleMetadata, VerificationClosureGuidance,
    VerificationEnvironment, VerificationEvidenceInput, VerificationEvidenceRecord,
    VerificationFirstFailureSummaryRef, VerificationGateAssessment, VerificationGateRequirement,
    VerificationOffload, VerificationOutputSummary, VerificationReuseAdvisory,
    VerificationReuseRepairAction, VerificationReuseRequest, VerificationReuseStatus,
    VerificationRunImportError, VerificationRunInput, VerificationRunProvenance,
    VerificationRunRecord, VerificationSelectorAdmission, VerificationStatus, command_hash,
    compile_blocker_cache_entry, compile_blocker_lookup, proof_broker_fingerprint,
    proof_broker_ledger_record, rch_cargo_closure_requirements, sample_proof_broker_ledger_records,
    sample_verification_broker_views, sample_verification_closeout_capsules,
    sample_verification_evidence_records, sample_verification_reuse_advisories,
    sample_verification_run_records, verification_broker_view, verification_closeout_capsule,
    verification_closure_guidance, verification_evidence_beads_summary,
    verification_evidence_record_from_github_actions_check_run,
    verification_evidence_record_from_rch_verify, verification_evidence_record_from_run_record,
    verification_reuse_advisory, verification_run_has_verified_remote_artifact,
    verification_run_records_from_j1_jsonl,
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

/// Response envelope schema (v2) for successful command output.
pub const RESPONSE_SCHEMA_V2: &str = "ee.response.v2";

/// Current error envelope schema for failed command output.
pub const ERROR_SCHEMA_V2: &str = "ee.error.v2";

/// Context pack response schema (v2) for context pack output.
pub const PACK_SCHEMA_V2: &str = "ee.pack.v2";

/// Schema for performance/bench metrics output.
pub const PERF_SCHEMA_V1: &str = "ee.perf.v1";

/// Schema for beads retry diagnostic wrapper.
pub const BEADS_RETRY_SCHEMA_V1: &str = "ee.beads_retry.v1";

/// Schema for doctor fix summary output.
pub const DOCTOR_FIX_SUMMARY_SCHEMA_V1: &str = "ee.doctor.fix_summary.v1";

/// Schema for doctor run diff output.
pub const DOCTOR_RUN_DIFF_SCHEMA_V1: &str = "ee.doctor.run_diff.v1";

/// Schema for failure mode fixtures used in tests and evaluations.
pub const FAILURE_MODE_FIXTURE_SCHEMA_V1: &str = "ee.failure_mode_fixture.v1";

/// External derivation source package is missing, malformed, or not canonical.
pub const DERIVED_SOURCES_INVALID_CODE: &str = "derived_sources_invalid";
/// External derivation source content changed after proposal.
pub const DERIVED_SOURCE_HASH_DRIFTED_CODE: &str = "derived_source_hash_drifted";
/// External derivation source content mismatched at apply-time revalidation.
pub const DERIVED_SOURCE_HASH_MISMATCH_CODE: &str = "derived_source_hash_mismatch";
/// External derivation source belongs to a different workspace.
pub const DERIVED_SOURCE_WORKSPACE_MISMATCH_CODE: &str = "derived_source_workspace_mismatch";
/// External derivation source memory was tombstoned before apply.
pub const DERIVED_SOURCE_MEMORY_TOMBSTONED_CODE: &str = "derived_source_memory_tombstoned";
/// External derivation source memory disappeared before apply.
pub const DERIVED_SOURCE_MEMORY_MISSING_CODE: &str = "derived_source_memory_missing";
/// External derivation evidence span is already attached to another memory.
pub const DERIVED_EVIDENCE_ALREADY_LINKED_CODE: &str = "derived_evidence_already_linked";
/// External derivation evidence span is already attached to another memory.
pub const DERIVED_SOURCE_EVIDENCE_ALREADY_LINKED_CODE: &str =
    "derived_source_evidence_already_linked";
/// External derivation evidence span disappeared before apply.
pub const DERIVED_SOURCE_EVIDENCE_MISSING_CODE: &str = "derived_source_evidence_missing";
/// Mutating derived apply needs an explicit target when the candidate type mutates one.
pub const DERIVED_TARGET_REQUIRED_FOR_MUTATION_CODE: &str = "derived_target_required_for_mutation";
/// Create-derived candidates must not target an existing memory.
pub const DERIVED_TARGET_FORBIDDEN_FOR_CREATE_CODE: &str = "derived_target_forbidden_for_create";
/// External derivation memory spec is missing or malformed.
pub const DERIVED_INVALID_MEMORY_SPEC_CODE: &str = "derived_invalid_memory_spec";
/// Applied create-derived candidate is missing the audit row needed for replay.
pub const CREATE_DERIVED_REPLAY_MISSING_AUDIT_CODE: &str = "create_derived_replay_missing_audit";
/// Applied create-derived candidate has multiple matching audit rows for replay.
pub const CREATE_DERIVED_REPLAY_AMBIGUOUS_AUDIT_CODE: &str =
    "create_derived_replay_ambiguous_audit";
/// Reflection request expired before result ingestion.
pub const REFLECT_REQUEST_EXPIRED_CODE: &str = "reflect_request_expired";
/// Reflection challenge binding is invalid or cannot verify.
pub const REFLECT_CHALLENGE_INVALID_CODE: &str = "reflect_challenge_invalid";
/// Reflection request has already been consumed by another result.
pub const REFLECT_REQUEST_CONSUMED_CODE: &str = "reflect_request_consumed";
/// Reflection source package drifted after request creation.
pub const REFLECT_SOURCE_DRIFTED_CODE: &str = "reflect_source_drifted";
/// Reflection result cites a source absent from the request package.
pub const REFLECT_UNKNOWN_CITED_SOURCE_CODE: &str = "reflect_unknown_cited_source";
/// Reflection result JSON does not satisfy the expected schema.
pub const REFLECT_RESULT_SCHEMA_INVALID_CODE: &str = "reflect_result_schema_invalid";
/// Reflection result attempted to return raw chain-of-thought.
pub const REFLECT_RAW_COT_REJECTED_CODE: &str = "reflect_raw_cot_rejected";
/// Reflection HMAC key material is unavailable.
pub const REFLECT_KEY_UNAVAILABLE_CODE: &str = "reflect_key_unavailable";

/// Schema for pack DNA context output.
pub const PACK_DNA_SCHEMA_V1: &str = "ee.context.pack_dna.v1";

/// Schema for Gomory-Hu proximity output.
pub const PROXIMITY_SCHEMA_V1: &str = "ee.proximity.v1";

/// Schema for proof check tool output.
pub const PROOF_CHECK_SCHEMA_V1: &str = "ee.proof_check.v1";

/// Schema for evaluation pack quality reports.
pub const PACK_QUALITY_REPORT_SCHEMA_V1: &str = "ee.eval.pack_quality_report.v1";

/// Schema for context pack streaming.
pub const PACK_STREAM_SCHEMA_V1: &str = "ee.pack.stream.v1";

/// Schema for test events and assertions.
pub const TEST_EVENT_SCHEMA_V1: &str = "ee.test_event.v1";

/// Schema for `ee model status` output.
pub const MODEL_STATUS_SCHEMA_V2: &str = "ee.model.status.v2";

/// Schema for `ee model list` output.
pub const MODEL_LIST_SCHEMA_V1: &str = "ee.model.list.v1";

/// Schema for query request documents (`--query-file`).
pub const QUERY_SCHEMA_V1: &str = "ee.query.v1";

/// Schema for CASS import reports (`ee import cass`).
pub const IMPORT_CASS_SCHEMA_V1: &str = "ee.import.cass.v1";

/// Schema for read-only legacy Eidetic import scans.
pub const IMPORT_EIDETIC_LEGACY_SCAN_SCHEMA_V1: &str = "ee.import.eidetic_legacy.scan.v1";

/// Schema for JSONL import reports (`ee import jsonl`).
pub const IMPORT_JSONL_SCHEMA_V1: &str = "ee.import.jsonl.v1";

/// Schema for review session reports (`ee review session --propose`).
///
/// V2 replaces raw upstream CASS identifiers with canonical opaque
/// provenance URIs.
pub const REVIEW_SESSION_SCHEMA_V2: &str = "ee.review.session.v2";

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

/// Schema for workspace-scoped mesh peer-group binding documents.
pub const MESH_PEER_GROUP_BINDING_SCHEMA_V1: &str = "ee.mesh.peer_group_binding.v1";

/// Schema for local mesh peer authorization and redaction policies.
pub const MESH_PEER_POLICY_SCHEMA_V1: &str = "ee.mesh.peer_policy.v1";

/// Schema for redaction-safe mesh policy decisions.
pub const MESH_POLICY_DECISION_SCHEMA_V1: &str = "ee.mesh.policy_decision.v1";

/// Schema for redaction-safe mesh policy failure surfaces.
pub const MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1: &str = "ee.mesh.policy_failure_surface.v1";

/// Schema for redaction-safe mesh storage status posture.
pub const MESH_STORAGE_STATUS_SCHEMA_V1: &str = "ee.mesh.storage_status.v1";

/// Schema for append-only optional mesh memory events.
pub const MESH_EVENT_SCHEMA_V1: &str = "ee.mesh.event.v1";

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
    ImportWithDetails {
        message: String,
        repair: Option<String>,
        details_json: String,
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
            Self::Import { message, .. } | Self::ImportWithDetails { message, .. } => {
                write!(f, "import error: {message}")
            }
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
    /// Run a command.
    Command,
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
            Self::Command => "command",
            Self::Broaden => "broaden",
            Self::Narrow => "narrow",
            Self::Seed => "seed",
            Self::None => "none",
        }
    }
}

/// Risk class for an agent-facing repair or recovery action.
///
/// These strings are part of the JSON contract used by agents to decide
/// whether a remediation hint is runnable, needs preflight, needs human
/// approval, or is only a manual handoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepairActionRiskClass {
    /// Pure inspection; does not mutate local state, trackers, mail, git, or workers.
    ReadOnlyProbe,
    /// Rebuilds or refreshes derived/idempotent local state.
    IdempotentRefresh,
    /// Mutates durable local state but not external coordination systems.
    MutatingLocalRepair,
    /// Mutates coordination/tracker systems such as Beads, Agent Mail, or RCH daemon state.
    MutatingExternalCoordinationRepair,
    /// Requires explicit human approval before an agent should run it.
    ApprovalRequiredRepair,
    /// Destructive, irreversible, or history-rewriting action.
    DestructiveOrIrreversibleRepair,
    /// No safe agent-runnable command exists; use a manual/operator path.
    UnavailableOrManualOnly,
}

impl RepairActionRiskClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnlyProbe => "read_only_probe",
            Self::IdempotentRefresh => "idempotent_refresh",
            Self::MutatingLocalRepair => "mutating_local_repair",
            Self::MutatingExternalCoordinationRepair => "mutating_external_coordination_repair",
            Self::ApprovalRequiredRepair => "approval_required_repair",
            Self::DestructiveOrIrreversibleRepair => "destructive_or_irreversible_repair",
            Self::UnavailableOrManualOnly => "unavailable_or_manual_only",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairActionSafety {
    pub risk_class: RepairActionRiskClass,
    pub preflight_command: Option<String>,
    pub requires_human_approval: bool,
    pub mutates_external_state: bool,
    pub mutates_tracker_state: bool,
    pub privacy_class: &'static str,
    pub manual_step: Option<&'static str>,
    pub evidence: Vec<&'static str>,
    pub preconditions: Vec<&'static str>,
}

fn quote_for_preflight_command(command: &str) -> String {
    format!("'{}'", command.replace('\'', "'\\''"))
}

fn preflight_command_for(command: &str) -> String {
    format!(
        "ee preflight check --cmd {} --workspace . --json",
        quote_for_preflight_command(command)
    )
}

fn command_matches_any(command: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| command.contains(needle))
}

fn command_has_flag(command: &str, flag: &str) -> bool {
    command.split_whitespace().any(|part| part == flag)
}

#[must_use]
pub fn repair_action_safety(kind: RecoveryKind, command: Option<&str>) -> RepairActionSafety {
    let Some(command) = command else {
        return match kind {
            RecoveryKind::Config => RepairActionSafety {
                risk_class: RepairActionRiskClass::MutatingLocalRepair,
                preflight_command: None,
                requires_human_approval: false,
                mutates_external_state: false,
                mutates_tracker_state: false,
                privacy_class: "path_and_key_only",
                manual_step: Some("Review the config edit before applying it."),
                evidence: vec!["recovery_kind_config"],
                preconditions: vec!["config_path_must_be_explicit"],
            },
            RecoveryKind::None => RepairActionSafety {
                risk_class: RepairActionRiskClass::UnavailableOrManualOnly,
                preflight_command: None,
                requires_human_approval: true,
                mutates_external_state: false,
                mutates_tracker_state: false,
                privacy_class: "no_command",
                manual_step: Some("No agent-runnable repair command is available."),
                evidence: vec!["recovery_kind_none"],
                preconditions: vec!["operator_decision_required"],
            },
            _ => RepairActionSafety {
                risk_class: RepairActionRiskClass::ReadOnlyProbe,
                preflight_command: None,
                requires_human_approval: false,
                mutates_external_state: false,
                mutates_tracker_state: false,
                privacy_class: "metadata_only",
                manual_step: None,
                evidence: vec!["recovery_kind_without_command"],
                preconditions: Vec::new(),
            },
        };
    };

    let command_lower = command.to_ascii_lowercase();
    let mut safety = if command_matches_any(
        &command_lower,
        &[
            "rm -rf",
            "git clean",
            "git reset --hard",
            "git checkout ",
            "git rebase",
            "mkfs",
            "diskutil erase",
        ],
    ) {
        RepairActionSafety {
            risk_class: RepairActionRiskClass::DestructiveOrIrreversibleRepair,
            preflight_command: Some(preflight_command_for(command)),
            requires_human_approval: true,
            mutates_external_state: false,
            mutates_tracker_state: false,
            privacy_class: "command_only",
            manual_step: Some("Stop and obtain explicit human approval for the exact command."),
            evidence: vec!["destructive_command_pattern"],
            preconditions: vec!["explicit_human_approval_required"],
        }
    } else if command_lower.contains("am doctor repair")
        || command_lower.contains("mcp_agent_mail.cli doctor repair")
        || command_lower.starts_with("rch daemon restart")
        || command_lower.starts_with("rch workers probe")
        || command_lower.starts_with("rch workers capabilities --refresh")
    {
        RepairActionSafety {
            risk_class: RepairActionRiskClass::MutatingExternalCoordinationRepair,
            preflight_command: Some(preflight_command_for(command)),
            requires_human_approval: command_lower.contains("doctor repair"),
            mutates_external_state: true,
            mutates_tracker_state: false,
            privacy_class: "bounded_command_no_raw_state",
            manual_step: Some("Coordinate before mutating shared coordination or worker state."),
            evidence: vec!["external_coordination_repair_command"],
            preconditions: vec!["shared_state_coordination_required"],
        }
    } else if command_lower.starts_with("br sync --status") {
        RepairActionSafety {
            risk_class: RepairActionRiskClass::ReadOnlyProbe,
            preflight_command: None,
            requires_human_approval: false,
            mutates_external_state: false,
            mutates_tracker_state: false,
            privacy_class: "tracker_metadata_only",
            manual_step: None,
            evidence: vec!["beads_status_probe_command"],
            preconditions: Vec::new(),
        }
    } else if command_lower.starts_with("br sync") {
        RepairActionSafety {
            risk_class: RepairActionRiskClass::MutatingExternalCoordinationRepair,
            preflight_command: Some(preflight_command_for(command)),
            requires_human_approval: false,
            mutates_external_state: true,
            mutates_tracker_state: true,
            privacy_class: "tracker_metadata_only",
            manual_step: Some("Coordinate before mutating shared tracker state."),
            evidence: vec!["beads_mutation_command"],
            preconditions: vec!["shared_state_coordination_required"],
        }
    } else if command_lower.starts_with("br update")
        || command_lower.starts_with("br close")
        || command_lower.starts_with("br reopen")
        || command_lower.starts_with("br comments add")
    {
        RepairActionSafety {
            risk_class: RepairActionRiskClass::MutatingExternalCoordinationRepair,
            preflight_command: Some(preflight_command_for(command)),
            requires_human_approval: false,
            mutates_external_state: true,
            mutates_tracker_state: true,
            privacy_class: "tracker_metadata_only",
            manual_step: Some("Announce tracker mutation through the bead thread."),
            evidence: vec!["beads_mutation_command"],
            preconditions: vec!["bead_id_must_be_explicit"],
        }
    } else if command_lower.starts_with("ee index status")
        || command_lower.starts_with("ee doctor --json")
        || command_lower.starts_with("ee doctor --robot")
        || command_lower.starts_with("ee doctor --capabilities")
        || command_lower.starts_with("ee doctor --list-runs")
        || command_lower.starts_with("ee doctor --gc-plan")
        || command_lower.starts_with("ee migrate status")
        || command_lower.starts_with("ee memory list")
        || command_lower.starts_with("ee memory show")
        || command_lower.starts_with("ee status")
        || command_lower.starts_with("ee why")
        || command_lower.starts_with("ee schema show")
        || command_lower.starts_with("ee curate candidates")
        || command_lower.starts_with("ee curate show")
        || command_lower.starts_with("ee curate validate")
        || command_lower.starts_with("ee reflect request-ledger diagnostics")
        || command_lower.starts_with("ee preflight check")
        || command_lower.starts_with("git status")
        || command_lower.starts_with("git diff")
        || command_lower.starts_with("rch status")
        || command_lower.starts_with("rch check")
        || command_lower.starts_with("rch queue")
        || (command_lower.starts_with("ee support bundle")
            && command_has_flag(&command_lower, "--dry-run"))
        || command_lower.starts_with("cargo fmt --check")
        || command_lower.starts_with("cargo metadata")
        || command_lower.starts_with("cargo tree")
        || command_lower.contains("doctor check")
    {
        RepairActionSafety {
            risk_class: RepairActionRiskClass::ReadOnlyProbe,
            preflight_command: None,
            requires_human_approval: false,
            mutates_external_state: false,
            mutates_tracker_state: false,
            privacy_class: "bounded_command_no_raw_state",
            manual_step: None,
            evidence: vec!["read_only_probe_command"],
            preconditions: Vec::new(),
        }
    } else if command_lower.starts_with("ee index rebuild")
        || command_lower.starts_with("ee index reembed")
        || command_lower.starts_with("ee graph centrality-refresh")
    {
        RepairActionSafety {
            risk_class: RepairActionRiskClass::IdempotentRefresh,
            preflight_command: None,
            requires_human_approval: false,
            mutates_external_state: false,
            mutates_tracker_state: false,
            privacy_class: "bounded_command_no_raw_state",
            manual_step: None,
            evidence: vec!["derived_asset_refresh_command"],
            preconditions: vec!["workspace_must_be_explicit"],
        }
    } else if command_lower.starts_with("ee migrate run")
        || command_lower.starts_with("ee init")
        || command_lower.starts_with("ee doctor --fix")
        || command_lower.starts_with("ee curate propose-derived")
        || command_lower.starts_with("ee curate apply")
        || command_lower.starts_with("ee reflect propose")
        || command_lower.starts_with("ee support bundle")
    {
        RepairActionSafety {
            risk_class: RepairActionRiskClass::MutatingLocalRepair,
            preflight_command: Some(preflight_command_for(command)),
            requires_human_approval: false,
            mutates_external_state: false,
            mutates_tracker_state: false,
            privacy_class: "bounded_command_no_raw_state",
            manual_step: Some("Review the dry-run or recovery details before applying."),
            evidence: vec!["local_state_repair_command"],
            preconditions: vec!["workspace_must_be_explicit"],
        }
    } else if command_lower.starts_with("brew install")
        || command_lower.starts_with("cargo install")
        || command_lower.starts_with("cargo build")
    {
        RepairActionSafety {
            risk_class: RepairActionRiskClass::MutatingLocalRepair,
            preflight_command: None,
            requires_human_approval: false,
            mutates_external_state: false,
            mutates_tracker_state: false,
            privacy_class: "bounded_command_no_raw_state",
            manual_step: None,
            evidence: vec!["local_toolchain_or_install_command"],
            preconditions: vec!["use_rch_for_cargo_when_applicable"],
        }
    } else {
        RepairActionSafety {
            risk_class: RepairActionRiskClass::ApprovalRequiredRepair,
            preflight_command: Some(preflight_command_for(command)),
            requires_human_approval: true,
            mutates_external_state: false,
            mutates_tracker_state: false,
            privacy_class: "bounded_command_no_raw_state",
            manual_step: Some("Classify this repair before running it."),
            evidence: vec!["unknown_repair_command"],
            preconditions: vec!["human_or_policy_review_required"],
        }
    };

    if matches!(kind, RecoveryKind::Install | RecoveryKind::Rebuild) {
        safety
            .preconditions
            .push("toolchain_or_feature_profile_must_match_request");
    }
    safety
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
    #[must_use]
    pub fn safety(&self) -> RepairActionSafety {
        repair_action_safety(self.kind, self.command.as_deref())
    }

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

    /// Render this action in the canonical error/degradation recovery shape.
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let mut action = serde_json::Map::new();
        let safety = self.safety();
        action.insert("priority".to_owned(), serde_json::json!(self.priority));
        action.insert("kind".to_owned(), serde_json::json!(self.kind.as_str()));
        action.insert(
            "rationale".to_owned(),
            serde_json::json!(self.rationale.as_str()),
        );
        action.insert(
            "riskClass".to_owned(),
            serde_json::json!(safety.risk_class.as_str()),
        );
        if let Some(preflight_command) = &safety.preflight_command {
            action.insert(
                "preflightCommand".to_owned(),
                serde_json::json!(preflight_command),
            );
        }
        action.insert(
            "requiresHumanApproval".to_owned(),
            serde_json::json!(safety.requires_human_approval),
        );
        action.insert(
            "mutatesExternalState".to_owned(),
            serde_json::json!(safety.mutates_external_state),
        );
        action.insert(
            "mutatesTrackerState".to_owned(),
            serde_json::json!(safety.mutates_tracker_state),
        );
        action.insert(
            "privacyClass".to_owned(),
            serde_json::json!(safety.privacy_class),
        );
        if let Some(manual_step) = safety.manual_step {
            action.insert("manualStep".to_owned(), serde_json::json!(manual_step));
        }
        if !safety.evidence.is_empty() {
            action.insert("evidence".to_owned(), serde_json::json!(safety.evidence));
        }
        if !safety.preconditions.is_empty() {
            action.insert(
                "preconditions".to_owned(),
                serde_json::json!(safety.preconditions),
            );
        }
        if let Some(name) = &self.env_name {
            action.insert("envName".to_owned(), serde_json::json!(name));
        }
        if let Some(hint) = &self.value_hint {
            action.insert("valueHint".to_owned(), serde_json::json!(hint));
        }
        if let Some(path) = &self.config_path {
            action.insert("configPath".to_owned(), serde_json::json!(path));
        }
        if let Some(key) = &self.config_key {
            action.insert("configKey".to_owned(), serde_json::json!(key));
        }
        if let Some(flag) = &self.flag_name {
            action.insert("flagName".to_owned(), serde_json::json!(flag));
        }
        if let Some(command) = &self.command {
            action.insert("command".to_owned(), serde_json::json!(command));
        }
        if let Some(results_in) = &self.results_in {
            action.insert("resultsIn".to_owned(), serde_json::json!(results_in));
        }
        if let Some(example) = &self.example {
            action.insert("example".to_owned(), serde_json::json!(example));
        }
        serde_json::Value::Object(action)
    }
}

/// Stable degraded-code specific recovery actions shared by renderers.
#[must_use]
pub fn degraded_recovery_actions(code: &str) -> Vec<RecoveryAction> {
    match code {
        "embed_model_unavailable" => vec![
            RecoveryAction {
                priority: 1,
                kind: RecoveryKind::Rebuild,
                rationale: "The embedder model is unloadable; re-embed forces a fresh initialization."
                    .to_owned(),
                env_name: None,
                value_hint: None,
                config_path: None,
                config_key: None,
                flag_name: None,
                command: Some("ee index reembed --workspace .".to_owned()),
                results_in: Some(
                    "Rebuilds the embedding index against the current embed-fast feature and model configuration."
                        .to_owned(),
                ),
                example: None,
            },
            RecoveryAction {
                priority: 2,
                kind: RecoveryKind::Rebuild,
                rationale: "If this binary was built without the dense embedder, rebuild with the supported embed-fast feature."
                    .to_owned(),
                env_name: None,
                value_hint: None,
                config_path: None,
                config_key: None,
                flag_name: None,
                command: Some("cargo build --features embed-fast".to_owned()),
                results_in: None,
                example: None,
            },
        ],
        "search_index_stale" | "index_stale" => vec![
            recovery_command(
                1,
                "ee index status --workspace . --json",
                "Inspect database and index generations before rebuilding the derived search asset.",
            ),
            recovery_command(
                2,
                "ee index rebuild --workspace . --json",
                "Rebuild the derived search index after confirming it is stale.",
            ),
        ],
        "index_missing" => vec![
            recovery_command(
                1,
                "ee index status --workspace . --json",
                "Confirm whether index metadata or files are missing.",
            ),
            recovery_command(
                2,
                "ee index rebuild --workspace . --json",
                "Recreate the derived search index from the source-of-truth database.",
            ),
        ],
        _ => Vec::new(),
    }
}

fn recovery_command(
    priority: u8,
    command: impl Into<String>,
    rationale: impl Into<String>,
) -> RecoveryAction {
    RecoveryAction {
        priority,
        kind: RecoveryKind::Command,
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

fn derivation_reflection_recovery_actions_for_code(code: &str) -> Vec<RecoveryAction> {
    match code {
        DERIVED_SOURCES_INVALID_CODE
        | "derived_source_refs_missing"
        | "derived_source_refs_invalid_json"
        | "derived_source_refs_not_array"
        | "derived_source_ref_invalid"
        | "derived_source_kind_invalid" => vec![
            recovery_command(
                1,
                "ee curate propose-derived --workspace . --json",
                "Regenerate the derivation source package with canonical source refs.",
            ),
            recovery_command(
                2,
                "ee curate show <candidate-id> --workspace . --json",
                "Inspect the stored candidate package before applying it.",
            ),
        ],
        DERIVED_SOURCE_HASH_DRIFTED_CODE | DERIVED_SOURCE_HASH_MISMATCH_CODE => vec![
            recovery_command(
                1,
                "ee curate propose-derived --workspace . --json",
                "Re-propose against current source hashes; do not bypass the drift guard.",
            ),
            recovery_command(
                2,
                "ee why <source-id> --workspace . --json",
                "Inspect the changed source before accepting a new derived memory.",
            ),
        ],
        DERIVED_SOURCE_MEMORY_TOMBSTONED_CODE => vec![
            recovery_command(
                1,
                "ee curate propose-derived --workspace . --json",
                "Re-propose from active source memories; tombstoned sources cannot be reused for apply.",
            ),
            recovery_command(
                2,
                "ee why <source-id> --workspace . --json",
                "Inspect the tombstoned source and its audit trail before choosing replacement evidence.",
            ),
        ],
        DERIVED_SOURCE_MEMORY_MISSING_CODE | DERIVED_SOURCE_EVIDENCE_MISSING_CODE => vec![
            recovery_command(
                1,
                "ee curate show <candidate-id> --workspace . --json",
                "Inspect the candidate's source refs and identify which source disappeared.",
            ),
            recovery_command(
                2,
                "ee curate propose-derived --workspace . --json",
                "Re-propose against sources that still exist in the selected workspace.",
            ),
        ],
        DERIVED_SOURCE_WORKSPACE_MISMATCH_CODE => vec![
            RecoveryAction::flag(
                1,
                "--workspace",
                "<path owning the cited source>",
                "Run derivation in the workspace that owns every cited source.",
            ),
            recovery_command(
                2,
                "ee memory list --workspace . --json",
                "List source IDs in the active workspace before re-proposing.",
            ),
        ],
        DERIVED_EVIDENCE_ALREADY_LINKED_CODE | DERIVED_SOURCE_EVIDENCE_ALREADY_LINKED_CODE => vec![
            recovery_command(
                1,
                "ee curate propose-derived --workspace . --json",
                "Choose an unlinked evidence span and create a fresh derived candidate.",
            ),
            recovery_command(
                2,
                "ee curate candidates --workspace . --json",
                "Inspect existing candidates that may have already consumed the evidence.",
            ),
        ],
        CREATE_DERIVED_REPLAY_MISSING_AUDIT_CODE
        | CREATE_DERIVED_REPLAY_AMBIGUOUS_AUDIT_CODE
        | "create_derived_replay_audit_unavailable"
        | "create_derived_replay_audit_missing_memory_id"
        | "create_derived_replay_audit_invalid_memory_id"
        | "create_derived_replay_audit_target_mismatch"
        | "create_derived_replay_memory_unavailable"
        | "create_derived_replay_memory_missing"
        | "create_derived_replay_memory_workspace_mismatch" => vec![
            recovery_command(
                1,
                "ee curate show <candidate-id> --workspace . --json",
                "Inspect the applied candidate before deciding whether replay is safe.",
            ),
            recovery_command(
                2,
                "ee audit timeline --surface curation --json",
                "Inspect curation audit rows for missing, duplicate, or mismatched apply records.",
            ),
            recovery_command(
                3,
                "ee doctor --workspace . --json",
                "Run read-only diagnostics for inconsistent derived-apply state; do not edit the database directly.",
            ),
        ],
        DERIVED_TARGET_REQUIRED_FOR_MUTATION_CODE | "target_mutation_target_required" => vec![
            recovery_command(
                1,
                "ee curate show <candidate-id> --workspace . --json",
                "Inspect the candidate type and target memory before mutation.",
            ),
            recovery_command(
                2,
                "ee curate propose-derived --workspace . --json",
                "Use create-derived when the intended operation is to create a new memory.",
            ),
        ],
        DERIVED_TARGET_FORBIDDEN_FOR_CREATE_CODE | "create_derived_target_forbidden" => vec![
            recovery_command(
                1,
                "ee curate propose-derived --workspace . --json",
                "Create-derived candidates must be re-proposed without targetMemoryId.",
            ),
            recovery_command(
                2,
                "ee curate validate <candidate-id> --workspace . --dry-run --json",
                "Validate the replacement candidate before applying it.",
            ),
        ],
        DERIVED_INVALID_MEMORY_SPEC_CODE
        | "derived_metadata_missing"
        | "derived_metadata_invalid_json"
        | "derived_metadata_invalid"
        | "derived_metadata_memory_spec_missing"
        | "derived_memory_level_invalid"
        | "derived_memory_kind_invalid"
        | "derived_memory_trust_class_invalid" => vec![
            recovery_command(
                1,
                "ee curate propose-derived --workspace . --json",
                "Regenerate the candidate with a valid memorySpec level, kind, trust, and validity window.",
            ),
            recovery_command(
                2,
                "ee schema show ee.curate.show.v1 --json",
                "Inspect the expected derived candidate shape before retrying.",
            ),
        ],
        REFLECT_REQUEST_EXPIRED_CODE | "reflection_request_expired" => vec![
            recovery_command(
                1,
                "ee reflect propose --workspace . --json",
                "Create a fresh reflection request; expired requests remain inspectable but cannot ingest results.",
            ),
            recovery_command(
                2,
                "ee reflect request-ledger diagnostics --workspace . --json",
                "Inspect the expired request posture by hash without mutating it.",
            ),
        ],
        REFLECT_CHALLENGE_INVALID_CODE
        | "missing_reflection_request_challenge"
        | "missing_reflection_request_expiry"
        | "empty_reflection_challenge_key_id"
        | "invalid_reflection_challenge_binding"
        | "reflection_challenge_json_serialization_failed"
        | "reflection_challenge_key_mismatch"
        | "reflection_challenge_algorithm_mismatch"
        | "reflection_challenge_hmac_mismatch"
        | "reflection_result_challenge_echo_mismatch"
        | "reflection_result_challenge_verification_failed" => vec![
            recovery_command(
                1,
                "ee reflect propose --workspace . --json",
                "Regenerate the request and challenge binding with the current HMAC key.",
            ),
            recovery_command(
                2,
                "ee status --workspace . --json",
                "Check reflection key and workspace posture before retrying.",
            ),
        ],
        REFLECT_REQUEST_CONSUMED_CODE | "reflection_result_replay_mismatch" => vec![
            recovery_command(
                1,
                "ee curate candidates --workspace . --json",
                "Find the candidate that already consumed this reflection request.",
            ),
            recovery_command(
                2,
                "ee reflect request-ledger diagnostics --workspace . --status consumed --json",
                "Inspect consumed request metadata without replaying ingestion.",
            ),
        ],
        REFLECT_SOURCE_DRIFTED_CODE | "reflection_request_ledger_mismatch" => vec![
            recovery_command(
                1,
                "ee reflect propose --workspace . --json",
                "Rebuild the reflection request from current source hashes.",
            ),
            recovery_command(
                2,
                "ee why <source-id> --workspace . --json",
                "Inspect the changed source before trusting a new reflection result.",
            ),
        ],
        REFLECT_UNKNOWN_CITED_SOURCE_CODE => vec![
            recovery_command(
                1,
                "ee reflect request-ledger diagnostics --workspace . --json",
                "Compare cited sources to the retained request package before ingesting a result.",
            ),
            recovery_command(
                2,
                "ee reflect propose --workspace . --json",
                "Create a new request if the producer needs a different source set.",
            ),
        ],
        REFLECT_RESULT_SCHEMA_INVALID_CODE
        | "invalid_reflection_result_artifact"
        | "reflection_result_json_serialization_failed" => vec![
            recovery_command(
                1,
                "ee reflect propose --workspace . --json",
                "Send the producer a fresh request carrying the current response schema.",
            ),
            recovery_command(
                2,
                "ee schema show ee.reflect.result.v1 --json",
                "Inspect the result schema before retrying ingest.",
            ),
        ],
        REFLECT_RAW_COT_REJECTED_CODE => vec![
            recovery_command(
                1,
                "ee reflect propose --workspace . --json",
                "Ask for concise conclusions and cited evidence only; raw chain-of-thought is never accepted.",
            ),
            recovery_command(
                2,
                "ee curate candidates --workspace . --json",
                "Use accepted non-CoT candidate content if one already exists.",
            ),
        ],
        REFLECT_KEY_UNAVAILABLE_CODE | "missing_reflection_challenge_key_material" => vec![
            recovery_command(
                1,
                "ee status --workspace . --json",
                "Inspect reflection key-store posture before creating a new request.",
            ),
            RecoveryAction::env(
                2,
                "EE_REFLECTION_HMAC_KEY",
                "<path to readable HMAC key material>",
                "Point reflection request signing at a readable key source.",
            ),
            recovery_command(
                3,
                "ee reflect propose --workspace . --json",
                "Retry request creation after the key source is available.",
            ),
        ],
        _ => Vec::new(),
    }
}

fn derivation_reflection_recovery_actions_for_message(
    error: &DomainError,
    lower_message: &str,
) -> Vec<RecoveryAction> {
    let code = match error {
        DomainError::UsageCodeWithDetails { code, .. }
        | DomainError::UnsatisfiedDegradedModeCode { code, .. } => Some(*code),
        _ => None,
    };
    if let Some(code) = code {
        let actions = derivation_reflection_recovery_actions_for_code(code);
        if !actions.is_empty() {
            return actions;
        }
    }

    let inferred_code = if lower_message.contains("reflection request source package")
        || lower_message.contains("reflection request ledger field `sourcerefsjson` does not match")
        || lower_message
            .contains("reflection request ledger field `sourcecontenthashesjson` does not match")
        || lower_message.contains("reflection result field `requesthash` mismatch")
        || lower_message.contains("reflection result field `sourcepackagehash` mismatch")
    {
        Some(REFLECT_SOURCE_DRIFTED_CODE)
    } else if lower_message.contains(CREATE_DERIVED_REPLAY_MISSING_AUDIT_CODE) {
        Some(CREATE_DERIVED_REPLAY_MISSING_AUDIT_CODE)
    } else if lower_message.contains(CREATE_DERIVED_REPLAY_AMBIGUOUS_AUDIT_CODE)
        || lower_message.contains("create_derived_replay_audit_")
        || lower_message.contains("create_derived_replay_memory_")
    {
        Some(CREATE_DERIVED_REPLAY_AMBIGUOUS_AUDIT_CODE)
    } else if lower_message.contains(DERIVED_SOURCE_MEMORY_TOMBSTONED_CODE)
        || lower_message.contains("was tombstoned before apply")
    {
        Some(DERIVED_SOURCE_MEMORY_TOMBSTONED_CODE)
    } else if lower_message.contains(DERIVED_SOURCE_EVIDENCE_ALREADY_LINKED_CODE)
        || lower_message.contains(DERIVED_EVIDENCE_ALREADY_LINKED_CODE)
        || lower_message.contains("already linked to a memory")
        || lower_message.contains("attached to another memory")
    {
        Some(DERIVED_SOURCE_EVIDENCE_ALREADY_LINKED_CODE)
    } else if lower_message.contains(DERIVED_SOURCE_MEMORY_MISSING_CODE) {
        Some(DERIVED_SOURCE_MEMORY_MISSING_CODE)
    } else if lower_message.contains(DERIVED_SOURCE_EVIDENCE_MISSING_CODE) {
        Some(DERIVED_SOURCE_EVIDENCE_MISSING_CODE)
    } else if lower_message.contains(DERIVED_SOURCE_HASH_MISMATCH_CODE)
        || lower_message.contains("hash drifted")
        || lower_message.contains("hash-drifted")
    {
        Some(DERIVED_SOURCE_HASH_DRIFTED_CODE)
    } else if lower_message.contains(DERIVED_SOURCE_WORKSPACE_MISMATCH_CODE)
        || lower_message.contains("different workspace")
        || lower_message.contains("belongs to workspace")
    {
        Some(DERIVED_SOURCE_WORKSPACE_MISMATCH_CODE)
    } else if lower_message.contains("derivation source refs")
        || lower_message.contains("derivation source json")
        || lower_message.contains("source refs array")
        || lower_message.contains("object source refs")
        || lower_message.contains("source package")
    {
        Some(DERIVED_SOURCES_INVALID_CODE)
    } else if lower_message.contains("no target memory id")
        || lower_message.contains("target-mutating operation")
    {
        Some(DERIVED_TARGET_REQUIRED_FOR_MUTATION_CODE)
    } else if lower_message.contains("targetmemoryid set to null")
        || lower_message.contains("must not target an existing memory")
        || lower_message.contains("create_derived_target_forbidden")
    {
        Some(DERIVED_TARGET_FORBIDDEN_FOR_CREATE_CODE)
    } else if lower_message.contains("memoryspec")
        || lower_message.contains("memory spec")
        || lower_message.contains("proposed trust class")
        || lower_message.contains("derivation metadata")
    {
        Some(DERIVED_INVALID_MEMORY_SPEC_CODE)
    } else if lower_message.contains("reflect_hmac_key")
        || lower_message.contains("reflection hmac")
        || lower_message.contains("hmac key")
        || lower_message.contains("reflection challenge hmac key material is not configured")
    {
        Some(REFLECT_KEY_UNAVAILABLE_CODE)
    } else if (lower_message.contains("reflect propose")
        && (lower_message.contains("source memory")
            || lower_message.contains("source evidence span")
            || lower_message.contains("source ids")))
        || (lower_message.contains("cited source id")
            && lower_message.contains("not a packaged source"))
    {
        Some(REFLECT_UNKNOWN_CITED_SOURCE_CODE)
    } else if lower_message.contains("reflection request") && lower_message.contains("expired") {
        Some(REFLECT_REQUEST_EXPIRED_CODE)
    } else if lower_message.contains("reflection request") && lower_message.contains("consumed") {
        Some(REFLECT_REQUEST_CONSUMED_CODE)
    } else if (lower_message.contains("challenge") && lower_message.contains("invalid"))
        || lower_message.contains("reflection result challenge")
        || lower_message.contains("challenge does not echo")
        || lower_message.contains("request challenge")
        || lower_message.contains("request expiry")
        || lower_message.contains("request artifact")
        || lower_message.contains("hmac did not match")
        || lower_message.contains("key id mismatch")
        || lower_message.contains("algorithm mismatch")
    {
        Some(REFLECT_CHALLENGE_INVALID_CODE)
    } else if (lower_message.contains("reflection result") && lower_message.contains("schema"))
        || lower_message.contains("expected ee.reflect.result.v1")
        || lower_message.contains("descriptor does not match the compiled reflection result schema")
        || lower_message.contains("failed to serialize reflection result material")
    {
        Some(REFLECT_RESULT_SCHEMA_INVALID_CODE)
    } else if lower_message.contains("chain-of-thought")
        || lower_message.contains("raw cot")
        || lower_message.contains("chain of thought")
        || lower_message.contains("private reasoning marker")
    {
        Some(REFLECT_RAW_COT_REJECTED_CODE)
    } else {
        None
    };

    inferred_code
        .map(derivation_reflection_recovery_actions_for_code)
        .unwrap_or_default()
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
            Self::Import { .. } | Self::ImportWithDetails { .. } => "import",
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
            | Self::ImportWithDetails { message, .. }
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
            | Self::ImportWithDetails { repair, .. }
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
        let derivation_reflection_actions =
            derivation_reflection_recovery_actions_for_message(self, &message);
        if !derivation_reflection_actions.is_empty() {
            return derivation_reflection_actions;
        }
        match self {
            Self::UsageCodeWithDetails {
                code: "curate_reason_too_large",
                ..
            } => vec![RecoveryAction::flag(
                1,
                "--reason",
                "short review reason <= 4096 bytes",
                "Store long rationale in an external note or memory, then pass a concise pointer.",
            )],
            // Cass binary not found in trusted locations.
            Self::Import { .. } | Self::ImportWithDetails { .. }
                if message.contains("cass binary not found") =>
            {
                vec![
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
                ]
            }
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
            // Memory ID lookups are often copied from a different
            // workspace or from stale context. Make the error envelope tell
            // agents how to recover without scraping the prose repair hint.
            Self::NotFound { resource, .. } if resource.to_ascii_lowercase().contains("memory") => {
                vec![
                    RecoveryAction {
                        priority: 1,
                        kind: RecoveryKind::Broaden,
                        rationale: "List known memories in the active workspace to discover the current ID."
                            .to_owned(),
                        env_name: None,
                        value_hint: None,
                        config_path: None,
                        config_key: None,
                        flag_name: None,
                        command: Some("ee memory list --workspace . --json".to_owned()),
                        results_in: None,
                        example: None,
                    },
                    RecoveryAction::flag(
                        2,
                        "--workspace",
                        "<path>",
                        "Point at the workspace that owns the memory ID.",
                    ),
                    RecoveryAction {
                        priority: 3,
                        kind: RecoveryKind::Narrow,
                        rationale: "Search by nearby content or provenance when the copied ID is stale."
                            .to_owned(),
                        env_name: None,
                        value_hint: None,
                        config_path: None,
                        config_key: None,
                        flag_name: None,
                        command: Some("ee search '<terms>' --workspace . --json".to_owned()),
                        results_in: None,
                        example: None,
                    },
                ]
            }
            // R-012 (Pass 2): Storage + "database not found" is the most
            // common first-time error path — agents that run `ee context`,
            // `ee remember`, `ee why`, or any other workspace-touching
            // command before `ee init` hit this. Surface the structured
            // recovery actions so the JSON envelope's details.recovery[]
            // tells the agent exactly what to do, not just the prose
            // repair hint.
            Self::Storage { .. } if message.contains("database not found") => vec![
                RecoveryAction {
                    priority: 1,
                    kind: RecoveryKind::Seed,
                    rationale: "Initialize the workspace at cwd; idempotent and audited."
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
                RecoveryAction::flag(
                    2,
                    "--workspace",
                    "<path>",
                    "Point at an already-initialized workspace instead of cwd.",
                ),
                RecoveryAction::env(
                    3,
                    "EE_DATABASE_PATH",
                    "<absolute path to ee.db>",
                    "Override the default <workspace>/.ee/ee.db location when the database lives elsewhere.",
                ),
            ],
            // Audit render failures are surfaced as storage errors because
            // the requested report could not cross the stable JSON boundary.
            // Keep the prose repair and the structured recovery contract in
            // lockstep so agents never receive an actionable repair string
            // with an empty `details.recovery[]` array.
            Self::Storage { repair, .. } if message.contains("audit report serialization") => {
                vec![RecoveryAction {
                    priority: 1,
                    kind: RecoveryKind::Command,
                    rationale: "Inspect storage and audit integrity before retrying the report."
                        .to_owned(),
                    env_name: None,
                    value_hint: None,
                    config_path: None,
                    config_key: None,
                    flag_name: None,
                    command: Some(repair.as_deref().unwrap_or("ee doctor --json").to_owned()),
                    results_in: None,
                    example: None,
                }]
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
            Self::Import { .. } | Self::ImportWithDetails { .. } => ProcessExitCode::Import,
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
    /// Operation was cancelled by the caller, a deadline, or a runtime budget.
    Cancelled = 130,
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
        assert_eq!(ProcessExitCode::Cancelled as u8, 130);
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
                DomainError::ImportWithDetails {
                    message: String::new(),
                    repair: None,
                    details_json: "{}".to_string(),
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
        assert_eq!(super::RecoveryKind::Command.as_str(), "command");
        assert_eq!(super::RecoveryKind::Broaden.as_str(), "broaden");
        assert_eq!(super::RecoveryKind::Narrow.as_str(), "narrow");
        assert_eq!(super::RecoveryKind::Seed.as_str(), "seed");
        assert_eq!(super::RecoveryKind::None.as_str(), "none");
    }

    #[test]
    fn repair_action_risk_class_wire_names_are_stable() {
        assert_eq!(
            super::RepairActionRiskClass::ReadOnlyProbe.as_str(),
            "read_only_probe"
        );
        assert_eq!(
            super::RepairActionRiskClass::IdempotentRefresh.as_str(),
            "idempotent_refresh"
        );
        assert_eq!(
            super::RepairActionRiskClass::MutatingLocalRepair.as_str(),
            "mutating_local_repair"
        );
        assert_eq!(
            super::RepairActionRiskClass::MutatingExternalCoordinationRepair.as_str(),
            "mutating_external_coordination_repair"
        );
        assert_eq!(
            super::RepairActionRiskClass::ApprovalRequiredRepair.as_str(),
            "approval_required_repair"
        );
        assert_eq!(
            super::RepairActionRiskClass::DestructiveOrIrreversibleRepair.as_str(),
            "destructive_or_irreversible_repair"
        );
        assert_eq!(
            super::RepairActionRiskClass::UnavailableOrManualOnly.as_str(),
            "unavailable_or_manual_only"
        );
    }

    #[test]
    fn repair_action_safety_classifies_representative_commands() {
        let read_only = super::repair_action_safety(
            super::RecoveryKind::Command,
            Some("am doctor check --verbose"),
        );
        assert_eq!(
            read_only.risk_class,
            super::RepairActionRiskClass::ReadOnlyProbe
        );
        assert!(!read_only.requires_human_approval);
        assert!(!read_only.mutates_external_state);

        let support_bundle = super::repair_action_safety(
            super::RecoveryKind::Command,
            Some("ee support bundle --workspace . --redacted --dry-run --json"),
        );
        assert_eq!(
            support_bundle.risk_class,
            super::RepairActionRiskClass::ReadOnlyProbe
        );
        assert!(!support_bundle.requires_human_approval);
        assert!(!support_bundle.mutates_external_state);

        let support_bundle_create = super::repair_action_safety(
            super::RecoveryKind::Command,
            Some("ee support bundle --workspace . --redacted --out /tmp/ee-bundle --json"),
        );
        assert_eq!(
            support_bundle_create.risk_class,
            super::RepairActionRiskClass::MutatingLocalRepair
        );
        assert!(!support_bundle_create.requires_human_approval);
        assert!(!support_bundle_create.mutates_external_state);

        let agent_mail_repair = super::repair_action_safety(
            super::RecoveryKind::Command,
            Some("am doctor repair --yes"),
        );
        assert_eq!(
            agent_mail_repair.risk_class,
            super::RepairActionRiskClass::MutatingExternalCoordinationRepair
        );
        assert!(agent_mail_repair.requires_human_approval);
        assert!(agent_mail_repair.mutates_external_state);
        assert!(agent_mail_repair.preflight_command.is_some());

        let beads_sync_status =
            super::repair_action_safety(super::RecoveryKind::Command, Some("br sync --status"));
        assert_eq!(
            beads_sync_status.risk_class,
            super::RepairActionRiskClass::ReadOnlyProbe
        );
        assert!(!beads_sync_status.requires_human_approval);
        assert!(!beads_sync_status.mutates_external_state);
        assert!(!beads_sync_status.mutates_tracker_state);
        assert!(beads_sync_status.preflight_command.is_none());

        let beads_sync =
            super::repair_action_safety(super::RecoveryKind::Command, Some("br sync --flush-only"));
        assert_eq!(
            beads_sync.risk_class,
            super::RepairActionRiskClass::MutatingExternalCoordinationRepair
        );
        assert!(beads_sync.mutates_external_state);
        assert!(beads_sync.mutates_tracker_state);
        assert_eq!(
            beads_sync.preconditions,
            vec!["shared_state_coordination_required"]
        );

        let beads_close = super::repair_action_safety(
            super::RecoveryKind::Command,
            Some("br close bd-123 --reason done"),
        );
        assert_eq!(beads_close.preconditions, vec!["bead_id_must_be_explicit"]);

        let index_status = super::repair_action_safety(
            super::RecoveryKind::Command,
            Some("ee index status --workspace . --json"),
        );
        assert_eq!(
            index_status.risk_class,
            super::RepairActionRiskClass::ReadOnlyProbe
        );

        let preflight_check = super::repair_action_safety(
            super::RecoveryKind::Command,
            Some("ee preflight check --cmd 'echo ok' --json"),
        );
        assert_eq!(
            preflight_check.risk_class,
            super::RepairActionRiskClass::ReadOnlyProbe
        );
        assert!(!preflight_check.mutates_external_state);

        let git_status =
            super::repair_action_safety(super::RecoveryKind::Command, Some("git status --short"));
        assert_eq!(
            git_status.risk_class,
            super::RepairActionRiskClass::ReadOnlyProbe
        );
        assert!(!git_status.mutates_external_state);

        let cargo_fmt_check =
            super::repair_action_safety(super::RecoveryKind::Command, Some("cargo fmt --check"));
        assert_eq!(
            cargo_fmt_check.risk_class,
            super::RepairActionRiskClass::ReadOnlyProbe
        );
        assert!(!cargo_fmt_check.requires_human_approval);
        assert!(!cargo_fmt_check.mutates_external_state);

        let index_rebuild = super::repair_action_safety(
            super::RecoveryKind::Migration,
            Some("ee index rebuild --workspace ."),
        );
        assert_eq!(
            index_rebuild.risk_class,
            super::RepairActionRiskClass::IdempotentRefresh
        );

        let destructive =
            super::repair_action_safety(super::RecoveryKind::Command, Some("git clean -fd"));
        assert_eq!(
            destructive.risk_class,
            super::RepairActionRiskClass::DestructiveOrIrreversibleRepair
        );
        assert!(destructive.requires_human_approval);
        assert!(!destructive.mutates_external_state);
        assert!(destructive.preflight_command.is_some());

        let file_removal_command = format!("{} {}", "rm", "-rf target");
        let destructive_file_removal =
            super::repair_action_safety(super::RecoveryKind::Command, Some(&file_removal_command));
        assert_eq!(
            destructive_file_removal.risk_class,
            super::RepairActionRiskClass::DestructiveOrIrreversibleRepair
        );
        assert!(destructive_file_removal.requires_human_approval);
        assert!(!destructive_file_removal.mutates_external_state);
        assert!(destructive_file_removal.preflight_command.is_some());

        let manual = super::repair_action_safety(super::RecoveryKind::None, None);
        assert_eq!(
            manual.risk_class,
            super::RepairActionRiskClass::UnavailableOrManualOnly
        );
        assert!(manual.requires_human_approval);
        assert!(manual.manual_step.is_some());
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
    fn degraded_recovery_actions_for_embed_model_unavailable_are_rebuilds() {
        let actions = super::degraded_recovery_actions("embed_model_unavailable");
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].priority, 1);
        assert_eq!(actions[0].kind, super::RecoveryKind::Rebuild);
        assert_eq!(
            actions[0].command.as_deref(),
            Some("ee index reembed --workspace .")
        );
        assert_eq!(
            actions[0].results_in.as_deref(),
            Some(
                "Rebuilds the embedding index against the current embed-fast feature and model configuration."
            )
        );
        assert_eq!(actions[1].priority, 2);
        assert_eq!(actions[1].kind, super::RecoveryKind::Rebuild);
        assert_eq!(
            actions[1].command.as_deref(),
            Some("cargo build --features embed-fast")
        );
        assert!(actions[1].results_in.is_none());

        let first_json = actions[0].data_json();
        assert_eq!(first_json["kind"], "rebuild");
        assert_eq!(first_json["command"], "ee index reembed --workspace .");
        assert_eq!(first_json["riskClass"], "idempotent_refresh");
        assert_eq!(first_json["requiresHumanApproval"], false);
        assert_eq!(first_json["mutatesExternalState"], false);
        assert_eq!(first_json["mutatesTrackerState"], false);
        assert_eq!(
            first_json["resultsIn"],
            "Rebuilds the embedding index against the current embed-fast feature and model configuration."
        );
    }

    #[test]
    fn degraded_recovery_actions_for_stale_index_start_with_status() {
        let actions = super::degraded_recovery_actions("search_index_stale");
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].priority, 1);
        assert_eq!(actions[0].kind, super::RecoveryKind::Command);
        assert_eq!(
            actions[0].command.as_deref(),
            Some("ee index status --workspace . --json")
        );
        assert_eq!(
            actions[1].command.as_deref(),
            Some("ee index rebuild --workspace . --json")
        );
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
    fn domain_error_recovery_for_storage_database_not_found() {
        // R-012 (Pass 2): the canonical first-time path. An agent that runs
        // `ee context` / `ee remember` / `ee why` before `ee init` hits this
        // error. The structured recovery[] must surface `ee init` as the
        // top action and provide alternative escape hatches via flag/env.
        let error = super::DomainError::Storage {
            message: "Database not found at /tmp/x/.ee/ee.db".to_owned(),
            repair: Some("ee init --workspace .".to_owned()),
        };
        let actions = error.recovery_actions();
        assert_eq!(actions.len(), 3, "expected 3 options, got {actions:?}");
        assert_eq!(actions[0].kind, super::RecoveryKind::Seed);
        assert_eq!(actions[0].priority, 1);
        assert_eq!(
            actions[0].command.as_deref(),
            Some("ee init --workspace ."),
            "ee init must be the top recovery action"
        );
        assert_eq!(actions[1].kind, super::RecoveryKind::Flag);
        assert_eq!(actions[1].flag_name.as_deref(), Some("--workspace"));
        assert_eq!(actions[2].kind, super::RecoveryKind::Env);
        assert_eq!(actions[2].env_name.as_deref(), Some("EE_DATABASE_PATH"));
    }

    #[test]
    fn domain_error_recovery_for_audit_serialization_uses_doctor_command() {
        let error = super::DomainError::Storage {
            message: "Audit report serialization produced invalid JSON".to_owned(),
            repair: Some("ee doctor --json".to_owned()),
        };
        let actions = error.recovery_actions();
        assert_eq!(actions.len(), 1, "expected one bounded recovery action");
        assert_eq!(actions[0].kind, super::RecoveryKind::Command);
        assert_eq!(actions[0].command.as_deref(), Some("ee doctor --json"));
        assert!(!actions[0].safety().requires_human_approval);
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

    #[test]
    fn domain_error_recovery_for_derivation_codes_has_commands() {
        let cases = [
            (
                super::DERIVED_SOURCE_HASH_DRIFTED_CODE,
                "ee curate propose-derived --workspace . --json",
            ),
            (
                "derived_source_hash_mismatch",
                "ee curate propose-derived --workspace . --json",
            ),
            (
                super::DERIVED_SOURCE_MEMORY_TOMBSTONED_CODE,
                "ee curate propose-derived --workspace . --json",
            ),
            (
                super::DERIVED_SOURCE_MEMORY_MISSING_CODE,
                "ee curate show <candidate-id> --workspace . --json",
            ),
            (
                super::DERIVED_SOURCE_EVIDENCE_MISSING_CODE,
                "ee curate show <candidate-id> --workspace . --json",
            ),
            (
                super::DERIVED_EVIDENCE_ALREADY_LINKED_CODE,
                "ee curate propose-derived --workspace . --json",
            ),
            (
                super::DERIVED_SOURCE_EVIDENCE_ALREADY_LINKED_CODE,
                "ee curate propose-derived --workspace . --json",
            ),
            (
                super::CREATE_DERIVED_REPLAY_MISSING_AUDIT_CODE,
                "ee curate show <candidate-id> --workspace . --json",
            ),
            (
                super::CREATE_DERIVED_REPLAY_AMBIGUOUS_AUDIT_CODE,
                "ee curate show <candidate-id> --workspace . --json",
            ),
            (
                super::DERIVED_TARGET_FORBIDDEN_FOR_CREATE_CODE,
                "ee curate propose-derived --workspace . --json",
            ),
            (
                "create_derived_target_forbidden",
                "ee curate propose-derived --workspace . --json",
            ),
            (
                super::DERIVED_INVALID_MEMORY_SPEC_CODE,
                "ee curate propose-derived --workspace . --json",
            ),
            (
                "derived_metadata_memory_spec_missing",
                "ee curate propose-derived --workspace . --json",
            ),
        ];
        for (code, expected_command) in cases {
            let error = super::DomainError::UsageCodeWithDetails {
                code,
                message: format!("derivation fixture for {code}"),
                repair: Some(expected_command.to_owned()),
                details_json: "{}".to_owned(),
            };
            let actions = error.recovery_actions();
            assert!(
                !actions.is_empty(),
                "{code} should produce recovery actions"
            );
            assert!(
                actions
                    .iter()
                    .any(|action| action.command.as_deref() == Some(expected_command)),
                "{code} should include {expected_command}, got {actions:?}"
            );
            let safety = actions[0].safety();
            assert!(
                matches!(
                    safety.risk_class,
                    super::RepairActionRiskClass::MutatingLocalRepair
                        | super::RepairActionRiskClass::ReadOnlyProbe
                ),
                "{code} should have classified safety metadata, got {safety:?}"
            );
        }
    }

    #[test]
    fn domain_error_recovery_for_reflection_codes_has_commands() {
        let cases = [
            (
                super::REFLECT_REQUEST_EXPIRED_CODE,
                "ee reflect propose --workspace . --json",
            ),
            (
                "reflection_request_expired",
                "ee reflect propose --workspace . --json",
            ),
            (
                super::REFLECT_REQUEST_CONSUMED_CODE,
                "ee curate candidates --workspace . --json",
            ),
            (
                super::REFLECT_SOURCE_DRIFTED_CODE,
                "ee reflect propose --workspace . --json",
            ),
            (
                super::REFLECT_RESULT_SCHEMA_INVALID_CODE,
                "ee reflect propose --workspace . --json",
            ),
            (
                "invalid_reflection_result_artifact",
                "ee reflect propose --workspace . --json",
            ),
            (
                super::REFLECT_RAW_COT_REJECTED_CODE,
                "ee reflect propose --workspace . --json",
            ),
            (
                super::REFLECT_KEY_UNAVAILABLE_CODE,
                "ee status --workspace . --json",
            ),
        ];
        for (code, expected_command) in cases {
            let error = super::DomainError::UsageCodeWithDetails {
                code,
                message: format!("reflection fixture for {code}"),
                repair: Some(expected_command.to_owned()),
                details_json: "{}".to_owned(),
            };
            let actions = error.recovery_actions();
            assert!(
                !actions.is_empty(),
                "{code} should produce recovery actions"
            );
            assert!(
                actions
                    .iter()
                    .any(|action| action.command.as_deref() == Some(expected_command)),
                "{code} should include {expected_command}, got {actions:?}"
            );
            assert!(
                actions.iter().all(|action| !action.rationale.is_empty()),
                "{code} recovery actions should carry reasons"
            );
        }
    }

    #[test]
    fn derivation_reflection_message_inference_adds_recovery() {
        let derived = super::DomainError::Usage {
            message: "Memory source mem_1 hash drifted from blake3:a to blake3:b.".to_owned(),
            repair: Some("Re-propose the candidate against the current source content.".to_owned()),
        };
        let derived_actions = derived.recovery_actions();
        assert!(
            derived_actions
                .iter()
                .any(|action| action.command.as_deref()
                    == Some("ee curate propose-derived --workspace . --json")),
            "hash drift should point at a fresh derived proposal"
        );

        let reflect = super::DomainError::Configuration {
            message: "reflect_hmac_key_missing: reflection HMAC key unavailable".to_owned(),
            repair: Some("Configure reflection HMAC key material.".to_owned()),
        };
        let reflect_actions = reflect.recovery_actions();
        assert!(
            reflect_actions
                .iter()
                .any(|action| action.command.as_deref() == Some("ee status --workspace . --json")),
            "reflection key failures should point at status"
        );
        assert!(
            reflect_actions
                .iter()
                .any(|action| action.env_name.as_deref() == Some("EE_REFLECTION_HMAC_KEY")),
            "reflection key failures should expose env recovery"
        );
    }
}
