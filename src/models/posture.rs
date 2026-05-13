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

/// E6 workspace/subsystem posture status.
///
/// This is intentionally separate from the older `Posture` enum above:
/// `Posture` powers the coarse `ee check` summary, while this enum is the
/// stable machine-facing status for per-subsystem diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubsystemPostureStatus {
    Ok,
    DegradedRecoverable,
    DegradedRequired,
    Blocked,
    Unimplemented,
    Initializing,
}

impl SubsystemPostureStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::DegradedRecoverable => "degraded_recoverable",
            Self::DegradedRequired => "degraded_required",
            Self::Blocked => "blocked",
            Self::Unimplemented => "unimplemented",
            Self::Initializing => "initializing",
        }
    }

    /// Aggregate subsystem statuses into the workspace-wide status.
    ///
    /// Deterministic precedence:
    /// blocked > degraded_required > degraded_recoverable/unimplemented >
    /// initializing > ok. An all-initializing workspace stays initializing.
    #[must_use]
    pub fn aggregate(statuses: &[Self]) -> Self {
        if statuses.is_empty() {
            return Self::Ok;
        }
        if statuses.iter().all(|status| *status == Self::Initializing) {
            return Self::Initializing;
        }
        if statuses.contains(&Self::Blocked) {
            return Self::Blocked;
        }
        if statuses.contains(&Self::DegradedRequired) {
            return Self::DegradedRequired;
        }
        if statuses
            .iter()
            .any(|status| matches!(status, Self::DegradedRecoverable | Self::Unimplemented))
        {
            return Self::DegradedRecoverable;
        }
        if statuses.contains(&Self::Initializing) {
            return Self::Initializing;
        }
        Self::Ok
    }
}

/// Per-subsystem posture row for workspace diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubsystemPostureReport {
    pub id: &'static str,
    pub status: SubsystemPostureStatus,
    pub reason: Option<&'static str>,
    pub fallback: Option<&'static str>,
    pub checks_passed: u32,
}

impl SubsystemPostureReport {
    #[must_use]
    pub const fn new(id: &'static str, status: SubsystemPostureStatus) -> Self {
        Self {
            id,
            status,
            reason: None,
            fallback: None,
            checks_passed: 0,
        }
    }

    #[must_use]
    pub const fn with_reason(mut self, reason: &'static str) -> Self {
        self.reason = Some(reason);
        self
    }

    #[must_use]
    pub const fn with_fallback(mut self, fallback: &'static str) -> Self {
        self.fallback = Some(fallback);
        self
    }

    #[must_use]
    pub const fn with_checks_passed(mut self, checks_passed: u32) -> Self {
        self.checks_passed = checks_passed;
        self
    }
}

/// Posture for the command currently being rendered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationPostureReport {
    pub status: SubsystemPostureStatus,
    pub subsystems_used: Vec<&'static str>,
    pub subsystems_skipped: Vec<&'static str>,
    pub degradations_applied: Vec<&'static str>,
}

impl OperationPostureReport {
    #[must_use]
    pub fn ok(subsystems_used: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            status: SubsystemPostureStatus::Ok,
            subsystems_used: subsystems_used.into_iter().collect(),
            subsystems_skipped: Vec::new(),
            degradations_applied: Vec::new(),
        }
    }
}

/// Workspace posture with aggregate, operation, and fixed subsystem rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspacePostureReport {
    pub overall: SubsystemPostureStatus,
    pub this_operation: OperationPostureReport,
    pub subsystems: Vec<SubsystemPostureReport>,
}

impl WorkspacePostureReport {
    #[must_use]
    pub fn new(
        subsystems: Vec<SubsystemPostureReport>,
        this_operation: OperationPostureReport,
    ) -> Self {
        let statuses = subsystems
            .iter()
            .map(|subsystem| subsystem.status)
            .collect::<Vec<_>>();
        Self {
            overall: SubsystemPostureStatus::aggregate(&statuses),
            this_operation,
            subsystems,
        }
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

    #[test]
    fn subsystem_posture_status_wire_strings_are_stable() -> TestResult {
        ensure(SubsystemPostureStatus::Ok.as_str(), "ok", "ok")?;
        ensure(
            SubsystemPostureStatus::DegradedRecoverable.as_str(),
            "degraded_recoverable",
            "recoverable",
        )?;
        ensure(
            SubsystemPostureStatus::DegradedRequired.as_str(),
            "degraded_required",
            "required",
        )?;
        ensure(
            SubsystemPostureStatus::Blocked.as_str(),
            "blocked",
            "blocked",
        )?;
        ensure(
            SubsystemPostureStatus::Unimplemented.as_str(),
            "unimplemented",
            "unimplemented",
        )?;
        ensure(
            SubsystemPostureStatus::Initializing.as_str(),
            "initializing",
            "initializing",
        )
    }

    #[test]
    fn subsystem_posture_aggregation_precedence() -> TestResult {
        use SubsystemPostureStatus as S;
        ensure(S::aggregate(&[S::Ok, S::Ok]), S::Ok, "all ok")?;
        ensure(
            S::aggregate(&[S::Initializing, S::Initializing]),
            S::Initializing,
            "all initializing",
        )?;
        ensure(
            S::aggregate(&[S::Ok, S::Blocked, S::DegradedRequired]),
            S::Blocked,
            "blocked wins",
        )?;
        ensure(
            S::aggregate(&[S::Ok, S::DegradedRequired, S::DegradedRecoverable]),
            S::DegradedRequired,
            "required beats recoverable",
        )?;
        ensure(
            S::aggregate(&[S::Ok, S::DegradedRecoverable]),
            S::DegradedRecoverable,
            "recoverable beats ok",
        )?;
        ensure(
            S::aggregate(&[S::Ok, S::Unimplemented]),
            S::DegradedRecoverable,
            "unimplemented rolls up recoverable",
        )?;
        ensure(
            S::aggregate(&[S::Ok, S::Initializing]),
            S::Initializing,
            "mixed initializing remains initializing",
        )
    }

    #[test]
    fn workspace_posture_report_aggregates_subsystems() -> TestResult {
        let report = WorkspacePostureReport::new(
            vec![
                SubsystemPostureReport::new("runtime", SubsystemPostureStatus::Ok),
                SubsystemPostureReport::new("search", SubsystemPostureStatus::DegradedRecoverable)
                    .with_reason("index_missing")
                    .with_fallback("lexical_fallback"),
            ],
            OperationPostureReport::ok(["runtime", "search"]),
        );

        ensure(
            report.overall,
            SubsystemPostureStatus::DegradedRecoverable,
            "overall",
        )?;
        ensure(
            report.this_operation.status,
            SubsystemPostureStatus::Ok,
            "operation",
        )?;
        ensure(report.subsystems.len(), 2, "subsystem count")
    }
}
