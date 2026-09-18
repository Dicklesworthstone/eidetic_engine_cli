//! Native CASS answers through the actual CLI; all rows use the real store API.
use super::*;
use ee::db::{
    CreateEvidenceSpanInput, CreateSessionInput, EvidenceProducerKind, StoredEvidenceSpan,
};
use ee::models::{EvidenceId, SessionId};
use std::path::{Path, PathBuf};

const TEXT: &str = "Café release notes. Run cargo fmt before release.";
const QUESTION: &str = "Run cargo fmt before release";

fn run(workspace: &Path, flags: &[&str]) -> Result<Value, String> {
    let output = crate::common_spawn::serialized_real_ee_with(|command| {
        command
            .env_remove("EE_AGENT_NAME")
            .env("EE_CASS_BINARY", "/nonexistent-cass-for-offline-ask")
            .arg("--json")
            .arg("--workspace")
            .arg(workspace)
            .arg("ask")
            .arg(QUESTION)
            .args(flags);
    })
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "ask {flags:?} failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    assert_eq!(value["success"], true);
    Ok(value)
}

fn fixture() -> Result<
    (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        String,
        StoredEvidenceSpan,
    ),
    String,
> {
    let (root, workspace, database) = super::super::build_empty_workspace()?;
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let workspace_id = db
        .get_workspace_by_path(&workspace.to_string_lossy())
        .map_err(|error| error.to_string())?
        .ok_or("missing workspace")?
        .id;
    let session = SessionId::from_uuid(uuid::Uuid::from_u128(78001)).to_string();
    db.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: workspace_id.clone(),
            cass_session_id: "private-source-session".to_owned(),
            source_path: Some("/home/private/transcript.jsonl".to_owned()),
            agent_name: Some("Alice".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 1,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(TEXT.as_bytes()).to_hex()),
            metadata_json: None,
        },
    )
    .map_err(|error| error.to_string())?;
    let row = insert(&db, &workspace_id, &session, 78002, TEXT)?;
    Ok((root, workspace, database, workspace_id, row))
}

fn insert(
    db: &DbConnection,
    workspace: &str,
    session: &str,
    number: u128,
    text: &str,
) -> Result<StoredEvidenceSpan, String> {
    let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
    db.insert_evidence_span(
        &id,
        &CreateEvidenceSpanInput {
            workspace_id: workspace.to_owned(),
            session_id: session.to_owned(),
            memory_id: None,
            producer_kind: EvidenceProducerKind::CassImport,
            cass_span_id: format!("private-source-span-{number}"),
            span_kind: "message".to_owned(),
            start_line: 12,
            end_line: 13,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".to_owned()),
            excerpt: text.to_owned(),
            content_hash: format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex()),
            metadata_json: None,
            inherited_redaction_classes: Vec::new(),
        },
    )
    .map_err(|error| error.to_string())?;
    db.get_evidence_span(&id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "missing evidence".to_owned())
}

fn check_citation(citation: &Value, row: &StoredEvidenceSpan) -> Result<(), String> {
    assert_eq!(citation["entityKind"], "evidence_span");
    assert_eq!(citation["entityId"], row.id);
    assert_eq!(citation["evidenceId"], row.id);
    assert_eq!(citation["entityRevision"], row.pack_entity_revision());
    assert_eq!(citation["provenanceUri"], row.canonical_provenance_uri());
    assert_eq!(citation["trustClass"], "cass_evidence");
    assert!(citation.get("memoryId").is_none());
    assert!(citation.get("ruleId").is_none());
    let start = citation["span"]["byteStart"]
        .as_u64()
        .ok_or("missing byteStart")? as usize;
    let end = citation["span"]["byteEnd"]
        .as_u64()
        .ok_or("missing byteEnd")? as usize;
    assert_eq!(row.excerpt.get(start..end), citation["text"].as_str());
    Ok(())
}

#[test]
fn public_transcript_answers_and_hints_keep_native_identity_without_mutation() -> Result<(), String>
{
    let (_root, workspace, database, workspace_id, row) = fixture()?;
    let before_db = std::fs::read(&database).map_err(|error| error.to_string())?;
    let before = DbConnection::open_file_read_only(&database)
        .map_err(|error| error.to_string())?
        .list_audit_entries(Some(&workspace_id), None)
        .map_err(|error| error.to_string())?;
    let answer = run(&workspace, &["--read-only"])?;
    assert_eq!(answer["data"]["abstained"], false);
    assert_eq!(answer["data"]["candidatesScanned"], 1);
    check_citation(&answer["data"]["citations"][0], &row)?;
    assert_eq!(answer, run(&workspace, &["--read-only"])?);
    let weak = run(&workspace, &["--read-only", "--min-confidence", "1"])?;
    assert_eq!(weak["data"]["abstained"], true);
    assert_eq!(weak["data"]["nearestEvidence"][0]["evidenceId"], row.id);
    assert_eq!(
        weak["data"]["queryAssist"]["didYouMean"][0]["entityKind"],
        "evidence_span"
    );
    assert_eq!(
        weak["data"]["queryAssist"]["reformulations"][0]["matchedEvidenceId"],
        row.id
    );
    for value in [answer, weak] {
        for forbidden in [
            "private-source",
            "/home/private",
            "\"memoryId\"",
            "\"ruleId\"",
        ] {
            assert!(!value.to_string().contains(forbidden));
        }
    }
    let db = DbConnection::open_file_read_only(&database).map_err(|error| error.to_string())?;
    assert_eq!(
        db.get_evidence_span(&row.id)
            .map_err(|error| error.to_string())?,
        Some(row)
    );
    assert_eq!(
        db.list_audit_entries(Some(&workspace_id), None)
            .map_err(|error| error.to_string())?,
        before
    );
    assert!(
        db.list_memories(&workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    assert_eq!(
        std::fs::read(&database).map_err(|error| error.to_string())?,
        before_db
    );
    Ok(())
}

#[test]
fn ordinary_transcript_answers_audit_evidence_not_a_surrogate_memory() -> Result<(), String> {
    let (_root, workspace, database, workspace_id, row) = fixture()?;
    let dry = run(&workspace, &["--read-only"])?;
    assert_eq!(dry, run(&workspace, &[])?);
    let db = DbConnection::open_file_read_only(&database).map_err(|error| error.to_string())?;
    let audits = db
        .list_audit_by_target("evidence", &row.id, None)
        .map_err(|error| error.to_string())?;
    let reads: Vec<_> = audits
        .iter()
        .filter(|entry| entry.action == ee::db::audit_actions::SEARCH_RETURNED_MEM)
        .collect();
    assert_eq!(reads.len(), 1);
    let detail: Value =
        serde_json::from_str(reads[0].details.as_deref().ok_or("missing audit detail")?)
            .map_err(|error| error.to_string())?;
    assert_eq!(detail["evidenceId"], row.id);
    assert_eq!(detail["entityRevision"], row.pack_entity_revision());
    assert!(detail.get("memoryId").is_none());
    assert!(
        db.list_audit_by_target("memory", &row.id, None)
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    assert!(
        db.list_memories(&workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    assert_eq!(
        db.get_evidence_span(&row.id)
            .map_err(|error| error.to_string())?,
        Some(row)
    );
    Ok(())
}

#[test]
fn public_transcript_scope_and_revocation_are_checked_on_every_invocation() -> Result<(), String> {
    let (_root, workspace, database, _workspace_id, row) = fixture()?;
    for scope in ["self", "team", "verified", "global"] {
        let answer = run(&workspace, &["--read-only", "--memory-scope", scope])?;
        assert_eq!(answer["data"]["abstained"], true, "{scope}");
        assert_eq!(answer["data"]["candidatesScanned"], 0, "{scope}");
        assert!(!answer.to_string().contains(&row.id));
    }
    let swarm = run(&workspace, &["--read-only", "--memory-scope", "swarm"])?;
    check_citation(&swarm["data"]["citations"][0], &row)?;
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    db.execute_raw(&format!(
        "UPDATE evidence_spans SET search_eligibility = 'denied' WHERE id = '{}'",
        row.id
    ))
    .map_err(|error| error.to_string())?;
    drop(db);
    let denied = run(&workspace, &["--read-only", "--min-confidence", "0"])?;
    assert_eq!(denied["data"]["abstained"], true);
    assert_eq!(denied["data"]["candidatesScanned"], 0);
    assert!(!denied.to_string().contains(&row.id));
    Ok(())
}

#[test]
fn opposing_transcript_excerpts_keep_both_cited_sides_without_human_trust() -> Result<(), String> {
    let (_root, workspace, database, workspace_id, yes) = fixture()?;
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let no = insert(
        &db,
        &workspace_id,
        &yes.session_id,
        78003,
        "Do not run cargo fmt before release.",
    )?;
    drop(db);
    let answer = run(&workspace, &["--read-only"])?;
    assert_eq!(answer["data"]["_conflictDetected"], true);
    assert!(answer["data"]["answerText"].is_null());
    let sides = answer["data"]["sides"].as_array().ok_or("missing sides")?;
    assert_eq!(sides.len(), 2);
    let mut cited = std::collections::BTreeSet::new();
    for side in sides {
        for citation in side["citations"].as_array().ok_or("missing citations")? {
            let id = citation["evidenceId"]
                .as_str()
                .ok_or("missing evidenceId")?;
            assert!(id == yes.id || id == no.id);
            check_citation(citation, if id == yes.id { &yes } else { &no })?;
            cited.insert(id.to_owned());
        }
    }
    assert_eq!(cited, [yes.id, no.id].into_iter().collect());
    Ok(())
}
