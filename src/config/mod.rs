pub mod file;
pub mod merge;
pub mod path;
pub mod workspace;

pub use file::{
    CassConfig, ConfigFile, ConfigParseError, CurationConfig, PackConfig, PrivacyConfig,
    RuntimeConfig, SearchConfig, SearchSpeed, StorageConfig, TrustConfig,
};
pub use merge::{
    CASS_BINARY_KEY, CASS_ENABLED_KEY, CASS_SINCE_KEY, CURATION_DECAY_HALF_LIFE_DAYS_KEY,
    CURATION_DUPLICATE_SIMILARITY_KEY, CURATION_HARMFUL_WEIGHT_KEY, ConfigLayers,
    ConfigValueSource, EnvironmentConfigError, MergedConfig, PACK_CANDIDATE_POOL_KEY,
    PACK_DEFAULT_FORMAT_KEY, PACK_DEFAULT_MAX_TOKENS_KEY, PACK_DEFAULT_PROFILE_KEY,
    PACK_MMR_LAMBDA_KEY, PRIVACY_REDACT_SECRETS_KEY, PRIVACY_REDACTION_CLASSES_KEY,
    RUNTIME_DAEMON_KEY, RUNTIME_IMPORT_BATCH_SIZE_KEY, RUNTIME_JOB_BUDGET_MS_KEY,
    SEARCH_DEFAULT_SPEED_KEY, SEARCH_GRAPH_WEIGHT_KEY, SEARCH_LEXICAL_WEIGHT_KEY,
    SEARCH_SEMANTIC_WEIGHT_KEY, STORAGE_DATABASE_PATH_KEY, STORAGE_INDEX_DIR_KEY,
    STORAGE_JSONL_EXPORT_KEY, TRUST_DEFAULT_CLASS_KEY, TRUST_PROMPT_INJECTION_GUARD_KEY,
    built_in_config, config_from_env, merge_config,
};
pub use path::{PathExpander, PathExpansionError};
pub use workspace::{
    WORKSPACE_MARKER, WorkspaceError, WorkspaceLocation, discover, discover_from_current_dir,
};

pub const SUBSYSTEM: &str = "config";

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[cfg(test)]
mod tests {
    use super::subsystem_name;

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
        ensure_equal(&subsystem_name(), &"config", "config subsystem name")
    }
}
