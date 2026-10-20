//! Recovered debugging sessions through the public proposal workflow.

use super::*;
use crate::core::error_diagnosis::{ErrorRepairLinkRecording, record_error_repair_links};
use crate::core::error_recall::from_rustc;
use crate::core::init::{InitOptions, init_workspace};
use crate::db::{CreateEvidenceSpanInput, CreateSessionInput};
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const COMMAND: &str = "cargo check";
const FAILURE: &str =
    "error[E0277]: the trait bound `Widget: Serialize` is not satisfied\n --> src/widget.rs:3:5\n";
const NOTICE: &str = "The earlier attempt did not fix E0277.";
const FIX: &str = "I fixed E0277 by deriving Serialize for Widget before writing the value to the store in src/widget.rs.";

struct Workspace {
    _temp: tempfile::TempDir,
    path: PathBuf,
    database: PathBuf,
    id: String,
    db: DbConnection,
}

struct Episode {
    session: String,
    repair: String,
    last_line: u32,
    seed: u128,
}

impl Workspace {
    fn new() -> TestResult<Self> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().canonicalize()?;
        let report = init_workspace(&InitOptions {
            workspace_path: path.clone(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        });
        assert!(report.status.is_success(), "{report:?}");
        let db = DbConnection::open_file(&report.database_path)?;
        let workspace = db
            .get_workspace_by_path(&path.to_string_lossy())?
            .ok_or("workspace")?;
        Ok(Self {
            _temp: temp,
            path,
            database: report.database_path,
            id: workspace.id,
            db,
        })
    }

    fn row(
        &self,
        episode: &mut Episode,
        kind: &str,
        role: &str,
        block: Value,
    ) -> TestResult<String> {
        episode.last_line += 1;
        let line = episode.last_line;
        let id =
            EvidenceId::from_uuid(uuid::Uuid::from_u128(episode.seed * 100 + u128::from(line)))
                .to_string();
        let excerpt = json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
        self.db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: self.id.clone(),
                session_id: episode.session.clone(),
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("{}:{line}", episode.session),
                span_kind: kind.to_owned(),
                start_line: line,
                end_line: line,
                start_byte: None,
                end_byte: None,
                role: Some(role.to_owned()),
                content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                excerpt,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )?;
        Ok(id)
    }

    fn message(&self, episode: &mut Episode, text: &str) -> TestResult<String> {
        self.row(
            episode,
            "message",
            "assistant",
            json!({"type":"text","text":text}),
        )
    }

    fn episode(
        &self,
        seed: u128,
        context: &str,
        before: &[&str],
        after: &[&str],
        retry: &str,
        passed: bool,
    ) -> TestResult<Episode> {
        let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
        self.db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: self.id.clone(),
                cass_session_id: format!("/sessions/recovery-{seed}.jsonl"),
                source_path: Some(format!("/sessions/recovery-{seed}.jsonl")),
                agent_name: Some("claude_code".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: u32::try_from(6 + before.len() + after.len())?,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(context.as_bytes()).to_hex()),
                metadata_json: None,
            },
        )?;
        let mut episode = Episode {
            session,
            repair: String::new(),
            last_line: 0,
            seed,
        };
        self.row(
            &mut episode,
            "message",
            "user",
            json!({"type":"text","text":context}),
        )?;
        self.row(
            &mut episode,
            "tool_call",
            "assistant",
            json!({
                "type":"tool_use","id":"failed-run","name":"Bash","input":{"command":COMMAND}
            }),
        )?;
        self.row(
            &mut episode,
            "tool_result",
            "user",
            json!({
                "type":"tool_result","tool_use_id":"failed-run","content":FAILURE,"is_error":true
            }),
        )?;
        for text in before {
            self.message(&mut episode, text)?;
        }
        episode.repair = self.message(&mut episode, FIX)?;
        for text in after {
            self.message(&mut episode, text)?;
        }
        self.row(
            &mut episode,
            "tool_call",
            "assistant",
            json!({
                "type":"tool_use","id":"retry","name":"Bash","input":{"command":retry}
            }),
        )?;
        self.row(&mut episode, "tool_result", "user", json!({
            "type":"tool_result","tool_use_id":"retry","content":if passed {""} else {FAILURE},"is_error":!passed
        }))?;
        Ok(episode)
    }

    fn review(
        &self,
        episode: &Episode,
        floor: f32,
        persist: bool,
    ) -> TestResult<ReviewSessionReport> {
        review_session_proposals(&ReviewSessionOptions {
            workspace_path: &self.path,
            database_path: Some(&self.database),
            session_id: Some(&episode.session),
            propose: persist,
            dry_run: !persist,
            min_confidence: floor,
            limit: 100,
        })
        .map_err(|error| error.message().into())
    }
}

fn native(report: &ReviewSessionReport) -> Vec<&ReviewSessionCandidate> {
    report
        .candidates
        .iter()
        .filter(|candidate| {
            candidate
                .proposed_content
                .starts_with("Observed repair for ")
        })
        .collect()
}

#[test]
fn earlier_failed_attempt_does_not_discard_a_later_source_proven_lesson() -> TestResult {
    let workspace = Workspace::new()?;
    let episode = workspace.episode(81_001, "Work item alpha.", &[NOTICE], &[], COMMAND, true)?;
    let session = workspace
        .db
        .get_session(&episode.session)?
        .ok_or("session")?;
    let sources = workspace
        .db
        .list_evidence_spans_for_session(&episode.session)?;
    assert!(admitted_session(&workspace.id, &session, &sources));
    assert!(
        !eligible_session(&workspace.id, &session, &sources),
        "old whole-session veto"
    );
    let report = workspace.review(&episode, 0.6, false)?;
    let proposed = native(&report);
    assert_eq!(proposed.len(), 1);
    assert_eq!(
        report.candidates.len(),
        1,
        "do not rehabilitate prose-derived lessons"
    );
    assert_eq!(proposed[0].confidence, 0.6);
    assert_eq!(proposed[0].source_ids, vec![episode.repair.clone()]);
    assert!(proposed[0].proposed_content.contains(FIX));
    assert!(!proposed[0].proposed_content.contains(NOTICE));
    validate_draft(&workspace.db, &workspace.id, proposed[0]).map_err(|error| error.message)?;
    let written = workspace.review(&episode, 0.6, true)?;
    assert!(written.durable_mutation);
    assert_eq!(native(&written)[0].candidate_id, proposed[0].candidate_id);
    let stored = workspace
        .db
        .get_curation_candidate(&workspace.id, &proposed[0].candidate_id)?
        .ok_or("stored")?;
    validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
    let package: Value = serde_json::from_str(
        stored
            .derivation_metadata_json
            .as_deref()
            .ok_or("package")?,
    )?;
    let context = package
        .pointer("/producer/producerPayload/corroboration/sessions/0/contextSources")
        .and_then(Value::as_array)
        .ok_or("context")?;
    assert_eq!(
        context.len(),
        sources.len(),
        "failed-attempt context remains committed"
    );
    assert!(!workspace.review(&episode, 0.6, true)?.durable_mutation);
    assert_eq!(
        workspace
            .db
            .list_evidence_spans_for_session(&episode.session)?,
        sources
    );
    assert!(
        workspace
            .db
            .list_memories(&workspace.id, None, true)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn later_denials_retractions_and_unverified_retries_remain_abstentions_at_zero_floor() -> TestResult
{
    let workspace = Workspace::new()?;
    for (seed, before, after, retry, passed) in [
        (82_001, vec!["This advice is wrong."], vec![], COMMAND, true),
        (82_002, vec![], vec![NOTICE], COMMAND, true),
        (
            82_003,
            vec![NOTICE],
            vec!["The recommendation was retracted."],
            COMMAND,
            true,
        ),
        (82_004, vec![NOTICE], vec![], "cargo check --lib", true),
        (82_005, vec![NOTICE], vec![], COMMAND, false),
    ] {
        let episode =
            workspace.episode(seed, "Work item denied.", &before, &after, retry, passed)?;
        let report = workspace.review(&episode, 0.0, false)?;
        assert!(
            report.candidates.is_empty(),
            "{seed}: {:?}",
            report.candidates
        );
        assert!(!report.durable_mutation);
    }
    assert!(
        workspace
            .db
            .list_memories(&workspace.id, None, true)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn recovered_sessions_corroborate_without_overriding_harmful_supporter_feedback() -> TestResult {
    let workspace = Workspace::new()?;
    let anchor = workspace.episode(83_001, "Work item alpha.", &[NOTICE], &[], COMMAND, true)?;
    workspace.episode(83_002, "Work item alpha.", &[NOTICE], &[], COMMAND, true)?;
    assert!(
        native(&workspace.review(&anchor, 0.7, false)?).is_empty(),
        "copied content is not independent"
    );
    let supporter = workspace.episode(83_003, "Work item beta.", &[NOTICE], &[], COMMAND, true)?;
    let report = workspace.review(&anchor, 0.7, false)?;
    let proposed = native(&report);
    assert_eq!(proposed.len(), 1);
    assert_eq!(proposed[0].confidence, 0.7);
    validate_draft(&workspace.db, &workspace.id, proposed[0]).map_err(|error| error.message)?;
    record_error_repair_links(
        &workspace.db,
        &workspace.id,
        &from_rustc(Some("E0277"), "trait bound failed"),
        &ErrorRepairLinkRecording {
            harmful_repairs: vec![supporter.repair],
            ..Default::default()
        },
    )?;
    assert!(validate_draft(&workspace.db, &workspace.id, proposed[0]).is_err());
    assert!(native(&workspace.review(&anchor, 0.7, false)?).is_empty());
    assert_eq!(native(&workspace.review(&anchor, 0.6, false)?).len(), 1);
    Ok(())
}

#[test]
fn a_later_failure_notice_invalidates_recorded_recovery_without_rewriting_history() -> TestResult {
    let workspace = Workspace::new()?;
    let mut episode =
        workspace.episode(84_001, "Work item alpha.", &[NOTICE], &[], COMMAND, true)?;
    let report = workspace.review(&episode, 0.6, true)?;
    let proposed = native(&report);
    assert_eq!(proposed.len(), 1);
    let stored = workspace
        .db
        .get_curation_candidate(&workspace.id, &proposed[0].candidate_id)?
        .ok_or("stored")?;
    validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
    workspace.message(&mut episode, "The same check failed again.")?;
    assert!(validate_recorded(&workspace.db, &stored).is_err());
    assert!(
        workspace
            .review(&episode, 0.0, false)?
            .candidates
            .is_empty()
    );
    let after = workspace
        .db
        .get_curation_candidate(&workspace.id, &stored.id)?
        .ok_or("history")?;
    assert_eq!(
        after.derivation_metadata_json,
        stored.derivation_metadata_json
    );
    assert_eq!(after.proposed_content, stored.proposed_content);
    assert_eq!(after.confidence, stored.confidence);
    assert!(
        workspace
            .db
            .list_memories(&workspace.id, None, true)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn a_contributing_explanation_before_the_failed_attempt_notice_is_not_rehabilitated() -> TestResult
{
    let workspace = Workspace::new()?;
    let earlier = "I fixed E0277 by adding the Serialize import in src/widget.rs.";
    let episode = workspace.episode(
        85_001,
        "Work item alpha.",
        &[earlier, NOTICE],
        &[],
        COMMAND,
        true,
    )?;
    let session = workspace
        .db
        .get_session(&episode.session)?
        .ok_or("session")?;
    let spans = workspace
        .db
        .list_evidence_spans_for_session(&episode.session)?;
    let arcs =
        crate::core::cass_error_recall::session_failure_arcs(&workspace.id, &session, &spans);
    assert!(
        arcs.iter().any(|arc| arc
            .resolution
            .as_ref()
            .is_some_and(|resolution| resolution.repair_ids.len() == 2)),
        "both turns were contributing explanations"
    );
    let evidence = SessionEvidence::new(&workspace.id, &session, &spans);
    assert!(evidence.native().candidates.is_empty());
    assert!(
        workspace
            .review(&episode, 0.0, false)?
            .candidates
            .is_empty()
    );
    Ok(())
}
