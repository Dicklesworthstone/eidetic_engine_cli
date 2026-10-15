//! Cooperative planning before either ordinary or bulk index publication.
//!
//! Reusing vectors avoids inference, not the source walk. The ordinary pass
//! must observe cancellation too: every growing/maintenance fallback reaches
//! it first, and an unchanged corpus may never reach a fallback at all. Keep
//! original identity/digest/budget semantics and return no partial plan.

use std::collections::{BTreeMap, BTreeSet};

use crate::search::IndexableDocument;

use super::{
    Delta, EmbeddingReuse, IncrementalFallback, IncrementalFallbackReason, LexicalBudget,
    document_payload_bytes, embedding_input_digest, incremental_fallback, over_limit,
    vector_delta_limit,
};

pub(super) fn diff_with_digest(
    live: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, String>,
    documents: &[IndexableDocument],
    identity: String,
    digest: impl FnMut(&IndexableDocument) -> String,
) -> Result<Delta, IncrementalFallback> {
    let request = asupersync::Cx::current();
    diff_with_checkpoint(live, inputs, documents, identity, digest, || {
        if let Some(cx) = &request {
            super::super::batch::checkpoint(cx, "source delta planning")?;
        }
        Ok(())
    })
}

fn diff_with_checkpoint(
    live: &BTreeMap<String, String>,
    inputs: &BTreeMap<String, String>,
    documents: &[IndexableDocument],
    identity: String,
    mut digest: impl FnMut(&IndexableDocument) -> String,
    mut checkpoint: impl FnMut() -> Result<(), IncrementalFallback>,
) -> Result<Delta, IncrementalFallback> {
    checkpoint()?;
    let inference_limit = vector_delta_limit(live.len(), documents.len());
    let mut inference_inputs = 0;
    let mut lexical = LexicalBudget::default();
    let mut current = BTreeSet::new();
    let mut changes = Vec::new();
    for document in documents {
        checkpoint()?;
        if !current.insert(document.id.as_str()) {
            return Err(incremental_fallback(
                IncrementalFallbackReason::CorpusRevisionMismatch,
                "source snapshot contains duplicate document identities",
            ));
        }
        let actual_digest = digest(document);
        checkpoint()?;
        if live.get(&document.id) == Some(&actual_digest) {
            continue;
        }
        let bytes = document_payload_bytes(document);
        checkpoint()?;
        lexical.admit(bytes)?;
        let input = embedding_input_digest(document);
        checkpoint()?;
        let reusable = if live.contains_key(&document.id) && inputs.get(&document.id) == Some(&input) {
            Some(input)
        } else {
            if inference_inputs == inference_limit {
                return Err(over_limit("vector delta exceeds the inference budget"));
            }
            inference_inputs += 1;
            None
        };
        changes.push((document, reusable));
    }
    let mut withdrawn = Vec::new();
    for id in live.keys() {
        // Check ALL keys, not only removals selected by an iterator filter.
        // An unchanged million-row corpus is still a long membership walk.
        checkpoint()?;
        if !current.contains(id.as_str()) {
            // Removal capacity is not inference capacity. Charge complete IDs
            // before ownership, under the same budget as all changed upserts.
            lexical.admit(id.len())?;
            withdrawn.push(id);
        }
    }
    checkpoint()?;

    // Ownership transfer may clone the full allowed payload, so it also has
    // cooperative boundaries. A refused/cancelled plan owns no escaping delta.
    let mut upserts = Vec::with_capacity(changes.len());
    let mut reusable = BTreeMap::new();
    for (document, input) in changes {
        checkpoint()?;
        if let Some(input) = input {
            reusable.insert(document.id.clone(), input);
        }
        upserts.push(document.clone());
    }
    let mut removals = Vec::with_capacity(withdrawn.len());
    for id in withdrawn {
        checkpoint()?;
        removals.push(id.clone());
    }
    checkpoint()?;
    Ok(Delta {
        upserts,
        removals,
        reuse: Some(EmbeddingReuse {
            identity,
            inputs: reusable,
        }),
    })
}

#[cfg(test)]
#[path = "index_delta_planning_tests.rs"]
mod tests;
