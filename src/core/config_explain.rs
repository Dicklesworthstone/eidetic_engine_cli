//! Self-explaining config + advisory config-lint (bd-2vq2z.15).
//!
//! `ee` has many config knobs (search weights, profiles, budgets, embeddings,
//! mesh) but no surface that explains what a setting *does*, its effective
//! value, which layer set it, its valid range, and — crucially — whether it
//! actually affects runtime today. The analyst was misled by
//! `search.semantic_weight` (the name implies neural semantics, but the default
//! vector tier is deterministic hash embedding). This module ends that class of
//! silent-misconfig confusion.
//!
//! Two surfaces are built on this core:
//! - `ee config explain [<key>] [--json]` — describe a knob: effect, effective
//!   value, source layer (`cli|environment|project|user|default`, reusing the
//!   merge machinery's [`crate::config::merge::ConfigValueSource`]), valid
//!   range, and honest runtime status (active / forward-looking / inert);
//! - a config-lint advisory check inside `ee doctor` — flag suspicious config
//!   (a weight set but the tier it weights is non-neural, an embedding model
//!   path that does not exist, an env var `ee` does not consume, contradictory
//!   settings). **Advisory only — config-lint never flips core health.**
//!
//! This module is the pure, deterministic policy core: the knob registry, the
//! explain assembly, and the lint rules as pure functions over config facts —
//! unit-testable without a live config. The CLI/doctor wiring adapts the merged
//! config to these cores in the follow-on leaves.

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::merge::{
    ConfigValueSource, MESH_ENABLED_KEY, PACK_DEFAULT_MAX_TOKENS_KEY, PACK_MMR_LAMBDA_KEY,
    SEARCH_DEFAULT_SPEED_KEY, SEARCH_GRAPH_WEIGHT_KEY, SEARCH_LEXICAL_WEIGHT_KEY,
    SEARCH_SEMANTIC_WEIGHT_KEY, STORAGE_DATABASE_PATH_KEY,
};

/// Schema identifier for the config-explain block.
pub const CONFIG_EXPLAIN_SCHEMA_V1: &str = "ee.config_explain.v1";

/// Severity of every config-lint finding. Config-lint is advisory: it informs,
/// it never degrades the top-line `ee doctor`/`ee status` health verdict.
pub const CONFIG_LINT_SEVERITY: &str = "advisory";

/// Whether this knob actually affects runtime behavior today — the
/// truth-in-labeling axis the analyst needed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigRuntimeStatus {
    /// Wired and affecting runtime behavior now.
    Active,
    /// Parsed and accepted, but not yet wired into live behavior (reserved). An
    /// honest "you can set this, but it does not do what the name implies yet".
    ForwardLooking,
    /// Currently has no effect given other active settings.
    Inert,
}

impl ConfigRuntimeStatus {
    /// Stable machine token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::ForwardLooking => "forward_looking",
            Self::Inert => "inert",
        }
    }
}

/// A static description of one config knob: what it does, its range, and its
/// honest runtime status. The registry of these is the backbone of
/// `ee config explain`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigKnob {
    /// The fully-qualified config key (e.g. `search.semantic_weight`).
    pub key: &'static str,
    /// Coarse category for grouping (`search`, `pack`, `storage`, `mesh`, ...).
    pub category: &'static str,
    /// What the knob affects at runtime, in one sentence.
    pub effect: &'static str,
    /// The valid range / accepted values, human-readable.
    pub valid_range: &'static str,
    /// Whether it actually affects runtime today.
    pub status: ConfigRuntimeStatus,
    /// A truth-in-labeling caveat when the name could mislead, else `None`.
    pub caveat: Option<&'static str>,
}

/// The config-knob registry. Covers the knobs with real semantic nuance and the
/// analyst's exact trap; extend as knobs gain explain coverage. Sorted by key so
/// `ee config explain` (no key) output is byte-stable.
#[must_use]
pub fn config_knobs() -> &'static [ConfigKnob] {
    // Keep sorted by `key`.
    &[
        ConfigKnob {
            key: MESH_ENABLED_KEY,
            category: "mesh",
            effect: "Enables the optional machine-to-machine memory mesh; off by default and never required for local operation.",
            valid_range: "true | false",
            status: ConfigRuntimeStatus::Active,
            caveat: None,
        },
        ConfigKnob {
            key: PACK_DEFAULT_MAX_TOKENS_KEY,
            category: "pack",
            effect: "Default token budget for `ee pack` when --max-tokens is not given.",
            valid_range: "positive integer (tokens)",
            status: ConfigRuntimeStatus::Active,
            caveat: None,
        },
        ConfigKnob {
            key: PACK_MMR_LAMBDA_KEY,
            category: "pack",
            effect: "MMR diversity/relevance tradeoff during pack selection: higher favors relevance, lower favors diversity.",
            valid_range: "0.0 ..= 1.0",
            status: ConfigRuntimeStatus::Active,
            caveat: None,
        },
        ConfigKnob {
            key: SEARCH_DEFAULT_SPEED_KEY,
            category: "search",
            effect: "Default latency/quality tradeoff for search; maps to the frankensearch embedder stack tier.",
            valid_range: "instant | default | quality",
            status: ConfigRuntimeStatus::Active,
            caveat: None,
        },
        ConfigKnob {
            key: SEARCH_GRAPH_WEIGHT_KEY,
            category: "search",
            effect: "Fusion weight applied to graph-proximity signal when combining ranked result lists.",
            valid_range: "0.0 ..= 1.0",
            status: ConfigRuntimeStatus::Active,
            caveat: None,
        },
        ConfigKnob {
            key: SEARCH_LEXICAL_WEIGHT_KEY,
            category: "search",
            effect: "Fusion weight applied to the lexical (BM25/FTS) tier when combining ranked result lists.",
            valid_range: "0.0 ..= 1.0",
            status: ConfigRuntimeStatus::Active,
            caveat: None,
        },
        ConfigKnob {
            key: SEARCH_SEMANTIC_WEIGHT_KEY,
            category: "search",
            effect: "Intended fusion weight for the vector (semantic) tier when combining ranked result lists.",
            valid_range: "0.0 ..= 1.0",
            // The analyst's trap: the name implies neural semantics, but the
            // default vector tier is deterministic hash embedding.
            status: ConfigRuntimeStatus::ForwardLooking,
            caveat: Some(
                "Does NOT imply neural semantic search: the default vector tier uses deterministic hash embeddings, not a neural model. Check `ee index status` for the active embedding mode (see ADR 0070 and the bundled-embeddings work, bd-1et0v).",
            ),
        },
        ConfigKnob {
            key: STORAGE_DATABASE_PATH_KEY,
            category: "storage",
            effect: "Filesystem path to the durable ee memory database (the source of truth).",
            valid_range: "filesystem path",
            status: ConfigRuntimeStatus::Active,
            caveat: None,
        },
    ]
}

/// Look up a knob by exact key.
#[must_use]
pub fn knob_for_key(key: &str) -> Option<&'static ConfigKnob> {
    config_knobs().iter().find(|knob| knob.key == key)
}

/// A resolved explanation of one config knob: its static description plus the
/// effective value and the layer that set it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigExplanation {
    /// The static knob description.
    pub knob: ConfigKnob,
    /// The current effective value rendered as a string, when resolved.
    pub effective_value: Option<String>,
    /// The layer that supplied the effective value (`cli|...|default`).
    pub source_layer: &'static str,
}

impl ConfigExplanation {
    /// Render the `ee.config_explain.v1` block for one knob.
    #[must_use]
    pub fn data_json(&self) -> Value {
        json!({
            "schema": CONFIG_EXPLAIN_SCHEMA_V1,
            "key": self.knob.key,
            "category": self.knob.category,
            "effect": self.knob.effect,
            "validRange": self.knob.valid_range,
            "status": self.knob.status.as_str(),
            "caveat": self.knob.caveat,
            "effectiveValue": self.effective_value,
            "sourceLayer": self.source_layer,
        })
    }
}

/// Build an explanation for `key`, given its effective value and the source
/// layer the merge machinery attributed to it. Returns `None` for an unknown
/// key (the caller surfaces an honest "no explain coverage for <key>").
#[must_use]
pub fn explain(
    key: &str,
    effective_value: Option<String>,
    source: Option<ConfigValueSource>,
) -> Option<ConfigExplanation> {
    knob_for_key(key).map(|knob| ConfigExplanation {
        knob: *knob,
        effective_value,
        source_layer: source.map_or("unknown", ConfigValueSource::as_str),
    })
}

/// A config-lint finding. Always advisory; carries a stable code and the key it
/// concerns so doctor can render it without degrading health.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigLintFinding {
    /// Stable machine code for the finding class.
    pub code: &'static str,
    /// The config key the finding concerns.
    pub key: String,
    /// Advisory, human-readable explanation.
    pub message: String,
    /// Always [`CONFIG_LINT_SEVERITY`] (`"advisory"`).
    pub severity: &'static str,
}

impl ConfigLintFinding {
    fn advisory(code: &'static str, key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            key: key.into(),
            message: message.into(),
            severity: CONFIG_LINT_SEVERITY,
        }
    }
}

/// Lint code: a semantic weight is set but the active vector tier is non-neural.
pub const LINT_WEIGHT_WITHOUT_NEURAL_TIER: &str = "config_semantic_weight_without_neural_tier";
/// Lint code: an embedding-model path is configured but does not exist on disk.
pub const LINT_EMBEDDING_MODEL_PATH_MISSING: &str = "config_embedding_model_path_missing";
/// Lint code: an environment variable was set that `ee` does not consume.
pub const LINT_UNKNOWN_ENV_VAR: &str = "config_unknown_env_var";
/// Lint code: `search.mode = lexical` while a semantic weight is also set.
pub const LINT_CONTRADICTORY_LEXICAL_MODE: &str = "config_contradictory_lexical_mode";

/// Advisory: `search.semantic_weight` is set but the active vector tier is not
/// neural, so the weight does not buy neural semantics (the analyst trap).
#[must_use]
pub fn lint_semantic_weight_without_neural_tier(
    semantic_weight: Option<f64>,
    neural_tier_active: bool,
) -> Option<ConfigLintFinding> {
    match semantic_weight {
        Some(weight) if !neural_tier_active => Some(ConfigLintFinding::advisory(
            LINT_WEIGHT_WITHOUT_NEURAL_TIER,
            SEARCH_SEMANTIC_WEIGHT_KEY,
            format!(
                "search.semantic_weight={weight} is set, but the active vector tier is deterministic hash, not neural — this does not enable neural semantic search. Check `ee index status` for the active embedding mode."
            ),
        )),
        _ => None,
    }
}

/// Advisory: an embedding-model path is configured but does not exist.
#[must_use]
pub fn lint_embedding_model_path_missing(
    key: &str,
    configured_path: Option<&str>,
    path_exists: bool,
) -> Option<ConfigLintFinding> {
    match configured_path {
        Some(path) if !path_exists => Some(ConfigLintFinding::advisory(
            LINT_EMBEDDING_MODEL_PATH_MISSING,
            key,
            format!("configured embedding model path does not exist: {path}"),
        )),
        _ => None,
    }
}

/// Advisory: an `EE_*`-adjacent environment variable was set that `ee` does not
/// consume (e.g. the `EMBEDDING_MODEL` trap). `recognized` is whether the env
/// registry recognizes the variable.
#[must_use]
pub fn lint_unknown_env_var(var_name: &str, recognized: bool) -> Option<ConfigLintFinding> {
    if recognized {
        None
    } else {
        Some(ConfigLintFinding::advisory(
            LINT_UNKNOWN_ENV_VAR,
            var_name,
            format!(
                "environment variable `{var_name}` is set but `ee` does not consume it; it has no effect. See `ee config env` for the variables ee honors."
            ),
        ))
    }
}

/// Advisory: `search.mode = lexical` while a semantic weight is also configured
/// — the semantic weight cannot take effect under lexical-only mode.
#[must_use]
pub fn lint_contradictory_lexical_mode(
    mode: Option<&str>,
    semantic_weight: Option<f64>,
) -> Option<ConfigLintFinding> {
    match (mode, semantic_weight) {
        (Some("lexical"), Some(weight)) => Some(ConfigLintFinding::advisory(
            LINT_CONTRADICTORY_LEXICAL_MODE,
            SEARCH_SEMANTIC_WEIGHT_KEY,
            format!(
                "search mode is `lexical`, so search.semantic_weight={weight} has no effect. Set mode to `hybrid` to use the vector tier."
            ),
        )),
        _ => None,
    }
}

/// Config facts the lint rules operate over. Pure inputs gathered by the doctor
/// wiring from the merged config + filesystem + env registry.
#[derive(Clone, Debug, Default)]
pub struct ConfigLintFacts {
    /// Effective `search.semantic_weight`, if set.
    pub semantic_weight: Option<f64>,
    /// Whether the active vector tier is a neural model (vs hash fallback).
    pub neural_tier_active: bool,
    /// Effective `search.mode`, if set.
    pub search_mode: Option<String>,
    /// A configured embedding-model path key + value + existence, if any.
    pub embedding_model_path: Option<(String, String, bool)>,
    /// Set-but-unrecognized environment variables: (name, recognized).
    pub env_vars: Vec<(String, bool)>,
}

/// Run all config-lint rules over the gathered facts, returning advisory
/// findings sorted by `(code, key)` for byte-stable output. Deterministic, and
/// — by construction — every finding is advisory; this function can never
/// produce a health-degrading result.
#[must_use]
pub fn run_config_lint(facts: &ConfigLintFacts) -> Vec<ConfigLintFinding> {
    let mut findings = Vec::new();

    if let Some(finding) =
        lint_semantic_weight_without_neural_tier(facts.semantic_weight, facts.neural_tier_active)
    {
        findings.push(finding);
    }
    if let Some(finding) =
        lint_contradictory_lexical_mode(facts.search_mode.as_deref(), facts.semantic_weight)
    {
        findings.push(finding);
    }
    if let Some((key, path, exists)) = &facts.embedding_model_path {
        if let Some(finding) = lint_embedding_model_path_missing(key, Some(path), *exists) {
            findings.push(finding);
        }
    }
    for (name, recognized) in &facts.env_vars {
        if let Some(finding) = lint_unknown_env_var(name, *recognized) {
            findings.push(finding);
        }
    }

    findings.sort_by(|a, b| a.code.cmp(b.code).then_with(|| a.key.cmp(&b.key)));
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_sorted_by_key_for_stable_output() {
        let keys: Vec<&str> = config_knobs().iter().map(|knob| knob.key).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "config knob registry must stay sorted by key");
    }

    #[test]
    fn semantic_weight_knob_is_honest_about_neural() {
        let knob = knob_for_key(SEARCH_SEMANTIC_WEIGHT_KEY).expect("semantic_weight is covered");
        assert_eq!(knob.status, ConfigRuntimeStatus::ForwardLooking);
        let caveat = knob.caveat.expect("semantic_weight must carry a caveat");
        assert!(
            caveat.contains("hash") && caveat.contains("neural"),
            "caveat must explain the hash-vs-neural truth"
        );
    }

    #[test]
    fn explain_reports_effect_value_and_source_layer() {
        let explanation = explain(
            SEARCH_SEMANTIC_WEIGHT_KEY,
            Some("0.45".to_owned()),
            Some(ConfigValueSource::Project),
        )
        .expect("known key explains");
        let value = explanation.data_json();
        assert_eq!(value["key"], SEARCH_SEMANTIC_WEIGHT_KEY);
        assert_eq!(value["effectiveValue"], "0.45");
        assert_eq!(value["sourceLayer"], "project");
        assert_eq!(value["status"], "forward_looking");
        assert!(value["caveat"].is_string());
    }

    #[test]
    fn explain_unknown_key_is_none() {
        assert!(explain("does.not.exist", None, None).is_none());
    }

    #[test]
    fn explain_unset_value_falls_back_to_unknown_source() {
        let explanation =
            explain(MESH_ENABLED_KEY, None, None).expect("known key explains even when unset");
        assert_eq!(explanation.source_layer, "unknown");
        assert_eq!(explanation.data_json()["effectiveValue"], Value::Null);
    }

    #[test]
    fn lint_flags_semantic_weight_without_neural_tier() {
        let finding = lint_semantic_weight_without_neural_tier(Some(0.45), false)
            .expect("a weight without a neural tier is suspicious");
        assert_eq!(finding.code, LINT_WEIGHT_WITHOUT_NEURAL_TIER);
        assert_eq!(finding.severity, CONFIG_LINT_SEVERITY);
        // Not a finding once a neural tier is active.
        assert!(lint_semantic_weight_without_neural_tier(Some(0.45), true).is_none());
        // Not a finding when the weight is unset.
        assert!(lint_semantic_weight_without_neural_tier(None, false).is_none());
    }

    #[test]
    fn lint_flags_missing_embedding_model_path() {
        let finding = lint_embedding_model_path_missing(
            "embedding.model_path",
            Some("/no/such/model"),
            false,
        )
        .expect("a missing model path is suspicious");
        assert_eq!(finding.code, LINT_EMBEDDING_MODEL_PATH_MISSING);
        // Present path → no finding.
        assert!(
            lint_embedding_model_path_missing("embedding.model_path", Some("/exists"), true)
                .is_none()
        );
    }

    #[test]
    fn lint_flags_unknown_env_var() {
        let finding = lint_unknown_env_var("EMBEDDING_MODEL", false).expect("unknown var flagged");
        assert_eq!(finding.code, LINT_UNKNOWN_ENV_VAR);
        assert!(lint_unknown_env_var("EE_DB", true).is_none());
    }

    #[test]
    fn lint_flags_contradictory_lexical_mode() {
        let finding = lint_contradictory_lexical_mode(Some("lexical"), Some(0.45))
            .expect("lexical mode + semantic weight is contradictory");
        assert_eq!(finding.code, LINT_CONTRADICTORY_LEXICAL_MODE);
        assert!(lint_contradictory_lexical_mode(Some("hybrid"), Some(0.45)).is_none());
        assert!(lint_contradictory_lexical_mode(Some("lexical"), None).is_none());
    }

    #[test]
    fn run_config_lint_is_advisory_only_and_deterministic() {
        let facts = ConfigLintFacts {
            semantic_weight: Some(0.45),
            neural_tier_active: false,
            search_mode: Some("lexical".to_owned()),
            embedding_model_path: Some((
                "embedding.model_path".to_owned(),
                "/no/such".to_owned(),
                false,
            )),
            env_vars: vec![
                ("EMBEDDING_MODEL".to_owned(), false),
                ("EE_DB".to_owned(), true),
            ],
        };
        let findings = run_config_lint(&facts);
        // semantic-without-neural + contradictory-mode + missing-path + unknown-env.
        assert_eq!(findings.len(), 4);
        // Every finding is advisory — config-lint never degrades health.
        assert!(findings.iter().all(|f| f.severity == CONFIG_LINT_SEVERITY));
        // Deterministic ordering by (code, key).
        let again = run_config_lint(&facts);
        assert_eq!(findings, again);
    }

    #[test]
    fn run_config_lint_clean_config_has_no_findings() {
        let facts = ConfigLintFacts {
            semantic_weight: Some(0.45),
            neural_tier_active: true,
            search_mode: Some("hybrid".to_owned()),
            embedding_model_path: None,
            env_vars: vec![("EE_DB".to_owned(), true)],
        };
        assert!(run_config_lint(&facts).is_empty());
    }
}
