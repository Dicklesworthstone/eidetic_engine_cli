//! Isolated rehearsal sandbox for EE command sequences (EE-REHEARSE-001).
//!
//! Provides plan, run, inspect, and promote-plan operations for rehearsing
//! EE command sequences in an isolated side-path copy before real execution.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::models::DomainError;

/// Schema for rehearsal plan report.
pub const REHEARSE_PLAN_SCHEMA_V1: &str = "ee.rehearse.plan.v1";

/// Schema for rehearsal run report.
pub const REHEARSE_RUN_SCHEMA_V1: &str = "ee.rehearse.run.v1";

/// Schema for rehearsal inspect report.
pub const REHEARSE_INSPECT_SCHEMA_V1: &str = "ee.rehearse.inspect.v1";

/// Schema for rehearsal promote-plan report.
pub const REHEARSE_PROMOTE_PLAN_SCHEMA_V1: &str = "ee.rehearse.promote_plan.v1";

const REHEARSAL_UNAVAILABLE_CODE: &str = "rehearsal_unavailable";
const REHEARSAL_UNAVAILABLE_MESSAGE: &str =
    "Rehearsal sandbox execution is unavailable until core helpers create real isolated artifacts.";
const UNAVAILABLE_VALUE: &str = "unavailable";

// ============================================================================
// Command Spec Types
// ============================================================================

/// A command specification for rehearsal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandSpec {
    /// Unique ID for this command in the sequence.
    pub id: String,
    /// EE command path (e.g., "remember", "search", "context").
    pub command: String,
    /// Command arguments.
    pub args: Vec<String>,
    /// Expected effect class: read_only, write_memory, write_index, write_config.
    pub expected_effect: String,
    /// Whether to stop on failure.
    pub stop_on_failure: bool,
    /// Idempotency key for deduplication.
    pub idempotency_key: Option<String>,
}

/// Rehearsal profile for controlling behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RehearsalProfile {
    /// Quick validation without full execution.
    Quick,
    /// Full execution with all checks.
    #[default]
    Full,
    /// Privacy-focused with redaction.
    Privacy,
}

impl RehearsalProfile {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Full => "full",
            Self::Privacy => "privacy",
        }
    }

    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        <Self as std::str::FromStr>::from_str(s).ok()
    }
}

impl std::str::FromStr for RehearsalProfile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "quick" => Ok(Self::Quick),
            "full" => Ok(Self::Full),
            "privacy" => Ok(Self::Privacy),
            _ => Err(format!("invalid rehearsal profile: {s}")),
        }
    }
}

// ============================================================================
// Plan Operation
// ============================================================================

/// Options for planning a rehearsal.
#[derive(Clone, Debug, Default)]
pub struct RehearsePlanOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Command specifications to validate.
    pub commands: Vec<CommandSpec>,
    /// Rehearsal profile.
    pub profile: RehearsalProfile,
}

/// Report from planning a rehearsal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RehearsePlanReport {
    pub schema: String,
    pub plan_id: String,
    pub workspace_id: String,
    pub command_count: u32,
    pub rehearsable_count: u32,
    pub non_rehearsable: Vec<NonRehearsableCommand>,
    pub estimated_artifacts: Vec<String>,
    pub side_path_size_estimate: String,
    pub profile: String,
    pub can_proceed: bool,
    pub degradation_codes: Vec<String>,
    pub next_actions: Vec<String>,
    pub created_at: String,
}

/// A command that cannot be rehearsed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NonRehearsableCommand {
    pub command_id: String,
    pub command: String,
    pub reason_code: String,
    pub reason: String,
    pub next_action: Option<String>,
}

impl RehearsePlanReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("Rehearsal Plan\n");
        out.push_str("==============\n\n");
        out.push_str(&format!("Plan ID:     {}\n", self.plan_id));
        out.push_str(&format!("Workspace:   {}\n", self.workspace_id));
        out.push_str(&format!("Profile:     {}\n", self.profile));
        out.push_str(&format!("Commands:    {} total\n", self.command_count));
        out.push_str(&format!("Rehearsable: {}\n", self.rehearsable_count));

        if !self.non_rehearsable.is_empty() {
            out.push_str(&format!(
                "\nNon-rehearsable: {} command(s)\n",
                self.non_rehearsable.len()
            ));
            for cmd in &self.non_rehearsable {
                out.push_str(&format!(
                    "  - {} ({}): {}\n",
                    cmd.command_id, cmd.reason_code, cmd.reason
                ));
            }
        }

        out.push_str(&format!(
            "\nCan proceed: {}\n",
            if self.can_proceed { "yes" } else { "no" }
        ));

        if !self.next_actions.is_empty() {
            out.push_str("\nNext actions:\n");
            for action in &self.next_actions {
                out.push_str(&format!("  - {action}\n"));
            }
        }
        out
    }
}

/// Plan a rehearsal by validating command specs.
pub fn plan_rehearsal(options: &RehearsePlanOptions) -> Result<RehearsePlanReport, DomainError> {
    let plan_id = format!("rplan_{}", generate_id());
    let created_at = Utc::now().to_rfc3339();
    let workspace_id = options
        .workspace
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());

    let non_rehearsable = options
        .commands
        .iter()
        .map(|cmd| {
            if is_rehearsable(&cmd.command) {
                NonRehearsableCommand {
                    command_id: cmd.id.clone(),
                    command: cmd.command.clone(),
                    reason_code: REHEARSAL_UNAVAILABLE_CODE.to_string(),
                    reason: REHEARSAL_UNAVAILABLE_MESSAGE.to_string(),
                    next_action: Some("ee status --json".to_string()),
                }
            } else {
                NonRehearsableCommand {
                    command_id: cmd.id.clone(),
                    command: cmd.command.clone(),
                    reason_code: "external_io".to_string(),
                    reason: format!(
                        "Command '{}' requires external I/O or is not supported in rehearsal",
                        cmd.command
                    ),
                    next_action: Some("Use --dry-run flag on the real command instead".to_string()),
                }
            }
        })
        .collect();

    let next_actions = vec![
        "ee status --json".to_string(),
        "Use command-specific --dry-run paths until rehearsal isolation is implemented".to_string(),
    ];

    Ok(RehearsePlanReport {
        schema: REHEARSE_PLAN_SCHEMA_V1.to_string(),
        plan_id,
        workspace_id,
        command_count: options.commands.len() as u32,
        rehearsable_count: 0,
        non_rehearsable,
        estimated_artifacts: Vec::new(),
        side_path_size_estimate: UNAVAILABLE_VALUE.to_string(),
        profile: options.profile.as_str().to_string(),
        can_proceed: false,
        degradation_codes: vec![REHEARSAL_UNAVAILABLE_CODE.to_string()],
        next_actions,
        created_at,
    })
}

// ============================================================================
// Run Operation
// ============================================================================

/// Options for running a rehearsal.
#[derive(Clone, Debug, Default)]
pub struct RehearseRunOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Command specifications to execute.
    pub commands: Vec<CommandSpec>,
    /// Output directory for artifacts.
    pub output_dir: Option<PathBuf>,
    /// Rehearsal profile.
    pub profile: RehearsalProfile,
}

/// Report from running a rehearsal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RehearseRunReport {
    pub schema: String,
    pub run_id: String,
    pub workspace_id: String,
    pub sandbox_path: String,
    pub source_state_hash: String,
    pub command_list_hash: String,
    pub command_results: Vec<CommandResult>,
    pub expected_effects: Vec<EffectRecord>,
    pub observed_effects: Vec<EffectRecord>,
    pub changed_artifacts: Vec<String>,
    pub degradation_codes: Vec<String>,
    pub redaction_status: String,
    pub elapsed_ms: u64,
    pub overall_result: String,
    pub artifact_paths: HashMap<String, String>,
    pub next_actions: Vec<String>,
    pub created_at: String,
}

/// Result of executing a single command.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandResult {
    pub command_id: String,
    pub command: String,
    pub exit_code: i32,
    pub result: String,
    pub elapsed_ms: u64,
    pub operation_id: Option<String>,
    pub error_message: Option<String>,
}

/// An effect record for comparison.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectRecord {
    pub command_id: String,
    pub effect_type: String,
    pub target: String,
    pub description: String,
}

impl RehearseRunReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(1024);
        out.push_str("Rehearsal Run\n");
        out.push_str("=============\n\n");
        out.push_str(&format!("Run ID:      {}\n", self.run_id));
        out.push_str(&format!("Workspace:   {}\n", self.workspace_id));
        out.push_str(&format!("Sandbox:     {}\n", self.sandbox_path));
        out.push_str(&format!("Result:      {}\n", self.overall_result));
        out.push_str(&format!("Elapsed:     {}ms\n\n", self.elapsed_ms));

        out.push_str(&format!(
            "Commands executed: {}\n",
            self.command_results.len()
        ));
        for result in &self.command_results {
            let status = if result.exit_code == 0 { "✓" } else { "✗" };
            out.push_str(&format!(
                "  {} {} ({}): {} ({}ms)\n",
                status, result.command_id, result.command, result.result, result.elapsed_ms
            ));
        }

        if !self.changed_artifacts.is_empty() {
            out.push_str(&format!(
                "\nChanged artifacts: {}\n",
                self.changed_artifacts.len()
            ));
            for artifact in &self.changed_artifacts {
                out.push_str(&format!("  - {artifact}\n"));
            }
        }

        if !self.next_actions.is_empty() {
            out.push_str("\nNext actions:\n");
            for action in &self.next_actions {
                out.push_str(&format!("  - {action}\n"));
            }
        }
        out
    }
}

/// Run a rehearsal in an isolated sandbox.
pub fn run_rehearsal(options: &RehearseRunOptions) -> Result<RehearseRunReport, DomainError> {
    let run_id = format!("rrun_{}", generate_id());
    let created_at = Utc::now().to_rfc3339();
    let workspace_id = options
        .workspace
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());

    let command_results = options
        .commands
        .iter()
        .map(|cmd| CommandResult {
            command_id: cmd.id.clone(),
            command: cmd.command.clone(),
            exit_code: -1,
            result: "not_executed".to_string(),
            elapsed_ms: 0,
            operation_id: None,
            error_message: Some(REHEARSAL_UNAVAILABLE_MESSAGE.to_string()),
        })
        .collect();

    let expected_effects = options
        .commands
        .iter()
        .map(|cmd| EffectRecord {
            command_id: cmd.id.clone(),
            effect_type: cmd.expected_effect.clone(),
            target: UNAVAILABLE_VALUE.to_string(),
            description: format!(
                "Planned {} effect from {}; not observed because rehearsal execution is unavailable",
                cmd.expected_effect, cmd.command
            ),
        })
        .collect();

    Ok(RehearseRunReport {
        schema: REHEARSE_RUN_SCHEMA_V1.to_string(),
        run_id,
        workspace_id,
        sandbox_path: UNAVAILABLE_VALUE.to_string(),
        source_state_hash: UNAVAILABLE_VALUE.to_string(),
        command_list_hash: UNAVAILABLE_VALUE.to_string(),
        command_results,
        expected_effects,
        observed_effects: Vec::new(),
        changed_artifacts: Vec::new(),
        degradation_codes: vec![REHEARSAL_UNAVAILABLE_CODE.to_string()],
        redaction_status: if options.profile == RehearsalProfile::Privacy {
            "redacted".to_string()
        } else {
            "none".to_string()
        },
        elapsed_ms: 0,
        overall_result: UNAVAILABLE_VALUE.to_string(),
        artifact_paths: HashMap::new(),
        next_actions: vec!["ee status --json".to_string()],
        created_at,
    })
}

// ============================================================================
// Inspect Operation
// ============================================================================

/// Options for inspecting a rehearsal artifact.
#[derive(Clone, Debug, Default)]
pub struct RehearseInspectOptions {
    /// Run ID or artifact path to inspect.
    pub artifact_id: String,
    /// Workspace path for context.
    pub workspace: PathBuf,
}

/// Report from inspecting a rehearsal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RehearseInspectReport {
    pub schema: String,
    pub artifact_id: String,
    pub artifact_type: String,
    pub schema_version: String,
    pub manifest_hash: String,
    pub command_spec_hash: String,
    pub source_state_hash: String,
    pub sandbox_state_hash: String,
    pub command_count: u32,
    pub success_count: u32,
    pub failure_count: u32,
    pub redaction_summary: RedactionSummary,
    pub effect_summary: EffectSummary,
    pub integrity_status: String,
    pub warnings: Vec<String>,
    pub inspected_at: String,
}

/// Summary of redaction status.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RedactionSummary {
    pub redacted_fields: u32,
    pub redacted_artifacts: u32,
    pub policy: String,
}

/// Summary of effects.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EffectSummary {
    pub read_only: u32,
    pub write_memory: u32,
    pub write_index: u32,
    pub write_config: u32,
}

impl RehearseInspectReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("Rehearsal Inspection\n");
        out.push_str("====================\n\n");
        out.push_str(&format!("Artifact:   {}\n", self.artifact_id));
        out.push_str(&format!("Type:       {}\n", self.artifact_type));
        out.push_str(&format!("Schema:     {}\n", self.schema_version));
        out.push_str(&format!("Integrity:  {}\n\n", self.integrity_status));

        out.push_str(&format!(
            "Commands: {} total, {} success, {} failed\n",
            self.command_count, self.success_count, self.failure_count
        ));

        out.push_str(&format!(
            "\nEffects: {} read, {} write_memory, {} write_index, {} write_config\n",
            self.effect_summary.read_only,
            self.effect_summary.write_memory,
            self.effect_summary.write_index,
            self.effect_summary.write_config
        ));

        if !self.warnings.is_empty() {
            out.push_str("\nWarnings:\n");
            for warning in &self.warnings {
                out.push_str(&format!("  - {warning}\n"));
            }
        }
        out
    }
}

/// Inspect a rehearsal artifact.
pub fn inspect_rehearsal(
    options: &RehearseInspectOptions,
) -> Result<RehearseInspectReport, DomainError> {
    let inspected_at = Utc::now().to_rfc3339();

    Ok(RehearseInspectReport {
        schema: REHEARSE_INSPECT_SCHEMA_V1.to_string(),
        artifact_id: options.artifact_id.clone(),
        artifact_type: UNAVAILABLE_VALUE.to_string(),
        schema_version: REHEARSE_RUN_SCHEMA_V1.to_string(),
        manifest_hash: UNAVAILABLE_VALUE.to_string(),
        command_spec_hash: UNAVAILABLE_VALUE.to_string(),
        source_state_hash: UNAVAILABLE_VALUE.to_string(),
        sandbox_state_hash: UNAVAILABLE_VALUE.to_string(),
        command_count: 0,
        success_count: 0,
        failure_count: 0,
        redaction_summary: RedactionSummary {
            redacted_fields: 0,
            redacted_artifacts: 0,
            policy: UNAVAILABLE_VALUE.to_string(),
        },
        effect_summary: EffectSummary::default(),
        integrity_status: UNAVAILABLE_VALUE.to_string(),
        warnings: vec![REHEARSAL_UNAVAILABLE_MESSAGE.to_string()],
        inspected_at,
    })
}

// ============================================================================
// Promote-Plan Operation
// ============================================================================

/// Options for generating a promote plan.
#[derive(Clone, Debug, Default)]
pub struct RehearsePromotePlanOptions {
    /// Run ID or artifact to promote.
    pub artifact_id: String,
    /// Workspace path for context.
    pub workspace: PathBuf,
}

/// Report from generating a promote plan.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RehearsePromotePlanReport {
    pub schema: String,
    pub artifact_id: String,
    pub plan_steps: Vec<PromoteStep>,
    pub preconditions: Vec<Precondition>,
    pub backup_requirements: Vec<String>,
    pub audit_checks: Vec<String>,
    pub stop_conditions: Vec<String>,
    pub expected_queries: Vec<String>,
    pub warnings: Vec<String>,
    pub is_safe: bool,
    pub created_at: String,
}

/// A step in the promote plan.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromoteStep {
    pub sequence: u32,
    pub command: String,
    pub args: Vec<String>,
    pub expected_effect: String,
    pub confirmation_required: bool,
    pub rollback_command: Option<String>,
}

/// A precondition for promotion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Precondition {
    pub check: String,
    pub description: String,
    pub required: bool,
}

impl RehearsePromotePlanReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(1024);
        out.push_str("Rehearsal Promote Plan\n");
        out.push_str("======================\n\n");
        out.push_str(&format!("Artifact: {}\n", self.artifact_id));
        out.push_str(&format!(
            "Safe:     {}\n\n",
            if self.is_safe { "yes" } else { "no" }
        ));

        out.push_str("Preconditions:\n");
        for pre in &self.preconditions {
            let marker = if pre.required {
                "[required]"
            } else {
                "[optional]"
            };
            out.push_str(&format!(
                "  {} {}: {}\n",
                marker, pre.check, pre.description
            ));
        }

        out.push_str("\nBackup requirements:\n");
        for req in &self.backup_requirements {
            out.push_str(&format!("  - {req}\n"));
        }

        out.push_str("\nExecution steps:\n");
        for step in &self.plan_steps {
            let confirm = if step.confirmation_required {
                " [confirm]"
            } else {
                ""
            };
            out.push_str(&format!(
                "  {}. ee {} {}{}\n",
                step.sequence,
                step.command,
                step.args.join(" "),
                confirm
            ));
        }

        out.push_str("\nStop conditions:\n");
        for cond in &self.stop_conditions {
            out.push_str(&format!("  - {cond}\n"));
        }

        if !self.warnings.is_empty() {
            out.push_str("\nWarnings:\n");
            for warning in &self.warnings {
                out.push_str(&format!("  - {warning}\n"));
            }
        }
        out
    }
}

/// Generate a promote plan from a rehearsal artifact.
pub fn promote_plan_rehearsal(
    options: &RehearsePromotePlanOptions,
) -> Result<RehearsePromotePlanReport, DomainError> {
    let created_at = Utc::now().to_rfc3339();

    Ok(RehearsePromotePlanReport {
        schema: REHEARSE_PROMOTE_PLAN_SCHEMA_V1.to_string(),
        artifact_id: options.artifact_id.clone(),
        plan_steps: Vec::new(),
        preconditions: vec![Precondition {
            check: "artifact_manifest_available".to_string(),
            description:
                "A real rehearsal artifact manifest must exist before promotion can be planned"
                    .to_string(),
            required: true,
        }],
        backup_requirements: Vec::new(),
        audit_checks: Vec::new(),
        stop_conditions: Vec::new(),
        expected_queries: Vec::new(),
        warnings: vec![REHEARSAL_UNAVAILABLE_MESSAGE.to_string()],
        is_safe: false,
        created_at,
    })
}

// ============================================================================
// Helpers
// ============================================================================

/// Check if a command is rehearsable.
fn is_rehearsable(command: &str) -> bool {
    matches!(
        command,
        "remember"
            | "search"
            | "context"
            | "why"
            | "status"
            | "health"
            | "capabilities"
            | "memory"
            | "index"
            | "doctor"
            | "check"
            | "situation"
            | "economy"
            | "procedure"
            | "preflight"
    )
}

/// Generate a short random ID.
fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{:x}", timestamp & 0xFFFFFFFF)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    #[test]
    fn plan_reports_unavailable_without_sandbox_artifact_claims() -> TestResult {
        let options = RehearsePlanOptions {
            commands: vec![CommandSpec {
                id: "cmd_1".to_string(),
                command: "remember".to_string(),
                args: vec!["test".to_string()],
                expected_effect: "write_memory".to_string(),
                stop_on_failure: true,
                idempotency_key: None,
            }],
            ..Default::default()
        };

        let report = plan_rehearsal(&options).map_err(|e| e.message())?;
        assert!(report.plan_id.starts_with("rplan_"));
        assert_eq!(report.command_count, 1);
        assert_eq!(report.rehearsable_count, 0);
        assert_eq!(report.non_rehearsable.len(), 1);
        assert_eq!(
            report.non_rehearsable[0].reason_code,
            REHEARSAL_UNAVAILABLE_CODE
        );
        assert!(report.estimated_artifacts.is_empty());
        assert_eq!(report.side_path_size_estimate, UNAVAILABLE_VALUE);
        assert!(!report.can_proceed);
        assert_eq!(
            report.degradation_codes,
            vec![REHEARSAL_UNAVAILABLE_CODE.to_string()]
        );
        Ok(())
    }

    #[test]
    fn plan_identifies_non_rehearsable_commands() -> TestResult {
        let options = RehearsePlanOptions {
            commands: vec![CommandSpec {
                id: "cmd_1".to_string(),
                command: "external_api_call".to_string(),
                args: vec![],
                expected_effect: "external_io".to_string(),
                stop_on_failure: true,
                idempotency_key: None,
            }],
            ..Default::default()
        };

        let report = plan_rehearsal(&options).map_err(|e| e.message())?;
        assert_eq!(report.non_rehearsable.len(), 1);
        assert_eq!(report.non_rehearsable[0].reason_code, "external_io");
        assert!(!report.can_proceed);
        Ok(())
    }

    #[test]
    fn quick_profile_does_not_override_unavailable_rehearsal_isolation() -> TestResult {
        let options = RehearsePlanOptions {
            profile: RehearsalProfile::Quick,
            commands: vec![CommandSpec {
                id: "cmd_1".to_string(),
                command: "status".to_string(),
                args: Vec::new(),
                expected_effect: "read_only".to_string(),
                stop_on_failure: true,
                idempotency_key: None,
            }],
            ..Default::default()
        };

        let report = plan_rehearsal(&options).map_err(|e| e.message())?;
        assert_eq!(report.profile, "quick");
        assert_eq!(report.rehearsable_count, 0);
        assert!(!report.can_proceed);
        assert!(report.estimated_artifacts.is_empty());
        Ok(())
    }

    #[test]
    fn run_reports_unavailable_without_simulated_artifacts() -> TestResult {
        let options = RehearseRunOptions {
            commands: vec![CommandSpec {
                id: "cmd_1".to_string(),
                command: "remember".to_string(),
                args: vec!["test".to_string()],
                expected_effect: "write_memory".to_string(),
                stop_on_failure: true,
                idempotency_key: None,
            }],
            ..Default::default()
        };

        let report = run_rehearsal(&options).map_err(|e| e.message())?;
        assert!(report.run_id.starts_with("rrun_"));
        assert_eq!(report.sandbox_path, UNAVAILABLE_VALUE);
        assert_eq!(report.source_state_hash, UNAVAILABLE_VALUE);
        assert_eq!(report.command_list_hash, UNAVAILABLE_VALUE);
        assert_eq!(report.command_results.len(), 1);
        assert_eq!(report.command_results[0].exit_code, -1);
        assert_eq!(report.command_results[0].result, "not_executed");
        assert!(report.command_results[0].operation_id.is_none());
        assert!(report.observed_effects.is_empty());
        assert!(report.changed_artifacts.is_empty());
        assert!(report.artifact_paths.is_empty());
        assert_eq!(
            report.degradation_codes,
            vec![REHEARSAL_UNAVAILABLE_CODE.to_string()]
        );
        assert_eq!(report.overall_result, UNAVAILABLE_VALUE);
        Ok(())
    }

    #[test]
    fn inspect_reports_unavailable_without_hash_claims() -> TestResult {
        let options = RehearseInspectOptions {
            artifact_id: "rrun_test".to_string(),
            ..Default::default()
        };

        let report = inspect_rehearsal(&options).map_err(|e| e.message())?;
        assert_eq!(report.artifact_type, UNAVAILABLE_VALUE);
        assert_eq!(report.manifest_hash, UNAVAILABLE_VALUE);
        assert_eq!(report.command_spec_hash, UNAVAILABLE_VALUE);
        assert_eq!(report.source_state_hash, UNAVAILABLE_VALUE);
        assert_eq!(report.sandbox_state_hash, UNAVAILABLE_VALUE);
        assert_eq!(report.command_count, 0);
        assert_eq!(report.success_count, 0);
        assert_eq!(report.integrity_status, UNAVAILABLE_VALUE);
        assert!(!report.warnings.is_empty());
        Ok(())
    }

    #[test]
    fn promote_plan_reports_unavailable_without_canned_steps() -> TestResult {
        let options = RehearsePromotePlanOptions {
            artifact_id: "rrun_test".to_string(),
            ..Default::default()
        };

        let report = promote_plan_rehearsal(&options).map_err(|e| e.message())?;
        assert!(report.plan_steps.is_empty());
        assert!(!report.preconditions.is_empty());
        assert!(report.backup_requirements.is_empty());
        assert!(report.audit_checks.is_empty());
        assert!(report.stop_conditions.is_empty());
        assert!(report.expected_queries.is_empty());
        assert!(!report.is_safe);
        assert!(!report.warnings.is_empty());
        Ok(())
    }

    #[test]
    fn rehearsal_profile_roundtrips() {
        assert_eq!(
            RehearsalProfile::from_str(RehearsalProfile::Quick.as_str()),
            Some(RehearsalProfile::Quick)
        );
        assert_eq!(
            RehearsalProfile::from_str(RehearsalProfile::Full.as_str()),
            Some(RehearsalProfile::Full)
        );
        assert_eq!(
            RehearsalProfile::from_str(RehearsalProfile::Privacy.as_str()),
            Some(RehearsalProfile::Privacy)
        );
    }
}
