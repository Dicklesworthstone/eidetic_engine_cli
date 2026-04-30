//! Preflight risk assessment operations (EE-391).
//!
//! Assess task-specific risks before work starts so memory changes
//! agent behavior at the moment of risk rather than only after a mistake.
//!
//! # Operations
//!
//! - **run**: Execute a preflight risk assessment for a task
//! - **show**: Display details of a preflight run
//! - **close**: Mark a preflight run as completed

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::models::preflight::{
    PREFLIGHT_RUN_ID_PREFIX, RISK_BRIEF_ID_PREFIX, PreflightRun, PreflightStatus, RiskBrief,
    RiskItem, RiskLevel,
};
use crate::models::DomainError;

/// Schema for preflight reports.
pub const PREFLIGHT_REPORT_SCHEMA_V1: &str = "ee.preflight.report.v1";

/// Options for running a preflight assessment.
#[derive(Clone, Debug)]
pub struct RunOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Task input/prompt to assess.
    pub task_input: String,
    /// Check for similar past failures.
    pub check_history: bool,
    /// Check for related tripwires.
    pub check_tripwires: bool,
    /// Maximum risk level to auto-clear.
    pub auto_clear_threshold: Option<RiskLevel>,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            task_input: String::new(),
            check_history: true,
            check_tripwires: true,
            auto_clear_threshold: Some(RiskLevel::Medium),
            dry_run: false,
        }
    }
}

/// Options for showing a preflight run.
#[derive(Clone, Debug)]
pub struct ShowOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Preflight run ID to show.
    pub run_id: String,
    /// Include risk brief details.
    pub include_brief: bool,
    /// Include tripwire details.
    pub include_tripwires: bool,
}

impl Default for ShowOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            run_id: String::new(),
            include_brief: true,
            include_tripwires: true,
        }
    }
}

/// Options for closing a preflight run.
#[derive(Clone, Debug)]
pub struct CloseOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Preflight run ID to close.
    pub run_id: String,
    /// Close as cleared for execution.
    pub cleared: bool,
    /// Reason for closing (especially if blocked).
    pub reason: Option<String>,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for CloseOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            run_id: String::new(),
            cleared: false,
            reason: None,
            dry_run: false,
        }
    }
}

/// Report from running a preflight assessment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunReport {
    pub schema: String,
    pub run_id: String,
    pub task_input: String,
    pub status: String,
    pub risk_level: String,
    pub cleared: bool,
    pub block_reason: Option<String>,
    pub risk_brief_id: Option<String>,
    pub risks_identified: usize,
    pub tripwires_set: usize,
    pub dry_run: bool,
    pub started_at: String,
    pub completed_at: Option<String>,
}

impl RunReport {
    #[must_use]
    pub fn new(run_id: String, task_input: String) -> Self {
        Self {
            schema: PREFLIGHT_REPORT_SCHEMA_V1.to_owned(),
            run_id,
            task_input,
            status: PreflightStatus::Running.as_str().to_owned(),
            risk_level: RiskLevel::Unknown.as_str().to_owned(),
            cleared: false,
            block_reason: None,
            risk_brief_id: None,
            risks_identified: 0,
            tripwires_set: 0,
            dry_run: false,
            started_at: Utc::now().to_rfc3339(),
            completed_at: None,
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Report from showing a preflight run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShowReport {
    pub schema: String,
    pub run: PreflightRunView,
    pub brief: Option<RiskBriefView>,
    pub tripwires: Vec<TripwireView>,
}

impl ShowReport {
    #[must_use]
    pub fn new(run: PreflightRunView) -> Self {
        Self {
            schema: PREFLIGHT_REPORT_SCHEMA_V1.to_owned(),
            run,
            brief: None,
            tripwires: Vec::new(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// View of a preflight run for display.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreflightRunView {
    pub id: String,
    pub task_input: String,
    pub status: String,
    pub risk_level: String,
    pub cleared: bool,
    pub block_reason: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<u64>,
}

impl From<&PreflightRun> for PreflightRunView {
    fn from(run: &PreflightRun) -> Self {
        Self {
            id: run.id.clone(),
            task_input: run.task_input.clone(),
            status: run.status.as_str().to_owned(),
            risk_level: run.risk_level.as_str().to_owned(),
            cleared: run.cleared,
            block_reason: run.block_reason.clone(),
            started_at: run.started_at.clone(),
            completed_at: run.completed_at.clone(),
            duration_ms: run.duration_ms,
        }
    }
}

/// View of a risk brief for display.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskBriefView {
    pub id: String,
    pub risk_level: String,
    pub summary: Option<String>,
    pub risks: Vec<RiskItemView>,
    pub recommendations: Vec<String>,
}

impl From<&RiskBrief> for RiskBriefView {
    fn from(brief: &RiskBrief) -> Self {
        Self {
            id: brief.id.clone(),
            risk_level: brief.risk_level.as_str().to_owned(),
            summary: brief.summary.clone(),
            risks: brief.risks.iter().map(RiskItemView::from).collect(),
            recommendations: brief.recommendations.clone(),
        }
    }
}

/// View of a risk item for display.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskItemView {
    pub category: String,
    pub level: String,
    pub description: String,
    pub mitigation: Option<String>,
}

impl From<&RiskItem> for RiskItemView {
    fn from(item: &RiskItem) -> Self {
        Self {
            category: item.category.as_str().to_owned(),
            level: item.level.as_str().to_owned(),
            description: item.description.clone(),
            mitigation: item.mitigation.clone(),
        }
    }
}

/// View of a tripwire for display.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TripwireView {
    pub id: String,
    pub name: String,
    pub status: String,
}

/// Report from closing a preflight run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloseReport {
    pub schema: String,
    pub run_id: String,
    pub previous_status: String,
    pub new_status: String,
    pub cleared: bool,
    pub reason: Option<String>,
    pub dry_run: bool,
    pub closed_at: String,
}

impl CloseReport {
    #[must_use]
    pub fn new(run_id: String, previous_status: PreflightStatus) -> Self {
        Self {
            schema: PREFLIGHT_REPORT_SCHEMA_V1.to_owned(),
            run_id,
            previous_status: previous_status.as_str().to_owned(),
            new_status: PreflightStatus::Completed.as_str().to_owned(),
            cleared: false,
            reason: None,
            dry_run: false,
            closed_at: Utc::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

fn generate_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Run a preflight risk assessment.
pub fn run_preflight(options: &RunOptions) -> Result<RunReport, DomainError> {
    let run_id = format!("{}{}", PREFLIGHT_RUN_ID_PREFIX, generate_id());
    let mut report = RunReport::new(run_id.clone(), options.task_input.clone());
    report.dry_run = options.dry_run;

    if options.dry_run {
        report.status = PreflightStatus::Completed.as_str().to_owned();
        report.completed_at = Some(Utc::now().to_rfc3339());
        return Ok(report);
    }

    // Assess risk level based on task input patterns
    let risk_level = assess_task_risk(&options.task_input);
    report.risk_level = risk_level.as_str().to_owned();

    // Generate risk brief
    let brief_id = format!("{}{}", RISK_BRIEF_ID_PREFIX, generate_id());
    report.risk_brief_id = Some(brief_id);

    // Determine clearance
    let auto_clear_threshold = options.auto_clear_threshold.unwrap_or(RiskLevel::Medium);
    if risk_level <= auto_clear_threshold {
        report.cleared = true;
    } else {
        report.cleared = false;
        report.block_reason = Some(format!(
            "Risk level {} exceeds auto-clear threshold {}",
            risk_level.as_str(),
            auto_clear_threshold.as_str()
        ));
    }

    report.status = PreflightStatus::Completed.as_str().to_owned();
    report.completed_at = Some(Utc::now().to_rfc3339());

    Ok(report)
}

/// Show details of a preflight run.
pub fn show_preflight(options: &ShowOptions) -> Result<ShowReport, DomainError> {
    // For now, return a stub since we don't have persistence wired
    // In a real implementation, this would query the database

    if !options.run_id.starts_with(PREFLIGHT_RUN_ID_PREFIX) {
        return Err(DomainError::Usage {
            message: format!(
                "Invalid preflight run ID: expected prefix '{}', got '{}'",
                PREFLIGHT_RUN_ID_PREFIX,
                &options.run_id[..options.run_id.len().min(3)]
            ),
            repair: Some("Provide a valid preflight run ID (format: pf_<uuid>)".to_owned()),
        });
    }

    // Create a stub run view
    let run = PreflightRunView {
        id: options.run_id.clone(),
        task_input: "(run not found in storage)".to_owned(),
        status: PreflightStatus::Completed.as_str().to_owned(),
        risk_level: RiskLevel::Unknown.as_str().to_owned(),
        cleared: false,
        block_reason: Some("Storage not yet wired".to_owned()),
        started_at: Utc::now().to_rfc3339(),
        completed_at: Some(Utc::now().to_rfc3339()),
        duration_ms: None,
    };

    Ok(ShowReport::new(run))
}

/// Close a preflight run.
pub fn close_preflight(options: &CloseOptions) -> Result<CloseReport, DomainError> {
    if !options.run_id.starts_with(PREFLIGHT_RUN_ID_PREFIX) {
        return Err(DomainError::Usage {
            message: format!(
                "Invalid preflight run ID: expected prefix '{}', got '{}'",
                PREFLIGHT_RUN_ID_PREFIX,
                &options.run_id[..options.run_id.len().min(3)]
            ),
            repair: Some("Provide a valid preflight run ID (format: pf_<uuid>)".to_owned()),
        });
    }

    let mut report = CloseReport::new(options.run_id.clone(), PreflightStatus::Running);
    report.cleared = options.cleared;
    report.reason = options.reason.clone();
    report.dry_run = options.dry_run;

    if options.cleared {
        report.new_status = PreflightStatus::Completed.as_str().to_owned();
    } else {
        report.new_status = PreflightStatus::Cancelled.as_str().to_owned();
    }

    Ok(report)
}

/// Assess risk level from task input text.
fn assess_task_risk(task_input: &str) -> RiskLevel {
    let lower = task_input.to_lowercase();

    // Critical risk patterns
    if lower.contains("delete")
        || lower.contains("rm -rf")
        || lower.contains("drop table")
        || lower.contains("truncate")
    {
        return RiskLevel::Critical;
    }

    // High risk patterns
    if lower.contains("production")
        || lower.contains("deploy")
        || lower.contains("migrate")
        || lower.contains("force")
    {
        return RiskLevel::High;
    }

    // Medium risk patterns
    if lower.contains("update")
        || lower.contains("modify")
        || lower.contains("change")
        || lower.contains("refactor")
    {
        return RiskLevel::Medium;
    }

    // Low risk patterns
    if lower.contains("read")
        || lower.contains("list")
        || lower.contains("show")
        || lower.contains("search")
    {
        return RiskLevel::Low;
    }

    RiskLevel::None
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
    fn run_dry_run_completes_immediately() -> TestResult {
        let options = RunOptions {
            task_input: "test task".to_owned(),
            dry_run: true,
            ..Default::default()
        };

        let report = run_preflight(&options).map_err(|e| e.message())?;
        ensure(report.dry_run, true, "dry_run")?;
        ensure(
            report.status,
            PreflightStatus::Completed.as_str().to_owned(),
            "status",
        )?;
        ensure(report.run_id.starts_with(PREFLIGHT_RUN_ID_PREFIX), true, "run_id prefix")
    }

    #[test]
    fn run_assesses_critical_risk() -> TestResult {
        let options = RunOptions {
            task_input: "delete all production data".to_owned(),
            dry_run: false,
            ..Default::default()
        };

        let report = run_preflight(&options).map_err(|e| e.message())?;
        ensure(
            report.risk_level,
            RiskLevel::Critical.as_str().to_owned(),
            "risk_level",
        )?;
        ensure(report.cleared, false, "should not be cleared")
    }

    #[test]
    fn run_assesses_low_risk() -> TestResult {
        let options = RunOptions {
            task_input: "list all files".to_owned(),
            dry_run: false,
            ..Default::default()
        };

        let report = run_preflight(&options).map_err(|e| e.message())?;
        ensure(
            report.risk_level,
            RiskLevel::Low.as_str().to_owned(),
            "risk_level",
        )?;
        ensure(report.cleared, true, "should be cleared")
    }

    #[test]
    fn show_rejects_invalid_run_id() -> TestResult {
        let options = ShowOptions {
            run_id: "invalid_id".to_owned(),
            ..Default::default()
        };

        let result = show_preflight(&options);
        ensure(result.is_err(), true, "should reject invalid ID")
    }

    #[test]
    fn close_sets_status_based_on_cleared() -> TestResult {
        let options = CloseOptions {
            run_id: format!("{}test", PREFLIGHT_RUN_ID_PREFIX),
            cleared: true,
            ..Default::default()
        };

        let report = close_preflight(&options).map_err(|e| e.message())?;
        ensure(
            report.new_status,
            PreflightStatus::Completed.as_str().to_owned(),
            "cleared status",
        )?;

        let options_blocked = CloseOptions {
            run_id: format!("{}test", PREFLIGHT_RUN_ID_PREFIX),
            cleared: false,
            reason: Some("Task rejected".to_owned()),
            ..Default::default()
        };

        let report_blocked = close_preflight(&options_blocked).map_err(|e| e.message())?;
        ensure(
            report_blocked.new_status,
            PreflightStatus::Cancelled.as_str().to_owned(),
            "blocked status",
        )
    }

    #[test]
    fn report_serializes_to_json() -> TestResult {
        let report = RunReport::new("pf_test".to_owned(), "test task".to_owned());
        let json = report.to_json();
        ensure(json.contains("pf_test"), true, "json contains run_id")?;
        ensure(json.contains(PREFLIGHT_REPORT_SCHEMA_V1), true, "json contains schema")
    }

    #[test]
    fn assess_task_risk_patterns() -> TestResult {
        ensure(
            assess_task_risk("rm -rf /"),
            RiskLevel::Critical,
            "rm -rf",
        )?;
        ensure(
            assess_task_risk("deploy to production"),
            RiskLevel::High,
            "production deploy",
        )?;
        ensure(
            assess_task_risk("refactor the module"),
            RiskLevel::Medium,
            "refactor",
        )?;
        ensure(assess_task_risk("search for files"), RiskLevel::Low, "search")?;
        ensure(assess_task_risk("hello world"), RiskLevel::None, "no pattern")
    }
}
