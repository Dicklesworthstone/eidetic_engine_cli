//! Native Jest evidence through recall, review, validation and durable apply.

use super::*;
use crate::core::cass_error_recall::{record_session_error_recall, structured_error_diagnostics};
use crate::core::error_diagnosis::{error_recall_report, recalled_repair_evidence};

const JEST: &str = "npm test -- --runInBand --verbose";
const FAILED: &str = include_str!("../../tests/fixtures/jest_terminal/failed.txt");
const PASSED: &str = include_str!("../../tests/fixtures/jest_terminal/passed.txt");
const REPAIR: &str = "I fixed the cache lookup by using the same normalized key for reads and writes in src/cache.ts. The cache no longer misses entries created with mixed-case input.";

fn native_episode(
    workspace: &Workspace,
    seed: u128,
    failure: &str,
    retry_command: &str,
    proof: &str,
    proof_status: Option<bool>,
) -> TestResult<Episode> {
    let session = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
    workspace.db.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: workspace.id.clone(),
            cass_session_id: format!("/sessions/jest-{seed}.jsonl"),
            source_path: Some(format!("/sessions/jest-{seed}.jsonl")),
            agent_name: Some("claude_code".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 5,
            token_count: None,
            content_hash: format!(
                "blake3:{}",
                blake3::hash(format!("jest-{seed}").as_bytes()).to_hex()
            ),
            metadata_json: None,
        },
    )?;
    let mut episode = Episode {
        session,
        repair: String::new(),
        last_line: 0,
        seed,
    };
    workspace.row(
        &mut episode,
        "tool_call",
        "assistant",
        json!({
            "type":"tool_use", "id":"failed", "name":"Bash", "input":{"command":JEST}
        }),
    )?;
    workspace.row(
        &mut episode,
        "tool_result",
        "user",
        json!({
            "type":"tool_result", "tool_use_id":"failed", "content":failure, "is_error":true
        }),
    )?;
    episode.repair = workspace.message(&mut episode, REPAIR)?;
    workspace.row(
        &mut episode,
        "tool_call",
        "assistant",
        json!({
            "type":"tool_use", "id":"retry", "name":"Bash", "input":{"command":retry_command}
        }),
    )?;
    let mut result = json!({"type":"tool_result", "tool_use_id":"retry", "content":proof});
    if let Some(status) = proof_status {
        result["is_error"] = json!(status);
    }
    workspace.row(&mut episode, "tool_result", "user", result)?;
    Ok(episode)
}

#[test]
fn native_jest_repair_reaches_recall_card_review_and_explicit_durable_apply() -> TestResult {
    let workspace = Workspace::new()?;
    let episode = native_episode(&workspace, 91_001, FAILED, JEST, PASSED, Some(false))?;
    let before = workspace
        .db
        .list_evidence_spans_for_session(&episode.session)?;
    let diagnostic = structured_error_diagnostics(FAILED)
        .into_iter()
        .next()
        .ok_or("Jest diagnostic")?;
    let imported = record_session_error_recall(&workspace.db, &workspace.id, &episode.session)?;
    assert_eq!(imported.failures_seen, 1);
    assert_eq!(imported.fingerprints_recorded, 1);
    assert_eq!(imported.resolved_failures, 1);
    assert_eq!(imported.repair_links_recorded, 2);
    assert_eq!(imported.incident_cards_recorded, 1);
    let recall = error_recall_report(&workspace.db, &workspace.id, &diagnostic)?;
    assert!(recall.exact);
    assert!(recall.helpful_repairs.contains(&episode.repair));
    assert_eq!(recall.proof_links.len(), 1);
    let evidence = recalled_repair_evidence(&workspace.db, &workspace.id, &recall)?;
    assert!(evidence.iter().any(|source| {
        source.role == "incident_card"
            && source
                .text
                .as_deref()
                .is_some_and(|text| text.contains("normalized key"))
    }));
    assert!(
        workspace
            .db
            .list_memories(&workspace.id, None, true)?
            .is_empty()
    );

    let reviewed = workspace.review(&episode, 0.6, true)?;
    let candidates = native(&reviewed);
    assert_eq!(candidates.len(), 1, "{:?}", reviewed.candidates);
    let candidate = candidates[0];
    assert_eq!(candidate.confidence, 0.6);
    assert_eq!(candidate.source_ids, vec![episode.repair.clone()]);
    assert!(candidate.proposed_content.contains(REPAIR));
    assert!(
        !candidate
            .proposed_content
            .contains("PRIVATE_STACK_SENTINEL")
    );
    assert!(candidate.persisted);
    let validated = validate_curation_candidate(&CurateValidateOptions {
        workspace_path: &workspace.path,
        database_path: Some(&workspace.database),
        candidate_id: &candidate.candidate_id,
        actor: Some("native-jest-regression"),
        dry_run: false,
    })
    .map_err(|error| error.message())?;
    assert!(
        validated.validation.errors.is_empty(),
        "{:?}",
        validated.validation
    );
    let applied = apply_curation_candidate(&CurateApplyOptions {
        workspace_path: &workspace.path,
        database_path: Some(&workspace.database),
        candidate_id: &candidate.candidate_id,
        actor: Some("native-jest-regression"),
        dry_run: false,
        allow_tombstone_load_bearing: false,
    })
    .map_err(|error| error.message())?;
    assert_eq!(
        applied.application.status, "applied",
        "{:?}",
        applied.application
    );
    let memory_id = applied
        .application
        .created_memory_id
        .as_deref()
        .ok_or("applied lesson")?;
    let memory = workspace
        .db
        .get_memory(memory_id)?
        .ok_or("durable memory")?;
    assert_eq!(memory.content, candidate.proposed_content);
    assert_eq!(
        workspace.db.list_memories(&workspace.id, None, true)?.len(),
        1
    );
    for source in before {
        let current = workspace
            .db
            .get_evidence_span(&source.id)?
            .ok_or("retained source")?;
        assert_eq!(current.excerpt, source.excerpt);
        assert_eq!(current.content_hash, source.content_hash);
        assert_eq!(
            current.canonical_provenance_uri(),
            source.canonical_provenance_uri()
        );
        if source.id == episode.repair {
            assert_eq!(current.memory_id.as_deref(), Some(memory_id));
        } else {
            assert!(
                current.memory_id.is_none(),
                "tool records remain proof, not memory sources"
            );
        }
    }
    let again = record_session_error_recall(&workspace.db, &workspace.id, &episode.session)?;
    assert_eq!(again.incident_cards_recorded, 0);
    assert_eq!(
        workspace.db.list_memories(&workspace.id, None, true)?.len(),
        1
    );
    Ok(())
}

#[test]
fn native_jest_incomplete_or_different_retries_cannot_become_cards_or_reviewed_lessons()
-> TestResult {
    let workspace = Workspace::new()?;
    let aggregate = PASSED
        .lines()
        .filter(|line| !line.starts_with("  cache") && !line.starts_with("    ✓"))
        .collect::<Vec<_>>()
        .join("\n");
    let renamed = PASSED.replace("returns cached value", "unrelated replacement");
    let skipped = PASSED
        .replace(
            "    ✓ returns cached value (2 ms)",
            "    ○ skipped returns cached value",
        )
        .replace("2 passed, 2 total", "1 skipped, 1 passed, 2 total");
    let truncated = format!("{PASSED}\nOutput truncated");
    for (offset, retry, proof, status) in [
        (0, JEST, aggregate.as_str(), Some(false)),
        (1, JEST, renamed.as_str(), Some(false)),
        (2, JEST, skipped.as_str(), Some(false)),
        (3, JEST, truncated.as_str(), Some(false)),
        (
            4,
            "npm test -- --runInBand --verbose other.test.ts",
            PASSED,
            Some(false),
        ),
        (5, JEST, PASSED, None),
    ] {
        let episode = native_episode(&workspace, 91_010 + offset, FAILED, retry, proof, status)?;
        let report = record_session_error_recall(&workspace.db, &workspace.id, &episode.session)?;
        assert_eq!(report.fingerprints_recorded, 1, "case {offset}");
        assert_eq!(report.resolved_failures, 0, "case {offset}");
        assert_eq!(report.repair_links_recorded, 0, "case {offset}");
        assert_eq!(report.incident_cards_recorded, 0, "case {offset}");
        assert!(
            native(&workspace.review(&episode, 0.0, false)?).is_empty(),
            "case {offset}"
        );
    }
    assert!(
        workspace
            .db
            .list_memories(&workspace.id, None, true)?
            .is_empty()
    );
    Ok(())
}
