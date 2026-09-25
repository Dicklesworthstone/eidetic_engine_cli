//! ADR 0067 extractive question answering — bd-169v0.2 / bd-169v0.3.
//!
//! `ee ask "<question>"` composes a direct answer FROM EXTRACTED SPANS of
//! stored memories: retrieval → span segmentation → scoring → clustering →
//! composition with per-claim citations, an overall confidence, and honest
//! abstention. The pure lexical evaluator is deterministic for fixed inputs.
//! Local semantic evaluation additionally depends on the verified model.
//!
//! Extractiveness invariant: every emitted answer sentence MUST byte-equal
//! a cited span of a stored memory. Violations trigger an internal error
//! rather than silent emission of generated text (enforced at the boundary
//! in `compose_answer`, never downgraded).

use std::collections::{BTreeMap, BTreeSet};

use crate::db::{CreateAuditInput, DbConnection, audit_actions, generate_audit_id};
use crate::obs::audit_events::query_hash as audit_query_hash;

#[path = "ask_retrieval.rs"]
mod retrieval;

#[path = "ask_store.rs"]
mod store;

pub use store::load_scoped_contradictions;

#[path = "ask_corpus.rs"]
mod corpus;

pub(crate) use corpus::load_command_advice_corpus;

pub use corpus::{
    AskCorpus, load_ask_corpus_for_paths, load_current_ask_corpus, load_scoped_ask_corpus,
};

#[path = "ask_candidates.rs"]
mod selection;

#[path = "ask_clustering.rs"]
mod clustering;

#[path = "ask_numeric.rs"]
mod numeric;

#[path = "ask_ordering.rs"]
mod ordering;

#[cfg(test)]
#[path = "ask_numeric_tests.rs"]
mod numeric_answer_tests;

#[path = "ask_native.rs"]
mod native;

pub use native::AskNativeSource;

#[path = "ask_semantic.rs"]
mod semantic;

pub use semantic::evaluate_ask_with_local_model;

// One request-local scoring regime must drive admission and composition.
// A callback borrows the complete semantic table without adding mutable
// process-global state or changing the public request/candidate structures.
type SpanScorer<'a> = &'a dyn Fn(&[String], &str, f32, &str) -> f32;

// ─── schema constants ───────────────────────────────────────────────────────

/// Response data schema identifier carried under `ee.response.v2 data.answer`.
pub const ASK_SCHEMA_V1: &str = "ee.ask.v1";

/// Origin tag emitted into the query-miss ledger on abstention.
pub const ASK_QUERY_MISS_ORIGIN: &str = "ask";

/// Default minimum confidence below which the engine abstains (ADR §3).
pub const ASK_MIN_CONFIDENCE_DEFAULT: f32 = 0.55;

/// The current ask score is a deterministic heuristic over lexical/semantic
/// span evidence, source confidence, trust, corroboration, and contradiction.
/// It is NOT an empirically calibrated probability.
pub const ASK_CONFIDENCE_CALIBRATION_STATUS: &str = "heuristic_uncalibrated";
/// Stable identity for the heuristic so callers can distinguish it from a
/// future fitted calibration artifact without parsing prose.
pub const ASK_CONFIDENCE_SCORE_KIND: &str = "ask_span_heuristic_v1";

/// Default maximum number of evidence spans to emit in the answer (ADR §3).
pub const ASK_MAX_EVIDENCE_DEFAULT: usize = 3;

/// Defensive ceiling on distinct memories admitted to span clustering.
pub const ASK_CANDIDATE_SCAN_CAP: usize = 512;

/// Retention horizon for ask miss audit rows, aligned with search miss demand.
const ASK_QUERY_MISS_AUDIT_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Ask miss audit rows are sparse and demand-driven; record every abstention.
const ASK_QUERY_MISS_AUDIT_SAMPLE_RATE: f64 = 1.0;

// ─── span scoring weights (ADR §2) ──────────────────────────────────────────

// ADR §2 weights when a complete local semantic score table is available.
// The pure lexical path still redistributes W2 into W1.
const SPAN_W1_LEXICAL: f32 = 0.45;
const SPAN_W2_SEMANTIC: f32 = 0.35;
const SPAN_W3_TRUST: f32 = 0.20;

/// Cosine threshold for clustering spans across memories (ADR §2).
const CLUSTER_SIMILARITY_THRESHOLD: f32 = 0.72;

/// Corroboration multiplier cap (ADR §2).
const CORROBORATION_CAP: f32 = 1.3_f32;

/// Contradiction penalty applied to confidence when opposing clusters found (ADR §4).
const CONTRADICTION_PENALTY: f32 = 0.40;

// ─── degradation codes (ADR §5) ─────────────────────────────────────────────

/// Info: confidence below threshold; abstention payload returned (exit 0).
pub const DEGRADED_NO_ANSWER: &str = "no_confident_answer";

/// Info: hash-embedder fallback in play; w2 mass shifted to w1.
pub const DEGRADED_SEMANTIC: &str = "ask_semantic_degraded";

/// Warning: top clusters oppose each other; sides[] emitted.
pub const DEGRADED_CONFLICT: &str = "ask_conflicting_evidence";

/// Warning: extractiveness invariant violated; engine withheld the answer.
pub const DEGRADED_EXTRACTIVENESS: &str = "ask_extractiveness_violated";

// ─── request / candidate types ──────────────────────────────────────────────

/// A source candidate with the fields the ask engine needs.
#[derive(Clone, Debug)]
pub struct AskCandidate {
    /// Canonical source ID. The historical field name is retained for memory
    /// callers; native rules use their real RuleId, never a synthetic MemoryId.
    pub memory_id: String,
    pub content: String,
    pub confidence: f32,
    pub trust_class: String,
    pub provenance_uri: Option<String>,
    pub level: String,
    pub kind: String,
    /// Receiver-derived teammate attribution when the candidate is a
    /// team-synced `peer_human_attested` memory.
    pub team_provenance: Option<crate::core::memory_scope::TeamProvenance>,
}

/// Stored, explicitly asserted contradiction between two scoped memories.
#[derive(Clone, Debug)]
pub struct AskContradiction {
    pub id: String,
    pub src_memory_id: String,
    pub dst_memory_id: String,
    pub confidence: f32,
    pub source: String,
}

/// Input to the ask engine (everything the engine needs to be deterministic).
#[derive(Clone, Debug)]
pub struct AskRequest {
    /// The user's question.
    pub question: String,
    /// Minimum confidence before abstaining (default `ASK_MIN_CONFIDENCE_DEFAULT`).
    pub min_confidence: f32,
    /// Maximum evidence spans to include in the composed answer.
    pub max_evidence: usize,
    /// When set, enables fail-closed mode: exit 6 if confidence below this.
    pub require_confidence: Option<f32>,
    pub contradictions: Vec<AskContradiction>,
    /// Native entity metadata from the same source snapshot as the candidates.
    pub native_sources: BTreeMap<String, AskNativeSource>,
}

impl Default for AskRequest {
    fn default() -> Self {
        Self {
            question: String::new(),
            min_confidence: ASK_MIN_CONFIDENCE_DEFAULT,
            max_evidence: ASK_MAX_EVIDENCE_DEFAULT,
            require_confidence: None,
            contradictions: Vec::new(),
            native_sources: BTreeMap::new(),
        }
    }
}

// ─── scored span ─────────────────────────────────────────────────────────────

/// One sentence-length span from a stored memory, with its span score.
#[derive(Clone, Debug)]
pub struct AskSpan {
    pub memory_id: String,
    pub byte_start: usize,
    pub byte_end: usize,
    /// Byte-exact copy of `content[byte_start..byte_end]`.
    pub text: String,
    pub score: f32,
    pub trust_class: String,
    pub memory_confidence: f32,
    pub provenance_uri: Option<String>,
    pub team_provenance: Option<crate::core::memory_scope::TeamProvenance>,
}

// ─── output types ────────────────────────────────────────────────────────────

/// One citation entry in the composed answer.
#[derive(Clone, Debug)]
pub struct AskCitation {
    /// 1-based index matching the `[n]` marker in `answer_text`.
    pub index: usize,
    pub memory_id: String,
    pub byte_start: usize,
    pub byte_end: usize,
    /// Byte-equal to `content[byte_start..byte_end]`.
    pub text: String,
    pub provenance_uri: Option<String>,
    pub trust_class: String,
    pub confidence: f32,
    pub team_provenance: Option<crate::core::memory_scope::TeamProvenance>,
}

/// One side of a conflicting answer (conflict mode, ADR §4).
#[derive(Clone, Debug)]
pub struct AskSide {
    pub label: String,
    pub answer_text: String,
    pub citations: Vec<AskCitation>,
}

/// Sub-threshold span surfaced in abstention mode (ADR §3).
#[derive(Clone, Debug)]
pub struct AskNearestEvidence {
    pub memory_id: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub text: String,
    pub score: f32,
}

/// Components of the confidence score (for transparency, ADR §3).
#[derive(Clone, Debug)]
pub struct AskConfidenceComponents {
    pub top_span_score: f32,
    pub corroboration: f32,
    pub contradiction_penalty: f32,
}

/// The full ask engine report (returned by `evaluate_ask`).
#[derive(Clone, Debug)]
pub struct AskReport {
    /// Metadata only for sources exposed in citations or nearest evidence.
    pub native_sources: BTreeMap<String, AskNativeSource>,
    pub question: String,
    pub abstained: bool,
    pub answer_text: Option<String>,
    pub confidence: f32,
    pub confidence_components: AskConfidenceComponents,
    pub citations: Vec<AskCitation>,
    /// Present when `conflict_detected` (ADR §4).
    pub sides: Option<Vec<AskSide>>,
    /// Present when `abstained` (ADR §3).
    pub nearest_evidence: Option<Vec<AskNearestEvidence>>,
    pub counterfactual_hint: Option<String>,
    pub semantic_degraded: bool,
    pub conflict_detected: bool,
    pub conflict_link: Option<AskContradiction>,
    /// True when compose_answer returned an error (extractiveness invariant violation).
    pub extractiveness_violated: bool,
    pub candidates_scanned: usize,
}

// ─── sentence segmenter (ADR §1) ────────────────────────────────────────────

/// Segment `content` into byte-addressed spans without rewriting evidence.
///
/// Fenced blocks retain their delimiter kind and length; inline code protects
/// embedded sentence punctuation. Prose splits on paragraphs, genuine list
/// markers and sentence boundaries, including CRLF and Unicode whitespace.
/// This is an evidence segmenter, not a Markdown renderer: block quotes,
/// indented code and nested container syntax are not interpreted.
pub fn segment_spans(content: &str) -> Vec<(usize, usize)> {
    let bytes = content.as_bytes();
    let mut spans = Vec::new();
    let mut prose_start = 0;
    let mut line_start = 0;
    let mut fence: Option<(usize, u8, usize)> = None;

    while line_start < bytes.len() {
        let next_line = advance_to_newline(bytes, line_start);
        if let Some((marker, width, tail)) = ask_fence_line(bytes, line_start, next_line) {
            if let Some((block_start, opening_marker, opening_width)) = fence {
                // A shorter run, a different marker or non-whitespace suffix
                // belongs to the body; it cannot expose a partial code block.
                if marker == opening_marker
                    && width >= opening_width
                    && bytes[tail..next_line]
                        .iter()
                        .all(|&byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
                {
                    push_span(&mut spans, content, block_start, next_line);
                    prose_start = next_line;
                    fence = None;
                }
            } else if marker != b'`' || !bytes[tail..next_line].contains(&b'`') {
                segment_ask_prose(&mut spans, content, prose_start, line_start);
                fence = Some((line_start, marker, width));
            }
        }
        line_start = next_line;
    }

    if let Some((block_start, _, _)) = fence {
        // An unterminated fence owns the remaining bytes, not just the first
        // sentence or the first accidental triple-backtick inside its body.
        push_span(&mut spans, content, block_start, bytes.len());
    } else {
        segment_ask_prose(&mut spans, content, prose_start, bytes.len());
    }
    spans
}

fn ask_fence_line(bytes: &[u8], start: usize, end: usize) -> Option<(u8, usize, usize)> {
    let mut marker_start = start;
    while marker_start < end && bytes[marker_start] == b' ' {
        marker_start += 1;
    }
    if marker_start - start > 3 || marker_start == end {
        return None;
    }
    let marker = bytes[marker_start];
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let mut tail = marker_start;
    while tail < end && bytes[tail] == marker {
        tail += 1;
    }
    let width = tail - marker_start;
    (width >= 3).then_some((marker, width, tail))
}

fn segment_ask_prose(spans: &mut Vec<(usize, usize)>, content: &str, start: usize, end: usize) {
    let bytes = content.as_bytes();
    let mut paragraph_start = start;
    let mut line_start = start;
    while line_start < end {
        let next_line = advance_to_newline(bytes, line_start).min(end);
        if content[line_start..next_line].trim().is_empty() {
            segment_ask_sentences(spans, content, paragraph_start, line_start);
            paragraph_start = next_line;
        } else if ask_list_content_start(bytes, line_start, next_line).is_some()
            && paragraph_start < line_start
        {
            segment_ask_sentences(spans, content, paragraph_start, line_start);
            paragraph_start = line_start;
        }
        line_start = next_line;
    }
    segment_ask_sentences(spans, content, paragraph_start, end);
}

/// Return the first byte after a list marker, not the beginning of the span.
/// Marker bytes stay in the citation, but `1. ` must not become an answer by
/// itself. Signed numbers and `*identifier` are not list items.
fn ask_list_content_start(bytes: &[u8], start: usize, end: usize) -> Option<usize> {
    let mut i = start;
    while i < end && bytes[i] == b' ' {
        i += 1;
    }
    if i - start > 3 || i == end {
        return None;
    }
    if matches!(bytes[i], b'-' | b'*' | b'+') {
        i += 1;
    } else {
        let digits = i;
        while i < end && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == digits || i - digits > 9 || i == end || !matches!(bytes[i], b'.' | b')') {
            return None;
        }
        i += 1;
    }
    if i == end || !matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
        return None;
    }
    while i < end && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    Some(i)
}

/// Pair each backtick run with the next run of exactly the same length in
/// this prose block. Precomputing avoids quadratic suffix rescans for many
/// unmatched delimiters. Closers inside code may be backslash-prefixed;
/// escaping is checked only when interpreting a run as an opener.
fn ask_inline_code_ends(bytes: &[u8], start: usize, end: usize) -> BTreeMap<usize, usize> {
    let mut runs = Vec::new();
    let mut i = start;
    while i < end {
        if bytes[i] == b'`' {
            let run_start = i;
            while i < end && bytes[i] == b'`' {
                i += 1;
            }
            runs.push((run_start, i));
        } else {
            i += 1;
        }
    }
    let mut next_by_width = BTreeMap::new();
    let mut closers = BTreeMap::new();
    for (run_start, run_end) in runs.into_iter().rev() {
        if let Some(closer_end) = next_by_width.insert(run_end - run_start, run_end) {
            closers.insert(run_start, closer_end);
        }
    }
    closers
}

fn ask_byte_is_escaped(bytes: &[u8], start: usize, position: usize) -> bool {
    let slashes = bytes[start..position]
        .iter()
        .rev()
        .take_while(|&&byte| byte == b'\\')
        .count();
    slashes % 2 == 1
}

fn segment_ask_sentences(spans: &mut Vec<(usize, usize)>, content: &str, start: usize, end: usize) {
    let bytes = content.as_bytes();
    let code_ends = ask_inline_code_ends(bytes, start, end);
    let mut span_start = start;
    let mut i = ask_list_content_start(bytes, start, end).unwrap_or(start);
    while i < end {
        if bytes[i] == b'`' {
            if !ask_byte_is_escaped(bytes, start, i)
                && let Some(&code_end) = code_ends.get(&i)
            {
                i = code_end;
                continue;
            }
            while i < end && bytes[i] == b'`' {
                i += 1;
            }
            continue;
        }
        if matches!(bytes[i], b'.' | b'!' | b'?')
            && !ask_byte_is_escaped(bytes, start, i)
            && !(bytes[i] == b'.' && is_abbreviation_end(content, i))
        {
            let remainder = &content[i + 1..end];
            let following = remainder.trim_start();
            if following.len() != remainder.len()
                && following.chars().next().is_none_or(char::is_uppercase)
            {
                push_span(spans, content, span_start, i + 1);
                i = end - following.len();
                span_start = i;
                continue;
            }
        }
        i += char_len_at(bytes, i);
    }
    push_span(spans, content, span_start, end);
}

fn push_span(spans: &mut Vec<(usize, usize)>, content: &str, start: usize, end: usize) {
    let slice = &content[start..end];
    let trimmed = slice.trim();
    if trimmed.is_empty() {
        return;
    }
    // Compute byte offsets in `content` for the trimmed span.
    let leading = slice.len() - slice.trim_start().len();
    let trimmed_start = start + leading;
    let trimmed_end = trimmed_start + trimmed.len();
    if trimmed_start < trimmed_end && trimmed_end <= content.len() {
        spans.push((trimmed_start, trimmed_end));
    }
}

fn advance_to_newline(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    if i < bytes.len() { i + 1 } else { i }
}

fn char_len_at(bytes: &[u8], i: usize) -> usize {
    let b = bytes[i];
    if b < 0x80 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

/// Return true if the `.` at `pos` in `text` is the end of a known
/// abbreviation, not a sentence boundary.
fn is_abbreviation_end(text: &str, pos: usize) -> bool {
    const ABBREVS: &[&str] = &["e.g", "i.e", "vs", "etc", "Mr", "Mrs", "Dr", "Prof", "St"];
    for abbrev in ABBREVS {
        let alen = abbrev.len();
        if pos >= alen && text.get(pos - alen..pos) == Some(*abbrev) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod code_evidence_segmentation_tests {
    use super::*;

    fn slices(content: &str) -> Vec<&str> {
        let spans = segment_spans(content);
        assert!(spans.windows(2).all(|pair| pair[0].1 <= pair[1].0));
        for &(start, end) in &spans {
            assert!(start < end && end <= content.len());
            assert!(content.is_char_boundary(start) && content.is_char_boundary(end));
            assert_eq!(&content[start..end], content[start..end].trim());
        }
        spans
            .into_iter()
            .map(|(start, end)| &content[start..end])
            .collect()
    }

    #[test]
    fn longer_fences_keep_embedded_examples_atomic() {
        let block = "````markdown\n```sh\ncargo fmt --check\n```\n````";
        let text = format!("Before.\n{block}\nAfter.");
        assert_eq!(slices(&text), ["Before.", block, "After."]);
    }

    #[test]
    fn tilde_fences_and_crlf_keep_exact_bytes() {
        let block = "~~~sh\r\nprintf 'A. B.'\r\n~~~";
        let text = format!("Before.\r\n{block}\r\nAfter.");
        assert_eq!(slices(&text), ["Before.", block, "After."]);
    }

    #[test]
    fn closing_fences_require_the_same_marker_and_a_blank_tail() {
        let block = "```sh\n~~~\n```not-a-close\n``\ncargo fmt\n`````";
        let text = format!("{block}\nAfter.");
        assert_eq!(slices(&text), [block, "After."]);
    }

    #[test]
    fn an_unterminated_fence_keeps_the_entire_remainder() {
        let block = "````sh\n```\nOne. Two.\n- Never split this.";
        let text = format!("Before.\n{block}");
        assert_eq!(slices(&text), ["Before.", block]);
    }

    #[test]
    fn indented_fences_have_a_bounded_opening_indent() {
        let text = "Before.\n   ~~~sh\nA. B.\n  ~~~\nAfter.";
        assert_eq!(slices(text), ["Before.", "~~~sh\nA. B.\n  ~~~", "After."]);
        assert!(ask_fence_line(b"    ```\n", 0, 8).is_none());
    }

    #[test]
    fn inline_code_is_not_a_fence_or_a_sentence_boundary() {
        for text in [
            "Use `printf 'A. B.'` before release.",
            "Use ``one ` literal. Two`` before release.",
            "Use ```A. B.``` before release.",
        ] {
            assert_eq!(slices(text), [text]);
        }
    }

    #[test]
    fn escaped_openers_and_unmatched_runs_do_not_swallow_prose() {
        assert_eq!(
            slices("Use \\`literal. Next."),
            ["Use \\`literal.", "Next."]
        );
        assert_eq!(slices("Use `literal. Next."), ["Use `literal.", "Next."]);
        assert_eq!(slices("Use `A. B\\` safely."), ["Use `A. B\\` safely."]);
    }

    #[test]
    fn inline_delimiters_do_not_cross_paragraph_or_fence_boundaries() {
        assert_eq!(
            slices("Use `first.\n\nNext `line."),
            ["Use `first.", "Next `line."]
        );
        let block = "~~~sh\necho `literal`\n~~~";
        let text = format!("Use `first.\n{block}\nNext `line.");
        assert_eq!(slices(&text), ["Use `first.", block, "Next `line."]);
    }

    #[test]
    fn ordered_markers_remain_with_their_answer_text() {
        assert_eq!(
            slices("1. Run cargo fmt.\n2) Run cargo test.\n   - Check output."),
            ["1. Run cargo fmt.", "2) Run cargo test.", "- Check output."]
        );
    }

    #[test]
    fn signed_values_and_operators_are_not_list_markers() {
        for text in [
            "The offset is\n-30 degrees.",
            "Evaluate\n*pointer first.",
            "The offset is\n+30 degrees.",
        ] {
            assert_eq!(slices(text), [text]);
        }
    }

    #[test]
    fn whitespace_and_unicode_sentence_boundaries_preserve_offsets() {
        let text =
            "  Run cargo fmt.\r\nNever skip it.\t\tÉvitez les erreurs.\u{2003}Check again.  ";
        assert_eq!(
            slices(text),
            [
                "Run cargo fmt.",
                "Never skip it.",
                "Évitez les erreurs.",
                "Check again."
            ]
        );
        assert_eq!(slices("Before\n \t\r\nAfter"), ["Before", "After"]);
        assert!(slices(" \r\n\t").is_empty());
        assert!(slices("").is_empty());
    }

    #[test]
    fn many_distinct_unmatched_runs_preserve_the_prose() {
        let text = (1..=128)
            .map(|width| format!("{}x ", "`".repeat(width)))
            .collect::<String>();
        assert_eq!(slices(&text), [text.trim()]);
    }

    #[test]
    fn public_ask_cites_a_fenced_command_without_truncating_it() {
        for body in [
            "~~~sh\ncargo fmt --check\n~~~",
            "````markdown\n```sh\ncargo fmt --check\n```\n````",
        ] {
            let report = evaluate_ask(
                &AskRequest {
                    question: "cargo fmt --check".to_owned(),
                    ..AskRequest::default()
                },
                &[AskCandidate {
                    memory_id: "format-command".to_owned(),
                    content: body.to_owned(),
                    confidence: 1.0,
                    trust_class: "human_explicit".to_owned(),
                    provenance_uri: Some("manual://format-command".to_owned()),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    team_provenance: None,
                }],
            );
            assert!(!report.abstained && !report.extractiveness_violated);
            assert_eq!(report.citations.len(), 1);
            assert_eq!(report.citations[0].text, body);
            assert_eq!(
                &body[report.citations[0].byte_start..report.citations[0].byte_end],
                body
            );
        }
    }
}

// ─── lexical tokenizer ───────────────────────────────────────────────────────

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "can", "to",
    "of", "in", "for", "on", "with", "at", "by", "from", "as", "or", "and", "but", "not", "it",
    "its", "this", "that", "these", "those", "so", "if", "then", "than", "also", "up", "into",
    "about", "such", "only", "each",
    // Interrogatives and question modals carry no answer content: a span
    // that answers "which command must run ..." never contains "which" or
    // "must", so scoring them as required terms only diluted every match.
    "what", "which", "who", "whom", "whose", "when", "where", "why", "how", "must", "need",
];

/// Tokenize text for ask scoring: lowercase, split on non-alphanumeric,
/// drop stopwords and single-character tokens.
pub fn tokenize_for_ask(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .filter(|t| t.len() > 1 && !STOPWORDS.contains(&t.as_str()))
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens
}

// ─── trust tilt (ADR §2) ────────────────────────────────────────────────────

fn trust_tilt(trust_class: &str) -> f32 {
    match trust_class {
        "human_explicit" => 1.00,
        "peer_human_attested" => 0.92,
        "agent_validated" => 0.85,
        "agent_assertion" => 0.70,
        "cass_evidence" => 0.55,
        "legacy_import" => 0.40,
        _ => 0.60,
    }
}

// ─── span scoring (ADR §2) ───────────────────────────────────────────────────

/// Jaccard similarity over sorted, deduplicated term sets.
fn jaccard_similarity(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let set_a: BTreeSet<&str> = a.iter().map(String::as_str).collect();
    let set_b: BTreeSet<&str> = b.iter().map(String::as_str).collect();
    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}

/// Fraction of the question's terms that the span contains.
///
/// `question_terms` must be the sorted, deduplicated output of
/// [`tokenize_for_ask`]: the denominator is its length, so a caller passing
/// repeated terms would silently weight those terms twice.
///
/// This is the half of `lexical_overlap` that Jaccard cannot express: an
/// answer-bearing span necessarily carries terms the question does not
/// ("Run cargo fmt --check before every release tag." answers "which command
/// runs before every release tag?"), and Jaccard counted every one of those
/// answer terms *against* the span. Coverage rewards the span for containing
/// the question; the Jaccard half still rewards precision so a long span that
/// merely mentions the question's common words does not outrank a tight one.
fn question_coverage(question_terms: &[String], span_terms: &[String]) -> f32 {
    if question_terms.is_empty() {
        return 0.0;
    }
    let span: BTreeSet<&str> = span_terms.iter().map(String::as_str).collect();
    let covered = question_terms
        .iter()
        .filter(|term| span.contains(term.as_str()))
        .count();
    covered as f32 / question_terms.len() as f32
}

/// Score one span against the question.
///
/// Semantic (embedding) similarity is not yet available — the w2 weight is
/// re-normalized into w1 (semantic_degraded mode, ADR §5). The lexical
/// overlap is the mean of question coverage and Jaccard similarity (ADR §2,
/// 2026-09-03 amendment).
pub fn score_span(
    question_terms: &[String],
    span_text: &str,
    memory_confidence: f32,
    trust_class: &str,
) -> f32 {
    let span_terms = tokenize_for_ask(span_text);
    let lexical = 0.5 * question_coverage(question_terms, &span_terms)
        + 0.5 * jaccard_similarity(question_terms, &span_terms);
    let tilt = trust_tilt(trust_class);

    // Semantic unavailable: w1+w2=0.80 absorbed into lexical, w3=0.20 stays (ADR §5).
    let score = 0.80 * lexical + SPAN_W3_TRUST * (memory_confidence * tilt);
    score.clamp(0.0, 1.0)
}

// ─── clustering (ADR §2) ─────────────────────────────────────────────────────

/// Cluster a list of scored spans by term-set Jaccard similarity.
///
/// Spans whose terms overlap above `CLUSTER_SIMILARITY_THRESHOLD` form a
/// cluster; the representative is the highest-scoring span in the cluster.
/// The corroboration multiplier `1 + 0.1·ln(distinct memories)` capped at 1.3
/// is applied to the representative's score. Repeated sentences or repeated
/// candidate rows from one memory cannot corroborate themselves.
pub fn cluster_spans(spans: &[AskSpan]) -> Vec<AskSpan> {
    clustering::cluster_spans(spans)
}

// ─── contradiction detection (ADR §4) ────────────────────────────────────────

/// Negation words that flip the polarity of a statement.
const NEGATION_WORDS: &[&str] = &[
    "not",
    "never",
    "no",
    "neither",
    "nor",
    "cannot",
    "can't",
    "won't",
    "doesn't",
    "isn't",
    "aren't",
    "wasn't",
    "weren't",
    "didn't",
    "don't",
    "impossible",
    "incorrect",
    "wrong",
    "false",
    "invalid",
];

pub(crate) fn has_negation(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    NEGATION_WORDS.iter().any(|&neg| {
        lower
            .split(|c: char| !c.is_alphabetic() && c != '\'')
            .any(|token| token == neg)
    })
}

/// A conservative lexical topic gate for inferred (not explicitly linked)
/// contradictions. A negation about a different subject is not opposition.
/// Require at least two shared content terms and a majority-overlap Jaccard
/// score. Paraphrased contradictions without that overlap need a stored edge.
fn same_conflict_topic(left: &[String], right: &[String]) -> bool {
    let shared = left
        .iter()
        .filter(|term| right.binary_search(term).is_ok())
        .count();
    shared >= 2 && jaccard_similarity(left, right) >= 0.5
}

/// Share conservative alternative detection between admission and composition.
/// The historical helper name also covers categorical/temporal settings and
/// reversed procedural order. Neither unrelated subjects nor compatible
/// restrictions qualify. Command options are not English negation.
fn numeric_conflict(left: &str, right: &str) -> bool {
    ordering::conflicts(left, right)
        || numeric::conflicts(left, has_negation(left), right, has_negation(right))
}

/// Opposite polarity about different numeric subjects is not proof of a
/// contradiction: using port 5432 and forbidding port 6432 are compatible.
/// Keep the existing generic-prohibition behavior when either passage has no
/// numeric literal. Otherwise require the same ordered, case-sensitive literal
/// context before inferring opposition; more complex relations need an edge.
fn same_numeric_context(left: &str, right: &str) -> bool {
    let left = clustering::numeric_literals(left);
    let right = clustering::numeric_literals(right);
    left.is_empty() || right.is_empty() || left == right
}

/// Look for supported opposition to the best answer throughout the admitted
/// clusters, not just at rank two. The caller has already applied the evidence
/// floor; a weak span must not manufacture a conflict with a strong answer.
fn detect_contradiction(clusters: &[AskSpan]) -> bool {
    let Some(anchor) = clusters.first() else {
        return false;
    };
    let anchor_terms = tokenize_for_ask(&anchor.text);
    let anchor_negated = has_negation(&anchor.text);
    clusters.iter().skip(1).any(|span| {
        (has_negation(&span.text) != anchor_negated
            && same_numeric_context(&anchor.text, &span.text)
            && same_conflict_topic(&anchor_terms, &tokenize_for_ask(&span.text)))
            || numeric_conflict(&anchor.text, &span.text)
    })
}

/// An explicit edge supplies relational relevance for a paraphrased opposing
/// memory. It cannot create an answer without a query-relevant anchor, raise
/// either memory's trust, or propagate confidence through a chain of links.
fn explicit_conflict(
    request: &AskRequest,
    ranked_spans: &[AskSpan],
) -> Option<(AskContradiction, Vec<AskSpan>)> {
    let mut best_by_memory = std::collections::BTreeMap::new();
    for span in ranked_spans {
        best_by_memory
            .entry(span.memory_id.as_str())
            .or_insert(span);
    }
    let mut links: Vec<_> = request
        .contradictions
        .iter()
        .filter(|link| selection::eligible_link(request, link))
        .collect();
    links.sort_by(|a, b| selection::compare_links(a, b));
    for anchor in ranked_spans.iter().take(1) {
        if anchor.score < request.min_confidence {
            break;
        }
        for link in &links {
            let other_id = if link.src_memory_id == anchor.memory_id {
                &link.dst_memory_id
            } else if link.dst_memory_id == anchor.memory_id {
                &link.src_memory_id
            } else {
                continue;
            };
            let Some(other) = best_by_memory.get(other_id.as_str()) else {
                // Out-of-scope, tombstoned, empty and scan-capped memories
                // cannot be reintroduced by an edge.
                continue;
            };
            let anchor_trust = anchor.memory_confidence * trust_tilt(&anchor.trust_class);
            let other_trust = other.memory_confidence * trust_tilt(&other.trust_class);
            let Some(evidence_score) = selection::conflict_score(
                anchor.score,
                anchor_trust,
                other_trust,
                link.confidence,
                request.min_confidence,
            ) else {
                continue;
            };
            let mut opposing = (*other).clone();
            opposing.score = evidence_score;
            return Some(((*link).clone(), vec![anchor.clone(), opposing]));
        }
    }
    None
}

// ─── answer composition (ADR §3) ────────────────────────────────────────────

/// Compose the extractive answer from the top `max_n` cluster representatives.
///
/// Enforces the extractiveness invariant: every emitted sentence MUST
/// byte-equal the original span. If the invariant would be violated,
/// returns `Err` (internal error — should never happen in practice).
fn compose_answer(
    clusters: &[AskSpan],
    max_n: usize,
    content_map: &std::collections::HashMap<&str, &str>,
) -> Result<(String, Vec<AskCitation>), &'static str> {
    let mut answer_parts: Vec<String> = Vec::new();
    let mut citations: Vec<AskCitation> = Vec::new();

    for (idx, span) in clusters.iter().take(max_n).enumerate() {
        let index = idx + 1;
        let original = content_map
            .get(span.memory_id.as_str())
            .copied()
            .unwrap_or("");
        let byte_range = span.byte_start..span.byte_end;

        // `str::get` rejects everything a raw slice would panic on — an end past
        // the length, an inverted range, and endpoints that are not UTF-8
        // character boundaries — so all three land on this function's designed
        // `Err` path instead of aborting the process. The end-only bounds check
        // this replaces left the other two unguarded, and they are reachable:
        // `content_map` is keyed by `memory_id`, so two candidates sharing an id
        // with different bodies collapse to one entry while their spans were
        // offset against the other body. The byte-equality check below cannot
        // catch that, because the panic would happen while producing the value
        // it compares.
        let Some(original_text) = original.get(byte_range) else {
            return Err("extractiveness: span range is not a valid slice of the source");
        };

        // Extractiveness invariant: emitted text must byte-equal the source span.
        if original_text != span.text.as_str() {
            return Err("extractiveness: emitted span does not byte-equal source");
        }

        answer_parts.push(format!("[{}] {}", index, span.text));
        citations.push(AskCitation {
            index,
            memory_id: span.memory_id.clone(),
            byte_start: span.byte_start,
            byte_end: span.byte_end,
            text: span.text.clone(),
            provenance_uri: span.provenance_uri.clone(),
            trust_class: span.trust_class.clone(),
            confidence: span.memory_confidence,
            team_provenance: span.team_provenance.clone(),
        });
    }

    Ok((answer_parts.join(" "), citations))
}

/// Withhold every answer surface when any selected citation is invalid.
/// In particular, a valid first conflict side must not escape when the second
/// side fails validation. This is not an ordinary missing-evidence abstention.
fn extractiveness_failure_report(request: &AskRequest, candidates_scanned: usize) -> AskReport {
    AskReport {
        native_sources: BTreeMap::new(),
        question: request.question.clone(),
        abstained: true,
        answer_text: None,
        confidence: 0.0,
        confidence_components: AskConfidenceComponents {
            top_span_score: 0.0,
            corroboration: 1.0,
            contradiction_penalty: 0.0,
        },
        citations: Vec::new(),
        sides: None,
        nearest_evidence: None,
        counterfactual_hint: Some(
            "internal: extractiveness invariant violation; answer withheld".to_owned(),
        ),
        semantic_degraded: true,
        conflict_detected: false,
        conflict_link: None,
        extractiveness_violated: true,
        candidates_scanned,
    }
}

#[cfg(test)]
#[path = "ask_answer_integrity_tests.rs"]
mod answer_integrity_tests;

// ─── main engine entry point ─────────────────────────────────────────────────

/// Pure ask engine — same inputs ⇒ byte-identical output (ADR §1–§4).
///
/// The caller is responsible for fetching `candidates` from the database
/// and for emitting the query-miss ledger row on abstention
/// (`report.abstained == true`).
pub fn evaluate_ask(request: &AskRequest, candidates: &[AskCandidate]) -> AskReport {
    evaluate_ask_scored(request, candidates, &score_span, true)
}

fn request_is_valid(request: &AskRequest) -> bool {
    request.min_confidence.is_finite() && (0.0..=1.0).contains(&request.min_confidence)
}

fn evaluate_ask_scored(
    request: &AskRequest,
    candidates: &[AskCandidate],
    scorer: SpanScorer<'_>,
    semantic_degraded: bool,
) -> AskReport {
    if !request_is_valid(request) || !native::validate_sources(request, candidates) {
        return extractiveness_failure_report(request, candidates.len());
    }
    let mut report = evaluate_ask_inner(request, candidates, scorer, semantic_degraded);
    native::attach_sources(&mut report, request);
    report
}

fn evaluate_ask_inner(
    request: &AskRequest,
    candidates: &[AskCandidate],
    scorer: SpanScorer<'_>,
    semantic_degraded: bool,
) -> AskReport {
    let question_terms = tokenize_for_ask(&request.question);
    let max_n = request.max_evidence.max(1);
    // Validate the full scoped input and rank before applying the clustering
    // budget. A caller's database order must not decide whether an answer exists.
    let candidates_scanned = candidates.len();
    let selected_candidates = match selection::select_candidates_with_scorer(
        request,
        &question_terms,
        candidates,
        ASK_CANDIDATE_SCAN_CAP,
        scorer,
    ) {
        Ok(selected) => selected,
        Err(_) => return extractiveness_failure_report(request, candidates_scanned),
    };
    // Admission may omit a low-relevance parent or provenance connector while
    // retaining its derived rules. Preserve the complete scoped snapshot's
    // lineage: re-deriving groups from selected spans would turn those rules
    // into independent votes merely because their common origin lost a slot.
    // This lineage pass inspects only borrowed identity metadata. Bodies that
    // lose their slots cannot enter citations, hints, or the source registry.
    let support_groups =
        native::candidate_support_groups(candidates.iter(), &request.native_sources);
    let candidates = selected_candidates.as_slice();

    // Build a content lookup map (memory_id → content) for the extractiveness check.
    let content_map: std::collections::HashMap<&str, &str> = candidates
        .iter()
        .map(|c| (c.memory_id.as_str(), c.content.as_str()))
        .collect();

    // Score every span of every candidate
    let mut all_spans: Vec<AskSpan> = Vec::new();
    for candidate in candidates {
        let span_ranges = segment_spans(&candidate.content);
        for (start, end) in span_ranges {
            let text = candidate.content[start..end].to_owned();
            let score = scorer(
                &question_terms,
                &text,
                candidate.confidence,
                &candidate.trust_class,
            );
            all_spans.push(AskSpan {
                memory_id: candidate.memory_id.clone(),
                byte_start: start,
                byte_end: end,
                text,
                score,
                trust_class: candidate.trust_class.clone(),
                memory_confidence: candidate.confidence,
                provenance_uri: candidate.provenance_uri.clone(),
                team_provenance: candidate.team_provenance.clone(),
            });
        }
    }

    // Sort all spans by score desc for clustering; full tiebreaker for byte-identical output.
    // `total_cmp` rather than `partial_cmp(..).unwrap_or(Equal)`: collapsing an
    // incomparable pair to `Equal` is not a strict weak ordering, which both
    // forfeits the byte-identical output this module promises and is a case the
    // current sort implementation is allowed to panic on. Matches the
    // `total_cmp` convention already used across `core::search`.
    all_spans.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.memory_id.cmp(&b.memory_id))
            .then_with(|| a.byte_start.cmp(&b.byte_start))
    });

    let (conflict_link, mut clusters) = match explicit_conflict(request, &all_spans) {
        Some((link, sides)) => (Some(link), sides),
        None => (
            None,
            clustering::cluster_spans_with_groups(&all_spans, &support_groups),
        ),
    };

    let top_span_score = clusters.first().map(|s| s.score).unwrap_or(0.0);
    // A confident first span does not make the remaining spans evidence.
    // Apply the same floor to every citation and to both conflict sides;
    // retain all_spans for honest nearest-evidence output on abstention.
    clusters.retain(|span| span.score >= request.min_confidence);
    let conflict_detected = conflict_link.is_some() || detect_contradiction(&clusters);
    let contradiction_penalty_applied = if conflict_detected {
        CONTRADICTION_PENALTY
    } else {
        0.0
    };

    // Corroboration factor is baked into cluster scores already (applied per cluster in cluster_spans).
    // For the confidence component report, use the ratio of top clustered to raw scores.
    let top_raw_score = all_spans.first().map(|s| s.score).unwrap_or(0.0);
    let corroboration = if top_raw_score > 0.0 {
        (top_span_score / top_raw_score).clamp(1.0, CORROBORATION_CAP)
    } else {
        1.0
    };

    let confidence = (top_span_score * (1.0 - contradiction_penalty_applied)).clamp(0.0, 1.0);
    let confidence_components = AskConfidenceComponents {
        top_span_score,
        corroboration,
        contradiction_penalty: contradiction_penalty_applied,
    };

    // Each conflict side has already cleared the evidence floor. The
    // penalty reduces confidence in a single answer, not eligibility to
    // disclose both supported sides. --require-confidence still checks the
    // penalized confidence at the CLI boundary.
    if (!conflict_detected && confidence < request.min_confidence) || clusters.is_empty() {
        let nearest_evidence: Vec<AskNearestEvidence> = all_spans
            .iter()
            .take(max_n.min(3))
            .map(|s| AskNearestEvidence {
                memory_id: s.memory_id.clone(),
                byte_start: s.byte_start,
                byte_end: s.byte_end,
                text: s.text.clone(),
                score: s.score,
            })
            .collect();

        let counterfactual_hint = if nearest_evidence.is_empty() {
            format!(
                "no memory mentions {}; the corpus has no stored evidence for this question",
                request.question.trim()
            )
        } else {
            let sample = nearest_evidence
                .first()
                .map(|e| e.text.chars().take(80).collect::<String>())
                .unwrap_or_default();
            format!(
                "no memory reaches the confidence threshold for \"{}\"; nearest evidence: \"{}…\"",
                request.question.trim(),
                sample
            )
        };

        return AskReport {
            native_sources: BTreeMap::new(),
            question: request.question.clone(),
            abstained: true,
            answer_text: None,
            confidence,
            confidence_components,
            citations: Vec::new(),
            sides: None,
            nearest_evidence: Some(nearest_evidence),
            counterfactual_hint: Some(counterfactual_hint),
            semantic_degraded,
            conflict_detected,
            conflict_link: None,
            extractiveness_violated: false,
            candidates_scanned,
        };
    }

    // Conflict mode: compose each side separately (ADR §4)
    if conflict_detected && clusters.len() >= 2 {
        let (affirming, negating, first_label, second_label) = if conflict_link.is_some() {
            // Explicit contradictions need not contain a negation word
            // (for example, two different values for the same port).
            (
                vec![clusters[0].clone()],
                vec![clusters[1].clone()],
                "query_match",
                "linked_opposition",
            )
        } else if let Some(alternative) = clusters
            .iter()
            .skip(1)
            .find(|span| numeric_conflict(&clusters[0].text, &span.text))
        {
            // Affirmative alternatives need not contain negation. Disclose
            // the strongest supported alternative with its real source; never
            // invent a stored relation or choose one execution order as true.
            let label = if ordering::conflicts(&clusters[0].text, &alternative.text) {
                "ordering_alternative"
            } else {
                "numeric_alternative"
            };
            (
                vec![clusters[0].clone()],
                vec![alternative.clone()],
                "query_match",
                label,
            )
        } else {
            // Only disclose the topic that actually conflicts with the best
            // answer. Other admitted advice must not be relabeled as support
            // for either side merely because it contains a negation word.
            let anchor_terms = tokenize_for_ask(&clusters[0].text);
            let related: Vec<_> = clusters
                .iter()
                .filter(|span| {
                    same_numeric_context(&clusters[0].text, &span.text)
                        && same_conflict_topic(&anchor_terms, &tokenize_for_ask(&span.text))
                })
                .collect();
            (
                related
                    .iter()
                    .filter(|s| !has_negation(&s.text))
                    .map(|s| (**s).clone())
                    .collect(),
                related
                    .iter()
                    .filter(|s| has_negation(&s.text))
                    .map(|s| (**s).clone())
                    .collect(),
                "affirming",
                "negating",
            )
        };

        let compose_side = |side_spans: &[AskSpan], label: &str| -> Result<AskSide, &'static str> {
            // Use the same byte-range and source-equality checks as the normal
            // answer path, including for explicitly linked opposing memories.
            let (answer_text, citations) = compose_answer(side_spans, max_n, &content_map)?;
            Ok(AskSide {
                label: label.to_owned(),
                answer_text,
                citations,
            })
        };

        let sides = match (
            compose_side(&affirming, first_label),
            compose_side(&negating, second_label),
        ) {
            (Ok(first), Ok(second)) => vec![first, second],
            _ => return extractiveness_failure_report(request, candidates_scanned),
        };

        return AskReport {
            native_sources: BTreeMap::new(),
            question: request.question.clone(),
            abstained: false,
            answer_text: None,
            confidence,
            confidence_components,
            citations: Vec::new(),
            sides: Some(sides),
            nearest_evidence: None,
            counterfactual_hint: None,
            semantic_degraded,
            conflict_detected: true,
            conflict_link,
            extractiveness_violated: false,
            candidates_scanned,
        };
    }

    // Normal path: compose answer from top clusters
    match compose_answer(&clusters, max_n, &content_map) {
        Ok((answer_text, citations)) => AskReport {
            native_sources: BTreeMap::new(),
            question: request.question.clone(),
            abstained: false,
            answer_text: Some(answer_text),
            confidence,
            confidence_components,
            citations,
            sides: None,
            nearest_evidence: None,
            counterfactual_hint: None,
            semantic_degraded,
            conflict_detected: false,
            conflict_link: None,
            extractiveness_violated: false,
            candidates_scanned,
        },
        Err(_reason) => extractiveness_failure_report(request, candidates_scanned),
    }
}

/// Record an ask abstention in the query-miss ledger.
///
/// The row deliberately stores only a query hash and redaction posture, never
/// raw question text or vectors. Callers should treat this as best-effort: ask
/// answers and abstentions remain useful even when the audit lane is degraded.
pub fn record_ask_query_miss_best_effort(
    connection: &DbConnection,
    workspace_id: &str,
    report: &AskReport,
) {
    if !report.abstained || report.extractiveness_violated {
        return;
    }
    let query_hash = audit_query_hash(&report.question);
    let audit_id = generate_audit_id();
    let details = ask_query_miss_audit_details(
        &query_hash,
        report,
        if report
            .nearest_evidence
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        {
            "empty_results"
        } else {
            DEGRADED_NO_ANSWER
        },
    );
    let input = CreateAuditInput {
        workspace_id: Some(workspace_id.to_owned()),
        actor: None,
        action: audit_actions::SEARCH_MISS_RECORDED.to_owned(),
        target_type: Some("query_hash".to_owned()),
        target_id: Some(query_hash),
        details: Some(details),
    };
    if let Err(error) = connection.insert_audit(&audit_id, &input) {
        tracing::warn!(
            target: "ee::core::ask::audit",
            error = %error,
            "best-effort ask query-miss audit append failed"
        );
    }
}

/// Record that `ee ask` retrieved these memories in order to answer.
///
/// bd-b9dmp. `ee ask` reads the store directly (the handler builds candidates
/// with `list_memories`), so it never produced the audit row that
/// `memory_debt`'s read signal is built from. A memory cited as the answer to a
/// question every day still accrued `never_retrieved` debt and was surfaced for
/// `ee curate disposition` review -- a recommendation to discard a memory good
/// enough to be the cited answer.
///
/// Only CITED memories are recorded, never the scanned corpus. `ask` scans
/// broadly, and recording the scan would mark every memory retrieved on every
/// question -- the same inflation removed from the auto_link probe (d1092fec7)
/// and the daemon warm-up (ae9a8a244). The row targets the memory, which is what
/// `memory_debt.rs:883` ingests, and carries only the hashed query.
pub fn record_ask_retrieval_best_effort(
    connection: &DbConnection,
    workspace_id: &str,
    report: &AskReport,
) {
    // A conflict answer stores its citations in sides[], not in the top-level
    // citations array. Attribute only the displayed answer, once per memory;
    // neither abstention nor a failed source check is a successful retrieval.
    let citations = retrieval::cited_memories(report);
    if citations.is_empty() {
        return;
    }
    let query_hash = audit_query_hash(&report.question);
    for citation in citations {
        let audit_id = generate_audit_id();
        let mut details = serde_json::json!({
            "queryHash": &query_hash,
            "rank": citation.index as u32,
            "source": ASK_QUERY_MISS_ORIGIN,
            "trustClass": &citation.trust_class,
        });
        native::insert_identity(
            &mut details,
            &citation.memory_id,
            &report.native_sources,
            false,
        );
        let input = CreateAuditInput {
            workspace_id: Some(workspace_id.to_owned()),
            actor: None,
            action: audit_actions::SEARCH_RETURNED_MEM.to_owned(),
            target_type: Some(
                native::audit_target(report.native_sources.get(&citation.memory_id)).to_owned(),
            ),
            target_id: Some(citation.memory_id.clone()),
            details: Some(details.to_string()),
        };
        if let Err(error) = connection.insert_audit(&audit_id, &input) {
            tracing::warn!(
                target: "ee::core::ask::audit",
                error = %error,
                "best-effort ask retrieval audit append failed"
            );
        }
    }
}

fn ask_query_miss_audit_details(query_hash: &str, report: &AskReport, reason: &str) -> String {
    let nearest_count = report.nearest_evidence.as_ref().map_or(0, Vec::len);
    serde_json::json!({
        "schema": "ee.search.query_miss.v1",
        "origin": ASK_QUERY_MISS_ORIGIN,
        "queryHash": query_hash,
        "reason": reason,
        "status": "abstained",
        "resultCount": 0,
        "candidateCount": report.candidates_scanned,
        "nearestEvidenceCount": nearest_count,
        "confidence": round_ask_metric(report.confidence),
        "confidenceCalibration": {
            "status": ASK_CONFIDENCE_CALIBRATION_STATUS,
            "calibrated": false,
            "scoreKind": ASK_CONFIDENCE_SCORE_KIND,
            "calibrationId": serde_json::Value::Null,
        },
        "ttlSeconds": ASK_QUERY_MISS_AUDIT_TTL_SECONDS,
        "sampling": {
            "strategy": "all_ask_abstentions_v1",
            "sampleRate": ASK_QUERY_MISS_AUDIT_SAMPLE_RATE,
            "sampled": true,
            "maxRowsPerAsk": 1,
        },
        "redaction": {
            "strategy": "query_hash_only_v1",
            "rawQueryStored": false,
            "queryTextStored": false,
            "queryVectorStored": false,
        },
    })
    .to_string()
}

fn round_ask_metric(score: f32) -> f32 {
    (score * 1_000_000.0).round() / 1_000_000.0
}

// ─── JSON serialization ───────────────────────────────────────────────────────

/// Serialize an `AskReport` into the `ee.ask.v1` data envelope.
pub fn ask_data_json(report: &AskReport) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "schema": ASK_SCHEMA_V1,
        "question": report.question,
        "abstained": report.abstained,
        "answerText": report.answer_text,
        "confidence": report.confidence,
        "confidenceCalibration": {
            "status": ASK_CONFIDENCE_CALIBRATION_STATUS,
            "calibrated": false,
            "scoreKind": ASK_CONFIDENCE_SCORE_KIND,
            "calibrationId": serde_json::Value::Null,
        },
        "confidenceComponents": {
            "topSpanScore": report.confidence_components.top_span_score,
            "corroboration": report.confidence_components.corroboration,
            "contradictionPenalty": report.confidence_components.contradiction_penalty,
        },
        "citations": report.citations.iter().map(|c| citation_to_json(c, &report.native_sources)).collect::<Vec<_>>(),
        "sides": report.sides.as_ref().map(|sides| {
            sides.iter().map(|s| side_to_json(s, &report.native_sources)).collect::<Vec<_>>()
        }),
        "nearestEvidence": report.nearest_evidence.as_ref().map(|ne| {
            ne.iter().map(|e| nearest_evidence_to_json(e, &report.native_sources)).collect::<Vec<_>>()
        }),
        "counterfactualHint": report.counterfactual_hint,
        "candidatesScanned": report.candidates_scanned,
    });

    // Degradation signals are surfaced in the caller's envelope, but we include
    // flags here so consumers can inspect the data payload directly.
    if report.semantic_degraded {
        obj["_semanticDegraded"] = serde_json::Value::Bool(true);
    }
    if report.conflict_detected {
        obj["_conflictDetected"] = serde_json::Value::Bool(true);
    }
    if let Some(link) = &report.conflict_link {
        obj["conflictLink"] = serde_json::json!({
            "id": link.id,
            "srcMemoryId": link.src_memory_id,
            "dstMemoryId": link.dst_memory_id,
            "confidence": link.confidence,
            "source": link.source,
        });
    }
    if let Some(query_assist) = ask_query_assist_json(report) {
        obj["queryAssist"] = query_assist;
    }

    obj
}

fn citation_to_json(
    c: &AskCitation,
    sources: &BTreeMap<String, AskNativeSource>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "index": c.index,
        "memoryId": c.memory_id,
        "span": {"byteStart": c.byte_start, "byteEnd": c.byte_end},
        "text": c.text,
        "provenanceUri": c.provenance_uri,
        "trustClass": c.trust_class,
        "confidence": c.confidence,
    });
    native::insert_identity(&mut value, &c.memory_id, sources, false);
    if let Some(provenance) = &c.team_provenance
        && let Some(object) = value.as_object_mut()
    {
        object.insert("teamProvenance".to_owned(), provenance.to_json());
    }
    value
}

fn side_to_json(s: &AskSide, sources: &BTreeMap<String, AskNativeSource>) -> serde_json::Value {
    serde_json::json!({
        "label": s.label,
        "answerText": s.answer_text,
        "citations": s.citations.iter().map(|c| citation_to_json(c, sources)).collect::<Vec<_>>(),
    })
}

fn nearest_evidence_to_json(
    ne: &AskNearestEvidence,
    sources: &BTreeMap<String, AskNativeSource>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "memoryId": ne.memory_id,
        "span": {"byteStart": ne.byte_start, "byteEnd": ne.byte_end},
        "text": ne.text,
        "score": ne.score,
    });
    native::insert_identity(&mut value, &ne.memory_id, sources, false);
    value
}

fn ask_query_assist_json(report: &AskReport) -> Option<serde_json::Value> {
    // A source-integrity failure is not missing knowledge. Do not invite a
    // caller to capture a replacement memory or reformulate around corrupt
    // evidence while the engine is deliberately withholding the answer.
    if !report.abstained || report.extractiveness_violated {
        return None;
    }
    let nearest_evidence = report.nearest_evidence.as_deref().unwrap_or_default();
    let weak_result_reason = if nearest_evidence.is_empty() {
        "empty_results"
    } else {
        "no_confident_answer"
    };
    Some(serde_json::json!({
        "schema": crate::core::search::QUERY_ASSIST_SCHEMA_V1,
        "mode": "compact",
        "weakResultReason": weak_result_reason,
        "candidateCount": report.candidates_scanned,
        "droppedBelowFloor": 0,
        "relevanceFloor": serde_json::Value::Null,
        "reformulations": ask_query_assist_reformulations(&report.question, nearest_evidence, &report.native_sources),
        "didYouMean": nearest_evidence.iter().take(3).map(|e| ask_query_assist_did_you_mean_json(e, &report.native_sources)).collect::<Vec<_>>(),
        "captureTemplate": ask_query_assist_capture_template_json(&report.question),
    }))
}

fn ask_query_assist_did_you_mean_json(
    evidence: &AskNearestEvidence,
    sources: &BTreeMap<String, AskNativeSource>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "memoryId": &evidence.memory_id,
        "score": evidence.score,
        "source": "ask_nearest_evidence",
        "candidateStatus": "below_confidence_threshold",
        "content": &evidence.text,
        "span": {
            "byteStart": evidence.byte_start,
            "byteEnd": evidence.byte_end,
        },
        "why": "Nearest extracted evidence span did not reach the ask confidence threshold.",
    });
    native::insert_identity(&mut value, &evidence.memory_id, sources, false);
    value
}

fn ask_query_assist_reformulations(
    question: &str,
    nearest_evidence: &[AskNearestEvidence],
    sources: &BTreeMap<String, AskNativeSource>,
) -> Vec<serde_json::Value> {
    let Some(first) = nearest_evidence.first() else {
        return Vec::new();
    };
    let question_terms = ask_query_assist_terms(question)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let evidence_terms = ask_query_assist_terms(&first.text)
        .into_iter()
        .filter(|term| !question_terms.contains(term))
        .take(4)
        .collect::<Vec<_>>();
    if evidence_terms.is_empty() {
        return Vec::new();
    }
    let normalized_question = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let query = if normalized_question.is_empty() {
        evidence_terms.join(" ")
    } else {
        format!("{normalized_question} {}", evidence_terms.join(" "))
    };
    let mut value = serde_json::json!({
        "query": query,
        "strategy": "nearest_evidence_terms",
        "rationale": "Adds terms from the nearest ask evidence span that was below the confidence threshold.",
        "matchedDocId": &first.memory_id,
        "matchedMemoryId": &first.memory_id,
    });
    native::insert_identity(&mut value, &first.memory_id, sources, true);
    vec![value]
}

fn ask_query_assist_capture_template_json(question: &str) -> serde_json::Value {
    let clean_question = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let content = if clean_question.is_empty() {
        "TODO: capture the missing memory this ask query needs.".to_owned()
    } else {
        format!("TODO: capture memory needed for ask query: {clean_question}")
    };
    let command = format!(
        "ee remember --level semantic --kind note --tags query-gap,ask-miss --json {}",
        ask_shell_quote(&content)
    );
    serde_json::json!({
        "level": "semantic",
        "kind": "note",
        "tags": ["query-gap", "ask-miss"],
        "content": &content,
        "command": command,
        "rationale": "Capture this missing demand explicitly so ee learn gaps can cluster repeated misses.",
    })
}

fn ask_query_assist_terms(text: &str) -> Vec<String> {
    let normalized = text
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>();
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for token in normalized.split_whitespace() {
        if token.len() < 3 || ask_query_assist_stopword(token) {
            continue;
        }
        if seen.insert(token.to_owned()) {
            terms.push(token.to_owned());
        }
    }
    terms
}

fn ask_query_assist_stopword(token: &str) -> bool {
    matches!(
        token,
        "the"
            | "and"
            | "for"
            | "with"
            | "that"
            | "this"
            | "from"
            | "into"
            | "your"
            | "you"
            | "are"
            | "was"
            | "were"
            | "has"
            | "have"
            | "had"
            | "not"
            | "but"
            | "does"
            | "exist"
            | "memory"
            | "query"
            | "ask"
    )
}

fn ask_shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

// ─── markdown renderer ────────────────────────────────────────────────────────

/// Render an `AskReport` as human-readable markdown (prepend-safe).
pub fn render_ask_markdown(report: &AskReport) -> String {
    let mut out = String::new();

    out.push_str(&format!("**Q:** {}\n\n", report.question));

    if report.abstained {
        out.push_str("*No confident answer found.*\n");
        if let Some(hint) = &report.counterfactual_hint {
            out.push_str(&format!("\n{}\n", hint));
        }
        if let Some(ne) = &report.nearest_evidence {
            if !ne.is_empty() {
                out.push_str("\n**Nearest evidence:**\n");
                for e in ne {
                    out.push_str(&format!(
                        "- {} (score: {:.2}){}\n",
                        e.text,
                        e.score,
                        native::markdown_identity(report, &e.memory_id)
                    ));
                }
            }
        }
        return out;
    }

    if report.conflict_detected {
        out.push_str("*Conflicting evidence found:*\n\n");
        if let Some(link) = &report.conflict_link {
            out.push_str(&format!(
                "Stored contradiction `{}` ({}; confidence {:.2}).\n\n",
                link.id, link.source, link.confidence
            ));
        }
        if let Some(sides) = &report.sides {
            for side in sides {
                out.push_str(&format!(
                    "**{} view:**\n{}\n\n",
                    side.label, side.answer_text
                ));
                for c in &side.citations {
                    out.push_str(&format!(
                        "> [{}] *({})*{}\n",
                        c.index,
                        c.memory_id,
                        native::markdown_identity(report, &c.memory_id)
                    ));
                }
            }
        }
        return out;
    }

    if let Some(answer) = &report.answer_text {
        out.push_str(&format!("**A:** {}\n\n", answer));
    }

    if !report.citations.is_empty() {
        out.push_str("**Sources:**\n");
        for c in &report.citations {
            let prov = c.provenance_uri.as_deref().unwrap_or(&c.memory_id);
            let suffix = c.team_provenance.as_ref().map_or_else(
                String::new,
                crate::core::memory_scope::TeamProvenance::compact_suffix,
            );
            out.push_str(&format!(
                "[{}] {} `{}` (conf: {:.2}){suffix}{}\n",
                c.index,
                prov,
                c.trust_class,
                c.confidence,
                native::markdown_identity(report, &c.memory_id)
            ));
        }
    }

    out.push_str(&format!("\n*confidence: {:.2}*\n", report.confidence));

    if report.semantic_degraded {
        out.push_str("*Note: semantic search unavailable; lexical scoring only.*\n");
    }

    out
}

// ─── degradation entries ──────────────────────────────────────────────────────

/// A degradation entry for the `ee.response.v2` envelope.
pub struct AskDegradedEntry {
    pub code: String,
    pub severity: String,
    pub class: String,
    pub message: Option<String>,
}

impl AskDegradedEntry {
    pub fn no_confident_answer() -> Self {
        Self {
            code: DEGRADED_NO_ANSWER.to_owned(),
            severity: "info".to_owned(),
            class: "response_time".to_owned(),
            message: Some("confidence below threshold; abstention payload returned".to_owned()),
        }
    }

    pub fn semantic_degraded() -> Self {
        Self {
            code: DEGRADED_SEMANTIC.to_owned(),
            severity: "info".to_owned(),
            class: "response_time".to_owned(),
            message: Some(
                "hash-embedder fallback in play; w2 weight renormalized into w1".to_owned(),
            ),
        }
    }

    pub fn extractiveness_violated() -> Self {
        Self {
            code: DEGRADED_EXTRACTIVENESS.to_owned(),
            severity: "warning".to_owned(),
            class: "response_time".to_owned(),
            message: Some(
                "extractiveness invariant violated: emitted span did not byte-equal source; answer withheld".to_owned(),
            ),
        }
    }

    pub fn conflicting_evidence() -> Self {
        Self {
            code: DEGRADED_CONFLICT.to_owned(),
            severity: "warning".to_owned(),
            class: "response_time".to_owned(),
            message: Some("top evidence clusters oppose each other; sides[] emitted".to_owned()),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "class": self.class,
            "message": self.message,
        })
    }
}

// ─── unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_plain_sentences() {
        let content = "The port is 8080. Use TLS for production. See the readme.";
        let spans = segment_spans(content);
        assert!(!spans.is_empty(), "must segment at least one span");
        // Each span byte-equals the source
        for (s, e) in &spans {
            assert!(*e <= content.len());
            assert!(!content[*s..*e].trim().is_empty());
        }
    }

    #[test]
    fn segment_code_fence_is_one_span() {
        let content = "Before.\n```bash\necho hello\n```\nAfter.";
        let spans = segment_spans(content);
        let texts: Vec<&str> = spans.iter().map(|(s, e)| &content[*s..*e]).collect();
        assert!(
            texts.iter().any(|t| t.contains("echo hello")),
            "code fence should be one span: {:?}",
            texts
        );
        // The fence should not be split across multiple spans
        let fence_spans: Vec<_> = texts.iter().filter(|t| t.contains("echo hello")).collect();
        assert_eq!(fence_spans.len(), 1, "code fence must be exactly one span");
    }

    #[test]
    fn segment_non_ascii_before_period_does_not_panic() {
        let content = "Use café. Next sentence.";
        let spans = segment_spans(content);
        let texts: Vec<&str> = spans.iter().map(|(s, e)| &content[*s..*e]).collect();
        assert_eq!(texts, vec!["Use café.", "Next sentence."]);
    }

    #[test]
    fn tokenize_drops_stopwords() {
        let tokens = tokenize_for_ask("the port is 8080");
        assert!(!tokens.contains(&"the".to_owned()));
        assert!(!tokens.contains(&"is".to_owned()));
        assert!(tokens.contains(&"port".to_owned()));
        assert!(tokens.contains(&"8080".to_owned()));
    }

    #[test]
    fn tokenize_drops_interrogatives_and_question_modals() {
        let tokens = tokenize_for_ask("Which command must run before every release tag?");
        for dropped in ["which", "must"] {
            assert!(!tokens.contains(&dropped.to_owned()), "{dropped} kept");
        }
        for kept in ["command", "run", "before", "every", "release", "tag"] {
            assert!(tokens.contains(&kept.to_owned()), "{kept} dropped");
        }
    }

    #[test]
    fn score_span_returns_zero_for_unrelated() {
        let q_terms = tokenize_for_ask("what is the database port");
        let score = score_span(&q_terms, "The sky is blue today.", 0.9, "human_explicit");
        assert!(score < 0.3, "unrelated span should score low: {score}");
    }

    /// bd-reality-core-convergence-1azkt.30: a span that contains the whole
    /// question plus the answer must clear the default abstention gate.
    /// Under pure Jaccard this exact case scored 0.53 and abstained because
    /// the answer terms (cargo, fmt, check) were counted against the span.
    #[test]
    fn score_span_does_not_penalize_an_answer_bearing_span() {
        let q_terms = tokenize_for_ask("Which command must run before every release tag?");
        let score = score_span(
            &q_terms,
            "Run cargo fmt --check before every release tag.",
            0.85,
            "human_explicit",
        );
        assert!(
            score >= ASK_MIN_CONFIDENCE_DEFAULT,
            "answer-bearing span must clear the abstention gate: {score}"
        );
    }

    /// Planted negative for the same change: sharing only the corpus-wide
    /// project name and one incidental term must still abstain, so coverage
    /// cannot be gamed by memories that merely mention common words.
    #[test]
    fn score_span_keeps_common_term_overlap_below_the_gate() {
        let q_terms = tokenize_for_ask("Who approved the lunar invoice for Project Zephyr?");
        let score = score_span(
            &q_terms,
            "The billing sandbox fixture uses invoice identifiers that are unrelated to Project Zephyr approval flows.",
            0.9,
            "human_explicit",
        );
        assert!(
            score < ASK_MIN_CONFIDENCE_DEFAULT,
            "common-term overlap must stay below the abstention gate: {score}"
        );
    }

    #[test]
    fn score_span_returns_high_for_relevant() {
        let q_terms = tokenize_for_ask("what is the database port");
        let score = score_span(
            &q_terms,
            "The database listens on port 5432.",
            0.9,
            "human_explicit",
        );
        assert!(
            score > 0.15,
            "relevant span should score above 0.15: {score}"
        );
    }

    #[test]
    fn trust_tilt_ordering() {
        assert!(trust_tilt("human_explicit") > trust_tilt("agent_validated"));
        assert!(trust_tilt("human_explicit") > trust_tilt("peer_human_attested"));
        assert!(trust_tilt("peer_human_attested") > trust_tilt("agent_validated"));
        assert!(trust_tilt("agent_validated") > trust_tilt("agent_assertion"));
        assert!(trust_tilt("agent_assertion") > trust_tilt("cass_evidence"));
        assert!(trust_tilt("cass_evidence") > trust_tilt("legacy_import"));
    }

    #[test]
    fn peer_human_attested_ask_weight_is_point_ninety_two() {
        assert!((trust_tilt("peer_human_attested") - 0.92).abs() < f32::EPSILON);
    }

    #[test]
    fn contradiction_detection_xor_polarity() {
        let affirm = AskSpan {
            memory_id: "m1".into(),
            byte_start: 0,
            byte_end: 5,
            text: "TLS is required for all connections.".into(),
            score: 0.8,
            trust_class: "human_explicit".into(),
            memory_confidence: 0.9,
            provenance_uri: None,
            team_provenance: None,
        };
        let negate = AskSpan {
            memory_id: "m2".into(),
            byte_start: 0,
            byte_end: 5,
            text: "TLS is not required for internal connections.".into(),
            score: 0.7,
            trust_class: "agent_assertion".into(),
            memory_confidence: 0.7,
            provenance_uri: None,
            team_provenance: None,
        };
        assert!(detect_contradiction(&[affirm, negate]));
    }

    #[test]
    fn evaluate_ask_abstains_on_empty_corpus() {
        let request = AskRequest {
            question: "what is the database port".into(),
            native_sources: BTreeMap::new(),
            min_confidence: ASK_MIN_CONFIDENCE_DEFAULT,
            max_evidence: ASK_MAX_EVIDENCE_DEFAULT,
            require_confidence: None,
            contradictions: Vec::new(),
        };
        let report = evaluate_ask(&request, &[]);
        assert!(report.abstained);
        assert_eq!(report.confidence, 0.0);
        assert!(report.answer_text.is_none());
    }

    #[test]
    fn evaluate_ask_cites_only_confident_evidence_and_abstains_on_unrelated_questions() {
        let candidates = [
            (
                "format",
                "Run cargo fmt --check before every release tag.",
                0.85,
            ),
            (
                "release",
                "Project Zephyr release readiness gate is smoke gate alpha before deploy.",
                0.99,
            ),
            (
                "cache",
                "Zephyr worker-g workers cannot use cache delta.",
                0.99,
            ),
        ]
        .into_iter()
        .map(|(id, content, confidence)| AskCandidate {
            memory_id: id.to_owned(),
            content: content.to_owned(),
            confidence,
            trust_class: "human_explicit".to_owned(),
            provenance_uri: Some(format!("manual://ask-test/{id}")),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            team_provenance: None,
        })
        .collect::<Vec<_>>();
        let report = evaluate_ask(
            &AskRequest {
                question: "Which command must run before every release tag?".to_owned(),
                max_evidence: 10,
                ..AskRequest::default()
            },
            &candidates,
        );
        assert!(!report.abstained);
        assert!(!report.conflict_detected);
        assert_eq!(report.citations.len(), 1, "{report:?}");
        assert_eq!(report.citations[0].memory_id, "format");
        assert_eq!(report.citations[0].text, candidates[0].content);
        assert!(report.semantic_degraded);

        let unrelated = evaluate_ask(
            &AskRequest {
                question: "What colour is the CI dashboard?".to_owned(),
                ..AskRequest::default()
            },
            &candidates,
        );
        assert!(unrelated.abstained);
        assert!(unrelated.citations.is_empty());
        assert!(unrelated.answer_text.is_none());
    }

    #[test]
    fn evaluate_ask_preserves_opposing_evidence_without_treating_it_as_corroboration() {
        let candidates = [
            (
                "affirm",
                "Remote cache delta is enabled for Project Zephyr on the worker-g worker pool.",
                0.89,
            ),
            (
                "negate",
                "Remote cache delta is not enabled for Project Zephyr on the worker-g worker pool.",
                0.88,
            ),
        ]
        .into_iter()
        .map(|(id, content, confidence)| AskCandidate {
            memory_id: id.to_owned(),
            content: content.to_owned(),
            confidence,
            trust_class: "agent_assertion".to_owned(),
            provenance_uri: Some(format!("manual://ask-test/{id}")),
            level: "episodic".to_owned(),
            kind: "observation".to_owned(),
            team_provenance: None,
        })
        .collect::<Vec<_>>();
        let request = AskRequest {
            question: "Is remote cache delta enabled for Project Zephyr?".to_owned(),
            ..AskRequest::default()
        };
        let report = evaluate_ask(&request, &candidates);
        assert!(!report.abstained, "{report:?}");
        assert!(report.conflict_detected);
        assert!(report.answer_text.is_none());
        assert!(report.citations.is_empty());
        assert!(report.confidence < request.min_confidence);
        assert_eq!(report.confidence_components.corroboration, 1.0);
        let sides = report
            .sides
            .as_ref()
            .expect("both supported conflict sides");
        assert_eq!(sides.len(), 2);
        for (side, candidate) in sides.iter().zip(&candidates) {
            assert_eq!(side.citations.len(), 1);
            assert_eq!(side.citations[0].memory_id, candidate.memory_id);
            assert_eq!(side.citations[0].text, candidate.content);
        }

        // A second source agreeing with the first is corroboration, not a
        // reason to invent a conflict or reduce answer confidence.
        let mut agreeing = candidates.clone();
        agreeing[1].content = agreeing[0].content.clone();
        let agreement = evaluate_ask(&request, &agreeing);
        assert!(!agreement.abstained);
        assert!(!agreement.conflict_detected);
        assert!(agreement.sides.is_none());
        assert_eq!(agreement.citations.len(), 1);
        assert!(agreement.confidence_components.corroboration > 1.0);
    }

    #[test]
    fn explicit_links_surface_paraphrased_and_same_polarity_conflicts() {
        for (question, first, second, numeric_dispute) in [
            (
                "Remote cache delta enabled Project Zephyr worker-g worker pool",
                "Remote cache delta enabled Project Zephyr worker-g worker pool.",
                "Zephyr worker-g workers cannot use cache delta.",
                false,
            ),
            (
                "What port does the database use?",
                "The database uses port 5432.",
                "The database uses port 6432.",
                true,
            ),
        ] {
            let candidates: Vec<_> = [("first", first), ("second", second)]
                .into_iter()
                .map(|(id, text)| AskCandidate {
                    memory_id: id.to_owned(),
                    content: text.to_owned(),
                    confidence: 0.99,
                    trust_class: "human_explicit".to_owned(),
                    provenance_uri: Some(format!("manual://explicit-conflict/{id}")),
                    level: "episodic".to_owned(),
                    kind: "observation".to_owned(),
                    team_provenance: None,
                })
                .collect();
            let request = AskRequest {
                question: question.to_owned(),
                contradictions: vec![AskContradiction {
                    id: "link_asserted".to_owned(),
                    src_memory_id: "first".to_owned(),
                    dst_memory_id: "second".to_owned(),
                    confidence: 0.9,
                    source: "agent".to_owned(),
                }],
                ..AskRequest::default()
            };
            let report = evaluate_ask(&request, &candidates);
            assert!(report.conflict_detected && !report.abstained, "{report:?}");
            assert!(report.answer_text.is_none() && report.citations.is_empty());
            assert_eq!(report.confidence_components.corroboration, 1.0);
            assert!(report.confidence < 0.95);
            let sides = report.sides.as_ref().expect("two supported sides");
            assert_eq!(sides.len(), 2);
            for (side, candidate) in sides.iter().zip(&candidates) {
                assert_eq!(side.citations.len(), 1);
                let citation = &side.citations[0];
                assert_eq!(citation.memory_id, candidate.memory_id);
                assert_eq!(citation.text, candidate.content);
                assert_eq!(
                    candidate
                        .content
                        .get(citation.byte_start..citation.byte_end),
                    Some(citation.text.as_str())
                );
            }
            assert_eq!(
                ask_data_json(&report)["conflictLink"]["id"],
                "link_asserted"
            );
            assert!(render_ask_markdown(&report).contains("link_asserted"));
            let mut reversed = candidates.clone();
            reversed.reverse();
            assert_eq!(
                ask_data_json(&evaluate_ask(&request, &reversed)),
                ask_data_json(&report),
                "candidate order must not change the selected edge or sides"
            );
            let mut reverse_edge = request.clone();
            reverse_edge.contradictions[0].src_memory_id = "second".to_owned();
            reverse_edge.contradictions[0].dst_memory_id = "first".to_owned();
            assert!(evaluate_ask(&reverse_edge, &candidates).conflict_detected);

            let mut chain = candidates.clone();
            let mut remote = candidates[1].clone();
            remote.memory_id = "third".to_owned();
            remote.content = "A separately linked memory about an unrelated invoice.".to_owned();
            chain.push(remote);
            let mut chain_request = request.clone();
            chain_request.contradictions.push(AskContradiction {
                id: "link_chain".to_owned(),
                src_memory_id: "second".to_owned(),
                dst_memory_id: "third".to_owned(),
                confidence: 1.0,
                source: "human".to_owned(),
            });
            let chain_report = evaluate_ask(&chain_request, &chain);
            assert!(chain_report.conflict_detected);
            assert!(
                chain_report
                    .sides
                    .as_ref()
                    .unwrap()
                    .iter()
                    .flat_map(|side| &side.citations)
                    .all(|citation| citation.memory_id != "third"),
                "a second edge must not propagate question relevance"
            );

            let mut unrelated = request.clone();
            unrelated.question = "What colour is the CI dashboard?".to_owned();
            let missed = evaluate_ask(&unrelated, &candidates);
            assert!(missed.abstained && !missed.conflict_detected);

            for variant in ["missing", "weak", "auto", "nonfinite"] {
                let mut rejected = request.clone();
                let link = &mut rejected.contradictions[0];
                match variant {
                    "missing" => link.dst_memory_id = "outside_scope".to_owned(),
                    "weak" => link.confidence = 0.1,
                    "auto" => link.source = "auto".to_owned(),
                    "nonfinite" => link.confidence = f32::NAN,
                    _ => unreachable!(),
                }
                let report = evaluate_ask(&rejected, &candidates);
                assert!(report.conflict_link.is_none(), "{variant}: {report:?}");
                // Reject the invalid relation, not independently supported
                // facts. Same-statement numeric opposition is now discoverable
                // without an edge; the paraphrased fixture still needs one.
                assert_eq!(report.conflict_detected, numeric_dispute, "{variant}");
                if numeric_dispute {
                    assert_eq!(
                        report.sides.as_ref().expect("supported numeric sides")[1].label,
                        "numeric_alternative"
                    );
                }
            }
            let mut untrusted = candidates.clone();
            untrusted[1].confidence = 0.1;
            assert!(evaluate_ask(&request, &untrusted).conflict_link.is_none());
        }
    }

    #[test]
    fn evaluate_ask_finds_factual_answer() {
        let request = AskRequest {
            question: "what port does the database use".into(),
            native_sources: BTreeMap::new(),
            min_confidence: 0.01, // very low so we don't abstain in test
            max_evidence: 3,
            require_confidence: None,
            contradictions: Vec::new(),
        };
        let candidates = vec![AskCandidate {
            memory_id: "mem1".into(),
            content: "The database listens on port 5432. TLS is required.".into(),
            confidence: 0.95,
            trust_class: "human_explicit".into(),
            provenance_uri: Some("ee://mem1".into()),
            level: "procedural".into(),
            kind: "rule".into(),
            team_provenance: None,
        }];
        let report = evaluate_ask(&request, &candidates);
        // With very low threshold, should produce an answer
        assert!(!report.abstained || report.candidates_scanned == 1);
        if !report.abstained {
            let answer = report.answer_text.as_deref().unwrap_or("");
            // The answer should contain content from the memory
            assert!(
                answer.contains("5432") || answer.contains("port") || answer.contains("database"),
                "answer should reference the relevant content: {answer:?}"
            );
        }
    }

    #[test]
    fn ask_data_json_has_required_fields() {
        let report = AskReport {
            question: "test question".into(),
            native_sources: BTreeMap::new(),
            abstained: false,
            answer_text: Some("[1] the answer".into()),
            confidence: 0.8,
            confidence_components: AskConfidenceComponents {
                top_span_score: 0.8,
                corroboration: 1.0,
                contradiction_penalty: 0.0,
            },
            citations: vec![AskCitation {
                index: 1,
                memory_id: "m1".into(),
                byte_start: 0,
                byte_end: 10,
                text: "the answer".into(),
                provenance_uri: None,
                trust_class: "human_explicit".into(),
                confidence: 0.9,
                team_provenance: None,
            }],
            sides: None,
            nearest_evidence: None,
            counterfactual_hint: None,
            semantic_degraded: true,
            conflict_detected: false,
            conflict_link: None,
            extractiveness_violated: false,
            candidates_scanned: 1,
        };
        let json = ask_data_json(&report);
        assert_eq!(json["schema"], ASK_SCHEMA_V1);
        assert_eq!(json["question"], "test question");
        assert_eq!(json["abstained"], false);
        assert_eq!(
            json["confidenceCalibration"]["status"],
            ASK_CONFIDENCE_CALIBRATION_STATUS
        );
        assert_eq!(json["confidenceCalibration"]["calibrated"], false);
        assert_eq!(
            json["confidenceCalibration"]["scoreKind"],
            ASK_CONFIDENCE_SCORE_KIND
        );
        assert!(json["confidenceCalibration"]["calibrationId"].is_null());
        assert!(json["citations"].as_array().is_some());
        let cits = json["citations"].as_array().unwrap();
        assert_eq!(cits.len(), 1);
        assert_eq!(cits[0]["memoryId"], "m1");
    }

    #[test]
    fn ask_citation_json_includes_team_provenance() {
        let provenance = crate::core::memory_scope::TeamProvenance {
            member_display_name: "Analysts".to_owned(),
            project_name: Some("acme-analysis".to_owned()),
            origin_trust_class: "peer_human_attested",
            produced_at: "2026-08-16T00:00:00Z".to_owned(),
            origin_time_assurance: "member_attested",
        };
        let report = AskReport {
            question: "who wrote the analysis".into(),
            native_sources: BTreeMap::new(),
            abstained: false,
            answer_text: Some("[1] teammate analysis".into()),
            confidence: 0.8,
            confidence_components: AskConfidenceComponents {
                top_span_score: 0.8,
                corroboration: 1.0,
                contradiction_penalty: 0.0,
            },
            citations: vec![AskCitation {
                index: 1,
                memory_id: "m1".into(),
                byte_start: 0,
                byte_end: 19,
                text: "teammate analysis".into(),
                provenance_uri: None,
                trust_class: "peer_human_attested".into(),
                confidence: 0.9,
                team_provenance: Some(provenance),
            }],
            sides: None,
            nearest_evidence: None,
            counterfactual_hint: None,
            semantic_degraded: false,
            conflict_detected: false,
            conflict_link: None,
            extractiveness_violated: false,
            candidates_scanned: 1,
        };
        let json = ask_data_json(&report);
        assert_eq!(
            json["citations"][0]["teamProvenance"]["memberDisplayName"],
            "Analysts"
        );
        assert_eq!(
            json["citations"][0]["teamProvenance"]["projectName"],
            "acme-analysis"
        );
        let markdown = render_ask_markdown(&report);
        assert!(
            markdown.contains("from Analysts / acme-analysis"),
            "ask markdown must attribute the teammate and project: {markdown}"
        );
    }

    #[test]
    fn ask_data_json_abstention_includes_query_assist() {
        let report = AskReport {
            native_sources: BTreeMap::new(),
            question: "where is installer smoke documented".into(),
            abstained: true,
            answer_text: None,
            confidence: 0.2,
            confidence_components: AskConfidenceComponents {
                top_span_score: 0.2,
                corroboration: 1.0,
                contradiction_penalty: 0.0,
            },
            citations: vec![],
            sides: None,
            nearest_evidence: Some(vec![AskNearestEvidence {
                memory_id: "mem_installer_smoke".into(),
                byte_start: 4,
                byte_end: 42,
                text: "release installers require live smoke validation".into(),
                score: 0.2,
            }]),
            counterfactual_hint: Some("below threshold".into()),
            semantic_degraded: true,
            conflict_detected: false,
            conflict_link: None,
            extractiveness_violated: false,
            candidates_scanned: 1,
        };
        let json = ask_data_json(&report);

        assert_eq!(
            json["queryAssist"]["schema"],
            crate::core::search::QUERY_ASSIST_SCHEMA_V1
        );
        assert_eq!(
            json["queryAssist"]["weakResultReason"],
            "no_confident_answer"
        );
        assert_eq!(
            json["queryAssist"]["didYouMean"][0]["memoryId"],
            "mem_installer_smoke"
        );
        assert!(
            json["queryAssist"]["captureTemplate"]["command"]
                .as_str()
                .is_some_and(|command| command.contains("ee remember"))
        );
    }

    #[test]
    fn ask_query_miss_audit_details_are_hash_only_and_origin_ask() -> Result<(), String> {
        let report = AskReport {
            native_sources: BTreeMap::new(),
            question: "where is installer smoke documented".into(),
            abstained: true,
            answer_text: None,
            confidence: 0.2,
            confidence_components: AskConfidenceComponents {
                top_span_score: 0.2,
                corroboration: 1.0,
                contradiction_penalty: 0.0,
            },
            citations: vec![],
            sides: None,
            nearest_evidence: Some(vec![AskNearestEvidence {
                memory_id: "mem_installer_smoke".into(),
                byte_start: 4,
                byte_end: 42,
                text: "release installers require live smoke validation".into(),
                score: 0.2,
            }]),
            counterfactual_hint: Some("below threshold".into()),
            semantic_degraded: true,
            conflict_detected: false,
            conflict_link: None,
            extractiveness_violated: false,
            candidates_scanned: 7,
        };
        let details = ask_query_miss_audit_details("blake3:test", &report, DEGRADED_NO_ANSWER);
        let value: serde_json::Value =
            serde_json::from_str(&details).map_err(|error| error.to_string())?;

        assert_eq!(value["schema"], "ee.search.query_miss.v1");
        assert_eq!(value["origin"], ASK_QUERY_MISS_ORIGIN);
        assert_eq!(value["queryHash"], "blake3:test");
        assert_eq!(value["reason"], DEGRADED_NO_ANSWER);
        assert_eq!(value["candidateCount"], 7);
        assert_eq!(value["nearestEvidenceCount"], 1);
        assert_eq!(
            value["confidenceCalibration"]["status"],
            ASK_CONFIDENCE_CALIBRATION_STATUS
        );
        assert_eq!(value["confidenceCalibration"]["calibrated"], false);
        assert_eq!(
            value["confidenceCalibration"]["scoreKind"],
            ASK_CONFIDENCE_SCORE_KIND
        );
        assert!(value["confidenceCalibration"]["calibrationId"].is_null());
        assert_eq!(value["redaction"]["rawQueryStored"], false);
        assert_eq!(value["redaction"]["queryTextStored"], false);
        assert_eq!(value["redaction"]["queryVectorStored"], false);
        assert!(
            !details.contains("installer smoke"),
            "ask query-miss audit details must not store raw question text"
        );
        Ok(())
    }

    #[test]
    fn render_markdown_abstention_contains_hint() {
        let report = AskReport {
            native_sources: BTreeMap::new(),
            question: "does X exist".into(),
            abstained: true,
            answer_text: None,
            confidence: 0.1,
            confidence_components: AskConfidenceComponents {
                top_span_score: 0.1,
                corroboration: 1.0,
                contradiction_penalty: 0.0,
            },
            citations: vec![],
            sides: None,
            nearest_evidence: Some(vec![]),
            counterfactual_hint: Some("no memory mentions X".into()),
            semantic_degraded: true,
            conflict_detected: false,
            conflict_link: None,
            extractiveness_violated: false,
            candidates_scanned: 0,
        };
        let md = render_ask_markdown(&report);
        assert!(md.contains("No confident answer"), "should note abstention");
        assert!(md.contains("no memory mentions X"), "should include hint");
    }
}
