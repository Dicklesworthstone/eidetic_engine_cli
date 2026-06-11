//! AGENTS.md bridge (ADR 0065 §5, bd-39tzu.4).
//!
//! Three surfaces over one marker-delimited managed block:
//!
//! - `ee export agentsmd` renders the primer `rules` + `warnings` sections
//!   into a managed block delimited by `<!-- ee:agentsmd:begin ... -->` /
//!   `<!-- ee:agentsmd:end -->`. It NEVER edits outside its markers, writes
//!   `<file>.ee-backup` before any mutation of an existing file, creates
//!   files only with explicit `--create`, and refuses a hand-edited managed
//!   block (content-hash mismatch) unless `--force-managed-block` is passed.
//! - `ee import agentsmd` parses rule-like statements OUTSIDE the markers
//!   into pending curation candidates (trust class capped at
//!   `agent_assertion`, provenance `file://<path>#L<n>`); near-duplicates of
//!   existing memories become reinforce proposals with the same dedup
//!   semantics as ADR 0062 journal distillation. Parser bias is precision
//!   over recall: a missed rule costs little; a false extraction pollutes
//!   the curation queue.
//! - `ee diag agentsmd-drift` (read-only) reports stale exports, file-vs-
//!   memory contradictions (conflict-surface vocabulary), and memory rules
//!   absent from the file.
//!
//! Dry-run reports carry no wall-clock timestamps, no absolute paths, and
//! no binary version, so golden tests stay byte-identical across machines.

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::core::primer::{
    PrimerFormat, PrimerReport, PrimerSection, primer_settings_from_workspace,
    run_primer_with_persistence,
};
use crate::curate::{CandidateSource, CandidateStatus, CandidateType};
use crate::db::{
    CreateAuditInput, CreateCurationCandidateInput, CreateEvidenceSpanInput, CreateSessionInput,
    DbConnection, StoredMemory, audit_actions, generate_audit_id,
};
use crate::models::{CandidateId, DomainError};
use crate::search::HashEmbedder;
use crate::search::simhash::{cosine_similarity, hamming_distance, simhash_128};

/// `ee export agentsmd` payload schema id.
pub const AGENTSMD_EXPORT_SCHEMA_V1: &str = "ee.agentsmd.export.v1";
/// `ee import agentsmd` payload schema id.
pub const AGENTSMD_IMPORT_SCHEMA_V1: &str = "ee.agentsmd.import.v1";
/// `ee diag agentsmd-drift` payload schema id.
pub const AGENTSMD_DRIFT_SCHEMA_V1: &str = "ee.agentsmd.drift.v1";

/// Target file absent and `--create` not passed (ADR 0065 §6, info).
pub const AGENTSMD_FILE_MISSING_CODE: &str = "agentsmd_file_missing";
/// File exists but has no managed block — import-only file or first export
/// (ADR 0065 §6, info).
pub const AGENTSMD_MARKERS_MISSING_CODE: &str = "agentsmd_markers_missing";
/// Managed block content hash mismatch: the block was hand-edited; export
/// refuses without `--force-managed-block` (ADR 0065 §6, warning).
pub const AGENTSMD_UNMANAGED_EDIT_DETECTED_CODE: &str = "agentsmd_unmanaged_edit_detected";

/// Audit `details` schema for `agentsmd.import` rows.
const AGENTSMD_IMPORT_AUDIT_SCHEMA_V1: &str = "ee.audit.agentsmd_import.v1";
/// Synthetic per-workspace session that owns import evidence spans
/// (`evidence_spans.session_id` is NOT NULL); mirrors the
/// `ee-journal-distill` session pattern.
const AGENTSMD_IMPORT_SESSION_KEY: &str = "ee-agentsmd-import";
/// Metadata schema for evidence spans minted by `import agentsmd --apply`.
const AGENTSMD_IMPORT_EVIDENCE_SCHEMA_V1: &str = "ee.agentsmd.import_evidence.v1";
/// Default bridge target relative to the workspace root.
pub const AGENTSMD_DEFAULT_FILE: &str = "AGENTS.md";
/// Backup sibling written before any mutation of an existing file (RULE 1).
pub const AGENTSMD_BACKUP_SUFFIX: &str = ".ee-backup";
/// Managed-block begin marker prefix; attributes follow as `key=value`.
const MARKER_BEGIN_PREFIX: &str = "<!-- ee:agentsmd:begin";
/// Managed-block end marker (exact line, modulo surrounding whitespace).
const MARKER_END: &str = "<!-- ee:agentsmd:end -->";

/// Most recent live memories scanned for dedup neighbor discovery (mirrors
/// the ADR 0062 §6 step 4 journal-distill machinery until the shared
/// extraction lands).
const AGENTSMD_DEDUP_SCAN_LIMIT: usize = 256;
/// SimHash candidate gate for dedup neighbor discovery.
const AGENTSMD_DEDUP_HAMMING_K: u32 = 32;
/// Maximum gated candidates ranked by cosine similarity.
const AGENTSMD_DEDUP_CANDIDATE_LIMIT: usize = 16;
/// Statements shorter than this are skipped (precision over recall).
const AGENTSMD_RULE_MIN_CHARS: usize = 20;
/// Statements longer than this are skipped (prose, not a rule).
const AGENTSMD_RULE_MAX_CHARS: usize = 400;
/// Similarity gate for file-vs-memory contradiction pairing. Lower than the
/// duplicate threshold on purpose: a contradiction is about the same topic
/// with opposite polarity, not a near-duplicate.
const AGENTSMD_CONTRADICTION_SIMILARITY: f32 = 0.55;
/// Memories below this confidence never produce contradiction findings
/// ("high-confidence memory says not-X", ADR 0065 §5).
const AGENTSMD_CONTRADICTION_MIN_CONFIDENCE: f32 = 0.7;
/// Flat proposal confidence for imported singleton statements; matches the
/// `agent_assertion` trust-class baseline.
const AGENTSMD_IMPORT_CONFIDENCE: f32 = 0.5;

/// One `degraded[]` entry emitted by the bridge surfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentsmdDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

impl AgentsmdDegradation {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

fn file_missing_degradation(display_path: &str, create_hint: bool) -> AgentsmdDegradation {
    let repair = if create_hint {
        Some(format!(
            "ee export agentsmd --workspace . --file {display_path} --create"
        ))
    } else {
        None
    };
    AgentsmdDegradation {
        code: AGENTSMD_FILE_MISSING_CODE,
        severity: "info",
        message: format!(
            "Bridge target {display_path} does not exist; nothing to read. Pass --create to \
             materialize it with a fresh managed block."
        ),
        repair,
    }
}

fn markers_missing_degradation(display_path: &str) -> AgentsmdDegradation {
    AgentsmdDegradation {
        code: AGENTSMD_MARKERS_MISSING_CODE,
        severity: "info",
        message: format!(
            "{display_path} has no ee:agentsmd managed block yet (import-only file or first \
             export); the bridge only ever writes between its own markers."
        ),
        repair: None,
    }
}

fn unmanaged_edit_degradation(display_path: &str) -> AgentsmdDegradation {
    AgentsmdDegradation {
        code: AGENTSMD_UNMANAGED_EDIT_DETECTED_CODE,
        severity: "warning",
        message: format!(
            "The managed block in {display_path} was hand-edited since the last export \
             (content hash mismatch); export refuses to overwrite it without \
             --force-managed-block. The hand edit is preserved in {display_path}{AGENTSMD_BACKUP_SUFFIX} \
             when forced."
        ),
        repair: Some(
            "ee export agentsmd --workspace . --dry-run  # review, then --force-managed-block"
                .to_owned(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Managed block machinery
// ---------------------------------------------------------------------------

/// A located managed block inside the bridge target file. Line indexes are
/// zero-based and inclusive of the marker lines themselves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedBlock {
    pub begin_index: usize,
    pub end_index: usize,
    /// `generation=<N>` attribute on the begin marker, when parseable.
    pub generation: Option<i64>,
    /// `hash=blake3:<hex>` attribute on the begin marker, when present.
    pub recorded_hash: Option<String>,
    /// Lines strictly between the markers, joined with `\n`.
    pub body: String,
}

/// Outcome of scanning a file for the managed block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManagedBlockScan {
    Missing,
    Found(ManagedBlock),
}

/// Scan file content for the ee:agentsmd managed block. Returns an error
/// description for structurally broken markers (unterminated, repeated, or
/// out of order) so callers refuse instead of guessing at boundaries.
pub fn scan_managed_block(content: &str) -> Result<ManagedBlockScan, String> {
    let mut begin: Option<(usize, Option<i64>, Option<String>)> = None;
    let mut found: Option<ManagedBlock> = None;
    let lines: Vec<&str> = content.lines().collect();
    for (index, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim();
        if line.starts_with(MARKER_BEGIN_PREFIX) {
            if begin.is_some() {
                return Err(format!(
                    "nested ee:agentsmd begin marker at line {}",
                    index + 1
                ));
            }
            if found.is_some() {
                return Err(format!(
                    "second ee:agentsmd managed block at line {}",
                    index + 1
                ));
            }
            if !line.ends_with("-->") {
                return Err(format!(
                    "unterminated ee:agentsmd begin marker at line {}",
                    index + 1
                ));
            }
            let attributes = line
                .strip_prefix(MARKER_BEGIN_PREFIX)
                .unwrap_or_default()
                .trim_end_matches("-->")
                .trim();
            let mut generation = None;
            let mut recorded_hash = None;
            for attribute in attributes.split_whitespace() {
                if let Some(value) = attribute.strip_prefix("generation=") {
                    generation = value.parse::<i64>().ok();
                } else if let Some(value) = attribute.strip_prefix("hash=") {
                    recorded_hash = Some(value.to_owned());
                }
            }
            begin = Some((index, generation, recorded_hash));
        } else if line == MARKER_END {
            let Some((begin_index, generation, recorded_hash)) = begin.take() else {
                return Err(format!(
                    "ee:agentsmd end marker without begin at line {}",
                    index + 1
                ));
            };
            let body = lines[begin_index + 1..index].join("\n");
            found = Some(ManagedBlock {
                begin_index,
                end_index: index,
                generation,
                recorded_hash,
                body,
            });
        }
    }
    if let Some((begin_index, _, _)) = begin {
        return Err(format!(
            "ee:agentsmd begin marker at line {} has no end marker",
            begin_index + 1
        ));
    }
    Ok(found.map_or(ManagedBlockScan::Missing, ManagedBlockScan::Found))
}

/// Content hash over the managed-block body (the lines strictly between the
/// markers). Recorded on the begin marker; a mismatch on a later export
/// means the block was hand-edited. Canonicalized over the line content
/// (one trailing newline stripped) so the rendered form and the
/// scanned-back form hash identically.
#[must_use]
pub fn managed_block_body_hash(body: &str) -> String {
    let canonical = body.strip_suffix('\n').unwrap_or(body);
    format!(
        "blake3:{}",
        blake3::hash(canonical.as_bytes())
            .to_hex()
            .chars()
            .take(16)
            .collect::<String>()
    )
}

/// Render the managed-block body from the primer `rules` + `warnings`
/// sections (markdown-form lines already carry the ADR 0065 §4 short
/// provenance refs). Deterministic: unchanged memory renders byte-identical
/// output.
#[must_use]
pub fn render_managed_body(sections: &[PrimerSection]) -> String {
    let mut body = String::new();
    body.push_str(
        "<!-- generated by `ee export agentsmd`; hand-edit OUTSIDE the ee markers only -->\n",
    );
    for section in sections {
        let heading = match section.name.as_str() {
            "rules" => "## Workspace rules (ee memory)",
            "warnings" => "## Workspace warnings (ee memory)",
            _ => continue,
        };
        if section.items.is_empty() {
            continue;
        }
        body.push('\n');
        body.push_str(heading);
        body.push('\n');
        body.push('\n');
        for item in &section.items {
            body.push_str("- ");
            body.push_str(&item.line);
            body.push('\n');
        }
    }
    body
}

/// Compose the full managed block: begin marker (generation + body hash),
/// body, end marker. No trailing newline; callers splice it into file
/// content as a line sequence.
#[must_use]
pub fn render_managed_block(body: &str, db_generation: i64) -> String {
    format!(
        "{MARKER_BEGIN_PREFIX} generation={db_generation} hash={} -->\n{body}{MARKER_END}",
        managed_block_body_hash(body),
    )
}

/// Minimal deterministic preview for `--dry-run`: the removed block lines
/// prefixed `-` and the added block lines prefixed `+`. The bridge replaces
/// the whole block, so a positional line diff would only obscure that.
#[must_use]
pub fn render_block_diff(old_block: &str, new_block: &str) -> String {
    let mut diff = String::new();
    for line in old_block.lines() {
        diff.push_str("- ");
        diff.push_str(line);
        diff.push('\n');
    }
    for line in new_block.lines() {
        diff.push_str("+ ");
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

fn malformed_markers_error(display_path: &str, reason: &str) -> DomainError {
    DomainError::Usage {
        message: format!(
            "Refusing to touch {display_path}: malformed ee:agentsmd markers ({reason}). The \
             bridge only operates on a single well-formed begin/end marker pair."
        ),
        repair: Some(format!(
            "Repair or remove the ee:agentsmd marker lines in {display_path}, then re-run."
        )),
    }
}

// ---------------------------------------------------------------------------
// Rule-statement parser (shared by import and drift)
// ---------------------------------------------------------------------------

/// Statement polarity, used for contradiction pairing: `ALWAYS X` vs
/// `NEVER X` style conflicts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RulePolarity {
    Positive,
    Negative,
}

impl RulePolarity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
        }
    }
}

/// One extracted rule-like statement with its 1-based source line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedStatement {
    pub line_number: usize,
    pub text: String,
    /// Proposed memory kind: `rule` for hard modality, `convention` for
    /// soft preference cues.
    pub kind: &'static str,
    pub polarity: RulePolarity,
    /// The modality token that admitted the statement (for explainability).
    pub modality: &'static str,
}

fn strip_bullet_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    let mut chars = trimmed.char_indices();
    let mut digits = 0_usize;
    for (index, character) in chars.by_ref() {
        if character.is_ascii_digit() {
            digits = index + 1;
        } else {
            break;
        }
    }
    if digits > 0 {
        if let Some(rest) = trimmed.get(digits..) {
            if let Some(rest) = rest.strip_prefix(". ") {
                return Some(rest);
            }
        }
    }
    None
}

/// Uppercase hard-modality scan over whitespace tokens (punctuation
/// trimmed): `MUST NOT`, `MUST`, `NEVER`, `ALWAYS`, `DO NOT`, `DON'T`.
fn uppercase_modality(text: &str) -> Option<(&'static str, RulePolarity, &'static str)> {
    let tokens: Vec<&str> = text
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’'))
        .collect();
    for (index, token) in tokens.iter().enumerate() {
        let next = tokens.get(index + 1).copied().unwrap_or_default();
        match *token {
            "MUST" if next == "NOT" => return Some(("rule", RulePolarity::Negative, "MUST NOT")),
            "MUST" => return Some(("rule", RulePolarity::Positive, "MUST")),
            "NEVER" => return Some(("rule", RulePolarity::Negative, "NEVER")),
            "ALWAYS" => return Some(("rule", RulePolarity::Positive, "ALWAYS")),
            "DO" if next == "NOT" => return Some(("rule", RulePolarity::Negative, "DO NOT")),
            "DON'T" | "DON’T" => return Some(("rule", RulePolarity::Negative, "DON'T")),
            _ => {}
        }
    }
    None
}

/// Leading-cue scan for bullet statements: sentence-initial imperatives that
/// are reliable rule signals even in lowercase prose.
fn leading_cue_modality(text: &str) -> Option<(&'static str, RulePolarity, &'static str)> {
    const CUES: &[(&str, &str, RulePolarity, &str)] = &[
        ("Never ", "rule", RulePolarity::Negative, "Never"),
        ("Always ", "rule", RulePolarity::Positive, "Always"),
        ("Do not ", "rule", RulePolarity::Negative, "Do not"),
        ("Don't ", "rule", RulePolarity::Negative, "Don't"),
        ("Don’t ", "rule", RulePolarity::Negative, "Don't"),
        ("Avoid ", "convention", RulePolarity::Negative, "Avoid"),
        ("Prefer ", "convention", RulePolarity::Positive, "Prefer"),
    ];
    for (cue, kind, polarity, modality) in CUES {
        if text.starts_with(cue) {
            return Some((kind, *polarity, modality));
        }
    }
    None
}

/// Classify one candidate statement. `from_bullet` widens the cue set:
/// non-bullet paragraph lines only qualify through uppercase hard modality
/// (precision over recall, ADR 0065 §5).
#[must_use]
pub fn classify_statement(
    text: &str,
    from_bullet: bool,
) -> Option<(&'static str, RulePolarity, &'static str)> {
    let length = text.chars().count();
    if !(AGENTSMD_RULE_MIN_CHARS..=AGENTSMD_RULE_MAX_CHARS).contains(&length) {
        return None;
    }
    if let Some(classified) = uppercase_modality(text) {
        return Some(classified);
    }
    if from_bullet {
        return leading_cue_modality(text);
    }
    None
}

/// Extract rule-like statements from markdown content, skipping the managed
/// block (`exclude`, inclusive zero-based marker line range), fenced code,
/// headings, HTML comments, tables, and blockquotes.
#[must_use]
pub fn parse_rule_statements(
    content: &str,
    exclude: Option<(usize, usize)>,
) -> Vec<ParsedStatement> {
    let mut statements = Vec::new();
    let mut in_fence = false;
    for (index, raw_line) in content.lines().enumerate() {
        if let Some((begin, end)) = exclude {
            if index >= begin && index <= end {
                continue;
            }
        }
        let trimmed = raw_line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence
            || trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("<!--")
            || trimmed.starts_with('|')
            || trimmed.starts_with('>')
        {
            continue;
        }
        let (text, from_bullet) = strip_bullet_prefix(raw_line)
            .map_or((trimmed, false), |stripped| (stripped.trim(), true));
        let text = text
            .trim_start_matches("**")
            .trim_end_matches("**")
            .trim()
            .to_owned();
        let Some((kind, polarity, modality)) = classify_statement(&text, from_bullet) else {
            continue;
        };
        statements.push(ParsedStatement {
            line_number: index + 1,
            text,
            kind,
            polarity,
            modality,
        });
    }
    statements
}

// ---------------------------------------------------------------------------
// Shared dedup neighbor machinery (mirror of ADR 0062 §6 step 4)
// ---------------------------------------------------------------------------

/// Top near-duplicate neighbor for a statement. Mirrors the journal-distill
/// remember-time neighbor machinery (SimHash gate over the most recent live
/// memories, cosine ranking, deterministic tie-break); kept local so the
/// in-flight journal module stays untouched — unify once both land.
fn top_neighbor(memories: &[StoredMemory], content: &str) -> Option<(String, f32)> {
    if memories.is_empty() {
        return None;
    }
    let window_start = memories.len().saturating_sub(AGENTSMD_DEDUP_SCAN_LIMIT);
    let query_fingerprint = simhash_128(content);
    let mut gated: Vec<(u32, &StoredMemory)> = memories[window_start..]
        .iter()
        .filter_map(|memory| {
            let distance = hamming_distance(query_fingerprint, simhash_128(&memory.content));
            (distance <= AGENTSMD_DEDUP_HAMMING_K).then_some((distance, memory))
        })
        .collect();
    gated.sort_by(|(left_distance, left), (right_distance, right)| {
        left_distance
            .cmp(right_distance)
            .then_with(|| left.id.cmp(&right.id))
    });
    gated.truncate(AGENTSMD_DEDUP_CANDIDATE_LIMIT);

    let embedder = HashEmbedder::default_256();
    let query_embedding = embedder.embed_sync(content);
    let mut top: Option<(String, f32, u32)> = None;
    for (hamming, memory) in gated {
        let candidate_embedding = embedder.embed_sync(&memory.content);
        let Some(similarity) = cosine_similarity(&query_embedding, &candidate_embedding) else {
            continue;
        };
        let better = match &top {
            None => true,
            Some((current_id, current_similarity, current_hamming)) => {
                match similarity.partial_cmp(current_similarity) {
                    Some(std::cmp::Ordering::Greater) => true,
                    Some(std::cmp::Ordering::Equal) => {
                        (hamming, memory.id.as_str()) < (*current_hamming, current_id.as_str())
                    }
                    _ => false,
                }
            }
        };
        if better {
            top = Some((memory.id.clone(), similarity, hamming));
        }
    }
    top.map(|(memory_id, similarity, _)| (memory_id, similarity))
}

/// `[curation] duplicate_similarity` with the remember-time default (same
/// threshold the distill pipeline uses).
fn duplicate_similarity_threshold(workspace_path: &Path) -> f32 {
    crate::config::workspace_config(workspace_path)
        .and_then(|config| config.curation.duplicate_similarity)
        .map_or(
            crate::core::memory::REMEMBER_DEFAULT_DUPLICATE_SIMILARITY,
            |value| value as f32,
        )
}

/// Deterministic blake3-derived id with a stable prefix; mirrors the
/// distill id construction so bridge candidate ids look like every other
/// curation candidate id.
fn deterministic_agentsmd_id(prefix: &str, parts: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    let hash = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    let candidate = CandidateId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string();
    format!("{prefix}{}", candidate.trim_start_matches("cand_"))
}

fn storage_error(context: &str, error: impl std::fmt::Display) -> DomainError {
    DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some("ee doctor".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Options for `ee export agentsmd`.
#[derive(Clone, Debug, Default)]
pub struct AgentsmdExportOptions {
    /// Target file; relative paths resolve against the workspace root.
    pub file: Option<PathBuf>,
    /// Primer budget override (`--tokens`).
    pub tokens: Option<u32>,
    /// Print the would-be diff and write nothing.
    pub dry_run: bool,
    /// Create the file when absent.
    pub create: bool,
    /// Overwrite a hand-edited managed block (the edit lands in the backup).
    pub force_managed_block: bool,
}

/// Result of one `ee export agentsmd` run (`ee.agentsmd.export.v1`).
#[derive(Clone, Debug, PartialEq)]
pub struct AgentsmdExportReport {
    /// `ok`, `file_missing`, or `refused_unmanaged_edit`.
    pub status: &'static str,
    pub workspace_id: String,
    /// Workspace-relative display path.
    pub file: String,
    pub db_generation: i64,
    pub dry_run: bool,
    /// True when the run materialized a new file (`--create`).
    pub created: bool,
    /// True when the managed block differs from what is (or would be) on
    /// disk; a no-op re-export reports `false`.
    pub changed: bool,
    /// Backup sibling written before mutation (`null` for dry runs,
    /// no-ops, and fresh creates).
    pub backup_path: Option<String>,
    /// Content hash of the rendered managed-block body.
    pub block_hash: String,
    pub rules_count: usize,
    pub warnings_count: usize,
    /// Primer redaction skip count carried through for honesty.
    pub redaction_skipped: u32,
    /// Dry-run block replacement preview; `null` otherwise.
    pub diff: Option<String>,
    pub degraded: Vec<AgentsmdDegradation>,
}

impl AgentsmdExportReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": AGENTSMD_EXPORT_SCHEMA_V1,
            "command": "export agentsmd",
            "status": self.status,
            "workspaceId": self.workspace_id,
            "file": self.file,
            "dbGeneration": self.db_generation,
            "dryRun": self.dry_run,
            "created": self.created,
            "changed": self.changed,
            "backupPath": self.backup_path,
            "blockHash": self.block_hash,
            "rulesCount": self.rules_count,
            "warningsCount": self.warnings_count,
            "redactionSkipped": self.redaction_skipped,
            "diff": self.diff,
            "degraded": self.degraded.iter().map(AgentsmdDegradation::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = format!(
            "agentsmd export — {} (generation {})\nstatus: {}{}{}\nrules: {}, warnings: {}, redaction skipped: {}\n",
            self.file,
            self.db_generation,
            self.status,
            if self.dry_run { ", dry run" } else { "" },
            if self.created {
                ", created"
            } else if self.changed {
                ", changed"
            } else {
                ", unchanged"
            },
            self.rules_count,
            self.warnings_count,
            self.redaction_skipped,
        );
        if let Some(backup) = &self.backup_path {
            out.push_str(&format!("backup: {backup}\n"));
        }
        for entry in &self.degraded {
            out.push_str(&format!("degraded: {} ({})\n", entry.code, entry.severity));
        }
        if let Some(diff) = &self.diff {
            out.push_str("--- managed block diff ---\n");
            out.push_str(diff);
        }
        out
    }
}

fn resolve_bridge_file(workspace_path: &Path, file: Option<&Path>) -> (PathBuf, String) {
    let absolute = file.map_or_else(
        || workspace_path.join(AGENTSMD_DEFAULT_FILE),
        |file| {
            if file.is_absolute() {
                file.to_path_buf()
            } else {
                workspace_path.join(file)
            }
        },
    );
    let display = absolute
        .strip_prefix(workspace_path)
        .unwrap_or(&absolute)
        .display()
        .to_string();
    (absolute, display)
}

fn read_bridge_file(path: &Path, display_path: &str) -> Result<Option<String>, DomainError> {
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|error| storage_error(&format!("Failed to read {display_path}"), error))
}

fn write_bridge_file(path: &Path, content: &str, display_path: &str) -> Result<(), DomainError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            storage_error(&format!("Failed to create parent of {display_path}"), error)
        })?;
    }
    std::fs::write(path, content)
        .map_err(|error| storage_error(&format!("Failed to write {display_path}"), error))
}

fn assemble_bridge_primer(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &Path,
    tokens: Option<u32>,
) -> Result<PrimerReport, DomainError> {
    // The block is markdown, so the primer always renders markdown-form
    // lines (with the §4 short provenance refs) regardless of the CLI
    // output format. Read-only: never warms the primer cache.
    let settings = primer_settings_from_workspace(workspace_path, PrimerFormat::Markdown, tokens);
    run_primer_with_persistence(connection, workspace_id, &settings, false, false)
        .map_err(|error| storage_error("Failed to assemble primer for agentsmd export", error))
}

/// Execute `ee export agentsmd` (ADR 0065 §5 export contract).
pub fn run_agentsmd_export(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &Path,
    options: &AgentsmdExportOptions,
) -> Result<AgentsmdExportReport, DomainError> {
    let primer = assemble_bridge_primer(connection, workspace_id, workspace_path, options.tokens)?;
    let body = render_managed_body(&primer.sections);
    let block = render_managed_block(&body, primer.db_generation);
    let block_hash = managed_block_body_hash(&body);
    let section_count = |name: &str| {
        primer
            .sections
            .iter()
            .find(|section| section.name == name)
            .map_or(0, |section| section.items.len())
    };
    let (path, display_path) = resolve_bridge_file(workspace_path, options.file.as_deref());

    let mut report = AgentsmdExportReport {
        status: "ok",
        workspace_id: workspace_id.to_owned(),
        file: display_path.clone(),
        db_generation: primer.db_generation,
        dry_run: options.dry_run,
        created: false,
        changed: false,
        backup_path: None,
        block_hash,
        rules_count: section_count("rules"),
        warnings_count: section_count("warnings"),
        redaction_skipped: primer.meta.skipped.redaction,
        diff: None,
        degraded: Vec::new(),
    };

    let Some(existing) = read_bridge_file(&path, &display_path)? else {
        if !options.create {
            report.status = "file_missing";
            report
                .degraded
                .push(file_missing_degradation(&display_path, true));
            return Ok(report);
        }
        report.created = true;
        report.changed = true;
        if options.dry_run {
            report.diff = Some(render_block_diff("", &block));
            return Ok(report);
        }
        write_bridge_file(&path, &format!("{block}\n"), &display_path)?;
        return Ok(report);
    };

    let scan = scan_managed_block(&existing)
        .map_err(|reason| malformed_markers_error(&display_path, &reason))?;
    let lines: Vec<&str> = existing.lines().collect();
    let (old_block, new_content) = match &scan {
        ManagedBlockScan::Missing => {
            // First export into an existing file: append the block after the
            // current content, never touching what is already there.
            report
                .degraded
                .push(markers_missing_degradation(&display_path));
            let mut content = existing.clone();
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&block);
            content.push('\n');
            (String::new(), content)
        }
        ManagedBlockScan::Found(found) => {
            let actual_hash = managed_block_body_hash(&found.body);
            let hand_edited = found.recorded_hash.as_deref() != Some(actual_hash.as_str());
            if hand_edited && !options.force_managed_block {
                report.status = "refused_unmanaged_edit";
                report
                    .degraded
                    .push(unmanaged_edit_degradation(&display_path));
                return Ok(report);
            }
            let old_block = lines[found.begin_index..=found.end_index].join("\n");
            let mut content_lines: Vec<&str> = Vec::with_capacity(lines.len());
            content_lines.extend_from_slice(&lines[..found.begin_index]);
            let mut content = if content_lines.is_empty() {
                String::new()
            } else {
                content_lines.join("\n") + "\n"
            };
            content.push_str(&block);
            content.push('\n');
            let tail = &lines[found.end_index + 1..];
            if !tail.is_empty() {
                content.push_str(&tail.join("\n"));
                content.push('\n');
            }
            (old_block, content)
        }
    };

    report.changed = new_content != existing;
    if options.dry_run {
        if report.changed {
            report.diff = Some(render_block_diff(&old_block, &block));
        }
        return Ok(report);
    }
    if !report.changed {
        return Ok(report);
    }

    // RULE 1: the pre-mutation content (including any forced-over hand
    // edit) lands in the backup sibling before the file changes.
    let backup_path = PathBuf::from(format!("{}{AGENTSMD_BACKUP_SUFFIX}", path.display()));
    let backup_display = format!("{display_path}{AGENTSMD_BACKUP_SUFFIX}");
    write_bridge_file(&backup_path, &existing, &backup_display)?;
    report.backup_path = Some(backup_display);
    write_bridge_file(&path, &new_content, &display_path)?;
    Ok(report)
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Options for `ee import agentsmd`.
#[derive(Clone, Debug, Default)]
pub struct AgentsmdImportOptions {
    /// Source file; relative paths resolve against the workspace root.
    pub file: Option<PathBuf>,
    /// Write pending candidates + audit rows; the default is a dry run
    /// that writes NOTHING.
    pub apply: bool,
}

/// One import proposal (`ee.agentsmd.import.v1` `proposals[]`).
#[derive(Clone, Debug, PartialEq)]
pub struct AgentsmdImportProposal {
    pub proposal_id: String,
    /// `create_candidate` or `reinforce_existing`.
    pub action: &'static str,
    pub target_memory_id: Option<String>,
    /// Proposed memory kind (`rule` or `convention`); level is always
    /// `procedural`.
    pub kind: &'static str,
    pub content_draft: String,
    /// `file://<workspace-relative-path>#L<n>`.
    pub evidence: Vec<String>,
    pub line_number: usize,
    pub modality: &'static str,
    pub dedup_nearest_memory_id: Option<String>,
    pub dedup_similarity: Option<f32>,
}

impl AgentsmdImportProposal {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "proposalId": &self.proposal_id,
            "action": self.action,
            "targetMemoryId": &self.target_memory_id,
            "level": "procedural",
            "kind": self.kind,
            "contentDraft": &self.content_draft,
            "evidence": &self.evidence,
            "lineNumber": self.line_number,
            "modality": self.modality,
            "dedup": {
                "nearestMemoryId": &self.dedup_nearest_memory_id,
                "similarity": &self.dedup_similarity,
            },
        })
    }
}

/// One abstention (`ee.agentsmd.import.v1` `abstentions[]`): a statement
/// whose deterministic candidate already exists from an earlier import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentsmdImportAbstention {
    pub line_number: usize,
    pub text: String,
    pub reason: &'static str,
}

impl AgentsmdImportAbstention {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "lineNumber": self.line_number,
            "text": &self.text,
            "reason": self.reason,
        })
    }
}

/// Durable write summary for one `--apply` run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentsmdImportApplied {
    pub candidate_ids: Vec<String>,
    pub audit_ids: Vec<String>,
}

/// Result of one `ee import agentsmd` run (`ee.agentsmd.import.v1`).
#[derive(Clone, Debug, PartialEq)]
pub struct AgentsmdImportReport {
    /// `ok` or `file_missing`.
    pub status: &'static str,
    pub workspace_id: String,
    pub file: String,
    pub dry_run: bool,
    /// Total lines in the scanned file.
    pub scanned_lines: usize,
    /// True when a managed block was present and excluded from parsing.
    pub managed_block_excluded: bool,
    pub proposals: Vec<AgentsmdImportProposal>,
    pub abstentions: Vec<AgentsmdImportAbstention>,
    pub applied: Option<AgentsmdImportApplied>,
    pub degraded: Vec<AgentsmdDegradation>,
}

impl AgentsmdImportReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": AGENTSMD_IMPORT_SCHEMA_V1,
            "command": "import agentsmd",
            "status": self.status,
            "workspaceId": self.workspace_id,
            "file": self.file,
            "dryRun": self.dry_run,
            "scannedLines": self.scanned_lines,
            "managedBlockExcluded": self.managed_block_excluded,
            "proposals": self.proposals.iter().map(AgentsmdImportProposal::data_json).collect::<Vec<_>>(),
            "abstentions": self.abstentions.iter().map(AgentsmdImportAbstention::data_json).collect::<Vec<_>>(),
            "applied": self.applied.as_ref().map(|applied| serde_json::json!({
                "candidateIds": &applied.candidate_ids,
                "auditIds": &applied.audit_ids,
            })),
            "degraded": self.degraded.iter().map(AgentsmdDegradation::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let creates = self
            .proposals
            .iter()
            .filter(|proposal| proposal.action == "create_candidate")
            .count();
        let mut out = format!(
            "agentsmd import — {}{}\nstatus: {}, scanned {} lines{}\nproposals: {} (create {}, reinforce {}), abstentions: {}\n",
            self.file,
            if self.dry_run { " (dry run)" } else { "" },
            self.status,
            self.scanned_lines,
            if self.managed_block_excluded {
                ", managed block excluded"
            } else {
                ""
            },
            self.proposals.len(),
            creates,
            self.proposals.len() - creates,
            self.abstentions.len(),
        );
        for proposal in &self.proposals {
            out.push_str(&format!(
                "- L{} {} {}: {}\n",
                proposal.line_number, proposal.kind, proposal.action, proposal.content_draft
            ));
        }
        if let Some(applied) = &self.applied {
            out.push_str(&format!(
                "applied: {} candidates, {} audit rows\n",
                applied.candidate_ids.len(),
                applied.audit_ids.len()
            ));
        }
        for entry in &self.degraded {
            out.push_str(&format!("degraded: {} ({})\n", entry.code, entry.severity));
        }
        out
    }
}

fn import_candidate_id(
    workspace_id: &str,
    action: &str,
    kind: &str,
    display_path: &str,
    text: &str,
) -> String {
    deterministic_agentsmd_id(
        "curate_",
        &[
            workspace_id,
            "agentsmd_import_candidate",
            action,
            kind,
            display_path,
            text,
        ],
    )
}

/// Execute `ee import agentsmd` (ADR 0065 §5 import contract).
pub fn run_agentsmd_import(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &Path,
    options: &AgentsmdImportOptions,
) -> Result<AgentsmdImportReport, DomainError> {
    let (path, display_path) = resolve_bridge_file(workspace_path, options.file.as_deref());
    let mut report = AgentsmdImportReport {
        status: "ok",
        workspace_id: workspace_id.to_owned(),
        file: display_path.clone(),
        dry_run: !options.apply,
        scanned_lines: 0,
        managed_block_excluded: false,
        proposals: Vec::new(),
        abstentions: Vec::new(),
        applied: None,
        degraded: Vec::new(),
    };

    let Some(content) = read_bridge_file(&path, &display_path)? else {
        report.status = "file_missing";
        report
            .degraded
            .push(file_missing_degradation(&display_path, false));
        return Ok(report);
    };
    report.scanned_lines = content.lines().count();

    let exclude = match scan_managed_block(&content)
        .map_err(|reason| malformed_markers_error(&display_path, &reason))?
    {
        ManagedBlockScan::Found(block) => {
            report.managed_block_excluded = true;
            Some((block.begin_index, block.end_index))
        }
        ManagedBlockScan::Missing => {
            report
                .degraded
                .push(markers_missing_degradation(&display_path));
            None
        }
    };

    let statements = parse_rule_statements(&content, exclude);
    if statements.is_empty() {
        return Ok(report);
    }

    let duplicate_threshold = duplicate_similarity_threshold(workspace_path);
    let memories = connection
        .list_memories(workspace_id, None, false)
        .map_err(|error| storage_error("Failed to list memories for agentsmd dedup", error))?;

    for statement in statements {
        let neighbor = top_neighbor(&memories, &statement.text);
        let (action, target_memory_id): (&'static str, Option<String>) = match &neighbor {
            Some((memory_id, similarity)) if *similarity >= duplicate_threshold => {
                ("reinforce_existing", Some(memory_id.clone()))
            }
            _ => ("create_candidate", None),
        };
        let candidate_id = import_candidate_id(
            workspace_id,
            action,
            statement.kind,
            &display_path,
            &statement.text,
        );
        let already_present = connection
            .get_curation_candidate(workspace_id, &candidate_id)
            .map_err(|error| storage_error("Failed to check existing agentsmd candidate", error))?
            .is_some();
        if already_present {
            report.abstentions.push(AgentsmdImportAbstention {
                line_number: statement.line_number,
                text: statement.text,
                reason: "already_imported",
            });
            continue;
        }
        let proposal_id = deterministic_agentsmd_id(
            "aip_",
            &[
                workspace_id,
                "agentsmd_import",
                &display_path,
                statement.kind,
                &statement.text,
            ],
        );
        report.proposals.push(AgentsmdImportProposal {
            proposal_id,
            action,
            target_memory_id,
            kind: statement.kind,
            content_draft: statement.text,
            evidence: vec![format!("file://{display_path}#L{}", statement.line_number)],
            line_number: statement.line_number,
            modality: statement.modality,
            dedup_nearest_memory_id: neighbor.as_ref().map(|(memory_id, _)| memory_id.clone()),
            dedup_similarity: neighbor.as_ref().map(|(_, similarity)| *similarity),
        });
    }

    if options.apply {
        report.applied = Some(apply_import_proposals(
            connection,
            workspace_id,
            &display_path,
            &report.proposals,
            duplicate_threshold,
        )?);
    }
    Ok(report)
}

/// Ensure the synthetic import session exists; tolerate losing an insert
/// race exactly like the distill bootstrap.
fn ensure_agentsmd_session(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<String, DomainError> {
    if let Some(session) = connection
        .get_session_by_cass_id(workspace_id, AGENTSMD_IMPORT_SESSION_KEY)
        .map_err(|error| storage_error("Failed to look up agentsmd import session", error))?
    {
        return Ok(session.id);
    }
    // sessions.id CHECK requires `sess_` + a 26-char ULID payload (= 31);
    // reuse the memory-id payload exactly like distill and
    // remember-reinforce do.
    let session_id = {
        let memory_id = crate::models::MemoryId::now().to_string();
        let payload = memory_id.trim_start_matches("mem_").to_owned();
        format!("sess_{payload}")
    };
    let input = CreateSessionInput {
        workspace_id: workspace_id.to_owned(),
        cass_session_id: AGENTSMD_IMPORT_SESSION_KEY.to_owned(),
        source_path: None,
        agent_name: None,
        model: None,
        started_at: None,
        ended_at: None,
        message_count: 0,
        token_count: None,
        content_hash: format!(
            "blake3:{}",
            blake3::hash(AGENTSMD_IMPORT_SESSION_KEY.as_bytes()).to_hex()
        ),
        metadata_json: None,
    };
    match connection.insert_session(&session_id, &input) {
        Ok(()) => Ok(session_id),
        Err(error) => connection
            .get_session_by_cass_id(workspace_id, AGENTSMD_IMPORT_SESSION_KEY)
            .map_err(|query_error| {
                storage_error(
                    "Failed to re-query raced agentsmd import session",
                    query_error,
                )
            })?
            .map(|session| session.id)
            .ok_or_else(|| storage_error("Failed to create agentsmd import session", error)),
    }
}

/// Durable phase of `ee import agentsmd --apply`: per proposal, one
/// transaction writes the evidence span for the source line (create
/// proposals), the pending curation candidate, and one `agentsmd.import`
/// audit row. Candidates only, never direct memories
/// (evidence-before-promotion, ADR 0065 §5).
fn apply_import_proposals(
    connection: &DbConnection,
    workspace_id: &str,
    display_path: &str,
    proposals: &[AgentsmdImportProposal],
    duplicate_threshold: f32,
) -> Result<AgentsmdImportApplied, DomainError> {
    let mut applied = AgentsmdImportApplied::default();
    if proposals.is_empty() {
        return Ok(applied);
    }
    let needs_session = proposals
        .iter()
        .any(|proposal| proposal.action == "create_candidate");
    let session_id = if needs_session {
        Some(ensure_agentsmd_session(connection, workspace_id)?)
    } else {
        None
    };
    let imported_at = Utc::now().to_rfc3339();
    for proposal in proposals {
        let candidate_id = import_candidate_id(
            workspace_id,
            proposal.action,
            proposal.kind,
            display_path,
            &proposal.content_draft,
        );
        let span_id = deterministic_agentsmd_id(
            "ev_",
            &[
                workspace_id,
                "agentsmd_import",
                display_path,
                &proposal.content_draft,
            ],
        );
        let statement_hash = format!(
            "blake3:{}",
            blake3::hash(proposal.content_draft.as_bytes()).to_hex()
        );
        let candidate_input = if proposal.action == "reinforce_existing" {
            CreateCurationCandidateInput {
                workspace_id: workspace_id.to_owned(),
                candidate_type: CandidateType::Promote.as_str().to_owned(),
                target_memory_id: proposal.target_memory_id.clone(),
                proposed_content: None,
                proposed_confidence: proposal.dedup_similarity,
                proposed_trust_class: None,
                source_type: CandidateSource::AgentInference.as_str().to_owned(),
                source_id: Some("agentsmd_import".to_owned()),
                reason: format!(
                    "AGENTS.md bridge import: near-duplicate of {} at similarity {:.4} \
                     (threshold {:.4}); reinforce the existing memory instead of creating a \
                     new one. Evidence: {}",
                    proposal.target_memory_id.as_deref().unwrap_or("unknown"),
                    proposal.dedup_similarity.unwrap_or_default(),
                    duplicate_threshold,
                    proposal.evidence.join(", "),
                ),
                confidence: AGENTSMD_IMPORT_CONFIDENCE,
                status: Some(CandidateStatus::Pending.as_str().to_owned()),
                created_at: Some(imported_at.clone()),
                ttl_expires_at: None,
                derivation_source_refs_json: None,
                derivation_metadata_json: None,
            }
        } else {
            let source_refs_json = serde_json::json!([{
                "kind": "evidence_span",
                "id": &span_id,
                "contentHash": &statement_hash,
            }])
            .to_string();
            let metadata_json = serde_json::json!({
                "memorySpec": {
                    "level": "procedural",
                    "kind": proposal.kind,
                    "tags": ["agentsmd-import"],
                    "confidence": AGENTSMD_IMPORT_CONFIDENCE,
                    "utility": serde_json::Value::Null,
                    "importance": serde_json::Value::Null,
                    "validFrom": serde_json::Value::Null,
                    "validTo": serde_json::Value::Null,
                },
                "producer": {
                    "producer": "agentsmd_import",
                    "producerPayload": {
                        "proposalId": &proposal.proposal_id,
                        "evidence": &proposal.evidence,
                        "file": display_path,
                        "lineNumber": proposal.line_number,
                        "modality": proposal.modality,
                    },
                },
            })
            .to_string();
            CreateCurationCandidateInput {
                workspace_id: workspace_id.to_owned(),
                candidate_type: CandidateType::CreateDerivedMemory.as_str().to_owned(),
                target_memory_id: None,
                proposed_content: Some(proposal.content_draft.clone()),
                proposed_confidence: Some(AGENTSMD_IMPORT_CONFIDENCE),
                proposed_trust_class: Some("agent_assertion".to_owned()),
                source_type: CandidateSource::AgentInference.as_str().to_owned(),
                source_id: Some("agentsmd_import".to_owned()),
                reason: format!(
                    "AGENTS.md bridge import: {} statement extracted from {}. Evidence: {}",
                    proposal.kind,
                    display_path,
                    proposal.evidence.join(", "),
                ),
                confidence: AGENTSMD_IMPORT_CONFIDENCE,
                status: Some(CandidateStatus::Pending.as_str().to_owned()),
                created_at: Some(imported_at.clone()),
                ttl_expires_at: None,
                derivation_source_refs_json: Some(source_refs_json),
                derivation_metadata_json: Some(metadata_json),
            }
        };

        let audit_id = generate_audit_id();
        let audit_details = serde_json::json!({
            "schema": AGENTSMD_IMPORT_AUDIT_SCHEMA_V1,
            "command": "ee import agentsmd --apply",
            "proposalId": &proposal.proposal_id,
            "action": proposal.action,
            "candidateId": &candidate_id,
            "level": "procedural",
            "kind": proposal.kind,
            "evidence": &proposal.evidence,
            "dedup": {
                "nearestMemoryId": &proposal.dedup_nearest_memory_id,
                "similarity": &proposal.dedup_similarity,
                "threshold": duplicate_threshold,
            },
            "importedAt": &imported_at,
        })
        .to_string();
        let audit_input = CreateAuditInput {
            workspace_id: Some(workspace_id.to_owned()),
            actor: Some("ee import agentsmd".to_owned()),
            action: audit_actions::AGENTSMD_IMPORT.to_owned(),
            target_type: Some("curation_candidate".to_owned()),
            target_id: Some(candidate_id.clone()),
            details: Some(audit_details),
        };

        connection
            .with_transaction(|| {
                if proposal.action == "create_candidate" {
                    // `needs_session` above guarantees this for create
                    // proposals; treat a miss as a storage invariant break.
                    let Some(session_id) = session_id.as_deref() else {
                        return Err(crate::db::DbError::MalformedRow {
                            operation: crate::db::DbOperation::Execute,
                            message: "agentsmd import session missing for create proposal"
                                .to_owned(),
                        });
                    };
                    if connection.get_evidence_span(&span_id)?.is_none() {
                        let line = u32::try_from(proposal.line_number).unwrap_or(1);
                        let metadata_json = serde_json::json!({
                            "schema": AGENTSMD_IMPORT_EVIDENCE_SCHEMA_V1,
                            "command": "ee import agentsmd --apply",
                            "fileUri": proposal.evidence.first(),
                            "file": display_path,
                            "lineNumber": proposal.line_number,
                            "modality": proposal.modality,
                        })
                        .to_string();
                        connection.insert_evidence_span(
                            &span_id,
                            &CreateEvidenceSpanInput {
                                workspace_id: workspace_id.to_owned(),
                                session_id: session_id.to_owned(),
                                memory_id: None,
                                cass_span_id: format!(
                                    "agentsmd:{display_path}#L{}",
                                    proposal.line_number
                                ),
                                span_kind: "summary".to_owned(),
                                start_line: line,
                                end_line: line,
                                start_byte: None,
                                end_byte: None,
                                role: Some("agentsmd_import".to_owned()),
                                excerpt: proposal.content_draft.clone(),
                                content_hash: statement_hash.clone(),
                                metadata_json: Some(metadata_json),
                            },
                        )?;
                    }
                }
                connection.insert_curation_candidate(&candidate_id, &candidate_input)?;
                connection.insert_audit(&audit_id, &audit_input)
            })
            .map_err(|error| storage_error("Failed to apply agentsmd import proposal", error))?;
        applied.candidate_ids.push(candidate_id);
        applied.audit_ids.push(audit_id);
    }
    Ok(applied)
}

// ---------------------------------------------------------------------------
// Drift diagnostic
// ---------------------------------------------------------------------------

/// Options for `ee diag agentsmd-drift`.
#[derive(Clone, Debug, Default)]
pub struct AgentsmdDriftOptions {
    /// Target file; relative paths resolve against the workspace root.
    pub file: Option<PathBuf>,
}

/// Managed-block freshness summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentsmdManagedBlockStatus {
    pub generation: Option<i64>,
    /// Block generation behind the current DB generation (or unparseable).
    pub stale: bool,
    /// Recorded body hash matches the current body bytes.
    pub hash_matches: bool,
}

/// One file-vs-memory contradiction finding, expressed in the conflict
/// surface vocabulary (`contradiction_link`).
#[derive(Clone, Debug, PartialEq)]
pub struct AgentsmdContradictionFinding {
    pub line_number: usize,
    pub file_text: String,
    pub file_polarity: &'static str,
    pub memory_id: String,
    pub memory_polarity: &'static str,
    pub similarity: f32,
    pub signal: &'static str,
}

/// One memory rule with no counterpart anywhere in the file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentsmdMissingRuleFinding {
    pub memory_id: String,
    pub line: String,
}

/// Result of one `ee diag agentsmd-drift` run (`ee.agentsmd.drift.v1`).
#[derive(Clone, Debug, PartialEq)]
pub struct AgentsmdDriftReport {
    /// `ok` or `file_missing`.
    pub status: &'static str,
    pub workspace_id: String,
    pub file: String,
    pub db_generation: i64,
    /// `None` when the file has no managed block.
    pub managed_block: Option<AgentsmdManagedBlockStatus>,
    pub contradictions: Vec<AgentsmdContradictionFinding>,
    pub missing_rules: Vec<AgentsmdMissingRuleFinding>,
    /// Advisory only: the diagnostic never mutates.
    pub suggested_commands: Vec<String>,
    pub degraded: Vec<AgentsmdDegradation>,
}

impl AgentsmdDriftReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": AGENTSMD_DRIFT_SCHEMA_V1,
            "command": "diag agentsmd-drift",
            "status": self.status,
            "workspaceId": self.workspace_id,
            "file": self.file,
            "dbGeneration": self.db_generation,
            "managedBlock": self.managed_block.as_ref().map(|block| serde_json::json!({
                "generation": block.generation,
                "stale": block.stale,
                "hashMatches": block.hash_matches,
            })),
            "contradictions": self.contradictions.iter().map(|finding| serde_json::json!({
                "lineNumber": finding.line_number,
                "fileText": &finding.file_text,
                "filePolarity": finding.file_polarity,
                "memoryId": &finding.memory_id,
                "memoryPolarity": finding.memory_polarity,
                "similarity": finding.similarity,
                "signal": finding.signal,
            })).collect::<Vec<_>>(),
            "missingRules": self.missing_rules.iter().map(|finding| serde_json::json!({
                "memoryId": &finding.memory_id,
                "line": &finding.line,
            })).collect::<Vec<_>>(),
            "suggestedCommands": &self.suggested_commands,
            "degraded": self.degraded.iter().map(AgentsmdDegradation::data_json).collect::<Vec<_>>(),
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = format!(
            "agentsmd drift — {} (db generation {})\nstatus: {}\n",
            self.file, self.db_generation, self.status,
        );
        match &self.managed_block {
            Some(block) => out.push_str(&format!(
                "managed block: generation {}, {}, hash {}\n",
                block
                    .generation
                    .map_or_else(|| "unknown".to_owned(), |generation| generation.to_string()),
                if block.stale { "stale" } else { "current" },
                if block.hash_matches {
                    "ok"
                } else {
                    "MISMATCH (hand-edited)"
                },
            )),
            None => out.push_str("managed block: none\n"),
        }
        out.push_str(&format!(
            "contradictions: {}, missing rules: {}\n",
            self.contradictions.len(),
            self.missing_rules.len()
        ));
        for finding in &self.contradictions {
            out.push_str(&format!(
                "- L{} {} vs {} ({}): {}\n",
                finding.line_number,
                finding.file_polarity,
                finding.memory_id,
                finding.memory_polarity,
                finding.file_text,
            ));
        }
        for finding in &self.missing_rules {
            out.push_str(&format!(
                "- missing: {} ({})\n",
                finding.line, finding.memory_id
            ));
        }
        for command in &self.suggested_commands {
            out.push_str(&format!("suggest: {command}\n"));
        }
        for entry in &self.degraded {
            out.push_str(&format!("degraded: {} ({})\n", entry.code, entry.severity));
        }
        out
    }
}

fn push_unique(commands: &mut Vec<String>, command: String) {
    if !commands.contains(&command) {
        commands.push(command);
    }
}

/// Execute `ee diag agentsmd-drift` (ADR 0065 §5 drift contract).
/// Read-only: reports findings and suggested commands, never mutates.
pub fn run_agentsmd_drift(
    connection: &DbConnection,
    workspace_id: &str,
    workspace_path: &Path,
    options: &AgentsmdDriftOptions,
) -> Result<AgentsmdDriftReport, DomainError> {
    let db_generation = i64::try_from(
        connection
            .get_workspace_generation(workspace_id)
            .map_err(|error| storage_error("Failed to read workspace generation", error))?
            .unwrap_or(0),
    )
    .unwrap_or(i64::MAX);
    let (path, display_path) = resolve_bridge_file(workspace_path, options.file.as_deref());

    let mut report = AgentsmdDriftReport {
        status: "ok",
        workspace_id: workspace_id.to_owned(),
        file: display_path.clone(),
        db_generation,
        managed_block: None,
        contradictions: Vec::new(),
        missing_rules: Vec::new(),
        suggested_commands: Vec::new(),
        degraded: Vec::new(),
    };

    let Some(content) = read_bridge_file(&path, &display_path)? else {
        report.status = "file_missing";
        report
            .degraded
            .push(file_missing_degradation(&display_path, true));
        push_unique(
            &mut report.suggested_commands,
            "ee export agentsmd --workspace . --create".to_owned(),
        );
        return Ok(report);
    };

    let scan = scan_managed_block(&content)
        .map_err(|reason| malformed_markers_error(&display_path, &reason))?;
    let exclude = match &scan {
        ManagedBlockScan::Found(block) => {
            let hash_matches = block.recorded_hash.as_deref()
                == Some(managed_block_body_hash(&block.body).as_str());
            let stale = block
                .generation
                .is_none_or(|generation| generation < db_generation);
            if stale {
                push_unique(
                    &mut report.suggested_commands,
                    "ee export agentsmd --workspace .".to_owned(),
                );
            }
            if !hash_matches {
                report
                    .degraded
                    .push(unmanaged_edit_degradation(&display_path));
                push_unique(
                    &mut report.suggested_commands,
                    "ee export agentsmd --workspace . --dry-run".to_owned(),
                );
            }
            report.managed_block = Some(AgentsmdManagedBlockStatus {
                generation: block.generation,
                stale,
                hash_matches,
            });
            Some((block.begin_index, block.end_index))
        }
        ManagedBlockScan::Missing => {
            report
                .degraded
                .push(markers_missing_degradation(&display_path));
            push_unique(
                &mut report.suggested_commands,
                "ee export agentsmd --workspace .".to_owned(),
            );
            None
        }
    };

    // File-vs-memory contradictions: a hand-written statement that pairs
    // with a high-confidence procedural rule on the same topic but with
    // opposite polarity (conflict surface vocabulary, ADR 0065 §5).
    let memories = connection
        .list_memories(workspace_id, None, false)
        .map_err(|error| storage_error("Failed to list memories for drift detection", error))?;
    let rule_memories: Vec<&StoredMemory> = memories
        .iter()
        .filter(|memory| {
            memory.level == "procedural"
                && memory.kind == "rule"
                && memory.confidence >= AGENTSMD_CONTRADICTION_MIN_CONFIDENCE
                && memory.tombstoned_at.is_none()
        })
        .collect();
    let hand_statements = parse_rule_statements(&content, exclude);
    if !rule_memories.is_empty() && !hand_statements.is_empty() {
        let embedder = HashEmbedder::default_256();
        for statement in &hand_statements {
            let statement_embedding = embedder.embed_sync(&statement.text);
            let mut best: Option<(&StoredMemory, f32)> = None;
            for memory in &rule_memories {
                let memory_embedding = embedder.embed_sync(&memory.content);
                let Some(similarity) = cosine_similarity(&statement_embedding, &memory_embedding)
                else {
                    continue;
                };
                let better = match &best {
                    None => similarity >= AGENTSMD_CONTRADICTION_SIMILARITY,
                    Some((current, current_similarity)) => {
                        similarity > *current_similarity
                            || (similarity == *current_similarity && memory.id < current.id)
                    }
                };
                if better && similarity >= AGENTSMD_CONTRADICTION_SIMILARITY {
                    best = Some((memory, similarity));
                }
            }
            let Some((memory, similarity)) = best else {
                continue;
            };
            let Some((_, memory_polarity, _)) = classify_statement(&memory.content, true) else {
                continue;
            };
            if memory_polarity == statement.polarity {
                continue;
            }
            push_unique(
                &mut report.suggested_commands,
                format!("ee why {} --workspace . --json", memory.id),
            );
            report.contradictions.push(AgentsmdContradictionFinding {
                line_number: statement.line_number,
                file_text: statement.text.clone(),
                file_polarity: statement.polarity.as_str(),
                memory_id: memory.id.clone(),
                memory_polarity: memory_polarity.as_str(),
                similarity,
                signal: "contradiction_link",
            });
        }
    }

    // Memory rules absent from the file: primer-selected rules with no
    // sufficiently similar statement anywhere in the file (managed block
    // included — presence inside the block counts as presence).
    let primer = assemble_bridge_primer(connection, workspace_id, workspace_path, None)?;
    let all_statements = parse_rule_statements(&content, None);
    let duplicate_threshold = duplicate_similarity_threshold(workspace_path);
    let memory_content_by_id: std::collections::BTreeMap<&str, &str> = memories
        .iter()
        .map(|memory| (memory.id.as_str(), memory.content.as_str()))
        .collect();
    if let Some(rules_section) = primer
        .sections
        .iter()
        .find(|section| section.name == "rules")
    {
        let embedder = HashEmbedder::default_256();
        for item in &rules_section.items {
            let Some(memory_content) = memory_content_by_id.get(item.memory_id.as_str()) else {
                continue;
            };
            let memory_embedding = embedder.embed_sync(memory_content);
            let present = all_statements.iter().any(|statement| {
                let statement_embedding = embedder.embed_sync(&statement.text);
                cosine_similarity(&memory_embedding, &statement_embedding)
                    .is_some_and(|similarity| similarity >= duplicate_threshold)
            });
            if !present {
                push_unique(
                    &mut report.suggested_commands,
                    "ee export agentsmd --workspace .".to_owned(),
                );
                report.missing_rules.push(AgentsmdMissingRuleFinding {
                    memory_id: item.memory_id.clone(),
                    line: item.line.clone(),
                });
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(name: &str, lines: &[(&str, &str)]) -> PrimerSection {
        PrimerSection {
            name: name.to_owned(),
            items: lines
                .iter()
                .map(|(memory_id, line)| crate::core::primer::PrimerItem {
                    memory_id: (*memory_id).to_owned(),
                    line: (*line).to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    confidence: 0.9,
                    provenance: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn scan_finds_block_with_attributes() {
        let content = "intro\n<!-- ee:agentsmd:begin generation=7 hash=blake3:abcd -->\nbody one\nbody two\n<!-- ee:agentsmd:end -->\ntail\n";
        let ManagedBlockScan::Found(block) = scan_managed_block(content).expect("scan") else {
            panic!("expected managed block");
        };
        assert_eq!(block.begin_index, 1);
        assert_eq!(block.end_index, 4);
        assert_eq!(block.generation, Some(7));
        assert_eq!(block.recorded_hash.as_deref(), Some("blake3:abcd"));
        assert_eq!(block.body, "body one\nbody two");
    }

    #[test]
    fn scan_reports_missing_markers() {
        assert_eq!(
            scan_managed_block("no markers here\n").expect("scan"),
            ManagedBlockScan::Missing
        );
    }

    #[test]
    fn scan_refuses_malformed_marker_structures() {
        assert!(scan_managed_block("<!-- ee:agentsmd:begin generation=1 -->\n").is_err());
        assert!(scan_managed_block("<!-- ee:agentsmd:end -->\n").is_err());
        let nested =
            "<!-- ee:agentsmd:begin -->\n<!-- ee:agentsmd:begin -->\n<!-- ee:agentsmd:end -->\n";
        assert!(scan_managed_block(nested).is_err());
        let double = "<!-- ee:agentsmd:begin -->\n<!-- ee:agentsmd:end -->\n<!-- ee:agentsmd:begin -->\n<!-- ee:agentsmd:end -->\n";
        assert!(scan_managed_block(double).is_err());
    }

    #[test]
    fn rendered_block_round_trips_through_scan_with_matching_hash() {
        let sections = vec![
            section("rules", &[("mem_a", "Always run verify. [mem_a]")]),
            section("warnings", &[("mem_b", "Goldens drift on Mac. [mem_b]")]),
            section("decisions", &[("mem_c", "excluded section [mem_c]")]),
        ];
        let body = render_managed_body(&sections);
        assert!(body.contains("## Workspace rules (ee memory)"));
        assert!(body.contains("## Workspace warnings (ee memory)"));
        assert!(!body.contains("excluded section"));
        let block = render_managed_block(&body, 42);
        let ManagedBlockScan::Found(scanned) =
            scan_managed_block(&format!("{block}\n")).expect("scan")
        else {
            panic!("expected managed block");
        };
        assert_eq!(scanned.generation, Some(42));
        assert_eq!(
            scanned.recorded_hash.as_deref(),
            Some(managed_block_body_hash(&scanned.body).as_str()),
            "recorded hash matches the scanned body"
        );
    }

    #[test]
    fn render_is_deterministic_and_body_hash_detects_edits() {
        let sections = vec![section("rules", &[("mem_a", "Always run verify. [mem_a]")])];
        let body_one = render_managed_body(&sections);
        let body_two = render_managed_body(&sections);
        assert_eq!(body_one, body_two, "byte-identical re-render");
        let edited = body_one.clone() + "sneaky hand edit\n";
        assert_ne!(
            managed_block_body_hash(&body_one),
            managed_block_body_hash(&edited)
        );
    }

    #[test]
    fn classify_accepts_hard_modality_everywhere_and_cues_only_on_bullets() {
        let must = "The release pipeline MUST run the verify script first.";
        assert!(classify_statement(must, false).is_some());
        let cue = "Never commit directly to the release branch here.";
        assert!(classify_statement(cue, true).is_some());
        assert!(
            classify_statement(cue, false).is_none(),
            "leading cues only count on bullets"
        );
        let soft = "Prefer structured logging over print statements.";
        let (kind, polarity, _) = classify_statement(soft, true).expect("convention");
        assert_eq!(kind, "convention");
        assert_eq!(polarity, RulePolarity::Positive);
    }

    #[test]
    fn classify_enforces_length_bounds() {
        assert!(
            classify_statement("MUST do it.", true).is_none(),
            "too short"
        );
        let long = format!("ALWAYS {}", "x".repeat(AGENTSMD_RULE_MAX_CHARS));
        assert!(classify_statement(&long, true).is_none(), "too long");
    }

    #[test]
    fn parser_skips_fences_headings_tables_comments_and_managed_block() {
        let content = "\
# Heading MUST NOT match here at all costs

- Always run the verify script before pushing changes.
| MUST not match inside a table row, ever |
> NEVER match inside a blockquote either, please.
<!-- NEVER match inside an html comment line. -->

```bash
echo 'NEVER match inside a fenced code block, period.'
```

<!-- ee:agentsmd:begin generation=1 hash=blake3:x -->
- NEVER match inside the managed block region.
<!-- ee:agentsmd:end -->

The deploy job MUST wait for the smoke suite to finish.
";
        let exclude = match scan_managed_block(content).expect("scan") {
            ManagedBlockScan::Found(block) => Some((block.begin_index, block.end_index)),
            ManagedBlockScan::Missing => None,
        };
        let statements = parse_rule_statements(content, exclude);
        let texts: Vec<&str> = statements
            .iter()
            .map(|statement| statement.text.as_str())
            .collect();
        assert_eq!(
            texts,
            vec![
                "Always run the verify script before pushing changes.",
                "The deploy job MUST wait for the smoke suite to finish.",
            ],
        );
        assert_eq!(statements[0].kind, "rule");
        assert_eq!(statements[0].polarity, RulePolarity::Positive);
        assert_eq!(statements[1].line_number, 16);
    }

    #[test]
    fn parser_extracts_numbered_bullets_and_negative_modality() {
        let content = "1. Do not regenerate goldens on the Mac checkout.\n2. plain step without any modality cue\n";
        let statements = parse_rule_statements(content, None);
        assert_eq!(statements.len(), 1);
        assert_eq!(statements[0].polarity, RulePolarity::Negative);
        assert_eq!(statements[0].modality, "Do not");
    }

    #[test]
    fn block_diff_lists_removed_then_added_lines() {
        let diff = render_block_diff("old line", "new one\nnew two");
        assert_eq!(diff, "- old line\n+ new one\n+ new two\n");
    }

    #[test]
    fn import_candidate_ids_are_deterministic_and_text_keyed() {
        let first = import_candidate_id("wsp_1", "create_candidate", "rule", "AGENTS.md", "text");
        let second = import_candidate_id("wsp_1", "create_candidate", "rule", "AGENTS.md", "text");
        assert_eq!(first, second);
        let moved_line_same_text =
            import_candidate_id("wsp_1", "create_candidate", "rule", "AGENTS.md", "text");
        assert_eq!(first, moved_line_same_text, "line moves do not duplicate");
        let other = import_candidate_id("wsp_1", "create_candidate", "rule", "AGENTS.md", "other");
        assert_ne!(first, other);
        assert!(first.starts_with("curate_"));
    }
}
