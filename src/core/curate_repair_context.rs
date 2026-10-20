//! Refutation boundaries for source-proven recovery episodes.
//!
//! A failed attempt earlier in a conversation is not a retraction of a later
//! repair. Only native completed-invocation proofs may use this distinction;
//! prose-derived lessons retain their whole-session refutation policy.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Refutation {
    FailedAttempt,
    Retraction,
}

/// Keep the existing conservative marker vocabulary. Explicit withdrawal of
/// advice remains global, including when the same turn also mentions a failed
/// attempt. No sentiment inference or approximate text matching grants proof.
pub(super) fn classify(text: &str) -> Option<Refutation> {
    let lower = text.to_ascii_lowercase().replace('’', "'");
    if [
        "this rule is wrong",
        "this advice is wrong",
        "incorrect advice",
        "do not follow",
        "don't follow",
        "retract this",
        "retracted",
        "refuted",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return Some(Refutation::Retraction);
    }
    [
        "failed again",
        "did not fix",
        "didn't fix",
        "does not fix",
        "doesn't fix",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    .then_some(Refutation::FailedAttempt)
}

/// One request-local summary over the complete admitted source transcript.
/// Both source order and line ranges must put every failed-attempt notice
/// strictly before every contributing repair. Overlapping windows do not
/// establish a causal order; an unresolved retraction never authorizes recovery.
#[derive(Default)]
pub(super) struct RefutationBoundary {
    retracted: bool,
    last_position: Option<usize>,
    last_end_line: u32,
}

impl RefutationBoundary {
    pub(super) fn observe(&mut self, position: usize, end_line: u32, text: &str) {
        match classify(text) {
            Some(Refutation::Retraction) => self.retracted = true,
            Some(Refutation::FailedAttempt) => {
                self.last_position =
                    Some(self.last_position.map_or(position, |old| old.max(position)));
                self.last_end_line = self.last_end_line.max(end_line);
            }
            None => {}
        }
    }

    pub(super) fn permits_source(&self, position: usize, start_line: u32) -> bool {
        !self.retracted
            && self
                .last_position
                .is_none_or(|last| last < position && self.last_end_line < start_line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_failed_attempts_from_withdrawn_advice() {
        for text in [
            "The earlier attempt did not fix E0277.",
            "That didn’t fix the trait bound.",
            "The previous build FAILED AGAIN.",
            "The old implementation doesn't fix this case.",
        ] {
            assert_eq!(classify(text), Some(Refutation::FailedAttempt));
        }
        for text in [
            "This rule is wrong.",
            "Do not follow this advice; the attempt did not fix it.",
            "The advice was retracted.",
            "The proposed workaround was refuted.",
        ] {
            assert_eq!(classify(text), Some(Refutation::Retraction));
        }
        assert_eq!(
            classify("The build passed after the concrete repair."),
            None
        );
    }

    #[test]
    fn recovery_requires_both_strict_order_and_nonoverlapping_lines() {
        let mut boundary = RefutationBoundary::default();
        boundary.observe(3, 8, "The earlier attempt did not fix E0277.");
        assert!(!boundary.permits_source(2, 9));
        assert!(!boundary.permits_source(3, 9));
        assert!(!boundary.permits_source(4, 8));
        assert!(!boundary.permits_source(4, 7));
        assert!(boundary.permits_source(4, 9));
    }

    #[test]
    fn a_later_failed_attempt_stops_an_earlier_repair_from_being_reused() {
        let mut boundary = RefutationBoundary::default();
        boundary.observe(2, 3, "That did not fix E0277.");
        assert!(boundary.permits_source(3, 4));
        boundary.observe(6, 7, "The build failed again.");
        assert!(!boundary.permits_source(3, 4));
        assert!(boundary.permits_source(7, 8));
    }

    #[test]
    fn retractions_are_not_overridden_by_later_success_or_narration() {
        let mut boundary = RefutationBoundary::default();
        boundary.observe(0, 1, "This advice is wrong.");
        boundary.observe(1, 2, "The earlier attempt did not fix E0277.");
        boundary.observe(3, 4, "The new command succeeded.");
        assert!(!boundary.permits_source(4, 5));
        assert!(!boundary.permits_source(usize::MAX, u32::MAX));
    }

    #[test]
    fn notices_are_combined_without_assuming_their_ranges_are_sorted() {
        let mut forward = RefutationBoundary::default();
        forward.observe(2, 20, "That did not fix E0277.");
        forward.observe(8, 9, "The build failed again.");
        let mut reverse = RefutationBoundary::default();
        reverse.observe(8, 9, "The build failed again.");
        reverse.observe(2, 20, "That did not fix E0277.");
        for position in 0..12 {
            for line in 0..25 {
                assert_eq!(
                    forward.permits_source(position, line),
                    reverse.permits_source(position, line)
                );
                assert_eq!(
                    forward.permits_source(position, line),
                    position > 8 && line > 20
                );
            }
        }
    }

    #[test]
    fn clean_context_preserves_existing_sources_and_maximum_boundaries_do_not_wrap() {
        let mut clean = RefutationBoundary::default();
        clean.observe(0, 1, "The implementation now uses the correct trait.");
        assert!(clean.permits_source(0, 1));
        clean.observe(usize::MAX, u32::MAX, "The build failed again.");
        assert!(!clean.permits_source(usize::MAX, u32::MAX));
    }
}
