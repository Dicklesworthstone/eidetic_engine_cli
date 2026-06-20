use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{ArgAction, Parser, Subcommand};
use serde_json::{Value as JsonValue, json};

use crate::db::{CreateAuditInput, DbConnection, StoredMemory, generate_audit_id};
use crate::models::{DomainError, ProcessExitCode};
use crate::output;
use crate::policy::{
    SharePreviewCandidate, SharePreviewConsentAudit, SharePreviewInput, SharePreviewReport,
    build_share_preview, redact_secret_like_content, share_preview_consent_audit,
    share_preview_hash,
};

use super::{Cli, write_domain_error, write_stdout};

const SHARE_PREVIEW_COMMAND: &str = "share preview";
const SHARE_CONSENT_ACTION: &str = "mesh.share.consent";
const EMBEDDING_ESTIMATED_BYTES: u64 = 1536 * 4;

/// Subcommands for `ee share`.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum ShareCommand {
    /// Preview what would be shared with a peer without exporting anything.
    Preview(SharePreviewArgs),
}

/// Arguments for `ee share preview`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct SharePreviewArgs {
    /// Target peer identity or local peer alias.
    #[arg(long = "peer", value_name = "PEER_ID")]
    pub peer_id: String,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Only include memories at this level.
    #[arg(long, value_name = "LEVEL")]
    pub level: Option<String>,

    /// Include memory body lane in the allowed estimate.
    #[arg(long = "include-body", action = ArgAction::SetTrue)]
    pub include_body: bool,

    /// Include embedding lane in the allowed estimate.
    #[arg(long = "include-embeddings", action = ArgAction::SetTrue)]
    pub include_embeddings: bool,

    /// Maximum number of representative redacted examples.
    #[arg(long = "max-examples", default_value_t = 6)]
    pub max_examples: usize,

    /// Persist an audit row confirming the operator reviewed this preview.
    #[arg(long = "record-consent", action = ArgAction::SetTrue)]
    pub record_consent: bool,

    /// Actor recorded when --record-consent writes the audit row.
    #[arg(long, value_name = "ACTOR", default_value = "operator")]
    pub actor: String,

    /// Reason recorded when --record-consent writes the audit row.
    #[arg(
        long = "consent-reason",
        value_name = "REASON",
        default_value = "operator_preview_ack"
    )]
    pub consent_reason: String,
}

pub fn handle_share<W, E>(
    cli: &Cli,
    command: &ShareCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    match command {
        ShareCommand::Preview(args) => handle_share_preview(cli, args, stdout, stderr),
    }
}

fn handle_share_preview<W, E>(
    cli: &Cli,
    args: &SharePreviewArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let validated_args = match validate_share_preview_args(args) {
        Ok(validated_args) => validated_args,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };

    let workspace_path = cli.resolve_workspace();
    let database_path = args
        .database
        .clone()
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let connection = match DbConnection::open_file(&database_path) {
        Ok(connection) => connection,
        Err(error) => {
            let domain_error = storage_error("Failed to open share-preview database", error);
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
    };
    let workspace_id = match resolve_share_workspace_id(&connection, &workspace_path) {
        Ok(workspace_id) => workspace_id,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let memories = match connection.list_memories(&workspace_id, args.level.as_deref(), false) {
        Ok(memories) => memories,
        Err(error) => {
            let domain_error = storage_error("Failed to list share-preview memories", error);
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
    };
    let candidates =
        share_preview_candidates(&memories, args.include_body, args.include_embeddings);
    let report = build_share_preview(&SharePreviewInput {
        target_peer_id: validated_args.peer_id,
        candidates: &candidates,
        consent_required: true,
        max_examples: args.max_examples,
    });
    let preview_hash = share_preview_hash(&report);
    let consent_record = if args.record_consent {
        match record_share_consent(&connection, &workspace_id, &validated_args, &report) {
            Ok(record) => Some(record),
            Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
        }
    } else {
        None
    };

    let render_input = SharePreviewRenderInput {
        workspace_path: &workspace_path,
        database_path: &database_path,
        workspace_id: &workspace_id,
        args,
        report: &report,
        preview_hash: &preview_hash,
        consent_record: consent_record.as_ref(),
    };

    write_share_preview_report(cli, &render_input, stdout)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SharePreviewValidatedArgs<'a> {
    peer_id: &'a str,
    actor: &'a str,
    consent_reason: &'a str,
}

fn validate_share_preview_args(
    args: &SharePreviewArgs,
) -> Result<SharePreviewValidatedArgs<'_>, DomainError> {
    let peer_id = args.peer_id.trim();
    if peer_id.is_empty() {
        return Err(DomainError::Usage {
            message: "share preview requires --peer to name the target peer".to_owned(),
            repair: Some("Use `ee share preview --peer peer_alpha --json`.".to_owned()),
        });
    }

    let actor = args.actor.trim();
    if args.record_consent && actor.is_empty() {
        return Err(DomainError::Usage {
            message: "share preview --record-consent requires --actor to name the consenting operator".to_owned(),
            repair: Some(
                "Use `ee share preview --peer peer_alpha --record-consent --actor operator --json`."
                    .to_owned(),
            ),
        });
    }

    let consent_reason = args.consent_reason.trim();
    if args.record_consent && consent_reason.is_empty() {
        return Err(DomainError::Usage {
            message: "share preview --record-consent requires a non-empty --consent-reason"
                .to_owned(),
            repair: Some(
                "Use `ee share preview --peer peer_alpha --record-consent --consent-reason operator_preview_ack --json`."
                    .to_owned(),
            ),
        });
    }

    Ok(SharePreviewValidatedArgs {
        peer_id,
        actor,
        consent_reason,
    })
}

fn share_preview_candidates<'a>(
    memories: &'a [StoredMemory],
    include_body: bool,
    include_embeddings: bool,
) -> Vec<SharePreviewCandidate<'a>> {
    let mut candidates = Vec::with_capacity(memories.len().saturating_mul(3));
    for memory in memories {
        candidates.push(SharePreviewCandidate {
            memory_id: &memory.id,
            level: &memory.level,
            kind: &memory.kind,
            trust_class: &memory.trust_class,
            material_lane: "metadata",
            redaction_class: "metadata_only",
            policy_action: "allow",
            content_preview: &memory.content,
            estimated_bytes: metadata_estimated_bytes(memory),
            body_bytes: 0,
            embedding_bytes: 0,
        });

        let body_redaction = redact_secret_like_content(&memory.content);
        let body_exportable = include_body && !body_redaction.redacted;
        candidates.push(SharePreviewCandidate {
            memory_id: &memory.id,
            level: &memory.level,
            kind: &memory.kind,
            trust_class: &memory.trust_class,
            material_lane: "body",
            redaction_class: if include_body {
                if body_redaction.redacted {
                    "body_redacted"
                } else {
                    "body_allowed"
                }
            } else {
                "body_denied"
            },
            policy_action: if body_exportable { "allow" } else { "deny" },
            content_preview: &memory.content,
            estimated_bytes: if body_exportable {
                memory.content.len() as u64
            } else {
                0
            },
            body_bytes: if body_exportable {
                memory.content.len() as u64
            } else {
                0
            },
            embedding_bytes: 0,
        });

        candidates.push(SharePreviewCandidate {
            memory_id: &memory.id,
            level: &memory.level,
            kind: &memory.kind,
            trust_class: &memory.trust_class,
            material_lane: "embedding",
            redaction_class: if include_embeddings {
                "embedding_allowed"
            } else {
                "embedding_denied"
            },
            policy_action: if include_embeddings { "allow" } else { "deny" },
            content_preview: &memory.content,
            estimated_bytes: if include_embeddings {
                EMBEDDING_ESTIMATED_BYTES
            } else {
                0
            },
            body_bytes: 0,
            embedding_bytes: if include_embeddings {
                EMBEDDING_ESTIMATED_BYTES
            } else {
                0
            },
        });
    }
    candidates
}

fn metadata_estimated_bytes(memory: &StoredMemory) -> u64 {
    [
        memory.id.as_str(),
        memory.workspace_id.as_str(),
        memory.level.as_str(),
        memory.kind.as_str(),
        memory.trust_class.as_str(),
        memory.trust_subclass.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .map(|value| value.len() as u64)
    .sum::<u64>()
    .saturating_add(32)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ShareConsentRecord {
    audit_id: String,
    audit: SharePreviewConsentAudit,
}

struct SharePreviewRenderInput<'a> {
    workspace_path: &'a Path,
    database_path: &'a Path,
    workspace_id: &'a str,
    args: &'a SharePreviewArgs,
    report: &'a SharePreviewReport,
    preview_hash: &'a str,
    consent_record: Option<&'a ShareConsentRecord>,
}

fn record_share_consent(
    connection: &DbConnection,
    workspace_id: &str,
    args: &SharePreviewValidatedArgs<'_>,
    report: &SharePreviewReport,
) -> Result<ShareConsentRecord, DomainError> {
    let audit = share_preview_consent_audit(report, true, false, args.consent_reason);
    let details = json!({
        "schema": audit.schema,
        "targetPeerId": &audit.target_peer_id,
        "previewHash": &audit.preview_hash,
        "consentRecorded": audit.consent_recorded,
        "exportAfterConsent": audit.export_after_consent,
        "dryRun": audit.dry_run,
        "reason": &audit.reason,
        "events": [
            "consent_recorded",
            "export_after_consent"
        ]
    });
    let audit_id = generate_audit_id();
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: Some(args.actor.to_owned()),
                action: SHARE_CONSENT_ACTION.to_owned(),
                target_type: Some("mesh_peer".to_owned()),
                target_id: Some(args.peer_id.to_owned()),
                details: Some(details.to_string()),
            },
        )
        .map_err(|error| storage_error("Failed to record share-preview consent audit", error))?;

    Ok(ShareConsentRecord { audit_id, audit })
}

fn write_share_preview_report<W>(
    cli: &Cli,
    input: &SharePreviewRenderInput<'_>,
    stdout: &mut W,
) -> ProcessExitCode
where
    W: Write,
{
    match cli.renderer() {
        output::Renderer::Human | output::Renderer::Markdown => write_stdout(
            stdout,
            &render_share_preview_human(input.report, input.preview_hash, input.consent_record),
        ),
        output::Renderer::Toon => {
            let data = share_preview_data_json(input);
            write_stdout(
                stdout,
                &(output::render_toon_from_json(&data.to_string()) + "\n"),
            )
        }
        output::Renderer::Json
        | output::Renderer::Jsonl
        | output::Renderer::Compact
        | output::Renderer::Hook => {
            let json = json!({
                "schema": crate::models::RESPONSE_SCHEMA_V2,
                "success": true,
                "data": share_preview_data_json(input),
                "degraded": [],
            });
            write_stdout(stdout, &(json.to_string() + "\n"))
        }
    }
}

fn share_preview_data_json(input: &SharePreviewRenderInput<'_>) -> JsonValue {
    json!({
        "schema": input.report.schema,
        "command": SHARE_PREVIEW_COMMAND,
        "workspacePath": input.workspace_path.display().to_string(),
        "workspaceId": input.workspace_id,
        "databasePath": input.database_path.display().to_string(),
        "dryRun": true,
        "exportPerformed": false,
        "includeBody": input.args.include_body,
        "includeEmbeddings": input.args.include_embeddings,
        "previewHash": input.preview_hash,
        "preview": input.report,
        "consentAudit": input
            .consent_record
            .map(consent_audit_json)
            .unwrap_or(JsonValue::Null),
        "events": share_preview_events(input.preview_hash, input.consent_record),
    })
}

fn consent_audit_json(record: &ShareConsentRecord) -> JsonValue {
    json!({
        "auditId": &record.audit_id,
        "schema": record.audit.schema,
        "targetPeerId": &record.audit.target_peer_id,
        "previewHash": &record.audit.preview_hash,
        "consentRecorded": record.audit.consent_recorded,
        "exportAfterConsent": record.audit.export_after_consent,
        "dryRun": record.audit.dry_run,
        "reason": &record.audit.reason,
    })
}

fn share_preview_events(
    preview_hash: &str,
    consent_record: Option<&ShareConsentRecord>,
) -> Vec<JsonValue> {
    let mut events = vec![
        json!({
            "event": "preview_generated",
            "previewHash": preview_hash,
        }),
        json!({
            "event": "export_not_performed",
            "dryRun": true,
        }),
    ];
    if let Some(record) = consent_record {
        events.push(json!({
            "event": "consent_recorded",
            "auditId": &record.audit_id,
            "previewHash": &record.audit.preview_hash,
        }));
        events.push(json!({
            "event": "export_after_consent",
            "performed": record.audit.export_after_consent,
        }));
    }
    events
}

fn render_share_preview_human(
    report: &SharePreviewReport,
    preview_hash: &str,
    consent_record: Option<&ShareConsentRecord>,
) -> String {
    let mut output = format!(
        "Share preview for {peer}\n  Dry run: yes\n  Export performed: no\n  Preview hash: {preview_hash}\n  Candidates: {total} total, {allowed} exportable, {denied} denied\n  Estimated exposure: {bytes} bytes ({body} body, {embedding} embedding)\n",
        peer = report.target_peer_id,
        total = report.total_candidates,
        allowed = report.exportable_count,
        denied = report.denied_count,
        bytes = report.estimated_bytes,
        body = report.estimated_body_bytes,
        embedding = report.estimated_embedding_bytes,
    );
    if !report.denied_classes.is_empty() {
        output.push_str("  Denied classes:\n");
        for denied in &report.denied_classes {
            output.push_str(&format!("    - {denied}\n"));
        }
    }
    if let Some(record) = consent_record {
        output.push_str(&format!(
            "  Consent audit: {} recorded; no export performed\n",
            record.audit_id
        ));
    }
    output
}

fn resolve_share_workspace_id(
    connection: &DbConnection,
    workspace_path: &std::path::Path,
) -> Result<String, DomainError> {
    let primary = workspace_path.to_string_lossy().into_owned();
    if let Some(workspace) = connection
        .get_workspace_by_path(&primary)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace: {error}"),
            repair: Some("ee doctor".to_string()),
        })?
    {
        return Ok(workspace.id);
    }

    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let canonical_str = canonical.to_string_lossy().into_owned();
    if canonical_str != primary
        && let Some(workspace) =
            connection
                .get_workspace_by_path(&canonical_str)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to query workspace: {error}"),
                    repair: Some("ee doctor".to_string()),
                })?
    {
        return Ok(workspace.id);
    }

    Ok(super::stable_cli_workspace_id(&canonical))
}

fn storage_error(context: &str, error: crate::db::DbError) -> DomainError {
    DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some("Run `ee doctor --json` and verify the workspace database.".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_memory(id: &str, content: &str) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
            workspace_id: "wsp_sharepreview0000000000001".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.7,
            importance: 0.6,
            provenance_uri: None,
            trust_class: "agent_validated".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "ee.memory.provenance_chain.v1".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-05-19T00:00:00Z".to_owned(),
            updated_at: "2026-05-19T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: Some("2026-05-19T00:00:00Z".to_owned()),
            valid_to: None,
        }
    }

    fn share_preview_args() -> SharePreviewArgs {
        SharePreviewArgs {
            peer_id: "peer_alpha".to_owned(),
            database: None,
            level: None,
            include_body: false,
            include_embeddings: false,
            max_examples: 6,
            record_consent: false,
            actor: "operator".to_owned(),
            consent_reason: "operator_preview_ack".to_owned(),
        }
    }

    #[test]
    fn share_preview_validation_rejects_blank_peer() {
        let mut args = share_preview_args();
        args.peer_id = " \t ".to_owned();

        let error = validate_share_preview_args(&args).expect_err("blank peer must fail");
        let DomainError::Usage { message, repair } = error else {
            panic!("expected usage error for blank peer");
        };

        assert!(message.contains("--peer"));
        assert_eq!(
            repair.as_deref(),
            Some("Use `ee share preview --peer peer_alpha --json`.")
        );
    }

    #[test]
    fn share_preview_validation_rejects_blank_consent_actor() {
        let mut args = share_preview_args();
        args.record_consent = true;
        args.actor = " \n ".to_owned();

        let error = validate_share_preview_args(&args).expect_err("blank actor must fail");
        let DomainError::Usage { message, repair } = error else {
            panic!("expected usage error for blank consent actor");
        };

        assert!(message.contains("--actor"));
        assert!(
            repair
                .as_deref()
                .is_some_and(|repair| repair.contains("--record-consent --actor operator"))
        );
    }

    #[test]
    fn share_preview_validation_rejects_blank_consent_reason() {
        let mut args = share_preview_args();
        args.record_consent = true;
        args.consent_reason = " \r\n ".to_owned();

        let error = validate_share_preview_args(&args).expect_err("blank reason must fail");
        let DomainError::Usage { message, repair } = error else {
            panic!("expected usage error for blank consent reason");
        };

        assert!(message.contains("--consent-reason"));
        assert!(
            repair
                .as_deref()
                .is_some_and(|repair| repair.contains("--consent-reason operator_preview_ack"))
        );
    }

    #[test]
    fn share_preview_validation_trims_consent_audit_fields() {
        let mut args = share_preview_args();
        args.peer_id = " peer_alpha ".to_owned();
        args.record_consent = true;
        args.actor = " operator ".to_owned();
        args.consent_reason = " reviewed ".to_owned();

        let validated = validate_share_preview_args(&args).expect("valid consent fields");

        assert_eq!(validated.peer_id, "peer_alpha");
        assert_eq!(validated.actor, "operator");
        assert_eq!(validated.consent_reason, "reviewed");
    }

    #[test]
    fn metadata_only_preview_denies_body_and_embedding_lanes() {
        let memories = [stored_memory(
            "mem_sharepreview00000000000001",
            "Never send API_KEY=sk-proj-local-secret over mesh.",
        )];
        let candidates = share_preview_candidates(&memories, false, false);
        let report = build_share_preview(&SharePreviewInput {
            target_peer_id: "peer_alpha",
            candidates: &candidates,
            consent_required: true,
            max_examples: 4,
        });

        assert_eq!(report.total_candidates, 3);
        assert_eq!(report.exportable_count, 1);
        assert_eq!(report.denied_count, 2);
        assert_eq!(report.estimated_body_bytes, 0);
        assert_eq!(report.estimated_embedding_bytes, 0);
        assert!(
            report
                .denied_classes
                .contains(&"redaction_class:body_denied".to_owned())
        );
        assert!(
            report
                .denied_classes
                .contains(&"redaction_class:embedding_denied".to_owned())
        );
        assert!(
            report
                .examples
                .iter()
                .all(|example| !example.redacted_preview.contains("sk-proj"))
        );
    }

    #[test]
    fn body_allowed_preview_still_marks_embeddings_separately() {
        let memories = [stored_memory(
            "mem_sharepreview00000000000002",
            "Public release note can be shared after review.",
        )];
        let candidates = share_preview_candidates(&memories, true, false);
        let report = build_share_preview(&SharePreviewInput {
            target_peer_id: "peer_alpha",
            candidates: &candidates,
            consent_required: true,
            max_examples: 0,
        });

        assert_eq!(report.exportable_count, 2);
        assert_eq!(report.denied_count, 1);
        assert!(report.estimated_body_bytes > 0);
        assert_eq!(report.estimated_embedding_bytes, 0);
        assert_eq!(report.counts_by_material_lane.get("body"), Some(&1));
        assert!(
            report
                .denied_classes
                .contains(&"redaction_class:embedding_denied".to_owned())
        );
    }

    #[test]
    fn body_redacted_preview_is_not_exportable() {
        let memories = [stored_memory(
            "mem_sharepreview00000000000004",
            "Never send API_KEY=sk-proj-local-secret over mesh.",
        )];
        let candidates = share_preview_candidates(&memories, true, false);
        let report = build_share_preview(&SharePreviewInput {
            target_peer_id: "peer_alpha",
            candidates: &candidates,
            consent_required: true,
            max_examples: 4,
        });

        assert_eq!(report.total_candidates, 3);
        assert_eq!(report.exportable_count, 1);
        assert_eq!(report.denied_count, 2);
        assert_eq!(report.estimated_body_bytes, 0);
        assert_eq!(report.counts_by_policy_action.get("allow"), Some(&1));
        assert_eq!(report.counts_by_policy_action.get("deny"), Some(&2));
        assert_eq!(
            report.counts_by_redaction_class.get("body_redacted"),
            Some(&1)
        );
        assert!(
            report
                .denied_classes
                .contains(&"material_lane:body".to_owned())
        );
        assert!(
            report
                .denied_classes
                .contains(&"redaction_class:body_redacted".to_owned())
        );
        assert!(
            report
                .examples
                .iter()
                .all(|example| !example.redacted_preview.contains("sk-proj"))
        );
    }

    #[test]
    fn share_preview_json_envelope_includes_clean_degraded_array() {
        let cli = Cli::try_parse_from(["ee", "--json"]).expect("parse json cli");
        let args = share_preview_args();
        let memories = [stored_memory(
            "mem_sharepreview00000000000003",
            "Public release note can be shared after review.",
        )];
        let candidates = share_preview_candidates(&memories, false, false);
        let report = build_share_preview(&SharePreviewInput {
            target_peer_id: "peer_alpha",
            candidates: &candidates,
            consent_required: true,
            max_examples: 0,
        });
        let preview_hash = share_preview_hash(&report);
        let input = SharePreviewRenderInput {
            workspace_path: Path::new("/tmp/share-preview-workspace"),
            database_path: Path::new("/tmp/share-preview-workspace/.ee/ee.db"),
            workspace_id: "wsp_sharepreview0000000000001",
            args: &args,
            report: &report,
            preview_hash: &preview_hash,
            consent_record: None,
        };
        let mut stdout = Vec::new();

        let exit = write_share_preview_report(&cli, &input, &mut stdout);

        assert_eq!(exit, ProcessExitCode::Success);
        let envelope: serde_json::Value =
            serde_json::from_slice(&stdout).expect("share preview json envelope");
        assert_eq!(envelope["schema"], crate::models::RESPONSE_SCHEMA_V2);
        assert_eq!(envelope["success"], true);
        assert_eq!(envelope["degraded"], serde_json::json!([]));
        assert_eq!(
            envelope["data"]["schema"],
            crate::policy::SHARE_PREVIEW_SCHEMA_V1
        );
    }
}
