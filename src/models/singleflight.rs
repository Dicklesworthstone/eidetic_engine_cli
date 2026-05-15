//! Canonical single-flight keys for duplicate read-heavy operations.
//!
//! These keys are intentionally redaction-safe: callers provide raw query text
//! only long enough to hash it, and the serialized key stores only hashes plus
//! output-affecting options.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SINGLEFLIGHT_KEY_SCHEMA_V1: &str = "ee.singleflight.key.v1";
pub const SINGLEFLIGHT_POSTURE_SCHEMA_V1: &str = "ee.singleflight.posture.v1";
pub const SINGLEFLIGHT_KEY_CANONICAL_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SingleFlightSurface {
    Context,
    Search,
    GraphSnapshot,
    GraphFeatureEnrichment,
}

impl SingleFlightSurface {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Search => "search",
            Self::GraphSnapshot => "graph_snapshot",
            Self::GraphFeatureEnrichment => "graph_feature_enrichment",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SingleFlightKeyInput<'a> {
    pub surface: SingleFlightSurface,
    pub workspace_identity: &'a str,
    pub workspace_generation: u64,
    pub index_generation: Option<u64>,
    pub graph_generation: Option<u64>,
    pub output_schema: &'a str,
    pub query_text: Option<&'a str>,
    pub query_shape_hash: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub max_tokens: Option<u32>,
    pub as_of: Option<&'a str>,
    pub source_mode: Option<&'a str>,
    pub redaction_level: Option<&'a str>,
    pub explain: bool,
    pub verbose: bool,
    pub feature_flags: &'a [&'a str],
    pub option_pairs: &'a [(&'a str, &'a str)],
}

impl<'a> SingleFlightKeyInput<'a> {
    #[must_use]
    pub const fn new(
        surface: SingleFlightSurface,
        workspace_identity: &'a str,
        workspace_generation: u64,
        output_schema: &'a str,
    ) -> Self {
        Self {
            surface,
            workspace_identity,
            workspace_generation,
            index_generation: None,
            graph_generation: None,
            output_schema,
            query_text: None,
            query_shape_hash: None,
            profile: None,
            max_tokens: None,
            as_of: None,
            source_mode: None,
            redaction_level: None,
            explain: false,
            verbose: false,
            feature_flags: &[],
            option_pairs: &[],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SingleFlightKey {
    pub schema: String,
    pub canonical_version: u32,
    pub key_hash: String,
    pub surface: SingleFlightSurface,
    pub workspace_hash: String,
    pub workspace_generation: u64,
    pub index_generation: Option<u64>,
    pub graph_generation: Option<u64>,
    pub output_schema: String,
    pub query_shape_hash: Option<String>,
    pub option_hash: String,
    pub feature_flag_hash: String,
    pub profile: Option<String>,
    pub max_tokens: Option<u32>,
    pub as_of: Option<String>,
    pub source_mode: Option<String>,
    pub redaction_level: Option<String>,
    pub explain: bool,
    pub verbose: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SingleFlightSurfacePosture {
    pub surface: SingleFlightSurface,
    pub status: String,
    pub configured: bool,
    pub active_leader_count: u32,
    pub leader_start_count: u64,
    pub completed_leader_count: u64,
    pub follower_join_count: u64,
    pub follower_timeout_count: u64,
    pub leader_failure_count: u64,
    pub reused_result_count: u64,
    pub state_poisoned_count: u64,
    pub follower_timeout_ms: u64,
    pub suggested_action: String,
}

impl SingleFlightSurfacePosture {
    #[must_use]
    pub fn new(
        surface: SingleFlightSurface,
        configured: bool,
        active_leader_count: u32,
        counters: SingleFlightSurfaceCounters,
        follower_timeout_ms: u64,
    ) -> Self {
        let status = if counters.state_poisoned_count > 0 {
            "state_poisoned"
        } else if counters.follower_timeout_count > 0 || counters.leader_failure_count > 0 {
            "observed_failures"
        } else if active_leader_count > 0 {
            "active"
        } else if configured {
            "idle"
        } else {
            "unconfigured"
        };
        let suggested_action = match status {
            "state_poisoned" => {
                "restart the process to clear poisoned in-memory single-flight state"
            }
            "observed_failures" => "inspect degraded entries before rerunning duplicate work",
            "active" => "wait for the active leader or lower request pressure",
            "idle" => "no action; single-flight is available for duplicate read-heavy work",
            _ => "enable a configured single-flight surface before expecting coalescing",
        };

        Self {
            surface,
            status: status.to_owned(),
            configured,
            active_leader_count,
            leader_start_count: counters.leader_start_count,
            completed_leader_count: counters.completed_leader_count,
            follower_join_count: counters.follower_join_count,
            follower_timeout_count: counters.follower_timeout_count,
            leader_failure_count: counters.leader_failure_count,
            reused_result_count: counters.reused_result_count,
            state_poisoned_count: counters.state_poisoned_count,
            follower_timeout_ms,
            suggested_action: suggested_action.to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SingleFlightSurfaceCounters {
    pub leader_start_count: u64,
    pub completed_leader_count: u64,
    pub follower_join_count: u64,
    pub follower_timeout_count: u64,
    pub leader_failure_count: u64,
    pub reused_result_count: u64,
    pub state_poisoned_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SingleFlightPostureReport {
    pub schema: String,
    pub status: String,
    pub configured_surface_count: u32,
    pub active_leader_count: u32,
    pub leader_start_count: u64,
    pub follower_wait_count: u64,
    pub follower_timeout_count: u64,
    pub leader_failure_count: u64,
    pub reused_result_count: u64,
    pub surfaces: Vec<SingleFlightSurfacePosture>,
}

impl SingleFlightPostureReport {
    #[must_use]
    pub fn from_surfaces(surfaces: Vec<SingleFlightSurfacePosture>) -> Self {
        let configured_surface_count =
            u32::try_from(surfaces.iter().filter(|surface| surface.configured).count())
                .unwrap_or(u32::MAX);
        let active_leader_count = surfaces
            .iter()
            .map(|surface| surface.active_leader_count)
            .fold(0_u32, u32::saturating_add);
        let follower_wait_count = surfaces
            .iter()
            .map(|surface| surface.follower_join_count)
            .fold(0_u64, u64::saturating_add);
        let leader_start_count = surfaces
            .iter()
            .map(|surface| surface.leader_start_count)
            .fold(0_u64, u64::saturating_add);
        let follower_timeout_count = surfaces
            .iter()
            .map(|surface| surface.follower_timeout_count)
            .fold(0_u64, u64::saturating_add);
        let leader_failure_count = surfaces
            .iter()
            .map(|surface| surface.leader_failure_count)
            .fold(0_u64, u64::saturating_add);
        let reused_result_count = surfaces
            .iter()
            .map(|surface| surface.reused_result_count)
            .fold(0_u64, u64::saturating_add);
        let status = if surfaces
            .iter()
            .any(|surface| surface.status == "state_poisoned")
        {
            "state_poisoned"
        } else if follower_timeout_count > 0 || leader_failure_count > 0 {
            "observed_failures"
        } else if active_leader_count > 0 {
            "active"
        } else {
            "idle"
        };

        Self {
            schema: SINGLEFLIGHT_POSTURE_SCHEMA_V1.to_owned(),
            status: status.to_owned(),
            configured_surface_count,
            active_leader_count,
            leader_start_count,
            follower_wait_count,
            follower_timeout_count,
            leader_failure_count,
            reused_result_count,
            surfaces,
        }
    }
}

impl SingleFlightKey {
    #[must_use]
    pub fn from_input(input: &SingleFlightKeyInput<'_>) -> Self {
        let query_shape_hash = input
            .query_shape_hash
            .and_then(non_empty)
            .map(ToOwned::to_owned)
            .or_else(|| input.query_text.and_then(non_empty).map(query_shape_hash));
        let option_hash = option_pairs_hash(input.option_pairs);
        let feature_flag_hash = string_list_hash("singleflight.feature_flags", input.feature_flags);
        let workspace_hash = redacted_hash("singleflight.workspace", input.workspace_identity);

        let mut key = Self {
            schema: SINGLEFLIGHT_KEY_SCHEMA_V1.to_owned(),
            canonical_version: SINGLEFLIGHT_KEY_CANONICAL_VERSION,
            key_hash: String::new(),
            surface: input.surface,
            workspace_hash,
            workspace_generation: input.workspace_generation,
            index_generation: input.index_generation,
            graph_generation: input.graph_generation,
            output_schema: input.output_schema.to_owned(),
            query_shape_hash,
            option_hash,
            feature_flag_hash,
            profile: normalized(input.profile),
            max_tokens: input.max_tokens,
            as_of: normalized(input.as_of),
            source_mode: normalized(input.source_mode),
            redaction_level: normalized(input.redaction_level),
            explain: input.explain,
            verbose: input.verbose,
        };
        key.key_hash = key.canonical_hash();
        key
    }

    #[must_use]
    pub fn canonical_hash(&self) -> String {
        let mut lines = Vec::with_capacity(18);
        lines.push(format!("schema={}", self.schema));
        lines.push(format!("canonicalVersion={}", self.canonical_version));
        lines.push(format!("surface={}", self.surface.as_str()));
        lines.push(format!("workspaceHash={}", self.workspace_hash));
        lines.push(format!("workspaceGeneration={}", self.workspace_generation));
        lines.push(format!(
            "indexGeneration={}",
            optional_u64(self.index_generation)
        ));
        lines.push(format!(
            "graphGeneration={}",
            optional_u64(self.graph_generation)
        ));
        lines.push(format!("outputSchema={}", self.output_schema));
        lines.push(format!(
            "queryShapeHash={}",
            optional_str(self.query_shape_hash.as_deref())
        ));
        lines.push(format!("optionHash={}", self.option_hash));
        lines.push(format!("featureFlagHash={}", self.feature_flag_hash));
        lines.push(format!("profile={}", optional_str(self.profile.as_deref())));
        lines.push(format!("maxTokens={}", optional_u32(self.max_tokens)));
        lines.push(format!("asOf={}", optional_str(self.as_of.as_deref())));
        lines.push(format!(
            "sourceMode={}",
            optional_str(self.source_mode.as_deref())
        ));
        lines.push(format!(
            "redactionLevel={}",
            optional_str(self.redaction_level.as_deref())
        ));
        lines.push(format!("explain={}", self.explain));
        lines.push(format!("verbose={}", self.verbose));
        redacted_hash("singleflight.key", &lines.join("\n"))
    }
}

#[must_use]
pub fn query_shape_hash(query_text: &str) -> String {
    redacted_hash(
        "singleflight.query_shape",
        &normalized_query_shape(query_text),
    )
}

#[must_use]
pub fn sample_singleflight_keys() -> Vec<SingleFlightKey> {
    let mut context = SingleFlightKeyInput::new(
        SingleFlightSurface::Context,
        "/workspace/eidetic_engine_cli",
        42,
        "ee.context.v1",
    );
    context.index_generation = Some(17);
    context.graph_generation = Some(9);
    context.query_text = Some("release token secret should not appear");
    context.profile = Some("balanced");
    context.max_tokens = Some(4000);
    context.source_mode = Some("hybrid");
    context.redaction_level = Some("standard");
    context.explain = true;
    context.feature_flags = &["graph", "lexical-bm25"];
    context.option_pairs = &[("format", "markdown"), ("packDna", "enabled")];

    let mut graph = SingleFlightKeyInput::new(
        SingleFlightSurface::GraphSnapshot,
        "/workspace/eidetic_engine_cli",
        42,
        "ee.graph.snapshot.v1",
    );
    graph.graph_generation = Some(9);
    graph.option_pairs = &[("graph", "memory_links")];

    vec![
        SingleFlightKey::from_input(&context),
        SingleFlightKey::from_input(&graph),
    ]
}

fn option_pairs_hash(pairs: &[(&str, &str)]) -> String {
    let mut normalized = BTreeMap::new();
    for (key, value) in pairs {
        if let (Some(key), Some(value)) = (non_empty(key), non_empty(value)) {
            normalized.insert(key.to_owned(), value.to_owned());
        }
    }

    let mut lines = Vec::with_capacity(normalized.len());
    for (key, value) in normalized {
        lines.push(format!("{key}={value}"));
    }
    redacted_hash("singleflight.options", &lines.join("\n"))
}

fn string_list_hash(label: &str, values: &[&str]) -> String {
    let mut normalized = values
        .iter()
        .filter_map(|value| non_empty(value))
        .collect::<Vec<_>>();
    normalized.sort_unstable();
    normalized.dedup();
    redacted_hash(label, &normalized.join("\n"))
}

fn normalized_query_shape(query_text: &str) -> String {
    query_text
        .split_whitespace()
        .map(|token| token.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn redacted_hash(label: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn normalized(value: Option<&str>) -> Option<String> {
    value.and_then(non_empty).map(ToOwned::to_owned)
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn optional_str(value: Option<&str>) -> &str {
    value.unwrap_or("<none>")
}

fn optional_u32(value: Option<u32>) -> String {
    value.map_or_else(|| "<none>".to_owned(), |value| value.to_string())
}

fn optional_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "<none>".to_owned(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const SINGLEFLIGHT_KEYS_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/singleflight/key_samples.json.golden");

    #[test]
    fn identical_inputs_produce_identical_keys() {
        let mut input = SingleFlightKeyInput::new(
            SingleFlightSurface::Search,
            "workspace-a",
            7,
            "ee.search.v1",
        );
        input.index_generation = Some(3);
        input.query_text = Some("same query");
        input.option_pairs = &[("sourceMode", "hybrid"), ("limit", "10")];

        let left = SingleFlightKey::from_input(&input);
        let right = SingleFlightKey::from_input(&input);
        assert_eq!(left, right);
        assert_eq!(left.key_hash, right.key_hash);
    }

    #[test]
    fn output_affecting_flags_change_key_hash() {
        let mut base = SingleFlightKeyInput::new(
            SingleFlightSurface::Context,
            "workspace-a",
            7,
            "ee.context.v1",
        );
        base.query_text = Some("same query");

        let mut explained = base.clone();
        explained.explain = true;

        let base_key = SingleFlightKey::from_input(&base);
        let explained_key = SingleFlightKey::from_input(&explained);
        assert_ne!(base_key.key_hash, explained_key.key_hash);
    }

    #[test]
    fn stale_generations_do_not_share_keys() {
        let mut first = SingleFlightKeyInput::new(
            SingleFlightSurface::Search,
            "workspace-a",
            7,
            "ee.search.v1",
        );
        first.index_generation = Some(3);

        let mut second = first.clone();
        second.index_generation = Some(4);

        assert_ne!(
            SingleFlightKey::from_input(&first).key_hash,
            SingleFlightKey::from_input(&second).key_hash
        );
    }

    #[test]
    fn key_serialization_excludes_raw_query_and_workspace() -> TestResult {
        let raw_query = "secret-token-123 should be redacted";
        let raw_workspace = "/private/user/project-with-secret-name";
        let mut input = SingleFlightKeyInput::new(
            SingleFlightSurface::Context,
            raw_workspace,
            1,
            "ee.context.v1",
        );
        input.query_text = Some(raw_query);
        input.option_pairs = &[("maxTokens", "4000")];

        let serialized = serde_json::to_string(&SingleFlightKey::from_input(&input))?;
        assert!(!serialized.contains(raw_query));
        assert!(!serialized.contains("secret-token-123"));
        assert!(!serialized.contains(raw_workspace));
        assert!(!serialized.contains("project-with-secret-name"));
        Ok(())
    }

    #[test]
    fn sorted_flags_and_options_are_canonical() {
        let mut left = SingleFlightKeyInput::new(
            SingleFlightSurface::Search,
            "workspace-a",
            7,
            "ee.search.v1",
        );
        left.feature_flags = &["graph", "fts5", "graph"];
        left.option_pairs = &[("limit", "10"), ("sourceMode", "hybrid")];

        let mut right = left.clone();
        right.feature_flags = &["fts5", "graph"];
        right.option_pairs = &[("sourceMode", "hybrid"), ("limit", "10")];

        assert_eq!(
            SingleFlightKey::from_input(&left).key_hash,
            SingleFlightKey::from_input(&right).key_hash
        );
    }

    #[test]
    fn sample_singleflight_keys_match_golden_fixture() -> TestResult {
        let json = serde_json::to_string_pretty(&sample_singleflight_keys())?;
        assert_eq!(json, SINGLEFLIGHT_KEYS_GOLDEN.trim_end_matches('\n'));
        Ok(())
    }
}
