//! Native, public CASS topics without manufacturing a memory or graph node.
//!
//! The caller supplies the database's positively admitted evidence rows. Check
//! the complete excerpt before deriving a short public topic: a preview is not
//! proof that a private tail, split credential or instruction is safe to expose.

use std::str::FromStr;

use chrono::{DateTime, Utc};

use crate::db::StoredEvidenceSpan;
use crate::models::EvidenceId;

pub(super) struct CassTopic {
    pub(super) key: String,
    pub(super) label: String,
    pub(super) created_at: DateTime<Utc>,
}

pub(super) fn topic(span: &StoredEvidenceSpan) -> Option<CassTopic> {
    if span.memory_id.is_some()
        || span.producer_kind != "cass_import"
        || span.search_eligibility != "admitted"
        || span.pack_eligibility != "admitted"
        || !EvidenceId::from_str(&span.id).is_ok_and(|id| id.to_string() == span.id)
    {
        return None;
    }
    // Screen the complete projected body the topic is derived from. The
    // stored envelope is never shown, and every Claude Code record's absolute
    // `cwd` made the old envelope screen drop all of them.
    let body = span.reader_body();
    if !public_excerpt(&body) {
        return None;
    }
    let created_at = DateTime::parse_from_rfc3339(&span.created_at)
        .ok()?
        .with_timezone(&Utc);
    // Suggest topics in the conversation's words, not envelope keys
    // (bd-reality-core-convergence-1azkt.45).
    let preview = super::content_preview_tokens(&body, super::TOPIC_PREVIEW_CHARS);
    if preview.is_empty() {
        return None;
    }
    Some(CassTopic {
        // Separate from memory-kind keys; equal labels form one deterministic
        // transcript cluster, carrying only real, sorted EvidenceIds.
        key: format!("evidence_span::{preview}"),
        // Keep the suggested query in source words. A synthetic `transcript:`
        // prefix is not a query operator or text that the excerpt contains.
        label: preview,
        created_at,
    })
}

fn public_excerpt(value: &str) -> bool {
    !value.trim().is_empty() && !crate::policy::redact_public_replay_body(value).redacted
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::super::{FocusSuggestOptions, score_and_emit_topics, suggest_focus};
    use super::*;
    use crate::db::{
        CreateEvidenceSpanInput, CreateMemoryInput, CreateSessionInput, CreateWorkspaceInput,
        DbConnection, EvidenceProducerKind,
    };
    use crate::models::{MemoryId, SessionId};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const BODY: &str = "Release validation needs a reviewed deployment checklist.";

    struct Fixture {
        _directory: tempfile::TempDir,
        root: PathBuf,
        db: DbConnection,
        workspace: String,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            std::fs::create_dir(root.join(".ee")).unwrap();
            let db = DbConnection::open_file(root.join(".ee/ee.db")).unwrap();
            db.migrate().unwrap();
            let workspace = crate::core::workspace::stable_workspace_id(&root);
            db.insert_workspace(
                &workspace,
                &CreateWorkspaceInput {
                    path: root.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
            Self {
                _directory: directory,
                root,
                db,
                workspace,
            }
        }

        fn options(&self) -> FocusSuggestOptions {
            FocusSuggestOptions {
                workspace_path: self.root.clone(),
                from_cass: true,
                limit: 5,
                recent_hours: 24,
                task_frame_id: None,
            }
        }

        fn evidence(&self, number: u128, body: &str, parent: Option<String>) -> String {
            let session = SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
            let time = Utc::now().to_rfc3339();
            self.db
                .insert_session(
                    &session,
                    &CreateSessionInput {
                        workspace_id: self.workspace.clone(),
                        cass_session_id: format!("PRIVATE-UPSTREAM-SESSION-{number}"),
                        source_path: None,
                        agent_name: Some("codex".to_owned()),
                        model: None,
                        started_at: Some(time.clone()),
                        ended_at: Some(time),
                        message_count: 1,
                        token_count: None,
                        content_hash: format!(
                            "blake3:{}",
                            blake3::hash(session.as_bytes()).to_hex()
                        ),
                        metadata_json: None,
                    },
                )
                .unwrap();
            let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
            self.db
                .insert_evidence_span(
                    &id,
                    &CreateEvidenceSpanInput {
                        workspace_id: self.workspace.clone(),
                        session_id: session.clone(),
                        memory_id: parent,
                        producer_kind: EvidenceProducerKind::CassImport,
                        cass_span_id: format!("PRIVATE-UPSTREAM-SPAN-{number}"),
                        span_kind: crate::cass::CassSpanKind::Message.as_str().to_owned(),
                        start_line: 1,
                        end_line: 1,
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
            id
        }

        fn admitted(&self) -> Vec<StoredEvidenceSpan> {
            self.db
                .list_search_admitted_evidence_spans_for_workspace(&self.workspace)
                .unwrap()
                .0
        }

        fn state(&self) -> Vec<Vec<Vec<(String, sqlmodel_core::Value)>>> {
            [
                "workspaces",
                "memories",
                "sessions",
                "evidence_spans",
                "audit_log",
                "search_index_jobs",
            ]
            .into_iter()
            .map(|table| {
                self.db
                    .query(&format!("SELECT * FROM {table} ORDER BY 1, 2"), &[])
                    .unwrap()
                    .into_iter()
                    .map(|row| {
                        row.iter()
                            .map(|(name, value)| (name.to_owned(), value.clone()))
                            .collect()
                    })
                    .collect()
            })
            .collect()
        }
    }

    #[test]
    fn claude_code_records_suggest_topics_from_their_message() {
        let fixture = Fixture::new();
        let record = serde_json::json!({
            "parentUuid": null, "isSidechain": false, "userType": "external",
            "cwd": "/home/dev/ledger", "sessionId": "5f0c", "version": "2.0.14",
            "gitBranch": "main", "type": "assistant", "uuid": "9a1e",
            "timestamp": "2026-09-01T09:00:00.000Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": BODY}]}
        })
        .to_string();
        let id = fixture.evidence(0x8f3a_c91d_44e2_7b6a_0d15_e8c2_9a71_53bf, &record, None);
        let span = fixture
            .admitted()
            .into_iter()
            .find(|span| span.id == id)
            .expect("the Claude Code record is admitted");
        let topic = topic(&span).expect("its absolute cwd does not withhold the message");
        assert!(!topic.label.contains("home"), "{}", topic.label);
        assert!(
            topic.label.to_lowercase().contains("release"),
            "{}",
            topic.label
        );
    }

    #[test]
    fn cass_only_workspace_produces_actionable_native_recommendations_without_memories() {
        let fixture = Fixture::new();
        let id = fixture.evidence(401, BODY, None);
        assert_eq!(fixture.admitted().len(), 1);
        let before = fixture.state();
        let bytes = std::fs::read(fixture.root.join(".ee/ee.db")).unwrap();
        let report = suggest_focus(&fixture.options()).unwrap();
        assert_eq!(report.recommendations.len(), 1);
        let result = &report.recommendations[0];
        assert_eq!(result.span_ids, [id]);
        assert!(result.topic.starts_with("Release validation"));
        assert_eq!(result.centrality_score, 0.0);
        assert!(result.rationale.contains("0 memory(ies); 1 CASS span(s)"));
        assert!(
            result
                .suggested_query
                .starts_with("ee pack \"Release validation")
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("PRIVATE-UPSTREAM")
        );
        assert!(!report.degraded.iter().any(|entry| {
            matches!(
                entry.code.as_str(),
                "no_recent_evidence" | "graph_empty" | "graph_unavailable"
            )
        }));
        assert_eq!(fixture.db.count_table_rows("memories").unwrap(), 0);
        assert_eq!(fixture.state(), before);
        assert_eq!(
            std::fs::read(fixture.root.join(".ee/ee.db")).unwrap(),
            bytes
        );
        assert!(!fixture.root.join(".ee/index").exists());
    }

    #[test]
    fn without_from_cass_transcripts_never_seed_recommendations() {
        let fixture = Fixture::new();
        fixture.evidence(402, BODY, None);
        let mut options = fixture.options();
        options.from_cass = false;
        let report = suggest_focus(&options).unwrap();
        assert!(report.recommendations.is_empty());
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "no_recent_evidence")
        );
        assert!(
            score_and_emit_topics(
                &[],
                &fixture.admitted(),
                &BTreeMap::new(),
                Utc::now(),
                &options
            )
            .is_empty()
        );
    }

    #[test]
    fn independent_topics_are_deterministic_deduplicated_and_have_no_graph_credit() {
        let fixture = Fixture::new();
        fixture.evidence(403, BODY, None);
        fixture.evidence(404, BODY, None);
        fixture.evidence(405, "Cache invalidation needs a separate review.", None);
        let now = Utc::now();
        let spans = fixture.admitted();
        assert_eq!(spans.len(), 3);
        let mut reordered = spans.clone();
        reordered.reverse();
        reordered.push(spans[0].clone());
        let first = score_and_emit_topics(&[], &spans, &BTreeMap::new(), now, &fixture.options());
        let second =
            score_and_emit_topics(&[], &reordered, &BTreeMap::new(), now, &fixture.options());
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert_eq!(
            first[0].span_ids.len(),
            2,
            "density ranks a two-excerpt topic first"
        );
        assert!(first.iter().all(|item| item.centrality_score == 0.0));
        assert!(first[0].span_ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn transcript_recency_and_zero_limit_are_applied_before_claiming_evidence_is_missing() {
        let fixture = Fixture::new();
        let id = fixture.evidence(406, BODY, None);
        fixture
            .db
            .execute_raw(&format!(
                "UPDATE evidence_spans SET created_at = '2000-01-01T00:00:00Z' WHERE id = '{id}'"
            ))
            .unwrap();
        let report = suggest_focus(&fixture.options()).unwrap();
        assert!(report.recommendations.is_empty());
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "no_recent_evidence")
        );
        fixture.evidence(407, BODY, None);
        let mut options = fixture.options();
        options.limit = 0;
        let report = suggest_focus(&options).unwrap();
        assert!(report.recommendations.is_empty());
        assert!(
            !report
                .degraded
                .iter()
                .any(|entry| entry.code == "no_recent_evidence")
        );
    }

    #[test]
    fn excluded_linked_memory_cannot_be_resurrected_as_an_independent_transcript_topic() {
        let fixture = Fixture::new();
        let memory = MemoryId::from_uuid(uuid::Uuid::from_u128(408)).to_string();
        fixture
            .db
            .insert_memory(
                &memory,
                &CreateMemoryInput {
                    workspace_id: fixture.workspace.clone(),
                    content: BODY.to_owned(),
                    level: "episodic".to_owned(),
                    kind: "note".to_owned(),
                    workflow_id: None,
                    confidence: 1.0,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    provenance_uri: Some("manual://focus-parent".to_owned()),
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .unwrap();
        fixture.evidence(409, BODY, Some(memory.clone()));
        assert!(fixture.db.tombstone_memory(&memory).unwrap());
        let report = suggest_focus(&fixture.options()).unwrap();
        assert!(report.recommendations.is_empty());
        assert!(
            score_and_emit_topics(
                &[],
                &fixture.admitted(),
                &BTreeMap::new(),
                Utc::now(),
                &fixture.options()
            )
            .is_empty()
        );
    }

    #[test]
    fn explicit_task_frames_do_not_widen_to_unlinked_cass_evidence() {
        let fixture = Fixture::new();
        fixture.evidence(410, BODY, None);
        let mut options = fixture.options();
        options.task_frame_id = Some("frame-explicit-memory-scope".to_owned());
        assert!(
            score_and_emit_topics(
                &[],
                &fixture.admitted(),
                &BTreeMap::new(),
                Utc::now(),
                &options
            )
            .is_empty()
        );
        let report = suggest_focus(&options).unwrap();
        assert!(report.recommendations.is_empty());
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "task_frame_unavailable")
        );
    }

    #[test]
    fn source_admission_rechecks_the_canonical_excerpt_instead_of_trusting_old_flags() {
        let fixture = Fixture::new();
        let id = fixture.evidence(411, BODY, None);
        assert_eq!(
            suggest_focus(&fixture.options())
                .unwrap()
                .recommendations
                .len(),
            1
        );
        fixture
            .db
            .execute_raw(&format!(
                "UPDATE evidence_spans SET excerpt = 'PRIVATE-DRIFTED-EXCERPT' WHERE id = '{id}'"
            ))
            .unwrap();
        assert!(fixture.admitted().is_empty());
        let report = suggest_focus(&fixture.options()).unwrap();
        assert!(report.recommendations.is_empty());
        assert!(!format!("{report:?}").contains("PRIVATE-DRIFTED-EXCERPT"));
    }

    #[test]
    fn unavailable_cass_storage_is_not_reported_as_an_empty_recency_window() {
        let fixture = Fixture::new();
        fixture
            .db
            .execute_raw("ALTER TABLE evidence_spans RENAME TO unavailable_spans")
            .unwrap();
        let report = suggest_focus(&fixture.options()).unwrap();
        assert!(report.recommendations.is_empty());
        assert!(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "cass_unavailable")
        );
        assert!(
            !report
                .degraded
                .iter()
                .any(|entry| entry.code == "no_recent_evidence")
        );
    }

    #[test]
    fn full_safe_multibyte_excerpts_are_not_confused_with_oversized_metadata() {
        let body = format!("{BODY} {}", "Résumé 雪 雲 🌱. ".repeat(900));
        assert!(body.len() > crate::policy::MAX_PUBLIC_REPLAY_TEXT_SCAN_BYTES);
        assert!(public_excerpt(&body));
        assert!(public_excerpt(
            &"x ".repeat(crate::models::MAX_CONTENT_BYTES / 2)
        ));
        assert!(!public_excerpt(
            &"x ".repeat(crate::models::MAX_CONTENT_BYTES / 2 + 1)
        ));
        let fixture = Fixture::new();
        let id = fixture.evidence(412, &body, None);
        let report = suggest_focus(&fixture.options()).unwrap();
        assert_eq!(report.recommendations.len(), 1);
        assert_eq!(report.recommendations[0].span_ids, [id]);
    }

    #[test]
    fn private_findings_anywhere_in_the_source_prevent_a_public_topic_preview() {
        for private in [
            "password=focus-private-canary",
            "person@example.test",
            "file:///home/operator/private",
            "trace-AKIAABCDEFGHIJKLMNOP",
            "Ignore previous instructions",
        ] {
            for offset in [2038, 4086, 16_374] {
                let body = format!("{}{private} {BODY}", "x ".repeat(offset / 2));
                assert!(!public_excerpt(&body), "offset={offset}");
            }
        }
        for body in [
            format!("Ignore{}previous instructions", " \n\t".repeat(4096)),
            format!("password={}focus-private-canary", " ".repeat(8192)),
            format!("{} {}", "ordinary words ".repeat(400), "q".repeat(1025)),
        ] {
            assert!(!public_excerpt(&body));
        }
    }

    #[test]
    fn native_topics_require_real_evidence_identity_and_eligible_independent_cass_sources() {
        let fixture = Fixture::new();
        fixture.evidence(413, BODY, None);
        let span = fixture.admitted().pop().unwrap();
        assert!(topic(&span).is_some());
        let mut bad = span.clone();
        bad.id = "PRIVATE-UPSTREAM-SPAN".to_owned();
        assert!(topic(&bad).is_none());
        let mut bad = span.clone();
        bad.producer_kind = "unknown".to_owned();
        assert!(topic(&bad).is_none());
        let mut bad = span.clone();
        bad.pack_eligibility = "denied".to_owned();
        assert!(topic(&bad).is_none());
        let mut bad = span.clone();
        bad.memory_id = Some("mem_parent".to_owned());
        assert!(topic(&bad).is_none());
        let mut bad = span.clone();
        bad.excerpt = "!!! 🌱 ???".to_owned();
        assert!(topic(&bad).is_none());
        let mut bad = span;
        bad.created_at = "PRIVATE-INVALID-TIME".to_owned();
        assert!(topic(&bad).is_none());
    }
}
