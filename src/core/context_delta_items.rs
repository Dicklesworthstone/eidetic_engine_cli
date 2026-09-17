//! Lossless item reconstruction for the existing context-delta v2 protocol.
//!
//! V2 consumers remove IDs, update retained items, then append additions in
//! envelope order. There is no move operation or field-deletion sentinel. A
//! map-only diff therefore loses both ordering and the distinction between a
//! missing field and JSON null. Encode moves and field deletions as remove +
//! full re-add, retaining the longest new prefix that the old order can supply.
//! No schema extension, invented rank, or fabricated old field value is needed.

use std::collections::BTreeMap;

use super::{ContextDeltaError, ContextDeltaItemSnapshot, ContextDeltaItems, diff_item_fields};

#[path = "context_delta_apply.rs"]
mod apply;

#[path = "context_delta_wire.rs"]
mod wire;

type ItemIndex<'a> = BTreeMap<&'a str, (usize, &'a ContextDeltaItemSnapshot)>;

fn index_items<'a>(
    items: &'a [ContextDeltaItemSnapshot],
    side: &str,
) -> Result<ItemIndex<'a>, ContextDeltaError> {
    let mut index = BTreeMap::new();
    for (position, item) in items.iter().enumerate() {
        let problem = if item.id.trim().is_empty() {
            Some("blank item identity")
        } else if index.insert(item.id.as_str(), (position, item)).is_some() {
            Some("duplicate item identity")
        } else {
            None
        };
        if let Some(problem) = problem {
            // IDs and fields may contain private source data. Diagnose only
            // the input side and position, never interpolate the raw item.
            return Err(ContextDeltaError {
                message: format!(
                    "{side} context snapshot has a {problem} at position {position}; emit the full pack instead"
                ),
            });
        }
    }
    Ok(index)
}

pub(super) fn diff_items(
    prior_items: &[ContextDeltaItemSnapshot],
    new_items: &[ContextDeltaItemSnapshot],
) -> Result<ContextDeltaItems, ContextDeltaError> {
    let prior_by_id = index_items(prior_items, "prior")?;
    let new_by_id = index_items(new_items, "new")?;

    // A retained item cannot be placed after an appended one. Consequently
    // the retained items must be a prefix of the new sequence AND a subsequence
    // of the old sequence. Once either condition fails, resend the new suffix.
    // A field deletion also forces replacement: [old, null] means assignment
    // of JSON null in v2, not deletion of the key.
    let mut retained_len = 0;
    let mut last_prior_position = None;
    for new_item in new_items {
        let Some(&(position, prior_item)) = prior_by_id.get(new_item.id.as_str()) else {
            break;
        };
        if last_prior_position.is_some_and(|last| position <= last)
            || prior_item
                .fields
                .keys()
                .any(|field| !new_item.fields.contains_key(field))
        {
            break;
        }
        last_prior_position = Some(position);
        retained_len += 1;
    }

    let added = new_items[retained_len..].to_vec();
    let mut removed = Vec::new();
    for id in prior_by_id.keys() {
        if new_by_id
            .get(*id)
            .is_none_or(|(position, _)| *position >= retained_len)
        {
            removed.push((*id).to_owned());
        }
    }
    let mut modified = new_items[..retained_len]
        .iter()
        .filter_map(|item| {
            prior_by_id
                .get(item.id.as_str())
                .and_then(|(_, prior)| diff_item_fields(prior, item))
        })
        .collect::<Vec<_>>();
    // Modification order does not affect placement; retain the historical
    // stable ID ordering here. Additions MUST retain the new snapshot order.
    modified.sort_by(|left, right| left.id.cmp(&right.id));

    Ok(ContextDeltaItems {
        added,
        removed,
        modified,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{
        ContextDeltaFieldChange, ContextDeltaOptions, ContextDeltaPackSnapshot,
        compute_context_delta,
    };
    use super::*;
    use serde_json::json;

    fn item(id: &str) -> ContextDeltaItemSnapshot {
        ContextDeltaItemSnapshot::new(id)
            .with_field("content", json!(format!("content for {id}")))
            .with_field("provenance", json!({"source": id, "confidence": 0.8}))
    }

    fn items(ids: &[&str]) -> Vec<ContextDeltaItemSnapshot> {
        ids.iter().map(|id| item(id)).collect()
    }

    // Independent implementation of the documented remove/update/append
    // contract. In particular, a pair's new null assigns null; it does not
    // erase a key. Compare complete ordered items, not just ID membership.
    fn apply_documented(
        prior: &[ContextDeltaItemSnapshot],
        delta: &ContextDeltaItems,
    ) -> Vec<ContextDeltaItemSnapshot> {
        let mut result: Vec<_> = prior
            .iter()
            .filter(|item| !delta.removed.contains(&item.id))
            .cloned()
            .collect();
        for change in &delta.modified {
            let item = result
                .iter_mut()
                .find(|item| item.id == change.id)
                .expect("modified item must survive removals");
            for (field, change) in &change.field_changes {
                let value = match change {
                    ContextDeltaFieldChange::Pair([_, new]) => new.clone(),
                    ContextDeltaFieldChange::Redacted(redaction) => redaction.new_value.clone(),
                };
                item.fields.insert(field.clone(), value);
            }
        }
        for item in &delta.added {
            assert!(result.iter().all(|existing| existing.id != item.id));
            result.push(item.clone());
        }
        result
    }

    fn assert_roundtrip(
        prior: &[ContextDeltaItemSnapshot],
        new: &[ContextDeltaItemSnapshot],
    ) -> ContextDeltaItems {
        let delta = diff_items(prior, new).expect("valid snapshots");
        assert_eq!(apply_documented(prior, &delta), new);
        assert_eq!(
            delta
                .apply_to_items(prior)
                .expect("validated client application"),
            new
        );
        delta
    }

    #[test]
    fn additions_follow_the_new_pack_not_lexical_id_order() {
        let new = items(&["ev_z", "mem_b", "rule_a"]);
        let delta = assert_roundtrip(&[], &new);
        assert_eq!(delta.added, new);
        assert!(delta.removed.is_empty());
    }

    #[test]
    fn a_prepended_item_repositions_the_existing_suffix() {
        let prior = items(&["mem_a", "mem_b"]);
        let new = items(&["ev_z", "mem_a", "mem_b"]);
        let delta = assert_roundtrip(&prior, &new);
        assert_eq!(delta.added, new);
        assert_eq!(delta.removed, ["mem_a", "mem_b"]);
        assert!(delta.modified.is_empty());
    }

    #[test]
    fn rotation_retains_the_maximal_prefix_and_moves_only_the_tail() {
        let prior = items(&["mem_a", "mem_b", "mem_c"]);
        let new = items(&["mem_b", "mem_c", "mem_a"]);
        let delta = assert_roundtrip(&prior, &new);
        assert_eq!(delta.removed, ["mem_a"]);
        assert_eq!(delta.added, items(&["mem_a"]));
        assert!(delta.modified.is_empty());
    }

    #[test]
    fn a_middle_insertion_preserves_the_unchanged_prefix() {
        let prior = items(&["mem_a", "mem_b", "mem_c"]);
        let new = items(&["mem_a", "ev_z", "mem_b", "mem_c"]);
        let delta = assert_roundtrip(&prior, &new);
        assert_eq!(delta.removed, ["mem_b", "mem_c"]);
        assert_eq!(delta.added, new[1..]);
    }

    #[test]
    fn deletion_only_does_not_resend_surviving_items() {
        let prior = items(&["mem_a", "ev_z", "rule_b", "mem_c"]);
        let new = items(&["mem_a", "mem_c"]);
        let delta = assert_roundtrip(&prior, &new);
        assert_eq!(delta.removed, ["ev_z", "rule_b"]);
        assert!(delta.added.is_empty());
        assert!(delta.modified.is_empty());
    }

    #[test]
    fn append_and_field_updates_keep_the_compact_path() {
        let prior = items(&["mem_a", "rule_b"]);
        let mut new = items(&["mem_a", "rule_b", "ev_z"]);
        new[1].fields.insert("confidence".into(), json!(0.9));
        let delta = assert_roundtrip(&prior, &new);
        assert!(delta.removed.is_empty());
        assert_eq!(delta.added, items(&["ev_z"]));
        assert_eq!(delta.modified.len(), 1);
        assert_eq!(delta.modified[0].id, "rule_b");
    }

    #[test]
    fn field_deletion_replaces_the_item_without_reemitting_the_removed_value() {
        let mut prior = items(&["mem_a", "rule_b", "ev_z"]);
        prior[1]
            .fields
            .insert("oldPolicy".into(), json!("private-obsolete-value"));
        let new = items(&["mem_a", "rule_b", "ev_z"]);
        let delta = assert_roundtrip(&prior, &new);
        assert_eq!(delta.removed, ["ev_z", "rule_b"]);
        assert_eq!(delta.added, new[1..]);
        let wire = serde_json::to_string(&delta).expect("serialize item diff");
        assert!(!wire.contains("private-obsolete-value"));
        assert!(!wire.contains("oldPolicy"));
    }

    #[test]
    fn absent_and_explicit_null_fields_roundtrip_in_both_directions() {
        let absent = items(&["mem_a"]);
        let mut present = absent.clone();
        present[0].fields.insert("optional".into(), json!(null));
        let insert = assert_roundtrip(&absent, &present);
        assert!(insert.added.is_empty());
        assert_eq!(insert.modified.len(), 1);
        let remove = assert_roundtrip(&present, &absent);
        assert_eq!(remove.removed, ["mem_a"]);
        assert_eq!(remove.added, absent);
        assert!(remove.modified.is_empty());
    }

    #[test]
    fn duplicate_ids_fail_before_a_map_can_discard_evidence() {
        for different_body in [false, true] {
            let mut duplicate = item("private-duplicate-id");
            if different_body {
                duplicate
                    .fields
                    .insert("content".into(), json!("private-other-body"));
            }
            let invalid = [item("private-duplicate-id"), duplicate];
            for (prior, new) in [(&invalid[..], &[][..]), (&[][..], &invalid[..])] {
                let error = diff_items(prior, new).expect_err("ambiguous identities");
                let text = error.to_string();
                assert!(text.contains("duplicate item identity"));
                assert!(!text.contains("private-duplicate-id"));
                assert!(!text.contains("private-other-body"));
            }
        }
    }

    #[test]
    fn blank_ids_fail_on_either_snapshot() {
        for id in ["", " ", "\t\n"] {
            let invalid = [item(id)];
            assert!(diff_items(&invalid, &[]).is_err());
            assert!(diff_items(&[], &invalid).is_err());
        }
    }

    #[test]
    fn public_kernel_preserves_order_fields_hashes_and_input_immutability() {
        let prior = ContextDeltaPackSnapshot::new(
            "blake3:prior",
            10,
            1_000_000,
            100,
            items(&["mem_a", "ev_z"]),
        );
        let mut new = ContextDeltaPackSnapshot::new(
            "blake3:new",
            11,
            1_000_000,
            90,
            items(&["ev_z", "mem_a"]),
        );
        new.items[0].fields.remove("provenance");
        let prior_copy = prior.clone();
        let new_copy = new.clone();
        let envelope = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .expect("public delta computation");
        assert!(envelope.emits_delta());
        assert_eq!(
            apply_documented(&prior.items, &envelope.data.items),
            new.items
        );
        assert_eq!(envelope.data.prior_pack_hash, prior.pack_hash);
        assert_eq!(envelope.data.new_pack_hash, new.pack_hash);
        assert_eq!(envelope.data.token_savings.net_pack_tokens, 90);
        assert_eq!(prior, prior_copy);
        assert_eq!(new, new_copy);
        assert!(
            !envelope
                .data
                .server_decision
                .computed_from_server_verified_pack_record
        );
    }

    fn ordered_subsets(
        prefix: Vec<&'static str>,
        remaining: &[&'static str],
        out: &mut Vec<Vec<&'static str>>,
    ) {
        out.push(prefix.clone());
        for (index, id) in remaining.iter().enumerate() {
            let mut next = prefix.clone();
            next.push(*id);
            let rest: Vec<_> = remaining
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, id)| *id)
                .collect();
            ordered_subsets(next, &rest, out);
        }
    }

    #[test]
    fn all_ordered_subset_transitions_reconstruct_exactly_with_and_without_field_changes() {
        let mut sequences = Vec::new();
        ordered_subsets(
            Vec::new(),
            &["mem_a", "ev_z", "rule_b", "mem_c"],
            &mut sequences,
        );
        assert_eq!(sequences.len(), 65);
        let mut transitions = 0;
        for prior_ids in &sequences {
            for new_ids in &sequences {
                let prior = items(prior_ids);
                let new = items(new_ids);
                assert_roundtrip(&prior, &new);
                let mut changed = new.clone();
                for (index, item) in changed.iter_mut().enumerate() {
                    item.fields.insert("rank".into(), json!(index + 1));
                    if index % 2 == 0 {
                        item.fields.remove("provenance");
                    } else {
                        item.fields.insert("optional".into(), json!(null));
                    }
                }
                assert_roundtrip(&prior, &changed);
                transitions += 2;
            }
        }
        assert_eq!(transitions, 8_450);
    }
}
