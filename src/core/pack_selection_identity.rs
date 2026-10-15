//! Canonical identity classification for integrity-verified pack selections.
//!
//! Pack ledgers are heterogeneous: durable memories retain `memoryId`, while
//! native evidence spans and procedural rules deliberately use their own typed
//! identity and must not fabricate a memory alias. Memory-only analytics need
//! to ignore those native selections without turning a malformed or ambiguous
//! row into a valid non-memory item.

use serde_json::Value as JsonValue;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackSelectionIdentity<'a> {
    Memory(&'a str),
    Native {
        kind: &'a str,
        entity_id: &'a str,
    },
}

/// Classify one selected item from an already integrity-verified pack ledger.
///
/// Legacy memory rows may omit `entityKind`; typed memory rows must retain a
/// matching `memoryId`. Native evidence/rule rows must not carry a memory id
/// and must expose their canonical typed identity. Unknown kinds, contradictory
/// aliases, empty IDs and malformed field types fail closed.
pub(crate) fn classify(item: &JsonValue) -> Result<PackSelectionIdentity<'_>, &'static str> {
    let kind = match item.get("entityKind") {
        None => None,
        Some(JsonValue::String(kind)) if !kind.is_empty() => Some(kind.as_str()),
        Some(_) => return Err("contained an invalid entityKind"),
    };
    let memory_id = match item.get("memoryId") {
        None => None,
        Some(JsonValue::String(memory_id)) if !memory_id.is_empty() => Some(memory_id.as_str()),
        Some(_) => return Err("contained an invalid memory id"),
    };

    match kind {
        None => memory_id
            .map(PackSelectionIdentity::Memory)
            .ok_or("omitted a memory id"),
        Some("memory") => {
            let memory_id = memory_id.ok_or("omitted a memory id")?;
            if let Some(entity_id) = item.get("entityId") {
                let Some(entity_id) = entity_id.as_str().filter(|value| !value.is_empty()) else {
                    return Err("contained an invalid memory entity id");
                };
                if entity_id != memory_id {
                    return Err("contained conflicting memory identities");
                }
            }
            Ok(PackSelectionIdentity::Memory(memory_id))
        }
        Some("evidence_span") => {
            if memory_id.is_some() {
                return Err("attached a memory id to native evidence");
            }
            let Some(entity_id) = item
                .get("entityId")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
            else {
                return Err("omitted the native evidence entity id");
            };
            if item.get("evidenceSpanId").and_then(JsonValue::as_str) != Some(entity_id) {
                return Err("contained conflicting native evidence identities");
            }
            Ok(PackSelectionIdentity::Native {
                kind: "evidence_span",
                entity_id,
            })
        }
        Some("rule") => {
            if memory_id.is_some() || item.get("evidenceSpanId").is_some() {
                return Err("contained conflicting native rule identities");
            }
            let Some(entity_id) = item
                .get("entityId")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
            else {
                return Err("omitted the native rule entity id");
            };
            Ok(PackSelectionIdentity::Native {
                kind: "rule",
                entity_id,
            })
        }
        Some(_) => Err("contained an unsupported entityKind"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_and_typed_memories_retain_memory_identity() {
        assert_eq!(
            classify(&json!({"memoryId":"mem_a"})),
            Ok(PackSelectionIdentity::Memory("mem_a"))
        );
        assert_eq!(
            classify(&json!({
                "entityKind":"memory", "entityId":"mem_b", "memoryId":"mem_b"
            })),
            Ok(PackSelectionIdentity::Memory("mem_b"))
        );
    }

    #[test]
    fn native_evidence_and_rules_are_valid_memoryless_selections() {
        assert_eq!(
            classify(&json!({
                "entityKind":"evidence_span",
                "entityId":"ev_a",
                "evidenceSpanId":"ev_a"
            })),
            Ok(PackSelectionIdentity::Native {
                kind: "evidence_span",
                entity_id: "ev_a",
            })
        );
        assert_eq!(
            classify(&json!({"entityKind":"rule", "entityId":"rul_a"})),
            Ok(PackSelectionIdentity::Native {
                kind: "rule",
                entity_id: "rul_a",
            })
        );
    }

    #[test]
    fn native_items_cannot_smuggle_or_fabricate_memory_identity() {
        for item in [
            json!({
                "entityKind":"evidence_span",
                "entityId":"ev_a",
                "evidenceSpanId":"ev_a",
                "memoryId":"mem_alias"
            }),
            json!({
                "entityKind":"rule",
                "entityId":"rul_a",
                "memoryId":"mem_alias"
            }),
            json!({
                "entityKind":"memory",
                "entityId":"mem_a",
                "memoryId":"mem_b"
            }),
        ] {
            assert!(classify(&item).is_err(), "{item}");
        }
    }

    #[test]
    fn malformed_or_unknown_typed_identity_fails_closed() {
        for item in [
            json!({}),
            json!({"memoryId":""}),
            json!({"entityKind":7, "memoryId":"mem_a"}),
            json!({"entityKind":"memory", "entityId":"mem_a"}),
            json!({"entityKind":"evidence_span", "entityId":"ev_a"}),
            json!({
                "entityKind":"evidence_span",
                "entityId":"ev_a",
                "evidenceSpanId":"ev_b"
            }),
            json!({"entityKind":"rule"}),
            json!({"entityKind":"future_kind", "entityId":"x"}),
        ] {
            assert!(classify(&item).is_err(), "{item}");
        }
    }
}
