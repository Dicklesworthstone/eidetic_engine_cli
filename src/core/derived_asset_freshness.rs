//! Deterministic freshness planner for rebuildable derived assets.
//!
//! The planner is intentionally pure: callers provide the source watermarks,
//! config sections, feature flags, workspace identity, and optional input
//! manifest hash they already know. The result is a stable verdict and
//! dependency hash that status, doctor, support bundles, and rebuild commands
//! can render without re-implementing stale checks per asset.

use serde::Serialize;

pub const DERIVED_ASSET_FRESHNESS_SCHEMA_V1: &str = "ee.derived_asset_freshness.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivedAssetFreshnessVerdict {
    Fresh,
    Stale,
    Missing,
    Incompatible,
    RebuildNeeded,
    NotInspected,
    Unavailable,
}

impl DerivedAssetFreshnessVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Missing => "missing",
            Self::Incompatible => "incompatible",
            Self::RebuildNeeded => "rebuild_needed",
            Self::NotInspected => "not_inspected",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FreshnessDependency {
    pub category: &'static str,
    pub key: String,
    pub value: String,
}

impl FreshnessDependency {
    #[must_use]
    pub fn new(category: &'static str, key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            category,
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedAssetFreshnessInput {
    pub asset_id: &'static str,
    pub asset_kind: &'static str,
    pub inspected: bool,
    pub available: bool,
    pub artifact_present: bool,
    pub artifact_compatible: bool,
    pub source_high_watermark: Option<u64>,
    pub asset_high_watermark: Option<u64>,
    pub source_dependencies: Vec<FreshnessDependency>,
    pub config_dependencies: Vec<FreshnessDependency>,
    pub feature_dependencies: Vec<FreshnessDependency>,
    pub input_manifest_hash: Option<String>,
    /// Dependency hash the asset was last built against. When provided
    /// AND different from the newly computed dependency hash, the
    /// planner emits `Stale` even if source watermarks still match;
    /// this catches config / feature-flag / input-manifest drift that
    /// happens without source-table mutation. `None` skips the check
    /// (callers that don't track a baseline hash treat all unmatched
    /// states as Fresh provided the watermarks agree exactly).
    pub previous_dependency_hash: Option<String>,
    pub repair_action: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedAssetFreshnessReport {
    pub schema: &'static str,
    pub verdict: DerivedAssetFreshnessVerdict,
    pub dependency_hash: String,
    pub source_dependency_hash: String,
    pub config_hash: String,
    pub feature_flags_hash: String,
    pub input_manifest_hash: Option<String>,
    pub invalidates: Vec<&'static str>,
    pub repair_action: &'static str,
}

#[must_use]
pub fn plan_derived_asset_freshness(
    input: DerivedAssetFreshnessInput,
) -> DerivedAssetFreshnessReport {
    let source_dependency_hash = hash_dependencies(&input.source_dependencies);
    let config_hash = hash_dependencies(&input.config_dependencies);
    let feature_flags_hash = hash_dependencies(&input.feature_dependencies);
    let dependency_hash = hash_planner_input(
        &input,
        &source_dependency_hash,
        &config_hash,
        &feature_flags_hash,
    );
    let verdict = freshness_verdict(&input, &dependency_hash);
    let invalidates = invalidation_scope(verdict, input.asset_id);

    DerivedAssetFreshnessReport {
        schema: DERIVED_ASSET_FRESHNESS_SCHEMA_V1,
        verdict,
        dependency_hash,
        source_dependency_hash,
        config_hash,
        feature_flags_hash,
        input_manifest_hash: input.input_manifest_hash,
        invalidates,
        repair_action: input.repair_action,
    }
}

#[must_use]
pub fn hash_dependencies(dependencies: &[FreshnessDependency]) -> String {
    let mut sorted = dependencies.to_vec();
    sorted.sort_by(|left, right| {
        (left.category, left.key.as_str(), left.value.as_str()).cmp(&(
            right.category,
            right.key.as_str(),
            right.value.as_str(),
        ))
    });

    let mut hasher = blake3::Hasher::new();
    hasher.update(DERIVED_ASSET_FRESHNESS_SCHEMA_V1.as_bytes());
    hasher.update(b"\0");
    for dependency in sorted {
        hasher.update(dependency.category.as_bytes());
        hasher.update(b"\0");
        hasher.update(dependency.key.as_bytes());
        hasher.update(b"\0");
        hasher.update(dependency.value.as_bytes());
        hasher.update(b"\0");
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_planner_input(
    input: &DerivedAssetFreshnessInput,
    source_dependency_hash: &str,
    config_hash: &str,
    feature_flags_hash: &str,
) -> String {
    hash_dependencies(&[
        FreshnessDependency::new("identity", "asset_id", input.asset_id),
        FreshnessDependency::new("identity", "asset_kind", input.asset_kind),
        FreshnessDependency::new(
            "source",
            "source_high_watermark",
            watermark_value(input.source_high_watermark),
        ),
        FreshnessDependency::new(
            "asset",
            "asset_high_watermark",
            watermark_value(input.asset_high_watermark),
        ),
        FreshnessDependency::new("source", "dependency_hash", source_dependency_hash),
        FreshnessDependency::new("config", "dependency_hash", config_hash),
        FreshnessDependency::new("feature", "dependency_hash", feature_flags_hash),
        FreshnessDependency::new(
            "manifest",
            "input_manifest_hash",
            input.input_manifest_hash.as_deref().unwrap_or("none"),
        ),
    ])
}

fn freshness_verdict(
    input: &DerivedAssetFreshnessInput,
    current_dependency_hash: &str,
) -> DerivedAssetFreshnessVerdict {
    if !input.inspected {
        return DerivedAssetFreshnessVerdict::NotInspected;
    }
    if !input.available {
        return DerivedAssetFreshnessVerdict::Unavailable;
    }
    if !input.artifact_present {
        return DerivedAssetFreshnessVerdict::Missing;
    }
    if !input.artifact_compatible {
        return DerivedAssetFreshnessVerdict::Incompatible;
    }
    match (input.source_high_watermark, input.asset_high_watermark) {
        (Some(source), Some(asset)) if source > asset => {
            return DerivedAssetFreshnessVerdict::RebuildNeeded;
        }
        (Some(source), Some(asset)) if asset > source => {
            return DerivedAssetFreshnessVerdict::Incompatible;
        }
        (Some(_), None) => return DerivedAssetFreshnessVerdict::RebuildNeeded,
        _ => {}
    }
    // Watermarks match (or are absent). If the caller tracks a baseline
    // dependency hash and the current hash differs, the asset is Stale —
    // config / feature flags / input manifest drifted without a
    // corresponding source-table mutation. Without a baseline hash the
    // planner cannot honestly emit Stale and defaults to Fresh.
    if let Some(previous) = input.previous_dependency_hash.as_deref() {
        if previous != current_dependency_hash {
            return DerivedAssetFreshnessVerdict::Stale;
        }
    }
    DerivedAssetFreshnessVerdict::Fresh
}

fn invalidation_scope(
    verdict: DerivedAssetFreshnessVerdict,
    asset_id: &'static str,
) -> Vec<&'static str> {
    match verdict {
        DerivedAssetFreshnessVerdict::Fresh
        | DerivedAssetFreshnessVerdict::NotInspected
        | DerivedAssetFreshnessVerdict::Unavailable => Vec::new(),
        DerivedAssetFreshnessVerdict::Stale
        | DerivedAssetFreshnessVerdict::Missing
        | DerivedAssetFreshnessVerdict::Incompatible
        | DerivedAssetFreshnessVerdict::RebuildNeeded => vec![asset_id],
    }
}

fn watermark_value(value: Option<u64>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency(category: &'static str, key: &str, value: &str) -> FreshnessDependency {
        FreshnessDependency::new(category, key, value)
    }

    fn planner_input() -> DerivedAssetFreshnessInput {
        DerivedAssetFreshnessInput {
            asset_id: "search_index",
            asset_kind: "persisted_index",
            inspected: true,
            available: true,
            artifact_present: true,
            artifact_compatible: true,
            source_high_watermark: Some(7),
            asset_high_watermark: Some(7),
            source_dependencies: vec![dependency("source", "memories", "7")],
            config_dependencies: vec![dependency("config", "storage.index_dir", ".ee/index")],
            feature_dependencies: vec![dependency("feature", "lexical-bm25", "true")],
            input_manifest_hash: Some("blake3:manifest".to_owned()),
            previous_dependency_hash: None,
            repair_action: "ee index rebuild --workspace .",
        }
    }

    #[test]
    fn dependency_hash_is_order_independent() {
        let first = vec![
            dependency("config", "b", "2"),
            dependency("config", "a", "1"),
        ];
        let second = vec![
            dependency("config", "a", "1"),
            dependency("config", "b", "2"),
        ];

        assert_eq!(hash_dependencies(&first), hash_dependencies(&second));
    }

    #[test]
    fn source_watermark_change_requires_rebuild() {
        let mut input = planner_input();
        input.source_high_watermark = Some(9);
        input.asset_high_watermark = Some(7);

        let report = plan_derived_asset_freshness(input);

        assert_eq!(report.verdict, DerivedAssetFreshnessVerdict::RebuildNeeded);
        assert_eq!(report.invalidates, vec!["search_index"]);
    }

    #[test]
    fn asset_ahead_of_source_is_incompatible_not_fresh() {
        let mut input = planner_input();
        input.source_high_watermark = Some(7);
        input.asset_high_watermark = Some(9);

        let report = plan_derived_asset_freshness(input);

        assert_eq!(report.verdict, DerivedAssetFreshnessVerdict::Incompatible);
        assert_eq!(report.invalidates, vec!["search_index"]);
        assert_eq!(report.repair_action, "ee index rebuild --workspace .");
    }

    #[test]
    fn config_and_feature_changes_alter_dependency_hashes() {
        let base = plan_derived_asset_freshness(planner_input());
        let mut changed_config = planner_input();
        changed_config.config_dependencies =
            vec![dependency("config", "storage.index_dir", ".ee/other-index")];
        let changed_config = plan_derived_asset_freshness(changed_config);
        let mut changed_feature = planner_input();
        changed_feature.feature_dependencies = vec![dependency("feature", "lexical-bm25", "false")];
        let changed_feature = plan_derived_asset_freshness(changed_feature);

        assert_ne!(base.config_hash, changed_config.config_hash);
        assert_ne!(base.dependency_hash, changed_config.dependency_hash);
        assert_ne!(base.feature_flags_hash, changed_feature.feature_flags_hash);
        assert_ne!(base.dependency_hash, changed_feature.dependency_hash);
    }

    #[test]
    fn missing_and_incompatible_artifacts_get_distinct_verdicts() {
        let mut missing = planner_input();
        missing.artifact_present = false;
        let mut incompatible = planner_input();
        incompatible.artifact_compatible = false;

        assert_eq!(
            plan_derived_asset_freshness(missing).verdict,
            DerivedAssetFreshnessVerdict::Missing
        );
        assert_eq!(
            plan_derived_asset_freshness(incompatible).verdict,
            DerivedAssetFreshnessVerdict::Incompatible
        );
    }

    #[test]
    fn previous_dependency_hash_mismatch_emits_stale_verdict() {
        // Baseline: compute the current dependency hash, then seed a
        // planner input whose previous_dependency_hash is something
        // OTHER than that current hash. With watermarks still matching,
        // the planner must emit Stale (caught by config/feature/manifest
        // drift detection added in this slice).
        let mut shifted = planner_input();
        shifted.previous_dependency_hash = Some("blake3:0000000000000000".to_owned());
        let report = plan_derived_asset_freshness(shifted);
        assert_eq!(report.verdict, DerivedAssetFreshnessVerdict::Stale);
        assert_eq!(report.invalidates, vec!["search_index"]);
        assert_eq!(report.repair_action, "ee index rebuild --workspace .");
    }

    #[test]
    fn matching_previous_dependency_hash_keeps_verdict_fresh() {
        // First pass: get the current hash.
        let baseline = plan_derived_asset_freshness(planner_input());
        // Second pass: round-trip the recorded hash back in. The planner
        // must agree the asset is still Fresh (idempotence guarantee).
        let mut roundtrip = planner_input();
        roundtrip.previous_dependency_hash = Some(baseline.dependency_hash.clone());
        let again = plan_derived_asset_freshness(roundtrip);
        assert_eq!(again.verdict, DerivedAssetFreshnessVerdict::Fresh);
        assert_eq!(again.dependency_hash, baseline.dependency_hash);
        assert!(again.invalidates.is_empty());
    }

    #[test]
    fn planner_is_deterministic_across_repeat_calls() {
        let a = plan_derived_asset_freshness(planner_input());
        let b = plan_derived_asset_freshness(planner_input());
        assert_eq!(a, b);
        let a_json = serde_json::to_string(&a).expect("serialize a");
        let b_json = serde_json::to_string(&b).expect("serialize b");
        assert_eq!(a_json, b_json);
    }

    #[test]
    fn five_acceptance_verdicts_round_trip_with_stable_invalidates() {
        // The bead acceptance says callers receive one of fresh, stale,
        // missing, incompatible, or rebuild_needed. Pin each by name +
        // its invalidates[] shape (empty for Fresh; [asset_id] for the
        // four invalidating verdicts).
        let fresh = plan_derived_asset_freshness(planner_input());
        assert_eq!(fresh.verdict, DerivedAssetFreshnessVerdict::Fresh);
        assert!(fresh.invalidates.is_empty());

        let mut stale = planner_input();
        stale.previous_dependency_hash = Some("blake3:11".to_owned());
        let stale = plan_derived_asset_freshness(stale);
        assert_eq!(stale.verdict, DerivedAssetFreshnessVerdict::Stale);
        assert_eq!(stale.invalidates, vec!["search_index"]);

        let mut missing = planner_input();
        missing.artifact_present = false;
        let missing = plan_derived_asset_freshness(missing);
        assert_eq!(missing.verdict, DerivedAssetFreshnessVerdict::Missing);
        assert_eq!(missing.invalidates, vec!["search_index"]);

        let mut incompatible = planner_input();
        incompatible.artifact_compatible = false;
        let incompatible = plan_derived_asset_freshness(incompatible);
        assert_eq!(
            incompatible.verdict,
            DerivedAssetFreshnessVerdict::Incompatible
        );
        assert_eq!(incompatible.invalidates, vec!["search_index"]);

        let mut rebuild = planner_input();
        rebuild.source_high_watermark = Some(11);
        rebuild.asset_high_watermark = Some(7);
        let rebuild = plan_derived_asset_freshness(rebuild);
        assert_eq!(rebuild.verdict, DerivedAssetFreshnessVerdict::RebuildNeeded);
        assert_eq!(rebuild.invalidates, vec!["search_index"]);
    }
}
