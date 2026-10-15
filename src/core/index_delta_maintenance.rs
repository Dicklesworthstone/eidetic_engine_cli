//! Inference-free bulk maintenance of an identity-validated V3 generation.
//!
//! Refreshing metadata or withdrawing evidence does not change a survivor's
//! vector. The ordinary mixed-mutation ceiling must not make a large archive
//! re-infer every survivor. This lane shares bulk intake's row/payload budgets,
//! but requires a stronger proof: every surviving ID and embedding input is
//! unchanged. A new ID or even one changed input retains the ordinary refusal.
//! No source, index, policy epoch or publication protocol is changed here.

use std::collections::{BTreeMap, BTreeSet};

use crate::search::IndexableDocument;

use super::{
    Delta, EmbeddingReuse, IncrementalFallback, IncrementalFallbackReason, append,
    document_digest, document_payload_bytes, embedding_input_digest, incremental_fallback,
    invalid_identity_map, over_limit,
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
        let Some(expected_document) = live.get(&document.id) else {
            // Equal text is not enough: a new identity needs a vector entry.
            return Ok(None);
        };
        let expected_input = inputs.get(&document.id).ok_or_else(invalid_identity_map)?;
        let input = embedding_input_digest(document);
        checkpoint()?;
        if *expected_input != input {
            return Ok(None);
        }
        // Check every input above, including a supposedly unchanged document;
        // inconsistent document/input maps must not confer reuse authority.
        let digest = document_digest(document);
        checkpoint()?;
        if *expected_document == digest {
            continue;
        }
        let bytes = document_payload_bytes(document);
        checkpoint()?;
        budget.admit(bytes)?;
        changed.push((document, expected_input));
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
        reusable.insert(document.id.clone(), input.clone());
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
        metadata_upserts = upserts.len(),
        withdrawals = removals.len(),
        "planned bounded index maintenance with unchanged embedding inputs"
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
