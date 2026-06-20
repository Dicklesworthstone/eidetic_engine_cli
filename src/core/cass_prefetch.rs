//! SRR5 (bd-16pwc) — Speculative CASS pre-fetch scaffold.
//!
//! This module ships the v1 surface for SRR5's speculative CASS pre-fetch.
//! The companion per-agent adaptive scheduling model lives in
//! `core::adaptive_scheduler`; optional-daemon wiring still lands in follow-up
//! slices. This file owns:
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
//!      reaching into the predictor's internal state. Daemon callers
//!      that serve multiple workspaces must store these counters
//!      through [`CassPrefetchWorkspaceMetrics`], keyed by a stable,
//!      redaction-safe workspace id, so one workspace switch cannot
//!      leak hit-rate posture into the next workspace's rollup.
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
//!   - Daemon application of `core::adaptive_scheduler` decisions and live
//!     `adaptive_backoff_applied` degraded-code emission.
//!   - Swarm brief adaptive reporting; that's a CLI surface slice that consumes
//!     [`CassPrefetchMetrics`] and scheduler decisions.
//!   - Budget-exceeded `cass_prefetch_budget_exceeded` degraded code
//!     emission. The module exposes
//!     [`CassPrefetchMetrics::record_budget_exceeded`] so the daemon slice can
//!     wire it without changing this file.
//!
//! Determinism contract (load-bearing): the predictor must be a pure
//! function of its input history. No time-of-day, no RNG, no env, no
//! workspace I/O. Same input → byte-identical output. This is what
//! lets the v1 ship without affecting the existing pack-hash
//! determinism gate — the speculative pre-fetch is a CACHE
//! population, never a retrieval-policy mutation.
//!
//! Agent-isolation contract (bd-298n0): every [`CassPrefetchHistory`] carries
//! an [`AgentScope`]. The predictor intentionally ignores the scope while
//! scoring topics, but the history value itself cannot be constructed without
//! naming an owner, so daemon wiring cannot flatten all agents into one shared
//! rolling accumulator by accident.
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
//!
//! Resource-amplification bound (bd-1suaa): `predict_next_n` is `O(N)` in
//! history length, and a daemon dispatch method that hydrates a
//! [`CassPrefetchHistory`] from a 4 MiB request envelope would otherwise
//! let an attacker drive that loop with `N ≈ 10^5`. [`MAX_PREFETCH_HISTORY`]
//! (64) and [`MAX_PREFETCH_TOPIC_ID_BYTES`] (256) cap the work at a
//! constant: [`SpeculativePrefetch::predict_next_n_gated`] refuses an
//! out-of-bounds history with [`CASS_PREFETCH_HISTORY_OVERSIZED_CODE`]
//! before doing any work, [`CassPrefetchHistory::try_from_topics`] enforces
//! the bounds by construction, and the default predictor declines an
//! oversized history in `O(1)` even on a direct call.
//!
//! Redaction contract (bd-3aczq): topic ids are derived from agent query
//! text, which can contain secrets. [`TopicId`] routes every value
//! through `policy::redact_secret_like_content` at construction AND at
//! deserialize time, so a secret-like fragment can never reach the
//! predictor accumulator, the returned [`CassPrefetchCandidate`] vector,
//! or the `--explain` decision blob — and a daemon hydrating a history
//! from untrusted request params cannot smuggle one through `serde`.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::models::CorpusRevision;

/// Schema id for the per-call prefetch decision blob emitted under
/// `--explain` (a follow-up CLI slice surfaces it; the constant is
/// pinned here so the schema + producer share a single source of
/// truth).
pub const CASS_PREFETCH_DECISION_SCHEMA_V1: &str = "ee.cass_prefetch.decision.v1";

/// Schema id for the prefetch metrics rollup the daemon slice will
/// surface through `ee swarm brief --include-adaptive --json` and the
/// flight recorder.
pub const CASS_PREFETCH_METRICS_SCHEMA_V1: &str = "ee.cass_prefetch.metrics.v1";

/// Degraded code emitted when a speculative prefetch slot exceeds its
/// configured soft budget and is cancelled without changing retrieval
/// results (bd-16pwc.2). The daemon-wiring slice owns emission.
pub const CASS_PREFETCH_BUDGET_EXCEEDED_CODE: &str = "cass_prefetch_budget_exceeded";

/// Degraded code emitted when the adaptive scheduler applies a
/// per-agent noisy-neighbor soft backoff (bd-16pwc.2). The scheduler
/// slice owns emission.
pub const ADAPTIVE_BACKOFF_APPLIED_CODE: &str = "adaptive_backoff_applied";

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

/// Hard upper bound on the number of observations a prefetch history may
/// carry before the predictor refuses it (bd-1suaa). The default rolling
/// window is [`DEFAULT_PREFETCH_HISTORY_WINDOW`] (10); 64 is generous
/// headroom while still capping the work at a constant. `predict_next_n`
/// is `O(N)` in history length (a recency weight + a bounded map lookup
/// per observation), so an unbounded `N` is a CPU/RAM amplification
/// vector the instant the daemon wires a dispatch method that
/// deserializes a `CassPrefetchHistory` from a (4 MiB) request envelope.
pub const MAX_PREFETCH_HISTORY: usize = 64;

/// Hard upper bound on the byte length of a single observation's
/// `topic_id` (bd-1suaa). ULID/UUID-shaped IDs are well under this; a
/// multi-KiB `topic_id` is an amplification attempt (the accumulator
/// clones each id into a `HashMap`, doubling RAM cost, and the final
/// sort compares them with `O(len)` `String::cmp`).
pub const MAX_PREFETCH_TOPIC_ID_BYTES: usize = 256;

/// Degraded code emitted when a prefetch history is refused for
/// exceeding [`MAX_PREFETCH_HISTORY`] observations or carrying a
/// `topic_id` longer than [`MAX_PREFETCH_TOPIC_ID_BYTES`] (bd-1suaa).
/// Surfacing it on the `--explain` `degraded[]` array (and landing its
/// failure-mode fixture + taxonomy row) is deferred to the daemon-wiring
/// slice, same as `cass_prefetch_budget_exceeded`.
pub const CASS_PREFETCH_HISTORY_OVERSIZED_CODE: &str = "cass_prefetch_history_oversized";

/// Degraded code emitted when a generation-gated prediction is skipped
/// because the history's [`PrefetchGeneration`] no longer matches the
/// live gate — i.e. a reindex (index_generation bump) or workspace
/// switch (workspace_generation bump) invalidated the topic
/// distribution the history was built against (bd-qud3c). Surfacing it
/// on the `--explain` `degraded[]` array (and landing its failure-mode
/// fixture + taxonomy row) is deferred to the daemon-wiring slice, same
/// as `cass_prefetch_budget_exceeded`.
pub const CASS_PREFETCH_STALE_GENERATION_CODE: &str = "cass_prefetch_stale_generation";

/// Degraded code emitted when a revision-gated prediction is skipped because
/// the history's corpus stamp no longer matches the live lexical/index corpus
/// revision (bd-1eh60). This is more specific than a generation mismatch: a
/// caller can bump revision identity when the underlying segment set changes
/// even if its coarse numeric generation source has not been wired yet.
pub const CASS_PREFETCH_STALE_CORPUS_REVISION_CODE: &str = "cass_prefetch_stale_corpus_revision";

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

/// A prefetch topic identifier with a load-bearing redaction invariant
/// (bd-3aczq).
///
/// The module documents `topic_id` as derived from the agent's query
/// string. Nothing previously stopped a raw query fragment — e.g.
/// `ee context "deploy with AWS_SECRET_ACCESS_KEY=..."` — from flowing
/// verbatim into the predictor accumulator, the returned candidate
/// vector, and the `--explain` decision blob (`CASS_PREFETCH_DECISION_SCHEMA_V1`).
/// `TopicId` closes that leak by routing EVERY value through
/// `policy::redact_secret_like_content` — the same scanner the rest of
/// `ee` uses (`src/cass/import.rs`, `src/policy/mod.rs`) — at BOTH
/// construction and deserialize time. A daemon hydrating a history from
/// untrusted request `params` therefore cannot smuggle an unredacted
/// secret through `serde`, and no caller can construct an unredacted
/// observation. Redaction is a pure string transform, so the
/// determinism contract (bd-kpynd) and `Hash`/`Ord` keying are
/// preserved.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct TopicId(String);

impl TopicId {
    /// Construct a redacted topic id. Any secret-like substring (API
    /// keys, URL passwords, PEM blocks, JWTs, high-entropy values, PII)
    /// is replaced with a `[REDACTED:<scanner>]` placeholder before the
    /// value is stored — there is no way to hold the raw form.
    #[must_use]
    pub fn new(raw: impl AsRef<str>) -> Self {
        Self(crate::policy::redact_secret_like_content(raw.as_ref()).content)
    }

    /// Borrow the redacted id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True iff the redacted id is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Consume into the owned redacted `String`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl From<&str> for TopicId {
    fn from(raw: &str) -> Self {
        Self::new(raw)
    }
}

impl From<String> for TopicId {
    fn from(raw: String) -> Self {
        Self::new(raw)
    }
}

impl From<TopicId> for String {
    fn from(topic_id: TopicId) -> Self {
        topic_id.0
    }
}

impl std::fmt::Display for TopicId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl PartialEq<str> for TopicId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for TopicId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// One observed prior ee context call in the per-agent rolling
/// history window. The predictor sees these and nothing else — no
/// memory IDs, no raw query text, no clock — so the result is a pure
/// function of the topic sequence.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassPrefetchObservation {
    /// Normalized, ALWAYS-redacted topic identifier (bd-3aczq).
    /// Production callers derive this from the query string + workspace
    /// task-template mapping; storing it as a [`TopicId`] guarantees a
    /// secret-like fragment is masked before it reaches the predictor.
    pub topic_id: TopicId,
    /// Opaque corpus/index revision this observation was measured against
    /// (bd-1eh60). Legacy observations default to `unknown`; revision-aware
    /// gates treat that as stale once a live revision is available, so an old
    /// history cannot silently warm entries for a regenerated index.
    #[serde(default)]
    pub corpus_revision: CorpusRevision,
}

impl CassPrefetchObservation {
    #[must_use]
    pub fn new(topic_id: impl Into<String>) -> Self {
        Self {
            topic_id: TopicId::from(topic_id.into()),
            corpus_revision: CorpusRevision::unknown(),
        }
    }

    #[must_use]
    pub fn with_corpus_revision(mut self, corpus_revision: CorpusRevision) -> Self {
        self.corpus_revision = corpus_revision;
        self
    }
}

/// Opaque owner of a CASS prefetch history (bd-298n0).
///
/// The daemon-side wiring keeps one rolling history per agent. Carrying the
/// scope on the history itself makes that isolation invariant type-visible:
/// a caller cannot build a history without naming the owning agent, and two
/// histories with identical topics but different owners are distinct values.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct AgentScope(String);

impl AgentScope {
    /// Sentinel used when a caller supplies an empty or whitespace-only owner.
    pub const UNKNOWN: &'static str = "agent:unknown";

    /// Construct an agent scope from the stable agent identity used by the
    /// caller. The value is not interpreted by the predictor; it exists to keep
    /// the per-agent ownership boundary attached to the history. Blank values
    /// canonicalize to [`Self::UNKNOWN`] so a malformed caller cannot create an
    /// empty owner that collapses the isolation boundary invisibly.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            Self::unknown()
        } else {
            Self(trimmed.to_owned())
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn unknown() -> Self {
        Self(Self::UNKNOWN.to_owned())
    }

    #[must_use]
    pub fn is_unknown(&self) -> bool {
        self.0 == Self::UNKNOWN
    }
}

impl From<&str> for AgentScope {
    fn from(raw: &str) -> Self {
        Self::new(raw)
    }
}

impl From<String> for AgentScope {
    fn from(raw: String) -> Self {
        Self::new(raw)
    }
}

impl From<AgentScope> for String {
    fn from(scope: AgentScope) -> Self {
        scope.0
    }
}

impl std::fmt::Display for AgentScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl PartialEq<str> for AgentScope {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for AgentScope {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Per-agent rolling history of recent ee context queries. Position 0
/// is the most recent call; position `len()-1` is the oldest in the
/// retained window.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassPrefetchHistory {
    /// Stable owner of this rolling history. It is deliberately not used in the
    /// scoring loop; it exists so daemon wiring cannot accidentally flatten all
    /// agents into one shared accumulator.
    pub agent_scope: AgentScope,
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
    pub fn new(
        agent_scope: impl Into<AgentScope>,
        recent_first: Vec<CassPrefetchObservation>,
    ) -> Self {
        Self {
            agent_scope: agent_scope.into(),
            generation: PrefetchGeneration::default(),
            recent_first,
        }
    }

    /// Construct a history from an iterator of topic IDs in
    /// most-recent-first order. Helper for the tests + the daemon's
    /// rolling-window adapter.
    #[must_use]
    pub fn from_topics<I, S>(agent_scope: impl Into<AgentScope>, recent_first_topics: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            agent_scope: agent_scope.into(),
            generation: PrefetchGeneration::default(),
            recent_first: recent_first_topics
                .into_iter()
                .map(CassPrefetchObservation::new)
                .collect(),
        }
    }

    /// Fallible constructor that enforces the bd-1suaa admission bounds
    /// BY CONSTRUCTION: returns `None` if more than
    /// [`MAX_PREFETCH_HISTORY`] topics are supplied, or if any `topic_id`
    /// exceeds [`MAX_PREFETCH_TOPIC_ID_BYTES`] bytes. Trusted in-process
    /// callers may still use [`from_topics`]; the daemon dispatch slice
    /// that hydrates a history from UNTRUSTED request params MUST use
    /// this (or check the bounds itself) so attacker-controlled input
    /// cannot drive the `O(N)` predictor with an unbounded `N`.
    #[must_use]
    pub fn try_from_topics<I, S>(
        agent_scope: impl Into<AgentScope>,
        recent_first_topics: I,
    ) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut recent_first = Vec::new();
        for topic in recent_first_topics {
            if recent_first.len() >= MAX_PREFETCH_HISTORY {
                return None;
            }
            let raw_topic = topic.into();
            if raw_topic.len() > MAX_PREFETCH_TOPIC_ID_BYTES {
                return None;
            }
            let observation = CassPrefetchObservation::new(raw_topic);
            if observation.topic_id.as_str().len() > MAX_PREFETCH_TOPIC_ID_BYTES {
                return None;
            }
            recent_first.push(observation);
        }
        Some(Self {
            agent_scope: agent_scope.into(),
            generation: PrefetchGeneration::default(),
            recent_first,
        })
    }

    /// Stamp the history with the generation it was built against
    /// (bd-qud3c). Builder-style so callers can chain it onto
    /// [`from_topics`]/[`new`]: `CassPrefetchHistory::from_topics(scope, ..)
    /// .with_generation(PrefetchGeneration::new(ws, idx))`.
    #[must_use]
    pub fn with_generation(mut self, generation: PrefetchGeneration) -> Self {
        self.generation = generation;
        self
    }

    /// Stamp every retained observation with the corpus/index revision it was
    /// measured against (bd-1eh60). Builder-style to pair naturally with
    /// [`with_generation`](Self::with_generation).
    #[must_use]
    pub fn with_corpus_revision(mut self, corpus_revision: CorpusRevision) -> Self {
        for observation in &mut self.recent_first {
            observation.corpus_revision = corpus_revision.clone();
        }
        self
    }

    /// True iff every non-empty observation was measured against the live
    /// corpus revision. `unknown` is deliberately incoherent for non-empty
    /// histories so legacy serialized observations cannot mask index
    /// regeneration staleness.
    #[must_use]
    pub fn corpus_revision_is_coherent_with(&self, current: &CorpusRevision) -> bool {
        self.recent_first.is_empty()
            || self
                .recent_first
                .iter()
                .all(|observation| observation.corpus_revision.is_coherent_with(current))
    }

    /// True iff this history is within the bd-1suaa admission bounds:
    /// at most [`MAX_PREFETCH_HISTORY`] observations, each with a
    /// `topic_id` no longer than [`MAX_PREFETCH_TOPIC_ID_BYTES`] bytes.
    /// The length predicate is checked first and short-circuits the
    /// `topic_id` scan, so an oversized-count history is rejected in
    /// `O(1)` without walking attacker-controlled observations.
    #[must_use]
    pub fn is_within_admission_bounds(&self) -> bool {
        self.recent_first.len() <= MAX_PREFETCH_HISTORY
            && self.recent_first.iter().all(|observation| {
                observation.topic_id.as_str().len() <= MAX_PREFETCH_TOPIC_ID_BYTES
            })
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

/// Per-(agent, workspace) store of rolling CASS prefetch histories that the
/// daemon coordinator (bd-16pwc.4) accumulates from observed context requests.
///
/// Each `(AgentScope, workspace)` pair owns an isolated [`CassPrefetchHistory`]:
/// the [`AgentScope`] newtype (bd-298n0) already makes cross-agent flattening
/// unrepresentable, and keying additionally on the workspace keeps one agent's
/// two workspaces apart. Observations are retained most-recent-first and trimmed
/// to a fixed rolling window (clamped to `[1, MAX_PREFETCH_HISTORY]`) so the
/// `O(N)` predictor always sees a bounded, in-admission-bounds history. The
/// store is pure and deterministic; the daemon thread wiring (post-context
/// hook, idle-budget scheduling, shutdown cancellation) lives in the dispatch
/// slice, not here.
#[derive(Clone, Debug)]
pub struct CassPrefetchHistoryStore {
    window: usize,
    histories: BTreeMap<(AgentScope, String), CassPrefetchHistory>,
}

impl CassPrefetchHistoryStore {
    /// Create a store whose per-key rolling window is `window`, clamped to
    /// `[1, MAX_PREFETCH_HISTORY]` so a caller cannot disable trimming or drive
    /// the `O(N)` predictor past the bd-1suaa admission bound.
    #[must_use]
    pub fn new(window: usize) -> Self {
        Self {
            window: window.clamp(1, MAX_PREFETCH_HISTORY),
            histories: BTreeMap::new(),
        }
    }

    /// Record one observed `topic` for `(agent_scope, workspace)`, stamping the
    /// `generation` it was measured against on the history and `corpus_revision`
    /// on the observation, then trim to the most-recent `window` entries. The
    /// history is created on first observation; two distinct scopes or
    /// workspaces never share an accumulator (bd-298n0). `topic` is redacted at
    /// [`TopicId`] construction (bd-3aczq), so a secret-shaped fragment cannot
    /// enter the accumulator.
    pub fn observe(
        &mut self,
        agent_scope: impl Into<AgentScope>,
        workspace: impl Into<String>,
        topic: impl Into<String>,
        generation: PrefetchGeneration,
        corpus_revision: &CorpusRevision,
    ) {
        let scope = agent_scope.into();
        let entry = self
            .histories
            .entry((scope.clone(), workspace.into()))
            .or_insert_with(|| CassPrefetchHistory::new(scope, Vec::new()));
        // Most-recent-first: the newest observation goes to the front, stamped
        // with the live corpus revision so the revision gate can later reject a
        // trail measured against a since-regenerated index.
        entry.recent_first.insert(
            0,
            CassPrefetchObservation::new(topic).with_corpus_revision(corpus_revision.clone()),
        );
        entry.recent_first.truncate(self.window);
        entry.generation = generation;
    }

    /// Borrow the rolling history for `(agent_scope, workspace)`, or `None` if
    /// nothing has been observed for that pair yet.
    #[must_use]
    pub fn history_for(
        &self,
        agent_scope: &AgentScope,
        workspace: &str,
    ) -> Option<&CassPrefetchHistory> {
        self.histories
            .get(&(agent_scope.clone(), workspace.to_owned()))
    }

    /// Number of distinct `(agent, workspace)` histories retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.histories.len()
    }

    /// True when no observations have been recorded for any pair.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.histories.is_empty()
    }
}

/// A single predicted candidate the speculative pre-fetcher emits for
/// idle-slot warming.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassPrefetchCandidate {
    /// Topic the daemon should pre-stage CASS evidence for. Always a
    /// redacted [`TopicId`] (bd-3aczq), so the candidate vector and any
    /// `--explain` blob that embeds it carry no secret-like fragment.
    pub topic_id: TopicId,
    /// Predictor-internal score in `[0.0, 1.0]`. Higher is more
    /// confident. Pinned to a finite, non-negative number by
    /// construction; predictors that compute non-finite intermediates
    /// must filter before emitting.
    #[serde(
        deserialize_with = "deserialize_candidate_score",
        serialize_with = "serialize_candidate_score"
    )]
    pub score: f64,
    /// Predictor identifier for audit / explain blobs. Defaults to
    /// the predictor's `name()`; tests can override.
    #[serde(deserialize_with = "deserialize_predictor_cow")]
    pub predictor: Cow<'static, str>,
}

impl CassPrefetchCandidate {
    #[must_use]
    pub fn new(
        topic_id: impl Into<TopicId>,
        score: f64,
        predictor: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            topic_id: topic_id.into(),
            score: normalize_candidate_score(score),
            predictor: predictor.into(),
        }
    }
}

fn normalize_candidate_score(score: f64) -> f64 {
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn deserialize_candidate_score<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let score = f64::deserialize(deserializer)?;
    Ok(normalize_candidate_score(score))
}

fn serialize_candidate_score<S>(score: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_f64(normalize_candidate_score(*score))
}

fn deserialize_predictor_cow<'de, D>(deserializer: D) -> Result<Cow<'static, str>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Cow::Owned)
}

/// Result of a generation-gated prediction
/// ([`SpeculativePrefetch::predict_next_n_gated`], bd-qud3c).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GatedPrediction {
    /// Candidates to warm. Empty when `degraded` is set.
    pub candidates: Vec<CassPrefetchCandidate>,
    /// `Some(...)` when prediction was dropped because the history was
    /// oversized, stale by generation, or stale by corpus revision; `None` on
    /// a coherent prediction.
    pub degraded: Option<&'static str>,
}

/// Predictor trait. Implementations are pure functions of the input
/// history — see the module-level determinism contract.
///
/// Implementer contract (bd-1suaa): `predict_next_n` is `O(N)` in
/// history length, so a daemon dispatch site that hydrates a
/// [`CassPrefetchHistory`] from untrusted request params MUST cap
/// `recent_first.len()` (and reject oversized `topic_id`s) BEFORE
/// calling the predictor — either by constructing the history through
/// [`CassPrefetchHistory::try_from_topics`] or by routing through
/// [`predict_next_n_gated`], which refuses an out-of-bounds history with
/// [`CASS_PREFETCH_HISTORY_OVERSIZED_CODE`] without doing `O(N)` work.
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
        // Resource-amplification admission FIRST (bd-1suaa): refuse an
        // oversized history in O(1)/O(N<=cap) before any predictor work,
        // so an attacker-controlled length cannot drive the O(N) loop.
        if !history.is_within_admission_bounds() {
            return GatedPrediction {
                candidates: Vec::new(),
                degraded: Some(CASS_PREFETCH_HISTORY_OVERSIZED_CODE),
            };
        }
        // Cache-coherence gate (bd-qud3c): drop a stale-generation history.
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

    /// Revision-aware generation-gated prediction (bd-1eh60). In addition to
    /// the coarse [`PrefetchGeneration`] gate, every observation in the history
    /// must carry the same non-unknown [`CorpusRevision`] as the live lexical
    /// corpus. This keeps a regenerated index from accepting a hot trail that
    /// was measured against a different segment set.
    fn predict_next_n_gated_for_revision(
        &self,
        history: &CassPrefetchHistory,
        current_generation: PrefetchGeneration,
        current_corpus_revision: &CorpusRevision,
        top_k: usize,
    ) -> GatedPrediction {
        if !history.is_within_admission_bounds() {
            return GatedPrediction {
                candidates: Vec::new(),
                degraded: Some(CASS_PREFETCH_HISTORY_OVERSIZED_CODE),
            };
        }
        if !history.generation.is_coherent_with(current_generation) {
            return GatedPrediction {
                candidates: Vec::new(),
                degraded: Some(CASS_PREFETCH_STALE_GENERATION_CODE),
            };
        }
        if !history.corpus_revision_is_coherent_with(current_corpus_revision) {
            return GatedPrediction {
                candidates: Vec::new(),
                degraded: Some(CASS_PREFETCH_STALE_CORPUS_REVISION_CODE),
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
#[derive(Clone, Debug)]
pub struct RecencyWeightedFrequencyPredictor {
    half_life: f64,
    min_score: f64,
}

impl Default for RecencyWeightedFrequencyPredictor {
    fn default() -> Self {
        Self::new()
    }
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
        // Defense in depth (bd-1suaa, bd-3mhyr): an oversized history is
        // refused here too, so a DIRECT (ungated) caller cannot drive the
        // O(N) loop below with an attacker-controlled length, and cannot
        // surface an over-cap `topic_id` as a candidate. The admission
        // check short-circuits in O(1) on the count cap and bounds the
        // per-observation `topic_id` byte scan at MAX_PREFETCH_HISTORY, so
        // the work stays bounded regardless of input. The gated entry point
        // reports CASS_PREFETCH_HISTORY_OVERSIZED_CODE; the bare trait
        // method has no degraded channel, so it just declines.
        if top_k == 0 || history.is_empty() || !history.is_within_admission_bounds() {
            return Vec::new();
        }

        // Skip the most-recent topic as a candidate (predicting an
        // immediate repeat provides no prefetch value). All other
        // topics in the rolling window contribute their recency
        // weight.
        let most_recent_topic = history.recent_first.iter().find_map(|observation| {
            let topic = observation.topic_id.as_str();
            (!topic.is_empty()).then_some(topic)
        });

        // Bounded accumulator: capacity is capped at MAX_PREFETCH_HISTORY
        // (the count is already <= the cap by the guard above), so the
        // hash-table grow cost cannot scale with attacker input.
        let mut accumulator: HashMap<TopicId, f64> =
            HashMap::with_capacity(history.recent_first.len().min(MAX_PREFETCH_HISTORY));
        let mut candidate_total_weight: f64 = 0.0;
        for (position, observation) in history.recent_first.iter().enumerate() {
            let topic_id = observation.topic_id.as_str();
            if topic_id.is_empty() {
                continue;
            }
            let weight = self.recency_weight(position);
            if !weight.is_finite() || weight < 0.0 {
                continue;
            }
            if Some(topic_id) == most_recent_topic {
                continue;
            }
            candidate_total_weight += weight;
            if let Some(existing_weight) = accumulator.get_mut(&observation.topic_id) {
                *existing_weight += weight;
            } else {
                accumulator.insert(observation.topic_id.clone(), weight);
            }
        }

        if candidate_total_weight <= 0.0 || !candidate_total_weight.is_finite() {
            return Vec::new();
        }

        // Normalize so the score is bounded in [0.0, 1.0] regardless
        // of how full the rolling window is. The normalization also
        // makes the admission threshold (min_score) workspace-
        // independent.
        let mut scored: Vec<CassPrefetchCandidate> = accumulator
            .into_iter()
            .map(|(topic_id, weighted_sum)| {
                let normalized = (weighted_sum / candidate_total_weight).clamp(0.0, 1.0);
                CassPrefetchCandidate::new(topic_id, normalized, self.name())
            })
            .filter(|candidate| {
                candidate.score.is_finite()
                    && candidate.score >= self.min_score
                    && candidate.score >= 0.0
            })
            .collect();

        sort_prefetch_candidates_deterministically(&mut scored);

        scored.truncate(top_k);
        scored
    }
}

fn sort_prefetch_candidates_deterministically(scored: &mut [CassPrefetchCandidate]) {
    // Deterministic order: highest score first, lexicographic topic_id
    // tie-break. `total_cmp` keeps ordering total even if a future caller
    // bypasses the normal finite-score filter.
    scored.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.topic_id.cmp(&right.topic_id))
    });
}

/// Hit / miss / budget counter the daemon and the flight recorder
/// consume. The struct is intentionally simple — `u64` saturating
/// adds, no `Atomic*` — because the daemon owns the call site and
/// can wrap it in whatever synchronization it needs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassPrefetchMetrics {
    /// Opaque corpus/index revision the counters were measured against
    /// (bd-1eh60). A daemon serving regenerated indexes can reset or rotate
    /// metrics by revision instead of carrying hit-rate posture across
    /// incompatible segment sets.
    #[serde(default)]
    pub measured_against_revision: CorpusRevision,
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
    /// Predictions refused because the history exceeded the bd-1suaa
    /// admission bounds (`> MAX_PREFETCH_HISTORY` observations, or a
    /// `topic_id > MAX_PREFETCH_TOPIC_ID_BYTES`). A nonzero value on a
    /// daemon-wired host is an amplification-probe signal.
    #[serde(default)]
    pub history_oversized: u64,
    /// Predictions refused because the history's observations carried a
    /// [`CorpusRevision`] that no longer matched the live lexical corpus
    /// (bd-1eh60 revision gate). Distinct from `stale_generation_drop`:
    /// this counts a coherent-generation history whose evidence was
    /// measured against a since-regenerated segment set, the invalidation
    /// the daemon-wiring slice (bd-16pwc.4) observes when only the corpus
    /// revision — not the coarse `(workspace, index)` generation — moved.
    /// `#[serde(default)]` keeps metrics serialized before this counter
    /// decodable.
    #[serde(default)]
    pub stale_corpus_revision_drop: u64,
}

impl CassPrefetchMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self {
            measured_against_revision: CorpusRevision::unknown(),
            hits: 0,
            misses: 0,
            candidates_emitted: 0,
            budget_exceeded: 0,
            history_too_short: 0,
            stale_generation_drop: 0,
            history_oversized: 0,
            stale_corpus_revision_drop: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
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

    /// Record a prediction refused for exceeding the admission bounds —
    /// the daemon-wiring slice calls this whenever
    /// [`SpeculativePrefetch::predict_next_n_gated`] returns
    /// [`CASS_PREFETCH_HISTORY_OVERSIZED_CODE`] (bd-1suaa).
    pub fn record_history_oversized(&mut self) {
        self.history_oversized = self.history_oversized.saturating_add(1);
    }

    /// Record a prediction dropped because the history's corpus revision no
    /// longer matched the live lexical corpus — the daemon-wiring slice calls
    /// this whenever [`SpeculativePrefetch::predict_next_n_gated_for_revision`]
    /// returns [`CASS_PREFETCH_STALE_CORPUS_REVISION_CODE`] (bd-1eh60 /
    /// bd-16pwc.4).
    pub fn record_stale_corpus_revision_drop(&mut self) {
        self.stale_corpus_revision_drop = self.stale_corpus_revision_drop.saturating_add(1);
    }

    pub fn set_measured_against_revision(&mut self, revision: CorpusRevision) {
        self.measured_against_revision = revision;
    }

    /// Hit rate as a fraction in `[0.0, 1.0]`. Returns 0.0 when no
    /// hits or misses have been observed (zero-attempt case is "no
    /// data," not "0% hit rate" — but callers that want to distinguish
    /// the two should check `attempts()` directly).
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let hits = self.hits as f64;
        let misses = self.misses as f64;
        let attempts = hits + misses;
        if attempts == 0.0 || !attempts.is_finite() {
            0.0
        } else {
            (hits / attempts).clamp(0.0, 1.0)
        }
    }

    /// Total hit + miss observations.
    #[must_use]
    pub const fn attempts(&self) -> u64 {
        self.hits.saturating_add(self.misses)
    }
}

/// Caller-owned workspace boundary for CASS prefetch metrics.
///
/// [`CassPrefetchMetrics`] intentionally remains a small per-instance counter.
/// A daemon or flight recorder that serves more than one workspace must keep one
/// bucket per workspace instead of sharing a global counter. The key is a stable,
/// redaction-safe workspace id supplied by the caller; this module does not
/// derive it from paths so the pure predictor surface stays free of workspace
/// I/O.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CassPrefetchWorkspaceMetrics {
    workspaces: BTreeMap<String, CassPrefetchMetrics>,
}

impl CassPrefetchWorkspaceMetrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the mutable metrics bucket for `workspace_id`, creating it with
    /// zeroed counters when this is the first observation for the workspace.
    pub fn for_workspace_mut(
        &mut self,
        workspace_id: impl Into<String>,
    ) -> &mut CassPrefetchMetrics {
        self.workspaces.entry(workspace_id.into()).or_default()
    }

    /// Return the metrics bucket for `workspace_id` when it has been observed.
    #[must_use]
    pub fn for_workspace(&self, workspace_id: &str) -> Option<&CassPrefetchMetrics> {
        self.workspaces.get(workspace_id)
    }

    /// Reset only the named workspace bucket. Returns `true` when a bucket was
    /// present and reset, or `false` when the workspace had no metrics yet.
    pub fn reset_workspace(&mut self, workspace_id: &str) -> bool {
        if let Some(metrics) = self.workspaces.get_mut(workspace_id) {
            metrics.reset();
            true
        } else {
            false
        }
    }

    /// Deterministic snapshot view ordered by workspace id.
    #[must_use]
    pub fn snapshot(&self) -> &BTreeMap<String, CassPrefetchMetrics> {
        &self.workspaces
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.workspaces.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_AGENT_SCOPE: &str = "agent:test";

    fn test_agent_scope() -> AgentScope {
        AgentScope::new(TEST_AGENT_SCOPE)
    }

    fn history(topics_recent_first: &[&str]) -> CassPrefetchHistory {
        CassPrefetchHistory::from_topics(test_agent_scope(), topics_recent_first.iter().copied())
    }

    #[test]
    fn empty_history_predicts_nothing() {
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let h = history(&[]);
        let predictions = predictor.predict_next_n(&h, 3);
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
    fn predictions_are_deterministic_under_recency_scores() {
        // Position 1 outranks position 2 under the recency-weighted
        // heuristic. This still must be byte-identical across runs and
        // independent of HashMap iteration order.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let h = history(&["current", "zeta", "alpha"]);
        let predictions = predictor.predict_next_n(&h, 3);
        assert_eq!(predictions.len(), 2);
        assert_eq!(predictions[0].topic_id, "zeta");
        assert_eq!(predictions[1].topic_id, "alpha");
    }

    #[test]
    fn equal_scores_tie_break_by_topic_id() {
        let mut candidates = vec![
            CassPrefetchCandidate::new("zeta", 0.5, "test_predictor"),
            CassPrefetchCandidate::new("alpha", 0.5, "test_predictor"),
        ];
        sort_prefetch_candidates_deterministically(&mut candidates);
        assert_eq!(candidates[0].topic_id, "alpha");
        assert_eq!(candidates[1].topic_id, "zeta");
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
    fn default_predictor_matches_new_bd_8rdn7() {
        let h = history(&["current", "alpha", "bravo", "alpha"]);
        let from_default = RecencyWeightedFrequencyPredictor::default().predict_next_n(&h, 5);
        let from_new = RecencyWeightedFrequencyPredictor::new().predict_next_n(&h, 5);

        assert!(
            !from_default.is_empty(),
            "Default must remain a functional predictor"
        );
        assert_eq!(from_default, from_new);
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
    fn metrics_reset_clears_all_counters() {
        let mut metrics = CassPrefetchMetrics::new();
        metrics.record_hit();
        metrics.record_miss();
        metrics.record_candidate();
        metrics.record_budget_exceeded();
        metrics.record_history_too_short();
        metrics.record_stale_generation_drop();
        metrics.record_history_oversized();
        metrics.record_stale_corpus_revision_drop();

        assert_ne!(metrics, CassPrefetchMetrics::new());
        metrics.reset();
        assert_eq!(metrics, CassPrefetchMetrics::new());
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
        assert!(
            (metrics.hit_rate() - 0.5).abs() < 1e-12,
            "hit_rate should not use the saturated attempts denominator"
        );
    }

    #[test]
    fn workspace_metrics_keep_counters_isolated_bd_1brl3() {
        let mut metrics = CassPrefetchWorkspaceMetrics::new();
        assert!(metrics.is_empty());

        {
            let workspace_a = metrics.for_workspace_mut("workspace-a");
            workspace_a.record_hit();
            workspace_a.record_hit();
            workspace_a.record_candidate();
        }
        {
            let workspace_b = metrics.for_workspace_mut("workspace-b");
            workspace_b.record_miss();
            workspace_b.record_budget_exceeded();
        }

        let workspace_a = metrics
            .for_workspace("workspace-a")
            .expect("workspace-a metrics should exist");
        let workspace_b = metrics
            .for_workspace("workspace-b")
            .expect("workspace-b metrics should exist");

        assert_eq!(workspace_a.hits, 2);
        assert_eq!(workspace_a.misses, 0);
        assert_eq!(workspace_a.candidates_emitted, 1);
        assert_eq!(workspace_a.attempts(), 2);
        assert_eq!(workspace_a.hit_rate(), 1.0);

        assert_eq!(workspace_b.hits, 0);
        assert_eq!(workspace_b.misses, 1);
        assert_eq!(workspace_b.budget_exceeded, 1);
        assert_eq!(workspace_b.attempts(), 1);
        assert_eq!(workspace_b.hit_rate(), 0.0);

        let workspace_ids: Vec<&str> = metrics.snapshot().keys().map(String::as_str).collect();
        assert_eq!(workspace_ids, vec!["workspace-a", "workspace-b"]);
        assert_eq!(metrics.len(), 2);
    }

    #[test]
    fn workspace_metrics_reset_only_named_workspace_bd_1brl3() {
        let mut metrics = CassPrefetchWorkspaceMetrics::new();
        metrics.for_workspace_mut("workspace-a").record_hit();
        metrics.for_workspace_mut("workspace-a").record_candidate();
        metrics.for_workspace_mut("workspace-b").record_miss();
        metrics
            .for_workspace_mut("workspace-b")
            .record_history_too_short();

        assert!(metrics.reset_workspace("workspace-a"));
        assert!(!metrics.reset_workspace("workspace-c"));

        assert_eq!(
            metrics.for_workspace("workspace-a"),
            Some(&CassPrefetchMetrics::new())
        );

        let workspace_b = metrics
            .for_workspace("workspace-b")
            .expect("workspace-b metrics should remain");
        assert_eq!(workspace_b.misses, 1);
        assert_eq!(workspace_b.history_too_short, 1);
    }

    #[test]
    fn history_helpers_match_iteration_order() {
        let topics = ["recent", "older", "oldest"];
        let h = CassPrefetchHistory::from_topics(test_agent_scope(), topics.iter().copied());
        assert_eq!(h.agent_scope, TEST_AGENT_SCOPE);
        assert_eq!(h.len(), 3);
        assert!(!h.is_empty());
        let observed: Vec<&str> = h.iter().map(|o| o.topic_id.as_str()).collect();
        assert_eq!(observed, vec!["recent", "older", "oldest"]);
    }

    #[test]
    fn history_scope_is_part_of_identity_bd_298n0() {
        let agent_a = CassPrefetchHistory::from_topics("agent:a", ["current", "alpha"]);
        let agent_b = CassPrefetchHistory::from_topics("agent:b", ["current", "alpha"]);

        assert_eq!(agent_a.agent_scope, "agent:a");
        assert_eq!(agent_b.agent_scope, "agent:b");
        assert_eq!(agent_a.recent_first, agent_b.recent_first);
        assert_ne!(
            agent_a, agent_b,
            "identical topic windows from different agents must stay distinct"
        );
    }

    #[test]
    fn blank_agent_scope_canonicalizes_to_unknown_owner_bd_298n0() {
        assert_eq!(AgentScope::new("").as_str(), AgentScope::UNKNOWN);
        assert_eq!(AgentScope::new(" \n\t ").as_str(), AgentScope::UNKNOWN);
        assert!(AgentScope::new("").is_unknown());
        assert_eq!(AgentScope::new(" agent:a ").as_str(), "agent:a");

        let unnamed = CassPrefetchHistory::from_topics(" ", ["current", "alpha"]);
        let named = CassPrefetchHistory::from_topics("agent:a", ["current", "alpha"]);
        assert!(unnamed.agent_scope.is_unknown());
        assert_ne!(
            unnamed, named,
            "blank scopes must not serialize as an empty owner indistinguishable from a real scope"
        );

        let decoded: CassPrefetchHistory =
            serde_json::from_str(r#"{"agentScope":"   ","recentFirst":[{"topicId":"current"}]}"#)
                .expect("deserialize blank scoped history");
        assert!(decoded.agent_scope.is_unknown());
    }

    #[test]
    fn history_store_accumulates_most_recent_first_bd_16pwc_4() {
        let mut store = CassPrefetchHistoryStore::new(DEFAULT_PREFETCH_HISTORY_WINDOW);
        let scope = test_agent_scope();
        let rev = CorpusRevision::from("corpus:v1");
        let generation = PrefetchGeneration::new(3, 7);
        for topic in ["alpha", "bravo", "charlie"] {
            store.observe(scope.clone(), "ws-a", topic, generation, &rev);
        }
        let history = store.history_for(&scope, "ws-a").expect("history present");
        let topics: Vec<&str> = history.iter().map(|o| o.topic_id.as_str()).collect();
        assert_eq!(topics, vec!["charlie", "bravo", "alpha"]);
        assert_eq!(history.generation, generation);
        assert!(history.corpus_revision_is_coherent_with(&rev));
        assert!(history.is_within_admission_bounds());
    }

    #[test]
    fn history_store_trims_to_window_bd_16pwc_4() {
        let mut store = CassPrefetchHistoryStore::new(2);
        let scope = test_agent_scope();
        let rev = CorpusRevision::from("corpus:v1");
        let generation = PrefetchGeneration::new(1, 1);
        for topic in ["t1", "t2", "t3", "t4"] {
            store.observe(scope.clone(), "ws", topic, generation, &rev);
        }
        let history = store.history_for(&scope, "ws").expect("history present");
        let topics: Vec<&str> = history.iter().map(|o| o.topic_id.as_str()).collect();
        assert_eq!(topics, vec!["t4", "t3"]);
    }

    #[test]
    fn history_store_isolates_distinct_scopes_and_workspaces_bd_16pwc_4() {
        let mut store = CassPrefetchHistoryStore::new(DEFAULT_PREFETCH_HISTORY_WINDOW);
        let rev = CorpusRevision::from("corpus:v1");
        let generation = PrefetchGeneration::new(1, 1);
        let alice = AgentScope::new("alice");
        let bob = AgentScope::new("bob");
        store.observe(alice.clone(), "ws-a", "alice-topic", generation, &rev);
        store.observe(bob.clone(), "ws-a", "bob-topic", generation, &rev);
        store.observe(alice.clone(), "ws-b", "alice-other-ws", generation, &rev);
        assert_eq!(store.len(), 3);
        // Same workspace, different agents: no shared accumulator (bd-298n0).
        let alice_a: Vec<&str> = store
            .history_for(&alice, "ws-a")
            .unwrap()
            .iter()
            .map(|o| o.topic_id.as_str())
            .collect();
        assert_eq!(alice_a, vec!["alice-topic"]);
        let bob_a: Vec<&str> = store
            .history_for(&bob, "ws-a")
            .unwrap()
            .iter()
            .map(|o| o.topic_id.as_str())
            .collect();
        assert_eq!(bob_a, vec!["bob-topic"]);
        // Same agent, different workspace: also isolated.
        let alice_b: Vec<&str> = store
            .history_for(&alice, "ws-b")
            .unwrap()
            .iter()
            .map(|o| o.topic_id.as_str())
            .collect();
        assert_eq!(alice_b, vec!["alice-other-ws"]);
    }

    #[test]
    fn history_store_window_clamps_to_admission_bound_bd_16pwc_4() {
        // An oversized window is clamped so the predictor never sees a history
        // past the bd-1suaa admission bound, even after many observations.
        let mut store = CassPrefetchHistoryStore::new(MAX_PREFETCH_HISTORY + 100);
        let scope = test_agent_scope();
        let rev = CorpusRevision::from("corpus:v1");
        let generation = PrefetchGeneration::new(1, 1);
        for n in 0..(MAX_PREFETCH_HISTORY + 50) {
            store.observe(scope.clone(), "ws", format!("topic-{n}"), generation, &rev);
        }
        let history = store.history_for(&scope, "ws").expect("history present");
        assert!(history.len() <= MAX_PREFETCH_HISTORY);
        assert!(history.is_within_admission_bounds());
    }

    #[test]
    fn history_store_missing_pair_returns_none_bd_16pwc_4() {
        let store = CassPrefetchHistoryStore::new(DEFAULT_PREFETCH_HISTORY_WINDOW);
        assert!(store.is_empty());
        assert!(store.history_for(&test_agent_scope(), "nope").is_none());
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
            CassPrefetchHistory::from_topics(test_agent_scope(), ["a", "b"]).generation,
            PrefetchGeneration::new(0, 0)
        );
        assert_eq!(
            CassPrefetchHistory::new(test_agent_scope(), vec![CassPrefetchObservation::new("a")])
                .generation,
            PrefetchGeneration::default()
        );
        // with_generation stamps without disturbing the observations.
        let stamped = CassPrefetchHistory::from_topics(test_agent_scope(), ["a", "b"])
            .with_generation(PrefetchGeneration::new(1, 9));
        assert_eq!(stamped.generation, PrefetchGeneration::new(1, 9));
        assert_eq!(stamped.len(), 2);
    }

    #[test]
    fn history_corpus_revision_defaults_unknown_and_builder_stamps_bd_1eh60() {
        let legacy = CassPrefetchHistory::from_topics(test_agent_scope(), ["a", "b"]);
        assert!(
            legacy
                .recent_first
                .iter()
                .all(|observation| observation.corpus_revision.is_unknown())
        );
        assert!(!legacy.corpus_revision_is_coherent_with(&CorpusRevision::from("corpus:v1")));

        let stamped = legacy.with_corpus_revision(CorpusRevision::from("corpus:v1"));
        assert!(stamped.recent_first.iter().all(|observation| {
            observation
                .corpus_revision
                .is_coherent_with(&CorpusRevision::from("corpus:v1"))
        }));
        assert!(stamped.corpus_revision_is_coherent_with(&CorpusRevision::from("corpus:v1")));
        assert!(!stamped.corpus_revision_is_coherent_with(&CorpusRevision::from("corpus:v2")));
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
    fn revision_gated_prediction_drops_stale_corpus_revision_bd_1eh60() {
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let current_generation = PrefetchGeneration::new(1, 5);
        let revision_v1 = CorpusRevision::from("corpus:v1");
        let revision_v2 = CorpusRevision::from("corpus:v2");
        let history = history(&["current", "alpha", "alpha", "bravo"])
            .with_generation(current_generation)
            .with_corpus_revision(revision_v1.clone());

        let stale = predictor.predict_next_n_gated_for_revision(
            &history,
            current_generation,
            &revision_v2,
            3,
        );
        assert!(stale.candidates.is_empty());
        assert_eq!(
            stale.degraded,
            Some(CASS_PREFETCH_STALE_CORPUS_REVISION_CODE)
        );

        let fresh = predictor.predict_next_n_gated_for_revision(
            &history,
            current_generation,
            &revision_v1,
            3,
        );
        assert_eq!(fresh.degraded, None);
        assert_eq!(fresh.candidates, predictor.predict_next_n(&history, 3));
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
    fn metrics_record_stale_corpus_revision_drop_bd_16pwc_4() {
        let mut metrics = CassPrefetchMetrics::new();
        assert_eq!(metrics.stale_corpus_revision_drop, 0);
        metrics.record_stale_corpus_revision_drop();
        metrics.record_stale_corpus_revision_drop();
        assert_eq!(metrics.stale_corpus_revision_drop, 2);
        // Distinct counter from the coarse generation gate: a corpus-revision
        // drop must not bump stale_generation_drop.
        assert_eq!(metrics.stale_generation_drop, 0);
        // Saturates rather than panicking.
        metrics.stale_corpus_revision_drop = u64::MAX;
        metrics.record_stale_corpus_revision_drop();
        assert_eq!(metrics.stale_corpus_revision_drop, u64::MAX);
    }

    #[test]
    fn metrics_record_measured_against_revision_bd_1eh60() {
        let mut metrics = CassPrefetchMetrics::new();
        assert!(metrics.measured_against_revision.is_unknown());

        metrics.set_measured_against_revision(CorpusRevision::from("corpus:v1"));
        assert_eq!(metrics.measured_against_revision.as_str(), "corpus:v1");

        metrics.record_hit();
        assert_ne!(metrics, CassPrefetchMetrics::new());
        metrics.reset();
        assert_eq!(metrics, CassPrefetchMetrics::new());
    }

    #[test]
    fn oversized_history_is_refused_in_constant_time_bd_1suaa() {
        // A history far beyond MAX_PREFETCH_HISTORY must be refused: the
        // gated method returns the oversized degraded code, and the bare
        // predictor declines (empty) — neither does the O(N) loop.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let mut topics: Vec<String> = Vec::with_capacity(MAX_PREFETCH_HISTORY + 50);
        topics.push("current".to_owned());
        for i in 0..(MAX_PREFETCH_HISTORY + 49) {
            topics.push(format!("topic_{i}"));
        }
        let oversized = CassPrefetchHistory::from_topics(test_agent_scope(), topics);
        assert!(oversized.recent_first.len() > MAX_PREFETCH_HISTORY);
        assert!(!oversized.is_within_admission_bounds());

        assert!(predictor.predict_next_n(&oversized, 3).is_empty());
        let gated = predictor.predict_next_n_gated(&oversized, PrefetchGeneration::default(), 3);
        assert!(gated.candidates.is_empty());
        assert_eq!(gated.degraded, Some(CASS_PREFETCH_HISTORY_OVERSIZED_CODE));
    }

    #[test]
    fn at_bound_history_is_admitted_bd_1suaa() {
        // Exactly MAX_PREFETCH_HISTORY observations is within bounds and
        // still yields a capped, non-empty candidate set.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let mut topics: Vec<String> = Vec::with_capacity(MAX_PREFETCH_HISTORY);
        topics.push("current".to_owned());
        // Repeat one topic so a candidate clears the min_score gate.
        for _ in 1..MAX_PREFETCH_HISTORY {
            topics.push("alpha".to_owned());
        }
        let at_bound = CassPrefetchHistory::from_topics(test_agent_scope(), topics);
        assert_eq!(at_bound.recent_first.len(), MAX_PREFETCH_HISTORY);
        assert!(at_bound.is_within_admission_bounds());
        let gated = predictor.predict_next_n_gated(&at_bound, PrefetchGeneration::default(), 3);
        assert_eq!(gated.degraded, None);
        assert!(!gated.candidates.is_empty());
        assert!(gated.candidates.len() <= 3);
    }

    #[test]
    fn oversized_topic_id_is_refused_bd_1suaa() {
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let huge_topic = "x".repeat(MAX_PREFETCH_TOPIC_ID_BYTES + 1);
        let history = CassPrefetchHistory::from_topics(
            test_agent_scope(),
            ["current".to_owned(), huge_topic],
        );
        assert!(!history.is_within_admission_bounds());
        let gated = predictor.predict_next_n_gated(&history, PrefetchGeneration::default(), 3);
        assert!(gated.candidates.is_empty());
        assert_eq!(gated.degraded, Some(CASS_PREFETCH_HISTORY_OVERSIZED_CODE));
    }

    #[test]
    fn bare_predict_next_n_refuses_oversized_topic_id_bd_3mhyr() {
        // Regression pin (bd-3mhyr): the bare, ungated predict_next_n must
        // decline a history that is within the COUNT cap but carries an
        // over-cap topic_id, matching the defense-in-depth contract on the
        // trait. Before the fix the bare path only checked
        // recent_first.len() > MAX_PREFETCH_HISTORY, so an oversized
        // topic_id within the count bound could be emitted as a candidate.
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let huge = "x".repeat(MAX_PREFETCH_TOPIC_ID_BYTES + 1);
        let history = CassPrefetchHistory::from_topics(
            test_agent_scope(),
            ["current".to_owned(), huge.clone(), huge.clone()],
        );
        // The count is within bounds, so the count-only guard would admit
        // it; only the topic_id byte-length check refuses it.
        assert!(history.recent_first.len() <= MAX_PREFETCH_HISTORY);
        assert!(!history.is_within_admission_bounds());

        let candidates = predictor.predict_next_n(&history, 3);
        assert!(
            candidates.is_empty(),
            "bare predictor must decline an out-of-bounds history, got {} candidate(s)",
            candidates.len()
        );
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.topic_id.as_str().len() <= MAX_PREFETCH_TOPIC_ID_BYTES),
            "bare predictor must never surface an over-cap topic_id"
        );
    }

    #[test]
    fn try_from_topics_enforces_bounds_by_construction_bd_1suaa() {
        // Within bounds -> Some.
        assert!(
            CassPrefetchHistory::try_from_topics(test_agent_scope(), ["a", "b", "c"]).is_some()
        );
        // Too many topics -> None.
        let too_many: Vec<String> = (0..=MAX_PREFETCH_HISTORY)
            .map(|i| format!("t{i}"))
            .collect();
        assert!(CassPrefetchHistory::try_from_topics(test_agent_scope(), too_many).is_none());
        // Oversized topic_id -> None.
        let huge = "y".repeat(MAX_PREFETCH_TOPIC_ID_BYTES + 1);
        assert!(CassPrefetchHistory::try_from_topics(test_agent_scope(), [huge]).is_none());
        // Exactly at the byte cap -> Some.
        let at_cap = "z".repeat(MAX_PREFETCH_TOPIC_ID_BYTES);
        assert!(CassPrefetchHistory::try_from_topics(test_agent_scope(), [at_cap]).is_some());
    }

    #[test]
    fn try_from_topics_checks_raw_topic_size_before_redaction_bd_1suaa() {
        let oversized_secret = format!(
            "postgres://user:{}@localhost/db",
            "s".repeat(MAX_PREFETCH_TOPIC_ID_BYTES)
        );
        assert!(
            TopicId::new(&oversized_secret).as_str().len() <= MAX_PREFETCH_TOPIC_ID_BYTES,
            "test fixture must redact to a short placeholder"
        );
        assert!(
            CassPrefetchHistory::try_from_topics(
                test_agent_scope(),
                ["current".to_owned(), oversized_secret],
            )
            .is_none(),
            "raw oversized topic must be refused before redaction can shrink it"
        );
    }

    #[test]
    fn candidate_constructor_and_deserialize_normalize_score_bd_1suaa() -> Result<(), String> {
        assert_eq!(CassPrefetchCandidate::new("low", -0.25, "test").score, 0.0);
        assert_eq!(CassPrefetchCandidate::new("high", 1.25, "test").score, 1.0);
        assert_eq!(
            CassPrefetchCandidate::new("nan", f64::NAN, "test").score,
            0.0
        );

        let decoded: CassPrefetchCandidate =
            serde_json::from_str(r#"{"topicId":"refactor","score":2.5,"predictor":"external"}"#)
                .map_err(|error| format!("deserialize candidate: {error}"))?;
        assert_eq!(decoded.score, 1.0);

        let literal = CassPrefetchCandidate {
            topic_id: TopicId::new("refactor"),
            score: f64::NAN,
            predictor: Cow::Borrowed("external"),
        };
        let encoded = serde_json::to_string(&literal)
            .map_err(|error| format!("serialize literal: {error}"))?;
        assert!(
            encoded.contains(r#""score":0.0"#),
            "serialized candidate must normalize invalid score, got {encoded}"
        );
        Ok(())
    }

    #[test]
    fn metrics_record_history_oversized_bd_1suaa() {
        let mut metrics = CassPrefetchMetrics::new();
        assert_eq!(metrics.history_oversized, 0);
        metrics.record_history_oversized();
        assert_eq!(metrics.history_oversized, 1);
        metrics.history_oversized = u64::MAX;
        metrics.record_history_oversized();
        assert_eq!(metrics.history_oversized, u64::MAX);
    }

    #[test]
    fn topic_id_redacts_secret_on_construction_bd_3aczq() {
        // A URL-embedded password is the documented leak vector (an agent
        // query like `ee context "connect to postgres://user:PW@host"`).
        // TopicId::new must mask it before storing.
        let secret = "pg_pw_do_not_leak";
        let topic = TopicId::new(format!("postgres://user:{secret}@localhost/db"));
        assert!(
            !topic.as_str().contains(secret),
            "raw secret leaked into topic_id: {}",
            topic.as_str()
        );
        assert!(
            topic.as_str().contains("[REDACTED"),
            "redaction placeholder missing: {}",
            topic.as_str()
        );
        // A benign topic is preserved unchanged (heuristic behavior intact).
        assert_eq!(TopicId::new("refactor").as_str(), "refactor");
    }

    #[test]
    fn topic_id_redacts_on_deserialize_bd_3aczq() {
        // The daemon-hydration vector: a history decoded from untrusted
        // JSON params must come back redacted, not verbatim. The serde
        // `from = "String"` shim routes the decoded value through
        // TopicId::new before it ever reaches the predictor.
        let secret = "pg_pw_via_serde";
        let json = format!(r#"{{"topicId":"postgres://user:{secret}@host/db"}}"#);
        let observation: CassPrefetchObservation =
            serde_json::from_str(&json).expect("deserialize observation");
        assert!(
            !observation.topic_id.as_str().contains(secret),
            "secret survived deserialize: {}",
            observation.topic_id.as_str()
        );
        assert!(observation.topic_id.as_str().contains("[REDACTED"));
    }

    #[test]
    fn predictor_never_emits_unredacted_topic_bd_3aczq() {
        // End to end: a secret-bearing observation flows through the
        // predictor; no emitted candidate may carry the raw secret.
        let secret = "pg_pw_end_to_end";
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let leaky = format!("postgres://user:{secret}@host/db");
        let history = CassPrefetchHistory::from_topics(
            test_agent_scope(),
            ["current", leaky.as_str(), leaky.as_str()],
        );
        let candidates = predictor.predict_next_n(&history, 3);
        assert!(!candidates.is_empty());
        for candidate in &candidates {
            assert!(
                !candidate.topic_id.as_str().contains(secret),
                "predictor emitted an unredacted secret: {}",
                candidate.topic_id.as_str()
            );
        }
    }

    #[test]
    fn topic_id_serializes_transparently_as_string_bd_3aczq() {
        // The wire shape is unchanged: a TopicId is a bare JSON string,
        // so the CASS_PREFETCH_DECISION_SCHEMA_V1 contract is preserved.
        let candidate = CassPrefetchCandidate::new("refactor", 0.5, "p");
        let json = serde_json::to_string(&candidate).expect("serialize candidate");
        assert!(json.contains(r#""topicId":"refactor""#), "got {json}");
    }

    #[test]
    fn built_in_predictor_borrows_static_name_bd_1cc1c() {
        let predictor = RecencyWeightedFrequencyPredictor::new();
        let history = history(&["current", "alpha", "alpha"]);
        let candidates = predictor.predict_next_n(&history, 3);
        assert_eq!(candidates.len(), 1);
        assert!(matches!(
            candidates[0].predictor,
            Cow::Borrowed("recency_weighted_frequency_v1")
        ));

        let json = serde_json::to_string(&candidates[0]).expect("serialize candidate");
        let decoded: CassPrefetchCandidate =
            serde_json::from_str(&json).expect("deserialize candidate");
        assert_eq!(decoded.predictor.as_ref(), "recency_weighted_frequency_v1");
        assert!(matches!(decoded.predictor, Cow::Owned(_)));
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
