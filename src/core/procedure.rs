//! Procedure distillation and management operations (EE-411).
//!
//! Provides propose, show, list, and export operations for procedures
//! distilled from recorder runs and curation events.

use std::fs::OpenOptions;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::models::{
    DomainError, ProcedureExportFormat, ProcedureStatus, ProcedureVerificationStatus,
    SKILL_CAPSULE_SCHEMA_V1, SkillCapsuleInstallMode,
};

/// Schema for procedure propose report.
pub const PROCEDURE_PROPOSE_REPORT_SCHEMA_V1: &str = "ee.procedure.propose_report.v1";

/// Schema for procedure show report.
pub const PROCEDURE_SHOW_REPORT_SCHEMA_V1: &str = "ee.procedure.show_report.v1";

/// Schema for procedure list report.
pub const PROCEDURE_LIST_REPORT_SCHEMA_V1: &str = "ee.procedure.list_report.v1";

/// Schema for procedure export report.
pub const PROCEDURE_EXPORT_REPORT_SCHEMA_V1: &str = "ee.procedure.export_report.v1";

/// Schema for procedure promotion dry-run reports.
pub const PROCEDURE_PROMOTE_REPORT_SCHEMA_V1: &str = "ee.procedure.promote_report.v1";

/// Schema for procedure drift detection reports.
pub const PROCEDURE_DRIFT_REPORT_SCHEMA_V1: &str = "ee.procedure.drift_report.v1";

/// Schema for planned procedure promotion curation records.
pub const PROCEDURE_PROMOTION_CURATION_SCHEMA_V1: &str = "ee.procedure.promotion_curation.v1";

/// Schema for planned procedure promotion audit records.
pub const PROCEDURE_PROMOTION_AUDIT_SCHEMA_V1: &str = "ee.procedure.promotion_audit.v1";

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

/// Durable procedure projection supplied by a repository or fixture.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedureRecord {
    pub procedure: ProcedureDetail,
    pub steps: Vec<ProcedureStepDetail>,
    pub verification: Option<VerificationDetail>,
}

impl ProcedureRecord {
    #[must_use]
    pub fn new(procedure: ProcedureDetail) -> Self {
        Self {
            procedure,
            steps: Vec::new(),
            verification: None,
        }
    }

    #[must_use]
    pub fn with_steps(mut self, steps: Vec<ProcedureStepDetail>) -> Self {
        self.steps = steps;
        self
    }

    #[must_use]
    pub fn with_verification(mut self, verification: VerificationDetail) -> Self {
        self.verification = Some(verification);
        self
    }

    #[must_use]
    pub fn list_item(&self) -> ProcedureListItem {
        ProcedureListItem {
            procedure_id: self.procedure.procedure_id.clone(),
            title: self.procedure.title.clone(),
            status: self.procedure.status.clone(),
            step_count: usize_to_u32_saturating(self.steps.len()),
            created_at: self.procedure.created_at.clone(),
        }
    }
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
    show_procedure_from_records(options, &[])
}

/// Show details from explicit procedure records.
pub fn show_procedure_from_records(
    options: &ProcedureShowOptions,
    records: &[ProcedureRecord],
) -> Result<ProcedureShowReport, DomainError> {
    let procedure_id = options.procedure_id.trim();
    if procedure_id.is_empty() {
        return Err(DomainError::Usage {
            message: "procedure id is required".to_owned(),
            repair: Some("ee procedure show <procedure-id> --json".to_owned()),
        });
    }

    let Some(record) = records
        .iter()
        .find(|record| record.procedure.procedure_id == procedure_id)
    else {
        return Err(procedure_not_found(procedure_id));
    };

    Ok(ProcedureShowReport {
        schema: PROCEDURE_SHOW_REPORT_SCHEMA_V1.to_owned(),
        procedure: record.procedure.clone(),
        steps: if options.include_steps {
            record.steps.clone()
        } else {
            Vec::new()
        },
        verification: if options.include_verification {
            record.verification.clone()
        } else {
            None
        },
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
    list_procedures_from_records(options, &[])
}

/// List procedures from explicit records.
pub fn list_procedures_from_records(
    options: &ProcedureListOptions,
    records: &[ProcedureRecord],
) -> Result<ProcedureListReport, DomainError> {
    let mut all_procedures: Vec<_> = records.iter().map(ProcedureRecord::list_item).collect();
    all_procedures.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.procedure_id.cmp(&right.procedure_id))
    });

    let total_count = usize_to_u32_saturating(all_procedures.len());
    let mut filtered: Vec<_> = if let Some(ref status_filter) = options.status_filter {
        all_procedures
            .into_iter()
            .filter(|procedure| procedure.status == *status_filter)
            .collect()
    } else {
        all_procedures
    };
    let filtered_count = usize_to_u32_saturating(filtered.len());
    filtered.truncate(u32_to_usize_saturating(options.limit));

    Ok(ProcedureListReport {
        schema: PROCEDURE_LIST_REPORT_SCHEMA_V1.to_owned(),
        total_count,
        filtered_count,
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
    pub export_id: String,
    pub procedure_id: String,
    pub format: String,
    pub artifact_kind: String,
    pub output_path: Option<String>,
    pub content: String,
    pub content_length: usize,
    pub content_hash: String,
    pub includes_evidence: bool,
    pub redaction_status: String,
    pub install_mode: Option<String>,
    pub warnings: Vec<String>,
    pub exported_at: String,
}

impl ProcedureExportReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Export a procedure as Markdown, playbook YAML, or a render-only skill capsule.
pub fn export_procedure(
    options: &ProcedureExportOptions,
) -> Result<ProcedureExportReport, DomainError> {
    export_procedure_from_records(options, &[])
}

/// Export a procedure supplied by explicit records.
pub fn export_procedure_from_records(
    options: &ProcedureExportOptions,
    records: &[ProcedureRecord],
) -> Result<ProcedureExportReport, DomainError> {
    let format =
        ProcedureExportFormat::from_str(&options.format).map_err(|error| DomainError::Usage {
            message: error.to_string(),
            repair: Some(
                "Use --export-format markdown, --export-format playbook, or --export-format skill-capsule."
                    .to_owned(),
            ),
        })?;
    let exported_at = Utc::now().to_rfc3339();
    let export_id = format!("exp_{}", generate_id());

    let snapshot = procedure_export_snapshot(&options.procedure_id, &exported_at, records)?;
    let content = render_export_content(&snapshot, format, &export_id, &exported_at)?;
    let content_hash = format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex());
    let warnings = export_warnings(format);
    let output_path = options
        .output_path
        .as_ref()
        .map(|path| path.display().to_string());

    if let Some(path) = &options.output_path {
        write_export_file(path, &content)?;
    }

    let content_length = content.len();

    Ok(ProcedureExportReport {
        schema: PROCEDURE_EXPORT_REPORT_SCHEMA_V1.to_owned(),
        export_id,
        procedure_id: options.procedure_id.clone(),
        format: format.as_str().to_owned(),
        artifact_kind: artifact_kind(format).to_owned(),
        output_path,
        content,
        content_length,
        content_hash,
        includes_evidence: true,
        redaction_status: "not_required".to_owned(),
        install_mode: install_mode(format),
        warnings,
        exported_at,
    })
}

#[derive(Clone, Debug)]
struct ProcedureExportSnapshot {
    procedure: ProcedureDetail,
    steps: Vec<ProcedureStepDetail>,
}

fn procedure_export_snapshot(
    procedure_id: &str,
    exported_at: &str,
    records: &[ProcedureRecord],
) -> Result<ProcedureExportSnapshot, DomainError> {
    let report = show_procedure_from_records(
        &ProcedureShowOptions {
            procedure_id: procedure_id.to_owned(),
            include_steps: true,
            include_verification: false,
            ..Default::default()
        },
        records,
    )?;

    let mut procedure = report.procedure;
    procedure.updated_at = exported_at.to_owned();

    Ok(ProcedureExportSnapshot {
        procedure,
        steps: report.steps,
    })
}

fn render_export_content(
    snapshot: &ProcedureExportSnapshot,
    format: ProcedureExportFormat,
    export_id: &str,
    exported_at: &str,
) -> Result<String, DomainError> {
    Ok(match format {
        ProcedureExportFormat::Json => render_procedure_export_manifest(
            snapshot,
            export_id,
            exported_at,
            ProcedureExportFormat::Json,
        )?,
        ProcedureExportFormat::Markdown => render_procedure_markdown(snapshot, exported_at),
        ProcedureExportFormat::Playbook => render_procedure_playbook(snapshot, exported_at),
        ProcedureExportFormat::SkillCapsule => {
            render_skill_capsule(snapshot, export_id, exported_at)
        }
    })
}

fn render_procedure_markdown(snapshot: &ProcedureExportSnapshot, exported_at: &str) -> String {
    let procedure = &snapshot.procedure;
    let mut out = String::with_capacity(1024);
    out.push_str(&format!("# {}\n\n", procedure.title));
    out.push_str(&format!("{}\n\n", procedure.summary));
    out.push_str("## Procedure\n\n");
    out.push_str(&format!("- ID: `{}`\n", procedure.procedure_id));
    out.push_str(&format!("- Status: `{}`\n", procedure.status));
    out.push_str(&format!("- Generated: `{exported_at}`\n"));
    out.push_str("- Redaction: `not_required`\n\n");

    out.push_str("## Steps\n\n");
    for step in &snapshot.steps {
        out.push_str(&format!("{}. **{}**\n", step.sequence, step.title));
        out.push_str(&format!("   {}\n", step.instruction));
        if let Some(command) = &step.command_hint {
            out.push_str(&format!("   Command: `{command}`\n"));
        }
        out.push_str(&format!("   Required: `{}`\n\n", step.required));
    }

    push_markdown_provenance(&mut out, procedure);
    out
}

fn render_procedure_playbook(snapshot: &ProcedureExportSnapshot, exported_at: &str) -> String {
    let procedure = &snapshot.procedure;
    let mut out = String::with_capacity(1024);
    out.push_str("schema: \"ee.procedure.playbook.v1\"\n");
    out.push_str(&format!(
        "procedure_id: {}\n",
        yaml_string(&procedure.procedure_id)
    ));
    out.push_str(&format!("title: {}\n", yaml_string(&procedure.title)));
    out.push_str(&format!("summary: {}\n", yaml_string(&procedure.summary)));
    out.push_str(&format!("status: {}\n", yaml_string(&procedure.status)));
    out.push_str(&format!("generated_at: {}\n", yaml_string(exported_at)));
    out.push_str("redaction_status: \"not_required\"\n");
    out.push_str("steps:\n");
    for step in &snapshot.steps {
        out.push_str(&format!("  - sequence: {}\n", step.sequence));
        out.push_str(&format!("    step_id: {}\n", yaml_string(&step.step_id)));
        out.push_str(&format!("    title: {}\n", yaml_string(&step.title)));
        out.push_str(&format!(
            "    instruction: {}\n",
            yaml_string(&step.instruction)
        ));
        match &step.command_hint {
            Some(command) => out.push_str(&format!("    command_hint: {}\n", yaml_string(command))),
            None => out.push_str("    command_hint: null\n"),
        }
        out.push_str(&format!("    required: {}\n", step.required));
    }
    push_yaml_array(&mut out, "source_run_ids", &procedure.source_run_ids);
    push_yaml_array(&mut out, "evidence_ids", &procedure.evidence_ids);
    out
}

fn render_skill_capsule(
    snapshot: &ProcedureExportSnapshot,
    export_id: &str,
    exported_at: &str,
) -> String {
    let procedure = &snapshot.procedure;
    let capsule_id = format!("capsule_{}", generate_id());
    let capsule_name = format!("procedure-{}", slugify_identifier(&procedure.procedure_id));
    let mut body = String::with_capacity(1536);
    body.push_str("---\n");
    body.push_str(&format!("name: {}\n", yaml_string(&capsule_name)));
    body.push_str(&format!(
        "description: {}\n",
        yaml_string(&format!(
            "Render-only procedure capsule for {}",
            procedure.title
        ))
    ));
    body.push_str(&format!("schema: \"{}\"\n", SKILL_CAPSULE_SCHEMA_V1));
    body.push_str(&format!("capsule_id: {}\n", yaml_string(&capsule_id)));
    body.push_str(&format!(
        "procedure_id: {}\n",
        yaml_string(&procedure.procedure_id)
    ));
    body.push_str(&format!("source_export_id: {}\n", yaml_string(export_id)));
    body.push_str(&format!(
        "install_mode: {}\n",
        yaml_string(SkillCapsuleInstallMode::RenderOnly.as_str())
    ));
    body.push_str("---\n\n");
    body.push_str(&format!("# {}\n\n", procedure.title));
    body.push_str(&format!("{}\n\n", procedure.summary));
    body.push_str("## Safety\n\n");
    body.push_str("- This capsule is render-only and is not installed automatically.\n");
    body.push_str(
        "- Review the procedure and evidence before copying it into a skill directory.\n",
    );
    body.push_str(&format!("- Generated: `{exported_at}`\n\n"));
    body.push_str("## Procedure Steps\n\n");
    for step in &snapshot.steps {
        body.push_str(&format!("{}. **{}**\n", step.sequence, step.title));
        body.push_str(&format!("   {}\n", step.instruction));
        if let Some(command) = &step.command_hint {
            body.push_str(&format!("   Command: `{command}`\n"));
        }
        body.push('\n');
    }
    push_markdown_provenance(&mut body, procedure);
    body
}

fn render_procedure_export_manifest(
    snapshot: &ProcedureExportSnapshot,
    export_id: &str,
    exported_at: &str,
    format: ProcedureExportFormat,
) -> Result<String, DomainError> {
    let value = json!({
        "schema": "ee.procedure.export_artifact.v1",
        "exportId": export_id,
        "procedureId": snapshot.procedure.procedure_id,
        "format": format.as_str(),
        "generatedAt": exported_at,
        "includesEvidence": true,
        "redactionStatus": "not_required",
        "procedure": snapshot.procedure,
        "steps": snapshot.steps,
    });
    serde_json::to_string_pretty(&value).map_err(|error| DomainError::Usage {
        message: format!("failed to render procedure export manifest: {error}"),
        repair: Some("retry the export or choose --export-format markdown".to_owned()),
    })
}

fn push_markdown_provenance(out: &mut String, procedure: &ProcedureDetail) {
    out.push_str("## Provenance\n\n");
    out.push_str("Source runs:\n");
    for run_id in &procedure.source_run_ids {
        out.push_str(&format!("- `{run_id}`\n"));
    }
    out.push_str("\nEvidence IDs:\n");
    for evidence_id in &procedure.evidence_ids {
        out.push_str(&format!("- `{evidence_id}`\n"));
    }
    out.push('\n');
}

fn push_yaml_array(out: &mut String, name: &str, values: &[String]) {
    out.push_str(&format!("{name}:\n"));
    if values.is_empty() {
        out.push_str("  []\n");
    } else {
        for value in values {
            out.push_str(&format!("  - {}\n", yaml_string(value)));
        }
    }
}

fn yaml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

fn slugify_identifier(input: &str) -> String {
    let mut slug = String::with_capacity(input.len());
    let mut previous_dash = false;
    for ch in input.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            previous_dash = false;
            Some(ch.to_ascii_lowercase())
        } else if !previous_dash {
            previous_dash = true;
            Some('-')
        } else {
            None
        };
        if let Some(ch) = next {
            slug.push(ch);
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "procedure".to_owned()
    } else {
        slug.to_owned()
    }
}

fn artifact_kind(format: ProcedureExportFormat) -> &'static str {
    match format {
        ProcedureExportFormat::Json => "procedure_export_manifest",
        ProcedureExportFormat::Markdown => "procedure_markdown",
        ProcedureExportFormat::Playbook => "procedure_playbook",
        ProcedureExportFormat::SkillCapsule => "skill_capsule",
    }
}

fn install_mode(format: ProcedureExportFormat) -> Option<String> {
    match format {
        ProcedureExportFormat::SkillCapsule => {
            Some(SkillCapsuleInstallMode::RenderOnly.as_str().to_owned())
        }
        ProcedureExportFormat::Json
        | ProcedureExportFormat::Markdown
        | ProcedureExportFormat::Playbook => None,
    }
}

fn export_warnings(format: ProcedureExportFormat) -> Vec<String> {
    match format {
        ProcedureExportFormat::SkillCapsule => vec![
            "skill capsule is render-only; no files are installed".to_owned(),
            "manual review is required before copying into a skill directory".to_owned(),
        ],
        ProcedureExportFormat::Json
        | ProcedureExportFormat::Markdown
        | ProcedureExportFormat::Playbook => Vec::new(),
    }
}

fn write_export_file(path: &Path, content: &str) -> Result<(), DomainError> {
    if path.exists() {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "refusing to overwrite existing procedure export: {}",
                path.display()
            ),
            repair: Some("choose a new --output path".to_owned()),
        });
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| DomainError::Storage {
            message: format!(
                "failed to create procedure export at {}: {error}",
                path.display()
            ),
            repair: Some("choose a writable output path whose parent exists".to_owned()),
        })?;
    file.write_all(content.as_bytes())
        .map_err(|error| DomainError::Storage {
            message: format!(
                "failed to write procedure export at {}: {error}",
                path.display()
            ),
            repair: Some("retry with a writable output path".to_owned()),
        })
}

// ============================================================================
// Promote Operation (EE-414)
// ============================================================================

/// Options for promoting a procedure through the dry-run curation path.
#[derive(Clone, Debug, Default)]
pub struct ProcedurePromoteOptions {
    pub workspace: PathBuf,
    pub procedure_id: String,
    pub dry_run: bool,
    pub actor: Option<String>,
    pub reason: Option<String>,
}

/// Report from a procedure promotion dry-run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedurePromoteReport {
    pub schema: String,
    pub promotion_id: String,
    pub procedure_id: String,
    pub dry_run: bool,
    pub status: String,
    pub from_status: String,
    pub to_status: String,
    pub curation: ProcedurePromotionCurationPlan,
    pub audit: ProcedurePromotionAuditPlan,
    pub verification: ProcedurePromotionVerificationSummary,
    pub planned_effects: Vec<ProcedurePromotionEffect>,
    pub warnings: Vec<String>,
    pub next_actions: Vec<String>,
    pub generated_at: String,
}

/// Curation candidate that would be staged for a real procedure promotion.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedurePromotionCurationPlan {
    pub schema: String,
    pub candidate_id: String,
    pub candidate_type: String,
    pub target_type: String,
    pub target_id: String,
    pub target_title: String,
    pub source_type: String,
    pub source_id: Option<String>,
    pub reason: String,
    pub confidence: f64,
    pub evidence_ids: Vec<String>,
    pub status: String,
    pub would_persist: bool,
    pub applied: bool,
}

/// Audit record that would be written for a real procedure promotion.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedurePromotionAuditPlan {
    pub schema: String,
    pub operation_id: String,
    pub action: String,
    pub effect_class: String,
    pub outcome: String,
    pub dry_run: bool,
    pub actor: String,
    pub target_type: String,
    pub target_id: String,
    pub changed_surfaces: Vec<String>,
    pub transaction_status: String,
    pub would_record: bool,
    pub recorded: bool,
}

/// Verification summary used to decide whether promotion can proceed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedurePromotionVerificationSummary {
    pub verification_id: String,
    pub status: String,
    pub overall_result: String,
    pub pass_count: u32,
    pub fail_count: u32,
    pub skip_count: u32,
    pub confidence: f64,
    pub evidence_checked: Vec<String>,
}

/// Planned durable effect for a non-dry-run promotion.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedurePromotionEffect {
    pub surface: String,
    pub operation: String,
    pub target_id: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub would_write: bool,
    pub applied: bool,
}

impl ProcedurePromoteReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Build a dry-run promotion plan for a procedure.
pub fn promote_procedure(
    options: &ProcedurePromoteOptions,
) -> Result<ProcedurePromoteReport, DomainError> {
    let procedure_id = options.procedure_id.trim();
    if procedure_id.is_empty() {
        return Err(DomainError::Usage {
            message: "procedure id is required for promotion".to_owned(),
            repair: Some("ee procedure promote <procedure-id> --dry-run --json".to_owned()),
        });
    }

    if !options.dry_run {
        return Err(DomainError::PolicyDenied {
            message: "procedure promotion is dry-run-only in this implementation slice".to_owned(),
            repair: Some("ee procedure promote <procedure-id> --dry-run --json".to_owned()),
        });
    }

    let generated_at = Utc::now().to_rfc3339();
    let show = show_procedure(&ProcedureShowOptions {
        workspace: options.workspace.clone(),
        procedure_id: procedure_id.to_owned(),
        include_steps: true,
        include_verification: true,
    })?;
    let verification = verify_procedure(&ProcedureVerifyOptions {
        workspace: options.workspace.clone(),
        procedure_id: procedure_id.to_owned(),
        source_kind: Some("eval_fixture".to_owned()),
        source_ids: show.procedure.evidence_ids.clone(),
        dry_run: true,
        allow_failure: true,
    })?;

    let from_status = show.procedure.status.clone();
    let to_status = ProcedureStatus::Verified.as_str().to_owned();
    let promotion_id = format!("pprom_{}", generate_id());
    let candidate_id = format!("curate_{}", generate_id());
    let operation_id = format!("audit_{}", generate_id());
    let actor = options
        .actor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("agent")
        .to_owned();
    let reason = options
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "Promote procedure {} after dry-run verification passed with {:.1}% confidence.",
                show.procedure.procedure_id,
                verification.confidence * 100.0
            )
        });

    let already_verified = from_status == ProcedureStatus::Verified.as_str();
    let blocked = verification.fail_count > 0 || verification.overall_result != "passed";
    let status = if blocked {
        "blocked"
    } else if already_verified {
        "already_verified_dry_run"
    } else {
        "dry_run"
    }
    .to_owned();

    let curation_status = if blocked {
        "blocked"
    } else if already_verified {
        "idempotent"
    } else {
        "pending_review"
    }
    .to_owned();

    let audit_outcome = if blocked { "failure" } else { "dry_run" }.to_owned();
    let verification_summary = ProcedurePromotionVerificationSummary {
        verification_id: verification.verification_id,
        status: verification.status,
        overall_result: verification.overall_result,
        pass_count: verification.pass_count,
        fail_count: verification.fail_count,
        skip_count: verification.skip_count,
        confidence: verification.confidence,
        evidence_checked: verification
            .sources_checked
            .iter()
            .map(|source| source.source_id.clone())
            .collect(),
    };

    let curation = ProcedurePromotionCurationPlan {
        schema: PROCEDURE_PROMOTION_CURATION_SCHEMA_V1.to_owned(),
        candidate_id: candidate_id.clone(),
        candidate_type: "promote".to_owned(),
        target_type: "procedure".to_owned(),
        target_id: show.procedure.procedure_id.clone(),
        target_title: show.procedure.title,
        source_type: "human_request".to_owned(),
        source_id: Some(procedure_id.to_owned()),
        reason,
        confidence: verification_summary.confidence,
        evidence_ids: show.procedure.evidence_ids,
        status: curation_status,
        would_persist: false,
        applied: false,
    };

    let audit = ProcedurePromotionAuditPlan {
        schema: PROCEDURE_PROMOTION_AUDIT_SCHEMA_V1.to_owned(),
        operation_id: operation_id.clone(),
        action: "procedure.promote".to_owned(),
        effect_class: "durable_memory_write".to_owned(),
        outcome: audit_outcome,
        dry_run: true,
        actor,
        target_type: "procedure".to_owned(),
        target_id: procedure_id.to_owned(),
        changed_surfaces: vec![
            "procedures".to_owned(),
            "curation_candidates".to_owned(),
            "audit_log".to_owned(),
        ],
        transaction_status: "not_started".to_owned(),
        would_record: false,
        recorded: false,
    };

    let planned_effects = vec![
        ProcedurePromotionEffect {
            surface: "procedures".to_owned(),
            operation: "update_status".to_owned(),
            target_id: procedure_id.to_owned(),
            before: Some(from_status.clone()),
            after: Some(to_status.clone()),
            would_write: !blocked && !already_verified,
            applied: false,
        },
        ProcedurePromotionEffect {
            surface: "curation_candidates".to_owned(),
            operation: "insert".to_owned(),
            target_id: candidate_id,
            before: None,
            after: Some("pending_review".to_owned()),
            would_write: !blocked && !already_verified,
            applied: false,
        },
        ProcedurePromotionEffect {
            surface: "audit_log".to_owned(),
            operation: "insert".to_owned(),
            target_id: operation_id,
            before: None,
            after: Some("procedure.promote".to_owned()),
            would_write: !blocked,
            applied: false,
        },
    ];

    let mut warnings = vec![
        "dry-run only: no procedure status, curation candidate, or audit row was persisted"
            .to_owned(),
    ];
    if blocked {
        warnings.push("promotion is blocked until verification passes".to_owned());
    }
    if already_verified {
        warnings.push("procedure is already verified; promotion would be idempotent".to_owned());
    }

    let next_actions = if blocked {
        vec![
            format!("ee procedure verify {procedure_id} --json"),
            format!("ee procedure show {procedure_id} --include-verification --json"),
        ]
    } else {
        vec![
            "Review the planned curation candidate before enabling durable promotion".to_owned(),
            "ee audit timeline --json".to_owned(),
        ]
    };

    Ok(ProcedurePromoteReport {
        schema: PROCEDURE_PROMOTE_REPORT_SCHEMA_V1.to_owned(),
        promotion_id,
        procedure_id: procedure_id.to_owned(),
        dry_run: true,
        status,
        from_status,
        to_status,
        curation,
        audit,
        verification: verification_summary,
        planned_effects,
        warnings,
        next_actions,
        generated_at,
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
    let source_kind = options
        .source_kind
        .clone()
        .unwrap_or_else(|| "eval_fixture".to_owned());
    let sources_checked: Vec<_> = options
        .source_ids
        .iter()
        .map(|source_id| {
            VerificationSourceResult {
            source_id: source_id.clone(),
            source_kind: source_kind.clone(),
            result: "skipped".to_owned(),
            step_results: Vec::new(),
            message: Some(
                "verification evidence was named but no executable or inspected result was supplied"
                    .to_owned(),
            ),
        }
        })
        .collect();
    let mut report = build_verification_report(options, source_kind, sources_checked)?;
    if report.next_actions.is_empty() {
        report.next_actions.push(
            "Provide explicit verification evidence before trusting or promoting this procedure."
                .to_owned(),
        );
    }
    Ok(report)
}

/// Verify a procedure from explicit source results supplied by a fixture or repository.
pub fn verify_procedure_from_results(
    options: &ProcedureVerifyOptions,
    source_results: &[VerificationSourceResult],
) -> Result<ProcedureVerifyReport, DomainError> {
    let source_kind = options.source_kind.clone().unwrap_or_else(|| {
        source_results
            .first()
            .map(|source| source.source_kind.clone())
            .unwrap_or_else(|| "eval_fixture".to_owned())
    });
    let mut sources_checked = source_results.to_vec();
    sources_checked.sort_by(|left, right| {
        left.source_kind
            .cmp(&right.source_kind)
            .then_with(|| left.source_id.cmp(&right.source_id))
    });
    build_verification_report(options, source_kind, sources_checked)
}

fn build_verification_report(
    options: &ProcedureVerifyOptions,
    source_kind: String,
    sources_checked: Vec<VerificationSourceResult>,
) -> Result<ProcedureVerifyReport, DomainError> {
    let procedure_id = options.procedure_id.trim();
    if procedure_id.is_empty() {
        return Err(DomainError::Usage {
            message: "procedure id is required for verification".to_owned(),
            repair: Some("ee procedure verify <procedure-id> --json".to_owned()),
        });
    }

    let verification_id = format!("ver_{}", generate_id());
    let verified_at = Utc::now().to_rfc3339();

    let pass_count = count_source_results(&sources_checked, "passed");
    let fail_count = count_source_results(&sources_checked, "failed");
    let skip_count = count_source_results(&sources_checked, "skipped");

    let overall_result = if fail_count > 0 {
        "failed".to_owned()
    } else if pass_count > 0 {
        "passed".to_owned()
    } else {
        "skipped".to_owned()
    };
    let status = if fail_count > 0 {
        ProcedureVerificationStatus::Failed
    } else if pass_count > 0 && skip_count == 0 {
        ProcedureVerificationStatus::Passed
    } else {
        ProcedureVerificationStatus::Pending
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
        procedure_id: procedure_id.to_owned(),
        verification_id,
        status: status.as_str().to_owned(),
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

// ============================================================================
// Drift Detection Operation (EE-415)
// ============================================================================

/// Options for checking whether a procedure has drifted.
#[derive(Clone, Debug, Default)]
pub struct ProcedureDriftOptions {
    /// Workspace path.
    pub workspace: PathBuf,
    /// Procedure ID to check.
    pub procedure_id: String,
    /// Fixed check timestamp. Defaults to now when omitted.
    pub checked_at: Option<String>,
    /// Evidence older than this threshold is stale. Defaults to 30 days.
    pub staleness_threshold_days: u32,
    /// Optional verification report; failed reports produce drift signals.
    pub verification: Option<ProcedureVerifyReport>,
    /// Evidence freshness observations.
    pub evidence: Vec<ProcedureDriftEvidenceInput>,
    /// Dependency contract observations.
    pub dependency_contracts: Vec<ProcedureDependencyContractInput>,
    /// Dry-run posture; drift checks never mutate in this implementation slice.
    pub dry_run: bool,
}

/// Evidence freshness observation used by drift detection.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureDriftEvidenceInput {
    pub evidence_id: String,
    pub last_seen_at: String,
    pub source_kind: String,
}

/// Dependency contract observation used by drift detection.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureDependencyContractInput {
    pub dependency_name: String,
    pub owning_surface: String,
    pub expected_contract: String,
    pub actual_contract: String,
    pub compatibility: String,
}

/// Report from checking a procedure for drift.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureDriftReport {
    pub schema: String,
    pub procedure_id: String,
    pub status: String,
    pub drift_detected: bool,
    pub checked_at: String,
    pub staleness_threshold_days: u32,
    pub dry_run: bool,
    pub mutation: ProcedureDriftMutationPlan,
    pub counts: ProcedureDriftCounts,
    pub signals: Vec<ProcedureDriftSignal>,
    pub next_actions: Vec<String>,
}

/// Dry-run mutation posture for drift detection.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureDriftMutationPlan {
    pub would_mark_stale: bool,
    pub would_open_curation_candidate: bool,
    pub would_record_audit: bool,
    pub applied: bool,
}

/// Deterministic signal counters for drift detection.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureDriftCounts {
    pub total: u32,
    pub failed_verifications: u32,
    pub stale_evidence: u32,
    pub dependency_contract_changes: u32,
    pub high: u32,
    pub medium: u32,
    pub low: u32,
}

/// A single reason a procedure may have drifted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureDriftSignal {
    pub signal_id: String,
    pub kind: String,
    pub severity: String,
    pub source_id: String,
    pub summary: String,
    pub evidence_ids: Vec<String>,
    pub details: Vec<ProcedureDriftDetail>,
    pub recommended_action: String,
}

/// Stable key/value detail for a drift signal.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureDriftDetail {
    pub name: String,
    pub value: String,
}

impl ProcedureDriftReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Detect failed-verification, stale-evidence, and dependency-contract drift.
pub fn detect_procedure_drift(
    options: &ProcedureDriftOptions,
) -> Result<ProcedureDriftReport, DomainError> {
    let procedure_id = options.procedure_id.trim();
    if procedure_id.is_empty() {
        return Err(DomainError::Usage {
            message: "procedure id is required for drift detection".to_owned(),
            repair: Some(
                "ee procedure show <procedure-id> --include-verification --json".to_owned(),
            ),
        });
    }

    let checked_at = options
        .checked_at
        .clone()
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let checked_time = parse_rfc3339_utc(&checked_at, "checked_at")?;
    let threshold_days = if options.staleness_threshold_days == 0 {
        30
    } else {
        options.staleness_threshold_days
    };

    let mut signals = Vec::new();
    let mut next_actions = Vec::new();

    if let Some(verification) = &options.verification {
        push_failed_verification_signal(
            &mut signals,
            &mut next_actions,
            procedure_id,
            verification,
        );
    }

    for evidence in &options.evidence {
        push_stale_evidence_signal(
            &mut signals,
            &mut next_actions,
            procedure_id,
            evidence,
            checked_time,
            threshold_days,
        )?;
    }

    for dependency in &options.dependency_contracts {
        push_dependency_contract_signal(&mut signals, &mut next_actions, procedure_id, dependency);
    }

    let counts = count_drift_signals(&signals);
    let status = if counts.high > 0 {
        "drifted"
    } else if counts.total > 0 {
        "at_risk"
    } else {
        "current"
    }
    .to_owned();
    let drift_detected = counts.total > 0;

    if drift_detected {
        push_unique_action(
            &mut next_actions,
            "Review drift signals before promoting or exporting this procedure.",
        );
    } else {
        push_unique_action(&mut next_actions, "No drift action required.");
    }

    Ok(ProcedureDriftReport {
        schema: PROCEDURE_DRIFT_REPORT_SCHEMA_V1.to_owned(),
        procedure_id: procedure_id.to_owned(),
        status: status.clone(),
        drift_detected,
        checked_at,
        staleness_threshold_days: threshold_days,
        dry_run: true,
        mutation: ProcedureDriftMutationPlan {
            would_mark_stale: drift_detected,
            would_open_curation_candidate: status == "drifted",
            would_record_audit: drift_detected,
            applied: false,
        },
        counts,
        signals,
        next_actions,
    })
}

fn push_failed_verification_signal(
    signals: &mut Vec<ProcedureDriftSignal>,
    next_actions: &mut Vec<String>,
    procedure_id: &str,
    verification: &ProcedureVerifyReport,
) {
    let failed = verification.fail_count > 0
        || verification.overall_result == "failed"
        || verification.status == "failed";
    if !failed {
        return;
    }

    let evidence_ids = verification
        .sources_checked
        .iter()
        .map(|source| source.source_id.clone())
        .collect();
    signals.push(ProcedureDriftSignal {
        signal_id: next_drift_signal_id(signals),
        kind: "failed_verification".to_owned(),
        severity: "high".to_owned(),
        source_id: verification.verification_id.clone(),
        summary: format!(
            "Procedure verification failed with {} failed source(s).",
            verification.fail_count
        ),
        evidence_ids,
        details: vec![
            ProcedureDriftDetail {
                name: "verificationId".to_owned(),
                value: verification.verification_id.clone(),
            },
            ProcedureDriftDetail {
                name: "overallResult".to_owned(),
                value: verification.overall_result.clone(),
            },
            ProcedureDriftDetail {
                name: "verifiedAt".to_owned(),
                value: verification.verified_at.clone(),
            },
        ],
        recommended_action: format!(
            "Re-run or inspect verification before reusing procedure {procedure_id}."
        ),
    });
    push_unique_action(
        next_actions,
        &format!("ee procedure verify {procedure_id} --json"),
    );
}

fn push_stale_evidence_signal(
    signals: &mut Vec<ProcedureDriftSignal>,
    next_actions: &mut Vec<String>,
    procedure_id: &str,
    evidence: &ProcedureDriftEvidenceInput,
    checked_time: DateTime<Utc>,
    threshold_days: u32,
) -> Result<(), DomainError> {
    let evidence_time = parse_rfc3339_utc(&evidence.last_seen_at, "evidence.last_seen_at")?;
    let staleness_days = checked_time
        .signed_duration_since(evidence_time)
        .num_days()
        .max(0);
    let staleness_days = u32::try_from(staleness_days).unwrap_or(u32::MAX);
    if staleness_days < threshold_days {
        return Ok(());
    }

    signals.push(ProcedureDriftSignal {
        signal_id: next_drift_signal_id(signals),
        kind: "stale_evidence".to_owned(),
        severity: "medium".to_owned(),
        source_id: evidence.evidence_id.clone(),
        summary: format!(
            "Evidence has not been refreshed for {staleness_days} day(s), meeting the {threshold_days}-day threshold."
        ),
        evidence_ids: vec![evidence.evidence_id.clone()],
        details: vec![
            ProcedureDriftDetail {
                name: "sourceKind".to_owned(),
                value: evidence.source_kind.clone(),
            },
            ProcedureDriftDetail {
                name: "lastSeenAt".to_owned(),
                value: evidence.last_seen_at.clone(),
            },
            ProcedureDriftDetail {
                name: "stalenessDays".to_owned(),
                value: staleness_days.to_string(),
            },
        ],
        recommended_action: format!("Refresh evidence before trusting procedure {procedure_id}."),
    });
    push_unique_action(
        next_actions,
        &format!("ee procedure show {procedure_id} --include-verification --json"),
    );
    Ok(())
}

fn push_dependency_contract_signal(
    signals: &mut Vec<ProcedureDriftSignal>,
    next_actions: &mut Vec<String>,
    procedure_id: &str,
    dependency: &ProcedureDependencyContractInput,
) {
    let changed = dependency.expected_contract.trim() != dependency.actual_contract.trim()
        || !matches!(
            dependency.compatibility.trim(),
            "compatible" | "unchanged" | "same"
        );
    if !changed {
        return;
    }

    let compatibility = dependency.compatibility.trim();
    let severity = if matches!(compatibility, "breaking" | "incompatible" | "removed") {
        "high"
    } else {
        "medium"
    };

    signals.push(ProcedureDriftSignal {
        signal_id: next_drift_signal_id(signals),
        kind: "dependency_contract_change".to_owned(),
        severity: severity.to_owned(),
        source_id: dependency.dependency_name.clone(),
        summary: format!(
            "{} contract changed for {}.",
            dependency.dependency_name, dependency.owning_surface
        ),
        evidence_ids: Vec::new(),
        details: vec![
            ProcedureDriftDetail {
                name: "owningSurface".to_owned(),
                value: dependency.owning_surface.clone(),
            },
            ProcedureDriftDetail {
                name: "expectedContract".to_owned(),
                value: dependency.expected_contract.clone(),
            },
            ProcedureDriftDetail {
                name: "actualContract".to_owned(),
                value: dependency.actual_contract.clone(),
            },
            ProcedureDriftDetail {
                name: "compatibility".to_owned(),
                value: dependency.compatibility.clone(),
            },
        ],
        recommended_action: format!(
            "Review {} contract drift before reusing procedure {procedure_id}.",
            dependency.dependency_name
        ),
    });
    push_unique_action(
        next_actions,
        "ee schema export ee.procedure.schemas.v1 --json",
    );
}

fn count_drift_signals(signals: &[ProcedureDriftSignal]) -> ProcedureDriftCounts {
    let mut counts = ProcedureDriftCounts {
        total: usize_to_u32_saturating(signals.len()),
        ..ProcedureDriftCounts::default()
    };
    for signal in signals {
        match signal.kind.as_str() {
            "failed_verification" => counts.failed_verifications += 1,
            "stale_evidence" => counts.stale_evidence += 1,
            "dependency_contract_change" => counts.dependency_contract_changes += 1,
            _ => {}
        }
        match signal.severity.as_str() {
            "high" => counts.high += 1,
            "medium" => counts.medium += 1,
            "low" => counts.low += 1,
            _ => {}
        }
    }
    counts
}

fn parse_rfc3339_utc(value: &str, field: &str) -> Result<DateTime<Utc>, DomainError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| DomainError::Usage {
            message: format!("invalid {field} timestamp '{value}': {error}"),
            repair: Some("Use RFC 3339 timestamps such as 2026-05-01T12:00:00Z.".to_owned()),
        })
}

fn next_drift_signal_id(signals: &[ProcedureDriftSignal]) -> String {
    format!("drift_sig_{:03}", signals.len() + 1)
}

fn push_unique_action(actions: &mut Vec<String>, action: &str) {
    if !actions.iter().any(|existing| existing == action) {
        actions.push(action.to_owned());
    }
}

fn count_source_results(sources: &[VerificationSourceResult], result: &str) -> u32 {
    let count = sources.iter().filter(|s| s.result == result).count();
    usize_to_u32_saturating(count)
}

fn procedure_not_found(procedure_id: &str) -> DomainError {
    DomainError::NotFound {
        resource: "procedure".to_owned(),
        id: procedure_id.to_owned(),
        repair: Some("create or import a procedure record before using this command".to_owned()),
    }
}

fn usize_to_u32_saturating(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn u32_to_usize_saturating(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
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

    fn procedure_record(status: &str) -> ProcedureRecord {
        ProcedureRecord::new(ProcedureDetail {
            procedure_id: "proc_test".to_owned(),
            title: "Stored procedure".to_owned(),
            summary: "Procedure loaded from explicit records.".to_owned(),
            status: status.to_owned(),
            step_count: 2,
            source_run_ids: vec!["run_1".to_owned()],
            evidence_ids: vec!["ev_1".to_owned()],
            created_at: "2026-05-01T10:00:00Z".to_owned(),
            updated_at: "2026-05-01T11:00:00Z".to_owned(),
            verified_at: None,
        })
        .with_steps(vec![
            ProcedureStepDetail {
                step_id: "step_1".to_owned(),
                sequence: 1,
                title: "Prepare".to_owned(),
                instruction: "Prepare explicit inputs.".to_owned(),
                command_hint: None,
                required: true,
            },
            ProcedureStepDetail {
                step_id: "step_2".to_owned(),
                sequence: 2,
                title: "Verify".to_owned(),
                instruction: "Inspect explicit evidence.".to_owned(),
                command_hint: Some("ee procedure verify proc_test --json".to_owned()),
                required: true,
            },
        ])
        .with_verification(VerificationDetail {
            status: ProcedureVerificationStatus::Pending.as_str().to_owned(),
            verified_at: None,
            verified_by: None,
            pass_count: 0,
            fail_count: 0,
        })
    }

    fn second_procedure_record(status: &str) -> ProcedureRecord {
        let mut record = procedure_record(status);
        record.procedure.procedure_id = "proc_other".to_owned();
        record.procedure.title = "Other stored procedure".to_owned();
        record.procedure.created_at = "2026-05-01T09:00:00Z".to_owned();
        record
    }

    fn verification_source(source_id: &str, result: &str) -> VerificationSourceResult {
        VerificationSourceResult {
            source_id: source_id.to_owned(),
            source_kind: "eval_fixture".to_owned(),
            result: result.to_owned(),
            step_results: vec![StepVerificationResult {
                step_id: "step_1".to_owned(),
                sequence: 1,
                result: result.to_owned(),
                expected: Some("explicit evidence expected".to_owned()),
                actual: Some(format!("explicit evidence {result}")),
            }],
            message: None,
        }
    }

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
    fn show_returns_not_found_without_explicit_record() -> TestResult {
        let options = ProcedureShowOptions {
            procedure_id: "proc_test".to_owned(),
            include_steps: true,
            include_verification: false,
            ..Default::default()
        };

        let Err(error) = show_procedure(&options) else {
            return Err("show should not fabricate a procedure".to_owned());
        };
        assert_eq!(error.code(), "not_found");
        Ok(())
    }

    #[test]
    fn show_from_records_includes_requested_sections() -> TestResult {
        let options = ProcedureShowOptions {
            procedure_id: "proc_test".to_owned(),
            include_steps: true,
            include_verification: true,
            ..Default::default()
        };
        let records = [procedure_record("candidate")];

        let report = show_procedure_from_records(&options, &records).map_err(|e| e.message())?;
        assert_eq!(report.procedure.procedure_id, "proc_test");
        assert_eq!(report.steps.len(), 2);
        assert!(report.verification.is_some());
        Ok(())
    }

    #[test]
    fn list_returns_empty_without_explicit_records() -> TestResult {
        let report = list_procedures(&ProcedureListOptions {
            limit: 10,
            ..Default::default()
        })
        .map_err(|e| e.message())?;

        assert_eq!(report.total_count, 0);
        assert_eq!(report.filtered_count, 0);
        assert!(report.procedures.is_empty());
        Ok(())
    }

    #[test]
    fn list_filters_by_status() -> TestResult {
        let options = ProcedureListOptions {
            status_filter: Some("verified".to_owned()),
            limit: 10,
            ..Default::default()
        };
        let records = [
            procedure_record("candidate"),
            second_procedure_record("verified"),
        ];

        let report = list_procedures_from_records(&options, &records).map_err(|e| e.message())?;
        assert_eq!(report.total_count, 2);
        assert_eq!(report.filtered_count, 1);
        assert!(report.procedures.iter().all(|p| p.status == "verified"));
        Ok(())
    }

    #[test]
    fn export_returns_not_found_without_explicit_record() -> TestResult {
        let options = ProcedureExportOptions {
            procedure_id: "proc_test".to_owned(),
            format: "markdown".to_owned(),
            ..Default::default()
        };

        let Err(error) = export_procedure(&options) else {
            return Err("export should not fabricate a procedure".to_owned());
        };
        assert_eq!(error.code(), "not_found");
        Ok(())
    }

    #[test]
    fn export_markdown_format_contains_steps_and_provenance() -> TestResult {
        let options = ProcedureExportOptions {
            procedure_id: "proc_test".to_owned(),
            format: "markdown".to_owned(),
            ..Default::default()
        };
        let records = [procedure_record("candidate")];

        let report = export_procedure_from_records(&options, &records).map_err(|e| e.message())?;
        assert_eq!(report.format, "markdown");
        assert_eq!(report.artifact_kind, "procedure_markdown");
        assert!(report.content_length > 0);
        assert!(report.content.contains("# Stored procedure"));
        assert!(report.content.contains("## Steps"));
        assert!(report.content.contains("## Provenance"));
        assert!(report.content_hash.starts_with("blake3:"));
        Ok(())
    }

    #[test]
    fn export_playbook_format_is_yaml_like() -> TestResult {
        let options = ProcedureExportOptions {
            procedure_id: "proc_test".to_owned(),
            format: "playbook".to_owned(),
            ..Default::default()
        };
        let records = [procedure_record("candidate")];

        let report = export_procedure_from_records(&options, &records).map_err(|e| e.message())?;
        assert_eq!(report.format, "playbook");
        assert_eq!(report.artifact_kind, "procedure_playbook");
        assert!(
            report
                .content
                .contains("schema: \"ee.procedure.playbook.v1\"")
        );
        assert!(report.content.contains("steps:"));
        assert!(report.content.contains("source_run_ids:"));
        Ok(())
    }

    #[test]
    fn export_skill_capsule_is_render_only() -> TestResult {
        let options = ProcedureExportOptions {
            procedure_id: "proc_test".to_owned(),
            format: "skill-capsule".to_owned(),
            ..Default::default()
        };
        let records = [procedure_record("candidate")];

        let report = export_procedure_from_records(&options, &records).map_err(|e| e.message())?;
        assert_eq!(report.format, "skill_capsule");
        assert_eq!(report.artifact_kind, "skill_capsule");
        assert_eq!(report.install_mode.as_deref(), Some("render_only"));
        assert!(report.content.contains("schema: \"ee.skill_capsule.v1\""));
        assert!(report.content.contains("install_mode: \"render_only\""));
        assert!(report.content.contains("This capsule is render-only"));
        assert_eq!(report.warnings.len(), 2);
        Ok(())
    }

    #[test]
    fn export_rejects_unknown_format() -> TestResult {
        let options = ProcedureExportOptions {
            procedure_id: "proc_export".to_owned(),
            format: "zip".to_owned(),
            ..Default::default()
        };

        let Err(error) = export_procedure(&options) else {
            return Err("unknown format should fail".to_owned());
        };
        assert_eq!(error.code(), "usage");
        Ok(())
    }

    #[test]
    fn promote_dry_run_returns_not_found_without_record() -> TestResult {
        let options = ProcedurePromoteOptions {
            procedure_id: "proc_promote".to_owned(),
            dry_run: true,
            actor: Some("MistySalmon".to_owned()),
            reason: Some("Verified enough to promote".to_owned()),
            ..Default::default()
        };

        let Err(error) = promote_procedure(&options) else {
            return Err("promotion should require a stored procedure".to_owned());
        };
        assert_eq!(error.code(), "not_found");
        Ok(())
    }

    #[test]
    fn promote_without_dry_run_is_policy_denied() -> TestResult {
        let options = ProcedurePromoteOptions {
            procedure_id: "proc_promote".to_owned(),
            dry_run: false,
            ..Default::default()
        };

        let Err(error) = promote_procedure(&options) else {
            return Err("non-dry-run promotion should be denied".to_owned());
        };
        assert_eq!(error.code(), "policy_denied");
        assert!(
            error
                .repair()
                .unwrap_or_default()
                .contains("procedure promote")
        );
        Ok(())
    }

    #[test]
    fn verify_without_explicit_results_does_not_pass() -> TestResult {
        let options = ProcedureVerifyOptions {
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("eval_fixture".to_owned()),
            dry_run: false,
            ..Default::default()
        };

        let report = verify_procedure(&options).map_err(|e| e.message())?;
        assert!(report.verification_id.starts_with("ver_"));
        assert_eq!(report.status, "pending");
        assert_eq!(report.overall_result, "skipped");
        assert_eq!(report.pass_count, 0);
        assert_eq!(report.fail_count, 0);
        assert!(report.sources_checked.is_empty());
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
        assert!(report.sources_checked.iter().all(|s| s.result == "skipped"));
        assert_eq!(report.pass_count, 0);
        assert_eq!(report.overall_result, "skipped");
        Ok(())
    }

    #[test]
    fn verify_from_explicit_results_can_pass_or_fail() -> TestResult {
        let options = ProcedureVerifyOptions {
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("eval_fixture".to_owned()),
            dry_run: true,
            ..Default::default()
        };
        let passed = [verification_source("fixture_pass", "passed")];
        let passed_report =
            verify_procedure_from_results(&options, &passed).map_err(|e| e.message())?;
        assert_eq!(passed_report.status, "passed");
        assert_eq!(passed_report.overall_result, "passed");
        assert_eq!(passed_report.pass_count, 1);

        let failed = [verification_source("fixture_fail", "failed")];
        let failed_report =
            verify_procedure_from_results(&options, &failed).map_err(|e| e.message())?;
        assert_eq!(failed_report.status, "failed");
        assert_eq!(failed_report.overall_result, "failed");
        assert_eq!(failed_report.fail_count, 1);
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

    #[test]
    fn drift_detection_marks_failed_verification_as_drifted() -> TestResult {
        let verification = ProcedureVerifyReport {
            schema: PROCEDURE_VERIFY_REPORT_SCHEMA_V1.to_owned(),
            procedure_id: "proc_test".to_owned(),
            verification_id: "ver_failed".to_owned(),
            status: "failed".to_owned(),
            source_kind: "eval_fixture".to_owned(),
            sources_checked: vec![VerificationSourceResult {
                source_id: "fixture_failure".to_owned(),
                source_kind: "eval_fixture".to_owned(),
                result: "failed".to_owned(),
                step_results: Vec::new(),
                message: Some("expected command failed".to_owned()),
            }],
            pass_count: 0,
            fail_count: 1,
            skip_count: 0,
            overall_result: "failed".to_owned(),
            verified_at: "2026-05-01T10:00:00Z".to_owned(),
            dry_run: true,
            confidence: 0.0,
            next_actions: Vec::new(),
        };
        let report = detect_procedure_drift(&ProcedureDriftOptions {
            procedure_id: "proc_test".to_owned(),
            checked_at: Some("2026-05-01T12:00:00Z".to_owned()),
            verification: Some(verification),
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, PROCEDURE_DRIFT_REPORT_SCHEMA_V1);
        assert_eq!(report.status, "drifted");
        assert!(report.drift_detected);
        assert_eq!(report.counts.failed_verifications, 1);
        assert!(report.mutation.would_open_curation_candidate);
        assert!(!report.mutation.applied);
        Ok(())
    }

    #[test]
    fn drift_detection_marks_stale_evidence_as_at_risk() -> TestResult {
        let report = detect_procedure_drift(&ProcedureDriftOptions {
            procedure_id: "proc_test".to_owned(),
            checked_at: Some("2026-05-01T12:00:00Z".to_owned()),
            staleness_threshold_days: 30,
            evidence: vec![ProcedureDriftEvidenceInput {
                evidence_id: "ev_old".to_owned(),
                last_seen_at: "2026-03-20T12:00:00Z".to_owned(),
                source_kind: "recorder_run".to_owned(),
            }],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.status, "at_risk");
        assert_eq!(report.counts.stale_evidence, 1);
        assert_eq!(report.signals[0].kind, "stale_evidence");
        assert!(report.mutation.would_mark_stale);
        assert!(!report.mutation.would_open_curation_candidate);
        Ok(())
    }

    #[test]
    fn drift_detection_marks_breaking_dependency_contract_as_drifted() -> TestResult {
        let report = detect_procedure_drift(&ProcedureDriftOptions {
            procedure_id: "proc_test".to_owned(),
            checked_at: Some("2026-05-01T12:00:00Z".to_owned()),
            dependency_contracts: vec![ProcedureDependencyContractInput {
                dependency_name: "cass".to_owned(),
                owning_surface: "procedure verification".to_owned(),
                expected_contract: "cass.robot.v1:abc".to_owned(),
                actual_contract: "cass.robot.v2:def".to_owned(),
                compatibility: "breaking".to_owned(),
            }],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.status, "drifted");
        assert_eq!(report.counts.dependency_contract_changes, 1);
        assert_eq!(report.counts.high, 1);
        assert_eq!(report.signals[0].kind, "dependency_contract_change");
        Ok(())
    }

    #[test]
    fn drift_detection_current_when_no_signals() -> TestResult {
        let report = detect_procedure_drift(&ProcedureDriftOptions {
            procedure_id: "proc_test".to_owned(),
            checked_at: Some("2026-05-01T12:00:00Z".to_owned()),
            staleness_threshold_days: 30,
            evidence: vec![ProcedureDriftEvidenceInput {
                evidence_id: "ev_fresh".to_owned(),
                last_seen_at: "2026-04-30T12:00:00Z".to_owned(),
                source_kind: "recorder_run".to_owned(),
            }],
            dependency_contracts: vec![ProcedureDependencyContractInput {
                dependency_name: "cass".to_owned(),
                owning_surface: "procedure verification".to_owned(),
                expected_contract: "cass.robot.v1:abc".to_owned(),
                actual_contract: "cass.robot.v1:abc".to_owned(),
                compatibility: "compatible".to_owned(),
            }],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.status, "current");
        assert!(!report.drift_detected);
        assert_eq!(report.counts.total, 0);
        assert_eq!(report.next_actions, vec!["No drift action required."]);
        Ok(())
    }
}
