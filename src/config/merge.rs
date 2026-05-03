//! Config precedence merging (EE-022).
//!
//! This module combines the optional values parsed from config files,
//! environment variables, and CLI-derived overrides into one deterministic
//! config view. It does not perform filesystem discovery or write config
//! files; callers feed it already-parsed layers in the documented order:
//! CLI > environment > project config > user config > built-in defaults.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use super::file::{
    CassConfig, ConfigFile, CurationConfig, FeedbackConfig, PackConfig, PrivacyConfig,
    RuntimeConfig, SearchConfig, SearchSpeed, StorageConfig, TrustConfig,
};
use super::path::{PathExpander, PathExpansionError};

pub const STORAGE_DATABASE_PATH_KEY: &str = "storage.database_path";
pub const STORAGE_INDEX_DIR_KEY: &str = "storage.index_dir";
pub const STORAGE_JSONL_EXPORT_KEY: &str = "storage.jsonl_export";
pub const RUNTIME_DAEMON_KEY: &str = "runtime.daemon";
pub const RUNTIME_JOB_BUDGET_MS_KEY: &str = "runtime.job_budget_ms";
pub const RUNTIME_IMPORT_BATCH_SIZE_KEY: &str = "runtime.import_batch_size";
pub const CASS_ENABLED_KEY: &str = "cass.enabled";
pub const CASS_BINARY_KEY: &str = "cass.binary";
pub const CASS_SINCE_KEY: &str = "cass.since";
pub const SEARCH_DEFAULT_SPEED_KEY: &str = "search.default_speed";
pub const SEARCH_LEXICAL_WEIGHT_KEY: &str = "search.lexical_weight";
pub const SEARCH_SEMANTIC_WEIGHT_KEY: &str = "search.semantic_weight";
pub const SEARCH_GRAPH_WEIGHT_KEY: &str = "search.graph_weight";
pub const PACK_DEFAULT_PROFILE_KEY: &str = "pack.default_profile";
pub const PACK_DEFAULT_FORMAT_KEY: &str = "pack.default_format";
pub const PACK_DEFAULT_MAX_TOKENS_KEY: &str = "pack.default_max_tokens";
pub const PACK_MMR_LAMBDA_KEY: &str = "pack.mmr_lambda";
pub const PACK_CANDIDATE_POOL_KEY: &str = "pack.candidate_pool";
pub const CURATION_DUPLICATE_SIMILARITY_KEY: &str = "curation.duplicate_similarity";
pub const CURATION_HARMFUL_WEIGHT_KEY: &str = "curation.harmful_weight";
pub const CURATION_DECAY_HALF_LIFE_DAYS_KEY: &str = "curation.decay_half_life_days";
pub const CURATION_SPECIFICITY_MIN_KEY: &str = "curation.specificity_min";
pub const FEEDBACK_HARMFUL_PER_SOURCE_PER_HOUR_KEY: &str = "feedback.harmful_per_source_per_hour";
pub const FEEDBACK_HARMFUL_BURST_WINDOW_SECONDS_KEY: &str = "feedback.harmful_burst_window_seconds";
pub const PRIVACY_REDACT_SECRETS_KEY: &str = "privacy.redact_secrets";
pub const PRIVACY_REDACTION_CLASSES_KEY: &str = "privacy.redaction_classes";
pub const TRUST_DEFAULT_CLASS_KEY: &str = "trust.default_class";
pub const TRUST_PROMPT_INJECTION_GUARD_KEY: &str = "trust.prompt_injection_guard";

const BUILT_IN_DATABASE_PATH: &str = "~/.local/share/ee/ee.db";
const BUILT_IN_INDEX_DIR: &str = "~/.local/share/ee/indexes";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ConfigValueSource {
    Cli,
    Environment,
    Project,
    User,
    Default,
}

impl ConfigValueSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Environment => "environment",
            Self::Project => "project",
            Self::User => "user",
            Self::Default => "default",
        }
    }
}

/// Parsed config layers in precedence order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigLayers {
    pub cli: ConfigFile,
    pub environment: ConfigFile,
    pub project: ConfigFile,
    pub user: ConfigFile,
    pub defaults: ConfigFile,
}

impl ConfigLayers {
    #[must_use]
    pub fn with_defaults(defaults: ConfigFile) -> Self {
        Self {
            defaults,
            ..Self::default()
        }
    }
}

/// A merged config plus source metadata for each value that resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct MergedConfig {
    pub values: ConfigFile,
    sources: BTreeMap<&'static str, ConfigValueSource>,
}

impl MergedConfig {
    #[must_use]
    pub fn source(&self, key: &str) -> Option<ConfigValueSource> {
        self.sources.get(key).copied()
    }

    #[must_use]
    pub const fn sources(&self) -> &BTreeMap<&'static str, ConfigValueSource> {
        &self.sources
    }
}

/// Build the documented default config.
///
/// # Errors
///
/// Returns [`EnvironmentConfigError::PathExpansion`] when the default storage
/// paths cannot be expanded with the supplied expander.
pub fn built_in_config(expander: &PathExpander) -> Result<ConfigFile, EnvironmentConfigError> {
    Ok(ConfigFile {
        storage: StorageConfig {
            database_path: Some(expand_env_path(
                "EE_BUILT_IN_DATABASE_PATH",
                BUILT_IN_DATABASE_PATH,
                expander,
            )?),
            index_dir: Some(expand_env_path(
                "EE_BUILT_IN_INDEX_DIR",
                BUILT_IN_INDEX_DIR,
                expander,
            )?),
            jsonl_export: Some(false),
        },
        runtime: RuntimeConfig {
            daemon: Some(false),
            job_budget_ms: Some(5000),
            import_batch_size: Some(200),
        },
        cass: CassConfig {
            enabled: Some(true),
            binary: Some("cass".to_string()),
            since: Some("90d".to_string()),
        },
        search: SearchConfig {
            default_speed: Some(SearchSpeed::Balanced),
            lexical_weight: Some(0.45),
            semantic_weight: Some(0.45),
            graph_weight: Some(0.10),
        },
        pack: PackConfig {
            default_profile: Some("default".to_string()),
            default_format: Some("markdown".to_string()),
            default_max_tokens: Some(4000),
            mmr_lambda: Some(0.7),
            candidate_pool: Some(100),
        },
        curation: CurationConfig {
            duplicate_similarity: Some(0.92),
            harmful_weight: Some(2.5),
            decay_half_life_days: Some(60),
            specificity_min: Some(0.45),
        },
        feedback: FeedbackConfig {
            harmful_per_source_per_hour: Some(5),
            harmful_burst_window_seconds: Some(3600),
        },
        privacy: PrivacyConfig {
            redact_secrets: Some(true),
            redaction_classes: Some(vec![
                "api_key".to_string(),
                "jwt".to_string(),
                "password".to_string(),
                "private_key".to_string(),
                "ssh_key".to_string(),
            ]),
        },
        trust: TrustConfig {
            default_class: Some("agent_assertion".to_string()),
            prompt_injection_guard: Some(true),
        },
    })
}

/// Parse supported `EE_*` environment variables into a config layer.
///
/// # Errors
///
/// Returns [`EnvironmentConfigError`] when an override cannot be decoded,
/// parsed, or path-expanded.
pub fn config_from_env(
    env: &BTreeMap<String, OsString>,
    expander: &PathExpander,
) -> Result<ConfigFile, EnvironmentConfigError> {
    Ok(ConfigFile {
        storage: StorageConfig {
            database_path: optional_env_path(env, "EE_DATABASE_PATH", expander)?,
            index_dir: optional_env_path(env, "EE_INDEX_DIR", expander)?,
            jsonl_export: None,
        },
        runtime: RuntimeConfig::default(),
        cass: CassConfig::default(),
        search: SearchConfig::default(),
        pack: PackConfig {
            default_profile: optional_env_string(env, "EE_PROFILE")?,
            default_format: None,
            default_max_tokens: optional_env_u64(env, "EE_MAX_TOKENS")?,
            mmr_lambda: None,
            candidate_pool: None,
        },
        curation: CurationConfig::default(),
        feedback: FeedbackConfig {
            harmful_per_source_per_hour: optional_env_u64(env, "EE_HARMFUL_PER_SOURCE_PER_HOUR")?,
            harmful_burst_window_seconds: optional_env_u64(env, "EE_HARMFUL_BURST_WINDOW_SECONDS")?,
        },
        privacy: PrivacyConfig::default(),
        trust: TrustConfig::default(),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentConfigError {
    InvalidUnicode {
        variable: &'static str,
    },
    InvalidUnsignedInteger {
        variable: &'static str,
        value: String,
    },
    PathExpansion {
        variable: &'static str,
        source: PathExpansionError,
    },
}

impl fmt::Display for EnvironmentConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUnicode { variable } => {
                write!(
                    formatter,
                    "environment variable `{variable}` is not valid UTF-8"
                )
            }
            Self::InvalidUnsignedInteger { variable, value } => write!(
                formatter,
                "environment variable `{variable}` must be a non-negative integer, got `{value}`"
            ),
            Self::PathExpansion { variable, source } => {
                write!(formatter, "failed to expand `{variable}`: {source}")
            }
        }
    }
}

impl std::error::Error for EnvironmentConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PathExpansion { source, .. } => Some(source),
            Self::InvalidUnicode { .. } | Self::InvalidUnsignedInteger { .. } => None,
        }
    }
}

#[must_use]
pub fn merge_config(layers: &ConfigLayers) -> MergedConfig {
    let mut sources = BTreeMap::new();
    let values = ConfigFile {
        storage: StorageConfig {
            database_path: pick_field(
                &mut sources,
                STORAGE_DATABASE_PATH_KEY,
                &layers.cli.storage.database_path,
                &layers.environment.storage.database_path,
                &layers.project.storage.database_path,
                &layers.user.storage.database_path,
                &layers.defaults.storage.database_path,
            ),
            index_dir: pick_field(
                &mut sources,
                STORAGE_INDEX_DIR_KEY,
                &layers.cli.storage.index_dir,
                &layers.environment.storage.index_dir,
                &layers.project.storage.index_dir,
                &layers.user.storage.index_dir,
                &layers.defaults.storage.index_dir,
            ),
            jsonl_export: pick_field(
                &mut sources,
                STORAGE_JSONL_EXPORT_KEY,
                &layers.cli.storage.jsonl_export,
                &layers.environment.storage.jsonl_export,
                &layers.project.storage.jsonl_export,
                &layers.user.storage.jsonl_export,
                &layers.defaults.storage.jsonl_export,
            ),
        },
        runtime: RuntimeConfig {
            daemon: pick_field(
                &mut sources,
                RUNTIME_DAEMON_KEY,
                &layers.cli.runtime.daemon,
                &layers.environment.runtime.daemon,
                &layers.project.runtime.daemon,
                &layers.user.runtime.daemon,
                &layers.defaults.runtime.daemon,
            ),
            job_budget_ms: pick_field(
                &mut sources,
                RUNTIME_JOB_BUDGET_MS_KEY,
                &layers.cli.runtime.job_budget_ms,
                &layers.environment.runtime.job_budget_ms,
                &layers.project.runtime.job_budget_ms,
                &layers.user.runtime.job_budget_ms,
                &layers.defaults.runtime.job_budget_ms,
            ),
            import_batch_size: pick_field(
                &mut sources,
                RUNTIME_IMPORT_BATCH_SIZE_KEY,
                &layers.cli.runtime.import_batch_size,
                &layers.environment.runtime.import_batch_size,
                &layers.project.runtime.import_batch_size,
                &layers.user.runtime.import_batch_size,
                &layers.defaults.runtime.import_batch_size,
            ),
        },
        cass: CassConfig {
            enabled: pick_field(
                &mut sources,
                CASS_ENABLED_KEY,
                &layers.cli.cass.enabled,
                &layers.environment.cass.enabled,
                &layers.project.cass.enabled,
                &layers.user.cass.enabled,
                &layers.defaults.cass.enabled,
            ),
            binary: pick_field(
                &mut sources,
                CASS_BINARY_KEY,
                &layers.cli.cass.binary,
                &layers.environment.cass.binary,
                &layers.project.cass.binary,
                &layers.user.cass.binary,
                &layers.defaults.cass.binary,
            ),
            since: pick_field(
                &mut sources,
                CASS_SINCE_KEY,
                &layers.cli.cass.since,
                &layers.environment.cass.since,
                &layers.project.cass.since,
                &layers.user.cass.since,
                &layers.defaults.cass.since,
            ),
        },
        search: SearchConfig {
            default_speed: pick_field(
                &mut sources,
                SEARCH_DEFAULT_SPEED_KEY,
                &layers.cli.search.default_speed,
                &layers.environment.search.default_speed,
                &layers.project.search.default_speed,
                &layers.user.search.default_speed,
                &layers.defaults.search.default_speed,
            ),
            lexical_weight: pick_field(
                &mut sources,
                SEARCH_LEXICAL_WEIGHT_KEY,
                &layers.cli.search.lexical_weight,
                &layers.environment.search.lexical_weight,
                &layers.project.search.lexical_weight,
                &layers.user.search.lexical_weight,
                &layers.defaults.search.lexical_weight,
            ),
            semantic_weight: pick_field(
                &mut sources,
                SEARCH_SEMANTIC_WEIGHT_KEY,
                &layers.cli.search.semantic_weight,
                &layers.environment.search.semantic_weight,
                &layers.project.search.semantic_weight,
                &layers.user.search.semantic_weight,
                &layers.defaults.search.semantic_weight,
            ),
            graph_weight: pick_field(
                &mut sources,
                SEARCH_GRAPH_WEIGHT_KEY,
                &layers.cli.search.graph_weight,
                &layers.environment.search.graph_weight,
                &layers.project.search.graph_weight,
                &layers.user.search.graph_weight,
                &layers.defaults.search.graph_weight,
            ),
        },
        pack: PackConfig {
            default_profile: pick_field(
                &mut sources,
                PACK_DEFAULT_PROFILE_KEY,
                &layers.cli.pack.default_profile,
                &layers.environment.pack.default_profile,
                &layers.project.pack.default_profile,
                &layers.user.pack.default_profile,
                &layers.defaults.pack.default_profile,
            ),
            default_format: pick_field(
                &mut sources,
                PACK_DEFAULT_FORMAT_KEY,
                &layers.cli.pack.default_format,
                &layers.environment.pack.default_format,
                &layers.project.pack.default_format,
                &layers.user.pack.default_format,
                &layers.defaults.pack.default_format,
            ),
            default_max_tokens: pick_field(
                &mut sources,
                PACK_DEFAULT_MAX_TOKENS_KEY,
                &layers.cli.pack.default_max_tokens,
                &layers.environment.pack.default_max_tokens,
                &layers.project.pack.default_max_tokens,
                &layers.user.pack.default_max_tokens,
                &layers.defaults.pack.default_max_tokens,
            ),
            mmr_lambda: pick_field(
                &mut sources,
                PACK_MMR_LAMBDA_KEY,
                &layers.cli.pack.mmr_lambda,
                &layers.environment.pack.mmr_lambda,
                &layers.project.pack.mmr_lambda,
                &layers.user.pack.mmr_lambda,
                &layers.defaults.pack.mmr_lambda,
            ),
            candidate_pool: pick_field(
                &mut sources,
                PACK_CANDIDATE_POOL_KEY,
                &layers.cli.pack.candidate_pool,
                &layers.environment.pack.candidate_pool,
                &layers.project.pack.candidate_pool,
                &layers.user.pack.candidate_pool,
                &layers.defaults.pack.candidate_pool,
            ),
        },
        curation: CurationConfig {
            duplicate_similarity: pick_field(
                &mut sources,
                CURATION_DUPLICATE_SIMILARITY_KEY,
                &layers.cli.curation.duplicate_similarity,
                &layers.environment.curation.duplicate_similarity,
                &layers.project.curation.duplicate_similarity,
                &layers.user.curation.duplicate_similarity,
                &layers.defaults.curation.duplicate_similarity,
            ),
            harmful_weight: pick_field(
                &mut sources,
                CURATION_HARMFUL_WEIGHT_KEY,
                &layers.cli.curation.harmful_weight,
                &layers.environment.curation.harmful_weight,
                &layers.project.curation.harmful_weight,
                &layers.user.curation.harmful_weight,
                &layers.defaults.curation.harmful_weight,
            ),
            decay_half_life_days: pick_field(
                &mut sources,
                CURATION_DECAY_HALF_LIFE_DAYS_KEY,
                &layers.cli.curation.decay_half_life_days,
                &layers.environment.curation.decay_half_life_days,
                &layers.project.curation.decay_half_life_days,
                &layers.user.curation.decay_half_life_days,
                &layers.defaults.curation.decay_half_life_days,
            ),
            specificity_min: pick_field(
                &mut sources,
                CURATION_SPECIFICITY_MIN_KEY,
                &layers.cli.curation.specificity_min,
                &layers.environment.curation.specificity_min,
                &layers.project.curation.specificity_min,
                &layers.user.curation.specificity_min,
                &layers.defaults.curation.specificity_min,
            ),
        },
        feedback: FeedbackConfig {
            harmful_per_source_per_hour: pick_field(
                &mut sources,
                FEEDBACK_HARMFUL_PER_SOURCE_PER_HOUR_KEY,
                &layers.cli.feedback.harmful_per_source_per_hour,
                &layers.environment.feedback.harmful_per_source_per_hour,
                &layers.project.feedback.harmful_per_source_per_hour,
                &layers.user.feedback.harmful_per_source_per_hour,
                &layers.defaults.feedback.harmful_per_source_per_hour,
            ),
            harmful_burst_window_seconds: pick_field(
                &mut sources,
                FEEDBACK_HARMFUL_BURST_WINDOW_SECONDS_KEY,
                &layers.cli.feedback.harmful_burst_window_seconds,
                &layers.environment.feedback.harmful_burst_window_seconds,
                &layers.project.feedback.harmful_burst_window_seconds,
                &layers.user.feedback.harmful_burst_window_seconds,
                &layers.defaults.feedback.harmful_burst_window_seconds,
            ),
        },
        privacy: PrivacyConfig {
            redact_secrets: pick_field(
                &mut sources,
                PRIVACY_REDACT_SECRETS_KEY,
                &layers.cli.privacy.redact_secrets,
                &layers.environment.privacy.redact_secrets,
                &layers.project.privacy.redact_secrets,
                &layers.user.privacy.redact_secrets,
                &layers.defaults.privacy.redact_secrets,
            ),
            redaction_classes: pick_field(
                &mut sources,
                PRIVACY_REDACTION_CLASSES_KEY,
                &layers.cli.privacy.redaction_classes,
                &layers.environment.privacy.redaction_classes,
                &layers.project.privacy.redaction_classes,
                &layers.user.privacy.redaction_classes,
                &layers.defaults.privacy.redaction_classes,
            ),
        },
        trust: TrustConfig {
            default_class: pick_field(
                &mut sources,
                TRUST_DEFAULT_CLASS_KEY,
                &layers.cli.trust.default_class,
                &layers.environment.trust.default_class,
                &layers.project.trust.default_class,
                &layers.user.trust.default_class,
                &layers.defaults.trust.default_class,
            ),
            prompt_injection_guard: pick_field(
                &mut sources,
                TRUST_PROMPT_INJECTION_GUARD_KEY,
                &layers.cli.trust.prompt_injection_guard,
                &layers.environment.trust.prompt_injection_guard,
                &layers.project.trust.prompt_injection_guard,
                &layers.user.trust.prompt_injection_guard,
                &layers.defaults.trust.prompt_injection_guard,
            ),
        },
    };

    MergedConfig { values, sources }
}

fn pick_field<T: Clone>(
    sources: &mut BTreeMap<&'static str, ConfigValueSource>,
    key: &'static str,
    cli: &Option<T>,
    environment: &Option<T>,
    project: &Option<T>,
    user: &Option<T>,
    default: &Option<T>,
) -> Option<T> {
    if let Some(value) = cli {
        sources.insert(key, ConfigValueSource::Cli);
        return Some(value.clone());
    }
    if let Some(value) = environment {
        sources.insert(key, ConfigValueSource::Environment);
        return Some(value.clone());
    }
    if let Some(value) = project {
        sources.insert(key, ConfigValueSource::Project);
        return Some(value.clone());
    }
    if let Some(value) = user {
        sources.insert(key, ConfigValueSource::User);
        return Some(value.clone());
    }
    default.as_ref().map(|value| {
        sources.insert(key, ConfigValueSource::Default);
        value.clone()
    })
}

fn optional_env_string(
    env: &BTreeMap<String, OsString>,
    variable: &'static str,
) -> Result<Option<String>, EnvironmentConfigError> {
    let Some(value) = env.get(variable) else {
        return Ok(None);
    };
    match value.to_str() {
        Some(value) => Ok(Some(value.to_string())),
        None => Err(EnvironmentConfigError::InvalidUnicode { variable }),
    }
}

fn optional_env_u64(
    env: &BTreeMap<String, OsString>,
    variable: &'static str,
) -> Result<Option<u64>, EnvironmentConfigError> {
    let Some(value) = optional_env_string(env, variable)? else {
        return Ok(None);
    };
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| EnvironmentConfigError::InvalidUnsignedInteger { variable, value })
}

fn optional_env_path(
    env: &BTreeMap<String, OsString>,
    variable: &'static str,
    expander: &PathExpander,
) -> Result<Option<PathBuf>, EnvironmentConfigError> {
    let Some(value) = optional_env_string(env, variable)? else {
        return Ok(None);
    };
    expand_env_path(variable, &value, expander).map(Some)
}

fn expand_env_path(
    variable: &'static str,
    value: &str,
    expander: &PathExpander,
) -> Result<PathBuf, EnvironmentConfigError> {
    expander
        .expand(value)
        .map_err(|source| EnvironmentConfigError::PathExpansion { variable, source })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{
        CURATION_SPECIFICITY_MIN_KEY, ConfigLayers, ConfigValueSource, EnvironmentConfigError,
        PACK_DEFAULT_MAX_TOKENS_KEY, PACK_DEFAULT_PROFILE_KEY, SEARCH_DEFAULT_SPEED_KEY,
        STORAGE_DATABASE_PATH_KEY, STORAGE_INDEX_DIR_KEY, built_in_config, config_from_env,
        merge_config,
    };
    use crate::config::{
        ConfigFile, CurationConfig, PackConfig, PathExpander, SearchConfig, SearchSpeed,
        StorageConfig,
    };

    type TestResult = Result<(), String>;

    fn expander() -> PathExpander {
        PathExpander::with_env(Some(PathBuf::from("/home/agent")), BTreeMap::new())
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
    fn built_in_defaults_match_readme_contract() -> TestResult {
        let defaults =
            built_in_config(&expander()).map_err(|error| format!("defaults failed: {error}"))?;

        ensure_equal(
            &defaults.storage.database_path,
            &Some(PathBuf::from("/home/agent/.local/share/ee/ee.db")),
            "database path",
        )?;
        ensure_equal(
            &defaults.storage.index_dir,
            &Some(PathBuf::from("/home/agent/.local/share/ee/indexes")),
            "index dir",
        )?;
        ensure_equal(&defaults.runtime.job_budget_ms, &Some(5000), "job budget")?;
        ensure_equal(
            &defaults.search.default_speed,
            &Some(SearchSpeed::Balanced),
            "search speed",
        )?;
        ensure_equal(&defaults.pack.default_max_tokens, &Some(4000), "max tokens")?;
        ensure_equal(
            &defaults.curation.specificity_min,
            &Some(0.45),
            "specificity min",
        )?;
        ensure_equal(
            &defaults.trust.default_class.as_deref(),
            &Some("agent_assertion"),
            "trust default class",
        )
    }

    #[test]
    fn environment_layer_parses_documented_overrides() -> TestResult {
        let mut env = BTreeMap::new();
        env.insert(
            "EE_DATABASE_PATH".to_string(),
            OsString::from("~/custom/ee.db"),
        );
        env.insert("EE_INDEX_DIR".to_string(), OsString::from("/tmp/index"));
        env.insert("EE_PROFILE".to_string(), OsString::from("release"));
        env.insert("EE_MAX_TOKENS".to_string(), OsString::from("8192"));

        let parsed =
            config_from_env(&env, &expander()).map_err(|error| format!("env failed: {error}"))?;

        ensure_equal(
            &parsed.storage.database_path,
            &Some(PathBuf::from("/home/agent/custom/ee.db")),
            "env database path",
        )?;
        ensure_equal(
            &parsed.storage.index_dir,
            &Some(PathBuf::from("/tmp/index")),
            "env index dir",
        )?;
        ensure_equal(
            &parsed.pack.default_profile.as_deref(),
            &Some("release"),
            "env profile",
        )?;
        ensure_equal(
            &parsed.pack.default_max_tokens,
            &Some(8192),
            "env max tokens",
        )
    }

    #[test]
    fn environment_layer_rejects_invalid_integer() -> TestResult {
        let mut env = BTreeMap::new();
        env.insert("EE_MAX_TOKENS".to_string(), OsString::from("many"));

        let error = match config_from_env(&env, &expander()) {
            Ok(config) => return Err(format!("expected env error, got {config:?}")),
            Err(error) => error,
        };

        ensure_equal(
            &error,
            &EnvironmentConfigError::InvalidUnsignedInteger {
                variable: "EE_MAX_TOKENS",
                value: "many".to_string(),
            },
            "invalid integer error",
        )
    }

    #[test]
    fn merge_uses_cli_environment_project_user_default_order() -> TestResult {
        let defaults =
            built_in_config(&expander()).map_err(|error| format!("defaults failed: {error}"))?;
        let user = ConfigFile {
            storage: StorageConfig {
                database_path: Some(PathBuf::from("/user/ee.db")),
                ..StorageConfig::default()
            },
            search: SearchConfig {
                default_speed: Some(SearchSpeed::Fast),
                ..SearchConfig::default()
            },
            ..ConfigFile::default()
        };
        let project = ConfigFile {
            storage: StorageConfig {
                database_path: Some(PathBuf::from("/project/ee.db")),
                ..StorageConfig::default()
            },
            search: SearchConfig {
                default_speed: Some(SearchSpeed::Thorough),
                ..SearchConfig::default()
            },
            curation: CurationConfig {
                specificity_min: Some(0.60),
                ..CurationConfig::default()
            },
            ..ConfigFile::default()
        };
        let environment = ConfigFile {
            storage: StorageConfig {
                index_dir: Some(PathBuf::from("/env/index")),
                ..StorageConfig::default()
            },
            pack: PackConfig {
                default_profile: Some("env-profile".to_string()),
                ..PackConfig::default()
            },
            ..ConfigFile::default()
        };
        let cli = ConfigFile {
            pack: PackConfig {
                default_profile: Some("cli-profile".to_string()),
                ..PackConfig::default()
            },
            ..ConfigFile::default()
        };

        let merged = merge_config(&ConfigLayers {
            cli,
            environment,
            project,
            user,
            defaults,
        });

        ensure_equal(
            &merged.values.storage.database_path,
            &Some(PathBuf::from("/project/ee.db")),
            "project beats user database path",
        )?;
        ensure_equal(
            &merged.source(STORAGE_DATABASE_PATH_KEY),
            &Some(ConfigValueSource::Project),
            "database path source",
        )?;
        ensure_equal(
            &merged.values.storage.index_dir,
            &Some(PathBuf::from("/env/index")),
            "env index dir",
        )?;
        ensure_equal(
            &merged.source(STORAGE_INDEX_DIR_KEY),
            &Some(ConfigValueSource::Environment),
            "index dir source",
        )?;
        ensure_equal(
            &merged.values.search.default_speed,
            &Some(SearchSpeed::Thorough),
            "project beats user search speed",
        )?;
        ensure_equal(
            &merged.source(SEARCH_DEFAULT_SPEED_KEY),
            &Some(ConfigValueSource::Project),
            "search speed source",
        )?;
        ensure_equal(
            &merged.values.pack.default_profile.as_deref(),
            &Some("cli-profile"),
            "cli beats env profile",
        )?;
        ensure_equal(
            &merged.source(PACK_DEFAULT_PROFILE_KEY),
            &Some(ConfigValueSource::Cli),
            "profile source",
        )?;
        ensure_equal(
            &merged.source(PACK_DEFAULT_MAX_TOKENS_KEY),
            &Some(ConfigValueSource::Default),
            "default max tokens source",
        )?;
        ensure_equal(
            &merged.values.curation.specificity_min,
            &Some(0.60),
            "project specificity threshold",
        )?;
        ensure_equal(
            &merged.source(CURATION_SPECIFICITY_MIN_KEY),
            &Some(ConfigValueSource::Project),
            "specificity threshold source",
        )
    }

    #[test]
    fn source_keys_are_deterministically_ordered() -> TestResult {
        let defaults =
            built_in_config(&expander()).map_err(|error| format!("defaults failed: {error}"))?;
        let merged = merge_config(&ConfigLayers::with_defaults(defaults));

        let keys: Vec<&str> = merged.sources().keys().copied().collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();

        ensure_equal(&keys, &sorted, "source key ordering")
    }
}
