//! Posture summary and structured suggested action model (EE-041).
//!
//! Provides domain types for workspace posture assessment and
//! agent-friendly action suggestions. These types are designed
//! for deterministic JSON output and machine consumption.

/// Overall posture of the workspace.
///
/// Represents the operational readiness state as a tri-state:
/// ready for normal operation, degraded but usable, or needs attention.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Posture {
    /// All systems operational, ready for normal use.
    Ready,
    /// Some features degraded but core functionality works.
    Degraded,
    /// Workspace requires initialization or repair.
    NeedsAttention,
}

impl Posture {
    /// Stable string representation for JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::NeedsAttention => "needs_attention",
        }
    }

    /// Returns true if the workspace is usable (ready or degraded).
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Ready | Self::Degraded)
    }

    /// All posture variants in severity order.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Ready, Self::Degraded, Self::NeedsAttention]
    }
}

impl std::fmt::Display for Posture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Category of a suggested action.
///
/// Groups actions by their purpose to help agents prioritize.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionCategory {
    /// Workspace initialization or setup.
    Setup,
    /// Diagnostic or health check.
    Diagnostic,
    /// Repair or recovery action.
    Repair,
    /// Index rebuild or maintenance.
    Maintenance,
    /// Configuration change.
    Configuration,
}

impl ActionCategory {
    /// Stable string representation for JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Diagnostic => "diagnostic",
            Self::Repair => "repair",
            Self::Maintenance => "maintenance",
            Self::Configuration => "configuration",
        }
    }

    /// All action categories.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Setup,
            Self::Diagnostic,
            Self::Repair,
            Self::Maintenance,
            Self::Configuration,
        ]
    }
}

impl std::fmt::Display for ActionCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A structured suggested action for agents.
///
/// Contains all information needed for an agent to understand,
/// prioritize, and execute a remediation step.
#[derive(Clone, Debug)]
pub struct SuggestedAction {
    /// Priority level (1 = highest, lower is more urgent).
    pub priority: u8,
    /// Category of this action.
    pub category: ActionCategory,
    /// The ee command to run.
    pub command: &'static str,
    /// Human-readable reason for this action.
    pub reason: &'static str,
    /// Whether this action is safe to run automatically.
    pub safe_for_automation: bool,
    /// Whether this action is idempotent (safe to retry).
    pub idempotent: bool,
}

impl SuggestedAction {
    /// Create a new suggested action with common defaults.
    #[must_use]
    pub const fn new(
        priority: u8,
        category: ActionCategory,
        command: &'static str,
        reason: &'static str,
    ) -> Self {
        Self {
            priority,
            category,
            command,
            reason,
            safe_for_automation: true,
            idempotent: true,
        }
    }

    /// Mark this action as not safe for automation.
    #[must_use]
    pub const fn requires_confirmation(mut self) -> Self {
        self.safe_for_automation = false;
        self
    }

    /// Mark this action as not idempotent.
    #[must_use]
    pub const fn not_idempotent(mut self) -> Self {
        self.idempotent = false;
        self
    }
}

/// Summary of workspace posture for quick assessment.
///
/// Provides a snapshot of workspace state with actionable suggestions.
#[derive(Clone, Debug)]
pub struct PostureSummary {
    /// Overall posture state.
    pub posture: Posture,
    /// Number of ready subsystems.
    pub ready_count: u8,
    /// Number of degraded subsystems.
    pub degraded_count: u8,
    /// Number of unavailable subsystems.
    pub unavailable_count: u8,
    /// Ordered list of suggested actions.
    pub suggested_actions: Vec<SuggestedAction>,
}

impl PostureSummary {
    /// Create a new posture summary.
    #[must_use]
    pub fn new(
        posture: Posture,
        ready_count: u8,
        degraded_count: u8,
        unavailable_count: u8,
    ) -> Self {
        Self {
            posture,
            ready_count,
            degraded_count,
            unavailable_count,
            suggested_actions: Vec::new(),
        }
    }

    /// Add a suggested action.
    pub fn add_action(&mut self, action: SuggestedAction) {
        self.suggested_actions.push(action);
    }

    /// Sort actions by priority (lowest number first).
    pub fn sort_actions(&mut self) {
        self.suggested_actions.sort_by_key(|a| a.priority);
    }

    /// Get the most urgent action, if any.
    #[must_use]
    pub fn most_urgent_action(&self) -> Option<&SuggestedAction> {
        self.suggested_actions.iter().min_by_key(|a| a.priority)
    }

    /// Get actions that are safe for automation.
    #[must_use]
    pub fn automatable_actions(&self) -> Vec<&SuggestedAction> {
        self.suggested_actions
            .iter()
            .filter(|a| a.safe_for_automation)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: PartialEq + std::fmt::Debug>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn posture_as_str_is_stable() -> TestResult {
        ensure(Posture::Ready.as_str(), "ready", "ready")?;
        ensure(Posture::Degraded.as_str(), "degraded", "degraded")?;
        ensure(
            Posture::NeedsAttention.as_str(),
            "needs_attention",
            "needs_attention",
        )
    }

    #[test]
    fn posture_is_usable() -> TestResult {
        ensure(Posture::Ready.is_usable(), true, "ready is usable")?;
        ensure(Posture::Degraded.is_usable(), true, "degraded is usable")?;
        ensure(
            Posture::NeedsAttention.is_usable(),
            false,
            "needs_attention is not usable",
        )
    }

    #[test]
    fn action_category_as_str_is_stable() -> TestResult {
        ensure(ActionCategory::Setup.as_str(), "setup", "setup")?;
        ensure(
            ActionCategory::Diagnostic.as_str(),
            "diagnostic",
            "diagnostic",
        )?;
        ensure(ActionCategory::Repair.as_str(), "repair", "repair")?;
        ensure(
            ActionCategory::Maintenance.as_str(),
            "maintenance",
            "maintenance",
        )?;
        ensure(
            ActionCategory::Configuration.as_str(),
            "configuration",
            "configuration",
        )
    }

    #[test]
    fn suggested_action_builder_defaults() -> TestResult {
        let action =
            SuggestedAction::new(1, ActionCategory::Setup, "ee init", "Initialize workspace");
        ensure(action.priority, 1, "priority")?;
        ensure(action.safe_for_automation, true, "safe_for_automation")?;
        ensure(action.idempotent, true, "idempotent")
    }

    #[test]
    fn suggested_action_requires_confirmation() -> TestResult {
        let action = SuggestedAction::new(1, ActionCategory::Repair, "ee reset", "Reset workspace")
            .requires_confirmation();
        ensure(action.safe_for_automation, false, "not safe_for_automation")
    }

    #[test]
    fn posture_summary_most_urgent_action() -> TestResult {
        let mut summary = PostureSummary::new(Posture::NeedsAttention, 0, 0, 3);
        summary.add_action(SuggestedAction::new(
            3,
            ActionCategory::Maintenance,
            "ee index rebuild",
            "Rebuild index",
        ));
        summary.add_action(SuggestedAction::new(
            1,
            ActionCategory::Setup,
            "ee init",
            "Initialize",
        ));
        summary.add_action(SuggestedAction::new(
            2,
            ActionCategory::Diagnostic,
            "ee doctor",
            "Run doctor",
        ));

        let urgent = summary
            .most_urgent_action()
            .ok_or_else(|| "expected an urgent action".to_string())?;
        ensure(urgent.priority, 1, "most urgent is priority 1")
    }

    #[test]
    fn posture_summary_automatable_actions() -> TestResult {
        let mut summary = PostureSummary::new(Posture::Degraded, 1, 1, 1);
        summary.add_action(SuggestedAction::new(
            1,
            ActionCategory::Setup,
            "ee init",
            "Initialize",
        ));
        summary.add_action(
            SuggestedAction::new(2, ActionCategory::Repair, "ee reset", "Reset")
                .requires_confirmation(),
        );

        let automatable = summary.automatable_actions();
        ensure(automatable.len(), 1, "one automatable action")?;
        let first = automatable
            .first()
            .ok_or_else(|| "expected an automatable action".to_string())?;
        ensure(first.command, "ee init", "automatable command")
    }

    #[test]
    fn posture_all_returns_all_variants() -> TestResult {
        ensure(Posture::all().len(), 3, "three posture variants")
    }

    #[test]
    fn action_category_all_returns_all_variants() -> TestResult {
        ensure(ActionCategory::all().len(), 5, "five action categories")
    }
}
