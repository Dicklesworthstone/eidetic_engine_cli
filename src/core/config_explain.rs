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
//! config to these cores. The config-explain adapter below gathers configuration,
//! environment names, and path existence without opening stores or loading models.

use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::env_registry::{EmbeddingTrapEnvVar, EnvVar, read_os};

use crate::config::merge::{
    ConfigValueSource, MESH_ENABLED_KEY, PACK_DEFAULT_MAX_TOKENS_KEY, PACK_MMR_LAMBDA_KEY,
    SEARCH_DEFAULT_SPEED_KEY, SEARCH_GRAPH_WEIGHT_KEY, SEARCH_LEXICAL_WEIGHT_KEY,
    SEARCH_RERANK_KEY, SEARCH_RERANK_TOP_K_KEY, SEARCH_SEMANTIC_WEIGHT_KEY,
    STORAGE_DATABASE_PATH_KEY,
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
            valid_range: "fast | balanced | thorough",
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
            key: SEARCH_RERANK_KEY,
            category: "search",
            effect: "Controls whether the local reranker is auto-used when an available model is registered, or fully disabled before model lookup.",
            valid_range: "auto | off",
            status: ConfigRuntimeStatus::Active,
            caveat: None,
        },
        ConfigKnob {
            key: SEARCH_RERANK_TOP_K_KEY,
            category: "search",
            effect: "Candidate pool size collected for reranking before truncating to the requested search limit.",
            valid_range: "positive integer",
            status: ConfigRuntimeStatus::Active,
            caveat: None,
        },
        ConfigKnob {
            key: SEARCH_SEMANTIC_WEIGHT_KEY,
            category: "search",
            effect: "Intended fusion weight for the vector (semantic) tier when combining ranked result lists.",
            valid_range: "0.0 ..= 1.0",
            // The analyst's trap: this setting is active, but the name does
            // not guarantee a neural vector tier.
            status: ConfigRuntimeStatus::Active,
            caveat: Some(
                "Does not imply neural semantic search by itself: this weights the active vector tier, which may be a neural model or the deterministic hash fallback. Check `ee index status` for the active embedding mode (see ADR 0070 and the bundled-embeddings work, bd-1et0v).",
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
    /// Advisory lint findings relevant to this key.
    pub lint_findings: Vec<ConfigLintFinding>,
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
            "lintFindings": self
                .lint_findings
                .iter()
                .map(ConfigLintFinding::data_json)
                .collect::<Vec<_>>(),
        })
    }

    /// Attach key-specific findings. Search explanations also carry model-path
    /// and ignored-environment advisories: those inputs affect retrieval but
    /// are not TOML keys in the knob registry. Unknown EE_* names are reported
    /// here as configuration diagnostics, without their values. Other config
    /// categories retain exact-key scoping.
    #[must_use]
    pub fn with_lint_findings(mut self, findings: &[ConfigLintFinding]) -> Self {
        self.lint_findings = findings
            .iter()
            .filter(|finding| {
                finding.key == self.knob.key
                    || (self.knob.category == "search"
                        && matches!(
                            finding.code,
                            LINT_EMBEDDING_MODEL_PATH_MISSING | LINT_UNKNOWN_ENV_VAR
                        ))
            })
            .cloned()
            .collect();
        self
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
        lint_findings: Vec::new(),
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

    /// Render the advisory finding in the `ee.config_explain.v1` shape.
    #[must_use]
    pub fn data_json(&self) -> Value {
        json!({
            "code": self.code,
            "key": self.key,
            "message": self.message,
            "severity": self.severity,
        })
    }
}

/// Lint code: a semantic weight is set but neural retrieval is unavailable.
pub const LINT_WEIGHT_WITHOUT_NEURAL_TIER: &str = "config_semantic_weight_without_neural_tier";
/// Lint code: an embedding-model path is configured but does not exist on disk.
pub const LINT_EMBEDDING_MODEL_PATH_MISSING: &str = "config_embedding_model_path_missing";
/// Lint code: an environment variable was set that ee does not consume.
pub const LINT_UNKNOWN_ENV_VAR: &str = "config_unknown_env_var";
/// Lint code: a configured task lens selects lexical-only retrieval while a
/// semantic weight is also set.
pub const LINT_CONTRADICTORY_LEXICAL_MODE: &str = "config_contradictory_lexical_mode";

/// Advisory for a known unavailable neural tier. Callers must not pass false
/// merely because a configuration-only inspection has not loaded a model.
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
                "search.semantic_weight={weight} is set, but neural semantic retrieval is unavailable in this configuration; the weight alone does not enable it. Check `ee index status` for the active embedding mode."
            ),
        )),
        _ => None,
    }
}

/// Advisory: an explicit embedding-model path is configured but does not exist.
#[must_use]
pub fn lint_embedding_model_path_missing(
    key: &str,
    configured_path: Option<&str>,
    path_exists: bool,
) -> Option<ConfigLintFinding> {
    match configured_path {
        Some(path) if !path_exists => {
            let message = if key == EnvVar::EmbedModelPath.name() {
                format!(
                    "{key} points at missing path `{path}`; this fault-injection override makes the search embedder unavailable. Unset it to stop simulating an unavailable model; it does not load alternate models."
                )
            } else {
                format!("{key}: configured embedding model path does not exist: {path}")
            };
            Some(ConfigLintFinding::advisory(
                LINT_EMBEDDING_MODEL_PATH_MISSING,
                key,
                message,
            ))
        }
        _ => None,
    }
}

/// Advisory: an EE_* or foreign embedding environment variable is set that ee
/// does not consume. Only its name is reported; its value is never retained.
#[must_use]
pub fn lint_unknown_env_var(var_name: &str, recognized: bool) -> Option<ConfigLintFinding> {
    if recognized {
        None
    } else {
        Some(ConfigLintFinding::advisory(
            LINT_UNKNOWN_ENV_VAR,
            var_name,
            format!(
                "environment variable `{var_name}` is set but `ee` does not consume it; it has no effect. See `ee capabilities --json` data.envOverrides[] for the variables ee honors."
            ),
        ))
    }
}

/// Advisory for the real task-lens source_mode setting. A lexical-only lens
/// makes the global semantic weight ineffective only when that lens is used.
#[must_use]
pub fn lint_contradictory_lexical_mode(
    lens_id: &str,
    source_mode: &str,
    semantic_weight: Option<f64>,
) -> Option<ConfigLintFinding> {
    let lexical_only = source_mode.eq_ignore_ascii_case("lexical_only")
        || source_mode.eq_ignore_ascii_case("lexical-only");
    match semantic_weight {
        Some(weight) if lexical_only => Some(ConfigLintFinding::advisory(
            LINT_CONTRADICTORY_LEXICAL_MODE,
            SEARCH_SEMANTIC_WEIGHT_KEY,
            format!(
                "task_lens.overrides entry `{lens_id}` sets source_mode=lexical_only, so search.semantic_weight={weight} has no effect when this lens is selected. Use source_mode=hybrid in that lens to include the vector tier."
            ),
        )),
        _ => None,
    }
}

/// Observed inputs to the pure lint rules. Unknown neural posture is distinct
/// from an observed unavailable tier.
#[derive(Clone, Debug, Default)]
pub struct ConfigLintFacts {
    /// Effective search.semantic_weight, if set.
    pub semantic_weight: Option<f64>,
    /// Whether neural retrieval is active, if known. None means uninspected.
    pub neural_tier_active: Option<bool>,
    /// Effective configured task-lens IDs and their source_mode values.
    pub task_lens_source_modes: Vec<(String, String)>,
    /// Explicit embedding-model environment keys, paths, and existence facts.
    pub embedding_model_paths: Vec<(String, String, bool)>,
    /// Environment variable names and registry recognition, never their values.
    pub env_vars: Vec<(String, bool)>,
}

/// Gather real config-explain facts without opening a database, initializing
/// an embedder, or downloading models. Path errors are unknown, not proof that
/// a path is missing. Existing model files do not prove an active neural tier.
#[must_use]
pub fn gather_config_lint_facts(config: &crate::config::ConfigFile) -> ConfigLintFacts {
    let mut facts = ConfigLintFacts {
        semantic_weight: config.search.semantic_weight,
        task_lens_source_modes: config
            .task_lens
            .overrides
            .iter()
            .filter_map(|lens| {
                lens.overlay
                    .source_mode
                    .as_ref()
                    .map(|mode| (lens.id.clone(), mode.clone()))
            })
            .collect(),
        env_vars: std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .filter(|name| name.starts_with("EE_"))
            .map(|name| {
                let recognized = EnvVar::all().iter().any(|var| var.name() == name);
                (name, recognized)
            })
            .collect(),
        ..ConfigLintFacts::default()
    };
    for trap in EmbeddingTrapEnvVar::all() {
        if trap.is_present() {
            facts.env_vars.push((trap.name().to_owned(), false));
        }
    }
    for variable in [EnvVar::EmbedModelDir, EnvVar::EmbedModelPath] {
        let Some(value) = read_os(variable).filter(|value| !value.is_empty()) else {
            continue;
        };
        // Match the retrieval path's UTF-8/trim handling for the fault injector.
        // The actual model-directory override accepts an unmodified OS path.
        let path = if variable == EnvVar::EmbedModelPath {
            let Some(value) = value.to_str().map(str::trim).filter(|value| !value.is_empty())
            else {
                continue;
            };
            PathBuf::from(value)
        } else {
            PathBuf::from(value)
        };
        let Ok(exists) = path.try_exists() else {
            continue;
        };
        if variable == EnvVar::EmbedModelPath && !exists {
            // resolve_source_mode treats this explicit fault as unavailable,
            // including when a neural model would otherwise be ready.
            facts.neural_tier_active = Some(false);
        }
        facts.embedding_model_paths.push((
            variable.name().to_owned(),
            path.to_string_lossy().into_owned(),
            exists,
        ));
    }
    facts
}

/// Run the rules over observed facts. Findings are always advisory and sorted
/// by (code, key, message), including multiple lenses concerning the same key.
#[must_use]
pub fn run_config_lint(facts: &ConfigLintFacts) -> Vec<ConfigLintFinding> {
    let mut findings = Vec::new();

    if let Some(neural_tier_active) = facts.neural_tier_active
        && let Some(finding) =
            lint_semantic_weight_without_neural_tier(facts.semantic_weight, neural_tier_active)
    {
        findings.push(finding);
    }
    for (lens_id, source_mode) in &facts.task_lens_source_modes {
        if let Some(finding) =
            lint_contradictory_lexical_mode(lens_id, source_mode, facts.semantic_weight)
        {
            findings.push(finding);
        }
    }
    for (key, path, exists) in &facts.embedding_model_paths {
        if let Some(finding) = lint_embedding_model_path_missing(key, Some(path), *exists) {
            findings.push(finding);
        }
    }
    for (name, recognized) in &facts.env_vars {
        if let Some(finding) = lint_unknown_env_var(name, *recognized) {
            findings.push(finding);
        }
    }

    findings.sort_by(|a, b| {
        a.code
            .cmp(b.code)
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.message.cmp(&b.message))
    });
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
        assert_eq!(knob.status, ConfigRuntimeStatus::Active);
        let caveat = knob.caveat.expect("semantic_weight must carry a caveat");
        assert!(
            caveat.contains("vector") && caveat.contains("neural"),
            "caveat must explain the vector-vs-neural truth"
        );
    }

    #[test]
    fn rerank_knobs_are_active_and_specific() {
        let mode = knob_for_key(SEARCH_RERANK_KEY).expect("search.rerank is covered");
        assert_eq!(mode.status, ConfigRuntimeStatus::Active);
        assert_eq!(mode.valid_range, "auto | off");
        assert!(
            mode.effect.contains("disabled before model lookup"),
            "rerank mode must document that off suppresses lookup"
        );

        let top_k = knob_for_key(SEARCH_RERANK_TOP_K_KEY).expect("rerank_top_k is covered");
        assert_eq!(top_k.status, ConfigRuntimeStatus::Active);
        assert_eq!(top_k.valid_range, "positive integer");
        assert!(
            top_k.effect.contains("Candidate pool size"),
            "rerank_top_k must describe the collect-limit effect"
        );
    }

    #[test]
    fn default_speed_knob_matches_parser_tokens() {
        let speed =
            knob_for_key(SEARCH_DEFAULT_SPEED_KEY).expect("search.default_speed is covered");
        assert_eq!(speed.status, ConfigRuntimeStatus::Active);
        assert_eq!(speed.valid_range, "fast | balanced | thorough");
        assert!(
            !speed.valid_range.contains("instant") && !speed.valid_range.contains("quality"),
            "config explain must not advertise query-schema speed tokens for search.default_speed"
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
        assert_eq!(value["status"], "active");
        assert!(value["caveat"].is_string());
        assert_eq!(
            value["lintFindings"].as_array().map(Vec::len),
            Some(0),
            "explanations always carry a deterministic lintFindings array"
        );
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
    fn explain_data_json_keeps_required_shape_when_values_are_absent() {
        let explanation = explain(MESH_ENABLED_KEY, None, None).expect("known key explains");
        let value = explanation.data_json();

        assert_eq!(value["schema"], CONFIG_EXPLAIN_SCHEMA_V1);
        assert_eq!(value["key"], MESH_ENABLED_KEY);
        assert_eq!(value["category"], "mesh");
        assert_eq!(value["status"], "active");
        assert_eq!(value["caveat"], Value::Null);
        assert_eq!(value["effectiveValue"], Value::Null);
        assert_eq!(value["sourceLayer"], "unknown");
        assert!(
            value["effect"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "effect must be populated for human-facing explain output"
        );
        assert!(
            value["validRange"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "valid range must be populated for human-facing explain output"
        );
        assert_eq!(
            value["lintFindings"].as_array().map(Vec::len),
            Some(0),
            "lintFindings is always present, even when empty"
        );
    }

    #[test]
    fn explain_source_layer_tokens_match_merge_sources() {
        for (source, expected) in [
            (ConfigValueSource::Cli, "cli"),
            (ConfigValueSource::Environment, "environment"),
            (ConfigValueSource::Project, "project"),
            (ConfigValueSource::User, "user"),
            (ConfigValueSource::Default, "default"),
        ] {
            let explanation = explain(MESH_ENABLED_KEY, Some("true".to_owned()), Some(source))
                .expect("known key explains");
            assert_eq!(explanation.source_layer, expected);
            assert_eq!(explanation.data_json()["sourceLayer"], expected);
        }
    }

    #[test]
    fn explain_attaches_search_environment_diagnostics() {
        let facts = ConfigLintFacts {
            semantic_weight: Some(0.45),
            neural_tier_active: Some(false),
            task_lens_source_modes: vec![("local-review".to_owned(), "lexical_only".to_owned())],
            embedding_model_paths: vec![(
                EnvVar::EmbedModelDir.name().to_owned(),
                "/no/such".to_owned(),
                false,
            )],
            env_vars: vec![("EE_UNRECOGNIZED_SETTING".to_owned(), false)],
        };
        let findings = run_config_lint(&facts);
        let explanation = explain(
            SEARCH_SEMANTIC_WEIGHT_KEY,
            Some("0.45".to_owned()),
            Some(ConfigValueSource::Project),
        )
        .expect("known key explains")
        .with_lint_findings(&findings);
        let json = explanation.data_json();
        let attached = json["lintFindings"]
            .as_array()
            .expect("lint findings array");
        assert_eq!(attached.len(), 4);
        assert!(
            attached.iter().any(|finding| {
                finding["code"] == LINT_UNKNOWN_ENV_VAR
                    && finding["key"] == "EE_UNRECOGNIZED_SETTING"
            }),
            "unknown environment names must not be dropped by exact-key filtering"
        );
        assert!(
            attached.iter().any(|finding| {
                finding["code"] == LINT_EMBEDDING_MODEL_PATH_MISSING
                    && finding["key"] == EnvVar::EmbedModelDir.name()
            }),
            "model-path advisories retain the real environment key"
        );
        assert!(
            attached
                .iter()
                .all(|finding| finding["severity"] == CONFIG_LINT_SEVERITY),
            "config-explain lint findings remain advisory-only"
        );
    }

    #[test]
    fn explain_ignores_empty_and_non_matching_lint_findings() {
        let unrelated = ConfigLintFinding::advisory(
            LINT_UNKNOWN_ENV_VAR,
            "EMBEDDING_MODEL",
            "foreign env var has no effect",
        );

        let empty = explain(
            MESH_ENABLED_KEY,
            Some("false".to_owned()),
            Some(ConfigValueSource::User),
        )
        .expect("known key explains")
        .with_lint_findings(&[]);
        assert!(empty.lint_findings.is_empty());
        assert_eq!(
            empty.data_json()["lintFindings"].as_array().map(Vec::len),
            Some(0)
        );

        let unrelated = explain(
            MESH_ENABLED_KEY,
            Some("false".to_owned()),
            Some(ConfigValueSource::User),
        )
        .expect("known key explains")
        .with_lint_findings(&[unrelated]);
        assert!(
            unrelated.lint_findings.is_empty(),
            "findings for other keys/env vars must not leak into this key"
        );
        assert_eq!(
            unrelated.data_json()["lintFindings"]
                .as_array()
                .map(Vec::len),
            Some(0)
        );
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
    fn lint_flags_semantic_weight_boundaries_without_neural_tier() {
        for weight in [0.0, 1.0] {
            let finding = lint_semantic_weight_without_neural_tier(Some(weight), false)
                .expect("boundary weights still need the neural-tier honesty warning");
            assert_eq!(finding.code, LINT_WEIGHT_WITHOUT_NEURAL_TIER);
            assert_eq!(finding.key.as_str(), SEARCH_SEMANTIC_WEIGHT_KEY);
            assert!(
                finding
                    .message
                    .contains(&format!("search.semantic_weight={weight}")),
                "finding should report the exact configured boundary weight"
            );
        }
    }

    #[test]
    fn lint_flags_missing_embedding_model_path() {
        let key = EnvVar::EmbedModelDir.name();
        let finding = lint_embedding_model_path_missing(key, Some("/no/such/model"), false)
            .expect("a missing model path is suspicious");
        assert_eq!(finding.code, LINT_EMBEDDING_MODEL_PATH_MISSING);
        assert_eq!(finding.key, key);
        assert!(lint_embedding_model_path_missing(key, Some("/exists"), true).is_none());
        assert!(lint_embedding_model_path_missing(key, None, false).is_none());

        let injected = lint_embedding_model_path_missing(
            EnvVar::EmbedModelPath.name(),
            Some("/no/such/model"),
            false,
        )
        .expect("the missing fault-injection path is diagnosed");
        assert!(injected.message.contains("fault-injection"));
        assert!(injected.message.contains("does not load alternate models"));
    }

    #[test]
    fn lint_flags_unknown_env_var() {
        let finding = lint_unknown_env_var("EMBEDDING_MODEL", false).expect("unknown var flagged");
        assert_eq!(finding.code, LINT_UNKNOWN_ENV_VAR);
        assert!(
            finding.message.contains("ee capabilities --json")
                && finding.message.contains("data.envOverrides[]"),
            "unknown-env lint must point at the shipped env-var discovery surface"
        );
        assert!(
            !finding.message.contains("ee config env"),
            "unknown-env lint must not point at the nonexistent `ee config env` command"
        );
        assert!(lint_unknown_env_var("EE_DB", true).is_none());
    }

    #[test]
    fn lint_flags_real_lexical_lens_source_modes() {
        for mode in ["lexical-only", "lexical_only"] {
            let finding = lint_contradictory_lexical_mode("local-review", mode, Some(0.45))
                .expect("lexical-only lens + semantic weight is diagnosed");
            assert_eq!(finding.code, LINT_CONTRADICTORY_LEXICAL_MODE);
            assert!(finding.message.contains("local-review"));
            assert!(finding.message.contains("when this lens is selected"));
        }
        for mode in ["hybrid", "semantic_only", "fast", "lexical"] {
            assert!(lint_contradictory_lexical_mode("local-review", mode, Some(0.45)).is_none());
        }
        assert!(lint_contradictory_lexical_mode("local-review", "lexical_only", None).is_none());
    }

    #[test]
    fn run_config_lint_is_advisory_only_and_deterministic() {
        let facts = ConfigLintFacts {
            semantic_weight: Some(0.45),
            neural_tier_active: Some(false),
            task_lens_source_modes: vec![("local-review".to_owned(), "lexical_only".to_owned())],
            embedding_model_paths: vec![(
                EnvVar::EmbedModelDir.name().to_owned(),
                "/no/such".to_owned(),
                false,
            )],
            env_vars: vec![
                ("EMBEDDING_MODEL".to_owned(), false),
                (EnvVar::DatabasePath.name().to_owned(), true),
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
    fn run_config_lint_orders_same_code_findings_by_key() {
        let facts = ConfigLintFacts {
            env_vars: vec![
                ("EE_Z_UNUSED".to_owned(), false),
                ("EE_A_UNUSED".to_owned(), false),
                (EnvVar::DatabasePath.name().to_owned(), true),
            ],
            ..ConfigLintFacts::default()
        };
        let findings = run_config_lint(&facts);

        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].code, LINT_UNKNOWN_ENV_VAR);
        assert_eq!(findings[0].key.as_str(), "EE_A_UNUSED");
        assert_eq!(findings[1].code, LINT_UNKNOWN_ENV_VAR);
        assert_eq!(findings[1].key.as_str(), "EE_Z_UNUSED");
    }

    #[test]
    fn run_config_lint_clean_config_has_no_findings() {
        let facts = ConfigLintFacts {
            semantic_weight: Some(0.45),
            neural_tier_active: Some(true),
            task_lens_source_modes: vec![("local-review".to_owned(), "hybrid".to_owned())],
            embedding_model_paths: Vec::new(),
            env_vars: vec![(EnvVar::DatabasePath.name().to_owned(), true)],
        };
        assert!(run_config_lint(&facts).is_empty());
    }

    #[test]
    fn run_config_lint_preserves_unknown_neural_posture() {
        let mut facts = ConfigLintFacts {
            semantic_weight: Some(0.45),
            ..ConfigLintFacts::default()
        };
        assert!(run_config_lint(&facts).is_empty());
        facts.neural_tier_active = Some(false);
        let findings = run_config_lint(&facts);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, LINT_WEIGHT_WITHOUT_NEURAL_TIER);
        assert!(!findings[0].message.contains("deterministic hash"));
        facts.neural_tier_active = Some(true);
        assert!(run_config_lint(&facts).is_empty());
    }

    #[test]
    fn run_config_lint_orders_multiple_lenses_for_the_same_key() {
        let mut facts = ConfigLintFacts {
            semantic_weight: Some(0.45),
            task_lens_source_modes: vec![
                ("z-review".to_owned(), "lexical_only".to_owned()),
                ("a-review".to_owned(), "lexical_only".to_owned()),
            ],
            ..ConfigLintFacts::default()
        };
        let findings = run_config_lint(&facts);
        assert_eq!(findings.len(), 2);
        facts.task_lens_source_modes.reverse();
        assert_eq!(findings, run_config_lint(&facts));
    }
}
