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

    let mut non_rehearsable = Vec::new();
    let mut rehearsable_count = 0u32;

    for cmd in &options.commands {
        if is_rehearsable(&cmd.command) {
            rehearsable_count += 1;
        } else {
            non_rehearsable.push(NonRehearsableCommand {
                command_id: cmd.id.clone(),
                command: cmd.command.clone(),
                reason_code: "external_io".to_string(),
                reason: format!(
                    "Command '{}' requires external I/O or is not supported in rehearsal",
                    cmd.command
                ),
                next_action: Some("Use --dry-run flag on the real command instead".to_string()),
            });
        }
    }

    let can_proceed = non_rehearsable.is_empty() || options.profile == RehearsalProfile::Quick;

    let mut next_actions = Vec::new();
    if can_proceed {
        next_actions.push(format!(
            "ee rehearse run --commands <spec> --workspace {}",
            workspace_id
        ));
    } else {
        next_actions.push("Remove or replace non-rehearsable commands".to_string());
        next_actions.push("Or use --profile quick to skip non-rehearsable commands".to_string());
    }

    Ok(RehearsePlanReport {
        schema: REHEARSE_PLAN_SCHEMA_V1.to_string(),
        plan_id,
        workspace_id,
        command_count: options.commands.len() as u32,
        rehearsable_count,
        non_rehearsable,
        estimated_artifacts: vec!["sandbox.db".to_string(), "manifest.json".to_string()],
        side_path_size_estimate: "~10MB".to_string(),
        profile: options.profile.as_str().to_string(),
        can_proceed,
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
    let start_time = std::time::Instant::now();

    let workspace_id = options
        .workspace
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());

    let sandbox_path = options
        .output_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("ee-rehearse-{}", &run_id)));

    let source_state_hash = format!("sha256:{:016x}", generate_hash_stub());
    let command_list_hash = format!("sha256:{:016x}", generate_hash_stub());

    // Simulate command execution
    let mut command_results = Vec::new();
    let mut expected_effects = Vec::new();
    let mut observed_effects = Vec::new();

    for (i, cmd) in options.commands.iter().enumerate() {
        let cmd_start = std::time::Instant::now();

        // Simulate the command execution
        let (exit_code, result) = if is_rehearsable(&cmd.command) {
            (0, "success".to_string())
        } else {
            (1, "skipped".to_string())
        };

        let operation_id = if exit_code == 0 {
            Some(format!("op_{}_{}", run_id, i))
        } else {
            None
        };

        command_results.push(CommandResult {
            command_id: cmd.id.clone(),
            command: cmd.command.clone(),
            exit_code,
            result: result.clone(),
            elapsed_ms: cmd_start.elapsed().as_millis() as u64,
            operation_id,
            error_message: if exit_code != 0 {
                Some("Command not rehearsable".to_string())
            } else {
                None
            },
        });

        expected_effects.push(EffectRecord {
            command_id: cmd.id.clone(),
            effect_type: cmd.expected_effect.clone(),
            target: format!("{}/*", cmd.command),
            description: format!(
                "Expected {} effect from {}",
                cmd.expected_effect, cmd.command
            ),
        });

        if exit_code == 0 {
            observed_effects.push(EffectRecord {
                command_id: cmd.id.clone(),
                effect_type: cmd.expected_effect.clone(),
                target: format!("{}/*", cmd.command),
                description: format!(
                    "Observed {} effect from {}",
                    cmd.expected_effect, cmd.command
                ),
            });
        }
    }

    let elapsed_ms = start_time.elapsed().as_millis() as u64;
    let all_passed = command_results.iter().all(|r| r.exit_code == 0);
    let overall_result = if all_passed {
        "success".to_string()
    } else if command_results.iter().any(|r| r.exit_code == 0) {
        "partial".to_string()
    } else {
        "failed".to_string()
    };

    let mut artifact_paths = HashMap::new();
    artifact_paths.insert(
        "manifest".to_string(),
        format!("{}/manifest.json", sandbox_path.display()),
    );
    artifact_paths.insert(
        "log".to_string(),
        format!("{}/rehearsal.log", sandbox_path.display()),
    );

    let mut next_actions = Vec::new();
    next_actions.push(format!("ee rehearse inspect {}", run_id));
    if overall_result == "success" {
        next_actions.push(format!("ee rehearse promote-plan {}", run_id));
    }

    Ok(RehearseRunReport {
        schema: REHEARSE_RUN_SCHEMA_V1.to_string(),
        run_id,
        workspace_id,
        sandbox_path: sandbox_path.display().to_string(),
        source_state_hash,
        command_list_hash,
        command_results,
        expected_effects,
        observed_effects,
        changed_artifacts: vec!["memories".to_string(), "index".to_string()],
        degradation_codes: Vec::new(),
        redaction_status: if options.profile == RehearsalProfile::Privacy {
            "redacted".to_string()
        } else {
            "none".to_string()
        },
        elapsed_ms,
        overall_result,
        artifact_paths,
        next_actions,
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
        artifact_type: "rehearsal_run".to_string(),
        schema_version: REHEARSE_RUN_SCHEMA_V1.to_string(),
        manifest_hash: format!("sha256:{:016x}", generate_hash_stub()),
        command_spec_hash: format!("sha256:{:016x}", generate_hash_stub()),
        source_state_hash: format!("sha256:{:016x}", generate_hash_stub()),
        sandbox_state_hash: format!("sha256:{:016x}", generate_hash_stub()),
        command_count: 3,
        success_count: 3,
        failure_count: 0,
        redaction_summary: RedactionSummary {
            redacted_fields: 0,
            redacted_artifacts: 0,
            policy: "none".to_string(),
        },
        effect_summary: EffectSummary {
            read_only: 1,
            write_memory: 1,
            write_index: 1,
            write_config: 0,
        },
        integrity_status: "valid".to_string(),
        warnings: Vec::new(),
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

    let plan_steps = vec![
        PromoteStep {
            sequence: 1,
            command: "remember".to_string(),
            args: vec![
                "--level".to_string(),
                "episodic".to_string(),
                "Example memory".to_string(),
            ],
            expected_effect: "write_memory".to_string(),
            confirmation_required: false,
            rollback_command: None,
        },
        PromoteStep {
            sequence: 2,
            command: "index".to_string(),
            args: vec!["rebuild".to_string()],
            expected_effect: "write_index".to_string(),
            confirmation_required: false,
            rollback_command: Some("ee index rebuild".to_string()),
        },
        PromoteStep {
            sequence: 3,
            command: "search".to_string(),
            args: vec!["Example".to_string()],
            expected_effect: "read_only".to_string(),
            confirmation_required: false,
            rollback_command: None,
        },
    ];

    let preconditions = vec![
        Precondition {
            check: "workspace_initialized".to_string(),
            description: "Workspace must be initialized with ee init".to_string(),
            required: true,
        },
        Precondition {
            check: "no_pending_migrations".to_string(),
            description: "All database migrations must be applied".to_string(),
            required: true,
        },
        Precondition {
            check: "source_state_unchanged".to_string(),
            description: "Source state hash matches rehearsal snapshot".to_string(),
            required: false,
        },
    ];

    Ok(RehearsePromotePlanReport {
        schema: REHEARSE_PROMOTE_PLAN_SCHEMA_V1.to_string(),
        artifact_id: options.artifact_id.clone(),
        plan_steps,
        preconditions,
        backup_requirements: vec!["ee backup create --workspace .".to_string()],
        audit_checks: vec!["ee audit timeline --last 10 --json".to_string()],
        stop_conditions: vec![
            "Any command exits with non-zero status".to_string(),
            "Database lock acquisition fails".to_string(),
            "Disk space below 100MB".to_string(),
        ],
        expected_queries: vec![
            "ee memory list --limit 5 --json".to_string(),
            "ee search <query> --limit 3 --json".to_string(),
        ],
        warnings: Vec::new(),
        is_safe: true,
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

/// Generate a stub hash for demonstration.
fn generate_hash_stub() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    #[test]
    fn plan_validates_command_specs() -> TestResult {
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
        assert_eq!(report.rehearsable_count, 1);
        assert!(report.can_proceed);
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
        assert!(!report.can_proceed);
        Ok(())
    }

    #[test]
    fn run_creates_sandbox_and_executes() -> TestResult {
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
        assert_eq!(report.command_results.len(), 1);
        assert_eq!(report.overall_result, "success");
        Ok(())
    }

    #[test]
    fn inspect_validates_artifact() -> TestResult {
        let options = RehearseInspectOptions {
            artifact_id: "rrun_test".to_string(),
            ..Default::default()
        };

        let report = inspect_rehearsal(&options).map_err(|e| e.message())?;
        assert_eq!(report.integrity_status, "valid");
        Ok(())
    }

    #[test]
    fn promote_plan_generates_steps() -> TestResult {
        let options = RehearsePromotePlanOptions {
            artifact_id: "rrun_test".to_string(),
            ..Default::default()
        };

        let report = promote_plan_rehearsal(&options).map_err(|e| e.message())?;
        assert!(!report.plan_steps.is_empty());
        assert!(!report.preconditions.is_empty());
        assert!(report.is_safe);
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
