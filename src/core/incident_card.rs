//! Derived incident cards (bd-reality-core-convergence-1azkt.59, ADR 0091).
//!
//! A single imported transcript turn is a fragment. An agent needs the
//! incident: what broke, the command that failed, what fixed it, and the run
//! that proved the fix. A card is a compact (at most
//! [`INCIDENT_CARD_TOKEN_BUDGET`] tokens) extractive summary of one resolved
//! failure->fix arc found by [`crate::core::cass_error_recall`], with fixed
//! facet labels and no generated prose.
//!
//! Governance:
//! - A card is not a memory. It is a derived evidence span of the session it
//!   summarizes (`span_kind = summary`, producer `cass_import`), covering the
//!   transcript lines from the failing command to its verifying run, so it is
//!   searched, admitted, packed, replayed and explained exactly like any other
//!   imported evidence, under the fixed `cass_evidence` trust class.
//! - Its identity is deterministic over the derivation version, workspace and
//!   failing span, so re-deriving a session never duplicates a card.
//! - Text comes only from sources the derivation policy allows: the fix facet
//!   from retrieval-admitted assistant turns; the symptom from a class-A tool
//!   result, secret-redacted and instruction-screened, falling back to the
//!   variable-masked error template; command facets are reduced to program
//!   and subcommand. A card whose text would not be admitted is not written.
//! - A card is linked as a helpful repair of its failure's fingerprint, with
//!   the failing span as `evidence_ref`, so error recall surfaces it and
//!   `ee why` can name its sources.

use std::collections::BTreeSet;

use crate::core::cass_error_recall::FailureArc;
use crate::db::{CreateEvidenceSpanInput, EvidenceProducerKind, StoredEvidenceSpan};

/// Version of the extractor. Part of every card's identity.
pub const INCIDENT_CARD_DERIVATION: &str = "incident_card.v1";
/// `created_by` of the repair links that point at cards.
pub const INCIDENT_CARD_ACTOR: &str = "ee import cass (incident_card.v1)";
/// Every card excerpt starts with this. A transcript record is a JSON line,
/// so no imported span can carry it as a plain-text `summary` with no role.
pub const INCIDENT_CARD_PREFIX: &str = "Incident card (derived by ee from lines ";
/// Upper bound on a card's estimated tokens.
pub const INCIDENT_CARD_TOKEN_BUDGET: u32 = 120;
/// A fix facet shorter than this is not worth a card.
const MIN_FIX_TOKENS: u32 = 12;
const SYMPTOM_MAX_CHARS: usize = 220;
const MIN_SENTENCE_CHARS: usize = 12;

/// Deterministic card identity over the derivation, workspace and failure.
#[must_use]
pub fn incident_card_id(workspace_id: &str, failure_span_id: &str) -> String {
    let digest = blake3::hash(
        format!("{INCIDENT_CARD_DERIVATION}\0{workspace_id}\0{failure_span_id}").as_bytes(),
    );
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    crate::models::EvidenceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

/// Whether a stored evidence row is a derived incident card rather than an
/// imported transcript line.
#[must_use]
pub fn is_incident_card_row(
    producer_kind: &str,
    span_kind: &str,
    role: Option<&str>,
    excerpt: &str,
) -> bool {
    producer_kind == EvidenceProducerKind::CassImport.as_str()
        && span_kind == "summary"
        && role.is_none()
        && excerpt.starts_with(INCIDENT_CARD_PREFIX)
}

/// A card ready to be written as evidence.
#[derive(Clone, Debug)]
pub(crate) struct IncidentCardDraft {
    pub id: String,
    pub input: CreateEvidenceSpanInput,
}

/// Build the card for one resolved arc, or `None` when the arc is unresolved,
/// has no admitted explanation of its fix, or its text would not be admitted.
pub(crate) fn draft_incident_card(
    workspace_id: &str,
    session_id: &str,
    spans: &[StoredEvidenceSpan],
    arc: &FailureArc,
) -> Option<IncidentCardDraft> {
    let resolution = arc.resolution.as_ref()?;
    let by_id = |id: &str| spans.iter().find(|span| span.id == id);
    let attempt = by_id(&arc.attempt_id)?;
    let failure = by_id(&arc.failure_id)?;
    let proof = by_id(&resolution.proof_id)?;
    // Newest-first in the arc; a reader wants them in transcript order.
    let repairs = resolution
        .repair_ids
        .iter()
        .rev()
        .filter_map(|id| by_id(id))
        .map(|span| span.reader_body().into_owned())
        .collect::<Vec<_>>();
    if repairs.is_empty() {
        return None;
    }

    let start_line = attempt.start_line.min(failure.start_line);
    let end_line = proof.end_line.max(failure.end_line);
    let symptom = symptom_facet(arc);
    let header = format!(
        "{INCIDENT_CARD_PREFIX}{start_line}-{end_line}): `{}` failed, then passed after a fix.",
        arc.family
    );
    let verified = format!(
        "Verified: `{}` succeeded afterwards.",
        resolution.proof_family
    );
    let symptom_line = format!("Symptom: {symptom}");
    let fixed_tokens = crate::pack::estimate_tokens_default(&format!(
        "{header}\n{symptom_line}\nFix: \n{verified}"
    ));
    let fix_budget = INCIDENT_CARD_TOKEN_BUDGET.checked_sub(fixed_tokens)?;
    if fix_budget < MIN_FIX_TOKENS {
        return None;
    }
    let fix = fix_facet(&repairs, &anchor_terms(&symptom), fix_budget)?;
    let excerpt = format!("{header}\n{symptom_line}\nFix: {fix}\n{verified}");
    if crate::pack::estimate_tokens_default(&excerpt) > INCIDENT_CARD_TOKEN_BUDGET {
        return None;
    }
    if !safe_to_show(&excerpt) {
        return None;
    }

    let id = incident_card_id(workspace_id, &arc.failure_id);
    let content_hash = format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex());
    let metadata = serde_json::json!({
        "derivation": INCIDENT_CARD_DERIVATION,
        "failureSpanId": arc.failure_id,
        "attemptSpanId": arc.attempt_id,
        "proofSpanId": resolution.proof_id,
        "repairSpanIds": resolution.repair_ids.iter().rev().collect::<Vec<_>>(),
    });
    Some(IncidentCardDraft {
        input: CreateEvidenceSpanInput {
            workspace_id: workspace_id.to_owned(),
            session_id: session_id.to_owned(),
            memory_id: None,
            producer_kind: EvidenceProducerKind::CassImport,
            cass_span_id: format!("ee-incident-card:{INCIDENT_CARD_DERIVATION}:{id}"),
            span_kind: "summary".to_owned(),
            start_line,
            end_line,
            start_byte: None,
            end_byte: None,
            role: None,
            excerpt,
            content_hash,
            metadata_json: Some(metadata.to_string()),
            inherited_redaction_classes: Vec::new(),
        },
        id,
    })
}

/// The failing output's first diagnostic line when it is safe to show, else
/// the variable-masked template of its error class, else the class alone.
fn symptom_facet(arc: &FailureArc) -> String {
    let class =
        arc.diagnostics
            .first()
            .map(|diagnostic| match diagnostic.canonical_code.as_deref() {
                Some(code) => format!("{} {code}", diagnostic.tool.as_str()),
                None => diagnostic.tool.as_str().to_owned(),
            });
    let template = arc.diagnostics.first().and_then(|diagnostic| {
        class
            .as_deref()
            .map(|class| format!("{class}: {}", diagnostic.message_template))
    });
    [arc.symptom.clone(), template, class]
        .into_iter()
        .flatten()
        .map(|candidate| bounded_chars(candidate.trim(), SYMPTOM_MAX_CHARS))
        .find(|candidate| safe_to_show(candidate))
        .unwrap_or_else(|| format!("`{}` reported a failure", arc.family))
}

/// Derived text may enter search and packs only when it carries no secret, no
/// redaction marker and no instruction-like content.
fn safe_to_show(text: &str) -> bool {
    if text.is_empty() || text.contains("[REDACTED") {
        return false;
    }
    let screen = crate::policy::screen_external_text_for_ingestion(text);
    !screen.redacted
        && !screen.instruction_like
        && matches!(screen.instruction_risk, "none" | "low")
        && screen.content == text
}

/// Terms from the symptom that a sentence about its fix is likely to repeat:
/// the error code, file names and backticked identifiers.
fn anchor_terms(symptom: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    for raw in symptom.split(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                '`' | '(' | ')' | '[' | ']' | ',' | ':' | '\'' | '"'
            )
    }) {
        let term = raw.trim_matches(|character: char| !character.is_alphanumeric());
        if term.len() < 3 {
            continue;
        }
        let looks_specific = term.contains('.')
            || term.contains('/')
            || term.contains('_')
            || term.chars().any(|character| character.is_ascii_digit())
            || term.chars().skip(1).any(char::is_uppercase)
            || term.chars().next().is_some_and(char::is_uppercase);
        if looks_specific {
            terms.insert(term.to_ascii_lowercase());
            if let Some(file) = term.rsplit('/').next().filter(|file| *file != term) {
                terms.insert(file.to_ascii_lowercase());
            }
        }
    }
    terms
}

const FIX_WORDS: &[&str] = &[
    "fix",
    "fixed",
    "fixes",
    "add",
    "added",
    "adding",
    "change",
    "changed",
    "changing",
    "rename",
    "renamed",
    "remove",
    "removed",
    "replace",
    "replaced",
    "update",
    "updated",
    "derive",
    "import",
    "imported",
    "use",
    "using",
    "move",
    "moved",
    "wrap",
    "handle",
    "implement",
    "implemented",
    "missing",
    "needs",
    "need",
    "because",
    "caused",
    "instead",
    "should",
    "must",
    "convert",
];

const NARRATION_PREFIXES: &[&str] = &[
    "let me",
    "i'll",
    "i will",
    "now ",
    "next,",
    "running",
    "let's run",
    "let's check",
    "ok",
    "okay",
    "great",
    "perfect",
    "done",
];

/// Extractive fix facet: the sentences of the repair turns that best explain
/// the fix, chosen greedily by score under the token budget and shown in
/// transcript order. Deterministic: ties break by position.
fn fix_facet(repairs: &[String], anchors: &BTreeSet<String>, budget: u32) -> Option<String> {
    let sentences = repairs
        .iter()
        .flat_map(|text| split_sentences(text))
        .collect::<Vec<_>>();
    let mut scored = sentences
        .iter()
        .enumerate()
        .map(|(position, sentence)| (sentence_score(sentence, anchors), position))
        .filter(|(score, _)| *score > i32::MIN)
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));

    let mut chosen = Vec::new();
    let mut used = 0_u32;
    for (score, position) in &scored {
        if *score < 1 && !chosen.is_empty() {
            break;
        }
        let tokens = crate::pack::estimate_tokens_default(&sentences[*position]).saturating_add(1);
        if used.saturating_add(tokens) > budget {
            continue;
        }
        used = used.saturating_add(tokens);
        chosen.push(*position);
    }
    if chosen.is_empty() {
        // The best sentence alone is over budget: keep its head.
        let (_, position) = scored.first()?;
        let budget_chars = usize::try_from(budget)
            .unwrap_or(usize::MAX)
            .saturating_mul(3);
        let head = bounded_chars(&sentences[*position], budget_chars);
        return (crate::pack::estimate_tokens_default(&head) <= budget).then_some(head);
    }
    chosen.sort_unstable();
    Some(
        chosen
            .into_iter()
            .map(|position| sentences[position].as_str())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn sentence_score(sentence: &str, anchors: &BTreeSet<String>) -> i32 {
    let lower = sentence.to_ascii_lowercase();
    // A sentence that lost a secret at ingest is not shown at all: a card
    // carries no redaction markers.
    if sentence.len() < MIN_SENTENCE_CHARS
        || sentence.split_whitespace().count() < 3
        || sentence.contains("[REDACTED")
    {
        return i32::MIN;
    }
    let words = lower
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .collect::<BTreeSet<_>>();
    let mut score = 0;
    if anchors.iter().any(|anchor| lower.contains(anchor.as_str())) {
        score += 3;
    }
    if FIX_WORDS.iter().any(|word| words.contains(word)) {
        score += 2;
    }
    if NARRATION_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        score -= 2;
    }
    score
}

/// Split projected turn text into sentences, dropping code fences, list
/// markers and blank lines.
fn split_sentences(text: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed.is_empty() {
            continue;
        }
        let trimmed = trimmed
            .trim_start_matches(['-', '*', '>', '#'])
            .trim_start();
        let trimmed = strip_numbered_marker(trimmed);
        let mut start = 0;
        let bytes = trimmed.as_bytes();
        for (index, byte) in bytes.iter().enumerate() {
            let ends_sentence = matches!(byte, b'.' | b'!' | b'?')
                && bytes
                    .get(index + 1)
                    .is_none_or(|next| next.is_ascii_whitespace());
            if ends_sentence {
                push_sentence(&mut output, &trimmed[start..=index]);
                start = index + 1;
            }
        }
        push_sentence(&mut output, &trimmed[start..]);
    }
    output
}

fn strip_numbered_marker(line: &str) -> &str {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && line[digits..].starts_with(". ") {
        &line[digits + 2..]
    } else {
        line
    }
}

fn push_sentence(output: &mut Vec<String>, sentence: &str) {
    let sentence = sentence.trim();
    if !sentence.is_empty() {
        output.push(sentence.to_owned());
    }
}

fn bounded_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut bounded = text
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    bounded.push_str("...");
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_facet_prefers_sentences_that_explain_the_fix_and_keeps_order() {
        let anchors = anchor_terms(
            "error[E0277]: the trait bound `Widget: Serialize` is not satisfied (src/widget.rs:41:9)",
        );
        let repairs = vec![
            "Let me look at the error. Widget needs to derive Serialize, because the store serializes it. I'll run the tests now.".to_owned(),
            "```rust\n#[derive(Serialize)]\n```\nAdded the derive in src/widget.rs.".to_owned(),
        ];
        let fix = fix_facet(&repairs, &anchors, 60).expect("fix");
        assert_eq!(
            fix,
            "Widget needs to derive Serialize, because the store serializes it. Added the derive in src/widget.rs."
        );
        assert!(!fix.contains("#[derive"), "code fences are not sentences");
    }

    #[test]
    fn fix_facet_respects_its_budget_and_is_deterministic() {
        let anchors = BTreeSet::new();
        let long = "Fixed the bound by adding a missing import of the serde derive macro to the widget module. ".repeat(6);
        let first = fix_facet(&[long.clone()], &anchors, 20).expect("fix");
        assert!(
            crate::pack::estimate_tokens_default(&first) <= 20,
            "{first}"
        );
        assert_eq!(fix_facet(&[long], &anchors, 20), Some(first));
        assert_eq!(fix_facet(&["ok".to_owned()], &anchors, 20), None);
    }

    #[test]
    fn card_rows_are_recognized_only_with_the_derived_shape() {
        let excerpt =
            format!("{INCIDENT_CARD_PREFIX}1-5): `cargo test` failed, then passed after a fix.");
        assert!(is_incident_card_row(
            "cass_import",
            "summary",
            None,
            &excerpt
        ));
        assert!(!is_incident_card_row(
            "cass_import",
            "message",
            None,
            &excerpt
        ));
        assert!(!is_incident_card_row(
            "cass_import",
            "summary",
            Some("assistant"),
            &excerpt
        ));
        assert!(!is_incident_card_row(
            "journal_distill",
            "summary",
            None,
            &excerpt
        ));
        assert!(!is_incident_card_row(
            "cass_import",
            "summary",
            None,
            r#"{"type":"summary","summary":"Incident card (derived by ee from lines 1-2)"}"#
        ));
        assert_eq!(
            incident_card_id("wsp_a", "ev_1"),
            incident_card_id("wsp_a", "ev_1")
        );
        assert_ne!(
            incident_card_id("wsp_a", "ev_1"),
            incident_card_id("wsp_b", "ev_1")
        );
        assert!(incident_card_id("wsp_a", "ev_1").starts_with("ev_"));
    }
}
