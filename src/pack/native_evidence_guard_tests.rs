//! Native evidence remains part of the token budget after memory suppression.

use super::*;
use crate::models::{LineSpan, SessionId};

fn mixed_draft() -> PackDraft {
    let candidates = [
        (
            1,
            "Use bounded worker retries with backoff.",
            TrustClass::HumanExplicit,
        ),
        (
            2,
            "Disable every retry after a transient fault.",
            TrustClass::AgentAssertion,
        ),
    ]
    .into_iter()
    .map(|(number, content, class)| {
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(number));
        PackCandidate::new(PackCandidateInput {
            memory_id,
            section: PackSection::ProceduralRules,
            content: content.to_owned(),
            estimated_tokens: 8,
            relevance: UnitScore::parse(0.9).unwrap(),
            utility: UnitScore::parse(0.5).unwrap(),
            provenance: vec![
                PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "guard fixture").unwrap(),
            ],
            why: "worker retry policy".to_owned(),
        })
        .unwrap()
        .with_trust_signal(PackTrustSignal::new(class, None))
    });
    let mut draft = assemble_draft(
        "worker retry policy",
        TokenBudget::new(128).unwrap(),
        candidates,
    )
    .unwrap();
    assert_eq!(draft.items.len(), 2);
    let session = SessionId::from_uuid(uuid::Uuid::from_u128(0x2000)).to_string();
    draft.evidence_items.push(PackEvidenceItem {
        rank: 3,
        evidence_id: EvidenceId::from_uuid(uuid::Uuid::from_u128(0x3000)).to_string(),
        entity_revision: format!("blake3:{}", "a".repeat(64)),
        session_id: session.clone(),
        start_line: 3,
        end_line: 4,
        section: PackSection::Evidence,
        content: "The café worker recovered after its bounded retry.".to_owned(),
        estimated_tokens: 17,
        relevance: UnitScore::parse(0.8).unwrap(),
        utility: UnitScore::parse(0.5).unwrap(),
        provenance: vec![
            PackProvenance::new(
                ProvenanceUri::CassSession {
                    session,
                    span: Some(LineSpan::range(3, 4).unwrap()),
                },
                "imported worker incident",
            )
            .unwrap(),
        ],
        why: "live-admitted CASS incident".to_owned(),
        trust: PackTrustSignal::new(TrustClass::CassEvidence, None),
    });
    draft.used_tokens += 17;
    draft.hash = Some("blake3:old-pack".to_owned());
    draft
}

fn pair() -> Vec<(String, String)> {
    vec![(
        MemoryId::from_uuid(uuid::Uuid::from_u128(1)).to_string(),
        MemoryId::from_uuid(uuid::Uuid::from_u128(2)).to_string(),
    )]
}

#[test]
fn suppressing_a_memory_does_not_erase_native_evidence_token_cost() {
    let mut draft = mixed_draft();
    let evidence = draft.evidence_items.clone();
    assert_eq!(draft.used_tokens, 33);
    assert_eq!(draft.apply_contradiction_guard(&pair(), false), 1);
    assert_eq!(draft.items.len(), 1);
    assert_eq!(
        draft.used_tokens, 25,
        "8 memory tokens plus 17 evidence tokens"
    );
    assert_eq!(draft.selection_audit.budget_used, 25);
    assert_eq!(
        draft.evidence_items, evidence,
        "identity, revision, source and text stay intact"
    );
    assert!(
        draft.hash.is_none(),
        "a changed selection invalidates the old pack hash"
    );
}

#[test]
fn public_budget_metrics_include_all_retained_evidence() {
    let mut draft = mixed_draft();
    draft.apply_contradiction_guard(&pair(), false);
    let metrics = draft.quality_metrics();
    assert_eq!(metrics.item_count, 2);
    assert_eq!(metrics.used_tokens, 25);
    assert_eq!(metrics.budget_utilization, 25.0 / 128.0);
    assert_eq!(
        metrics
            .sections
            .iter()
            .map(|section| section.used_tokens)
            .sum::<u32>(),
        25
    );
}

#[test]
fn repeated_guard_application_keeps_the_correct_mixed_budget() {
    let mut draft = mixed_draft();
    assert_eq!(draft.apply_contradiction_guard(&pair(), false), 1);
    let once = draft.clone();
    assert_eq!(draft.apply_contradiction_guard(&pair(), false), 0);
    assert_eq!(draft, once);
    assert_eq!(draft.used_tokens, 25);
}

#[test]
fn forced_conflict_disclosure_does_not_mutate_a_mixed_pack() {
    let mut draft = mixed_draft();
    let before = draft.clone();
    assert_eq!(draft.apply_contradiction_guard(&pair(), true), 0);
    assert_eq!(draft, before);
}

#[test]
fn unrelated_pairs_do_not_mutate_or_reclassify_native_evidence() {
    let mut draft = mixed_draft();
    let before = draft.clone();
    let native_id = draft.evidence_items[0].evidence_id.clone();
    let memory_id = draft.items[0].memory_id.to_string();
    assert_eq!(
        draft.apply_contradiction_guard(&[(native_id, memory_id)], false),
        0
    );
    assert_eq!(
        draft, before,
        "a memory-only guard must not invent a native-memory identity"
    );
}

#[test]
fn memory_only_guard_retains_its_previous_budget_behavior() {
    let mut draft = mixed_draft();
    draft.evidence_items.clear();
    draft.used_tokens = 16;
    assert_eq!(draft.apply_contradiction_guard(&pair(), false), 1);
    assert_eq!(draft.used_tokens, 8);
    assert_eq!(draft.selection_audit.budget_used, 8);
}

fn graph_memory_id(number: u128) -> MemoryId {
    MemoryId::from_uuid(uuid::Uuid::from_u128(number))
}

fn mixed_chain_draft() -> PackDraft {
    let candidates = [
        (
            1,
            "Use bounded worker retries with backoff.",
            TrustClass::HumanExplicit,
        ),
        (
            2,
            "Disable every retry after a transient fault.",
            TrustClass::AgentAssertion,
        ),
        (
            3,
            "Record the incident identifier before restarting the worker.",
            TrustClass::LegacyImport,
        ),
    ]
    .into_iter()
    .map(|(number, content, class)| {
        let memory_id = graph_memory_id(number);
        PackCandidate::new(PackCandidateInput {
            memory_id,
            section: PackSection::ProceduralRules,
            content: content.to_owned(),
            estimated_tokens: 8,
            relevance: UnitScore::parse(0.9).unwrap(),
            utility: UnitScore::parse(0.5).unwrap(),
            provenance: vec![
                PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "graph fixture").unwrap(),
            ],
            why: "worker recovery policy".to_owned(),
        })
        .unwrap()
        .with_trust_signal(PackTrustSignal::new(class, None))
    });
    let mut draft = assemble_draft(
        "worker recovery policy",
        TokenBudget::new(128).unwrap(),
        candidates,
    )
    .unwrap();
    assert_eq!(draft.items.len(), 3);
    let mut evidence = mixed_draft().evidence_items.into_iter().next().unwrap();
    evidence.rank = 4;
    draft.used_tokens += evidence.estimated_tokens;
    draft.evidence_items.push(evidence);
    draft.hash = Some("blake3:old-chain-pack".to_owned());
    draft
}

fn chain_pairs() -> Vec<(String, String)> {
    vec![
        (
            graph_memory_id(2).to_string(),
            graph_memory_id(3).to_string(),
        ),
        (
            graph_memory_id(1).to_string(),
            graph_memory_id(2).to_string(),
        ),
    ]
}

#[test]
fn graph_guard_retains_compatible_memory_and_native_evidence() {
    let mut draft = mixed_chain_draft();
    let evidence = draft.evidence_items.clone();
    assert_eq!(draft.used_tokens, 41);
    assert_eq!(draft.apply_contradiction_guard(&chain_pairs(), false), 1);
    let selected: BTreeSet<_> = draft
        .items
        .iter()
        .map(|item| item.memory_id.to_string())
        .collect();
    assert_eq!(
        selected,
        BTreeSet::from([
            graph_memory_id(1).to_string(),
            graph_memory_id(3).to_string()
        ])
    );
    assert_eq!(draft.omitted.len(), 1);
    assert_eq!(draft.omitted[0].memory_id, graph_memory_id(2));
    assert_eq!(
        draft.omitted[0].reason,
        PackOmissionReason::ContradictionSuppressed
    );
    assert_eq!(draft.evidence_items, evidence);
    assert_eq!(draft.used_tokens, 33);
    assert_eq!(draft.selection_audit.selected_count, 2);
    assert_eq!(draft.selection_audit.budget_used, 33);
    assert!(draft.hash.is_none());

    let metrics = draft.quality_metrics();
    assert_eq!(metrics.item_count, 3);
    assert_eq!(metrics.used_tokens, 33);
    assert_eq!(
        metrics
            .sections
            .iter()
            .map(|section| section.used_tokens)
            .sum::<u32>(),
        33
    );
}

#[test]
fn graph_guard_public_draft_is_identical_for_equivalent_pair_inputs() {
    let initial = mixed_chain_draft();
    let mut expected = initial.clone();
    expected.apply_contradiction_guard(&chain_pairs(), false);
    let mut reversed = chain_pairs();
    reversed.reverse();
    let flipped: Vec<_> = chain_pairs().into_iter().map(|(a, b)| (b, a)).collect();
    let mut noisy = reversed.clone();
    noisy.extend(flipped.clone());
    noisy.push((
        graph_memory_id(1).to_string(),
        graph_memory_id(1).to_string(),
    ));
    noisy.push((
        initial.evidence_items[0].evidence_id.clone(),
        graph_memory_id(3).to_string(),
    ));
    noisy.push((
        format!(" {} ", graph_memory_id(1)),
        format!(" {} ", graph_memory_id(2)),
    ));
    for variant in [reversed, flipped, noisy] {
        let mut actual = initial.clone();
        assert_eq!(actual.apply_contradiction_guard(&variant, false), 1);
        assert_eq!(
            actual, expected,
            "selection, omissions, audit and evidence must agree"
        );
    }
}

#[test]
fn graph_guard_is_idempotent_after_preserving_a_compatible_member() {
    let mut draft = mixed_chain_draft();
    assert_eq!(draft.apply_contradiction_guard(&chain_pairs(), false), 1);
    let once = draft.clone();
    assert_eq!(draft.apply_contradiction_guard(&chain_pairs(), false), 0);
    assert_eq!(draft, once);
    assert_eq!(draft.used_tokens, 33);
}

#[test]
fn malformed_self_conflicts_cannot_delete_a_selected_memory() {
    let mut draft = mixed_draft();
    let before = draft.clone();
    let id = graph_memory_id(1).to_string();
    assert_eq!(
        draft.apply_contradiction_guard(&[(id.clone(), id)], false),
        0
    );
    assert_eq!(draft, before);
}

#[test]
fn forced_graph_conflicts_preserve_the_original_draft() {
    let mut draft = mixed_chain_draft();
    let before = draft.clone();
    assert_eq!(draft.apply_contradiction_guard(&chain_pairs(), true), 0);
    assert_eq!(draft, before);
}

#[test]
fn graph_guard_uses_trust_not_canonical_detector_order() {
    let mut draft = mixed_chain_draft();
    for item in &mut draft.items {
        if item.memory_id == graph_memory_id(1) {
            item.trust = PackTrustSignal::new(TrustClass::LegacyImport, None);
        } else if item.memory_id == graph_memory_id(3) {
            item.trust = PackTrustSignal::new(TrustClass::HumanExplicit, None);
        }
    }
    let mut edges = chain_pairs();
    edges.sort();
    assert_eq!(draft.apply_contradiction_guard(&edges, false), 1);
    assert_eq!(draft.items.len(), 2);
    assert_eq!(draft.omitted[0].memory_id, graph_memory_id(2));
    assert_eq!(draft.used_tokens, 33);
}
