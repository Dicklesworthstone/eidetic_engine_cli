//! Deterministic ee-owned retrieval multipliers.
//!
//! Frankensearch owns candidate retrieval and fused base scores. This module
//! only applies the project-specific, explainable multipliers from the ee
//! retrieval contract: freshness, confidence, utility, maturity, harmful
//! feedback, scope, graph centrality, redundancy, opt-in anchor matches, and
//! opt-in bead affinity.

use std::collections::BTreeSet;

use crate::models::MemoryAnchorFreshnessState;

/// Default recency time constant from the retrieval contract.
pub const DEFAULT_RECENCY_TAU_DAYS: f32 = 30.0;
/// Lower bound the code-coupled freshness drift multiplier is clamped to
/// (ADR 0056, bd-1n0np.3.7). A `floor` of `1.0` means "no penalty"; a smaller
/// floor lets a drifted anchor rank DOWN but never vanish. Use
/// [`stale_anchor_floor`] to derive this from the configurable
/// `stale_anchor_penalty`.
pub const DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR: f32 = 0.4;
/// Default `stale_anchor_penalty` for the retrieval contract (bd-2vq2z.1,
/// Phase-6 pass 2 — supersedes ADR 0056 part B). `0.0` means **FLAG, DON'T
/// PENALIZE**: a memory whose code anchor drifted is surfaced via its freshness
/// flag but keeps its rank, because a memory about code that just changed is
/// often exactly what the agent wants when touching that code again. Operators
/// may opt into a small tie-breaker via `[retrieval] stale_anchor_penalty`; even
/// then a drifted memory never vanishes because the multiplier is bounded by
/// [`DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR`].
pub const DEFAULT_STALE_ANCHOR_PENALTY: f32 = 0.0;
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
/// Default hard cap for bead-aware additive retrieval bias.
pub const DEFAULT_BEAD_AFFINITY_BIAS_CAP: f32 = 0.05;
/// Default hard cap for exact memory-anchor additive retrieval bias.
pub const DEFAULT_ANCHOR_MATCH_BIAS_CAP: f32 = 0.08;

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
    pub anchor_match_bias_cap: f32,
    pub bead_affinity_bias_cap: f32,
    /// Opt-in rank reduction for a memory whose code anchor drifted
    /// (bd-2vq2z.1). `0.0` (the default) means FLAG, DON'T PENALIZE: a drifted
    /// anchor keeps its rank and is surfaced only via its freshness flag. A
    /// value `p` in `(0.0, 1.0]` lets a `Stale` anchor fall at most to
    /// `max(1.0 - p, DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR)`, never vanishing.
    pub stale_anchor_penalty: f32,
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
            anchor_match_bias_cap: DEFAULT_ANCHOR_MATCH_BIAS_CAP,
            bead_affinity_bias_cap: DEFAULT_BEAD_AFFINITY_BIAS_CAP,
            stale_anchor_penalty: DEFAULT_STALE_ANCHOR_PENALTY,
        }
    }
}

/// Redaction-safe anchor query context for deterministic exact-match boosts.
///
/// Entries are `(anchor_kind, anchor_value_hash)`. Callers must pass hashes or
/// redacted-safe values only; raw paths, symbols, commands, or schema values do
/// not belong in this scoring primitive.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnchorMatchContext {
    pub anchors: BTreeSet<(String, String)>,
}

impl AnchorMatchContext {
    #[must_use]
    pub fn new<K, H, I>(anchors: I) -> Self
    where
        K: Into<String>,
        H: Into<String>,
        I: IntoIterator<Item = (K, H)>,
    {
        Self {
            anchors: normalize_anchor_pairs(anchors),
        }
    }

    #[must_use]
    pub fn is_cold_start(&self) -> bool {
        self.anchors.is_empty()
    }
}

/// Candidate-side redaction-safe anchor signals.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnchorMatchCandidateSignals {
    pub anchors: BTreeSet<(String, String)>,
}

impl AnchorMatchCandidateSignals {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_anchors<K, H, I>(mut self, anchors: I) -> Self
    where
        K: Into<String>,
        H: Into<String>,
        I: IntoIterator<Item = (K, H)>,
    {
        self.anchors = normalize_anchor_pairs(anchors);
        self
    }
}

/// Explanation for exact-anchor additive score.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnchorMatchScore {
    pub value: f32,
    pub exact_matches: usize,
    pub capped: bool,
}

impl AnchorMatchScore {
    #[must_use]
    pub fn applied(self) -> bool {
        self.value > 0.0
    }
}

/// Redacted, local bead context used to bias retrieval without reading raw
/// tracker internals at every candidate comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BeadAffinityContext {
    pub bead_id: String,
    pub labels: BTreeSet<String>,
    pub tokens: BTreeSet<String>,
}

impl BeadAffinityContext {
    #[must_use]
    pub fn new(
        bead_id: impl Into<String>,
        labels: impl IntoIterator<Item = impl Into<String>>,
        text: &str,
    ) -> Self {
        Self {
            bead_id: bead_id.into(),
            labels: normalize_label_set(labels),
            tokens: bead_affinity_tokens(text),
        }
    }

    #[must_use]
    pub fn is_cold_start(&self) -> bool {
        self.bead_id.trim().is_empty() && self.labels.is_empty() && self.tokens.is_empty()
    }
}

/// Candidate-side signals used by the bead-affinity soft prior.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BeadAffinityCandidateSignals {
    pub tags: BTreeSet<String>,
    pub content_tokens: BTreeSet<String>,
    pub content_hash: Option<String>,
    pub link_refs: BTreeSet<String>,
}

impl BeadAffinityCandidateSignals {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = normalize_label_set(tags);
        self
    }

    #[must_use]
    pub fn with_content(mut self, content: &str) -> Self {
        self.content_tokens = bead_affinity_tokens(content);
        self
    }

    #[must_use]
    pub fn with_content_hash(mut self, content_hash: Option<impl Into<String>>) -> Self {
        self.content_hash = content_hash.map(Into::into);
        self
    }

    #[must_use]
    pub fn with_link_refs(mut self, refs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.link_refs = refs.into_iter().map(Into::into).collect();
        self
    }
}

/// Explanation for the additive bead-affinity score.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BeadAffinityScore {
    pub value: f32,
    pub tag_overlap: usize,
    pub content_token_overlap: usize,
    pub content_hash_overlap: bool,
    pub link_overlap: usize,
    pub capped: bool,
}

impl BeadAffinityScore {
    #[must_use]
    pub fn applied(self) -> bool {
        self.value > 0.0
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
        match s.trim().to_ascii_lowercase().as_str() {
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
    pub anchor_match: Option<f32>,
    pub bead_affinity: Option<f32>,
    /// Code-coupled freshness state of the candidate's anchored symbol, if any
    /// (bd-1n0np.3.7). `None` means unanchored or freshness unknown — a neutral
    /// `1.0` multiplier, so existing callers are unaffected.
    pub freshness_drift: Option<MemoryAnchorFreshnessState>,
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
            anchor_match: None,
            bead_affinity: None,
            freshness_drift: None,
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
    pub anchor_match: f32,
    pub bead_affinity: f32,
    pub freshness_drift: f32,
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
        let confidence = finite_unit(signals.confidence).max(finite_unit(config.confidence_floor));
        let utility = lerp(
            finite_unit(config.utility_floor),
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
            finite_nonnegative(config.scope_match_bonus)
        } else {
            1.0
        };
        let graph_centrality = 1.0
            + finite_unit(signals.graph_centrality.unwrap_or(0.0))
                * finite_nonnegative(config.graph_centrality_weight);
        let redundancy = redundancy_multiplier(signals.redundancy, config.redundancy_lambda);
        // Code-coupled freshness drift signal (bd-2vq2z.1, Phase-6 pass 2 —
        // supersedes ADR 0056 part B). FLAG, DON'T PENALIZE: the default
        // `stale_anchor_penalty` of 0.0 yields a neutral 1.0 multiplier, so a
        // drifted anchored memory keeps its rank (the drift is surfaced via the
        // freshness flag, not suppressed). An operator may opt into a small
        // tie-breaker via `[retrieval] stale_anchor_penalty`. `None` (unanchored
        // or unknown freshness) is neutral (1.0).
        let freshness_drift = signals.freshness_drift.map_or(1.0, |state| {
            freshness_drift_multiplier(state, stale_anchor_floor(config.stale_anchor_penalty))
        });
        let multiplicative_score = base
            * recency
            * confidence
            * utility
            * maturity
            * harmful_penalty
            * scope_match
            * graph_centrality
            * redundancy
            * freshness_drift;
        let anchor_match_cap = finite_nonnegative(config.anchor_match_bias_cap.abs());
        let anchor_match = finite_signed(signals.anchor_match.unwrap_or(0.0))
            .clamp(-anchor_match_cap, anchor_match_cap);
        let bead_affinity_cap = finite_nonnegative(config.bead_affinity_bias_cap.abs());
        let bead_affinity = finite_signed(signals.bead_affinity.unwrap_or(0.0))
            .clamp(-bead_affinity_cap, bead_affinity_cap);
        let final_score = finite_nonnegative(multiplicative_score + anchor_match + bead_affinity);

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
            anchor_match,
            bead_affinity,
            freshness_drift,
            final_score,
        }
    }
}

/// Score exact anchor overlap as a deterministic additive boost capped to the
/// configured magnitude. This preserves Frankensearch as candidate/ranking
/// owner while letting ee nudge already-retrieved candidates with matching
/// hash-only anchors.
#[must_use]
pub fn anchor_match_score(
    context: &AnchorMatchContext,
    candidate: &AnchorMatchCandidateSignals,
    max_abs_bias: f32,
) -> AnchorMatchScore {
    if context.is_cold_start() || candidate.anchors.is_empty() {
        return AnchorMatchScore::default();
    }

    let exact_matches = context.anchors.intersection(&candidate.anchors).count();
    let raw = exact_matches as f32 * 0.04;
    let cap = finite_nonnegative(max_abs_bias).min(DEFAULT_ANCHOR_MATCH_BIAS_CAP);
    let value = raw.min(cap);
    AnchorMatchScore {
        value,
        exact_matches,
        capped: raw > cap,
    }
}

/// Score bead affinity as an additive soft prior capped to the configured
/// magnitude. This function is deterministic and side-effect free; callers
/// decide whether to attach the returned value to [`SearchScoringSignals`].
#[must_use]
pub fn bead_affinity_score(
    context: &BeadAffinityContext,
    candidate: &BeadAffinityCandidateSignals,
    max_abs_bias: f32,
) -> BeadAffinityScore {
    if context.is_cold_start() {
        return BeadAffinityScore::default();
    }

    let tag_overlap = intersection_count(&context.labels, &candidate.tags);
    let content_token_overlap = intersection_count(&context.tokens, &candidate.content_tokens);
    let content_hash_overlap = candidate.content_hash.as_deref().is_some_and(|hash| {
        let digest = hash.strip_prefix("blake3:").unwrap_or(hash);
        context
            .tokens
            .iter()
            .any(|token| token.len() >= 8 && digest.contains(token))
    });
    let link_overlap = candidate
        .link_refs
        .iter()
        .filter(|link| link_ref_mentions_bead(link, &context.bead_id))
        .count();

    let raw = tag_overlap as f32 * 0.025
        + content_token_overlap as f32 * 0.015
        + if content_hash_overlap { 0.01 } else { 0.0 }
        + link_overlap as f32 * 0.01;
    let cap = finite_nonnegative(max_abs_bias).min(DEFAULT_BEAD_AFFINITY_BIAS_CAP);
    let value = raw.min(cap);
    BeadAffinityScore {
        value,
        tag_overlap,
        content_token_overlap,
        content_hash_overlap,
        link_overlap,
        capped: raw > cap,
    }
}

/// Apply ee retrieval multipliers to one Frankensearch base score.
#[must_use]
pub fn final_score(signals: SearchScoringSignals, config: SearchScoringConfig) -> f32 {
    SearchScoreComponents::from_signals(signals, config).final_score
}

/// Rank-down multiplier for an anchored memory's code-coupled freshness state
/// (ADR 0056, bd-1n0np.3.7). Multiply this into the recency/freshness term so a
/// memory whose anchored symbol drifted falls in rank without disappearing.
///
/// - `Current` (or unanchored): neutral `1.0`.
/// - `Suspect` (ambiguous drift, e.g. an unresolved rename): a partial penalty
///   halfway between `floor` and `1.0`.
/// - `Stale` (resolved content change or disappearance): the full penalty down
///   to `floor`.
///
/// The result is clamped to `[floor, 1.0]` (with `floor` itself clamped to the
/// unit interval), so a drifted memory ranks down but never vanishes. Mirrors
/// the standalone, deterministic shape of [`anchor_match_score`] /
/// [`bead_affinity_score`]; the live ranking path multiplies it in once the
/// scoring pipeline is wired into retrieval.
/// Convert a configurable `stale_anchor_penalty` (the opt-in rank reduction for
/// a drifted code anchor; clamped to `0.0..=1.0`) into the `floor` consumed by
/// [`freshness_drift_multiplier`].
///
/// bd-2vq2z.1 (Phase-6 pass 2) makes anchor drift a FLAG, not a rank penalty:
/// the default penalty of `0.0` maps to a floor of `1.0`, so a `Stale` anchor
/// keeps a neutral `1.0` multiplier (no suppression). A penalty `p` lets a
/// `Stale` anchor fall at most to
/// `max(1.0 - p, DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR)` (a small tie-breaker),
/// and the multiplier never reaches zero, so a drifted memory never vanishes.
#[must_use]
pub fn stale_anchor_floor(stale_anchor_penalty: f32) -> f32 {
    (1.0 - finite_unit(stale_anchor_penalty)).clamp(DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR, 1.0)
}

pub fn freshness_drift_multiplier(state: MemoryAnchorFreshnessState, floor: f32) -> f32 {
    let floor = finite_unit(floor);
    match state {
        MemoryAnchorFreshnessState::Current => 1.0,
        MemoryAnchorFreshnessState::Suspect => ((1.0 + floor) / 2.0).clamp(floor, 1.0),
        MemoryAnchorFreshnessState::Stale => floor,
    }
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
    let lambda = finite_unit_or(lambda, 1.0);
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

fn finite_unit_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        fallback.clamp(0.0, 1.0)
    }
}

fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn finite_signed(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn finite_positive(value: f32) -> Option<f32> {
    if value.is_finite() && value > 0.0 {
        Some(value)
    } else {
        None
    }
}

fn bead_affinity_tokens(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn normalize_label_set(values: impl IntoIterator<Item = impl Into<String>>) -> BTreeSet<String> {
    values
        .into_iter()
        .map(Into::into)
        .flat_map(|value| {
            value
                .split(|ch: char| !ch.is_ascii_alphanumeric())
                .filter(|token| token.len() >= 2)
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn normalize_anchor_pairs<K, H, I>(anchors: I) -> BTreeSet<(String, String)>
where
    K: Into<String>,
    H: Into<String>,
    I: IntoIterator<Item = (K, H)>,
{
    anchors
        .into_iter()
        .filter_map(|(kind, hash)| {
            let kind = kind.into().trim().to_ascii_lowercase();
            let hash = hash.into().trim().to_ascii_lowercase();
            (!kind.is_empty() && !hash.is_empty()).then_some((kind, hash))
        })
        .collect()
}

fn intersection_count(left: &BTreeSet<String>, right: &BTreeSet<String>) -> usize {
    left.intersection(right).count()
}

fn link_ref_mentions_bead(link_ref: &str, bead_id: &str) -> bool {
    let bead_id = bead_id.trim();
    if bead_id.is_empty() {
        return false;
    }

    let mut search_from = 0;
    while let Some(offset) = link_ref[search_from..].find(bead_id) {
        let start = search_from + offset;
        let end = start + bead_id.len();
        let before_is_boundary = link_ref[..start]
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric());
        let after_is_boundary = link_ref[end..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric());

        if before_is_boundary && after_is_boundary {
            return true;
        }
        search_from = end;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::{
        AnchorMatchCandidateSignals, AnchorMatchContext, BeadAffinityCandidateSignals,
        BeadAffinityContext, DEFAULT_ANCHOR_MATCH_BIAS_CAP, DEFAULT_BEAD_AFFINITY_BIAS_CAP,
        DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR, DEFAULT_GRAPH_CENTRALITY_WEIGHT,
        DEFAULT_RECENCY_TAU_DAYS, DEFAULT_STALE_ANCHOR_PENALTY, RetrievalMaturity,
        SearchScoreComponents, SearchScoringConfig, SearchScoringSignals, SpeedMode,
        anchor_match_score, bead_affinity_score, final_score, freshness_drift_multiplier,
        stale_anchor_floor,
    };
    use crate::models::MemoryAnchorFreshnessState;

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
            anchor_match: Some(0.04),
            bead_affinity: Some(0.03),
            freshness_drift: None,
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
        assert_close(components.anchor_match, 0.04);
        assert_close(components.bead_affinity, 0.03);
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
            anchor_match_bias_cap: DEFAULT_ANCHOR_MATCH_BIAS_CAP,
            bead_affinity_bias_cap: DEFAULT_BEAD_AFFINITY_BIAS_CAP,
            stale_anchor_penalty: DEFAULT_STALE_ANCHOR_PENALTY,
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
                anchor_match: Some(f32::NAN),
                bead_affinity: Some(f32::NAN),
                freshness_drift: None,
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
        assert_close(components.anchor_match, 0.0);
        assert_close(components.final_score, 0.0);
    }

    #[test]
    fn finite_utility_floor_above_one_is_clamped_to_one() {
        let config = SearchScoringConfig {
            utility_floor: 2.0,
            ..SearchScoringConfig::default()
        };
        let components = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                base_score: 1.0,
                utility_score: 0.0,
                maturity: RetrievalMaturity::Semantic,
                ..SearchScoringSignals::new(1.0, RetrievalMaturity::Semantic)
            },
            config,
        );

        assert_close(components.utility, 1.0);
        assert_close(components.final_score, 1.0);
    }

    #[test]
    fn non_finite_config_values_fail_closed_without_panicking() {
        let config = SearchScoringConfig {
            recency_tau_days: f32::NAN,
            confidence_floor: f32::INFINITY,
            utility_floor: f32::INFINITY,
            harmful_penalty_per_hit: f32::NEG_INFINITY,
            harmful_penalty_floor: f32::INFINITY,
            scope_match_bonus: f32::INFINITY,
            graph_centrality_weight: f32::INFINITY,
            redundancy_lambda: f32::NEG_INFINITY,
            anchor_match_bias_cap: f32::NAN,
            bead_affinity_bias_cap: f32::NAN,
            stale_anchor_penalty: DEFAULT_STALE_ANCHOR_PENALTY,
        };
        let components = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                base_score: 1.0,
                age_days: Some(10.0),
                confidence: 1.0,
                utility_score: 1.0,
                maturity: RetrievalMaturity::Semantic,
                harmful_count: 1,
                scope_match: true,
                graph_centrality: Some(1.0),
                redundancy: Some(1.0),
                anchor_match: Some(DEFAULT_ANCHOR_MATCH_BIAS_CAP),
                bead_affinity: Some(DEFAULT_BEAD_AFFINITY_BIAS_CAP),
                freshness_drift: None,
            },
            config,
        );

        assert_close(components.confidence, 1.0);
        assert_close(components.utility, 1.0);
        assert_close(components.harmful_penalty, 1.0);
        assert_close(components.scope_match, 0.0);
        assert_close(components.graph_centrality, 1.0);
        assert_close(components.redundancy, 1.0);
        assert_close(components.anchor_match, 0.0);
        assert_close(components.bead_affinity, 0.0);
        assert!(
            components.final_score.is_finite(),
            "final score must stay finite for malformed scoring config"
        );
        assert_close(components.final_score, 0.0);
    }

    #[test]
    fn anchor_match_scores_exact_kind_hash_overlap_under_cap() {
        let context =
            AnchorMatchContext::new([("path", "blake3:aaaabbbb"), ("schema", "blake3:ccccdddd")]);
        let candidate = AnchorMatchCandidateSignals::new()
            .with_anchors([("schema", "BLAKE3:CCCCDDDD"), ("path", "blake3:eeeeffff")]);

        let score = anchor_match_score(&context, &candidate, DEFAULT_ANCHOR_MATCH_BIAS_CAP);

        assert!(score.applied());
        assert_eq!(score.exact_matches, 1);
        assert_close(score.value, 0.04);
        assert!(!score.capped);
    }

    #[test]
    fn anchor_match_cold_start_and_non_matches_are_zero() {
        let cold = AnchorMatchContext::default();
        let candidate =
            AnchorMatchCandidateSignals::new().with_anchors([("path", "blake3:aaaabbbb")]);
        assert_eq!(
            anchor_match_score(&cold, &candidate, DEFAULT_ANCHOR_MATCH_BIAS_CAP).value,
            0.0
        );

        let context = AnchorMatchContext::new([("schema", "blake3:ccccdddd")]);
        assert_eq!(
            anchor_match_score(&context, &candidate, DEFAULT_ANCHOR_MATCH_BIAS_CAP).value,
            0.0
        );
    }

    #[test]
    fn anchor_match_is_additive_and_clamped_in_final_score() {
        let config = SearchScoringConfig::default();
        let base = SearchScoringSignals::new(0.50, RetrievalMaturity::Semantic);
        let boosted = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                anchor_match: Some(1.0),
                ..base
            },
            config,
        );

        assert_close(boosted.anchor_match, DEFAULT_ANCHOR_MATCH_BIAS_CAP);
        assert_close(boosted.final_score, 0.58);
    }

    #[test]
    fn bead_affinity_matches_labels_content_hash_and_links_under_cap() {
        let context = BeadAffinityContext::new(
            "bd-2942u",
            ["swarmx", "retrieval"],
            "bead-aware retrieval prioritization for context and search",
        );
        let candidate = BeadAffinityCandidateSignals::new()
            .with_tags(["retrieval", "agent-ux"])
            .with_content("context retrieval ranking should use bead tokens")
            .with_content_hash(Some("blake3:retrieval-deadbeef"))
            .with_link_refs(["source_uri:bd-2942u-parent"]);

        let score = bead_affinity_score(&context, &candidate, DEFAULT_BEAD_AFFINITY_BIAS_CAP);

        assert!(score.applied());
        assert_eq!(score.tag_overlap, 1);
        assert!(score.content_token_overlap > 0);
        assert!(score.content_hash_overlap);
        assert_eq!(score.link_overlap, 1);
        assert!(score.value <= DEFAULT_BEAD_AFFINITY_BIAS_CAP);
        assert!(score.capped);
    }

    #[test]
    fn bead_affinity_cold_start_and_non_matches_are_zero() {
        let cold = BeadAffinityContext::new("bd-empty", std::iter::empty::<String>(), "");
        let candidate = BeadAffinityCandidateSignals::new()
            .with_tags(["release"])
            .with_content("formatting release notes");

        assert_eq!(
            bead_affinity_score(&cold, &candidate, DEFAULT_BEAD_AFFINITY_BIAS_CAP).value,
            0.0
        );

        let context = BeadAffinityContext::new("bd-2942u", ["swarmx"], "retrieval prioritization");
        assert_eq!(
            bead_affinity_score(&context, &candidate, DEFAULT_BEAD_AFFINITY_BIAS_CAP).value,
            0.0
        );
    }

    #[test]
    fn bead_affinity_empty_bead_id_does_not_credit_every_link() {
        // Regression: an empty bead_id with non-empty labels/tokens used
        // to evade `is_cold_start()` and then `link.contains("")` matched
        // every candidate link, inflating `link_overlap` and pushing the
        // bias toward the cap. The link matcher now skips empty bead ids.
        let context = BeadAffinityContext::new("", ["retrieval"], "ranking");
        let candidate = BeadAffinityCandidateSignals::new()
            .with_tags(["release"])
            .with_link_refs([
                "source_uri:bd-aaaaa-parent",
                "source_uri:bd-bbbbb-child",
                "source_uri:bd-ccccc",
            ]);

        let score = bead_affinity_score(&context, &candidate, DEFAULT_BEAD_AFFINITY_BIAS_CAP);

        assert_eq!(score.link_overlap, 0);
    }

    #[test]
    fn bead_affinity_bead_id_only_context_can_match_links() {
        let context = BeadAffinityContext::new("bd-2942u", std::iter::empty::<String>(), "");
        let candidate = BeadAffinityCandidateSignals::new()
            .with_tags(["unrelated"])
            .with_link_refs(["source_uri:bd-2942u-parent"]);

        let score = bead_affinity_score(&context, &candidate, DEFAULT_BEAD_AFFINITY_BIAS_CAP);

        assert_eq!(score.link_overlap, 1);
        assert!(score.applied());
    }

    #[test]
    fn bead_affinity_link_overlap_matches_bead_id_boundaries() {
        let context = BeadAffinityContext::new("bd-1", ["retrieval"], "ranking");
        let candidate = BeadAffinityCandidateSignals::new().with_link_refs([
            "source_uri:bd-1-parent",
            "source_uri:bd-10-parent",
            "source_uri:bd-1a-child",
            "xdb-1",
            "bd-1",
        ]);

        let score = bead_affinity_score(&context, &candidate, DEFAULT_BEAD_AFFINITY_BIAS_CAP);

        assert_eq!(score.link_overlap, 2);
    }

    #[test]
    fn bead_affinity_is_additive_and_clamped_in_final_score() {
        let config = SearchScoringConfig::default();
        let base = SearchScoringSignals::new(0.50, RetrievalMaturity::Semantic);
        let boosted = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                bead_affinity: Some(1.0),
                ..base
            },
            config,
        );

        assert_close(boosted.bead_affinity, DEFAULT_BEAD_AFFINITY_BIAS_CAP);
        assert_close(boosted.final_score, 0.55);
    }

    #[test]
    fn speed_mode_strings() {
        assert_eq!(SpeedMode::Instant.as_str(), "instant");
        assert_eq!(SpeedMode::Default.as_str(), "default");
        assert_eq!(SpeedMode::Quality.as_str(), "quality");
    }

    #[test]
    fn speed_mode_parse() -> Result<(), String> {
        assert_eq!(
            "instant"
                .parse::<SpeedMode>()
                .map_err(|error| error.to_string())?,
            SpeedMode::Instant
        );
        assert_eq!(
            "default"
                .parse::<SpeedMode>()
                .map_err(|error| error.to_string())?,
            SpeedMode::Default
        );
        assert_eq!(
            "quality"
                .parse::<SpeedMode>()
                .map_err(|error| error.to_string())?,
            SpeedMode::Quality
        );
        assert_eq!(
            " Quality "
                .parse::<SpeedMode>()
                .map_err(|error| error.to_string())?,
            SpeedMode::Quality
        );
        assert!("fast".parse::<SpeedMode>().is_err());
        Ok(())
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

    #[test]
    fn freshness_drift_multiplier_ranks_down_without_vanishing() {
        let floor = DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR;
        let current = freshness_drift_multiplier(MemoryAnchorFreshnessState::Current, floor);
        let suspect = freshness_drift_multiplier(MemoryAnchorFreshnessState::Suspect, floor);
        let stale = freshness_drift_multiplier(MemoryAnchorFreshnessState::Stale, floor);

        assert_eq!(current, 1.0, "current freshness is neutral");
        assert_eq!(
            stale, floor,
            "stale takes the full penalty down to the floor"
        );
        assert!(stale > 0.0, "a stale memory ranks down but never vanishes");
        assert!(
            stale < suspect && suspect < current,
            "suspect penalty is partial: between stale floor and neutral"
        );
    }

    #[test]
    fn freshness_drift_multiplier_clamps_floor_to_unit_interval() {
        // A negative or >1 floor cannot make a memory vanish or boost it.
        assert_eq!(
            freshness_drift_multiplier(MemoryAnchorFreshnessState::Stale, 2.0),
            1.0
        );
        let stale_neg = freshness_drift_multiplier(MemoryAnchorFreshnessState::Stale, -1.0);
        assert!((0.0..=1.0).contains(&stale_neg));
    }

    #[test]
    fn stale_anchor_floor_maps_penalty_to_clamped_floor() {
        // bd-2vq2z.1: penalty 0.0 -> neutral floor 1.0 (flag, don't penalize).
        assert_eq!(stale_anchor_floor(DEFAULT_STALE_ANCHOR_PENALTY), 1.0);
        assert_eq!(stale_anchor_floor(0.0), 1.0);
        assert!((stale_anchor_floor(0.6) - 0.4).abs() < 1e-6);
        assert_eq!(
            stale_anchor_floor(1.0),
            DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR
        );
        // Out-of-range penalties are clamped into the unit interval.
        assert_eq!(
            stale_anchor_floor(2.0),
            DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR
        );
        assert_eq!(stale_anchor_floor(-1.0), 1.0);
        assert_eq!(stale_anchor_floor(f32::NAN), 1.0);
    }

    #[test]
    fn from_signals_flags_drift_without_penalizing_by_default() {
        // bd-2vq2z.1 (Phase-6 pass 2): the DEFAULT retrieval contract FLAGS a
        // drifted anchor but does NOT rank it down. A stale anchored memory keeps
        // the same final score as a neutral one under the default config.
        let config = SearchScoringConfig::default();
        assert_eq!(config.stale_anchor_penalty, DEFAULT_STALE_ANCHOR_PENALTY);
        let base = SearchScoringSignals::new(1.0, RetrievalMaturity::Semantic);

        let neutral = SearchScoreComponents::from_signals(base, config);
        assert_eq!(neutral.freshness_drift, 1.0);

        let stale = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                freshness_drift: Some(MemoryAnchorFreshnessState::Stale),
                ..base
            },
            config,
        );
        assert_eq!(
            stale.freshness_drift, 1.0,
            "default penalty 0.0 leaves a stale anchor un-penalized (flag, don't penalize)"
        );
        assert_eq!(
            stale.final_score, neutral.final_score,
            "a drifted memory keeps its rank under the default config"
        );
    }

    #[test]
    fn from_signals_applies_opt_in_stale_anchor_penalty() {
        // An operator may opt into a small tie-breaker. With penalty p, a Stale
        // anchor's multiplier falls to max(1.0 - p, floor), and never vanishes.
        let config = SearchScoringConfig {
            stale_anchor_penalty: 0.6,
            ..SearchScoringConfig::default()
        };
        let base = SearchScoringSignals::new(1.0, RetrievalMaturity::Semantic);

        let neutral = SearchScoreComponents::from_signals(base, config);
        let stale = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                freshness_drift: Some(MemoryAnchorFreshnessState::Stale),
                ..base
            },
            config,
        );
        assert!((stale.freshness_drift - DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR).abs() < 1e-6);
        assert!(stale.final_score < neutral.final_score);
        assert!(stale.final_score > 0.0, "penalized but never vanishes");
    }

    #[test]
    fn max_stale_anchor_penalty_keeps_drifted_memory_visible() {
        let config = SearchScoringConfig {
            stale_anchor_penalty: 1.0,
            ..SearchScoringConfig::default()
        };
        let stale = SearchScoreComponents::from_signals(
            SearchScoringSignals {
                freshness_drift: Some(MemoryAnchorFreshnessState::Stale),
                ..SearchScoringSignals::new(1.0, RetrievalMaturity::Semantic)
            },
            config,
        );

        assert_eq!(stale.freshness_drift, DEFAULT_FRESHNESS_DRIFT_PENALTY_FLOOR);
        assert!(
            stale.final_score > 0.0,
            "even the maximum opt-in penalty cannot suppress a drifted memory to zero"
        );
    }
}
