use std::path::Path;

pub mod env_registry;
pub mod file;
pub mod merge;
pub mod path;
pub mod path_resolver;
pub mod workspace;

pub use env_registry::{EnvVar, is_set as env_var_is_set, read as read_env_var};
pub use env_registry::{read_or_default as read_env_var_or_default, read_os as read_env_var_os};
pub use file::{
    CacheConfig, CassConfig, ConfigFile, ConfigParseError, CurationConfig, FeedbackConfig,
    GraphCausalConfig, GraphConfig, GraphCurateConfig, GraphFeatureFlagsConfig,
    GraphGomoryHuConfig, GraphHealthConfig, GraphHitsConfig, GraphPackDnaConfig, GraphPprConfig,
    HandoffConfig, HandoffStaleThresholdConfig, LearnConfig, LearnDecayConfig,
    MeshBodyFetchPolicyConfig, MeshCommandMode, MeshConfig, MeshLane, MeshLaneDecision,
    MeshLaneGrants, MeshPeerGroupBinding, MeshPeerPolicyConfig, MeshRedactionDecision,
    MeshRedactionPolicyConfig, MeshTrustLane, OutputRedactionConfig, PackConfig, PackL2CacheConfig,
    PolicyConfig, PrivacyConfig, ReadPoolConfig, RuntimeConfig, SearchConfig, SearchSpeed,
    SecretDetectorConfig, StorageConfig, TrustConfig,
};
pub use merge::{
    CACHE_PACK_L2_DIRECTORY_KEY, CACHE_PACK_L2_ENABLED_KEY, CACHE_PACK_L2_MAX_AGE_DAYS_KEY,
    CACHE_PACK_L2_MAX_BYTES_KEY, CASS_BINARY_KEY, CASS_ENABLED_KEY, CASS_SINCE_KEY,
    CURATION_DECAY_HALF_LIFE_DAYS_KEY, CURATION_DUPLICATE_SIMILARITY_KEY,
    CURATION_HARMFUL_WEIGHT_KEY, ConfigLayers, ConfigShowEntry, ConfigShowReport,
    ConfigValueSource, EnvironmentConfigError, FEEDBACK_HARMFUL_BURST_WINDOW_SECONDS_KEY,
    FEEDBACK_HARMFUL_PER_SOURCE_PER_HOUR_KEY, GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY,
    GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY, GRAPH_CURATE_ONION_DECAY_MAX_KEY,
    GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY, GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY,
    GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY, GRAPH_FEATURE_PACK_DNA_ENABLED_KEY,
    GRAPH_FEATURE_PPR_ENABLED_KEY, GRAPH_FEATURE_PROXIMITY_ENABLED_KEY,
    GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY, GRAPH_FEATURE_SKYLINE_ENABLED_KEY,
    GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY, GRAPH_FEATURE_STRUCTURAL_HEALTH_ENABLED_KEY,
    GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY, GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY,
    GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY, GRAPH_HITS_PROFILE_BOOST_KEY,
    GRAPH_PACK_DNA_MAX_EDGES_KEY, GRAPH_PACK_DNA_MAX_ITEMS_KEY, GRAPH_PPR_ALPHA_KEY,
    LEARN_CLUSTER_COHERENCE_THRESHOLD_KEY, LEARN_DECAY_DEFAULT_HALF_LIFE_DAYS_KEY,
    LEARN_DECAY_DEMOTE_THRESHOLD_KEY, LEARN_DECAY_EPISODIC_EVENT_HALF_LIFE_DAYS_KEY,
    LEARN_DECAY_EPISODIC_FAILURE_HALF_LIFE_DAYS_KEY, LEARN_DECAY_FORGET_THRESHOLD_KEY,
    LEARN_DECAY_PROCEDURAL_RULE_HALF_LIFE_DAYS_KEY, LEARN_DECAY_SEMANTIC_FACT_HALF_LIFE_DAYS_KEY,
    LEARN_DECAY_WORKING_HALF_LIFE_DAYS_KEY, MESH_COMMAND_MODE_KEY, MESH_ENABLED_KEY,
    MESH_PEER_GROUP_BINDINGS_KEY, MESH_PEER_POLICIES_KEY, MergedConfig, PACK_CANDIDATE_POOL_KEY,
    PACK_DEFAULT_FORMAT_KEY, PACK_DEFAULT_MAX_TOKENS_KEY, PACK_DEFAULT_PROFILE_KEY,
    PACK_MMR_LAMBDA_KEY, POLICY_OUTPUT_REDACTION_ENABLED_KEY,
    POLICY_SECRET_DETECTOR_ALLOW_PHRASES_KEY, POLICY_SECRET_DETECTOR_ALLOW_REGEX_KEY,
    PRIVACY_REDACT_SECRETS_KEY, PRIVACY_REDACTION_CLASSES_KEY, RUNTIME_DAEMON_KEY,
    RUNTIME_IMPORT_BATCH_SIZE_KEY, RUNTIME_JOB_BUDGET_MS_KEY, SEARCH_DEFAULT_SPEED_KEY,
    SEARCH_GRAPH_WEIGHT_KEY, SEARCH_LEXICAL_WEIGHT_KEY, SEARCH_SEMANTIC_WEIGHT_KEY,
    STORAGE_DATABASE_PATH_KEY, STORAGE_INDEX_DIR_KEY, STORAGE_JSONL_EXPORT_KEY,
    STORAGE_READ_POOL_IDLE_TIMEOUT_SECONDS_KEY, STORAGE_READ_POOL_PIN_SNAPSHOT_KEY,
    STORAGE_READ_POOL_SIZE_KEY, TRUST_DEFAULT_CLASS_KEY, TRUST_PROMPT_INJECTION_GUARD_KEY,
    TRUST_TEAM_MEMBERS_KEY, built_in_config, config_from_env, merge_config,
};
pub use path::{PathExpander, PathExpansionError};
pub use path_resolver::{
    PlatformDataDirError, WINDOWS_APPDATA_UNAVAILABLE_CODE, resolve_dir_unix_xdg,
    resolve_dir_windows_appdata, resolve_dir_windows_localappdata,
};
pub use workspace::{
    WORKSPACE_ENV_VAR, WORKSPACE_MARKER, WorkspaceDiagnostic, WorkspaceDiagnosticSeverity,
    WorkspaceError, WorkspaceLocation, WorkspaceResolution, WorkspaceResolutionMode,
    WorkspaceResolutionRequest, WorkspaceResolutionSource, WorkspaceScope, WorkspaceScopeKind,
    derive_workspace_scope, diagnose_workspace_resolution, discover, discover_all,
    discover_from_current_dir, resolve_workspace, workspace_fingerprint,
    workspace_scope_from_repository_root,
};

pub const SUBSYSTEM: &str = "config";

#[cfg(test)]
fn trace_minhash_rank_centrality_config(
    phase: &'static str,
    elapsed_ms: u64,
    degraded_codes: &[&str],
) {
    tracing::info!(
        workspace_id = "config",
        request_id = "minhash_rank_config",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.46"),
        surface = "minhash_rank_centrality",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "minhash rank centrality config checkpoint"
    );
}

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[must_use]
pub fn workspace_output_redaction_enabled(workspace_path: &Path) -> bool {
    let config_path = workspace_path.join(".ee").join("config.toml");
    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return true;
    };
    ConfigFile::parse(&contents)
        .ok()
        .and_then(|config| config.policy.output_redaction.enabled)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::{subsystem_name, trace_minhash_rank_centrality_config};

    type TestResult = Result<(), String>;

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn subsystem_name_is_stable() -> TestResult {
        trace_minhash_rank_centrality_config("input", 0, &[]);
        trace_minhash_rank_centrality_config("response", 0, &[]);
        ensure_equal(&subsystem_name(), &"config", "config subsystem name")
    }
}
