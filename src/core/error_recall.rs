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
}

impl FingerprintLayer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CanonicalCode => "canonical_code",
            Self::MessageTemplate => "message_template",
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

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticTool, FingerprintLayer, canonical_message_template, from_cargo, from_ee_error,
        from_rch_blocker, from_rustc, from_shell,
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
}
