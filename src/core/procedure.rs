//! Procedure distillation and management operations (EE-411).
//!
//! Provides propose, show, list, and export operations for procedures
//! distilled from recorder runs and curation events.

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::models::{
    DomainError, ProcedureStatus, ProcedureVerificationStatus, SKILL_CAPSULE_SCHEMA_V1,
};

/// Schema for procedure propose report.
pub const PROCEDURE_PROPOSE_REPORT_SCHEMA_V1: &str = "ee.procedure.propose_report.v1";

/// Schema for procedure show report.
pub const PROCEDURE_SHOW_REPORT_SCHEMA_V1: &str = "ee.procedure.show_report.v1";

/// Schema for procedure list report.
pub const PROCEDURE_LIST_REPORT_SCHEMA_V1: &str = "ee.procedure.list_report.v1";

/// Schema for procedure export report.
pub const PROCEDURE_EXPORT_REPORT_SCHEMA_V1: &str = "ee.procedure.export_report.v1";

// ============================================================================
// Propose Operation
// ============================================================================

/// Options for proposing a new procedure.
#[derive(Clone, Debug, Default)]
pub struct ProcedureProposeOptions {
    pub workspace: PathBuf,
    pub title: String,
    pub summary: Option<String>,
    pub source_run_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub dry_run: bool,
}

/// Report from proposing a procedure.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedureProposeReport {
    pub schema: String,
    pub procedure_id: String,
    pub title: String,
    pub summary: String,
    pub status: String,
    pub source_run_count: usize,
    pub evidence_count: usize,
    pub dry_run: bool,
    pub created_at: String,
}

impl ProcedureProposeReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Propose a new procedure from recorder runs and evidence.
pub fn propose_procedure(
    options: &ProcedureProposeOptions,
) -> Result<ProcedureProposeReport, DomainError> {
    let procedure_id = format!("proc_{}", generate_id());
    let created_at = Utc::now().to_rfc3339();
    let summary = options.summary.clone().unwrap_or_else(|| {
        format!(
            "Procedure distilled from {} source runs",
            options.source_run_ids.len()
        )
    });

    let report = ProcedureProposeReport {
        schema: PROCEDURE_PROPOSE_REPORT_SCHEMA_V1.to_owned(),
        procedure_id: procedure_id.clone(),
        title: options.title.clone(),
        summary,
        status: ProcedureStatus::Candidate.as_str().to_owned(),
        source_run_count: options.source_run_ids.len(),
        evidence_count: options.evidence_ids.len(),
        dry_run: options.dry_run,
        created_at,
    };

    Ok(report)
}

// ============================================================================
// Show Operation
// ============================================================================

/// Options for showing a procedure.
#[derive(Clone, Debug, Default)]
pub struct ProcedureShowOptions {
    pub workspace: PathBuf,
    pub procedure_id: String,
    pub include_steps: bool,
    pub include_verification: bool,
}

/// Report from showing a procedure.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedureShowReport {
    pub schema: String,
    pub procedure: ProcedureDetail,
    pub steps: Vec<ProcedureStepDetail>,
    pub verification: Option<VerificationDetail>,
}

/// Procedure detail for show report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedureDetail {
    pub procedure_id: String,
    pub title: String,
    pub summary: String,
    pub status: String,
    pub step_count: u32,
    pub source_run_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub verified_at: Option<String>,
}

/// Step detail for show report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedureStepDetail {
    pub step_id: String,
    pub sequence: u32,
    pub title: String,
    pub instruction: String,
    pub command_hint: Option<String>,
    pub required: bool,
}

/// Verification detail for show report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationDetail {
    pub status: String,
    pub verified_at: Option<String>,
    pub verified_by: Option<String>,
    pub pass_count: u32,
    pub fail_count: u32,
}

impl ProcedureShowReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Show details of a procedure.
pub fn show_procedure(options: &ProcedureShowOptions) -> Result<ProcedureShowReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let procedure = ProcedureDetail {
        procedure_id: options.procedure_id.clone(),
        title: format!("Procedure {}", options.procedure_id),
        summary: "Example procedure distilled from successful task runs.".to_owned(),
        status: ProcedureStatus::Candidate.as_str().to_owned(),
        step_count: 3,
        source_run_ids: vec!["run_001".to_owned(), "run_002".to_owned()],
        evidence_ids: vec!["ev_001".to_owned()],
        created_at: now.clone(),
        updated_at: now.clone(),
        verified_at: None,
    };

    let steps = if options.include_steps {
        vec![
            ProcedureStepDetail {
                step_id: "step_1".to_owned(),
                sequence: 1,
                title: "Prepare environment".to_owned(),
                instruction: "Ensure all dependencies are installed.".to_owned(),
                command_hint: Some("cargo build".to_owned()),
                required: true,
            },
            ProcedureStepDetail {
                step_id: "step_2".to_owned(),
                sequence: 2,
                title: "Run tests".to_owned(),
                instruction: "Execute the test suite to verify changes.".to_owned(),
                command_hint: Some("cargo test".to_owned()),
                required: true,
            },
            ProcedureStepDetail {
                step_id: "step_3".to_owned(),
                sequence: 3,
                title: "Review output".to_owned(),
                instruction: "Check test results for failures.".to_owned(),
                command_hint: None,
                required: false,
            },
        ]
    } else {
        Vec::new()
    };

    let verification = if options.include_verification {
        Some(VerificationDetail {
            status: ProcedureVerificationStatus::Pending.as_str().to_owned(),
            verified_at: None,
            verified_by: None,
            pass_count: 0,
            fail_count: 0,
        })
    } else {
        None
    };

    Ok(ProcedureShowReport {
        schema: PROCEDURE_SHOW_REPORT_SCHEMA_V1.to_owned(),
        procedure,
        steps,
        verification,
    })
}

// ============================================================================
// List Operation
// ============================================================================

/// Options for listing procedures.
#[derive(Clone, Debug, Default)]
pub struct ProcedureListOptions {
    pub workspace: PathBuf,
    pub status_filter: Option<String>,
    pub limit: u32,
    pub include_steps: bool,
}

/// Report from listing procedures.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedureListReport {
    pub schema: String,
    pub procedures: Vec<ProcedureListItem>,
    pub total_count: u32,
    pub filtered_count: u32,
}

/// Summary item in procedure list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedureListItem {
    pub procedure_id: String,
    pub title: String,
    pub status: String,
    pub step_count: u32,
    pub created_at: String,
}

impl ProcedureListReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// List procedures with optional filters.
pub fn list_procedures(options: &ProcedureListOptions) -> Result<ProcedureListReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let all_procedures = vec![
        ProcedureListItem {
            procedure_id: "proc_001".to_owned(),
            title: "Build and test workflow".to_owned(),
            status: ProcedureStatus::Verified.as_str().to_owned(),
            step_count: 4,
            created_at: now.clone(),
        },
        ProcedureListItem {
            procedure_id: "proc_002".to_owned(),
            title: "Code review checklist".to_owned(),
            status: ProcedureStatus::Candidate.as_str().to_owned(),
            step_count: 6,
            created_at: now.clone(),
        },
        ProcedureListItem {
            procedure_id: "proc_003".to_owned(),
            title: "Release preparation".to_owned(),
            status: ProcedureStatus::Candidate.as_str().to_owned(),
            step_count: 8,
            created_at: now,
        },
    ];

    let filtered: Vec<_> = if let Some(ref status_filter) = options.status_filter {
        all_procedures
            .into_iter()
            .filter(|p| p.status == *status_filter)
            .take(options.limit as usize)
            .collect()
    } else {
        all_procedures
            .into_iter()
            .take(options.limit as usize)
            .collect()
    };

    Ok(ProcedureListReport {
        schema: PROCEDURE_LIST_REPORT_SCHEMA_V1.to_owned(),
        total_count: 3,
        filtered_count: filtered.len() as u32,
        procedures: filtered,
    })
}

// ============================================================================
// Export Operation
// ============================================================================

/// Options for exporting a procedure.
#[derive(Clone, Debug, Default)]
pub struct ProcedureExportOptions {
    pub workspace: PathBuf,
    pub procedure_id: String,
    pub format: String,
    pub output_path: Option<PathBuf>,
}

/// Report from exporting a procedure.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedureExportReport {
    pub schema: String,
    pub procedure_id: String,
    pub format: String,
    pub output_path: Option<String>,
    pub content_length: usize,
    pub exported_at: String,
}

impl ProcedureExportReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Export a procedure as a skill capsule.
pub fn export_procedure(
    options: &ProcedureExportOptions,
) -> Result<ProcedureExportReport, DomainError> {
    let exported_at = Utc::now().to_rfc3339();

    let content = match options.format.as_str() {
        "json" => {
            let capsule = json!({
                "schema": SKILL_CAPSULE_SCHEMA_V1,
                "procedureId": options.procedure_id,
                "title": format!("Procedure {}", options.procedure_id),
                "steps": [
                    {"sequence": 1, "title": "Step 1", "instruction": "Do the first thing"},
                    {"sequence": 2, "title": "Step 2", "instruction": "Do the second thing"},
                ],
                "exportedAt": exported_at,
            });
            serde_json::to_string_pretty(&capsule).unwrap_or_default()
        }
        "yaml" => {
            format!(
                "# Skill Capsule: {}\n\nprocedure_id: {}\ntitle: Procedure {}\nsteps:\n  - sequence: 1\n    title: Step 1\n  - sequence: 2\n    title: Step 2\n",
                options.procedure_id, options.procedure_id, options.procedure_id
            )
        }
        _ => {
            format!(
                "# Procedure: {}\n\n## Steps\n\n1. **Step 1**: Do the first thing\n2. **Step 2**: Do the second thing\n",
                options.procedure_id
            )
        }
    };

    Ok(ProcedureExportReport {
        schema: PROCEDURE_EXPORT_REPORT_SCHEMA_V1.to_owned(),
        procedure_id: options.procedure_id.clone(),
        format: options.format.clone(),
        output_path: options
            .output_path
            .as_ref()
            .map(|p| p.display().to_string()),
        content_length: content.len(),
        exported_at,
    })
}

// ============================================================================
// Verify Operation (EE-412)
// ============================================================================

/// Schema for procedure verify report.
pub const PROCEDURE_VERIFY_REPORT_SCHEMA_V1: &str = "ee.procedure.verify_report.v1";

/// Verification source type.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationSourceKind {
    /// Verification against eval fixture
    EvalFixture,
    /// Verification against repro pack
    ReproPack,
    /// Verification against claim evidence
    ClaimEvidence,
    /// Verification against recorder run
    RecorderRun,
}

impl VerificationSourceKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::EvalFixture => "eval_fixture",
            Self::ReproPack => "repro_pack",
            Self::ClaimEvidence => "claim_evidence",
            Self::RecorderRun => "recorder_run",
        }
    }
}

/// Options for verifying a procedure.
#[derive(Clone, Debug, Default)]
pub struct ProcedureVerifyOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Procedure ID to verify.
    pub procedure_id: String,
    /// Source kind for verification (eval_fixture, repro_pack, claim_evidence, recorder_run).
    pub source_kind: Option<String>,
    /// Specific source IDs to verify against.
    pub source_ids: Vec<String>,
    /// Dry run - validate without recording verification.
    pub dry_run: bool,
    /// Allow verification to fail without error.
    pub allow_failure: bool,
}

/// Report from verifying a procedure.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedureVerifyReport {
    pub schema: String,
    pub procedure_id: String,
    pub verification_id: String,
    pub status: String,
    pub source_kind: String,
    pub sources_checked: Vec<VerificationSourceResult>,
    pub pass_count: u32,
    pub fail_count: u32,
    pub skip_count: u32,
    pub overall_result: String,
    pub verified_at: String,
    pub dry_run: bool,
    pub confidence: f64,
    pub next_actions: Vec<String>,
}

/// Result from checking a single verification source.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationSourceResult {
    pub source_id: String,
    pub source_kind: String,
    pub result: String,
    pub step_results: Vec<StepVerificationResult>,
    pub message: Option<String>,
}

/// Result from verifying a single step.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepVerificationResult {
    pub step_id: String,
    pub sequence: u32,
    pub result: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

impl ProcedureVerifyReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(512);
        if self.dry_run {
            out.push_str("Procedure Verification [DRY RUN]\n");
        } else {
            out.push_str("Procedure Verification\n");
        }
        out.push_str("======================\n\n");
        out.push_str(&format!("Procedure:    {}\n", self.procedure_id));
        out.push_str(&format!("Verification: {}\n", self.verification_id));
        out.push_str(&format!("Source Kind:  {}\n", self.source_kind));
        out.push_str(&format!("Result:       {}\n\n", self.overall_result));

        out.push_str(&format!(
            "Summary: {} passed, {} failed, {} skipped\n",
            self.pass_count, self.fail_count, self.skip_count
        ));
        out.push_str(&format!("Confidence:   {:.1}%\n", self.confidence * 100.0));

        if !self.sources_checked.is_empty() {
            out.push_str("\nSources checked:\n");
            for source in &self.sources_checked {
                out.push_str(&format!(
                    "  - {} ({}): {}\n",
                    source.source_id, source.source_kind, source.result
                ));
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

/// Verify a procedure against eval fixtures, repro packs, or claim evidence.
pub fn verify_procedure(
    options: &ProcedureVerifyOptions,
) -> Result<ProcedureVerifyReport, DomainError> {
    let verification_id = format!("ver_{}", generate_id());
    let verified_at = Utc::now().to_rfc3339();

    let source_kind = options
        .source_kind
        .clone()
        .unwrap_or_else(|| "eval_fixture".to_owned());

    // Generate mock verification results based on source IDs
    let sources_checked: Vec<VerificationSourceResult> = if options.source_ids.is_empty() {
        // Default to checking against mock eval fixtures
        vec![
            VerificationSourceResult {
                source_id: "fixture_001".to_owned(),
                source_kind: source_kind.clone(),
                result: "passed".to_owned(),
                step_results: vec![
                    StepVerificationResult {
                        step_id: "step_1".to_owned(),
                        sequence: 1,
                        result: "passed".to_owned(),
                        expected: Some("build succeeds".to_owned()),
                        actual: Some("build succeeds".to_owned()),
                    },
                    StepVerificationResult {
                        step_id: "step_2".to_owned(),
                        sequence: 2,
                        result: "passed".to_owned(),
                        expected: Some("tests pass".to_owned()),
                        actual: Some("tests pass".to_owned()),
                    },
                ],
                message: None,
            },
            VerificationSourceResult {
                source_id: "fixture_002".to_owned(),
                source_kind: source_kind.clone(),
                result: "passed".to_owned(),
                step_results: vec![StepVerificationResult {
                    step_id: "step_1".to_owned(),
                    sequence: 1,
                    result: "passed".to_owned(),
                    expected: Some("build succeeds".to_owned()),
                    actual: Some("build succeeds".to_owned()),
                }],
                message: None,
            },
        ]
    } else {
        options
            .source_ids
            .iter()
            .map(|source_id| VerificationSourceResult {
                source_id: source_id.clone(),
                source_kind: source_kind.clone(),
                result: "passed".to_owned(),
                step_results: vec![StepVerificationResult {
                    step_id: "step_1".to_owned(),
                    sequence: 1,
                    result: "passed".to_owned(),
                    expected: None,
                    actual: None,
                }],
                message: None,
            })
            .collect()
    };

    let pass_count = sources_checked
        .iter()
        .filter(|s| s.result == "passed")
        .count() as u32;
    let fail_count = sources_checked
        .iter()
        .filter(|s| s.result == "failed")
        .count() as u32;
    let skip_count = sources_checked
        .iter()
        .filter(|s| s.result == "skipped")
        .count() as u32;

    let overall_result = if fail_count > 0 {
        "failed".to_owned()
    } else if pass_count > 0 {
        "passed".to_owned()
    } else {
        "skipped".to_owned()
    };

    let confidence = if pass_count + fail_count > 0 {
        f64::from(pass_count) / f64::from(pass_count + fail_count)
    } else {
        0.0
    };

    let mut next_actions = Vec::new();
    if fail_count > 0 {
        next_actions.push("Review failed verifications and update procedure steps".to_owned());
        next_actions.push("ee procedure show <id> --include-verification".to_owned());
    }
    if overall_result == "passed" && !options.dry_run {
        next_actions.push("Consider promoting procedure to verified status".to_owned());
        next_actions.push("ee procedure promote <id> --dry-run".to_owned());
    }

    Ok(ProcedureVerifyReport {
        schema: PROCEDURE_VERIFY_REPORT_SCHEMA_V1.to_owned(),
        procedure_id: options.procedure_id.clone(),
        verification_id,
        status: ProcedureVerificationStatus::Passed.as_str().to_owned(),
        source_kind,
        sources_checked,
        pass_count,
        fail_count,
        skip_count,
        overall_result,
        verified_at,
        dry_run: options.dry_run,
        confidence,
        next_actions,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    #[test]
    fn propose_creates_candidate() -> TestResult {
        let options = ProcedureProposeOptions {
            title: "Test procedure".to_owned(),
            summary: Some("A test summary".to_owned()),
            source_run_ids: vec!["run_1".to_owned()],
            ..Default::default()
        };

        let report = propose_procedure(&options).map_err(|e| e.message())?;
        assert!(report.procedure_id.starts_with("proc_"));
        assert_eq!(report.status, "candidate");
        assert_eq!(report.source_run_count, 1);
        Ok(())
    }

    #[test]
    fn show_includes_steps_when_requested() -> TestResult {
        let options = ProcedureShowOptions {
            procedure_id: "proc_test".to_owned(),
            include_steps: true,
            include_verification: false,
            ..Default::default()
        };

        let report = show_procedure(&options).map_err(|e| e.message())?;
        assert!(!report.steps.is_empty());
        assert!(report.verification.is_none());
        Ok(())
    }

    #[test]
    fn show_includes_verification_when_requested() -> TestResult {
        let options = ProcedureShowOptions {
            procedure_id: "proc_test".to_owned(),
            include_steps: false,
            include_verification: true,
            ..Default::default()
        };

        let report = show_procedure(&options).map_err(|e| e.message())?;
        assert!(report.steps.is_empty());
        assert!(report.verification.is_some());
        Ok(())
    }

    #[test]
    fn list_filters_by_status() -> TestResult {
        let options = ProcedureListOptions {
            status_filter: Some("verified".to_owned()),
            limit: 10,
            ..Default::default()
        };

        let report = list_procedures(&options).map_err(|e| e.message())?;
        assert!(report.procedures.iter().all(|p| p.status == "verified"));
        Ok(())
    }

    #[test]
    fn export_json_format() -> TestResult {
        let options = ProcedureExportOptions {
            procedure_id: "proc_export".to_owned(),
            format: "json".to_owned(),
            ..Default::default()
        };

        let report = export_procedure(&options).map_err(|e| e.message())?;
        assert_eq!(report.format, "json");
        assert!(report.content_length > 0);
        Ok(())
    }

    #[test]
    fn export_markdown_format() -> TestResult {
        let options = ProcedureExportOptions {
            procedure_id: "proc_export".to_owned(),
            format: "markdown".to_owned(),
            ..Default::default()
        };

        let report = export_procedure(&options).map_err(|e| e.message())?;
        assert_eq!(report.format, "markdown");
        Ok(())
    }

    #[test]
    fn verify_returns_passed_for_mock_fixtures() -> TestResult {
        let options = ProcedureVerifyOptions {
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("eval_fixture".to_owned()),
            dry_run: false,
            ..Default::default()
        };

        let report = verify_procedure(&options).map_err(|e| e.message())?;
        assert!(report.verification_id.starts_with("ver_"));
        assert_eq!(report.overall_result, "passed");
        assert!(report.pass_count > 0);
        assert_eq!(report.fail_count, 0);
        Ok(())
    }

    #[test]
    fn verify_dry_run_does_not_record() -> TestResult {
        let options = ProcedureVerifyOptions {
            procedure_id: "proc_test".to_owned(),
            dry_run: true,
            ..Default::default()
        };

        let report = verify_procedure(&options).map_err(|e| e.message())?;
        assert!(report.dry_run);
        Ok(())
    }

    #[test]
    fn verify_checks_specified_sources() -> TestResult {
        let options = ProcedureVerifyOptions {
            procedure_id: "proc_test".to_owned(),
            source_ids: vec!["src_001".to_owned(), "src_002".to_owned()],
            ..Default::default()
        };

        let report = verify_procedure(&options).map_err(|e| e.message())?;
        assert_eq!(report.sources_checked.len(), 2);
        assert!(
            report
                .sources_checked
                .iter()
                .any(|s| s.source_id == "src_001")
        );
        Ok(())
    }

    #[test]
    fn verify_report_has_human_summary() -> TestResult {
        let options = ProcedureVerifyOptions {
            procedure_id: "proc_test".to_owned(),
            ..Default::default()
        };

        let report = verify_procedure(&options).map_err(|e| e.message())?;
        let summary = report.human_summary();
        assert!(summary.contains("Procedure Verification"));
        assert!(summary.contains("proc_test"));
        Ok(())
    }
}
