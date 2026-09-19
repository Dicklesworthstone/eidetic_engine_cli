//! Scope-bound, atomic application of the public context-delta transport.
//!
//! A pack hash alone does not bind a client to a workspace or feature-flag set.
//! The expected identities come from the client's retained baseline context,
//! never from the arriving delta. Transport authentication remains external.

use super::super::super::ContextDeltaPackSnapshot;
use super::{ContextDeltaEnvelope, ContextDeltaError};

fn scope_error(reason: &'static str) -> ContextDeltaError {
    ContextDeltaError {
        message: format!("context delta cannot be applied: {reason}; request a fresh full pack"),
    }
}

impl ContextDeltaEnvelope {
    /// Reconstruct a baseline only within its retained workspace and flag set.
    ///
    /// Supply `workspace_id` and `feature_flag_set_hash` from the context saved
    /// with the baseline full pack, not from this envelope. Both declared flag
    /// hashes must match that saved identity. `None` means the baseline had no
    /// flag hash; it is not a wildcard accepting an arbitrary hash from a peer.
    ///
    /// The source generation may stay equal (a different task, query or token
    /// budget can produce another pack without a database write), but may not
    /// move backwards. The ordinary v2 baseline/hash/edit checks still apply.
    ///
    /// These checks prevent accidental cross-workspace, stale and policy-drift
    /// application; they do not authenticate a sender, recompute the full pack
    /// hash, or validate policy fields that are not present in the v2 envelope.
    /// The returned item projection never gains server-ledger verification.
    ///
    /// # Errors
    ///
    /// Rejects missing, blank or mismatched scope, changed feature-flag identity,
    /// generation rollback, or anything rejected by [`Self::apply_to_snapshot`].
    /// Diagnostics never include the supplied identities or evidence contents.
    pub fn apply_to_snapshot_scoped(
        &self,
        prior: &ContextDeltaPackSnapshot,
        workspace_id: &str,
        feature_flag_set_hash: Option<&str>,
    ) -> Result<ContextDeltaPackSnapshot, ContextDeltaError> {
        if workspace_id.trim().is_empty()
            || self.data.workspace_id.as_deref() != Some(workspace_id)
        {
            return Err(scope_error(
                "workspace does not match the retained baseline context",
            ));
        }
        if feature_flag_set_hash.is_some_and(|hash| hash.trim().is_empty())
            || self.data.prior_feature_flag_set_hash.as_deref() != feature_flag_set_hash
            || self.data.new_feature_flag_set_hash.as_deref() != feature_flag_set_hash
        {
            return Err(scope_error(
                "feature-flag identity does not match the retained baseline context",
            ));
        }
        if self
            .data
            .new_db_generation
            .is_some_and(|generation| generation < prior.db_generation)
        {
            return Err(scope_error("source generation would move backwards"));
        }
        self.apply_to_snapshot(prior)
    }
}

impl ContextDeltaPackSnapshot {
    /// Decode and atomically apply a scope-bound delta to this client baseline.
    ///
    /// The default input limit is four MiB. Parse, scope, generation and all edit
    /// preconditions are checked before replacing `self`. Any error leaves the
    /// original baseline intact, including its hash and verification posture.
    ///
    /// The returned envelope preserves the server's degradation signals and
    /// trace for the consumer; a successful reconstruction must not silently
    /// discard them. The resulting snapshot remains a local item projection,
    /// not an authenticated or hash-reverified canonical pack.
    ///
    /// # Errors
    ///
    /// Returns the bounded decoder or scoped application error without changing
    /// client state. See [`ContextDeltaEnvelope::apply_to_snapshot_scoped`] for
    /// the provenance required of the expected workspace and feature-flag hash.
    pub fn apply_json_delta_scoped(
        &mut self,
        bytes: &[u8],
        workspace_id: &str,
        feature_flag_set_hash: Option<&str>,
    ) -> Result<ContextDeltaEnvelope, ContextDeltaError> {
        self.apply_json_delta_scoped_with_limit(
            bytes,
            ContextDeltaEnvelope::DEFAULT_MAX_JSON_BYTES,
            workspace_id,
            feature_flag_set_hash,
        )
    }

    /// Apply a scope-bound wire delta with a caller-selected inclusive byte cap.
    ///
    /// The cap measures the actual transport bytes, including whitespace and
    /// line terminators, not the sender's `tokenSavings.deltaBytes` claim.
    /// All validation and reconstruction complete before the single assignment.
    ///
    /// # Errors
    ///
    /// Fails without advancing the baseline on any transport, scope, generation,
    /// fallback or item precondition error. Retain the old baseline or obtain a
    /// fresh full pack rather than trying to apply only part of the delta.
    pub fn apply_json_delta_scoped_with_limit(
        &mut self,
        bytes: &[u8],
        max_bytes: usize,
        workspace_id: &str,
        feature_flag_set_hash: Option<&str>,
    ) -> Result<ContextDeltaEnvelope, ContextDeltaError> {
        let delta = ContextDeltaEnvelope::from_json_slice_with_limit(bytes, max_bytes)?;
        let next = delta.apply_to_snapshot_scoped(self, workspace_id, feature_flag_set_hash)?;
        *self = next;
        Ok(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_delta::{
        ContextDeltaFallbackReason, ContextDeltaFieldChange, ContextDeltaItemSnapshot,
        ContextDeltaModifiedItem, ContextDeltaOptions, compute_context_delta,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    const WORKSPACE: &str = "wsp_client_binding";
    const FLAGS: &str = "flags:baseline";

    fn fixture() -> (
        ContextDeltaPackSnapshot,
        ContextDeltaPackSnapshot,
        ContextDeltaEnvelope,
    ) {
        let prior = ContextDeltaPackSnapshot::new(
            "blake3:baseline",
            7,
            1_000_000,
            100,
            vec![
                ContextDeltaItemSnapshot::new("mem_first").with_field("content", json!("alpha")),
                ContextDeltaItemSnapshot::new("evd_second")
                    .with_field("entityKind", json!("evidence_span"))
                    .with_field("content", json!("café\nquoted evidence")),
            ],
        );
        let mut next = prior.clone();
        next.pack_hash = "blake3:next".to_owned();
        next.db_generation = 8;
        next.items[0]
            .fields
            .insert("content".to_owned(), json!("updated alpha"));
        let mut delta = compute_context_delta(&prior, &next, ContextDeltaOptions::new(None))
            .expect("compute scoped fixture");
        delta.data.workspace_id = Some(WORKSPACE.to_owned());
        delta.data.prior_feature_flag_set_hash = Some(FLAGS.to_owned());
        delta.data.new_feature_flag_set_hash = Some(FLAGS.to_owned());
        (prior, next, delta)
    }

    fn bytes(delta: &ContextDeltaEnvelope) -> Vec<u8> {
        serde_json::to_vec(delta).expect("serialize scoped fixture")
    }

    #[test]
    fn scoped_wire_application_advances_exact_items_and_preserves_degradations() {
        let (mut prior, next, mut delta) = fixture();
        delta.append_response_degradation(
            "semantic_unavailable",
            "low",
            "Lexical retrieval only",
            None,
        );
        let received = prior
            .apply_json_delta_scoped(&bytes(&delta), WORKSPACE, Some(FLAGS))
            .expect("apply scoped transport");
        assert_eq!(prior, next);
        assert_eq!(received, delta);
        assert_eq!(received.degraded.len(), 1);
        assert_eq!(prior.items[1].id, "evd_second");
    }

    #[test]
    fn scoped_application_rejects_workspace_and_flag_rebinding_without_leaking_identities() {
        let (prior, _, delta) = fixture();
        for case in 0..8 {
            let mut rejected = delta.clone();
            let mut workspace = WORKSPACE;
            let mut flags = Some(FLAGS);
            match case {
                0 => rejected.data.workspace_id = None,
                1 => rejected.data.workspace_id = Some("private-other-workspace".to_owned()),
                2 => workspace = "private-other-workspace",
                3 => workspace = " ",
                4 => rejected.data.prior_feature_flag_set_hash = None,
                5 => {
                    rejected.data.prior_feature_flag_set_hash =
                        Some("private-other-flags".to_owned());
                    rejected.data.new_feature_flag_set_hash =
                        Some("private-other-flags".to_owned());
                }
                6 => flags = None,
                7 => flags = Some(" "),
                _ => unreachable!(),
            }
            let mut client = prior.clone();
            let error = client
                .apply_json_delta_scoped(&bytes(&rejected), workspace, flags)
                .expect_err("mismatched retained context");
            assert_eq!(client, prior, "case {case}");
            assert!(!error.to_string().contains("private-other"));
            assert!(!error.to_string().contains("quoted evidence"));
        }
    }

    #[test]
    fn scoped_receiver_rejects_rollback_stale_baselines_and_full_pack_fallback() {
        let (prior, _, delta) = fixture();
        for case in 0..8 {
            let mut rejected = delta.clone();
            match case {
                0 => rejected.data.new_db_generation = Some(6),
                1 => rejected.data.new_db_generation = None,
                2 => rejected.data.base_db_generation = Some(6),
                3 => rejected.data.base_db_generation = None,
                4 => rejected.data.prior_pack_hash = "private-other-pack".to_owned(),
                5 => rejected.data.new_feature_flag_set_hash = Some("other".to_owned()),
                6 => {
                    rejected.data.server_decision.fallback_reason =
                        Some(ContextDeltaFallbackReason::PriorCorrupted);
                }
                7 => rejected.data.server_decision.format = "markdown",
                _ => unreachable!(),
            }
            let mut client = prior.clone();
            assert!(
                client
                    .apply_json_delta_scoped(&bytes(&rejected), WORKSPACE, Some(FLAGS))
                    .is_err()
            );
            assert_eq!(client, prior, "case {case}");
        }
    }

    #[test]
    fn scoped_receiver_checks_all_edit_preconditions_before_advancing() {
        let (mut prior, _, mut delta) = fixture();
        let original = prior.clone();
        delta.data.items.modified.push(ContextDeltaModifiedItem {
            id: "evd_second".to_owned(),
            field_changes: BTreeMap::from([(
                "content".to_owned(),
                ContextDeltaFieldChange::Pair([json!("wrong old value"), json!("rejected value")]),
            )]),
        });
        assert!(
            prior
                .apply_json_delta_scoped(&bytes(&delta), WORKSPACE, Some(FLAGS))
                .is_err()
        );
        assert_eq!(
            prior, original,
            "the first valid edit must not have been applied"
        );
    }

    #[test]
    fn scoped_receiver_enforces_actual_bytes_with_an_inclusive_limit() {
        let (prior, next, mut delta) = fixture();
        delta.data.token_savings.delta_bytes = 0; // Sender's claim is not a limit.
        let mut transport = bytes(&delta);
        transport.extend_from_slice(b" \n");
        let mut client = prior.clone();
        assert!(
            client
                .apply_json_delta_scoped_with_limit(
                    &transport,
                    transport.len() - 1,
                    WORKSPACE,
                    Some(FLAGS),
                )
                .is_err()
        );
        assert_eq!(client, prior);
        client
            .apply_json_delta_scoped_with_limit(
                &transport,
                transport.len(),
                WORKSPACE,
                Some(FLAGS),
            )
            .expect("the exact inclusive limit accepts the entire JSON transport");
        assert_eq!(client, next);
    }

    #[test]
    fn scoped_receiver_rejects_bad_transport_without_changing_the_baseline() {
        let (prior, _, delta) = fixture();
        let mut trailing = bytes(&delta);
        trailing.extend_from_slice(b"{}");
        let duplicate = String::from_utf8(bytes(&delta))
            .expect("JSON UTF-8")
            .replacen("{", "{\"schema\":\"ee.context.delta.v2\",", 1)
            .into_bytes();
        for transport in [trailing, duplicate, b"{private malformed content".to_vec()] {
            let mut client = prior.clone();
            let error = client
                .apply_json_delta_scoped(&transport, WORKSPACE, Some(FLAGS))
                .expect_err("bad transport");
            assert_eq!(client, prior);
            assert!(!error.to_string().contains("private malformed content"));
        }
    }

    #[test]
    fn scoped_same_generation_and_explicitly_absent_flags_are_supported() {
        let (mut prior, mut next, mut delta) = fixture();
        next.db_generation = prior.db_generation;
        delta.data.new_db_generation = Some(prior.db_generation);
        delta.data.prior_feature_flag_set_hash = None;
        delta.data.new_feature_flag_set_hash = None;
        prior
            .apply_json_delta_scoped(&bytes(&delta), WORKSPACE, None)
            .expect("same source generation can produce a different context pack");
        assert_eq!(prior, next);
    }

    #[test]
    fn scoped_application_does_not_mint_new_ledger_verification() {
        let (prior, _, mut delta) = fixture();
        let mut client = prior.with_server_verified_pack_record();
        delta.data.server_decision.computed_from_server_verified_pack_record = true;
        let received = client
            .apply_json_delta_scoped(&bytes(&delta), WORKSPACE, Some(FLAGS))
            .expect("receive a claimed server-verified delta");
        assert!(
            received
                .data
                .server_decision
                .computed_from_server_verified_pack_record
        );
        assert!(!client.server_verified_pack_record);
    }
}
