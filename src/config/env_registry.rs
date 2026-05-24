//! Central registry for `EE_*` environment variables honored by ee.
//!
//! Adding a new `EE_*` environment variable requires adding a variant here.
//! Tests enforce that production code reads these variables through this
//! registry rather than spelling raw names at call sites.

use std::ffi::OsString;
use std::str::FromStr;

/// Every `EE_*` environment variable honored by ee.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EnvVar {
    /// `EE_AGENT_NAME`
    AgentName,
    /// `EE_AGENT_MODE`
    AgentMode,
    /// `EE_AUDIT_LANE_BATCH_MAX`
    AuditLaneBatchMax,
    /// `EE_AUDIT_LANE_CAPACITY`
    AuditLaneCapacity,
    /// `EE_AUDIT_LANE_FLUSH_MS`
    AuditLaneFlushMs,
    /// `EE_CASS_BINARY`
    CassBinary,
    /// `EE_CURATION_AUTO_PROMOTE_CONFIDENCE_FLOOR`
    CurationAutoPromoteConfidenceFloor,
    /// `EE_CURATION_AUTO_PROMOTE_MAX_PER_RUN`
    CurationAutoPromoteMaxPerRun,
    /// `EE_CURATION_DERIVED_PREVIEW_LIMIT`
    CurationDerivedPreviewLimit,
    /// `EE_DATABASE_PATH`
    DatabasePath,
    /// `EE_DEMO_EVIDENCE_ROOT`
    DemoEvidenceRoot,
    /// `EE_DIAG_FORCE_CAPABILITY_GAP`
    DiagForceCapabilityGap,
    /// `EE_DISABLE_TOON`
    DisableToon,
    /// `EE_DISABLE_REMEMBER_SEARCH_NEIGHBORS`
    DisableRememberSearchNeighbors,
    /// `EE_E2E_RETENTION_MANIFEST`
    E2eRetentionManifest,
    /// `EE_EMBED_DEDUP_COSINE_FLOOR`
    EmbedDedupCosineFloor,
    /// `EE_EMBED_DEDUP_ENABLED`
    EmbedDedupEnabled,
    /// `EE_EMBED_DEDUP_HAMMING_K`
    EmbedDedupHammingK,
    /// `EE_EMBED_MODEL_PATH`
    EmbedModelPath,
    /// `EE_EXPERIMENTAL_TRIAD`
    ExperimentalTriad,
    /// `EE_FLIGHT_RECORDER`
    FlightRecorder,
    /// `EE_FLIGHT_RECORDER_DIR`
    FlightRecorderDir,
    /// `EE_FLIGHT_RECORDER_RETENTION_DAYS`
    FlightRecorderRetentionDays,
    /// `EE_FORMAT`
    Format,
    /// `EE_GRAPH_MEMORY_DEGRADED_BELOW_PCT`
    GraphMemoryDegradedBelowPct,
    /// `EE_GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS`
    GraphMemoryGrowthMultiplierBasisPoints,
    /// `EE_GRAPH_MEMORY_PER_ALGORITHM_CAP_MB`
    GraphMemoryPerAlgorithmCapMb,
    /// `EE_GRAPH_MEMORY_SNAPSHOT_CAP_MB`
    GraphMemorySnapshotCapMb,
    /// `EE_GRAPH_WITNESSES_RETENTION_DAYS`
    GraphWitnessesRetentionDays,
    /// `EE_HARMFUL_BURST_WINDOW_SECONDS`
    HarmfulBurstWindowSeconds,
    /// `EE_HARMFUL_PER_SOURCE_PER_HOUR`
    HarmfulPerSourcePerHour,
    /// `EE_HOOK_MODE`
    HookMode,
    /// `EE_INDEX_DIR`
    IndexDir,
    /// `EE_INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS`
    IndexPublishLockRetryAttempts,
    /// `EE_JSON`
    Json,
    /// `EE_L2_PACK_CACHE_BYTES`
    L2PackCacheBytes,
    /// `EE_L2_PACK_CACHE_DIR`
    L2PackCacheDir,
    /// `EE_L2_PACK_CACHE_DISABLE`
    L2PackCacheDisable,
    /// `EE_LEGACY_SELECTION_CERTIFICATE`
    LegacySelectionCertificate,
    /// `EE_LEXICAL_INDEX_HUGEPAGES`
    LexicalIndexHugepages,
    /// `EE_LEXICAL_INDEX_PIN_RAM`
    LexicalIndexPinRam,
    /// `EE_LOG_FORMAT`
    LogFormat,
    /// `EE_LOG_JSON`
    LogJson,
    /// `EE_MAX_TOKENS`
    MaxTokens,
    /// `EE_MESH_DISCOVERY_CACHE_TTL_SECONDS`
    MeshDiscoveryCacheTtlSeconds,
    /// `EE_MESH_DRIFT_SOFT_STALE_AFTER`
    MeshDriftSoftStaleAfter,
    /// `EE_MESH_DRIFT_SOFT_STALE_AFTER_SECONDS`
    MeshDriftSoftStaleAfterSeconds,
    /// `EE_MESH_DRIFT_HARD_STALE_AFTER`
    MeshDriftHardStaleAfter,
    /// `EE_MESH_DRIFT_HARD_STALE_AFTER_SECONDS`
    MeshDriftHardStaleAfterSeconds,
    /// `EE_MESH_ENABLED`
    MeshEnabled,
    /// `EE_MESH_HELLO_PORT`
    MeshHelloPort,
    /// `EE_MESH_HELLO_RESPONDER_DISABLED`
    MeshHelloResponderDisabled,
    /// `EE_MESH_MODE`
    MeshMode,
    /// `EE_NO_COLOR`
    NoColor,
    /// `EE_OUTPUT_FORMAT`
    OutputFormat,
    /// `EE_PREFLIGHT_BYPASS_SECRET`
    PreflightBypassSecret,
    /// `EE_PROFILE`
    Profile,
    /// `EE_PPR_CACHE_ENTRIES`
    PprCacheEntries,
    /// `EE_QUERY_PLAN_CACHE_ENTRIES`
    QueryPlanCacheEntries,
    /// `EE_READ_POOL_DISABLE_PIN`
    ReadPoolDisablePin,
    /// `EE_READ_POOL_ACQUIRE_TIMEOUT_MS`
    ReadPoolAcquireTimeoutMs,
    /// `EE_READ_POOL_IDLE_TIMEOUT_S`
    ReadPoolIdleTimeoutSeconds,
    /// `EE_READ_POOL_MAX_PIN_SECONDS`
    ReadPoolMaxPinSeconds,
    /// `EE_READ_POOL_SIZE`
    ReadPoolSize,
    /// `EE_REFLECTION_CONSUMED_RETENTION_DAYS`
    ReflectionConsumedRetentionDays,
    /// `EE_REFLECTION_EXPIRED_RETENTION_DAYS`
    ReflectionExpiredRetentionDays,
    /// `EE_REFLECTION_HMAC_KEY_ID`
    ReflectionHmacKeyId,
    /// `EE_REFLECTION_HMAC_KEY_PATH`
    ReflectionHmacKeyPath,
    /// `EE_REFLECTION_HMAC_ROTATION_GRACE_SECONDS`
    ReflectionHmacRotationGraceSeconds,
    /// `EE_REFLECTION_REQUEST_LIST_LIMIT`
    ReflectionRequestListLimit,
    /// `EE_REFLECTION_REQUEST_SHOW_SOURCE_LIMIT`
    ReflectionRequestShowSourceLimit,
    /// `EE_REFLECTION_REQUEST_TTL_SECONDS`
    ReflectionRequestTtlSeconds,
    /// `EE_REFLECTION_SOURCE_BUDGET_BYTES`
    ReflectionSourceBudgetBytes,
    /// `EE_REMEMBER_CURATION_SYNC_BUDGET_MS`
    RememberCurationSyncBudgetMs,
    /// `EE_SECURITY_PROFILE`
    SecurityProfile,
    /// `EE_SERVE_TOKEN`
    ServeToken,
    /// `EE_SCIENCE_BACKEND_PATH`
    ScienceBackendPath,
    /// `EE_SHARD_FANOUT_ENABLED`
    ShardFanoutEnabled,
    /// `EE_SHARDS_DIR`
    ShardsDir,
    /// `EE_TEST_LOG_LEVEL`
    TestLogLevel,
    /// `EE_TEST_LOG_PATH`
    TestLogPath,
    /// `EE_TEST_LOG_TEST_ID`
    TestLogTestId,
    /// `EE_TAILSCALE_BINARY_OVERRIDE`
    TailscaleBinaryOverride,
    /// `EE_TAILSCALE_PROBE_TIMEOUT_MS`
    TailscaleProbeTimeoutMs,
    /// `EE_TAILSCALE_PROBE_SOCKET_OVERRIDE`
    TailscaleProbeSocketOverride,
    /// `EE_TAILSCALE_DISCOVERY_MODE`
    TailscaleDiscoveryMode,
    /// `EE_TAILSCALE_PEER_PROBE_TIMEOUT_MS`
    TailscalePeerProbeTimeoutMs,
    /// `EE_TAILSCALE_DISCOVERY_BUDGET_MS`
    TailscaleDiscoveryBudgetMs,
    /// `EE_TAILSCALE_RESPOND_MODE`
    TailscaleRespondMode,
    /// `EE_WORKSPACE_HYGIENE_ALWAYS_REVIEW_PATTERNS`
    WorkspaceHygieneAlwaysReviewPatterns,
    /// `EE_WORKSPACE_HYGIENE_GENERATED_PATTERNS`
    WorkspaceHygieneGeneratedPatterns,
    /// `EE_WORKSPACE_HYGIENE_LOCAL_MACHINE_PATTERNS`
    WorkspaceHygieneLocalMachinePatterns,
    /// `EE_WORKSPACE_HYGIENE_SCRATCH_PATTERNS`
    WorkspaceHygieneScratchPatterns,
    /// `EE_WAL_CHECKPOINT_BYTES_THRESHOLD`
    WalCheckpointBytesThreshold,
    /// `EE_WORKSPACE`
    Workspace,
    /// `EE_WORKSPACE_CLOSE_DRAIN_TIMEOUT_S`
    WorkspaceCloseDrainTimeoutSeconds,
    /// `EE_WORKSPACE_REGISTRY`
    WorkspaceRegistry,
}

impl EnvVar {
    /// Return all registered variables in stable display order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::AgentName,
            Self::AgentMode,
            Self::AuditLaneBatchMax,
            Self::AuditLaneCapacity,
            Self::AuditLaneFlushMs,
            Self::CassBinary,
            Self::CurationAutoPromoteConfidenceFloor,
            Self::CurationAutoPromoteMaxPerRun,
            Self::CurationDerivedPreviewLimit,
            Self::DatabasePath,
            Self::DemoEvidenceRoot,
            Self::DiagForceCapabilityGap,
            Self::DisableToon,
            Self::DisableRememberSearchNeighbors,
            Self::E2eRetentionManifest,
            Self::EmbedDedupCosineFloor,
            Self::EmbedDedupEnabled,
            Self::EmbedDedupHammingK,
            Self::EmbedModelPath,
            Self::ExperimentalTriad,
            Self::FlightRecorder,
            Self::FlightRecorderDir,
            Self::FlightRecorderRetentionDays,
            Self::Format,
            Self::GraphMemoryDegradedBelowPct,
            Self::GraphMemoryGrowthMultiplierBasisPoints,
            Self::GraphMemoryPerAlgorithmCapMb,
            Self::GraphMemorySnapshotCapMb,
            Self::GraphWitnessesRetentionDays,
            Self::HarmfulBurstWindowSeconds,
            Self::HarmfulPerSourcePerHour,
            Self::HookMode,
            Self::IndexDir,
            Self::IndexPublishLockRetryAttempts,
            Self::Json,
            Self::L2PackCacheBytes,
            Self::L2PackCacheDir,
            Self::L2PackCacheDisable,
            Self::LegacySelectionCertificate,
            Self::LexicalIndexHugepages,
            Self::LexicalIndexPinRam,
            Self::LogFormat,
            Self::LogJson,
            Self::MaxTokens,
            Self::MeshDiscoveryCacheTtlSeconds,
            Self::MeshDriftSoftStaleAfter,
            Self::MeshDriftSoftStaleAfterSeconds,
            Self::MeshDriftHardStaleAfter,
            Self::MeshDriftHardStaleAfterSeconds,
            Self::MeshEnabled,
            Self::MeshHelloPort,
            Self::MeshHelloResponderDisabled,
            Self::MeshMode,
            Self::NoColor,
            Self::OutputFormat,
            Self::PreflightBypassSecret,
            Self::Profile,
            Self::PprCacheEntries,
            Self::QueryPlanCacheEntries,
            Self::ReadPoolDisablePin,
            Self::ReadPoolAcquireTimeoutMs,
            Self::ReadPoolIdleTimeoutSeconds,
            Self::ReadPoolMaxPinSeconds,
            Self::ReadPoolSize,
            Self::ReflectionConsumedRetentionDays,
            Self::ReflectionExpiredRetentionDays,
            Self::ReflectionHmacKeyId,
            Self::ReflectionHmacKeyPath,
            Self::ReflectionHmacRotationGraceSeconds,
            Self::ReflectionRequestListLimit,
            Self::ReflectionRequestShowSourceLimit,
            Self::ReflectionRequestTtlSeconds,
            Self::ReflectionSourceBudgetBytes,
            Self::RememberCurationSyncBudgetMs,
            Self::SecurityProfile,
            Self::ServeToken,
            Self::ScienceBackendPath,
            Self::ShardFanoutEnabled,
            Self::ShardsDir,
            Self::TestLogLevel,
            Self::TestLogPath,
            Self::TestLogTestId,
            Self::TailscaleBinaryOverride,
            Self::TailscaleProbeTimeoutMs,
            Self::TailscaleProbeSocketOverride,
            Self::TailscaleDiscoveryMode,
            Self::TailscalePeerProbeTimeoutMs,
            Self::TailscaleDiscoveryBudgetMs,
            Self::TailscaleRespondMode,
            Self::WorkspaceHygieneAlwaysReviewPatterns,
            Self::WorkspaceHygieneGeneratedPatterns,
            Self::WorkspaceHygieneLocalMachinePatterns,
            Self::WorkspaceHygieneScratchPatterns,
            Self::WalCheckpointBytesThreshold,
            Self::Workspace,
            Self::WorkspaceCloseDrainTimeoutSeconds,
            Self::WorkspaceRegistry,
        ]
    }

    /// Stable environment variable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::AgentName => "EE_AGENT_NAME",
            Self::AgentMode => "EE_AGENT_MODE",
            Self::AuditLaneBatchMax => "EE_AUDIT_LANE_BATCH_MAX",
            Self::AuditLaneCapacity => "EE_AUDIT_LANE_CAPACITY",
            Self::AuditLaneFlushMs => "EE_AUDIT_LANE_FLUSH_MS",
            Self::CassBinary => "EE_CASS_BINARY",
            Self::CurationAutoPromoteConfidenceFloor => "EE_CURATION_AUTO_PROMOTE_CONFIDENCE_FLOOR",
            Self::CurationAutoPromoteMaxPerRun => "EE_CURATION_AUTO_PROMOTE_MAX_PER_RUN",
            Self::CurationDerivedPreviewLimit => "EE_CURATION_DERIVED_PREVIEW_LIMIT",
            Self::DatabasePath => "EE_DATABASE_PATH",
            Self::DemoEvidenceRoot => "EE_DEMO_EVIDENCE_ROOT",
            Self::DiagForceCapabilityGap => "EE_DIAG_FORCE_CAPABILITY_GAP",
            Self::DisableToon => "EE_DISABLE_TOON",
            Self::DisableRememberSearchNeighbors => "EE_DISABLE_REMEMBER_SEARCH_NEIGHBORS",
            Self::E2eRetentionManifest => "EE_E2E_RETENTION_MANIFEST",
            Self::EmbedDedupCosineFloor => "EE_EMBED_DEDUP_COSINE_FLOOR",
            Self::EmbedDedupEnabled => "EE_EMBED_DEDUP_ENABLED",
            Self::EmbedDedupHammingK => "EE_EMBED_DEDUP_HAMMING_K",
            Self::EmbedModelPath => "EE_EMBED_MODEL_PATH",
            Self::ExperimentalTriad => "EE_EXPERIMENTAL_TRIAD",
            Self::FlightRecorder => "EE_FLIGHT_RECORDER",
            Self::FlightRecorderDir => "EE_FLIGHT_RECORDER_DIR",
            Self::FlightRecorderRetentionDays => "EE_FLIGHT_RECORDER_RETENTION_DAYS",
            Self::Format => "EE_FORMAT",
            Self::GraphMemoryDegradedBelowPct => "EE_GRAPH_MEMORY_DEGRADED_BELOW_PCT",
            Self::GraphMemoryGrowthMultiplierBasisPoints => {
                "EE_GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS"
            }
            Self::GraphMemoryPerAlgorithmCapMb => "EE_GRAPH_MEMORY_PER_ALGORITHM_CAP_MB",
            Self::GraphMemorySnapshotCapMb => "EE_GRAPH_MEMORY_SNAPSHOT_CAP_MB",
            Self::GraphWitnessesRetentionDays => "EE_GRAPH_WITNESSES_RETENTION_DAYS",
            Self::HarmfulBurstWindowSeconds => "EE_HARMFUL_BURST_WINDOW_SECONDS",
            Self::HarmfulPerSourcePerHour => "EE_HARMFUL_PER_SOURCE_PER_HOUR",
            Self::HookMode => "EE_HOOK_MODE",
            Self::IndexDir => "EE_INDEX_DIR",
            Self::IndexPublishLockRetryAttempts => "EE_INDEX_PUBLISH_LOCK_RETRY_ATTEMPTS",
            Self::Json => "EE_JSON",
            Self::L2PackCacheBytes => "EE_L2_PACK_CACHE_BYTES",
            Self::L2PackCacheDir => "EE_L2_PACK_CACHE_DIR",
            Self::L2PackCacheDisable => "EE_L2_PACK_CACHE_DISABLE",
            Self::LegacySelectionCertificate => "EE_LEGACY_SELECTION_CERTIFICATE",
            Self::LexicalIndexHugepages => "EE_LEXICAL_INDEX_HUGEPAGES",
            Self::LexicalIndexPinRam => "EE_LEXICAL_INDEX_PIN_RAM",
            Self::LogFormat => "EE_LOG_FORMAT",
            Self::LogJson => "EE_LOG_JSON",
            Self::MaxTokens => "EE_MAX_TOKENS",
            Self::MeshDiscoveryCacheTtlSeconds => "EE_MESH_DISCOVERY_CACHE_TTL_SECONDS",
            Self::MeshDriftSoftStaleAfter => "EE_MESH_DRIFT_SOFT_STALE_AFTER",
            Self::MeshDriftSoftStaleAfterSeconds => "EE_MESH_DRIFT_SOFT_STALE_AFTER_SECONDS",
            Self::MeshDriftHardStaleAfter => "EE_MESH_DRIFT_HARD_STALE_AFTER",
            Self::MeshDriftHardStaleAfterSeconds => "EE_MESH_DRIFT_HARD_STALE_AFTER_SECONDS",
            Self::MeshEnabled => "EE_MESH_ENABLED",
            Self::MeshHelloPort => "EE_MESH_HELLO_PORT",
            Self::MeshHelloResponderDisabled => "EE_MESH_HELLO_RESPONDER_DISABLED",
            Self::MeshMode => "EE_MESH_MODE",
            Self::NoColor => "EE_NO_COLOR",
            Self::OutputFormat => "EE_OUTPUT_FORMAT",
            Self::PreflightBypassSecret => "EE_PREFLIGHT_BYPASS_SECRET",
            Self::Profile => "EE_PROFILE",
            Self::PprCacheEntries => "EE_PPR_CACHE_ENTRIES",
            Self::QueryPlanCacheEntries => "EE_QUERY_PLAN_CACHE_ENTRIES",
            Self::ReadPoolDisablePin => "EE_READ_POOL_DISABLE_PIN",
            Self::ReadPoolAcquireTimeoutMs => "EE_READ_POOL_ACQUIRE_TIMEOUT_MS",
            Self::ReadPoolIdleTimeoutSeconds => "EE_READ_POOL_IDLE_TIMEOUT_S",
            Self::ReadPoolMaxPinSeconds => "EE_READ_POOL_MAX_PIN_SECONDS",
            Self::ReadPoolSize => "EE_READ_POOL_SIZE",
            Self::ReflectionConsumedRetentionDays => "EE_REFLECTION_CONSUMED_RETENTION_DAYS",
            Self::ReflectionExpiredRetentionDays => "EE_REFLECTION_EXPIRED_RETENTION_DAYS",
            Self::ReflectionHmacKeyId => "EE_REFLECTION_HMAC_KEY_ID",
            Self::ReflectionHmacKeyPath => "EE_REFLECTION_HMAC_KEY_PATH",
            Self::ReflectionHmacRotationGraceSeconds => "EE_REFLECTION_HMAC_ROTATION_GRACE_SECONDS",
            Self::ReflectionRequestListLimit => "EE_REFLECTION_REQUEST_LIST_LIMIT",
            Self::ReflectionRequestShowSourceLimit => "EE_REFLECTION_REQUEST_SHOW_SOURCE_LIMIT",
            Self::ReflectionRequestTtlSeconds => "EE_REFLECTION_REQUEST_TTL_SECONDS",
            Self::ReflectionSourceBudgetBytes => "EE_REFLECTION_SOURCE_BUDGET_BYTES",
            Self::RememberCurationSyncBudgetMs => "EE_REMEMBER_CURATION_SYNC_BUDGET_MS",
            Self::SecurityProfile => "EE_SECURITY_PROFILE",
            Self::ServeToken => "EE_SERVE_TOKEN",
            Self::ScienceBackendPath => "EE_SCIENCE_BACKEND_PATH",
            Self::ShardFanoutEnabled => "EE_SHARD_FANOUT_ENABLED",
            Self::ShardsDir => "EE_SHARDS_DIR",
            Self::TestLogLevel => "EE_TEST_LOG_LEVEL",
            Self::TestLogPath => "EE_TEST_LOG_PATH",
            Self::TestLogTestId => "EE_TEST_LOG_TEST_ID",
            Self::TailscaleBinaryOverride => "EE_TAILSCALE_BINARY_OVERRIDE",
            Self::TailscaleProbeTimeoutMs => "EE_TAILSCALE_PROBE_TIMEOUT_MS",
            Self::TailscaleProbeSocketOverride => "EE_TAILSCALE_PROBE_SOCKET_OVERRIDE",
            Self::TailscaleDiscoveryMode => "EE_TAILSCALE_DISCOVERY_MODE",
            Self::TailscalePeerProbeTimeoutMs => "EE_TAILSCALE_PEER_PROBE_TIMEOUT_MS",
            Self::TailscaleDiscoveryBudgetMs => "EE_TAILSCALE_DISCOVERY_BUDGET_MS",
            Self::TailscaleRespondMode => "EE_TAILSCALE_RESPOND_MODE",
            Self::WorkspaceHygieneAlwaysReviewPatterns => {
                "EE_WORKSPACE_HYGIENE_ALWAYS_REVIEW_PATTERNS"
            }
            Self::WorkspaceHygieneGeneratedPatterns => "EE_WORKSPACE_HYGIENE_GENERATED_PATTERNS",
            Self::WorkspaceHygieneLocalMachinePatterns => {
                "EE_WORKSPACE_HYGIENE_LOCAL_MACHINE_PATTERNS"
            }
            Self::WorkspaceHygieneScratchPatterns => "EE_WORKSPACE_HYGIENE_SCRATCH_PATTERNS",
            Self::WalCheckpointBytesThreshold => "EE_WAL_CHECKPOINT_BYTES_THRESHOLD",
            Self::Workspace => "EE_WORKSPACE",
            Self::WorkspaceCloseDrainTimeoutSeconds => "EE_WORKSPACE_CLOSE_DRAIN_TIMEOUT_S",
            Self::WorkspaceRegistry => "EE_WORKSPACE_REGISTRY",
        }
    }

    /// Human-readable control surface description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::AgentName => "Identify the current agent for scoped memory retrieval.",
            Self::AgentMode => "Use agent-oriented output defaults.",
            Self::AuditLaneBatchMax => "Override the audit-lane writer batch size before flushing.",
            Self::AuditLaneCapacity => "Override the audit-lane producer queue capacity.",
            Self::AuditLaneFlushMs => {
                "Override the audit-lane time-based flush interval in milliseconds."
            }
            Self::WalCheckpointBytesThreshold => {
                "Override the WAL checkpoint warning threshold in bytes."
            }
            Self::CassBinary => "Override the trusted cass import binary path.",
            Self::CurationAutoPromoteConfidenceFloor => {
                "Override the minimum confidence required by curation auto-promotion."
            }
            Self::CurationAutoPromoteMaxPerRun => {
                "Override the maximum curation candidates auto-promotion may accept per run."
            }
            Self::CurationDerivedPreviewLimit => {
                "Override the derived-candidate preview/reject listing limit."
            }
            Self::DatabasePath => "Override the configured storage database path.",
            Self::DemoEvidenceRoot => "Override the demo evidence storage root.",
            Self::DiagForceCapabilityGap => {
                "Force selected capability probes to report build-gap diagnostics."
            }
            Self::DisableToon => "Disable TOON output capability reporting and auto-selection.",
            Self::DisableRememberSearchNeighbors => {
                "Disable Frankensearch neighbors during remember-time proposal."
            }
            Self::E2eRetentionManifest => {
                "Override the retained-artifact manifest path used by diagnostics."
            }
            Self::EmbedDedupCosineFloor => {
                "Set the cosine-similarity floor for insert-time embedding dedup confirmation."
            }
            Self::EmbedDedupEnabled => {
                "Enable insert-time embedding deduplication after storage and write-path gates are wired."
            }
            Self::EmbedDedupHammingK => {
                "Set the maximum SimHash Hamming distance admitted to dedup cosine confirmation."
            }
            Self::EmbedModelPath => {
                "Override the embedder model path used by search-time embedder availability checks."
            }
            Self::ExperimentalTriad => {
                "Compatibility no-op for the promoted ee pack/note/why aliases."
            }
            Self::FlightRecorder => {
                "Enable the redacted command flight recorder for ee subcommands."
            }
            Self::FlightRecorderDir => {
                "Override the directory where flight recorder traces are written."
            }
            Self::FlightRecorderRetentionDays => {
                "Override the flight recorder trace retention window in days."
            }
            Self::Format => "Select the default output renderer.",
            Self::GraphMemoryDegradedBelowPct => {
                "Override the graph snapshot advisory threshold as a percent of the snapshot cap."
            }
            Self::GraphMemoryGrowthMultiplierBasisPoints => {
                "Override the graph snapshot in-build growth tripwire ratio in basis points."
            }
            Self::GraphMemoryPerAlgorithmCapMb => {
                "Override the per-algorithm graph working-set cap in MiB."
            }
            Self::GraphMemorySnapshotCapMb => "Override the graph snapshot admission cap in MiB.",
            Self::GraphWitnessesRetentionDays => {
                "Override the default graph algorithm witness retention window in days."
            }
            Self::HarmfulBurstWindowSeconds => {
                "Override the harmful feedback burst window in seconds."
            }
            Self::HarmfulPerSourcePerHour => "Override the harmful feedback rate limit per source.",
            Self::HookMode => "Use hook-oriented machine output defaults.",
            Self::IndexDir => "Override the configured search index directory.",
            Self::IndexPublishLockRetryAttempts => {
                "Override index publish advisory-lock retry attempts."
            }
            Self::Json => "Request JSON output from renderer auto-detection.",
            Self::L2PackCacheBytes => "Override the L2 pack cache byte cap per workspace.",
            Self::L2PackCacheDir => "Override the L2 pack cache root directory.",
            Self::L2PackCacheDisable => "Disable L2 pack cache lookup and writes.",
            Self::LegacySelectionCertificate => {
                "Include the legacy selectionCertificate field in context JSON."
            }
            Self::LexicalIndexHugepages => {
                "Request transparent hugepage hints for opt-in lexical index RAM-tier pinning."
            }
            Self::LexicalIndexPinRam => "Opt in to lexical index RAM-tier page-cache population.",
            Self::LogFormat => "Select structured log format.",
            Self::LogJson => "Enable JSON command-start logs on stderr.",
            Self::MaxTokens => "Override the default context pack token budget.",
            Self::MeshDiscoveryCacheTtlSeconds => {
                "Override the mesh autodiscovery cache TTL in seconds."
            }
            Self::MeshDriftSoftStaleAfter => {
                "Override missed mesh hello probes before soft-stale drift grace."
            }
            Self::MeshDriftSoftStaleAfterSeconds => {
                "Override seconds since last successful mesh probe before soft-stale drift grace."
            }
            Self::MeshDriftHardStaleAfter => {
                "Override missed mesh hello probes before hard-stale drift."
            }
            Self::MeshDriftHardStaleAfterSeconds => {
                "Override seconds since last successful mesh probe before hard-stale drift."
            }
            Self::MeshEnabled => "Enable optional mesh-memory surfaces.",
            Self::MeshHelloPort => {
                "Override the mesh hello responder bind port on the local Tailscale address."
            }
            Self::MeshHelloResponderDisabled => {
                "Disable the mesh hello responder lifecycle job while leaving other mesh surfaces enabled."
            }
            Self::MeshMode => "Select the default mesh command mode.",
            Self::NoColor => "Disable colored diagnostics.",
            Self::OutputFormat => "Select the default output renderer.",
            Self::PreflightBypassSecret => "Supply preflight bypass secret material.",
            Self::Profile => "Override the default context pack profile.",
            Self::PprCacheEntries => "Override the in-process PPR prefetch cache entry cap.",
            Self::QueryPlanCacheEntries => {
                "Override the in-process EQL query plan cache entry cap."
            }
            Self::ReadPoolDisablePin => "Disable read-side snapshot pinning.",
            Self::ReadPoolAcquireTimeoutMs => {
                "Override the read-side connection pool acquire timeout in milliseconds."
            }
            Self::ReadPoolIdleTimeoutSeconds => {
                "Override the read-side connection pool idle timeout in seconds."
            }
            Self::ReadPoolMaxPinSeconds => {
                "Override the read-side snapshot pin maximum lifetime in seconds."
            }
            Self::ReadPoolSize => "Override the read-side connection pool size.",
            Self::ReflectionConsumedRetentionDays => {
                "Override retention for consumed reflection requests in days."
            }
            Self::ReflectionExpiredRetentionDays => {
                "Override retention for expired reflection requests in days."
            }
            Self::ReflectionHmacKeyId => "Select the reflection request HMAC key identifier.",
            Self::ReflectionHmacKeyPath => {
                "Select the reflection request HMAC key file path without exposing key material."
            }
            Self::ReflectionHmacRotationGraceSeconds => {
                "Override reflection HMAC key rotation grace in seconds."
            }
            Self::ReflectionRequestListLimit => {
                "Override the default reflection request list limit."
            }
            Self::ReflectionRequestShowSourceLimit => {
                "Override how many source-package entries reflection request show may include."
            }
            Self::ReflectionRequestTtlSeconds => {
                "Override the default reflection request TTL in seconds."
            }
            Self::ReflectionSourceBudgetBytes => {
                "Override the reflection source-package byte budget."
            }
            Self::RememberCurationSyncBudgetMs => {
                "Override remember-time curation sync budget in milliseconds."
            }
            Self::SecurityProfile => "Select security profile.",
            Self::ServeToken => {
                "Configure the bearer token required by the localhost serve adapter."
            }
            Self::ScienceBackendPath => {
                "Configure an optional science analytics backend path; missing paths report backend-unavailable."
            }
            Self::ShardFanoutEnabled => {
                "Enable read-only shard fan-out planning and, after migration, per-workspace shard routing."
            }
            Self::ShardsDir => {
                "Override the per-workspace shard directory used by shard fan-out planning."
            }
            Self::TestLogLevel => "Control structured test-log verbosity.",
            Self::TestLogPath => "Enable structured test logging at this JSONL path.",
            Self::TestLogTestId => "Name the active structured test-log scenario.",
            Self::TailscaleBinaryOverride => {
                "Test-only override for the tailscale binary used by fake-tailnet harnesses."
            }
            Self::TailscaleProbeTimeoutMs => "Override the local Tailscale probe timeout budget.",
            Self::TailscaleProbeSocketOverride => {
                "Test-only override for fake mesh hello responder socket discovery."
            }
            Self::TailscaleDiscoveryMode => {
                "Select the caller-side mesh peer discovery policy (service_tag, auto_admit, allowlist)."
            }
            Self::TailscalePeerProbeTimeoutMs => {
                "Override the per-peer Tailscale hello probe timeout budget."
            }
            Self::TailscaleDiscoveryBudgetMs => {
                "Override the total Tailscale peer autodiscovery wall-clock budget."
            }
            Self::TailscaleRespondMode => {
                "Select the responder-side mesh discovery consent policy (service_tag, auto_admit, allowlist)."
            }
            Self::WorkspaceHygieneAlwaysReviewPatterns => {
                "Add local workspace-hygiene patterns that force matching paths into human review."
            }
            Self::WorkspaceHygieneGeneratedPatterns => {
                "Add local workspace-hygiene generated-artifact path patterns."
            }
            Self::WorkspaceHygieneLocalMachinePatterns => {
                "Add local workspace-hygiene machine-local artifact path patterns."
            }
            Self::WorkspaceHygieneScratchPatterns => {
                "Add local workspace-hygiene scratch-artifact path patterns."
            }
            Self::Workspace => "Override workspace root discovery.",
            Self::WorkspaceCloseDrainTimeoutSeconds => {
                "Override workspace-close wait time for read snapshot pins in seconds."
            }
            Self::WorkspaceRegistry => "Override the workspace alias registry database path.",
        }
    }

    /// Default value, when the variable has a registry-defined default.
    #[must_use]
    pub const fn default_value(self) -> Option<&'static str> {
        match self {
            Self::MeshMode => Some("off"),
            Self::MeshEnabled => Some("false"),
            Self::ShardFanoutEnabled => Some("false"),
            Self::AuditLaneBatchMax => Some("64"),
            Self::AuditLaneCapacity => Some("1024"),
            Self::AuditLaneFlushMs => Some("5"),
            Self::TailscaleProbeTimeoutMs => Some("1500"),
            Self::MeshDiscoveryCacheTtlSeconds => Some("30"),
            Self::MeshDriftSoftStaleAfter => Some("1"),
            Self::MeshDriftSoftStaleAfterSeconds => Some("300"),
            Self::MeshDriftHardStaleAfter => Some("3"),
            Self::MeshDriftHardStaleAfterSeconds => Some("3600"),
            Self::MeshHelloPort => Some("41888"),
            Self::MeshHelloResponderDisabled => Some("false"),
            Self::TailscaleDiscoveryMode => Some("service_tag"),
            Self::TailscalePeerProbeTimeoutMs => Some("750"),
            Self::TailscaleDiscoveryBudgetMs => Some("5000"),
            Self::TailscaleRespondMode => Some("service_tag"),
            Self::EmbedDedupCosineFloor => Some("0.97"),
            Self::EmbedDedupEnabled => Some("false"),
            Self::EmbedDedupHammingK => Some("12"),
            Self::FlightRecorder => Some("false"),
            Self::FlightRecorderRetentionDays => Some("7"),
            Self::LexicalIndexHugepages => Some("false"),
            Self::LexicalIndexPinRam => Some("false"),
            Self::PprCacheEntries => Some("4096"),
            Self::QueryPlanCacheEntries => Some("1024"),
            Self::GraphMemoryDegradedBelowPct => Some("80"),
            Self::GraphMemoryGrowthMultiplierBasisPoints => Some("15000"),
            Self::GraphMemoryPerAlgorithmCapMb => Some("100"),
            Self::GraphMemorySnapshotCapMb => Some("250"),
            Self::GraphWitnessesRetentionDays => Some("30"),
            Self::ReadPoolAcquireTimeoutMs => Some("5000"),
            Self::ReadPoolMaxPinSeconds => Some("30"),
            Self::WalCheckpointBytesThreshold => Some("67108864"),
            Self::WorkspaceCloseDrainTimeoutSeconds => Some("5"),
            Self::IndexPublishLockRetryAttempts => Some("200"),
            Self::CurationAutoPromoteConfidenceFloor => Some("0.80"),
            Self::CurationAutoPromoteMaxPerRun => Some("10"),
            Self::CurationDerivedPreviewLimit => Some("20"),
            Self::ReflectionConsumedRetentionDays => Some("30"),
            Self::ReflectionExpiredRetentionDays => Some("7"),
            Self::ReflectionHmacRotationGraceSeconds => Some("86400"),
            Self::ReflectionRequestListLimit => Some("50"),
            Self::ReflectionRequestShowSourceLimit => Some("20"),
            Self::ReflectionRequestTtlSeconds => Some("86400"),
            Self::ReflectionSourceBudgetBytes => Some("65536"),
            Self::RememberCurationSyncBudgetMs => Some("50"),
            _ => None,
        }
    }

    /// Whether capabilities output may include this variable's current value.
    #[must_use]
    pub const fn exposes_value(self) -> bool {
        !matches!(
            self,
            Self::PreflightBypassSecret | Self::ReflectionHmacKeyPath | Self::ServeToken
        )
    }

    /// Broad documentation category for agent docs and env-var catalogs.
    #[must_use]
    pub const fn category(self) -> &'static str {
        match self {
            Self::CassBinary => "integration",
            Self::CurationAutoPromoteConfidenceFloor
            | Self::CurationAutoPromoteMaxPerRun
            | Self::CurationDerivedPreviewLimit
            | Self::RememberCurationSyncBudgetMs => "curation",
            Self::DatabasePath
            | Self::DemoEvidenceRoot
            | Self::E2eRetentionManifest
            | Self::FlightRecorderDir
            | Self::IndexDir
            | Self::L2PackCacheDir
            | Self::ReflectionHmacKeyPath
            | Self::ShardsDir
            | Self::Workspace
            | Self::WorkspaceRegistry => "paths",
            Self::DiagForceCapabilityGap => "diagnostics",
            Self::AgentMode
            | Self::AgentName
            | Self::DisableToon
            | Self::ExperimentalTriad
            | Self::Format
            | Self::HookMode
            | Self::Json
            | Self::LegacySelectionCertificate
            | Self::NoColor
            | Self::OutputFormat => "output",
            Self::FlightRecorder
            | Self::FlightRecorderRetentionDays
            | Self::LogFormat
            | Self::LogJson
            | Self::TestLogLevel
            | Self::TestLogPath
            | Self::TestLogTestId => "diagnostics",
            Self::EmbedDedupCosineFloor
            | Self::EmbedDedupEnabled
            | Self::EmbedDedupHammingK
            | Self::EmbedModelPath => "embeddings",
            Self::MeshEnabled
            | Self::MeshMode
            | Self::MeshDiscoveryCacheTtlSeconds
            | Self::MeshDriftSoftStaleAfter
            | Self::MeshDriftSoftStaleAfterSeconds
            | Self::MeshDriftHardStaleAfter
            | Self::MeshDriftHardStaleAfterSeconds
            | Self::MeshHelloPort
            | Self::MeshHelloResponderDisabled
            | Self::TailscaleBinaryOverride
            | Self::TailscaleProbeTimeoutMs
            | Self::TailscaleProbeSocketOverride
            | Self::TailscaleDiscoveryMode
            | Self::TailscalePeerProbeTimeoutMs
            | Self::TailscaleDiscoveryBudgetMs
            | Self::TailscaleRespondMode => "mesh",
            Self::ReflectionConsumedRetentionDays
            | Self::ReflectionExpiredRetentionDays
            | Self::ReflectionHmacKeyId
            | Self::ReflectionHmacRotationGraceSeconds
            | Self::ReflectionRequestListLimit
            | Self::ReflectionRequestShowSourceLimit
            | Self::ReflectionRequestTtlSeconds
            | Self::ReflectionSourceBudgetBytes => "reflection",
            Self::HarmfulBurstWindowSeconds
            | Self::AuditLaneBatchMax
            | Self::AuditLaneCapacity
            | Self::AuditLaneFlushMs
            | Self::GraphMemoryDegradedBelowPct
            | Self::GraphMemoryGrowthMultiplierBasisPoints
            | Self::GraphMemoryPerAlgorithmCapMb
            | Self::GraphMemorySnapshotCapMb
            | Self::GraphWitnessesRetentionDays
            | Self::HarmfulPerSourcePerHour
            | Self::L2PackCacheBytes
            | Self::L2PackCacheDisable
            | Self::LexicalIndexHugepages
            | Self::LexicalIndexPinRam
            | Self::MaxTokens
            | Self::Profile
            | Self::PprCacheEntries
            | Self::QueryPlanCacheEntries
            | Self::ReadPoolDisablePin
            | Self::ReadPoolAcquireTimeoutMs
            | Self::ReadPoolIdleTimeoutSeconds
            | Self::ReadPoolMaxPinSeconds
            | Self::ReadPoolSize
            | Self::WalCheckpointBytesThreshold
            | Self::WorkspaceCloseDrainTimeoutSeconds
            | Self::DisableRememberSearchNeighbors
            | Self::IndexPublishLockRetryAttempts => "tuning",
            Self::ScienceBackendPath => "integration",
            Self::ShardFanoutEnabled => "storage",
            Self::PreflightBypassSecret
            | Self::SecurityProfile
            | Self::ServeToken
            | Self::WorkspaceHygieneAlwaysReviewPatterns
            | Self::WorkspaceHygieneGeneratedPatterns
            | Self::WorkspaceHygieneLocalMachinePatterns
            | Self::WorkspaceHygieneScratchPatterns => "policy",
        }
    }

    /// Parse this variable through [`FromStr`].
    #[must_use]
    pub fn parse_into<T>(self) -> Option<T>
    where
        T: FromStr,
    {
        read(self).and_then(|value| value.parse::<T>().ok())
    }
}

/// Read an `EE_*` environment variable as UTF-8.
#[must_use]
pub fn read(var: EnvVar) -> Option<String> {
    read_os(var).and_then(|value| value.into_string().ok())
}

/// Read an `EE_*` environment variable as an OS string.
#[must_use]
pub fn read_os(var: EnvVar) -> Option<OsString> {
    let value = std::env::var_os(var.name());
    trace_env_read(var, value.as_ref(), "process_env");
    value
}

/// Read an `EE_*` environment variable or its registry-defined default.
#[must_use]
pub fn read_or_default(var: EnvVar) -> Option<String> {
    if let Some(value) = read(var) {
        return Some(value);
    }

    let default = var.default_value().map(str::to_owned);
    if let Some(value) = default.as_deref() {
        tracing::trace!(
            var_name = var.name(),
            found = true,
            value_hash = %hash_bytes(value.as_bytes()),
            source = "registry_default",
            "ee_env_registry_read"
        );
    }
    default
}

/// Return whether an `EE_*` environment variable is present.
#[must_use]
pub fn is_set(var: EnvVar) -> bool {
    read_os(var).is_some()
}

fn trace_env_read(var: EnvVar, value: Option<&OsString>, source: &'static str) {
    let value_hash = value.map(|value| hash_os_value(value.as_os_str()));
    tracing::trace!(
        var_name = var.name(),
        found = value.is_some(),
        value_hash = value_hash.as_deref().unwrap_or(""),
        source,
        "ee_env_registry_read"
    );
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[cfg(unix)]
fn hash_os_value(value: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;

    hash_bytes(value.as_bytes())
}

#[cfg(not(unix))]
fn hash_os_value(value: &std::ffi::OsStr) -> String {
    hash_bytes(value.to_string_lossy().as_bytes())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::EnvVar;

    type TestResult = Result<(), String>;

    #[test]
    fn every_env_var_has_name_and_description() -> TestResult {
        for var in EnvVar::all() {
            if !var.name().starts_with("EE_") {
                return Err(format!("{} does not start with EE_", var.name()));
            }
            if var.description().trim().is_empty() {
                return Err(format!("{} has an empty description", var.name()));
            }
        }
        Ok(())
    }

    #[test]
    fn env_var_names_are_unique() -> TestResult {
        let mut names = BTreeSet::new();
        for var in EnvVar::all() {
            if !names.insert(var.name()) {
                return Err(format!("duplicate env var registered: {}", var.name()));
            }
        }
        Ok(())
    }

    #[test]
    fn registry_default_is_available() -> TestResult {
        let value = EnvVar::RememberCurationSyncBudgetMs
            .default_value()
            .ok_or_else(|| "remember curation budget default missing".to_owned())?;
        if value == "50" {
            Ok(())
        } else {
            Err(format!(
                "unexpected remember curation budget default: {value}"
            ))
        }
    }

    #[test]
    fn curation_policy_env_vars_are_registered_with_defaults() -> TestResult {
        let expected = [
            (
                EnvVar::CurationDerivedPreviewLimit,
                "EE_CURATION_DERIVED_PREVIEW_LIMIT",
                "20",
            ),
            (
                EnvVar::CurationAutoPromoteMaxPerRun,
                "EE_CURATION_AUTO_PROMOTE_MAX_PER_RUN",
                "10",
            ),
            (
                EnvVar::CurationAutoPromoteConfidenceFloor,
                "EE_CURATION_AUTO_PROMOTE_CONFIDENCE_FLOOR",
                "0.80",
            ),
        ];

        for (var, name, default) in expected {
            if !EnvVar::all().contains(&var) {
                return Err(format!("{name} missing from registry order"));
            }
            if var.name() != name {
                return Err(format!("unexpected env name for {var:?}: {}", var.name()));
            }
            if var.default_value() != Some(default) {
                return Err(format!("{name} default drifted"));
            }
            if var.category() != "curation" {
                return Err(format!("{name} must be categorized as curation"));
            }
        }
        Ok(())
    }

    #[test]
    fn reflection_policy_env_vars_are_registered_with_safe_exposure() -> TestResult {
        let expected = [
            (
                EnvVar::ReflectionSourceBudgetBytes,
                "EE_REFLECTION_SOURCE_BUDGET_BYTES",
                "65536",
            ),
            (
                EnvVar::ReflectionRequestTtlSeconds,
                "EE_REFLECTION_REQUEST_TTL_SECONDS",
                "86400",
            ),
            (
                EnvVar::ReflectionRequestListLimit,
                "EE_REFLECTION_REQUEST_LIST_LIMIT",
                "50",
            ),
            (
                EnvVar::ReflectionRequestShowSourceLimit,
                "EE_REFLECTION_REQUEST_SHOW_SOURCE_LIMIT",
                "20",
            ),
            (
                EnvVar::ReflectionExpiredRetentionDays,
                "EE_REFLECTION_EXPIRED_RETENTION_DAYS",
                "7",
            ),
            (
                EnvVar::ReflectionConsumedRetentionDays,
                "EE_REFLECTION_CONSUMED_RETENTION_DAYS",
                "30",
            ),
            (
                EnvVar::ReflectionHmacRotationGraceSeconds,
                "EE_REFLECTION_HMAC_ROTATION_GRACE_SECONDS",
                "86400",
            ),
        ];

        for (var, name, default) in expected {
            if !EnvVar::all().contains(&var) {
                return Err(format!("{name} missing from registry order"));
            }
            if var.name() != name {
                return Err(format!("unexpected env name for {var:?}: {}", var.name()));
            }
            if var.default_value() != Some(default) {
                return Err(format!("{name} default drifted"));
            }
            if var.category() != "reflection" {
                return Err(format!("{name} must be categorized as reflection"));
            }
            if !var.exposes_value() {
                return Err(format!("{name} should expose non-secret effective values"));
            }
        }

        for var in [EnvVar::ReflectionHmacKeyId, EnvVar::ReflectionHmacKeyPath] {
            if !EnvVar::all().contains(&var) {
                return Err(format!("{} missing from registry order", var.name()));
            }
            if var.default_value().is_some() {
                return Err(format!("{} must not have a baked-in default", var.name()));
            }
        }
        if EnvVar::ReflectionHmacKeyId.category() != "reflection" {
            return Err("EE_REFLECTION_HMAC_KEY_ID must be categorized as reflection".to_owned());
        }
        if EnvVar::ReflectionHmacKeyPath.category() != "paths" {
            return Err("EE_REFLECTION_HMAC_KEY_PATH must be categorized as paths".to_owned());
        }
        if EnvVar::ReflectionHmacKeyPath.exposes_value() {
            return Err("EE_REFLECTION_HMAC_KEY_PATH must not expose currentValue".to_owned());
        }
        Ok(())
    }

    #[test]
    fn shard_fanout_env_vars_are_registered() -> TestResult {
        if !EnvVar::all().contains(&EnvVar::ShardFanoutEnabled) {
            return Err("EE_SHARD_FANOUT_ENABLED missing from registry order".to_owned());
        }
        if !EnvVar::all().contains(&EnvVar::ShardsDir) {
            return Err("EE_SHARDS_DIR missing from registry order".to_owned());
        }
        if EnvVar::ShardFanoutEnabled.default_value() != Some("false") {
            return Err("EE_SHARD_FANOUT_ENABLED must default to false".to_owned());
        }
        if EnvVar::ShardsDir.category() != "paths" {
            return Err("EE_SHARDS_DIR must be categorized as a path override".to_owned());
        }
        Ok(())
    }

    #[test]
    fn embed_dedup_env_vars_are_registered_disabled_by_default() -> TestResult {
        if !EnvVar::all().contains(&EnvVar::EmbedDedupEnabled) {
            return Err("EE_EMBED_DEDUP_ENABLED missing from registry order".to_owned());
        }
        if !EnvVar::all().contains(&EnvVar::EmbedDedupHammingK) {
            return Err("EE_EMBED_DEDUP_HAMMING_K missing from registry order".to_owned());
        }
        if !EnvVar::all().contains(&EnvVar::EmbedDedupCosineFloor) {
            return Err("EE_EMBED_DEDUP_COSINE_FLOOR missing from registry order".to_owned());
        }
        if EnvVar::EmbedDedupEnabled.default_value() != Some("false") {
            return Err("EE_EMBED_DEDUP_ENABLED must default to false".to_owned());
        }
        if EnvVar::EmbedDedupHammingK.default_value() != Some("12") {
            return Err("EE_EMBED_DEDUP_HAMMING_K must default to 12".to_owned());
        }
        if EnvVar::EmbedDedupCosineFloor.default_value() != Some("0.97") {
            return Err("EE_EMBED_DEDUP_COSINE_FLOOR must default to 0.97".to_owned());
        }
        if EnvVar::EmbedDedupEnabled.category() != "embeddings" {
            return Err("embed dedup vars must be categorized as embeddings".to_owned());
        }
        Ok(())
    }

    #[test]
    fn flight_recorder_env_vars_are_registered_disabled_by_default() -> TestResult {
        if !EnvVar::all().contains(&EnvVar::FlightRecorder) {
            return Err("EE_FLIGHT_RECORDER missing from registry order".to_owned());
        }
        if !EnvVar::all().contains(&EnvVar::FlightRecorderDir) {
            return Err("EE_FLIGHT_RECORDER_DIR missing from registry order".to_owned());
        }
        if !EnvVar::all().contains(&EnvVar::FlightRecorderRetentionDays) {
            return Err("EE_FLIGHT_RECORDER_RETENTION_DAYS missing from registry order".to_owned());
        }
        if EnvVar::FlightRecorder.default_value() != Some("false") {
            return Err("EE_FLIGHT_RECORDER must default to false".to_owned());
        }
        if EnvVar::FlightRecorderRetentionDays.default_value() != Some("7") {
            return Err("EE_FLIGHT_RECORDER_RETENTION_DAYS must default to 7".to_owned());
        }
        if EnvVar::FlightRecorder.category() != "diagnostics" {
            return Err("EE_FLIGHT_RECORDER must be categorized as diagnostics".to_owned());
        }
        if EnvVar::FlightRecorderRetentionDays.category() != "diagnostics" {
            return Err(
                "EE_FLIGHT_RECORDER_RETENTION_DAYS must be categorized as diagnostics".to_owned(),
            );
        }
        if EnvVar::FlightRecorderDir.category() != "paths" {
            return Err("EE_FLIGHT_RECORDER_DIR must be categorized as a path override".to_owned());
        }
        Ok(())
    }

    #[test]
    fn mesh_discovery_cache_and_drift_grace_env_vars_are_registered() -> TestResult {
        let expected = [
            (
                EnvVar::MeshDiscoveryCacheTtlSeconds,
                "EE_MESH_DISCOVERY_CACHE_TTL_SECONDS",
                "30",
            ),
            (
                EnvVar::MeshDriftSoftStaleAfter,
                "EE_MESH_DRIFT_SOFT_STALE_AFTER",
                "1",
            ),
            (
                EnvVar::MeshDriftSoftStaleAfterSeconds,
                "EE_MESH_DRIFT_SOFT_STALE_AFTER_SECONDS",
                "300",
            ),
            (
                EnvVar::MeshDriftHardStaleAfter,
                "EE_MESH_DRIFT_HARD_STALE_AFTER",
                "3",
            ),
            (
                EnvVar::MeshDriftHardStaleAfterSeconds,
                "EE_MESH_DRIFT_HARD_STALE_AFTER_SECONDS",
                "3600",
            ),
        ];

        for (var, name, default) in expected {
            if !EnvVar::all().contains(&var) {
                return Err(format!("{name} missing from registry order"));
            }
            if var.name() != name {
                return Err(format!("unexpected env name for {var:?}: {}", var.name()));
            }
            if var.default_value() != Some(default) {
                return Err(format!("{name} default drifted"));
            }
            if var.category() != "mesh" {
                return Err(format!("{name} must be categorized as mesh"));
            }
        }
        Ok(())
    }

    #[test]
    fn sensitive_env_vars_do_not_expose_values() -> TestResult {
        if EnvVar::PreflightBypassSecret.exposes_value() {
            return Err("EE_PREFLIGHT_BYPASS_SECRET must not expose currentValue".to_owned());
        }
        if EnvVar::ServeToken.exposes_value() {
            return Err("EE_SERVE_TOKEN must not expose currentValue".to_owned());
        }
        if EnvVar::ReflectionHmacKeyPath.exposes_value() {
            return Err("EE_REFLECTION_HMAC_KEY_PATH must not expose currentValue".to_owned());
        }
        Ok(())
    }
}
