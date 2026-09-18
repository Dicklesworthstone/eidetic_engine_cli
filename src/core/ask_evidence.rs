//! Direct CASS evidence for extractive answers, without manufacturing memories.
//!
//! Only the database's current-snapshot positive-admission visitor may feed
//! this loader. Historical excerpts retain their native identity, revision and
//! exact bytes; they never inherit a linked memory's trust or scope.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use crate::core::ask::{AskCandidate, AskNativeSource};
use crate::db::{DbConnection, StoredEvidenceSpan};
use crate::models::{DomainError, EvidenceId, MemoryId, MemoryScope, SessionId, TrustClass};
use crate::pack::PackEntityRef;

/// Append positively admitted transcript sources inside the caller's snapshot.
/// Narrow scopes deliberately match search: transcript attribution is not a
/// verified memory, a global tag, or authenticated self/team membership.
pub(crate) fn load_candidates(
    connection: &DbConnection,
    workspace_id: &str,
    scope: MemoryScope,
    candidates: &mut Vec<AskCandidate>,
    native_sources: &mut BTreeMap<String, AskNativeSource>,
) -> Result<(), DomainError> {
    if !matches!(scope, MemoryScope::Workspace | MemoryScope::Swarm) {
        return Ok(());
    }
    let admitted_memories: BTreeSet<_> = candidates
        .iter()
        .filter(|candidate| MemoryId::from_str(&candidate.memory_id).is_ok())
        .map(|candidate| candidate.memory_id.clone())
        .collect();
    connection
        .visit_search_admitted_evidence_spans_in_current_snapshot(workspace_id, |span| {
            // An attached excerpt must not resurrect an excluded, retired,
            // sealed, missing or foreign-workspace memory through another ID.
            if span
                .memory_id
                .as_ref()
                .is_some_and(|id| !admitted_memories.contains(id))
            {
                return Ok(());
            }
            if let Some((candidate, source)) = candidate(span) {
                native_sources.insert(candidate.memory_id.clone(), source);
                candidates.push(candidate);
            }
            Ok(())
        })
        .map_err(|_| DomainError::Storage {
            message: "Could not read admitted transcript evidence; answer withheld".to_owned(),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    Ok(())
}

fn candidate(span: StoredEvidenceSpan) -> Option<(AskCandidate, AskNativeSource)> {
    let evidence_id = EvidenceId::from_str(&span.id).ok()?;
    SessionId::from_str(&span.session_id).ok()?;
    if span.start_line == 0
        || span.end_line < span.start_line
        || span.excerpt.trim().is_empty()
        || span.excerpt == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
        || !super::public_text(&span.excerpt)
    {
        return None;
    }
    let provenance = span.canonical_provenance_uri();
    if !super::public_text(&provenance) {
        return None;
    }
    let mut source_memory_ids = Vec::new();
    if let Some(id) = &span.memory_id {
        MemoryId::from_str(id).ok()?;
        source_memory_ids.push(id.clone());
    }
    let source = AskNativeSource {
        entity: PackEntityRef::EvidenceSpan(evidence_id),
        entity_revision: span.pack_entity_revision(),
        source_memory_ids,
    };
    Some((
        AskCandidate {
            memory_id: span.id,
            content: span.excerpt,
            // Evidence has no learned posterior. Use the neutral prior and
            // the existing CassEvidence tilt, not a fabricated human score.
            confidence: 0.5,
            trust_class: TrustClass::CassEvidence.as_str().to_owned(),
            provenance_uri: Some(provenance),
            level: "episodic".to_owned(),
            kind: "evidence_span".to_owned(),
            team_provenance: None,
        },
        source,
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::core::ask::{
        AskCorpus, AskRequest, ask_data_json, evaluate_ask, load_current_ask_corpus,
        load_scoped_ask_corpus, render_ask_markdown,
    };
    use crate::db::{
        CreateEvidenceSpanInput, CreateMemoryInput, CreateSessionInput, CreateWorkspaceInput,
        EvidenceProducerKind,
    };
    use crate::models::WorkspaceId;
    use chrono::Utc;

    const BODY: &str = "Run cargo fmt before release.";
    const QUESTION: &str = "Run cargo fmt before release";

    fn fixture() -> (tempfile::TempDir, DbConnection, String) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".ee")).unwrap();
        let db = DbConnection::open_file(&root.path().join(".ee/ee.db")).unwrap();
        db.migrate().unwrap();
        let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_u128(77001)).to_string();
        db.insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: root.path().to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        (root, db, workspace)
    }

    fn session(db: &DbConnection, workspace: &str, number: u128) -> String {
        let id = SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        db.insert_session(
            &id,
            &CreateSessionInput {
                workspace_id: workspace.to_owned(),
                cass_session_id: format!("private-upstream-{number}"),
                source_path: Some("/home/private/transcript.jsonl".to_owned()),
                agent_name: Some("Alice".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 1,
                token_count: None,
                content_hash: format!("blake3:{}", "a".repeat(64)),
                metadata_json: None,
            },
        )
        .unwrap();
        id
    }

    fn evidence(
        db: &DbConnection,
        workspace: &str,
        session: &str,
        number: u128,
        body: &str,
        parent: Option<&str>,
    ) -> StoredEvidenceSpan {
        let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        db.insert_evidence_span(
            &id,
            &CreateEvidenceSpanInput {
                workspace_id: workspace.to_owned(),
                session_id: session.to_owned(),
                memory_id: parent.map(str::to_owned),
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: format!("upstream-span-{number}"),
                span_kind: "message".to_owned(),
                start_line: 7,
                end_line: 8,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                excerpt: body.to_owned(),
                content_hash: format!("blake3:{}", blake3::hash(body.as_bytes()).to_hex()),
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )
        .unwrap();
        db.get_evidence_span(&id).unwrap().unwrap()
    }

    fn corpus(db: &DbConnection, workspace: &str) -> AskCorpus {
        load_current_ask_corpus(db, workspace, Utc::now()).unwrap()
    }

    fn request(corpus: &AskCorpus) -> AskRequest {
        AskRequest {
            question: QUESTION.to_owned(),
            native_sources: corpus.native_sources.clone(),
            contradictions: corpus.contradictions.clone(),
            ..AskRequest::default()
        }
    }

    #[test]
    fn native_transcript_answer_preserves_identity_revision_and_utf8_offsets() {
        let (_root, db, workspace) = fixture();
        let session = session(&db, &workspace, 1);
        let row = evidence(
            &db,
            &workspace,
            &session,
            2,
            "Café notes. Run cargo fmt before release.",
            None,
        );
        let corpus = corpus(&db, &workspace);
        assert_eq!(corpus.candidates.len(), 1);
        let report = evaluate_ask(&request(&corpus), &corpus.candidates);
        assert!(!report.abstained);
        let data = ask_data_json(&report);
        let citation = &data["citations"][0];
        assert_eq!(citation["entityKind"], "evidence_span");
        assert_eq!(citation["entityId"], row.id);
        assert_eq!(citation["evidenceId"], row.id);
        assert_eq!(citation["entityRevision"], row.pack_entity_revision());
        assert!(citation.get("memoryId").is_none());
        assert_eq!(citation["provenanceUri"], row.canonical_provenance_uri());
        assert_eq!(citation["trustClass"], "cass_evidence");
        assert_eq!(citation["confidence"], 0.5);
        for citation in &report.citations {
            assert_eq!(
                row.excerpt.get(citation.byte_start..citation.byte_end),
                Some(citation.text.as_str())
            );
        }
        assert!(render_ask_markdown(&report).contains(&row.id));
        assert!(!data.to_string().contains("private-upstream"));
        assert!(!data.to_string().contains("/home/private"));
        assert_eq!(db.get_evidence_span(&row.id).unwrap().unwrap(), row);
        assert!(db.list_memories(&workspace, None, true).unwrap().is_empty());
    }

    #[test]
    fn denied_tampered_and_private_evidence_cannot_leak_into_hints() {
        let (_root, db, workspace) = fixture();
        let session = session(&db, &workspace, 1);
        let safe = evidence(&db, &workspace, &session, 2, BODY, None);
        let denied = evidence(
            &db,
            &workspace,
            &session,
            3,
            "Run cargo fmt with denied-canary.",
            None,
        );
        let tampered = evidence(
            &db,
            &workspace,
            &session,
            4,
            "Run cargo fmt with tampered-canary.",
            None,
        );
        evidence(
            &db,
            &workspace,
            &session,
            5,
            "Run cargo fmt using file:///home/private/private-canary.",
            None,
        );
        db.execute_raw(&format!(
            "UPDATE evidence_spans SET search_eligibility = 'denied' WHERE id = '{}'",
            denied.id
        ))
        .unwrap();
        db.execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = 'tampered-canary cargo fmt' WHERE id = '{}'",
            tampered.id
        ))
        .unwrap();
        let corpus = corpus(&db, &workspace);
        assert_eq!(corpus.candidates.len(), 1);
        assert_eq!(corpus.candidates[0].memory_id, safe.id);
        let mut request = request(&corpus);
        request.min_confidence = 1.0;
        let report = evaluate_ask(&request, &corpus.candidates);
        assert!(report.abstained);
        let output = ask_data_json(&report).to_string();
        assert!(output.contains(&safe.id));
        for forbidden in [
            "denied-canary",
            "tampered-canary",
            "private-canary",
            "private-upstream",
            "/home/private",
        ] {
            assert!(!output.contains(forbidden));
            assert!(!render_ask_markdown(&report).contains(forbidden));
        }
    }

    #[test]
    fn transcript_labels_do_not_grant_narrow_memory_scope() {
        let (_root, db, workspace) = fixture();
        let session = session(&db, &workspace, 1);
        evidence(&db, &workspace, &session, 2, BODY, None);
        for scope in [
            MemoryScope::SelfOnly,
            MemoryScope::Team,
            MemoryScope::Verified,
            MemoryScope::Global,
        ] {
            let corpus = load_scoped_ask_corpus(&db, &workspace, Utc::now(), scope).unwrap();
            assert!(corpus.candidates.is_empty());
            assert!(corpus.native_sources.is_empty());
        }
        assert_eq!(
            load_scoped_ask_corpus(&db, &workspace, Utc::now(), MemoryScope::Swarm)
                .unwrap()
                .candidates
                .len(),
            1
        );
    }

    #[test]
    fn one_session_is_one_support_group_but_independent_sessions_can_corroborate() {
        let (_root, db, workspace) = fixture();
        let first = session(&db, &workspace, 1);
        evidence(&db, &workspace, &first, 2, BODY, None);
        let initial = corpus(&db, &workspace);
        let baseline = evaluate_ask(&request(&initial), &initial.candidates);
        for number in 3..20 {
            evidence(&db, &workspace, &first, number, BODY, None);
        }
        let repeated = corpus(&db, &workspace);
        let report = evaluate_ask(&request(&repeated), &repeated.candidates);
        assert_eq!(report.confidence.to_bits(), baseline.confidence.to_bits());
        let mut reversed = repeated.candidates.clone();
        reversed.reverse();
        assert_eq!(
            ask_data_json(&report),
            ask_data_json(&evaluate_ask(&request(&repeated), &reversed))
        );
        let second = session(&db, &workspace, 21);
        evidence(&db, &workspace, &second, 22, BODY, None);
        let independent = corpus(&db, &workspace);
        assert!(
            evaluate_ask(&request(&independent), &independent.candidates).confidence
                > report.confidence
        );
    }

    #[test]
    fn linked_excerpts_cannot_resurrect_retired_memories_or_multiply_their_votes() {
        let (_root, db, workspace) = fixture();
        let memory = MemoryId::from_uuid(uuid::Uuid::from_u128(1)).to_string();
        db.insert_memory(
            &memory,
            &CreateMemoryInput {
                workspace_id: workspace.clone(),
                content: BODY.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.5,
                importance: 0.5,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                provenance_uri: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .unwrap();
        let initial = corpus(&db, &workspace);
        let baseline = evaluate_ask(&request(&initial), &initial.candidates);
        let session = session(&db, &workspace, 2);
        evidence(&db, &workspace, &session, 3, BODY, Some(&memory));
        let linked = corpus(&db, &workspace);
        assert_eq!(linked.candidates.len(), 2);
        assert_eq!(
            evaluate_ask(&request(&linked), &linked.candidates)
                .confidence
                .to_bits(),
            baseline.confidence.to_bits()
        );
        db.execute_raw(&format!(
            "UPDATE memories SET valid_to = '2001-01-01T00:00:00Z' WHERE id = '{memory}'"
        ))
        .unwrap();
        assert!(corpus(&db, &workspace).candidates.is_empty());
    }

    #[test]
    fn evidence_revocation_during_read_is_visible_only_to_the_next_snapshot() {
        let (root, db, workspace) = fixture();
        let session = session(&db, &workspace, 1);
        let row = evidence(&db, &workspace, &session, 2, BODY, None);
        let writer = DbConnection::open_file(&root.path().join(".ee/ee.db")).unwrap();
        let pinned = crate::core::ask::corpus::load_corpus_with_boundary(&db, &workspace, Utc::now(), || {
            writer.with_transaction(|| writer.execute_raw(&format!("UPDATE evidence_spans SET search_eligibility = 'denied' WHERE id = '{}'", row.id))).unwrap();
            Ok(())
        }).unwrap();
        assert_eq!(pinned.candidates.len(), 1);
        assert!(corpus(&db, &workspace).candidates.is_empty());
    }

    #[test]
    fn missing_evidence_storage_fails_closed_and_releases_the_snapshot() {
        let (_root, db, workspace) = fixture();
        db.execute_raw("ALTER TABLE evidence_spans RENAME TO unavailable_evidence")
            .unwrap();
        let error = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap_err();
        assert!(!error.message().contains("unavailable_evidence"));
        db.begin_read_snapshot().unwrap();
        db.commit_read_snapshot().unwrap();
    }

    #[test]
    fn evidence_after_the_old_candidate_limit_is_still_answerable() {
        let (_root, db, workspace) = fixture();
        let session = session(&db, &workspace, 1);
        db.with_transaction(|| {
            for number in 2..530 {
                evidence(
                    &db,
                    &workspace,
                    &session,
                    number,
                    "Unrelated orchard inventory.",
                    None,
                );
            }
            Ok(())
        })
        .unwrap();
        let target = evidence(&db, &workspace, &session, 530, BODY, None);
        let corpus = corpus(&db, &workspace);
        assert_eq!(corpus.candidates.len(), 529);
        let report = evaluate_ask(&request(&corpus), &corpus.candidates);
        assert!(!report.abstained);
        assert_eq!(report.citations[0].memory_id, target.id);
    }

    #[test]
    fn native_evidence_requires_transcript_provenance_and_cannot_claim_human_trust() {
        let (_root, db, workspace) = fixture();
        let session = session(&db, &workspace, 1);
        evidence(&db, &workspace, &session, 2, BODY, None);
        let corpus = corpus(&db, &workspace);
        for field in ["trust", "provenance"] {
            let mut candidates = corpus.candidates.clone();
            if field == "trust" {
                candidates[0].trust_class = "human_explicit".to_owned();
            } else {
                candidates[0].provenance_uri = Some("manual://not-a-transcript".to_owned());
            }
            let report = evaluate_ask(&request(&corpus), &candidates);
            assert!(report.extractiveness_violated);
            assert!(report.citations.is_empty());
        }
    }
}
