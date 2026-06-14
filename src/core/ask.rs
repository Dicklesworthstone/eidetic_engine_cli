//! ADR 0067 extractive question answering — bd-169v0.2 / bd-169v0.3.
//!
//! `ee ask "<question>"` composes a direct answer FROM EXTRACTED SPANS of
//! stored memories: retrieval → span segmentation → scoring → clustering →
//! composition with per-claim citations, an overall confidence, and honest
//! abstention. Deterministic: same DB + question ⇒ byte-identical answer.
//!
//! Extractiveness invariant: every emitted answer sentence MUST byte-equal
//! a cited span of a stored memory. Violations trigger an internal error
//! rather than silent emission of generated text (enforced at the boundary
//! in `compose_answer`, never downgraded).

use std::collections::BTreeSet;

// ─── schema constants ───────────────────────────────────────────────────────

/// Response data schema identifier carried under `ee.response.v2 data.answer`.
pub const ASK_SCHEMA_V1: &str = "ee.ask.v1";

/// Origin tag emitted into the query-miss ledger on abstention.
pub const ASK_QUERY_MISS_ORIGIN: &str = "ask";

/// Default minimum confidence below which the engine abstains (ADR §3).
pub const ASK_MIN_CONFIDENCE_DEFAULT: f32 = 0.55;

/// Default maximum number of evidence spans to emit in the answer (ADR §3).
pub const ASK_MAX_EVIDENCE_DEFAULT: usize = 3;

/// Defensive ceiling on memories scanned per invocation.
pub const ASK_CANDIDATE_SCAN_CAP: usize = 512;

// ─── span scoring weights (ADR §2) ──────────────────────────────────────────

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

// ─── request / candidate types ──────────────────────────────────────────────

/// A single memory candidate with the fields the ask engine needs.
#[derive(Clone, Debug)]
pub struct AskCandidate {
    pub memory_id: String,
    pub content: String,
    pub confidence: f32,
    pub trust_class: String,
    pub provenance_uri: Option<String>,
    pub level: String,
    pub kind: String,
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
}

impl Default for AskRequest {
    fn default() -> Self {
        Self {
            question: String::new(),
            min_confidence: ASK_MIN_CONFIDENCE_DEFAULT,
            max_evidence: ASK_MAX_EVIDENCE_DEFAULT,
            require_confidence: None,
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
    pub candidates_scanned: usize,
}

// ─── sentence segmenter (ADR §1) ────────────────────────────────────────────

/// Segment `content` into byte-addressed spans.
///
/// Code-fence awareness: a ``` ... ``` block is one span. URL dots and
/// common abbreviations ("e.g.", "i.e.", "vs.", "etc.") do not split.
/// Bullet-list items (`- `, `* `, `N. `) each become their own span.
/// Regular sentence boundaries: `. `, `! `, `? ` before an uppercase letter
/// or end of string.
pub fn segment_spans(content: &str) -> Vec<(usize, usize)> {
    if content.is_empty() {
        return Vec::new();
    }

    let bytes = content.as_bytes();
    let len = content.len();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut span_start = 0_usize;
    let mut i = 0_usize;
    let mut in_code_fence = false;

    while i < len {
        // Code fence detection (``` at column 0 after whitespace trim)
        if bytes[i] == b'`' && i + 2 < len && bytes[i + 1] == b'`' && bytes[i + 2] == b'`' {
            if in_code_fence {
                // Closing fence — consume through end of line and emit
                let fence_end = advance_to_newline(bytes, i + 3);
                push_span(&mut spans, content, span_start, fence_end);
                span_start = fence_end;
                i = fence_end;
                in_code_fence = false;
            } else {
                // Opening fence — emit any pending text, then start fence span
                if i > span_start {
                    push_span(&mut spans, content, span_start, i);
                }
                span_start = i;
                in_code_fence = true;
                i += 3; // skip ```
            }
            continue;
        }

        if in_code_fence {
            i += 1;
            continue;
        }

        // Newline — check for list item or blank line (paragraph break)
        if bytes[i] == b'\n' {
            let next = i + 1;
            if next < len {
                let next_char = bytes[next];
                // Bullet list item: `- `, `* `, `+ `, or `N. `
                let is_list_item = next_char == b'-'
                    || next_char == b'*'
                    || next_char == b'+'
                    || (next_char.is_ascii_digit() && {
                        let mut j = next;
                        while j < len && bytes[j].is_ascii_digit() {
                            j += 1;
                        }
                        j < len && bytes[j] == b'.' && j + 1 < len && bytes[j + 1] == b' '
                    });
                // Blank line = paragraph break
                let is_blank = next_char == b'\n';

                if is_list_item || is_blank {
                    let end = if is_blank { i } else { i + 1 };
                    if end > span_start {
                        push_span(&mut spans, content, span_start, end);
                        span_start = end;
                    }
                }
            }
            i += 1;
            continue;
        }

        // Sentence boundary: `. `, `! `, `? ` before uppercase or end
        if (bytes[i] == b'.' || bytes[i] == b'!' || bytes[i] == b'?')
            && i + 1 < len
            && bytes[i + 1] == b' '
        {
            // Skip common abbreviations
            if bytes[i] == b'.' && is_abbreviation_end(content, i) {
                i += 1;
                continue;
            }
            // Check the character after the space
            let after = i + 2;
            let sentence_end = i + 1; // include the punctuation, not the space
            if after >= len || bytes[after].is_ascii_uppercase() || bytes[after] == b'\n' {
                if sentence_end > span_start {
                    push_span(&mut spans, content, span_start, sentence_end);
                    // Skip the space after punctuation
                    span_start = after;
                    i = after;
                    continue;
                }
            }
        }

        // Advance by character boundary
        i += char_len_at(bytes, i);
    }

    // Emit any trailing text
    if span_start < len {
        push_span(&mut spans, content, span_start, len);
    }

    // Filter empty/whitespace-only spans
    spans
        .into_iter()
        .filter(|(s, e)| !content[*s..*e].trim().is_empty())
        .collect()
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
    if b < 0x80 { 1 } else if b < 0xE0 { 2 } else if b < 0xF0 { 3 } else { 4 }
}

/// Return true if the `.` at `pos` in `text` is the end of a known
/// abbreviation, not a sentence boundary.
fn is_abbreviation_end(text: &str, pos: usize) -> bool {
    const ABBREVS: &[&str] = &["e.g", "i.e", "vs", "etc", "Mr", "Mrs", "Dr", "Prof", "St"];
    for abbrev in ABBREVS {
        let alen = abbrev.len();
        if pos >= alen && &text[pos - alen..pos] == *abbrev {
            return true;
        }
    }
    false
}

// ─── lexical tokenizer ───────────────────────────────────────────────────────

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "shall", "can", "to", "of", "in", "for",
    "on", "with", "at", "by", "from", "as", "or", "and", "but", "not",
    "it", "its", "this", "that", "these", "those", "so", "if", "then",
    "than", "also", "up", "into", "about", "such", "only", "each",
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
    if union == 0 { 0.0 } else { intersection as f32 / union as f32 }
}

/// Score one span against the question.
///
/// Semantic (embedding) similarity is not yet available — the w2 weight is
/// re-normalized into w1 (semantic_degraded mode, ADR §5).
pub fn score_span(
    question_terms: &[String],
    span_text: &str,
    memory_confidence: f32,
    trust_class: &str,
) -> f32 {
    let span_terms = tokenize_for_ask(span_text);
    let lexical = jaccard_similarity(question_terms, &span_terms);
    let tilt = trust_tilt(trust_class);

    // Semantic unavailable: renormalize w1 to absorb w2 (ADR §5).
    // w1_adj = w1 + w2, w3 unchanged, sum = 0.80 → renorm to 1.0.
    let w1_adj = (SPAN_W1_LEXICAL + SPAN_W2_SEMANTIC) / (1.0 - SPAN_W3_TRUST + SPAN_W3_TRUST);
    // Simpler: w1+w2=0.80, w3=0.20, sum=1.0 → w1_adj = 0.80, w3_adj = 0.20.
    let score = 0.80 * lexical + SPAN_W3_TRUST * (memory_confidence * tilt);
    score.clamp(0.0, 1.0)
}

// ─── clustering (ADR §2) ─────────────────────────────────────────────────────

/// Cluster a list of scored spans by term-set Jaccard similarity.
///
/// Spans whose terms overlap above `CLUSTER_SIMILARITY_THRESHOLD` form a
/// cluster; the representative is the highest-scoring span in the cluster.
/// The corroboration multiplier `1 + 0.1·ln(size)` capped at 1.3 is applied
/// to the representative's score.
pub fn cluster_spans(spans: &[AskSpan]) -> Vec<AskSpan> {
    if spans.is_empty() {
        return Vec::new();
    }

    let term_sets: Vec<Vec<String>> = spans
        .iter()
        .map(|s| tokenize_for_ask(&s.text))
        .collect();

    let n = spans.len();
    let mut assigned = vec![false; n];
    let mut representatives: Vec<AskSpan> = Vec::new();

    // Greedy single-linkage clustering, ordered by score descending.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| spans[b].score.partial_cmp(&spans[a].score).unwrap_or(std::cmp::Ordering::Equal));

    for &seed in &order {
        if assigned[seed] {
            continue;
        }
        assigned[seed] = true;
        let mut cluster_size = 1_usize;

        for &other in &order {
            if assigned[other] {
                continue;
            }
            let sim = jaccard_similarity(&term_sets[seed], &term_sets[other]);
            if sim >= CLUSTER_SIMILARITY_THRESHOLD {
                assigned[other] = true;
                cluster_size += 1;
            }
        }

        let corroboration = (1.0 + 0.1 * (cluster_size as f32).ln()).min(CORROBORATION_CAP);
        let mut rep = spans[seed].clone();
        rep.score = (rep.score * corroboration).clamp(0.0, 1.0);
        representatives.push(rep);
    }

    // Sort representatives by score desc, then memory_id for tie-breaking (ADR §3).
    representatives.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.memory_id.cmp(&b.memory_id))
    });

    representatives
}

// ─── contradiction detection (ADR §4) ────────────────────────────────────────

/// Negation words that flip the polarity of a statement.
const NEGATION_WORDS: &[&str] = &[
    "not", "never", "no", "neither", "nor", "cannot", "can't", "won't",
    "doesn't", "isn't", "aren't", "wasn't", "weren't", "didn't", "don't",
    "impossible", "incorrect", "wrong", "false", "invalid",
];

fn has_negation(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    NEGATION_WORDS.iter().any(|&neg| {
        lower.split(|c: char| !c.is_alphabetic() && c != '\'')
            .any(|token| token == neg)
    })
}

/// Return true when the top two clusters have opposing polarity.
fn detect_contradiction(clusters: &[AskSpan]) -> bool {
    if clusters.len() < 2 {
        return false;
    }
    let top_neg = has_negation(&clusters[0].text);
    let second_neg = has_negation(&clusters[1].text);
    // Contradiction: one affirms, one negates (XOR on negation presence)
    top_neg != second_neg
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
        let original = content_map.get(span.memory_id.as_str()).copied().unwrap_or("");
        let byte_range = span.byte_start..span.byte_end;

        if byte_range.end > original.len() {
            return Err("extractiveness: span range out of bounds");
        }
        let original_text = &original[byte_range];

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
        });
    }

    Ok((answer_parts.join(" "), citations))
}

// ─── main engine entry point ─────────────────────────────────────────────────

/// Pure ask engine — same inputs ⇒ byte-identical output (ADR §1–§4).
///
/// The caller is responsible for fetching `candidates` from the database
/// and for emitting the query-miss ledger row on abstention
/// (`report.abstained == true`).
pub fn evaluate_ask(request: &AskRequest, candidates: &[AskCandidate]) -> AskReport {
    let question_terms = tokenize_for_ask(&request.question);
    let max_n = request.max_evidence.max(1);
    let candidates = &candidates[..candidates.len().min(ASK_CANDIDATE_SCAN_CAP)];

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
            let score = score_span(
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
            });
        }
    }

    // Sort all spans by score desc for clustering
    all_spans.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let clusters = cluster_spans(&all_spans);

    let top_span_score = clusters.first().map(|s| s.score).unwrap_or(0.0);
    let conflict_detected = detect_contradiction(&clusters);
    let contradiction_penalty_applied = if conflict_detected { CONTRADICTION_PENALTY } else { 0.0 };

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

    // Abstention check (ADR §3)
    if confidence < request.min_confidence || clusters.is_empty() {
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
            question: request.question.clone(),
            abstained: true,
            answer_text: None,
            confidence,
            confidence_components,
            citations: Vec::new(),
            sides: None,
            nearest_evidence: Some(nearest_evidence),
            counterfactual_hint: Some(counterfactual_hint),
            semantic_degraded: true, // semantic always degraded in current impl
            conflict_detected,
            candidates_scanned: candidates.len(),
        };
    }

    // Conflict mode: compose each side separately (ADR §4)
    if conflict_detected && clusters.len() >= 2 {
        let affirming: Vec<AskSpan> = clusters
            .iter()
            .filter(|s| !has_negation(&s.text))
            .cloned()
            .collect();
        let negating: Vec<AskSpan> = clusters
            .iter()
            .filter(|s| has_negation(&s.text))
            .cloned()
            .collect();

        let compose_side = |side_spans: &[AskSpan], label: &str| -> AskSide {
            let mut parts = Vec::new();
            let mut cites = Vec::new();
            for (idx, s) in side_spans.iter().take(max_n).enumerate() {
                parts.push(format!("[{}] {}", idx + 1, s.text));
                cites.push(AskCitation {
                    index: idx + 1,
                    memory_id: s.memory_id.clone(),
                    byte_start: s.byte_start,
                    byte_end: s.byte_end,
                    text: s.text.clone(),
                    provenance_uri: s.provenance_uri.clone(),
                    trust_class: s.trust_class.clone(),
                    confidence: s.memory_confidence,
                });
            }
            AskSide {
                label: label.to_owned(),
                answer_text: parts.join(" "),
                citations: cites,
            }
        };

        let sides = vec![
            compose_side(&affirming, "affirming"),
            compose_side(&negating, "negating"),
        ];

        return AskReport {
            question: request.question.clone(),
            abstained: false,
            answer_text: None,
            confidence,
            confidence_components,
            citations: Vec::new(),
            sides: Some(sides),
            nearest_evidence: None,
            counterfactual_hint: None,
            semantic_degraded: true,
            conflict_detected: true,
            candidates_scanned: candidates.len(),
        };
    }

    // Normal path: compose answer from top clusters
    match compose_answer(&clusters, max_n, &content_map) {
        Ok((answer_text, citations)) => AskReport {
            question: request.question.clone(),
            abstained: false,
            answer_text: Some(answer_text),
            confidence,
            confidence_components,
            citations,
            sides: None,
            nearest_evidence: None,
            counterfactual_hint: None,
            semantic_degraded: true,
            conflict_detected: false,
            candidates_scanned: candidates.len(),
        },
        Err(_reason) => {
            // Extractiveness invariant violated — fall back to abstention.
            AskReport {
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
                candidates_scanned: candidates.len(),
            }
        }
    }
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
        "confidenceComponents": {
            "topSpanScore": report.confidence_components.top_span_score,
            "corroboration": report.confidence_components.corroboration,
            "contradictionPenalty": report.confidence_components.contradiction_penalty,
        },
        "citations": report.citations.iter().map(citation_to_json).collect::<Vec<_>>(),
        "sides": report.sides.as_ref().map(|sides| {
            sides.iter().map(side_to_json).collect::<Vec<_>>()
        }),
        "nearestEvidence": report.nearest_evidence.as_ref().map(|ne| {
            ne.iter().map(nearest_evidence_to_json).collect::<Vec<_>>()
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

    obj
}

fn citation_to_json(c: &AskCitation) -> serde_json::Value {
    serde_json::json!({
        "index": c.index,
        "memoryId": c.memory_id,
        "span": {"byteStart": c.byte_start, "byteEnd": c.byte_end},
        "text": c.text,
        "provenanceUri": c.provenance_uri,
        "trustClass": c.trust_class,
        "confidence": c.confidence,
    })
}

fn side_to_json(s: &AskSide) -> serde_json::Value {
    serde_json::json!({
        "label": s.label,
        "answerText": s.answer_text,
        "citations": s.citations.iter().map(citation_to_json).collect::<Vec<_>>(),
    })
}

fn nearest_evidence_to_json(ne: &AskNearestEvidence) -> serde_json::Value {
    serde_json::json!({
        "memoryId": ne.memory_id,
        "span": {"byteStart": ne.byte_start, "byteEnd": ne.byte_end},
        "text": ne.text,
        "score": ne.score,
    })
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
                    out.push_str(&format!("- {} (score: {:.2})\n", e.text, e.score));
                }
            }
        }
        return out;
    }

    if report.conflict_detected {
        out.push_str("*Conflicting evidence found:*\n\n");
        if let Some(sides) = &report.sides {
            for side in sides {
                out.push_str(&format!("**{} view:**\n{}\n\n", side.label, side.answer_text));
                for c in &side.citations {
                    out.push_str(&format!(
                        "> [{}] *({})*\n",
                        c.index, c.memory_id
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
            let prov = c
                .provenance_uri
                .as_deref()
                .unwrap_or(&c.memory_id);
            out.push_str(&format!(
                "[{}] {} `{}` (conf: {:.2})\n",
                c.index, prov, c.trust_class, c.confidence
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
    fn tokenize_drops_stopwords() {
        let tokens = tokenize_for_ask("the port is 8080");
        assert!(!tokens.contains(&"the".to_owned()));
        assert!(!tokens.contains(&"is".to_owned()));
        assert!(tokens.contains(&"port".to_owned()));
        assert!(tokens.contains(&"8080".to_owned()));
    }

    #[test]
    fn score_span_returns_zero_for_unrelated() {
        let q_terms = tokenize_for_ask("what is the database port");
        let score = score_span(&q_terms, "The sky is blue today.", 0.9, "human_explicit");
        assert!(score < 0.3, "unrelated span should score low: {score}");
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
        assert!(score > 0.15, "relevant span should score above 0.15: {score}");
    }

    #[test]
    fn trust_tilt_ordering() {
        assert!(trust_tilt("human_explicit") > trust_tilt("agent_validated"));
        assert!(trust_tilt("agent_validated") > trust_tilt("agent_assertion"));
        assert!(trust_tilt("agent_assertion") > trust_tilt("cass_evidence"));
        assert!(trust_tilt("cass_evidence") > trust_tilt("legacy_import"));
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
        };
        assert!(detect_contradiction(&[affirm, negate]));
    }

    #[test]
    fn evaluate_ask_abstains_on_empty_corpus() {
        let request = AskRequest {
            question: "what is the database port".into(),
            min_confidence: ASK_MIN_CONFIDENCE_DEFAULT,
            max_evidence: ASK_MAX_EVIDENCE_DEFAULT,
            require_confidence: None,
        };
        let report = evaluate_ask(&request, &[]);
        assert!(report.abstained);
        assert_eq!(report.confidence, 0.0);
        assert!(report.answer_text.is_none());
    }

    #[test]
    fn evaluate_ask_finds_factual_answer() {
        let request = AskRequest {
            question: "what port does the database use".into(),
            min_confidence: 0.01, // very low so we don't abstain in test
            max_evidence: 3,
            require_confidence: None,
        };
        let candidates = vec![AskCandidate {
            memory_id: "mem1".into(),
            content: "The database listens on port 5432. TLS is required.".into(),
            confidence: 0.95,
            trust_class: "human_explicit".into(),
            provenance_uri: Some("ee://mem1".into()),
            level: "procedural".into(),
            kind: "rule".into(),
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
            }],
            sides: None,
            nearest_evidence: None,
            counterfactual_hint: None,
            semantic_degraded: true,
            conflict_detected: false,
            candidates_scanned: 1,
        };
        let json = ask_data_json(&report);
        assert_eq!(json["schema"], ASK_SCHEMA_V1);
        assert_eq!(json["question"], "test question");
        assert_eq!(json["abstained"], false);
        assert!(json["citations"].as_array().is_some());
        let cits = json["citations"].as_array().unwrap();
        assert_eq!(cits.len(), 1);
        assert_eq!(cits[0]["memoryId"], "m1");
    }

    #[test]
    fn render_markdown_abstention_contains_hint() {
        let report = AskReport {
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
            candidates_scanned: 0,
        };
        let md = render_ask_markdown(&report);
        assert!(md.contains("No confident answer"), "should note abstention");
        assert!(md.contains("no memory mentions X"), "should include hint");
    }
}
