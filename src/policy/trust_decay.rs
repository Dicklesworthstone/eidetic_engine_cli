//! Source trust decay (EE-278).
//!
//! Implements trust decay for import sources based on accumulated negative
//! feedback signals. Sources that repeatedly produce quarantined, contradicted,
//! or harmful content receive progressively lower trust scores.
//!
//! Decay factors are applied per signal type and accumulate multiplicatively.
//! A source's effective trust is: base_trust * decay_factor.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::models::TrustClass;

/// Harmful event count where positive recovery must keep source risk visible.
pub const SEVERE_HARMFUL_HISTORY_COUNT: u32 = 3;

/// Recovery ceiling for sources with severe harmful history.
pub const SEVERE_HARM_RECOVERY_CEILING: f32 = 0.30;

/// Decay factor configuration per signal type.
#[derive(Clone, Copy, Debug)]
pub struct DecayConfig {
    /// Decay factor per quarantine event (0.0 to 1.0).
    pub quarantine_decay: f32,
    /// Decay factor per contradiction event (0.0 to 1.0).
    pub contradiction_decay: f32,
    /// Decay factor per harmful content event (0.0 to 1.0).
    pub harmful_decay: f32,
    /// Decay factor per inaccurate/stale event (0.0 to 1.0).
    pub inaccurate_decay: f32,
    /// Minimum trust floor (decay cannot go below this).
    pub trust_floor: f32,
    /// Recovery factor per positive signal (multiplicative boost).
    pub positive_recovery: f32,
    /// Maximum trust ceiling for recovery.
    pub trust_ceiling: f32,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            quarantine_decay: 0.85,
            contradiction_decay: 0.90,
            harmful_decay: 0.70,
            inaccurate_decay: 0.92,
            trust_floor: 0.05,
            positive_recovery: 1.05,
            trust_ceiling: 1.0,
        }
    }
}

impl DecayConfig {
    /// Create a strict decay configuration for high-security contexts.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            quarantine_decay: 0.70,
            contradiction_decay: 0.80,
            harmful_decay: 0.50,
            inaccurate_decay: 0.85,
            trust_floor: 0.01,
            positive_recovery: 1.02,
            trust_ceiling: 0.95,
        }
    }

    /// Create a lenient decay configuration for exploratory contexts.
    #[must_use]
    pub fn lenient() -> Self {
        Self {
            quarantine_decay: 0.92,
            contradiction_decay: 0.95,
            harmful_decay: 0.85,
            inaccurate_decay: 0.96,
            trust_floor: 0.10,
            positive_recovery: 1.10,
            trust_ceiling: 1.0,
        }
    }
}

/// Tracks accumulated trust signals for a source.
#[derive(Clone, Debug, Default)]
pub struct SourceTrustState {
    /// Source identifier (e.g., session path, import connector name).
    pub source_id: String,
    /// Base trust class for this source type.
    pub base_trust_class: Option<TrustClass>,
    /// Number of quarantine events observed.
    pub quarantine_count: u32,
    /// Number of contradiction events observed.
    pub contradiction_count: u32,
    /// Number of harmful content events observed.
    pub harmful_count: u32,
    /// Number of inaccurate/stale events observed.
    pub inaccurate_count: u32,
    /// Number of positive feedback events observed.
    pub positive_count: u32,
    /// Number of total imports from this source.
    pub total_imports: u32,
}

impl SourceTrustState {
    /// Create a new trust state for a source.
    #[must_use]
    pub fn new(source_id: impl Into<String>) -> Self {
        Self {
            source_id: source_id.into(),
            ..Default::default()
        }
    }

    /// Set the base trust class for this source.
    #[must_use]
    pub fn with_trust_class(mut self, class: TrustClass) -> Self {
        self.base_trust_class = Some(class);
        self
    }

    /// Record a quarantine event.
    pub fn record_quarantine(&mut self) {
        self.quarantine_count = self.quarantine_count.saturating_add(1);
    }

    /// Record a contradiction event.
    pub fn record_contradiction(&mut self) {
        self.contradiction_count = self.contradiction_count.saturating_add(1);
    }

    /// Record a harmful content event.
    pub fn record_harmful(&mut self) {
        self.harmful_count = self.harmful_count.saturating_add(1);
    }

    /// Record an inaccurate/stale event.
    pub fn record_inaccurate(&mut self) {
        self.inaccurate_count = self.inaccurate_count.saturating_add(1);
    }

    /// Record a positive feedback event.
    pub fn record_positive(&mut self) {
        self.positive_count = self.positive_count.saturating_add(1);
    }

    /// Record an import from this source.
    pub fn record_import(&mut self) {
        self.total_imports = self.total_imports.saturating_add(1);
    }

    /// Total negative signal count.
    #[must_use]
    pub fn negative_signal_count(&self) -> u32 {
        self.quarantine_count
            .saturating_add(self.contradiction_count)
            .saturating_add(self.harmful_count)
            .saturating_add(self.inaccurate_count)
    }

    /// Negative signal rate (negative / total imports).
    #[must_use]
    pub fn negative_rate(&self) -> f32 {
        if self.total_imports == 0 {
            0.0
        } else {
            self.negative_signal_count() as f32 / self.total_imports as f32
        }
    }
}

/// Calculates effective trust for a source based on accumulated signals.
#[derive(Clone, Debug)]
pub struct TrustDecayCalculator {
    config: DecayConfig,
}

impl Default for TrustDecayCalculator {
    fn default() -> Self {
        Self::new()
    }
}

impl TrustDecayCalculator {
    /// Create a calculator with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: DecayConfig::default(),
        }
    }

    /// Create a calculator with custom configuration.
    #[must_use]
    pub fn with_config(config: DecayConfig) -> Self {
        Self { config }
    }

    /// Calculate the decay factor based on accumulated negative signals.
    #[must_use]
    pub fn calculate_decay_factor(&self, state: &SourceTrustState) -> f32 {
        let mut factor = 1.0_f32;

        // Apply quarantine decay
        factor *= self.config.quarantine_decay.powi(state.quarantine_count.min(i32::MAX as u32) as i32);

        // Apply contradiction decay
        factor *= self.config.contradiction_decay.powi(state.contradiction_count.min(i32::MAX as u32) as i32);

        // Apply harmful decay
        factor *= self.config.harmful_decay.powi(state.harmful_count.min(i32::MAX as u32) as i32);

        // Apply inaccurate decay
        factor *= self.config.inaccurate_decay.powi(state.inaccurate_count.min(i32::MAX as u32) as i32);

        // Apply positive recovery without allowing severe harm history to disappear.
        let recovery_ceiling = self.positive_recovery_ceiling(state);
        let max_positive_events = state.positive_count.min(i32::MAX as u32) as i32;
        if max_positive_events > 0 {
            factor *= self.config.positive_recovery.powi(max_positive_events);
            if factor > recovery_ceiling {
                factor = recovery_ceiling;
            }
        }

        // Enforce floor
        factor.max(self.config.trust_floor)
    }

    /// Calculate effective trust score for a source.
    #[must_use]
    pub fn effective_trust(&self, state: &SourceTrustState) -> f32 {
        let base_confidence = state
            .base_trust_class
            .map(|c| c.initial_confidence())
            .unwrap_or(0.50);

        let decay_factor = self.calculate_decay_factor(state);
        (base_confidence * decay_factor).max(self.config.trust_floor)
    }

    /// Determine if a source should be blocked based on decay.
    #[must_use]
    pub fn should_block(&self, state: &SourceTrustState) -> bool {
        Self::has_severe_harmful_history(state)
            || self.effective_trust(state) <= self.config.trust_floor * 2.0
    }

    fn has_severe_harmful_history(state: &SourceTrustState) -> bool {
        state.harmful_count >= SEVERE_HARMFUL_HISTORY_COUNT
    }

    fn positive_recovery_ceiling(&self, state: &SourceTrustState) -> f32 {
        let ceiling = if Self::has_severe_harmful_history(state) {
            self.config.trust_ceiling.min(SEVERE_HARM_RECOVERY_CEILING)
        } else {
            self.config.trust_ceiling
        };
        ceiling.max(self.config.trust_floor)
    }

    /// Get a trust advisory for a source.
    #[must_use]
    pub fn advisory(&self, state: &SourceTrustState) -> TrustAdvisory {
        let effective = self.effective_trust(state);
        let decay_factor = self.calculate_decay_factor(state);

        if Self::has_severe_harmful_history(state) {
            TrustAdvisory::Block {
                reason: format!(
                    "Source has {} harmful events; manual review required before import",
                    state.harmful_count
                ),
            }
        } else if effective <= self.config.trust_floor * 2.0 {
            TrustAdvisory::Block {
                reason: format!(
                    "Source trust ({:.2}) below threshold after {} negative signals",
                    effective,
                    state.negative_signal_count()
                ),
            }
        } else if decay_factor < 0.5 {
            TrustAdvisory::Quarantine {
                effective_trust: effective,
                decay_factor,
                negative_rate: state.negative_rate(),
            }
        } else if decay_factor < 0.8 {
            TrustAdvisory::Warn {
                effective_trust: effective,
                decay_factor,
                message: format!(
                    "Source has {} negative signals across {} imports",
                    state.negative_signal_count(),
                    state.total_imports
                ),
            }
        } else {
            TrustAdvisory::Allow {
                effective_trust: effective,
            }
        }
    }
}

/// Advisory result from trust decay evaluation.
#[derive(Clone, Debug)]
pub enum TrustAdvisory {
    /// Source is allowed with full trust.
    Allow { effective_trust: f32 },
    /// Source allowed but with warning.
    Warn {
        effective_trust: f32,
        decay_factor: f32,
        message: String,
    },
    /// Source should be quarantined (imports need extra validation).
    Quarantine {
        effective_trust: f32,
        decay_factor: f32,
        negative_rate: f32,
    },
    /// Source should be blocked entirely.
    Block { reason: String },
}

impl TrustAdvisory {
    /// Stable string code for the advisory level.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Allow { .. } => "allow",
            Self::Warn { .. } => "warn",
            Self::Quarantine { .. } => "quarantine",
            Self::Block { .. } => "block",
        }
    }

    /// Whether this advisory permits import.
    #[must_use]
    pub const fn permits_import(&self) -> bool {
        !matches!(self, Self::Block { .. })
    }

    /// Whether this advisory requires additional validation.
    #[must_use]
    pub const fn requires_validation(&self) -> bool {
        matches!(self, Self::Quarantine { .. } | Self::Block { .. })
    }
}

/// Stable schema for peer outcome feedback summaries.
pub const PEER_OUTCOME_FEEDBACK_SCHEMA_V1: &str = "ee.mesh.peer_outcome_feedback.v1";

/// Structured audit event recorded when a peer feedback row is accepted for evaluation.
pub const PEER_FEEDBACK_RECEIVED_EVENT: &str = "peer_feedback_received";
/// Structured audit event recorded when peer trust is adjusted.
pub const PEER_TRUST_DELTA_APPLIED_EVENT: &str = "trust_delta_applied";
/// Structured audit event recorded when policy rejects or ignores feedback.
pub const PEER_FEEDBACK_IGNORED_BY_POLICY_EVENT: &str = "feedback_ignored_by_policy";
/// Structured audit event recorded when feedback changes candidate ranking.
pub const PEER_RANKING_ADJUSTMENT_REASON_EVENT: &str = "ranking_adjustment_reason";

/// Peer-reported outcome for a cached memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerOutcomeFeedbackKind {
    Helped,
    Misled,
    Stale,
    Contradicted,
    Withdrawn,
}

impl PeerOutcomeFeedbackKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Helped => "helped",
            Self::Misled => "misled",
            Self::Stale => "stale",
            Self::Contradicted => "contradicted",
            Self::Withdrawn => "withdrawn",
        }
    }

    const fn trust_delta(self) -> f32 {
        match self {
            Self::Helped => 0.025,
            Self::Misled => -0.060,
            Self::Stale => -0.040,
            Self::Contradicted => -0.070,
            Self::Withdrawn => -0.050,
        }
    }

    const fn quality_delta(self) -> f32 {
        match self {
            Self::Helped => 0.060,
            Self::Misled => -0.080,
            Self::Stale => -0.050,
            Self::Contradicted => -0.100,
            Self::Withdrawn => -0.070,
        }
    }

    const fn ranking_delta(self) -> f32 {
        match self {
            Self::Helped => 0.050,
            Self::Misled => -0.070,
            Self::Stale => -0.045,
            Self::Contradicted => -0.080,
            Self::Withdrawn => -0.055,
        }
    }
}

/// Redacted peer feedback row. The text that originally led to the outcome is
/// intentionally not part of this input.
#[derive(Clone, Debug, PartialEq)]
pub struct PeerOutcomeFeedbackEvent<'a> {
    pub peer_id: &'a str,
    pub memory_id: &'a str,
    pub kind: PeerOutcomeFeedbackKind,
    pub peer_weight: f32,
    pub evidence_ref: Option<&'a str>,
}

/// Local view of a peer's historical outcome quality.
#[derive(Clone, Debug, PartialEq)]
pub struct PeerOutcomePeerState<'a> {
    pub peer_id: &'a str,
    pub trust_score: f32,
    pub feedback_count: u32,
    pub misleading_count: u32,
    pub stale_count: u32,
    pub contradiction_count: u32,
    pub withdrawn_count: u32,
}

impl<'a> PeerOutcomePeerState<'a> {
    #[must_use]
    pub const fn new(peer_id: &'a str, trust_score: f32) -> Self {
        Self {
            peer_id,
            trust_score,
            feedback_count: 0,
            misleading_count: 0,
            stale_count: 0,
            contradiction_count: 0,
            withdrawn_count: 0,
        }
    }

    #[must_use]
    pub const fn with_counts(
        mut self,
        feedback_count: u32,
        misleading_count: u32,
        stale_count: u32,
        contradiction_count: u32,
        withdrawn_count: u32,
    ) -> Self {
        self.feedback_count = feedback_count;
        self.misleading_count = misleading_count;
        self.stale_count = stale_count;
        self.contradiction_count = contradiction_count;
        self.withdrawn_count = withdrawn_count;
        self
    }

    #[must_use]
    pub fn negative_count(&self) -> u32 {
        self.misleading_count
            .saturating_add(self.stale_count)
            .saturating_add(self.contradiction_count)
            .saturating_add(self.withdrawn_count)
    }

    #[must_use]
    pub fn negative_rate(&self) -> f32 {
        if self.feedback_count == 0 {
            0.0
        } else {
            (self.negative_count() as f32 / self.feedback_count as f32).clamp(0.0, 1.0)
        }
    }
}

/// Local policy for receiving and using peer outcome feedback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeerOutcomeFeedbackPolicy {
    pub receive_peer_feedback: bool,
    pub use_feedback_for_ranking: bool,
    pub max_peer_weight: f32,
    pub min_peer_trust_for_ranking: f32,
    pub max_single_peer_adjustment: f32,
    pub bad_peer_weight_cap: f32,
}

impl Default for PeerOutcomeFeedbackPolicy {
    fn default() -> Self {
        Self {
            receive_peer_feedback: true,
            use_feedback_for_ranking: true,
            max_peer_weight: 1.0,
            min_peer_trust_for_ranking: 0.20,
            max_single_peer_adjustment: 0.080,
            bad_peer_weight_cap: 0.050,
        }
    }
}

/// One peer feedback signal after local policy and trust decay are applied.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerOutcomeFeedbackSignal {
    pub peer_id: String,
    pub memory_id: String,
    pub kind: &'static str,
    pub evidence_ref: Option<String>,
    pub peer_weight: f32,
    pub effective_weight: f32,
    pub trust_delta: f32,
    pub quality_delta: f32,
    pub ranking_adjustment: f32,
    pub ranking_adjustment_reason: &'static str,
    pub ignored: bool,
    pub ignore_reason: Option<&'static str>,
}

/// Structured audit log for peer outcome feedback handling.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerOutcomeFeedbackLog {
    pub event: &'static str,
    pub peer_id: String,
    pub memory_id: String,
    pub reason: &'static str,
    pub trust_delta: f32,
    pub ranking_adjustment: f32,
}

/// Deterministic local summary for peer feedback on one memory.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerOutcomeFeedbackSummary {
    pub schema: &'static str,
    pub memory_id: String,
    pub local_truth_mutation_allowed: bool,
    pub signal_count: usize,
    pub ignored_count: usize,
    pub ranking_adjustment: f32,
    pub quality_delta: f32,
    pub trust_delta_by_peer: BTreeMap<String, f32>,
    pub signals: Vec<PeerOutcomeFeedbackSignal>,
    pub logs: Vec<PeerOutcomeFeedbackLog>,
}

/// Summarize peer outcome feedback without mutating local truth.
///
/// The output is deterministic and advisory: both positive and negative peer
/// signals remain visible, but rank/quality effects are capped by local policy
/// and by the peer's historical trust state.
#[must_use]
pub fn summarize_peer_outcome_feedback(
    memory_id: &str,
    events: &[PeerOutcomeFeedbackEvent<'_>],
    peer_states: &[PeerOutcomePeerState<'_>],
    policy: PeerOutcomeFeedbackPolicy,
) -> PeerOutcomeFeedbackSummary {
    let peer_states_by_id = peer_states
        .iter()
        .map(|state| (state.peer_id, state))
        .collect::<BTreeMap<_, _>>();
    let mut ordered_events = events
        .iter()
        .filter(|event| event.memory_id == memory_id)
        .collect::<Vec<_>>();
    ordered_events.sort_by(|left, right| {
        left.peer_id
            .cmp(right.peer_id)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.evidence_ref.cmp(&right.evidence_ref))
    });

    let mut signals = Vec::with_capacity(ordered_events.len());
    let mut logs = Vec::new();
    let mut trust_delta_by_peer = BTreeMap::<String, f32>::new();

    for event in ordered_events {
        let state = peer_states_by_id.get(event.peer_id).copied();
        let (signal, mut signal_logs) = plan_peer_outcome_feedback(event, state, policy);
        if !signal.ignored {
            let entry = trust_delta_by_peer
                .entry(signal.peer_id.clone())
                .or_insert(0.0);
            *entry = round_peer_feedback_metric(*entry + signal.trust_delta);
        }
        logs.append(&mut signal_logs);
        signals.push(signal);
    }

    let ignored_count = signals.iter().filter(|signal| signal.ignored).count();
    let ranking_adjustment = round_peer_feedback_metric(
        signals
            .iter()
            .map(|signal| signal.ranking_adjustment)
            .sum::<f32>(),
    );
    let quality_delta = round_peer_feedback_metric(
        signals
            .iter()
            .map(|signal| signal.quality_delta)
            .sum::<f32>(),
    );

    PeerOutcomeFeedbackSummary {
        schema: PEER_OUTCOME_FEEDBACK_SCHEMA_V1,
        memory_id: memory_id.to_owned(),
        local_truth_mutation_allowed: false,
        signal_count: signals.len(),
        ignored_count,
        ranking_adjustment,
        quality_delta,
        trust_delta_by_peer,
        signals,
        logs,
    }
}

fn plan_peer_outcome_feedback(
    event: &PeerOutcomeFeedbackEvent<'_>,
    state: Option<&PeerOutcomePeerState<'_>>,
    policy: PeerOutcomeFeedbackPolicy,
) -> (PeerOutcomeFeedbackSignal, Vec<PeerOutcomeFeedbackLog>) {
    let mut logs = Vec::new();
    if !policy.receive_peer_feedback {
        return (
            ignored_peer_feedback_signal(event, "peer_feedback_opt_out"),
            vec![peer_feedback_log(
                PEER_FEEDBACK_IGNORED_BY_POLICY_EVENT,
                event,
                "peer_feedback_opt_out",
                0.0,
                0.0,
            )],
        );
    }

    logs.push(peer_feedback_log(
        PEER_FEEDBACK_RECEIVED_EVENT,
        event,
        "received_redacted_peer_outcome",
        0.0,
        0.0,
    ));

    if !policy.use_feedback_for_ranking {
        logs.push(peer_feedback_log(
            PEER_FEEDBACK_IGNORED_BY_POLICY_EVENT,
            event,
            "ranking_feedback_opt_out",
            0.0,
            0.0,
        ));
        return (
            ignored_peer_feedback_signal(event, "ranking_feedback_opt_out"),
            logs,
        );
    }

    let peer_trust = finite_clamped(
        state.map_or(0.50, |state| state.trust_score),
        0.0,
        1.0,
        0.50,
    );
    let min_peer_trust = finite_clamped(policy.min_peer_trust_for_ranking, 0.0, 1.0, 0.20);
    if peer_trust < min_peer_trust {
        logs.push(peer_feedback_log(
            PEER_FEEDBACK_IGNORED_BY_POLICY_EVENT,
            event,
            "peer_trust_below_policy_floor",
            0.0,
            0.0,
        ));
        return (
            ignored_peer_feedback_signal(event, "peer_trust_below_policy_floor"),
            logs,
        );
    }

    let max_peer_weight = finite_clamped(policy.max_peer_weight, 0.0, 1.0, 1.0);
    let peer_weight = finite_clamped(event.peer_weight, 0.0, max_peer_weight, 1.0);
    let effective_weight = effective_peer_feedback_weight(event, state, policy, peer_trust);
    let trust_delta = round_peer_feedback_metric(event.kind.trust_delta() * effective_weight);
    let quality_delta = round_peer_feedback_metric(event.kind.quality_delta() * effective_weight);
    let max_adjustment = finite_clamped(policy.max_single_peer_adjustment, 0.0, 1.0, 0.080);
    let ranking_adjustment = round_peer_feedback_metric(
        (event.kind.ranking_delta() * effective_weight).clamp(-max_adjustment, max_adjustment),
    );

    logs.push(peer_feedback_log(
        PEER_TRUST_DELTA_APPLIED_EVENT,
        event,
        event.kind.as_str(),
        trust_delta,
        0.0,
    ));
    logs.push(peer_feedback_log(
        PEER_RANKING_ADJUSTMENT_REASON_EVENT,
        event,
        event.kind.as_str(),
        0.0,
        ranking_adjustment,
    ));

    (
        PeerOutcomeFeedbackSignal {
            peer_id: event.peer_id.to_owned(),
            memory_id: event.memory_id.to_owned(),
            kind: event.kind.as_str(),
            evidence_ref: event.evidence_ref.map(str::to_owned),
            peer_weight,
            effective_weight,
            trust_delta,
            quality_delta,
            ranking_adjustment,
            ranking_adjustment_reason: event.kind.as_str(),
            ignored: false,
            ignore_reason: None,
        },
        logs,
    )
}

fn effective_peer_feedback_weight(
    event: &PeerOutcomeFeedbackEvent<'_>,
    state: Option<&PeerOutcomePeerState<'_>>,
    policy: PeerOutcomeFeedbackPolicy,
    peer_trust: f32,
) -> f32 {
    let max_peer_weight = finite_clamped(policy.max_peer_weight, 0.0, 1.0, 1.0);
    let peer_weight = finite_clamped(event.peer_weight, 0.0, max_peer_weight, 1.0);
    let history_decay = state.map_or(1.0, |state| 1.0 - state.negative_rate());
    let base_weight = peer_weight * peer_trust * history_decay.clamp(0.0, 1.0);
    let capped_weight = if state.is_some_and(is_bad_peer_feedback_source) {
        base_weight.min(finite_clamped(policy.bad_peer_weight_cap, 0.0, 1.0, 0.050))
    } else {
        base_weight
    };
    round_peer_feedback_metric(capped_weight.clamp(0.0, max_peer_weight))
}

fn is_bad_peer_feedback_source(state: &PeerOutcomePeerState<'_>) -> bool {
    state.negative_count() >= 3 && state.negative_rate() >= 0.60
}

fn ignored_peer_feedback_signal(
    event: &PeerOutcomeFeedbackEvent<'_>,
    reason: &'static str,
) -> PeerOutcomeFeedbackSignal {
    PeerOutcomeFeedbackSignal {
        peer_id: event.peer_id.to_owned(),
        memory_id: event.memory_id.to_owned(),
        kind: event.kind.as_str(),
        evidence_ref: event.evidence_ref.map(str::to_owned),
        peer_weight: 0.0,
        effective_weight: 0.0,
        trust_delta: 0.0,
        quality_delta: 0.0,
        ranking_adjustment: 0.0,
        ranking_adjustment_reason: "ignored",
        ignored: true,
        ignore_reason: Some(reason),
    }
}

fn peer_feedback_log(
    event: &'static str,
    feedback: &PeerOutcomeFeedbackEvent<'_>,
    reason: &'static str,
    trust_delta: f32,
    ranking_adjustment: f32,
) -> PeerOutcomeFeedbackLog {
    PeerOutcomeFeedbackLog {
        event,
        peer_id: feedback.peer_id.to_owned(),
        memory_id: feedback.memory_id.to_owned(),
        reason,
        trust_delta,
        ranking_adjustment,
    }
}

fn finite_clamped(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    let normalized = if value.is_finite() { value } else { fallback };
    normalized.clamp(min, max)
}

fn round_peer_feedback_metric(value: f32) -> f32 {
    (value * 1_000.0).round() / 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn ensure_approx(actual: f32, expected: f32, tolerance: f32, ctx: &str) -> TestResult {
        if (actual - expected).abs() < tolerance {
            Ok(())
        } else {
            Err(format!(
                "{ctx}: expected {expected:.4} ± {tolerance}, got {actual:.4}"
            ))
        }
    }

    #[test]
    fn fresh_source_has_no_decay() -> TestResult {
        let state = SourceTrustState::new("test_source");
        let calc = TrustDecayCalculator::new();

        ensure_approx(calc.calculate_decay_factor(&state), 1.0, 0.001, "no decay")
    }

    #[test]
    fn quarantine_applies_decay() -> TestResult {
        let mut state = SourceTrustState::new("test_source");
        state.record_quarantine();

        let calc = TrustDecayCalculator::new();
        let factor = calc.calculate_decay_factor(&state);

        ensure_approx(factor, 0.85, 0.001, "single quarantine decay")
    }

    #[test]
    fn multiple_quarantines_compound() -> TestResult {
        let mut state = SourceTrustState::new("test_source");
        state.record_quarantine();
        state.record_quarantine();

        let calc = TrustDecayCalculator::new();
        let factor = calc.calculate_decay_factor(&state);

        ensure_approx(factor, 0.85 * 0.85, 0.001, "compound quarantine decay")
    }

    #[test]
    fn harmful_has_stronger_decay() -> TestResult {
        let mut harmful_state = SourceTrustState::new("harmful");
        harmful_state.record_harmful();

        let mut quarantine_state = SourceTrustState::new("quarantine");
        quarantine_state.record_quarantine();

        let calc = TrustDecayCalculator::new();

        ensure(
            calc.calculate_decay_factor(&harmful_state)
                < calc.calculate_decay_factor(&quarantine_state),
            true,
            "harmful stronger than quarantine",
        )
    }

    #[test]
    fn decay_respects_floor() -> TestResult {
        let mut state = SourceTrustState::new("test_source");
        for _ in 0..100 {
            state.record_harmful();
        }

        let calc = TrustDecayCalculator::new();
        let factor = calc.calculate_decay_factor(&state);

        ensure(factor >= 0.05, true, "decay respects floor")
    }

    #[test]
    fn positive_signals_provide_recovery() -> TestResult {
        let mut state = SourceTrustState::new("test_source");
        state.record_quarantine();
        state.record_positive();

        let calc = TrustDecayCalculator::new();
        let factor = calc.calculate_decay_factor(&state);

        ensure(factor > 0.85, true, "positive provides recovery")
    }

    #[test]
    fn positive_recovery_does_not_erase_severe_harm_history() -> TestResult {
        let mut state = SourceTrustState::new("harmful_source");
        for _ in 0..SEVERE_HARMFUL_HISTORY_COUNT {
            state.record_harmful();
        }
        for _ in 0..100 {
            state.record_positive();
        }

        let calc = TrustDecayCalculator::new();
        let factor = calc.calculate_decay_factor(&state);
        let advisory = calc.advisory(&state);

        ensure_approx(
            factor,
            SEVERE_HARM_RECOVERY_CEILING,
            0.001,
            "severe harm recovery ceiling",
        )?;
        ensure(calc.should_block(&state), true, "severe harm blocks import")?;
        ensure(
            advisory.permits_import(),
            false,
            "severe harm recovery does not permit import",
        )?;
        ensure(
            advisory.code(),
            "block",
            "severe harm history remains blocking after recovery",
        )
    }

    #[test]
    fn effective_trust_uses_base_class() -> TestResult {
        let mut state =
            SourceTrustState::new("test_source").with_trust_class(TrustClass::CassEvidence);
        state.record_quarantine();

        let calc = TrustDecayCalculator::new();
        let effective = calc.effective_trust(&state);

        // CassEvidence is 0.45, decay is 0.85, so 0.45 * 0.85 = 0.3825
        ensure_approx(effective, 0.3825, 0.001, "effective trust with base class")
    }

    #[test]
    fn advisory_block_for_severe_decay() -> TestResult {
        let mut state = SourceTrustState::new("bad_source");
        for _ in 0..10 {
            state.record_harmful();
        }

        let calc = TrustDecayCalculator::new();
        let advisory = calc.advisory(&state);

        ensure(advisory.code(), "block", "severe decay blocks")
    }

    #[test]
    fn advisory_quarantine_for_moderate_decay() -> TestResult {
        let mut state =
            SourceTrustState::new("sketchy_source").with_trust_class(TrustClass::CassEvidence);
        state.record_quarantine();
        state.record_quarantine();
        state.record_contradiction();

        let calc = TrustDecayCalculator::new();
        let advisory = calc.advisory(&state);

        ensure(
            matches!(
                advisory,
                TrustAdvisory::Quarantine { .. } | TrustAdvisory::Warn { .. }
            ),
            true,
            "moderate decay triggers quarantine or warn",
        )
    }

    #[test]
    fn advisory_allow_for_clean_source() -> TestResult {
        let state =
            SourceTrustState::new("clean_source").with_trust_class(TrustClass::HumanExplicit);

        let calc = TrustDecayCalculator::new();
        let advisory = calc.advisory(&state);

        ensure(advisory.code(), "allow", "clean source allowed")
    }

    #[test]
    fn negative_rate_calculated_correctly() -> TestResult {
        let mut state = SourceTrustState::new("test_source");
        state.total_imports = 10;
        state.record_quarantine();
        state.record_contradiction();

        ensure_approx(state.negative_rate(), 0.2, 0.001, "negative rate")
    }

    #[test]
    fn strict_config_decays_faster() -> TestResult {
        let mut state = SourceTrustState::new("test_source");
        state.record_harmful();

        let default_calc = TrustDecayCalculator::new();
        let strict_calc = TrustDecayCalculator::with_config(DecayConfig::strict());

        ensure(
            strict_calc.calculate_decay_factor(&state)
                < default_calc.calculate_decay_factor(&state),
            true,
            "strict decays faster",
        )
    }

    #[test]
    fn lenient_config_decays_slower() -> TestResult {
        let mut state = SourceTrustState::new("test_source");
        state.record_harmful();

        let default_calc = TrustDecayCalculator::new();
        let lenient_calc = TrustDecayCalculator::with_config(DecayConfig::lenient());

        ensure(
            lenient_calc.calculate_decay_factor(&state)
                > default_calc.calculate_decay_factor(&state),
            true,
            "lenient decays slower",
        )
    }

    #[test]
    fn advisory_codes_are_stable() -> TestResult {
        ensure(
            TrustAdvisory::Allow {
                effective_trust: 1.0,
            }
            .code(),
            "allow",
            "allow code",
        )?;
        ensure(
            TrustAdvisory::Warn {
                effective_trust: 0.5,
                decay_factor: 0.8,
                message: String::new(),
            }
            .code(),
            "warn",
            "warn code",
        )?;
        ensure(
            TrustAdvisory::Quarantine {
                effective_trust: 0.3,
                decay_factor: 0.4,
                negative_rate: 0.3,
            }
            .code(),
            "quarantine",
            "quarantine code",
        )?;
        ensure(
            TrustAdvisory::Block {
                reason: String::new(),
            }
            .code(),
            "block",
            "block code",
        )
    }

    #[test]
    fn advisory_permits_import_logic() -> TestResult {
        ensure(
            TrustAdvisory::Allow {
                effective_trust: 1.0,
            }
            .permits_import(),
            true,
            "allow permits",
        )?;
        ensure(
            TrustAdvisory::Warn {
                effective_trust: 0.5,
                decay_factor: 0.8,
                message: String::new(),
            }
            .permits_import(),
            true,
            "warn permits",
        )?;
        ensure(
            TrustAdvisory::Quarantine {
                effective_trust: 0.3,
                decay_factor: 0.4,
                negative_rate: 0.3,
            }
            .permits_import(),
            true,
            "quarantine permits (with validation)",
        )?;
        ensure(
            TrustAdvisory::Block {
                reason: String::new(),
            }
            .permits_import(),
            false,
            "block does not permit",
        )
    }

    #[test]
    fn peer_outcome_feedback_keeps_conflicting_signals_advisory() -> TestResult {
        let events = vec![
            PeerOutcomeFeedbackEvent {
                peer_id: "peer_beta",
                memory_id: "mem_shared",
                kind: PeerOutcomeFeedbackKind::Misled,
                peer_weight: 1.0,
                evidence_ref: Some("evidence://beta_run_001"),
            },
            PeerOutcomeFeedbackEvent {
                peer_id: "peer_alpha",
                memory_id: "mem_shared",
                kind: PeerOutcomeFeedbackKind::Helped,
                peer_weight: 1.0,
                evidence_ref: Some("evidence://alpha_run_001"),
            },
        ];
        let states = vec![
            PeerOutcomePeerState::new("peer_alpha", 0.80),
            PeerOutcomePeerState::new("peer_beta", 0.70),
        ];

        let summary = summarize_peer_outcome_feedback(
            "mem_shared",
            &events,
            &states,
            PeerOutcomeFeedbackPolicy::default(),
        );

        ensure(
            summary.schema,
            PEER_OUTCOME_FEEDBACK_SCHEMA_V1,
            "peer outcome schema",
        )?;
        ensure(
            summary.local_truth_mutation_allowed,
            false,
            "peer feedback never mutates local truth",
        )?;
        ensure(summary.signal_count, 2, "both peer signals visible")?;
        ensure(summary.ignored_count, 0, "no peer signal ignored")?;
        ensure(
            summary
                .signals
                .iter()
                .map(|signal| signal.kind)
                .collect::<Vec<_>>(),
            vec!["helped", "misled"],
            "signals sorted deterministically",
        )?;
        ensure(
            summary
                .logs
                .iter()
                .any(|log| log.event == PEER_RANKING_ADJUSTMENT_REASON_EVENT),
            true,
            "ranking adjustment reason logged",
        )
    }

    #[test]
    fn peer_outcome_feedback_policy_opt_out_ignores_feedback() -> TestResult {
        let events = vec![PeerOutcomeFeedbackEvent {
            peer_id: "peer_alpha",
            memory_id: "mem_shared",
            kind: PeerOutcomeFeedbackKind::Helped,
            peer_weight: 1.0,
            evidence_ref: None,
        }];
        let policy = PeerOutcomeFeedbackPolicy {
            receive_peer_feedback: false,
            ..PeerOutcomeFeedbackPolicy::default()
        };

        let summary = summarize_peer_outcome_feedback("mem_shared", &events, &[], policy);

        ensure(summary.ignored_count, 1, "feedback ignored by opt-out")?;
        ensure_approx(
            summary.ranking_adjustment,
            0.0,
            0.001,
            "opt-out ranking adjustment",
        )?;
        ensure(
            summary.signals[0].ignore_reason,
            Some("peer_feedback_opt_out"),
            "opt-out reason",
        )?;
        ensure(
            summary
                .logs
                .iter()
                .any(|log| log.event == PEER_FEEDBACK_IGNORED_BY_POLICY_EVENT),
            true,
            "policy ignore logged",
        )
    }

    #[test]
    fn peer_outcome_feedback_caps_weight_and_single_adjustment() -> TestResult {
        let events = vec![PeerOutcomeFeedbackEvent {
            peer_id: "peer_alpha",
            memory_id: "mem_shared",
            kind: PeerOutcomeFeedbackKind::Contradicted,
            peer_weight: 42.0,
            evidence_ref: Some("evidence://alpha_run_001"),
        }];
        let states = vec![PeerOutcomePeerState::new("peer_alpha", 1.0)];
        let policy = PeerOutcomeFeedbackPolicy {
            max_peer_weight: 0.50,
            max_single_peer_adjustment: 0.030,
            ..PeerOutcomeFeedbackPolicy::default()
        };

        let summary = summarize_peer_outcome_feedback("mem_shared", &events, &states, policy);

        ensure_approx(
            summary.signals[0].peer_weight,
            0.50,
            0.001,
            "peer weight cap",
        )?;
        ensure_approx(
            summary.signals[0].ranking_adjustment,
            -0.030,
            0.001,
            "single peer ranking cap",
        )
    }

    #[test]
    fn peer_outcome_feedback_bad_peer_history_decays_influence() -> TestResult {
        let events = vec![PeerOutcomeFeedbackEvent {
            peer_id: "peer_bad",
            memory_id: "mem_shared",
            kind: PeerOutcomeFeedbackKind::Contradicted,
            peer_weight: 1.0,
            evidence_ref: Some("evidence://bad_run_001"),
        }];
        let states = vec![PeerOutcomePeerState::new("peer_bad", 1.0).with_counts(10, 7, 1, 1, 0)];
        let policy = PeerOutcomeFeedbackPolicy {
            bad_peer_weight_cap: 0.030,
            ..PeerOutcomeFeedbackPolicy::default()
        };

        let summary = summarize_peer_outcome_feedback("mem_shared", &events, &states, policy);

        ensure(
            summary.signals[0].effective_weight <= 0.030,
            true,
            "bad peer effective weight capped",
        )?;
        ensure(
            summary.ranking_adjustment.abs() <= 0.003,
            true,
            "bad peer cannot poison ranking",
        )
    }

    #[test]
    fn peer_outcome_feedback_trust_delta_is_per_peer_and_logged() -> TestResult {
        let events = vec![
            PeerOutcomeFeedbackEvent {
                peer_id: "peer_alpha",
                memory_id: "mem_shared",
                kind: PeerOutcomeFeedbackKind::Helped,
                peer_weight: 1.0,
                evidence_ref: None,
            },
            PeerOutcomeFeedbackEvent {
                peer_id: "peer_alpha",
                memory_id: "mem_shared",
                kind: PeerOutcomeFeedbackKind::Stale,
                peer_weight: 0.5,
                evidence_ref: None,
            },
        ];
        let states = vec![PeerOutcomePeerState::new("peer_alpha", 0.80)];

        let summary = summarize_peer_outcome_feedback(
            "mem_shared",
            &events,
            &states,
            PeerOutcomeFeedbackPolicy::default(),
        );

        ensure(
            summary.trust_delta_by_peer.contains_key("peer_alpha"),
            true,
            "trust delta by peer",
        )?;
        ensure(
            summary
                .logs
                .iter()
                .any(|log| log.event == PEER_FEEDBACK_RECEIVED_EVENT),
            true,
            "peer feedback received log",
        )?;
        ensure(
            summary
                .logs
                .iter()
                .any(|log| log.event == PEER_TRUST_DELTA_APPLIED_EVENT),
            true,
            "trust delta log",
        )
    }
}
