//! Native imported-evidence projections at the context-delta boundary.
//!
//! The selection ledger has already passed the central integrity check and
//! public replay projection before it reaches this module. Do not hydrate a
//! historical item from the live transcript: that would change the baseline
//! and could reintroduce content that is no longer eligible for retrieval.
//! These are the same hash-only item snapshots used for memory deltas, not a
//! reconstruction of the full context-response body.

use serde_json::{Value, json};

use super::{ContextDeltaItemSnapshot, blake3_text_hash_for_replay};
use crate::models::EvidenceId;
use crate::pack::{PackEvidenceItem, pack_item_provenance_json};

pub(super) fn from_item(item: &PackEvidenceItem) -> ContextDeltaItemSnapshot {
    let provenance = pack_item_provenance_json(&item.provenance);
    ContextDeltaItemSnapshot::new(&item.evidence_id)
        .with_field("entityKind", json!("evidence_span"))
        .with_field("entityRevision", json!(&item.entity_revision))
        .with_field("rank", json!(item.rank))
        .with_field("section", json!(item.section.as_str()))
        .with_field("estimatedTokens", json!(item.estimated_tokens))
        .with_field("relevance", json!(item.relevance.into_inner()))
        .with_field("utility", json!(item.utility.into_inner()))
        .with_field("whyHash", json!(blake3_text_hash_for_replay(&item.why)))
        // Native evidence has no memory diversity key. Keep the shared
        // snapshot shape explicit rather than inventing memory metadata.
        .with_field("diversityKeyHash", Value::Null)
        .with_field("provenanceHash", json!(blake3_text_hash_for_replay(&provenance)))
        .with_field("trustClass", json!(item.trust.class.as_str()))
        .with_field(
            "trustSubclass",
            json!(item.trust.subclass.as_deref().map(|value| {
                crate::policy::redact_public_replay_field("trustSubclass", value).content
            })),
        )
}

pub(super) fn from_ledger(item: &Value) -> Result<ContextDeltaItemSnapshot, String> {
    let invalid = || "verified prior pack evidence has an invalid native identity".to_owned();
    if item.get("entityKind").and_then(Value::as_str) != Some("evidence_span")
        || item.get("memoryId").is_some()
    {
        return Err(invalid());
    }
    let evidence_id = item
        .get("evidenceSpanId")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    if evidence_id.parse::<EvidenceId>().is_err()
        || item.get("entityId").and_then(Value::as_str) != Some(evidence_id)
    {
        return Err(invalid());
    }
    let revision = item
        .get("entityRevision")
        .and_then(Value::as_str)
        .filter(|revision| crate::db::is_canonical_blake3_hash(revision))
        .ok_or_else(|| {
            "verified prior pack evidence omitted a canonical entityRevision".to_owned()
        })?;

    // Match the existing memory projection. All values are read from the
    // verified, public ledger, never from mutable evidence/session rows.
    Ok(ContextDeltaItemSnapshot::new(evidence_id)
        .with_field("entityKind", json!("evidence_span"))
        .with_field("entityRevision", json!(revision))
        .with_field("rank", item.get("rank").cloned().unwrap_or_default())
        .with_field("section", item.get("section").cloned().unwrap_or_default())
        .with_field(
            "estimatedTokens",
            item.get("estimatedTokens").cloned().unwrap_or_default(),
        )
        .with_field(
            "relevance",
            item.pointer("/scores/relevance")
                .cloned()
                .unwrap_or_default(),
        )
        .with_field(
            "utility",
            item.pointer("/scores/utility").cloned().unwrap_or_default(),
        )
        .with_field(
            "whyHash",
            item.pointer("/why/hash").cloned().unwrap_or_default(),
        )
        .with_field("diversityKeyHash", Value::Null)
        .with_field(
            "provenanceHash",
            item.pointer("/provenance/hash")
                .cloned()
                .unwrap_or_default(),
        )
        .with_field(
            "trustClass",
            item.get("trustClass").cloned().unwrap_or_default(),
        )
        .with_field(
            "trustSubclass",
            redacted_trust_subclass(item.get("trustSubclass")),
        ))
}

/// Project a ledger `trustSubclass` the same way the item path projects it.
///
/// bd-rm8wj cause 1. `from_item` redacts this field with
/// `redact_public_replay_field`; the ledger path used to pass it through raw.
/// Two projections of the SAME logical item therefore compared unequal, so an
/// unchanged item reported as MODIFIED in every context delta.
///
/// The trigger is narrow and that is why it survived: the redactor is
/// selective. `project-rule` does not trip it, which is why
/// `pack_diff_redaction_change.json.golden` carries that value raw and stays
/// green. `imported_transcript_excerpt` does trip it, which is what the unit
/// tests use.
///
/// Redacting on BOTH sides is safe whichever form the persisted ledger holds,
/// because `redact_public_replay_field` is idempotent on its own output:
/// `redact_public_replay_text` returns an existing
/// `[REDACTED:public_replay_text:<64 hex>]` unchanged with the reason
/// `public_replay_text_already_redacted`, and the non-hash field path returns
/// that report directly. So a stored-raw value gets redacted into agreement and
/// a stored-redacted value passes through untouched. That idempotence is the
/// reason this is a repair rather than a decision about which side wins.
pub(super) fn redacted_trust_subclass(raw: Option<&Value>) -> Value {
    match raw.and_then(Value::as_str) {
        Some(value) => {
            json!(crate::policy::redact_public_replay_field("trustSubclass", value).content)
        }
        // Absent or explicitly null stays null; only a string is projected.
        None => raw.cloned().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_delta::{
        ContextDeltaOptions, ContextDeltaPackSnapshot, compute_context_delta,
    };
    use crate::models::{LineSpan, ProvenanceUri, SessionId, TrustClass, UnitScore};
    use crate::pack::{PackProvenance, PackSection, PackTrustSignal};

    fn evidence(index: u128) -> PackEvidenceItem {
        let session = SessionId::from_uuid(uuid::Uuid::from_u128(0x2000 + index)).to_string();
        PackEvidenceItem {
            rank: 1,
            evidence_id: EvidenceId::from_uuid(uuid::Uuid::from_u128(0x3000 + index)).to_string(),
            entity_revision: format!("blake3:{}", "a".repeat(64)),
            session_id: session.clone(),
            start_line: 3,
            end_line: 4,
            section: PackSection::Evidence,
            content: "Café deployment passed the release checks.".to_owned(),
            estimated_tokens: 8,
            relevance: UnitScore::parse(0.8).expect("relevance"),
            utility: UnitScore::parse(0.5).expect("utility"),
            provenance: vec![
                PackProvenance::new(
                    ProvenanceUri::CassSession {
                        session,
                        span: Some(LineSpan::range(3, 4).expect("line span")),
                    },
                    "CASS evidence",
                )
                .expect("provenance"),
            ],
            why: "matched imported session".to_owned(),
            trust: PackTrustSignal {
                class: TrustClass::CassEvidence,
                subclass: Some("imported_transcript_excerpt".to_owned()),
            },
        }
    }

    // Unit fixture for the already verified ledger projection. The public-CLI
    // regression in no_mocks_e2e exercises the actual persisted ledger writer.
    fn ledger(item: &PackEvidenceItem) -> Value {
        json!({
            "entityKind": "evidence_span",
            "entityId": item.evidence_id,
            "evidenceSpanId": item.evidence_id,
            "entityRevision": item.entity_revision,
            "rank": item.rank,
            "section": item.section.as_str(),
            "estimatedTokens": item.estimated_tokens,
            "scores": {"relevance": item.relevance.into_inner(), "utility": item.utility.into_inner()},
            "why": {"hash": blake3_text_hash_for_replay(&item.why)},
            "provenance": {"hash": blake3_text_hash_for_replay(&pack_item_provenance_json(&item.provenance))},
            "trustClass": item.trust.class.as_str(),
            "trustSubclass": item.trust.subclass,
        })
    }

    fn snapshot(hash: &str, items: Vec<ContextDeltaItemSnapshot>) -> ContextDeltaPackSnapshot {
        ContextDeltaPackSnapshot::new(hash, 0, 1_000_000, 100, items)
    }

    #[test]
    fn native_identity_and_revision_match_the_verified_ledger_projection() {
        let item = evidence(0);
        let current = from_item(&item);
        let prior = from_ledger(&ledger(&item)).expect("native ledger item");
        assert_eq!(current, prior);
        assert_eq!(current.id, item.evidence_id);
        assert!(current.id.parse::<EvidenceId>().is_ok());
        assert!(current.id.parse::<crate::models::MemoryId>().is_err());
        assert_eq!(current.fields["entityKind"], "evidence_span");
        assert_eq!(current.fields["entityRevision"], item.entity_revision);
        assert!(!current.fields.contains_key("memoryId"));
    }

    #[test]
    fn prior_loader_routes_evidence_without_a_memory_id() {
        let item = evidence(0);
        assert_eq!(
            super::super::context_delta_item_snapshot_from_pack_ledger(&ledger(&item))
                .expect("CLI must accept typed evidence"),
            from_item(&item),
        );
    }

    #[test]
    fn current_response_includes_native_evidence_in_canonical_order() {
        use crate::pack::{ContextRequest, ContextResponse, TokenBudget, assemble_draft};
        let mut draft =
            assemble_draft("release checks", TokenBudget::new(128).unwrap(), Vec::new()).unwrap();
        let first = evidence(1);
        let mut second = evidence(0);
        second.rank = 2;
        draft.evidence_items = vec![first.clone(), second.clone()];
        draft.used_tokens = 16;
        draft.hash = Some("blake3:fixture".to_owned());
        let response = ContextResponse::new(
            ContextRequest::from_query("release checks").unwrap(),
            draft,
            Vec::new(),
        )
        .unwrap();
        let json = crate::output::render_context_response_json(&response);
        let snapshot = super::super::context_delta_snapshot_from_response(&response, &json);
        assert_eq!(snapshot.items, vec![from_item(&first), from_item(&second)]);
        assert_eq!(snapshot.pack_hash, "blake3:fixture");
        assert_eq!(snapshot.net_pack_tokens, 16);
        assert_eq!(snapshot.full_bytes, json.len() as u64);
    }

    #[test]
    fn an_unchanged_native_item_is_a_true_no_op() {
        let item = evidence(0);
        let prior = snapshot("old", vec![from_ledger(&ledger(&item)).unwrap()]);
        let new = snapshot("new", vec![from_item(&item)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None)).unwrap();
        assert!(delta.emits_delta());
        assert!(delta.data.items.added.is_empty());
        assert!(delta.data.items.removed.is_empty());
        assert!(delta.data.items.modified.is_empty());
        assert_eq!(delta.apply_to_snapshot(&prior).unwrap().items, new.items);
    }

    #[test]
    fn native_addition_revision_change_and_removal_round_trip() {
        let item = evidence(0);
        let empty = snapshot("empty", Vec::new());
        let first = snapshot("first", vec![from_item(&item)]);
        let mut changed = item.clone();
        changed.entity_revision = format!("blake3:{}", "b".repeat(64));
        changed.rank = 2;
        let second = snapshot("second", vec![from_item(&changed)]);
        for (prior, new) in [(&empty, &first), (&first, &second), (&second, &empty)] {
            let delta = compute_context_delta(prior, new, ContextDeltaOptions::new(None)).unwrap();
            assert!(delta.emits_delta());
            assert_eq!(delta.apply_to_snapshot(prior).unwrap().items, new.items);
        }
        let changed_delta =
            compute_context_delta(&first, &second, ContextDeltaOptions::new(None)).unwrap();
        assert_eq!(changed_delta.data.items.modified.len(), 1);
        assert!(
            changed_delta.data.items.modified[0]
                .field_changes
                .contains_key("entityRevision")
        );
    }

    #[test]
    fn native_projection_does_not_copy_transcript_text_or_provenance_notes() {
        let mut item = evidence(0);
        item.content = "private-transcript-canary /Users/alice/private/café.txt".to_owned();
        item.why = "private-explanation-canary".to_owned();
        let wire = serde_json::to_string(&from_item(&item)).unwrap();
        assert!(!wire.contains("private-transcript-canary"));
        assert!(!wire.contains("private-explanation-canary"));
        assert!(!wire.contains("/Users/alice"));
        assert!(!wire.contains("cass-session://"));
    }

    #[test]
    fn conflicting_or_wrongly_typed_identities_are_rejected_without_echoing_them() {
        let item = evidence(0);
        for field in ["entityId", "evidenceSpanId"] {
            let mut invalid = ledger(&item);
            invalid[field] = json!("private-identity-canary");
            let error = from_ledger(&invalid).unwrap_err();
            assert!(!error.contains("private-identity-canary"));
        }
        let mut wrong_type = ledger(&item);
        let memory_id = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(1)).to_string();
        wrong_type["entityId"] = json!(&memory_id);
        wrong_type["evidenceSpanId"] = json!(&memory_id);
        assert!(from_ledger(&wrong_type).is_err());
        for memory_id in [Value::Null, json!("mem_private-canary")] {
            let mut ambiguous = ledger(&item);
            ambiguous["memoryId"] = memory_id;
            assert!(from_ledger(&ambiguous).is_err());
        }
    }

    #[test]
    fn a_missing_or_malformed_revision_does_not_become_null_metadata() {
        let item = evidence(0);
        for revision in [
            Value::Null,
            json!(""),
            json!("blake3:short"),
            json!("private-revision-canary"),
            json!(format!("blake3:{}", "g".repeat(64))),
        ] {
            let mut invalid = ledger(&item);
            invalid["entityRevision"] = revision;
            let error = from_ledger(&invalid).unwrap_err();
            assert!(!error.contains("private-revision-canary"));
        }
        let mut missing = ledger(&item);
        missing.as_object_mut().unwrap().remove("entityRevision");
        assert!(from_ledger(&missing).is_err());
    }
}
