//! Quarantine advisory report (EE-305).
//!
//! Provides advisory output about sources that are quarantined or approaching
//! quarantine based on accumulated negative feedback signals. Used by
//! `ee diag quarantine --json` to surface trust decay information to agents.

use crate::policy::{DecayConfig, SourceTrustState, TrustAdvisory, TrustDecayCalculator};

/// Report summarizing quarantine state across all tracked sources.
#[derive(Clone, Debug)]
pub struct QuarantineReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Sources currently in quarantine state.
    pub quarantined_sources: Vec<QuarantineEntry>,
    /// Sources at risk of quarantine (warn advisory).
    pub at_risk_sources: Vec<QuarantineEntry>,
    /// Sources that are blocked entirely.
    pub blocked_sources: Vec<QuarantineEntry>,
    /// Summary counts.
    pub summary: QuarantineSummary,
}

/// A single source with its quarantine status.
#[derive(Clone, Debug)]
pub struct QuarantineEntry {
    /// Source identifier.
    pub source_id: String,
    /// Current advisory level.
    pub advisory: AdvisoryLevel,
    /// Effective trust score after decay.
    pub effective_trust: f32,
    /// Decay factor applied.
    pub decay_factor: f32,
    /// Negative feedback rate (negative / total).
    pub negative_rate: f32,
    /// Total negative signals recorded.
    pub negative_count: u32,
    /// Total imports from this source.
    pub total_imports: u32,
    /// Advisory message.
    pub message: String,
    /// Whether import is still permitted.
    pub permits_import: bool,
    /// Whether additional validation is required.
    pub requires_validation: bool,
}

/// Summary counts for the quarantine report.
#[derive(Clone, Copy, Debug, Default)]
pub struct QuarantineSummary {
    /// Number of sources in quarantine state.
    pub quarantined_count: u32,
    /// Number of sources at risk (warn).
    pub at_risk_count: u32,
    /// Number of blocked sources.
    pub blocked_count: u32,
    /// Total tracked sources.
    pub total_sources: u32,
    /// Sources in healthy state.
    pub healthy_count: u32,
}

/// Simplified advisory level for report output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdvisoryLevel {
    /// Source is healthy.
    Allow,
    /// Source has warnings but is usable.
    Warn,
    /// Source is quarantined (requires validation).
    Quarantine,
    /// Source is blocked.
    Block,
}

impl AdvisoryLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Warn => "warn",
            Self::Quarantine => "quarantine",
            Self::Block => "block",
        }
    }

    #[must_use]
    pub const fn is_problematic(self) -> bool {
        matches!(self, Self::Warn | Self::Quarantine | Self::Block)
    }
}

impl From<&TrustAdvisory> for AdvisoryLevel {
    fn from(advisory: &TrustAdvisory) -> Self {
        match advisory {
            TrustAdvisory::Allow { .. } => Self::Allow,
            TrustAdvisory::Warn { .. } => Self::Warn,
            TrustAdvisory::Quarantine { .. } => Self::Quarantine,
            TrustAdvisory::Block { .. } => Self::Block,
        }
    }
}

impl QuarantineReport {
    /// Gather quarantine report from current trust state.
    ///
    /// In the current implementation, this returns a placeholder report since
    /// no persistent source tracking exists yet. Once EE-278's storage is wired,
    /// this will query the feedback_events table.
    #[must_use]
    pub fn gather() -> Self {
        Self::gather_with_sources(&[])
    }

    /// Gather quarantine report from provided source states.
    #[must_use]
    pub fn gather_with_sources(sources: &[SourceTrustState]) -> Self {
        let calculator = TrustDecayCalculator::new();
        let _config = DecayConfig::default();

        let mut quarantined_sources = Vec::new();
        let mut at_risk_sources = Vec::new();
        let mut blocked_sources = Vec::new();

        let mut quarantined_count = 0u32;
        let mut at_risk_count = 0u32;
        let mut blocked_count = 0u32;
        let mut healthy_count = 0u32;

        for state in sources {
            let advisory = calculator.advisory(state);
            let level = AdvisoryLevel::from(&advisory);
            let effective_trust = calculator.effective_trust(state);
            let decay_factor = calculator.calculate_decay_factor(state);

            let entry = QuarantineEntry {
                source_id: state.source_id.clone(),
                advisory: level,
                effective_trust,
                decay_factor,
                negative_rate: state.negative_rate(),
                negative_count: state.negative_signal_count(),
                total_imports: state.total_imports,
                message: advisory_message(&advisory),
                permits_import: advisory.permits_import(),
                requires_validation: advisory.requires_validation(),
            };

            match level {
                AdvisoryLevel::Allow => {
                    healthy_count = healthy_count.saturating_add(1);
                }
                AdvisoryLevel::Warn => {
                    at_risk_count = at_risk_count.saturating_add(1);
                    at_risk_sources.push(entry);
                }
                AdvisoryLevel::Quarantine => {
                    quarantined_count = quarantined_count.saturating_add(1);
                    quarantined_sources.push(entry);
                }
                AdvisoryLevel::Block => {
                    blocked_count = blocked_count.saturating_add(1);
                    blocked_sources.push(entry);
                }
            }
        }

        #[allow(clippy::cast_possible_truncation)]
        let total_sources = sources.len() as u32;

        Self {
            version: env!("CARGO_PKG_VERSION"),
            quarantined_sources,
            at_risk_sources,
            blocked_sources,
            summary: QuarantineSummary {
                quarantined_count,
                at_risk_count,
                blocked_count,
                total_sources,
                healthy_count,
            },
        }
    }

    /// Whether any sources require attention.
    #[must_use]
    pub fn has_issues(&self) -> bool {
        self.summary.quarantined_count > 0
            || self.summary.at_risk_count > 0
            || self.summary.blocked_count > 0
    }

    /// Total number of sources requiring attention.
    #[must_use]
    pub fn issue_count(&self) -> u32 {
        self.summary
            .quarantined_count
            .saturating_add(self.summary.at_risk_count)
            .saturating_add(self.summary.blocked_count)
    }
}

fn advisory_message(advisory: &TrustAdvisory) -> String {
    match advisory {
        TrustAdvisory::Allow { effective_trust } => {
            format!("Source is healthy with effective trust {effective_trust:.2}")
        }
        TrustAdvisory::Warn {
            effective_trust,
            message,
            ..
        } => format!("Warning (trust {effective_trust:.2}): {message}"),
        TrustAdvisory::Quarantine {
            effective_trust,
            negative_rate,
            ..
        } => format!(
            "Quarantined: trust {effective_trust:.2}, negative rate {:.0}%",
            negative_rate * 100.0
        ),
        TrustAdvisory::Block { reason } => format!("Blocked: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TrustClass;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn empty_sources_produces_empty_report() -> TestResult {
        let report = QuarantineReport::gather_with_sources(&[]);

        ensure(report.summary.total_sources, 0, "total sources")?;
        ensure(report.summary.quarantined_count, 0, "quarantined count")?;
        ensure(report.summary.blocked_count, 0, "blocked count")?;
        ensure(report.has_issues(), false, "no issues")
    }

    #[test]
    fn healthy_source_not_in_issues() -> TestResult {
        let state =
            SourceTrustState::new("clean_source").with_trust_class(TrustClass::HumanExplicit);

        let report = QuarantineReport::gather_with_sources(&[state]);

        ensure(report.summary.healthy_count, 1, "healthy count")?;
        ensure(report.summary.quarantined_count, 0, "quarantined count")?;
        ensure(report.has_issues(), false, "no issues")
    }

    #[test]
    fn quarantined_source_appears_in_report() -> TestResult {
        let mut state =
            SourceTrustState::new("bad_source").with_trust_class(TrustClass::CassEvidence);
        state.record_quarantine();
        state.record_quarantine();
        state.record_contradiction();
        state.total_imports = 5;

        let report = QuarantineReport::gather_with_sources(&[state]);

        ensure(report.has_issues(), true, "has issues")?;
        ensure(
            report.summary.quarantined_count > 0 || report.summary.at_risk_count > 0,
            true,
            "source flagged",
        )
    }

    #[test]
    fn blocked_source_appears_in_blocked_list() -> TestResult {
        let mut state = SourceTrustState::new("terrible_source");
        for _ in 0..10 {
            state.record_harmful();
        }

        let report = QuarantineReport::gather_with_sources(&[state]);

        ensure(report.summary.blocked_count, 1, "blocked count")?;
        ensure(report.blocked_sources.len(), 1, "blocked list")?;
        ensure(
            report.blocked_sources[0].permits_import,
            false,
            "import not permitted",
        )
    }

    #[test]
    fn advisory_level_strings_are_stable() -> TestResult {
        ensure(AdvisoryLevel::Allow.as_str(), "allow", "allow")?;
        ensure(AdvisoryLevel::Warn.as_str(), "warn", "warn")?;
        ensure(
            AdvisoryLevel::Quarantine.as_str(),
            "quarantine",
            "quarantine",
        )?;
        ensure(AdvisoryLevel::Block.as_str(), "block", "block")
    }

    #[test]
    fn advisory_level_is_problematic_classification() -> TestResult {
        ensure(
            AdvisoryLevel::Allow.is_problematic(),
            false,
            "allow not problematic",
        )?;
        ensure(
            AdvisoryLevel::Warn.is_problematic(),
            true,
            "warn is problematic",
        )?;
        ensure(
            AdvisoryLevel::Quarantine.is_problematic(),
            true,
            "quarantine is problematic",
        )?;
        ensure(
            AdvisoryLevel::Block.is_problematic(),
            true,
            "block is problematic",
        )
    }

    #[test]
    fn issue_count_sums_all_problematic() -> TestResult {
        let mut state1 = SourceTrustState::new("warn_source");
        state1.record_quarantine();
        state1.record_contradiction();

        let mut state2 = SourceTrustState::new("blocked_source");
        for _ in 0..10 {
            state2.record_harmful();
        }

        let report = QuarantineReport::gather_with_sources(&[state1, state2]);

        ensure(report.issue_count() >= 2, true, "at least 2 issues")
    }

    #[test]
    fn version_matches_package() -> TestResult {
        let report = QuarantineReport::gather();
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }

    #[test]
    fn gather_produces_valid_placeholder() -> TestResult {
        let report = QuarantineReport::gather();
        ensure(
            report.summary.total_sources,
            0,
            "placeholder has no sources",
        )?;
        ensure(report.has_issues(), false, "placeholder has no issues")
    }
}
