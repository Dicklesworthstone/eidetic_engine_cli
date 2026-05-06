//! Deterministic ee-owned retrieval multipliers.
//!
//! Frankensearch owns candidate retrieval and fused base scores. This module
//! only applies the project-specific, explainable multipliers from the ee
//! retrieval contract: freshness, confidence, utility, maturity, harmful
//! feedback, scope, graph centrality, and redundancy.

/// Default recency time constant from the retrieval contract.
pub const DEFAULT_RECENCY_TAU_DAYS: f32 = 30.0;
/// Default confidence floor from the retrieval contract.
pub const DEFAULT_CONFIDENCE_FLOOR: f32 = 0.1;
/// Default lower bound for the utility multiplier.
pub const DEFAULT_UTILITY_FLOOR: f32 = 0.5;
/// Default harmful-feedback penalty per hit.
pub const DEFAULT_HARMFUL_PENALTY_PER_HIT: f32 = 0.1;
/// Default lower bound for the harmful-feedback multiplier.
pub const DEFAULT_HARMFUL_PENALTY_FLOOR: f32 = 0.2;
/// Default multiplier for exact workspace/scope matches.
pub const DEFAULT_SCOPE_MATCH_BONUS: f32 = 1.2;
/// Default graph-centrality weight. A centrality signal of 1.0 yields 1.10.
pub const DEFAULT_GRAPH_CENTRALITY_WEIGHT: f32 = 0.10;
/// Default MMR lambda used to dampen redundant candidates.
pub const DEFAULT_REDUNDANCY_LAMBDA: f32 = 0.7;

/// Scoring constants normally sourced from the `[scoring]` config block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchScoringConfig {
    pub recency_tau_days: f32,
    pub confidence_floor: f32,
    pub utility_floor: f32,
    pub harmful_penalty_per_hit: f32,
    pub harmful_penalty_floor: f32,
    pub scope_match_bonus: f32,
    pub graph_centrality_weight: f32,
    pub redundancy_lambda: f32,
}

impl Default for SearchScoringConfig {
    fn default() -> Self {
        Self {
            recency_tau_days: DEFAULT_RECENCY_TAU_DAYS,
            confidence_floor: DEFAULT_CONFIDENCE_FLOOR,
            utility_floor: DEFAULT_UTILITY_FLOOR,
            harmful_penalty_per_hit: DEFAULT_HARMFUL_PENALTY_PER_HIT,
            harmful_penalty_floor: DEFAULT_HARMFUL_PENALTY_FLOOR,
            scope_match_bonus: DEFAULT_SCOPE_MATCH_BONUS,
            graph_centrality_weight: DEFAULT_GRAPH_CENTRALITY_WEIGHT,
            redundancy_lambda: DEFAULT_REDUNDANCY_LAMBDA,
        }
    }
}

/// Maturity class used by retrieval scoring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetrievalMaturity {
    Working,
    Episodic,
    Semantic,
    ProceduralCandidate,
    ProceduralEstablished,
    ProceduralProven,
    ProceduralDeprecated,
    ProceduralRetired,
}

impl RetrievalMaturity {
    #[must_use]
    pub const fn multiplier(self) -> f32 {
        match self {
            Self::Working | Self::Episodic | Self::Semantic | Self::ProceduralEstablished => 1.0,
            Self::ProceduralCandidate => 0.5,
            Self::ProceduralProven => 1.5,
            Self::ProceduralDeprecated | Self::ProceduralRetired => 0.0,
        }
    }
}

/// Speed mode for retrieval (latency vs quality tradeoff).
///
/// Maps to TwoTier budget configuration without exposing embedding model names.
/// Model selection is owned by Frankensearch (ADR-0016).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SpeedMode {
    /// Lexical only. No embedding computation. Fastest, lowest quality.
    Instant,
    /// Hybrid retrieval with reasonable latency. Balanced tradeoff.
    #[default]
    Default,
    /// Full semantic retrieval. Highest quality, slowest.
    Quality,
}

impl SpeedMode {
    /// Stable string form for config and JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Instant => "instant",
            Self::Default => "default",
            Self::Quality => "quality",
        }
    }

    /// All speed mode variants for iteration.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Instant, Self::Default, Self::Quality]
    }

    /// Whether this mode uses embedding-based retrieval.
    #[must_use]
    pub const fn uses_embeddings(self) -> bool {
        !matches!(self, Self::Instant)
    }

    /// Suggested candidate limit for this speed mode.
    ///
    /// Lower limits for faster modes, higher for quality modes.
    #[must_use]
    pub const fn candidate_limit(self) -> usize {
        match self {
            Self::Instant => 50,
            Self::Default => 100,
            Self::Quality => 200,
        }
    }

    /// Suggested rerank depth for MMR/diversity.
    ///
    /// Quality mode does deeper reranking for better diversity.
    #[must_use]
    pub const fn rerank_depth(self) -> usize {
        match self {
            Self::Instant => 10,
            Self::Default => 25,
            Self::Quality => 50,
        }
    }
}

impl std::fmt::Display for SpeedMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SpeedMode {
    type Err = ParseSpeedModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "instant" => Ok(Self::Instant),
            "default" => Ok(Self::Default),
            "quality" => Ok(Self::Quality),
            _ => Err(ParseSpeedModeError {
                input: s.to_owned(),
            }),
        }
    }
}

/// Error when parsing an invalid speed mode string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseSpeedModeError {
    input: String,
}

impl ParseSpeedModeError {
    /// The input string that failed to parse.
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl std::fmt::Display for ParseSpeedModeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown speed mode `{}`; expected one of instant, default, quality",
            self.input
        )
    }
}

impl std::error::Error for ParseSpeedModeError {}

/// Signals supplied by the retrieval pipeline for one candidate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchScoringSignals {
    pub base_score: f32,
    pub age_days: Option<f32>,
    pub confidence: f32,
    pub utility_score: f32,
    pub maturity: RetrievalMaturity,
    pub harmful_count: u32,
    pub scope_match: bool,
    pub graph_centrality: Option<f32>,
    pub redundancy: Option<f32>,
}

impl SearchScoringSignals {
    #[must_use]
    pub const fn new(base_score: f32, maturity: RetrievalMaturity) -> Self {
        Self {
            base_score,
            age_days: None,
            confidence: 1.0,
            utility_score: 1.0,
            maturity,
            harmful_count: 0,
            scope_match: false,
            graph_centrality: None,
            redundancy: None,
        }
    }
}

/// Component expansion for one final retrieval score.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchScoreComponents {
    pub base: f32,
    pub recency: f32,
    pub confidence: f32,
    pub utility: f32,
    pub maturity: f32,
    pub harmful_penalty: f32,
    pub scope_match: f32,
    pub graph_centrality: f32,
    pub redundancy: f32,
    pub final_score: f32,
}

impl SearchScoreComponents {
    #[must_use]
    pub fn from_signals(
        signals: SearchScoringSignals,
        config: SearchScoringConfig,
    ) -> SearchScoreComponents {
        let base = finite_nonnegative(signals.base_score);
        let recency = recency_multiplier(signals.age_days, config.recency_tau_days);
        let confidence = finite_unit(signals.confidence).max(config.confidence_floor);
        let utility = lerp(
            config.utility_floor,
            1.0,
            finite_unit(signals.utility_score),
        );
        let maturity = signals.maturity.multiplier();
        let harmful_penalty = harmful_penalty(
            signals.harmful_count,
            config.harmful_penalty_per_hit,
            config.harmful_penalty_floor,
        );
        let scope_match = if signals.scope_match {
            config.scope_match_bonus.max(0.0)
        } else {
            1.0
        };
        let graph_centrality = 1.0
            + finite_unit(signals.graph_centrality.unwrap_or(0.0))
                * config.graph_centrality_weight.max(0.0);
        let redundancy = redundancy_multiplier(signals.redundancy, config.redundancy_lambda);
        let final_score = base
            * recency
            * confidence
            * utility
            * maturity
            * harmful_penalty
            * scope_match
            * graph_centrality
            * redundancy;

        SearchScoreComponents {
            base,
            recency,
            confidence,
            utility,
            maturity,
            harmful_penalty,
            scope_match,
            graph_centrality,
            redundancy,
            final_score,
        }
    }
}

/// Apply ee retrieval multipliers to one Frankensearch base score.
#[must_use]
pub fn final_score(signals: SearchScoringSignals, config: SearchScoringConfig) -> f32 {
    SearchScoreComponents::from_signals(signals, config).final_score
}

fn recency_multiplier(age_days: Option<f32>, tau_days: f32) -> f32 {
    let Some(age_days) = age_days else {
        return 1.0;
    };
    let tau = finite_positive(tau_days).unwrap_or(DEFAULT_RECENCY_TAU_DAYS);
    (-finite_nonnegative(age_days) / tau).exp()
}

fn harmful_penalty(harmful_count: u32, per_hit: f32, floor: f32) -> f32 {
    let effective_count = f32::from(u16::try_from(harmful_count).unwrap_or(u16::MAX));
    let penalty = 1.0 - finite_nonnegative(per_hit) * effective_count;
    penalty.max(finite_nonnegative(floor)).min(1.0)
}

fn redundancy_multiplier(redundancy: Option<f32>, lambda: f32) -> f32 {
    let lambda = finite_unit(lambda);
    1.0 - (1.0 - lambda) * finite_unit(redundancy.unwrap_or(0.0))
}

fn lerp(start: f32, end: f32, amount: f32) -> f32 {
    finite_nonnegative(start) + (end - finite_nonnegative(start)) * amount
}

fn finite_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn finite_positive(value: f32) -> Option<f32> {
    if value.is_finite() && value > 0.0 {
        Some(value)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_GRAPH_CENTRALITY_WEIGHT, DEFAULT_RECENCY_TAU_DAYS, RetrievalMaturity,
        SearchScoreComponents, SearchScoringConfig, SearchScoringSignals, SpeedMode, final_score,
    };

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 0.000_01,
            "expected {actual} to be close to {expected}"
        );
    }

    #[test]
    fn recency_multiplier_matches_zero_one_two_and_ten_tau_boundaries() {
        let config = SearchScoringConfig::default();
        let base = SearchScoringSignals::new(1.0, RetrievalMaturity::Semantic);

        let at_zero = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                age_days: Some(0.0),
                ..base
            },
            config,
        );
        let at_one_tau = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                age_days: Some(DEFAULT_RECENCY_TAU_DAYS),
                ..base
            },
            config,
        );
        let at_two_tau = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                age_days: Some(DEFAULT_RECENCY_TAU_DAYS * 2.0),
                ..base
            },
            config,
        );
        let at_ten_tau = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                age_days: Some(DEFAULT_RECENCY_TAU_DAYS * 10.0),
                ..base
            },
            config,
        );

        assert_close(at_zero.recency, 1.0);
        assert_close(at_one_tau.recency, std::f32::consts::E.recip());
        assert_close(at_two_tau.recency, (-2.0_f32).exp());
        assert_close(at_ten_tau.recency, (-10.0_f32).exp());
    }

    #[test]
    fn harmful_penalty_uses_per_hit_penalty_with_floor() {
        let config = SearchScoringConfig::default();
        let base = SearchScoringSignals::new(1.0, RetrievalMaturity::Semantic);

        let no_hits = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                harmful_count: 0,
                ..base
            },
            config,
        );
        let six_hits = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                harmful_count: 6,
                ..base
            },
            config,
        );
        let many_hits = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                harmful_count: 100,
                ..base
            },
            config,
        );

        assert_close(no_hits.harmful_penalty, 1.0);
        assert_close(six_hits.harmful_penalty, 0.4);
        assert_close(many_hits.harmful_penalty, 0.2);
    }

    #[test]
    fn maturity_multiplier_covers_plan_boundary_classes() {
        assert_close(RetrievalMaturity::Working.multiplier(), 1.0);
        assert_close(RetrievalMaturity::Episodic.multiplier(), 1.0);
        assert_close(RetrievalMaturity::Semantic.multiplier(), 1.0);
        assert_close(RetrievalMaturity::ProceduralCandidate.multiplier(), 0.5);
        assert_close(RetrievalMaturity::ProceduralEstablished.multiplier(), 1.0);
        assert_close(RetrievalMaturity::ProceduralProven.multiplier(), 1.5);
        assert_close(RetrievalMaturity::ProceduralDeprecated.multiplier(), 0.0);
        assert_close(RetrievalMaturity::ProceduralRetired.multiplier(), 0.0);
    }

    #[test]
    fn final_score_expands_all_components_deterministically() {
        let config = SearchScoringConfig::default();
        let signals = SearchScoringSignals {
            base_score: 2.0,
            age_days: Some(0.0),
            confidence: 0.8,
            utility_score: 0.6,
            maturity: RetrievalMaturity::ProceduralProven,
            harmful_count: 2,
            scope_match: true,
            graph_centrality: Some(0.5),
            redundancy: Some(0.25),
        };

        let components = SearchScoreComponents::from_signals(signals, config);
        assert_close(components.base, 2.0);
        assert_close(components.recency, 1.0);
        assert_close(components.confidence, 0.8);
        assert_close(components.utility, 0.8);
        assert_close(components.maturity, 1.5);
        assert_close(components.harmful_penalty, 0.8);
        assert_close(components.scope_match, 1.2);
        assert_close(
            components.graph_centrality,
            1.0 + DEFAULT_GRAPH_CENTRALITY_WEIGHT * 0.5,
        );
        assert_close(components.redundancy, 0.925);
        assert_close(components.final_score, final_score(signals, config));
    }

    #[test]
    fn invalid_or_out_of_range_inputs_are_clamped() {
        let config = SearchScoringConfig {
            recency_tau_days: -1.0,
            confidence_floor: 0.1,
            utility_floor: 0.5,
            harmful_penalty_per_hit: f32::NAN,
            harmful_penalty_floor: 0.2,
            scope_match_bonus: -3.0,
            graph_centrality_weight: f32::NAN,
            redundancy_lambda: 2.0,
        };
        let components = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                base_score: f32::NAN,
                age_days: Some(-5.0),
                confidence: -0.4,
                utility_score: 8.0,
                maturity: RetrievalMaturity::Semantic,
                harmful_count: 5,
                scope_match: true,
                graph_centrality: Some(7.0),
                redundancy: Some(9.0),
            },
            config,
        );

        assert_close(components.base, 0.0);
        assert_close(components.recency, 1.0);
        assert_close(components.confidence, 0.1);
        assert_close(components.utility, 1.0);
        assert_close(components.harmful_penalty, 1.0);
        assert_close(components.scope_match, 0.0);
        assert_close(components.graph_centrality, 1.0);
        assert_close(components.redundancy, 1.0);
        assert_close(components.final_score, 0.0);
    }

    #[test]
    fn speed_mode_strings() {
        assert_eq!(SpeedMode::Instant.as_str(), "instant");
        assert_eq!(SpeedMode::Default.as_str(), "default");
        assert_eq!(SpeedMode::Quality.as_str(), "quality");
    }

    #[test]
    fn speed_mode_parse() {
        assert_eq!("instant".parse::<SpeedMode>().unwrap(), SpeedMode::Instant);
        assert_eq!("default".parse::<SpeedMode>().unwrap(), SpeedMode::Default);
        assert_eq!("quality".parse::<SpeedMode>().unwrap(), SpeedMode::Quality);
        assert!("fast".parse::<SpeedMode>().is_err());
    }

    #[test]
    fn speed_mode_properties() {
        assert!(!SpeedMode::Instant.uses_embeddings());
        assert!(SpeedMode::Default.uses_embeddings());
        assert!(SpeedMode::Quality.uses_embeddings());

        assert!(SpeedMode::Instant.candidate_limit() < SpeedMode::Default.candidate_limit());
        assert!(SpeedMode::Default.candidate_limit() < SpeedMode::Quality.candidate_limit());

        assert!(SpeedMode::Instant.rerank_depth() < SpeedMode::Default.rerank_depth());
        assert!(SpeedMode::Default.rerank_depth() < SpeedMode::Quality.rerank_depth());
    }

    #[test]
    fn speed_mode_default() {
        assert_eq!(SpeedMode::default(), SpeedMode::Default);
    }
}
