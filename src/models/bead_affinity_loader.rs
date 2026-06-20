//! Pure-policy bead-record loader for `BeadAffinityBead` (bd-2942u,
//! swarmx.barp).
//!
//! Callers supply already-read `.beads/issues.jsonl` content as a
//! string; this module parses the JSONL, finds the requested bead-id,
//! and normalises the bead's `labels`, `title`, and `description`
//! through the existing token normalisers in
//! [`crate::models::bead_affinity`]. No file I/O happens here — the
//! file read sits in the CLI layer so the loader stays pure and
//! deterministic (same input string → byte-identical
//! [`BeadAffinityBead`]).
//!
//! Family-link seeding (parent/child bead memory-id population for the
//! `LinkPeer` component) is deliberately left to a follow-up slice; the
//! `family_memory_ids` set is empty after this slice. The cold-start
//! degraded code in the explainer surfaces the missing link path until
//! the family-link seeder lands.

use std::collections::BTreeSet;

use serde::Deserialize;

use crate::models::bead_affinity::{
    BEAD_AFFINITY_LOOKUP_FAILED_CODE, BEAD_AFFINITY_UNAVAILABLE_CODE, BeadAffinityBead,
    normalize_bead_label_tokens, normalize_bead_text_tokens,
};

/// Outcome when [`load_bead_affinity_from_jsonl`] cannot produce a
/// `BeadAffinityBead`. The CLI maps these into the documented degraded
/// codes:
/// - [`Self::BeadNotFound`] → `bead_affinity_lookup_failed`
/// - [`Self::MalformedLine`] → `bead_affinity_unavailable`
/// - [`Self::EmptyInput`] → `bead_affinity_unavailable`
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BeadAffinityLoadError {
    /// Input string contained no non-blank lines.
    EmptyInput,
    /// A non-blank line could not be parsed as a beads record.
    /// `line_number` is 1-indexed across the original input.
    MalformedLine { line_number: usize, message: String },
    /// Every line parsed but none matched `target_bead_id`.
    BeadNotFound { target_bead_id: String },
}

impl BeadAffinityLoadError {
    /// Stable degraded code for this load failure.
    #[must_use]
    pub const fn degraded_code(&self) -> &'static str {
        match self {
            Self::BeadNotFound { .. } => BEAD_AFFINITY_LOOKUP_FAILED_CODE,
            Self::EmptyInput | Self::MalformedLine { .. } => BEAD_AFFINITY_UNAVAILABLE_CODE,
        }
    }
}

#[derive(Debug, Deserialize)]
struct BeadRecord {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    labels: Vec<String>,
}

/// Parse a `.beads/issues.jsonl` string and build the bead-affinity
/// context for `target_bead_id`. Returns the normalised
/// [`BeadAffinityBead`] on success; see [`BeadAffinityLoadError`] for
/// the recoverable failure modes the caller maps to degraded codes.
///
/// Determinism: same `jsonl` string + same `target_bead_id` produces
/// byte-identical token sets across hosts (token normalisation is
/// ASCII-only and order-independent via `BTreeSet`).
pub fn load_bead_affinity_from_jsonl(
    jsonl: &str,
    target_bead_id: &str,
) -> Result<BeadAffinityBead, BeadAffinityLoadError> {
    let mut saw_any_line = false;
    let mut target_bead = None;
    for (idx, raw) in jsonl.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        saw_any_line = true;
        let record: BeadRecord =
            serde_json::from_str(line).map_err(|error| BeadAffinityLoadError::MalformedLine {
                line_number: idx + 1,
                message: error.to_string(),
            })?;
        if record.id == target_bead_id && target_bead.is_none() {
            target_bead = Some(build_bead(record));
        }
    }
    if let Some(bead) = target_bead {
        return Ok(bead);
    }
    if !saw_any_line {
        return Err(BeadAffinityLoadError::EmptyInput);
    }
    Err(BeadAffinityLoadError::BeadNotFound {
        target_bead_id: target_bead_id.to_owned(),
    })
}

fn build_bead(record: BeadRecord) -> BeadAffinityBead {
    let mut label_tokens: BTreeSet<String> = BTreeSet::new();
    for label in &record.labels {
        label_tokens.extend(normalize_bead_label_tokens(label));
    }
    BeadAffinityBead {
        bead_id: record.id,
        label_tokens,
        title_tokens: normalize_bead_text_tokens(&record.title),
        description_tokens: normalize_bead_text_tokens(&record.description),
        family_memory_ids: BTreeSet::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_JSONL: &str = concat!(
        r#"{"id":"bd-other","title":"Unrelated","description":"","labels":["foo"]}"#,
        "\n",
        r#"{"id":"bd-2942u","title":"swarmx.barp: bead-aware retrieval prioritization","description":"Bias ee context retrieval scoring with the active bead labels.","labels":["swarm-scale","retrieval","idea-wizard","implements-surface:query-file-tags"]}"#,
        "\n",
        r#"{"id":"bd-zzzz","title":"Another","description":"","labels":[]}"#,
        "\n",
    );

    #[test]
    fn loads_target_bead_and_normalises_tokens() {
        let bead = load_bead_affinity_from_jsonl(SAMPLE_JSONL, "bd-2942u").expect("load");
        assert_eq!(bead.bead_id, "bd-2942u");
        assert!(bead.label_tokens.contains("swarm"));
        assert!(bead.label_tokens.contains("scale"));
        assert!(bead.label_tokens.contains("retrieval"));
        assert!(bead.label_tokens.contains("idea"));
        assert!(bead.label_tokens.contains("wizard"));
        assert!(bead.label_tokens.contains("query"));
        assert!(bead.label_tokens.contains("file"));
        assert!(bead.label_tokens.contains("tags"));
        assert!(bead.title_tokens.contains("swarmx"));
        assert!(bead.title_tokens.contains("bead"));
        assert!(bead.title_tokens.contains("aware"));
        assert!(bead.title_tokens.contains("retrieval"));
        assert!(bead.description_tokens.contains("ee"));
        assert!(bead.description_tokens.contains("context"));
        assert!(bead.description_tokens.contains("scoring"));
        assert!(
            bead.family_memory_ids.is_empty(),
            "family seeding deferred to follow-up slice"
        );
    }

    #[test]
    fn missing_bead_returns_lookup_failed_shape() {
        let err = load_bead_affinity_from_jsonl(SAMPLE_JSONL, "bd-does-not-exist").unwrap_err();
        assert_eq!(
            err,
            BeadAffinityLoadError::BeadNotFound {
                target_bead_id: "bd-does-not-exist".to_owned(),
            }
        );
        assert_eq!(
            err.degraded_code(),
            "bead_affinity_lookup_failed",
            "missing bead should map to lookup_failed degraded code"
        );
    }

    #[test]
    fn empty_input_returns_empty_input_error() {
        let err = load_bead_affinity_from_jsonl("", "bd-2942u").unwrap_err();
        assert_eq!(err, BeadAffinityLoadError::EmptyInput);
        assert_eq!(err.degraded_code(), "bead_affinity_unavailable");
        let err = load_bead_affinity_from_jsonl("\n   \n\t\n", "bd-2942u").unwrap_err();
        assert_eq!(err, BeadAffinityLoadError::EmptyInput);
        assert_eq!(err.degraded_code(), "bead_affinity_unavailable");
    }

    #[test]
    fn malformed_line_reports_line_number() {
        let jsonl = "not-json\n";
        match load_bead_affinity_from_jsonl(jsonl, "bd-2942u").unwrap_err() {
            BeadAffinityLoadError::MalformedLine { line_number, .. } => {
                assert_eq!(line_number, 1)
            }
            other => panic!("expected MalformedLine, got {other:?}"),
        }
        let err = load_bead_affinity_from_jsonl(jsonl, "bd-2942u").unwrap_err();
        assert_eq!(err.degraded_code(), "bead_affinity_unavailable");
    }

    #[test]
    fn malformed_line_after_target_fails_closed() {
        let jsonl = concat!(
            r#"{"id":"bd-2942u","title":"Target","description":"","labels":[]}"#,
            "\n",
            "not-json\n",
        );
        match load_bead_affinity_from_jsonl(jsonl, "bd-2942u").unwrap_err() {
            BeadAffinityLoadError::MalformedLine { line_number, .. } => {
                assert_eq!(line_number, 2)
            }
            other => panic!("expected MalformedLine, got {other:?}"),
        }
    }

    #[test]
    fn blank_lines_between_records_are_ignored() {
        let jsonl = format!("\n   \n{SAMPLE_JSONL}\n\n");
        let bead = load_bead_affinity_from_jsonl(&jsonl, "bd-2942u").expect("load");
        assert_eq!(bead.bead_id, "bd-2942u");
    }

    #[test]
    fn output_is_deterministic_across_repeated_loads() {
        let first = load_bead_affinity_from_jsonl(SAMPLE_JSONL, "bd-2942u").expect("load");
        let second = load_bead_affinity_from_jsonl(SAMPLE_JSONL, "bd-2942u").expect("load");
        assert_eq!(first.bead_id, second.bead_id);
        assert_eq!(first.label_tokens, second.label_tokens);
        assert_eq!(first.title_tokens, second.title_tokens);
        assert_eq!(first.description_tokens, second.description_tokens);
        assert_eq!(first.family_memory_ids, second.family_memory_ids);
    }

    #[test]
    fn missing_optional_fields_yield_empty_token_sets() {
        let jsonl = r#"{"id":"bd-bare"}"#;
        let bead = load_bead_affinity_from_jsonl(jsonl, "bd-bare").expect("load");
        assert!(bead.label_tokens.is_empty());
        assert!(bead.title_tokens.is_empty());
        assert!(bead.description_tokens.is_empty());
    }
}
