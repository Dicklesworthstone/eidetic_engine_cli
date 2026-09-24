//! Snapshot-bound structured data and attempt evidence for global promotion.
//!
//! A prose duplicate is not a structured duplicate. Admission reads the typed
//! sidecar and revision-stable attempt ledger with the source body, not after
//! opening the destination. Publication keeps that payload in the transaction
//! containing the memory, index obligation and audit. Only sanitized evidence
//! summaries and commitments, never sidecar bodies or raw family IDs, enter
//! the promotion audit.

use std::str::FromStr;

use serde_json::{Value, json};

use super::PromotionRefusal;
use crate::db::{DbConnection, DbError, DbOperation, StoredMemory};
use crate::models::MemoryKind;
use crate::models::memory::{
    canonicalize_typed_memory_fields_json, canonicalize_typed_memory_fields_json_with_redactor,
};

#[derive(Default)]
pub(super) struct PromotionPayload {
    typed_fields: Option<String>,
    family_evidence: Option<Value>,
}

fn invalid_payload() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not verify structured global-promotion data".to_owned(),
    }
}

fn canonical_fields(kind: &str, raw: Option<&str>) -> crate::db::Result<Option<String>> {
    let kind = MemoryKind::from_str(kind).map_err(|_| invalid_payload())?;
    raw.map(|raw| {
        canonicalize_typed_memory_fields_json(&kind, raw).map_err(|_| invalid_payload())
    })
    .transpose()
}

impl PromotionPayload {
    /// The caller owns the source read snapshot. No nested transaction, source
    /// mutation, destination inspection or promotion of sibling bodies occurs.
    pub(super) fn capture(
        db: &DbConnection,
        memory: &StoredMemory,
    ) -> Result<(Self, Option<PromotionRefusal>), String> {
        let logical_id = db
            .get_memory_logical_id(&memory.id)
            .map_err(|_| "Could not verify promotion logical identity".to_owned())?
            .unwrap_or_else(|| memory.id.clone());
        let membership = db
            .get_attempt_family_membership_snapshot(&memory.workspace_id, &logical_id)
            .map_err(|_| "Could not verify promotion attempt-family evidence".to_owned())?;
        if !membership.is_promotion_eligible() {
            let posture = membership
                .promotion_posture()
                .ok_or_else(|| "Promotion attempt-family posture is missing".to_owned())?;
            return Ok((
                Self::default(),
                Some(PromotionRefusal::AttemptFamily { posture }),
            ));
        }
        let family_evidence = membership.families.first().map(|family| {
            let state = family.multiplicity();
            json!({
                "familyAlias": crate::models::public_attempt_family_alias(&family.family_id),
                "declaredSize": state.declared_size,
                "recordedSlots": state.recorded_slots,
                "selectedCount": state.selected_count,
                "rejectedCount": state.rejected_count,
                "promotionPosture": state.promotion_posture().as_str(),
            })
        });
        let raw = db
            .get_memory_typed_fields_json(&memory.id)
            .map_err(|_| "Could not read promotion typed fields".to_owned())?;
        let kind = MemoryKind::from_str(&memory.kind)
            .map_err(|_| "Invalid promotion memory kind".to_owned())?;
        let mut reasons = Vec::new();
        let typed_fields = raw
            .as_deref()
            .map(|raw| {
                // Inspect decoded field values, including each list element,
                // not serialized JSON where escaping can hide a multiline
                // secret. Refuse unsafe fields instead of changing values.
                canonicalize_typed_memory_fields_json_with_redactor(&kind, raw, |text| {
                    let redaction = crate::policy::redact_secret_like_content(text);
                    if redaction.redacted {
                        reasons.extend(redaction.redacted_reasons);
                    }
                    text.to_owned()
                })
            })
            .transpose()
            .map_err(|_| "Invalid promotion typed fields; source withheld".to_owned())?;
        if !reasons.is_empty() {
            reasons.sort_unstable();
            reasons.dedup();
            return Ok((
                Self::default(),
                Some(PromotionRefusal::RedactionRefused { reasons }),
            ));
        }
        Ok((
            Self {
                typed_fields,
                family_evidence,
            },
            None,
        ))
    }

    pub(super) fn matches_fields(&self, kind: &str, raw: Option<&str>) -> crate::db::Result<bool> {
        Ok(self.typed_fields == canonical_fields(kind, raw)?)
    }

    pub(super) fn apply(&self, db: &DbConnection, memory_id: &str) -> crate::db::Result<()> {
        if let Some(fields) = &self.typed_fields
            && !db.set_memory_typed_fields_json(memory_id, Some(fields))?
        {
            return Err(DbError::MalformedRow {
                operation: DbOperation::Execute,
                message: "Promoted memory did not retain its structured payload".to_owned(),
            });
        }
        Ok(())
    }

    pub(super) fn audit_evidence(&self) -> Value {
        json!({
            "schema": "ee.global_promotion.source_payload.v1",
            "typedFieldsHash": self.typed_fields.as_ref().map(|fields| {
                format!("blake3:{}", blake3::hash(fields.as_bytes()).to_hex())
            }),
            "attemptFamily": self.family_evidence,
        })
    }
}

#[cfg(test)]
#[path = "global_promotion_payload_tests.rs"]
mod tests;

#[cfg(test)]
mod field_policy_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

    #[test]
    fn decoded_multiline_and_list_fields_are_withheld_before_publication() {
        let root = tempfile::tempdir().unwrap();
        let db = DbConnection::open_file(&root.path().join("fields.db")).unwrap();
        db.migrate().unwrap();
        let workspace = "wsp_00000000000000000000000189";
        let id = "mem_00000000000000000000000189";
        db.insert_workspace(
            workspace,
            &CreateWorkspaceInput {
                path: root.path().display().to_string(),
                name: None,
            },
        )
        .unwrap();
        db.insert_memory(
            id,
            &CreateMemoryInput {
                workspace_id: workspace.to_owned(),
                level: "semantic".to_owned(),
                kind: "decision".to_owned(),
                content: "Use the reviewed design.".to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: None,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .unwrap();
        let key = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7\n-----END PRIVATE KEY-----";
        for fields in [json!({"rationale": key}), json!({"options": ["public", key]})] {
            db.set_memory_typed_fields_json(id, Some(&fields.to_string()))
                .unwrap();
            let memory = db.get_memory(id).unwrap().unwrap();
            let (payload, refusal) = PromotionPayload::capture(&db, &memory).unwrap();
            assert!(matches!(
                refusal,
                Some(PromotionRefusal::RedactionRefused { .. })
            ));
            assert!(payload.typed_fields.is_none());
            assert!(payload.audit_evidence()["typedFieldsHash"].is_null());
            assert_eq!(db.count_table_rows("search_index_jobs").unwrap(), 0);
        }
    }
}
