//! Source trust decay (EE-278).
//!
//! Implements trust decay for import sources based on accumulated negative
//! feedback signals. Sources that repeatedly produce quarantined, contradicted,
//! or harmful content receive progressively lower trust scores.
//!
//! Decay factors are applied per signal type and accumulate multiplicatively.
//! A source's effective trust is: base_trust * decay_factor.

use crate::models::TrustClass;

/// Harmful event count where positive recovery must keep source risk visible.
pub const SEVERE_HARMFUL_HISTORY_COUNT: u32 = 3;

/// Recovery ceiling for sources with severe harmful history.
pub const SEVERE_HARM_RECOVERY_CEILING: f32 = 0.49;

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
        for _ in 0..state.quarantine_count {
            factor *= self.config.quarantine_decay;
        }

        // Apply contradiction decay
        for _ in 0..state.contradiction_count {
            factor *= self.config.contradiction_decay;
        }

        // Apply harmful decay
        for _ in 0..state.harmful_count {
            factor *= self.config.harmful_decay;
        }

        // Apply inaccurate decay
        for _ in 0..state.inaccurate_count {
            factor *= self.config.inaccurate_decay;
        }

        // Apply positive recovery without allowing severe harm history to disappear.
        let recovery_ceiling = self.positive_recovery_ceiling(state);
        for _ in 0..state.positive_count {
            factor *= self.config.positive_recovery;
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
        let effective = self.effective_trust(state);
        effective <= self.config.trust_floor * 2.0
    }

    fn positive_recovery_ceiling(&self, state: &SourceTrustState) -> f32 {
        let ceiling = if state.harmful_count >= SEVERE_HARMFUL_HISTORY_COUNT {
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

        if effective <= self.config.trust_floor * 2.0 {
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
        ensure(
            advisory.code(),
            "quarantine",
            "severe harm history remains visible after recovery",
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
}
