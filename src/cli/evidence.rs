//! Explicit operator withdrawal of retained CASS evidence.

use std::io::Write;
use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

use super::{
    Cli, ProcessExitCode, output, workspace_response_json_v2, write_domain_error, write_stdout,
};
use crate::core::evidence::{
    EvidenceSessionWithdrawOptions, EvidenceSessionWithdrawReport, EvidenceWithdrawOptions,
    EvidenceWithdrawReport, withdraw_cass_evidence, withdraw_cass_session_evidence,
};

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum EvidenceCommand {
    /// Withdraw retained CASS evidence by source ID or session, without erasing history.
    Withdraw(EvidenceWithdrawArgs),
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
    let EvidenceCommand::Withdraw(args) = command;
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
