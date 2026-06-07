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

use serde::Serialize;

use crate::models::{RedactionLevel, TaskLens};

use super::env_registry::EnvVar;
use super::file::{
    CacheConfig, CassConfig, ConfigFile, CurationConfig, FeedbackConfig, GraphCausalConfig,
    GraphConfig, GraphCurateConfig, GraphFeatureFlagsConfig, GraphGomoryHuConfig,
    GraphHealthConfig, GraphHitsConfig, GraphMemoryConfig, GraphPackDnaConfig, GraphPprConfig,
    GraphWitnessesConfig, HandoffConfig, LearnConfig, LearnDecayConfig, MeshCommandMode,
    MeshConfig, OutputRedactionConfig, PackConfig, PackL2CacheConfig, PolicyConfig, PrivacyConfig,
    ReadPoolConfig, RedactionConfig, RedactionDefaultsConfig, RuntimeConfig, SearchConfig,
    SearchLexicalRamTierConfig, SearchSpeed, SecretDetectorConfig, StorageConfig,
    SwarmAdaptiveConfig, SwarmConfig, TaskLensConfig, TrustConfig,
};
use super::path::{PathExpander, PathExpansionError};

pub const STORAGE_DATABASE_PATH_KEY: &str = "storage.database_path";
pub const STORAGE_INDEX_DIR_KEY: &str = "storage.index_dir";
pub const STORAGE_JSONL_EXPORT_KEY: &str = "storage.jsonl_export";
pub const STORAGE_READ_POOL_SIZE_KEY: &str = "storage.read_pool.size";
pub const STORAGE_READ_POOL_IDLE_TIMEOUT_SECONDS_KEY: &str =
    "storage.read_pool.idle_timeout_seconds";
pub const STORAGE_READ_POOL_MAX_PIN_DURATION_SECONDS_KEY: &str =
    "storage.read_pool.max_pin_duration_seconds";
pub const STORAGE_READ_POOL_ACQUIRE_TIMEOUT_MS_KEY: &str = "storage.read_pool.acquire_timeout_ms";
pub const STORAGE_READ_POOL_PIN_SNAPSHOT_KEY: &str = "storage.read_pool.pin_snapshot";
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
pub const SEARCH_LEXICAL_RAM_TIER_ENABLED_KEY: &str = "search.lexical_ram_tier.enabled";
pub const SEARCH_LEXICAL_RAM_TIER_REQUEST_HUGEPAGES_KEY: &str =
    "search.lexical_ram_tier.request_hugepages";
pub const SEARCH_LEXICAL_RAM_TIER_POPULATE_ON_OPEN_KEY: &str =
    "search.lexical_ram_tier.populate_on_open";
pub const PACK_DEFAULT_PROFILE_KEY: &str = "pack.default_profile";
pub const PACK_DEFAULT_FORMAT_KEY: &str = "pack.default_format";
pub const PACK_DEFAULT_MAX_TOKENS_KEY: &str = "pack.default_max_tokens";
pub const PACK_ADAPTIVE_BUDGET_KEY: &str = "pack.adaptive_budget";
pub const PACK_MMR_LAMBDA_KEY: &str = "pack.mmr_lambda";
pub const PACK_CANDIDATE_POOL_KEY: &str = "pack.candidate_pool";
pub const PACK_MEMORY_TIER_ADMISSION_KEY: &str = "pack.memory_tier_admission";
pub const CACHE_PACK_L2_ENABLED_KEY: &str = "cache.pack_l2.enabled";
pub const CACHE_PACK_L2_DIRECTORY_KEY: &str = "cache.pack_l2.directory";
pub const CACHE_PACK_L2_MAX_BYTES_KEY: &str = "cache.pack_l2.max_bytes";
pub const CACHE_PACK_L2_MAX_AGE_DAYS_KEY: &str = "cache.pack_l2.max_age_days";
pub const MESH_ENABLED_KEY: &str = "mesh.enabled";
pub const MESH_COMMAND_MODE_KEY: &str = "mesh.command_mode";
pub const MESH_PEER_GROUP_BINDINGS_KEY: &str = "mesh.peer_group_bindings";
pub const MESH_PEER_POLICIES_KEY: &str = "mesh.peer_policies";
pub const SWARM_ADAPTIVE_ENABLED_KEY: &str = "swarm.adaptive.enabled";
pub const SWARM_ADAPTIVE_PREFETCH_TOP_K_KEY: &str = "swarm.adaptive.prefetch_top_k";
pub const SWARM_ADAPTIVE_PREFETCH_BUDGET_MS_KEY: &str = "swarm.adaptive.prefetch_budget_ms";
pub const SWARM_ADAPTIVE_SIMILARITY_THRESHOLD_KEY: &str = "swarm.adaptive.similarity_threshold";
pub const SWARM_ADAPTIVE_NOISY_NEIGHBOR_P99_MS_KEY: &str = "swarm.adaptive.noisy_neighbor_p99_ms";
pub const SWARM_ADAPTIVE_NOISY_NEIGHBOR_BACKOFF_MS_KEY: &str =
    "swarm.adaptive.noisy_neighbor_backoff_ms";
pub const GRAPH_PPR_ALPHA_KEY: &str = "graph.ppr.alpha";
pub const GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY: &str = "graph.health.contradiction_threshold";
pub const GRAPH_CURATE_ONION_DECAY_MAX_KEY: &str = "graph.curate.onion_decay_max";
pub const GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY: &str =
    "graph.curate.articulation_protection_multiplier";
pub const GRAPH_HITS_PROFILE_BOOST_KEY: &str = "graph.hits.profile_boost";
pub const GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY: &str = "graph.causal.min_cost_normalization";
pub const GRAPH_PACK_DNA_MAX_ITEMS_KEY: &str = "graph.pack_dna.max_items";
pub const GRAPH_PACK_DNA_MAX_EDGES_KEY: &str = "graph.pack_dna.max_edges";
pub const GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY: &str = "graph.gomory_hu.sample_threshold";
pub const GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY: &str = "graph.gomory_hu.sample_size";
pub const GRAPH_MEMORY_SNAPSHOT_CAP_MB_KEY: &str = "graph.memory.snapshot_cap_mb";
pub const GRAPH_MEMORY_PER_ALGORITHM_CAP_MB_KEY: &str = "graph.memory.per_algorithm_cap_mb";
pub const GRAPH_MEMORY_DEGRADED_BELOW_PCT_KEY: &str = "graph.memory.degraded_below_pct";
pub const GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS_KEY: &str =
    "graph.memory.growth_multiplier_basis_points";
pub const GRAPH_WITNESSES_RETENTION_DAYS_KEY: &str = "graph.witnesses.retention_days";
pub const GRAPH_WITNESSES_ALGORITHM_TTL_DAYS_KEY: &str = "graph.witnesses.algorithm_ttl_days";
pub const GRAPH_FEATURE_PPR_ENABLED_KEY: &str = "graph.feature.ppr.enabled";
pub const GRAPH_FEATURE_PACK_DNA_ENABLED_KEY: &str = "graph.feature.pack_dna.enabled";
pub const GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY: &str = "graph.feature.causal_explain.enabled";
pub const GRAPH_FEATURE_STRUCTURAL_HEALTH_ENABLED_KEY: &str =
    "graph.feature.structural_health.enabled";
pub const GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY: &str =
    "graph.feature.structural_decay.enabled";
pub const GRAPH_FEATURE_PROXIMITY_ENABLED_KEY: &str = "graph.feature.proximity.enabled";
pub const GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY: &str =
    "graph.feature.revision_dominance.enabled";
pub const GRAPH_FEATURE_SKYLINE_ENABLED_KEY: &str = "graph.feature.skyline.enabled";
pub const GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY: &str = "graph.feature.load_bearing.enabled";
pub const GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY: &str = "graph.feature.hits_profiles.enabled";
pub const CURATION_DUPLICATE_SIMILARITY_KEY: &str = "curation.duplicate_similarity";
pub const CURATION_HARMFUL_WEIGHT_KEY: &str = "curation.harmful_weight";
pub const CURATION_DECAY_HALF_LIFE_DAYS_KEY: &str = "curation.decay_half_life_days";
pub const CURATION_SPECIFICITY_MIN_KEY: &str = "curation.specificity_min";
pub const LEARN_DECAY_DEMOTE_THRESHOLD_KEY: &str = "learn.decay.demote_threshold";
pub const LEARN_DECAY_FORGET_THRESHOLD_KEY: &str = "learn.decay.forget_threshold";
pub const LEARN_DECAY_WORKING_HALF_LIFE_DAYS_KEY: &str = "learn.decay.working_half_life_days";
pub const LEARN_DECAY_EPISODIC_EVENT_HALF_LIFE_DAYS_KEY: &str =
    "learn.decay.episodic_event_half_life_days";
pub const LEARN_DECAY_EPISODIC_FAILURE_HALF_LIFE_DAYS_KEY: &str =
    "learn.decay.episodic_failure_half_life_days";
pub const LEARN_DECAY_SEMANTIC_FACT_HALF_LIFE_DAYS_KEY: &str =
    "learn.decay.semantic_fact_half_life_days";
pub const LEARN_DECAY_PROCEDURAL_RULE_HALF_LIFE_DAYS_KEY: &str =
    "learn.decay.procedural_rule_half_life_days";
pub const LEARN_DECAY_DEFAULT_HALF_LIFE_DAYS_KEY: &str = "learn.decay.default_half_life_days";
pub const LEARN_CLUSTER_COHERENCE_THRESHOLD_KEY: &str = "learn.cluster_coherence_threshold";
pub const FEEDBACK_HARMFUL_PER_SOURCE_PER_HOUR_KEY: &str = "feedback.harmful_per_source_per_hour";
pub const FEEDBACK_HARMFUL_BURST_WINDOW_SECONDS_KEY: &str = "feedback.harmful_burst_window_seconds";
pub const REDACTION_DEFAULT_EXPORT_KEY: &str = "redaction.defaults.export";
pub const REDACTION_DEFAULT_HANDOFF_CREATE_KEY: &str = "redaction.defaults.handoff_create";
pub const REDACTION_DEFAULT_CONTEXT_JSON_KEY: &str = "redaction.defaults.context_json";
pub const REDACTION_DEFAULT_SUPPORT_BUNDLE_KEY: &str = "redaction.defaults.support_bundle";
pub const POLICY_SECRET_DETECTOR_ALLOW_PHRASES_KEY: &str = "policy.secret_detector.allow_phrases";
pub const POLICY_SECRET_DETECTOR_ALLOW_REGEX_KEY: &str = "policy.secret_detector.allow_regex";
pub const POLICY_OUTPUT_REDACTION_ENABLED_KEY: &str = "policy.output_redaction.enabled";
pub const PRIVACY_REDACT_SECRETS_KEY: &str = "privacy.redact_secrets";
pub const PRIVACY_REDACTION_CLASSES_KEY: &str = "privacy.redaction_classes";
pub const TRUST_DEFAULT_CLASS_KEY: &str = "trust.default_class";
pub const TRUST_PROMPT_INJECTION_GUARD_KEY: &str = "trust.prompt_injection_guard";
pub const TRUST_TEAM_MEMBERS_KEY: &str = "trust.team_members";

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

    /// Generate a config show report with source attribution for each key.
    #[must_use]
    pub fn to_show_report(&self) -> ConfigShowReport {
        let mut entries = Vec::new();

        // Storage section
        if let Some(ref path) = self.values.storage.database_path {
            entries.push(ConfigShowEntry::new(
                STORAGE_DATABASE_PATH_KEY,
                path.display().to_string(),
                self.source(STORAGE_DATABASE_PATH_KEY),
            ));
        }
        if let Some(ref path) = self.values.storage.index_dir {
            entries.push(ConfigShowEntry::new(
                STORAGE_INDEX_DIR_KEY,
                path.display().to_string(),
                self.source(STORAGE_INDEX_DIR_KEY),
            ));
        }
        if let Some(export) = self.values.storage.jsonl_export {
            entries.push(ConfigShowEntry::new(
                STORAGE_JSONL_EXPORT_KEY,
                export.to_string(),
                self.source(STORAGE_JSONL_EXPORT_KEY),
            ));
        }
        if let Some(size) = self.values.storage.read_pool.size {
            entries.push(ConfigShowEntry::new(
                STORAGE_READ_POOL_SIZE_KEY,
                size.to_string(),
                self.source(STORAGE_READ_POOL_SIZE_KEY),
            ));
        }
        if let Some(seconds) = self.values.storage.read_pool.idle_timeout_seconds {
            entries.push(ConfigShowEntry::new(
                STORAGE_READ_POOL_IDLE_TIMEOUT_SECONDS_KEY,
                seconds.to_string(),
                self.source(STORAGE_READ_POOL_IDLE_TIMEOUT_SECONDS_KEY),
            ));
        }
        if let Some(seconds) = self.values.storage.read_pool.max_pin_duration_seconds {
            entries.push(ConfigShowEntry::new(
                STORAGE_READ_POOL_MAX_PIN_DURATION_SECONDS_KEY,
                seconds.to_string(),
                self.source(STORAGE_READ_POOL_MAX_PIN_DURATION_SECONDS_KEY),
            ));
        }
        if let Some(ms) = self.values.storage.read_pool.acquire_timeout_ms {
            entries.push(ConfigShowEntry::new(
                STORAGE_READ_POOL_ACQUIRE_TIMEOUT_MS_KEY,
                ms.to_string(),
                self.source(STORAGE_READ_POOL_ACQUIRE_TIMEOUT_MS_KEY),
            ));
        }
        if let Some(pin_snapshot) = self.values.storage.read_pool.pin_snapshot {
            entries.push(ConfigShowEntry::new(
                STORAGE_READ_POOL_PIN_SNAPSHOT_KEY,
                pin_snapshot.to_string(),
                self.source(STORAGE_READ_POOL_PIN_SNAPSHOT_KEY),
            ));
        }

        // Runtime section
        if let Some(daemon) = self.values.runtime.daemon {
            entries.push(ConfigShowEntry::new(
                RUNTIME_DAEMON_KEY,
                daemon.to_string(),
                self.source(RUNTIME_DAEMON_KEY),
            ));
        }
        if let Some(budget) = self.values.runtime.job_budget_ms {
            entries.push(ConfigShowEntry::new(
                RUNTIME_JOB_BUDGET_MS_KEY,
                budget.to_string(),
                self.source(RUNTIME_JOB_BUDGET_MS_KEY),
            ));
        }
        if let Some(batch) = self.values.runtime.import_batch_size {
            entries.push(ConfigShowEntry::new(
                RUNTIME_IMPORT_BATCH_SIZE_KEY,
                batch.to_string(),
                self.source(RUNTIME_IMPORT_BATCH_SIZE_KEY),
            ));
        }

        // CASS section
        if let Some(enabled) = self.values.cass.enabled {
            entries.push(ConfigShowEntry::new(
                CASS_ENABLED_KEY,
                enabled.to_string(),
                self.source(CASS_ENABLED_KEY),
            ));
        }
        if let Some(ref binary) = self.values.cass.binary {
            entries.push(ConfigShowEntry::new(
                CASS_BINARY_KEY,
                binary.clone(),
                self.source(CASS_BINARY_KEY),
            ));
        }
        if let Some(ref since) = self.values.cass.since {
            entries.push(ConfigShowEntry::new(
                CASS_SINCE_KEY,
                since.clone(),
                self.source(CASS_SINCE_KEY),
            ));
        }

        // Search section
        if let Some(ref speed) = self.values.search.default_speed {
            entries.push(ConfigShowEntry::new(
                SEARCH_DEFAULT_SPEED_KEY,
                speed.as_str().to_owned(),
                self.source(SEARCH_DEFAULT_SPEED_KEY),
            ));
        }
        if let Some(weight) = self.values.search.lexical_weight {
            entries.push(ConfigShowEntry::new(
                SEARCH_LEXICAL_WEIGHT_KEY,
                weight.to_string(),
                self.source(SEARCH_LEXICAL_WEIGHT_KEY),
            ));
        }
        if let Some(weight) = self.values.search.semantic_weight {
            entries.push(ConfigShowEntry::new(
                SEARCH_SEMANTIC_WEIGHT_KEY,
                weight.to_string(),
                self.source(SEARCH_SEMANTIC_WEIGHT_KEY),
            ));
        }
        if let Some(weight) = self.values.search.graph_weight {
            entries.push(ConfigShowEntry::new(
                SEARCH_GRAPH_WEIGHT_KEY,
                weight.to_string(),
                self.source(SEARCH_GRAPH_WEIGHT_KEY),
            ));
        }
        if let Some(enabled) = self.values.search.lexical_ram_tier.enabled {
            entries.push(ConfigShowEntry::new(
                SEARCH_LEXICAL_RAM_TIER_ENABLED_KEY,
                enabled.to_string(),
                self.source(SEARCH_LEXICAL_RAM_TIER_ENABLED_KEY),
            ));
        }
        if let Some(request_hugepages) = self.values.search.lexical_ram_tier.request_hugepages {
            entries.push(ConfigShowEntry::new(
                SEARCH_LEXICAL_RAM_TIER_REQUEST_HUGEPAGES_KEY,
                request_hugepages.to_string(),
                self.source(SEARCH_LEXICAL_RAM_TIER_REQUEST_HUGEPAGES_KEY),
            ));
        }
        if let Some(populate_on_open) = self.values.search.lexical_ram_tier.populate_on_open {
            entries.push(ConfigShowEntry::new(
                SEARCH_LEXICAL_RAM_TIER_POPULATE_ON_OPEN_KEY,
                populate_on_open.to_string(),
                self.source(SEARCH_LEXICAL_RAM_TIER_POPULATE_ON_OPEN_KEY),
            ));
        }

        // Pack section
        if let Some(ref profile) = self.values.pack.default_profile {
            entries.push(ConfigShowEntry::new(
                PACK_DEFAULT_PROFILE_KEY,
                profile.clone(),
                self.source(PACK_DEFAULT_PROFILE_KEY),
            ));
        }
        if let Some(ref format) = self.values.pack.default_format {
            entries.push(ConfigShowEntry::new(
                PACK_DEFAULT_FORMAT_KEY,
                format.clone(),
                self.source(PACK_DEFAULT_FORMAT_KEY),
            ));
        }
        if let Some(tokens) = self.values.pack.default_max_tokens {
            entries.push(ConfigShowEntry::new(
                PACK_DEFAULT_MAX_TOKENS_KEY,
                tokens.to_string(),
                self.source(PACK_DEFAULT_MAX_TOKENS_KEY),
            ));
        }
        if let Some(adaptive) = self.values.pack.adaptive_budget {
            entries.push(ConfigShowEntry::new(
                PACK_ADAPTIVE_BUDGET_KEY,
                adaptive.to_string(),
                self.source(PACK_ADAPTIVE_BUDGET_KEY),
            ));
        }
        if let Some(lambda) = self.values.pack.mmr_lambda {
            entries.push(ConfigShowEntry::new(
                PACK_MMR_LAMBDA_KEY,
                lambda.to_string(),
                self.source(PACK_MMR_LAMBDA_KEY),
            ));
        }
        if let Some(pool) = self.values.pack.candidate_pool {
            entries.push(ConfigShowEntry::new(
                PACK_CANDIDATE_POOL_KEY,
                pool.to_string(),
                self.source(PACK_CANDIDATE_POOL_KEY),
            ));
        }
        if let Some(enabled) = self.values.pack.memory_tier_admission {
            entries.push(ConfigShowEntry::new(
                PACK_MEMORY_TIER_ADMISSION_KEY,
                enabled.to_string(),
                self.source(PACK_MEMORY_TIER_ADMISSION_KEY),
            ));
        }

        // Cache section
        if let Some(enabled) = self.values.cache.pack_l2.enabled {
            entries.push(ConfigShowEntry::new(
                CACHE_PACK_L2_ENABLED_KEY,
                enabled.to_string(),
                self.source(CACHE_PACK_L2_ENABLED_KEY),
            ));
        }
        if let Some(ref directory) = self.values.cache.pack_l2.directory {
            entries.push(ConfigShowEntry::new(
                CACHE_PACK_L2_DIRECTORY_KEY,
                directory.display().to_string(),
                self.source(CACHE_PACK_L2_DIRECTORY_KEY),
            ));
        }
        if let Some(max_bytes) = self.values.cache.pack_l2.max_bytes {
            entries.push(ConfigShowEntry::new(
                CACHE_PACK_L2_MAX_BYTES_KEY,
                max_bytes.to_string(),
                self.source(CACHE_PACK_L2_MAX_BYTES_KEY),
            ));
        }
        if let Some(max_age_days) = self.values.cache.pack_l2.max_age_days {
            entries.push(ConfigShowEntry::new(
                CACHE_PACK_L2_MAX_AGE_DAYS_KEY,
                max_age_days.to_string(),
                self.source(CACHE_PACK_L2_MAX_AGE_DAYS_KEY),
            ));
        }

        // Mesh section
        if let Some(enabled) = self.values.mesh.enabled {
            entries.push(ConfigShowEntry::new(
                MESH_ENABLED_KEY,
                enabled.to_string(),
                self.source(MESH_ENABLED_KEY),
            ));
        }
        if let Some(command_mode) = self.values.mesh.command_mode {
            entries.push(ConfigShowEntry::new(
                MESH_COMMAND_MODE_KEY,
                command_mode.as_str().to_string(),
                self.source(MESH_COMMAND_MODE_KEY),
            ));
        }
        if let Some(ref bindings) = self.values.mesh.peer_group_bindings {
            entries.push(ConfigShowEntry::new(
                MESH_PEER_GROUP_BINDINGS_KEY,
                bindings.len().to_string(),
                self.source(MESH_PEER_GROUP_BINDINGS_KEY),
            ));
        }
        if let Some(ref policies) = self.values.mesh.peer_policies {
            entries.push(ConfigShowEntry::new(
                MESH_PEER_POLICIES_KEY,
                policies.len().to_string(),
                self.source(MESH_PEER_POLICIES_KEY),
            ));
        }

        // Swarm adaptive section
        if let Some(enabled) = self.values.swarm.adaptive.enabled {
            entries.push(ConfigShowEntry::new(
                SWARM_ADAPTIVE_ENABLED_KEY,
                enabled.to_string(),
                self.source(SWARM_ADAPTIVE_ENABLED_KEY),
            ));
        }
        if let Some(top_k) = self.values.swarm.adaptive.prefetch_top_k {
            entries.push(ConfigShowEntry::new(
                SWARM_ADAPTIVE_PREFETCH_TOP_K_KEY,
                top_k.to_string(),
                self.source(SWARM_ADAPTIVE_PREFETCH_TOP_K_KEY),
            ));
        }
        if let Some(budget_ms) = self.values.swarm.adaptive.prefetch_budget_ms {
            entries.push(ConfigShowEntry::new(
                SWARM_ADAPTIVE_PREFETCH_BUDGET_MS_KEY,
                budget_ms.to_string(),
                self.source(SWARM_ADAPTIVE_PREFETCH_BUDGET_MS_KEY),
            ));
        }
        if let Some(threshold) = self.values.swarm.adaptive.similarity_threshold {
            entries.push(ConfigShowEntry::new(
                SWARM_ADAPTIVE_SIMILARITY_THRESHOLD_KEY,
                threshold.to_string(),
                self.source(SWARM_ADAPTIVE_SIMILARITY_THRESHOLD_KEY),
            ));
        }
        if let Some(p99_ms) = self.values.swarm.adaptive.noisy_neighbor_p99_ms {
            entries.push(ConfigShowEntry::new(
                SWARM_ADAPTIVE_NOISY_NEIGHBOR_P99_MS_KEY,
                p99_ms.to_string(),
                self.source(SWARM_ADAPTIVE_NOISY_NEIGHBOR_P99_MS_KEY),
            ));
        }
        if let Some(backoff_ms) = self.values.swarm.adaptive.noisy_neighbor_backoff_ms {
            entries.push(ConfigShowEntry::new(
                SWARM_ADAPTIVE_NOISY_NEIGHBOR_BACKOFF_MS_KEY,
                backoff_ms.to_string(),
                self.source(SWARM_ADAPTIVE_NOISY_NEIGHBOR_BACKOFF_MS_KEY),
            ));
        }

        // Graph section
        if let Some(alpha) = self.values.graph.ppr.alpha {
            entries.push(ConfigShowEntry::new(
                GRAPH_PPR_ALPHA_KEY,
                alpha.to_string(),
                self.source(GRAPH_PPR_ALPHA_KEY),
            ));
        }
        if let Some(threshold) = self.values.graph.health.contradiction_threshold {
            entries.push(ConfigShowEntry::new(
                GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY,
                threshold.to_string(),
                self.source(GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY),
            ));
        }
        if let Some(multiplier) = self.values.graph.curate.onion_decay_max {
            entries.push(ConfigShowEntry::new(
                GRAPH_CURATE_ONION_DECAY_MAX_KEY,
                multiplier.to_string(),
                self.source(GRAPH_CURATE_ONION_DECAY_MAX_KEY),
            ));
        }
        if let Some(multiplier) = self.values.graph.curate.articulation_protection_multiplier {
            entries.push(ConfigShowEntry::new(
                GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY,
                multiplier.to_string(),
                self.source(GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY),
            ));
        }
        if let Some(boost) = self.values.graph.hits.profile_boost {
            entries.push(ConfigShowEntry::new(
                GRAPH_HITS_PROFILE_BOOST_KEY,
                boost.to_string(),
                self.source(GRAPH_HITS_PROFILE_BOOST_KEY),
            ));
        }
        if let Some(normalization) = self.values.graph.causal.min_cost_normalization {
            entries.push(ConfigShowEntry::new(
                GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY,
                normalization.to_string(),
                self.source(GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY),
            ));
        }
        if let Some(max_items) = self.values.graph.pack_dna.max_items {
            entries.push(ConfigShowEntry::new(
                GRAPH_PACK_DNA_MAX_ITEMS_KEY,
                max_items.to_string(),
                self.source(GRAPH_PACK_DNA_MAX_ITEMS_KEY),
            ));
        }
        if let Some(max_edges) = self.values.graph.pack_dna.max_edges {
            entries.push(ConfigShowEntry::new(
                GRAPH_PACK_DNA_MAX_EDGES_KEY,
                max_edges.to_string(),
                self.source(GRAPH_PACK_DNA_MAX_EDGES_KEY),
            ));
        }
        if let Some(threshold) = self.values.graph.gomory_hu.sample_threshold {
            entries.push(ConfigShowEntry::new(
                GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY,
                threshold.to_string(),
                self.source(GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY),
            ));
        }
        if let Some(size) = self.values.graph.gomory_hu.sample_size {
            entries.push(ConfigShowEntry::new(
                GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY,
                size.to_string(),
                self.source(GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY),
            ));
        }
        if let Some(cap) = self.values.graph.memory.snapshot_cap_mb {
            entries.push(ConfigShowEntry::new(
                GRAPH_MEMORY_SNAPSHOT_CAP_MB_KEY,
                cap.to_string(),
                self.source(GRAPH_MEMORY_SNAPSHOT_CAP_MB_KEY),
            ));
        }
        if let Some(cap) = self.values.graph.memory.per_algorithm_cap_mb {
            entries.push(ConfigShowEntry::new(
                GRAPH_MEMORY_PER_ALGORITHM_CAP_MB_KEY,
                cap.to_string(),
                self.source(GRAPH_MEMORY_PER_ALGORITHM_CAP_MB_KEY),
            ));
        }
        if let Some(percent) = self.values.graph.memory.degraded_below_pct {
            entries.push(ConfigShowEntry::new(
                GRAPH_MEMORY_DEGRADED_BELOW_PCT_KEY,
                percent.to_string(),
                self.source(GRAPH_MEMORY_DEGRADED_BELOW_PCT_KEY),
            ));
        }
        if let Some(basis_points) = self.values.graph.memory.growth_multiplier_basis_points {
            entries.push(ConfigShowEntry::new(
                GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS_KEY,
                basis_points.to_string(),
                self.source(GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS_KEY),
            ));
        }
        if let Some(days) = self.values.graph.witnesses.retention_days {
            entries.push(ConfigShowEntry::new(
                GRAPH_WITNESSES_RETENTION_DAYS_KEY,
                days.to_string(),
                self.source(GRAPH_WITNESSES_RETENTION_DAYS_KEY),
            ));
        }
        if let Some(ref overrides) = self.values.graph.witnesses.algorithm_ttl_days {
            entries.push(ConfigShowEntry::new(
                GRAPH_WITNESSES_ALGORITHM_TTL_DAYS_KEY,
                overrides.len().to_string(),
                self.source(GRAPH_WITNESSES_ALGORITHM_TTL_DAYS_KEY),
            ));
        }
        if let Some(enabled) = self.values.graph.feature.ppr_enabled {
            entries.push(ConfigShowEntry::new(
                GRAPH_FEATURE_PPR_ENABLED_KEY,
                enabled.to_string(),
                self.source(GRAPH_FEATURE_PPR_ENABLED_KEY),
            ));
        }
        if let Some(enabled) = self.values.graph.feature.pack_dna_enabled {
            entries.push(ConfigShowEntry::new(
                GRAPH_FEATURE_PACK_DNA_ENABLED_KEY,
                enabled.to_string(),
                self.source(GRAPH_FEATURE_PACK_DNA_ENABLED_KEY),
            ));
        }
        if let Some(enabled) = self.values.graph.feature.causal_explain_enabled {
            entries.push(ConfigShowEntry::new(
                GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY,
                enabled.to_string(),
                self.source(GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY),
            ));
        }
        if let Some(enabled) = self.values.graph.feature.structural_health_enabled {
            entries.push(ConfigShowEntry::new(
                GRAPH_FEATURE_STRUCTURAL_HEALTH_ENABLED_KEY,
                enabled.to_string(),
                self.source(GRAPH_FEATURE_STRUCTURAL_HEALTH_ENABLED_KEY),
            ));
        }
        if let Some(enabled) = self.values.graph.feature.structural_decay_enabled {
            entries.push(ConfigShowEntry::new(
                GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY,
                enabled.to_string(),
                self.source(GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY),
            ));
        }
        if let Some(enabled) = self.values.graph.feature.proximity_enabled {
            entries.push(ConfigShowEntry::new(
                GRAPH_FEATURE_PROXIMITY_ENABLED_KEY,
                enabled.to_string(),
                self.source(GRAPH_FEATURE_PROXIMITY_ENABLED_KEY),
            ));
        }
        if let Some(enabled) = self.values.graph.feature.revision_dominance_enabled {
            entries.push(ConfigShowEntry::new(
                GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY,
                enabled.to_string(),
                self.source(GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY),
            ));
        }
        if let Some(enabled) = self.values.graph.feature.skyline_enabled {
            entries.push(ConfigShowEntry::new(
                GRAPH_FEATURE_SKYLINE_ENABLED_KEY,
                enabled.to_string(),
                self.source(GRAPH_FEATURE_SKYLINE_ENABLED_KEY),
            ));
        }
        if let Some(enabled) = self.values.graph.feature.load_bearing_enabled {
            entries.push(ConfigShowEntry::new(
                GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY,
                enabled.to_string(),
                self.source(GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY),
            ));
        }
        if let Some(enabled) = self.values.graph.feature.hits_profiles_enabled {
            entries.push(ConfigShowEntry::new(
                GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY,
                enabled.to_string(),
                self.source(GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY),
            ));
        }

        // Curation section
        if let Some(sim) = self.values.curation.duplicate_similarity {
            entries.push(ConfigShowEntry::new(
                CURATION_DUPLICATE_SIMILARITY_KEY,
                sim.to_string(),
                self.source(CURATION_DUPLICATE_SIMILARITY_KEY),
            ));
        }
        if let Some(weight) = self.values.curation.harmful_weight {
            entries.push(ConfigShowEntry::new(
                CURATION_HARMFUL_WEIGHT_KEY,
                weight.to_string(),
                self.source(CURATION_HARMFUL_WEIGHT_KEY),
            ));
        }
        if let Some(days) = self.values.curation.decay_half_life_days {
            entries.push(ConfigShowEntry::new(
                CURATION_DECAY_HALF_LIFE_DAYS_KEY,
                days.to_string(),
                self.source(CURATION_DECAY_HALF_LIFE_DAYS_KEY),
            ));
        }
        if let Some(threshold) = self.values.curation.specificity_min {
            entries.push(ConfigShowEntry::new(
                CURATION_SPECIFICITY_MIN_KEY,
                threshold.to_string(),
                self.source(CURATION_SPECIFICITY_MIN_KEY),
            ));
        }

        // Learn section
        if let Some(threshold) = self.values.learn.cluster_coherence_threshold {
            entries.push(ConfigShowEntry::new(
                LEARN_CLUSTER_COHERENCE_THRESHOLD_KEY,
                threshold.to_string(),
                self.source(LEARN_CLUSTER_COHERENCE_THRESHOLD_KEY),
            ));
        }
        if let Some(threshold) = self.values.learn.decay.demote_threshold {
            entries.push(ConfigShowEntry::new(
                LEARN_DECAY_DEMOTE_THRESHOLD_KEY,
                threshold.to_string(),
                self.source(LEARN_DECAY_DEMOTE_THRESHOLD_KEY),
            ));
        }
        if let Some(threshold) = self.values.learn.decay.forget_threshold {
            entries.push(ConfigShowEntry::new(
                LEARN_DECAY_FORGET_THRESHOLD_KEY,
                threshold.to_string(),
                self.source(LEARN_DECAY_FORGET_THRESHOLD_KEY),
            ));
        }
        if let Some(days) = self.values.learn.decay.working_half_life_days {
            entries.push(ConfigShowEntry::new(
                LEARN_DECAY_WORKING_HALF_LIFE_DAYS_KEY,
                days.to_string(),
                self.source(LEARN_DECAY_WORKING_HALF_LIFE_DAYS_KEY),
            ));
        }
        if let Some(days) = self.values.learn.decay.episodic_event_half_life_days {
            entries.push(ConfigShowEntry::new(
                LEARN_DECAY_EPISODIC_EVENT_HALF_LIFE_DAYS_KEY,
                days.to_string(),
                self.source(LEARN_DECAY_EPISODIC_EVENT_HALF_LIFE_DAYS_KEY),
            ));
        }
        if let Some(days) = self.values.learn.decay.episodic_failure_half_life_days {
            entries.push(ConfigShowEntry::new(
                LEARN_DECAY_EPISODIC_FAILURE_HALF_LIFE_DAYS_KEY,
                days.to_string(),
                self.source(LEARN_DECAY_EPISODIC_FAILURE_HALF_LIFE_DAYS_KEY),
            ));
        }
        if let Some(days) = self.values.learn.decay.semantic_fact_half_life_days {
            entries.push(ConfigShowEntry::new(
                LEARN_DECAY_SEMANTIC_FACT_HALF_LIFE_DAYS_KEY,
                days.to_string(),
                self.source(LEARN_DECAY_SEMANTIC_FACT_HALF_LIFE_DAYS_KEY),
            ));
        }
        if let Some(days) = self.values.learn.decay.procedural_rule_half_life_days {
            entries.push(ConfigShowEntry::new(
                LEARN_DECAY_PROCEDURAL_RULE_HALF_LIFE_DAYS_KEY,
                days.to_string(),
                self.source(LEARN_DECAY_PROCEDURAL_RULE_HALF_LIFE_DAYS_KEY),
            ));
        }
        if let Some(days) = self.values.learn.decay.default_half_life_days {
            entries.push(ConfigShowEntry::new(
                LEARN_DECAY_DEFAULT_HALF_LIFE_DAYS_KEY,
                days.to_string(),
                self.source(LEARN_DECAY_DEFAULT_HALF_LIFE_DAYS_KEY),
            ));
        }

        // Feedback section
        if let Some(rate) = self.values.feedback.harmful_per_source_per_hour {
            entries.push(ConfigShowEntry::new(
                FEEDBACK_HARMFUL_PER_SOURCE_PER_HOUR_KEY,
                rate.to_string(),
                self.source(FEEDBACK_HARMFUL_PER_SOURCE_PER_HOUR_KEY),
            ));
        }
        if let Some(window) = self.values.feedback.harmful_burst_window_seconds {
            entries.push(ConfigShowEntry::new(
                FEEDBACK_HARMFUL_BURST_WINDOW_SECONDS_KEY,
                window.to_string(),
                self.source(FEEDBACK_HARMFUL_BURST_WINDOW_SECONDS_KEY),
            ));
        }

        // Redaction defaults section
        if let Some(level) = self.values.redaction.defaults.export {
            entries.push(ConfigShowEntry::new(
                REDACTION_DEFAULT_EXPORT_KEY,
                level.as_str().to_string(),
                self.source(REDACTION_DEFAULT_EXPORT_KEY),
            ));
        }
        if let Some(level) = self.values.redaction.defaults.handoff_create {
            entries.push(ConfigShowEntry::new(
                REDACTION_DEFAULT_HANDOFF_CREATE_KEY,
                level.as_str().to_string(),
                self.source(REDACTION_DEFAULT_HANDOFF_CREATE_KEY),
            ));
        }
        if let Some(level) = self.values.redaction.defaults.context_json {
            entries.push(ConfigShowEntry::new(
                REDACTION_DEFAULT_CONTEXT_JSON_KEY,
                level.as_str().to_string(),
                self.source(REDACTION_DEFAULT_CONTEXT_JSON_KEY),
            ));
        }
        if let Some(level) = self.values.redaction.defaults.support_bundle {
            entries.push(ConfigShowEntry::new(
                REDACTION_DEFAULT_SUPPORT_BUNDLE_KEY,
                level.as_str().to_string(),
                self.source(REDACTION_DEFAULT_SUPPORT_BUNDLE_KEY),
            ));
        }

        // Policy section
        if let Some(ref phrases) = self.values.policy.secret_detector.allow_phrases {
            entries.push(ConfigShowEntry::new(
                POLICY_SECRET_DETECTOR_ALLOW_PHRASES_KEY,
                phrases.join(","),
                self.source(POLICY_SECRET_DETECTOR_ALLOW_PHRASES_KEY),
            ));
        }
        if let Some(ref regexes) = self.values.policy.secret_detector.allow_regex {
            entries.push(ConfigShowEntry::new(
                POLICY_SECRET_DETECTOR_ALLOW_REGEX_KEY,
                regexes.join(","),
                self.source(POLICY_SECRET_DETECTOR_ALLOW_REGEX_KEY),
            ));
        }
        if let Some(enabled) = self.values.policy.output_redaction.enabled {
            entries.push(ConfigShowEntry::new(
                POLICY_OUTPUT_REDACTION_ENABLED_KEY,
                enabled.to_string(),
                self.source(POLICY_OUTPUT_REDACTION_ENABLED_KEY),
            ));
        }

        // Privacy section
        if let Some(redact) = self.values.privacy.redact_secrets {
            entries.push(ConfigShowEntry::new(
                PRIVACY_REDACT_SECRETS_KEY,
                redact.to_string(),
                self.source(PRIVACY_REDACT_SECRETS_KEY),
            ));
        }
        if let Some(ref classes) = self.values.privacy.redaction_classes {
            entries.push(ConfigShowEntry::new(
                PRIVACY_REDACTION_CLASSES_KEY,
                classes.join(","),
                self.source(PRIVACY_REDACTION_CLASSES_KEY),
            ));
        }

        // Trust section
        if let Some(ref class) = self.values.trust.default_class {
            entries.push(ConfigShowEntry::new(
                TRUST_DEFAULT_CLASS_KEY,
                class.clone(),
                self.source(TRUST_DEFAULT_CLASS_KEY),
            ));
        }
        if let Some(guard) = self.values.trust.prompt_injection_guard {
            entries.push(ConfigShowEntry::new(
                TRUST_PROMPT_INJECTION_GUARD_KEY,
                guard.to_string(),
                self.source(TRUST_PROMPT_INJECTION_GUARD_KEY),
            ));
        }
        if let Some(ref team_members) = self.values.trust.team_members {
            entries.push(ConfigShowEntry::new(
                TRUST_TEAM_MEMBERS_KEY,
                team_members.join(","),
                self.source(TRUST_TEAM_MEMBERS_KEY),
            ));
        }

        let entry_count = entries.len();
        ConfigShowReport {
            schema: "ee.config.show.v1",
            entries,
            entry_count,
        }
    }
}

/// A single config entry with source attribution.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConfigShowEntry {
    pub key: &'static str,
    pub value: String,
    pub source: &'static str,
}

impl ConfigShowEntry {
    #[must_use]
    pub fn new(key: &'static str, value: String, source: Option<ConfigValueSource>) -> Self {
        Self {
            key,
            value,
            source: source.map_or("unknown", ConfigValueSource::as_str),
        }
    }
}

/// Report showing merged config with source attribution for each key.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ConfigShowReport {
    pub schema: &'static str,
    pub entries: Vec<ConfigShowEntry>,
    pub entry_count: usize,
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
            read_pool: ReadPoolConfig {
                size: Some(1),
                idle_timeout_seconds: Some(30),
                max_pin_duration_seconds: Some(30),
                acquire_timeout_ms: Some(5000),
                pin_snapshot: Some(true),
            },
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
            lexical_ram_tier: SearchLexicalRamTierConfig {
                enabled: Some(false),
                request_hugepages: Some(false),
                populate_on_open: Some(true),
            },
        },
        pack: PackConfig {
            default_profile: Some("balanced".to_string()),
            default_format: Some("markdown".to_string()),
            default_max_tokens: Some(4000),
            adaptive_budget: Some(false),
            mmr_lambda: Some(0.7),
            candidate_pool: Some(100),
            memory_tier_admission: Some(false),
        },
        task_lens: TaskLensConfig::default(),
        handoff: HandoffConfig::default(),
        cache: CacheConfig {
            pack_l2: PackL2CacheConfig {
                enabled: Some(true),
                directory: Some(PathBuf::new()),
                max_bytes: Some(1_073_741_824),
                max_age_days: Some(30),
            },
        },
        mesh: MeshConfig {
            enabled: Some(false),
            command_mode: Some(MeshCommandMode::Off),
            peer_group_bindings: Some(Vec::new()),
            peer_policies: Some(Vec::new()),
        },
        swarm: SwarmConfig {
            adaptive: SwarmAdaptiveConfig {
                enabled: Some(false),
                prefetch_top_k: Some(3),
                prefetch_budget_ms: Some(50),
                similarity_threshold: Some(0.10),
                noisy_neighbor_p99_ms: Some(200),
                noisy_neighbor_backoff_ms: Some(25),
            },
        },
        graph: GraphConfig {
            ppr: GraphPprConfig { alpha: Some(0.30) },
            health: GraphHealthConfig {
                contradiction_threshold: Some(0.20),
            },
            curate: GraphCurateConfig {
                onion_decay_max: Some(3.0),
                articulation_protection_multiplier: Some(0.5),
            },
            hits: GraphHitsConfig {
                profile_boost: Some(0.5),
            },
            causal: GraphCausalConfig {
                min_cost_normalization: Some(1.0),
            },
            pack_dna: GraphPackDnaConfig {
                max_items: Some(10),
                max_edges: Some(30),
            },
            gomory_hu: GraphGomoryHuConfig {
                sample_threshold: Some(500),
                sample_size: Some(100),
            },
            memory: GraphMemoryConfig {
                snapshot_cap_mb: Some(250),
                per_algorithm_cap_mb: Some(100),
                degraded_below_pct: Some(80),
                growth_multiplier_basis_points: Some(15_000),
            },
            witnesses: GraphWitnessesConfig {
                retention_days: Some(30),
                algorithm_ttl_days: Some(BTreeMap::new()),
            },
            feature: GraphFeatureFlagsConfig {
                ppr_enabled: Some(false),
                pack_dna_enabled: Some(false),
                causal_explain_enabled: Some(false),
                structural_health_enabled: Some(false),
                structural_decay_enabled: Some(false),
                proximity_enabled: Some(false),
                revision_dominance_enabled: Some(false),
                skyline_enabled: Some(false),
                load_bearing_enabled: Some(false),
                hits_profiles_enabled: Some(false),
            },
        },
        curation: CurationConfig {
            duplicate_similarity: Some(0.92),
            harmful_weight: Some(2.5),
            decay_half_life_days: Some(60),
            specificity_min: Some(0.45),
        },
        learn: LearnConfig {
            cluster_coherence_threshold: Some(0.55),
            decay: LearnDecayConfig {
                demote_threshold: Some(0.05),
                forget_threshold: Some(0.01),
                working_half_life_days: Some(1.0),
                episodic_event_half_life_days: Some(30.0),
                episodic_failure_half_life_days: Some(90.0),
                semantic_fact_half_life_days: Some(180.0),
                procedural_rule_half_life_days: Some(365.0),
                default_half_life_days: Some(30.0),
            },
        },
        feedback: FeedbackConfig {
            harmful_per_source_per_hour: Some(5),
            harmful_burst_window_seconds: Some(3600),
        },
        redaction: RedactionConfig {
            defaults: RedactionDefaultsConfig {
                export: Some(RedactionLevel::Strict),
                handoff_create: Some(RedactionLevel::Standard),
                context_json: Some(RedactionLevel::Minimal),
                support_bundle: Some(RedactionLevel::Paranoid),
            },
        },
        policy: PolicyConfig {
            output_redaction: OutputRedactionConfig {
                enabled: Some(true),
            },
            ..PolicyConfig::default()
        },
        privacy: PrivacyConfig {
            redact_secrets: Some(true),
            redaction_classes: Some(vec![
                "api_key".to_string(),
                "jwt".to_string(),
                "password".to_string(),
                "private_key".to_string(),
                "ssh_key".to_string(),
                // SRR6.46.1 / bd-36bbk.1.1: tailscale identity material
                // (selfNodeKey, selfTailscaleIp, selfMagicDnsName, tailnetId,
                // tailnetDisplayName, selfAdvertisedTags, binaryVersionRaw,
                // binaryAbsolutePath) is sensitive in shared support bundles.
                // The strip pass for these fields lives in
                // src/obs/volatile_fields.rs (registered there as VOLATILE_FIELD_NAMES);
                // adding the class here tags any payload that contained
                // them with the well-known `tailscale_metadata` class name
                // so `ee support bundle --redaction standard|paranoid`
                // surfaces the redaction provenance in
                // ee.redaction.event.v1 records and the recorder
                // payload-class label.
                "tailscale_metadata".to_string(),
            ]),
        },
        trust: TrustConfig {
            default_class: Some("agent_assertion".to_string()),
            prompt_injection_guard: Some(true),
            team_members: None,
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
    let adaptive_enabled = optional_env_bool_flag(env, EnvVar::DisableAdaptive.name())?
        .and_then(|disabled| if disabled { Some(false) } else { None });

    Ok(ConfigFile {
        storage: StorageConfig {
            database_path: optional_env_path(env, EnvVar::DatabasePath.name(), expander)?,
            index_dir: optional_env_path(env, EnvVar::IndexDir.name(), expander)?,
            jsonl_export: None,
            read_pool: ReadPoolConfig {
                size: optional_env_u64(env, EnvVar::ReadPoolSize.name())?,
                idle_timeout_seconds: optional_env_u64(
                    env,
                    EnvVar::ReadPoolIdleTimeoutSeconds.name(),
                )?,
                max_pin_duration_seconds: optional_env_u64(
                    env,
                    EnvVar::ReadPoolMaxPinSeconds.name(),
                )?,
                acquire_timeout_ms: optional_env_u64(env, EnvVar::ReadPoolAcquireTimeoutMs.name())?,
                pin_snapshot: optional_env_bool(env, EnvVar::ReadPoolDisablePin.name())?
                    .map(|disabled| !disabled),
            },
        },
        runtime: RuntimeConfig::default(),
        cass: CassConfig::default(),
        search: SearchConfig {
            lexical_ram_tier: SearchLexicalRamTierConfig {
                enabled: optional_env_bool_flag(env, EnvVar::LexicalIndexPinRam.name())?,
                request_hugepages: optional_env_bool_flag(
                    env,
                    EnvVar::LexicalIndexHugepages.name(),
                )?,
                populate_on_open: None,
            },
            ..SearchConfig::default()
        },
        handoff: HandoffConfig::default(),
        cache: CacheConfig {
            pack_l2: PackL2CacheConfig {
                enabled: optional_env_bool(env, EnvVar::L2PackCacheDisable.name())?
                    .map(|disabled| !disabled),
                directory: optional_env_path(env, EnvVar::L2PackCacheDir.name(), expander)?,
                max_bytes: optional_env_u64(env, EnvVar::L2PackCacheBytes.name())?,
                max_age_days: None,
            },
        },
        mesh: MeshConfig {
            enabled: optional_env_bool(env, EnvVar::MeshEnabled.name())?,
            command_mode: optional_env_mesh_command_mode(env, EnvVar::MeshMode.name())?,
            peer_group_bindings: None,
            peer_policies: None,
        },
        swarm: SwarmConfig {
            adaptive: SwarmAdaptiveConfig {
                enabled: adaptive_enabled,
                prefetch_top_k: None,
                prefetch_budget_ms: None,
                similarity_threshold: None,
                noisy_neighbor_p99_ms: optional_env_u64(env, EnvVar::AdaptiveNoisyP99Ms.name())?,
                noisy_neighbor_backoff_ms: optional_env_u64(env, EnvVar::AdaptiveBackoffMs.name())?,
            },
        },
        graph: GraphConfig {
            memory: GraphMemoryConfig {
                snapshot_cap_mb: optional_env_u64(env, EnvVar::GraphMemorySnapshotCapMb.name())?,
                per_algorithm_cap_mb: optional_env_u64(
                    env,
                    EnvVar::GraphMemoryPerAlgorithmCapMb.name(),
                )?,
                degraded_below_pct: optional_env_u64(
                    env,
                    EnvVar::GraphMemoryDegradedBelowPct.name(),
                )?,
                growth_multiplier_basis_points: optional_env_u64(
                    env,
                    EnvVar::GraphMemoryGrowthMultiplierBasisPoints.name(),
                )?,
            },
            witnesses: GraphWitnessesConfig {
                retention_days: optional_env_u64(env, EnvVar::GraphWitnessesRetentionDays.name())?,
                algorithm_ttl_days: None,
            },
            ..GraphConfig::default()
        },
        pack: PackConfig {
            default_profile: optional_env_string(env, EnvVar::Profile.name())?,
            default_format: None,
            default_max_tokens: optional_env_u64(env, EnvVar::MaxTokens.name())?,
            adaptive_budget: None,
            mmr_lambda: None,
            candidate_pool: None,
            memory_tier_admission: None,
        },
        task_lens: TaskLensConfig::default(),
        curation: CurationConfig::default(),
        learn: LearnConfig::default(),
        feedback: FeedbackConfig {
            harmful_per_source_per_hour: optional_env_u64(
                env,
                EnvVar::HarmfulPerSourcePerHour.name(),
            )?,
            harmful_burst_window_seconds: optional_env_u64(
                env,
                EnvVar::HarmfulBurstWindowSeconds.name(),
            )?,
        },
        redaction: RedactionConfig::default(),
        policy: PolicyConfig::default(),
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
    InvalidBoolean {
        variable: &'static str,
        value: String,
    },
    InvalidMeshCommandMode {
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
            Self::InvalidBoolean { variable, value } => write!(
                formatter,
                "environment variable `{variable}` must be `true` or `false`, got `{value}`"
            ),
            Self::InvalidMeshCommandMode { variable, value } => write!(
                formatter,
                "environment variable `{variable}` must be one of `off`, `cache`, `revisable`, or `blocking`, got `{value}`"
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
            Self::InvalidUnicode { .. }
            | Self::InvalidUnsignedInteger { .. }
            | Self::InvalidBoolean { .. }
            | Self::InvalidMeshCommandMode { .. } => None,
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
            read_pool: ReadPoolConfig {
                size: pick_field(
                    &mut sources,
                    STORAGE_READ_POOL_SIZE_KEY,
                    &layers.cli.storage.read_pool.size,
                    &layers.environment.storage.read_pool.size,
                    &layers.project.storage.read_pool.size,
                    &layers.user.storage.read_pool.size,
                    &layers.defaults.storage.read_pool.size,
                ),
                idle_timeout_seconds: pick_field(
                    &mut sources,
                    STORAGE_READ_POOL_IDLE_TIMEOUT_SECONDS_KEY,
                    &layers.cli.storage.read_pool.idle_timeout_seconds,
                    &layers.environment.storage.read_pool.idle_timeout_seconds,
                    &layers.project.storage.read_pool.idle_timeout_seconds,
                    &layers.user.storage.read_pool.idle_timeout_seconds,
                    &layers.defaults.storage.read_pool.idle_timeout_seconds,
                ),
                max_pin_duration_seconds: pick_field(
                    &mut sources,
                    STORAGE_READ_POOL_MAX_PIN_DURATION_SECONDS_KEY,
                    &layers.cli.storage.read_pool.max_pin_duration_seconds,
                    &layers
                        .environment
                        .storage
                        .read_pool
                        .max_pin_duration_seconds,
                    &layers.project.storage.read_pool.max_pin_duration_seconds,
                    &layers.user.storage.read_pool.max_pin_duration_seconds,
                    &layers.defaults.storage.read_pool.max_pin_duration_seconds,
                ),
                acquire_timeout_ms: pick_field(
                    &mut sources,
                    STORAGE_READ_POOL_ACQUIRE_TIMEOUT_MS_KEY,
                    &layers.cli.storage.read_pool.acquire_timeout_ms,
                    &layers.environment.storage.read_pool.acquire_timeout_ms,
                    &layers.project.storage.read_pool.acquire_timeout_ms,
                    &layers.user.storage.read_pool.acquire_timeout_ms,
                    &layers.defaults.storage.read_pool.acquire_timeout_ms,
                ),
                pin_snapshot: pick_field(
                    &mut sources,
                    STORAGE_READ_POOL_PIN_SNAPSHOT_KEY,
                    &layers.cli.storage.read_pool.pin_snapshot,
                    &layers.environment.storage.read_pool.pin_snapshot,
                    &layers.project.storage.read_pool.pin_snapshot,
                    &layers.user.storage.read_pool.pin_snapshot,
                    &layers.defaults.storage.read_pool.pin_snapshot,
                ),
            },
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
            lexical_ram_tier: SearchLexicalRamTierConfig {
                enabled: pick_field(
                    &mut sources,
                    SEARCH_LEXICAL_RAM_TIER_ENABLED_KEY,
                    &layers.cli.search.lexical_ram_tier.enabled,
                    &layers.environment.search.lexical_ram_tier.enabled,
                    &layers.project.search.lexical_ram_tier.enabled,
                    &layers.user.search.lexical_ram_tier.enabled,
                    &layers.defaults.search.lexical_ram_tier.enabled,
                ),
                request_hugepages: pick_field(
                    &mut sources,
                    SEARCH_LEXICAL_RAM_TIER_REQUEST_HUGEPAGES_KEY,
                    &layers.cli.search.lexical_ram_tier.request_hugepages,
                    &layers.environment.search.lexical_ram_tier.request_hugepages,
                    &layers.project.search.lexical_ram_tier.request_hugepages,
                    &layers.user.search.lexical_ram_tier.request_hugepages,
                    &layers.defaults.search.lexical_ram_tier.request_hugepages,
                ),
                populate_on_open: pick_field(
                    &mut sources,
                    SEARCH_LEXICAL_RAM_TIER_POPULATE_ON_OPEN_KEY,
                    &layers.cli.search.lexical_ram_tier.populate_on_open,
                    &layers.environment.search.lexical_ram_tier.populate_on_open,
                    &layers.project.search.lexical_ram_tier.populate_on_open,
                    &layers.user.search.lexical_ram_tier.populate_on_open,
                    &layers.defaults.search.lexical_ram_tier.populate_on_open,
                ),
            },
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
            adaptive_budget: pick_field(
                &mut sources,
                PACK_ADAPTIVE_BUDGET_KEY,
                &layers.cli.pack.adaptive_budget,
                &layers.environment.pack.adaptive_budget,
                &layers.project.pack.adaptive_budget,
                &layers.user.pack.adaptive_budget,
                &layers.defaults.pack.adaptive_budget,
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
            memory_tier_admission: pick_field(
                &mut sources,
                PACK_MEMORY_TIER_ADMISSION_KEY,
                &layers.cli.pack.memory_tier_admission,
                &layers.environment.pack.memory_tier_admission,
                &layers.project.pack.memory_tier_admission,
                &layers.user.pack.memory_tier_admission,
                &layers.defaults.pack.memory_tier_admission,
            ),
        },
        task_lens: merge_task_lens_config(layers),
        handoff: HandoffConfig::default(),
        cache: CacheConfig {
            pack_l2: PackL2CacheConfig {
                enabled: pick_field(
                    &mut sources,
                    CACHE_PACK_L2_ENABLED_KEY,
                    &layers.cli.cache.pack_l2.enabled,
                    &layers.environment.cache.pack_l2.enabled,
                    &layers.project.cache.pack_l2.enabled,
                    &layers.user.cache.pack_l2.enabled,
                    &layers.defaults.cache.pack_l2.enabled,
                ),
                directory: pick_field(
                    &mut sources,
                    CACHE_PACK_L2_DIRECTORY_KEY,
                    &layers.cli.cache.pack_l2.directory,
                    &layers.environment.cache.pack_l2.directory,
                    &layers.project.cache.pack_l2.directory,
                    &layers.user.cache.pack_l2.directory,
                    &layers.defaults.cache.pack_l2.directory,
                ),
                max_bytes: pick_field(
                    &mut sources,
                    CACHE_PACK_L2_MAX_BYTES_KEY,
                    &layers.cli.cache.pack_l2.max_bytes,
                    &layers.environment.cache.pack_l2.max_bytes,
                    &layers.project.cache.pack_l2.max_bytes,
                    &layers.user.cache.pack_l2.max_bytes,
                    &layers.defaults.cache.pack_l2.max_bytes,
                ),
                max_age_days: pick_field(
                    &mut sources,
                    CACHE_PACK_L2_MAX_AGE_DAYS_KEY,
                    &layers.cli.cache.pack_l2.max_age_days,
                    &layers.environment.cache.pack_l2.max_age_days,
                    &layers.project.cache.pack_l2.max_age_days,
                    &layers.user.cache.pack_l2.max_age_days,
                    &layers.defaults.cache.pack_l2.max_age_days,
                ),
            },
        },
        mesh: MeshConfig {
            enabled: pick_field(
                &mut sources,
                MESH_ENABLED_KEY,
                &layers.cli.mesh.enabled,
                &layers.environment.mesh.enabled,
                &layers.project.mesh.enabled,
                &layers.user.mesh.enabled,
                &layers.defaults.mesh.enabled,
            ),
            command_mode: pick_field(
                &mut sources,
                MESH_COMMAND_MODE_KEY,
                &layers.cli.mesh.command_mode,
                &layers.environment.mesh.command_mode,
                &layers.project.mesh.command_mode,
                &layers.user.mesh.command_mode,
                &layers.defaults.mesh.command_mode,
            ),
            peer_group_bindings: pick_field(
                &mut sources,
                MESH_PEER_GROUP_BINDINGS_KEY,
                &layers.cli.mesh.peer_group_bindings,
                &layers.environment.mesh.peer_group_bindings,
                &layers.project.mesh.peer_group_bindings,
                &layers.user.mesh.peer_group_bindings,
                &layers.defaults.mesh.peer_group_bindings,
            ),
            peer_policies: pick_field(
                &mut sources,
                MESH_PEER_POLICIES_KEY,
                &layers.cli.mesh.peer_policies,
                &layers.environment.mesh.peer_policies,
                &layers.project.mesh.peer_policies,
                &layers.user.mesh.peer_policies,
                &layers.defaults.mesh.peer_policies,
            ),
        },
        swarm: SwarmConfig {
            adaptive: SwarmAdaptiveConfig {
                enabled: pick_field(
                    &mut sources,
                    SWARM_ADAPTIVE_ENABLED_KEY,
                    &layers.cli.swarm.adaptive.enabled,
                    &layers.environment.swarm.adaptive.enabled,
                    &layers.project.swarm.adaptive.enabled,
                    &layers.user.swarm.adaptive.enabled,
                    &layers.defaults.swarm.adaptive.enabled,
                ),
                prefetch_top_k: pick_field(
                    &mut sources,
                    SWARM_ADAPTIVE_PREFETCH_TOP_K_KEY,
                    &layers.cli.swarm.adaptive.prefetch_top_k,
                    &layers.environment.swarm.adaptive.prefetch_top_k,
                    &layers.project.swarm.adaptive.prefetch_top_k,
                    &layers.user.swarm.adaptive.prefetch_top_k,
                    &layers.defaults.swarm.adaptive.prefetch_top_k,
                ),
                prefetch_budget_ms: pick_field(
                    &mut sources,
                    SWARM_ADAPTIVE_PREFETCH_BUDGET_MS_KEY,
                    &layers.cli.swarm.adaptive.prefetch_budget_ms,
                    &layers.environment.swarm.adaptive.prefetch_budget_ms,
                    &layers.project.swarm.adaptive.prefetch_budget_ms,
                    &layers.user.swarm.adaptive.prefetch_budget_ms,
                    &layers.defaults.swarm.adaptive.prefetch_budget_ms,
                ),
                similarity_threshold: pick_field(
                    &mut sources,
                    SWARM_ADAPTIVE_SIMILARITY_THRESHOLD_KEY,
                    &layers.cli.swarm.adaptive.similarity_threshold,
                    &layers.environment.swarm.adaptive.similarity_threshold,
                    &layers.project.swarm.adaptive.similarity_threshold,
                    &layers.user.swarm.adaptive.similarity_threshold,
                    &layers.defaults.swarm.adaptive.similarity_threshold,
                ),
                noisy_neighbor_p99_ms: pick_field(
                    &mut sources,
                    SWARM_ADAPTIVE_NOISY_NEIGHBOR_P99_MS_KEY,
                    &layers.cli.swarm.adaptive.noisy_neighbor_p99_ms,
                    &layers.environment.swarm.adaptive.noisy_neighbor_p99_ms,
                    &layers.project.swarm.adaptive.noisy_neighbor_p99_ms,
                    &layers.user.swarm.adaptive.noisy_neighbor_p99_ms,
                    &layers.defaults.swarm.adaptive.noisy_neighbor_p99_ms,
                ),
                noisy_neighbor_backoff_ms: pick_field(
                    &mut sources,
                    SWARM_ADAPTIVE_NOISY_NEIGHBOR_BACKOFF_MS_KEY,
                    &layers.cli.swarm.adaptive.noisy_neighbor_backoff_ms,
                    &layers.environment.swarm.adaptive.noisy_neighbor_backoff_ms,
                    &layers.project.swarm.adaptive.noisy_neighbor_backoff_ms,
                    &layers.user.swarm.adaptive.noisy_neighbor_backoff_ms,
                    &layers.defaults.swarm.adaptive.noisy_neighbor_backoff_ms,
                ),
            },
        },
        graph: GraphConfig {
            ppr: GraphPprConfig {
                alpha: pick_field(
                    &mut sources,
                    GRAPH_PPR_ALPHA_KEY,
                    &layers.cli.graph.ppr.alpha,
                    &layers.environment.graph.ppr.alpha,
                    &layers.project.graph.ppr.alpha,
                    &layers.user.graph.ppr.alpha,
                    &layers.defaults.graph.ppr.alpha,
                ),
            },
            health: GraphHealthConfig {
                contradiction_threshold: pick_field(
                    &mut sources,
                    GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY,
                    &layers.cli.graph.health.contradiction_threshold,
                    &layers.environment.graph.health.contradiction_threshold,
                    &layers.project.graph.health.contradiction_threshold,
                    &layers.user.graph.health.contradiction_threshold,
                    &layers.defaults.graph.health.contradiction_threshold,
                ),
            },
            curate: GraphCurateConfig {
                onion_decay_max: pick_field(
                    &mut sources,
                    GRAPH_CURATE_ONION_DECAY_MAX_KEY,
                    &layers.cli.graph.curate.onion_decay_max,
                    &layers.environment.graph.curate.onion_decay_max,
                    &layers.project.graph.curate.onion_decay_max,
                    &layers.user.graph.curate.onion_decay_max,
                    &layers.defaults.graph.curate.onion_decay_max,
                ),
                articulation_protection_multiplier: pick_field(
                    &mut sources,
                    GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY,
                    &layers.cli.graph.curate.articulation_protection_multiplier,
                    &layers
                        .environment
                        .graph
                        .curate
                        .articulation_protection_multiplier,
                    &layers
                        .project
                        .graph
                        .curate
                        .articulation_protection_multiplier,
                    &layers.user.graph.curate.articulation_protection_multiplier,
                    &layers
                        .defaults
                        .graph
                        .curate
                        .articulation_protection_multiplier,
                ),
            },
            hits: GraphHitsConfig {
                profile_boost: pick_field(
                    &mut sources,
                    GRAPH_HITS_PROFILE_BOOST_KEY,
                    &layers.cli.graph.hits.profile_boost,
                    &layers.environment.graph.hits.profile_boost,
                    &layers.project.graph.hits.profile_boost,
                    &layers.user.graph.hits.profile_boost,
                    &layers.defaults.graph.hits.profile_boost,
                ),
            },
            causal: GraphCausalConfig {
                min_cost_normalization: pick_field(
                    &mut sources,
                    GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY,
                    &layers.cli.graph.causal.min_cost_normalization,
                    &layers.environment.graph.causal.min_cost_normalization,
                    &layers.project.graph.causal.min_cost_normalization,
                    &layers.user.graph.causal.min_cost_normalization,
                    &layers.defaults.graph.causal.min_cost_normalization,
                ),
            },
            pack_dna: GraphPackDnaConfig {
                max_items: pick_field(
                    &mut sources,
                    GRAPH_PACK_DNA_MAX_ITEMS_KEY,
                    &layers.cli.graph.pack_dna.max_items,
                    &layers.environment.graph.pack_dna.max_items,
                    &layers.project.graph.pack_dna.max_items,
                    &layers.user.graph.pack_dna.max_items,
                    &layers.defaults.graph.pack_dna.max_items,
                ),
                max_edges: pick_field(
                    &mut sources,
                    GRAPH_PACK_DNA_MAX_EDGES_KEY,
                    &layers.cli.graph.pack_dna.max_edges,
                    &layers.environment.graph.pack_dna.max_edges,
                    &layers.project.graph.pack_dna.max_edges,
                    &layers.user.graph.pack_dna.max_edges,
                    &layers.defaults.graph.pack_dna.max_edges,
                ),
            },
            gomory_hu: GraphGomoryHuConfig {
                sample_threshold: pick_field(
                    &mut sources,
                    GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY,
                    &layers.cli.graph.gomory_hu.sample_threshold,
                    &layers.environment.graph.gomory_hu.sample_threshold,
                    &layers.project.graph.gomory_hu.sample_threshold,
                    &layers.user.graph.gomory_hu.sample_threshold,
                    &layers.defaults.graph.gomory_hu.sample_threshold,
                ),
                sample_size: pick_field(
                    &mut sources,
                    GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY,
                    &layers.cli.graph.gomory_hu.sample_size,
                    &layers.environment.graph.gomory_hu.sample_size,
                    &layers.project.graph.gomory_hu.sample_size,
                    &layers.user.graph.gomory_hu.sample_size,
                    &layers.defaults.graph.gomory_hu.sample_size,
                ),
            },
            memory: GraphMemoryConfig {
                snapshot_cap_mb: pick_field(
                    &mut sources,
                    GRAPH_MEMORY_SNAPSHOT_CAP_MB_KEY,
                    &layers.cli.graph.memory.snapshot_cap_mb,
                    &layers.environment.graph.memory.snapshot_cap_mb,
                    &layers.project.graph.memory.snapshot_cap_mb,
                    &layers.user.graph.memory.snapshot_cap_mb,
                    &layers.defaults.graph.memory.snapshot_cap_mb,
                ),
                per_algorithm_cap_mb: pick_field(
                    &mut sources,
                    GRAPH_MEMORY_PER_ALGORITHM_CAP_MB_KEY,
                    &layers.cli.graph.memory.per_algorithm_cap_mb,
                    &layers.environment.graph.memory.per_algorithm_cap_mb,
                    &layers.project.graph.memory.per_algorithm_cap_mb,
                    &layers.user.graph.memory.per_algorithm_cap_mb,
                    &layers.defaults.graph.memory.per_algorithm_cap_mb,
                ),
                degraded_below_pct: pick_field(
                    &mut sources,
                    GRAPH_MEMORY_DEGRADED_BELOW_PCT_KEY,
                    &layers.cli.graph.memory.degraded_below_pct,
                    &layers.environment.graph.memory.degraded_below_pct,
                    &layers.project.graph.memory.degraded_below_pct,
                    &layers.user.graph.memory.degraded_below_pct,
                    &layers.defaults.graph.memory.degraded_below_pct,
                ),
                growth_multiplier_basis_points: pick_field(
                    &mut sources,
                    GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS_KEY,
                    &layers.cli.graph.memory.growth_multiplier_basis_points,
                    &layers
                        .environment
                        .graph
                        .memory
                        .growth_multiplier_basis_points,
                    &layers.project.graph.memory.growth_multiplier_basis_points,
                    &layers.user.graph.memory.growth_multiplier_basis_points,
                    &layers.defaults.graph.memory.growth_multiplier_basis_points,
                ),
            },
            witnesses: GraphWitnessesConfig {
                retention_days: pick_field(
                    &mut sources,
                    GRAPH_WITNESSES_RETENTION_DAYS_KEY,
                    &layers.cli.graph.witnesses.retention_days,
                    &layers.environment.graph.witnesses.retention_days,
                    &layers.project.graph.witnesses.retention_days,
                    &layers.user.graph.witnesses.retention_days,
                    &layers.defaults.graph.witnesses.retention_days,
                ),
                algorithm_ttl_days: pick_field(
                    &mut sources,
                    GRAPH_WITNESSES_ALGORITHM_TTL_DAYS_KEY,
                    &layers.cli.graph.witnesses.algorithm_ttl_days,
                    &layers.environment.graph.witnesses.algorithm_ttl_days,
                    &layers.project.graph.witnesses.algorithm_ttl_days,
                    &layers.user.graph.witnesses.algorithm_ttl_days,
                    &layers.defaults.graph.witnesses.algorithm_ttl_days,
                ),
            },
            feature: GraphFeatureFlagsConfig {
                ppr_enabled: pick_field(
                    &mut sources,
                    GRAPH_FEATURE_PPR_ENABLED_KEY,
                    &layers.cli.graph.feature.ppr_enabled,
                    &layers.environment.graph.feature.ppr_enabled,
                    &layers.project.graph.feature.ppr_enabled,
                    &layers.user.graph.feature.ppr_enabled,
                    &layers.defaults.graph.feature.ppr_enabled,
                ),
                pack_dna_enabled: pick_field(
                    &mut sources,
                    GRAPH_FEATURE_PACK_DNA_ENABLED_KEY,
                    &layers.cli.graph.feature.pack_dna_enabled,
                    &layers.environment.graph.feature.pack_dna_enabled,
                    &layers.project.graph.feature.pack_dna_enabled,
                    &layers.user.graph.feature.pack_dna_enabled,
                    &layers.defaults.graph.feature.pack_dna_enabled,
                ),
                causal_explain_enabled: pick_field(
                    &mut sources,
                    GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY,
                    &layers.cli.graph.feature.causal_explain_enabled,
                    &layers.environment.graph.feature.causal_explain_enabled,
                    &layers.project.graph.feature.causal_explain_enabled,
                    &layers.user.graph.feature.causal_explain_enabled,
                    &layers.defaults.graph.feature.causal_explain_enabled,
                ),
                structural_health_enabled: pick_field(
                    &mut sources,
                    GRAPH_FEATURE_STRUCTURAL_HEALTH_ENABLED_KEY,
                    &layers.cli.graph.feature.structural_health_enabled,
                    &layers.environment.graph.feature.structural_health_enabled,
                    &layers.project.graph.feature.structural_health_enabled,
                    &layers.user.graph.feature.structural_health_enabled,
                    &layers.defaults.graph.feature.structural_health_enabled,
                ),
                structural_decay_enabled: pick_field(
                    &mut sources,
                    GRAPH_FEATURE_STRUCTURAL_DECAY_ENABLED_KEY,
                    &layers.cli.graph.feature.structural_decay_enabled,
                    &layers.environment.graph.feature.structural_decay_enabled,
                    &layers.project.graph.feature.structural_decay_enabled,
                    &layers.user.graph.feature.structural_decay_enabled,
                    &layers.defaults.graph.feature.structural_decay_enabled,
                ),
                proximity_enabled: pick_field(
                    &mut sources,
                    GRAPH_FEATURE_PROXIMITY_ENABLED_KEY,
                    &layers.cli.graph.feature.proximity_enabled,
                    &layers.environment.graph.feature.proximity_enabled,
                    &layers.project.graph.feature.proximity_enabled,
                    &layers.user.graph.feature.proximity_enabled,
                    &layers.defaults.graph.feature.proximity_enabled,
                ),
                revision_dominance_enabled: pick_field(
                    &mut sources,
                    GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY,
                    &layers.cli.graph.feature.revision_dominance_enabled,
                    &layers.environment.graph.feature.revision_dominance_enabled,
                    &layers.project.graph.feature.revision_dominance_enabled,
                    &layers.user.graph.feature.revision_dominance_enabled,
                    &layers.defaults.graph.feature.revision_dominance_enabled,
                ),
                skyline_enabled: pick_field(
                    &mut sources,
                    GRAPH_FEATURE_SKYLINE_ENABLED_KEY,
                    &layers.cli.graph.feature.skyline_enabled,
                    &layers.environment.graph.feature.skyline_enabled,
                    &layers.project.graph.feature.skyline_enabled,
                    &layers.user.graph.feature.skyline_enabled,
                    &layers.defaults.graph.feature.skyline_enabled,
                ),
                load_bearing_enabled: pick_field(
                    &mut sources,
                    GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY,
                    &layers.cli.graph.feature.load_bearing_enabled,
                    &layers.environment.graph.feature.load_bearing_enabled,
                    &layers.project.graph.feature.load_bearing_enabled,
                    &layers.user.graph.feature.load_bearing_enabled,
                    &layers.defaults.graph.feature.load_bearing_enabled,
                ),
                hits_profiles_enabled: pick_field(
                    &mut sources,
                    GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY,
                    &layers.cli.graph.feature.hits_profiles_enabled,
                    &layers.environment.graph.feature.hits_profiles_enabled,
                    &layers.project.graph.feature.hits_profiles_enabled,
                    &layers.user.graph.feature.hits_profiles_enabled,
                    &layers.defaults.graph.feature.hits_profiles_enabled,
                ),
            },
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
        learn: LearnConfig {
            cluster_coherence_threshold: pick_field(
                &mut sources,
                LEARN_CLUSTER_COHERENCE_THRESHOLD_KEY,
                &layers.cli.learn.cluster_coherence_threshold,
                &layers.environment.learn.cluster_coherence_threshold,
                &layers.project.learn.cluster_coherence_threshold,
                &layers.user.learn.cluster_coherence_threshold,
                &layers.defaults.learn.cluster_coherence_threshold,
            ),
            decay: LearnDecayConfig {
                demote_threshold: pick_field(
                    &mut sources,
                    LEARN_DECAY_DEMOTE_THRESHOLD_KEY,
                    &layers.cli.learn.decay.demote_threshold,
                    &layers.environment.learn.decay.demote_threshold,
                    &layers.project.learn.decay.demote_threshold,
                    &layers.user.learn.decay.demote_threshold,
                    &layers.defaults.learn.decay.demote_threshold,
                ),
                forget_threshold: pick_field(
                    &mut sources,
                    LEARN_DECAY_FORGET_THRESHOLD_KEY,
                    &layers.cli.learn.decay.forget_threshold,
                    &layers.environment.learn.decay.forget_threshold,
                    &layers.project.learn.decay.forget_threshold,
                    &layers.user.learn.decay.forget_threshold,
                    &layers.defaults.learn.decay.forget_threshold,
                ),
                working_half_life_days: pick_field(
                    &mut sources,
                    LEARN_DECAY_WORKING_HALF_LIFE_DAYS_KEY,
                    &layers.cli.learn.decay.working_half_life_days,
                    &layers.environment.learn.decay.working_half_life_days,
                    &layers.project.learn.decay.working_half_life_days,
                    &layers.user.learn.decay.working_half_life_days,
                    &layers.defaults.learn.decay.working_half_life_days,
                ),
                episodic_event_half_life_days: pick_field(
                    &mut sources,
                    LEARN_DECAY_EPISODIC_EVENT_HALF_LIFE_DAYS_KEY,
                    &layers.cli.learn.decay.episodic_event_half_life_days,
                    &layers.environment.learn.decay.episodic_event_half_life_days,
                    &layers.project.learn.decay.episodic_event_half_life_days,
                    &layers.user.learn.decay.episodic_event_half_life_days,
                    &layers.defaults.learn.decay.episodic_event_half_life_days,
                ),
                episodic_failure_half_life_days: pick_field(
                    &mut sources,
                    LEARN_DECAY_EPISODIC_FAILURE_HALF_LIFE_DAYS_KEY,
                    &layers.cli.learn.decay.episodic_failure_half_life_days,
                    &layers
                        .environment
                        .learn
                        .decay
                        .episodic_failure_half_life_days,
                    &layers.project.learn.decay.episodic_failure_half_life_days,
                    &layers.user.learn.decay.episodic_failure_half_life_days,
                    &layers.defaults.learn.decay.episodic_failure_half_life_days,
                ),
                semantic_fact_half_life_days: pick_field(
                    &mut sources,
                    LEARN_DECAY_SEMANTIC_FACT_HALF_LIFE_DAYS_KEY,
                    &layers.cli.learn.decay.semantic_fact_half_life_days,
                    &layers.environment.learn.decay.semantic_fact_half_life_days,
                    &layers.project.learn.decay.semantic_fact_half_life_days,
                    &layers.user.learn.decay.semantic_fact_half_life_days,
                    &layers.defaults.learn.decay.semantic_fact_half_life_days,
                ),
                procedural_rule_half_life_days: pick_field(
                    &mut sources,
                    LEARN_DECAY_PROCEDURAL_RULE_HALF_LIFE_DAYS_KEY,
                    &layers.cli.learn.decay.procedural_rule_half_life_days,
                    &layers
                        .environment
                        .learn
                        .decay
                        .procedural_rule_half_life_days,
                    &layers.project.learn.decay.procedural_rule_half_life_days,
                    &layers.user.learn.decay.procedural_rule_half_life_days,
                    &layers.defaults.learn.decay.procedural_rule_half_life_days,
                ),
                default_half_life_days: pick_field(
                    &mut sources,
                    LEARN_DECAY_DEFAULT_HALF_LIFE_DAYS_KEY,
                    &layers.cli.learn.decay.default_half_life_days,
                    &layers.environment.learn.decay.default_half_life_days,
                    &layers.project.learn.decay.default_half_life_days,
                    &layers.user.learn.decay.default_half_life_days,
                    &layers.defaults.learn.decay.default_half_life_days,
                ),
            },
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
        redaction: RedactionConfig {
            defaults: RedactionDefaultsConfig {
                export: pick_field(
                    &mut sources,
                    REDACTION_DEFAULT_EXPORT_KEY,
                    &layers.cli.redaction.defaults.export,
                    &layers.environment.redaction.defaults.export,
                    &layers.project.redaction.defaults.export,
                    &layers.user.redaction.defaults.export,
                    &layers.defaults.redaction.defaults.export,
                ),
                handoff_create: pick_field(
                    &mut sources,
                    REDACTION_DEFAULT_HANDOFF_CREATE_KEY,
                    &layers.cli.redaction.defaults.handoff_create,
                    &layers.environment.redaction.defaults.handoff_create,
                    &layers.project.redaction.defaults.handoff_create,
                    &layers.user.redaction.defaults.handoff_create,
                    &layers.defaults.redaction.defaults.handoff_create,
                ),
                context_json: pick_field(
                    &mut sources,
                    REDACTION_DEFAULT_CONTEXT_JSON_KEY,
                    &layers.cli.redaction.defaults.context_json,
                    &layers.environment.redaction.defaults.context_json,
                    &layers.project.redaction.defaults.context_json,
                    &layers.user.redaction.defaults.context_json,
                    &layers.defaults.redaction.defaults.context_json,
                ),
                support_bundle: pick_field(
                    &mut sources,
                    REDACTION_DEFAULT_SUPPORT_BUNDLE_KEY,
                    &layers.cli.redaction.defaults.support_bundle,
                    &layers.environment.redaction.defaults.support_bundle,
                    &layers.project.redaction.defaults.support_bundle,
                    &layers.user.redaction.defaults.support_bundle,
                    &layers.defaults.redaction.defaults.support_bundle,
                ),
            },
        },
        policy: PolicyConfig {
            secret_detector: SecretDetectorConfig {
                allow_phrases: pick_field(
                    &mut sources,
                    POLICY_SECRET_DETECTOR_ALLOW_PHRASES_KEY,
                    &layers.cli.policy.secret_detector.allow_phrases,
                    &layers.environment.policy.secret_detector.allow_phrases,
                    &layers.project.policy.secret_detector.allow_phrases,
                    &layers.user.policy.secret_detector.allow_phrases,
                    &layers.defaults.policy.secret_detector.allow_phrases,
                ),
                allow_regex: pick_field(
                    &mut sources,
                    POLICY_SECRET_DETECTOR_ALLOW_REGEX_KEY,
                    &layers.cli.policy.secret_detector.allow_regex,
                    &layers.environment.policy.secret_detector.allow_regex,
                    &layers.project.policy.secret_detector.allow_regex,
                    &layers.user.policy.secret_detector.allow_regex,
                    &layers.defaults.policy.secret_detector.allow_regex,
                ),
            },
            output_redaction: OutputRedactionConfig {
                enabled: pick_field(
                    &mut sources,
                    POLICY_OUTPUT_REDACTION_ENABLED_KEY,
                    &layers.cli.policy.output_redaction.enabled,
                    &layers.environment.policy.output_redaction.enabled,
                    &layers.project.policy.output_redaction.enabled,
                    &layers.user.policy.output_redaction.enabled,
                    &layers.defaults.policy.output_redaction.enabled,
                ),
            },
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
            team_members: pick_field(
                &mut sources,
                TRUST_TEAM_MEMBERS_KEY,
                &layers.cli.trust.team_members,
                &layers.environment.trust.team_members,
                &layers.project.trust.team_members,
                &layers.user.trust.team_members,
                &layers.defaults.trust.team_members,
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

fn merge_task_lens_config(layers: &ConfigLayers) -> TaskLensConfig {
    let mut overrides = Vec::new();
    merge_task_lens_overrides(&mut overrides, &layers.defaults.task_lens.overrides);
    merge_task_lens_overrides(&mut overrides, &layers.user.task_lens.overrides);
    merge_task_lens_overrides(&mut overrides, &layers.project.task_lens.overrides);
    merge_task_lens_overrides(&mut overrides, &layers.environment.task_lens.overrides);
    merge_task_lens_overrides(&mut overrides, &layers.cli.task_lens.overrides);
    overrides.sort_by(|left, right| left.id.cmp(&right.id));
    TaskLensConfig { overrides }
}

fn merge_task_lens_overrides(merged: &mut Vec<TaskLens>, layer: &[TaskLens]) {
    for lens in layer {
        if let Some(existing) = merged.iter_mut().find(|existing| existing.id == lens.id) {
            *existing = lens.clone();
        } else {
            merged.push(lens.clone());
        }
    }
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

fn optional_env_bool(
    env: &BTreeMap<String, OsString>,
    variable: &'static str,
) -> Result<Option<bool>, EnvironmentConfigError> {
    let Some(value) = optional_env_string(env, variable)? else {
        return Ok(None);
    };
    match value.as_str() {
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        _ => Err(EnvironmentConfigError::InvalidBoolean { variable, value }),
    }
}

fn optional_env_bool_flag(
    env: &BTreeMap<String, OsString>,
    variable: &'static str,
) -> Result<Option<bool>, EnvironmentConfigError> {
    let Some(value) = optional_env_string(env, variable)? else {
        return Ok(None);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(Some(true)),
        "false" | "0" | "no" | "off" => Ok(Some(false)),
        _ => Err(EnvironmentConfigError::InvalidBoolean { variable, value }),
    }
}

fn optional_env_mesh_command_mode(
    env: &BTreeMap<String, OsString>,
    variable: &'static str,
) -> Result<Option<MeshCommandMode>, EnvironmentConfigError> {
    let Some(value) = optional_env_string(env, variable)? else {
        return Ok(None);
    };
    match value.parse::<MeshCommandMode>() {
        Ok(mode) => Ok(Some(mode)),
        Err(_) => Err(EnvironmentConfigError::InvalidMeshCommandMode { variable, value }),
    }
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
        CACHE_PACK_L2_DIRECTORY_KEY, CACHE_PACK_L2_ENABLED_KEY, CACHE_PACK_L2_MAX_AGE_DAYS_KEY,
        CACHE_PACK_L2_MAX_BYTES_KEY, CURATION_SPECIFICITY_MIN_KEY, ConfigLayers, ConfigValueSource,
        EnvironmentConfigError, GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY,
        GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY, GRAPH_CURATE_ONION_DECAY_MAX_KEY,
        GRAPH_FEATURE_PPR_ENABLED_KEY, GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY,
        GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY, GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY,
        GRAPH_HITS_PROFILE_BOOST_KEY, GRAPH_MEMORY_DEGRADED_BELOW_PCT_KEY,
        GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS_KEY, GRAPH_MEMORY_PER_ALGORITHM_CAP_MB_KEY,
        GRAPH_MEMORY_SNAPSHOT_CAP_MB_KEY, GRAPH_PACK_DNA_MAX_EDGES_KEY,
        GRAPH_PACK_DNA_MAX_ITEMS_KEY, GRAPH_PPR_ALPHA_KEY, LEARN_CLUSTER_COHERENCE_THRESHOLD_KEY,
        LEARN_DECAY_DEMOTE_THRESHOLD_KEY, LEARN_DECAY_PROCEDURAL_RULE_HALF_LIFE_DAYS_KEY,
        MESH_COMMAND_MODE_KEY, MESH_ENABLED_KEY, MESH_PEER_GROUP_BINDINGS_KEY,
        MESH_PEER_POLICIES_KEY, PACK_ADAPTIVE_BUDGET_KEY, PACK_DEFAULT_MAX_TOKENS_KEY,
        PACK_DEFAULT_PROFILE_KEY, PACK_MEMORY_TIER_ADMISSION_KEY,
        POLICY_SECRET_DETECTOR_ALLOW_PHRASES_KEY, SEARCH_DEFAULT_SPEED_KEY,
        SEARCH_LEXICAL_RAM_TIER_ENABLED_KEY, SEARCH_LEXICAL_RAM_TIER_POPULATE_ON_OPEN_KEY,
        SEARCH_LEXICAL_RAM_TIER_REQUEST_HUGEPAGES_KEY, STORAGE_DATABASE_PATH_KEY,
        STORAGE_INDEX_DIR_KEY, STORAGE_READ_POOL_ACQUIRE_TIMEOUT_MS_KEY,
        STORAGE_READ_POOL_IDLE_TIMEOUT_SECONDS_KEY, STORAGE_READ_POOL_MAX_PIN_DURATION_SECONDS_KEY,
        STORAGE_READ_POOL_PIN_SNAPSHOT_KEY, STORAGE_READ_POOL_SIZE_KEY, SWARM_ADAPTIVE_ENABLED_KEY,
        SWARM_ADAPTIVE_NOISY_NEIGHBOR_BACKOFF_MS_KEY, SWARM_ADAPTIVE_NOISY_NEIGHBOR_P99_MS_KEY,
        SWARM_ADAPTIVE_PREFETCH_BUDGET_MS_KEY, SWARM_ADAPTIVE_PREFETCH_TOP_K_KEY,
        SWARM_ADAPTIVE_SIMILARITY_THRESHOLD_KEY, built_in_config, config_from_env, merge_config,
    };
    use crate::config::file::TaskLensConfig;
    use crate::config::{
        CacheConfig, ConfigFile, CurationConfig, GraphConfig, GraphCurateConfig,
        GraphFeatureFlagsConfig, GraphGomoryHuConfig, GraphHealthConfig, GraphMemoryConfig,
        GraphPprConfig, LearnConfig, LearnDecayConfig, MeshCommandMode, MeshConfig, PackConfig,
        PackL2CacheConfig, PathExpander, PolicyConfig, ReadPoolConfig, SearchConfig,
        SearchLexicalRamTierConfig, SearchSpeed, SecretDetectorConfig, StorageConfig,
        SwarmAdaptiveConfig, SwarmConfig,
    };
    use crate::models::{TaskLens, TaskLensInput, TaskLensOverlay};

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

    fn task_lens_override(id: &str, version: u32, description: &str) -> Result<TaskLens, String> {
        TaskLens::new(TaskLensInput {
            id: id.to_string(),
            version,
            description: description.to_string(),
            overlay: TaskLensOverlay::default(),
        })
        .map_err(|error| error.to_string())
    }

    fn ensure_graph_feature_flags_default_disabled(config: &GraphFeatureFlagsConfig) -> TestResult {
        let flags = [
            ("ppr", config.ppr_enabled),
            ("pack_dna", config.pack_dna_enabled),
            ("causal_explain", config.causal_explain_enabled),
            ("structural_health", config.structural_health_enabled),
            ("structural_decay", config.structural_decay_enabled),
            ("proximity", config.proximity_enabled),
            ("revision_dominance", config.revision_dominance_enabled),
            ("skyline", config.skyline_enabled),
            ("load_bearing", config.load_bearing_enabled),
            ("hits_profiles", config.hits_profiles_enabled),
        ];
        for (name, value) in flags {
            ensure_equal(
                &value,
                &Some(false),
                &format!("graph feature {name} default"),
            )?;
        }
        Ok(())
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
        ensure_equal(
            &defaults.search.lexical_ram_tier.enabled,
            &Some(false),
            "lexical RAM tier default off",
        )?;
        ensure_equal(
            &defaults.search.lexical_ram_tier.request_hugepages,
            &Some(false),
            "lexical RAM tier hugepages default off",
        )?;
        ensure_equal(
            &defaults.search.lexical_ram_tier.populate_on_open,
            &Some(true),
            "lexical RAM tier populate default on",
        )?;
        ensure_equal(
            &defaults.pack.default_profile.as_deref(),
            &Some("balanced"),
            "default profile",
        )?;
        ensure_equal(&defaults.pack.default_max_tokens, &Some(4000), "max tokens")?;
        ensure_equal(
            &defaults.pack.adaptive_budget,
            &Some(false),
            "adaptive budget default off",
        )?;
        ensure_equal(
            &defaults.pack.memory_tier_admission,
            &Some(false),
            "memory tier admission default off",
        )?;
        ensure_equal(
            &defaults.cache.pack_l2.enabled,
            &Some(true),
            "pack L2 cache enabled",
        )?;
        ensure_equal(
            &defaults.cache.pack_l2.directory,
            &Some(PathBuf::new()),
            "pack L2 cache directory",
        )?;
        ensure_equal(
            &defaults.cache.pack_l2.max_bytes,
            &Some(1_073_741_824),
            "pack L2 cache max bytes",
        )?;
        ensure_equal(
            &defaults.cache.pack_l2.max_age_days,
            &Some(30),
            "pack L2 cache max age",
        )?;
        ensure_equal(&defaults.mesh.enabled, &Some(false), "mesh default off")?;
        ensure_equal(
            &defaults.mesh.command_mode,
            &Some(MeshCommandMode::Off),
            "mesh default command mode",
        )?;
        ensure_equal(
            &defaults.swarm.adaptive.enabled,
            &Some(false),
            "swarm adaptive default off",
        )?;
        ensure_equal(
            &defaults.swarm.adaptive.prefetch_top_k,
            &Some(3),
            "swarm adaptive prefetch top-k default",
        )?;
        ensure_equal(
            &defaults.swarm.adaptive.prefetch_budget_ms,
            &Some(50),
            "swarm adaptive prefetch budget default",
        )?;
        ensure_equal(
            &defaults.swarm.adaptive.similarity_threshold,
            &Some(0.10),
            "swarm adaptive similarity threshold default",
        )?;
        ensure_equal(
            &defaults.swarm.adaptive.noisy_neighbor_p99_ms,
            &Some(200),
            "swarm adaptive noisy-neighbor p99 default",
        )?;
        ensure_equal(
            &defaults.swarm.adaptive.noisy_neighbor_backoff_ms,
            &Some(25),
            "swarm adaptive noisy-neighbor backoff default",
        )?;
        ensure_equal(&defaults.storage.read_pool.size, &Some(1), "read pool size")?;
        ensure_equal(
            &defaults.storage.read_pool.idle_timeout_seconds,
            &Some(30),
            "read pool idle timeout",
        )?;
        ensure_equal(
            &defaults.storage.read_pool.max_pin_duration_seconds,
            &Some(30),
            "read pool max pin duration",
        )?;
        ensure_equal(
            &defaults.storage.read_pool.acquire_timeout_ms,
            &Some(5000),
            "read pool acquire timeout",
        )?;
        ensure_equal(
            &defaults.storage.read_pool.pin_snapshot,
            &Some(true),
            "read pool snapshot pinning",
        )?;
        ensure_equal(&defaults.graph.ppr.alpha, &Some(0.30), "graph ppr alpha")?;
        ensure_equal(
            &defaults.graph.health.contradiction_threshold,
            &Some(0.20),
            "graph contradiction threshold",
        )?;
        ensure_equal(
            &defaults.graph.curate.onion_decay_max,
            &Some(3.0),
            "graph onion decay max",
        )?;
        ensure_equal(
            &defaults.graph.gomory_hu.sample_threshold,
            &Some(500),
            "graph gomory-hu sample threshold",
        )?;
        ensure_equal(
            &defaults.graph.memory.snapshot_cap_mb,
            &Some(250),
            "graph memory snapshot cap",
        )?;
        ensure_equal(
            &defaults.graph.memory.per_algorithm_cap_mb,
            &Some(100),
            "graph memory per-algorithm cap",
        )?;
        ensure_equal(
            &defaults.graph.memory.degraded_below_pct,
            &Some(80),
            "graph memory degraded threshold",
        )?;
        ensure_equal(
            &defaults.graph.memory.growth_multiplier_basis_points,
            &Some(15_000),
            "graph memory growth multiplier",
        )?;
        ensure_graph_feature_flags_default_disabled(&defaults.graph.feature)?;
        ensure_equal(
            &defaults.curation.specificity_min,
            &Some(0.45),
            "specificity min",
        )?;
        ensure_equal(
            &defaults.learn.cluster_coherence_threshold,
            &Some(0.55),
            "cluster coherence threshold",
        )?;
        ensure_equal(
            &defaults.learn.decay.procedural_rule_half_life_days,
            &Some(365.0),
            "procedural rule half-life",
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
        env.insert("EE_READ_POOL_SIZE".to_string(), OsString::from("4"));
        env.insert(
            "EE_READ_POOL_IDLE_TIMEOUT_S".to_string(),
            OsString::from("120"),
        );
        env.insert(
            "EE_READ_POOL_MAX_PIN_SECONDS".to_string(),
            OsString::from("45"),
        );
        env.insert(
            "EE_READ_POOL_ACQUIRE_TIMEOUT_MS".to_string(),
            OsString::from("250"),
        );
        env.insert(
            "EE_READ_POOL_DISABLE_PIN".to_string(),
            OsString::from("true"),
        );
        env.insert(
            "EE_L2_PACK_CACHE_BYTES".to_string(),
            OsString::from("268435456"),
        );
        env.insert(
            "EE_L2_PACK_CACHE_DIR".to_string(),
            OsString::from("~/ee-pack-cache"),
        );
        env.insert(
            "EE_L2_PACK_CACHE_DISABLE".to_string(),
            OsString::from("true"),
        );
        env.insert("EE_MESH_ENABLED".to_string(), OsString::from("true"));
        env.insert("EE_MESH_MODE".to_string(), OsString::from("cache"));
        env.insert("EE_DISABLE_ADAPTIVE".to_string(), OsString::from("1"));
        env.insert(
            "EE_ADAPTIVE_NOISY_P99_MS".to_string(),
            OsString::from("275"),
        );
        env.insert("EE_ADAPTIVE_BACKOFF_MS".to_string(), OsString::from("40"));
        env.insert(
            "EE_GRAPH_MEMORY_SNAPSHOT_CAP_MB".to_string(),
            OsString::from("384"),
        );
        env.insert(
            "EE_GRAPH_MEMORY_PER_ALGORITHM_CAP_MB".to_string(),
            OsString::from("128"),
        );
        env.insert(
            "EE_GRAPH_MEMORY_DEGRADED_BELOW_PCT".to_string(),
            OsString::from("85"),
        );
        env.insert(
            "EE_GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS".to_string(),
            OsString::from("12500"),
        );
        env.insert("EE_LEXICAL_INDEX_PIN_RAM".to_string(), OsString::from("1"));
        env.insert(
            "EE_LEXICAL_INDEX_HUGEPAGES".to_string(),
            OsString::from("on"),
        );
        env.insert("EE_PROFILE".to_string(), OsString::from("thorough"));
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
            &Some("thorough"),
            "env profile",
        )?;
        ensure_equal(
            &parsed.pack.default_max_tokens,
            &Some(8192),
            "env max tokens",
        )?;
        ensure_equal(
            &parsed.storage.read_pool.size,
            &Some(4),
            "env read pool size",
        )?;
        ensure_equal(
            &parsed.storage.read_pool.idle_timeout_seconds,
            &Some(120),
            "env read pool idle timeout",
        )?;
        ensure_equal(
            &parsed.storage.read_pool.max_pin_duration_seconds,
            &Some(45),
            "env read pool max pin duration",
        )?;
        ensure_equal(
            &parsed.storage.read_pool.acquire_timeout_ms,
            &Some(250),
            "env read pool acquire timeout",
        )?;
        ensure_equal(
            &parsed.storage.read_pool.pin_snapshot,
            &Some(false),
            "env read pool disable pin inversion",
        )?;
        ensure_equal(
            &parsed.cache.pack_l2.max_bytes,
            &Some(268_435_456),
            "env L2 pack cache max bytes",
        )?;
        ensure_equal(
            &parsed.cache.pack_l2.directory,
            &Some(PathBuf::from("/home/agent/ee-pack-cache")),
            "env L2 pack cache directory",
        )?;
        ensure_equal(
            &parsed.cache.pack_l2.enabled,
            &Some(false),
            "env L2 pack cache disable inversion",
        )?;
        ensure_equal(&parsed.mesh.enabled, &Some(true), "env mesh enabled")?;
        ensure_equal(
            &parsed.mesh.command_mode,
            &Some(MeshCommandMode::Cache),
            "env mesh command mode",
        )?;
        ensure_equal(
            &parsed.swarm.adaptive.enabled,
            &Some(false),
            "env disable adaptive",
        )?;
        ensure_equal(
            &parsed.swarm.adaptive.noisy_neighbor_p99_ms,
            &Some(275),
            "env adaptive noisy-neighbor p99",
        )?;
        ensure_equal(
            &parsed.swarm.adaptive.noisy_neighbor_backoff_ms,
            &Some(40),
            "env adaptive backoff",
        )?;
        ensure_equal(
            &parsed.graph.memory.snapshot_cap_mb,
            &Some(384),
            "env graph memory snapshot cap",
        )?;
        ensure_equal(
            &parsed.graph.memory.per_algorithm_cap_mb,
            &Some(128),
            "env graph memory per-algorithm cap",
        )?;
        ensure_equal(
            &parsed.graph.memory.degraded_below_pct,
            &Some(85),
            "env graph memory degraded threshold",
        )?;
        ensure_equal(
            &parsed.graph.memory.growth_multiplier_basis_points,
            &Some(12_500),
            "env graph memory growth multiplier",
        )?;
        ensure_equal(
            &parsed.search.lexical_ram_tier.enabled,
            &Some(true),
            "env lexical RAM tier enabled",
        )?;
        ensure_equal(
            &parsed.search.lexical_ram_tier.request_hugepages,
            &Some(true),
            "env lexical RAM tier hugepages",
        )?;
        ensure_equal(
            &parsed.search.lexical_ram_tier.populate_on_open,
            &None,
            "env lexical RAM tier populate omitted",
        )
    }

    #[test]
    fn environment_layer_rejects_invalid_integer() -> TestResult {
        let mut env = BTreeMap::new();
        env.insert(
            "EE_ADAPTIVE_NOISY_P99_MS".to_string(),
            OsString::from("many"),
        );

        let error = match config_from_env(&env, &expander()) {
            Ok(config) => return Err(format!("expected env error, got {config:?}")),
            Err(error) => error,
        };

        ensure_equal(
            &error,
            &EnvironmentConfigError::InvalidUnsignedInteger {
                variable: "EE_ADAPTIVE_NOISY_P99_MS",
                value: "many".to_string(),
            },
            "invalid integer error",
        )
    }

    #[test]
    fn environment_layer_rejects_invalid_bool() -> TestResult {
        let mut env = BTreeMap::new();
        env.insert("EE_DISABLE_ADAPTIVE".to_string(), OsString::from("maybe"));

        let error = match config_from_env(&env, &expander()) {
            Ok(config) => return Err(format!("expected env error, got {config:?}")),
            Err(error) => error,
        };

        ensure_equal(
            &error,
            &EnvironmentConfigError::InvalidBoolean {
                variable: "EE_DISABLE_ADAPTIVE",
                value: "maybe".to_string(),
            },
            "invalid bool error",
        )
    }

    #[test]
    fn environment_disable_adaptive_false_is_noop() -> TestResult {
        let mut env = BTreeMap::new();
        env.insert("EE_DISABLE_ADAPTIVE".to_string(), OsString::from("0"));

        let parsed =
            config_from_env(&env, &expander()).map_err(|error| format!("env failed: {error}"))?;

        ensure_equal(
            &parsed.swarm.adaptive.enabled,
            &None,
            "false disable flag must not opt in adaptive scheduling",
        )
    }

    #[test]
    fn environment_layer_rejects_invalid_mesh_mode() -> TestResult {
        let mut env = BTreeMap::new();
        env.insert("EE_MESH_MODE".to_string(), OsString::from("auto"));

        let error = match config_from_env(&env, &expander()) {
            Ok(config) => return Err(format!("expected env error, got {config:?}")),
            Err(error) => error,
        };

        ensure_equal(
            &error,
            &EnvironmentConfigError::InvalidMeshCommandMode {
                variable: "EE_MESH_MODE",
                value: "auto".to_string(),
            },
            "invalid mesh mode",
        )
    }

    #[test]
    fn merge_uses_cli_environment_project_user_default_order() -> TestResult {
        let defaults =
            built_in_config(&expander()).map_err(|error| format!("defaults failed: {error}"))?;
        let user_bugfix = task_lens_override("bugfix", 1, "User bugfix lens.")?;
        let user_custom = task_lens_override("local-review", 1, "User local review lens.")?;
        let project_bugfix = task_lens_override("bugfix", 2, "Project bugfix lens.")?;
        let user = ConfigFile {
            storage: StorageConfig {
                database_path: Some(PathBuf::from("/user/ee.db")),
                ..StorageConfig::default()
            },
            search: SearchConfig {
                default_speed: Some(SearchSpeed::Fast),
                ..SearchConfig::default()
            },
            task_lens: TaskLensConfig {
                overrides: vec![user_bugfix, user_custom.clone()],
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
                lexical_ram_tier: SearchLexicalRamTierConfig {
                    enabled: Some(false),
                    request_hugepages: Some(false),
                    populate_on_open: Some(false),
                },
                ..SearchConfig::default()
            },
            pack: PackConfig {
                adaptive_budget: Some(true),
                memory_tier_admission: Some(true),
                ..PackConfig::default()
            },
            swarm: SwarmConfig {
                adaptive: SwarmAdaptiveConfig {
                    enabled: Some(true),
                    prefetch_top_k: Some(3),
                    prefetch_budget_ms: Some(50),
                    similarity_threshold: Some(0.20),
                    noisy_neighbor_p99_ms: Some(200),
                    noisy_neighbor_backoff_ms: Some(25),
                },
            },
            graph: GraphConfig {
                ppr: GraphPprConfig { alpha: Some(0.40) },
                health: GraphHealthConfig {
                    contradiction_threshold: Some(0.25),
                },
                curate: GraphCurateConfig {
                    onion_decay_max: Some(2.5),
                    ..GraphCurateConfig::default()
                },
                gomory_hu: GraphGomoryHuConfig {
                    sample_threshold: Some(750),
                    ..GraphGomoryHuConfig::default()
                },
                memory: GraphMemoryConfig {
                    snapshot_cap_mb: Some(384),
                    per_algorithm_cap_mb: Some(192),
                    ..GraphMemoryConfig::default()
                },
                feature: GraphFeatureFlagsConfig {
                    ppr_enabled: Some(true),
                    ..GraphFeatureFlagsConfig::default()
                },
                ..GraphConfig::default()
            },
            curation: CurationConfig {
                specificity_min: Some(0.60),
                ..CurationConfig::default()
            },
            learn: LearnConfig {
                cluster_coherence_threshold: Some(0.80),
                decay: LearnDecayConfig {
                    demote_threshold: Some(0.08),
                    procedural_rule_half_life_days: Some(730.0),
                    ..LearnDecayConfig::default()
                },
            },
            policy: PolicyConfig {
                secret_detector: SecretDetectorConfig {
                    allow_phrases: Some(vec!["OAuth refresh token".to_string()]),
                    ..SecretDetectorConfig::default()
                },
                ..PolicyConfig::default()
            },
            task_lens: TaskLensConfig {
                overrides: vec![project_bugfix.clone()],
            },
            ..ConfigFile::default()
        };
        let environment = ConfigFile {
            storage: StorageConfig {
                index_dir: Some(PathBuf::from("/env/index")),
                read_pool: ReadPoolConfig {
                    size: Some(8),
                    acquire_timeout_ms: Some(750),
                    ..ReadPoolConfig::default()
                },
                ..StorageConfig::default()
            },
            pack: PackConfig {
                default_profile: Some("env-profile".to_string()),
                ..PackConfig::default()
            },
            cache: CacheConfig {
                pack_l2: PackL2CacheConfig {
                    max_bytes: Some(268_435_456),
                    ..PackL2CacheConfig::default()
                },
            },
            mesh: MeshConfig {
                enabled: Some(true),
                command_mode: Some(MeshCommandMode::Cache),
                peer_group_bindings: None,
                peer_policies: None,
            },
            swarm: SwarmConfig {
                adaptive: SwarmAdaptiveConfig {
                    enabled: Some(false),
                    noisy_neighbor_p99_ms: Some(275),
                    noisy_neighbor_backoff_ms: Some(40),
                    ..SwarmAdaptiveConfig::default()
                },
            },
            graph: GraphConfig {
                memory: GraphMemoryConfig {
                    snapshot_cap_mb: Some(512),
                    ..GraphMemoryConfig::default()
                },
                ..GraphConfig::default()
            },
            search: SearchConfig {
                lexical_ram_tier: SearchLexicalRamTierConfig {
                    enabled: Some(true),
                    request_hugepages: Some(true),
                    populate_on_open: None,
                },
                ..SearchConfig::default()
            },
            ..ConfigFile::default()
        };
        let cli = ConfigFile {
            pack: PackConfig {
                default_profile: Some("cli-profile".to_string()),
                ..PackConfig::default()
            },
            search: SearchConfig {
                lexical_ram_tier: SearchLexicalRamTierConfig {
                    populate_on_open: Some(true),
                    ..SearchLexicalRamTierConfig::default()
                },
                ..SearchConfig::default()
            },
            swarm: SwarmConfig {
                adaptive: SwarmAdaptiveConfig {
                    prefetch_top_k: Some(5),
                    ..SwarmAdaptiveConfig::default()
                },
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
            &merged.values.storage.read_pool.size,
            &Some(8),
            "env read pool size",
        )?;
        ensure_equal(
            &merged.source(STORAGE_READ_POOL_SIZE_KEY),
            &Some(ConfigValueSource::Environment),
            "read pool size source",
        )?;
        ensure_equal(
            &merged.values.storage.read_pool.idle_timeout_seconds,
            &Some(30),
            "default read pool idle timeout",
        )?;
        ensure_equal(
            &merged.source(STORAGE_READ_POOL_IDLE_TIMEOUT_SECONDS_KEY),
            &Some(ConfigValueSource::Default),
            "read pool idle timeout source",
        )?;
        ensure_equal(
            &merged.values.storage.read_pool.max_pin_duration_seconds,
            &Some(30),
            "default read pool max pin duration",
        )?;
        ensure_equal(
            &merged.source(STORAGE_READ_POOL_MAX_PIN_DURATION_SECONDS_KEY),
            &Some(ConfigValueSource::Default),
            "read pool max pin duration source",
        )?;
        ensure_equal(
            &merged.values.storage.read_pool.acquire_timeout_ms,
            &Some(750),
            "env read pool acquire timeout",
        )?;
        ensure_equal(
            &merged.source(STORAGE_READ_POOL_ACQUIRE_TIMEOUT_MS_KEY),
            &Some(ConfigValueSource::Environment),
            "read pool acquire timeout source",
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
            &merged.values.search.lexical_ram_tier.enabled,
            &Some(true),
            "environment beats project lexical RAM tier enabled",
        )?;
        ensure_equal(
            &merged.source(SEARCH_LEXICAL_RAM_TIER_ENABLED_KEY),
            &Some(ConfigValueSource::Environment),
            "lexical RAM tier enabled source",
        )?;
        ensure_equal(
            &merged.values.search.lexical_ram_tier.request_hugepages,
            &Some(true),
            "environment lexical RAM tier hugepages",
        )?;
        ensure_equal(
            &merged.source(SEARCH_LEXICAL_RAM_TIER_REQUEST_HUGEPAGES_KEY),
            &Some(ConfigValueSource::Environment),
            "lexical RAM tier hugepages source",
        )?;
        ensure_equal(
            &merged.values.search.lexical_ram_tier.populate_on_open,
            &Some(true),
            "cli lexical RAM tier populate override",
        )?;
        ensure_equal(
            &merged.source(SEARCH_LEXICAL_RAM_TIER_POPULATE_ON_OPEN_KEY),
            &Some(ConfigValueSource::Cli),
            "lexical RAM tier populate source",
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
            &merged.values.pack.adaptive_budget,
            &Some(true),
            "project adaptive budget",
        )?;
        ensure_equal(
            &merged.source(PACK_ADAPTIVE_BUDGET_KEY),
            &Some(ConfigValueSource::Project),
            "adaptive budget source",
        )?;
        ensure_equal(
            &merged.values.pack.memory_tier_admission,
            &Some(true),
            "project memory tier admission",
        )?;
        ensure_equal(
            &merged.source(PACK_MEMORY_TIER_ADMISSION_KEY),
            &Some(ConfigValueSource::Project),
            "memory tier admission source",
        )?;
        let task_lens_ids: Vec<&str> = merged
            .values
            .task_lens
            .overrides
            .iter()
            .map(|lens| lens.id.as_str())
            .collect();
        ensure_equal(
            &task_lens_ids,
            &vec!["bugfix", "local-review"],
            "merged task lens ids",
        )?;
        let merged_bugfix = merged
            .values
            .task_lens
            .overrides
            .iter()
            .find(|lens| lens.id == "bugfix")
            .ok_or_else(|| "merged bugfix task lens missing".to_string())?;
        ensure_equal(
            &merged_bugfix.lens_hash,
            &project_bugfix.lens_hash,
            "project task lens replaces user lens with same id",
        )?;
        let merged_custom = merged
            .values
            .task_lens
            .overrides
            .iter()
            .find(|lens| lens.id == "local-review")
            .ok_or_else(|| "merged custom task lens missing".to_string())?;
        ensure_equal(
            &merged_custom.lens_hash,
            &user_custom.lens_hash,
            "user custom task lens is preserved",
        )?;
        ensure_equal(
            &merged.values.cache.pack_l2.max_bytes,
            &Some(268_435_456),
            "env L2 pack cache max bytes",
        )?;
        ensure_equal(
            &merged.source(CACHE_PACK_L2_MAX_BYTES_KEY),
            &Some(ConfigValueSource::Environment),
            "L2 pack cache max bytes source",
        )?;
        ensure_equal(
            &merged.values.cache.pack_l2.max_age_days,
            &Some(30),
            "default L2 pack cache max age",
        )?;
        ensure_equal(
            &merged.source(CACHE_PACK_L2_MAX_AGE_DAYS_KEY),
            &Some(ConfigValueSource::Default),
            "L2 pack cache max age source",
        )?;
        ensure_equal(&merged.values.mesh.enabled, &Some(true), "env mesh enabled")?;
        ensure_equal(
            &merged.source(MESH_ENABLED_KEY),
            &Some(ConfigValueSource::Environment),
            "mesh enabled source",
        )?;
        ensure_equal(
            &merged.values.mesh.command_mode,
            &Some(MeshCommandMode::Cache),
            "env mesh command mode",
        )?;
        ensure_equal(
            &merged.source(MESH_COMMAND_MODE_KEY),
            &Some(ConfigValueSource::Environment),
            "mesh command mode source",
        )?;
        ensure_equal(
            &merged.values.swarm.adaptive.enabled,
            &Some(false),
            "environment disables project adaptive opt-in",
        )?;
        ensure_equal(
            &merged.source(SWARM_ADAPTIVE_ENABLED_KEY),
            &Some(ConfigValueSource::Environment),
            "swarm adaptive enabled source",
        )?;
        ensure_equal(
            &merged.values.swarm.adaptive.prefetch_top_k,
            &Some(5),
            "cli adaptive prefetch top-k",
        )?;
        ensure_equal(
            &merged.source(SWARM_ADAPTIVE_PREFETCH_TOP_K_KEY),
            &Some(ConfigValueSource::Cli),
            "swarm adaptive prefetch top-k source",
        )?;
        ensure_equal(
            &merged.values.swarm.adaptive.prefetch_budget_ms,
            &Some(50),
            "project adaptive prefetch budget",
        )?;
        ensure_equal(
            &merged.source(SWARM_ADAPTIVE_PREFETCH_BUDGET_MS_KEY),
            &Some(ConfigValueSource::Project),
            "swarm adaptive prefetch budget source",
        )?;
        ensure_equal(
            &merged.values.swarm.adaptive.similarity_threshold,
            &Some(0.20),
            "project adaptive similarity threshold",
        )?;
        ensure_equal(
            &merged.source(SWARM_ADAPTIVE_SIMILARITY_THRESHOLD_KEY),
            &Some(ConfigValueSource::Project),
            "swarm adaptive similarity threshold source",
        )?;
        ensure_equal(
            &merged.values.swarm.adaptive.noisy_neighbor_p99_ms,
            &Some(275),
            "env adaptive noisy-neighbor p99",
        )?;
        ensure_equal(
            &merged.source(SWARM_ADAPTIVE_NOISY_NEIGHBOR_P99_MS_KEY),
            &Some(ConfigValueSource::Environment),
            "swarm adaptive noisy-neighbor p99 source",
        )?;
        ensure_equal(
            &merged.values.swarm.adaptive.noisy_neighbor_backoff_ms,
            &Some(40),
            "env adaptive noisy-neighbor backoff",
        )?;
        ensure_equal(
            &merged.source(SWARM_ADAPTIVE_NOISY_NEIGHBOR_BACKOFF_MS_KEY),
            &Some(ConfigValueSource::Environment),
            "swarm adaptive noisy-neighbor backoff source",
        )?;
        ensure_equal(
            &merged.values.graph.ppr.alpha,
            &Some(0.40),
            "project graph ppr alpha",
        )?;
        ensure_equal(
            &merged.source(GRAPH_PPR_ALPHA_KEY),
            &Some(ConfigValueSource::Project),
            "graph ppr alpha source",
        )?;
        ensure_equal(
            &merged.values.graph.health.contradiction_threshold,
            &Some(0.25),
            "project graph contradiction threshold",
        )?;
        ensure_equal(
            &merged.source(GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY),
            &Some(ConfigValueSource::Project),
            "graph contradiction threshold source",
        )?;
        ensure_equal(
            &merged.values.graph.curate.onion_decay_max,
            &Some(2.5),
            "project graph onion decay max",
        )?;
        ensure_equal(
            &merged.source(GRAPH_CURATE_ONION_DECAY_MAX_KEY),
            &Some(ConfigValueSource::Project),
            "graph onion decay max source",
        )?;
        ensure_equal(
            &merged.values.graph.gomory_hu.sample_threshold,
            &Some(750),
            "project graph gomory-hu sample threshold",
        )?;
        ensure_equal(
            &merged.source(GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY),
            &Some(ConfigValueSource::Project),
            "graph gomory-hu sample threshold source",
        )?;
        ensure_equal(
            &merged.values.graph.memory.snapshot_cap_mb,
            &Some(512),
            "env graph memory snapshot cap",
        )?;
        ensure_equal(
            &merged.source(GRAPH_MEMORY_SNAPSHOT_CAP_MB_KEY),
            &Some(ConfigValueSource::Environment),
            "graph memory snapshot cap source",
        )?;
        ensure_equal(
            &merged.values.graph.memory.per_algorithm_cap_mb,
            &Some(192),
            "project graph memory per-algorithm cap",
        )?;
        ensure_equal(
            &merged.source(GRAPH_MEMORY_PER_ALGORITHM_CAP_MB_KEY),
            &Some(ConfigValueSource::Project),
            "graph memory per-algorithm cap source",
        )?;
        ensure_equal(
            &merged.values.graph.memory.degraded_below_pct,
            &Some(80),
            "default graph memory advisory threshold",
        )?;
        ensure_equal(
            &merged.source(GRAPH_MEMORY_DEGRADED_BELOW_PCT_KEY),
            &Some(ConfigValueSource::Default),
            "graph memory advisory threshold source",
        )?;
        ensure_equal(
            &merged.values.graph.feature.ppr_enabled,
            &Some(true),
            "project graph feature ppr enabled",
        )?;
        ensure_equal(
            &merged.source(GRAPH_FEATURE_PPR_ENABLED_KEY),
            &Some(ConfigValueSource::Project),
            "graph feature ppr source",
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
        )?;
        ensure_equal(
            &merged.values.learn.cluster_coherence_threshold,
            &Some(0.80),
            "project learn cluster coherence threshold",
        )?;
        ensure_equal(
            &merged.source(LEARN_CLUSTER_COHERENCE_THRESHOLD_KEY),
            &Some(ConfigValueSource::Project),
            "learn cluster coherence threshold source",
        )?;
        ensure_equal(
            &merged.values.learn.decay.demote_threshold,
            &Some(0.08),
            "project learn decay demote threshold",
        )?;
        ensure_equal(
            &merged.source(LEARN_DECAY_DEMOTE_THRESHOLD_KEY),
            &Some(ConfigValueSource::Project),
            "learn decay demote threshold source",
        )?;
        ensure_equal(
            &merged.values.learn.decay.procedural_rule_half_life_days,
            &Some(730.0),
            "project procedural rule half-life",
        )?;
        ensure_equal(
            &merged.source(LEARN_DECAY_PROCEDURAL_RULE_HALF_LIFE_DAYS_KEY),
            &Some(ConfigValueSource::Project),
            "procedural rule half-life source",
        )?;
        ensure_equal(
            &merged.values.policy.secret_detector.allow_phrases,
            &Some(vec!["OAuth refresh token".to_string()]),
            "project policy allow phrase",
        )?;
        ensure_equal(
            &merged.source(POLICY_SECRET_DETECTOR_ALLOW_PHRASES_KEY),
            &Some(ConfigValueSource::Project),
            "policy allow phrase source",
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

    #[test]
    fn show_report_includes_graph_threshold_keys() -> TestResult {
        let defaults =
            built_in_config(&expander()).map_err(|error| format!("defaults failed: {error}"))?;
        let report = merge_config(&ConfigLayers::with_defaults(defaults)).to_show_report();
        let keys: Vec<&str> = report.entries.iter().map(|entry| entry.key).collect();

        for expected in [
            CACHE_PACK_L2_ENABLED_KEY,
            CACHE_PACK_L2_DIRECTORY_KEY,
            CACHE_PACK_L2_MAX_BYTES_KEY,
            CACHE_PACK_L2_MAX_AGE_DAYS_KEY,
            SWARM_ADAPTIVE_ENABLED_KEY,
            SWARM_ADAPTIVE_PREFETCH_TOP_K_KEY,
            SWARM_ADAPTIVE_PREFETCH_BUDGET_MS_KEY,
            SWARM_ADAPTIVE_SIMILARITY_THRESHOLD_KEY,
            SWARM_ADAPTIVE_NOISY_NEIGHBOR_P99_MS_KEY,
            SWARM_ADAPTIVE_NOISY_NEIGHBOR_BACKOFF_MS_KEY,
            GRAPH_PPR_ALPHA_KEY,
            GRAPH_HEALTH_CONTRADICTION_THRESHOLD_KEY,
            GRAPH_CURATE_ONION_DECAY_MAX_KEY,
            GRAPH_CURATE_ARTICULATION_PROTECTION_MULTIPLIER_KEY,
            GRAPH_HITS_PROFILE_BOOST_KEY,
            GRAPH_CAUSAL_MIN_COST_NORMALIZATION_KEY,
            GRAPH_PACK_DNA_MAX_ITEMS_KEY,
            GRAPH_PACK_DNA_MAX_EDGES_KEY,
            GRAPH_GOMORY_HU_SAMPLE_THRESHOLD_KEY,
            GRAPH_GOMORY_HU_SAMPLE_SIZE_KEY,
            GRAPH_MEMORY_SNAPSHOT_CAP_MB_KEY,
            GRAPH_MEMORY_PER_ALGORITHM_CAP_MB_KEY,
            GRAPH_MEMORY_DEGRADED_BELOW_PCT_KEY,
            GRAPH_MEMORY_GROWTH_MULTIPLIER_BASIS_POINTS_KEY,
            MESH_ENABLED_KEY,
            MESH_COMMAND_MODE_KEY,
            MESH_PEER_GROUP_BINDINGS_KEY,
            MESH_PEER_POLICIES_KEY,
            PACK_ADAPTIVE_BUDGET_KEY,
            STORAGE_READ_POOL_SIZE_KEY,
            STORAGE_READ_POOL_IDLE_TIMEOUT_SECONDS_KEY,
            STORAGE_READ_POOL_MAX_PIN_DURATION_SECONDS_KEY,
            STORAGE_READ_POOL_PIN_SNAPSHOT_KEY,
            SEARCH_LEXICAL_RAM_TIER_ENABLED_KEY,
            SEARCH_LEXICAL_RAM_TIER_REQUEST_HUGEPAGES_KEY,
            SEARCH_LEXICAL_RAM_TIER_POPULATE_ON_OPEN_KEY,
        ] {
            if !keys.contains(&expected) {
                return Err(format!("show report missing {expected}"));
            }
        }

        Ok(())
    }

    #[test]
    fn show_report_covers_every_resolved_source_key() -> TestResult {
        let defaults =
            built_in_config(&expander()).map_err(|error| format!("defaults failed: {error}"))?;
        let merged = merge_config(&ConfigLayers::with_defaults(defaults));
        let report = merged.to_show_report();
        let report_keys: Vec<&str> = report.entries.iter().map(|entry| entry.key).collect();

        for key in merged.sources().keys() {
            if !report_keys.contains(key) {
                return Err(format!("show report missing resolved source key {key}"));
            }
        }
        ensure_equal(&report.entry_count, &report.entries.len(), "entry count")
    }
}
