//! Producer-identity normalization for outcome attribution (bd-pxi7f).
//!
//! Audit details, derivation metadata, and Agent Mail traffic all carry
//! producer identifiers in slightly different spellings: tmux pane ids
//! (`cc_1`, `%4`), Agent Mail names (`PinkOriole`), harness names
//! (`claude-code`, `codex-cli`), human handles (`jeff-emanuel`,
//! `jeff@example.com`), and structured contexts
//! (`reflection:gaps:claude-opus-4-7`, `review_session:bootstrap`).
//!
//! Before harmful-feedback statistics, quarantine, or trust adjustments
//! consume one of those strings, they need to share a canonical key — or
//! `cc_1` and `Cc_1`, `PinkOriole` and `pinkoriole`, will look like
//! different producers and accumulate independent (and therefore too weak
//! to trip quarantine) bad-outcome counts.
//!
//! This module declares `normalize_producer_id` and the supporting
//! `NormalizedProducerId` / `ProducerIdKind` types. It does **not** yet
//! wire the normalized ids into outcome resolution — that wiring belongs
//! with `record_outcome` and `harmful_feedback` paths in
//! `src/core/outcome.rs` and is tracked by the broader bd-pxi7f goal.
//!
//! Migration plan for existing audit rows lives at
//! `docs/migration_producer_normalization.md`.

use std::fmt;

/// What kind of producer the input string most likely names.
///
/// Detection is best-effort and conservative: if a string doesn't look
/// unambiguously like one of the structured forms, it falls into
/// `Unknown` with a sanitized canonical key rather than being forced
/// into the wrong bucket.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ProducerIdKind {
    /// tmux pane identifier (`cc_1`, `cod_3`, `%4`, ...) used by the
    /// orchestrator to scope a swarm agent.
    AgentPane,
    /// Agent Mail handle (`PinkOriole`, `FoggyBison`, ...). Persistent
    /// across panes within a project.
    AgentName,
    /// Harness program name (`claude-code`, `codex-cli`, `cursor`, ...).
    Harness,
    /// Human producer — email or username.
    Human,
    /// Structured reflection context (`reflection:<kind>:<model>`).
    ReflectionContext,
    /// Other structured workflow id (`review_session:bootstrap`,
    /// `swarm:idea-wizards`, ...).
    Workflow,
    /// Anything that didn't match a known shape. Still gets a sanitized
    /// canonical key so attribution can group equal raw strings.
    Unknown,
}

impl ProducerIdKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentPane => "agent_pane",
            Self::AgentName => "agent_name",
            Self::Harness => "harness",
            Self::Human => "human",
            Self::ReflectionContext => "reflection_context",
            Self::Workflow => "workflow",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ProducerIdKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canonical producer identity used for outcome attribution. `canonical`
/// is the form persistent stores should index on; `kind` remains
/// explanatory metadata because producer-kind detection is heuristic.
/// `original` is preserved verbatim so audit trails remain traceable.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NormalizedProducerId {
    pub kind: ProducerIdKind,
    pub canonical: String,
    pub original: String,
}

impl NormalizedProducerId {
    /// Render the stable attribution key for feedback counters and
    /// quarantine statistics.
    ///
    /// This intentionally uses the canonical identity without the
    /// best-effort kind prefix so casing variants or heuristic kind drift
    /// (for example `PinkOriole` vs `pinkoriole`) cannot fragment one
    /// producer into multiple too-small buckets.
    #[must_use]
    pub fn attribution_key(&self) -> String {
        self.canonical.clone()
    }

    /// True when the input was empty or pure whitespace.
    #[must_use]
    pub fn is_unknown_empty(&self) -> bool {
        self.kind == ProducerIdKind::Unknown && self.original.trim().is_empty()
    }
}

// ---- Known harness names ---------------------------------------------
//
// Lowercase, kebab-case canonical forms. Matched case-insensitively
// after `_` and whitespace are converted to `-`. Add a new entry here
// when a new harness starts producing durable memories — adding to the
// table is preferable to letting `cc_1` and `cc1` drift apart silently.
const KNOWN_HARNESSES: &[&str] = &[
    "claude-code",
    "claude-cli",
    "codex-cli",
    "cursor",
    "aider",
    "gemini-cli",
    "jetbrains",
    "vscode",
    "neovim",
];

const KNOWN_WORKFLOW_PREFIXES: &[&str] = &[
    "review_session",
    "swarm",
    "verification_run",
    "audit_run",
    "outcome_run",
];

const REFLECTION_PREFIX: &str = "reflection";

/// Normalize a raw producer-identity string into a canonical attribution
/// key. The function never returns `Err`: unknown shapes fall through to
/// `ProducerIdKind::Unknown` with a sanitized canonical form so callers
/// can still group equal raw strings together.
#[must_use]
pub fn normalize_producer_id(raw: &str) -> NormalizedProducerId {
    let original = raw.to_owned();
    let trimmed = raw.trim();

    if trimmed.is_empty() {
        return NormalizedProducerId {
            kind: ProducerIdKind::Unknown,
            canonical: String::new(),
            original,
        };
    }

    // Pane id: `%4` or `%12`
    if let Some(rest) = trimmed.strip_prefix('%') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return NormalizedProducerId {
                kind: ProducerIdKind::AgentPane,
                canonical: format!("pane_{rest}"),
                original,
            };
        }
    }

    // Pane id: `cc_1`, `cod_12`, `cl_3` — short program prefix followed
    // by `_<digits>`. Accept `-` as a separator too so `cc-1` collapses
    // onto `cc_1`.
    if let Some((prefix, suffix)) = split_pane_prefix(trimmed) {
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return NormalizedProducerId {
                kind: ProducerIdKind::AgentPane,
                canonical: format!("{}_{}", prefix.to_ascii_lowercase(), suffix),
                original,
            };
        }
    }

    // Structured kind:tag forms — reflection / workflow.
    if let Some((head, rest)) = trimmed.split_once(':') {
        let head_norm = head.trim().to_ascii_lowercase();
        if head_norm == REFLECTION_PREFIX {
            return NormalizedProducerId {
                kind: ProducerIdKind::ReflectionContext,
                canonical: format!("{}:{}", REFLECTION_PREFIX, sanitize_colon_tail(rest)),
                original,
            };
        }
        if KNOWN_WORKFLOW_PREFIXES
            .iter()
            .any(|prefix| *prefix == head_norm.as_str())
        {
            return NormalizedProducerId {
                kind: ProducerIdKind::Workflow,
                canonical: format!("{head_norm}:{}", sanitize_colon_tail(rest)),
                original,
            };
        }
        // Unknown structured prefix still groups under Workflow so the
        // attribution key is stable.
        return NormalizedProducerId {
            kind: ProducerIdKind::Workflow,
            canonical: format!("{head_norm}:{}", sanitize_colon_tail(rest)),
            original,
        };
    }

    // Human: anything with `@` (email-like).
    if trimmed.contains('@') {
        return NormalizedProducerId {
            kind: ProducerIdKind::Human,
            canonical: trimmed.to_ascii_lowercase(),
            original,
        };
    }

    // Harness match (case- and separator-insensitive).
    let harness_candidate = trimmed.to_ascii_lowercase().replace([' ', '_'], "-");
    if KNOWN_HARNESSES
        .iter()
        .any(|h| *h == harness_candidate.as_str())
    {
        return NormalizedProducerId {
            kind: ProducerIdKind::Harness,
            canonical: harness_candidate,
            original,
        };
    }

    // Agent Mail handle heuristic: a single ASCII alpha token with at
    // least one uppercase letter and at least one lowercase letter,
    // optionally followed by digits. Examples: PinkOriole, FoggyBison,
    // Cc2Helper.
    if looks_like_agent_mail_handle(trimmed) {
        return NormalizedProducerId {
            kind: ProducerIdKind::AgentName,
            canonical: trimmed.to_ascii_lowercase(),
            original,
        };
    }

    // Fall back: treat as a human handle if it's a plausible username
    // (alphanumeric plus `_`/`-`/`.`), else Unknown.
    let sanitized = sanitize_fallback(trimmed);
    if !sanitized.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return NormalizedProducerId {
            kind: ProducerIdKind::Human,
            canonical: sanitized,
            original,
        };
    }
    NormalizedProducerId {
        kind: ProducerIdKind::Unknown,
        canonical: if sanitized.is_empty() {
            hashed_unknown_canonical(trimmed)
        } else {
            sanitized
        },
        original,
    }
}

fn split_pane_prefix(s: &str) -> Option<(&str, &str)> {
    // Accept both `cc_1` and `cc-1` forms. Prefix is 2-3 ASCII letters
    // (programmatic short name like `cc`, `cod`, `cl`).
    let bytes = s.as_bytes();
    for sep_pos in 1..bytes.len() {
        let c = bytes[sep_pos] as char;
        if c == '_' || c == '-' {
            let prefix = &s[..sep_pos];
            let suffix = &s[sep_pos + 1..];
            if (2..=3).contains(&prefix.len()) && prefix.chars().all(|c| c.is_ascii_alphabetic()) {
                return Some((prefix, suffix));
            }
            return None;
        }
    }
    None
}

fn looks_like_agent_mail_handle(s: &str) -> bool {
    let mut has_upper = false;
    let mut has_lower = false;
    let mut all_alnum = true;
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            has_upper = true;
        } else if c.is_ascii_lowercase() {
            has_lower = true;
        } else if !c.is_ascii_digit() {
            all_alnum = false;
            break;
        }
    }
    all_alnum && has_upper && has_lower
}

fn sanitize_colon_tail(rest: &str) -> String {
    rest.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == ':' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn sanitize_fallback(s: &str) -> String {
    s.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(['_', '-'])
        .to_owned()
}

fn hashed_unknown_canonical(s: &str) -> String {
    let hash = blake3::hash(s.as_bytes()).to_hex().to_string();
    format!("unknown_{}", &hash[..12])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> NormalizedProducerId {
        normalize_producer_id(s)
    }

    #[test]
    fn empty_input_is_unknown_empty() {
        let n = norm("");
        assert_eq!(n.kind, ProducerIdKind::Unknown);
        assert_eq!(n.canonical, "");
        assert!(n.is_unknown_empty());
    }

    #[test]
    fn whitespace_only_is_unknown_empty() {
        let n = norm("   \t\n");
        assert_eq!(n.kind, ProducerIdKind::Unknown);
        assert!(n.is_unknown_empty());
    }

    #[test]
    fn pane_id_with_underscore() {
        let n = norm("cc_1");
        assert_eq!(n.kind, ProducerIdKind::AgentPane);
        assert_eq!(n.canonical, "cc_1");
        assert_eq!(n.original, "cc_1");
    }

    #[test]
    fn pane_id_with_hyphen_collapses_to_underscore() {
        let n = norm("cc-1");
        assert_eq!(n.kind, ProducerIdKind::AgentPane);
        assert_eq!(n.canonical, "cc_1");
    }

    #[test]
    fn pane_id_uppercase_normalizes() {
        let n = norm("CC_1");
        assert_eq!(n.kind, ProducerIdKind::AgentPane);
        assert_eq!(n.canonical, "cc_1");
    }

    #[test]
    fn pane_id_cod_prefix() {
        let n = norm("cod_2");
        assert_eq!(n.kind, ProducerIdKind::AgentPane);
        assert_eq!(n.canonical, "cod_2");
    }

    #[test]
    fn pane_id_three_letter_prefix() {
        let n = norm("cli_42");
        assert_eq!(n.kind, ProducerIdKind::AgentPane);
        assert_eq!(n.canonical, "cli_42");
    }

    #[test]
    fn pane_id_percent_form_canonicalizes_to_pane_n() {
        let n = norm("%4");
        assert_eq!(n.kind, ProducerIdKind::AgentPane);
        assert_eq!(n.canonical, "pane_4");
    }

    #[test]
    fn pane_id_too_many_letters_is_not_a_pane() {
        let n = norm("agent_1");
        // 5-letter prefix is not a pane id; falls back to Human (looks
        // like a plausible username).
        assert_ne!(n.kind, ProducerIdKind::AgentPane);
    }

    #[test]
    fn agent_mail_name_lowercases() {
        let n = norm("PinkOriole");
        assert_eq!(n.kind, ProducerIdKind::AgentName);
        assert_eq!(n.canonical, "pinkoriole");
        assert_eq!(n.original, "PinkOriole");
    }

    #[test]
    fn agent_mail_name_with_digits() {
        let n = norm("FoggyBison2");
        assert_eq!(n.kind, ProducerIdKind::AgentName);
        assert_eq!(n.canonical, "foggybison2");
    }

    #[test]
    fn harness_kebab_case_passes_through() {
        let n = norm("claude-code");
        assert_eq!(n.kind, ProducerIdKind::Harness);
        assert_eq!(n.canonical, "claude-code");
    }

    #[test]
    fn harness_underscore_normalizes_to_kebab() {
        let n = norm("claude_code");
        assert_eq!(n.kind, ProducerIdKind::Harness);
        assert_eq!(n.canonical, "claude-code");
    }

    #[test]
    fn harness_space_normalizes_to_kebab() {
        let n = norm("Claude Code");
        assert_eq!(n.kind, ProducerIdKind::Harness);
        assert_eq!(n.canonical, "claude-code");
    }

    #[test]
    fn harness_codex_cli() {
        let n = norm("codex-cli");
        assert_eq!(n.kind, ProducerIdKind::Harness);
        assert_eq!(n.canonical, "codex-cli");
    }

    #[test]
    fn human_email_lowercases() {
        let n = norm("Jeff.Emanuel@example.com");
        assert_eq!(n.kind, ProducerIdKind::Human);
        assert_eq!(n.canonical, "jeff.emanuel@example.com");
    }

    #[test]
    fn human_handle_no_special_chars() {
        let n = norm("jeff-emanuel");
        assert_eq!(n.kind, ProducerIdKind::Human);
        assert_eq!(n.canonical, "jeff-emanuel");
    }

    #[test]
    fn reflection_context_structured() {
        let n = norm("reflection:gaps:claude-opus-4-7");
        assert_eq!(n.kind, ProducerIdKind::ReflectionContext);
        assert_eq!(n.canonical, "reflection:gaps:claude-opus-4-7");
    }

    #[test]
    fn reflection_context_uppercase_normalizes() {
        let n = norm("REFLECTION:Gaps:Claude-Opus-4-7");
        assert_eq!(n.kind, ProducerIdKind::ReflectionContext);
        assert_eq!(n.canonical, "reflection:gaps:claude-opus-4-7");
    }

    #[test]
    fn workflow_known_prefix_review_session() {
        let n = norm("review_session:bootstrap");
        assert_eq!(n.kind, ProducerIdKind::Workflow);
        assert_eq!(n.canonical, "review_session:bootstrap");
    }

    #[test]
    fn workflow_unknown_structured_prefix_still_groups() {
        let n = norm("custom_lane:experiment_a");
        assert_eq!(n.kind, ProducerIdKind::Workflow);
        assert_eq!(n.canonical, "custom_lane:experiment_a");
    }

    #[test]
    fn attribution_key_disambiguates_distinct_canonical_fragments() {
        let pane = norm("cc_1");
        let agent = norm("Cc1"); // matches agent-mail PascalCase shape
        assert_ne!(pane.attribution_key(), agent.attribution_key());
    }

    #[test]
    fn attribution_key_format() {
        let n = norm("PinkOriole");
        assert_eq!(n.attribution_key(), "pinkoriole");
    }

    #[test]
    fn equal_raw_strings_canonicalize_equally() {
        let a = norm("PinkOriole");
        let b = norm("pinkoriole");
        assert_eq!(a.canonical, b.canonical);
        assert_eq!(a.attribution_key(), b.attribution_key());
    }

    #[test]
    fn capitalized_human_handle_shares_lowercase_attribution_bucket() {
        let capitalized = norm("Jeff");
        let lowercase = norm("jeff");
        assert_eq!(capitalized.canonical, lowercase.canonical);
        assert_eq!(capitalized.attribution_key(), lowercase.attribution_key());
    }

    #[test]
    fn unknown_with_punctuation_preserves_sanitized_canonical() {
        let n = norm("weird/thing!");
        // Non-alphanum punctuation that's not / \ outside the username
        // charset → Unknown with sanitized canonical.
        assert_eq!(n.kind, ProducerIdKind::Unknown);
        assert!(!n.canonical.is_empty());
        assert!(!n.canonical.contains('/'));
        assert!(!n.canonical.contains('!'));
    }

    #[test]
    fn punctuation_only_unknown_gets_non_empty_stable_key() {
        let first = norm("!!!");
        let same = norm(" !!! ");
        let different = norm("???");
        let blank = norm("   ");

        assert_eq!(first.kind, ProducerIdKind::Unknown);
        assert!(first.canonical.starts_with("unknown_"));
        assert!(!first.is_unknown_empty());
        assert_eq!(first.canonical, same.canonical);
        assert_ne!(first.canonical, different.canonical);
        assert_ne!(first.canonical, blank.canonical);
    }

    #[test]
    fn kind_as_str_is_snake_case() {
        for kind in [
            ProducerIdKind::AgentPane,
            ProducerIdKind::AgentName,
            ProducerIdKind::Harness,
            ProducerIdKind::Human,
            ProducerIdKind::ReflectionContext,
            ProducerIdKind::Workflow,
            ProducerIdKind::Unknown,
        ] {
            let s = kind.as_str();
            assert!(s.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
    }

    #[test]
    fn original_field_preserves_input() {
        let n = norm("  PinkOriole  ");
        assert_eq!(n.original, "  PinkOriole  ");
        assert_eq!(n.canonical, "pinkoriole");
    }
}
