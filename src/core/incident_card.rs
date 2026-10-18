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

#[path = "incident_card_proof.rs"]
pub(crate) mod proof;

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

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

/// Borrowed lookup shared by every card derived from one source-row slice.
///
/// The first resolved arc with repair references builds the index. Repeated
/// cards then do logarithmic lookups instead of scanning the transcript for
/// each attempt, failure, proof and repair. No source text is copied or cached
/// across calls with different source slices. Callers still own source
/// admission, transcript interpretation and database snapshot consistency.
pub(crate) struct IncidentCardSources<'a> {
    spans: &'a [StoredEvidenceSpan],
    by_id: OnceCell<BTreeMap<&'a str, &'a StoredEvidenceSpan>>,
}

impl<'a> IncidentCardSources<'a> {
    pub(crate) fn new(spans: &'a [StoredEvidenceSpan]) -> Self {
        Self {
            spans,
            by_id: OnceCell::new(),
        }
    }

    pub(crate) fn draft(
        &self,
        workspace_id: &str,
        session_id: &str,
        arc: &FailureArc,
    ) -> Option<IncidentCardDraft> {
        if arc.resolution.as_ref()?.repair_ids.is_empty() {
            return None;
        }
        let by_id = self.by_id.get_or_init(|| {
            let mut by_id = BTreeMap::new();
            for span in self.spans {
                // Match the original slice::iter().find(): a later duplicate
                // must not replace the first observation under the same ID.
                by_id.entry(span.id.as_str()).or_insert(span);
            }
            by_id
        });
        draft_incident_card_with_lookup(workspace_id, session_id, arc, |id| {
            by_id.get(id).copied()
        })
    }
}

/// Render a single card without allocating a session index. Multi-card proof
/// evaluation uses IncidentCardSources; both paths share the unchanged renderer
/// and admission checks below. Tests retain this linear lookup as their oracle.
pub(crate) fn draft_incident_card(
    workspace_id: &str,
    session_id: &str,
    spans: &[StoredEvidenceSpan],
    arc: &FailureArc,
) -> Option<IncidentCardDraft> {
    draft_incident_card_with_lookup(workspace_id, session_id, arc, |id| {
        spans.iter().find(|span| span.id == id)
    })
}

/// Render one resolved arc using exact source references. The lookup strategy
/// cannot change facet selection, token budgets, identities or source metadata.
fn draft_incident_card_with_lookup<'a>(
    workspace_id: &str,
    session_id: &str,
    arc: &FailureArc,
    mut by_id: impl FnMut(&str) -> Option<&'a StoredEvidenceSpan>,
) -> Option<IncidentCardDraft> {
    let resolution = arc.resolution.as_ref()?;
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
    "wrapped",
    "handle",
    "handled",
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
    "converted",
    "converting",
    "switched",
    "deleted",
    "corrected",
    "adjusted",
    "reverted",
    "restored",
    "declared",
    "annotated",
    "injecting",
    "injected",
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

    // Only sentences that explain something: a fix verb or a term from the
    // symptom. Narration alone ("Let me look at that.") is not a fix.
    scored.retain(|(score, _)| *score >= 1);
    let mut chosen = Vec::new();
    let mut used = 0_u32;
    for (_, position) in &scored {
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

/// Whether a repair turn explains a fix at all, judged by the same sentence
/// scoring the card's Fix facet uses (anchors come from the failure's
/// symptom line when there is one).
pub(crate) fn explains_a_fix(text: &str, symptom: Option<&str>) -> bool {
    let anchors = symptom.map(anchor_terms).unwrap_or_default();
    split_sentences(text)
        .iter()
        .any(|sentence| sentence_score(sentence, &anchors) >= 1)
}

/// A short, readable name for a fingerprint key: `rustc:E0277` as is, and a
/// message-template key as its tool plus a short digest.
pub(crate) fn error_class_label(key: &str) -> String {
    match key.split_once(":tmpl:") {
        Some((tool, digest)) => {
            let hex = digest.strip_prefix("blake3:").unwrap_or(digest);
            format!("{tool} failure template {}", &hex[..hex.len().min(12)])
        }
        None => key.to_owned(),
    }
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
    use crate::core::cass_error_recall::{record_session_error_recall, session_failure_arcs};
    use crate::core::incident_card::proof::tests::{Fixture, TestResult, WS};

    fn source_index_fixture() -> TestResult<(Fixture, Vec<StoredEvidenceSpan>, FailureArc)> {
        let fixture = Fixture::new_public(true)?;
        let spans = fixture.db.list_evidence_spans_for_session(&fixture.session)?;
        let session = fixture.db.get_session(&fixture.session)?.ok_or("session")?;
        let arc = session_failure_arcs(WS, &session, &spans)
            .into_iter()
            .find(|arc| arc.resolution.is_some())
            .ok_or("resolved source arc")?;
        Ok((fixture, spans, arc))
    }

    // Compare every persisted field, not just readable text or a declared hash.
    fn draft_fields(draft: Option<IncidentCardDraft>) -> Option<serde_json::Value> {
        draft.map(|draft| {
            let input = draft.input;
            serde_json::json!({
                "id": draft.id,
                "workspace_id": input.workspace_id,
                "session_id": input.session_id,
                "memory_id": input.memory_id,
                "producer_kind": input.producer_kind.as_str(),
                "cass_span_id": input.cass_span_id,
                "span_kind": input.span_kind,
                "start_line": input.start_line,
                "end_line": input.end_line,
                "start_byte": input.start_byte,
                "end_byte": input.end_byte,
                "role": input.role,
                "excerpt": input.excerpt,
                "content_hash": input.content_hash,
                "metadata_json": input.metadata_json,
                "inherited_redaction_classes": input.inherited_redaction_classes,
            })
        })
    }

    #[test]
    fn source_index_matches_linear_card_on_real_migrated_evidence() -> TestResult {
        let (fixture, spans, arc) = source_index_fixture()?;
        let expected = draft_fields(draft_incident_card(WS, &fixture.session, &spans, &arc));
        assert!(expected.is_some(), "positive control must produce a card");
        let sources = IncidentCardSources::new(&spans);
        assert!(sources.by_id.get().is_none());
        assert_eq!(
            draft_fields(sources.draft(WS, &fixture.session, &arc)),
            expected,
        );
        assert_eq!(sources.by_id.get().ok_or("built index")?.len(), spans.len());
        Ok(())
    }

    #[test]
    fn source_index_preserves_first_duplicate_source_identity() -> TestResult {
        let (fixture, spans, arc) = source_index_fixture()?;
        let proof = spans
            .iter()
            .find(|span| span.id == fixture.proof)
            .ok_or("proof source")?;
        let mut changed = proof.clone();
        changed.end_line += 3;
        let mut first_original = spans.clone();
        first_original.push(changed.clone());
        let mut first_changed = vec![changed];
        first_changed.extend(spans);
        let mut rendered = Vec::new();
        for rows in [&first_original, &first_changed] {
            let sources = IncidentCardSources::new(rows);
            let actual = draft_fields(sources.draft(WS, &fixture.session, &arc));
            assert!(actual.is_some());
            assert_eq!(
                actual,
                draft_fields(draft_incident_card(WS, &fixture.session, rows, &arc)),
            );
            let first = rows
                .iter()
                .find(|span| span.id == fixture.proof)
                .ok_or("first proof")?;
            let indexed = sources
                .by_id
                .get()
                .and_then(|by_id| by_id.get(fixture.proof.as_str()))
                .copied()
                .ok_or("indexed proof")?;
            assert!(std::ptr::eq(first, indexed));
            rendered.push(actual);
        }
        assert_ne!(
            rendered[0], rendered[1],
            "source coordinates are load-bearing",
        );
        Ok(())
    }

    #[test]
    fn source_index_keeps_missing_reference_and_partial_repair_semantics() -> TestResult {
        let (fixture, spans, arc) = source_index_fixture()?;
        for missing in [
            &fixture.attempt,
            &fixture.failure,
            &fixture.proof,
            &fixture.repair,
        ] {
            let rows = spans
                .iter()
                .filter(|span| &span.id != missing)
                .cloned()
                .collect::<Vec<_>>();
            let sources = IncidentCardSources::new(&rows);
            let actual = draft_fields(sources.draft(WS, &fixture.session, &arc));
            assert_eq!(actual, None);
            assert_eq!(
                actual,
                draft_fields(draft_incident_card(WS, &fixture.session, &rows, &arc)),
            );
        }
        let mut partial = arc;
        partial
            .resolution
            .as_mut()
            .ok_or("resolution")?
            .repair_ids
            .push("ev_missing_optional_repair".to_owned());
        let sources = IncidentCardSources::new(&spans);
        let actual = draft_fields(sources.draft(WS, &fixture.session, &partial));
        assert!(
            actual.is_some(),
            "the surviving admitted explanation still renders",
        );
        assert_eq!(
            actual,
            draft_fields(draft_incident_card(WS, &fixture.session, &spans, &partial)),
        );
        Ok(())
    }

    #[test]
    fn source_index_is_lazy_for_unresolved_and_explanation_free_arcs() -> TestResult {
        let (fixture, spans, arc) = source_index_fixture()?;
        let sources = IncidentCardSources::new(&spans);
        let mut unresolved = arc.clone();
        unresolved.resolution = None;
        assert!(sources.draft(WS, &fixture.session, &unresolved).is_none());
        assert!(sources.by_id.get().is_none());
        let mut unexplained = arc;
        unexplained
            .resolution
            .as_mut()
            .ok_or("resolution")?
            .repair_ids
            .clear();
        assert!(sources.draft(WS, &fixture.session, &unexplained).is_none());
        assert!(sources.by_id.get().is_none());
        let empty = IncidentCardSources::new(&[]);
        assert!(empty.draft(WS, &fixture.session, &unexplained).is_none());
        assert!(empty.by_id.get().is_none());
        Ok(())
    }

    #[test]
    fn source_index_is_reused_across_many_cards_without_copying_rows() -> TestResult {
        let (fixture, mut spans, arc) = source_index_fixture()?;
        let template = spans.first().ok_or("source template")?.clone();
        for index in 0_u128..512 {
            let mut unrelated = template.clone();
            unrelated.id =
                crate::models::EvidenceId::from_uuid(uuid::Uuid::from_u128(0x100000 + index))
                    .to_string();
            spans.insert(0, unrelated);
        }
        let sources = IncidentCardSources::new(&spans);
        let expected = draft_fields(draft_incident_card(WS, &fixture.session, &spans, &arc));
        assert!(expected.is_some());
        assert_eq!(
            draft_fields(sources.draft(WS, &fixture.session, &arc)),
            expected,
        );
        let initial = sources.by_id.get().ok_or("built index")?;
        for _ in 0..64 {
            assert_eq!(
                draft_fields(sources.draft(WS, &fixture.session, &arc)),
                expected,
            );
            assert!(std::ptr::eq(
                initial,
                sources.by_id.get().ok_or("reused index")?,
            ));
        }
        for span in &spans {
            let indexed = initial
                .get(span.id.as_str())
                .copied()
                .ok_or("indexed source")?;
            assert!(std::ptr::eq(span, indexed));
        }
        Ok(())
    }

    #[test]
    fn source_index_preserves_unique_row_permutations_and_does_not_cross_snapshots() -> TestResult {
        let (fixture, spans, arc) = source_index_fixture()?;
        let expected = draft_fields(draft_incident_card(WS, &fixture.session, &spans, &arc));
        assert!(expected.is_some());
        for shift in 0..spans.len() {
            let mut reordered = spans.clone();
            reordered.rotate_left(shift);
            let sources = IncidentCardSources::new(&reordered);
            assert_eq!(
                draft_fields(sources.draft(WS, &fixture.session, &arc)),
                expected,
            );
        }
        let original = IncidentCardSources::new(&spans);
        assert_eq!(
            draft_fields(original.draft(WS, &fixture.session, &arc)),
            expected,
        );
        let mut changed = spans.clone();
        changed
            .iter_mut()
            .find(|span| span.id == fixture.proof)
            .ok_or("changed proof")?
            .end_line += 4;
        let updated = IncidentCardSources::new(&changed);
        let actual = draft_fields(updated.draft(WS, &fixture.session, &arc));
        assert!(actual.is_some());
        assert_ne!(actual, expected);
        assert_eq!(
            actual,
            draft_fields(draft_incident_card(WS, &fixture.session, &changed, &arc)),
        );
        assert_eq!(
            draft_fields(original.draft(WS, &fixture.session, &arc)),
            expected,
        );
        Ok(())
    }

    #[test]
    fn source_index_agrees_with_idempotent_import_and_current_source_proof() -> TestResult {
        let (fixture, spans, arc) = source_index_fixture()?;
        let expected = draft_incident_card(WS, &fixture.session, &spans, &arc).ok_or("card")?;
        let first = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(first.resolved_failures, 1);
        assert_eq!(first.incident_cards_recorded, 1);
        let stored = fixture
            .db
            .get_evidence_span(&expected.id)?
            .ok_or("stored card")?;
        assert!(stored.matches_screened_input(&expected.input)?);
        let second = record_session_error_recall(&fixture.db, WS, &fixture.session)?;
        assert_eq!(second.resolved_failures, 1);
        assert_eq!(second.incident_cards_recorded, 0);
        let proofs = proof::session_proofs(&fixture.db, WS, None, &fixture.session)?;
        assert!(proofs.claims.contains(&proof::Claim {
            failure: fixture.failure.clone(),
            target: expected.id,
            card: true,
        }));
        assert_eq!(
            fixture
                .db
                .list_evidence_spans_for_session(&fixture.session)?
                .len(),
            spans.len() + 1,
        );
        Ok(())
    }

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
        let first = fix_facet(std::slice::from_ref(&long), &anchors, 20).expect("fix");
        assert!(
            crate::pack::estimate_tokens_default(&first) <= 20,
            "{first}"
        );
        assert_eq!(fix_facet(&[long], &anchors, 20), Some(first));
        assert_eq!(fix_facet(&["ok".to_owned()], &anchors, 20), None);
    }

    #[test]
    fn narration_alone_never_explains_a_fix() {
        assert!(!explains_a_fix(
            "Let me look at that failure. Running the tests now.",
            None
        ));
        assert!(explains_a_fix(
            "Let me look. Ledger::flush_all was renamed, so I updated the call site.",
            None
        ));
        assert!(explains_a_fix(
            "The widget store serializes it.",
            Some("error[E0277]: the trait bound `Widget: Serialize` is not satisfied")
        ));
        assert_eq!(
            fix_facet(
                &["Let me look at that failure.".to_owned()],
                &BTreeSet::new(),
                60
            ),
            None
        );
        assert_eq!(error_class_label("rustc:E0277"), "rustc:E0277");
        assert_eq!(
            error_class_label("cargo:tmpl:blake3:68e0e38e3119313b826b1cc84e2bcfeba7"),
            "cargo failure template 68e0e38e3119"
        );
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
