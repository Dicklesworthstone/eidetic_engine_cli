//! Proposed lessons from completed, source-backed tool repair episodes.
//!
//! Tool records prove the episode but never become ordinary memory sources.
//! Only the admitted assistant repair turns own a proposed memory. The existing
//! corroboration package commits to the complete source context and reconstructs
//! the episode again at validation/apply. Nothing here writes a card or memory.

use super::*;
use crate::core::cass_error_recall::session_failure_arcs;
use crate::core::incident_card::{IncidentCardSources, error_class_label};

const PRODUCER: &str = "verified_tool_repair.v1";
const COMPLETE_PRODUCER: &str = "verified_tool_repair.complete.v1";
// Durable lessons have a different budget from ephemeral 120-token cards.
// Refuse over-budget source explanations, never keep an unmarked prefix.
const MAX_COMPLETE_REPAIR_BYTES: usize = 8 * 1024;

pub(super) fn candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<ReviewSessionCandidate> {
    if session.workspace_id != workspace_id {
        return Vec::new();
    }
    let sources = IncidentCardSources::new(spans);
    let by_id = spans.iter().fold(BTreeMap::new(), |mut rows, span| {
        rows.entry(span.id.as_str()).or_insert(span);
        rows
    });
    let mut seen = BTreeSet::new();
    let mut proposals = Vec::new();
    for arc in session_failure_arcs(workspace_id, session, spans) {
        let Some(resolution) = &arc.resolution else {
            continue;
        };
        let Some(diagnostic) = arc.diagnostics.first() else {
            continue;
        };
        let source_ids = resolution.repair_ids.iter().cloned().collect::<BTreeSet<_>>();
        if source_ids.is_empty() || source_ids.iter().any(|id| {
            by_id.get(id.as_str()).is_none_or(|span| {
                !span.is_search_admitted_for_session(workspace_id, session)
                    || review_learning_text(span).is_none()
            })
        }) {
            continue;
        }
        // Preserve the compact proposal's existing content and identity when
        // the card has a complete fix. A card's presentation ceiling must not
        // prevent a longer, fully proved repair from entering the review queue.
        let compact = sources.draft(workspace_id, &session.id, &arc).and_then(|card| {
            card.input.excerpt.lines()
                .find_map(|line| line.strip_prefix("Fix: "))
                .filter(|fix| complete_fix(fix))
                .map(str::to_owned)
        });
        let (producer, fix) = match compact {
            Some(fix) => (PRODUCER, fix),
            None => {
                let repairs: Vec<_> = source_ids.iter()
                    .filter_map(|id| by_id.get(id.as_str()).copied())
                    .collect();
                let Some(fix) = complete_explanation(workspace_id, session, &repairs) else {
                    continue;
                };
                (COMPLETE_PRODUCER, fix)
            }
        };
        let source_ids: Vec<_> = source_ids.into_iter().collect();
        let class = error_class_label(&diagnostic.layered_key().key);
        let command = arc.family.to_string();
        let content = format!(
            "Observed repair for {class} during `{command}`:\nFailure: {}\nRepair: {fix}\nVerification: the same command and execution context succeeded afterwards.",
            diagnostic.message_template,
        );
        let screen = crate::policy::screen_external_text_for_ingestion(&content);
        if screen.redacted || screen.instruction_like || screen.content != content
            || !matches!(screen.instruction_risk, "none" | "low")
            || content.contains("[REDACTED")
            || !crate::curate::specificity_score(&content).passes_threshold
        {
            continue;
        }
        let topic_key = review_topic_key(&content);
        if topic_key == "noise" {
            continue;
        }
        let content_hash = content_hash_for_candidate(&content);
        let candidate_id = deterministic_curate_id(&[
            workspace_id, &session.id, producer, &arc.attempt_id,
            &arc.failure_id, &resolution.proof_id, &source_ids.join(","), &content_hash,
        ]);
        if !seen.insert(candidate_id.clone()) {
            continue;
        }
        // An exact diagnostic and complete repair records, not a repeated
        // keyword or shortened card, define independent corroboration. Process
        // binding remains separately proved inside every supporting episode.
        let mut signature_parts = vec![producer.to_owned(), command];
        for diagnostic in &arc.diagnostics {
            signature_parts.push(diagnostic.layered_key().key);
            signature_parts.push(diagnostic.message_template.clone());
        }
        let reason = format!(
            "Verified tool-repair proposal from session {}: failure {} followed by admitted repair turn(s) {} and completed same-context retry {}. Tool output is proof context, not a memory source. This observed repair requires review; it is not a general guarantee.",
            session.id, arc.failure_id, source_ids.join(", "), resolution.proof_id,
        );
        proposals.push(ReviewSessionCandidate {
            candidate_id,
            candidate_type: CandidateType::CreateDerivedMemory.as_str().to_owned(),
            candidate_kind: REVIEW_CANDIDATE_KIND_PROPOSE_NEW_MEMORY.to_owned(),
            topic_key,
            target_memory_id: None,
            proposed_content: content,
            // Only the canonical scorer may replace this pending score.
            proposed_confidence: 0.0,
            source_type: CandidateSource::AgentInference.as_str().to_owned(),
            source_ids,
            reason,
            confidence: 0.0,
            content_hash,
            session_arc: None,
            persisted: false,
            corroboration: Some(ReviewCorroboration::pending(digest(signature_parts))),
        });
    }
    proposals
}

/// A resolved arc has already selected performed repairs, not plans or tool
/// output. For the larger lesson surface, retain each complete admitted turn:
/// later qualifications and paragraph/code boundaries are part of the lesson.
/// Sorting affects presentation only; exact source IDs remain in the package.
fn complete_explanation(
    workspace_id: &str,
    session: &StoredSession,
    repairs: &[&StoredEvidenceSpan],
) -> Option<String> {
    let mut ordered = repairs.to_vec();
    ordered.sort_by(|left, right| session_arc_span_order(left, right));
    let bodies = ordered.iter().map(|span| {
        if span.span_kind != "message"
            || span.role.as_deref() != Some("assistant")
            || !complete_source(span)
            || !span.is_derivation_admitted_for_session(workspace_id, session)
        {
            return None;
        }
        review_learning_text(span)
    }).collect::<Option<Vec<_>>>()?;
    let texts: Vec<_> = bodies.iter().map(|body| body.as_ref()).collect();
    join_complete_explanations(&texts)
}

fn join_complete_explanations(texts: &[&str]) -> Option<String> {
    if texts.is_empty() {
        return None;
    }
    let mut bytes = 0_usize;
    for (index, text) in texts.iter().enumerate() {
        if !complete_fix(text) {
            return None;
        }
        bytes = bytes.checked_add(text.len())?;
        if index > 0 {
            bytes = bytes.checked_add(2)?;
        }
        if bytes > MAX_COMPLETE_REPAIR_BYTES {
            return None;
        }
    }
    Some(texts.join("\n\n"))
}

fn complete_fix(fix: &str) -> bool {
    let fix = fix.trim();
    !fix.is_empty() && !fix.ends_with("...") && !fix.ends_with('…')
        && !fix.contains("[truncated]") && !fix.contains("[TRUNCATED]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::incident_card::proof::tests::{Fixture, TestResult, WS};

    #[test]
    fn verified_tool_episode_proposes_admitted_repairs_without_persisting_cards() -> TestResult {
        let fixture = Fixture::new_public(true)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let spans = fixture.db.list_evidence_spans_for_session(&fixture.session)?;
        let proposed = candidates(WS, &session, &spans);
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].source_ids, vec![fixture.repair.clone()]);
        assert!(proposed[0].proposed_content.contains("Observed repair for rustc:E0308"));
        assert!(proposed[0].proposed_content.contains("flushing pending writes"));
        assert_eq!(proposed[0].confidence, 0.0);
        assert_eq!(candidates(WS, &session, &spans)[0].candidate_id, proposed[0].candidate_id);
        assert_eq!(fixture.db.list_evidence_spans_for_session(&fixture.session)?, spans);
        assert!(fixture.db.list_curation_candidates(WS, None, None, None)?.is_empty());
        Ok(())
    }

    #[test]
    fn no_completed_retry_means_no_verified_repair_proposal() -> TestResult {
        let fixture = Fixture::new_public(false)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let spans = fixture.db.list_evidence_spans_for_session(&fixture.session)?;
        assert!(candidates(WS, &session, &spans).is_empty());
        assert!(candidates("foreign-workspace", &session, &spans).is_empty());
        assert!(candidates(WS, &session, &[]).is_empty());
        Ok(())
    }

    #[test]
    fn an_empty_prose_queue_still_discovers_and_revalidates_a_verified_tool_repair() -> TestResult {
        let fixture = Fixture::new_public(true)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let mut proposed = Vec::new();
        score_candidates(&fixture.db, WS, &session, &mut proposed, 0.6, 10)?;
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].confidence, 0.6);
        assert!(proposed[0].corroboration.as_ref().is_some_and(ReviewCorroboration::recorded));
        validate_draft(&fixture.db, WS, &proposed[0]).map_err(|error| error.message)?;
        assert!(fixture.db.list_curation_candidates(WS, None, None, None)?.is_empty());
        Ok(())
    }

    #[test]
    fn incomplete_card_sentences_are_not_promoted_to_durable_lessons() {
        for text in ["", " ", "Fixed Widget...", "Fixed Widget…", "Fixed [truncated]", "Fixed [TRUNCATED]"] {
            assert!(!complete_fix(text), "{text}");
        }
        assert!(complete_fix("I fixed Widget only when its input is already validated."));
        assert!(complete_fix("Changed the literal `...` in src/widget.rs."));
    }

    #[test]
    fn complete_explanations_preserve_paragraphs_code_and_late_qualifications() {
        let first = "I fixed the cache by changing the key type.\n\n```rust\nlet key = 1;\n```";
        let second = "This repair applies only to validated inputs; it does not enable unchecked decoding.";
        assert_eq!(
            join_complete_explanations(&[first, second]),
            Some(format!("{first}\n\n{second}")),
        );
        assert!(join_complete_explanations(&[]).is_none());
        assert!(join_complete_explanations(&[first, "Still truncated..."]).is_none());
    }

    #[test]
    fn complete_explanation_budget_counts_all_utf8_bytes_and_separators() {
        let exact = "🦀".repeat(MAX_COMPLETE_REPAIR_BYTES / 4);
        assert_eq!(join_complete_explanations(&[&exact]), Some(exact.clone()));
        assert!(join_complete_explanations(&[&format!("{exact}x")]).is_none());
        let first = "x".repeat(MAX_COMPLETE_REPAIR_BYTES - 3);
        assert_eq!(
            join_complete_explanations(&[&first, "y"]).as_ref().map(String::len),
            Some(MAX_COMPLETE_REPAIR_BYTES),
        );
        assert!(join_complete_explanations(&[&first, "yz"]).is_none());
    }

    #[test]
    fn compact_proposals_keep_their_existing_identity_and_source_package() -> TestResult {
        let fixture = Fixture::new_public(true)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let spans = fixture.db.list_evidence_spans_for_session(&fixture.session)?;
        let arcs = session_failure_arcs(WS, &session, &spans);
        let arc = arcs.first().ok_or("arc")?;
        let resolution = arc.resolution.as_ref().ok_or("resolution")?;
        let card = IncidentCardSources::new(&spans)
            .draft(WS, &session.id, arc).ok_or("compact card")?;
        let fix = card.input.excerpt.lines()
            .find_map(|line| line.strip_prefix("Fix: ")).ok_or("fix")?;
        let diagnostic = arc.diagnostics.first().ok_or("diagnostic")?;
        let class = error_class_label(&diagnostic.layered_key().key);
        let command = arc.family.to_string();
        let expected = format!(
            "Observed repair for {class} during `{command}`:\nFailure: {}\nRepair: {fix}\nVerification: the same command and execution context succeeded afterwards.",
            diagnostic.message_template,
        );
        let source_ids: Vec<_> = resolution.repair_ids.iter().cloned()
            .collect::<BTreeSet<_>>().into_iter().collect();
        let id = deterministic_curate_id(&[
            WS, &session.id, PRODUCER, &arc.attempt_id, &arc.failure_id,
            &resolution.proof_id, &source_ids.join(","), &content_hash_for_candidate(&expected),
        ]);
        let proposed = candidates(WS, &session, &spans);
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].candidate_id, id);
        assert_eq!(proposed[0].proposed_content, expected);
        assert_eq!(proposed[0].source_ids, source_ids);
        Ok(())
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::*;
    use crate::core::init::{InitOptions, init_workspace};
    use crate::db::{CreateEvidenceSpanInput, CreateSessionInput};
    use serde_json::json;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
    const FIX: &str = "I fixed E0277 by deriving Serialize for Widget before writing the value to the store in src/widget.rs.";
    const FAILURE: &str = "error[E0277]: the trait bound `Widget: Serialize` is not satisfied\n --> src/widget.rs:3:5\n";
    const COMMAND: &str = "cargo check";

    struct Workspace {
        _temp: tempfile::TempDir,
        path: PathBuf,
        database: PathBuf,
        id: String,
        db: DbConnection,
    }

    impl Workspace {
        fn new() -> TestResult<Self> {
            let temp = tempfile::tempdir()?;
            let path = temp.path().canonicalize()?;
            let report = init_workspace(&InitOptions {
                workspace_path: path.clone(), dry_run: false, repair_plan: false,
                force: false, allow_symlink: false, skip_boilerplate: true,
            });
            assert!(report.status.is_success(), "{report:?}");
            let db = DbConnection::open_file(&report.database_path)?;
            let workspace = db.get_workspace_by_path(&path.to_string_lossy())?.ok_or("workspace")?;
            Ok(Self { _temp: temp, path, database: report.database_path, id: workspace.id, db })
        }

        fn episode(&self, number: u128, context: &str, retry_command: &str, passed: bool)
        -> TestResult<(String, String, String)> {
            self.episode_with_repair(number, context, retry_command, passed, FIX)
        }

        fn episode_with_repair(
            &self, number: u128, context: &str, retry_command: &str, passed: bool, repair: &str,
        ) -> TestResult<(String, String, String)> {
            let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
            self.db.insert_session(&session, &CreateSessionInput {
                workspace_id: self.id.clone(),
                cass_session_id: format!("/sessions/verified-learning-{number}.jsonl"),
                source_path: Some(format!("/sessions/verified-learning-{number}.jsonl")),
                agent_name: Some("claude_code".to_owned()), model: None,
                started_at: None, ended_at: None, message_count: 6, token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(context.as_bytes()).to_hex()),
                metadata_json: None,
            })?;
            // Call IDs are identical across independent episodes. Changing only
            // local IDs or upstream paths must not manufacture independence.
            let rows = [
                ("message", "user", json!({"type":"text","text":context})),
                ("tool_call", "assistant", json!({"type":"tool_use","id":"fail","name":"Bash","input":{"command":COMMAND}})),
                ("tool_result", "user", json!({"type":"tool_result","tool_use_id":"fail","content":FAILURE,"is_error":true})),
                ("message", "assistant", json!({"type":"text","text":repair})),
                ("tool_call", "assistant", json!({"type":"tool_use","id":"retry","name":"Bash","input":{"command":retry_command}})),
                ("tool_result", "user", json!({"type":"tool_result","tool_use_id":"retry","content":if passed {""} else {FAILURE},"is_error":!passed})),
            ];
            let mut ids = Vec::new();
            for (index, (kind, role, block)) in rows.into_iter().enumerate() {
                let line = u32::try_from(index + 1)?;
                let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(number * 100 + u128::from(line))).to_string();
                let excerpt = json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
                self.db.insert_evidence_span(&id, &CreateEvidenceSpanInput {
                    workspace_id: self.id.clone(), session_id: session.clone(), memory_id: None,
                    producer_kind: EvidenceProducerKind::CassImport,
                    cass_span_id: format!("{session}:{line}"), span_kind: kind.to_owned(),
                    start_line: line, end_line: line, start_byte: None, end_byte: None,
                    role: Some(role.to_owned()),
                    content_hash: format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex()),
                    excerpt, metadata_json: None, inherited_redaction_classes: Vec::new(),
                })?;
                ids.push(id);
            }
            Ok((session, ids[3].clone(), ids[5].clone()))
        }

        fn review(&self, session: &str, floor: f32, persist: bool) -> TestResult<ReviewSessionReport> {
            review_session_proposals(&ReviewSessionOptions {
                workspace_path: &self.path, database_path: Some(&self.database),
                session_id: Some(session), propose: persist, dry_run: !persist,
                min_confidence: floor, limit: 100,
            }).map_err(|error| error.message().into())
        }
    }

    fn native(report: &ReviewSessionReport) -> Vec<&ReviewSessionCandidate> {
        report.candidates.iter().filter(|candidate| {
            candidate.proposed_content.starts_with("Observed repair for ")
        }).collect()
    }

    #[test]
    fn public_review_persists_reproducible_source_packages_without_creating_memories() -> TestResult {
        let workspace = Workspace::new()?;
        let (session, repair, proof) = workspace.episode(90_001, "Work item alpha.", COMMAND, true)?;
        let sources = workspace.db.list_evidence_spans_for_session(&session)?;
        let preview = workspace.review(&session, 0.6, false)?;
        assert!(!preview.durable_mutation);
        assert_eq!(native(&preview).len(), 1);
        assert_eq!(native(&preview)[0].source_ids, vec![repair.clone()]);
        let written = workspace.review(&session, 0.6, true)?;
        assert!(written.durable_mutation);
        let proposed = native(&written);
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].candidate_id, native(&preview)[0].candidate_id);
        let stored = workspace.db.get_curation_candidate(&workspace.id, &proposed[0].candidate_id)?.ok_or("stored proposal")?;
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        let package: serde_json::Value = serde_json::from_str(stored.derivation_metadata_json.as_deref().ok_or("package")?)?;
        let support = package.pointer("/producer/producerPayload/corroboration/sessions/0").ok_or("support")?;
        assert_eq!(support["sources"].as_array().ok_or("sources")?.len(), 1);
        assert_eq!(support["sources"][0]["evidenceSpanId"], repair);
        assert!(support["contextSources"].as_array().ok_or("context")?.iter().any(|source| source["evidenceSpanId"] == proof));
        let again = workspace.review(&session, 0.6, true)?;
        assert!(!again.durable_mutation);
        assert_eq!(native(&again)[0].candidate_id, proposed[0].candidate_id);
        assert!(workspace.db.list_memories(&workspace.id, None, true)?.is_empty());
        assert_eq!(workspace.db.list_evidence_spans_for_session(&session)?, sources);
        Ok(())
    }

    #[test]
    fn copied_sessions_and_failed_retries_cannot_supply_independent_support() -> TestResult {
        let workspace = Workspace::new()?;
        let (anchor, _, _) = workspace.episode(91_001, "Work item alpha.", COMMAND, true)?;
        workspace.episode(91_002, "Work item alpha.", COMMAND, true)?;
        workspace.episode(91_003, "Work item beta.", COMMAND, false)?;
        assert!(native(&workspace.review(&anchor, 0.7, false)?).is_empty());
        let single = workspace.review(&anchor, 0.6, false)?;
        assert_eq!(native(&single).len(), 1);
        assert_eq!(native(&single)[0].confidence, 0.6);
        workspace.episode(91_004, "Work item gamma.", COMMAND, true)?;
        let two = workspace.review(&anchor, 0.7, false)?;
        assert_eq!(native(&two).len(), 1);
        assert_eq!(native(&two)[0].confidence, 0.7);
        assert!(native(&workspace.review(&anchor, 0.8, false)?).is_empty());
        workspace.episode(91_005, "Work item delta.", COMMAND, true)?;
        let three = workspace.review(&anchor, 0.8, false)?;
        assert_eq!(native(&three).len(), 1);
        assert_eq!(native(&three)[0].confidence, 0.8);
        validate_draft(&workspace.db, &workspace.id, native(&three)[0]).map_err(|error| error.message)?;
        Ok(())
    }

    #[test]
    fn unchanged_repairs_do_not_survive_changed_command_proof_at_validation() -> TestResult {
        let workspace = Workspace::new()?;
        let (session, _, proof) = workspace.episode(92_001, "Work item alpha.", COMMAND, true)?;
        let report = workspace.review(&session, 0.6, true)?;
        let id = &native(&report)[0].candidate_id;
        let stored = workspace.db.get_curation_candidate(&workspace.id, id)?.ok_or("stored")?;
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        let row = workspace.db.get_evidence_span(&proof)?.ok_or("proof")?;
        let mut body: serde_json::Value = serde_json::from_str(&row.excerpt)?;
        body["message"]["content"][0]["is_error"] = json!(true);
        body["message"]["content"][0]["content"] = json!(FAILURE);
        let revised = body.to_string().replace('\'', "''");
        workspace.db.execute_raw(&format!("UPDATE evidence_spans SET excerpt = '{revised}' WHERE id = '{proof}'"))?;
        assert!(validate_recorded(&workspace.db, &stored).is_err());
        assert_eq!(workspace.db.get_curation_candidate(&workspace.id, id)?.ok_or("history")?.derivation_metadata_json, stored.derivation_metadata_json);
        assert!(workspace.db.list_memories(&workspace.id, None, true)?.is_empty());
        Ok(())
    }

    #[test]
    fn public_review_does_not_certify_a_different_retry_target() -> TestResult {
        let workspace = Workspace::new()?;
        let (session, _, _) = workspace.episode(93_001, "Work item alpha.", "cargo check --lib", true)?;
        assert!(native(&workspace.review(&session, 0.0, false)?).is_empty());
        Ok(())
    }

    fn long_repair() -> String {
        format!(
            "I fixed E0277 by deriving Serialize for Widget in src/widget.rs and preserving {}only for internally validated inputs, not arbitrary untrusted values.",
            "the original field ordering and the explicit serialization boundary, ".repeat(18),
        )
    }

    #[test]
    fn a_verified_repair_larger_than_a_card_is_a_complete_reviewable_lesson() -> TestResult {
        let workspace = Workspace::new()?;
        let text = long_repair();
        let (session_id, repair, _) = workspace.episode_with_repair(
            94_001, "Work item alpha.", COMMAND, true, &text,
        )?;
        let spans = workspace.db.list_evidence_spans_for_session(&session_id)?;
        let session = workspace.db.get_session(&session_id)?.ok_or("session")?;
        let arcs = session_failure_arcs(&workspace.id, &session, &spans);
        let arc = arcs.first().ok_or("native failure arc")?;
        assert!(arc.resolution.as_ref().is_some_and(|resolution| {
            resolution.repair_ids.contains(&repair)
        }));
        let compact = IncidentCardSources::new(&spans).draft(&workspace.id, &session.id, arc);
        assert!(compact.as_ref().is_none_or(|card| {
            card.input.excerpt.lines().find_map(|line| line.strip_prefix("Fix: "))
                .is_none_or(|fix| !complete_fix(fix))
        }), "the fixture must miss the former compact-only path");
        let preview = workspace.review(&session.id, 0.6, false)?;
        let proposed = native(&preview);
        assert_eq!(proposed.len(), 1);
        assert!(proposed[0].proposed_content.contains(&text));
        assert_eq!(proposed[0].source_ids, vec![repair]);
        assert_eq!(proposed[0].confidence, 0.6);
        validate_draft(&workspace.db, &workspace.id, proposed[0]).map_err(|error| error.message)?;
        let written = workspace.review(&session.id, 0.6, true)?;
        let id = &native(&written)[0].candidate_id;
        assert_eq!(*id, proposed[0].candidate_id);
        let stored = workspace.db.get_curation_candidate(&workspace.id, id)?.ok_or("stored")?;
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        assert!(stored.proposed_content.as_ref().is_some_and(|content| content.contains(&text)));
        assert!(!workspace.review(&session.id, 0.6, true)?.durable_mutation);
        assert_eq!(workspace.db.list_evidence_spans_for_session(&session.id)?, spans);
        assert!(workspace.db.list_memories(&workspace.id, None, true)?.is_empty());
        Ok(())
    }

    #[test]
    fn expanded_lesson_admission_still_needs_complete_sources_and_an_exact_retry() -> TestResult {
        let workspace = Workspace::new()?;
        let long = long_repair();
        for (number, command, passed, text) in [
            (95_001, "cargo check --lib", true, long.clone()),
            (95_002, COMMAND, false, long.clone()),
            (95_003, COMMAND, true, format!("{long} [TRUNCATED]")),
            (95_004, COMMAND, true, format!("{long}{}", " x".repeat(MAX_COMPLETE_REPAIR_BYTES))),
        ] {
            let (session, _, _) = workspace.episode_with_repair(
                number, "Work item alpha.", command, passed, &text,
            )?;
            assert!(native(&workspace.review(&session, 0.6, false)?).is_empty(), "{number}");
        }
        Ok(())
    }
}
