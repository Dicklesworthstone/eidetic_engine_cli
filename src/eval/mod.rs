//! Evaluation fixture schema (EE-246, EE-254).
//!
//! Defines the schema for evaluation fixtures used to verify agent-facing
//! scenarios, command sequences, expected outputs, and degraded branches.
//!
//! Also provides redaction leak detection (EE-254) to verify that sensitive
//! data does not leak through command output.
//!
//! See `docs/agent-outcome-scenarios.md` and `docs/fixture-provenance-traceability.md`
//! for the full contract definitions.

pub mod redaction;

pub use crate::models::EVAL_FIXTURE_SCHEMA_V1;
pub use redaction::{LeakDetection, LeakPattern, RedactionLeakDetector, RedactionLeakEvaluation};

/// Schema version for release gate checks.
pub const RELEASE_GATE_SCHEMA_V1: &str = "ee.eval.release_gate.v1";

/// Schema version for tail budget configuration.
pub const TAIL_BUDGET_CONFIG_SCHEMA_V1: &str = "ee.eval.tail_budget_config.v1";

/// An evaluation scenario that tests an agent-facing journey.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvaluationScenario {
    /// Stable scenario ID (e.g., "usr_pre_task_brief").
    pub scenario_id: String,
    /// Human-readable journey description.
    pub journey: String,
    /// Fixture family this scenario belongs to.
    pub fixture_family: String,
    /// Ordered sequence of commands to execute.
    pub command_sequence: Vec<CommandStep>,
    /// Expected outputs for each command.
    pub expected_outputs: Vec<ExpectedOutput>,
    /// Degraded or failure branches to test.
    pub degraded_branches: Vec<DegradedBranch>,
    /// Redaction classes expected in this scenario.
    pub redaction_classes: Vec<RedactionClass>,
    /// Beads that own this scenario's implementation.
    pub owning_bead_ids: Vec<String>,
    /// Gates that require this scenario to pass.
    pub owning_gate_ids: Vec<String>,
    /// Success signal describing what agents can do better.
    pub agent_success_signal: String,
}

impl EvaluationScenario {
    #[must_use]
    pub fn builder(scenario_id: impl Into<String>) -> EvaluationScenarioBuilder {
        EvaluationScenarioBuilder::new(scenario_id)
    }
}

/// Builder for `EvaluationScenario`.
#[derive(Clone, Debug, Default)]
pub struct EvaluationScenarioBuilder {
    scenario_id: String,
    journey: String,
    fixture_family: String,
    command_sequence: Vec<CommandStep>,
    expected_outputs: Vec<ExpectedOutput>,
    degraded_branches: Vec<DegradedBranch>,
    redaction_classes: Vec<RedactionClass>,
    owning_bead_ids: Vec<String>,
    owning_gate_ids: Vec<String>,
    agent_success_signal: String,
}

impl EvaluationScenarioBuilder {
    #[must_use]
    pub fn new(scenario_id: impl Into<String>) -> Self {
        Self {
            scenario_id: scenario_id.into(),
            ..Default::default()
        }
    }

    #[must_use]
    pub fn journey(mut self, journey: impl Into<String>) -> Self {
        self.journey = journey.into();
        self
    }

    #[must_use]
    pub fn fixture_family(mut self, family: impl Into<String>) -> Self {
        self.fixture_family = family.into();
        self
    }

    #[must_use]
    pub fn command(mut self, step: CommandStep) -> Self {
        self.command_sequence.push(step);
        self
    }

    #[must_use]
    pub fn expected_output(mut self, output: ExpectedOutput) -> Self {
        self.expected_outputs.push(output);
        self
    }

    #[must_use]
    pub fn degraded_branch(mut self, branch: DegradedBranch) -> Self {
        self.degraded_branches.push(branch);
        self
    }

    #[must_use]
    pub fn redaction_class(mut self, class: RedactionClass) -> Self {
        self.redaction_classes.push(class);
        self
    }

    #[must_use]
    pub fn owning_bead(mut self, bead_id: impl Into<String>) -> Self {
        self.owning_bead_ids.push(bead_id.into());
        self
    }

    #[must_use]
    pub fn owning_gate(mut self, gate_id: impl Into<String>) -> Self {
        self.owning_gate_ids.push(gate_id.into());
        self
    }

    #[must_use]
    pub fn agent_success_signal(mut self, signal: impl Into<String>) -> Self {
        self.agent_success_signal = signal.into();
        self
    }

    #[must_use]
    pub fn build(self) -> EvaluationScenario {
        EvaluationScenario {
            scenario_id: self.scenario_id,
            journey: self.journey,
            fixture_family: self.fixture_family,
            command_sequence: self.command_sequence,
            expected_outputs: self.expected_outputs,
            degraded_branches: self.degraded_branches,
            redaction_classes: self.redaction_classes,
            owning_bead_ids: self.owning_bead_ids,
            owning_gate_ids: self.owning_gate_ids,
            agent_success_signal: self.agent_success_signal,
        }
    }
}

/// A single command step in a scenario.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandStep {
    /// Step index (1-based).
    pub step: u32,
    /// Command template with placeholders (e.g., `<workspace>`).
    pub command_template: String,
    /// Expected exit code.
    pub expected_exit_code: i32,
    /// Expected schema in stdout (if any).
    pub expected_schema: Option<String>,
}

impl CommandStep {
    #[must_use]
    pub fn new(step: u32, command_template: impl Into<String>) -> Self {
        Self {
            step,
            command_template: command_template.into(),
            expected_exit_code: 0,
            expected_schema: None,
        }
    }

    #[must_use]
    pub fn with_exit_code(mut self, code: i32) -> Self {
        self.expected_exit_code = code;
        self
    }

    #[must_use]
    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.expected_schema = Some(schema.into());
        self
    }
}

/// Expected output for a command step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedOutput {
    /// Step index this output corresponds to.
    pub step: u32,
    /// Path to golden file (relative to tests/fixtures/).
    pub golden_path: Option<String>,
    /// Schema name to validate against.
    pub schema: String,
    /// Required fields that must be present.
    pub required_fields: Vec<String>,
    /// Fields that must be absent (e.g., secrets).
    pub absent_fields: Vec<String>,
}

impl ExpectedOutput {
    #[must_use]
    pub fn new(step: u32, schema: impl Into<String>) -> Self {
        Self {
            step,
            golden_path: None,
            schema: schema.into(),
            required_fields: Vec::new(),
            absent_fields: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_golden(mut self, path: impl Into<String>) -> Self {
        self.golden_path = Some(path.into());
        self
    }

    #[must_use]
    pub fn require_field(mut self, field: impl Into<String>) -> Self {
        self.required_fields.push(field.into());
        self
    }

    #[must_use]
    pub fn absent_field(mut self, field: impl Into<String>) -> Self {
        self.absent_fields.push(field.into());
        self
    }
}

/// A degraded or failure branch in a scenario.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DegradedBranch {
    /// Stable degradation code (e.g., "semantic_disabled").
    pub code: String,
    /// Human-readable description of the degradation.
    pub description: String,
    /// Expected repair action command.
    pub repair_action: Option<String>,
    /// Whether the agent success signal is preserved.
    pub preserves_success_signal: bool,
}

impl DegradedBranch {
    #[must_use]
    pub fn new(code: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            description: description.into(),
            repair_action: None,
            preserves_success_signal: true,
        }
    }

    #[must_use]
    pub fn with_repair(mut self, action: impl Into<String>) -> Self {
        self.repair_action = Some(action.into());
        self
    }

    #[must_use]
    pub fn signal_not_preserved(mut self) -> Self {
        self.preserves_success_signal = false;
        self
    }
}

/// Redaction class for sensitive data.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RedactionClass {
    /// API keys, tokens, secrets.
    Secret,
    /// Personally identifiable information.
    Pii,
    /// Internal paths that leak system structure.
    InternalPath,
    /// Unpublished code or proprietary content.
    Proprietary,
    /// User-defined custom redaction.
    Custom,
}

impl RedactionClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Pii => "pii",
            Self::InternalPath => "internal_path",
            Self::Proprietary => "proprietary",
            Self::Custom => "custom",
        }
    }
}

/// Result of validating a scenario run.
#[derive(Clone, Debug)]
pub struct ScenarioValidationResult {
    pub scenario_id: String,
    pub passed: bool,
    pub steps_passed: u32,
    pub steps_total: u32,
    pub failures: Vec<ValidationFailure>,
}

/// Aggregate report of an evaluation run (EE-255).
#[derive(Clone, Debug, Default)]
pub struct EvaluationReport {
    /// Overall status of the evaluation run.
    pub status: EvaluationStatus,
    /// Total scenarios run.
    pub scenarios_run: u32,
    /// Scenarios that passed all validations.
    pub scenarios_passed: u32,
    /// Scenarios that failed one or more validations.
    pub scenarios_failed: u32,
    /// Individual scenario results.
    pub results: Vec<ScenarioValidationResult>,
    /// Elapsed time in milliseconds.
    pub elapsed_ms: f64,
    /// Fixture directory path used.
    pub fixture_dir: Option<String>,
}

impl EvaluationReport {
    /// Create a new empty evaluation report.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a scenario result to the report.
    pub fn add_result(&mut self, result: ScenarioValidationResult) {
        if result.passed {
            self.scenarios_passed += 1;
        } else {
            self.scenarios_failed += 1;
        }
        self.scenarios_run += 1;
        self.results.push(result);
    }

    /// Set the elapsed time.
    pub fn with_elapsed_ms(mut self, elapsed_ms: f64) -> Self {
        self.elapsed_ms = elapsed_ms;
        self
    }

    /// Set the fixture directory.
    pub fn with_fixture_dir(mut self, dir: impl Into<String>) -> Self {
        self.fixture_dir = Some(dir.into());
        self
    }

    /// Finalize the report status based on results.
    pub fn finalize(&mut self) {
        self.status = if self.scenarios_run == 0 {
            EvaluationStatus::NoScenarios
        } else if self.scenarios_failed == 0 {
            EvaluationStatus::AllPassed
        } else if self.scenarios_passed == 0 {
            EvaluationStatus::AllFailed
        } else {
            EvaluationStatus::SomeFailed
        };
    }
}

/// Overall status of an evaluation run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EvaluationStatus {
    /// No scenarios were available to run.
    #[default]
    NoScenarios,
    /// All scenarios passed.
    AllPassed,
    /// Some scenarios passed, some failed.
    SomeFailed,
    /// All scenarios failed.
    AllFailed,
}

impl EvaluationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoScenarios => "no_scenarios",
            Self::AllPassed => "all_passed",
            Self::SomeFailed => "some_failed",
            Self::AllFailed => "all_failed",
        }
    }

    /// Whether the evaluation is considered successful (all passed or no scenarios).
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::NoScenarios | Self::AllPassed)
    }
}

/// A single validation failure.
#[derive(Clone, Debug)]
pub struct ValidationFailure {
    pub step: u32,
    pub kind: ValidationFailureKind,
    pub message: String,
}

/// Kind of validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationFailureKind {
    ExitCodeMismatch,
    SchemaMismatch,
    GoldenMismatch,
    MissingField,
    ForbiddenField,
    RedactionLeak,
    DegradationMissing,
}

impl ValidationFailureKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExitCodeMismatch => "exit_code_mismatch",
            Self::SchemaMismatch => "schema_mismatch",
            Self::GoldenMismatch => "golden_mismatch",
            Self::MissingField => "missing_field",
            Self::ForbiddenField => "forbidden_field",
            Self::RedactionLeak => "redaction_leak",
            Self::DegradationMissing => "degradation_missing",
        }
    }
}

// ============================================================================
// Release Gate Checks (EE-348)
// ============================================================================

/// Kind of release gate check.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReleaseGateKind {
    /// All evaluation scenarios must pass.
    EvaluationPassed,
    /// Schema drift detection must pass.
    SchemaDriftPassed,
    /// Forbidden dependencies must not be present.
    ForbiddenDepsFree,
    /// Tail-risk budget must not be exceeded.
    TailBudgetWithinLimit,
    /// Privacy budget must not be exceeded.
    PrivacyBudgetWithinLimit,
    /// Conformal calibration must be valid.
    CalibrationValid,
    /// All required test coverage gates must pass.
    CoverageGatePassed,
}

impl ReleaseGateKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EvaluationPassed => "evaluation_passed",
            Self::SchemaDriftPassed => "schema_drift_passed",
            Self::ForbiddenDepsFree => "forbidden_deps_free",
            Self::TailBudgetWithinLimit => "tail_budget_within_limit",
            Self::PrivacyBudgetWithinLimit => "privacy_budget_within_limit",
            Self::CalibrationValid => "calibration_valid",
            Self::CoverageGatePassed => "coverage_gate_passed",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 7] {
        [
            Self::EvaluationPassed,
            Self::SchemaDriftPassed,
            Self::ForbiddenDepsFree,
            Self::TailBudgetWithinLimit,
            Self::PrivacyBudgetWithinLimit,
            Self::CalibrationValid,
            Self::CoverageGatePassed,
        ]
    }

    /// Whether this gate is critical (blocks release if failed).
    #[must_use]
    pub const fn is_critical(self) -> bool {
        matches!(
            self,
            Self::ForbiddenDepsFree | Self::TailBudgetWithinLimit | Self::SchemaDriftPassed
        )
    }
}

impl std::fmt::Display for ReleaseGateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of a single release gate check.
#[derive(Clone, Debug)]
pub struct ReleaseGateCheck {
    pub gate: ReleaseGateKind,
    pub passed: bool,
    pub message: String,
    pub details: Option<String>,
}

impl ReleaseGateCheck {
    #[must_use]
    pub fn passed(gate: ReleaseGateKind, message: impl Into<String>) -> Self {
        Self {
            gate,
            passed: true,
            message: message.into(),
            details: None,
        }
    }

    #[must_use]
    pub fn failed(gate: ReleaseGateKind, message: impl Into<String>) -> Self {
        Self {
            gate,
            passed: false,
            message: message.into(),
            details: None,
        }
    }

    #[must_use]
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// Whether this check blocks the release.
    #[must_use]
    pub fn blocks_release(&self) -> bool {
        !self.passed && self.gate.is_critical()
    }
}

/// Aggregate report of all release gate checks.
#[derive(Clone, Debug, Default)]
pub struct ReleaseGateReport {
    pub checks: Vec<ReleaseGateCheck>,
    pub all_passed: bool,
    pub critical_failed: bool,
    pub elapsed_ms: f64,
}

impl ReleaseGateReport {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a check result to the report.
    pub fn add_check(&mut self, check: ReleaseGateCheck) {
        if check.blocks_release() {
            self.critical_failed = true;
        }
        self.checks.push(check);
    }

    /// Finalize the report and compute aggregate status.
    pub fn finalize(&mut self) {
        self.all_passed = self.checks.iter().all(|c| c.passed);
    }

    /// Get checks that failed.
    #[must_use]
    pub fn failed_checks(&self) -> Vec<&ReleaseGateCheck> {
        self.checks.iter().filter(|c| !c.passed).collect()
    }

    /// Whether the release should be blocked.
    #[must_use]
    pub fn should_block(&self) -> bool {
        self.critical_failed
    }
}

// ============================================================================
// Tail Budget Checks (EE-348)
// ============================================================================

/// Configuration for tail-risk budget checks.
#[derive(Clone, Debug)]
pub struct TailBudgetConfig {
    /// Maximum acceptable observed risk value.
    pub max_observed_risk: f64,
    /// Maximum acceptable upper bound of risk estimate.
    pub max_upper_bound: f64,
    /// Minimum required confidence level for risk assessments.
    pub min_confidence_level: f64,
    /// Maximum number of metrics allowed to exceed thresholds.
    pub max_exceeded_metrics: usize,
    /// Whether to fail on any exceeded bound.
    pub strict_mode: bool,
}

impl Default for TailBudgetConfig {
    fn default() -> Self {
        Self {
            max_observed_risk: 0.15,
            max_upper_bound: 0.25,
            min_confidence_level: 0.90,
            max_exceeded_metrics: 0,
            strict_mode: true,
        }
    }
}

impl TailBudgetConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_max_observed_risk(mut self, value: f64) -> Self {
        self.max_observed_risk = value;
        self
    }

    #[must_use]
    pub fn with_max_upper_bound(mut self, value: f64) -> Self {
        self.max_upper_bound = value;
        self
    }

    #[must_use]
    pub fn with_min_confidence(mut self, value: f64) -> Self {
        self.min_confidence_level = value;
        self
    }

    #[must_use]
    pub fn lenient(mut self) -> Self {
        self.strict_mode = false;
        self.max_exceeded_metrics = 2;
        self
    }
}

/// Result of a tail budget check.
#[derive(Clone, Debug)]
pub struct TailBudgetResult {
    pub passed: bool,
    pub metrics_checked: usize,
    pub metrics_exceeded: usize,
    pub worst_metric: Option<String>,
    pub worst_observed: Option<f64>,
    pub worst_threshold: Option<f64>,
    pub message: String,
}

impl TailBudgetResult {
    #[must_use]
    pub fn passed(metrics_checked: usize) -> Self {
        Self {
            passed: true,
            metrics_checked,
            metrics_exceeded: 0,
            worst_metric: None,
            worst_observed: None,
            worst_threshold: None,
            message: format!("All {metrics_checked} tail-risk metrics within budget"),
        }
    }

    #[must_use]
    pub fn failed(
        metrics_checked: usize,
        metrics_exceeded: usize,
        worst_metric: String,
        worst_observed: f64,
        worst_threshold: f64,
    ) -> Self {
        Self {
            passed: false,
            metrics_checked,
            metrics_exceeded,
            worst_metric: Some(worst_metric.clone()),
            worst_observed: Some(worst_observed),
            worst_threshold: Some(worst_threshold),
            message: format!(
                "Tail budget exceeded: {metrics_exceeded}/{metrics_checked} metrics over limit. \
                 Worst: {worst_metric} ({worst_observed:.4} > {worst_threshold:.4})"
            ),
        }
    }

    /// Convert to a release gate check result.
    #[must_use]
    pub fn to_gate_check(&self) -> ReleaseGateCheck {
        if self.passed {
            ReleaseGateCheck::passed(ReleaseGateKind::TailBudgetWithinLimit, &self.message)
        } else {
            ReleaseGateCheck::failed(ReleaseGateKind::TailBudgetWithinLimit, &self.message)
                .with_details(format!(
                    "exceeded_count={}, worst_metric={:?}",
                    self.metrics_exceeded, self.worst_metric
                ))
        }
    }
}

/// Tail-risk stress fixture for testing edge cases.
#[derive(Clone, Debug)]
pub struct TailRiskStressFixture {
    pub name: String,
    pub description: String,
    pub metrics: Vec<StressMetric>,
    pub expected_outcome: StressOutcome,
}

/// A single metric in a stress fixture.
#[derive(Clone, Debug)]
pub struct StressMetric {
    pub name: String,
    pub observed: f64,
    pub threshold: f64,
    pub upper_bound: f64,
    pub confidence_level: f64,
}

impl StressMetric {
    #[must_use]
    pub fn new(name: impl Into<String>, observed: f64, threshold: f64) -> Self {
        Self {
            name: name.into(),
            observed,
            threshold,
            upper_bound: observed * 1.2,
            confidence_level: 0.95,
        }
    }

    #[must_use]
    pub fn with_upper_bound(mut self, value: f64) -> Self {
        self.upper_bound = value;
        self
    }

    #[must_use]
    pub fn with_confidence(mut self, value: f64) -> Self {
        self.confidence_level = value;
        self
    }

    /// Whether this metric exceeds its threshold.
    #[must_use]
    pub fn exceeds_threshold(&self) -> bool {
        self.observed > self.threshold
    }

    /// Margin to threshold (positive = safe, negative = exceeded).
    #[must_use]
    pub fn margin(&self) -> f64 {
        self.threshold - self.observed
    }
}

/// Expected outcome of a stress test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StressOutcome {
    Pass,
    Fail,
    Warning,
}

impl StressOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Warning => "warning",
        }
    }
}

impl std::fmt::Display for StressOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TailRiskStressFixture {
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            metrics: Vec::new(),
            expected_outcome: StressOutcome::Pass,
        }
    }

    #[must_use]
    pub fn with_metric(mut self, metric: StressMetric) -> Self {
        self.metrics.push(metric);
        self
    }

    #[must_use]
    pub fn expect_fail(mut self) -> Self {
        self.expected_outcome = StressOutcome::Fail;
        self
    }

    #[must_use]
    pub fn expect_warning(mut self) -> Self {
        self.expected_outcome = StressOutcome::Warning;
        self
    }

    /// Evaluate this fixture against a budget config.
    #[must_use]
    pub fn evaluate(&self, config: &TailBudgetConfig) -> TailBudgetResult {
        check_tail_budget(&self.metrics, config)
    }
}

/// Check tail-risk metrics against budget configuration.
#[must_use]
pub fn check_tail_budget(metrics: &[StressMetric], config: &TailBudgetConfig) -> TailBudgetResult {
    if metrics.is_empty() {
        return TailBudgetResult::passed(0);
    }

    let mut exceeded_count = 0_usize;
    let mut worst_metric: Option<&StressMetric> = None;
    let mut worst_margin = f64::MAX;

    for metric in metrics {
        let mut exceeds = false;

        if metric.observed > config.max_observed_risk {
            exceeds = true;
        }
        if metric.upper_bound > config.max_upper_bound {
            exceeds = true;
        }
        if metric.confidence_level < config.min_confidence_level {
            exceeds = true;
        }
        if metric.exceeds_threshold() {
            exceeds = true;
        }

        if exceeds {
            exceeded_count += 1;
            let margin = metric.margin();
            if margin < worst_margin {
                worst_margin = margin;
                worst_metric = Some(metric);
            }
        }
    }

    let passed = if config.strict_mode {
        exceeded_count == 0
    } else {
        exceeded_count <= config.max_exceeded_metrics
    };

    if passed {
        TailBudgetResult::passed(metrics.len())
    } else {
        match worst_metric {
            Some(worst) => TailBudgetResult::failed(
                metrics.len(),
                exceeded_count,
                worst.name.clone(),
                worst.observed,
                worst.threshold,
            ),
            None => TailBudgetResult::failed(
                metrics.len(),
                exceeded_count,
                "unknown".to_owned(),
                0.0,
                0.0,
            ),
        }
    }
}

/// Standard stress fixtures for tail-risk testing.
#[must_use]
pub fn tail_risk_stress_fixtures() -> Vec<TailRiskStressFixture> {
    vec![
        TailRiskStressFixture::new("all_safe", "All metrics well within bounds")
            .with_metric(StressMetric::new("false_positive_rate", 0.03, 0.10))
            .with_metric(StressMetric::new("false_negative_rate", 0.02, 0.10))
            .with_metric(StressMetric::new("calibration_error", 0.01, 0.05)),
        TailRiskStressFixture::new("single_exceeded", "One metric exceeds threshold")
            .with_metric(StressMetric::new("false_positive_rate", 0.12, 0.10))
            .with_metric(StressMetric::new("false_negative_rate", 0.02, 0.10))
            .expect_fail(),
        TailRiskStressFixture::new("boundary_exact", "Metric exactly at threshold")
            .with_metric(StressMetric::new("false_positive_rate", 0.10, 0.10))
            .with_metric(StressMetric::new("calibration_error", 0.05, 0.05)),
        TailRiskStressFixture::new("all_exceeded", "All metrics exceed thresholds")
            .with_metric(StressMetric::new("false_positive_rate", 0.25, 0.10))
            .with_metric(StressMetric::new("false_negative_rate", 0.30, 0.10))
            .with_metric(StressMetric::new("calibration_error", 0.20, 0.05))
            .expect_fail(),
        TailRiskStressFixture::new(
            "high_upper_bound",
            "Upper bound exceeds limit even though observed is OK",
        )
        .with_metric(StressMetric::new("latency_p99", 0.05, 0.10).with_upper_bound(0.40))
        .expect_fail(),
        TailRiskStressFixture::new("low_confidence", "Confidence level below minimum")
            .with_metric(StressMetric::new("error_rate", 0.02, 0.10).with_confidence(0.80))
            .expect_fail(),
        TailRiskStressFixture::new(
            "epsilon_under",
            "Just barely under threshold (epsilon test)",
        )
        .with_metric(StressMetric::new("budget_utilization", 0.0999999, 0.10)),
        TailRiskStressFixture::new("epsilon_over", "Just barely over threshold (epsilon test)")
            .with_metric(StressMetric::new("budget_utilization", 0.1000001, 0.10))
            .expect_fail(),
        TailRiskStressFixture::new("zero_values", "Zero observed and threshold values")
            .with_metric(StressMetric::new("zero_metric", 0.0, 0.0)),
        TailRiskStressFixture::new(
            "negative_margin",
            "Large negative margin (severely exceeded)",
        )
        .with_metric(StressMetric::new("catastrophic_failure", 0.95, 0.05))
        .expect_fail(),
    ]
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

    #[test]
    fn eval_fixture_schema_version_is_stable() -> TestResult {
        ensure(
            EVAL_FIXTURE_SCHEMA_V1,
            "ee.eval_fixture.v1",
            "schema version",
        )
    }

    #[test]
    fn scenario_builder_creates_valid_scenario() -> TestResult {
        let scenario = EvaluationScenario::builder("test_scenario")
            .journey("Test journey")
            .fixture_family("test_family")
            .command(CommandStep::new(1, "ee status --json").with_schema("ee.response.v1"))
            .expected_output(
                ExpectedOutput::new(1, "ee.response.v1")
                    .require_field("data.command")
                    .absent_field("secret"),
            )
            .degraded_branch(
                DegradedBranch::new("semantic_disabled", "Semantic search unavailable")
                    .with_repair("ee index rebuild"),
            )
            .redaction_class(RedactionClass::Secret)
            .owning_bead("eidetic_engine_cli-test")
            .agent_success_signal("Agent receives test data")
            .build();

        ensure(scenario.scenario_id, "test_scenario".to_string(), "id")?;
        ensure(scenario.command_sequence.len(), 1, "command count")?;
        ensure(scenario.expected_outputs.len(), 1, "output count")?;
        ensure(scenario.degraded_branches.len(), 1, "branch count")?;
        ensure(scenario.redaction_classes.len(), 1, "redaction count")
    }

    #[test]
    fn command_step_defaults_are_sensible() -> TestResult {
        let step = CommandStep::new(1, "ee status");
        ensure(step.expected_exit_code, 0, "default exit code")?;
        ensure(step.expected_schema, None, "default schema")
    }

    #[test]
    fn redaction_class_strings_are_stable() -> TestResult {
        ensure(RedactionClass::Secret.as_str(), "secret", "secret")?;
        ensure(RedactionClass::Pii.as_str(), "pii", "pii")?;
        ensure(
            RedactionClass::InternalPath.as_str(),
            "internal_path",
            "internal_path",
        )?;
        ensure(
            RedactionClass::Proprietary.as_str(),
            "proprietary",
            "proprietary",
        )?;
        ensure(RedactionClass::Custom.as_str(), "custom", "custom")
    }

    #[test]
    fn validation_failure_kind_strings_are_stable() -> TestResult {
        ensure(
            ValidationFailureKind::ExitCodeMismatch.as_str(),
            "exit_code_mismatch",
            "exit_code_mismatch",
        )?;
        ensure(
            ValidationFailureKind::SchemaMismatch.as_str(),
            "schema_mismatch",
            "schema_mismatch",
        )?;
        ensure(
            ValidationFailureKind::GoldenMismatch.as_str(),
            "golden_mismatch",
            "golden_mismatch",
        )?;
        ensure(
            ValidationFailureKind::MissingField.as_str(),
            "missing_field",
            "missing_field",
        )?;
        ensure(
            ValidationFailureKind::ForbiddenField.as_str(),
            "forbidden_field",
            "forbidden_field",
        )?;
        ensure(
            ValidationFailureKind::RedactionLeak.as_str(),
            "redaction_leak",
            "redaction_leak",
        )?;
        ensure(
            ValidationFailureKind::DegradationMissing.as_str(),
            "degradation_missing",
            "degradation_missing",
        )
    }

    #[test]
    fn degraded_branch_preserves_signal_by_default() -> TestResult {
        let branch = DegradedBranch::new("test", "Test degradation");
        ensure(branch.preserves_success_signal, true, "default preserves")?;

        let explicit = branch.signal_not_preserved();
        ensure(explicit.preserves_success_signal, false, "explicit false")
    }

    // ========================================================================
    // EvaluationReport Tests (EE-255)
    // ========================================================================

    #[test]
    fn evaluation_report_new_is_empty() -> TestResult {
        let report = EvaluationReport::new();
        ensure(report.scenarios_run, 0, "scenarios_run")?;
        ensure(report.scenarios_passed, 0, "scenarios_passed")?;
        ensure(report.scenarios_failed, 0, "scenarios_failed")?;
        ensure(report.results.len(), 0, "results empty")?;
        ensure(
            report.status,
            EvaluationStatus::NoScenarios,
            "initial status",
        )
    }

    #[test]
    fn evaluation_report_add_result_updates_counts() -> TestResult {
        let mut report = EvaluationReport::new();

        report.add_result(ScenarioValidationResult {
            scenario_id: "test_1".to_string(),
            passed: true,
            steps_passed: 2,
            steps_total: 2,
            failures: vec![],
        });
        ensure(report.scenarios_run, 1, "after first")?;
        ensure(report.scenarios_passed, 1, "passed after first")?;
        ensure(report.scenarios_failed, 0, "failed after first")?;

        report.add_result(ScenarioValidationResult {
            scenario_id: "test_2".to_string(),
            passed: false,
            steps_passed: 1,
            steps_total: 2,
            failures: vec![ValidationFailure {
                step: 2,
                kind: ValidationFailureKind::ExitCodeMismatch,
                message: "Failed".to_string(),
            }],
        });
        ensure(report.scenarios_run, 2, "after second")?;
        ensure(report.scenarios_passed, 1, "passed after second")?;
        ensure(report.scenarios_failed, 1, "failed after second")
    }

    #[test]
    fn evaluation_report_finalize_sets_status() -> TestResult {
        let mut empty = EvaluationReport::new();
        empty.finalize();
        ensure(empty.status, EvaluationStatus::NoScenarios, "empty")?;

        let mut all_pass = EvaluationReport::new();
        all_pass.add_result(ScenarioValidationResult {
            scenario_id: "t".to_string(),
            passed: true,
            steps_passed: 1,
            steps_total: 1,
            failures: vec![],
        });
        all_pass.finalize();
        ensure(all_pass.status, EvaluationStatus::AllPassed, "all_pass")?;

        let mut some_fail = EvaluationReport::new();
        some_fail.add_result(ScenarioValidationResult {
            scenario_id: "p".to_string(),
            passed: true,
            steps_passed: 1,
            steps_total: 1,
            failures: vec![],
        });
        some_fail.add_result(ScenarioValidationResult {
            scenario_id: "f".to_string(),
            passed: false,
            steps_passed: 0,
            steps_total: 1,
            failures: vec![ValidationFailure {
                step: 1,
                kind: ValidationFailureKind::SchemaMismatch,
                message: "x".to_string(),
            }],
        });
        some_fail.finalize();
        ensure(some_fail.status, EvaluationStatus::SomeFailed, "some_fail")?;

        let mut all_fail = EvaluationReport::new();
        all_fail.add_result(ScenarioValidationResult {
            scenario_id: "f".to_string(),
            passed: false,
            steps_passed: 0,
            steps_total: 1,
            failures: vec![ValidationFailure {
                step: 1,
                kind: ValidationFailureKind::MissingField,
                message: "x".to_string(),
            }],
        });
        all_fail.finalize();
        ensure(all_fail.status, EvaluationStatus::AllFailed, "all_fail")
    }

    #[test]
    fn evaluation_report_builder_methods() -> TestResult {
        let report = EvaluationReport::new()
            .with_elapsed_ms(123.45)
            .with_fixture_dir("/path/to/fixtures");

        ensure(report.elapsed_ms, 123.45, "elapsed_ms")?;
        ensure(
            report.fixture_dir,
            Some("/path/to/fixtures".to_string()),
            "fixture_dir",
        )
    }

    #[test]
    fn evaluation_status_strings_are_stable() -> TestResult {
        ensure(
            EvaluationStatus::NoScenarios.as_str(),
            "no_scenarios",
            "no_scenarios",
        )?;
        ensure(
            EvaluationStatus::AllPassed.as_str(),
            "all_passed",
            "all_passed",
        )?;
        ensure(
            EvaluationStatus::SomeFailed.as_str(),
            "some_failed",
            "some_failed",
        )?;
        ensure(
            EvaluationStatus::AllFailed.as_str(),
            "all_failed",
            "all_failed",
        )
    }

    #[test]
    fn evaluation_status_is_success_logic() -> TestResult {
        ensure(
            EvaluationStatus::NoScenarios.is_success(),
            true,
            "no_scenarios",
        )?;
        ensure(EvaluationStatus::AllPassed.is_success(), true, "all_passed")?;
        ensure(
            EvaluationStatus::SomeFailed.is_success(),
            false,
            "some_failed",
        )?;
        ensure(
            EvaluationStatus::AllFailed.is_success(),
            false,
            "all_failed",
        )
    }

    // ========================================================================
    // Release Gate Tests (EE-348)
    // ========================================================================

    #[test]
    fn release_gate_kind_strings_are_stable() -> TestResult {
        ensure(
            ReleaseGateKind::EvaluationPassed.as_str(),
            "evaluation_passed",
            "evaluation_passed",
        )?;
        ensure(
            ReleaseGateKind::SchemaDriftPassed.as_str(),
            "schema_drift_passed",
            "schema_drift_passed",
        )?;
        ensure(
            ReleaseGateKind::ForbiddenDepsFree.as_str(),
            "forbidden_deps_free",
            "forbidden_deps_free",
        )?;
        ensure(
            ReleaseGateKind::TailBudgetWithinLimit.as_str(),
            "tail_budget_within_limit",
            "tail_budget_within_limit",
        )?;
        ensure(
            ReleaseGateKind::PrivacyBudgetWithinLimit.as_str(),
            "privacy_budget_within_limit",
            "privacy_budget_within_limit",
        )?;
        ensure(
            ReleaseGateKind::CalibrationValid.as_str(),
            "calibration_valid",
            "calibration_valid",
        )?;
        ensure(
            ReleaseGateKind::CoverageGatePassed.as_str(),
            "coverage_gate_passed",
            "coverage_gate_passed",
        )
    }

    #[test]
    fn release_gate_critical_gates_are_identified() -> TestResult {
        ensure(
            ReleaseGateKind::TailBudgetWithinLimit.is_critical(),
            true,
            "tail_budget critical",
        )?;
        ensure(
            ReleaseGateKind::ForbiddenDepsFree.is_critical(),
            true,
            "forbidden_deps critical",
        )?;
        ensure(
            ReleaseGateKind::SchemaDriftPassed.is_critical(),
            true,
            "schema_drift critical",
        )?;
        ensure(
            ReleaseGateKind::EvaluationPassed.is_critical(),
            false,
            "evaluation not critical",
        )
    }

    #[test]
    fn release_gate_check_passed_does_not_block() -> TestResult {
        let check = ReleaseGateCheck::passed(ReleaseGateKind::TailBudgetWithinLimit, "All good");
        ensure(check.passed, true, "passed")?;
        ensure(check.blocks_release(), false, "does not block")
    }

    #[test]
    fn release_gate_check_failed_critical_blocks() -> TestResult {
        let check =
            ReleaseGateCheck::failed(ReleaseGateKind::TailBudgetWithinLimit, "Budget exceeded");
        ensure(check.passed, false, "failed")?;
        ensure(check.blocks_release(), true, "blocks release")
    }

    #[test]
    fn release_gate_check_failed_non_critical_does_not_block() -> TestResult {
        let check =
            ReleaseGateCheck::failed(ReleaseGateKind::EvaluationPassed, "Some tests failed");
        ensure(check.passed, false, "failed")?;
        ensure(check.blocks_release(), false, "does not block")
    }

    #[test]
    fn release_gate_report_tracks_critical_failures() -> TestResult {
        let mut report = ReleaseGateReport::new();

        report.add_check(ReleaseGateCheck::passed(
            ReleaseGateKind::EvaluationPassed,
            "OK",
        ));
        ensure(report.critical_failed, false, "no critical failure yet")?;

        report.add_check(ReleaseGateCheck::failed(
            ReleaseGateKind::TailBudgetWithinLimit,
            "Exceeded",
        ));
        ensure(report.critical_failed, true, "critical failure detected")?;
        ensure(report.should_block(), true, "should block")
    }

    #[test]
    fn release_gate_report_finalize_computes_all_passed() -> TestResult {
        let mut report = ReleaseGateReport::new();
        report.add_check(ReleaseGateCheck::passed(
            ReleaseGateKind::EvaluationPassed,
            "OK",
        ));
        report.add_check(ReleaseGateCheck::passed(
            ReleaseGateKind::TailBudgetWithinLimit,
            "OK",
        ));
        report.finalize();

        ensure(report.all_passed, true, "all passed")?;
        ensure(report.should_block(), false, "should not block")
    }

    // ========================================================================
    // Tail Budget Tests (EE-348)
    // ========================================================================

    #[test]
    fn tail_budget_config_default_is_strict() -> TestResult {
        let config = TailBudgetConfig::default();
        ensure(config.strict_mode, true, "strict_mode")?;
        ensure(config.max_exceeded_metrics, 0, "max_exceeded")
    }

    #[test]
    fn tail_budget_config_lenient_allows_some_exceeded() -> TestResult {
        let config = TailBudgetConfig::new().lenient();
        ensure(config.strict_mode, false, "not strict")?;
        ensure(config.max_exceeded_metrics, 2, "allows 2 exceeded")
    }

    #[test]
    fn stress_metric_exceeds_threshold_correctly() -> TestResult {
        let safe = StressMetric::new("test", 0.05, 0.10);
        ensure(safe.exceeds_threshold(), false, "safe does not exceed")?;

        let exceeded = StressMetric::new("test", 0.15, 0.10);
        ensure(exceeded.exceeds_threshold(), true, "exceeded does exceed")
    }

    #[test]
    fn stress_metric_margin_calculation() -> TestResult {
        let safe = StressMetric::new("test", 0.05, 0.10);
        ensure(safe.margin() > 0.0, true, "positive margin for safe")?;

        let exceeded = StressMetric::new("test", 0.15, 0.10);
        ensure(
            exceeded.margin() < 0.0,
            true,
            "negative margin for exceeded",
        )
    }

    #[test]
    fn check_tail_budget_empty_metrics_passes() -> TestResult {
        let config = TailBudgetConfig::default();
        let result = check_tail_budget(&[], &config);
        ensure(result.passed, true, "empty passes")?;
        ensure(result.metrics_checked, 0, "zero checked")
    }

    #[test]
    fn check_tail_budget_all_safe_passes() -> TestResult {
        let config = TailBudgetConfig::default();
        let metrics = vec![
            StressMetric::new("a", 0.05, 0.20),
            StressMetric::new("b", 0.08, 0.20),
        ];
        let result = check_tail_budget(&metrics, &config);
        ensure(result.passed, true, "all safe passes")?;
        ensure(result.metrics_exceeded, 0, "none exceeded")
    }

    #[test]
    fn check_tail_budget_one_exceeded_fails_strict() -> TestResult {
        let config = TailBudgetConfig::default();
        let metrics = vec![
            StressMetric::new("a", 0.05, 0.10),
            StressMetric::new("b", 0.25, 0.10),
        ];
        let result = check_tail_budget(&metrics, &config);
        ensure(result.passed, false, "one exceeded fails")?;
        ensure(result.metrics_exceeded, 1, "one exceeded")?;
        ensure(result.worst_metric, Some("b".to_string()), "worst is b")
    }

    #[test]
    fn check_tail_budget_one_exceeded_passes_lenient() -> TestResult {
        let config = TailBudgetConfig::new().lenient();
        let metrics = vec![
            StressMetric::new("a", 0.05, 0.10),
            StressMetric::new("b", 0.25, 0.10),
        ];
        let result = check_tail_budget(&metrics, &config);
        ensure(result.passed, true, "one exceeded passes lenient")
    }

    #[test]
    fn check_tail_budget_upper_bound_triggers_failure() -> TestResult {
        let config = TailBudgetConfig::default();
        let metrics = vec![StressMetric::new("a", 0.05, 0.20).with_upper_bound(0.50)];
        let result = check_tail_budget(&metrics, &config);
        ensure(result.passed, false, "high upper bound fails")
    }

    #[test]
    fn check_tail_budget_low_confidence_triggers_failure() -> TestResult {
        let config = TailBudgetConfig::default();
        let metrics = vec![StressMetric::new("a", 0.05, 0.20).with_confidence(0.70)];
        let result = check_tail_budget(&metrics, &config);
        ensure(result.passed, false, "low confidence fails")
    }

    #[test]
    fn tail_budget_result_to_gate_check_passed() -> TestResult {
        let result = TailBudgetResult::passed(5);
        let check = result.to_gate_check();
        ensure(check.passed, true, "gate passed")?;
        ensure(
            check.gate,
            ReleaseGateKind::TailBudgetWithinLimit,
            "correct gate kind",
        )
    }

    #[test]
    fn tail_budget_result_to_gate_check_failed() -> TestResult {
        let result = TailBudgetResult::failed(5, 2, "test".to_string(), 0.25, 0.10);
        let check = result.to_gate_check();
        ensure(check.passed, false, "gate failed")?;
        ensure(check.blocks_release(), true, "blocks release")?;
        ensure(check.details.is_some(), true, "has details")
    }

    #[test]
    fn stress_outcome_strings_are_stable() -> TestResult {
        ensure(StressOutcome::Pass.as_str(), "pass", "pass")?;
        ensure(StressOutcome::Fail.as_str(), "fail", "fail")?;
        ensure(StressOutcome::Warning.as_str(), "warning", "warning")
    }

    #[test]
    fn tail_risk_stress_fixtures_are_non_empty() -> TestResult {
        let fixtures = tail_risk_stress_fixtures();
        ensure(fixtures.len() >= 8, true, "at least 8 fixtures")
    }

    #[test]
    fn tail_risk_stress_fixtures_have_valid_expectations() -> TestResult {
        let fixtures = tail_risk_stress_fixtures();
        let config = TailBudgetConfig::default();

        for fixture in &fixtures {
            let result = fixture.evaluate(&config);
            let expected_pass = fixture.expected_outcome == StressOutcome::Pass;
            if result.passed != expected_pass {
                return Err(format!(
                    "Fixture '{}' expected {:?} but got passed={}",
                    fixture.name, fixture.expected_outcome, result.passed
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn stress_fixture_boundary_exact_is_not_exceeded() -> TestResult {
        let metric = StressMetric::new("exact", 0.10, 0.10);
        ensure(
            metric.exceeds_threshold(),
            false,
            "exact boundary does not exceed",
        )
    }

    #[test]
    fn stress_fixture_epsilon_detection() -> TestResult {
        let under = StressMetric::new("under", 0.0999999, 0.10);
        let over = StressMetric::new("over", 0.1000001, 0.10);

        ensure(under.exceeds_threshold(), false, "epsilon under")?;
        ensure(over.exceeds_threshold(), true, "epsilon over")
    }

    #[test]
    fn release_gate_schema_version_is_stable() -> TestResult {
        ensure(
            RELEASE_GATE_SCHEMA_V1,
            "ee.eval.release_gate.v1",
            "release gate schema",
        )?;
        ensure(
            TAIL_BUDGET_CONFIG_SCHEMA_V1,
            "ee.eval.tail_budget_config.v1",
            "tail budget config schema",
        )
    }
}
