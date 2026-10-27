//! Opt-in bead context at the shared, source-admitted pack ranking boundary.
//!
//! Tracker prose never becomes a prompt or search query. Only normalized
//! tokens feed the existing pure policy, and only admitted local memories
//! receive its bounded prior. Native rules, evidence and nonlocal memories
//! retain their existing scores.

use super::*;
use crate::models::bead_affinity::{
    BEAD_AFFINITY_BIAS_CAP, BEAD_AFFINITY_COLD_START_CODE, BEAD_AFFINITY_SCHEMA_V1,
    BEAD_AFFINITY_UNAVAILABLE_CODE, BeadAffinityBead, BeadAffinityComponentKind,
    BeadAffinityMemory, explain_bead_affinity, normalize_bead_label_tokens,
    normalize_bead_text_tokens,
};
use crate::models::bead_affinity_loader::load_bead_affinity_from_jsonl;

/// One bounded local export; unrelated tracker history never enters identity.
const MAX_BEAD_CONTEXT_BYTES: u64 = 64 * 1024 * 1024;
const BEAD_RANKING_POLICY: &str = "ee.context.bead_affinity.rank.v1";

pub(super) struct ResolvedBeadAffinity {
    bead_id: String,
    bead: Option<BeadAffinityBead>,
    pub(super) identity: String,
    degraded: Vec<ContextResponseDegradation>,
}

pub(super) fn validate_request(options: &ContextPackOptions) -> Result<(), ContextPackError> {
    let Some(bead_id) = options.bead_id.as_deref() else {
        return Ok(());
    };
    if bead_id.trim().is_empty() || bead_id.len() > 256 || bead_id.chars().any(char::is_control) {
        return Err(ContextPackError::Pack(
            "--bead-id must be a nonempty identifier of at most 256 bytes without control characters."
                .to_owned(),
        ));
    }
    // Candidate cursors are validated before core assembly and currently bind
    // only the query document. They cannot attest this mutable external input.
    if options.pagination.is_some() {
        return Err(ContextPackError::Pack(
            "--bead-id cannot be combined with candidate pagination; remove query-file pagination to build one bead-aware pack."
                .to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn resolve(
    options: &ContextPackOptions,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> Option<ResolvedBeadAffinity> {
    let bead_id = options.bead_id.as_deref()?.trim().to_owned();
    let path = options.workspace_path.join(".beads/issues.jsonl");
    let loaded = read_bead_contents(&path, MAX_BEAD_CONTEXT_BYTES)
        .map_err(|_| {
            notice(
                BEAD_AFFINITY_UNAVAILABLE_CODE,
                "The local bead export could not be read as a bounded regular file; bead affinity was skipped.",
            )
        })
        .and_then(|contents| {
            load_bead_affinity_from_jsonl(&contents, &bead_id).map_err(|error| {
                notice(
                    error.degraded_code(),
                    "The requested bead could not be resolved from the local export; bead affinity was skipped.",
                )
            })
        });
    let (bead, notices) = match loaded {
        Ok(bead) => (Some(bead), Vec::new()),
        Err(notice) => {
            degraded.push(notice.clone());
            (None, vec![notice])
        }
    };
    let identity = identity(&bead_id, bead.as_ref(), &notices);
    Some(ResolvedBeadAffinity {
        bead_id,
        bead,
        identity,
        degraded: notices,
    })
}

fn read_bead_contents(path: &Path, max_bytes: u64) -> io::Result<String> {
    if first_existing_context_path_symlink_component(path)
        .map_err(io::Error::other)?
        .is_some()
    {
        return Err(io::Error::other("bead export traverses a symbolic link"));
    }
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(io::Error::other("bead export is not a regular file"));
    }
    let file = open_context_file_for_read_no_follow(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(io::Error::other("bead export exceeds the bounded read"));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::other("bead export grew beyond the bounded read"));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn identity(
    bead_id: &str,
    bead: Option<&BeadAffinityBead>,
    degraded: &[ContextResponseDegradation],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_labeled_bytes(&mut hasher, "policy", BEAD_RANKING_POLICY.as_bytes());
    hash_labeled_bytes(&mut hasher, "bead_id", bead_id.as_bytes());
    hash_labeled_bytes(
        &mut hasher,
        "cap",
        &BEAD_AFFINITY_BIAS_CAP.to_bits().to_le_bytes(),
    );
    if let Some(bead) = bead {
        for (name, tokens) in [
            ("labels", &bead.label_tokens),
            ("title", &bead.title_tokens),
            ("description", &bead.description_tokens),
            ("family", &bead.family_memory_ids),
        ] {
            hash_labeled_count(&mut hasher, name, tokens.len());
            for token in tokens {
                hash_labeled_bytes(&mut hasher, name, token.as_bytes());
            }
        }
    } else {
        for entry in degraded {
            hash_labeled_bytes(&mut hasher, "unavailable", entry.code.as_bytes());
        }
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

pub(super) fn apply(
    context: &ResolvedBeadAffinity,
    connection: &DbConnection,
    candidates: &mut [PackCandidate],
    workspace_generation: Option<u64>,
    degraded: &mut Vec<ContextResponseDegradation>,
) -> serde_json::Value {
    let mut notices = context.degraded.clone();
    let mut top_biases = Vec::new();
    let mut matched_features = 0_usize;
    let mut applied_count = 0_usize;
    let mut magnitude = 0.0_f64;
    let mut observed = false;
    if let Some(bead) = &context.bead {
        let ids = candidates
            .iter()
            .filter(|candidate| candidate.origin.is_none())
            .map(|candidate| candidate.memory_id.to_string())
            .collect::<Vec<_>>();
        let ids = ids.iter().map(String::as_str).collect::<Vec<_>>();
        match connection.get_memory_tags_batch(&ids) {
            Ok(tags) => {
                observed = true;
                for candidate in candidates
                    .iter_mut()
                    .filter(|candidate| candidate.origin.is_none())
                {
                    let memory_id = candidate.memory_id.to_string();
                    let memory = BeadAffinityMemory {
                        memory_id: memory_id.clone(),
                        tag_tokens: tags
                            .get(&memory_id)
                            .into_iter()
                            .flatten()
                            .flat_map(|tag| normalize_bead_label_tokens(tag))
                            .collect(),
                        content_tokens: normalize_bead_text_tokens(&candidate.content),
                        // Family-link inference has no source-backed caller yet.
                        link_target_memory_ids: BTreeSet::new(),
                    };
                    let explanation = explain_bead_affinity(bead, &memory);
                    matched_features =
                        matched_features.saturating_add(explanation.components.len());
                    let base = candidate.relevance.into_inner();
                    let adjusted = bounded_bonus(base, explanation.weight);
                    let actual_bias = f64::from(adjusted) - f64::from(base);
                    if actual_bias <= 0.0 {
                        continue;
                    }
                    let Some(relevance) = unit_score(adjusted) else {
                        continue;
                    };
                    candidate.relevance = relevance;
                    let features = explanation
                        .components
                        .iter()
                        .map(|component| match component.kind {
                            BeadAffinityComponentKind::TagOverlap => "label",
                            BeadAffinityComponentKind::TitleToken => "titleToken",
                            BeadAffinityComponentKind::DescriptionToken => "descriptionToken",
                            BeadAffinityComponentKind::LinkPeer => "link",
                        })
                        .collect::<Vec<_>>();
                    candidate.why.push_str(&format!(
                        " Bead {} affinity added {:.6} to relevance ({}).",
                        context.bead_id,
                        actual_bias,
                        features.join(", "),
                    ));
                    applied_count = applied_count.saturating_add(1);
                    magnitude = magnitude.max(actual_bias);
                    top_biases.push(serde_json::json!({
                        "memoryId": memory_id,
                        "bias": actual_bias,
                        "features": features,
                    }));
                }
            }
            Err(_) => {
                let entry = notice(
                    BEAD_AFFINITY_UNAVAILABLE_CODE,
                    "Live memory tags could not be read; bead affinity was skipped without changing candidate scores.",
                );
                degraded.push(entry.clone());
                notices.push(entry);
            }
        }
    }
    let cold_start = observed && matched_features == 0;
    if cold_start {
        let entry = notice(
            BEAD_AFFINITY_COLD_START_CODE,
            "The bead's labels, title and description did not match admitted local memories; baseline ranking was retained.",
        );
        degraded.push(entry.clone());
        notices.push(entry);
    }
    top_biases.sort_by(|left, right| {
        right["bias"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&left["bias"].as_f64().unwrap_or(0.0))
            .then_with(|| left["memoryId"].as_str().cmp(&right["memoryId"].as_str()))
    });
    top_biases.truncate(8);
    serde_json::json!({
        "schema": BEAD_AFFINITY_SCHEMA_V1,
        "beadId": context.bead_id,
        "matchedFeatureCount": matched_features,
        "biasMagnitudeMax": magnitude,
        "maxBiasMagnitude": BEAD_AFFINITY_BIAS_CAP,
        "memoryBiasApplied": applied_count,
        "coldStart": cold_start,
        "determinismKey": {
            "workspaceGeneration": workspace_generation,
            "beadGeneration": null,
            "beadId": context.bead_id,
            "basePackHash": null,
            "beadContextHash": context.identity,
        },
        "topBiases": top_biases,
        "degraded": notices.iter().map(|entry| serde_json::json!({
            "code": entry.code,
            "severity": entry.severity.as_str(),
            "message": entry.message,
            "repair": entry.repair,
        })).collect::<Vec<_>>(),
    })
}

/// Round toward the baseline when f32 conversion would exceed the f64 cap.
/// The reported actual change and the stored selection score then agree.
fn bounded_bonus(base: f32, weight: f64) -> f32 {
    if !weight.is_finite() {
        return base;
    }
    let limit = (f64::from(base) + weight.clamp(0.0, BEAD_AFFINITY_BIAS_CAP)).min(1.0);
    let rounded = limit as f32;
    if f64::from(rounded) > limit {
        rounded.next_down().max(base)
    } else {
        rounded.max(base)
    }
}

fn notice(code: &str, message: &str) -> ContextResponseDegradation {
    ContextResponseDegradation {
        code: code.to_owned(),
        severity: ContextResponseSeverity::Low,
        message: message.to_owned(),
        repair: Some(
            "Check the requested ID and the local .beads/issues.jsonl export, or omit --bead-id."
                .to_owned(),
        ),
    }
}

#[cfg(test)]
#[path = "context_bead_affinity_tests.rs"]
mod tests;
