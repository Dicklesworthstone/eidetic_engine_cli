//! bd-1n0np.4.2 — structured-diagnostic canonicalizers + layered fingerprint
//! key for Error Fingerprint Recall (feature bd-1n0np.4).
//!
//! Canonicalize tool failures from STRUCTURED diagnostics first (rustc/cargo
//! error codes, `ee.error.v2` codes, RCH blocker kind+stage, shell exit + first
//! stable line) so deduplication keys on canonical codes. Fuzzy matching (a
//! simhash tail layer) is intentionally deferred — get the exact
//! `(tool, canonical_code)` layer right first; fuzzy matching is only the
//! long-tail fallback.
//!
//! This module is pure and deterministic: the same diagnostic always yields the
//! same canonical form and the same layered key. The fingerprint store
//! (bd-1n0np.4.3 / V069) and redaction defaults (bd-1n0np.4.6) build on these.

/// The tool a diagnostic originated from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticTool {
    Cargo,
    Rustc,
    Ee,
    Rch,
    Shell,
}

impl DiagnosticTool {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Rustc => "rustc",
            Self::Ee => "ee",
            Self::Rch => "rch",
            Self::Shell => "shell",
        }
    }
}

/// Which layer of the layered key produced the match (strongest → weakest).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FingerprintLayer {
    /// Exact `(tool, canonical_code)` — the strongest, fully-structured layer.
    CanonicalCode,
    /// Variable-masked message template — used when no canonical code exists.
    MessageTemplate,
    /// Fuzzy simhash neighborhood of the template — the long-tail fallback for
    /// near-duplicate code-less messages (matched by Hamming distance, not by
    /// exact key equality).
    SimhashTail,
}

impl FingerprintLayer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CanonicalCode => "canonical_code",
            Self::MessageTemplate => "message_template",
            Self::SimhashTail => "simhash_tail",
        }
    }
}

/// A canonicalized diagnostic ready for keying and storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalDiagnostic {
    pub tool: DiagnosticTool,
    /// Stable structured code when one exists (rustc `E0277`, an `ee.error.v2`
    /// code, an RCH `kind:stage`). `None` for code-less failures.
    pub canonical_code: Option<String>,
    /// Variable-masked message template (identifiers, paths, numbers, and hex
    /// masked) so two structurally-identical errors collapse to one template.
    pub message_template: String,
}

/// A layered fingerprint key; `layer` records which layer produced `key`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FingerprintKey {
    pub layer: FingerprintLayer,
    pub key: String,
}

impl CanonicalDiagnostic {
    /// Layered key: prefer the exact `(tool, canonical_code)` layer; fall back to
    /// the `tool:tmpl:<hash>` message-template layer when no code is present.
    #[must_use]
    pub fn layered_key(&self) -> FingerprintKey {
        match self.canonical_code.as_deref() {
            Some(code) if !code.is_empty() => FingerprintKey {
                layer: FingerprintLayer::CanonicalCode,
                key: format!("{}:{}", self.tool.as_str(), code),
            },
            _ => FingerprintKey {
                layer: FingerprintLayer::MessageTemplate,
                key: format!(
                    "{}:tmpl:{}",
                    self.tool.as_str(),
                    blake3_prefixed(&self.message_template)
                ),
            },
        }
    }
}

fn blake3_prefixed(value: &str) -> String {
    format!("blake3:{}", blake3::hash(value.as_bytes()).to_hex())
}

fn normalize_code(code: Option<&str>) -> Option<String> {
    code.map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_string)
}

/// True when a token is a number-like span (digits plus `: . , - _`), e.g. a
/// line:col, a version, or a byte count — masked so it cannot fragment a class.
fn is_numeric_token(token: &str) -> bool {
    let mut has_digit = false;
    for ch in token.chars() {
        if ch.is_ascii_digit() {
            has_digit = true;
        } else if !matches!(ch, ':' | '.' | ',' | '-' | '_') {
            return false;
        }
    }
    has_digit
}

fn mask_backtick_spans(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut in_span = false;
    for ch in message.chars() {
        if ch == '`' {
            if !in_span {
                out.push_str("<id>");
            }
            in_span = !in_span;
        } else if !in_span {
            out.push(ch);
        }
    }
    out
}

fn mask_token(raw: &str) -> String {
    if raw.contains('/') || raw.contains('\\') {
        return "<path>".to_string();
    }
    if let Some(rest) = raw.strip_prefix("0x")
        && !rest.is_empty()
        && rest.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return "<hex>".to_string();
    }
    if is_numeric_token(raw) {
        return "<num>".to_string();
    }
    raw.to_ascii_lowercase()
}

/// Normalize a diagnostic message into a stable, variable-masked template so
/// errors that differ only by identifier/path/line/number/hex collapse to one
/// class. Deterministic and allocation-bounded.
#[must_use]
pub fn canonical_message_template(message: &str) -> String {
    mask_backtick_spans(message)
        .split_whitespace()
        .map(mask_token)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Canonicalize a rustc diagnostic (e.g. `--error-format=json` `code.code`).
#[must_use]
pub fn from_rustc(code: Option<&str>, message: &str) -> CanonicalDiagnostic {
    CanonicalDiagnostic {
        tool: DiagnosticTool::Rustc,
        canonical_code: normalize_code(code),
        message_template: canonical_message_template(message),
    }
}

/// Canonicalize a cargo diagnostic (`--message-format=json`); the rustc code is
/// reused when cargo surfaces a compiler message.
#[must_use]
pub fn from_cargo(code: Option<&str>, message: &str) -> CanonicalDiagnostic {
    CanonicalDiagnostic {
        tool: DiagnosticTool::Cargo,
        canonical_code: normalize_code(code),
        message_template: canonical_message_template(message),
    }
}

/// Canonicalize an `ee.error.v2` failure keyed on its stable error code.
#[must_use]
pub fn from_ee_error(code: &str, message: &str) -> CanonicalDiagnostic {
    CanonicalDiagnostic {
        tool: DiagnosticTool::Ee,
        canonical_code: normalize_code(Some(code)),
        message_template: canonical_message_template(message),
    }
}

/// Canonicalize an RCH blocker keyed on `kind:stage`.
#[must_use]
pub fn from_rch_blocker(kind: &str, stage: &str, message: &str) -> CanonicalDiagnostic {
    let kind = kind.trim();
    let stage = stage.trim();
    let canonical_code = if kind.is_empty() {
        None
    } else if stage.is_empty() {
        Some(kind.to_string())
    } else {
        Some(format!("{kind}:{stage}"))
    };
    CanonicalDiagnostic {
        tool: DiagnosticTool::Rch,
        canonical_code,
        message_template: canonical_message_template(message),
    }
}

/// Canonicalize a code-less shell failure from its exit status and first stable
/// line. There is no structured code, so the exit status is folded into the
/// message template and the layered key falls to the template layer.
#[must_use]
pub fn from_shell(exit_code: i32, first_line: &str) -> CanonicalDiagnostic {
    CanonicalDiagnostic {
        tool: DiagnosticTool::Shell,
        canonical_code: None,
        message_template: format!(
            "exit_{exit_code} {}",
            canonical_message_template(first_line)
        ),
    }
}

/// Maximum Hamming distance (of 128 bits) for two message-template simhashes to
/// count as the same long-tail error class (bd-1n0np.4.2). Conservative so the
/// fuzzy tail never collapses genuinely distinct failures.
pub const SIMHASH_TAIL_MAX_DISTANCE: u32 = 6;

impl CanonicalDiagnostic {
    /// 128-bit Charikar simhash of the message template — the fuzzy tail
    /// fingerprint for code-less near-duplicates. Reuses the shared
    /// `search::simhash` so this matches the rest of the store.
    #[must_use]
    pub fn simhash_tail(&self) -> u128 {
        crate::search::simhash::simhash_128(&self.message_template).to_u128()
    }
}

/// Hamming distance between two template simhashes (count of differing bits).
#[must_use]
pub fn simhash_hamming_distance(left: u128, right: u128) -> u32 {
    (left ^ right).count_ones()
}

/// True when two template simhashes are within `max_distance` bits — i.e. the
/// same long-tail class. Use only as the weakest layer, after exact code and
/// exact template lookups miss.
#[must_use]
pub fn simhash_tail_matches(left: u128, right: u128, max_distance: u32) -> bool {
    simhash_hamming_distance(left, right) <= max_distance
}

/// A redaction-safe diagnostic record (bd-1n0np.4.6). Stores the fingerprint and
/// a policy-redacted message — never the raw log. The fingerprint is derived
/// from the *redacted* text, so no secret can leak into a key or a stored span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedactedDiagnostic {
    pub canonical: CanonicalDiagnostic,
    pub fingerprint_key: FingerprintKey,
    /// Secret/PII-redacted message, safe to persist and display.
    pub redacted_message: String,
    /// Number of secret-like spans removed before persistence.
    pub redacted_span_count: usize,
    /// Stable reasons for each redaction class applied.
    pub redaction_reasons: Vec<&'static str>,
}

/// Apply policy redaction to a raw diagnostic BEFORE it becomes fingerprint or
/// stored material (bd-1n0np.4.6): redact secrets/PII first, derive the
/// canonical fingerprint and message template from the redacted text, and return
/// the redacted message + span metadata. The raw log is never retained — store
/// fingerprints + redacted spans by default, never full logs.
#[must_use]
pub fn redact_diagnostic(
    tool: DiagnosticTool,
    canonical_code: Option<&str>,
    raw_message: &str,
) -> RedactedDiagnostic {
    let report = crate::policy::redact_secret_like_content(raw_message);
    let canonical = CanonicalDiagnostic {
        tool,
        canonical_code: normalize_code(canonical_code),
        message_template: canonical_message_template(&report.content),
    };
    let fingerprint_key = canonical.layered_key();
    RedactedDiagnostic {
        canonical,
        fingerprint_key,
        redacted_message: report.content,
        redacted_span_count: report.matches.len(),
        redaction_reasons: report.redacted_reasons,
    }
}

/// A redaction-safe, persistable error fingerprint (bd-1n0np.4.6): the
/// [`ErrorFingerprint`] record — which by construction stores no raw log — plus
/// the count and reasons of secret-like spans removed before fingerprinting.
/// This is the redaction default for what gets stored/emitted: fingerprints +
/// redacted-span metadata, never full logs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedactedErrorFingerprint {
    pub fingerprint: ErrorFingerprint,
    pub redacted_span_count: usize,
    pub redaction_reasons: Vec<&'static str>,
}

/// Build a redaction-safe [`ErrorFingerprint`] from a RAW diagnostic
/// (bd-1n0np.4.6): apply policy secret/PII redaction FIRST, canonicalize the
/// redacted text, then derive the fingerprint via the shared
/// [`ErrorFingerprint`] model. The raw log is never retained — only the
/// fingerprint (template signature, masked shape, simhash) and the redacted-span
/// metadata. Use this on the persistence/output path so secrets, paths, and user
/// data in stderr never reach storage.
#[must_use]
pub fn redact_to_fingerprint(
    tool: DiagnosticTool,
    canonical_code: Option<&str>,
    raw_message: &str,
) -> RedactedErrorFingerprint {
    let report = crate::policy::redact_secret_like_content(raw_message);
    let canonical = CanonicalDiagnostic {
        tool,
        canonical_code: normalize_code(canonical_code),
        message_template: canonical_message_template(&report.content),
    };
    RedactedErrorFingerprint {
        fingerprint: ErrorFingerprint::from_canonical(&canonical),
        redacted_span_count: report.matches.len(),
        redaction_reasons: report.redacted_reasons,
    }
}

/// The kind of downstream artifact an error fingerprint links to (bd-1n0np.4.3):
/// the failure → repair → proof → outcome chain. Persisted in the
/// `error_repair_links` table (V069) once the store lands; this models the
/// link semantics so the planner below stays pure and testable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorRepairLinkKind {
    /// A memory that repaired this failure.
    Repair,
    /// A proof / verification that the repair worked.
    Proof,
    /// An outcome attributing helpfulness or harm to the repair.
    Outcome,
    /// A curation candidate raised from this failure.
    CurationCandidate,
}

impl ErrorRepairLinkKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Repair => "repair",
            Self::Proof => "proof",
            Self::Outcome => "outcome",
            Self::CurationCandidate => "curation_candidate",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "repair" => Self::Repair,
            "proof" => Self::Proof,
            "outcome" => Self::Outcome,
            "curation_candidate" => Self::CurationCandidate,
            _ => return None,
        })
    }
}

/// One planned link from an error fingerprint to a downstream artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorRepairLink {
    pub fingerprint_key: String,
    pub kind: ErrorRepairLinkKind,
    pub target_id: String,
}

/// Plan the failure → repair → proof → outcome links for a fingerprint from the
/// already-known target ids (bd-1n0np.4.3). Pure: blank ids are dropped, output
/// is deduplicated and ordered deterministically by `(kind, target_id)`. The
/// caller persists the result into `error_repair_links` (V069) + the graph; this
/// keeps the link semantics verifiable without the migration.
#[must_use]
pub fn plan_error_repair_links(
    fingerprint_key: &str,
    repair_memory_ids: &[String],
    proof_ids: &[String],
    outcome_ids: &[String],
    curation_candidate_ids: &[String],
) -> Vec<ErrorRepairLink> {
    let groups: [(ErrorRepairLinkKind, &[String]); 4] = [
        (ErrorRepairLinkKind::Repair, repair_memory_ids),
        (ErrorRepairLinkKind::Proof, proof_ids),
        (ErrorRepairLinkKind::Outcome, outcome_ids),
        (
            ErrorRepairLinkKind::CurationCandidate,
            curation_candidate_ids,
        ),
    ];
    let mut links = Vec::new();
    for (kind, ids) in groups {
        for id in ids {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                links.push(ErrorRepairLink {
                    fingerprint_key: fingerprint_key.to_string(),
                    kind,
                    target_id: trimmed.to_string(),
                });
            }
        }
    }
    links.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then_with(|| left.target_id.cmp(&right.target_id))
    });
    links.dedup();
    links
}

/// Persistable error-fingerprint row model (bd-1n0np.4.1/4.3, ADR 0057). The
/// canonical projection of a [`CanonicalDiagnostic`] that the fingerprint store
/// (V069) persists as truth and indexes as a derived Frankensearch document.
///
/// Pure value type: derived deterministically from a canonicalized diagnostic,
/// carrying signatures/codes only (never raw log content) per the ADR 0057
/// redaction-by-default rule. The migration + repo (bd-1n0np.4.3 / V069) build
/// on this; the model and its derivation stay verifiable without the migration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorFingerprint {
    pub tool: DiagnosticTool,
    /// Stable structured code when one exists (rustc `E0277`, an `ee.error.v2`
    /// code, an RCH `kind:stage`); `None` for code-less failures.
    pub canonical_code: Option<String>,
    /// `blake3:`-prefixed signature of the variable-masked message template — the
    /// stable error-class identifier (never the raw message).
    pub message_template_signature: String,
    /// Masked location shape (e.g. `<path>:<num>:<num>`) when known; `None`
    /// otherwise. Variable-masked so it cannot fragment a class or leak a path.
    pub location_shape: Option<String>,
    /// 128-bit Charikar simhash of the message template — the fuzzy long-tail key.
    pub stderr_simhash: u128,
    /// Optional tool/version hints (e.g. `rustc 1.x`). Deduplicated and sorted.
    pub version_hints: Vec<String>,
}

impl ErrorFingerprint {
    /// Derive the persistable fingerprint from a canonicalized diagnostic.
    #[must_use]
    pub fn from_canonical(canonical: &CanonicalDiagnostic) -> Self {
        Self {
            tool: canonical.tool,
            canonical_code: canonical.canonical_code.clone(),
            message_template_signature: blake3_prefixed(&canonical.message_template),
            location_shape: None,
            stderr_simhash: canonical.simhash_tail(),
            version_hints: Vec::new(),
        }
    }

    /// Attach a masked location shape; blank shapes are dropped.
    #[must_use]
    pub fn with_location_shape(mut self, shape: impl Into<String>) -> Self {
        let shape = shape.into();
        let trimmed = shape.trim();
        self.location_shape = (!trimmed.is_empty()).then(|| trimmed.to_string());
        self
    }

    /// Attach tool/version hints; blanks dropped, result deduped and sorted so
    /// the fingerprint stays deterministic regardless of input order.
    #[must_use]
    pub fn with_version_hints(mut self, hints: Vec<String>) -> Self {
        let mut cleaned: Vec<String> = hints
            .into_iter()
            .map(|hint| hint.trim().to_string())
            .filter(|hint| !hint.is_empty())
            .collect();
        cleaned.sort();
        cleaned.dedup();
        self.version_hints = cleaned;
        self
    }

    /// Layered key for this fingerprint, consistent with
    /// [`CanonicalDiagnostic::layered_key`]: exact `(tool, canonical_code)` when a
    /// code exists, else the `tool:tmpl:<signature>` message-template layer.
    #[must_use]
    pub fn layered_key(&self) -> FingerprintKey {
        match self.canonical_code.as_deref() {
            Some(code) if !code.is_empty() => FingerprintKey {
                layer: FingerprintLayer::CanonicalCode,
                key: format!("{}:{}", self.tool.as_str(), code),
            },
            _ => FingerprintKey {
                layer: FingerprintLayer::MessageTemplate,
                key: format!(
                    "{}:tmpl:{}",
                    self.tool.as_str(),
                    self.message_template_signature
                ),
            },
        }
    }

    /// Deterministic text for the derived Frankensearch document (bd-1n0np.4.3).
    /// Composed only of tool, canonical code, template signature, masked location,
    /// simhash, and version hints — contains no raw log content (ADR 0057).
    #[must_use]
    pub fn derived_document_text(&self) -> String {
        let mut parts = vec![format!("tool:{}", self.tool.as_str())];
        if let Some(code) = &self.canonical_code {
            parts.push(format!("code:{code}"));
        }
        parts.push(format!("template:{}", self.message_template_signature));
        if let Some(location) = &self.location_shape {
            parts.push(format!("location:{location}"));
        }
        parts.push(format!("simhash:{:032x}", self.stderr_simhash));
        for hint in &self.version_hints {
            parts.push(format!("version:{hint}"));
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticTool, ErrorFingerprint, ErrorRepairLinkKind, FingerprintLayer,
        SIMHASH_TAIL_MAX_DISTANCE, canonical_message_template, from_cargo, from_ee_error,
        from_rch_blocker, from_rustc, from_shell, plan_error_repair_links, redact_diagnostic,
        redact_to_fingerprint, simhash_hamming_distance, simhash_tail_matches,
    };

    #[test]
    fn template_masks_variable_spans_and_dedups_by_class() {
        let a = canonical_message_template("cannot find value `foo` in path /a/b/c.rs:12:5");
        let b = canonical_message_template("cannot find value `bar` in path /x/y/z.rs:99:1");
        assert_eq!(
            a, b,
            "same class, different identifier/path/line -> one template"
        );
        assert!(a.contains("<id>"));
        assert!(a.contains("<path>"));
    }

    #[test]
    fn template_masks_numbers_and_hex() {
        let t = canonical_message_template("Segmentation fault at 0xDEADBEEF after 4096 bytes");
        assert!(t.contains("<hex>"));
        assert!(t.contains("<num>"));
        assert!(!t.contains("4096"));
    }

    #[test]
    fn rustc_code_uses_exact_canonical_layer() {
        let diag = from_rustc(Some("E0277"), "the trait bound `X: Y` is not satisfied");
        let key = diag.layered_key();
        assert_eq!(key.layer, FingerprintLayer::CanonicalCode);
        assert_eq!(key.key, "rustc:E0277");
        assert_eq!(diag.tool, DiagnosticTool::Rustc);
    }

    #[test]
    fn codeless_shell_falls_back_to_template_layer() {
        let diag = from_shell(1, "Segmentation fault (core dumped) at 0xdeadbeef");
        let key = diag.layered_key();
        assert_eq!(key.layer, FingerprintLayer::MessageTemplate);
        assert!(key.key.starts_with("shell:tmpl:blake3:"));
    }

    #[test]
    fn ee_and_cargo_canonicalizers_are_deterministic() {
        let first = from_ee_error(
            "migration_required",
            "schema migration required at /x/ee.db",
        );
        let second = from_ee_error(
            "migration_required",
            "schema migration required at /y/ee.db",
        );
        assert_eq!(first.layered_key(), second.layered_key());
        assert_eq!(first.layered_key().key, "ee:migration_required");

        let cargo = from_cargo(Some("E0599"), "no method named `frobnicate` found");
        assert_eq!(cargo.layered_key().key, "cargo:E0599");
    }

    #[test]
    fn rch_blocker_combines_kind_and_stage() {
        let diag = from_rch_blocker(
            "capacity_or_timeout",
            "execute",
            "remote worker unavailable",
        );
        assert_eq!(diag.layered_key().key, "rch:capacity_or_timeout:execute");
        let kind_only = from_rch_blocker("path_dep_missing", "", "frankensearch not materialized");
        assert_eq!(kind_only.layered_key().key, "rch:path_dep_missing");
    }

    #[test]
    fn simhash_tail_groups_near_duplicate_codeless_messages() {
        // Numbers are masked, so these two collapse to one template -> identical
        // simhash (the long-tail layer treats them as the same class).
        let a = from_shell(1, "connection refused after 3 retries on port 8080").simhash_tail();
        let b = from_shell(1, "connection refused after 9 retries on port 9090").simhash_tail();
        assert_eq!(simhash_hamming_distance(a, b), 0);
        assert!(simhash_tail_matches(a, b, SIMHASH_TAIL_MAX_DISTANCE));

        // A genuinely different failure stays outside the tail distance.
        let c =
            from_shell(1, "out of memory while allocating a large buffer region").simhash_tail();
        assert!(!simhash_tail_matches(a, c, SIMHASH_TAIL_MAX_DISTANCE));
    }

    #[test]
    fn redact_diagnostic_strips_secrets_and_never_keys_on_raw() {
        let secret = "sk-proj-ABCDEF1234567890ABCDEF1234567890";
        let raw = format!("auth failed api_key={secret} opening /Users/alice/app/main.rs:5");
        let red = redact_diagnostic(DiagnosticTool::Ee, Some("auth_rejected"), &raw);
        assert!(
            !red.redacted_message.contains(secret),
            "raw secret must not survive into the stored message"
        );
        assert!(
            !red.fingerprint_key.key.contains(secret),
            "raw secret must not leak into the fingerprint key"
        );
        assert!(
            red.redacted_span_count >= 1,
            "at least one secret span redacted"
        );
        assert_eq!(red.fingerprint_key.key, "ee:auth_rejected");
    }

    #[test]
    fn redact_diagnostic_is_clean_when_no_secrets() {
        let red = redact_diagnostic(DiagnosticTool::Rustc, Some("E0382"), "use of moved value x");
        assert_eq!(red.redacted_span_count, 0);
        assert_eq!(red.fingerprint_key.layer, FingerprintLayer::CanonicalCode);
        assert_eq!(red.fingerprint_key.key, "rustc:E0382");
        assert_eq!(red.redacted_message, "use of moved value x");
    }

    #[test]
    fn plan_error_repair_links_dedups_orders_and_skips_blanks() {
        let links = plan_error_repair_links(
            "rustc:E0277",
            &[
                "mem_fix".to_string(),
                "mem_fix".to_string(),
                "  ".to_string(),
            ],
            &["proof_1".to_string()],
            &["out_1".to_string()],
            &[],
        );
        // 1 repair (deduped, blank skipped) + 1 proof + 1 outcome, ordered by kind.
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].kind, ErrorRepairLinkKind::Outcome);
        assert_eq!(links[0].target_id, "out_1");
        assert_eq!(links[1].kind, ErrorRepairLinkKind::Proof);
        assert_eq!(links[2].kind, ErrorRepairLinkKind::Repair);
        assert_eq!(links[2].target_id, "mem_fix");
        assert!(
            links
                .iter()
                .all(|link| link.fingerprint_key == "rustc:E0277")
        );
    }

    #[test]
    fn error_repair_link_kind_roundtrips() {
        for kind in [
            ErrorRepairLinkKind::Repair,
            ErrorRepairLinkKind::Proof,
            ErrorRepairLinkKind::Outcome,
            ErrorRepairLinkKind::CurationCandidate,
        ] {
            assert_eq!(ErrorRepairLinkKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ErrorRepairLinkKind::parse("nope"), None);
    }

    #[test]
    fn error_fingerprint_derives_from_canonical_diagnostic() {
        let canonical = from_rustc(Some("E0277"), "the trait bound `Foo: Bar` is not satisfied");
        let fingerprint = ErrorFingerprint::from_canonical(&canonical);
        assert_eq!(fingerprint.tool, DiagnosticTool::Rustc);
        assert_eq!(fingerprint.canonical_code.as_deref(), Some("E0277"));
        assert!(
            fingerprint
                .message_template_signature
                .starts_with("blake3:")
        );
        assert_eq!(fingerprint.stderr_simhash, canonical.simhash_tail());
        assert!(fingerprint.location_shape.is_none());
        assert!(fingerprint.version_hints.is_empty());
    }

    #[test]
    fn error_fingerprint_layered_key_matches_canonical() {
        let coded = from_rustc(Some("E0277"), "trait bound not satisfied");
        assert_eq!(
            ErrorFingerprint::from_canonical(&coded).layered_key(),
            coded.layered_key(),
            "coded fingerprint key must match the canonical layered key",
        );
        let codeless = from_shell(1, "permission denied opening `/etc/secret`");
        assert_eq!(
            ErrorFingerprint::from_canonical(&codeless).layered_key(),
            codeless.layered_key(),
            "code-less fingerprint key must match the canonical template key",
        );
    }

    #[test]
    fn error_fingerprint_derived_document_is_deterministic_and_redacted() {
        let canonical = from_rustc(Some("E0277"), "cannot find value `secret_val` in scope");
        let fingerprint = ErrorFingerprint::from_canonical(&canonical)
            .with_location_shape("<path>:<num>:<num>")
            .with_version_hints(vec![
                "rustc 1.x".to_string(),
                " rustc 1.x ".to_string(),
                String::new(),
            ]);
        let doc_first = fingerprint.derived_document_text();
        let doc_second = fingerprint.derived_document_text();
        assert_eq!(
            doc_first, doc_second,
            "derived document text must be deterministic"
        );
        assert!(doc_first.contains("tool:rustc"));
        assert!(doc_first.contains("code:E0277"));
        assert!(doc_first.contains("template:blake3:"));
        assert!(doc_first.contains("location:<path>:<num>:<num>"));
        assert!(
            !doc_first.contains("secret_val"),
            "derived document must carry signatures only, never raw content",
        );
        assert_eq!(
            fingerprint.version_hints,
            vec!["rustc 1.x".to_string()],
            "version hints must be trimmed, deduplicated, and sorted",
        );
    }

    #[test]
    fn error_fingerprint_blank_location_is_dropped() {
        let canonical = from_cargo(Some("E0277"), "trait bound not satisfied");
        let fingerprint = ErrorFingerprint::from_canonical(&canonical).with_location_shape("   ");
        assert!(fingerprint.location_shape.is_none());
    }

    #[test]
    fn canonicalization_is_idempotent() {
        // ADR 0057 verification: re-canonicalizing an already-masked template is a
        // fixed point, so re-ingesting stored material can never drift the class.
        for message in [
            "cannot find value `foo` in path /a/b/c.rs:12:5",
            "Segmentation fault at 0xDEADBEEF after 4096 bytes",
            "mismatched types expected `Vec<u8>` found `String`",
            "",
            "   ",
        ] {
            let once = canonical_message_template(message);
            let twice = canonical_message_template(&once);
            assert_eq!(
                once, twice,
                "canonicalization must be idempotent for {message:?}"
            );
        }
    }

    #[test]
    fn distinct_codes_do_not_collide() {
        // ADR 0057 verification: the exact (tool, canonical_code) layer must keep
        // distinct codes and distinct tools in distinct keys (no dedup collision).
        let keys = [
            from_rustc(Some("E0277"), "a").layered_key().key,
            from_rustc(Some("E0382"), "a").layered_key().key,
            from_cargo(Some("E0277"), "a").layered_key().key,
            from_ee_error("auth_rejected", "a").layered_key().key,
            from_rch_blocker("capacity_or_timeout", "selection", "a")
                .layered_key()
                .key,
        ];
        let mut unique = keys.to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            keys.len(),
            "distinct (tool, code) fingerprints must not collide: {keys:?}"
        );
    }

    #[test]
    fn canonicalizer_does_not_panic_on_adversarial_input() {
        // ADR 0057 verification (fuzz-lite): the canonicalizer + redaction must be
        // total over hostile text — no panics on empty/unicode/control/unbalanced
        // backtick/very-long inputs.
        let long = "x ".repeat(10_000);
        for raw in [
            "",
            "   \t\n  ",
            "`unbalanced backtick start",
            "café ☃ 𝕏 \u{0007}\u{0000} control",
            "0x 0xZZ /a/b ::: ---",
            long.as_str(),
        ] {
            let template = canonical_message_template(raw);
            // Re-run through the keying + redaction paths to exercise the chain.
            let _ = from_rustc(Some("E0277"), raw).layered_key();
            let _ = from_shell(1, raw).layered_key();
            let redacted = redact_diagnostic(DiagnosticTool::Ee, None, raw);
            // Idempotent even for hostile input.
            assert_eq!(template, canonical_message_template(&template));
            assert!(!redacted.fingerprint_key.key.is_empty());
        }
    }

    #[test]
    fn redact_to_fingerprint_produces_redaction_safe_record() {
        let secret = "sk-proj-ABCDEF1234567890ABCDEF1234567890";
        let raw = format!("auth failed api_key={secret} opening /Users/alice/app/main.rs:5");
        let red = redact_to_fingerprint(DiagnosticTool::Ee, Some("auth_rejected"), &raw);

        // The persistable fingerprint + its derived document carry no raw secret
        // or raw path anywhere -- only signatures and masked shapes.
        let doc = red.fingerprint.derived_document_text();
        assert!(
            !doc.contains(secret),
            "derived document must not carry the raw secret"
        );
        assert!(
            !doc.contains("/Users/alice"),
            "derived document must not carry a raw path"
        );
        assert!(!red.fingerprint.message_template_signature.contains(secret));
        assert!(
            red.redacted_span_count >= 1,
            "at least one secret span redacted"
        );
        assert_eq!(red.fingerprint.layered_key().key, "ee:auth_rejected");
    }

    #[test]
    fn redact_to_fingerprint_dedups_class_and_is_clean_without_secrets() {
        // Different identifiers, same error class -> identical redaction-safe
        // fingerprint; no secrets -> zero redacted spans.
        let a = redact_to_fingerprint(
            DiagnosticTool::Rustc,
            Some("E0277"),
            "trait bound `X: Y` not satisfied",
        );
        let b = redact_to_fingerprint(
            DiagnosticTool::Rustc,
            Some("E0277"),
            "trait bound `Z: W` not satisfied",
        );
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.redacted_span_count, 0);
        assert!(a.redaction_reasons.is_empty());
    }
}
