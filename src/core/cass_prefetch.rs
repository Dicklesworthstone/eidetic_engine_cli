//! SRR5 (bd-16pwc) — Speculative CASS pre-fetch scaffold.
//!
//! This module ships the v1 surface for SRR5's first half — speculative
//! CASS pre-fetch. The full SRR5 acceptance also covers per-agent
//! adaptive scheduling (noisy-neighbor backoff); that wires through
//! the optional daemon and lives in a follow-up slice. This file owns:
//!
//!   1. A pure [`SpeculativePrefetch`] trait so callers can plug in
//!      alternate predictors (e.g. an ML-backed one) without touching
//!      callers. The signature
//!      `predict_next_n(&self, history: &CassPrefetchHistory) -> Vec<CassPrefetchCandidate>`
//!      is deterministic — no clock reads, no env, no I/O — so the
//!      same history always produces the same candidate list.
//!
//!   2. A default `RecencyWeightedFrequencyPredictor` implementation
//!      built on the heuristic the bead text calls out: recent topics
//!      + frequency, smoothed by a recency-decay weight. Same shape
//!      that other ee scoring code uses (see
//!      `core::budget_classifier::normalized_retrieval_entropy` for
//!      the deterministic-pure-function pattern).
//!
//!   3. A [`CassPrefetchMetrics`] counter that tracks hit/miss/budget
//!      events so the existing `--explain` surface and steward-side
//!      flight recorder can render the prefetch posture without
//!      reaching into the predictor's internal state.
//!
//!   4. Deterministic schema constants
//!      ([`CASS_PREFETCH_DECISION_SCHEMA_V1`],
//!      [`CASS_PREFETCH_METRICS_SCHEMA_V1`]) so the `ee.response.v2`
//!      `--explain` blob can pin the contract for agents reading the
//!      pre-fetch posture.
//!
//! Out of scope for this slice (separate bd-16pwc follow-up slices):
//!
//!   - Wiring the predictor into the optional SRR1 daemon's idle slots
//!     (needs Asupersync low-priority region scaffolding from SRR1).
//!   - Cosine similarity against per-workspace task templates (bead
//!     calls for this as an ALTERNATIVE heuristic; the trait is the
//!     extension seam).
//!   - Per-agent adaptive scheduling and `adaptive_backoff_applied`
//!     degraded code; that subsystem touches the daemon scheduler.
//!   - `ee swarm brief --include-adaptive --json` reporting; that's a
//!     CLI surface slice that consumes [`CassPrefetchMetrics`].
//!   - Budget-exceeded `cass_prefetch_budget_exceeded` degraded code
//!     emission and its failure-mode fixture + taxonomy row. The
//!     module exposes [`CassPrefetchMetrics::record_budget_exceeded`]
//!     so the daemon slice can wire it without changing this file.
//!
//! Determinism contract (load-bearing): the predictor must be a pure
//! function of its input history. No time-of-day, no RNG, no env, no
//! workspace I/O. Same input → byte-identical output. This is what
//! lets the v1 ship without affecting the existing pack-hash
//! determinism gate — the speculative pre-fetch is a CACHE
//! population, never a retrieval-policy mutation.
//!
//! Cross-platform determinism (bd-kpynd): the recency-decay weight is
//! `2^(-position / half_life)`. `f64::powf` is only specified to ~1 ulp
//! and is NOT bit-identical across libm implementations, so the same
//! history could yield different last-bit scores — and thus a different
//! `top_k` candidate set — on x86_64 vs aarch64. [`deterministic_pow_half`]
//! replaces `powf` with a range-reduction + degree-10 polynomial that
//! uses only correctly-rounded IEEE ops, restoring byte-identical output
//! across targets.
//!
//! Cache-coherence contract (bd-qud3c): a [`CassPrefetchHistory`] carries
//! the [`PrefetchGeneration`] `(workspace_generation, index_generation)`
//! it was built against, mirroring the gate `src/cache/hotset.rs` admits
//! cache entries on. [`SpeculativePrefetch::predict_next_n_gated`] drops a
//! prediction (returning [`CASS_PREFETCH_STALE_GENERATION_CODE`]) the
//! instant a reindex or workspace switch bumps the live generation, so the
//! daemon never warms slots from a stale topic distribution the hotset
//! gate would only reject downstream — and `hit_rate()` cannot silently
//! collapse across a reindex boundary. The daemon-wiring slice MUST call
//! the gated method and feed the live gate from the same source the hotset
//! reads.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Schema id for the per-call prefetch decision blob emitted under
/// `--explain` (a follow-up CLI slice surfaces it; the constant is
/// pinned here so the schema + producer share a single source of
/// truth).
pub const CASS_PREFETCH_DECISION_SCHEMA_V1: &str = "ee.cass_prefetch.decision.v1";

/// Schema id for the prefetch metrics rollup the daemon slice will
/// surface through `ee swarm brief --include-adaptive --json` and the
/// flight recorder.
pub const CASS_PREFETCH_METRICS_SCHEMA_V1: &str = "ee.cass_prefetch.metrics.v1";

/// Default per-call prefetch candidate cap. The bead specifies
/// `top-3`; we expose it as a const so the daemon slice can tune it
/// through `[swarm.adaptive].prefetch_top_k` without recompiling.
pub const DEFAULT_PREFETCH_TOP_K: usize = 3;

/// Default rolling history window the bead specifies (`last 10
/// queries`).
pub const DEFAULT_PREFETCH_HISTORY_WINDOW: usize = 10;

/// Default recency-decay half-life expressed as a position offset.
/// Position 0 (most recent) gets weight 1.0; position N gets weight
/// `0.5 ^ (N / DEFAULT_PREFETCH_RECENCY_HALF_LIFE)`. The bead
/// recommends recency-decay; the half-life shape lets the heuristic
/// degrade smoothly across the 10-element window without a hard
/// cutoff.
pub const DEFAULT_PREFETCH_RECENCY_HALF_LIFE: f64 = 3.0;

/// Default minimum weighted score a candidate must clear before the
/// predictor includes it in its return set. The bead's
/// `similarity_threshold_respected` test contract — even though our
/// default heuristic is frequency-based rather than cosine-based —
/// requires SOME admission gate so low-confidence candidates never
/// pollute the prefetch queue.
pub const DEFAULT_PREFETCH_MIN_SCORE: f64 = 0.10;

/// Default soft budget for a single prefetch slot. The bead specifies
/// `~50ms`; we expose it for daemon-side tuning.
pub const DEFAULT_PREFETCH_BUDGET: Duration = Duration::from_millis(50);

/// Degraded code emitted when a generation-gated prediction is skipped
/// because the history's [`PrefetchGeneration`] no longer matches the
/// live gate — i.e. a reindex (index_generation bump) or workspace
/// switch (workspace_generation bump) invalidated the topic
/// distribution the history was built against (bd-qud3c). Surfacing it
/// on the `--explain` `degraded[]` array (and landing its failure-mode
/// fixture + taxonomy row) is deferred to the daemon-wiring slice, same
/// as `cass_prefetch_budget_exceeded`.
pub const CASS_PREFETCH_STALE_GENERATION_CODE: &str = "cass_prefetch_stale_generation";

/// Cache-coherence generation tag (bd-qud3c).
///
/// Mirrors the `(workspace_generation, index_generation)` pair that
/// `src/cache/hotset.rs`'s `GenerationGate` admits cache entries on. The
/// prefetch layer was previously a pure function of topic history with
/// NO generation awareness: after a background reindex bumped
/// `index_generation`, the predictor kept emitting topics from the
/// stale-index distribution, the hotset admission gate rejected every
/// resulting entry, and `hit_rate()` silently collapsed toward zero with
/// no diagnostic. Tagging the history with the generation it was built
/// against lets [`SpeculativePrefetch::predict_next_n_gated`] drop stale
/// predictions BEFORE the daemon spends its per-slot budget warming them.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefetchGeneration {
    /// Bumped on workspace switch.
    pub workspace_generation: u64,
    /// Bumped on index regeneration (reindex) — the dominant
    /// invalidation event on a hot, frequently-reindexed host.
    pub index_generation: u64,
}

impl PrefetchGeneration {
    #[must_use]
    pub const fn new(workspace_generation: u64, index_generation: u64) -> Self {
        Self {
            workspace_generation,
            index_generation,
        }
    }

    /// True iff a prediction built at `self` is still coherent with the
    /// live gate `current`. Coherence requires an EXACT match on both
    /// generations: any bump (reindex or workspace switch) makes the
    /// prior topic distribution stale, so the predictor must drop rather
    /// than warm slots the hotset gate would only reject downstream.
    #[must_use]
    pub const fn is_coherent_with(self, current: Self) -> bool {
        self.workspace_generation == current.workspace_generation
            && self.index_generation == current.index_generation
    }
}

/// One observed prior ee context call in the per-agent rolling
/// history window. The predictor sees these and nothing else — no
/// memory IDs, no raw query text, no clock — so the result is a pure
/// function of the topic sequence.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassPrefetchObservation {
    /// Normalized topic identifier. Production callers will derive
    /// this from the query string + workspace task-template mapping
    /// (a follow-up slice); the trait does not need to know how the
    /// topic was identified.
    pub topic_id: String,
}

impl CassPrefetchObservation {
    #[must_use]
    pub fn new(topic_id: impl Into<String>) -> Self {
        Self {
            topic_id: topic_id.into(),
        }
    }
}

/// Per-agent rolling history of recent ee context queries. Position 0
/// is the most recent call; position `len()-1` is the oldest in the
/// retained window.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassPrefetchHistory {
    /// Cache-coherence tag the history was built against (bd-qud3c).
    /// `#[serde(default)]` keeps histories serialized before the
    /// coherence contract (no generation field) decodable; they default
    /// to `(0, 0)` and a generation-aware caller treats `(0, 0)` like
    /// any other generation when gating.
    #[serde(default)]
    pub generation: PrefetchGeneration,
    /// Most-recent-first. Pre-fetch math is easier when the array is
    /// ordered consistently and the producer owns the trimming.
    pub recent_first: Vec<CassPrefetchObservation>,
}

impl CassPrefetchHistory {
    #[must_use]
    pub fn new(recent_first: Vec<CassPrefetchObservation>) -> Self {
        Self {
            generation: PrefetchGeneration::default(),
            recent_first,
        }
    }

    /// Construct a history from an iterator of topic IDs in
    /// most-recent-first order. Helper for the tests + the daemon's
    /// rolling-window adapter.
    #[must_use]
    pub fn from_topics<I, S>(recent_first_topics: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            generation: PrefetchGeneration::default(),
            recent_first: recent_first_topics
                .into_iter()
                .map(CassPrefetchObservation::new)
                .collect(),
        }
    }

    /// Stamp the history with the generation it was built against
    /// (bd-qud3c). Builder-style so callers can chain it onto
    /// [`from_topics`]/[`new`]: `CassPrefetchHistory::from_topics(..)
    /// .with_generation(PrefetchGeneration::new(ws, idx))`.
    #[must_use]
    pub fn with_generation(mut self, generation: PrefetchGeneration) -> Self {
        self.generation = generation;
        self
    }

    /// Length of the retained window.
    #[must_use]
    pub fn len(&self) -> usize {
        self.recent_first.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recent_first.is_empty()
    }

    /// Most-recent-first iterator over the observations.
    pub fn iter(&self) -> std::slice::Iter<'_, CassPrefetchObservation> {
        self.recent_first.iter()
    }
}

/// A single predicted candidate the speculative pre-fetcher emits for
/// idle-slot warming.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassPrefetchCandidate {
    /// Topic the daemon should pre-stage CASS evidence for.
    pub topic_id: String,
    /// Predictor-internal score in `[0.0, 1.0]`. Higher is more
    /// confident. Pinned to a finite, non-negative number by
    /// construction; predictors that compute non-finite intermediates
    /// must filter before emitting.
    pub score: f64,
    /// Predictor identifier for audit / explain blobs. Defaults to
    /// the predictor's `name()`; tests can override.
    pub predictor: String,
}

impl CassPrefetchCandidate {
    #[must_use]
    pub fn new(topic_id: impl Into<String>, score: f64, predictor: impl Into<String>) -> Self {
        Self {
            topic_id: topic_id.into(),
            score,
            predictor: predictor.into(),
        }
    }
}

/// Result of a generation-gated prediction
/// ([`SpeculativePrefetch::predict_next_n_gated`], bd-qud3c).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GatedPrediction {
    /// Candidates to warm. Empty when `degraded` is set.
    pub candidates: Vec<CassPrefetchCandidate>,
    /// `Some(`[`CASS_PREFETCH_STALE_GENERATION_CODE`]`)` when the
    /// prediction was dropped because the history's generation tag no
    /// longer matched the live gate; `None` on a coherent prediction.
    pub degraded: Option<&'static str>,
}

/// Predictor trait. Implementations are pure functions of the input
/// history — see the module-level determinism contract.
pub trait SpeculativePrefetch {
    /// Stable predictor name surfaced in audit + explain blobs.
    fn name(&self) -> &'static str;

    /// Predict up to `top_k` next topics to pre-fetch. Implementations
    /// MUST:
    ///
    ///   - Return AT MOST `top_k` candidates (the bead caps prefetch
    ///     fan-out per call).
    ///   - Skip any candidate whose computed score falls below the
    ///     predictor's admission threshold.
    ///   - Order the returned vector deterministically: highest score
    ///     first, lexicographic `topic_id` tie-break (no use of
    ///     `partial_cmp(...).unwrap_or(Equal)`).
    ///   - Never return non-finite or negative scores.
    fn predict_next_n(
        &self,
        history: &CassPrefetchHistory,
        top_k: usize,
    ) -> Vec<CassPrefetchCandidate>;

    /// Generation-gated prediction (bd-qud3c). Guards [`predict_next_n`]
    /// with a cache-coherence check: if the history's
    /// [`PrefetchGeneration`] is not coherent with `current_generation`
    /// (a reindex or workspace switch has bumped a generation), returns
    /// no candidates and [`CASS_PREFETCH_STALE_GENERATION_CODE`] WITHOUT
    /// invoking the predictor — so the daemon never spends its per-slot
    /// budget warming evidence the hotset admission gate
    /// (`src/cache/hotset.rs`) would only reject downstream, and
    /// `hit_rate()` cannot silently collapse across a reindex boundary.
    /// On a coherent gate it delegates to [`predict_next_n`].
    ///
    /// Default-implemented so every predictor gains the gate for free;
    /// the daemon-wiring slice calls THIS method (not the bare
    /// [`predict_next_n`]) and feeds the live `(workspace_generation,
    /// index_generation)` from the same source the hotset gate reads.
    fn predict_next_n_gated(
        &self,
        history: &CassPrefetchHistory,
        current_generation: PrefetchGeneration,
        top_k: usize,
    ) -> GatedPrediction {
        if !history.generation.is_coherent_with(current_generation) {
            return GatedPrediction {
                candidates: Vec::new(),
                degraded: Some(CASS_PREFETCH_STALE_GENERATION_CODE),
            };
        }
        GatedPrediction {
            candidates: self.predict_next_n(history, top_k),
            degraded: None,
        }
    }
}

/// Default heuristic — recency-weighted frequency. For each topic in
/// the rolling history, sum a recency weight that decays with
/// position. Candidates are the unique topics, scored by their
/// weighted-frequency sum normalized to `[0.0, 1.0]`. The most-recent
/// topic itself is excluded as a candidate (predicting "you just ran
/// this query, run it again" provides no prefetch value).
///
/// Recency decay shape:
/// `weight(position) = 0.5 ^ (position / DEFAULT_PREFETCH_RECENCY_HALF_LIFE)`.
///
/// Position 0 (most recent) gets weight 1.0; position 3 gets ~0.5;
/// position 6 gets ~0.25; position 10 gets ~0.099. Smooth decay
/// across the canonical 10-element window without a hard cutoff.
#[derive(Clone, Debug, Default)]
pub struct RecencyWeightedFrequencyPredictor {
    half_life: f64,
    min_score: f64,
}

impl RecencyWeightedFrequencyPredictor {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            half_life: DEFAULT_PREFETCH_RECENCY_HALF_LIFE,
            min_score: DEFAULT_PREFETCH_MIN_SCORE,
        }
    }

    /// Override the recency half-life (must be positive + finite).
    /// Non-finite or non-positive values fall back to the default so
    /// the predictor never divides by zero or panics.
    #[must_use]
    pub fn with_half_life(mut self, half_life: f64) -> Self {
        if half_life.is_finite() && half_life > 0.0 {
            self.half_life = half_life;
        }
        self
    }

    /// Override the admission threshold. Clamped into `[0.0, 1.0]`
    /// and non-finite values fall back to the default.
    #[must_use]
    pub fn with_min_score(mut self, min_score: f64) -> Self {
        if min_score.is_finite() {
            self.min_score = min_score.clamp(0.0, 1.0);
        }
        self
    }

    /// Recency weight at a 0-indexed position (0 = most recent).
    ///
    /// `weight(position) = 2^(-position / half_life)`, evaluated through
    /// [`deterministic_pow_half`] rather than `0.5_f64.powf(..)` so the
    /// result is byte-identical across libm implementations (bd-kpynd).
    fn recency_weight(&self, position: usize) -> f64 {
        let exponent = position as f64 / self.half_life;
        deterministic_pow_half(exponent)
    }
}

/// Cross-platform-deterministic `2^(-x)` for `x >= 0` (bd-kpynd).
///
/// `f64::powf` / `f64::exp2` route through the platform libm, whose
/// transcendental approximations differ in the last ulp across targets
/// (x86_64 glibc vs aarch64 glibc vs macOS Accelerate vs musl vs MSVC).
/// That last-bit drift can flip the `total_cmp` sort order in
/// [`RecencyWeightedFrequencyPredictor::predict_next_n`] and change which
/// candidates survive the `top_k` truncation, violating the module's
/// load-bearing byte-identical determinism contract (and any `--explain`
/// artifact that embeds the prefetch decision blob).
///
/// This shim uses ONLY IEEE-754 operations specified to be correctly
/// rounded — and therefore bit-identical on every conforming target:
/// `+`, `-`, `*`, `/`, and `floor` (roundToIntegral). It deliberately
/// avoids `f64::mul_add`/FMA: Rust does not contract `a * b + c` into a
/// fused op without an explicit `mul_add` call or fast-math, so the
/// plain Horner form in [`exp2_neg_unit_interval`] is reproducible.
///
/// Range reduction `2^(-x) = 2^(-k) * 2^(-f)` (with `k = floor(x)`,
/// `f = x - k`) keeps the polynomial argument in `[0, 1)` where the
/// degree-10 Maclaurin truncation is accurate to < 1e-9, and makes
/// integer exponents collapse to EXACT powers of two on every platform.
fn deterministic_pow_half(exponent: f64) -> f64 {
    // Total + defensive over the whole domain so a future caller cannot
    // route a NaN/inf into a platform transcendental by accident. The
    // production caller already feeds a finite, non-negative
    // `position / half_life`.
    if exponent.is_nan() {
        return 0.0;
    }
    if exponent <= 0.0 {
        // Position 0 (and the clamped negative domain) has full weight.
        return 1.0;
    }
    if exponent >= 64.0 {
        // 2^-64 already underflows the heuristic to a weight the
        // min_score gate always drops; saturate so the halving loop
        // below stays bounded for a pathological half_life.
        return 0.0;
    }
    // 2^(-x) = 2^(-k) * 2^(-f), k = floor(x), f in [0, 1).
    let k = exponent.floor();
    let frac = exponent - k;
    // 2^(-k) is exact in f64 via repeated halving (k <= 63 here): each
    // `* 0.5` only decrements the binary exponent, no rounding.
    let mut pow2_neg_k = 1.0_f64;
    let mut steps = k as u32;
    while steps > 0 {
        pow2_neg_k *= 0.5;
        steps -= 1;
    }
    pow2_neg_k * exp2_neg_unit_interval(frac)
}

/// Polynomial approximation of `2^(-t)` for `t` in `[0, 1)`, evaluated in
/// Horner form with correctly-rounded ops only (bit-reproducible across
/// targets). Degree-10 truncation of the Maclaurin series
/// `2^(-t) = sum_n (-t * ln2)^n / n!`; max abs error < 1e-9 on the unit
/// interval. `poly(0.0) == 1.0` exactly (every `* t` term vanishes), so
/// integer exponents in [`deterministic_pow_half`] are exact.
fn exp2_neg_unit_interval(t: f64) -> f64 {
    // c_n = (-ln2)^n / n!, ln2 = 0.6931471805599453.
    const C0: f64 = 1.0;
    const C1: f64 = -0.6931471805599453;
    const C2: f64 = 0.2402265069591007;
    const C3: f64 = -0.055504108664821576;
    const C4: f64 = 0.009618129107628477;
    const C5: f64 = -0.0013333558146428441;
    const C6: f64 = 0.00015403530393381606;
    const C7: f64 = -1.5252733804059838e-05;
    const C8: f64 = 1.3215486790144305e-06;
    const C9: f64 = -1.0178086009239696e-07;
    const C10: f64 = 7.054911620801121e-09;
    // Horner, ascending-degree fold. Explicit `* t` + `+ c` (never
    // `mul_add`) keeps the evaluation bit-identical across platforms.
    let mut acc = C10;
    acc = acc * t + C9;
    acc = acc * t + C8;
    acc = acc * t + C7;
    acc = acc * t + C6;
    acc = acc * t + C5;
    acc = acc * t + C4;
    acc = acc * t + C3;
    acc = acc * t + C2;
    acc = acc * t + C1;
    acc = acc * t + C0;
    acc
}

impl SpeculativePrefetch for RecencyWeightedFrequencyPredictor {
    fn name(&self) -> &'static str {
        "recency_weighted_frequency_v1"
    }

    fn predict_next_n(
        &self,
        history: &CassPrefetchHistory,
        top_k: usize,
    ) -> Vec<CassPrefetchCandidate> {
        if top_k == 0 || history.is_empty() {
            return Vec::new();
        }

        // Skip the most-recent topic as a candidate (predicting an
        // immediate repeat provides no prefetch value). All other
        // topics in the rolling window contribute their recency
        // weight.
        let most_recent_topic = history
            .recent_first
            .first()
            .map(|obs| obs.topic_id.as_str());

        let mut accumulator: HashMap<String, f64> = HashMap::new();
        let mut total_weight: f64 = 0.0;
        for (position, observation) in history.recent_first.iter().enumerate() {
            let weight = self.recency_weight(position);
            if !weight.is_finite() || weight < 0.0 {
                continue;
            }
            total_weight += weight;
            if Some(observation.topic_id.as_str()) == most_recent_topic {
                continue;
            }
            *accumulator
                .entry(observation.topic_id.clone())
                .or_insert(0.0) += weight;
        }

        if total_weight <= 0.0 || !total_weight.is_finite() {
            return Vec::new();
        }

        // Normalize so the score is bounded in [0.0, 1.0] regardless
        // of how full the rolling window is. The normalization also
        // makes the admission threshold (min_score) workspace-
        // independent.
        let mut scored: Vec<CassPrefetchCandidate> = accumulator
            .into_iter()
            .map(|(topic_id, weighted_sum)| {
                let normalized = (weighted_sum / total_weight).clamp(0.0, 1.0);
                CassPrefetchCandidate::new(topic_id, normalized, self.name())
            })
            .filter(|candidate| {
                candidate.score.is_finite()
                    && candidate.score >= self.min_score
                    && candidate.score >= 0.0
            })
            .collect();

        // Deterministic order: highest score first, lexicographic
        // topic_id tie-break. `total_cmp` over `partial_cmp(...)
        // .unwrap_or(Equal)` because the filter above already
        // excluded NaNs but the contract is the determinism gate, not
        // the absence-of-NaN guarantee, and a future caller that
        // bypasses the filter must not silently break ordering.
        scored.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.topic_id.cmp(&right.topic_id))
        });

        scored.truncate(top_k);
        scored
    }
}

/// Hit / miss / budget counter the daemon and the flight recorder
/// consume. The struct is intentionally simple — `u64` saturating
/// adds, no `Atomic*` — because the daemon owns the call site and
/// can wrap it in whatever synchronization it needs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassPrefetchMetrics {
    pub hits: u64,
    pub misses: u64,
    pub candidates_emitted: u64,
    pub budget_exceeded: u64,
    pub history_too_short: u64,
    /// Predictions dropped because the history's generation tag no
    /// longer matched the live gate (bd-qud3c). Distinct from
    /// `history_too_short`: this counts STALE-but-present history, the
    /// dominant invalidation event across a reindex, so an operator can
    /// tell "predictor is stuck on a stale generation" apart from
    /// "predictor has not seen enough queries yet." `#[serde(default)]`
    /// keeps metrics serialized before the coherence contract decodable.
    #[serde(default)]
    pub stale_generation_drop: u64,
}

impl CassPrefetchMetrics {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            hits: 0,
            misses: 0,
            candidates_emitted: 0,
            budget_exceeded: 0,
            history_too_short: 0,
            stale_generation_drop: 0,
        }
    }

    pub fn record_hit(&mut self) {
        self.hits = self.hits.saturating_add(1);
    }

    pub fn record_miss(&mut self) {
        self.misses = self.misses.saturating_add(1);
    }

    pub fn record_candidate(&mut self) {
        self.candidates_emitted = self.candidates_emitted.saturating_add(1);
    }

    pub fn record_budget_exceeded(&mut self) {
        self.budget_exceeded = self.budget_exceeded.saturating_add(1);
    }

    pub fn record_history_too_short(&mut self) {
        self.history_too_short = self.history_too_short.saturating_add(1);
    }

    /// Record a prediction dropped for cache-coherence reasons — the
    /// daemon-wiring slice calls this whenever
    /// [`SpeculativePrefetch::predict_next_n_gated`] returns
    /// [`CASS_PREFETCH_STALE_GENERATION_CODE`] (bd-qud3c).
    pub fn record_stale_generation_drop(&mut self) {
        self.stale_generation_drop = self.stale_generation_drop.saturating_add(1);
    }

    /// Hit rate as a fraction in `[0.0, 1.0]`. Returns 0.0 when no
    /// hits or misses have been observed (zero-attempt case is "no
    /// data," not "0% hit rate" — but callers that want to distinguish
    /// the two should check `attempts()` directly).
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let attempts = self.attempts();
        if attempts == 0 {
            0.0
        } else {
            (self.hits as f64) / (attempts as f64)
        }
    }

    /// Total hit + miss observations.
    #[must_use]
    pub const fn attempts(&self) -> u64 {
        self.hits.saturating_add(self.misses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(topics_recent_first: &[&str]) -> CassPrefetchHistory {
        CassPrefetchHistory::from_topics(topics_recent_first.iter().copied())
    }

    #[test]
    fn empty_history_predicts_nothing() {
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let predictions = predictor.predict_next_n(&CassPrefetchHistory::default(), 3);
        assert!(predictions.is_empty());
    }

    #[test]
    fn top_k_zero_predicts_nothing() {
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let h = history(&["refactor", "debug", "refactor"]);
        let predictions = predictor.predict_next_n(&h, 0);
        assert!(predictions.is_empty());
    }

    #[test]
    fn most_recent_topic_is_not_a_candidate() {
        // The most-recent topic ("refactor" at position 0) should NOT
        // appear as a candidate because predicting "run the query you
        // just ran" produces no prefetch value. "debug" (position 1)
        // should be the sole candidate.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let h = history(&["refactor", "debug"]);
        let predictions = predictor.predict_next_n(&h, 3);
        assert_eq!(predictions.len(), 1);
        assert_eq!(predictions[0].topic_id, "debug");
    }

    #[test]
    fn predictions_are_capped_to_top_k() {
        let predictor = RecencyWeightedFrequencyPredictor::new();
        // Five distinct non-most-recent topics.
        let h = history(&[
            "current_query",
            "alpha",
            "bravo",
            "charlie",
            "delta",
            "echo",
        ]);
        let predictions = predictor.predict_next_n(&h, 2);
        assert_eq!(predictions.len(), 2);
        let predictions = predictor.predict_next_n(&h, 100);
        // Capped to the number of distinct non-most-recent topics.
        assert_eq!(predictions.len(), 5);
    }

    #[test]
    fn predictions_are_deterministic_under_tied_scores() {
        // Two distinct topics that BOTH appear once at the same
        // recency offset. They must tie-break by topic_id ascending
        // — not by HashMap iteration order — so the prediction list
        // is byte-identical across runs.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let h = history(&["current", "zeta", "alpha"]);
        let predictions = predictor.predict_next_n(&h, 3);
        assert_eq!(predictions.len(), 2);
        // alpha and zeta tie; alpha wins the lex tie-break.
        assert_eq!(predictions[0].topic_id, "alpha");
        assert_eq!(predictions[1].topic_id, "zeta");
    }

    #[test]
    fn higher_frequency_outranks_single_recent() {
        // "alpha" appears three times (positions 2, 3, 4); "bravo"
        // appears once (position 1). With the default half-life of
        // 3.0, position 1 has weight 0.5^(1/3) ≈ 0.794, and the three
        // alpha positions sum to ≈ 0.5^(2/3) + 0.5^(3/3) + 0.5^(4/3)
        // ≈ 0.630 + 0.500 + 0.397 ≈ 1.527. So alpha should outrank
        // bravo despite bravo being more recent.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let h = history(&["current", "bravo", "alpha", "alpha", "alpha"]);
        let predictions = predictor.predict_next_n(&h, 5);
        assert_eq!(predictions[0].topic_id, "alpha");
        assert_eq!(predictions[1].topic_id, "bravo");
        assert!(predictions[0].score > predictions[1].score);
    }

    #[test]
    fn scores_are_finite_and_normalized() {
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let h = history(&["current", "alpha", "bravo", "alpha", "charlie"]);
        for candidate in predictor.predict_next_n(&h, 10) {
            assert!(candidate.score.is_finite());
            assert!(candidate.score >= 0.0);
            assert!(candidate.score <= 1.0);
        }
    }

    #[test]
    fn min_score_threshold_drops_low_confidence_candidates() {
        // With an aggressive threshold, only the dominant topic
        // survives.
        let predictor = RecencyWeightedFrequencyPredictor::new().with_min_score(0.6);
        let h = history(&["current", "alpha", "alpha", "alpha", "noise", "noise"]);
        let predictions = predictor.predict_next_n(&h, 10);
        // "alpha" appears 3x with strong recency; "noise" twice but
        // older. Threshold 0.6 keeps alpha and drops noise.
        assert!(
            predictions.iter().any(|c| c.topic_id == "alpha"),
            "alpha must survive 0.6 threshold; got {predictions:?}"
        );
        assert!(
            !predictions.iter().any(|c| c.topic_id == "noise"),
            "noise must be dropped by 0.6 threshold; got {predictions:?}"
        );
    }

    #[test]
    fn predictor_is_pure_function_of_input() {
        // Same input → byte-identical output across two calls. This
        // is the determinism contract that lets the v1 ship without
        // touching the pack-hash gate.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let h = history(&["current", "alpha", "bravo", "alpha"]);
        let first = predictor.predict_next_n(&h, 5);
        let second = predictor.predict_next_n(&h, 5);
        assert_eq!(first, second);
    }

    #[test]
    fn non_finite_overrides_fall_back_to_defaults() {
        let predictor = RecencyWeightedFrequencyPredictor::new()
            .with_half_life(f64::NAN)
            .with_min_score(f64::NAN);
        let h = history(&["current", "alpha", "alpha"]);
        // Predictor should not panic, should not divide by zero,
        // should produce a finite-scored candidate.
        let predictions = predictor.predict_next_n(&h, 3);
        assert!(predictions.iter().all(|c| c.score.is_finite()));
        // alpha appears twice at non-zero recency, so it should be
        // emitted.
        assert!(predictions.iter().any(|c| c.topic_id == "alpha"));
    }

    #[test]
    fn metrics_record_and_compute_hit_rate() {
        let mut metrics = CassPrefetchMetrics::new();
        assert_eq!(metrics.attempts(), 0);
        assert_eq!(metrics.hit_rate(), 0.0);

        metrics.record_hit();
        metrics.record_hit();
        metrics.record_miss();
        metrics.record_candidate();
        metrics.record_candidate();
        metrics.record_candidate();
        metrics.record_budget_exceeded();
        metrics.record_history_too_short();

        assert_eq!(metrics.hits, 2);
        assert_eq!(metrics.misses, 1);
        assert_eq!(metrics.attempts(), 3);
        assert!((metrics.hit_rate() - (2.0 / 3.0)).abs() < 1e-12);
        assert_eq!(metrics.candidates_emitted, 3);
        assert_eq!(metrics.budget_exceeded, 1);
        assert_eq!(metrics.history_too_short, 1);
    }

    #[test]
    fn metrics_saturate_on_overflow() {
        let mut metrics = CassPrefetchMetrics::new();
        metrics.hits = u64::MAX;
        metrics.record_hit();
        assert_eq!(metrics.hits, u64::MAX);
        metrics.misses = u64::MAX;
        // attempts() must saturate, not panic.
        assert_eq!(metrics.attempts(), u64::MAX);
    }

    #[test]
    fn history_helpers_match_iteration_order() {
        let topics = ["recent", "older", "oldest"];
        let h = CassPrefetchHistory::from_topics(topics.iter().copied());
        assert_eq!(h.len(), 3);
        assert!(!h.is_empty());
        let observed: Vec<&str> = h.iter().map(|o| o.topic_id.as_str()).collect();
        assert_eq!(observed, vec!["recent", "older", "oldest"]);
    }

    #[test]
    fn recency_weight_is_cross_platform_deterministic_bd_kpynd() {
        // bd-kpynd: the recency weight must be byte-identical across
        // libm implementations. deterministic_pow_half uses only
        // correctly-rounded IEEE ops (+, -, *, /, floor), so these
        // assertions hold bit-for-bit on x86_64 AND aarch64 — unlike
        // 0.5_f64.powf(..), whose last bit is platform-dependent.
        let predictor = RecencyWeightedFrequencyPredictor::new(); // half_life = 3.0

        // Integer exponents (position / 3.0 == 1, 2, 3) range-reduce to
        // EXACT powers of two: no transcendental is evaluated, so the
        // result is identical on every target. Asserted with `==`.
        assert_eq!(predictor.recency_weight(0), 1.0);
        assert_eq!(predictor.recency_weight(3), 0.5);
        assert_eq!(predictor.recency_weight(6), 0.25);
        assert_eq!(predictor.recency_weight(9), 0.125);

        // Fractional exponents track true 2^(-x) to < 1e-9 (the
        // degree-10 polynomial), so the heuristic's behavior is
        // preserved relative to the old powf path.
        for position in [1_usize, 2, 4, 5, 7, 8] {
            let got = predictor.recency_weight(position);
            let want = 2.0_f64.powf(-(position as f64) / 3.0);
            assert!(
                (got - want).abs() < 1e-9,
                "position {position}: deterministic weight {got} diverged from 2^(-x) {want}"
            );
        }

        // Strictly decreasing in position (recency decays monotonically)
        // and never produces a non-finite or out-of-range weight.
        for position in 0..16 {
            let weight = predictor.recency_weight(position);
            assert!(weight.is_finite() && (0.0..=1.0).contains(&weight));
            assert!(
                predictor.recency_weight(position) > predictor.recency_weight(position + 1),
                "weight must strictly decrease at position {position}"
            );
        }
    }

    #[test]
    fn deterministic_pow_half_total_over_domain_bd_kpynd() {
        // The shim must be closed over its whole domain so no input can
        // route into a platform transcendental: NaN -> 0.0, the
        // non-positive domain -> 1.0, and the saturating tail -> 0.0.
        assert_eq!(deterministic_pow_half(f64::NAN), 0.0);
        assert_eq!(deterministic_pow_half(0.0), 1.0);
        assert_eq!(deterministic_pow_half(-1.0), 1.0);
        assert_eq!(deterministic_pow_half(f64::NEG_INFINITY), 1.0);
        assert_eq!(deterministic_pow_half(64.0), 0.0);
        assert_eq!(deterministic_pow_half(f64::INFINITY), 0.0);
        // poly(0) == 1.0 keeps integer exponents exact.
        assert_eq!(exp2_neg_unit_interval(0.0), 1.0);
    }

    #[test]
    fn predict_next_n_emits_byte_identical_json_across_calls_bd_kpynd() {
        // The bead's verification target: the same history must serialize
        // to byte-identical JSON. With the deterministic weight this is
        // now a cross-platform guarantee, not just an intra-run one.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let h = history(&[
            "current",
            "refactor",
            "debug",
            "refactor",
            "doc_update",
            "debug",
        ]);
        let first =
            serde_json::to_string(&predictor.predict_next_n(&h, 3)).expect("serialize first");
        let second =
            serde_json::to_string(&predictor.predict_next_n(&h, 3)).expect("serialize second");
        assert_eq!(first, second);
    }

    #[test]
    fn generation_coherence_is_exact_match_bd_qud3c() {
        let tag = PrefetchGeneration::new(7, 42);
        assert!(tag.is_coherent_with(PrefetchGeneration::new(7, 42)));
        // Index regen (idx bump) -> incoherent.
        assert!(!tag.is_coherent_with(PrefetchGeneration::new(7, 43)));
        // Workspace switch (ws bump) -> incoherent.
        assert!(!tag.is_coherent_with(PrefetchGeneration::new(8, 42)));
        // Default is (0, 0) and coherent only with itself.
        assert!(PrefetchGeneration::default().is_coherent_with(PrefetchGeneration::new(0, 0)));
        assert!(!PrefetchGeneration::default().is_coherent_with(PrefetchGeneration::new(0, 1)));
    }

    #[test]
    fn history_constructors_default_generation_to_zero_bd_qud3c() {
        // Back-compat: the pre-coherence constructors stamp (0, 0).
        assert_eq!(
            CassPrefetchHistory::from_topics(["a", "b"]).generation,
            PrefetchGeneration::new(0, 0)
        );
        assert_eq!(
            CassPrefetchHistory::new(vec![CassPrefetchObservation::new("a")]).generation,
            PrefetchGeneration::default()
        );
        // with_generation stamps without disturbing the observations.
        let stamped = CassPrefetchHistory::from_topics(["a", "b"])
            .with_generation(PrefetchGeneration::new(1, 9));
        assert_eq!(stamped.generation, PrefetchGeneration::new(1, 9));
        assert_eq!(stamped.len(), 2);
    }

    #[test]
    fn gated_prediction_drops_stale_generation_bd_qud3c() {
        // History was built at index_generation 5; a reindex has since
        // bumped the live gate to 6. The gated predictor MUST return no
        // candidates and the stale-generation code WITHOUT warming any
        // slot from the now-stale topic distribution.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let stale = history(&["current", "alpha", "alpha", "bravo"])
            .with_generation(PrefetchGeneration::new(1, 5));
        let outcome = predictor.predict_next_n_gated(&stale, PrefetchGeneration::new(1, 6), 3);
        assert!(outcome.candidates.is_empty());
        assert_eq!(outcome.degraded, Some(CASS_PREFETCH_STALE_GENERATION_CODE));

        // A workspace switch (ws bump) is equally stale.
        let outcome_ws = predictor.predict_next_n_gated(&stale, PrefetchGeneration::new(2, 5), 3);
        assert!(outcome_ws.candidates.is_empty());
        assert_eq!(
            outcome_ws.degraded,
            Some(CASS_PREFETCH_STALE_GENERATION_CODE)
        );
    }

    #[test]
    fn gated_prediction_passes_on_coherent_generation_bd_qud3c() {
        // Same generation on both sides -> delegate to predict_next_n,
        // no degraded code, identical candidates to the ungated call.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let fresh = history(&["current", "alpha", "alpha", "bravo"])
            .with_generation(PrefetchGeneration::new(1, 5));
        let outcome = predictor.predict_next_n_gated(&fresh, PrefetchGeneration::new(1, 5), 3);
        assert_eq!(outcome.degraded, None);
        assert!(!outcome.candidates.is_empty());
        assert_eq!(outcome.candidates, predictor.predict_next_n(&fresh, 3));
    }

    #[test]
    fn metrics_record_stale_generation_drop_bd_qud3c() {
        let mut metrics = CassPrefetchMetrics::new();
        assert_eq!(metrics.stale_generation_drop, 0);
        metrics.record_stale_generation_drop();
        metrics.record_stale_generation_drop();
        assert_eq!(metrics.stale_generation_drop, 2);
        // Saturates rather than panicking.
        metrics.stale_generation_drop = u64::MAX;
        metrics.record_stale_generation_drop();
        assert_eq!(metrics.stale_generation_drop, u64::MAX);
    }

    #[test]
    fn schema_constants_are_pinned() {
        // Renaming these constants without bumping the schema version
        // would break agents that pin to the literal string. Pin both
        // here so a future refactor that drops the `.v1` suffix fails
        // this test rather than silently shipping schema drift.
        assert_eq!(
            CASS_PREFETCH_DECISION_SCHEMA_V1,
            "ee.cass_prefetch.decision.v1"
        );
        assert_eq!(
            CASS_PREFETCH_METRICS_SCHEMA_V1,
            "ee.cass_prefetch.metrics.v1"
        );
    }
}
