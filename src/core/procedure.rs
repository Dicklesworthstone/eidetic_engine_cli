//! Procedure distillation and management operations (EE-411).
//!
//! Provides propose, show, list, and export operations for procedures
//! distilled from recorder runs and curation events.

use std::fs::{self, OpenOptions};
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::{
    CreateAuditInput, CreateProcedureEventInput, CreateProcedureInput, CreateWorkspaceInput,
    DbConnection, PromoteProcedureRecordInput, StoredProcedure, StoredProcedureEvent,
    audit_actions, generate_audit_id,
};
use crate::models::{
    DomainError, ProcedureExportFormat, ProcedureMaturity, ProcedureStatus,
    ProcedureVerificationStatus, SKILL_CAPSULE_SCHEMA_V1, SkillCapsuleInstallMode, WorkspaceId,
};
use crate::output::markdown;

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

/// Schema for procedure retirement reports.
pub const PROCEDURE_RETIRE_REPORT_SCHEMA_V1: &str = "ee.procedure.retire_report.v1";

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
        crate::core::serialize_or_error(self)
    }

    #[must_use]
    pub fn to_json_pretty(&self) -> String {
        crate::core::serialize_pretty_or_error(self)
    }
}

/// Propose a new procedure from recorder runs and evidence.
pub fn propose_procedure(
    options: &ProcedureProposeOptions,
) -> Result<ProcedureProposeReport, DomainError> {
    let procedure_id = format!("proc_{}", generate_id());
    let created_at = Utc::now().to_rfc3339();
    let summary = options.summary.clone().unwrap_or_else(|| {
        "Procedure candidate request from explicit evidence; ee did not distill steps or claims."
            .to_owned()
    });
    if !options.dry_run {
        let store = open_writable_procedure_store(
            &options.workspace,
            "procedure proposal requires an initialized ee workspace database",
        )?;
        let evidence_uris = procedure_evidence_uris(&options.source_run_ids, &options.evidence_ids);
        let event_id = format!("pevt_{}", generate_id());
        let procedure = store
            .connection
            .insert_procedure(
                &procedure_id,
                &CreateProcedureInput {
                    workspace_id: store.workspace_id.clone(),
                    name: options.title.clone(),
                    body: summary.clone(),
                    level: "procedural".to_owned(),
                    maturity: ProcedureMaturity::Provisional.as_str().to_owned(),
                    confidence: initial_procedure_confidence(evidence_uris.len()),
                    utility: 0.50,
                    importance: 0.60,
                    evidence_uris: evidence_uris.clone(),
                    created_at: Some(created_at.clone()),
                },
            )
            .map_err(storage_error("failed to persist procedure"))?;
        store
            .connection
            .insert_procedure_event(
                &event_id,
                &CreateProcedureEventInput {
                    workspace_id: store.workspace_id.clone(),
                    procedure_id: procedure_id.clone(),
                    event_type: "created".to_owned(),
                    from_maturity: None,
                    to_maturity: Some(procedure.maturity),
                    reason: Some("procedure proposed from explicit evidence".to_owned()),
                    evidence_uris,
                    actor: None,
                    created_at: Some(created_at.clone()),
                },
            )
            .map_err(storage_error("failed to record procedure creation event"))?;
        let audit_id = generate_audit_id();
        let audit_details = json!({
            "eventId": event_id,
            "title": options.title.as_str(),
            "sourceRunIds": &options.source_run_ids,
            "evidenceIds": &options.evidence_ids,
            "dryRun": false,
        })
        .to_string();
        store
            .connection
            .insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(store.workspace_id),
                    actor: Some("agent".to_owned()),
                    action: audit_actions::PROCEDURE_CREATE.to_owned(),
                    target_type: Some("procedure".to_owned()),
                    target_id: Some(procedure_id.clone()),
                    details: Some(audit_details),
                },
            )
            .map_err(storage_error("failed to audit procedure creation"))?;
    }

    let report = ProcedureProposeReport {
        schema: PROCEDURE_PROPOSE_REPORT_SCHEMA_V1.to_owned(),
        procedure_id: procedure_id.clone(),
        title: options.title.clone(),
        summary,
        status: if options.dry_run {
            ProcedureStatus::Candidate.as_str()
        } else {
            ProcedureMaturity::Provisional.as_str()
        }
        .to_owned(),
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
    pub history: Vec<ProcedureHistoryEvent>,
}

/// Durable procedure projection supplied by a repository or fixture.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcedureRecord {
    pub procedure: ProcedureDetail,
    pub steps: Vec<ProcedureStepDetail>,
    pub verification: Option<VerificationDetail>,
    pub history: Vec<ProcedureHistoryEvent>,
}

impl ProcedureRecord {
    #[must_use]
    pub fn new(procedure: ProcedureDetail) -> Self {
        Self {
            procedure,
            steps: Vec::new(),
            verification: None,
            history: Vec::new(),
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
    pub fn with_history(mut self, history: Vec<ProcedureHistoryEvent>) -> Self {
        self.history = history;
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

/// One persisted procedure history entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureHistoryEvent {
    pub event_id: String,
    pub event_type: String,
    pub from_maturity: Option<String>,
    pub to_maturity: Option<String>,
    pub reason: Option<String>,
    pub evidence_uris: Vec<String>,
    pub actor: Option<String>,
    pub created_at: String,
}

impl ProcedureShowReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Show details of a procedure.
pub fn show_procedure(options: &ProcedureShowOptions) -> Result<ProcedureShowReport, DomainError> {
    let records = load_procedure_records(&options.workspace, None, u32::MAX)?;
    show_procedure_from_records(options, &records)
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
        history: record.history.clone(),
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
        crate::core::serialize_or_error(self)
    }
}

/// List procedures with optional filters.
pub fn list_procedures(options: &ProcedureListOptions) -> Result<ProcedureListReport, DomainError> {
    let records = load_procedure_records(
        &options.workspace,
        options.status_filter.as_deref(),
        list_limit(options.limit),
    )?;
    list_procedures_from_records(options, &records)
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
        crate::core::serialize_or_error(self)
    }
}

/// Export a procedure as Markdown, playbook YAML, or a render-only skill capsule.
pub fn export_procedure(
    options: &ProcedureExportOptions,
) -> Result<ProcedureExportReport, DomainError> {
    let records = load_procedure_records(&options.workspace, None, u32::MAX)?;
    export_procedure_from_records(options, &records)
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
    let (public_snapshot, redacted_evidence_refs) = public_procedure_export_snapshot(&snapshot);
    let redaction_status = procedure_export_redaction_status(redacted_evidence_refs);
    let content = render_export_content(
        &public_snapshot,
        format,
        &export_id,
        &exported_at,
        redaction_status,
    )?;
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
        includes_evidence: snapshot.includes_evidence(),
        redaction_status: redaction_status.to_owned(),
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

impl ProcedureExportSnapshot {
    fn includes_evidence(&self) -> bool {
        !self.procedure.source_run_ids.is_empty() || !self.procedure.evidence_ids.is_empty()
    }
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
    redaction_status: &str,
) -> Result<String, DomainError> {
    Ok(match format {
        ProcedureExportFormat::Json => render_procedure_export_manifest(
            snapshot,
            export_id,
            exported_at,
            ProcedureExportFormat::Json,
            redaction_status,
        )?,
        ProcedureExportFormat::Markdown => {
            render_procedure_markdown(snapshot, exported_at, redaction_status)
        }
        ProcedureExportFormat::Playbook => {
            render_procedure_playbook(snapshot, exported_at, redaction_status)
        }
        ProcedureExportFormat::SkillCapsule => {
            render_skill_capsule(snapshot, export_id, exported_at, redaction_status)
        }
    })
}

fn render_procedure_markdown(
    snapshot: &ProcedureExportSnapshot,
    exported_at: &str,
    redaction_status: &str,
) -> String {
    let procedure = &snapshot.procedure;
    let mut out = String::with_capacity(1024);
    out.push_str(&format!(
        "# {}\n\n",
        markdown::escape_heading(&procedure.title)
    ));
    out.push_str(&format!(
        "{}\n\n",
        markdown::escape_text(&procedure.summary)
    ));
    out.push_str("## Procedure\n\n");
    out.push_str(&format!(
        "- ID: {}\n",
        markdown::inline_code(&procedure.procedure_id)
    ));
    out.push_str(&format!(
        "- Status: {}\n",
        markdown::inline_code(&procedure.status)
    ));
    out.push_str(&format!(
        "- Generated: {}\n",
        markdown::inline_code(exported_at)
    ));
    out.push_str(&format!(
        "- Redaction: {}\n\n",
        markdown::inline_code(redaction_status)
    ));

    out.push_str("## Steps\n\n");
    for step in &snapshot.steps {
        out.push_str(&format!(
            "{}. **{}**\n",
            step.sequence,
            markdown::escape_text(&step.title)
        ));
        out.push_str(&format!(
            "   {}\n",
            markdown::escape_text(&step.instruction)
        ));
        if let Some(command) = &step.command_hint {
            out.push_str(&format!("   Command: {}\n", markdown::inline_code(command)));
        }
        out.push_str(&format!(
            "   Required: {}\n\n",
            markdown::inline_code(&step.required.to_string())
        ));
    }

    push_markdown_provenance(&mut out, procedure);
    out
}

fn render_procedure_playbook(
    snapshot: &ProcedureExportSnapshot,
    exported_at: &str,
    redaction_status: &str,
) -> String {
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
    out.push_str(&format!(
        "redaction_status: {}\n",
        yaml_string(redaction_status)
    ));
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
    redaction_status: &str,
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
            markdown::escape_text(&procedure.title)
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
    body.push_str(&format!(
        "# {}\n\n",
        markdown::escape_heading(&procedure.title)
    ));
    body.push_str(&format!(
        "{}\n\n",
        markdown::escape_text(&procedure.summary)
    ));
    body.push_str("## Safety\n\n");
    body.push_str("- This capsule is render-only and is not installed automatically.\n");
    body.push_str(
        "- Review the procedure and evidence before copying it into a skill directory.\n",
    );
    body.push_str(&format!(
        "- Redaction: {}\n",
        markdown::inline_code(redaction_status)
    ));
    body.push_str(&format!("- Generated: `{exported_at}`\n\n"));
    body.push_str("## Procedure Steps\n\n");
    for step in &snapshot.steps {
        body.push_str(&format!(
            "{}. **{}**\n",
            step.sequence,
            markdown::escape_text(&step.title)
        ));
        body.push_str(&format!(
            "   {}\n",
            markdown::escape_text(&step.instruction)
        ));
        if let Some(command) = &step.command_hint {
            body.push_str(&format!("   Command: {}\n", markdown::inline_code(command)));
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
    redaction_status: &str,
) -> Result<String, DomainError> {
    let value = json!({
        "schema": "ee.procedure.export_artifact.v1",
        "exportId": export_id,
        "procedureId": snapshot.procedure.procedure_id,
        "format": format.as_str(),
        "generatedAt": exported_at,
        "includesEvidence": snapshot.includes_evidence(),
        "redactionStatus": redaction_status,
        "procedure": snapshot.procedure,
        "steps": snapshot.steps,
    });
    serde_json::to_string_pretty(&value).map_err(|error| DomainError::Usage {
        message: format!("failed to render procedure export manifest: {error}"),
        repair: Some("retry the export or choose --export-format markdown".to_owned()),
    })
}

fn procedure_export_redaction_status(redacted: bool) -> &'static str {
    if redacted { "standard" } else { "not_required" }
}

fn public_procedure_export_snapshot(
    snapshot: &ProcedureExportSnapshot,
) -> (ProcedureExportSnapshot, bool) {
    let mut public = snapshot.clone();
    let mut redacted = false;
    public.procedure.source_run_ids =
        redact_procedure_public_source_refs(&public.procedure.source_run_ids, &mut redacted);
    public.procedure.evidence_ids =
        redact_procedure_public_source_refs(&public.procedure.evidence_ids, &mut redacted);
    (public, redacted)
}

fn redact_procedure_public_source_refs(values: &[String], redacted: &mut bool) -> Vec<String> {
    values
        .iter()
        .map(|value| {
            let replacement = redact_procedure_public_source_ref(value);
            if replacement != *value {
                *redacted = true;
            }
            replacement
        })
        .collect()
}

fn redact_procedure_public_source_ref(value: &str) -> String {
    let secret_redacted = crate::policy::redact_secret_like_content(value).content;
    redact_procedure_public_path_like_segments(&secret_redacted)
}

fn redact_procedure_public_path_like_segments(value: &str) -> String {
    const REDACTED_PATH: &str = "[REDACTED_PATH]";
    const PREFIXES: &[&str] = &[
        "/Users/",
        "/Volumes/",
        "/private/",
        "/var/",
        "/tmp/",
        "/home/",
        "/data/",
        "/dp/",
        "/workspace/",
        "/repo/",
        "/etc/",
    ];

    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let Some((relative_index, _)) = value[cursor..].char_indices().find(|(_, ch)| *ch == '/')
        else {
            output.push_str(&value[cursor..]);
            break;
        };
        let start = cursor + relative_index;
        if !PREFIXES
            .iter()
            .any(|prefix| value[start..].starts_with(prefix))
        {
            output.push_str(&value[cursor..=start]);
            cursor = start + 1;
            continue;
        }

        output.push_str(&value[cursor..start]);
        output.push_str(REDACTED_PATH);
        cursor = value[start..]
            .char_indices()
            .find_map(|(index, ch)| {
                procedure_public_source_path_boundary(ch).then_some(start + index)
            })
            .unwrap_or(value.len());
    }
    output
}

fn procedure_public_source_path_boundary(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '?' | '#' | '"' | '\'' | ')' | ']' | '}' | ',' | ';')
}

fn push_markdown_provenance(out: &mut String, procedure: &ProcedureDetail) {
    out.push_str("## Provenance\n\n");
    out.push_str("Source runs:\n");
    for run_id in &procedure.source_run_ids {
        out.push_str(&format!("- {}\n", markdown::inline_code(run_id)));
    }
    out.push_str("\nEvidence IDs:\n");
    for evidence_id in &procedure.evidence_ids {
        out.push_str(&format!("- {}\n", markdown::inline_code(evidence_id)));
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
    ensure_no_procedure_path_symlink_components(path, "write procedure export").map_err(
        |error| DomainError::Storage {
            message: format!(
                "failed to create procedure export at {}: {error}",
                path.display()
            ),
            repair: Some("choose a real output path without symlinked components".to_owned()),
        },
    )?;
    ensure_procedure_export_path_is_regular_or_missing(path)?;
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

fn ensure_procedure_export_path_is_regular_or_missing(path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(DomainError::PolicyDenied {
            message: format!(
                "procedure export path '{}' is not a regular file",
                path.display()
            ),
            repair: Some(
                "choose a new --output path that does not already name a directory or special file"
                    .to_owned(),
            ),
        }),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "failed to inspect procedure export path '{}': {error}",
                path.display()
            ),
            repair: Some("choose a readable parent directory for --output".to_owned()),
        }),
    }
}

// ============================================================================
// Promote Operation (EE-414)
// ============================================================================

/// Options for promoting a procedure through the dry-run curation path.
#[derive(Clone, Debug, Default)]
pub struct ProcedurePromoteOptions {
    pub workspace: PathBuf,
    pub procedure_id: String,
    pub to_maturity: Option<String>,
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
        crate::core::serialize_or_error(self)
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
    let target_maturity = parse_target_maturity(options.to_maturity.as_deref())?;
    if !options.dry_run {
        return promote_persisted_procedure(options, procedure_id, target_maturity);
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
    let to_status = target_maturity.as_str().to_owned();
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

fn promote_persisted_procedure(
    options: &ProcedurePromoteOptions,
    procedure_id: &str,
    target_maturity: ProcedureMaturity,
) -> Result<ProcedurePromoteReport, DomainError> {
    let store = open_writable_procedure_store(
        &options.workspace,
        "procedure promotion requires an initialized ee workspace database",
    )?;
    let stored = store
        .connection
        .get_procedure(&store.workspace_id, procedure_id)
        .map_err(storage_error("failed to load procedure"))?
        .ok_or_else(|| procedure_not_found(procedure_id))?;
    let from_maturity =
        ProcedureMaturity::from_str(&stored.maturity).map_err(|error| DomainError::Storage {
            message: format!("stored procedure has invalid maturity: {error}"),
            repair: Some("repair the procedure row or re-import the candidate".to_owned()),
        })?;
    if !from_maturity.can_promote_to(target_maturity) {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "cannot promote procedure from {} to {}",
                from_maturity.as_str(),
                target_maturity.as_str()
            ),
            repair: Some("choose a forward maturity transition".to_owned()),
        });
    }
    let threshold = promotion_evidence_threshold(target_maturity);
    if stored.evidence_uris.len() < threshold {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "procedure promotion to {} requires at least {threshold} evidence URI(s); found {}",
                target_maturity.as_str(),
                stored.evidence_uris.len()
            ),
            repair: Some(format!(
                "add evidence before running ee procedure promote {procedure_id} --to {}",
                target_maturity.as_str()
            )),
        });
    }

    let generated_at = Utc::now().to_rfc3339();
    let promotion_id = format!("pprom_{}", generate_id());
    let operation_id = generate_audit_id();
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
                "Promote procedure {procedure_id} from {} to {} with {} evidence URI(s).",
                from_maturity.as_str(),
                target_maturity.as_str(),
                stored.evidence_uris.len()
            )
        });
    let event_id = format!("pevt_{}", generate_id());
    let event = store
        .connection
        .promote_procedure_record(PromoteProcedureRecordInput {
            workspace_id: &store.workspace_id,
            procedure_id,
            to_maturity: target_maturity.as_str(),
            event_id: &event_id,
            reason: Some(&reason),
            actor: Some(&actor),
            evidence_uris: &stored.evidence_uris,
        })
        .map_err(storage_error("failed to promote procedure"))?
        .ok_or_else(|| procedure_not_found(procedure_id))?;
    let audit_details = json!({
        "promotionId": promotion_id,
        "eventId": event.id.clone(),
        "fromMaturity": from_maturity.as_str(),
        "toMaturity": target_maturity.as_str(),
        "evidenceUris": stored.evidence_uris.clone(),
        "reason": reason,
    })
    .to_string();
    store
        .connection
        .insert_audit(
            &operation_id,
            &CreateAuditInput {
                workspace_id: Some(store.workspace_id.clone()),
                actor: Some(actor.clone()),
                action: audit_actions::PROCEDURE_PROMOTE.to_owned(),
                target_type: Some("procedure".to_owned()),
                target_id: Some(procedure_id.to_owned()),
                details: Some(audit_details),
            },
        )
        .map_err(storage_error("failed to audit procedure promotion"))?;

    let verification = ProcedurePromotionVerificationSummary {
        verification_id: event.id.clone(),
        status: "passed".to_owned(),
        overall_result: "passed".to_owned(),
        pass_count: usize_to_u32_saturating(event.evidence_uris.len()),
        fail_count: 0,
        skip_count: 0,
        confidence: stored.confidence.into(),
        evidence_checked: event.evidence_uris.clone(),
    };
    let curation = ProcedurePromotionCurationPlan {
        schema: PROCEDURE_PROMOTION_CURATION_SCHEMA_V1.to_owned(),
        candidate_id: "not_required".to_owned(),
        candidate_type: "procedure".to_owned(),
        target_type: "procedure".to_owned(),
        target_id: procedure_id.to_owned(),
        target_title: stored.name.clone(),
        source_type: "procedure_store".to_owned(),
        source_id: Some(event.id.clone()),
        reason: "direct procedure maturity promotion".to_owned(),
        confidence: stored.confidence.into(),
        evidence_ids: event.evidence_uris.clone(),
        status: "applied".to_owned(),
        would_persist: true,
        applied: true,
    };
    let audit = ProcedurePromotionAuditPlan {
        schema: PROCEDURE_PROMOTION_AUDIT_SCHEMA_V1.to_owned(),
        operation_id: operation_id.clone(),
        action: audit_actions::PROCEDURE_PROMOTE.to_owned(),
        effect_class: "durable_memory_write".to_owned(),
        outcome: "success".to_owned(),
        dry_run: false,
        actor,
        target_type: "procedure".to_owned(),
        target_id: procedure_id.to_owned(),
        changed_surfaces: vec!["procedures".to_owned(), "procedure_events".to_owned()],
        transaction_status: "committed".to_owned(),
        would_record: true,
        recorded: true,
    };

    Ok(ProcedurePromoteReport {
        schema: PROCEDURE_PROMOTE_REPORT_SCHEMA_V1.to_owned(),
        promotion_id,
        procedure_id: procedure_id.to_owned(),
        dry_run: false,
        status: "promoted".to_owned(),
        from_status: from_maturity.as_str().to_owned(),
        to_status: target_maturity.as_str().to_owned(),
        curation,
        audit,
        verification,
        planned_effects: vec![
            ProcedurePromotionEffect {
                surface: "procedures".to_owned(),
                operation: "update_maturity".to_owned(),
                target_id: procedure_id.to_owned(),
                before: Some(from_maturity.as_str().to_owned()),
                after: Some(target_maturity.as_str().to_owned()),
                would_write: true,
                applied: true,
            },
            ProcedurePromotionEffect {
                surface: "procedure_events".to_owned(),
                operation: "insert".to_owned(),
                target_id: event.id,
                before: None,
                after: Some("promoted".to_owned()),
                would_write: true,
                applied: true,
            },
            ProcedurePromotionEffect {
                surface: "audit_log".to_owned(),
                operation: "insert".to_owned(),
                target_id: operation_id,
                before: None,
                after: Some(audit_actions::PROCEDURE_PROMOTE.to_owned()),
                would_write: true,
                applied: true,
            },
        ],
        warnings: Vec::new(),
        next_actions: vec![format!("ee procedure show {procedure_id} --json")],
        generated_at,
    })
}

fn parse_target_maturity(raw: Option<&str>) -> Result<ProcedureMaturity, DomainError> {
    raw.map(ProcedureMaturity::from_str)
        .transpose()
        .map_err(|error| DomainError::Usage {
            message: error.to_string(),
            repair: Some("Use --to validated, --to mature, or --to retired.".to_owned()),
        })
        .map(|value| value.unwrap_or(ProcedureMaturity::Validated))
}

fn promotion_evidence_threshold(target: ProcedureMaturity) -> usize {
    match target {
        ProcedureMaturity::Provisional => 0,
        ProcedureMaturity::Validated => 1,
        ProcedureMaturity::Mature => 2,
        ProcedureMaturity::Retired => 0,
    }
}

/// Options for retiring a procedure.
#[derive(Clone, Debug, Default)]
pub struct ProcedureRetireOptions {
    pub workspace: PathBuf,
    pub procedure_id: String,
    pub reason: String,
    pub actor: Option<String>,
}

/// Report from retiring a procedure.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcedureRetireReport {
    pub schema: String,
    pub procedure_id: String,
    pub status: String,
    pub from_maturity: String,
    pub to_maturity: String,
    pub event_id: String,
    pub audit_id: String,
    pub reason: String,
    pub retired_at: String,
}

impl ProcedureRetireReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

/// Retire a procedure with an audited history row.
pub fn retire_procedure(
    options: &ProcedureRetireOptions,
) -> Result<ProcedureRetireReport, DomainError> {
    let procedure_id = options.procedure_id.trim();
    if procedure_id.is_empty() {
        return Err(DomainError::Usage {
            message: "procedure id is required for retirement".to_owned(),
            repair: Some("ee procedure retire <procedure-id> --reason <reason>".to_owned()),
        });
    }
    let reason = options.reason.trim();
    if reason.is_empty() {
        return Err(DomainError::Usage {
            message: "procedure retirement reason is required".to_owned(),
            repair: Some("ee procedure retire <procedure-id> --reason <reason>".to_owned()),
        });
    }
    let store = open_writable_procedure_store(
        &options.workspace,
        "procedure retirement requires an initialized ee workspace database",
    )?;
    let before = store
        .connection
        .get_procedure(&store.workspace_id, procedure_id)
        .map_err(storage_error("failed to load procedure"))?
        .ok_or_else(|| procedure_not_found(procedure_id))?;
    let actor = options
        .actor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("agent")
        .to_owned();
    let event_id = format!("pevt_{}", generate_id());
    let event = store
        .connection
        .retire_procedure_record(
            &store.workspace_id,
            procedure_id,
            &event_id,
            reason,
            Some(&actor),
        )
        .map_err(storage_error("failed to retire procedure"))?
        .ok_or_else(|| procedure_not_found(procedure_id))?;
    let audit_id = generate_audit_id();
    let audit_details = json!({
        "eventId": event.id.clone(),
        "fromMaturity": before.maturity,
        "toMaturity": "retired",
        "reason": reason,
    })
    .to_string();
    store
        .connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(store.workspace_id),
                actor: Some(actor),
                action: audit_actions::PROCEDURE_RETIRE.to_owned(),
                target_type: Some("procedure".to_owned()),
                target_id: Some(procedure_id.to_owned()),
                details: Some(audit_details),
            },
        )
        .map_err(storage_error("failed to audit procedure retirement"))?;

    Ok(ProcedureRetireReport {
        schema: PROCEDURE_RETIRE_REPORT_SCHEMA_V1.to_owned(),
        procedure_id: procedure_id.to_owned(),
        status: "retired".to_owned(),
        from_maturity: event
            .from_maturity
            .clone()
            .unwrap_or_else(|| ProcedureMaturity::Provisional.as_str().to_owned()),
        to_maturity: "retired".to_owned(),
        event_id: event.id,
        audit_id,
        reason: reason.to_owned(),
        retired_at: event.created_at,
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

impl FromStr for VerificationSourceKind {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().replace('-', "_").as_str() {
            "eval_fixture" => Ok(Self::EvalFixture),
            "repro_pack" => Ok(Self::ReproPack),
            "claim_evidence" => Ok(Self::ClaimEvidence),
            "recorder_run" => Ok(Self::RecorderRun),
            other => Err(DomainError::Usage {
                message: format!("unsupported procedure verification source kind '{other}'"),
                repair: Some(
                    "Use one of: eval_fixture, repro_pack, claim_evidence, recorder_run."
                        .to_owned(),
                ),
            }),
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
        crate::core::serialize_or_error(self)
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
    let source_kind = options.source_kind.as_deref().unwrap_or("eval_fixture");
    let source_kind = VerificationSourceKind::from_str(source_kind)?;
    let source_kind_name = source_kind.as_str().to_owned();
    let sources_checked: Vec<_> = options
        .source_ids
        .iter()
        .map(|source_id| inspect_named_verification_source(options, &source_kind, source_id))
        .collect();
    let mut report = build_verification_report(options, source_kind_name, sources_checked)?;
    if report.next_actions.is_empty() {
        report.next_actions.push(
            "Provide explicit verification evidence before trusting or promoting this procedure."
                .to_owned(),
        );
    }
    Ok(report)
}

fn inspect_named_verification_source(
    options: &ProcedureVerifyOptions,
    source_kind: &VerificationSourceKind,
    source_id: &str,
) -> VerificationSourceResult {
    let source_id = source_id.trim();
    if source_id.is_empty() {
        return verification_source_result(
            "",
            source_kind.as_str(),
            "failed",
            Some("verification source id is empty".to_owned()),
            Vec::new(),
        );
    }

    let file_result = match source_kind {
        VerificationSourceKind::ReproPack => inspect_repro_pack_source(options, source_id),
        VerificationSourceKind::EvalFixture
        | VerificationSourceKind::ClaimEvidence
        | VerificationSourceKind::RecorderRun => inspect_filesystem_verification_source(
            &options.workspace,
            source_kind.as_str(),
            source_id,
        ),
    };
    if let Some(result) = file_result {
        return result;
    }

    match source_kind {
        VerificationSourceKind::EvalFixture => verification_source_result(
            source_id,
            source_kind.as_str(),
            "failed",
            Some(
                "named eval fixture was not found in workspace evidence or eval fixtures"
                    .to_owned(),
            ),
            Vec::new(),
        ),
        VerificationSourceKind::ReproPack => verification_source_result(
            source_id,
            source_kind.as_str(),
            "failed",
            Some(
                "named repro pack was not found or did not contain required pack files".to_owned(),
            ),
            Vec::new(),
        ),
        VerificationSourceKind::ClaimEvidence => {
            inspect_persisted_claim_evidence(options, source_id).unwrap_or_else(|| {
                verification_source_result(
                    source_id,
                    source_kind.as_str(),
                    "failed",
                    Some(
                        "named claim evidence was not found in persisted evidence spans".to_owned(),
                    ),
                    Vec::new(),
                )
            })
        }
        VerificationSourceKind::RecorderRun => inspect_persisted_recorder_run(options, source_id)
            .unwrap_or_else(|| {
                verification_source_result(
                    source_id,
                    source_kind.as_str(),
                    "failed",
                    Some("named recorder run was not found in persisted recorder runs".to_owned()),
                    Vec::new(),
                )
            }),
    }
}

fn inspect_filesystem_verification_source(
    workspace: &Path,
    source_kind: &str,
    source_id: &str,
) -> Option<VerificationSourceResult> {
    for path in verification_source_candidate_paths(workspace, source_kind, source_id) {
        match procedure_path_is_file(&path) {
            Ok(true) => {
                return Some(inspect_verification_json_file(
                    &path,
                    source_kind,
                    source_id,
                ));
            }
            Ok(false) => {}
            Err(error) => {
                return Some(procedure_source_path_failure(
                    &path,
                    source_kind,
                    source_id,
                    error,
                ));
            }
        }
    }

    if source_kind == "eval_fixture" {
        for root in source_search_roots(workspace) {
            if let Some(path) = find_eval_fixture_scenario(&root, source_id) {
                return Some(inspect_verification_json_file(
                    &path,
                    source_kind,
                    source_id,
                ));
            }
        }
    }

    None
}

fn inspect_repro_pack_source(
    options: &ProcedureVerifyOptions,
    source_id: &str,
) -> Option<VerificationSourceResult> {
    for path in verification_source_candidate_paths(&options.workspace, "repro_pack", source_id) {
        match procedure_path_is_file(&path) {
            Ok(true) => {
                return Some(inspect_verification_json_file(
                    &path,
                    "repro_pack",
                    source_id,
                ));
            }
            Ok(false) => {}
            Err(error) => {
                return Some(procedure_source_path_failure(
                    &path,
                    "repro_pack",
                    source_id,
                    error,
                ));
            }
        }
        match procedure_path_is_dir(&path) {
            Ok(true) => return Some(inspect_repro_pack_dir(&path, source_id)),
            Ok(false) => {}
            Err(error) => {
                return Some(procedure_source_path_failure(
                    &path,
                    "repro_pack",
                    source_id,
                    error,
                ));
            }
        }
    }
    None
}

fn inspect_persisted_claim_evidence(
    options: &ProcedureVerifyOptions,
    source_id: &str,
) -> Option<VerificationSourceResult> {
    let store = match open_procedure_store(&options.workspace) {
        Ok(Some(store)) => store,
        Ok(None) => return None,
        Err(error) => {
            return Some(persisted_source_storage_failure(
                source_id,
                "claim_evidence",
                "failed to open procedure store for claim evidence",
                error,
            ));
        }
    };
    let evidence_id = source_id.strip_prefix("evidence://").unwrap_or(source_id);
    let evidence = match store.connection.get_evidence_span(evidence_id) {
        Ok(Some(evidence)) => evidence,
        Ok(None) => return None,
        Err(error) => {
            return Some(persisted_source_storage_failure(
                source_id,
                "claim_evidence",
                "failed to load persisted evidence span",
                storage_error("failed to load persisted evidence span")(error),
            ));
        }
    };
    let result =
        if evidence.workspace_id == store.workspace_id && !evidence.excerpt.trim().is_empty() {
            "passed"
        } else {
            "failed"
        };
    Some(verification_source_result(
        source_id,
        "claim_evidence",
        result,
        Some(format!(
            "inspected persisted evidence span {} from session {}",
            evidence.id, evidence.session_id
        )),
        vec![StepVerificationResult {
            step_id: format!("claim_evidence_{}", evidence.id),
            sequence: 1,
            result: result.to_owned(),
            expected: Some(
                "evidence span exists in this workspace with non-empty excerpt".to_owned(),
            ),
            actual: Some(format!(
                "workspace={}, lines={}-{}, hash={}",
                evidence.workspace_id,
                evidence.start_line,
                evidence.end_line,
                evidence.content_hash
            )),
        }],
    ))
}

fn inspect_persisted_recorder_run(
    options: &ProcedureVerifyOptions,
    source_id: &str,
) -> Option<VerificationSourceResult> {
    let store = match open_procedure_store(&options.workspace) {
        Ok(Some(store)) => store,
        Ok(None) => return None,
        Err(error) => {
            return Some(persisted_source_storage_failure(
                source_id,
                "recorder_run",
                "failed to open procedure store for recorder run",
                error,
            ));
        }
    };
    let run = match store.connection.get_recorder_run(source_id) {
        Ok(Some(run)) => run,
        Ok(None) => return None,
        Err(error) => {
            return Some(persisted_source_storage_failure(
                source_id,
                "recorder_run",
                "failed to load persisted recorder run",
                storage_error("failed to load persisted recorder run")(error),
            ));
        }
    };
    let workspace_matches = run
        .workspace_id
        .as_deref()
        .is_none_or(|workspace_id| workspace_id == store.workspace_id);
    let passed = workspace_matches
        && run.chain_complete
        && run.event_count > 0
        && matches!(run.status.as_str(), "completed" | "imported");
    let result = if passed { "passed" } else { "failed" };
    Some(verification_source_result(
        source_id,
        "recorder_run",
        result,
        Some(format!(
            "inspected persisted recorder run with status {} and {} event(s)",
            run.status, run.event_count
        )),
        vec![
            StepVerificationResult {
                step_id: format!("recorder_run_{source_id}_status"),
                sequence: 1,
                result: if matches!(run.status.as_str(), "completed" | "imported") {
                    "passed".to_owned()
                } else {
                    "failed".to_owned()
                },
                expected: Some("recorder run status is completed or imported".to_owned()),
                actual: Some(run.status),
            },
            StepVerificationResult {
                step_id: format!("recorder_run_{source_id}_chain"),
                sequence: 2,
                result: if run.chain_complete {
                    "passed"
                } else {
                    "failed"
                }
                .to_owned(),
                expected: Some("recorder event chain is complete".to_owned()),
                actual: Some(format!("chain_complete={}", run.chain_complete)),
            },
            StepVerificationResult {
                step_id: format!("recorder_run_{source_id}_events"),
                sequence: 3,
                result: if run.event_count > 0 {
                    "passed"
                } else {
                    "failed"
                }
                .to_owned(),
                expected: Some("recorder run has at least one event".to_owned()),
                actual: Some(format!("event_count={}", run.event_count)),
            },
        ],
    ))
}

fn persisted_source_storage_failure(
    source_id: &str,
    source_kind: &str,
    context: &str,
    error: DomainError,
) -> VerificationSourceResult {
    let repair = error.repair().unwrap_or("ee doctor --json");
    verification_source_result(
        source_id,
        source_kind,
        "failed",
        Some(format!("{context}: {}. Next: {repair}", error.message())),
        vec![StepVerificationResult {
            step_id: format!("{source_kind}_{source_id}_storage"),
            sequence: 1,
            result: "failed".to_owned(),
            expected: Some(format!("persisted {source_kind} source is readable")),
            actual: Some(format!("{} ({})", error.message(), error.code())),
        }],
    )
}

fn inspect_verification_json_file(
    path: &Path,
    source_kind: &str,
    source_id: &str,
) -> VerificationSourceResult {
    if let Err(error) =
        ensure_no_procedure_path_symlink_components(path, "read verification source")
    {
        return procedure_source_path_failure(path, source_kind, source_id, error);
    }
    match procedure_path_is_file(path) {
        Ok(true) => {}
        Ok(false) => {
            return verification_source_result(
                source_id,
                source_kind,
                "failed",
                Some(format!(
                    "verification source {} is not a regular file",
                    path.display()
                )),
                Vec::new(),
            );
        }
        Err(error) => return procedure_source_path_failure(path, source_kind, source_id, error),
    }
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            return verification_source_result(
                source_id,
                source_kind,
                "failed",
                Some(format!(
                    "failed to read verification source {}: {error}",
                    path.display()
                )),
                Vec::new(),
            );
        }
    };
    let value: Value = match serde_json::from_str(&content) {
        Ok(value) => value,
        Err(error) => {
            return verification_source_result(
                source_id,
                source_kind,
                "failed",
                Some(format!(
                    "verification source {} is not valid JSON: {error}",
                    path.display()
                )),
                Vec::new(),
            );
        }
    };
    verification_result_from_json(&value, source_kind, source_id).unwrap_or_else(|| {
        verification_source_result(
            source_id,
            source_kind,
            "failed",
            Some(format!(
                "verification source {} did not contain a recognized procedure verification result",
                path.display()
            )),
            Vec::new(),
        )
    })
}

fn verification_result_from_json(
    value: &Value,
    source_kind: &str,
    source_id: &str,
) -> Option<VerificationSourceResult> {
    if let Some(result) = procedure_report_source_result(value, source_kind, source_id) {
        return Some(result);
    }
    if source_kind == "eval_fixture"
        && value.get("schema").and_then(Value::as_str)
            == Some(crate::eval::runner::EVAL_FIXTURE_SCHEMA_V1)
    {
        return Some(eval_fixture_source_result(value, source_id));
    }
    if let Some(fixtures) = value.get("fixtures").and_then(Value::as_array) {
        for fixture in fixtures {
            let id_matches = fixture.get("id").and_then(Value::as_str) == Some(source_id);
            let command_matches = fixture
                .get("eeCommands")
                .and_then(Value::as_array)
                .is_some_and(|commands| {
                    commands.iter().filter_map(Value::as_str).any(|command| {
                        command.contains(&format!("--source {source_id}"))
                            || command.contains(&format!("--source={source_id}"))
                    })
                });
            if id_matches || command_matches {
                return Some(status_object_source_result(fixture, source_kind, source_id));
            }
        }
    }
    status_object_source_result_opt(value, source_kind, source_id)
}

fn procedure_report_source_result(
    value: &Value,
    source_kind: &str,
    source_id: &str,
) -> Option<VerificationSourceResult> {
    let sources = value
        .get("sources_checked")
        .or_else(|| value.get("sourcesChecked"))
        .and_then(Value::as_array)?;
    let source = sources
        .iter()
        .find(|source| {
            source
                .get("source_id")
                .or_else(|| source.get("sourceId"))
                .and_then(Value::as_str)
                == Some(source_id)
        })
        .or_else(|| (sources.len() == 1).then(|| &sources[0]))?;
    status_object_source_result_opt(source, source_kind, source_id)
}

fn eval_fixture_source_result(value: &Value, source_id: &str) -> VerificationSourceResult {
    let fixture_id = value
        .get("fixture_id")
        .or_else(|| value.get("fixtureId"))
        .and_then(Value::as_str)
        .unwrap_or(source_id);
    let mut step_results = Vec::new();
    let mut failed = fixture_id != source_id;
    if let Some(commands) = value.get("command_sequence").and_then(Value::as_array) {
        for (index, command) in commands.iter().enumerate() {
            let sequence = command
                .get("step")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or_else(|| usize_to_u32_saturating(index + 1));
            let stdout_schema = command
                .get("stdout_schema")
                .and_then(Value::as_str)
                .unwrap_or("unspecified");
            let expected_exit_code = command
                .get("expected_exit_code")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            step_results.push(StepVerificationResult {
                step_id: format!("eval_fixture_{fixture_id}_{sequence:03}"),
                sequence,
                result: "passed".to_owned(),
                expected: Some(format!(
                    "declared expected exit {expected_exit_code} and stdout schema {stdout_schema}"
                )),
                actual: Some("eval fixture command declaration parsed".to_owned()),
            });
        }
    } else {
        failed = true;
    }
    let implemented = value
        .get("coverage_state")
        .or_else(|| value.get("coverageState"))
        .and_then(Value::as_str)
        .is_none_or(|state| state == "implemented");
    if !implemented {
        failed = true;
    }
    let result = if failed { "failed" } else { "passed" };
    verification_source_result(
        source_id,
        "eval_fixture",
        result,
        Some(format!("inspected eval fixture {fixture_id}")),
        step_results,
    )
}

fn status_object_source_result(
    value: &Value,
    source_kind: &str,
    source_id: &str,
) -> VerificationSourceResult {
    status_object_source_result_opt(value, source_kind, source_id).unwrap_or_else(|| {
        verification_source_result(
            source_id,
            source_kind,
            "failed",
            Some("verification source did not include a result or verification status".to_owned()),
            Vec::new(),
        )
    })
}

fn status_object_source_result_opt(
    value: &Value,
    source_kind: &str,
    source_id: &str,
) -> Option<VerificationSourceResult> {
    let result = value
        .get("result")
        .or_else(|| value.get("status"))
        .or_else(|| value.get("verificationStatus"))
        .and_then(Value::as_str)
        .and_then(normalize_verification_result)?;
    let step_results = json_step_results(value);
    let message = value
        .get("message")
        .or_else(|| value.get("firstFailureDiagnosis"))
        .or_else(|| value.get("description"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let source_kind = value
        .get("source_kind")
        .or_else(|| value.get("sourceKind"))
        .and_then(Value::as_str)
        .unwrap_or(source_kind);
    let source_id = value
        .get("source_id")
        .or_else(|| value.get("sourceId"))
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .unwrap_or(source_id);
    Some(verification_source_result(
        source_id,
        source_kind,
        result,
        message,
        step_results,
    ))
}

fn json_step_results(value: &Value) -> Vec<StepVerificationResult> {
    let Some(steps) = value
        .get("step_results")
        .or_else(|| value.get("stepResults"))
        .or_else(|| value.get("steps"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let sequence = step
                .get("sequence")
                .or_else(|| step.get("step"))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or_else(|| usize_to_u32_saturating(index + 1));
            StepVerificationResult {
                step_id: step
                    .get("step_id")
                    .or_else(|| step.get("stepId"))
                    .or_else(|| step.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("step_{sequence:03}")),
                sequence,
                result: step
                    .get("result")
                    .or_else(|| step.get("status"))
                    .and_then(Value::as_str)
                    .and_then(normalize_verification_result)
                    .unwrap_or("failed")
                    .to_owned(),
                expected: step
                    .get("expected")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                actual: step
                    .get("actual")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }
        })
        .collect()
}

fn normalize_verification_result(value: &str) -> Option<&'static str> {
    match value.trim().replace('-', "_").as_str() {
        "passed" | "pass" | "success" | "succeeded" | "ok" => Some("passed"),
        "failed" | "fail" | "failure" | "error" | "blocked" | "invalid" => Some("failed"),
        "skipped" | "skip" | "missing" | "stale" | "pending" | "not_run" => Some("skipped"),
        _ => None,
    }
}

fn inspect_repro_pack_dir(path: &Path, source_id: &str) -> VerificationSourceResult {
    if let Err(error) = ensure_no_procedure_path_symlink_components(path, "inspect repro pack") {
        return procedure_source_path_failure(path, "repro_pack", source_id, error);
    }
    let required = ["env.json", "manifest.json", "repro.lock", "provenance.json"];
    let mut step_results = Vec::new();
    let mut failed = false;
    for (index, file_name) in required.iter().enumerate() {
        let file_path = path.join(file_name);
        let result = match procedure_path_is_file(&file_path) {
            Ok(true) if valid_json_file(&file_path) => "passed",
            Ok(_) => {
                failed = true;
                "failed"
            }
            Err(error) => {
                failed = true;
                step_results.push(StepVerificationResult {
                    step_id: format!("repro_pack_{source_id}_{file_name}"),
                    sequence: usize_to_u32_saturating(index + 1),
                    result: "failed".to_owned(),
                    expected: Some(format!("{file_name} exists and parses as JSON")),
                    actual: Some(format!("{}: {error}", file_path.display())),
                });
                continue;
            }
        };
        step_results.push(StepVerificationResult {
            step_id: format!("repro_pack_{source_id}_{file_name}"),
            sequence: usize_to_u32_saturating(index + 1),
            result: result.to_owned(),
            expected: Some(format!("{file_name} exists and parses as JSON")),
            actual: Some(file_path.display().to_string()),
        });
    }
    verification_source_result(
        source_id,
        "repro_pack",
        if failed { "failed" } else { "passed" },
        Some(format!("inspected repro pack {}", path.display())),
        step_results,
    )
}

fn valid_json_file(path: &Path) -> bool {
    let Ok(true) = procedure_path_is_file(path) else {
        return false;
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .is_some()
}

fn verification_source_candidate_paths(
    workspace: &Path,
    source_kind: &str,
    source_id: &str,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let source_path = Path::new(source_id);
    if source_path.is_absolute() {
        push_source_path_candidates(&mut paths, source_path.to_path_buf());
    }
    for root in source_search_roots(workspace) {
        push_source_path_candidates(&mut paths, root.join(source_id));
        push_source_path_candidates(
            &mut paths,
            root.join(".ee")
                .join("procedure-verification")
                .join(source_kind)
                .join(source_id),
        );
        push_source_path_candidates(
            &mut paths,
            root.join(".ee")
                .join("procedure-verification")
                .join(source_id),
        );
        push_source_path_candidates(
            &mut paths,
            root.join("tests")
                .join("fixtures")
                .join("procedure")
                .join(source_kind)
                .join(source_id),
        );
        push_source_path_candidates(
            &mut paths,
            root.join("tests")
                .join("fixtures")
                .join("procedure")
                .join(source_id),
        );
    }
    paths.sort();
    paths.dedup();
    paths
}

fn push_source_path_candidates(paths: &mut Vec<PathBuf>, path: PathBuf) {
    paths.push(path.clone());
    if path.extension().is_none() {
        paths.push(path.with_extension("json"));
    }
}

fn source_search_roots(workspace: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if !workspace.as_os_str().is_empty() {
        if let Ok(workspace) = resolve_workspace_path(workspace) {
            roots.push(workspace);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    roots.sort();
    roots.dedup();
    roots
}

fn find_eval_fixture_scenario(root: &Path, source_id: &str) -> Option<PathBuf> {
    let fixture_root = root.join("tests").join("fixtures").join("eval");
    let entries = fs::read_dir(fixture_root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path().join("scenario.json");
        let Ok(is_file) = procedure_path_is_file(&path) else {
            continue;
        };
        if !is_file {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&content) else {
            continue;
        };
        if value.get("fixture_id").and_then(Value::as_str) == Some(source_id)
            || value
                .get("scenario_ids")
                .and_then(Value::as_array)
                .is_some_and(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .any(|id| id == source_id)
                })
        {
            return Some(path);
        }
    }
    None
}

fn procedure_source_path_failure(
    path: &Path,
    source_kind: &str,
    source_id: &str,
    error: std::io::Error,
) -> VerificationSourceResult {
    verification_source_result(
        source_id,
        source_kind,
        "failed",
        Some(format!(
            "failed to inspect verification source {}: {error}",
            path.display()
        )),
        Vec::new(),
    )
}

fn procedure_path_is_file(path: &Path) -> Result<bool, std::io::Error> {
    ensure_no_procedure_path_symlink_components(path, "inspect verification source")?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_file()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn procedure_path_is_dir(path: &Path) -> Result<bool, std::io::Error> {
    ensure_no_procedure_path_symlink_components(path, "inspect verification source")?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_dir()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn ensure_no_procedure_path_symlink_components(
    path: &Path,
    operation: &'static str,
) -> Result<(), std::io::Error> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing to {operation} `{}` through symlinked path component `{}`",
                        path.display(),
                        current.display()
                    ),
                ));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn verification_source_result(
    source_id: &str,
    source_kind: &str,
    result: &str,
    message: Option<String>,
    step_results: Vec<StepVerificationResult>,
) -> VerificationSourceResult {
    VerificationSourceResult {
        source_id: source_id.to_owned(),
        source_kind: source_kind.to_owned(),
        result: result.to_owned(),
        step_results,
        message,
    }
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
        crate::core::serialize_or_error(self)
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

struct ProcedureStore {
    connection: DbConnection,
    workspace_id: String,
}

fn open_procedure_store(workspace: &Path) -> Result<Option<ProcedureStore>, DomainError> {
    open_procedure_store_with_workspace_mode(workspace, false)
}

fn open_writable_procedure_store(
    workspace: &Path,
    missing_database_message: &'static str,
) -> Result<ProcedureStore, DomainError> {
    let Some(store) = open_procedure_store_with_workspace_mode(workspace, true)? else {
        return Err(DomainError::Storage {
            message: missing_database_message.to_owned(),
            repair: Some("ee init --workspace .".to_owned()),
        });
    };
    Ok(store)
}

fn open_procedure_store_with_workspace_mode(
    workspace: &Path,
    create_workspace_row: bool,
) -> Result<Option<ProcedureStore>, DomainError> {
    if workspace.as_os_str().is_empty() {
        return Ok(None);
    }
    let workspace_path = resolve_workspace_path(workspace)?;
    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.is_file() {
        return Ok(None);
    }
    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("failed to open procedure database: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    connection.migrate().map_err(|error| DomainError::Storage {
        message: format!("failed to migrate procedure database: {error}"),
        repair: Some("ee init --workspace . --json".to_owned()),
    })?;
    let workspace_key = workspace_path.display().to_string();
    let workspace = connection
        .get_workspace_by_path(&workspace_key)
        .map_err(storage_error("failed to load workspace row"))?;
    let workspace_id = if let Some(workspace) = workspace {
        workspace.id
    } else if create_workspace_row {
        ensure_procedure_workspace(&connection, &workspace_path)?
    } else {
        return Ok(None);
    };
    Ok(Some(ProcedureStore {
        connection,
        workspace_id,
    }))
}

fn ensure_procedure_workspace(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, DomainError> {
    let workspace_key = workspace_path.display().to_string();
    let workspace_id = stable_workspace_id(workspace_path);
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace_key,
                name: workspace_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("failed to register procedure workspace: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    Ok(workspace_id)
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn resolve_workspace_path(workspace: &Path) -> Result<PathBuf, DomainError> {
    if workspace.is_absolute() {
        Ok(workspace.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(workspace))
            .map_err(|error| DomainError::Configuration {
                message: format!("failed to resolve current directory: {error}"),
                repair: Some("pass --workspace with an absolute path".to_owned()),
            })
    }
}

fn load_procedure_records(
    workspace: &Path,
    maturity: Option<&str>,
    limit: u32,
) -> Result<Vec<ProcedureRecord>, DomainError> {
    let Some(store) = open_procedure_store(workspace)? else {
        return Ok(Vec::new());
    };
    let procedures = store
        .connection
        .list_procedure_records(&store.workspace_id, maturity, list_limit(limit))
        .map_err(storage_error("failed to list procedures"))?;
    procedures
        .into_iter()
        .map(|procedure| {
            let events = store
                .connection
                .list_procedure_events(&procedure.id)
                .map_err(storage_error("failed to list procedure history"))?;
            Ok(procedure_record_from_stored(procedure, events))
        })
        .collect()
}

fn procedure_record_from_stored(
    procedure: StoredProcedure,
    events: Vec<StoredProcedureEvent>,
) -> ProcedureRecord {
    let steps = procedure_steps_from_body(&procedure);
    let verification = VerificationDetail {
        status: if procedure.last_validated_at.is_some() {
            ProcedureVerificationStatus::Passed.as_str().to_owned()
        } else {
            ProcedureVerificationStatus::Pending.as_str().to_owned()
        },
        verified_at: procedure.last_validated_at.clone(),
        verified_by: None,
        pass_count: if procedure.last_validated_at.is_some() {
            usize_to_u32_saturating(procedure.evidence_uris.len())
        } else {
            0
        },
        fail_count: procedure.harmful_count,
    };
    ProcedureRecord::new(ProcedureDetail {
        procedure_id: procedure.id.clone(),
        title: procedure.name,
        summary: procedure.body,
        status: procedure.maturity,
        step_count: usize_to_u32_saturating(steps.len()),
        source_run_ids: procedure_source_runs(&procedure.evidence_uris),
        evidence_ids: procedure.evidence_uris,
        created_at: procedure.created_at,
        updated_at: procedure.updated_at,
        verified_at: procedure.last_validated_at,
    })
    .with_steps(steps)
    .with_verification(verification)
    .with_history(
        events
            .into_iter()
            .map(procedure_history_from_stored)
            .collect(),
    )
}

fn procedure_steps_from_body(procedure: &StoredProcedure) -> Vec<ProcedureStepDetail> {
    let lines: Vec<_> = procedure
        .body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() <= 1 {
        return vec![ProcedureStepDetail {
            step_id: format!("step_{}_001", procedure.id),
            sequence: 1,
            title: procedure.name.clone(),
            instruction: procedure.body.clone(),
            command_hint: None,
            required: true,
        }];
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let sequence = usize_to_u32_saturating(index + 1);
            ProcedureStepDetail {
                step_id: format!("step_{}_{sequence:03}", procedure.id),
                sequence,
                title: format!("Step {sequence}"),
                instruction: line
                    .trim_start_matches(|ch: char| {
                        ch.is_ascii_digit() || matches!(ch, '.' | ')' | '-' | '*')
                    })
                    .trim()
                    .to_owned(),
                command_hint: None,
                required: true,
            }
        })
        .collect()
}

fn procedure_history_from_stored(event: StoredProcedureEvent) -> ProcedureHistoryEvent {
    ProcedureHistoryEvent {
        event_id: event.id,
        event_type: event.event_type,
        from_maturity: event.from_maturity,
        to_maturity: event.to_maturity,
        reason: event.reason,
        evidence_uris: event.evidence_uris,
        actor: event.actor,
        created_at: event.created_at,
    }
}

fn procedure_evidence_uris(source_run_ids: &[String], evidence_ids: &[String]) -> Vec<String> {
    let mut uris: Vec<String> = source_run_ids
        .iter()
        .map(|id| format!("cass-run://{}", id.trim()))
        .collect();
    uris.extend(
        evidence_ids
            .iter()
            .map(|id| format!("evidence://{}", id.trim())),
    );
    uris.retain(|uri| !uri.ends_with("://"));
    uris.sort();
    uris.dedup();
    uris
}

fn procedure_source_runs(evidence_uris: &[String]) -> Vec<String> {
    evidence_uris
        .iter()
        .filter_map(|uri| uri.strip_prefix("cass-run://").map(str::to_owned))
        .collect()
}

fn initial_procedure_confidence(evidence_count: usize) -> f32 {
    (0.45 + 0.05 * evidence_count as f32).clamp(0.45, 0.75)
}

fn list_limit(limit: u32) -> u32 {
    if limit == 0 { 20 } else { limit }
}

fn storage_error(
    context: &'static str,
) -> impl Fn(crate::db::DbError) -> DomainError + Copy + 'static {
    move |error| DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some("ee doctor --json".to_owned()),
    }
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
    uuid::Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        CreateEvidenceSpanInput, CreateRecorderRunInput, CreateSessionInput, CreateWorkspaceInput,
        DbConnection,
    };
    use std::fs;

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

    fn procedure_store_workspace() -> Result<PathBuf, String> {
        let mut payload = uuid::Uuid::now_v7().simple().to_string();
        payload.truncate(26);
        let workspace_id = format!("wsp_{payload}");
        let workspace = std::env::temp_dir().join(format!("ee-procedure-hssh-{payload}"));
        let ee_dir = workspace.join(".ee");
        fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        let connection =
            DbConnection::open_file(ee_dir.join("ee.db")).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("procedure hssh test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(workspace)
    }

    fn procedure_store_connection(workspace: &Path) -> Result<(DbConnection, String), String> {
        let connection = DbConnection::open_file(workspace.join(".ee").join("ee.db"))
            .map_err(|error| error.to_string())?;
        let workspace_row = connection
            .get_workspace_by_path(&workspace.display().to_string())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "workspace row missing".to_owned())?;
        Ok((connection, workspace_row.id))
    }

    fn insert_procedure_session(
        connection: &DbConnection,
        workspace_id: &str,
        session_id: &str,
    ) -> TestResult {
        connection
            .insert_session(
                session_id,
                &CreateSessionInput {
                    workspace_id: workspace_id.to_owned(),
                    cass_session_id: format!("cass-{session_id}"),
                    source_path: Some(format!("cass://{session_id}")),
                    agent_name: Some("procedure-test".to_owned()),
                    model: None,
                    started_at: Some("2026-05-01T00:00:00Z".to_owned()),
                    ended_at: None,
                    message_count: 1,
                    token_count: Some(16),
                    content_hash: format!("blake3:{session_id}"),
                    metadata_json: Some(r#"{"fixture":"procedure"}"#.to_owned()),
                },
            )
            .map_err(|error| error.to_string())
    }

    #[test]
    fn propose_creates_candidate() -> TestResult {
        let options = ProcedureProposeOptions {
            title: "Test procedure".to_owned(),
            summary: Some("A test summary".to_owned()),
            source_run_ids: vec!["run_1".to_owned()],
            dry_run: true,
            ..Default::default()
        };

        let report = propose_procedure(&options).map_err(|e| e.message())?;
        assert!(report.procedure_id.starts_with("proc_"));
        assert_eq!(report.status, "candidate");
        assert_eq!(report.source_run_count, 1);
        assert!(report.dry_run);
        Ok(())
    }

    #[test]
    fn propose_without_dry_run_reports_missing_store() -> TestResult {
        let options = ProcedureProposeOptions {
            title: "Test procedure".to_owned(),
            source_run_ids: vec!["run_1".to_owned()],
            dry_run: false,
            ..Default::default()
        };

        let Err(error) = propose_procedure(&options) else {
            return Err("non-dry-run proposal should require a procedure store".to_owned());
        };
        assert_eq!(error.code(), "storage");
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
    fn persisted_store_promotes_and_retires_with_history() -> TestResult {
        let workspace = procedure_store_workspace()?;
        let proposal = propose_procedure(&ProcedureProposeOptions {
            workspace: workspace.clone(),
            title: "Run release verification".to_owned(),
            summary: Some("1. Check formatting\n2. Run clippy through RCH".to_owned()),
            source_run_ids: vec!["run_release_001".to_owned()],
            evidence_ids: vec!["ev_release_log".to_owned()],
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        assert_eq!(proposal.status, "provisional");

        let listed = list_procedures(&ProcedureListOptions {
            workspace: workspace.clone(),
            status_filter: Some("provisional".to_owned()),
            limit: 10,
            include_steps: false,
        })
        .map_err(|error| error.message())?;
        assert_eq!(listed.filtered_count, 1);
        assert_eq!(listed.procedures[0].procedure_id, proposal.procedure_id);

        let promoted = promote_procedure(&ProcedurePromoteOptions {
            workspace: workspace.clone(),
            procedure_id: proposal.procedure_id.clone(),
            to_maturity: Some("validated".to_owned()),
            dry_run: false,
            actor: Some("BronzeTurtle".to_owned()),
            reason: Some("evidence threshold satisfied".to_owned()),
        })
        .map_err(|error| error.message())?;
        assert_eq!(promoted.status, "promoted");
        assert_eq!(promoted.to_status, "validated");
        assert!(promoted.audit.recorded);

        let retired = retire_procedure(&ProcedureRetireOptions {
            workspace: workspace.clone(),
            procedure_id: proposal.procedure_id.clone(),
            reason: "procedure drifted".to_owned(),
            actor: Some("BronzeTurtle".to_owned()),
        })
        .map_err(|error| error.message())?;
        assert_eq!(retired.status, "retired");

        let show = show_procedure(&ProcedureShowOptions {
            workspace,
            procedure_id: proposal.procedure_id,
            include_steps: true,
            include_verification: true,
        })
        .map_err(|error| error.message())?;
        assert_eq!(show.procedure.status, "retired");
        assert_eq!(show.steps.len(), 2);
        assert!(
            show.history
                .iter()
                .any(|event| event.event_type == "created")
        );
        assert!(
            show.history
                .iter()
                .any(|event| event.event_type == "promoted")
        );
        assert!(
            show.history
                .iter()
                .any(|event| event.event_type == "retired")
        );
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
        assert_eq!(report.redaction_status, "not_required");
        Ok(())
    }

    #[test]
    fn export_public_artifacts_redact_sensitive_evidence_refs() -> TestResult {
        let mut record = procedure_record("candidate");
        let source_ref =
            "/Users/alice/private/cass/session.jsonl?api_key=sk-FAKEabc123def456ghi789";
        let evidence_ref =
            "evidence:///Volumes/Secret/procedure/proof.json?token=ghp_FAKEabc123def456ghi7890";
        record.procedure.source_run_ids = vec![source_ref.to_owned()];
        record.procedure.evidence_ids = vec![evidence_ref.to_owned()];

        for format in ["markdown", "playbook", "json", "skill-capsule"] {
            let report = export_procedure_from_records(
                &ProcedureExportOptions {
                    procedure_id: "proc_test".to_owned(),
                    format: format.to_owned(),
                    ..Default::default()
                },
                &[record.clone()],
            )
            .map_err(|e| e.message())?;

            assert_eq!(report.redaction_status, "standard", "{format}");
            assert!(report.content.contains("[REDACTED_PATH]"), "{format}");
            assert!(report.content.contains("[REDACTED:"), "{format}");
            assert!(!report.content.contains(source_ref), "{format}");
            assert!(!report.content.contains(evidence_ref), "{format}");
            assert!(!report.content.contains("/Users/alice"), "{format}");
            assert!(!report.content.contains("/Volumes/Secret"), "{format}");
            assert!(!report.content.contains("sk-FAKE"), "{format}");
            assert!(!report.content.contains("ghp_FAKE"), "{format}");
        }

        assert_eq!(record.procedure.source_run_ids, vec![source_ref.to_owned()]);
        assert_eq!(record.procedure.evidence_ids, vec![evidence_ref.to_owned()]);
        Ok(())
    }

    #[test]
    fn export_markdown_and_skill_capsule_escape_adversarial_content() -> TestResult {
        let mut record = procedure_record("candidate");
        record.procedure.title =
            "Stored [title-link](javascript:alert(1)) <strong>html</strong>".to_owned();
        record.procedure.summary =
            "Summary with [summary-link](javascript:alert(2)) <script>bad()</script>".to_owned();
        record.procedure.source_run_ids =
            vec!["run`1 [source-link](javascript:alert(3))".to_owned()];
        record.steps[0].title = "Prepare `unsafe` [step-link](javascript:alert(4))".to_owned();
        record.steps[0].instruction =
            "Do not break out:\n```\n# injected\n```\n<iframe src=x>".to_owned();
        record.steps[0].command_hint = Some("echo `safe`\n--flag".to_owned());

        let markdown_options = ProcedureExportOptions {
            procedure_id: "proc_test".to_owned(),
            format: "markdown".to_owned(),
            ..Default::default()
        };
        let markdown_report = export_procedure_from_records(&markdown_options, &[record.clone()])
            .map_err(|e| e.message())?;

        // Bead bd-17c65.8.1 (H1) — spec-minimal escapes. Brackets and
        // backticks still escape (the security boundary); HTML chars
        // still become entities. Mid-text `-`, `(`, `)` no longer
        // escape because they aren't markdown syntax outside link
        // destinations.
        assert!(markdown_report.content.contains(
            "# Stored \\[title-link\\](javascript:alert(1)) &lt;strong&gt;html&lt;/strong&gt;"
        ));
        assert!(markdown_report.content.contains(
            "Summary with \\[summary-link\\](javascript:alert(2)) &lt;script&gt;bad()&lt;/script&gt;"
        ));
        assert!(
            markdown_report
                .content
                .contains("Prepare \\`unsafe\\` \\[step-link\\](javascript:alert(4))")
        );
        assert!(markdown_report.content.contains("\\`\\`\\`"));
        assert!(
            markdown_report
                .content
                .contains("Command: ``echo `safe` --flag``")
        );
        assert!(
            !markdown_report.content.contains("[title-link](javascript")
                && !markdown_report
                    .content
                    .contains("[summary-link](javascript")
                && !markdown_report.content.contains("[step-link](javascript")
                && !markdown_report.content.contains("<script>bad()</script>")
                && !markdown_report.content.contains("<iframe src=x>")
        );

        let capsule_options = ProcedureExportOptions {
            procedure_id: "proc_test".to_owned(),
            format: "skill-capsule".to_owned(),
            ..Default::default()
        };
        let capsule_report =
            export_procedure_from_records(&capsule_options, &[record]).map_err(|e| e.message())?;

        // H1: brackets escape; `-`, `(`, `)` mid-text do not.
        assert!(
            capsule_report
                .content
                .contains("\\[title-link\\](javascript:alert(1))")
        );
        assert!(
            !capsule_report.content.contains("[title-link](javascript")
                && !capsule_report.content.contains("[step-link](javascript")
                && !capsule_report.content.contains("<iframe src=x>")
        );
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

    #[cfg(unix)]
    #[test]
    fn export_rejects_symlinked_output_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_output_dir = temp.path().join("real-output");
        fs::create_dir_all(&real_output_dir).map_err(|error| error.to_string())?;
        symlink(&real_output_dir, temp.path().join("linked-output"))
            .map_err(|error| error.to_string())?;
        let options = ProcedureExportOptions {
            workspace: PathBuf::new(),
            procedure_id: "proc_test".to_owned(),
            format: "markdown".to_owned(),
            output_path: Some(temp.path().join("linked-output").join("procedure.md")),
        };

        let error = export_procedure_from_records(&options, &[procedure_record("candidate")])
            .expect_err("symlinked output parent should reject export");
        assert_eq!(error.code(), "storage");
        assert!(
            error.message().contains("symlinked path component"),
            "unexpected symlink error: {}",
            error.message()
        );
        assert!(
            !real_output_dir.join("procedure.md").exists(),
            "export must not write through symlinked output parent"
        );
        Ok(())
    }

    #[test]
    fn export_rejects_non_regular_output_path() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let output_path = temp.path().join("procedure.md");
        fs::create_dir(&output_path).map_err(|error| error.to_string())?;
        let options = ProcedureExportOptions {
            workspace: PathBuf::new(),
            procedure_id: "proc_test".to_owned(),
            format: "markdown".to_owned(),
            output_path: Some(output_path.clone()),
        };

        let error = export_procedure_from_records(&options, &[procedure_record("candidate")])
            .expect_err("non-regular output path should reject export");
        assert_eq!(error.code(), "policy_denied");
        assert!(
            error.message().contains("not a regular file"),
            "unexpected non-regular output error: {}",
            error.message()
        );
        assert!(
            output_path.is_dir(),
            "export must leave the non-regular output path untouched"
        );
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
            return Err("non-dry-run promotion should require a store".to_owned());
        };
        assert_eq!(error.code(), "storage");
        assert!(error.repair().unwrap_or_default().contains("ee init"));
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
    fn verify_fails_missing_named_sources() -> TestResult {
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
        assert!(report.sources_checked.iter().all(|s| s.result == "failed"));
        assert_eq!(report.pass_count, 0);
        assert_eq!(report.fail_count, 2);
        assert_eq!(report.overall_result, "failed");
        Ok(())
    }

    #[test]
    fn verify_inspects_repository_eval_fixture_by_id() -> TestResult {
        let options = ProcedureVerifyOptions {
            workspace: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("eval_fixture".to_owned()),
            source_ids: vec!["fx.release_failure.v1".to_owned()],
            dry_run: true,
            ..Default::default()
        };

        let report = verify_procedure(&options).map_err(|error| error.message())?;
        assert_eq!(report.overall_result, "passed");
        assert_eq!(report.pass_count, 1);
        assert_eq!(report.fail_count, 0);
        assert_eq!(report.sources_checked[0].source_id, "fx.release_failure.v1");
        assert_eq!(report.sources_checked[0].result, "passed");
        assert!(!report.sources_checked[0].step_results.is_empty());
        Ok(())
    }

    #[test]
    fn verify_fails_malformed_named_eval_fixture_file() -> TestResult {
        let workspace = procedure_store_workspace()?;
        let fixture_dir = workspace
            .join(".ee")
            .join("procedure-verification")
            .join("eval_fixture");
        fs::create_dir_all(&fixture_dir).map_err(|error| error.to_string())?;
        fs::write(
            fixture_dir.join("fixture_bad.json"),
            r#"{"schema":"ee.eval_fixture.v1","fixture_id":"different_fixture","coverage_state":"implemented"}"#,
        )
        .map_err(|error| error.to_string())?;

        let report = verify_procedure(&ProcedureVerifyOptions {
            workspace,
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("eval_fixture".to_owned()),
            source_ids: vec!["fixture_bad".to_owned()],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.overall_result, "failed");
        assert_eq!(report.fail_count, 1);
        assert_eq!(report.sources_checked[0].result, "failed");
        assert_eq!(
            report.sources_checked[0].message.as_deref(),
            Some("inspected eval fixture different_fixture")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn verify_rejects_symlinked_named_eval_fixture_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let workspace = procedure_store_workspace()?;
        let fixture_dir = workspace
            .join(".ee")
            .join("procedure-verification")
            .join("eval_fixture");
        fs::create_dir_all(&fixture_dir).map_err(|error| error.to_string())?;
        let outside_fixture = workspace.join("outside-fixture.json");
        fs::write(
            &outside_fixture,
            r#"{"schema":"ee.eval_fixture.v1","fixture_id":"fixture_link","coverage_state":"implemented","command_sequence":[{"step":1,"expected_exit_code":0,"stdout_schema":"ee.response.v2"}]}"#,
        )
        .map_err(|error| error.to_string())?;
        symlink(&outside_fixture, fixture_dir.join("fixture_link.json"))
            .map_err(|error| error.to_string())?;

        let report = verify_procedure(&ProcedureVerifyOptions {
            workspace,
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("eval_fixture".to_owned()),
            source_ids: vec!["fixture_link".to_owned()],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.overall_result, "failed");
        assert_eq!(report.fail_count, 1);
        assert_eq!(report.sources_checked[0].result, "failed");
        assert!(
            report.sources_checked[0]
                .message
                .as_deref()
                .is_some_and(|message| message.contains("symlinked path component")),
            "expected symlink failure, got {:?}",
            report.sources_checked[0].message
        );
        Ok(())
    }

    #[test]
    fn verify_rejects_non_regular_named_eval_fixture_file() -> TestResult {
        let workspace = procedure_store_workspace()?;
        let fixture_dir = workspace
            .join(".ee")
            .join("procedure-verification")
            .join("eval_fixture");
        fs::create_dir_all(fixture_dir.join("fixture_dir.json"))
            .map_err(|error| error.to_string())?;

        let report = verify_procedure(&ProcedureVerifyOptions {
            workspace,
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("eval_fixture".to_owned()),
            source_ids: vec!["fixture_dir".to_owned()],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.overall_result, "failed");
        assert_eq!(report.fail_count, 1);
        assert_eq!(report.sources_checked[0].result, "failed");
        assert!(
            report.sources_checked[0]
                .message
                .as_deref()
                .is_some_and(|message| message.contains("not a regular file")),
            "expected regular-file failure, got {:?}",
            report.sources_checked[0].message
        );
        Ok(())
    }

    #[test]
    fn verify_inspects_persisted_recorder_run() -> TestResult {
        let workspace = procedure_store_workspace()?;
        let (connection, workspace_id) = procedure_store_connection(&workspace)?;
        connection
            .insert_recorder_run(
                "run_verify_named_001",
                &CreateRecorderRunInput {
                    workspace_id: Some(workspace_id),
                    agent_id: "agent_verify".to_owned(),
                    session_id: Some("sess_verify".to_owned()),
                    source_type: "synthetic".to_owned(),
                    source_id: Some("fixture://procedure-verify".to_owned()),
                    status: "completed".to_owned(),
                    started_at: "2026-05-01T00:00:00Z".to_owned(),
                    ended_at: Some("2026-05-01T00:01:00Z".to_owned()),
                    event_count: 2,
                    redacted_count: 0,
                    payload_bytes: 128,
                    chain_complete: true,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = verify_procedure(&ProcedureVerifyOptions {
            workspace,
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("recorder_run".to_owned()),
            source_ids: vec!["run_verify_named_001".to_owned()],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.overall_result, "passed");
        assert_eq!(report.pass_count, 1);
        assert_eq!(report.sources_checked[0].source_kind, "recorder_run");
        assert_eq!(report.sources_checked[0].step_results.len(), 3);
        Ok(())
    }

    #[test]
    fn verify_inspects_persisted_claim_evidence() -> TestResult {
        let workspace = procedure_store_workspace()?;
        let (connection, workspace_id) = procedure_store_connection(&workspace)?;
        let session_id = "sess_61234567890123456789012345";
        insert_procedure_session(&connection, &workspace_id, session_id)?;
        connection
            .insert_evidence_span(
                "ev_61234567890123456789012345",
                &CreateEvidenceSpanInput {
                    workspace_id,
                    session_id: session_id.to_owned(),
                    memory_id: None,
                    cass_span_id: "span_verify_claim".to_owned(),
                    span_kind: "summary".to_owned(),
                    start_line: 10,
                    end_line: 12,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: "The verification command passed against real evidence.".to_owned(),
                    content_hash: "blake3:claimverify".to_owned(),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = verify_procedure(&ProcedureVerifyOptions {
            workspace,
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("claim_evidence".to_owned()),
            source_ids: vec!["ev_61234567890123456789012345".to_owned()],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.overall_result, "passed");
        assert_eq!(report.pass_count, 1);
        assert_eq!(report.sources_checked[0].source_kind, "claim_evidence");
        Ok(())
    }

    #[test]
    fn verify_missing_persisted_sources_remain_missing() -> TestResult {
        let workspace = procedure_store_workspace()?;

        let claim_report = verify_procedure(&ProcedureVerifyOptions {
            workspace: workspace.clone(),
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("claim_evidence".to_owned()),
            source_ids: vec!["ev_61234567890123456789054321".to_owned()],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;
        assert_eq!(claim_report.overall_result, "failed");
        assert_eq!(
            claim_report.sources_checked[0].message.as_deref(),
            Some("named claim evidence was not found in persisted evidence spans")
        );
        assert!(claim_report.sources_checked[0].step_results.is_empty());

        let recorder_report = verify_procedure(&ProcedureVerifyOptions {
            workspace,
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("recorder_run".to_owned()),
            source_ids: vec!["run_verify_missing_001".to_owned()],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;
        assert_eq!(recorder_report.overall_result, "failed");
        assert_eq!(
            recorder_report.sources_checked[0].message.as_deref(),
            Some("named recorder run was not found in persisted recorder runs")
        );
        assert!(recorder_report.sources_checked[0].step_results.is_empty());
        Ok(())
    }

    #[test]
    fn verify_persisted_claim_evidence_query_error_is_failed_source() -> TestResult {
        let workspace = procedure_store_workspace()?;
        let (connection, workspace_id) = procedure_store_connection(&workspace)?;
        let session_id = "sess_71234567890123456789012345";
        insert_procedure_session(&connection, &workspace_id, session_id)?;
        connection
            .insert_evidence_span(
                "ev_61234567890123456789067890",
                &CreateEvidenceSpanInput {
                    workspace_id,
                    session_id: session_id.to_owned(),
                    memory_id: None,
                    cass_span_id: "span_verify_claim_query_error".to_owned(),
                    span_kind: "summary".to_owned(),
                    start_line: 1,
                    end_line: 2,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: "Evidence exists before the table is made unreadable.".to_owned(),
                    content_hash: "blake3:claimqueryerror".to_owned(),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw("ALTER TABLE evidence_spans RENAME TO evidence_spans_unreadable")
            .map_err(|error| error.to_string())?;

        let report = verify_procedure(&ProcedureVerifyOptions {
            workspace,
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("claim_evidence".to_owned()),
            source_ids: vec!["ev_61234567890123456789067890".to_owned()],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.overall_result, "failed");
        assert_eq!(report.fail_count, 1);
        let source = &report.sources_checked[0];
        assert_eq!(source.source_kind, "claim_evidence");
        let message = source.message.as_deref().unwrap_or_default();
        assert!(message.contains("failed to load persisted evidence span"));
        assert!(!message.contains("not found"));
        assert_eq!(source.step_results.len(), 1);
        assert_eq!(
            source.step_results[0].expected.as_deref(),
            Some("persisted claim_evidence source is readable")
        );
        Ok(())
    }

    #[test]
    fn verify_persisted_recorder_run_query_error_is_failed_source() -> TestResult {
        let workspace = procedure_store_workspace()?;
        let (connection, workspace_id) = procedure_store_connection(&workspace)?;
        connection
            .insert_recorder_run(
                "run_verify_query_error_001",
                &CreateRecorderRunInput {
                    workspace_id: Some(workspace_id),
                    agent_id: "agent_verify".to_owned(),
                    session_id: Some("sess_verify_query_error".to_owned()),
                    source_type: "synthetic".to_owned(),
                    source_id: Some("fixture://procedure-verify-query-error".to_owned()),
                    status: "completed".to_owned(),
                    started_at: "2026-05-01T00:00:00Z".to_owned(),
                    ended_at: Some("2026-05-01T00:01:00Z".to_owned()),
                    event_count: 2,
                    redacted_count: 0,
                    payload_bytes: 128,
                    chain_complete: true,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw("ALTER TABLE recorder_runs RENAME TO recorder_runs_unreadable")
            .map_err(|error| error.to_string())?;

        let report = verify_procedure(&ProcedureVerifyOptions {
            workspace,
            procedure_id: "proc_test".to_owned(),
            source_kind: Some("recorder_run".to_owned()),
            source_ids: vec!["run_verify_query_error_001".to_owned()],
            dry_run: true,
            ..Default::default()
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.overall_result, "failed");
        assert_eq!(report.fail_count, 1);
        let source = &report.sources_checked[0];
        assert_eq!(source.source_kind, "recorder_run");
        let message = source.message.as_deref().unwrap_or_default();
        assert!(message.contains("failed to load persisted recorder run"));
        assert!(!message.contains("not found"));
        assert_eq!(source.step_results.len(), 1);
        assert_eq!(
            source.step_results[0].expected.as_deref(),
            Some("persisted recorder_run source is readable")
        );
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
