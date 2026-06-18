//! Central registry for `EE_*` environment variables honored by ee.
//!
//! Adding a new `EE_*` environment variable requires adding a variant here.
//! Tests enforce that production code reads these variables through this
//! registry rather than spelling raw names at call sites.

use std::ffi::OsString;
use std::str::FromStr;

/// Foreign embedding-related environment variables that ee detects but ignores.
///
/// These are analyst traps from other embedding stacks. ee only checks whether
/// they are present so `ee doctor` can disclose that they do not affect the
/// bundled local embedder.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingTrapEnvVar {
    /// `EMBEDDING_MODEL`
    EmbeddingModel,
    /// `OPENAI_API_KEY`
    OpenAiApiKey,
}

impl EmbeddingTrapEnvVar {
    /// Return all detected-but-ignored embedding variables in stable order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::EmbeddingModel, Self::OpenAiApiKey]
    }

    /// Stable environment variable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::EmbeddingModel => "EMBEDDING_MODEL",
            Self::OpenAiApiKey => "OPENAI_API_KEY",
        }
    }

    /// Human-readable explanation for why this variable is only detected.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::EmbeddingModel => {
                "`ee doctor` detects presence only; ee uses the bundled local embedder instead."
            }
            Self::OpenAiApiKey => {
                "`ee doctor` detects presence only; local semantic retrieval never consumes API keys."
            }
        }
    }

    /// Broad documentation category for ignored embedding-config traps.
    #[must_use]
    pub const fn category(self) -> &'static str {
        "embeddings"
    }

    /// Return whether this trap variable is present without exposing its value.
    #[must_use]
    pub fn is_present(self) -> bool {
        std::env::var_os(self.name()).is_some()
    }
}

/// Every `EE_*` environment variable honored by ee.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EnvVar {
    /// `EE_AGENT_NAME`
    AgentName,
    /// `EE_AGENT_MODE`
    AgentMode,
    /// `EE_AMBIENT_CONTEXT`
    AmbientContext,
    /// `EE_AMBIENT_CONTEXT_STATE_DIR`
    AmbientContextStateDir,
    /// `EE_AMBIENT_CONTEXT_VERBOSITY`
    AmbientContextVerbosity,
    /// `EE_ADAPTIVE_BACKOFF_MS`
    AdaptiveBackoffMs,
    /// `EE_ADAPTIVE_NOISY_P99_MS`
    AdaptiveNoisyP99Ms,
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
    /// `EE_DAEMON_ENABLE_ECHO`
    DaemonEnableEcho,
    /// `EE_DAEMON_MAX_INFLIGHT`
    DaemonMaxInflight,
    /// `EE_DATABASE_PATH`
    DatabasePath,
    /// `EE_DEMO_EVIDENCE_ROOT`
    DemoEvidenceRoot,
    /// `EE_DIAG_FORCE_CAPABILITY_GAP`
    DiagForceCapabilityGap,
    /// `EE_DISABLE_ADAPTIVE`
    DisableAdaptive,
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
    /// `EE_GRAPH_NUMA_PIN_DISABLE`
    GraphNumaPinDisable,
    /// `EE_GRAPH_NUMA_PIN_NODE`
    GraphNumaPinNode,
    /// `EE_GRAPH_NUMA_PIN_POPULATE`
    GraphNumaPinPopulate,
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
    /// `EE_MAX_OUTPUT_TOKENS`
    MaxOutputTokens,
    /// `EE_MAX_TOKENS`
    MaxTokens,
    /// `EE_MCP_MAX_REQUEST_BYTES`
    McpMaxRequestBytes,
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
    /// `EE_QUERY_MISS_RETENTION_DAYS`
    QueryMissRetentionDays,
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
            Self::AmbientContext,
            Self::AmbientContextStateDir,
            Self::AmbientContextVerbosity,
            Self::AdaptiveBackoffMs,
            Self::AdaptiveNoisyP99Ms,
            Self::AuditLaneBatchMax,
            Self::AuditLaneCapacity,
            Self::AuditLaneFlushMs,
            Self::CassBinary,
            Self::CurationAutoPromoteConfidenceFloor,
            Self::CurationAutoPromoteMaxPerRun,
            Self::CurationDerivedPreviewLimit,
            Self::DaemonEnableEcho,
            Self::DaemonMaxInflight,
            Self::DatabasePath,
            Self::DemoEvidenceRoot,
            Self::DiagForceCapabilityGap,
            Self::DisableAdaptive,
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
            Self::GraphNumaPinDisable,
            Self::GraphNumaPinNode,
            Self::GraphNumaPinPopulate,
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
            Self::MaxOutputTokens,
            Self::MaxTokens,
            Self::McpMaxRequestBytes,
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
            Self::QueryMissRetentionDays,
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
            Self::AmbientContext => "EE_AMBIENT_CONTEXT",
            Self::AmbientContextStateDir => "EE_AMBIENT_CONTEXT_STATE_DIR",
            Self::AmbientContextVerbosity => "EE_AMBIENT_CONTEXT_VERBOSITY",
            Self::AdaptiveBackoffMs => "EE_ADAPTIVE_BACKOFF_MS",
            Self::AdaptiveNoisyP99Ms => "EE_ADAPTIVE_NOISY_P99_MS",
            Self::AuditLaneBatchMax => "EE_AUDIT_LANE_BATCH_MAX",
            Self::AuditLaneCapacity => "EE_AUDIT_LANE_CAPACITY",
            Self::AuditLaneFlushMs => "EE_AUDIT_LANE_FLUSH_MS",
            Self::CassBinary => "EE_CASS_BINARY",
            Self::CurationAutoPromoteConfidenceFloor => "EE_CURATION_AUTO_PROMOTE_CONFIDENCE_FLOOR",
            Self::CurationAutoPromoteMaxPerRun => "EE_CURATION_AUTO_PROMOTE_MAX_PER_RUN",
            Self::CurationDerivedPreviewLimit => "EE_CURATION_DERIVED_PREVIEW_LIMIT",
            Self::DaemonEnableEcho => "EE_DAEMON_ENABLE_ECHO",
            Self::DaemonMaxInflight => "EE_DAEMON_MAX_INFLIGHT",
            Self::DatabasePath => "EE_DATABASE_PATH",
            Self::DemoEvidenceRoot => "EE_DEMO_EVIDENCE_ROOT",
            Self::DiagForceCapabilityGap => "EE_DIAG_FORCE_CAPABILITY_GAP",
            Self::DisableAdaptive => "EE_DISABLE_ADAPTIVE",
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
            Self::GraphNumaPinDisable => "EE_GRAPH_NUMA_PIN_DISABLE",
            Self::GraphNumaPinNode => "EE_GRAPH_NUMA_PIN_NODE",
            Self::GraphNumaPinPopulate => "EE_GRAPH_NUMA_PIN_POPULATE",
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
            Self::MaxOutputTokens => "EE_MAX_OUTPUT_TOKENS",
            Self::MaxTokens => "EE_MAX_TOKENS",
            Self::McpMaxRequestBytes => "EE_MCP_MAX_REQUEST_BYTES",
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
            Self::QueryMissRetentionDays => "EE_QUERY_MISS_RETENTION_DAYS",
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
            Self::AmbientContext => "Enable or disable proactive ambient context hook injection.",
            Self::AmbientContextStateDir => {
                "Override the ambient hook de-duplication state directory."
            }
            Self::AmbientContextVerbosity => {
                "Select quiet, standard, or verbose ambient hook budgets."
            }
            Self::AdaptiveBackoffMs => {
                "Override the SRR5 noisy-neighbor soft backoff delay in milliseconds."
            }
            Self::AdaptiveNoisyP99Ms => {
                "Override the SRR5 per-agent p99 latency threshold for noisy-neighbor backoff."
            }
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
            Self::DaemonEnableEcho => "Enable the diagnostic `ee.daemon.echo` round-trip method.",
            Self::DaemonMaxInflight => {
                "Override the cap on in-flight `ee daemon` worker threads; saturated accepts emit `daemon_overloaded`."
            }
            Self::DatabasePath => "Override the configured storage database path.",
            Self::DemoEvidenceRoot => "Override the demo evidence storage root.",
            Self::DiagForceCapabilityGap => {
                "Force selected capability probes to report build-gap diagnostics."
            }
            Self::DisableToon => "Disable TOON output capability reporting and auto-selection.",
            Self::DisableAdaptive => {
                "Disable SRR5 swarm adaptive prefetch and scheduling without editing config."
            }
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
            Self::GraphNumaPinDisable => {
                "Disable graph snapshot NUMA pinning without editing config."
            }
            Self::GraphNumaPinNode => {
                "Select auto NUMA placement or an explicit non-negative NUMA node for graph snapshot pinning."
            }
            Self::GraphNumaPinPopulate => {
                "Control whether graph snapshot loading pre-faults pages during NUMA pinning."
            }
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
            Self::MaxOutputTokens => {
                "Cap estimated response tokens for machine output (output governor ceiling)."
            }
            Self::MaxTokens => "Override the default context pack token budget.",
            Self::McpMaxRequestBytes => {
                "Override the MCP stdio JSON-RPC request and response byte cap."
            }
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
            Self::QueryMissRetentionDays => {
                "Override the query-miss ledger retention window used by ee learn gaps."
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
            Self::AmbientContext => Some("true"),
            Self::AmbientContextVerbosity => Some("standard"),
            Self::AdaptiveBackoffMs => Some("25"),
            Self::AdaptiveNoisyP99Ms => Some("200"),
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
            Self::QueryMissRetentionDays => Some("30"),
            Self::GraphMemoryDegradedBelowPct => Some("80"),
            Self::GraphMemoryGrowthMultiplierBasisPoints => Some("15000"),
            Self::GraphMemoryPerAlgorithmCapMb => Some("100"),
            Self::GraphMemorySnapshotCapMb => Some("250"),
            Self::GraphNumaPinDisable => Some("false"),
            Self::GraphNumaPinNode => Some("auto"),
            Self::GraphNumaPinPopulate => Some("true"),
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
            Self::McpMaxRequestBytes => Some("16777216"),
            Self::DaemonEnableEcho => Some("false"),
            Self::DaemonMaxInflight => Some("32"),
            Self::DisableAdaptive => Some("false"),
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
            Self::AmbientContext | Self::AmbientContextStateDir | Self::AmbientContextVerbosity => {
                "hooks"
            }
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
            Self::DaemonEnableEcho | Self::DiagForceCapabilityGap => "diagnostics",
            Self::AgentMode
            | Self::AgentName
            | Self::DisableToon
            | Self::ExperimentalTriad
            | Self::Format
            | Self::HookMode
            | Self::Json
            | Self::LegacySelectionCertificate
            | Self::MaxOutputTokens
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
            | Self::AdaptiveBackoffMs
            | Self::AdaptiveNoisyP99Ms
            | Self::AuditLaneBatchMax
            | Self::AuditLaneCapacity
            | Self::AuditLaneFlushMs
            | Self::DaemonMaxInflight
            | Self::GraphMemoryDegradedBelowPct
            | Self::GraphMemoryGrowthMultiplierBasisPoints
            | Self::GraphMemoryPerAlgorithmCapMb
            | Self::GraphMemorySnapshotCapMb
            | Self::GraphNumaPinDisable
            | Self::GraphNumaPinNode
            | Self::GraphNumaPinPopulate
            | Self::GraphWitnessesRetentionDays
            | Self::HarmfulPerSourcePerHour
            | Self::L2PackCacheBytes
            | Self::L2PackCacheDisable
            | Self::LexicalIndexHugepages
            | Self::LexicalIndexPinRam
            | Self::MaxTokens
            | Self::McpMaxRequestBytes
            | Self::Profile
            | Self::PprCacheEntries
            | Self::QueryPlanCacheEntries
            | Self::QueryMissRetentionDays
            | Self::ReadPoolDisablePin
            | Self::ReadPoolAcquireTimeoutMs
            | Self::ReadPoolIdleTimeoutSeconds
            | Self::ReadPoolMaxPinSeconds
            | Self::ReadPoolSize
            | Self::WalCheckpointBytesThreshold
            | Self::WorkspaceCloseDrainTimeoutSeconds
            | Self::DisableAdaptive
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

    use super::{EmbeddingTrapEnvVar, EnvVar};

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
    fn embedding_trap_env_vars_are_registered_outside_the_runtime_ee_surface() -> TestResult {
        let mut names = BTreeSet::new();
        for var in EmbeddingTrapEnvVar::all() {
            let name = var.name();
            if name.starts_with("EE_") {
                return Err(format!("{name} must not be a runtime EE_* override"));
            }
            if !names.insert(name) {
                return Err(format!(
                    "duplicate embedding trap env var registered: {name}"
                ));
            }
            if var.category() != "embeddings" {
                return Err(format!("{name} must be categorized as embeddings"));
            }
            if var.description().trim().is_empty() {
                return Err(format!("{name} is missing a description"));
            }
        }
        let expected = BTreeSet::from(["EMBEDDING_MODEL", "OPENAI_API_KEY"]);
        if names == expected {
            Ok(())
        } else {
            Err(format!(
                "detected-but-ignored embedding env registry drifted: {names:?}"
            ))
        }
    }

    #[test]
    fn ambient_context_env_vars_are_registered_with_defaults() -> TestResult {
        let expected = [
            (EnvVar::AmbientContext, "EE_AMBIENT_CONTEXT", Some("true")),
            (
                EnvVar::AmbientContextVerbosity,
                "EE_AMBIENT_CONTEXT_VERBOSITY",
                Some("standard"),
            ),
            (
                EnvVar::AmbientContextStateDir,
                "EE_AMBIENT_CONTEXT_STATE_DIR",
                None,
            ),
        ];

        for (var, name, default) in expected {
            if !EnvVar::all().contains(&var) {
                return Err(format!("{name} missing from registry order"));
            }
            if var.name() != name {
                return Err(format!("unexpected env name for {var:?}: {}", var.name()));
            }
            if var.default_value() != default {
                return Err(format!("{name} default drifted"));
            }
            if var.category() != "hooks" {
                return Err(format!("{name} must be categorized as hooks"));
            }
            if !var.exposes_value() {
                return Err(format!("{name} should expose non-secret effective values"));
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
    fn swarm_adaptive_env_vars_are_registered_with_defaults() -> TestResult {
        let expected = [
            (EnvVar::AdaptiveBackoffMs, "EE_ADAPTIVE_BACKOFF_MS", "25"),
            (
                EnvVar::AdaptiveNoisyP99Ms,
                "EE_ADAPTIVE_NOISY_P99_MS",
                "200",
            ),
            (EnvVar::DisableAdaptive, "EE_DISABLE_ADAPTIVE", "false"),
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
            if var.category() != "tuning" {
                return Err(format!("{name} must be categorized as tuning"));
            }
            if !var.exposes_value() {
                return Err(format!("{name} should expose non-secret effective values"));
            }
        }
        Ok(())
    }

    #[test]
    fn graph_numa_pin_env_vars_are_registered_with_defaults() -> TestResult {
        let expected = [
            (
                EnvVar::GraphNumaPinDisable,
                "EE_GRAPH_NUMA_PIN_DISABLE",
                "false",
            ),
            (EnvVar::GraphNumaPinNode, "EE_GRAPH_NUMA_PIN_NODE", "auto"),
            (
                EnvVar::GraphNumaPinPopulate,
                "EE_GRAPH_NUMA_PIN_POPULATE",
                "true",
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
            if var.category() != "tuning" {
                return Err(format!("{name} must be categorized as tuning"));
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
