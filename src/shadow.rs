//! Shadow-run gates for comparing policy candidates against incumbents (EE-367).
//!
//! Shadow-run gates execute both an incumbent and candidate policy on the same
//! input, comparing outputs without committing changes. This enables:
//!
//! - Safe A/B comparison of pack selection strategies
//! - Curation policy candidate evaluation
//! - Cache admission policy benchmarking
//!
//! All shadow runs are dry-run by default and emit comparison reports.

use std::fmt;

pub const SUBSYSTEM: &str = "shadow";
pub const SHADOW_REPORT_SCHEMA_V1: &str = "ee.shadow_report.v1";

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

/// Shadow-run mode controlling what actions are taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShadowMode {
    /// Compare only, no side effects.
    Compare,
    /// Compare and log metrics.
    CompareAndLog,
    /// Compare, log, and emit alerts on divergence.
    CompareLogAlert,
}

impl ShadowMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compare => "compare",
            Self::CompareAndLog => "compare_and_log",
            Self::CompareLogAlert => "compare_log_alert",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Compare, Self::CompareAndLog, Self::CompareLogAlert]
    }
}

impl fmt::Display for ShadowMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Policy domain being shadow-run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PolicyDomain {
    /// Pack selection policy (MMR, facility-location, etc.).
    PackSelection,
    /// Curation candidate filtering policy.
    CurationFilter,
    /// Cache admission/eviction policy.
    CacheAdmission,
}

impl PolicyDomain {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackSelection => "pack_selection",
            Self::CurationFilter => "curation_filter",
            Self::CacheAdmission => "cache_admission",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 3] {
        [
            Self::PackSelection,
            Self::CurationFilter,
            Self::CacheAdmission,
        ]
    }
}

impl fmt::Display for PolicyDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Verdict from comparing incumbent and candidate outputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShadowVerdict {
    /// Outputs are equivalent.
    Equivalent,
    /// Candidate produces better results.
    CandidateBetter,
    /// Incumbent produces better results.
    IncumbentBetter,
    /// Outputs differ but neither is clearly better.
    Divergent,
    /// Comparison could not be performed.
    Inconclusive,
}

impl ShadowVerdict {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Equivalent => "equivalent",
            Self::CandidateBetter => "candidate_better",
            Self::IncumbentBetter => "incumbent_better",
            Self::Divergent => "divergent",
            Self::Inconclusive => "inconclusive",
        }
    }

    #[must_use]
    pub const fn is_safe_to_promote(self) -> bool {
        matches!(self, Self::Equivalent | Self::CandidateBetter)
    }
}

impl fmt::Display for ShadowVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Safety guards that block policy promotion even when aggregate metrics improve.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShadowPromotionGuards {
    /// Candidate dropped a critical warning the incumbent kept.
    pub dropped_critical_warnings: bool,
    /// Candidate changed redaction posture or leaked a previously redacted field.
    pub redaction_differences: bool,
    /// Candidate regressed p99/tail latency beyond tolerance.
    pub p99_regression: bool,
    /// Candidate regressed catastrophic/tail-risk handling.
    pub tail_risk_regression: bool,
    /// Candidate mismatch exceeded the configured semantic tolerance.
    pub shadow_mismatch_above_tolerance: bool,
}

impl ShadowPromotionGuards {
    #[must_use]
    pub const fn blocks_promotion(&self) -> bool {
        self.dropped_critical_warnings
            || self.redaction_differences
            || self.p99_regression
            || self.tail_risk_regression
            || self.shadow_mismatch_above_tolerance
    }

    #[must_use]
    pub fn blocker_codes(&self) -> Vec<&'static str> {
        let mut codes = Vec::new();
        if self.dropped_critical_warnings {
            codes.push("dropped_critical_warnings");
        }
        if self.redaction_differences {
            codes.push("redaction_differences");
        }
        if self.p99_regression {
            codes.push("p99_regression");
        }
        if self.tail_risk_regression {
            codes.push("tail_risk_regression");
        }
        if self.shadow_mismatch_above_tolerance {
            codes.push("shadow_mismatch_above_tolerance");
        }
        codes
    }
}

#[must_use]
pub fn candidate_promotion_allowed(verdict: ShadowVerdict, guards: &ShadowPromotionGuards) -> bool {
    verdict.is_safe_to_promote() && !guards.blocks_promotion()
}

/// Comparison metrics from a shadow run.
#[derive(Clone, Debug, Default)]
pub struct ShadowMetrics {
    /// Size of incumbent output (items, bytes, etc.).
    pub incumbent_size: u64,
    /// Size of candidate output.
    pub candidate_size: u64,
    /// Quality score for incumbent (domain-specific).
    pub incumbent_quality: f64,
    /// Quality score for candidate.
    pub candidate_quality: f64,
    /// Overlap ratio between outputs (0.0 to 1.0).
    pub overlap_ratio: f64,
    /// Incumbent execution time in microseconds.
    pub incumbent_time_us: u64,
    /// Candidate execution time in microseconds.
    pub candidate_time_us: u64,
}

impl ShadowMetrics {
    #[must_use]
    pub fn quality_delta(&self) -> f64 {
        self.candidate_quality - self.incumbent_quality
    }

    #[must_use]
    pub fn size_delta(&self) -> i64 {
        self.candidate_size as i64 - self.incumbent_size as i64
    }

    #[must_use]
    pub fn time_delta_us(&self) -> i64 {
        self.candidate_time_us as i64 - self.incumbent_time_us as i64
    }
}

/// Configuration for shadow-run gates.
#[derive(Clone, Debug)]
pub struct ShadowGateConfig {
    /// Mode controlling side effects.
    pub mode: ShadowMode,
    /// Minimum quality improvement to consider candidate better.
    pub quality_threshold: f64,
    /// Maximum acceptable slowdown ratio (candidate_time / incumbent_time).
    pub max_slowdown_ratio: f64,
    /// Minimum overlap ratio required for equivalence verdict.
    pub min_overlap_for_equivalent: f64,
}

impl Default for ShadowGateConfig {
    fn default() -> Self {
        Self {
            mode: ShadowMode::Compare,
            quality_threshold: 0.05,
            max_slowdown_ratio: 2.0,
            min_overlap_for_equivalent: 0.95,
        }
    }
}

impl ShadowGateConfig {
    #[must_use]
    pub fn with_mode(mut self, mode: ShadowMode) -> Self {
        self.mode = mode;
        self
    }

    #[must_use]
    pub fn with_quality_threshold(mut self, threshold: f64) -> Self {
        self.quality_threshold = threshold;
        self
    }
}

/// Report from a shadow-run comparison.
#[derive(Clone, Debug)]
pub struct ShadowReport {
    /// Schema version.
    pub schema: &'static str,
    /// Unique ID for this report.
    pub id: String,
    /// Policy domain.
    pub domain: PolicyDomain,
    /// Incumbent policy name.
    pub incumbent_name: String,
    /// Candidate policy name.
    pub candidate_name: String,
    /// Comparison verdict.
    pub verdict: ShadowVerdict,
    /// Comparison metrics.
    pub metrics: ShadowMetrics,
    /// Mode used.
    pub mode: ShadowMode,
    /// Timestamp.
    pub timestamp: String,
    /// Optional explanation.
    pub explanation: Option<String>,
}

impl ShadowReport {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &str,
        domain: PolicyDomain,
        incumbent_name: &str,
        candidate_name: &str,
        verdict: ShadowVerdict,
        metrics: ShadowMetrics,
        mode: ShadowMode,
        timestamp: &str,
    ) -> Self {
        Self {
            schema: SHADOW_REPORT_SCHEMA_V1,
            id: id.to_string(),
            domain,
            incumbent_name: incumbent_name.to_string(),
            candidate_name: candidate_name.to_string(),
            verdict,
            metrics,
            mode,
            timestamp: timestamp.to_string(),
            explanation: None,
        }
    }

    pub fn with_explanation(mut self, explanation: &str) -> Self {
        self.explanation = Some(explanation.to_string());
        self
    }
}

/// Determine verdict from metrics using config thresholds.
#[must_use]
pub fn determine_verdict(metrics: &ShadowMetrics, config: &ShadowGateConfig) -> ShadowVerdict {
    let quality_delta = metrics.quality_delta();

    if metrics.overlap_ratio >= config.min_overlap_for_equivalent
        && quality_delta.abs() < config.quality_threshold
    {
        return ShadowVerdict::Equivalent;
    }

    let speedup_ratio = if metrics.candidate_time_us > 0 {
        metrics.incumbent_time_us as f64 / metrics.candidate_time_us as f64
    } else {
        1.0
    };

    let too_slow = speedup_ratio < (1.0 / config.max_slowdown_ratio);

    if quality_delta >= config.quality_threshold && !too_slow {
        ShadowVerdict::CandidateBetter
    } else if quality_delta <= -config.quality_threshold {
        ShadowVerdict::IncumbentBetter
    } else if metrics.overlap_ratio < 0.5 {
        ShadowVerdict::Divergent
    } else {
        ShadowVerdict::Inconclusive
    }
}

/// Build an explanation for the verdict.
#[must_use]
pub fn build_explanation(metrics: &ShadowMetrics, verdict: ShadowVerdict) -> String {
    let quality_delta = metrics.quality_delta();
    let time_delta = metrics.time_delta_us();

    match verdict {
        ShadowVerdict::Equivalent => format!(
            "Outputs are equivalent: {:.1}% overlap, quality delta {:.3}.",
            metrics.overlap_ratio * 100.0,
            quality_delta
        ),
        ShadowVerdict::CandidateBetter => format!(
            "Candidate outperforms incumbent: quality +{:.3}, time delta {}us.",
            quality_delta, time_delta
        ),
        ShadowVerdict::IncumbentBetter => format!(
            "Incumbent outperforms candidate: quality delta {:.3}.",
            quality_delta
        ),
        ShadowVerdict::Divergent => format!(
            "Outputs diverge significantly: only {:.1}% overlap.",
            metrics.overlap_ratio * 100.0
        ),
        ShadowVerdict::Inconclusive => {
            "Comparison inconclusive: insufficient data to determine winner.".to_string()
        }
    }
}

/// Gate for pack selection shadow runs.
pub mod pack {
    use super::*;
    use crate::pack::PackSelectionObjective;

    /// Input for pack selection shadow comparison.
    #[derive(Clone, Debug)]
    pub struct PackShadowInput {
        /// Candidate pool size.
        pub candidate_count: usize,
        /// Token budget.
        pub token_budget: u32,
        /// Incumbent objective.
        pub incumbent_objective: PackSelectionObjective,
        /// Candidate objective.
        pub candidate_objective: PackSelectionObjective,
    }

    /// Output from one pack selection run.
    #[derive(Clone, Debug)]
    pub struct PackShadowOutput {
        /// Memory IDs selected.
        pub selected_ids: Vec<String>,
        /// Total tokens used.
        pub tokens_used: u32,
        /// Quality score (aggregate relevance).
        pub quality_score: f64,
        /// Execution time in microseconds.
        pub time_us: u64,
    }

    /// Compare pack selection outputs.
    #[must_use]
    pub fn compare_outputs(
        incumbent: &PackShadowOutput,
        candidate: &PackShadowOutput,
        config: &ShadowGateConfig,
    ) -> (ShadowVerdict, ShadowMetrics) {
        let incumbent_set: std::collections::BTreeSet<_> = incumbent.selected_ids.iter().collect();
        let candidate_set: std::collections::BTreeSet<_> = candidate.selected_ids.iter().collect();

        let intersection = incumbent_set.intersection(&candidate_set).count();
        let union = incumbent_set.union(&candidate_set).count();
        let overlap_ratio = if union > 0 {
            intersection as f64 / union as f64
        } else {
            1.0
        };

        let metrics = ShadowMetrics {
            incumbent_size: incumbent.selected_ids.len() as u64,
            candidate_size: candidate.selected_ids.len() as u64,
            incumbent_quality: incumbent.quality_score,
            candidate_quality: candidate.quality_score,
            overlap_ratio,
            incumbent_time_us: incumbent.time_us,
            candidate_time_us: candidate.time_us,
        };

        let verdict = determine_verdict(&metrics, config);
        (verdict, metrics)
    }
}

/// Gate for curation filter shadow runs.
pub mod curation {
    use super::*;

    /// Output from one curation filter run.
    #[derive(Clone, Debug)]
    pub struct CurationShadowOutput {
        /// Candidate IDs accepted.
        pub accepted_ids: Vec<String>,
        /// Candidate IDs rejected.
        pub rejected_ids: Vec<String>,
        /// Total risk score.
        pub total_risk: f64,
        /// Execution time in microseconds.
        pub time_us: u64,
    }

    /// Compare curation filter outputs.
    #[must_use]
    pub fn compare_outputs(
        incumbent: &CurationShadowOutput,
        candidate: &CurationShadowOutput,
        config: &ShadowGateConfig,
    ) -> (ShadowVerdict, ShadowMetrics) {
        let incumbent_set: std::collections::BTreeSet<_> = incumbent.accepted_ids.iter().collect();
        let candidate_set: std::collections::BTreeSet<_> = candidate.accepted_ids.iter().collect();

        let intersection = incumbent_set.intersection(&candidate_set).count();
        let union = incumbent_set.union(&candidate_set).count();
        let overlap_ratio = if union > 0 {
            intersection as f64 / union as f64
        } else {
            1.0
        };

        let metrics = ShadowMetrics {
            incumbent_size: incumbent.accepted_ids.len() as u64,
            candidate_size: candidate.accepted_ids.len() as u64,
            incumbent_quality: 1.0 - incumbent.total_risk.min(1.0),
            candidate_quality: 1.0 - candidate.total_risk.min(1.0),
            overlap_ratio,
            incumbent_time_us: incumbent.time_us,
            candidate_time_us: candidate.time_us,
        };

        let verdict = determine_verdict(&metrics, config);
        (verdict, metrics)
    }
}

/// Gate for cache admission shadow runs.
pub mod cache {
    use super::*;
    use crate::cache::CacheStats;

    /// Output from one cache policy run.
    #[derive(Clone, Debug)]
    pub struct CacheShadowOutput {
        /// Cache stats after workload.
        pub stats: CacheStats,
        /// Final cache size.
        pub final_size: usize,
        /// Estimated memory used by the cache.
        pub memory_bytes: u64,
        /// Aggregate miss cost for the workload.
        pub miss_cost: u64,
        /// p95 latency for cache operations.
        pub p95_latency_us: u64,
        /// p99 latency for cache operations.
        pub p99_latency_us: u64,
        /// Execution time in microseconds.
        pub time_us: u64,
    }

    /// Compare cache policy outputs.
    #[must_use]
    pub fn compare_outputs(
        incumbent: &CacheShadowOutput,
        candidate: &CacheShadowOutput,
        config: &ShadowGateConfig,
    ) -> (ShadowVerdict, ShadowMetrics) {
        let incumbent_hit_rate = incumbent.stats.hit_rate();
        let candidate_hit_rate = candidate.stats.hit_rate();

        let metrics = ShadowMetrics {
            incumbent_size: incumbent.final_size as u64,
            candidate_size: candidate.final_size as u64,
            incumbent_quality: incumbent_hit_rate,
            candidate_quality: candidate_hit_rate,
            overlap_ratio: 1.0 - (incumbent_hit_rate - candidate_hit_rate).abs(),
            incumbent_time_us: incumbent.time_us,
            candidate_time_us: candidate.time_us,
        };

        let verdict = determine_verdict(&metrics, config);
        (verdict, metrics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_mode_strings_are_stable() {
        assert_eq!(ShadowMode::Compare.as_str(), "compare");
        assert_eq!(ShadowMode::CompareAndLog.as_str(), "compare_and_log");
        assert_eq!(ShadowMode::CompareLogAlert.as_str(), "compare_log_alert");
    }

    #[test]
    fn policy_domain_strings_are_stable() {
        assert_eq!(PolicyDomain::PackSelection.as_str(), "pack_selection");
        assert_eq!(PolicyDomain::CurationFilter.as_str(), "curation_filter");
        assert_eq!(PolicyDomain::CacheAdmission.as_str(), "cache_admission");
    }

    #[test]
    fn shadow_verdict_strings_are_stable() {
        assert_eq!(ShadowVerdict::Equivalent.as_str(), "equivalent");
        assert_eq!(ShadowVerdict::CandidateBetter.as_str(), "candidate_better");
        assert_eq!(ShadowVerdict::IncumbentBetter.as_str(), "incumbent_better");
        assert_eq!(ShadowVerdict::Divergent.as_str(), "divergent");
        assert_eq!(ShadowVerdict::Inconclusive.as_str(), "inconclusive");
    }

    #[test]
    fn verdict_safe_to_promote() {
        assert!(ShadowVerdict::Equivalent.is_safe_to_promote());
        assert!(ShadowVerdict::CandidateBetter.is_safe_to_promote());
        assert!(!ShadowVerdict::IncumbentBetter.is_safe_to_promote());
        assert!(!ShadowVerdict::Divergent.is_safe_to_promote());
        assert!(!ShadowVerdict::Inconclusive.is_safe_to_promote());
    }

    #[test]
    fn promotion_guards_block_unsafe_candidate() {
        let guards = ShadowPromotionGuards {
            dropped_critical_warnings: true,
            redaction_differences: false,
            p99_regression: true,
            tail_risk_regression: false,
            shadow_mismatch_above_tolerance: false,
        };

        assert!(guards.blocks_promotion());
        assert_eq!(
            guards.blocker_codes(),
            vec!["dropped_critical_warnings", "p99_regression"]
        );
        assert!(!candidate_promotion_allowed(
            ShadowVerdict::CandidateBetter,
            &guards
        ));
        assert!(candidate_promotion_allowed(
            ShadowVerdict::CandidateBetter,
            &ShadowPromotionGuards::default()
        ));
    }

    #[test]
    fn metrics_quality_delta() {
        let metrics = ShadowMetrics {
            incumbent_quality: 0.75,
            candidate_quality: 0.82,
            ..Default::default()
        };
        let delta = metrics.quality_delta();
        assert!((delta - 0.07).abs() < 0.001);
    }

    #[test]
    fn metrics_size_delta() {
        let metrics = ShadowMetrics {
            incumbent_size: 100,
            candidate_size: 85,
            ..Default::default()
        };
        assert_eq!(metrics.size_delta(), -15);
    }

    #[test]
    fn determine_verdict_equivalent() {
        let metrics = ShadowMetrics {
            overlap_ratio: 0.98,
            incumbent_quality: 0.80,
            candidate_quality: 0.82,
            ..Default::default()
        };
        let config = ShadowGateConfig::default();
        let verdict = determine_verdict(&metrics, &config);
        assert_eq!(verdict, ShadowVerdict::Equivalent);
    }

    #[test]
    fn determine_verdict_candidate_better() {
        let metrics = ShadowMetrics {
            overlap_ratio: 0.70,
            incumbent_quality: 0.75,
            candidate_quality: 0.90,
            incumbent_time_us: 100,
            candidate_time_us: 110,
            ..Default::default()
        };
        let config = ShadowGateConfig::default();
        let verdict = determine_verdict(&metrics, &config);
        assert_eq!(verdict, ShadowVerdict::CandidateBetter);
    }

    #[test]
    fn determine_verdict_incumbent_better() {
        let metrics = ShadowMetrics {
            overlap_ratio: 0.70,
            incumbent_quality: 0.90,
            candidate_quality: 0.75,
            ..Default::default()
        };
        let config = ShadowGateConfig::default();
        let verdict = determine_verdict(&metrics, &config);
        assert_eq!(verdict, ShadowVerdict::IncumbentBetter);
    }

    #[test]
    fn determine_verdict_divergent() {
        let metrics = ShadowMetrics {
            overlap_ratio: 0.30,
            incumbent_quality: 0.80,
            candidate_quality: 0.82,
            ..Default::default()
        };
        let config = ShadowGateConfig::default();
        let verdict = determine_verdict(&metrics, &config);
        assert_eq!(verdict, ShadowVerdict::Divergent);
    }

    #[test]
    fn shadow_report_creation() {
        let metrics = ShadowMetrics::default();
        let report = ShadowReport::new(
            "sr_test_001",
            PolicyDomain::PackSelection,
            "mmr_redundancy",
            "facility_location",
            ShadowVerdict::CandidateBetter,
            metrics,
            ShadowMode::Compare,
            "2026-05-01T12:00:00Z",
        )
        .with_explanation("Facility location improves diversity.");

        assert_eq!(report.id, "sr_test_001");
        assert_eq!(report.domain, PolicyDomain::PackSelection);
        assert_eq!(report.incumbent_name, "mmr_redundancy");
        assert_eq!(report.candidate_name, "facility_location");
        assert_eq!(report.verdict, ShadowVerdict::CandidateBetter);
        assert!(report.explanation.is_some());
    }

    #[test]
    fn build_explanation_for_verdicts() {
        let metrics = ShadowMetrics {
            overlap_ratio: 0.95,
            incumbent_quality: 0.80,
            candidate_quality: 0.82,
            candidate_time_us: 100,
            incumbent_time_us: 90,
            ..Default::default()
        };

        let expl = build_explanation(&metrics, ShadowVerdict::Equivalent);
        assert!(expl.contains("equivalent"));
        assert!(expl.contains("95.0%"));

        let expl2 = build_explanation(&metrics, ShadowVerdict::CandidateBetter);
        assert!(expl2.contains("outperforms"));
    }

    #[test]
    fn config_builder() {
        let config = ShadowGateConfig::default()
            .with_mode(ShadowMode::CompareLogAlert)
            .with_quality_threshold(0.10);

        assert_eq!(config.mode, ShadowMode::CompareLogAlert);
        assert!((config.quality_threshold - 0.10).abs() < 0.001);
    }

    mod pack_tests {
        use super::super::pack::*;
        use super::super::*;

        #[test]
        fn compare_pack_outputs_equivalent() {
            let incumbent = PackShadowOutput {
                selected_ids: vec!["m1".to_string(), "m2".to_string(), "m3".to_string()],
                tokens_used: 1000,
                quality_score: 0.85,
                time_us: 100,
            };
            let candidate = PackShadowOutput {
                selected_ids: vec!["m1".to_string(), "m2".to_string(), "m3".to_string()],
                tokens_used: 1000,
                quality_score: 0.86,
                time_us: 105,
            };
            let config = ShadowGateConfig::default();
            let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &config);

            assert_eq!(verdict, ShadowVerdict::Equivalent);
            assert!((metrics.overlap_ratio - 1.0).abs() < 0.001);
        }

        #[test]
        fn compare_pack_outputs_divergent() {
            let incumbent = PackShadowOutput {
                selected_ids: vec!["m1".to_string(), "m2".to_string()],
                tokens_used: 500,
                quality_score: 0.80,
                time_us: 100,
            };
            let candidate = PackShadowOutput {
                selected_ids: vec!["m3".to_string(), "m4".to_string()],
                tokens_used: 500,
                quality_score: 0.80,
                time_us: 100,
            };
            let config = ShadowGateConfig::default();
            let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &config);

            assert_eq!(verdict, ShadowVerdict::Divergent);
            assert!((metrics.overlap_ratio - 0.0).abs() < 0.001);
        }
    }

    mod curation_tests {
        use super::super::curation::*;
        use super::super::*;

        #[test]
        fn compare_curation_outputs_candidate_better() {
            let incumbent = CurationShadowOutput {
                accepted_ids: vec!["c1".to_string(), "c2".to_string()],
                rejected_ids: vec!["c3".to_string()],
                total_risk: 0.40,
                time_us: 50,
            };
            let candidate = CurationShadowOutput {
                accepted_ids: vec!["c1".to_string(), "c3".to_string()],
                rejected_ids: vec!["c2".to_string()],
                total_risk: 0.20,
                time_us: 55,
            };
            let config = ShadowGateConfig::default();
            let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &config);

            assert!(metrics.candidate_quality > metrics.incumbent_quality);
            assert!(matches!(
                verdict,
                ShadowVerdict::CandidateBetter | ShadowVerdict::Divergent
            ));
        }
    }

    mod cache_tests {
        use super::super::cache::*;
        use super::super::*;
        use crate::cache::CacheStats;

        #[test]
        fn compare_cache_outputs() {
            let incumbent = CacheShadowOutput {
                stats: CacheStats {
                    hits: 80,
                    misses: 20,
                    evictions: 10,
                    promotions: 5,
                },
                final_size: 50,
                memory_bytes: 4096,
                miss_cost: 20_000,
                p95_latency_us: 120,
                p99_latency_us: 200,
                time_us: 100,
            };
            let candidate = CacheShadowOutput {
                stats: CacheStats {
                    hits: 90,
                    misses: 10,
                    evictions: 8,
                    promotions: 7,
                },
                final_size: 50,
                memory_bytes: 3584,
                miss_cost: 10_000,
                p95_latency_us: 90,
                p99_latency_us: 150,
                time_us: 110,
            };
            let config = ShadowGateConfig::default();
            let (verdict, metrics) = compare_outputs(&incumbent, &candidate, &config);

            assert!(metrics.candidate_quality > metrics.incumbent_quality);
            assert_eq!(verdict, ShadowVerdict::CandidateBetter);
        }
    }
}
