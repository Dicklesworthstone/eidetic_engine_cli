//! Positive capture evidence for independently learned exact global twins.
//!
//! A start equal to creation time is not proof that the author omitted it.
//! Only the canonical creation receipt records that intent. Promoted copies
//! retain the source start but have their own creation instant, so their
//! original destination receipt carries the source proof without rewriting
//! either row. Missing or conflicting evidence retains strict bound matching.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::{DbConnection, StoredMemory};
use crate::models::{MemoryId, WorkspaceId};

const CAPTURE_INPUT_SCHEMA: &str = "ee.memory.capture_validity_input.v1";
const DEFAULT_START_SCHEMA: &str = "ee.global_promotion.capture_default_start.v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DefaultStartProof {
    schema: String,
    workspace_id: String,
    memory_id: String,
    content_hash: String,
    created_at: String,
    valid_from: String,
    valid_to: Option<String>,
}

fn instant(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// Outer None means absent/malformed; Some(None) is a positively recorded null.
fn optional_instant(value: Option<&Value>) -> Option<Option<DateTime<Utc>>> {
    match value? {
        Value::Null => Some(None),
        Value::String(raw) => instant(raw).map(Some),
        _ => None,
    }
}

fn bound_instant(value: Option<&str>) -> Option<Option<DateTime<Utc>>> {
    match value {
        None => Some(None),
        Some(raw) => instant(raw).map(Some),
    }
}

fn canonical_ids(workspace: &str, memory: &str) -> bool {
    workspace
        .parse::<WorkspaceId>()
        .is_ok_and(|id| id.to_string() == workspace)
        && memory
            .parse::<MemoryId>()
            .is_ok_and(|id| id.to_string() == memory)
}

fn content_hash(content: &str) -> String {
    format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex())
}

impl DefaultStartProof {
    fn matches_content_and_bounds(&self, memory: &StoredMemory) -> bool {
        let Some(created) = instant(&self.created_at) else {
            return false;
        };
        let Some(start) = memory.valid_from.as_deref().and_then(instant) else {
            return false;
        };
        let proof_end = bound_instant(self.valid_to.as_deref());
        let current_end = bound_instant(memory.valid_to.as_deref());
        self.schema == DEFAULT_START_SCHEMA
            && canonical_ids(&self.workspace_id, &self.memory_id)
            && canonical_ids(&memory.workspace_id, &memory.id)
            && instant(&self.valid_from) == Some(created)
            && start == created
            && proof_end.is_some()
            && proof_end == current_end
            && self.content_hash == content_hash(&memory.content)
    }

    fn matches_capture(&self, memory: &StoredMemory) -> bool {
        self.memory_id == memory.id
            && self.workspace_id == memory.workspace_id
            && instant(&self.created_at) == instant(&memory.created_at)
            && self.matches_content_and_bounds(memory)
    }
}

fn receipts(
    db: &DbConnection,
    memory: &StoredMemory,
    action: &str,
) -> crate::db::Result<Vec<Option<Value>>> {
    use sqlmodel_core::Value as DbValue;

    let rows = db.query(
        "SELECT details FROM audit_log WHERE workspace_id = ?1 AND action = ?2 AND target_type = 'memory' AND target_id = ?3 ORDER BY id ASC",
        &[
            DbValue::Text(memory.workspace_id.clone()),
            DbValue::Text(action.to_owned()),
            DbValue::Text(memory.id.clone()),
        ],
    )?;
    Ok(rows
        .iter()
        .map(|row| {
            row.get(0)
                .and_then(DbValue::as_str)
                .and_then(|raw| serde_json::from_str(raw).ok())
        })
        .collect())
}

/// Called while the source body, lifecycle and sidecars share one snapshot.
pub(super) fn capture_default_start(
    db: &DbConnection,
    memory: &StoredMemory,
) -> crate::db::Result<Option<DefaultStartProof>> {
    let receipts = receipts(db, memory, "memory.create")?;
    let [Some(details)] = receipts.as_slice() else {
        return Ok(None);
    };
    let capture = &details["captureValidityInput"];
    if details["schema"] != "ee.audit.memory_create.v1"
        || details["command"] != "ee remember"
        || details["memoryId"] != memory.id
        || capture["schema"] != CAPTURE_INPUT_SCHEMA
        || !capture.get("validFromInput").is_some_and(Value::is_null)
    {
        return Ok(None);
    }
    let (Some(created), Some(from), Some(hash), Some(end)) = (
        capture.get("createdAt").and_then(Value::as_str),
        memory.valid_from.as_deref(),
        capture.get("contentHash").and_then(Value::as_str),
        optional_instant(capture.get("validToInput")),
    ) else {
        return Ok(None);
    };
    if Some(end) != bound_instant(memory.valid_to.as_deref()) {
        return Ok(None);
    }
    let proof = DefaultStartProof {
        schema: DEFAULT_START_SCHEMA.to_owned(),
        workspace_id: memory.workspace_id.clone(),
        memory_id: memory.id.clone(),
        content_hash: hash.to_owned(),
        created_at: created.to_owned(),
        valid_from: from.to_owned(),
        valid_to: memory.valid_to.clone(),
    };
    Ok(proof.matches_capture(memory).then_some(proof))
}

fn original_promotion_proves_default_start(
    db: &DbConnection,
    target: &StoredMemory,
) -> crate::db::Result<bool> {
    let receipts = receipts(db, target, "memory.promote_global")?;
    let mut original = None;
    for receipt in &receipts {
        let Some(details) = receipt else {
            return Ok(false);
        };
        let (Some(workspace), Some(memory)) = (
            details.get("originWorkspaceId").and_then(Value::as_str),
            details.get("originMemoryId").and_then(Value::as_str),
        ) else {
            return Ok(false);
        };
        if details["schema"] != super::GLOBAL_PROMOTION_REPORT_SCHEMA_V1
            || details["globalMemoryId"] != target.id
            || !canonical_ids(workspace, memory)
        {
            return Ok(false);
        }
        match details.get("alreadyPromoted").and_then(Value::as_bool) {
            Some(true) => {}
            Some(false) if original.is_none() => original = Some(details),
            _ => return Ok(false),
        }
    }
    let Some(details) = original else {
        return Ok(false);
    };
    let payload = &details["sourcePayload"];
    let raw = &payload["captureDefaultStart"];
    // Serde treats an absent Option field as None; authority requires the
    // original null end to have actually been recorded.
    if raw.get("validTo").is_none() {
        return Ok(false);
    }
    let Ok(proof) = serde_json::from_value::<DefaultStartProof>(raw.clone()) else {
        return Ok(false);
    };
    let destination_created = details
        .get("destinationCreatedAt")
        .and_then(Value::as_str)
        .and_then(instant);
    Ok(
        details["schema"] == super::GLOBAL_PROMOTION_REPORT_SCHEMA_V1
            && details["globalMemoryId"] == target.id
            && details["originWorkspaceId"] == proof.workspace_id
            && details["originMemoryId"] == proof.memory_id
            && payload["schema"] == "ee.global_promotion.source_payload.v1"
            && destination_created.is_some()
            && destination_created == instant(&target.created_at)
            && target.provenance_uri.as_deref()
                == Some(
                    super::promotion_provenance_uri(&proof.workspace_id, &proof.memory_id).as_str(),
                )
            && proof.matches_content_and_bounds(target),
    )
}

/// The caller has already checked current lifecycle, seals, trust, kind and
/// exact structured payload under its read snapshot or destination writer.
pub(super) fn default_starts_compatible(
    db: &DbConnection,
    source: &StoredMemory,
    proof: &DefaultStartProof,
    target: &StoredMemory,
) -> crate::db::Result<bool> {
    if source.content != target.content {
        return Ok(false);
    }
    independent_default_starts(db, source, proof, target)
}

/// Similarity may propose review across independently captured default starts;
/// it does not relax exact twin equality or admit differing authored bounds.
pub(super) fn independent_default_starts(
    db: &DbConnection,
    source: &StoredMemory,
    proof: &DefaultStartProof,
    target: &StoredMemory,
) -> crate::db::Result<bool> {
    if !proof.matches_capture(source) {
        return Ok(false);
    }
    if capture_default_start(db, target)?.is_some() {
        return Ok(true);
    }
    original_promotion_proves_default_start(db, target)
}

#[cfg(test)]
#[path = "global_promotion_validity_tests.rs"]
mod tests;
