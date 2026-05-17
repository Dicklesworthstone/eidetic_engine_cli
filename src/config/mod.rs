use std::io::Read;
use std::path::{Path, PathBuf};

use crate::models::RedactionLevel;

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
    PolicyConfig, PrivacyConfig, ReadPoolConfig, RedactionConfig, RedactionDefaultsConfig,
    RuntimeConfig, SearchConfig, SearchSpeed, SecretDetectorConfig, StorageConfig, TrustConfig,
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
    STORAGE_READ_POOL_IDLE_TIMEOUT_SECONDS_KEY, STORAGE_READ_POOL_MAX_PIN_DURATION_SECONDS_KEY,
    STORAGE_READ_POOL_PIN_SNAPSHOT_KEY, STORAGE_READ_POOL_SIZE_KEY, TRUST_DEFAULT_CLASS_KEY,
    TRUST_PROMPT_INJECTION_GUARD_KEY, TRUST_TEAM_MEMBERS_KEY, built_in_config, config_from_env,
    merge_config,
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
    let Some(contents) = workspace_config_contents(workspace_path) else {
        return true;
    };
    ConfigFile::parse(&contents)
        .ok()
        .and_then(|config| config.policy.output_redaction.enabled)
        .unwrap_or(true)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedactionDefaultSurface {
    Export,
    HandoffCreate,
    ContextJson,
    SupportBundle,
}

#[must_use]
pub fn workspace_redaction_default(
    workspace_path: &Path,
    surface: RedactionDefaultSurface,
    built_in: RedactionLevel,
) -> RedactionLevel {
    configured_workspace_redaction_default(workspace_path, surface).unwrap_or(built_in)
}

pub(crate) fn configured_workspace_redaction_default(
    workspace_path: &Path,
    surface: RedactionDefaultSurface,
) -> Option<RedactionLevel> {
    let Some(contents) = workspace_config_contents(workspace_path) else {
        return None;
    };
    let Ok(config) = ConfigFile::parse(&contents) else {
        return None;
    };
    match surface {
        RedactionDefaultSurface::Export => config.redaction.defaults.export,
        RedactionDefaultSurface::HandoffCreate => config.redaction.defaults.handoff_create,
        RedactionDefaultSurface::ContextJson => config.redaction.defaults.context_json,
        RedactionDefaultSurface::SupportBundle => config.redaction.defaults.support_bundle,
    }
}

fn workspace_config_contents(workspace_path: &Path) -> Option<String> {
    read_workspace_config_contents(workspace_path)
        .ok()
        .flatten()
}

pub(crate) fn read_workspace_config_contents(
    workspace_path: &Path,
) -> std::io::Result<Option<String>> {
    let config_path = workspace_path.join(".ee").join("config.toml");
    if first_existing_config_symlink_component(&config_path)?.is_some() {
        return Ok(None);
    }

    let metadata = match std::fs::symlink_metadata(&config_path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    read_config_file_no_follow(&config_path).map(Some)
}

fn read_config_file_no_follow(path: &Path) -> std::io::Result<String> {
    let mut file = open_workspace_config_file_for_read(path)?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

fn open_workspace_config_file_for_read(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    configure_workspace_config_open_no_follow(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_workspace_config_open_no_follow(options: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_workspace_config_open_no_follow(_options: &mut std::fs::OpenOptions) {}

fn first_existing_config_symlink_component(path: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir | std::path::Component::Normal(_) => {
                current.push(component.as_os_str());
            }
        }

        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{
        RedactionDefaultSurface, subsystem_name, trace_minhash_rank_centrality_config,
        workspace_output_redaction_enabled, workspace_redaction_default,
    };
    use crate::models::RedactionLevel;

    type TestResult = Result<(), String>;

    fn unique_temp_path(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("ee-{label}-{}-{nanos}", std::process::id()))
    }

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

    #[test]
    fn workspace_redaction_default_reads_surface_config() -> TestResult {
        let workspace =
            std::env::temp_dir().join(format!("ee-redaction-defaults-{}", std::process::id()));
        fs::create_dir_all(workspace.join(".ee"))
            .map_err(|error| format!("create config dir: {error}"))?;
        fs::write(
            workspace.join(".ee").join("config.toml"),
            r#"
[redaction.defaults]
export = "strict"
context_json = "minimal"
support_bundle = "paranoid"
"#,
        )
        .map_err(|error| format!("write config: {error}"))?;

        ensure_equal(
            &workspace_redaction_default(
                &workspace,
                RedactionDefaultSurface::Export,
                RedactionLevel::Standard,
            ),
            &RedactionLevel::Strict,
            "export default",
        )?;
        ensure_equal(
            &workspace_redaction_default(
                &workspace,
                RedactionDefaultSurface::HandoffCreate,
                RedactionLevel::Standard,
            ),
            &RedactionLevel::Standard,
            "handoff default fallback",
        )?;
        ensure_equal(
            &workspace_redaction_default(
                &PathBuf::from("/definitely/missing/ee/workspace"),
                RedactionDefaultSurface::SupportBundle,
                RedactionLevel::Paranoid,
            ),
            &RedactionLevel::Paranoid,
            "missing config fallback",
        )
    }

    #[cfg(unix)]
    #[test]
    fn workspace_redaction_config_refuses_symlinked_config_file() -> TestResult {
        let workspace = unique_temp_path("redaction-symlink-config");
        let outside = unique_temp_path("redaction-symlink-outside");
        fs::create_dir_all(workspace.join(".ee"))
            .map_err(|error| format!("create workspace config dir: {error}"))?;
        fs::create_dir_all(&outside)
            .map_err(|error| format!("create outside config dir: {error}"))?;
        fs::write(
            outside.join("config.toml"),
            r#"
[policy.output_redaction]
enabled = false

[redaction.defaults]
export = "strict"
"#,
        )
        .map_err(|error| format!("write outside config: {error}"))?;
        std::os::unix::fs::symlink(
            outside.join("config.toml"),
            workspace.join(".ee").join("config.toml"),
        )
        .map_err(|error| format!("create config symlink: {error}"))?;

        ensure_equal(
            &workspace_output_redaction_enabled(&workspace),
            &true,
            "symlinked output redaction config falls back to enabled",
        )?;
        ensure_equal(
            &workspace_redaction_default(
                &workspace,
                RedactionDefaultSurface::Export,
                RedactionLevel::Standard,
            ),
            &RedactionLevel::Standard,
            "symlinked redaction default falls back to built-in",
        )
    }

    #[cfg(unix)]
    #[test]
    fn workspace_config_final_read_open_rejects_swapped_symlink_file() -> TestResult {
        let workspace = unique_temp_path("redaction-config-final-open");
        let outside = unique_temp_path("redaction-config-final-open-outside");
        let config_path = workspace.join(".ee").join("config.toml");
        let preserved_config = workspace.join(".ee").join("config.toml.preserved");
        let outside_config = outside.join("config.toml");
        let config_body = "[policy.output_redaction]\nenabled = true\n";
        fs::create_dir_all(workspace.join(".ee"))
            .map_err(|error| format!("create workspace config dir: {error}"))?;
        fs::create_dir_all(&outside)
            .map_err(|error| format!("create outside config dir: {error}"))?;
        fs::write(&config_path, config_body)
            .map_err(|error| format!("write workspace config: {error}"))?;
        ensure_equal(
            &first_existing_config_symlink_component(&config_path)
                .map_err(|error| error.to_string())?
                .is_none(),
            &true,
            "workspace config has no symlink components before swap",
        )?;
        ensure_equal(
            &fs::symlink_metadata(&config_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_file(),
            &true,
            "workspace config is regular before swap",
        )?;

        fs::rename(&config_path, &preserved_config)
            .map_err(|error| format!("preserve workspace config: {error}"))?;
        fs::write(
            &outside_config,
            "[policy.output_redaction]\nenabled = false\n",
        )
        .map_err(|error| format!("write outside config: {error}"))?;
        std::os::unix::fs::symlink(&outside_config, &config_path)
            .map_err(|error| format!("create swapped config symlink: {error}"))?;

        open_workspace_config_file_for_read(&config_path)
            .expect_err("final workspace config open should reject symlink after validation");
        ensure_equal(
            &fs::read_to_string(&outside_config).map_err(|error| error.to_string())?,
            &"[policy.output_redaction]\nenabled = false\n".to_owned(),
            "outside config remains unchanged",
        )?;
        ensure_equal(
            &fs::symlink_metadata(&config_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            &true,
            "rejected workspace config symlink remains for inspection",
        )?;
        ensure_equal(
            &fs::read_to_string(&preserved_config).map_err(|error| error.to_string())?,
            &config_body.to_owned(),
            "preserved validated config remains available",
        )
    }

    #[cfg(unix)]
    #[test]
    fn workspace_redaction_config_refuses_symlinked_config_parent() -> TestResult {
        let workspace = unique_temp_path("redaction-symlink-parent");
        let outside = unique_temp_path("redaction-symlink-parent-outside");
        fs::create_dir_all(&workspace).map_err(|error| format!("create workspace dir: {error}"))?;
        fs::create_dir_all(&outside)
            .map_err(|error| format!("create outside config dir: {error}"))?;
        fs::write(
            outside.join("config.toml"),
            "[policy.output_redaction]\nenabled = false\n",
        )
        .map_err(|error| format!("write outside config: {error}"))?;
        std::os::unix::fs::symlink(&outside, workspace.join(".ee"))
            .map_err(|error| format!("create config parent symlink: {error}"))?;

        ensure_equal(
            &workspace_output_redaction_enabled(&workspace),
            &true,
            "symlinked config parent falls back to enabled",
        )
    }
}
