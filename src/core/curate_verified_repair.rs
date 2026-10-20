//! Proposed lessons from completed, source-backed tool repair episodes.
//!
//! Tool records prove the episode but never become ordinary memory sources.
//! Only the admitted assistant repair turns own a proposed memory. The existing
//! corroboration package commits to the complete source context and reconstructs
//! the episode again at validation/apply. Nothing here writes a card or memory.

use super::*;
use crate::core::cass_error_recall::session_failure_arcs;
use crate::core::error_recall::CanonicalDiagnostic;
use crate::core::incident_card::{IncidentCardSources, error_class_label};

const PRODUCER: &str = "verified_tool_repair.v1";
const COMPLETE_PRODUCER: &str = "verified_tool_repair.complete.v1";
// Durable lessons have a different budget from ephemeral 120-token cards.
// Refuse over-budget source explanations, never keep an unmarked prefix.
const MAX_COMPLETE_REPAIR_BYTES: usize = 8 * 1024;

#[cfg(test)]
pub(super) fn candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<ReviewSessionCandidate> {
    let mut candidates = Vec::new();
    visit_candidates(workspace_id, session, spans, |candidate, _| {
        candidates.push(candidate);
    });
    candidates
}

/// Keep each emitted lesson attached to its actual failure episode while the
/// caller builds request-local feedback bindings. No presentation parsing or
/// additional transcript pass is needed to recover those identities later.
pub(super) fn visit_candidates(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
    mut visit: impl FnMut(ReviewSessionCandidate, &crate::core::cass_error_recall::FailureArc),
) {
    if session.workspace_id != workspace_id {
        return;
    }
    let sources = IncidentCardSources::new(spans);
    let by_id = spans.iter().fold(BTreeMap::new(), |mut rows, span| {
        rows.entry(span.id.as_str()).or_insert(span);
        rows
    });
    let mut seen = BTreeSet::new();
    for arc in session_failure_arcs(workspace_id, session, spans) {
        let Some(resolution) = &arc.resolution else {
            continue;
        };
        let Some(diagnostic) = episode_diagnostics(&arc.diagnostics) else {
            continue;
        };
        let source_ids = resolution
            .repair_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if source_ids.is_empty()
            || source_ids.iter().any(|id| {
                by_id.get(id.as_str()).is_none_or(|span| {
                    !span.is_search_admitted_for_session(workspace_id, session)
                        || review_learning_text(span).is_none()
                })
            })
        {
            continue;
        }
        // Validate every complete source before considering a card. A card can
        // omit a whole qualification, list or code block without an ellipsis;
        // a grammatical last sentence does not make its summary lossless.
        let Some(repairs) = source_ids
            .iter()
            .map(|id| by_id.get(id.as_str()).copied())
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let Some(fix) = complete_explanation(workspace_id, session, &repairs) else {
            continue;
        };
        // Keep existing compact identities only when the summary reproduces
        // the complete source explanation byte-for-byte. Otherwise the durable
        // lesson uses the full-text producer, including all qualifications.
        let compact = sources
            .draft(workspace_id, &session.id, &arc)
            .and_then(|card| {
                card.input
                    .excerpt
                    .lines()
                    .find_map(|line| line.strip_prefix("Fix: "))
                    .filter(|fix| complete_fix(fix))
                    .map(str::to_owned)
            });
        let producer = if compact.as_deref() == Some(fix.as_str()) {
            PRODUCER
        } else {
            COMPLETE_PRODUCER
        };
        let source_ids: Vec<_> = source_ids.into_iter().collect();
        let class = &diagnostic.classes;
        let command = arc.family.to_string();
        let content = format!(
            "Observed repair for {class} during `{command}`:\nFailure: {}\nRepair: {fix}\nVerification: the same command and execution context succeeded afterwards.",
            diagnostic.failure,
        );
        let screen = crate::policy::screen_external_text_for_ingestion(&content);
        if screen.redacted
            || screen.instruction_like
            || screen.content != content
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
            workspace_id,
            &session.id,
            producer,
            &arc.attempt_id,
            &arc.failure_id,
            &resolution.proof_id,
            &source_ids.join(","),
            &content_hash,
        ]);
        if !seen.insert(candidate_id.clone()) {
            continue;
        }
        // An exact diagnostic and complete repair records, not a repeated
        // keyword or shortened card, define independent corroboration. Process
        // binding remains separately proved inside every supporting episode.
        let mut signature_parts = vec![producer.to_owned(), command];
        signature_parts.extend(diagnostic.signature);
        let reason = format!(
            "Verified tool-repair proposal from session {}: failure {} followed by admitted repair turn(s) {} and completed same-context retry {}. Tool output is proof context, not a memory source. This observed repair requires review; it is not a general guarantee.",
            session.id,
            arc.failure_id,
            source_ids.join(", "),
            resolution.proof_id,
        );
        let candidate = ReviewSessionCandidate {
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
        };
        visit(candidate, &arc);
    }
}

struct EpisodeDiagnostics {
    classes: String,
    failure: String,
    signature: Vec<String>,
}

/// One completed invocation can fail with several diagnostic classes. Describe
/// every extracted diagnostic rather than attributing the entire repair to the
/// first printed error. Preserve the extractor's bound and masked templates.
/// Compiler emission order and identical repeats are not different lessons;
/// distinct templates under one native code remain distinct observations.
fn episode_diagnostics(diagnostics: &[CanonicalDiagnostic]) -> Option<EpisodeDiagnostics> {
    let ordered: BTreeSet<_> = diagnostics
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.layered_key().key,
                diagnostic.message_template.clone(),
            )
        })
        .collect();
    let (_, first_message) = ordered.first()?;
    let classes = ordered
        .iter()
        .map(|(key, _)| error_class_label(key))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    let failure = if ordered.len() == 1 {
        // Single-diagnostic proposals retain their exact previous content,
        // content-bound identity and corroboration signature.
        first_message.clone()
    } else {
        ordered
            .iter()
            .map(|(key, message)| format!("{}: {message}", error_class_label(key)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let signature = ordered
        .into_iter()
        .flat_map(|(key, message)| [key, message])
        .collect();
    Some(EpisodeDiagnostics {
        classes,
        failure,
        signature,
    })
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
    let bodies = ordered
        .iter()
        .map(|span| {
            if span.span_kind != "message"
                || span.role.as_deref() != Some("assistant")
                || !complete_source(span)
                || !span.is_derivation_admitted_for_session(workspace_id, session)
            {
                return None;
            }
            review_learning_text(span)
        })
        .collect::<Option<Vec<_>>>()?;
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
    !fix.is_empty()
        && !fix.ends_with("...")
        && !fix.ends_with('…')
        && !fix.contains("[truncated]")
        && !fix.contains("[TRUNCATED]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::incident_card::proof::tests::{Fixture, TestResult, WS};

    #[test]
    fn single_error_projection_preserves_the_existing_content_and_signature() {
        let diagnostic = crate::core::error_recall::from_rustc(Some("E0277"), "missing trait");
        let projected = episode_diagnostics(std::slice::from_ref(&diagnostic)).expect("diagnostic");
        assert_eq!(projected.classes, "rustc:E0277");
        assert_eq!(projected.failure, diagnostic.message_template);
        assert_eq!(
            projected.signature,
            [diagnostic.layered_key().key, diagnostic.message_template]
        );
        assert!(episode_diagnostics(&[]).is_none());
    }

    #[test]
    fn multiple_errors_survive_projection_in_canonical_order_without_duplicate_noise() {
        use crate::core::error_recall::from_rustc;
        let first = from_rustc(Some("E0277"), "missing trait");
        let second = from_rustc(Some("E0308"), "mismatched return type");
        let forward = episode_diagnostics(&[first.clone(), second.clone()]).expect("forward");
        let reverse = episode_diagnostics(&[second, first.clone(), first]).expect("reverse");
        assert_eq!(forward.classes, "rustc:E0277, rustc:E0308");
        assert_eq!(forward.failure.lines().count(), 2);
        assert!(forward.failure.contains("missing trait"));
        assert!(forward.failure.contains("mismatched return type"));
        assert_eq!(reverse.classes, forward.classes);
        assert_eq!(reverse.failure, forward.failure);
        assert_eq!(reverse.signature, forward.signature);
    }

    #[test]
    fn distinct_templates_are_not_erased_by_a_shared_code_or_tool() {
        use crate::core::error_recall::{from_cargo, from_rustc};
        for diagnostics in [
            vec![
                from_rustc(Some("E0277"), "missing trait"),
                from_rustc(Some("E0277"), "thread safety bound"),
            ],
            vec![
                from_cargo(None, "failed native header lookup"),
                from_cargo(None, "failed dependency resolution"),
            ],
        ] {
            let projected = episode_diagnostics(&diagnostics).expect("distinct diagnostics");
            assert_eq!(projected.failure.lines().count(), 2);
            assert_eq!(projected.signature.len(), 4);
            for diagnostic in diagnostics {
                assert!(projected.failure.contains(&diagnostic.message_template));
                assert!(projected.signature.contains(&diagnostic.layered_key().key));
            }
        }
    }

    #[test]
    fn verified_tool_episode_proposes_admitted_repairs_without_persisting_cards() -> TestResult {
        let fixture = Fixture::new_public(true)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let spans = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        let proposed = candidates(WS, &session, &spans);
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].source_ids, vec![fixture.repair.clone()]);
        assert!(
            proposed[0]
                .proposed_content
                .contains("Observed repair for rustc:E0308")
        );
        assert!(
            proposed[0]
                .proposed_content
                .contains("flushing pending writes")
        );
        assert_eq!(proposed[0].confidence, 0.0);
        assert_eq!(
            candidates(WS, &session, &spans)[0].candidate_id,
            proposed[0].candidate_id
        );
        assert_eq!(
            fixture
                .db
                .list_evidence_spans_for_session(&fixture.session)?,
            spans
        );
        assert!(
            fixture
                .db
                .list_curation_candidates(WS, None, None, None)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn no_completed_retry_means_no_verified_repair_proposal() -> TestResult {
        let fixture = Fixture::new_public(false)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let spans = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
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
        assert!(
            proposed[0]
                .corroboration
                .as_ref()
                .is_some_and(ReviewCorroboration::recorded)
        );
        validate_draft(&fixture.db, WS, &proposed[0]).map_err(|error| error.message)?;
        assert!(
            fixture
                .db
                .list_curation_candidates(WS, None, None, None)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn incomplete_card_sentences_are_not_promoted_to_durable_lessons() {
        for text in [
            "",
            " ",
            "Fixed Widget...",
            "Fixed Widget…",
            "Fixed [truncated]",
            "Fixed [TRUNCATED]",
        ] {
            assert!(!complete_fix(text), "{text}");
        }
        assert!(complete_fix(
            "I fixed Widget only when its input is already validated."
        ));
        assert!(complete_fix("Changed the literal `...` in src/widget.rs."));
    }

    #[test]
    fn complete_explanations_preserve_paragraphs_code_and_late_qualifications() {
        let first = "I fixed the cache by changing the key type.\n\n```rust\nlet key = 1;\n```";
        let second =
            "This repair applies only to validated inputs; it does not enable unchecked decoding.";
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
            join_complete_explanations(&[&first, "y"])
                .as_ref()
                .map(String::len),
            Some(MAX_COMPLETE_REPAIR_BYTES),
        );
        assert!(join_complete_explanations(&[&first, "yz"]).is_none());
    }

    #[test]
    fn compact_proposals_keep_their_existing_identity_and_source_package() -> TestResult {
        let fixture = Fixture::new_public(true)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let spans = fixture
            .db
            .list_evidence_spans_for_session(&fixture.session)?;
        let arcs = session_failure_arcs(WS, &session, &spans);
        let arc = arcs.first().ok_or("arc")?;
        let resolution = arc.resolution.as_ref().ok_or("resolution")?;
        let card = IncidentCardSources::new(&spans)
            .draft(WS, &session.id, arc)
            .ok_or("compact card")?;
        let fix = card
            .input
            .excerpt
            .lines()
            .find_map(|line| line.strip_prefix("Fix: "))
            .ok_or("fix")?;
        let diagnostic = arc.diagnostics.first().ok_or("diagnostic")?;
        let class = error_class_label(&diagnostic.layered_key().key);
        let command = arc.family.to_string();
        let expected = format!(
            "Observed repair for {class} during `{command}`:\nFailure: {}\nRepair: {fix}\nVerification: the same command and execution context succeeded afterwards.",
            diagnostic.message_template,
        );
        let source_ids: Vec<_> = resolution
            .repair_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let id = deterministic_curate_id(&[
            WS,
            &session.id,
            PRODUCER,
            &arc.attempt_id,
            &arc.failure_id,
            &resolution.proof_id,
            &source_ids.join(","),
            &content_hash_for_candidate(&expected),
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

        fn episode(
            &self,
            number: u128,
            context: &str,
            retry_command: &str,
            passed: bool,
        ) -> TestResult<(String, String, String)> {
            self.episode_with_repair(number, context, retry_command, passed, FIX)
        }

        fn episode_with_repair(
            &self,
            number: u128,
            context: &str,
            retry_command: &str,
            passed: bool,
            repair: &str,
        ) -> TestResult<(String, String, String)> {
            self.episode_with_output(number, context, retry_command, passed, repair, FAILURE)
        }

        fn episode_with_output(
            &self,
            number: u128,
            context: &str,
            retry_command: &str,
            passed: bool,
            repair: &str,
            failure: &str,
        ) -> TestResult<(String, String, String)> {
            let session =
                crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
            self.db.insert_session(
                &session,
                &CreateSessionInput {
                    workspace_id: self.id.clone(),
                    cass_session_id: format!("/sessions/verified-learning-{number}.jsonl"),
                    source_path: Some(format!("/sessions/verified-learning-{number}.jsonl")),
                    agent_name: Some("claude_code".to_owned()),
                    model: None,
                    started_at: None,
                    ended_at: None,
                    message_count: 6,
                    token_count: None,
                    content_hash: format!("blake3:{}", blake3::hash(context.as_bytes()).to_hex()),
                    metadata_json: None,
                },
            )?;
            // Call IDs are identical across independent episodes. Changing only
            // local IDs or upstream paths must not manufacture independence.
            let rows = [
                ("message", "user", json!({"type":"text","text":context})),
                (
                    "tool_call",
                    "assistant",
                    json!({"type":"tool_use","id":"fail","name":"Bash","input":{"command":COMMAND}}),
                ),
                (
                    "tool_result",
                    "user",
                    json!({"type":"tool_result","tool_use_id":"fail","content":failure,"is_error":true}),
                ),
                ("message", "assistant", json!({"type":"text","text":repair})),
                (
                    "tool_call",
                    "assistant",
                    json!({"type":"tool_use","id":"retry","name":"Bash","input":{"command":retry_command}}),
                ),
                (
                    "tool_result",
                    "user",
                    json!({"type":"tool_result","tool_use_id":"retry","content":if passed {""} else {failure},"is_error":!passed}),
                ),
            ];
            let mut ids = Vec::new();
            for (index, (kind, role, block)) in rows.into_iter().enumerate() {
                let line = u32::try_from(index + 1)?;
                let id =
                    EvidenceId::from_uuid(uuid::Uuid::from_u128(number * 100 + u128::from(line)))
                        .to_string();
                let excerpt =
                    json!({"type":role,"message":{"role":role,"content":[block]}}).to_string();
                self.db.insert_evidence_span(
                    &id,
                    &CreateEvidenceSpanInput {
                        workspace_id: self.id.clone(),
                        session_id: session.clone(),
                        memory_id: None,
                        producer_kind: EvidenceProducerKind::CassImport,
                        cass_span_id: format!("{session}:{line}"),
                        span_kind: kind.to_owned(),
                        start_line: line,
                        end_line: line,
                        start_byte: None,
                        end_byte: None,
                        role: Some(role.to_owned()),
                        content_hash: format!(
                            "blake3:{}",
                            blake3::hash(excerpt.as_bytes()).to_hex()
                        ),
                        excerpt,
                        metadata_json: None,
                        inherited_redaction_classes: Vec::new(),
                    },
                )?;
                ids.push(id);
            }
            Ok((session, ids[3].clone(), ids[5].clone()))
        }

        fn review(
            &self,
            session: &str,
            floor: f32,
            persist: bool,
        ) -> TestResult<ReviewSessionReport> {
            review_session_proposals(&ReviewSessionOptions {
                workspace_path: &self.path,
                database_path: Some(&self.database),
                session_id: Some(session),
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
    fn public_review_persists_reproducible_source_packages_without_creating_memories() -> TestResult
    {
        let workspace = Workspace::new()?;
        let (session, repair, proof) =
            workspace.episode(90_001, "Work item alpha.", COMMAND, true)?;
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
        let stored = workspace
            .db
            .get_curation_candidate(&workspace.id, &proposed[0].candidate_id)?
            .ok_or("stored proposal")?;
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        let package: serde_json::Value = serde_json::from_str(
            stored
                .derivation_metadata_json
                .as_deref()
                .ok_or("package")?,
        )?;
        let support = package
            .pointer("/producer/producerPayload/corroboration/sessions/0")
            .ok_or("support")?;
        assert_eq!(support["sources"].as_array().ok_or("sources")?.len(), 1);
        assert_eq!(support["sources"][0]["evidenceSpanId"], repair);
        assert!(
            support["contextSources"]
                .as_array()
                .ok_or("context")?
                .iter()
                .any(|source| source["evidenceSpanId"] == proof)
        );
        let again = workspace.review(&session, 0.6, true)?;
        assert!(!again.durable_mutation);
        assert_eq!(native(&again)[0].candidate_id, proposed[0].candidate_id);
        assert!(
            workspace
                .db
                .list_memories(&workspace.id, None, true)?
                .is_empty()
        );
        assert_eq!(
            workspace.db.list_evidence_spans_for_session(&session)?,
            sources
        );
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
        validate_draft(&workspace.db, &workspace.id, native(&three)[0])
            .map_err(|error| error.message)?;
        Ok(())
    }

    #[test]
    fn unchanged_repairs_do_not_survive_changed_command_proof_at_validation() -> TestResult {
        let workspace = Workspace::new()?;
        let (session, _, proof) = workspace.episode(92_001, "Work item alpha.", COMMAND, true)?;
        let report = workspace.review(&session, 0.6, true)?;
        let id = &native(&report)[0].candidate_id;
        let stored = workspace
            .db
            .get_curation_candidate(&workspace.id, id)?
            .ok_or("stored")?;
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        let row = workspace.db.get_evidence_span(&proof)?.ok_or("proof")?;
        let mut body: serde_json::Value = serde_json::from_str(&row.excerpt)?;
        body["message"]["content"][0]["is_error"] = json!(true);
        body["message"]["content"][0]["content"] = json!(FAILURE);
        let revised = body.to_string().replace('\'', "''");
        workspace.db.execute_raw(&format!(
            "UPDATE evidence_spans SET excerpt = '{revised}' WHERE id = '{proof}'"
        ))?;
        assert!(validate_recorded(&workspace.db, &stored).is_err());
        assert_eq!(
            workspace
                .db
                .get_curation_candidate(&workspace.id, id)?
                .ok_or("history")?
                .derivation_metadata_json,
            stored.derivation_metadata_json
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
    fn public_review_does_not_certify_a_different_retry_target() -> TestResult {
        let workspace = Workspace::new()?;
        let (session, _, _) =
            workspace.episode(93_001, "Work item alpha.", "cargo check --lib", true)?;
        assert!(native(&workspace.review(&session, 0.0, false)?).is_empty());
        Ok(())
    }

    #[test]
    fn multi_error_repairs_reach_public_preview_persistence_and_revalidation() -> TestResult {
        let workspace = Workspace::new()?;
        let output = format!("{FAILURE}error[E0308]: mismatched types\n --> src/widget.rs:8:2\n");
        let repair = "I fixed E0277 and E0308 by deriving Serialize for Widget and correcting its declared type in src/widget.rs.";
        let (session, repair_id, _) = workspace.episode_with_output(
            96_001,
            "Multi-error work item.",
            COMMAND,
            true,
            repair,
            &output,
        )?;
        let sources = workspace.db.list_evidence_spans_for_session(&session)?;
        let preview = workspace.review(&session, 0.6, false)?;
        let candidates = native(&preview);
        assert_eq!(candidates.len(), 1);
        let proposal = candidates[0];
        assert!(
            proposal
                .proposed_content
                .starts_with("Observed repair for rustc:E0277, rustc:E0308")
        );
        assert!(
            proposal
                .proposed_content
                .contains("rustc:E0308: mismatched types")
        );
        assert_eq!(proposal.source_ids, vec![repair_id]);
        let written = workspace.review(&session, 0.6, true)?;
        assert_eq!(native(&written)[0].candidate_id, proposal.candidate_id);
        let stored = workspace
            .db
            .get_curation_candidate(&workspace.id, &proposal.candidate_id)?
            .ok_or("stored")?;
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        assert_eq!(
            stored.proposed_content.as_deref(),
            Some(proposal.proposed_content.as_str())
        );
        assert!(!workspace.review(&session, 0.6, true)?.durable_mutation);
        assert_eq!(
            workspace.db.list_evidence_spans_for_session(&session)?,
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
    fn independent_compiler_emission_orders_corroborate_the_same_complete_repair() -> TestResult {
        let workspace = Workspace::new()?;
        let second = "error[E0308]: mismatched types\n --> src/widget.rs:8:2\n";
        let repair = "I fixed E0277 and E0308 by deriving Serialize for Widget and correcting its declared type in src/widget.rs.";
        let (anchor, _, _) = workspace.episode_with_output(
            97_001,
            "Work item alpha.",
            COMMAND,
            true,
            repair,
            &format!("{FAILURE}{second}"),
        )?;
        workspace.episode_with_output(
            97_002,
            "Work item beta.",
            COMMAND,
            true,
            repair,
            &format!("{second}{FAILURE}"),
        )?;
        let report = workspace.review(&anchor, 0.7, false)?;
        let candidates = native(&report);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].confidence, 0.7);
        validate_draft(&workspace.db, &workspace.id, candidates[0])
            .map_err(|error| error.message)?;
        assert!(native(&workspace.review(&anchor, 0.8, false)?).is_empty());
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
        let (session_id, repair, _) =
            workspace.episode_with_repair(94_001, "Work item alpha.", COMMAND, true, &text)?;
        let spans = workspace.db.list_evidence_spans_for_session(&session_id)?;
        let session = workspace.db.get_session(&session_id)?.ok_or("session")?;
        let arcs = session_failure_arcs(&workspace.id, &session, &spans);
        let arc = arcs.first().ok_or("native failure arc")?;
        assert!(
            arc.resolution
                .as_ref()
                .is_some_and(|resolution| { resolution.repair_ids.contains(&repair) })
        );
        let compact = IncidentCardSources::new(&spans).draft(&workspace.id, &session.id, arc);
        assert!(
            compact.as_ref().is_none_or(|card| {
                card.input
                    .excerpt
                    .lines()
                    .find_map(|line| line.strip_prefix("Fix: "))
                    .is_none_or(|fix| !complete_fix(fix))
            }),
            "the fixture must miss the former compact-only path"
        );
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
        let stored = workspace
            .db
            .get_curation_candidate(&workspace.id, id)?
            .ok_or("stored")?;
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        assert!(
            stored
                .proposed_content
                .as_ref()
                .is_some_and(|content| content.contains(&text))
        );
        assert!(!workspace.review(&session.id, 0.6, true)?.durable_mutation);
        assert_eq!(
            workspace.db.list_evidence_spans_for_session(&session.id)?,
            spans
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
    fn expanded_lesson_admission_still_needs_complete_sources_and_an_exact_retry() -> TestResult {
        let workspace = Workspace::new()?;
        let long = long_repair();
        for (number, command, passed, text) in [
            (95_001, "cargo check --lib", true, long.clone()),
            (95_002, COMMAND, false, long.clone()),
            (95_003, COMMAND, true, format!("{long} [TRUNCATED]")),
            (
                95_004,
                COMMAND,
                true,
                format!("{long}{}", " x".repeat(MAX_COMPLETE_REPAIR_BYTES)),
            ),
        ] {
            let (session, _, _) = workspace.episode_with_repair(
                number,
                "Work item alpha.",
                command,
                passed,
                &text,
            )?;
            assert!(
                native(&workspace.review(&session, 0.6, false)?).is_empty(),
                "{number}"
            );
        }
        Ok(())
    }

    #[test]
    fn short_card_cannot_erase_a_separate_scope_qualification_from_a_durable_lesson() -> TestResult
    {
        let workspace = Workspace::new()?;
        let restriction = "This applies only to validated records.";
        let text = format!("{FIX}\n\n{restriction}");
        let (session_id, repair_id, _) = workspace.episode_with_repair(
            98_001,
            "Qualified repair work item.",
            COMMAND,
            true,
            &text,
        )?;
        let session = workspace.db.get_session(&session_id)?.ok_or("session")?;
        let before = workspace.db.list_evidence_spans_for_session(&session_id)?;
        let arcs = session_failure_arcs(&workspace.id, &session, &before);
        let arc = arcs.first().ok_or("resolved failure")?;
        let card = IncidentCardSources::new(&before)
            .draft(&workspace.id, &session.id, arc)
            .ok_or("compact control")?;
        let compact_fix = card
            .input
            .excerpt
            .lines()
            .find_map(|line| line.strip_prefix("Fix: "))
            .ok_or("compact fix")?;
        assert!(
            complete_fix(compact_fix),
            "the old truncation check accepts this card"
        );
        assert!(
            !compact_fix.contains(restriction),
            "the fixture must lose its qualifier in a card"
        );

        let preview = workspace.review(&session.id, 0.6, false)?;
        let proposed = native(&preview);
        assert_eq!(proposed.len(), 1);
        assert!(
            proposed[0]
                .proposed_content
                .contains(&format!("Repair: {text}\nVerification:"))
        );
        assert_eq!(proposed[0].source_ids, vec![repair_id]);
        validate_draft(&workspace.db, &workspace.id, proposed[0]).map_err(|error| error.message)?;
        let written = workspace.review(&session.id, 0.6, true)?;
        let id = &native(&written)[0].candidate_id;
        assert_eq!(*id, proposed[0].candidate_id);
        let stored = workspace
            .db
            .get_curation_candidate(&workspace.id, id)?
            .ok_or("stored")?;
        assert_eq!(
            stored.proposed_content.as_deref(),
            Some(proposed[0].proposed_content.as_str())
        );
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        assert!(!workspace.review(&session.id, 0.6, true)?.durable_mutation);
        assert_eq!(
            workspace.db.list_evidence_spans_for_session(&session.id)?,
            before
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
    fn complete_repair_proposals_keep_code_lists_and_paragraphs_omitted_by_cards() -> TestResult {
        let workspace = Workspace::new()?;
        let context = "```rust\ntype ReadyRecord = ValidatedRecord;\n```\n\n- Only validated records belong to this type.\n- Legacy records retain their original representation.";
        let text = format!("{FIX}\n\n{context}");
        let (session_id, repair_id, _) = workspace.episode_with_repair(
            98_002,
            "Structured repair work item.",
            COMMAND,
            true,
            &text,
        )?;
        let session = workspace.db.get_session(&session_id)?.ok_or("session")?;
        let spans = workspace.db.list_evidence_spans_for_session(&session_id)?;
        let arcs = session_failure_arcs(&workspace.id, &session, &spans);
        let arc = arcs.first().ok_or("failure arc")?;
        let card = IncidentCardSources::new(&spans)
            .draft(&workspace.id, &session.id, arc)
            .ok_or("compact control")?;
        assert!(!card.input.excerpt.contains("type ReadyRecord"));
        let report = workspace.review(&session.id, 0.6, false)?;
        let proposed = native(&report);
        assert_eq!(proposed.len(), 1);
        assert!(proposed[0].proposed_content.contains(&text));
        assert_eq!(proposed[0].source_ids, vec![repair_id]);
        validate_draft(&workspace.db, &workspace.id, proposed[0]).map_err(|error| error.message)?;
        assert_eq!(
            workspace.db.list_evidence_spans_for_session(&session.id)?,
            spans
        );
        Ok(())
    }

    fn record_repair_feedback(
        workspace: &Workspace,
        scope: &str,
        code: &str,
        target: &str,
        harmful: bool,
    ) -> TestResult {
        crate::core::error_diagnosis::record_error_repair_links(
            &workspace.db,
            scope,
            &crate::core::error_recall::from_rustc(Some(code), "native repair feedback"),
            &crate::core::error_diagnosis::ErrorRepairLinkRecording {
                helpful_repairs: if harmful {
                    Vec::new()
                } else {
                    vec![target.to_owned()]
                },
                harmful_repairs: if harmful {
                    vec![target.to_owned()]
                } else {
                    Vec::new()
                },
                ..Default::default()
            },
        )?;
        Ok(())
    }

    #[test]
    fn harmful_native_repairs_are_not_reproposed_even_at_a_zero_floor() -> TestResult {
        let workspace = Workspace::new()?;
        let (session, repair, _) =
            workspace.episode(99_001, "Feedback work item.", COMMAND, true)?;
        let sources = workspace.db.list_evidence_spans_for_session(&session)?;
        let initial = workspace.review(&session, 0.6, true)?;
        let proposals = native(&initial);
        assert_eq!(proposals.len(), 1);
        let stored = workspace
            .db
            .get_curation_candidate(&workspace.id, &proposals[0].candidate_id)?
            .ok_or("stored proposal")?;
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        record_repair_feedback(&workspace, &workspace.id, "E0277", &repair, true)?;
        record_repair_feedback(&workspace, &workspace.id, "E0277", &repair, false)?;
        let history = workspace
            .db
            .list_error_repair_links(&workspace.id, "rustc:E0277")?;
        assert!(native(&workspace.review(&session, 0.0, false)?).is_empty());
        assert!(validate_recorded(&workspace.db, &stored).is_err());
        assert!(validate_draft(&workspace.db, &workspace.id, proposals[0]).is_err());
        assert_eq!(
            workspace
                .db
                .list_error_repair_links(&workspace.id, "rustc:E0277")?,
            history
        );
        assert_eq!(
            workspace.db.list_evidence_spans_for_session(&session)?,
            sources
        );
        assert_eq!(
            workspace
                .db
                .get_curation_candidate(&workspace.id, &stored.id)?
                .ok_or("historical proposal")?
                .derivation_metadata_json,
            stored.derivation_metadata_json,
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
    fn a_harmful_supporter_does_not_increase_native_lesson_confidence() -> TestResult {
        let workspace = Workspace::new()?;
        let (anchor, _, _) = workspace.episode(99_101, "Work item alpha.", COMMAND, true)?;
        let (_, supporter, _) = workspace.episode(99_102, "Work item beta.", COMMAND, true)?;
        workspace.episode(99_103, "Work item gamma.", COMMAND, true)?;
        let before = workspace.review(&anchor, 0.8, false)?;
        let proposals = native(&before);
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].confidence, 0.8);
        validate_draft(&workspace.db, &workspace.id, proposals[0])
            .map_err(|error| error.message)?;
        record_repair_feedback(&workspace, &workspace.id, "E0277", &supporter, true)?;
        assert!(native(&workspace.review(&anchor, 0.8, false)?).is_empty());
        let after = workspace.review(&anchor, 0.7, false)?;
        let remaining = native(&after);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].confidence, 0.7);
        validate_draft(&workspace.db, &workspace.id, remaining[0])
            .map_err(|error| error.message)?;
        assert!(validate_draft(&workspace.db, &workspace.id, proposals[0]).is_err());
        assert_eq!(remaining[0].source_ids, proposals[0].source_ids);
        assert!(
            workspace
                .db
                .list_memories(&workspace.id, None, true)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn card_feedback_checks_every_error_without_widening_its_scope() -> TestResult {
        let workspace = Workspace::new()?;
        let output = format!("{FAILURE}error[E0308]: mismatched types\n");
        let repair_text = "I fixed E0277 and E0308 by deriving Serialize for Widget and correcting its declared type in src/widget.rs.";
        let (session_id, repair, _) = workspace.episode_with_output(
            99_201,
            "Card feedback work item.",
            COMMAND,
            true,
            repair_text,
            &output,
        )?;
        let session = workspace.db.get_session(&session_id)?.ok_or("session")?;
        let sources = workspace.db.list_evidence_spans_for_session(&session_id)?;
        let arcs = session_failure_arcs(&workspace.id, &session, &sources);
        let arc = arcs.first().ok_or("resolved episode")?;
        assert!(arc.resolution.is_some());
        let card = crate::core::incident_card::incident_card_id(&workspace.id, &arc.failure_id);
        record_repair_feedback(&workspace, &workspace.id, "E0599", &repair, true)?;
        record_repair_feedback(
            &workspace,
            &workspace.id,
            "E0308",
            &format!("{repair}\0alias"),
            true,
        )?;
        let foreign =
            crate::models::WorkspaceId::from_uuid(uuid::Uuid::from_u128(99_202)).to_string();
        workspace.db.insert_workspace(
            &foreign,
            &crate::db::CreateWorkspaceInput {
                path: "/tmp/foreign-repair-feedback".to_owned(),
                name: None,
            },
        )?;
        record_repair_feedback(&workspace, &foreign, "E0308", &repair, true)?;
        assert_eq!(native(&workspace.review(&session_id, 0.6, false)?).len(), 1);
        record_repair_feedback(&workspace, &workspace.id, "E0308", &card, true)?;
        assert!(native(&workspace.review(&session_id, 0.0, false)?).is_empty());
        assert_eq!(
            workspace.db.list_evidence_spans_for_session(&session_id)?,
            sources
        );
        Ok(())
    }

    #[test]
    fn native_mining_and_feedback_are_reused_only_inside_one_source_snapshot() -> TestResult {
        let workspace = Workspace::new()?;
        let (id, _, _) = workspace.episode(99_301, "Cached feedback work item.", COMMAND, true)?;
        let session = workspace.db.get_session(&id)?.ok_or("session")?;
        let spans = workspace.db.list_evidence_spans_for_session(&id)?;
        workspace.db.begin_read_snapshot()?;
        let evidence = SessionEvidence::new(&workspace.id, &session, &spans);
        assert_eq!(evidence.native().candidates.len(), 1);
        let candidate = &evidence.native().candidates[0];
        let support = evidence.support(candidate).ok_or("support")?;
        assert!(evidence.reconstruct(&support).is_some());
        let first = evidence.withheld_repairs(&workspace.db)?;
        assert!(first.is_empty());
        assert!(std::ptr::eq(
            first,
            evidence.withheld_repairs(&workspace.db)?
        ));
        assert!(std::ptr::eq(evidence.native(), evidence.native()));
        drop(evidence);
        workspace.db.rollback_read_snapshot()?;
        Ok(())
    }

    #[test]
    fn unavailable_feedback_cannot_become_a_successful_review_or_validation() -> TestResult {
        let workspace = Workspace::new()?;
        let (id, _, _) = workspace.episode(99_401, "Storage feedback work item.", COMMAND, true)?;
        let preview = workspace.review(&id, 0.6, false)?;
        assert_eq!(native(&preview).len(), 1);
        let session = workspace.db.get_session(&id)?.ok_or("session")?;
        workspace
            .db
            .execute_raw("ALTER TABLE error_repair_links RENAME TO unavailable_native_feedback")?;
        let mut candidates = Vec::new();
        let error = score_candidates(
            &workspace.db,
            &workspace.id,
            &session,
            &mut candidates,
            0.6,
            10,
        )
        .expect_err("missing feedback authority");
        assert!(!error.message().contains("unavailable_native_feedback"));
        assert!(validate_draft(&workspace.db, &workspace.id, native(&preview)[0]).is_err());
        workspace.db.begin_read_snapshot()?;
        workspace.db.rollback_read_snapshot()?;
        assert!(
            NativeRepairCandidates::default()
                .withheld(&workspace.db, &workspace.id)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn recorded_repair_validation_leaves_feedback_rollback_with_its_caller() -> TestResult {
        let workspace = Workspace::new()?;
        let (session, repair, _) =
            workspace.episode(99_501, "Rollback feedback work item.", COMMAND, true)?;
        let written = workspace.review(&session, 0.6, true)?;
        assert_eq!(native(&written).len(), 1);
        let stored = workspace
            .db
            .get_curation_candidate(&workspace.id, &native(&written)[0].candidate_id)?
            .ok_or("stored proposal")?;
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        let history = workspace
            .db
            .list_error_repair_links(&workspace.id, "rustc:E0277")?;
        let result: crate::db::Result<()> = workspace.db.with_transaction(|| {
            crate::core::error_diagnosis::record_error_repair_links(
                &workspace.db,
                &workspace.id,
                &crate::core::error_recall::from_rustc(Some("E0277"), "rollback feedback"),
                &crate::core::error_diagnosis::ErrorRepairLinkRecording {
                    harmful_repairs: vec![repair.clone()],
                    ..Default::default()
                },
            )?;
            assert!(validate_recorded(&workspace.db, &stored).is_err());
            workspace
                .db
                .execute_raw("INSERT INTO missing_native_feedback_rollback VALUES (1)")?;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            workspace
                .db
                .list_error_repair_links(&workspace.id, "rustc:E0277")?,
            history
        );
        validate_recorded(&workspace.db, &stored).map_err(|error| error.message)?;
        assert!(
            workspace
                .db
                .list_memories(&workspace.id, None, true)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn compact_card_cannot_bypass_the_complete_source_byte_budget() -> TestResult {
        let workspace = Workspace::new()?;
        let text = format!("{FIX}\n{}", "Context for validated records. ".repeat(350));
        assert!(text.len() > MAX_COMPLETE_REPAIR_BYTES);
        assert!(text.len() < LEGACY_EXCERPT_CAP - 3);
        let (session_id, repair_id, _) = workspace.episode_with_repair(
            98_003,
            "Large complete repair work item.",
            COMMAND,
            true,
            &text,
        )?;
        let session = workspace.db.get_session(&session_id)?.ok_or("session")?;
        let spans = workspace.db.list_evidence_spans_for_session(&session_id)?;
        let repair = spans
            .iter()
            .find(|span| span.id == repair_id)
            .ok_or("source repair")?;
        assert!(complete_source(repair));
        assert!(repair.is_search_admitted_for_session(&workspace.id, &session));
        let arcs = session_failure_arcs(&workspace.id, &session, &spans);
        let arc = arcs.first().ok_or("failure arc")?;
        let card = IncidentCardSources::new(&spans)
            .draft(&workspace.id, &session.id, arc)
            .ok_or("small card control")?;
        let compact_fix = card
            .input
            .excerpt
            .lines()
            .find_map(|line| line.strip_prefix("Fix: "))
            .ok_or("compact fix")?;
        assert!(complete_fix(compact_fix));
        assert!(candidates(&workspace.id, &session, &spans).is_empty());
        assert!(native(&workspace.review(&session.id, 0.6, false)?).is_empty());
        assert_eq!(
            workspace.db.list_evidence_spans_for_session(&session.id)?,
            spans
        );
        assert!(
            workspace
                .db
                .list_memories(&workspace.id, None, true)?
                .is_empty()
        );
        Ok(())
    }
}
