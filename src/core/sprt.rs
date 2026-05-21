//! Wald sequential probability ratio test helpers.

/// Harmful-feedback rate treated as benign by the quarantine SPRT.
pub const SPRT_BENIGN_HARMFUL_RATE: f64 = 0.1;
/// Harmful-feedback rate treated as bad by the quarantine SPRT.
pub const SPRT_BAD_HARMFUL_RATE: f64 = 0.4;
/// False-positive budget for the harmful-feedback quarantine SPRT.
pub const SPRT_ALPHA: f64 = 0.01;
/// False-negative budget for the harmful-feedback quarantine SPRT.
pub const SPRT_BETA: f64 = 0.05;

/// One classified feedback observation for the quarantine SPRT.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SprtObservation {
    /// A harmful outcome signal.
    Harmful,
    /// A helpful outcome signal.
    Helpful,
}

/// Terminal or continuing SPRT decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SprtDecision {
    /// Not enough evidence to accept either hypothesis.
    Continue,
    /// Accept the benign hypothesis.
    Release,
    /// Accept the bad-source hypothesis.
    Quarantine,
}

impl SprtDecision {
    /// Stable string for audit rows and JSON payloads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Release => "release",
            Self::Quarantine => "quarantine",
        }
    }
}

/// Evaluation result for one source stream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SprtEvaluation {
    /// Log likelihood ratio accumulated from observations.
    pub statistic: f64,
    /// Wald upper threshold A; crossing it quarantines the source.
    pub upper_bound: f64,
    /// Wald lower threshold B; crossing it accepts the benign hypothesis.
    pub lower_bound: f64,
    /// Number of classified events in the stream.
    pub event_count: usize,
    /// Number of harmful observations.
    pub harmful_count: usize,
    /// Number of helpful observations.
    pub helpful_count: usize,
    /// Decision induced by the statistic and thresholds.
    pub decision: SprtDecision,
}

/// Evaluate Wald's SPRT for the configured harmful-feedback quarantine rates.
#[must_use]
pub fn evaluate_sprt<I>(observations: I) -> SprtEvaluation
where
    I: IntoIterator<Item = SprtObservation>,
{
    let upper_bound = ((1.0 - SPRT_BETA) / SPRT_ALPHA).ln();
    let lower_bound = (SPRT_BETA / (1.0 - SPRT_ALPHA)).ln();
    let harmful_increment = (SPRT_BAD_HARMFUL_RATE / SPRT_BENIGN_HARMFUL_RATE).ln();
    let helpful_increment = ((1.0 - SPRT_BAD_HARMFUL_RATE) / (1.0 - SPRT_BENIGN_HARMFUL_RATE)).ln();

    let mut statistic = 0.0;
    let mut event_count = 0;
    let mut harmful_count = 0;
    let mut helpful_count = 0;

    for observation in observations {
        event_count += 1;
        match observation {
            SprtObservation::Harmful => {
                harmful_count += 1;
                statistic += harmful_increment;
            }
            SprtObservation::Helpful => {
                helpful_count += 1;
                statistic += helpful_increment;
            }
        }
    }

    let decision = if statistic > upper_bound {
        SprtDecision::Quarantine
    } else if statistic < lower_bound {
        SprtDecision::Release
    } else {
        SprtDecision::Continue
    };

    SprtEvaluation {
        statistic,
        upper_bound,
        lower_bound,
        event_count,
        harmful_count,
        helpful_count,
        decision,
    }
}

#[cfg(test)]
mod tests {
    use super::{SprtDecision, SprtObservation, evaluate_sprt};

    #[test]
    fn harmful_stream_crosses_quarantine_threshold_quickly() {
        let evaluation = evaluate_sprt([SprtObservation::Harmful; 4]);

        assert_eq!(evaluation.event_count, 4);
        assert_eq!(evaluation.harmful_count, 4);
        assert_eq!(evaluation.decision, SprtDecision::Quarantine);
        assert!(evaluation.statistic > evaluation.upper_bound);
    }

    #[test]
    fn helpful_stream_crosses_release_threshold() {
        let evaluation = evaluate_sprt([SprtObservation::Helpful; 8]);

        assert_eq!(evaluation.helpful_count, 8);
        assert_eq!(evaluation.decision, SprtDecision::Release);
        assert!(evaluation.statistic < evaluation.lower_bound);
    }
}
