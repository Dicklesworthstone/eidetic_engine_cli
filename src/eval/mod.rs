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
}
