//! Client-side application of context-delta v2, without a server round trip.
//!
//! Validation precedes reconstruction. A failure never partially modifies the
//! caller's baseline, and diagnostics never echo item IDs or field contents.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as JsonValue;

use super::super::{
    CONTEXT_DELTA_SCHEMA_V2, ContextDeltaEnvelope, ContextDeltaError, ContextDeltaFieldChange,
    ContextDeltaItemSnapshot, ContextDeltaItems, ContextDeltaPackSnapshot,
};
use super::index_items;

fn apply_error(reason: &'static str) -> ContextDeltaError {
    ContextDeltaError {
        message: format!("context delta cannot be applied: {reason}; request a fresh full pack"),
    }
}

impl ContextDeltaItems {
    /// Reconstruct ordered item snapshots without mutating the baseline.
    ///
    /// Removals happen first, modifications affect only surviving items, and
    /// additions are appended in wire order. An ID in both `removed` and
    /// `added` is a complete replacement, not a field merge. New JSON null
    /// values remain explicit nulls; deleting a field requires replacement.
    ///
    /// Every ordinary pair checks its old value before applying the new one.
    /// V2 encodes both an absent old field and an old JSON null as null, so that
    /// one distinction cannot be used as a precondition. Redacted changes
    /// deliberately do not inspect the old value.
    ///
    /// This validates the item projection only. Callers must separately bind
    /// the envelope to their baseline hash, workspace, policy and transport.
    /// It neither recomputes the complete pack hash nor authenticates a sender.
    ///
    /// # Errors
    ///
    /// Rejects ambiguous identities/operations, absent mutation targets,
    /// mismatched old values and malformed redaction markers. All validation
    /// finishes before a replacement item vector is assembled.
    pub fn apply_to_items(
        &self,
        prior: &[ContextDeltaItemSnapshot],
    ) -> Result<Vec<ContextDeltaItemSnapshot>, ContextDeltaError> {
        let prior_by_id = index_items(prior, "prior")?;
        let added_by_id = index_items(&self.added, "added")?;
        let mut removed = BTreeSet::new();
        for id in &self.removed {
            if id.trim().is_empty() || !removed.insert(id.as_str()) {
                return Err(apply_error("blank or duplicate removal identity"));
            }
            if !prior_by_id.contains_key(id.as_str()) {
                return Err(apply_error("removal target is absent from the baseline"));
            }
        }
        for id in added_by_id.keys() {
            if prior_by_id.contains_key(*id) && !removed.contains(*id) {
                return Err(apply_error("addition would overwrite a retained item"));
            }
        }
        let mut modified_by_id = BTreeMap::new();
        for change in &self.modified {
            let id = change.id.as_str();
            if id.trim().is_empty() || modified_by_id.insert(id, change).is_some() {
                return Err(apply_error("blank or duplicate modification identity"));
            }
            if removed.contains(id) || added_by_id.contains_key(id) {
                return Err(apply_error("an item cannot be both modified and replaced"));
            }
            let Some(&(_, original)) = prior_by_id.get(id) else {
                return Err(apply_error("modification target is absent from the baseline"));
            };
            for (field, update) in &change.field_changes {
                match update {
                    ContextDeltaFieldChange::Pair([old, _]) => {
                        if original.fields.get(field).unwrap_or(&JsonValue::Null) != old {
                            return Err(apply_error("old field value does not match the baseline"));
                        }
                    }
                    ContextDeltaFieldChange::Redacted(redaction) => {
                        if !redaction.old_value_omitted {
                            return Err(apply_error("redacted change must omit its old value"));
                        }
                    }
                }
            }
        }

        let mut result = Vec::new();
        for original in prior {
            if removed.contains(original.id.as_str()) {
                continue;
            }
            let mut item = original.clone();
            if let Some(change) = modified_by_id.get(item.id.as_str()) {
                for (field, update) in &change.field_changes {
                    let new_value = match update {
                        ContextDeltaFieldChange::Pair([_, new]) => new,
                        ContextDeltaFieldChange::Redacted(redaction) => &redaction.new_value,
                    };
                    item.fields.insert(field.clone(), new_value.clone());
                }
            }
            result.push(item);
        }
        result.extend(self.added.iter().cloned());
        Ok(result)
    }
}

impl ContextDeltaEnvelope {
    /// Apply an in-process JSON delta to the exact named baseline snapshot.
    ///
    /// Checks the v2 envelope, prior hash and available generation metadata
    /// before applying any item operations. The returned snapshot is a local
    /// reconstruction, never a server-verified ledger record. Its hash is the
    /// envelope's claimed new pack hash: item snapshots are not the complete
    /// canonical pack, so this method cannot recompute that hash.
    ///
    /// Workspace and policy isolation remain the caller's responsibility:
    /// `ContextDeltaPackSnapshot` carries neither a workspace ID nor policy
    /// metadata. Do not accept an untrusted envelope merely because it claims
    /// `computedFromServerVerifiedPackRecord=true`.
    ///
    /// # Errors
    ///
    /// Rejects failed/fallback/non-JSON/chained envelopes, a wrong baseline,
    /// missing generations, declared feature-flag drift, or invalid item edits.
    pub fn apply_to_snapshot(
        &self,
        prior: &ContextDeltaPackSnapshot,
    ) -> Result<ContextDeltaPackSnapshot, ContextDeltaError> {
        if self.schema != CONTEXT_DELTA_SCHEMA_V2 || !self.success {
            return Err(apply_error("not a successful context-delta v2 envelope"));
        }
        let decision = &self.data.server_decision;
        if decision.fallback_reason.is_some() {
            return Err(apply_error("the server requested full-pack fallback"));
        }
        if decision.format != "json" || decision.delta_chained {
            return Err(apply_error("only unchained JSON deltas are machine-applicable"));
        }
        if prior.pack_hash.trim().is_empty()
            || self.data.new_pack_hash.trim().is_empty()
            || self.data.prior_pack_hash != prior.pack_hash
        {
            return Err(apply_error("delta does not name this baseline pack"));
        }
        let Some(base_generation) = self.data.base_db_generation else {
            return Err(apply_error("baseline generation is absent"));
        };
        if base_generation != prior.db_generation {
            return Err(apply_error("baseline generation does not match"));
        }
        let Some(new_generation) = self.data.new_db_generation else {
            return Err(apply_error("new generation is absent"));
        };
        if self.data.prior_feature_flag_set_hash != self.data.new_feature_flag_set_hash {
            return Err(apply_error("declared feature-flag sets differ"));
        }
        let items = self.data.items.apply_to_items(&prior.items)?;
        Ok(ContextDeltaPackSnapshot::new(
            self.data.new_pack_hash.clone(),
            new_generation,
            self.data.token_savings.full_bytes,
            self.data.token_savings.net_pack_tokens,
            items,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{
        ContextDeltaFallbackReason, ContextDeltaFieldChangeRedaction, ContextDeltaModifiedItem,
        ContextDeltaOptions, ContextDeltaRedactionReason, compute_context_delta,
    };
    use super::*;
    use serde_json::json;

    fn item(id: &str, content: &str) -> ContextDeltaItemSnapshot {
        ContextDeltaItemSnapshot::new(id).with_field("content", json!(content))
    }

    fn baseline() -> ContextDeltaPackSnapshot {
        ContextDeltaPackSnapshot::new(
            "blake3:prior",
            7,
            1_000_000,
            120,
            vec![item("a", "alpha"), item("b", "bravo")],
        )
    }

    fn modification(id: &str, old: JsonValue, new: JsonValue) -> ContextDeltaModifiedItem {
        ContextDeltaModifiedItem {
            id: id.to_owned(),
            field_changes: BTreeMap::from([(
                "content".to_owned(),
                ContextDeltaFieldChange::Pair([old, new]),
            )]),
        }
    }

    fn envelope() -> (ContextDeltaPackSnapshot, ContextDeltaEnvelope) {
        let prior = baseline();
        let new = ContextDeltaPackSnapshot::new(
            "blake3:new",
            8,
            1_000_000,
            110,
            vec![item("a", "updated alpha"), item("b", "bravo")],
        );
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .expect("compute fixture");
        (prior, delta)
    }

    #[test]
    fn public_application_reconstructs_exact_items_and_snapshot_metadata() {
        let prior = baseline();
        let mut new = ContextDeltaPackSnapshot::new(
            "blake3:new",
            8,
            1_000_000,
            99,
            vec![
                item("b", "café\n\"quoted\""),
                item("new", "new evidence"),
                item("a", "alpha"),
            ],
        );
        new.items[2].fields.clear();
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .expect("compute reorder/replacement fixture");
        let original = prior.clone();
        let actual = delta.apply_to_snapshot(&prior).expect("apply public delta");
        assert_eq!(actual, new);
        assert_eq!(prior, original);
    }

    #[test]
    fn reconstructing_a_verified_ledger_does_not_mint_new_verification_authority() {
        let prior = baseline().with_server_verified_pack_record();
        let mut new = baseline();
        new.pack_hash = "blake3:new".to_owned();
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .expect("verified baseline fixture");
        assert!(delta.data.server_decision.computed_from_server_verified_pack_record);
        let applied = delta.apply_to_snapshot(&prior).expect("local reconstruction");
        assert!(!applied.server_verified_pack_record);
        assert_eq!(applied, new);
    }

    #[test]
    fn stale_hash_and_generation_are_rejected_before_item_application() {
        let (prior, delta) = envelope();
        let mut wrong_hash = prior.clone();
        wrong_hash.pack_hash = "private-other-hash".to_owned();
        let error = delta.apply_to_snapshot(&wrong_hash).expect_err("wrong baseline");
        assert!(!error.to_string().contains("private-other-hash"));
        let mut wrong_generation = prior.clone();
        wrong_generation.db_generation += 1;
        assert!(delta.apply_to_snapshot(&wrong_generation).is_err());
    }

    #[test]
    fn missing_generation_metadata_is_not_invented() {
        let (prior, delta) = envelope();
        for baseline_missing in [true, false] {
            let mut malformed = delta.clone();
            if baseline_missing {
                malformed.data.base_db_generation = None;
            } else {
                malformed.data.new_db_generation = None;
            }
            assert!(malformed.apply_to_snapshot(&prior).is_err());
        }
    }

    #[test]
    fn rejects_non_applicable_envelopes() {
        let (prior, delta) = envelope();
        for case in 0..7 {
            let mut malformed = delta.clone();
            match case {
                0 => malformed.schema = "ee.response.v2",
                1 => malformed.success = false,
                2 => {
                    malformed.data.server_decision.fallback_reason =
                        Some(ContextDeltaFallbackReason::PriorCorrupted);
                }
                3 => malformed.data.server_decision.format = "markdown",
                4 => malformed.data.server_decision.delta_chained = true,
                5 => malformed.data.new_pack_hash = " ".to_owned(),
                6 => malformed.data.new_feature_flag_set_hash = Some("different".to_owned()),
                _ => unreachable!(),
            }
            assert!(malformed.apply_to_snapshot(&prior).is_err(), "case {case}");
        }
    }

    #[test]
    fn rejects_missing_duplicate_and_blank_removal_targets() {
        let prior = baseline();
        for removed in [vec!["missing"], vec!["a", "a"], vec![" "]] {
            let delta = ContextDeltaItems {
                removed: removed.into_iter().map(str::to_owned).collect(),
                ..ContextDeltaItems::default()
            };
            assert!(delta.apply_to_items(&prior.items).is_err());
        }
    }

    #[test]
    fn rejects_ambiguous_modification_targets() {
        let prior = baseline();
        let change = modification("a", json!("alpha"), json!("new"));
        for delta in [
            ContextDeltaItems {
                modified: vec![change.clone(), change.clone()],
                ..ContextDeltaItems::default()
            },
            ContextDeltaItems {
                removed: vec!["a".into()],
                modified: vec![change.clone()],
                ..ContextDeltaItems::default()
            },
            ContextDeltaItems {
                removed: vec!["a".into()],
                added: vec![item("a", "replacement")],
                modified: vec![change],
            },
            ContextDeltaItems {
                modified: vec![modification("missing", json!(null), json!("new"))],
                ..ContextDeltaItems::default()
            },
            ContextDeltaItems {
                modified: vec![modification(" ", json!(null), json!("new"))],
                ..ContextDeltaItems::default()
            },
        ] {
            assert!(delta.apply_to_items(&prior.items).is_err());
        }
    }

    #[test]
    fn additions_cannot_silently_overwrite_or_duplicate_an_identity() {
        let prior = baseline();
        for added in [
            vec![item("a", "not removed")],
            vec![item("new", "first"), item("new", "second")],
            vec![item("", "blank")],
        ] {
            let delta = ContextDeltaItems {
                added,
                ..ContextDeltaItems::default()
            };
            assert!(delta.apply_to_items(&prior.items).is_err());
        }
    }

    #[test]
    fn replacement_drops_old_fields_and_appends_in_wire_order() {
        let prior = baseline();
        let delta = ContextDeltaItems {
            removed: vec!["a".into()],
            added: vec![ContextDeltaItemSnapshot::new("a").with_field("newField", json!(null))],
            modified: Vec::new(),
        };
        let applied = delta.apply_to_items(&prior.items).expect("complete replacement");
        assert_eq!(applied[0], prior.items[1]);
        assert_eq!(applied[1], delta.added[0]);
        assert!(!applied[1].fields.contains_key("content"));
        assert_eq!(applied[1].fields.get("newField"), Some(&json!(null)));
    }

    #[test]
    fn ordinary_null_assignment_keeps_the_key() {
        let prior = baseline();
        let delta = ContextDeltaItems {
            modified: vec![modification("a", json!("alpha"), json!(null))],
            ..ContextDeltaItems::default()
        };
        let applied = delta.apply_to_items(&prior.items).expect("assign null");
        assert_eq!(applied[0].fields.get("content"), Some(&json!(null)));
    }

    #[test]
    fn late_precondition_failure_does_not_partially_modify_the_baseline() {
        let prior = baseline();
        let original = prior.clone();
        let delta = ContextDeltaItems {
            modified: vec![
                modification("a", json!("alpha"), json!("valid update")),
                modification("b", json!("private-wrong-old-value"), json!("bad update")),
            ],
            ..ContextDeltaItems::default()
        };
        let error = delta.apply_to_items(&prior.items).expect_err("stale second edit");
        assert_eq!(prior, original);
        assert!(!error.to_string().contains("private-wrong-old-value"));
        assert!(!error.to_string().contains("bravo"));
    }

    #[test]
    fn old_null_does_not_match_a_present_non_null_value() {
        let prior = baseline();
        let delta = ContextDeltaItems {
            modified: vec![modification("a", json!(null), json!("new"))],
            ..ContextDeltaItems::default()
        };
        assert!(delta.apply_to_items(&prior.items).is_err());
        let absent = [ContextDeltaItemSnapshot::new("a")];
        let applied = delta.apply_to_items(&absent).expect("old absent is encoded as null");
        assert_eq!(applied[0].fields["content"], json!("new"));
    }

    #[test]
    fn redacted_changes_replace_values_without_demanding_hidden_old_content() {
        let prior = [item("a", "private-old-secret")];
        let mut delta = ContextDeltaItems {
            modified: vec![ContextDeltaModifiedItem {
                id: "a".into(),
                field_changes: BTreeMap::from([(
                    "content".into(),
                    ContextDeltaFieldChange::redacted(
                        json!("[REDACTED]"),
                        ContextDeltaRedactionReason::PolicyRestricted,
                    ),
                )]),
            }],
            ..ContextDeltaItems::default()
        };
        let applied = delta.apply_to_items(&prior).expect("one-way redaction");
        assert_eq!(applied, [item("a", "[REDACTED]")]);
        assert!(!serde_json::to_string(&applied).unwrap().contains("private-old-secret"));
        delta.modified[0].field_changes.insert(
            "content".into(),
            ContextDeltaFieldChange::Redacted(ContextDeltaFieldChangeRedaction {
                new_value: json!("[REDACTED]"),
                old_value_omitted: false,
                reason: ContextDeltaRedactionReason::PolicyRestricted,
            }),
        );
        assert!(delta.apply_to_items(&prior).is_err());
        assert_eq!(prior[0].fields["content"], json!("private-old-secret"));
    }

    #[test]
    fn malformed_baseline_is_rejected_even_for_an_empty_delta() {
        let delta = ContextDeltaItems::default();
        assert!(delta.apply_to_items(&[item("same", "a"), item("same", "b")]).is_err());
        assert!(delta.apply_to_items(&[item(" ", "blank")]).is_err());
        assert_eq!(delta.apply_to_items(&[]).unwrap(), Vec::new());
    }

    #[test]
    fn item_diff_can_be_deserialized_and_applied_without_a_server_call() {
        let prior = baseline();
        let raw = r#"{"removed":["a"],"modified":[],"added":[{"id":"a","fields":{"content":"new"}}]}"#;
        let delta: ContextDeltaItems = serde_json::from_str(raw).expect("wire item diff");
        let applied = delta.apply_to_items(&prior.items).expect("local client application");
        assert_eq!(applied, [item("b", "bravo"), item("a", "new")]);
    }
}
