//! Mixed-source budget competition must retain native identity and real replay.

use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateMemoryInput, CreateSessionInput, CreateWorkspaceInput,
    EvidenceProducerKind,
};
use crate::pack::{PackAssemblyOptions, PackRuleItem, PackSelectionPhase};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn memory(seed: u128, relevance: f32) -> TestResult<PackCandidate> {
    Ok(PackCandidate::new(PackCandidateInput {
        memory_id: MemoryId::from_uuid(uuid::Uuid::from_u128(seed)),
        section: PackSection::Evidence,
        content: format!("Historical packaging observation {seed}."),
        estimated_tokens: 1,
        relevance: UnitScore::parse(relevance)?,
        utility: UnitScore::neutral(),
        provenance: vec![PackProvenance::new(
            ProvenanceUri::from_str(&format!("manual://mixed-budget-{seed}"))?,
            "Observed packaging history",
        )?],
        why: "Historical packaging evidence matched the task.".to_owned(),
    })?)
}

fn rule(seed: u128, relevance: f32, tokens: u32) -> TestResult<PackRuleItem> {
    let id = RuleId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
    Ok(PackRuleItem {
        rank: 0,
        rule_id: id.clone(),
        entity_revision: format!("blake3:{}", "1".repeat(64)),
        section: PackSection::ProceduralRules,
        content: format!("Review the packaging checks for rule {seed}."),
        estimated_tokens: tokens,
        relevance: UnitScore::parse(relevance)?,
        utility: UnitScore::neutral(),
        provenance: vec![PackProvenance::new(
            ProvenanceUri::from_str(&format!("ee://rule/{id}"))?,
            "Native procedural guidance",
        )?],
        why: "Native guidance matched the task.".to_owned(),
        trust: PackTrustSignal::new(
            TrustClass::AgentAssertion,
            Some("procedural_rule".to_owned()),
        ),
    })
}

fn evidence(seed: u128, relevance: f32, tokens: u32) -> TestResult<DirectEvidencePackCandidate> {
    let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
    let session_id = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(9)).to_string();
    Ok(DirectEvidencePackCandidate {
        linked_memory_id: None,
        source_role: Some("assistant".to_owned()),
        span_kind: "message".to_owned(),
        incident_card: false,
        source: "import".to_owned(),
        item: PackEvidenceItem {
            rank: 0,
            evidence_id: id,
            entity_revision: format!("blake3:{}", "2".repeat(64)),
            session_id: session_id.clone(),
            start_line: 17,
            end_line: 18,
            section: PackSection::Evidence,
            content: format!("assistant: The packaging repair {seed} passed its regression."),
            estimated_tokens: tokens,
            relevance: UnitScore::parse(relevance)?,
            utility: UnitScore::neutral(),
            provenance: vec![PackProvenance::new(
                ProvenanceUri::from_str(&format!("cass-session://{session_id}#L17-18"))?,
                "Observed repair and verification",
            )?],
            why: "Native transcript evidence matched the task.".to_owned(),
            trust: PackTrustSignal::new(
                TrustClass::CassEvidence,
                Some("imported_transcript_excerpt".to_owned()),
            ),
        },
    })
}

fn request(tokens: u32) -> TestResult<ContextRequest> {
    let mut input = ContextRequestInput::for_query("packaging repair verification");
    input.max_tokens = Some(tokens);
    Ok(ContextRequest::new(input)?)
}

fn memory_draft(
    request: &ContextRequest,
    candidate: PackCandidate,
    protect_failure: bool,
) -> TestResult<PackDraft> {
    let draft = crate::pack::assemble_draft_with_profile_and_options(
        request.profile,
        request.query.clone(),
        request.budget,
        vec![candidate],
        PackAssemblyOptions {
            include_anti_pattern_first: protect_failure,
            lod_budget_shares: None,
            ..PackAssemblyOptions::default()
        },
    )?;
    assert_eq!(
        draft.items.len(),
        1,
        "the memory phase must really spend tokens"
    );
    assert!(draft.used_tokens > 0);
    Ok(draft)
}

#[test]
fn a_top_evidence_hit_displaces_weaker_memory_and_survives_persisted_replay() -> TestResult {
    assert_native_evidence_replay_after_memory_displacement(false)?;
    assert_native_evidence_replay_after_memory_displacement(true)
}

fn assert_native_evidence_replay_after_memory_displacement(multiple_lods: bool) -> TestResult {
    let directory = tempfile::tempdir()?;
    let workspace = directory.path().canonicalize()?;
    let workspace_id = crate::core::workspace::stable_workspace_id(&workspace);
    let database_path = workspace.join("mixed-pack.db");
    let db = DbConnection::open_file(&database_path)?;
    db.migrate()?;
    db.insert_workspace(
        &workspace_id,
        &CreateWorkspaceInput {
            path: workspace.to_string_lossy().into_owned(),
            name: None,
        },
    )?;
    let candidate = memory(71, 0.6)?;
    db.insert_memory(
        &candidate.memory_id.to_string(),
        &CreateMemoryInput {
            workspace_id: workspace_id.clone(),
            content: candidate.content.clone(),
            level: "semantic".to_owned(),
            kind: "note".to_owned(),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.5,
            importance: 0.5,
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            provenance_uri: Some("manual://mixed-budget-71".to_owned()),
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        },
    )?;
    let session_id = crate::models::SessionId::from_uuid(uuid::Uuid::from_u128(72)).to_string();
    db.insert_session(
        &session_id,
        &CreateSessionInput {
            workspace_id: workspace_id.clone(),
            cass_session_id: "mixed-pack-session".to_owned(),
            source_path: None,
            agent_name: Some("claude".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 2,
            token_count: None,
            content_hash: format!("blake3:{}", "3".repeat(64)),
            metadata_json: None,
        },
    )?;
    let evidence_id = EvidenceId::from_uuid(uuid::Uuid::from_u128(73)).to_string();
    let source = format!(
        "{}\n{}",
        serde_json::json!({"type": "user", "content": "How was the packaging repair verified?"}),
        serde_json::json!({"type": "assistant", "content": "The package retained the correct workspace identity. The replay regression passed and the restored source hashes matched. ".repeat(12)})
    );
    db.insert_evidence_span(
        &evidence_id,
        &CreateEvidenceSpanInput {
            workspace_id: workspace_id.clone(),
            session_id: session_id.clone(),
            memory_id: None,
            producer_kind: EvidenceProducerKind::CassImport,
            cass_span_id: "mixed-pack-source-window".to_owned(),
            span_kind: "message".to_owned(),
            start_line: 17,
            end_line: 18,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".to_owned()),
            content_hash: format!("blake3:{}", blake3::hash(source.as_bytes()).to_hex()),
            excerpt: source,
            metadata_json: None,
            inherited_redaction_classes: Vec::new(),
        },
    )?;
    let span = db
        .get_search_admitted_evidence_span(&evidence_id, &workspace_id)?
        .ok_or("native evidence did not pass real database admission")?;
    let native = direct_evidence_pack_candidate(span, 0.98, "import", "packaging repair", None)
        .ok_or("live evidence did not project into a pack candidate")?;
    let expected = native.item.clone();
    let request = request(expected.estimated_tokens)?;
    let mut draft = memory_draft(&request, candidate.clone(), false)?;
    if multiple_lods {
        // A second selected representation of the same memory must leave one
        // durable omission: pack_omissions is unique by (pack, memory).
        let mut preview = draft.items[0].clone();
        preview.rank = 2;
        preview.content = "Historical packaging preview.".to_owned();
        preview.estimated_tokens = 1;
        preview.selected_in = PackSelectionPhase::CoverageFill;
        draft.used_tokens += preview.estimated_tokens;
        draft.items.push(preview);
        draft.selection_audit.selected_count = draft.items.len();
        draft.selection_audit.budget_used = draft.used_tokens;
    }
    let displaced_tokens = draft.used_tokens;
    assert!(draft.used_tokens + expected.estimated_tokens > request.budget.max_tokens());
    let mut degraded = Vec::new();
    append_ranked_native_pack_items(
        Vec::new(),
        vec![native],
        &request,
        &mut draft,
        &mut degraded,
    );
    assert!(
        draft.items.is_empty(),
        "weaker memory must release the shared budget"
    );
    assert_eq!(
        draft.evidence_items.len(),
        1,
        "the top native hit must enter the pack"
    );
    assert_eq!(draft.evidence_items[0].content, expected.content);
    assert_eq!(
        draft.evidence_items[0].entity_revision,
        expected.entity_revision
    );
    assert_eq!(
        draft.evidence_items[0].rendered_provenance(),
        expected.rendered_provenance()
    );
    assert_eq!(draft.evidence_items[0].rank, 1);
    assert_eq!(draft.used_tokens, request.budget.max_tokens());
    assert_eq!(draft.omitted.len(), 1);
    assert_eq!(draft.omitted[0].memory_id, candidate.memory_id);
    assert_eq!(draft.omitted[0].estimated_tokens, displaced_tokens);
    assert_eq!(
        draft.omitted[0].reason,
        PackOmissionReason::TokenBudgetExceeded
    );
    assert_eq!(draft.selection_audit.selected_count, 1);
    assert_eq!(draft.selection_audit.omitted_count, 1);
    assert!(draft.selection_audit.steps.is_empty());
    assert!(draft.selection_audit.selected_items.is_empty());
    draft.hash = Some(compute_pack_hash(&request, &draft, &degraded));
    let pack_id = persist_pack_record_with_pack_id(
        &db,
        &workspace,
        &request,
        &draft,
        &degraded,
        &BTreeSet::new(),
        None,
        None,
        PackId::from_uuid(uuid::Uuid::from_u128(74)),
        &mut PackPersistenceSubspans::default(),
    )?;
    db.close()?;
    let reopened = DbConnection::open_file_read_only(&database_path)?;
    assert!(reopened.get_pack_items(&pack_id)?.is_empty());
    let evidence_rows = reopened.get_pack_evidence_items(&pack_id)?;
    assert_eq!(evidence_rows.len(), 1);
    assert_eq!(evidence_rows[0].evidence_id, evidence_id);
    let record = reopened
        .get_pack_record(&pack_id)?
        .ok_or("persisted pack missing")?;
    assert_eq!(record.item_count, 1);
    assert_eq!(record.used_tokens, draft.used_tokens);
    let parsed = crate::db::parse_stored_pack_ledger(&record);
    let ledger = parsed
        .available_ledger()
        .ok_or("mixed pack replay did not verify")?;
    let selected = ledger["selectedItems"]
        .as_array()
        .ok_or("replay selectedItems missing")?;
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0]["entityKind"], "evidence_span");
    assert_eq!(selected[0]["entityId"], evidence_id);
    assert_eq!(selected[0]["entityRevision"], expected.entity_revision);
    assert_eq!(selected[0]["rank"], 1);
    assert!(selected[0].get("memoryId").is_none());
    reopened.close()?;
    Ok(())
}

#[test]
fn native_sources_compete_by_rank_instead_of_rule_then_evidence_phase_order() -> TestResult {
    let request = request(60)?;
    let mut draft = crate::pack::assemble_draft(&request.query, request.budget, Vec::new())?;
    let best = evidence(11, 0.95, 60)?;
    let best_id = best.item.evidence_id.clone();
    let lower = rule(12, 0.8, 40)?;
    let mut degraded = Vec::new();
    append_ranked_native_pack_items(vec![lower], vec![best], &request, &mut draft, &mut degraded);
    assert!(draft.rule_items.is_empty());
    assert_eq!(draft.evidence_items.len(), 1);
    assert_eq!(draft.evidence_items[0].evidence_id, best_id);
    assert_eq!(draft.used_tokens, 60);
    assert_eq!(draft.selection_audit.candidate_count, 2);
    assert_eq!(draft.selection_audit.selected_count, 1);
    assert!(
        degraded
            .iter()
            .any(|entry| entry.message.contains("token_budget_exceeded"))
    );
    Ok(())
}

#[test]
fn native_replacement_is_feasible_atomic_and_keeps_stronger_or_reserved_memories() -> TestResult {
    for (memory_relevance, native_relevance, native_tokens, protected) in [
        (0.6, 0.99, 81, false),
        (0.99, 0.6, 80, false),
        (0.6, 0.6, 80, false),
        (0.6, 0.99, 80, true),
    ] {
        let request = request(80)?;
        let mut candidate = memory(21, memory_relevance)?;
        if protected {
            candidate.section = PackSection::Failures;
        }
        let mut draft = memory_draft(&request, candidate, protected)?;
        if protected {
            assert_eq!(
                draft.items[0].selected_in,
                PackSelectionPhase::AntiPatternFirst
            );
        }
        let selected_before = draft.items.clone();
        let tokens_before = draft.used_tokens;
        let omitted_before = draft.omitted.clone();
        append_ranked_native_pack_items(
            Vec::new(),
            vec![evidence(22, native_relevance, native_tokens)?],
            &request,
            &mut draft,
            &mut Vec::new(),
        );
        assert_eq!(
            draft.items, selected_before,
            "no partial eviction for an ineligible replacement"
        );
        assert_eq!(draft.used_tokens, tokens_before);
        assert_eq!(draft.omitted, omitted_before);
        assert!(draft.evidence_items.is_empty());
    }
    Ok(())
}

#[test]
fn result_caps_and_pages_use_one_deterministic_mixed_candidate_order() -> TestResult {
    for reverse in [false, true] {
        let mut memories = vec![memory(31, 0.7)?, memory(32, 0.5)?];
        let mut rules = vec![rule(33, 0.8, 10)?, rule(34, 0.6, 10)?];
        let mut evidence = vec![evidence(35, 0.9, 10)?, evidence(36, 0.4, 10)?];
        if reverse {
            memories.reverse();
            rules.reverse();
            evidence.reverse();
        }
        let best_evidence_id = EvidenceId::from_uuid(uuid::Uuid::from_u128(35)).to_string();
        let mut cap_memories = memories.clone();
        let mut cap_rules = rules.clone();
        let mut cap_evidence = evidence.clone();
        apply_pagination_with_rules(
            &mut cap_memories,
            &mut cap_rules,
            &mut cap_evidence,
            &None,
            Some(1),
            &mut Vec::new(),
        );
        assert!(cap_memories.is_empty());
        assert!(cap_rules.is_empty());
        assert_eq!(cap_evidence.len(), 1);
        assert_eq!(cap_evidence[0].item.evidence_id, best_evidence_id);
        let page = apply_pagination_with_rules(
            &mut memories,
            &mut rules,
            &mut evidence,
            &Some(ContextPagination {
                offset: 1,
                limit: 2,
                query_hash: "mixed-ranking".to_owned(),
            }),
            Some(5),
            &mut Vec::new(),
        );
        assert_eq!(page.total, 5);
        assert_eq!(page.page_size, 2);
        assert!(page.has_more);
        assert!(page.next_cursor.is_some());
        assert_eq!(memories.len(), 1);
        assert_eq!(
            memories[0].memory_id,
            MemoryId::from_uuid(uuid::Uuid::from_u128(31))
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].rule_id,
            RuleId::from_uuid(uuid::Uuid::from_u128(33)).to_string()
        );
        assert!(evidence.is_empty());
    }
    Ok(())
}

#[test]
fn represented_linked_evidence_does_not_displace_its_selected_memory() -> TestResult {
    let request = request(80)?;
    let candidate = memory(41, 0.6)?;
    let mut linked = evidence(42, 0.99, 80)?;
    linked.linked_memory_id = Some(candidate.memory_id.to_string());
    let mut draft = memory_draft(&request, candidate, false)?;
    let before = draft.items.clone();
    append_ranked_native_pack_items(
        Vec::new(),
        vec![linked],
        &request,
        &mut draft,
        &mut Vec::new(),
    );
    assert_eq!(draft.items, before);
    assert!(draft.evidence_items.is_empty());
    Ok(())
}
