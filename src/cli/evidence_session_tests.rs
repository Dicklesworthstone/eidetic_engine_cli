use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use crate::models::{EvidenceId, SessionId, WorkspaceId};
use std::collections::BTreeMap;
use std::path::Path;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const SOURCE: &str = "The cache commits durable records before publishing the replacement checksum.";
const REASON: &str = "Operator requested withdrawal of the retained session observations.";

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    workspace: String,
    session: String,
    evidence: Vec<String>,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let database = root.path().join("custom.db");
        let db = DbConnection::open_file(&database)?;
        db.migrate()?;
        let workspace = WorkspaceId::now().to_string();
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: root.path().to_string_lossy().into_owned(),
                name: None,
            },
        )?;
        let session = SessionId::now().to_string();
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: workspace.clone(),
                cass_session_id: "session-withdraw-cli".to_owned(),
                source_path: None,
                agent_name: None,
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 3,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(SOURCE.as_bytes())),
                metadata_json: None,
            },
        )?;
        let mut evidence = Vec::new();
        for line in 1..=3 {
            let id = EvidenceId::now().to_string();
            db.insert_evidence_span(
                &id,
                &CreateEvidenceSpanInput {
                    workspace_id: workspace.clone(),
                    session_id: session.clone(),
                    memory_id: None,
                    producer_kind: if line == 3 {
                        EvidenceProducerKind::DocsBootstrap
                    } else {
                        EvidenceProducerKind::CassImport
                    },
                    cass_span_id: format!("withdraw-session-cli:{line}"),
                    span_kind: "message".to_owned(),
                    start_line: line,
                    end_line: line,
                    start_byte: None,
                    end_byte: None,
                    role: Some("assistant".to_owned()),
                    excerpt: SOURCE.to_owned(),
                    content_hash: format!("blake3:{}", blake3::hash(SOURCE.as_bytes())),
                    metadata_json: None,
                    inherited_redaction_classes: Vec::new(),
                },
            )?;
            evidence.push(id);
        }
        db.close()?;
        Ok(Self { root, database, workspace, session, evidence })
    }

    fn args(&self, format: &str, preview: bool) -> Vec<String> {
        let mut args = vec![
            "ee".to_owned(),
            "--workspace".to_owned(),
            self.root.path().display().to_string(),
            "--format".to_owned(),
            format.to_owned(),
            "evidence".to_owned(),
            "withdraw".to_owned(),
            "--session".to_owned(),
            self.session.clone(),
            "--reason".to_owned(),
            REASON.to_owned(),
            "--database".to_owned(),
            self.database.display().to_string(),
        ];
        if preview {
            args.push("--dry-run".to_owned());
        }
        args
    }
}

fn dispatch(args: Vec<String>) -> TestResult<String> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = super::super::run(
        args.into_iter().map(std::ffi::OsString::from),
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(exit, ProcessExitCode::Success, "{}", String::from_utf8_lossy(&stderr));
    assert!(stderr.is_empty());
    let text = String::from_utf8(stdout)?;
    assert!(!text.contains(SOURCE) && !text.contains(REASON));
    Ok(text)
}

fn files(root: &Path) -> TestResult<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(at: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) -> TestResult {
        for entry in std::fs::read_dir(at)? {
            let path = entry?.path();
            if path.is_dir() {
                visit(&path, found)?;
            } else {
                found.insert(path.clone(), std::fs::read(path)?);
            }
        }
        Ok(())
    }
    let mut found = BTreeMap::new();
    visit(root, &mut found)?;
    Ok(found)
}

#[test]
fn session_selector_is_explicit_and_exclusive_with_single_evidence() -> TestResult {
    let fixture = Fixture::new()?;
    let cli = Cli::try_parse_from(fixture.args("json", true))?;
    let Some(super::super::Command::Evidence(EvidenceCommand::Withdraw(args))) = cli.command else {
        return Err("session withdrawal command".into());
    };
    assert_eq!(args.session.as_deref(), Some(fixture.session.as_str()));
    assert!(args.evidence_id.is_none() && args.dry_run);
    assert_eq!(args.database.as_deref(), Some(fixture.database.as_path()));
    assert!(Cli::try_parse_from(["ee", "evidence", "withdraw", "--reason", REASON]).is_err());
    assert!(Cli::try_parse_from([
        "ee", "evidence", "withdraw", "--session", fixture.session.as_str(),
    ]).is_err());
    let mut ambiguous = fixture.args("json", true);
    ambiguous.push(fixture.evidence[0].clone());
    assert!(Cli::try_parse_from(ambiguous).is_err());
    Ok(())
}

#[test]
fn session_full_cli_preview_projects_counts_without_mutating_any_store_bytes() -> TestResult {
    let fixture = Fixture::new()?;
    let before = files(fixture.root.path())?;
    let mut args = fixture.args("json", true);
    args.extend(["--fields".to_owned(), "minimal".to_owned()]);
    let response: serde_json::Value = serde_json::from_str(&dispatch(args)?)?;
    assert_eq!(response["schema"], "ee.response.v2");
    assert_eq!(response["success"], true);
    let data = &response["data"];
    assert_eq!(data["schema"], "ee.evidence.withdraw_session.v1");
    assert_eq!(data["sessionId"], fixture.session);
    assert_eq!(data["status"], "would_withdraw");
    assert_eq!(data["dryRun"], true);
    assert_eq!(data["persisted"], false);
    assert_eq!(data["wouldWithdrawCount"], 2);
    assert_eq!(data["withdrawnCount"], 0);
    assert_eq!(data["indexStatus"], "dry_run_not_queued");
    assert!(data.get("retainedEvidenceCount").is_none());
    assert!(data.get("evidenceId").is_none());
    assert_eq!(files(fixture.root.path())?, before);
    assert!(!fixture.root.path().join(".ee").exists());
    Ok(())
}

#[test]
fn session_full_cli_commits_one_receipt_preserves_other_producers_and_repeats_as_noop() -> TestResult {
    let fixture = Fixture::new()?;
    let db = DbConnection::open_file_read_only(&fixture.database)?;
    let untouched = db.get_evidence_span(&fixture.evidence[2])?;
    db.close()?;
    let response: serde_json::Value = serde_json::from_str(&dispatch(fixture.args("json", false))?)?;
    let data = &response["data"];
    assert_eq!(data["status"], "withdrawn");
    assert_eq!(data["persisted"], true);
    assert_eq!(data["changed"], true);
    assert_eq!(data["retainedEvidenceCount"], 3);
    assert_eq!(data["cassEvidenceCount"], 2);
    assert_eq!(data["excludedProducerCount"], 1);
    assert_eq!(data["withdrawnCount"], 2);
    assert_eq!(data["indexStatus"], "queued");
    assert_eq!(data["scope"], "retained_sources");
    assert_eq!(data["futureImports"], "unchanged");
    assert!(data["auditId"].is_string() && data["indexJobId"].is_string());
    for preview in [true, false] {
        let repeated: serde_json::Value = serde_json::from_str(&dispatch(fixture.args("json", preview))?)?;
        assert_eq!(repeated["data"]["status"], "already_withdrawn");
        assert_eq!(repeated["data"]["changed"], false);
        assert_eq!(repeated["data"]["wouldWithdrawCount"], 0);
        assert_eq!(repeated["data"]["withdrawnCount"], 0);
        assert_eq!(repeated["data"]["alreadyWithdrawnCount"], 2);
        assert_eq!(repeated["data"]["indexStatus"], "not_scheduled");
        assert!(repeated["data"]["auditId"].is_null());
        assert!(repeated["data"]["indexJobId"].is_null());
    }
    let db = DbConnection::open_file_read_only(&fixture.database)?;
    assert_eq!(db.list_audit_entries(Some(&fixture.workspace), None)?.len(), 1);
    let jobs = db.list_search_index_jobs(&fixture.workspace, None)?;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, "pending");
    assert_eq!(jobs[0].document_id.as_deref(), Some(fixture.session.as_str()));
    for id in &fixture.evidence[..2] {
        assert!(db.get_search_admitted_evidence_span(id, &fixture.workspace)?.is_none());
        assert_eq!(db.get_evidence_span(id)?.ok_or("retained evidence")?.excerpt, SOURCE);
    }
    assert_eq!(db.get_evidence_span(&fixture.evidence[2])?, untouched);
    assert!(!fixture.root.path().join(".ee").exists());
    Ok(())
}

#[test]
fn session_toon_and_human_preview_keep_scope_and_actual_mutation_explicit() -> TestResult {
    let fixture = Fixture::new()?;
    let json: serde_json::Value = serde_json::from_str(&dispatch(fixture.args("json", true))?)?;
    let toon = dispatch(fixture.args("toon", true))?;
    let decoded = toon::try_decode(&toon, None).map_err(|error| error.to_string())?;
    let decoded = serde_json::Value::from(decoded);
    let expected = serde_json::Value::from(toon::JsonValue::from(json));
    assert_eq!(decoded["data"], expected["data"]);
    assert_eq!(decoded["success"], true);
    let human = dispatch(fixture.args("human", true))?;
    assert!(human.contains("Session evidence would be withdrawn"));
    assert!(human.contains("Would withdraw: 2"));
    assert!(human.contains("Withdrawn this invocation: 0"));
    assert!(human.contains("future imports are unchanged"));
    assert!(human.contains("no changes persisted and no index job queued"));
    let db = DbConnection::open_file_read_only(&fixture.database)?;
    assert!(db.list_audit_entries(Some(&fixture.workspace), None)?.is_empty());
    assert!(db.list_search_index_jobs(&fixture.workspace, None)?.is_empty());
    Ok(())
}
