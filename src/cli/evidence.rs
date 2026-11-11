//! Operator withdrawal and bounded policy re-screening of retained evidence.

use std::io::Write;
use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

use super::{
    Cli, ProcessExitCode, output, workspace_response_json_v2, write_domain_error, write_stdout,
};
use crate::core::evidence::{
    EvidenceRescreenOptions, EvidenceSecurityRescreenReport, EvidenceSessionWithdrawOptions,
    EvidenceSessionWithdrawReport, EvidenceWithdrawOptions, EvidenceWithdrawReport,
    rescreen_imported_evidence, withdraw_cass_evidence, withdraw_cass_session_evidence,
};

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum EvidenceCommand {
    /// Withdraw retained CASS evidence by source ID or session, without erasing history.
    Withdraw(EvidenceWithdrawArgs),
    /// Preview a bounded re-screen of retained evidence; use --apply to persist decisions.
    Rescreen(EvidenceRescreenArgs),
}

#[derive(Clone, Debug, Eq, PartialEq, Parser)]
pub struct EvidenceRescreenArgs {
    /// Persist the re-screened security decisions and audit receipts. Omit to preview.
    #[arg(long, action = ArgAction::SetTrue)]
    pub apply: bool,

    /// Explicitly request the default read-only preview.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "apply")]
    pub dry_run: bool,

    /// Maximum evidence rows to re-screen in this invocation (1..500).
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=500))]
    pub limit: u32,

    /// Actor recorded in audit receipts when applying.
    #[arg(long, value_name = "ACTOR")]
    pub actor: Option<String>,

    /// Existing database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Parser)]
pub struct EvidenceWithdrawArgs {
    /// Imported evidence ID from a search result or source citation.
    #[arg(
        value_name = "EVIDENCE_ID",
        required_unless_present = "session",
        conflicts_with = "session"
    )]
    pub evidence_id: Option<String>,

    /// Withdraw all currently retained CASS sources of this session atomically.
    /// Does not disable future imports or withdraw sources from other producers.
    #[arg(
        long,
        value_name = "SESSION_ID",
        required_unless_present = "evidence_id",
        conflicts_with = "evidence_id"
    )]
    pub session: Option<String>,

    /// Required audit reason; secrets are redacted before storage.
    #[arg(long, value_name = "REASON")]
    pub reason: String,

    /// Actor recorded in the audit trail.
    #[arg(long, value_name = "ACTOR")]
    pub actor: Option<String>,

    /// Preview without changing evidence, audit history, or pending index jobs.
    #[arg(long, action = ArgAction::SetTrue)]
    pub dry_run: bool,

    /// Existing database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

pub(super) fn handle<W: Write, E: Write>(
    cli: &Cli,
    command: &EvidenceCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode {
    match command {
        EvidenceCommand::Withdraw(args) => handle_withdraw(cli, args, stdout, stderr),
        EvidenceCommand::Rescreen(args) => handle_rescreen(cli, args, stdout, stderr),
    }
}

fn handle_rescreen<W: Write, E: Write>(
    cli: &Cli,
    args: &EvidenceRescreenArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode {
    if args.apply && args.dry_run {
        return write_domain_error(
            &crate::models::DomainError::Usage {
                message: "Choose --apply or --dry-run for evidence re-screening.".to_owned(),
                repair: None,
            },
            cli.renderer(),
            stdout,
            stderr,
        );
    }
    let workspace = cli.resolve_workspace();
    let report = match rescreen_imported_evidence(&EvidenceRescreenOptions {
        workspace_path: &workspace,
        database_path: args.database.as_deref(),
        limit: args.limit,
        actor: args.actor.as_deref().or(Some("ee evidence rescreen")),
        apply: args.apply,
    }) {
        Ok(report) => report,
        Err(error) => return write_domain_error(&error, cli.renderer(), stdout, stderr),
    };
    render_report(cli, &report, &rescreen_human_output(&report), stdout)
}

fn handle_withdraw<W: Write, E: Write>(
    cli: &Cli,
    args: &EvidenceWithdrawArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode {
    let workspace = cli.resolve_workspace();
    // Clap enforces exclusive selection. Keep the same boundary for library
    // callers constructing arguments directly: neither missing nor ambiguous
    // input may fall through to a broader operation.
    if args.evidence_id.is_some() == args.session.is_some() {
        return write_domain_error(
            &crate::models::DomainError::Usage {
                message: "Choose one evidence ID or --session, not both.".to_owned(),
                repair: None,
            },
            cli.renderer(),
            stdout,
            stderr,
        );
    }
    if let Some(session_id) = args.session.as_deref() {
        let report = match withdraw_cass_session_evidence(&EvidenceSessionWithdrawOptions {
            workspace_path: &workspace,
            database_path: args.database.as_deref(),
            session_id,
            reason: &args.reason,
            actor: args.actor.as_deref().or(Some("ee evidence withdraw")),
            dry_run: args.dry_run,
        }) {
            Ok(report) => report,
            Err(error) => return write_domain_error(&error, cli.renderer(), stdout, stderr),
        };
        return render_report(cli, &report, &session_human_output(&report), stdout);
    }
    let report = match withdraw_cass_evidence(&EvidenceWithdrawOptions {
        workspace_path: &workspace,
        database_path: args.database.as_deref(),
        evidence_id: args.evidence_id.as_deref().unwrap_or_default(),
        reason: &args.reason,
        actor: args.actor.as_deref().or(Some("ee evidence withdraw")),
        dry_run: args.dry_run,
    }) {
        Ok(report) => report,
        Err(error) => return write_domain_error(&error, cli.renderer(), stdout, stderr),
    };

    render_report(cli, &report, &human_output(&report), stdout)
}

fn render_report<T: serde::Serialize, W: Write>(
    cli: &Cli,
    report: &T,
    human: &str,
    stdout: &mut W,
) -> ProcessExitCode {
    match cli.renderer() {
        output::Renderer::Human | output::Renderer::Markdown => write_stdout(stdout, human),
        output::Renderer::Toon => write_stdout(
            stdout,
            &(output::render_toon_from_json(&workspace_response_json_v2(report)) + "\n"),
        ),
        output::Renderer::Json
        | output::Renderer::Jsonl
        | output::Renderer::Compact
        | output::Renderer::Hook => {
            write_stdout(stdout, &(workspace_response_json_v2(report) + "\n"))
        }
    }
}

fn human_output(report: &EvidenceWithdrawReport) -> String {
    let heading = match report.status {
        "would_withdraw" => "Evidence would be withdrawn",
        "already_withdrawn" => "Evidence already withdrawn",
        _ => "Evidence withdrawn",
    };
    let mut text = format!(
        "{heading}: {}\n  Workspace: {}\n  Session: {}\n  Search eligibility: {} -> {}\n  Pack eligibility: {} -> {}\n",
        report.evidence_id,
        report.workspace_id,
        report.session_id,
        report.previous_search_eligibility,
        report.search_eligibility,
        report.previous_pack_eligibility,
        report.pack_eligibility,
    );
    if let Some(audit_id) = &report.audit_id {
        text.push_str(&format!("  Audit: {audit_id}\n"));
    }
    match report.index_status {
        "queued" => {
            text.push_str("  Index: queued; publication pending");
            if let Some(job) = &report.index_job_id {
                text.push_str(&format!(" ({job})"));
            }
            text.push('\n');
            text.push_str(
                "  Live admission now excludes this source from retrieval and derivation.\n",
            );
        }
        "dry_run_not_queued" => {
            text.push_str("  Preview: no changes persisted and no index job queued.\n");
        }
        _ => text.push_str("  No changes persisted and no additional index job queued.\n"),
    }
    text.push_str(
        "  Transcript bytes, historical packs and curated memories/rules are retained.\n",
    );
    text
}

fn session_human_output(report: &EvidenceSessionWithdrawReport) -> String {
    let heading = match report.status {
        "would_withdraw" => "Session evidence would be withdrawn",
        "already_withdrawn" => "Session evidence already withdrawn",
        "no_cass_sources" => "No retained CASS sources in this session",
        _ => "Session evidence withdrawn",
    };
    let mut text = format!(
        "{heading}: {}\n  Workspace: {}\n  Retained sources: {}\n  CASS sources: {}\n  Other producers unchanged: {}\n  Already withdrawn: {}\n  Would withdraw: {}\n  Withdrawn this invocation: {}\n",
        report.session_id,
        report.workspace_id,
        report.retained_evidence_count,
        report.cass_evidence_count,
        report.excluded_producer_count,
        report.already_withdrawn_count,
        report.would_withdraw_count,
        report.withdrawn_count,
    );
    if let Some(audit) = &report.audit_id {
        text.push_str(&format!("  Audit: {audit}\n"));
    }
    if let Some(job) = &report.index_job_id {
        text.push_str(&format!("  Index: queued; publication pending ({job})\n"));
    } else if report.status == "would_withdraw" {
        text.push_str("  Preview: no changes persisted and no index job queued.\n");
    } else {
        text.push_str("  No changes persisted and no additional index job queued.\n");
    }
    text.push_str(
        "  Scope: currently retained CASS sources only; future imports are unchanged.\n  Transcript bytes, historical packs and curated memories/rules are retained.\n",
    );
    text
}

fn rescreen_human_output(report: &EvidenceSecurityRescreenReport) -> String {
    let heading = if report.dry_run {
        "Evidence re-screen preview"
    } else {
        "Evidence re-screen completed"
    };
    let mut text = format!(
        "{heading}\n  Workspace: {}\n  Selected: {} of {} pending (limit {})\n  Updated: {}\n  Pending after this invocation: {}\n",
        report.workspace_id,
        report.selected,
        report.pending_before,
        report.limit,
        report.updated,
        report.pending_after,
    );
    for item in &report.items {
        text.push_str(&format!(
            "  {}: {} ({})\n",
            item.evidence_id,
            item.disposition,
            item.reason_codes.join(", ")
        ));
    }
    if report.dry_run {
        text.push_str(
            "  Preview: no changes persisted. Repeat with --apply to record these decisions.\n",
        );
    } else if report.index_rebuild_required {
        text.push_str("  Database decisions saved; search publication is still pending.\n");
        text.push_str("  Rebuild the addressed database's search index with ee index rebuild before querying recovered evidence.\n");
    }
    if report.items.iter().any(|item| {
        item.reason_codes
            .iter()
            .any(|reason| reason == "source_commitment_key_unavailable")
    }) {
        text.push_str("  Restore the original workspace authentication key and retry; affected sources remain pending.\n");
    }
    if report.items.iter().any(|item| {
        item.reason_codes
            .iter()
            .any(|reason| reason == "complete_source_not_retained")
    }) {
        text.push_str("  Reimport the original source with ee import cass to verify bytes absent from retained excerpts.\n");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
        EvidenceProducerKind,
    };
    use crate::models::{EvidenceId, SessionId, WorkspaceId};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
    const SOURCE: &str =
        "The cache commits durable records after checking the transaction checksum.";

    struct Fixture {
        root: tempfile::TempDir,
        database: PathBuf,
        workspace_id: String,
        evidence_id: String,
    }

    impl Fixture {
        fn new() -> TestResult<Self> {
            let root = tempfile::tempdir()?;
            let database = root.path().join("custom.db");
            let connection = DbConnection::open_file(&database)?;
            connection.migrate()?;
            let workspace_id = WorkspaceId::now().to_string();
            connection.insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: root.path().to_string_lossy().into_owned(),
                    name: None,
                },
            )?;
            let session_id = SessionId::now().to_string();
            connection.insert_session(
                &session_id,
                &CreateSessionInput {
                    workspace_id: workspace_id.clone(),
                    cass_session_id: "withdraw-cli-session".to_owned(),
                    source_path: None,
                    agent_name: None,
                    model: None,
                    started_at: None,
                    ended_at: None,
                    message_count: 1,
                    token_count: None,
                    content_hash: format!("blake3:{}", blake3::hash(SOURCE.as_bytes())),
                    metadata_json: None,
                },
            )?;
            let evidence_id = EvidenceId::now().to_string();
            connection.insert_evidence_span(
                &evidence_id,
                &CreateEvidenceSpanInput {
                    workspace_id: workspace_id.clone(),
                    session_id,
                    memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: "withdraw-cli-line-1".to_owned(),
                    span_kind: "message".to_owned(),
                    start_line: 1,
                    end_line: 1,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: SOURCE.to_owned(),
                    content_hash: format!("blake3:{}", blake3::hash(SOURCE.as_bytes())),
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )?;
            connection.close()?;
            Ok(Self {
                root,
                database,
                workspace_id,
                evidence_id,
            })
        }

        fn cli(&self, format: &str, dry_run: bool) -> TestResult<Cli> {
            let mut args = vec![
                "ee".to_owned(),
                "--workspace".to_owned(),
                self.root.path().display().to_string(),
                "--format".to_owned(),
                format.to_owned(),
                "evidence".to_owned(),
                "withdraw".to_owned(),
                self.evidence_id.clone(),
                "--reason".to_owned(),
                "No longer authorized for retrieval.".to_owned(),
                "--database".to_owned(),
                self.database.display().to_string(),
            ];
            if dry_run {
                args.push("--dry-run".to_owned());
            }
            Ok(Cli::try_parse_from(args)?)
        }

        fn rescreen_cli(&self, format: &str, apply: bool) -> TestResult<Cli> {
            let mut args = vec![
                "ee".to_owned(),
                "--workspace".to_owned(),
                self.root.path().display().to_string(),
                "--format".to_owned(),
                format.to_owned(),
                "evidence".to_owned(),
                "rescreen".to_owned(),
                "--limit".to_owned(),
                "1".to_owned(),
                "--database".to_owned(),
                self.database.display().to_string(),
            ];
            if apply {
                args.push("--apply".to_owned());
            }
            Ok(Cli::try_parse_from(args)?)
        }

        fn historical_keyed_source(
            &self,
            original: &str,
        ) -> TestResult<crate::db::StoredEvidenceSpan> {
            use crate::policy::cass_source::{CassSourceCommitment, CassSourceIdentity};
            use crate::policy::store_auth::{StoreAuthRoot, workspace_keys_dir};

            let key = StoreAuthRoot::create(workspace_keys_dir(self.root.path()))?;
            let db = DbConnection::open_file(&self.database)?;
            let row = db.get_evidence_span(&self.evidence_id)?.ok_or("source")?;
            let source = CassSourceCommitment::capture(
                &key,
                CassSourceIdentity {
                    session_id: &row.session_id,
                    upstream_ref_hash: &row.cass_span_id,
                },
                &blake3::hash(original.as_bytes()),
                original.len().try_into()?,
                5,
            )?;
            let previous_epoch = crate::db::EVIDENCE_SECURITY_POLICY_EPOCH
                .saturating_sub(1)
                .max(1);
            let mut metadata: serde_json::Value =
                serde_json::from_str(row.metadata_json.as_deref().ok_or("security metadata")?)?;
            metadata["cassSource"] = serde_json::to_value(source)?;
            metadata["securityPolicyEpoch"] = previous_epoch.into();
            metadata["instructionRisk"] = "high".into();
            metadata["searchEligibility"] = "quarantined".into();
            metadata["packEligibility"] = "quarantined".into();
            db.execute_raw(&format!(
                "UPDATE evidence_spans SET security_policy_epoch = {previous_epoch}, instruction_risk = 'high', search_eligibility = 'quarantined', pack_eligibility = 'quarantined', metadata_json = '{}' WHERE id = '{}'",
                metadata.to_string().replace('\'', "''"),
                self.evidence_id,
            ))?;
            let historical = db
                .get_evidence_span(&self.evidence_id)?
                .ok_or("historical source")?;
            db.close()?;
            Ok(historical)
        }
    }

    fn invoke(cli: &Cli) -> TestResult<(ProcessExitCode, String, String)> {
        let Some(super::super::Command::Evidence(command)) = cli.command.as_ref() else {
            return Err("expected evidence command".into());
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = handle(cli, command, &mut stdout, &mut stderr);
        Ok((exit, String::from_utf8(stdout)?, String::from_utf8(stderr)?))
    }

    #[test]
    fn rescreen_cli_previews_by_default_and_rejects_unbounded_batches() -> TestResult {
        let cli = Cli::try_parse_from(["ee", "evidence", "rescreen"])?;
        let Some(super::super::Command::Evidence(EvidenceCommand::Rescreen(args))) = cli.command
        else {
            return Err("expected re-screen arguments".into());
        };
        assert!(!args.apply);
        assert!(!args.dry_run);
        assert_eq!(args.limit, 100);
        assert!(args.database.is_none());
        let preview = Cli::try_parse_from(["ee", "evidence", "rescreen", "--dry-run"])?;
        assert!(matches!(
            preview.command,
            Some(super::super::Command::Evidence(EvidenceCommand::Rescreen(
                EvidenceRescreenArgs {
                    apply: false,
                    dry_run: true,
                    ..
                }
            )))
        ));
        assert!(
            Cli::try_parse_from(["ee", "evidence", "rescreen", "--dry-run", "--apply"]).is_err()
        );
        let cli = Cli::try_parse_from([
            "ee", "evidence", "rescreen", "--apply", "--limit", "500", "--actor", "operator",
        ])?;
        let Some(super::super::Command::Evidence(EvidenceCommand::Rescreen(args))) = cli.command
        else {
            return Err("expected explicit re-screen apply".into());
        };
        assert!(args.apply);
        assert_eq!(args.limit, 500);
        assert_eq!(args.actor.as_deref(), Some("operator"));
        for invalid in ["0", "501", "-1", "unbounded"] {
            assert!(
                Cli::try_parse_from(["ee", "evidence", "rescreen", "--limit", invalid]).is_err()
            );
        }
        let missing_store = tempfile::tempdir()?;
        let workspace = missing_store.path().display().to_string();
        let mut direct = Cli::try_parse_from([
            "ee",
            "--workspace",
            &workspace,
            "--json",
            "evidence",
            "rescreen",
        ])?;
        let Some(super::super::Command::Evidence(EvidenceCommand::Rescreen(args))) =
            direct.command.as_mut()
        else {
            return Err("expected direct re-screen arguments".into());
        };
        args.apply = true;
        args.dry_run = true;
        let (exit, stdout, stderr) = invoke(&direct)?;
        assert_ne!(exit, ProcessExitCode::Success);
        assert!(stderr.is_empty());
        assert!(stdout.contains("Choose --apply or --dry-run"));
        assert!(!missing_store.path().join(".ee").exists());
        Ok(())
    }

    #[test]
    fn rescreen_cli_full_dispatch_previews_then_applies_authenticated_history() -> TestResult {
        let fixture = Fixture::new()?;
        let historical = fixture.historical_keyed_source(SOURCE)?;
        let bytes_before = std::fs::read(&fixture.database)?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let workspace = fixture.root.path().display().to_string();
        let database = fixture.database.display().to_string();
        let exit = super::super::run(
            [
                "ee",
                "--workspace",
                &workspace,
                "--json",
                "evidence",
                "rescreen",
                "--limit",
                "1",
                "--database",
                &database,
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(
            exit,
            ProcessExitCode::Success,
            "{}",
            String::from_utf8_lossy(&stderr)
        );
        assert!(stderr.is_empty());
        assert!(!String::from_utf8_lossy(&stdout).contains(SOURCE));
        let preview: serde_json::Value = serde_json::from_slice(&stdout)?;
        assert_eq!(preview["success"], true);
        assert_eq!(
            preview["data"]["schema"],
            "ee.evidence.security_rescreen.v1"
        );
        assert_eq!(preview["data"]["dryRun"], true);
        assert_eq!(preview["data"]["durableMutation"], false);
        assert_eq!(preview["data"]["selected"], 1);
        assert_eq!(preview["data"]["updated"], 0);
        assert_eq!(preview["data"]["items"][0]["disposition"], "admitted");
        assert_eq!(preview["data"]["rebuildWouldBeRequired"], true);
        assert_eq!(std::fs::read(&fixture.database)?, bytes_before);
        let db = DbConnection::open_file_read_only(&fixture.database)?;
        assert_eq!(
            db.get_evidence_span(&fixture.evidence_id)?,
            Some(historical.clone())
        );
        assert!(
            db.list_audit_entries(Some(&fixture.workspace_id), None)?
                .is_empty()
        );
        db.close()?;

        let (exit, json, stderr) = invoke(&fixture.rescreen_cli("json", true)?)?;
        assert_eq!(exit, ProcessExitCode::Success);
        assert!(stderr.is_empty() && !json.contains(SOURCE));
        let applied: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(applied["data"]["updated"], 1);
        assert_eq!(applied["data"]["dryRun"], false);
        assert_eq!(applied["data"]["durableMutation"], true);
        assert_eq!(applied["data"]["indexRebuildRequired"], true);
        assert_eq!(applied["data"]["complete"], true);
        let db = DbConnection::open_file_read_only(&fixture.database)?;
        let recovered = db
            .get_search_admitted_evidence_span(&fixture.evidence_id, &fixture.workspace_id)?
            .ok_or("recovered source")?;
        assert_eq!(
            recovered.security_policy_epoch,
            crate::db::EVIDENCE_SECURITY_POLICY_EPOCH
        );
        assert_eq!(recovered.excerpt, historical.excerpt);
        assert_eq!(recovered.content_hash, historical.content_hash);
        assert_eq!(recovered.cass_span_id, historical.cass_span_id);
        assert_eq!(
            db.list_audit_entries(Some(&fixture.workspace_id), None)?
                .len(),
            1
        );
        db.close()?;
        let (_, json, _) = invoke(&fixture.rescreen_cli("json", true)?)?;
        let repeated: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(repeated["data"]["selected"], 0);
        assert_eq!(repeated["data"]["durableMutation"], false);
        assert!(!fixture.root.path().join(".ee/ee.db").exists());
        Ok(())
    }

    #[test]
    fn rescreen_cli_preserves_key_loss_for_retry_with_the_original_workspace_key() -> TestResult {
        let fixture = Fixture::new()?;
        let historical = fixture.historical_keyed_source(SOURCE)?;
        let keys = crate::policy::store_auth::workspace_keys_dir(fixture.root.path());
        let saved = fixture.root.path().join("original-keys");
        std::fs::rename(&keys, &saved)?;
        let (exit, json, stderr) = invoke(&fixture.rescreen_cli("json", true)?)?;
        assert_eq!(exit, ProcessExitCode::Success);
        assert!(stderr.is_empty());
        let blocked: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(blocked["data"]["updated"], 0);
        assert_eq!(blocked["data"]["pendingAfter"], 1);
        assert_eq!(blocked["data"]["durableMutation"], false);
        assert_eq!(
            blocked["data"]["items"][0]["reasonCodes"],
            serde_json::json!(["source_commitment_key_unavailable"])
        );
        assert!(
            !keys.exists(),
            "a recovery command must not replace a missing key"
        );
        let db = DbConnection::open_file_read_only(&fixture.database)?;
        assert_eq!(
            db.get_evidence_span(&fixture.evidence_id)?,
            Some(historical)
        );
        assert!(
            db.list_audit_entries(Some(&fixture.workspace_id), None)?
                .is_empty()
        );
        db.close()?;
        std::fs::rename(&saved, &keys)?;
        let (_, json, _) = invoke(&fixture.rescreen_cli("json", true)?)?;
        let recovered: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(recovered["data"]["updated"], 1);
        assert_eq!(recovered["data"]["complete"], true);
        Ok(())
    }

    #[test]
    fn rescreen_cli_keeps_incomplete_source_refused_and_reports_the_recovery_route() -> TestResult {
        let fixture = Fixture::new()?;
        let original = format!("{SOURCE} Additional authenticated original material.");
        let historical = fixture.historical_keyed_source(&original)?;
        let (exit, human, stderr) = invoke(&fixture.rescreen_cli("human", false)?)?;
        assert_eq!(exit, ProcessExitCode::Success);
        assert!(stderr.is_empty() && !human.contains(SOURCE));
        assert!(human.contains("complete_source_not_retained"));
        assert!(human.contains("ee import cass"));
        assert!(human.contains("Preview: no changes persisted"));
        let (_, json, _) = invoke(&fixture.rescreen_cli("json", true)?)?;
        let applied: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(applied["data"]["updated"], 0);
        assert_eq!(applied["data"]["indexRebuildRequired"], false);
        assert_eq!(applied["data"]["items"][0]["disposition"], "retained");
        let db = DbConnection::open_file_read_only(&fixture.database)?;
        assert_eq!(
            db.get_evidence_span(&fixture.evidence_id)?,
            Some(historical)
        );
        assert!(
            db.get_search_admitted_evidence_span(&fixture.evidence_id, &fixture.workspace_id)?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn evidence_cli_requires_reason_and_preserves_explicit_preview_and_database() -> TestResult {
        let fixture = Fixture::new()?;
        let cli = fixture.cli("json", true)?;
        let Some(super::super::Command::Evidence(EvidenceCommand::Withdraw(args))) = cli.command
        else {
            return Err("expected withdrawal arguments".into());
        };
        assert!(args.dry_run);
        assert_eq!(
            args.evidence_id.as_deref(),
            Some(fixture.evidence_id.as_str())
        );
        assert!(args.session.is_none());
        assert_eq!(args.database.as_deref(), Some(fixture.database.as_path()));
        assert!(Cli::try_parse_from(["ee", "evidence", "withdraw", &fixture.evidence_id]).is_err());
        Ok(())
    }

    #[test]
    fn evidence_cli_full_dispatch_admits_dry_run_and_projects_minimal_fields() -> TestResult {
        let fixture = Fixture::new()?;
        let workspace = fixture.root.path().to_string_lossy();
        let database = fixture.database.to_string_lossy();
        let args = [
            "ee",
            "--workspace",
            workspace.as_ref(),
            "--json",
            "--fields",
            "minimal",
            "evidence",
            "withdraw",
            fixture.evidence_id.as_str(),
            "--reason",
            "Preview withdrawal of an obsolete observation.",
            "--database",
            database.as_ref(),
            "--dry-run",
        ];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = super::super::run(
            args.into_iter().map(std::ffi::OsString::from),
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(
            exit,
            ProcessExitCode::Success,
            "{}",
            String::from_utf8_lossy(&stderr)
        );
        assert!(stderr.is_empty());
        let response: serde_json::Value = serde_json::from_slice(&stdout)?;
        assert_eq!(response["success"], true);
        let data = &response["data"];
        assert_eq!(data["schema"], "ee.evidence.withdraw.v1");
        assert_eq!(data["evidenceId"], fixture.evidence_id);
        assert_eq!(data["status"], "would_withdraw");
        assert_eq!(data["dryRun"], true);
        assert_eq!(data["persisted"], false);
        assert_eq!(data["changed"], false);
        assert_eq!(data["wouldChange"], true);
        assert_eq!(data["indexStatus"], "dry_run_not_queued");
        assert!(data.get("sessionId").is_none());
        assert!(data.get("previousSearchEligibility").is_none());
        assert!(!String::from_utf8_lossy(&stdout).contains(SOURCE));
        let db = DbConnection::open_file_read_only(&fixture.database)?;
        assert!(
            db.get_search_admitted_evidence_span(&fixture.evidence_id, &fixture.workspace_id)?
                .is_some()
        );
        assert!(
            db.list_search_index_jobs(&fixture.workspace_id, None)?
                .is_empty()
        );
        assert!(
            db.list_audit_entries(Some(&fixture.workspace_id), None)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn evidence_cli_json_toon_and_human_keep_admission_and_index_status_honest() -> TestResult {
        let fixture = Fixture::new()?;
        let (exit, json, stderr) = invoke(&fixture.cli("json", true)?)?;
        assert_eq!(exit, ProcessExitCode::Success);
        assert!(stderr.is_empty());
        assert!(!json.contains(SOURCE));
        let expected: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(expected["schema"], "ee.response.v2");
        assert_eq!(expected["data"]["schema"], "ee.evidence.withdraw.v1");
        assert_eq!(expected["data"]["status"], "would_withdraw");
        assert_eq!(expected["data"]["persisted"], false);
        assert_eq!(expected["data"]["indexStatus"], "dry_run_not_queued");
        let (exit, toon, stderr) = invoke(&fixture.cli("toon", true)?)?;
        assert_eq!(exit, ProcessExitCode::Success);
        assert!(stderr.is_empty() && !toon.contains(SOURCE));
        let decoded = toon::try_decode(&toon, None).map_err(|error| error.to_string())?;
        assert_eq!(
            serde_json::Value::from(decoded),
            serde_json::Value::from(toon::JsonValue::from(expected))
        );

        let (exit, human, stderr) = invoke(&fixture.cli("human", false)?)?;
        assert_eq!(exit, ProcessExitCode::Success);
        assert!(stderr.is_empty() && !human.contains(SOURCE));
        assert!(human.contains("Evidence withdrawn:"));
        assert!(human.contains("Index: queued; publication pending"));
        assert!(human.contains(
            "Transcript bytes, historical packs and curated memories/rules are retained."
        ));
        let db = DbConnection::open_file_read_only(&fixture.database)?;
        assert!(
            db.get_search_admitted_evidence_span(&fixture.evidence_id, &fixture.workspace_id)?
                .is_none()
        );
        assert_eq!(
            db.get_evidence_span(&fixture.evidence_id)?
                .ok_or("retained source")?
                .excerpt,
            SOURCE
        );
        let jobs = db.list_search_index_jobs(&fixture.workspace_id, None)?;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, "pending");
        db.close()?;

        let (_, repeated, _) = invoke(&fixture.cli("json", false)?)?;
        let repeated: serde_json::Value = serde_json::from_str(&repeated)?;
        assert_eq!(repeated["data"]["status"], "already_withdrawn");
        assert_eq!(repeated["data"]["changed"], false);
        assert_eq!(repeated["data"]["indexStatus"], "not_scheduled");
        assert!(repeated["data"]["auditId"].is_null());
        assert!(repeated["data"]["indexJobId"].is_null());
        assert!(
            !fixture.root.path().join(".ee").exists(),
            "explicit database must not create the default store"
        );
        Ok(())
    }

    #[test]
    fn evidence_cli_errors_keep_machine_stdout_and_human_stderr_routing() -> TestResult {
        let fixture = Fixture::new()?;
        for format in ["json", "human"] {
            let mut cli = fixture.cli(format, false)?;
            let Some(super::super::Command::Evidence(EvidenceCommand::Withdraw(args))) =
                cli.command.as_mut()
            else {
                return Err("expected withdrawal arguments".into());
            };
            args.reason = " ".to_owned();
            let (exit, stdout, stderr) = invoke(&cli)?;
            assert_ne!(exit, ProcessExitCode::Success);
            if format == "json" {
                let error: serde_json::Value = serde_json::from_str(&stdout)?;
                assert_eq!(error["success"], false);
                assert!(stderr.is_empty());
            } else {
                assert!(stdout.is_empty());
                assert!(stderr.contains("nonempty reason"));
            }
        }
        let db = DbConnection::open_file_read_only(&fixture.database)?;
        assert!(
            db.get_search_admitted_evidence_span(&fixture.evidence_id, &fixture.workspace_id)?
                .is_some()
        );
        assert!(
            db.list_search_index_jobs(&fixture.workspace_id, None)?
                .is_empty()
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "evidence_session_tests.rs"]
mod session_tests;
