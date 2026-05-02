//! Procedural rule domain types (EE-084).
//!
//! Procedural rules are distilled lessons, patterns, and policies from
//! experience. They have lifecycle (maturity), scope (where they apply),
//! and feedback tracking.

use std::fmt;
use std::str::FromStr;

/// Scope of a procedural rule - where it applies.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuleScope {
    /// Applies to all workspaces.
    Global,
    /// Applies to the current workspace.
    Workspace,
    /// Applies to a specific project within workspace.
    Project,
    /// Applies to a specific directory pattern.
    Directory,
    /// Applies to files matching a pattern.
    FilePattern,
}

impl RuleScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
            Self::Project => "project",
            Self::Directory => "directory",
            Self::FilePattern => "file_pattern",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Global,
            Self::Workspace,
            Self::Project,
            Self::Directory,
            Self::FilePattern,
        ]
    }

    #[must_use]
    pub const fn requires_pattern(self) -> bool {
        matches!(self, Self::Directory | Self::FilePattern)
    }
}

impl fmt::Display for RuleScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid rule scope string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRuleScopeError {
    input: String,
}

impl ParseRuleScopeError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseRuleScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown rule scope `{}`; expected one of global, workspace, project, directory, file_pattern",
            self.input
        )
    }
}

impl std::error::Error for ParseRuleScopeError {}

impl FromStr for RuleScope {
    type Err = ParseRuleScopeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "global" => Ok(Self::Global),
            "workspace" => Ok(Self::Workspace),
            "project" => Ok(Self::Project),
            "directory" => Ok(Self::Directory),
            "file_pattern" => Ok(Self::FilePattern),
            _ => Err(ParseRuleScopeError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Maturity level of a procedural rule.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuleMaturity {
    /// Newly created, not yet reviewed.
    Draft,
    /// Proposed for validation.
    Candidate,
    /// Validated by positive outcomes.
    Validated,
    /// No longer recommended but kept for history.
    Deprecated,
    /// Replaced by another rule.
    Superseded,
}

impl RuleMaturity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Candidate => "candidate",
            Self::Validated => "validated",
            Self::Deprecated => "deprecated",
            Self::Superseded => "superseded",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Draft,
            Self::Candidate,
            Self::Validated,
            Self::Deprecated,
            Self::Superseded,
        ]
    }

    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Draft | Self::Candidate | Self::Validated)
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Deprecated | Self::Superseded)
    }
}

impl fmt::Display for RuleMaturity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid rule maturity string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRuleMaturityError {
    input: String,
}

impl ParseRuleMaturityError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseRuleMaturityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown rule maturity `{}`; expected one of draft, candidate, validated, deprecated, superseded",
            self.input
        )
    }
}

impl std::error::Error for ParseRuleMaturityError {}

impl FromStr for RuleMaturity {
    type Err = ParseRuleMaturityError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "draft" => Ok(Self::Draft),
            "candidate" => Ok(Self::Candidate),
            "validated" => Ok(Self::Validated),
            "deprecated" => Ok(Self::Deprecated),
            "superseded" => Ok(Self::Superseded),
            _ => Err(ParseRuleMaturityError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Event that asks the rule lifecycle planner to evaluate a transition.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuleLifecycleTrigger {
    /// Move a draft rule into the validation queue.
    ProposeValidation,
    /// Record that the rule helped an observed outcome.
    OutcomeHelpful,
    /// Record that the rule harmed or wasted an observed outcome.
    OutcomeHarmful,
    /// Record validation evidence for the rule.
    ValidationPassed,
    /// Record that validation contradicted the rule.
    ValidationContradicted,
    /// Record explicit review approval.
    ReviewApproved,
    /// Explicitly deprecate the rule.
    Deprecate,
    /// Replace the rule with another rule.
    Supersede,
}

impl RuleLifecycleTrigger {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProposeValidation => "propose_validation",
            Self::OutcomeHelpful => "outcome_helpful",
            Self::OutcomeHarmful => "outcome_harmful",
            Self::ValidationPassed => "validation_passed",
            Self::ValidationContradicted => "validation_contradicted",
            Self::ReviewApproved => "review_approved",
            Self::Deprecate => "deprecate",
            Self::Supersede => "supersede",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 8] {
        [
            Self::ProposeValidation,
            Self::OutcomeHelpful,
            Self::OutcomeHarmful,
            Self::ValidationPassed,
            Self::ValidationContradicted,
            Self::ReviewApproved,
            Self::Deprecate,
            Self::Supersede,
        ]
    }
}

impl fmt::Display for RuleLifecycleTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid rule lifecycle trigger string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRuleLifecycleTriggerError {
    input: String,
}

impl ParseRuleLifecycleTriggerError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseRuleLifecycleTriggerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown rule lifecycle trigger `{}`; expected one of propose_validation, outcome_helpful, outcome_harmful, validation_passed, validation_contradicted, review_approved, deprecate, supersede",
            self.input
        )
    }
}

impl std::error::Error for ParseRuleLifecycleTriggerError {}

impl FromStr for RuleLifecycleTrigger {
    type Err = ParseRuleLifecycleTriggerError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "propose_validation" => Ok(Self::ProposeValidation),
            "outcome_helpful" => Ok(Self::OutcomeHelpful),
            "outcome_harmful" => Ok(Self::OutcomeHarmful),
            "validation_passed" => Ok(Self::ValidationPassed),
            "validation_contradicted" => Ok(Self::ValidationContradicted),
            "review_approved" => Ok(Self::ReviewApproved),
            "deprecate" => Ok(Self::Deprecate),
            "supersede" => Ok(Self::Supersede),
            _ => Err(ParseRuleLifecycleTriggerError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Planned lifecycle action produced by the transition evaluator.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuleLifecycleAction {
    /// Keep the current maturity while recording evidence.
    Retain,
    /// Move to a stronger maturity state.
    Promote,
    /// Move to a weaker maturity state.
    Demote,
    /// Retire the rule but retain it for history.
    Deprecate,
    /// Replace the rule with another rule.
    Supersede,
    /// Reject the requested transition.
    Reject,
}

impl RuleLifecycleAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Retain => "retain",
            Self::Promote => "promote",
            Self::Demote => "demote",
            Self::Deprecate => "deprecate",
            Self::Supersede => "supersede",
            Self::Reject => "reject",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::Retain,
            Self::Promote,
            Self::Demote,
            Self::Deprecate,
            Self::Supersede,
            Self::Reject,
        ]
    }
}

impl fmt::Display for RuleLifecycleAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid rule lifecycle action string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRuleLifecycleActionError {
    input: String,
}

impl ParseRuleLifecycleActionError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseRuleLifecycleActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown rule lifecycle action `{}`; expected one of retain, promote, demote, deprecate, supersede, reject",
            self.input
        )
    }
}

impl std::error::Error for ParseRuleLifecycleActionError {}

impl FromStr for RuleLifecycleAction {
    type Err = ParseRuleLifecycleActionError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "retain" => Ok(Self::Retain),
            "promote" => Ok(Self::Promote),
            "demote" => Ok(Self::Demote),
            "deprecate" => Ok(Self::Deprecate),
            "supersede" => Ok(Self::Supersede),
            "reject" => Ok(Self::Reject),
            _ => Err(ParseRuleLifecycleActionError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Evidence available to a rule lifecycle transition.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuleLifecycleEvidence {
    pub helpful_outcomes: u32,
    pub harmful_outcomes: u32,
    pub distinct_harmful_sources: u32,
    pub validation_passes: u32,
    pub validation_contradictions: u32,
    pub review_approved: bool,
    pub superseding_rule_id: Option<String>,
}

impl RuleLifecycleEvidence {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            helpful_outcomes: 0,
            harmful_outcomes: 0,
            distinct_harmful_sources: 0,
            validation_passes: 0,
            validation_contradictions: 0,
            review_approved: false,
            superseding_rule_id: None,
        }
    }

    #[must_use]
    pub fn with_helpful_outcomes(mut self, count: u32) -> Self {
        self.helpful_outcomes = count;
        self
    }

    #[must_use]
    pub fn with_harmful_outcomes(mut self, count: u32, distinct_sources: u32) -> Self {
        self.harmful_outcomes = count;
        self.distinct_harmful_sources = distinct_sources;
        self
    }

    #[must_use]
    pub fn with_validation_passes(mut self, count: u32) -> Self {
        self.validation_passes = count;
        self
    }

    #[must_use]
    pub fn with_validation_contradictions(mut self, count: u32) -> Self {
        self.validation_contradictions = count;
        self
    }

    #[must_use]
    pub fn with_review_approved(mut self, approved: bool) -> Self {
        self.review_approved = approved;
        self
    }

    #[must_use]
    pub fn with_superseding_rule(mut self, rule_id: impl Into<String>) -> Self {
        self.superseding_rule_id = Some(rule_id.into());
        self
    }

    #[must_use]
    pub const fn has_validation_and_helpful_outcome(&self) -> bool {
        self.validation_passes > 0 && self.helpful_outcomes > 0
    }

    #[must_use]
    pub const fn has_harmful_quorum(&self) -> bool {
        self.harmful_outcomes >= 2 && self.distinct_harmful_sources >= 2
    }

    #[must_use]
    pub fn has_superseding_rule(&self) -> bool {
        self.superseding_rule_id
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty())
    }
}

#[derive(Clone, Copy, Debug)]
struct RuleLifecycleAllowed {
    next_maturity: RuleMaturity,
    action: RuleLifecycleAction,
    requires_curation: bool,
    confidence_delta: f64,
    utility_delta: f64,
    reason: &'static str,
}

impl RuleLifecycleAllowed {
    const fn new(
        next_maturity: RuleMaturity,
        action: RuleLifecycleAction,
        reason: &'static str,
    ) -> Self {
        Self {
            next_maturity,
            action,
            requires_curation: false,
            confidence_delta: 0.0,
            utility_delta: 0.0,
            reason,
        }
    }

    const fn requiring_curation(mut self) -> Self {
        self.requires_curation = true;
        self
    }

    const fn with_deltas(mut self, confidence_delta: f64, utility_delta: f64) -> Self {
        self.confidence_delta = confidence_delta;
        self.utility_delta = utility_delta;
        self
    }
}

/// Pure transition plan for procedural rule lifecycle changes.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleLifecycleTransition {
    pub prior_maturity: RuleMaturity,
    pub next_maturity: RuleMaturity,
    pub trigger: RuleLifecycleTrigger,
    pub action: RuleLifecycleAction,
    pub allowed: bool,
    pub requires_curation: bool,
    pub audit_required: bool,
    pub confidence_delta: f64,
    pub utility_delta: f64,
    pub reason: String,
}

impl RuleLifecycleTransition {
    #[must_use]
    pub fn evaluate(
        prior_maturity: RuleMaturity,
        trigger: RuleLifecycleTrigger,
        evidence: &RuleLifecycleEvidence,
    ) -> Self {
        if prior_maturity.is_terminal() && trigger != RuleLifecycleTrigger::Supersede {
            return Self::reject(
                prior_maturity,
                trigger,
                "terminal rule states require an explicit supersession transition",
            );
        }

        match trigger {
            RuleLifecycleTrigger::ProposeValidation => {
                if prior_maturity == RuleMaturity::Draft {
                    Self::allowed(
                        prior_maturity,
                        trigger,
                        RuleLifecycleAllowed::new(
                            RuleMaturity::Candidate,
                            RuleLifecycleAction::Promote,
                            "draft rule moved into the validation queue",
                        ),
                    )
                } else {
                    Self::reject(
                        prior_maturity,
                        trigger,
                        "only draft rules can be proposed for validation",
                    )
                }
            }
            RuleLifecycleTrigger::OutcomeHelpful => Self::allowed(
                prior_maturity,
                trigger,
                RuleLifecycleAllowed::new(
                    prior_maturity,
                    RuleLifecycleAction::Retain,
                    "helpful outcome adjusts confidence and utility without silent promotion",
                )
                .with_deltas(0.04, 0.08),
            ),
            RuleLifecycleTrigger::OutcomeHarmful => {
                if evidence.has_harmful_quorum() {
                    Self::allowed(
                        prior_maturity,
                        trigger,
                        RuleLifecycleAllowed::new(
                            RuleMaturity::Deprecated,
                            RuleLifecycleAction::Deprecate,
                            "harmful outcomes from distinct sources require curation before deprecation or inversion",
                        )
                        .requiring_curation()
                        .with_deltas(-0.10, -0.12),
                    )
                } else {
                    Self::allowed(
                        prior_maturity,
                        trigger,
                        RuleLifecycleAllowed::new(
                            prior_maturity,
                            RuleLifecycleAction::Retain,
                            "harmful outcome is recorded, but distinct-source quorum is not met",
                        )
                        .with_deltas(-0.10, -0.12),
                    )
                }
            }
            RuleLifecycleTrigger::ValidationPassed => {
                if prior_maturity == RuleMaturity::Draft {
                    Self::allowed(
                        prior_maturity,
                        trigger,
                        RuleLifecycleAllowed::new(
                            RuleMaturity::Candidate,
                            RuleLifecycleAction::Promote,
                            "validation evidence promotes a draft only into curation-backed candidacy",
                        )
                        .requiring_curation()
                        .with_deltas(0.06, 0.04),
                    )
                } else if prior_maturity == RuleMaturity::Candidate
                    && evidence.has_validation_and_helpful_outcome()
                    && evidence.review_approved
                {
                    Self::allowed(
                        prior_maturity,
                        trigger,
                        RuleLifecycleAllowed::new(
                            RuleMaturity::Validated,
                            RuleLifecycleAction::Promote,
                            "candidate has helpful outcome, validation evidence, and explicit review approval",
                        )
                        .with_deltas(0.06, 0.04),
                    )
                } else if prior_maturity == RuleMaturity::Candidate
                    && evidence.has_validation_and_helpful_outcome()
                {
                    Self::allowed(
                        prior_maturity,
                        trigger,
                        RuleLifecycleAllowed::new(
                            prior_maturity,
                            RuleLifecycleAction::Retain,
                            "candidate has evidence but still needs explicit review before validation",
                        )
                        .requiring_curation()
                        .with_deltas(0.06, 0.04),
                    )
                } else {
                    Self::allowed(
                        prior_maturity,
                        trigger,
                        RuleLifecycleAllowed::new(
                            prior_maturity,
                            RuleLifecycleAction::Retain,
                            "validation evidence is incomplete for maturity promotion",
                        )
                        .requiring_curation()
                        .with_deltas(0.06, 0.04),
                    )
                }
            }
            RuleLifecycleTrigger::ValidationContradicted => {
                let next = if prior_maturity == RuleMaturity::Validated {
                    RuleMaturity::Candidate
                } else {
                    prior_maturity
                };
                let action = if next == prior_maturity {
                    RuleLifecycleAction::Retain
                } else {
                    RuleLifecycleAction::Demote
                };
                Self::allowed(
                    prior_maturity,
                    trigger,
                    RuleLifecycleAllowed::new(
                        next,
                        action,
                        "contradictory validation evidence weakens the rule and demotes validated rules to candidacy",
                    )
                    .with_deltas(-0.08, -0.05),
                )
            }
            RuleLifecycleTrigger::ReviewApproved => {
                if prior_maturity == RuleMaturity::Candidate
                    && evidence.has_validation_and_helpful_outcome()
                {
                    Self::allowed(
                        prior_maturity,
                        trigger,
                        RuleLifecycleAllowed::new(
                            RuleMaturity::Validated,
                            RuleLifecycleAction::Promote,
                            "explicit review approval completes the candidate-to-validated transition",
                        ),
                    )
                } else {
                    Self::reject(
                        prior_maturity,
                        trigger,
                        "review approval requires candidate maturity plus helpful and validation evidence",
                    )
                }
            }
            RuleLifecycleTrigger::Deprecate => Self::allowed(
                prior_maturity,
                trigger,
                RuleLifecycleAllowed::new(
                    RuleMaturity::Deprecated,
                    RuleLifecycleAction::Deprecate,
                    "explicit deprecation retires the rule while preserving history",
                )
                .with_deltas(0.0, -0.05),
            ),
            RuleLifecycleTrigger::Supersede => {
                if evidence.has_superseding_rule() {
                    Self::allowed(
                        prior_maturity,
                        trigger,
                        RuleLifecycleAllowed::new(
                            RuleMaturity::Superseded,
                            RuleLifecycleAction::Supersede,
                            "explicit supersession replaces the rule with a newer rule",
                        )
                        .with_deltas(0.0, -0.03),
                    )
                } else {
                    Self::reject(
                        prior_maturity,
                        trigger,
                        "supersession requires a non-empty superseding rule id",
                    )
                }
            }
        }
    }

    #[must_use]
    pub fn changes_maturity(&self) -> bool {
        self.prior_maturity != self.next_maturity
    }

    #[must_use]
    pub fn is_demotion(&self) -> bool {
        matches!(
            self.action,
            RuleLifecycleAction::Demote | RuleLifecycleAction::Deprecate
        )
    }

    fn allowed(
        prior_maturity: RuleMaturity,
        trigger: RuleLifecycleTrigger,
        decision: RuleLifecycleAllowed,
    ) -> Self {
        Self {
            prior_maturity,
            next_maturity: decision.next_maturity,
            trigger,
            action: decision.action,
            allowed: true,
            requires_curation: decision.requires_curation,
            audit_required: true,
            confidence_delta: decision.confidence_delta,
            utility_delta: decision.utility_delta,
            reason: decision.reason.to_owned(),
        }
    }

    fn reject(
        prior_maturity: RuleMaturity,
        trigger: RuleLifecycleTrigger,
        reason: &'static str,
    ) -> Self {
        Self {
            prior_maturity,
            next_maturity: prior_maturity,
            trigger,
            action: RuleLifecycleAction::Reject,
            allowed: false,
            requires_curation: false,
            audit_required: true,
            confidence_delta: 0.0,
            utility_delta: 0.0,
            reason: reason.to_owned(),
        }
    }
}

impl RuleMaturity {
    #[must_use]
    pub fn evaluate_lifecycle_transition(
        self,
        trigger: RuleLifecycleTrigger,
        evidence: &RuleLifecycleEvidence,
    ) -> RuleLifecycleTransition {
        RuleLifecycleTransition::evaluate(self, trigger, evidence)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        ParseRuleLifecycleActionError, ParseRuleLifecycleTriggerError, ParseRuleMaturityError,
        ParseRuleScopeError, RuleLifecycleAction, RuleLifecycleEvidence, RuleLifecycleTransition,
        RuleLifecycleTrigger, RuleMaturity, RuleScope,
    };

    #[test]
    fn rule_scope_round_trip_for_every_variant() {
        for scope in RuleScope::all() {
            let rendered = scope.to_string();
            assert_eq!(RuleScope::from_str(&rendered), Ok(scope));
        }
    }

    #[test]
    fn rule_scope_rejects_unknown_input() {
        let err = RuleScope::from_str("unknown_scope");
        assert!(matches!(err, Err(ParseRuleScopeError { .. })));
    }

    #[test]
    fn rule_scope_requires_pattern() {
        assert!(!RuleScope::Global.requires_pattern());
        assert!(!RuleScope::Workspace.requires_pattern());
        assert!(!RuleScope::Project.requires_pattern());
        assert!(RuleScope::Directory.requires_pattern());
        assert!(RuleScope::FilePattern.requires_pattern());
    }

    #[test]
    fn rule_maturity_round_trip_for_every_variant() {
        for maturity in RuleMaturity::all() {
            let rendered = maturity.to_string();
            assert_eq!(RuleMaturity::from_str(&rendered), Ok(maturity));
        }
    }

    #[test]
    fn rule_maturity_rejects_unknown_input() {
        let err = RuleMaturity::from_str("unknown_maturity");
        assert!(matches!(err, Err(ParseRuleMaturityError { .. })));
    }

    #[test]
    fn rule_maturity_active_and_terminal_states() {
        assert!(RuleMaturity::Draft.is_active());
        assert!(RuleMaturity::Candidate.is_active());
        assert!(RuleMaturity::Validated.is_active());
        assert!(!RuleMaturity::Deprecated.is_active());
        assert!(!RuleMaturity::Superseded.is_active());

        assert!(!RuleMaturity::Draft.is_terminal());
        assert!(!RuleMaturity::Candidate.is_terminal());
        assert!(!RuleMaturity::Validated.is_terminal());
        assert!(RuleMaturity::Deprecated.is_terminal());
        assert!(RuleMaturity::Superseded.is_terminal());
    }

    #[test]
    fn rule_lifecycle_trigger_round_trip_for_every_variant() {
        for trigger in RuleLifecycleTrigger::all() {
            let rendered = trigger.to_string();
            assert_eq!(RuleLifecycleTrigger::from_str(&rendered), Ok(trigger));
        }
    }

    #[test]
    fn rule_lifecycle_trigger_rejects_unknown_input() {
        let err = RuleLifecycleTrigger::from_str("unknown_trigger");
        assert!(matches!(err, Err(ParseRuleLifecycleTriggerError { .. })));
    }

    #[test]
    fn rule_lifecycle_action_round_trip_for_every_variant() {
        for action in RuleLifecycleAction::all() {
            let rendered = action.to_string();
            assert_eq!(RuleLifecycleAction::from_str(&rendered), Ok(action));
        }
    }

    #[test]
    fn rule_lifecycle_action_rejects_unknown_input() {
        let err = RuleLifecycleAction::from_str("unknown_action");
        assert!(matches!(err, Err(ParseRuleLifecycleActionError { .. })));
    }

    #[test]
    fn draft_can_be_proposed_for_validation() {
        let transition = RuleLifecycleTransition::evaluate(
            RuleMaturity::Draft,
            RuleLifecycleTrigger::ProposeValidation,
            &RuleLifecycleEvidence::new(),
        );

        assert!(transition.allowed);
        assert_eq!(transition.next_maturity, RuleMaturity::Candidate);
        assert_eq!(transition.action, RuleLifecycleAction::Promote);
        assert!(transition.changes_maturity());
    }

    #[test]
    fn candidate_requires_review_before_validated_transition() {
        let evidence = RuleLifecycleEvidence::new()
            .with_helpful_outcomes(1)
            .with_validation_passes(1);
        let transition = RuleMaturity::Candidate
            .evaluate_lifecycle_transition(RuleLifecycleTrigger::ValidationPassed, &evidence);

        assert!(transition.allowed);
        assert!(transition.requires_curation);
        assert_eq!(transition.next_maturity, RuleMaturity::Candidate);
        assert_eq!(transition.action, RuleLifecycleAction::Retain);
        assert!(!transition.changes_maturity());
    }

    #[test]
    fn candidate_with_review_and_evidence_promotes_to_validated() {
        let evidence = RuleLifecycleEvidence::new()
            .with_helpful_outcomes(2)
            .with_validation_passes(1)
            .with_review_approved(true);
        let transition = RuleLifecycleTransition::evaluate(
            RuleMaturity::Candidate,
            RuleLifecycleTrigger::ValidationPassed,
            &evidence,
        );

        assert!(transition.allowed);
        assert!(!transition.requires_curation);
        assert_eq!(transition.next_maturity, RuleMaturity::Validated);
        assert_eq!(transition.action, RuleLifecycleAction::Promote);
        assert!(transition.confidence_delta > 0.0);
    }

    #[test]
    fn harmful_outcomes_from_distinct_sources_require_curation_deprecation() {
        let evidence = RuleLifecycleEvidence::new().with_harmful_outcomes(2, 2);
        let transition = RuleLifecycleTransition::evaluate(
            RuleMaturity::Validated,
            RuleLifecycleTrigger::OutcomeHarmful,
            &evidence,
        );

        assert!(transition.allowed);
        assert!(transition.requires_curation);
        assert_eq!(transition.next_maturity, RuleMaturity::Deprecated);
        assert_eq!(transition.action, RuleLifecycleAction::Deprecate);
        assert!(transition.is_demotion());
        assert!(transition.utility_delta < 0.0);
    }

    #[test]
    fn single_harmful_outcome_records_delta_without_maturity_change() {
        let evidence = RuleLifecycleEvidence::new().with_harmful_outcomes(1, 1);
        let transition = RuleLifecycleTransition::evaluate(
            RuleMaturity::Candidate,
            RuleLifecycleTrigger::OutcomeHarmful,
            &evidence,
        );

        assert!(transition.allowed);
        assert_eq!(transition.next_maturity, RuleMaturity::Candidate);
        assert_eq!(transition.action, RuleLifecycleAction::Retain);
        assert!(transition.confidence_delta < 0.0);
        assert!(transition.utility_delta < 0.0);
    }

    #[test]
    fn validation_contradiction_demotes_validated_rule_to_candidate() {
        let evidence = RuleLifecycleEvidence::new().with_validation_contradictions(1);
        let transition = RuleLifecycleTransition::evaluate(
            RuleMaturity::Validated,
            RuleLifecycleTrigger::ValidationContradicted,
            &evidence,
        );

        assert!(transition.allowed);
        assert_eq!(transition.next_maturity, RuleMaturity::Candidate);
        assert_eq!(transition.action, RuleLifecycleAction::Demote);
        assert!(transition.is_demotion());
    }

    #[test]
    fn supersede_requires_non_empty_replacement_rule() {
        let missing = RuleLifecycleTransition::evaluate(
            RuleMaturity::Validated,
            RuleLifecycleTrigger::Supersede,
            &RuleLifecycleEvidence::new(),
        );
        assert!(!missing.allowed);
        assert_eq!(missing.action, RuleLifecycleAction::Reject);

        let present = RuleLifecycleTransition::evaluate(
            RuleMaturity::Validated,
            RuleLifecycleTrigger::Supersede,
            &RuleLifecycleEvidence::new().with_superseding_rule("rule_new_release"),
        );
        assert!(present.allowed);
        assert_eq!(present.next_maturity, RuleMaturity::Superseded);
        assert_eq!(present.action, RuleLifecycleAction::Supersede);
    }

    #[test]
    fn terminal_states_reject_non_supersession_transitions() {
        let transition = RuleLifecycleTransition::evaluate(
            RuleMaturity::Deprecated,
            RuleLifecycleTrigger::OutcomeHelpful,
            &RuleLifecycleEvidence::new().with_helpful_outcomes(1),
        );

        assert!(!transition.allowed);
        assert_eq!(transition.next_maturity, RuleMaturity::Deprecated);
        assert_eq!(transition.action, RuleLifecycleAction::Reject);
        assert!(transition.audit_required);
    }
}
