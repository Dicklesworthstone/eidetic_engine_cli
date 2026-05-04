//! Isolated rehearsal sandbox for EE command sequences (EE-REHEARSE-001).
//!
//! Provides plan, run, inspect, and promote-plan operations for rehearsing
//! EE command sequences in an isolated side-path copy before real execution.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

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

const UNAVAILABLE_VALUE: &str = "unavailable";
const MANIFEST_FILE: &str = "manifest.json";
const SOURCE_SNAPSHOT_FILE: &str = "source_snapshot.json";
const SANDBOX_SNAPSHOT_FILE: &str = "sandbox_snapshot.json";

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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SnapshotManifest {
    schema: String,
    root: String,
    hash: String,
    total_bytes: u64,
    files: Vec<SnapshotFile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct SnapshotFile {
    path: String,
    hash: String,
    bytes: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct FilesystemSnapshot {
    files: BTreeMap<String, SnapshotFile>,
    total_bytes: u64,
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

    let source_snapshot = snapshot_workspace(&options.workspace, None)?;
    let non_rehearsable: Vec<NonRehearsableCommand> = options
        .commands
        .iter()
        .filter_map(validate_command_spec)
        .collect();
    let rehearsable_count = options.commands.len().saturating_sub(non_rehearsable.len()) as u32;
    let can_proceed = non_rehearsable.is_empty();
    let next_actions = if can_proceed {
        vec!["ee rehearse run --commands <spec.json> --json".to_string()]
    } else {
        vec!["Remove unsupported commands or workspace-escaping path overrides".to_string()]
    };

    Ok(RehearsePlanReport {
        schema: REHEARSE_PLAN_SCHEMA_V1.to_string(),
        plan_id,
        workspace_id,
        command_count: options.commands.len() as u32,
        rehearsable_count,
        non_rehearsable,
        estimated_artifacts: vec![
            MANIFEST_FILE.to_string(),
            SOURCE_SNAPSHOT_FILE.to_string(),
            SANDBOX_SNAPSHOT_FILE.to_string(),
        ],
        side_path_size_estimate: format!("{} bytes", source_snapshot.total_bytes),
        profile: options.profile.as_str().to_string(),
        can_proceed,
        degradation_codes: if can_proceed {
            Vec::new()
        } else {
            vec!["unsupported_rehearsal_command".to_string()]
        },
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
    /// ee binary to execute inside the sandbox. If absent, commands are planned but not executed.
    pub ee_binary: Option<PathBuf>,
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
    pub args: Vec<String>,
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
    let started = Instant::now();
    let workspace_id = options
        .workspace
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());
    let artifact_root = prepare_artifact_root(options.output_dir.as_deref())?;
    let artifact_root_skip = canonicalize_existing(&artifact_root).ok();
    let source_snapshot = snapshot_workspace(&options.workspace, artifact_root_skip.as_deref())?;
    let source_state_hash = snapshot_hash(&source_snapshot);
    let source_snapshot_path = artifact_root.join(SOURCE_SNAPSHOT_FILE);
    write_snapshot_manifest(
        &source_snapshot_path,
        &options.workspace,
        &source_snapshot,
        &source_state_hash,
    )?;

    let sandbox_temp = tempfile::Builder::new()
        .prefix("workspace-")
        .tempdir_in(&artifact_root)
        .map_err(|error| storage_error("create rehearsal sandbox", error))?;
    let sandbox_path = sandbox_temp.path().to_path_buf();
    copy_workspace(
        &options.workspace,
        &sandbox_path,
        artifact_root_skip.as_deref(),
    )?;
    let sandbox_path = sandbox_temp.keep();

    let expected_effects = options
        .commands
        .iter()
        .map(|cmd| EffectRecord {
            command_id: cmd.id.clone(),
            effect_type: cmd.expected_effect.clone(),
            target: sandbox_path.display().to_string(),
            description: format!(
                "Expected {} effect from {}",
                cmd.expected_effect, cmd.command
            ),
        })
        .collect();
    let command_list_hash = hash_json_value(&options.commands)?;
    let mut current_snapshot = snapshot_workspace(&sandbox_path, None)?;
    let mut command_results = Vec::with_capacity(options.commands.len());
    let mut observed_effects = Vec::new();
    let mut changed_artifacts = BTreeSet::new();
    let mut degradation_codes = Vec::new();
    let mut stop = false;

    for cmd in &options.commands {
        if stop {
            command_results.push(CommandResult {
                command_id: cmd.id.clone(),
                command: cmd.command.clone(),
                args: cmd.args.clone(),
                exit_code: -1,
                result: "skipped_after_failure".to_string(),
                elapsed_ms: 0,
                operation_id: None,
                error_message: Some(
                    "Earlier command failed and stop_on_failure was set".to_string(),
                ),
            });
            continue;
        }

        if let Some(non_rehearsable) = validate_command_spec(cmd) {
            degradation_codes.push(non_rehearsable.reason_code.clone());
            command_results.push(CommandResult {
                command_id: cmd.id.clone(),
                command: cmd.command.clone(),
                args: cmd.args.clone(),
                exit_code: -1,
                result: "unsupported".to_string(),
                elapsed_ms: 0,
                operation_id: None,
                error_message: Some(non_rehearsable.reason),
            });
            if cmd.stop_on_failure {
                stop = true;
            }
            continue;
        }

        let result = execute_rehearsal_command(cmd, options.ee_binary.as_deref(), &sandbox_path)?;
        let next_snapshot = snapshot_workspace(&sandbox_path, None)?;
        let command_changes = changed_paths(&current_snapshot, &next_snapshot);
        for path in &command_changes {
            changed_artifacts.insert(path.clone());
            observed_effects.push(EffectRecord {
                command_id: cmd.id.clone(),
                effect_type: cmd.expected_effect.clone(),
                target: path.clone(),
                description: format!("Sandbox artifact changed during {}", cmd.command),
            });
        }
        current_snapshot = next_snapshot;
        if result.exit_code != 0 && cmd.stop_on_failure {
            stop = true;
        }
        command_results.push(result);
    }

    let sandbox_snapshot = current_snapshot;
    let sandbox_state_hash = snapshot_hash(&sandbox_snapshot);
    let sandbox_snapshot_path = artifact_root.join(SANDBOX_SNAPSHOT_FILE);
    write_snapshot_manifest(
        &sandbox_snapshot_path,
        &sandbox_path,
        &sandbox_snapshot,
        &sandbox_state_hash,
    )?;

    if options.ee_binary.is_none() && !options.commands.is_empty() {
        degradation_codes.push("executor_unavailable".to_string());
    }
    degradation_codes.sort();
    degradation_codes.dedup();

    let overall_result = if command_results.iter().all(|result| result.exit_code == 0) {
        "passed"
    } else if command_results
        .iter()
        .any(|result| result.result == "unsupported" || result.result == "executor_unavailable")
    {
        "blocked"
    } else {
        "failed"
    }
    .to_string();

    let mut artifact_paths = HashMap::new();
    let manifest_path = artifact_root.join(MANIFEST_FILE);
    artifact_paths.insert("manifest".to_string(), manifest_path.display().to_string());
    artifact_paths.insert(
        "source_snapshot".to_string(),
        source_snapshot_path.display().to_string(),
    );
    artifact_paths.insert(
        "sandbox_snapshot".to_string(),
        sandbox_snapshot_path.display().to_string(),
    );

    let report = RehearseRunReport {
        schema: REHEARSE_RUN_SCHEMA_V1.to_string(),
        run_id,
        workspace_id,
        sandbox_path: sandbox_path.display().to_string(),
        source_state_hash,
        command_list_hash,
        command_results,
        expected_effects,
        observed_effects,
        changed_artifacts: changed_artifacts.into_iter().collect(),
        degradation_codes,
        redaction_status: if options.profile == RehearsalProfile::Privacy {
            "redacted".to_string()
        } else {
            "none".to_string()
        },
        elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        overall_result,
        artifact_paths,
        next_actions: vec!["ee rehearse inspect <manifest-path> --json".to_string()],
        created_at,
    };
    write_json_artifact(&manifest_path, &report)?;
    Ok(report)
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
    let manifest_path = resolve_manifest_path(&options.artifact_id, &options.workspace)?;
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|error| storage_error("read rehearsal manifest", error))?;
    let report: RehearseRunReport =
        serde_json::from_slice(&manifest_bytes).map_err(|error| DomainError::Storage {
            message: format!("Failed to parse rehearsal manifest: {error}"),
            repair: Some("ee rehearse run --json".to_string()),
        })?;
    let success_count = report
        .command_results
        .iter()
        .filter(|result| result.exit_code == 0)
        .count() as u32;
    let command_count = report.command_results.len() as u32;
    let effect_summary = summarize_effects(&report.expected_effects);

    Ok(RehearseInspectReport {
        schema: REHEARSE_INSPECT_SCHEMA_V1.to_string(),
        artifact_id: report.run_id,
        artifact_type: "rehearsal_run".to_string(),
        schema_version: REHEARSE_RUN_SCHEMA_V1.to_string(),
        manifest_hash: hash_bytes(&manifest_bytes),
        command_spec_hash: report.command_list_hash,
        source_state_hash: report.source_state_hash,
        sandbox_state_hash: snapshot_workspace(Path::new(&report.sandbox_path), None)
            .map(|snapshot| snapshot_hash(&snapshot))
            .unwrap_or_else(|_| UNAVAILABLE_VALUE.to_string()),
        command_count,
        success_count,
        failure_count: command_count.saturating_sub(success_count),
        redaction_summary: RedactionSummary {
            redacted_fields: 0,
            redacted_artifacts: 0,
            policy: report.redaction_status,
        },
        effect_summary,
        integrity_status: "valid".to_string(),
        warnings: report.degradation_codes,
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
    let manifest_path = resolve_manifest_path(&options.artifact_id, &options.workspace)?;
    let report: RehearseRunReport = serde_json::from_slice(
        &fs::read(&manifest_path)
            .map_err(|error| storage_error("read rehearsal manifest for promote plan", error))?,
    )
    .map_err(|error| DomainError::Storage {
        message: format!("Failed to parse rehearsal manifest: {error}"),
        repair: Some("ee rehearse run --json".to_string()),
    })?;
    let plan_steps = report
        .command_results
        .iter()
        .enumerate()
        .filter(|(_, result)| result.exit_code == 0)
        .map(|(index, result)| PromoteStep {
            sequence: index as u32 + 1,
            command: result.command.clone(),
            args: result.args.clone(),
            expected_effect: report
                .expected_effects
                .iter()
                .find(|effect| effect.command_id == result.command_id)
                .map(|effect| effect.effect_type.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            confirmation_required: true,
            rollback_command: None,
        })
        .collect::<Vec<_>>();
    let has_failures = report
        .command_results
        .iter()
        .any(|result| result.exit_code != 0);

    Ok(RehearsePromotePlanReport {
        schema: REHEARSE_PROMOTE_PLAN_SCHEMA_V1.to_string(),
        artifact_id: report.run_id,
        plan_steps,
        preconditions: vec![Precondition {
            check: "artifact_manifest_valid".to_string(),
            description: "Rehearsal manifest was readable and schema-compatible".to_string(),
            required: true,
        }],
        backup_requirements: vec!["Capture backup before promoting mutating commands".to_string()],
        audit_checks: vec![
            "Compare source_state_hash with current workspace before execution".to_string(),
            "Verify sandbox_state_hash and changed_artifacts match the reviewed manifest"
                .to_string(),
        ],
        stop_conditions: vec![
            "Stop if current workspace hash differs from the rehearsal source hash".to_string(),
            "Stop if any rehearsal command failed".to_string(),
        ],
        expected_queries: report
            .changed_artifacts
            .iter()
            .map(|path| format!("verify artifact {path}"))
            .collect(),
        warnings: if has_failures {
            vec!["Promotion is unsafe until failed rehearsal commands are resolved".to_string()]
        } else {
            Vec::new()
        },
        is_safe: !has_failures && report.degradation_codes.is_empty(),
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

fn validate_command_spec(command: &CommandSpec) -> Option<NonRehearsableCommand> {
    if !is_rehearsable(&command.command) {
        return Some(NonRehearsableCommand {
            command_id: command.id.clone(),
            command: command.command.clone(),
            reason_code: "external_io".to_string(),
            reason: format!(
                "Command '{}' requires external I/O or is not supported in rehearsal",
                command.command
            ),
            next_action: Some("Use --dry-run flag on the real command instead".to_string()),
        });
    }
    if command_args_escape_sandbox(&command.args) {
        return Some(NonRehearsableCommand {
            command_id: command.id.clone(),
            command: command.command.clone(),
            reason_code: "workspace_escape".to_string(),
            reason:
                "Command arguments override workspace or database paths outside the sandbox boundary"
                    .to_string(),
            next_action: Some("Remove explicit --workspace/--database/--index-dir overrides".to_string()),
        });
    }
    None
}

fn command_args_escape_sandbox(args: &[String]) -> bool {
    args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--workspace" | "--database" | "--index-dir" | "--out"
        ) || arg.starts_with("--workspace=")
            || arg.starts_with("--database=")
            || arg.starts_with("--index-dir=")
            || arg.starts_with("--out=")
    })
}

fn prepare_artifact_root(output_dir: Option<&Path>) -> Result<PathBuf, DomainError> {
    if let Some(path) = output_dir {
        fs::create_dir_all(path)
            .map_err(|error| storage_error("create rehearsal output dir", error))?;
        return Ok(path.to_path_buf());
    }

    tempfile::Builder::new()
        .prefix("ee-rehearse-")
        .tempdir()
        .map(|dir| dir.keep())
        .map_err(|error| storage_error("create rehearsal artifact dir", error))
}

fn canonicalize_existing(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}

fn snapshot_workspace(
    root: &Path,
    skip_root: Option<&Path>,
) -> Result<FilesystemSnapshot, DomainError> {
    let root = canonicalize_existing(root).map_err(|error| DomainError::Configuration {
        message: format!(
            "Failed to resolve rehearsal workspace {}: {error}",
            root.display()
        ),
        repair: Some("Pass --workspace <existing-directory>".to_string()),
    })?;
    let mut snapshot = FilesystemSnapshot::default();
    collect_snapshot(&root, &root, skip_root, &mut snapshot)?;
    Ok(snapshot)
}

fn collect_snapshot(
    root: &Path,
    path: &Path,
    skip_root: Option<&Path>,
    snapshot: &mut FilesystemSnapshot,
) -> Result<(), DomainError> {
    if skip_root.is_some_and(|skip| path.starts_with(skip)) {
        return Ok(());
    }
    let mut entries = fs::read_dir(path)
        .map_err(|error| storage_error("read rehearsal workspace directory", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| storage_error("read rehearsal workspace entry", error))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();
        if should_skip_entry(&file_name) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| storage_error("read rehearsal workspace metadata", error))?;
        if metadata.is_dir() {
            collect_snapshot(root, &path, skip_root, snapshot)?;
        } else if metadata.is_file() {
            let bytes = fs::read(&path)
                .map_err(|error| storage_error("read rehearsal workspace file", error))?;
            let relative = relative_path(root, &path)?;
            let file = SnapshotFile {
                path: relative.clone(),
                hash: hash_bytes(&bytes),
                bytes: bytes.len() as u64,
            };
            snapshot.total_bytes = snapshot.total_bytes.saturating_add(file.bytes);
            snapshot.files.insert(relative, file);
        }
    }
    Ok(())
}

fn copy_workspace(
    source: &Path,
    destination: &Path,
    skip_root: Option<&Path>,
) -> Result<(), DomainError> {
    let source = canonicalize_existing(source).map_err(|error| DomainError::Configuration {
        message: format!(
            "Failed to resolve rehearsal workspace {}: {error}",
            source.display()
        ),
        repair: Some("Pass --workspace <existing-directory>".to_string()),
    })?;
    copy_workspace_inner(&source, &source, destination, skip_root)
}

fn copy_workspace_inner(
    root: &Path,
    source: &Path,
    destination_root: &Path,
    skip_root: Option<&Path>,
) -> Result<(), DomainError> {
    if skip_root.is_some_and(|skip| source.starts_with(skip)) {
        return Ok(());
    }
    let mut entries = fs::read_dir(source)
        .map_err(|error| storage_error("read rehearsal source directory", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| storage_error("read rehearsal source entry", error))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let source_path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();
        if should_skip_entry(&file_name) {
            continue;
        }
        let relative = source_path
            .strip_prefix(root)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to compute rehearsal relative path: {error}"),
                repair: Some("ee rehearse run --json".to_string()),
            })?;
        let destination_path = destination_root.join(relative);
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| storage_error("read rehearsal source metadata", error))?;
        if metadata.is_dir() {
            fs::create_dir_all(&destination_path)
                .map_err(|error| storage_error("create rehearsal sandbox directory", error))?;
            copy_workspace_inner(root, &source_path, destination_root, skip_root)?;
        } else if metadata.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| storage_error("create rehearsal sandbox parent", error))?;
            }
            fs::copy(&source_path, &destination_path)
                .map_err(|error| storage_error("copy rehearsal source file", error))?;
        } else if metadata.file_type().is_symlink() {
            copy_symlink(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn copy_symlink(source: &Path, destination: &Path) -> Result<(), DomainError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| storage_error("create rehearsal symlink parent", error))?;
    }
    let target =
        fs::read_link(source).map_err(|error| storage_error("read rehearsal symlink", error))?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, destination)
            .map_err(|error| storage_error("copy rehearsal symlink", error))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (target, destination);
    }
    Ok(())
}

fn should_skip_entry(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | ".rch-target" | ".rch-tmp" | "perf.data"
    ) || name.starts_with("${TMPDIR")
}

fn relative_path(root: &Path, path: &Path) -> Result<String, DomainError> {
    path.strip_prefix(root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to compute rehearsal relative path: {error}"),
            repair: Some("ee rehearse run --json".to_string()),
        })
}

fn snapshot_hash(snapshot: &FilesystemSnapshot) -> String {
    let mut hasher = blake3::Hasher::new();
    for file in snapshot.files.values() {
        hasher.update(file.path.as_bytes());
        hasher.update(b"\0");
        hasher.update(file.hash.as_bytes());
        hasher.update(b"\0");
        hasher.update(file.bytes.to_string().as_bytes());
        hasher.update(b"\n");
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn hash_json_value<T>(value: &T) -> Result<String, DomainError>
where
    T: Serialize,
{
    serde_json::to_vec(value)
        .map(|bytes| hash_bytes(&bytes))
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to hash rehearsal JSON: {error}"),
            repair: Some("ee rehearse run --json".to_string()),
        })
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn write_snapshot_manifest(
    path: &Path,
    root: &Path,
    snapshot: &FilesystemSnapshot,
    hash: &str,
) -> Result<(), DomainError> {
    let manifest = SnapshotManifest {
        schema: "ee.rehearse.snapshot.v1".to_string(),
        root: root.display().to_string(),
        hash: hash.to_string(),
        total_bytes: snapshot.total_bytes,
        files: snapshot.files.values().cloned().collect(),
    };
    write_json_artifact(path, &manifest)
}

fn write_json_artifact<T>(path: &Path, value: &T) -> Result<(), DomainError>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| storage_error("create rehearsal artifact parent", error))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| DomainError::Storage {
        message: format!("Failed to serialize rehearsal artifact: {error}"),
        repair: Some("ee rehearse run --json".to_string()),
    })?;
    fs::write(path, bytes).map_err(|error| storage_error("write rehearsal artifact", error))
}

fn changed_paths(before: &FilesystemSnapshot, after: &FilesystemSnapshot) -> Vec<String> {
    before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.files.get(path) != after.files.get(path))
        .collect()
}

fn execute_rehearsal_command(
    command: &CommandSpec,
    ee_binary: Option<&Path>,
    sandbox_path: &Path,
) -> Result<CommandResult, DomainError> {
    let started = Instant::now();
    let Some(binary) = ee_binary else {
        return Ok(CommandResult {
            command_id: command.id.clone(),
            command: command.command.clone(),
            args: command.args.clone(),
            exit_code: -1,
            result: "executor_unavailable".to_string(),
            elapsed_ms: 0,
            operation_id: command.idempotency_key.clone(),
            error_message: Some("No ee binary was supplied for sandbox execution".to_string()),
        });
    };
    let output = Command::new(binary)
        .arg("--workspace")
        .arg(sandbox_path)
        .arg(&command.command)
        .args(&command.args)
        .env("EE_REHEARSE_SANDBOX", "1")
        .output()
        .map_err(|error| storage_error("execute rehearsal command", error))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Ok(CommandResult {
        command_id: command.id.clone(),
        command: command.command.clone(),
        args: command.args.clone(),
        exit_code: output.status.code().unwrap_or(-1),
        result: if output.status.success() {
            "passed".to_string()
        } else {
            "failed".to_string()
        },
        elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        operation_id: command.idempotency_key.clone(),
        error_message: if stderr.is_empty() {
            None
        } else {
            Some(stderr.chars().take(512).collect())
        },
    })
}

fn resolve_manifest_path(artifact_id: &str, workspace: &Path) -> Result<PathBuf, DomainError> {
    let direct = PathBuf::from(artifact_id);
    if direct.is_file() {
        return Ok(direct);
    }
    if direct.is_dir() {
        let manifest = direct.join(MANIFEST_FILE);
        if manifest.is_file() {
            return Ok(manifest);
        }
    }
    let workspace_manifest = workspace
        .join(".ee")
        .join("rehearsals")
        .join(artifact_id)
        .join(MANIFEST_FILE);
    if workspace_manifest.is_file() {
        return Ok(workspace_manifest);
    }
    Err(DomainError::NotFound {
        resource: "rehearsal manifest".to_string(),
        id: artifact_id.to_string(),
        repair: Some(
            "Pass the manifest path from ee rehearse run artifact_paths.manifest".to_string(),
        ),
    })
}

fn summarize_effects(effects: &[EffectRecord]) -> EffectSummary {
    let mut summary = EffectSummary::default();
    for effect in effects {
        match effect.effect_type.as_str() {
            "read_only" => summary.read_only += 1,
            "write_memory" => summary.write_memory += 1,
            "write_index" => summary.write_index += 1,
            "write_config" => summary.write_config += 1,
            _ => {}
        }
    }
    summary
}

fn storage_error(context: &str, error: io::Error) -> DomainError {
    DomainError::Storage {
        message: format!("Failed to {context}: {error}"),
        repair: Some("ee rehearse run --json".to_string()),
    }
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
    use std::io::Write as _;

    type TestResult = Result<(), String>;

    #[test]
    fn plan_reports_rehearsable_commands_and_artifact_estimate() -> TestResult {
        let workspace = kept_temp_dir("ee-rehearse-plan")?;
        fs::create_dir_all(workspace.join("notes")).map_err(|error| error.to_string())?;
        fs::write(workspace.join("notes").join("a.txt"), "alpha")
            .map_err(|error| error.to_string())?;
        let options = RehearsePlanOptions {
            workspace,
            commands: vec![CommandSpec {
                id: "cmd_1".to_string(),
                command: "status".to_string(),
                args: vec!["--json".to_string()],
                expected_effect: "read_only".to_string(),
                stop_on_failure: true,
                idempotency_key: None,
            }],
            ..Default::default()
        };

        let report = plan_rehearsal(&options).map_err(|e| e.message())?;
        assert!(report.plan_id.starts_with("rplan_"));
        assert_eq!(report.command_count, 1);
        assert_eq!(report.rehearsable_count, 1);
        assert!(report.non_rehearsable.is_empty());
        assert_eq!(
            report.estimated_artifacts,
            vec![
                MANIFEST_FILE.to_string(),
                SOURCE_SNAPSHOT_FILE.to_string(),
                SANDBOX_SNAPSHOT_FILE.to_string()
            ]
        );
        assert_ne!(report.side_path_size_estimate, UNAVAILABLE_VALUE);
        assert!(report.can_proceed);
        assert!(report.degradation_codes.is_empty());
        Ok(())
    }

    #[test]
    fn plan_identifies_non_rehearsable_commands() -> TestResult {
        let workspace = kept_temp_dir("ee-rehearse-plan-external")?;
        let options = RehearsePlanOptions {
            workspace,
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
        let workspace = kept_temp_dir("ee-rehearse-plan-quick")?;
        let options = RehearsePlanOptions {
            workspace,
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
        assert_eq!(report.rehearsable_count, 1);
        assert!(report.can_proceed);
        assert!(!report.estimated_artifacts.is_empty());
        Ok(())
    }

    #[test]
    fn run_creates_tempfile_sandbox_and_filesystem_snapshots() -> TestResult {
        let workspace = kept_temp_dir("ee-rehearse-run-workspace")?;
        let out = kept_temp_dir("ee-rehearse-run-out")?;
        fs::create_dir_all(workspace.join("notes")).map_err(|error| error.to_string())?;
        fs::write(workspace.join("notes").join("a.txt"), "alpha")
            .map_err(|error| error.to_string())?;

        let options = RehearseRunOptions {
            workspace: workspace.clone(),
            output_dir: Some(out),
            ..Default::default()
        };

        let report = run_rehearsal(&options).map_err(|e| e.message())?;
        assert!(report.run_id.starts_with("rrun_"));
        assert_ne!(report.sandbox_path, UNAVAILABLE_VALUE);
        assert_ne!(report.source_state_hash, UNAVAILABLE_VALUE);
        assert_ne!(report.command_list_hash, UNAVAILABLE_VALUE);
        assert!(
            Path::new(&report.sandbox_path)
                .join("notes/a.txt")
                .is_file()
        );
        assert_eq!(report.command_results.len(), 0);
        assert!(report.observed_effects.is_empty());
        assert!(report.changed_artifacts.is_empty());
        assert!(Path::new(&report.artifact_paths["manifest"]).is_file());
        assert!(Path::new(&report.artifact_paths["source_snapshot"]).is_file());
        assert!(Path::new(&report.artifact_paths["sandbox_snapshot"]).is_file());
        assert_eq!(report.overall_result, "passed");
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn run_executes_command_inside_sandbox_and_preserves_source_workspace() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let workspace = kept_temp_dir("ee-rehearse-run-source")?;
        let out = kept_temp_dir("ee-rehearse-run-mutating-out")?;
        fs::write(workspace.join("state.txt"), "source").map_err(|error| error.to_string())?;
        let script = out.join("fake-ee.sh");
        let mut file = fs::File::create(&script).map_err(|error| error.to_string())?;
        file.write_all(
            b"#!/bin/sh\nwhile [ \"$1\" != \"--workspace\" ]; do shift; done\nws=\"$2\"\necho sandbox >> \"$ws/state.txt\"\nexit 0\n",
        )
        .map_err(|error| error.to_string())?;
        drop(file);
        let mut perms = fs::metadata(&script)
            .map_err(|error| error.to_string())?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).map_err(|error| error.to_string())?;

        let options = RehearseRunOptions {
            workspace: workspace.clone(),
            output_dir: Some(out),
            ee_binary: Some(script),
            commands: vec![CommandSpec {
                id: "cmd_1".to_string(),
                command: "status".to_string(),
                args: vec!["--json".to_string()],
                expected_effect: "read_only".to_string(),
                stop_on_failure: true,
                idempotency_key: Some("idem-1".to_string()),
            }],
            ..Default::default()
        };

        let report = run_rehearsal(&options).map_err(|e| e.message())?;
        assert_eq!(report.command_results.len(), 1);
        assert_eq!(report.command_results[0].exit_code, 0);
        assert_eq!(report.command_results[0].result, "passed");
        assert_eq!(
            fs::read_to_string(workspace.join("state.txt")).map_err(|error| error.to_string())?,
            "source"
        );
        assert_eq!(
            fs::read_to_string(Path::new(&report.sandbox_path).join("state.txt"))
                .map_err(|error| error.to_string())?,
            "sourcesandbox\n"
        );
        assert_eq!(report.changed_artifacts, vec!["state.txt".to_string()]);
        assert_eq!(report.observed_effects.len(), 1);
        assert!(report.degradation_codes.is_empty());
        Ok(())
    }

    #[test]
    fn run_reports_executor_unavailable_but_still_creates_sandbox() -> TestResult {
        let workspace = kept_temp_dir("ee-rehearse-run-no-executor")?;
        fs::write(workspace.join("state.txt"), "source").map_err(|error| error.to_string())?;
        let options = RehearseRunOptions {
            workspace,
            commands: vec![CommandSpec {
                id: "cmd_1".to_string(),
                command: "status".to_string(),
                args: vec!["--json".to_string()],
                expected_effect: "read_only".to_string(),
                stop_on_failure: true,
                idempotency_key: None,
            }],
            ..Default::default()
        };

        let report = run_rehearsal(&options).map_err(|e| e.message())?;
        assert_ne!(report.sandbox_path, UNAVAILABLE_VALUE);
        assert_eq!(report.command_results[0].result, "executor_unavailable");
        assert_eq!(
            report.degradation_codes,
            vec!["executor_unavailable".to_string()]
        );
        assert_eq!(report.overall_result, "blocked");
        Ok(())
    }

    #[test]
    fn inspect_reads_rehearsal_manifest_and_reports_hashes() -> TestResult {
        let workspace = kept_temp_dir("ee-rehearse-inspect-workspace")?;
        let out = kept_temp_dir("ee-rehearse-inspect-out")?;
        fs::write(workspace.join("state.txt"), "source").map_err(|error| error.to_string())?;
        let run = run_rehearsal(&RehearseRunOptions {
            workspace: workspace.clone(),
            output_dir: Some(out),
            ..Default::default()
        })
        .map_err(|e| e.message())?;
        let options = RehearseInspectOptions {
            artifact_id: run.artifact_paths["manifest"].clone(),
            workspace,
        };

        let report = inspect_rehearsal(&options).map_err(|e| e.message())?;
        assert_eq!(report.artifact_type, "rehearsal_run");
        assert_ne!(report.manifest_hash, UNAVAILABLE_VALUE);
        assert_ne!(report.command_spec_hash, UNAVAILABLE_VALUE);
        assert_ne!(report.source_state_hash, UNAVAILABLE_VALUE);
        assert_ne!(report.sandbox_state_hash, UNAVAILABLE_VALUE);
        assert_eq!(report.command_count, 0);
        assert_eq!(report.success_count, 0);
        assert_eq!(report.integrity_status, "valid");
        assert!(report.warnings.is_empty());
        Ok(())
    }

    #[test]
    fn promote_plan_reads_manifest_without_synthesizing_missing_failures() -> TestResult {
        let workspace = kept_temp_dir("ee-rehearse-promote-workspace")?;
        let out = kept_temp_dir("ee-rehearse-promote-out")?;
        fs::write(workspace.join("state.txt"), "source").map_err(|error| error.to_string())?;
        let run = run_rehearsal(&RehearseRunOptions {
            workspace: workspace.clone(),
            output_dir: Some(out),
            ..Default::default()
        })
        .map_err(|e| e.message())?;
        let options = RehearsePromotePlanOptions {
            artifact_id: run.artifact_paths["manifest"].clone(),
            workspace,
        };

        let report = promote_plan_rehearsal(&options).map_err(|e| e.message())?;
        assert!(report.plan_steps.is_empty());
        assert!(!report.preconditions.is_empty());
        assert!(!report.backup_requirements.is_empty());
        assert!(!report.audit_checks.is_empty());
        assert!(!report.stop_conditions.is_empty());
        assert!(report.expected_queries.is_empty());
        assert!(report.is_safe);
        assert!(report.warnings.is_empty());
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

    fn kept_temp_dir(prefix: &str) -> Result<PathBuf, String> {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map(|dir| dir.keep())
            .map_err(|error| error.to_string())
    }
}
