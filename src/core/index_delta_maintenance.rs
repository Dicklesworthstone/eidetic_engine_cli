//! Bounded rolling intake and maintenance of an identity-validated V3 generation.
//!
//! A shrinking or constant-size archive may ingest new evidence while older
//! evidence is withdrawn. Net growth is not a prerequisite for incremental
//! publication. Share bulk intake's row/payload budgets for those operations,
//! while keeping the ordinary inference allowance for new AND rewritten inputs.
//! Metadata-only survivors reuse their exact old vectors; an edited body never
//! gains reuse authority merely because its document ID survived.
//! Metadata refreshes and withdrawals alone remain inference-free. Source
//! admission, producer identity, staging and publication are unchanged here.

use std::collections::{BTreeMap, BTreeSet};

use crate::search::IndexableDocument;

use super::{
    Delta, EmbeddingReuse, IncrementalFallback, IncrementalFallbackReason, append, document_digest,
    document_payload_bytes, embedding_input_digest, incremental_fallback, invalid_identity_map,
    over_limit,
};

pub(super) fn try_plan(
    live: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, String>,
    documents: &[IndexableDocument],
    identity: String,
) -> Result<Option<Delta>, IncrementalFallback> {
    // Borrow the installed request. Never renew the budget to make a large
    // maintenance operation finish, or turn cancellation into a rebuild retry.
    let request = asupersync::Cx::current();
    plan_with_checkpoint(live, inputs, documents, identity, || {
        if let Some(cx) = &request {
            super::super::super::batch::checkpoint(cx, "bulk maintenance planning")?;
        }
        Ok(())
    })
}

fn plan_with_checkpoint(
    live: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, String>,
    documents: &[IndexableDocument],
    identity: String,
    mut checkpoint: impl FnMut() -> Result<(), IncrementalFallback>,
) -> Result<Option<Delta>, IncrementalFallback> {
    checkpoint()?;
    let Some(minimum_removals) = live.len().checked_sub(documents.len()) else {
        return Ok(None);
    };
    if minimum_removals > append::MAX_APPEND_DOCUMENTS {
        return Err(over_limit("bulk maintenance exceeds the document budget"));
    }
    if live.len() != inputs.len() {
        return Err(invalid_identity_map());
    }
    for (document_id, input_id) in live.keys().zip(inputs.keys()) {
        checkpoint()?;
        if document_id != input_id {
            return Err(invalid_identity_map());
        }
    }

    let mut budget = append::AppendBudget::default();
    // Do not turn the larger maintenance mutation allowance into a larger
    // inference allowance. New IDs and rewritten old inputs share the ordinary
    // percentage/absolute limit measured against the live corpus. Withdrawals
    // and metadata refreshes cannot buy more embedding work.
    let input_limit = super::vector_delta_limit(live.len(), documents.len());
    let mut inferred_inputs = 0;
    let mut new_identities = 0;
    let mut present = BTreeSet::new();
    let mut changed = Vec::new();
    for document in documents {
        checkpoint()?;
        if !present.insert(document.id.as_str()) {
            return Err(incremental_fallback(
                IncrementalFallbackReason::CorpusRevisionMismatch,
                "source snapshot contains duplicate document identities",
            ));
        }
        let reusable = if let Some(expected_document) = live.get(&document.id) {
            let expected_input = inputs.get(&document.id).ok_or_else(invalid_identity_map)?;
            let input = embedding_input_digest(document);
            checkpoint()?;
            // Check every surviving input, including a supposedly unchanged
            // document: inconsistent maps must not confer reuse authority.
            let digest = document_digest(document);
            checkpoint()?;
            if *expected_document == digest {
                if *expected_input != input {
                    return Ok(None);
                }
                continue;
            }
            // Only metadata changes may reuse an existing vector. A changed
            // embedding input is submitted to both configured tiers below.
            (*expected_input == input).then_some(expected_input)
        } else {
            // A new ID always needs a vector, even if its text equals a
            // survivor's or a withdrawn document's. No equality hash can
            // establish a live vector record for this previously absent ID.
            new_identities += 1;
            None
        };
        if reusable.is_none() {
            if inferred_inputs == input_limit {
                return Err(over_limit(
                    "rolling intake exceeds the new-input inference budget (including rewritten inputs)",
                ));
            }
            inferred_inputs += 1;
        }
        let bytes = document_payload_bytes(document);
        checkpoint()?;
        budget.admit(bytes)?;
        changed.push((document, reusable));
    }
    let mut withdrawn = Vec::new();
    for id in live.keys() {
        checkpoint()?;
        if !present.contains(id.as_str()) {
            budget.admit(id.len())?;
            withdrawn.push(id);
        }
    }
    checkpoint()?;

    // Borrow bodies and input commitments until ALL identities and budgets
    // pass. Upserts preserve source order; withdrawals use canonical ID order.
    // The full source/membership scan still scales with the corpus, not delta.
    let mut upserts = Vec::with_capacity(changed.len());
    let mut reusable = BTreeMap::new();
    for (document, input) in changed {
        checkpoint()?;
        if let Some(input) = input {
            reusable.insert(document.id.clone(), input.clone());
        }
        upserts.push(document.clone());
    }
    let mut removals = Vec::with_capacity(withdrawn.len());
    for id in withdrawn {
        checkpoint()?;
        removals.push(id.clone());
    }
    checkpoint()?;
    tracing::debug!(
        target: "ee::index",
        source_documents = documents.len(),
        metadata_upserts = upserts.len().saturating_sub(inferred_inputs),
        new_inputs = new_identities,
        rewritten_inputs = inferred_inputs.saturating_sub(new_identities),
        withdrawals = removals.len(),
        "planned bounded rolling intake with exact per-input vector reuse"
    );
    Ok(Some(Delta {
        upserts,
        removals,
        reuse: Some(EmbeddingReuse {
            identity,
            inputs: reusable,
        }),
    }))
}

#[cfg(test)]
#[path = "index_delta_maintenance_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "index_delta_rolling_tests.rs"]
mod rolling_tests;

#[cfg(test)]
#[path = "index_delta_mixed_maintenance_tests.rs"]
mod mixed_tests;
