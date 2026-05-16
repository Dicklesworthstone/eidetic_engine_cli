//! TOML config file parsing (EE-021).
//!
//! Config files are intentionally parsed into optional, typed fields.
//! Precedence merging lives in the next layer; this module only answers
//! "what did this file say?" with deterministic validation errors.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use regex_lite::Regex;
use toml_edit::{DocumentMut, Item, Table, Value};

use super::path::{PathExpander, PathExpansionError};

/// Parsed `.ee/config.toml` or user config file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigFile {
    pub storage: StorageConfig,
    pub runtime: RuntimeConfig,
    pub cass: CassConfig,
    pub search: SearchConfig,
    pub pack: PackConfig,
    pub handoff: HandoffConfig,
    pub cache: CacheConfig,
    pub mesh: MeshConfig,
    pub graph: GraphConfig,
    pub curation: CurationConfig,
    pub learn: LearnConfig,
    pub feedback: FeedbackConfig,
    pub policy: PolicyConfig,
    pub privacy: PrivacyConfig,
    pub trust: TrustConfig,
}

impl ConfigFile {
    /// Parse a TOML config string without expanding storage paths.
    ///
    /// Path values are returned lexically. Call
    /// [`ConfigFile::parse_with_expander`] when user/home/env expansion is
    /// required.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigParseError`] when TOML syntax is invalid or a known
    /// key has the wrong type/value.
    pub fn parse(input: &str) -> Result<Self, ConfigParseError> {
        Self::parse_inner(input, None)
    }

    /// Parse a TOML config string and expand path-like storage values.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigParseError`] when TOML syntax is invalid, a known
    /// key has the wrong type/value, or path expansion fails.
    pub fn parse_with_expander(
        input: &str,
        expander: &PathExpander,
    ) -> Result<Self, ConfigParseError> {
        Self::parse_inner(input, Some(expander))
    }

    fn parse_inner(input: &str, expander: Option<&PathExpander>) -> Result<Self, ConfigParseError> {
        let document = input
            .parse::<DocumentMut>()
            .map_err(|source| ConfigParseError::Toml {
                message: source.to_string(),
            })?;

        Ok(Self {
            storage: StorageConfig::parse(&document, expander)?,
            runtime: RuntimeConfig::parse(&document)?,
            cass: CassConfig::parse(&document)?,
            search: SearchConfig::parse(&document)?,
            pack: PackConfig::parse(&document)?,
            handoff: HandoffConfig::parse(&document)?,
            cache: CacheConfig::parse(&document, expander)?,
            mesh: MeshConfig::parse(&document)?,
            graph: GraphConfig::parse(&document)?,
            curation: CurationConfig::parse(&document)?,
            learn: LearnConfig::parse(&document)?,
            feedback: FeedbackConfig::parse(&document)?,
            policy: PolicyConfig::parse(&document)?,
            privacy: PrivacyConfig::parse(&document)?,
            trust: TrustConfig::parse(&document)?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StorageConfig {
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub jsonl_export: Option<bool>,
    pub read_pool: ReadPoolConfig,
}

impl StorageConfig {
    fn parse(
        document: &DocumentMut,
        expander: Option<&PathExpander>,
    ) -> Result<Self, ConfigParseError> {
        Ok(Self {
            database_path: optional_path(document, "storage", "database_path", expander)?,
            index_dir: optional_path(document, "storage", "index_dir", expander)?,
            jsonl_export: optional_bool(document, "storage", "jsonl_export")?,
            read_pool: ReadPoolConfig::parse(document)?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReadPoolConfig {
    pub size: Option<u64>,
    pub idle_timeout_seconds: Option<u64>,
    pub pin_snapshot: Option<bool>,
}

impl ReadPoolConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        const SECTIONS: &[&str] = &["storage", "read_pool"];

        Ok(Self {
            size: optional_u64_path(document, SECTIONS, "size")?,
            idle_timeout_seconds: optional_u64_path(document, SECTIONS, "idle_timeout_seconds")?,
            pin_snapshot: optional_bool_path(document, SECTIONS, "pin_snapshot")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub daemon: Option<bool>,
    pub job_budget_ms: Option<u64>,
    pub import_batch_size: Option<u64>,
}

impl RuntimeConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            daemon: optional_bool(document, "runtime", "daemon")?,
            job_budget_ms: optional_u64(document, "runtime", "job_budget_ms")?,
            import_batch_size: optional_u64(document, "runtime", "import_batch_size")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CassConfig {
    pub enabled: Option<bool>,
    pub binary: Option<String>,
    pub since: Option<String>,
}

impl CassConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            enabled: optional_bool(document, "cass", "enabled")?,
            binary: optional_string(document, "cass", "binary")?,
            since: optional_string(document, "cass", "since")?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchConfig {
    pub default_speed: Option<SearchSpeed>,
    pub lexical_weight: Option<f64>,
    pub semantic_weight: Option<f64>,
    pub graph_weight: Option<f64>,
}

impl SearchConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            default_speed: optional_search_speed(document, "search", "default_speed")?,
            lexical_weight: optional_unit_float(document, "search", "lexical_weight")?,
            semantic_weight: optional_unit_float(document, "search", "semantic_weight")?,
            graph_weight: optional_unit_float(document, "search", "graph_weight")?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSpeed {
    Fast,
    Balanced,
    Thorough,
}

impl SearchSpeed {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Thorough => "thorough",
        }
    }
}

impl FromStr for SearchSpeed {
    type Err = ConfigParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "fast" => Ok(Self::Fast),
            "balanced" => Ok(Self::Balanced),
            "thorough" => Ok(Self::Thorough),
            other => Err(ConfigParseError::InvalidValue {
                key: "search.default_speed".to_string(),
                value: other.to_string(),
                message: "expected one of `fast`, `balanced`, or `thorough`".to_string(),
            }),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackConfig {
    pub default_profile: Option<String>,
    pub default_format: Option<String>,
    pub default_max_tokens: Option<u64>,
    pub mmr_lambda: Option<f64>,
    pub candidate_pool: Option<u64>,
}

impl PackConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            default_profile: optional_string(document, "pack", "default_profile")?,
            default_format: optional_string(document, "pack", "default_format")?,
            default_max_tokens: optional_u64(document, "pack", "default_max_tokens")?,
            mmr_lambda: optional_unit_float(document, "pack", "mmr_lambda")?,
            candidate_pool: optional_u64(document, "pack", "candidate_pool")?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HandoffConfig {
    pub stale_threshold: HandoffStaleThresholdConfig,
}

impl HandoffConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            stale_threshold: HandoffStaleThresholdConfig::parse(document)?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HandoffStaleThresholdConfig {
    pub memories_added: Option<u64>,
    pub any_expired_in_pack: Option<bool>,
    pub content_drift_score: Option<f64>,
    pub memories_revised: Option<u64>,
}

impl HandoffStaleThresholdConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        const SECTIONS: &[&str] = &["handoff", "stale_threshold"];
        Ok(Self {
            memories_added: optional_u64_path(document, SECTIONS, "memories_added")?,
            any_expired_in_pack: optional_bool_path(document, SECTIONS, "any_expired_in_pack")?,
            content_drift_score: optional_unit_float_path(
                document,
                SECTIONS,
                "content_drift_score",
            )?,
            memories_revised: optional_u64_path(document, SECTIONS, "memories_revised")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CacheConfig {
    pub pack_l2: PackL2CacheConfig,
}

impl CacheConfig {
    fn parse(
        document: &DocumentMut,
        expander: Option<&PathExpander>,
    ) -> Result<Self, ConfigParseError> {
        Ok(Self {
            pack_l2: PackL2CacheConfig::parse(document, expander)?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackL2CacheConfig {
    pub enabled: Option<bool>,
    pub directory: Option<PathBuf>,
    pub max_bytes: Option<u64>,
    pub max_age_days: Option<u64>,
}

impl PackL2CacheConfig {
    fn parse(
        document: &DocumentMut,
        expander: Option<&PathExpander>,
    ) -> Result<Self, ConfigParseError> {
        const SECTIONS: &[&str] = &["cache", "pack_l2"];

        Ok(Self {
            enabled: optional_bool_path(document, SECTIONS, "enabled")?,
            directory: optional_path_path(document, SECTIONS, "directory", expander)?,
            max_bytes: optional_u64_path(document, SECTIONS, "max_bytes")?,
            max_age_days: optional_u64_path(document, SECTIONS, "max_age_days")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MeshConfig {
    pub enabled: Option<bool>,
    pub command_mode: Option<MeshCommandMode>,
    pub peer_group_bindings: Option<Vec<MeshPeerGroupBinding>>,
}

impl MeshConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            enabled: optional_bool(document, "mesh", "enabled")?,
            command_mode: optional_mesh_command_mode(document, "mesh", "command_mode")?,
            peer_group_bindings: optional_peer_group_bindings(document)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MeshCommandMode {
    #[default]
    Off,
    Cache,
    Revisable,
    Blocking,
}

impl MeshCommandMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Cache => "cache",
            Self::Revisable => "revisable",
            Self::Blocking => "blocking",
        }
    }
}

impl FromStr for MeshCommandMode {
    type Err = ConfigParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "off" => Ok(Self::Off),
            "cache" => Ok(Self::Cache),
            "revisable" => Ok(Self::Revisable),
            "blocking" => Ok(Self::Blocking),
            other => Err(ConfigParseError::InvalidValue {
                key: "mesh.command_mode".to_string(),
                value: other.to_string(),
                message: "expected one of `off`, `cache`, `revisable`, or `blocking`".to_string(),
            }),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MeshPeerGroupBinding {
    pub workspace_id: Option<String>,
    pub workspace_alias: Option<String>,
    pub peer_group_id: Option<String>,
    pub peer_group_label: Option<String>,
    pub peer_ids: Option<Vec<String>>,
    pub origin_workspace_ids: Option<Vec<String>>,
    pub lanes: MeshLaneGrants,
    pub default_action: Option<MeshLaneDecision>,
}

impl MeshPeerGroupBinding {
    #[must_use]
    pub fn decision_for(
        &self,
        local_workspace_id: &str,
        peer_id: &str,
        origin_workspace_id: &str,
        lane: MeshLane,
    ) -> MeshLaneDecision {
        if self.workspace_id.as_deref() != Some(local_workspace_id) {
            return MeshLaneDecision::Deny;
        }
        if !self
            .peer_ids
            .as_ref()
            .is_some_and(|peers| peers.iter().any(|known| known == peer_id))
        {
            return MeshLaneDecision::Deny;
        }
        if !self
            .origin_workspace_ids
            .as_ref()
            .is_some_and(|origins| origins.iter().any(|known| known == origin_workspace_id))
        {
            return MeshLaneDecision::Deny;
        }
        self.lanes.decision(lane)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshLane {
    Metadata,
    Body,
    Embedding,
    GraphLink,
    RevisionNotice,
    CurationSignal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MeshLaneDecision {
    Allow,
    Quarantine,
    #[default]
    Deny,
}

impl MeshLaneDecision {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Quarantine => "quarantine",
            Self::Deny => "deny",
        }
    }

    fn parse_for_key(input: &str, key: String) -> Result<Self, ConfigParseError> {
        match input {
            "allow" => Ok(Self::Allow),
            "quarantine" => Ok(Self::Quarantine),
            "deny" => Ok(Self::Deny),
            other => Err(ConfigParseError::InvalidValue {
                key,
                value: other.to_string(),
                message: "expected one of `allow`, `quarantine`, or `deny`".to_string(),
            }),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MeshLaneGrants {
    pub metadata: Option<MeshLaneDecision>,
    pub body: Option<MeshLaneDecision>,
    pub embedding: Option<MeshLaneDecision>,
    pub graph_link: Option<MeshLaneDecision>,
    pub revision_notice: Option<MeshLaneDecision>,
    pub curation_signal: Option<MeshLaneDecision>,
}

impl MeshLaneGrants {
    #[must_use]
    pub fn decision(&self, lane: MeshLane) -> MeshLaneDecision {
        match lane {
            MeshLane::Metadata => self.metadata,
            MeshLane::Body => self.body,
            MeshLane::Embedding => self.embedding,
            MeshLane::GraphLink => self.graph_link,
            MeshLane::RevisionNotice => self.revision_notice,
            MeshLane::CurationSignal => self.curation_signal,
        }
        .unwrap_or(MeshLaneDecision::Deny)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphConfig {
    pub ppr: GraphPprConfig,
    pub health: GraphHealthConfig,
    pub curate: GraphCurateConfig,
    pub hits: GraphHitsConfig,
    pub causal: GraphCausalConfig,
    pub pack_dna: GraphPackDnaConfig,
    pub gomory_hu: GraphGomoryHuConfig,
    pub feature: GraphFeatureFlagsConfig,
}

impl GraphConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            ppr: GraphPprConfig::parse(document)?,
            health: GraphHealthConfig::parse(document)?,
            curate: GraphCurateConfig::parse(document)?,
            hits: GraphHitsConfig::parse(document)?,
            causal: GraphCausalConfig::parse(document)?,
            pack_dna: GraphPackDnaConfig::parse(document)?,
            gomory_hu: GraphGomoryHuConfig::parse(document)?,
            feature: GraphFeatureFlagsConfig::parse(document)?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphPprConfig {
    pub alpha: Option<f64>,
}

impl GraphPprConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            alpha: optional_unit_float_path(document, &["graph", "ppr"], "alpha")?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphHealthConfig {
    pub contradiction_threshold: Option<f64>,
}

impl GraphHealthConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            contradiction_threshold: optional_unit_float_path(
                document,
                &["graph", "health"],
                "contradiction_threshold",
            )?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphCurateConfig {
    pub onion_decay_max: Option<f64>,
    pub articulation_protection_multiplier: Option<f64>,
}

impl GraphCurateConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        const SECTIONS: &[&str] = &["graph", "curate"];
        Ok(Self {
            onion_decay_max: optional_positive_float_path(document, SECTIONS, "onion_decay_max")?,
            articulation_protection_multiplier: optional_unit_float_path(
                document,
                SECTIONS,
                "articulation_protection_multiplier",
            )?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphHitsConfig {
    pub profile_boost: Option<f64>,
}

impl GraphHitsConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            profile_boost: optional_nonnegative_float_path(
                document,
                &["graph", "hits"],
                "profile_boost",
            )?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphCausalConfig {
    pub min_cost_normalization: Option<f64>,
}

impl GraphCausalConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            min_cost_normalization: optional_positive_float_path(
                document,
                &["graph", "causal"],
                "min_cost_normalization",
            )?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphPackDnaConfig {
    pub max_items: Option<u64>,
    pub max_edges: Option<u64>,
}

impl GraphPackDnaConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        const SECTIONS: &[&str] = &["graph", "pack_dna"];
        Ok(Self {
            max_items: optional_u64_path(document, SECTIONS, "max_items")?,
            max_edges: optional_u64_path(document, SECTIONS, "max_edges")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphGomoryHuConfig {
    pub sample_threshold: Option<u64>,
    pub sample_size: Option<u64>,
}

impl GraphGomoryHuConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        const SECTIONS: &[&str] = &["graph", "gomory_hu"];
        Ok(Self {
            sample_threshold: optional_u64_path(document, SECTIONS, "sample_threshold")?,
            sample_size: optional_u64_path(document, SECTIONS, "sample_size")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphFeatureFlagsConfig {
    pub ppr_enabled: Option<bool>,
    pub pack_dna_enabled: Option<bool>,
    pub causal_explain_enabled: Option<bool>,
    pub structural_health_enabled: Option<bool>,
    pub structural_decay_enabled: Option<bool>,
    pub proximity_enabled: Option<bool>,
    pub revision_dominance_enabled: Option<bool>,
    pub skyline_enabled: Option<bool>,
    pub load_bearing_enabled: Option<bool>,
    pub hits_profiles_enabled: Option<bool>,
}

impl GraphFeatureFlagsConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            ppr_enabled: optional_bool_path(document, &["graph", "feature", "ppr"], "enabled")?,
            pack_dna_enabled: optional_bool_path(
                document,
                &["graph", "feature", "pack_dna"],
                "enabled",
            )?,
            causal_explain_enabled: optional_bool_path(
                document,
                &["graph", "feature", "causal_explain"],
                "enabled",
            )?,
            structural_health_enabled: optional_bool_path(
                document,
                &["graph", "feature", "structural_health"],
                "enabled",
            )?,
            structural_decay_enabled: optional_bool_path(
                document,
                &["graph", "feature", "structural_decay"],
                "enabled",
            )?,
            proximity_enabled: optional_bool_path(
                document,
                &["graph", "feature", "proximity"],
                "enabled",
            )?,
            revision_dominance_enabled: optional_bool_path(
                document,
                &["graph", "feature", "revision_dominance"],
                "enabled",
            )?,
            skyline_enabled: optional_bool_path(
                document,
                &["graph", "feature", "skyline"],
                "enabled",
            )?,
            load_bearing_enabled: optional_bool_path(
                document,
                &["graph", "feature", "load_bearing"],
                "enabled",
            )?,
            hits_profiles_enabled: optional_bool_path(
                document,
                &["graph", "feature", "hits_profiles"],
                "enabled",
            )?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CurationConfig {
    pub duplicate_similarity: Option<f64>,
    pub harmful_weight: Option<f64>,
    pub decay_half_life_days: Option<u64>,
    pub specificity_min: Option<f64>,
}

impl CurationConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            duplicate_similarity: optional_unit_float(
                document,
                "curation",
                "duplicate_similarity",
            )?,
            harmful_weight: optional_nonnegative_float(document, "curation", "harmful_weight")?,
            decay_half_life_days: optional_u64(document, "curation", "decay_half_life_days")?,
            specificity_min: optional_unit_float(document, "curation", "specificity_min")?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LearnConfig {
    pub cluster_coherence_threshold: Option<f64>,
    pub decay: LearnDecayConfig,
}

impl LearnConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            cluster_coherence_threshold: optional_unit_float_path(
                document,
                &["learn"],
                "cluster_coherence_threshold",
            )?,
            decay: LearnDecayConfig::parse(document)?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LearnDecayConfig {
    pub demote_threshold: Option<f64>,
    pub forget_threshold: Option<f64>,
    pub working_half_life_days: Option<f64>,
    pub episodic_event_half_life_days: Option<f64>,
    pub episodic_failure_half_life_days: Option<f64>,
    pub semantic_fact_half_life_days: Option<f64>,
    pub procedural_rule_half_life_days: Option<f64>,
    pub default_half_life_days: Option<f64>,
}

impl LearnDecayConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        const SECTIONS: &[&str] = &["learn", "decay"];
        Ok(Self {
            demote_threshold: optional_unit_float_path(document, SECTIONS, "demote_threshold")?,
            forget_threshold: optional_unit_float_path(document, SECTIONS, "forget_threshold")?,
            working_half_life_days: optional_positive_float_path(
                document,
                SECTIONS,
                "working_half_life_days",
            )?,
            episodic_event_half_life_days: optional_positive_float_path(
                document,
                SECTIONS,
                "episodic_event_half_life_days",
            )?,
            episodic_failure_half_life_days: optional_positive_float_path(
                document,
                SECTIONS,
                "episodic_failure_half_life_days",
            )?,
            semantic_fact_half_life_days: optional_positive_float_path(
                document,
                SECTIONS,
                "semantic_fact_half_life_days",
            )?,
            procedural_rule_half_life_days: optional_positive_float_path(
                document,
                SECTIONS,
                "procedural_rule_half_life_days",
            )?,
            default_half_life_days: optional_positive_float_path(
                document,
                SECTIONS,
                "default_half_life_days",
            )?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeedbackConfig {
    pub harmful_per_source_per_hour: Option<u64>,
    pub harmful_burst_window_seconds: Option<u64>,
}

impl FeedbackConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            harmful_per_source_per_hour: optional_u64(
                document,
                "feedback",
                "harmful_per_source_per_hour",
            )?,
            harmful_burst_window_seconds: optional_u64(
                document,
                "feedback",
                "harmful_burst_window_seconds",
            )?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PolicyConfig {
    pub secret_detector: SecretDetectorConfig,
    pub output_redaction: OutputRedactionConfig,
}

impl PolicyConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            secret_detector: SecretDetectorConfig::parse(document)?,
            output_redaction: OutputRedactionConfig::parse(document)?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OutputRedactionConfig {
    pub enabled: Option<bool>,
}

impl OutputRedactionConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            enabled: optional_bool_path(document, &["policy", "output_redaction"], "enabled")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SecretDetectorConfig {
    pub allow_phrases: Option<Vec<String>>,
    pub allow_regex: Option<Vec<String>>,
}

impl SecretDetectorConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            allow_phrases: optional_string_array_path(
                document,
                &["policy", "secret_detector"],
                "allow_phrases",
            )?,
            allow_regex: optional_regex_array_path(
                document,
                &["policy", "secret_detector"],
                "allow_regex",
            )?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PrivacyConfig {
    pub redact_secrets: Option<bool>,
    pub redaction_classes: Option<Vec<String>>,
}

impl PrivacyConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            redact_secrets: optional_bool(document, "privacy", "redact_secrets")?,
            redaction_classes: optional_string_array(document, "privacy", "redaction_classes")?,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrustConfig {
    pub default_class: Option<String>,
    pub prompt_injection_guard: Option<bool>,
    pub team_members: Option<Vec<String>>,
}

impl TrustConfig {
    fn parse(document: &DocumentMut) -> Result<Self, ConfigParseError> {
        Ok(Self {
            default_class: optional_string(document, "trust", "default_class")?,
            prompt_injection_guard: optional_bool(document, "trust", "prompt_injection_guard")?,
            team_members: optional_string_array(document, "trust", "team_members")?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigParseError {
    Toml {
        message: String,
    },
    InvalidType {
        key: String,
        expected: &'static str,
    },
    InvalidValue {
        key: String,
        value: String,
        message: String,
    },
    PathExpansion {
        key: String,
        source: PathExpansionError,
    },
}

impl fmt::Display for ConfigParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml { message } => write!(formatter, "invalid TOML config: {message}"),
            Self::InvalidType { key, expected } => {
                write!(formatter, "config key `{key}` must be {expected}")
            }
            Self::InvalidValue {
                key,
                value,
                message,
            } => write!(
                formatter,
                "config key `{key}` has invalid value `{value}`: {message}"
            ),
            Self::PathExpansion { key, source } => {
                write!(formatter, "failed to expand config path `{key}`: {source}")
            }
        }
    }
}

impl std::error::Error for ConfigParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PathExpansion { source, .. } => Some(source),
            Self::Toml { .. } | Self::InvalidType { .. } | Self::InvalidValue { .. } => None,
        }
    }
}

fn item<'a>(document: &'a DocumentMut, section: &str, key: &str) -> Option<&'a Item> {
    document.get(section).and_then(|table| table.get(key))
}

fn item_path<'a>(document: &'a DocumentMut, sections: &[&str], key: &str) -> Option<&'a Item> {
    let (first, rest) = sections.split_first()?;
    let mut current = document.get(first)?;
    for section in rest {
        current = current.get(section)?;
    }
    current.get(key)
}

fn key_name(section: &str, key: &str) -> String {
    format!("{section}.{key}")
}

fn key_path_name(sections: &[&str], key: &str) -> String {
    format!("{}.{}", sections.join("."), key)
}

fn optional_string(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<String>, ConfigParseError> {
    match item(document, section, key) {
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| ConfigParseError::InvalidType {
                key: key_name(section, key),
                expected: "a string",
            }),
        None => Ok(None),
    }
}

fn optional_bool(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<bool>, ConfigParseError> {
    match item(document, section, key) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| ConfigParseError::InvalidType {
                key: key_name(section, key),
                expected: "a boolean",
            }),
        None => Ok(None),
    }
}

fn optional_bool_path(
    document: &DocumentMut,
    sections: &[&str],
    key: &str,
) -> Result<Option<bool>, ConfigParseError> {
    match item_path(document, sections, key) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| ConfigParseError::InvalidType {
                key: key_path_name(sections, key),
                expected: "a boolean",
            }),
        None => Ok(None),
    }
}

fn optional_u64(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<u64>, ConfigParseError> {
    match item(document, section, key) {
        Some(value) => match value.as_integer() {
            Some(integer) if integer >= 0 => Ok(Some(integer as u64)),
            Some(integer) => Err(ConfigParseError::InvalidValue {
                key: key_name(section, key),
                value: integer.to_string(),
                message: "expected a non-negative integer".to_string(),
            }),
            None => Err(ConfigParseError::InvalidType {
                key: key_name(section, key),
                expected: "an integer",
            }),
        },
        None => Ok(None),
    }
}

fn optional_u64_path(
    document: &DocumentMut,
    sections: &[&str],
    key: &str,
) -> Result<Option<u64>, ConfigParseError> {
    match item_path(document, sections, key) {
        Some(value) => match value.as_integer() {
            Some(integer) if integer >= 0 => Ok(Some(integer as u64)),
            Some(integer) => Err(ConfigParseError::InvalidValue {
                key: key_path_name(sections, key),
                value: integer.to_string(),
                message: "expected a non-negative integer".to_string(),
            }),
            None => Err(ConfigParseError::InvalidType {
                key: key_path_name(sections, key),
                expected: "an integer",
            }),
        },
        None => Ok(None),
    }
}

fn optional_float(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<f64>, ConfigParseError> {
    match item(document, section, key) {
        Some(value) => match value
            .as_float()
            .or_else(|| value.as_integer().map(|i| i as f64))
        {
            Some(number) if number.is_finite() => Ok(Some(number)),
            Some(number) => Err(ConfigParseError::InvalidValue {
                key: key_name(section, key),
                value: number.to_string(),
                message: "expected a finite number".to_string(),
            }),
            None => Err(ConfigParseError::InvalidType {
                key: key_name(section, key),
                expected: "a number",
            }),
        },
        None => Ok(None),
    }
}

fn optional_float_path(
    document: &DocumentMut,
    sections: &[&str],
    key: &str,
) -> Result<Option<f64>, ConfigParseError> {
    match item_path(document, sections, key) {
        Some(value) => match value
            .as_float()
            .or_else(|| value.as_integer().map(|i| i as f64))
        {
            Some(number) if number.is_finite() => Ok(Some(number)),
            Some(number) => Err(ConfigParseError::InvalidValue {
                key: key_path_name(sections, key),
                value: number.to_string(),
                message: "expected a finite number".to_string(),
            }),
            None => Err(ConfigParseError::InvalidType {
                key: key_path_name(sections, key),
                expected: "a number",
            }),
        },
        None => Ok(None),
    }
}

fn optional_unit_float(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<f64>, ConfigParseError> {
    match optional_float(document, section, key)? {
        Some(number) if (0.0..=1.0).contains(&number) => Ok(Some(number)),
        Some(number) => Err(ConfigParseError::InvalidValue {
            key: key_name(section, key),
            value: number.to_string(),
            message: "expected a number in 0.0..=1.0".to_string(),
        }),
        None => Ok(None),
    }
}

fn optional_unit_float_path(
    document: &DocumentMut,
    sections: &[&str],
    key: &str,
) -> Result<Option<f64>, ConfigParseError> {
    match optional_float_path(document, sections, key)? {
        Some(number) if (0.0..=1.0).contains(&number) => Ok(Some(number)),
        Some(number) => Err(ConfigParseError::InvalidValue {
            key: key_path_name(sections, key),
            value: number.to_string(),
            message: "expected a number in 0.0..=1.0".to_string(),
        }),
        None => Ok(None),
    }
}

fn optional_nonnegative_float(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<f64>, ConfigParseError> {
    match optional_float(document, section, key)? {
        Some(number) if number >= 0.0 => Ok(Some(number)),
        Some(number) => Err(ConfigParseError::InvalidValue {
            key: key_name(section, key),
            value: number.to_string(),
            message: "expected a non-negative number".to_string(),
        }),
        None => Ok(None),
    }
}

fn optional_nonnegative_float_path(
    document: &DocumentMut,
    sections: &[&str],
    key: &str,
) -> Result<Option<f64>, ConfigParseError> {
    match optional_float_path(document, sections, key)? {
        Some(number) if number >= 0.0 => Ok(Some(number)),
        Some(number) => Err(ConfigParseError::InvalidValue {
            key: key_path_name(sections, key),
            value: number.to_string(),
            message: "expected a non-negative number".to_string(),
        }),
        None => Ok(None),
    }
}

fn optional_positive_float_path(
    document: &DocumentMut,
    sections: &[&str],
    key: &str,
) -> Result<Option<f64>, ConfigParseError> {
    match optional_float_path(document, sections, key)? {
        Some(number) if number > 0.0 => Ok(Some(number)),
        Some(number) => Err(ConfigParseError::InvalidValue {
            key: key_path_name(sections, key),
            value: number.to_string(),
            message: "expected a positive number".to_string(),
        }),
        None => Ok(None),
    }
}

fn optional_path(
    document: &DocumentMut,
    section: &str,
    key: &str,
    expander: Option<&PathExpander>,
) -> Result<Option<PathBuf>, ConfigParseError> {
    let Some(raw) = optional_string(document, section, key)? else {
        return Ok(None);
    };
    match expander {
        Some(expander) => {
            expander
                .expand(&raw)
                .map(Some)
                .map_err(|source| ConfigParseError::PathExpansion {
                    key: key_name(section, key),
                    source,
                })
        }
        None => Ok(Some(PathBuf::from(raw))),
    }
}

fn optional_path_path(
    document: &DocumentMut,
    sections: &[&str],
    key: &str,
    expander: Option<&PathExpander>,
) -> Result<Option<PathBuf>, ConfigParseError> {
    let Some(item) = item_path(document, sections, key) else {
        return Ok(None);
    };
    let Some(raw) = item.as_str().map(str::to_owned) else {
        return Err(ConfigParseError::InvalidType {
            key: key_path_name(sections, key),
            expected: "a string",
        });
    };
    match expander {
        Some(expander) => {
            expander
                .expand(&raw)
                .map(Some)
                .map_err(|source| ConfigParseError::PathExpansion {
                    key: key_path_name(sections, key),
                    source,
                })
        }
        None => Ok(Some(PathBuf::from(raw))),
    }
}

fn optional_search_speed(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<SearchSpeed>, ConfigParseError> {
    match optional_string(document, section, key)? {
        Some(value) => value.parse().map(Some),
        None => Ok(None),
    }
}

fn optional_mesh_command_mode(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<MeshCommandMode>, ConfigParseError> {
    match optional_string(document, section, key)? {
        Some(value) => value.parse().map(Some),
        None => Ok(None),
    }
}

fn optional_string_array(
    document: &DocumentMut,
    section: &str,
    key: &str,
) -> Result<Option<Vec<String>>, ConfigParseError> {
    let Some(value) = item(document, section, key) else {
        return Ok(None);
    };
    let Some(array) = value.as_array() else {
        return Err(ConfigParseError::InvalidType {
            key: key_name(section, key),
            expected: "an array of strings",
        });
    };

    let mut out = Vec::new();
    for entry in array.iter() {
        match entry {
            Value::String(text) => out.push(text.value().to_string()),
            _ => {
                return Err(ConfigParseError::InvalidType {
                    key: key_name(section, key),
                    expected: "an array of strings",
                });
            }
        }
    }
    Ok(Some(out))
}

fn optional_string_array_path(
    document: &DocumentMut,
    sections: &[&str],
    key: &str,
) -> Result<Option<Vec<String>>, ConfigParseError> {
    let Some(value) = item_path(document, sections, key) else {
        return Ok(None);
    };
    let Some(array) = value.as_array() else {
        return Err(ConfigParseError::InvalidType {
            key: key_path_name(sections, key),
            expected: "an array of strings",
        });
    };

    let mut out = Vec::new();
    for entry in array.iter() {
        match entry {
            Value::String(text) => out.push(text.value().to_string()),
            _ => {
                return Err(ConfigParseError::InvalidType {
                    key: key_path_name(sections, key),
                    expected: "an array of strings",
                });
            }
        }
    }
    Ok(Some(out))
}

fn optional_peer_group_bindings(
    document: &DocumentMut,
) -> Result<Option<Vec<MeshPeerGroupBinding>>, ConfigParseError> {
    let Some(item) = item_path(document, &["mesh"], "peer_group_bindings") else {
        return Ok(None);
    };
    let Some(tables) = item.as_array_of_tables() else {
        return Err(ConfigParseError::InvalidType {
            key: "mesh.peer_group_bindings".to_string(),
            expected: "an array of tables",
        });
    };

    let mut bindings = Vec::with_capacity(tables.len());
    for (index, table) in tables.iter().enumerate() {
        bindings.push(parse_peer_group_binding(table, index)?);
    }
    Ok(Some(bindings))
}

fn parse_peer_group_binding(
    table: &Table,
    index: usize,
) -> Result<MeshPeerGroupBinding, ConfigParseError> {
    let prefix = format!("mesh.peer_group_bindings[{index}]");
    Ok(MeshPeerGroupBinding {
        workspace_id: optional_table_string(table, &prefix, "workspace_id")?,
        workspace_alias: optional_table_string(table, &prefix, "workspace_alias")?,
        peer_group_id: optional_table_string(table, &prefix, "peer_group_id")?,
        peer_group_label: optional_table_string(table, &prefix, "peer_group_label")?,
        peer_ids: optional_table_string_array(table, &prefix, "peer_ids")?,
        origin_workspace_ids: optional_table_string_array(table, &prefix, "origin_workspace_ids")?,
        lanes: parse_lane_grants(table, &prefix)?,
        default_action: optional_table_default_action(table, &prefix)?,
    })
}

fn parse_lane_grants(table: &Table, prefix: &str) -> Result<MeshLaneGrants, ConfigParseError> {
    let Some(lanes) = table.get("lanes") else {
        return Ok(MeshLaneGrants::default());
    };
    let Some(lanes) = lanes.as_table() else {
        return Err(ConfigParseError::InvalidType {
            key: format!("{prefix}.lanes"),
            expected: "a table",
        });
    };
    Ok(MeshLaneGrants {
        metadata: optional_table_lane_decision(lanes, &format!("{prefix}.lanes"), "metadata")?,
        body: optional_table_lane_decision(lanes, &format!("{prefix}.lanes"), "body")?,
        embedding: optional_table_lane_decision(lanes, &format!("{prefix}.lanes"), "embedding")?,
        graph_link: optional_table_lane_decision(lanes, &format!("{prefix}.lanes"), "graph_link")?,
        revision_notice: optional_table_lane_decision(
            lanes,
            &format!("{prefix}.lanes"),
            "revision_notice",
        )?,
        curation_signal: optional_table_lane_decision(
            lanes,
            &format!("{prefix}.lanes"),
            "curation_signal",
        )?,
    })
}

fn optional_table_string(
    table: &Table,
    prefix: &str,
    key: &str,
) -> Result<Option<String>, ConfigParseError> {
    match table.get(key) {
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| ConfigParseError::InvalidType {
                key: format!("{prefix}.{key}"),
                expected: "a string",
            }),
        None => Ok(None),
    }
}

fn optional_table_lane_decision(
    table: &Table,
    prefix: &str,
    key: &str,
) -> Result<Option<MeshLaneDecision>, ConfigParseError> {
    let Some(value) = optional_table_string(table, prefix, key)? else {
        return Ok(None);
    };
    MeshLaneDecision::parse_for_key(&value, format!("{prefix}.{key}")).map(Some)
}

fn optional_table_default_action(
    table: &Table,
    prefix: &str,
) -> Result<Option<MeshLaneDecision>, ConfigParseError> {
    let key = "default_action";
    let Some(value) = optional_table_string(table, prefix, key)? else {
        return Ok(None);
    };
    match value.as_str() {
        "deny" => Ok(Some(MeshLaneDecision::Deny)),
        other => Err(ConfigParseError::InvalidValue {
            key: format!("{prefix}.{key}"),
            value: other.to_string(),
            message: "expected `deny`; mesh peer-group bindings are default-deny".to_string(),
        }),
    }
}

fn optional_table_string_array(
    table: &Table,
    prefix: &str,
    key: &str,
) -> Result<Option<Vec<String>>, ConfigParseError> {
    let Some(value) = table.get(key) else {
        return Ok(None);
    };
    let Some(array) = value.as_array() else {
        return Err(ConfigParseError::InvalidType {
            key: format!("{prefix}.{key}"),
            expected: "an array of strings",
        });
    };

    let mut out = Vec::new();
    for entry in array.iter() {
        match entry {
            Value::String(text) => out.push(text.value().to_string()),
            _ => {
                return Err(ConfigParseError::InvalidType {
                    key: format!("{prefix}.{key}"),
                    expected: "an array of strings",
                });
            }
        }
    }
    Ok(Some(out))
}

fn optional_regex_array_path(
    document: &DocumentMut,
    sections: &[&str],
    key: &str,
) -> Result<Option<Vec<String>>, ConfigParseError> {
    let Some(patterns) = optional_string_array_path(document, sections, key)? else {
        return Ok(None);
    };
    for pattern in &patterns {
        Regex::new(pattern).map_err(|source| ConfigParseError::InvalidValue {
            key: key_path_name(sections, key),
            value: pattern.clone(),
            message: format!("expected a valid regex: {source}"),
        })?;
    }
    Ok(Some(patterns))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{
        ConfigFile, ConfigParseError, MeshCommandMode, MeshLane, MeshLaneDecision, PathExpander,
        SearchSpeed, optional_string_array,
    };

    type TestResult = Result<(), String>;

    fn expect_config_error(input: &str) -> Result<ConfigParseError, String> {
        match ConfigFile::parse(input) {
            Ok(config) => Err(format!("expected parse error, got {config:?}")),
            Err(error) => Ok(error),
        }
    }

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
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
    fn parses_readme_style_config() -> TestResult {
        let input = r#"
[storage]
database_path = "~/.local/share/ee/ee.db"
index_dir = "$EE_INDEX_ROOT"
jsonl_export = false

[storage.read_pool]
size = 4
idle_timeout_seconds = 120
pin_snapshot = true

[runtime]
daemon = false
job_budget_ms = 5000
import_batch_size = 200

[cass]
enabled = true
binary = "cass"
since = "90d"

[search]
default_speed = "balanced"
lexical_weight = 0.45
semantic_weight = 0.45
graph_weight = 0.10

[pack]
default_profile = "balanced"
default_format = "markdown"
default_max_tokens = 4000
mmr_lambda = 0.7
candidate_pool = 100

[handoff.stale_threshold]
memories_added = 20
any_expired_in_pack = true
content_drift_score = 0.15
memories_revised = 0

[cache.pack_l2]
enabled = true
directory = "$EE_CACHE_ROOT"
max_bytes = 1073741824
max_age_days = 30

[mesh]
enabled = false
command_mode = "off"

[[mesh.peer_group_bindings]]
workspace_id = "wsp_local_release_001"
workspace_alias = "local-release"
peer_group_id = "pg_release_mesh_001"
peer_group_label = "release-mesh"
peer_ids = ["peer_alice_laptop_001", "peer_builder_host_001"]
origin_workspace_ids = ["wsp_remote_release_001"]
default_action = "deny"

[mesh.peer_group_bindings.lanes]
metadata = "allow"
body = "deny"
embedding = "deny"
graph_link = "allow"
revision_notice = "allow"
curation_signal = "quarantine"

[graph.ppr]
alpha = 0.30

[graph.health]
contradiction_threshold = 0.20

[graph.curate]
onion_decay_max = 3.0
articulation_protection_multiplier = 0.5

[graph.hits]
profile_boost = 0.5

[graph.causal]
min_cost_normalization = 1.0

[graph.pack_dna]
max_items = 10
max_edges = 30

[graph.gomory_hu]
sample_threshold = 500
sample_size = 100

[curation]
duplicate_similarity = 0.92
harmful_weight = 2.5
decay_half_life_days = 60
specificity_min = 0.45

[learn]
cluster_coherence_threshold = 0.55

[learn.decay]
demote_threshold = 0.05
forget_threshold = 0.01
working_half_life_days = 1
episodic_event_half_life_days = 30
episodic_failure_half_life_days = 90
semantic_fact_half_life_days = 180
procedural_rule_half_life_days = 365
default_half_life_days = 30

[policy.secret_detector]
allow_phrases = ["OAuth refresh token", "secret ballot"]
allow_regex = ["fake-key-[A-Z]{4}"]

[policy.output_redaction]
enabled = false

[privacy]
redact_secrets = true
redaction_classes = ["api_key", "jwt", "password"]

[trust]
default_class = "agent_assertion"
prompt_injection_guard = true
"#;
        let mut env = BTreeMap::new();
        env.insert(
            "EE_INDEX_ROOT".to_string(),
            OsString::from("/tmp/ee-indexes"),
        );
        env.insert("EE_CACHE_ROOT".to_string(), OsString::from("/tmp/ee-cache"));
        let expander = PathExpander::with_env(Some(PathBuf::from("/home/tester")), env);

        let config = ConfigFile::parse_with_expander(input, &expander)
            .map_err(|error| format!("config should parse: {error}"))?;

        ensure_equal(
            &config.storage.database_path,
            &Some(PathBuf::from("/home/tester/.local/share/ee/ee.db")),
            "database path",
        )?;
        ensure_equal(
            &config.storage.index_dir,
            &Some(PathBuf::from("/tmp/ee-indexes")),
            "index dir",
        )?;
        ensure_equal(&config.storage.jsonl_export, &Some(false), "jsonl export")?;
        ensure_equal(&config.storage.read_pool.size, &Some(4), "read pool size")?;
        ensure_equal(
            &config.storage.read_pool.idle_timeout_seconds,
            &Some(120),
            "read pool idle timeout",
        )?;
        ensure_equal(
            &config.storage.read_pool.pin_snapshot,
            &Some(true),
            "read pool snapshot pinning",
        )?;
        ensure_equal(&config.runtime.job_budget_ms, &Some(5000), "job budget")?;
        ensure_equal(&config.cass.binary.as_deref(), &Some("cass"), "cass binary")?;
        ensure_equal(
            &config.search.default_speed,
            &Some(SearchSpeed::Balanced),
            "search speed",
        )?;
        ensure_equal(&config.search.lexical_weight, &Some(0.45), "lexical weight")?;
        ensure_equal(
            &config.pack.default_profile.as_deref(),
            &Some("balanced"),
            "pack default profile",
        )?;
        ensure_equal(
            &config.pack.default_format.as_deref(),
            &Some("markdown"),
            "pack default format",
        )?;
        ensure_equal(&config.pack.default_max_tokens, &Some(4000), "max tokens")?;
        ensure_equal(
            &config.handoff.stale_threshold.memories_added,
            &Some(20),
            "handoff stale memories added threshold",
        )?;
        ensure_equal(
            &config.handoff.stale_threshold.any_expired_in_pack,
            &Some(true),
            "handoff stale expired threshold",
        )?;
        ensure_equal(
            &config.handoff.stale_threshold.content_drift_score,
            &Some(0.15),
            "handoff stale content drift threshold",
        )?;
        ensure_equal(
            &config.handoff.stale_threshold.memories_revised,
            &Some(0),
            "handoff stale memories revised threshold",
        )?;
        ensure_equal(
            &config.cache.pack_l2.enabled,
            &Some(true),
            "pack L2 cache enabled",
        )?;
        ensure_equal(
            &config.cache.pack_l2.directory,
            &Some(PathBuf::from("/tmp/ee-cache")),
            "pack L2 cache directory",
        )?;
        ensure_equal(
            &config.cache.pack_l2.max_bytes,
            &Some(1_073_741_824),
            "pack L2 cache max bytes",
        )?;
        ensure_equal(
            &config.cache.pack_l2.max_age_days,
            &Some(30),
            "pack L2 cache max age",
        )?;
        ensure_equal(&config.mesh.enabled, &Some(false), "mesh enabled")?;
        ensure_equal(
            &config.mesh.command_mode,
            &Some(MeshCommandMode::Off),
            "mesh command mode",
        )?;
        let binding = config
            .mesh
            .peer_group_bindings
            .as_ref()
            .and_then(|bindings| bindings.first())
            .ok_or_else(|| "expected one mesh peer-group binding".to_string())?;
        ensure_equal(
            &binding.workspace_id.as_deref(),
            &Some("wsp_local_release_001"),
            "mesh binding workspace id",
        )?;
        ensure_equal(
            &binding.decision_for(
                "wsp_local_release_001",
                "peer_alice_laptop_001",
                "wsp_remote_release_001",
                MeshLane::Metadata,
            ),
            &MeshLaneDecision::Allow,
            "mesh metadata lane",
        )?;
        ensure_equal(
            &binding.decision_for(
                "wsp_local_release_001",
                "peer_alice_laptop_001",
                "wsp_remote_release_001",
                MeshLane::Body,
            ),
            &MeshLaneDecision::Deny,
            "mesh body lane",
        )?;
        ensure_equal(&config.graph.ppr.alpha, &Some(0.30), "graph ppr alpha")?;
        ensure_equal(
            &config.graph.health.contradiction_threshold,
            &Some(0.20),
            "graph contradiction threshold",
        )?;
        ensure_equal(
            &config.graph.curate.onion_decay_max,
            &Some(3.0),
            "graph onion decay max",
        )?;
        ensure_equal(
            &config.graph.curate.articulation_protection_multiplier,
            &Some(0.5),
            "graph articulation protection multiplier",
        )?;
        ensure_equal(
            &config.graph.hits.profile_boost,
            &Some(0.5),
            "graph hits profile boost",
        )?;
        ensure_equal(
            &config.graph.causal.min_cost_normalization,
            &Some(1.0),
            "graph causal min-cost normalization",
        )?;
        ensure_equal(
            &config.graph.pack_dna.max_items,
            &Some(10),
            "graph pack dna max items",
        )?;
        ensure_equal(
            &config.graph.pack_dna.max_edges,
            &Some(30),
            "graph pack dna max edges",
        )?;
        ensure_equal(
            &config.graph.gomory_hu.sample_threshold,
            &Some(500),
            "graph gomory-hu sample threshold",
        )?;
        ensure_equal(
            &config.graph.gomory_hu.sample_size,
            &Some(100),
            "graph gomory-hu sample size",
        )?;
        ensure_equal(
            &config.curation.harmful_weight,
            &Some(2.5),
            "harmful weight",
        )?;
        ensure_equal(
            &config.curation.specificity_min,
            &Some(0.45),
            "specificity min",
        )?;
        ensure_equal(
            &config.learn.decay.demote_threshold,
            &Some(0.05),
            "learn decay demote threshold",
        )?;
        ensure_equal(
            &config.learn.cluster_coherence_threshold,
            &Some(0.55),
            "learn cluster coherence threshold",
        )?;
        ensure_equal(
            &config.learn.decay.forget_threshold,
            &Some(0.01),
            "learn decay forget threshold",
        )?;
        ensure_equal(
            &config.learn.decay.procedural_rule_half_life_days,
            &Some(365.0),
            "procedural rule half-life",
        )?;
        ensure_equal(
            &config.policy.secret_detector.allow_phrases,
            &Some(vec![
                "OAuth refresh token".to_string(),
                "secret ballot".to_string(),
            ]),
            "secret detector allow phrases",
        )?;
        ensure_equal(
            &config.policy.secret_detector.allow_regex,
            &Some(vec!["fake-key-[A-Z]{4}".to_string()]),
            "secret detector allow regex",
        )?;
        ensure_equal(
            &config.policy.output_redaction.enabled,
            &Some(false),
            "output redaction enabled",
        )?;
        ensure_equal(
            &config.privacy.redaction_classes,
            &Some(vec![
                "api_key".to_string(),
                "jwt".to_string(),
                "password".to_string(),
            ]),
            "redaction classes",
        )?;
        ensure_equal(
            &config.trust.prompt_injection_guard,
            &Some(true),
            "prompt injection guard",
        )
    }

    #[test]
    fn missing_sections_default_to_none() -> TestResult {
        let config =
            ConfigFile::parse("").map_err(|error| format!("empty config should parse: {error}"))?;

        ensure_equal(&config.storage.database_path, &None, "database path")?;
        ensure_equal(&config.storage.read_pool.size, &None, "read pool size")?;
        ensure_equal(
            &config.storage.read_pool.idle_timeout_seconds,
            &None,
            "read pool idle timeout",
        )?;
        ensure_equal(
            &config.storage.read_pool.pin_snapshot,
            &None,
            "read pool pin snapshot",
        )?;
        ensure_equal(&config.runtime.daemon, &None, "runtime daemon")?;
        ensure_equal(&config.search.default_speed, &None, "search default speed")?;
        ensure_equal(
            &config.learn.decay.demote_threshold,
            &None,
            "learn decay threshold",
        )?;
        ensure_equal(
            &config.learn.cluster_coherence_threshold,
            &None,
            "learn cluster coherence threshold",
        )?;
        ensure_equal(
            &config.policy.secret_detector.allow_phrases,
            &None,
            "allow phrases",
        )?;
        ensure_equal(
            &config.policy.output_redaction.enabled,
            &None,
            "output redaction enabled",
        )?;
        ensure_equal(
            &config.handoff.stale_threshold.memories_added,
            &None,
            "handoff stale memories added threshold",
        )?;
        ensure_equal(
            &config.handoff.stale_threshold.any_expired_in_pack,
            &None,
            "handoff stale expired threshold",
        )?;
        ensure_equal(
            &config.cache.pack_l2.enabled,
            &None,
            "pack L2 cache enabled",
        )?;
        ensure_equal(
            &config.cache.pack_l2.directory,
            &None,
            "pack L2 cache directory",
        )?;
        ensure_equal(
            &config.cache.pack_l2.max_bytes,
            &None,
            "pack L2 cache max bytes",
        )?;
        ensure_equal(
            &config.cache.pack_l2.max_age_days,
            &None,
            "pack L2 cache max age",
        )?;
        ensure_equal(&config.graph.ppr.alpha, &None, "graph ppr alpha")?;
        ensure_equal(&config.mesh.enabled, &None, "mesh enabled")?;
        ensure_equal(&config.mesh.command_mode, &None, "mesh command mode")?;
        ensure_equal(
            &config.mesh.peer_group_bindings,
            &None,
            "mesh peer-group bindings",
        )?;
        ensure_equal(
            &config.graph.gomory_hu.sample_threshold,
            &None,
            "graph gomory-hu sample threshold",
        )?;
        ensure_equal(
            &config.privacy.redaction_classes,
            &None,
            "redaction classes",
        )
    }

    #[test]
    fn rejects_wrong_type_for_known_key() -> TestResult {
        let error = expect_config_error("[runtime]\njob_budget_ms = \"slow\"\n")?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidType { ref key, expected }
                    if key == "runtime.job_budget_ms" && expected == "an integer"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn rejects_unknown_search_speed() -> TestResult {
        let error = expect_config_error("[search]\ndefault_speed = \"reckless\"\n")?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "search.default_speed"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn rejects_out_of_range_unit_weights() -> TestResult {
        let error = expect_config_error("[pack]\nmmr_lambda = 1.5\n")?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidValue { ref key, .. } if key == "pack.mmr_lambda"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn rejects_out_of_range_graph_thresholds() -> TestResult {
        let error = expect_config_error("[graph.health]\ncontradiction_threshold = 2.0\n")?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "graph.health.contradiction_threshold"
            ),
            format!("unexpected error: {error:?}"),
        )?;

        let error = expect_config_error("[graph.curate]\nonion_decay_max = 0.0\n")?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "graph.curate.onion_decay_max"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn peer_group_binding_denies_without_explicit_workspace_binding() -> TestResult {
        let config = ConfigFile::parse(
            r#"
[[mesh.peer_group_bindings]]
workspace_id = "wsp_workspace_a_001"
workspace_alias = "workspace-a"
peer_group_id = "pg_team_alpha_001"
peer_ids = ["peer_agent_001"]
origin_workspace_ids = ["wsp_origin_001"]

[mesh.peer_group_bindings.lanes]
metadata = "allow"
"#,
        )
        .map_err(|error| format!("config should parse: {error}"))?;
        let binding = config
            .mesh
            .peer_group_bindings
            .as_ref()
            .and_then(|bindings| bindings.first())
            .ok_or_else(|| "expected peer-group binding".to_string())?;

        ensure_equal(
            &binding.decision_for(
                "wsp_workspace_b_001",
                "peer_agent_001",
                "wsp_origin_001",
                MeshLane::Metadata,
            ),
            &MeshLaneDecision::Deny,
            "workspace B without explicit binding must deny",
        )
    }

    #[test]
    fn mesh_command_mode_parses_all_stable_modes() -> TestResult {
        for (raw, expected) in [
            ("off", MeshCommandMode::Off),
            ("cache", MeshCommandMode::Cache),
            ("revisable", MeshCommandMode::Revisable),
            ("blocking", MeshCommandMode::Blocking),
        ] {
            let config = ConfigFile::parse(&format!("[mesh]\ncommand_mode = \"{raw}\"\n"))
                .map_err(|error| format!("mesh mode {raw} should parse: {error}"))?;
            ensure_equal(
                &config.mesh.command_mode,
                &Some(expected),
                "mesh command mode",
            )?;
            ensure_equal(&expected.as_str(), &raw, "mesh command mode string")?;
        }
        Ok(())
    }

    #[test]
    fn mesh_command_mode_rejects_unknown_mode() -> TestResult {
        let error = expect_config_error("[mesh]\ncommand_mode = \"auto\"\n")?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "mesh.command_mode"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn peer_group_binding_can_allow_metadata_while_denying_body_and_embedding() -> TestResult {
        let config = ConfigFile::parse(
            r#"
[[mesh.peer_group_bindings]]
workspace_id = "wsp_workspace_a_001"
workspace_alias = "workspace-a"
peer_group_id = "pg_team_alpha_001"
peer_ids = ["peer_agent_001"]
origin_workspace_ids = ["wsp_origin_001"]

[mesh.peer_group_bindings.lanes]
metadata = "allow"
body = "deny"
embedding = "deny"
revision_notice = "allow"
"#,
        )
        .map_err(|error| format!("config should parse: {error}"))?;
        let binding = config
            .mesh
            .peer_group_bindings
            .as_ref()
            .and_then(|bindings| bindings.first())
            .ok_or_else(|| "expected peer-group binding".to_string())?;

        for (lane, expected, context) in [
            (MeshLane::Metadata, MeshLaneDecision::Allow, "metadata"),
            (MeshLane::Body, MeshLaneDecision::Deny, "body"),
            (MeshLane::Embedding, MeshLaneDecision::Deny, "embedding"),
        ] {
            ensure_equal(
                &binding.decision_for(
                    "wsp_workspace_a_001",
                    "peer_agent_001",
                    "wsp_origin_001",
                    lane,
                ),
                &expected,
                context,
            )?;
        }
        Ok(())
    }

    #[test]
    fn peer_group_binding_missing_lane_and_unknown_origin_deny_by_default() -> TestResult {
        let config = ConfigFile::parse(
            r#"
[[mesh.peer_group_bindings]]
workspace_id = "wsp_workspace_a_001"
workspace_alias = "workspace-a"
peer_group_id = "pg_team_alpha_001"
peer_ids = ["peer_agent_001"]
origin_workspace_ids = ["wsp_origin_001"]

[mesh.peer_group_bindings.lanes]
metadata = "allow"
"#,
        )
        .map_err(|error| format!("config should parse: {error}"))?;
        let binding = config
            .mesh
            .peer_group_bindings
            .as_ref()
            .and_then(|bindings| bindings.first())
            .ok_or_else(|| "expected peer-group binding".to_string())?;

        ensure_equal(
            &binding.decision_for(
                "wsp_workspace_a_001",
                "peer_agent_001",
                "wsp_unknown_origin_001",
                MeshLane::Metadata,
            ),
            &MeshLaneDecision::Deny,
            "unknown origin must deny",
        )?;
        ensure_equal(
            &binding.decision_for(
                "wsp_workspace_a_001",
                "peer_agent_001",
                "wsp_origin_001",
                MeshLane::CurationSignal,
            ),
            &MeshLaneDecision::Deny,
            "missing curation signal lane must deny",
        )
    }

    #[test]
    fn peer_group_binding_rejects_non_deny_default_action() -> TestResult {
        let error = expect_config_error(
            r#"
[[mesh.peer_group_bindings]]
workspace_id = "wsp_workspace_a_001"
workspace_alias = "workspace-a"
peer_group_id = "pg_team_alpha_001"
peer_ids = ["peer_agent_001"]
origin_workspace_ids = ["wsp_origin_001"]
default_action = "allow"
"#,
        )?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "mesh.peer_group_bindings[0].default_action"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn rejects_invalid_learn_decay_values() -> TestResult {
        let cluster_error = expect_config_error("[learn]\ncluster_coherence_threshold = 1.5\n")?;
        ensure(
            matches!(
                cluster_error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "learn.cluster_coherence_threshold"
            ),
            format!("unexpected cluster threshold error: {cluster_error:?}"),
        )?;

        let threshold_error = expect_config_error("[learn.decay]\ndemote_threshold = 1.5\n")?;
        ensure(
            matches!(
                threshold_error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "learn.decay.demote_threshold"
            ),
            format!("unexpected threshold error: {threshold_error:?}"),
        )?;

        let half_life_error =
            expect_config_error("[learn.decay]\nprocedural_rule_half_life_days = 0\n")?;
        ensure(
            matches!(
                half_life_error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "learn.decay.procedural_rule_half_life_days"
            ),
            format!("unexpected half-life error: {half_life_error:?}"),
        )
    }

    #[test]
    fn rejects_invalid_handoff_stale_threshold_values() -> TestResult {
        let drift_error =
            expect_config_error("[handoff.stale_threshold]\ncontent_drift_score = 1.5\n")?;
        ensure(
            matches!(
                drift_error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "handoff.stale_threshold.content_drift_score"
            ),
            format!("unexpected drift threshold error: {drift_error:?}"),
        )?;

        let added_error = expect_config_error("[handoff.stale_threshold]\nmemories_added = -1\n")?;
        ensure(
            matches!(
                added_error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "handoff.stale_threshold.memories_added"
            ),
            format!("unexpected memories added threshold error: {added_error:?}"),
        )
    }

    #[test]
    fn rejects_non_string_redaction_classes() -> TestResult {
        let parsed =
            "[privacy]\nredaction_classes = [\"api_key\", 7]\n".parse::<toml_edit::DocumentMut>();
        let document = parsed.map_err(|error| format!("test TOML should parse: {error}"))?;

        let error = match optional_string_array(&document, "privacy", "redaction_classes") {
            Ok(value) => return Err(format!("expected array type error, got {value:?}")),
            Err(error) => error,
        };

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidType { ref key, expected }
                    if key == "privacy.redaction_classes" && expected == "an array of strings"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn rejects_invalid_secret_detector_allow_regex() -> TestResult {
        let error = expect_config_error("[policy.secret_detector]\nallow_regex = [\"[\"]\n")?;

        ensure(
            matches!(
                error,
                ConfigParseError::InvalidValue { ref key, .. }
                    if key == "policy.secret_detector.allow_regex"
            ),
            format!("unexpected error: {error:?}"),
        )
    }

    #[test]
    fn wraps_path_expansion_errors_with_config_key() -> TestResult {
        let expander = PathExpander::with_env(Some(PathBuf::from("/home/tester")), BTreeMap::new());
        let error = match ConfigFile::parse_with_expander(
            "[storage]\nindex_dir = \"$EE_MISSING\"\n",
            &expander,
        ) {
            Ok(config) => return Err(format!("expected path expansion error, got {config:?}")),
            Err(error) => error,
        };

        ensure(
            matches!(
                error,
                ConfigParseError::PathExpansion { ref key, .. } if key == "storage.index_dir"
            ),
            format!("unexpected error: {error:?}"),
        )
    }
}
