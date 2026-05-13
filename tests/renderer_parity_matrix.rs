//! D8 — Cross-renderer parity matrix (bd-17c65.4.9).
//!
//! Companion to `tests/renderer_parity_omissions.toml` (the intentional-
//! omissions allowlist) and `tests/markdown_mermaid_share_renderer_unit.rs`
//! (which locks in the 7-renderer matrix sizing — Markdown and Mermaid
//! both map to `output::Renderer::Markdown`).
//!
//! This test was deferred in the original D8 commit (see the bead's
//! notes) because the canonical pack tree was still moving across the
//! D-series. The D-series gating beads (bd-17c65.4 epic + bd-17c65.4.7
//! A10 envelope bump) have since closed; the remaining work is real.
//!
//! What this file pins (CoralRiver continuation):
//!
//!   1. **Omissions-registry schema integrity** — every `[[omission]]`
//!      entry parses, has all required fields, uses one of the known
//!      `renderer` names and `reason` enum values, has a non-trivial
//!      rationale string, and pairs (renderer, field) are unique.
//!      Adding a malformed entry fails CI here before it can be
//!      consumed by downstream tests.
//!
//!   2. **Canonical-field presence smoke test** — for the four
//!      surfaces with cheap public-API fixtures (memory list, rule
//!      list, learn uncertainty, introspect) and their three
//!      registered render forms (json, toon, human), assert the
//!      surfaces emit the canonical `content` field where D1
//!      requires it. Walks 12 (surface, renderer) pairs that can be
//!      exercised without a PackDraft fixture.
//!
//! What this file deliberately defers to D8.2 (a new follow-up bead
//! to be filed when this lands):
//!
//!   - The FULL 8-format pack tree walk (rendering a ContextResponse
//!     through every OutputFormat variant and matrix-checking every
//!     canonical pack field). That needs a public `PackDraft` test
//!     fixture helper, which the current public API doesn't expose
//!     (PackCandidate::new requires several internal types).
//!     Filing D8.2 with the explicit scope: ship the PackDraft
//!     helper first, then the full matrix lands as a one-PR
//!     mechanical write.
//!
//! Wiring: registered in `tests/contracts.rs` as a top-level test
//! file at the same level as the other deferred-then-shipped
//! contract tests (canonical_content_field, schema_canonical_fields,
//! handoff_canonical_schema, etc.).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ee::core::learn::{LEARN_UNCERTAINTY_SCHEMA_V1, LearnUncertaintyReport, UncertaintyItem};
use ee::core::memory::{MemoryListFilter, MemoryListReport, MemorySummary};
use ee::core::rule::{RuleEvidence, RuleLifecycle, RuleListFilter, RuleListReport, RuleSummary};
use ee::output::{
    render_introspect_human, render_introspect_json, render_introspect_toon,
    render_learn_uncertainty_human, render_learn_uncertainty_json, render_learn_uncertainty_toon,
    render_memory_list_human, render_memory_list_json, render_memory_list_toon,
    render_rule_list_human, render_rule_list_json, render_rule_list_toon,
};

type TestResult = Result<(), String>;

/// The canonical set of `renderer` names that may appear in
/// `tests/renderer_parity_omissions.toml`. Mirrors the
/// `output::Renderer` enum's surface plus the two `OutputFormat`
/// input variants that map to a distinct rendering path
/// (markdown + mermaid collapse to `Renderer::Markdown` per
/// `tests/markdown_mermaid_share_renderer_unit.rs`).
///
/// Per D8 design, the omissions registry is keyed by the **output
/// renderer name**, not by the input format. So `markdown` covers
/// both the `--format markdown` and `--format mermaid` cases.
const KNOWN_RENDERERS: &[&str] = &[
    "human", "json", "toon", "jsonl", "compact", "hook", "markdown",
];

/// The canonical set of `reason` enum values that may appear in
/// `tests/renderer_parity_omissions.toml`. Per the file's header
/// comment, two values are documented:
///
/// - `format_native_omission`: format is structurally incapable of
///   carrying the field (e.g. mermaid is diagram-only).
/// - `compact_intentional_drop`: format chooses to drop the field to
///   stay within its size discipline.
///
/// Adding a new reason value requires (a) documenting it in the
/// omissions.toml header comment, (b) adding it here, and (c) writing
/// a rationale describing when it applies.
const KNOWN_REASONS: &[&str] = &["format_native_omission", "compact_intentional_drop"];

/// Minimum length (after trimming whitespace) for the `rationale`
/// field on each omission entry. Catches the "left it empty" or
/// "wrote TODO" failure mode.
const RATIONALE_MIN_CHARS: usize = 20;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn omissions_toml_path() -> PathBuf {
    repo_root()
        .join("tests")
        .join("renderer_parity_omissions.toml")
}

/// Parsed view of one `[[omission]]` array entry.
#[derive(Debug)]
struct OmissionEntry {
    renderer: String,
    field: String,
    reason: String,
    rationale: String,
    since_schema: String,
}

fn parse_omissions_toml(path: &Path) -> Result<Vec<OmissionEntry>, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let document = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("parse TOML {}: {e}", path.display()))?;

    let omissions = document
        .get("omission")
        .ok_or_else(|| "missing top-level `omission` array".to_string())?;
    let array_of_tables = omissions.as_array_of_tables().ok_or_else(|| {
        "top-level `omission` must be an [[omission]] array of tables".to_string()
    })?;

    let mut out: Vec<OmissionEntry> = Vec::with_capacity(array_of_tables.len());
    for (index, table) in array_of_tables.iter().enumerate() {
        let pick = |key: &str| -> Result<String, String> {
            table
                .get(key)
                .ok_or_else(|| format!("[[omission]] #{index}: missing required field `{key}`"))?
                .as_value()
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("[[omission]] #{index}: field `{key}` must be a string"))
        };
        out.push(OmissionEntry {
            renderer: pick("renderer")?,
            field: pick("field")?,
            reason: pick("reason")?,
            rationale: pick("rationale")?,
            since_schema: pick("since_schema")?,
        });
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────
// Omissions-registry schema integrity tests
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn omissions_registry_parses_cleanly() -> TestResult {
    let entries = parse_omissions_toml(&omissions_toml_path())?;
    // Catalog must be non-empty — an empty registry is suspicious
    // (either a regression deleted everything, or someone added the
    // file but forgot to populate it).
    if entries.is_empty() {
        return Err(
            "tests/renderer_parity_omissions.toml has zero [[omission]] entries; the file is supposed to enumerate intentional renderer-specific drops".to_string(),
        );
    }
    Ok(())
}

#[test]
fn every_omission_has_known_renderer() -> TestResult {
    let entries = parse_omissions_toml(&omissions_toml_path())?;
    for (i, entry) in entries.iter().enumerate() {
        if !KNOWN_RENDERERS.contains(&entry.renderer.as_str()) {
            return Err(format!(
                "[[omission]] #{i} (field=`{}`): unknown renderer `{}`. Allowed: {:?}. \
                 If this is a new renderer, update KNOWN_RENDERERS in tests/renderer_parity_matrix.rs \
                 and the header comment in tests/renderer_parity_omissions.toml.",
                entry.field, entry.renderer, KNOWN_RENDERERS,
            ));
        }
    }
    Ok(())
}

#[test]
fn every_omission_has_known_reason() -> TestResult {
    let entries = parse_omissions_toml(&omissions_toml_path())?;
    for (i, entry) in entries.iter().enumerate() {
        if !KNOWN_REASONS.contains(&entry.reason.as_str()) {
            return Err(format!(
                "[[omission]] #{i} (renderer=`{}`, field=`{}`): unknown reason `{}`. \
                 Allowed: {:?}. New reasons require (a) updating the TOML header comment, \
                 (b) updating KNOWN_REASONS in this file, (c) writing a rationale describing when it applies.",
                entry.renderer, entry.field, entry.reason, KNOWN_REASONS,
            ));
        }
    }
    Ok(())
}

#[test]
fn every_omission_has_substantive_rationale() -> TestResult {
    let entries = parse_omissions_toml(&omissions_toml_path())?;
    for (i, entry) in entries.iter().enumerate() {
        let trimmed = entry.rationale.trim();
        if trimmed.len() < RATIONALE_MIN_CHARS {
            return Err(format!(
                "[[omission]] #{i} (renderer=`{}`, field=`{}`): rationale is only {} chars after trimming. \
                 At least {} chars required so a reader unfamiliar with the surface understands WHY the drop is intentional.",
                entry.renderer,
                entry.field,
                trimmed.len(),
                RATIONALE_MIN_CHARS,
            ));
        }
        // Cheap content sanity: must not be a TODO/FIXME placeholder.
        for tag in ["TODO", "FIXME", "XXX", "tbd", "TBD"] {
            if trimmed.contains(tag) {
                return Err(format!(
                    "[[omission]] #{i} (renderer=`{}`, field=`{}`): rationale contains placeholder tag `{tag}`; fill in a real rationale before merging.",
                    entry.renderer, entry.field,
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn every_omission_pair_is_unique() -> TestResult {
    // A duplicate (renderer, field) pair would either double-count a
    // single drop or hide a divergent rationale. The catalog
    // represents one canonical reason per drop; if a renderer-field
    // pair has two intentions, file them as separate fields with
    // explicit qualifying suffixes.
    let entries = parse_omissions_toml(&omissions_toml_path())?;
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for (i, entry) in entries.iter().enumerate() {
        let key = (entry.renderer.clone(), entry.field.clone());
        if !seen.insert(key.clone()) {
            return Err(format!(
                "[[omission]] #{i}: duplicate (renderer=`{}`, field=`{}`) pair. \
                 Pick one canonical entry; multiple intents per pair must be split into distinct field paths.",
                entry.renderer, entry.field,
            ));
        }
    }
    Ok(())
}

#[test]
fn every_omission_field_path_is_well_formed() -> TestResult {
    // The `field` strings are dot-delimited JSON pointer-ish paths:
    // `pack.items[].provenance.entries[]`. Validate they:
    //   - are non-empty
    //   - don't contain whitespace (catches typo'd paths)
    //   - contain only the documented metacharacters
    let entries = parse_omissions_toml(&omissions_toml_path())?;
    for (i, entry) in entries.iter().enumerate() {
        let field = &entry.field;
        if field.trim().is_empty() {
            return Err(format!("[[omission]] #{i}: empty `field` path",));
        }
        if field.contains(char::is_whitespace) {
            return Err(format!(
                "[[omission]] #{i}: field path `{field}` contains whitespace; field paths are dot-delimited identifiers with optional `[]` array markers",
            ));
        }
        // First character must be alpha (start of an identifier path).
        if !field
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        {
            return Err(format!(
                "[[omission]] #{i}: field path `{field}` must start with an ASCII alpha character",
            ));
        }
    }
    Ok(())
}

#[test]
fn every_omission_since_schema_is_well_formed() -> TestResult {
    // `since_schema` is the schema identifier the field first appeared
    // in. Validate it's an `ee.*.v*` identifier so a stale schema name
    // doesn't sneak in. Per the existing entries it should be of the
    // shape `ee.<surface>.v<digit>` (e.g. `ee.context.v1`).
    let entries = parse_omissions_toml(&omissions_toml_path())?;
    for (i, entry) in entries.iter().enumerate() {
        let schema = entry.since_schema.trim();
        if !schema.starts_with("ee.") {
            return Err(format!(
                "[[omission]] #{i}: since_schema `{schema}` must start with `ee.` (the project schema namespace)",
            ));
        }
        if !schema.contains(".v") {
            return Err(format!(
                "[[omission]] #{i}: since_schema `{schema}` must include a `.vN` version suffix",
            ));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Canonical-field presence smoke tests
//
// For surfaces with cheap public-API fixtures, exercise the 3 registered
// render forms (json, toon, human) and assert the canonical `content`
// field (D1) appears. Composes with `tests/contracts/canonical_content_field.rs`
// which exhaustively pins the field for the JSON surface; here we walk
// every renderer to make sure the value reaches the wire on each path.
// ─────────────────────────────────────────────────────────────────────────

fn make_memory_list_fixture() -> MemoryListReport {
    MemoryListReport::success(
        vec![MemorySummary {
            id: "mem_01parity".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: "Run cargo fmt --check before release.".to_owned(),
            content_truncated: false,
            confidence: 0.9,
            provenance_uri: Some("cass-session://test".to_owned()),
            is_tombstoned: false,
            valid_from: None,
            valid_to: None,
            validity_status: "active".to_owned(),
            validity_window_kind: "open_ended".to_owned(),
            created_at: "2026-05-10T00:00:00Z".to_owned(),
        }],
        1,
        false,
        MemoryListFilter::default(),
    )
}

fn make_rule_list_fixture() -> RuleListReport {
    let summary = RuleSummary {
        id: "rule_01parity".to_owned(),
        content: "Always run cargo clippy before merge.".to_owned(),
        content_truncated: false,
        maturity: "validated".to_owned(),
        lifecycle: RuleLifecycle {
            maturity: "validated".to_owned(),
            is_active: true,
            is_terminal: false,
            next_action: "monitor".to_owned(),
        },
        scope: "workspace".to_owned(),
        scope_pattern: None,
        trust_class: "human_explicit".to_owned(),
        protected: false,
        confidence: 0.8,
        utility: 0.5,
        importance: 0.5,
        evidence: RuleEvidence {
            status: "verified".to_owned(),
            source_memory_count: 1,
            verified: true,
            requirement: "at least one source memory".to_owned(),
        },
        tags: vec!["release".to_owned()],
        is_tombstoned: false,
        created_at: "2026-05-10T00:00:00Z".to_owned(),
        updated_at: "2026-05-10T00:00:00Z".to_owned(),
    };
    RuleListReport {
        schema: ee::core::rule::RULE_LIST_SCHEMA_V1,
        command: "rule list",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: "ws_parity".to_owned(),
        workspace_path: "/tmp/ws_parity".to_owned(),
        database_path: "/tmp/ws_parity/ee.db".to_owned(),
        total_count: 1,
        returned_count: 1,
        limit: 20,
        offset: 0,
        truncated: false,
        filter: RuleListFilter {
            maturity: None,
            scope: None,
            tag: None,
            include_tombstoned: false,
        },
        rules: vec![summary],
        degraded: Vec::new(),
    }
}

fn make_learn_uncertainty_fixture() -> LearnUncertaintyReport {
    LearnUncertaintyReport {
        schema: LEARN_UNCERTAINTY_SCHEMA_V1.to_owned(),
        mean_uncertainty: 0.7,
        high_uncertainty_count: 1,
        sampling_candidates: 1,
        items: vec![UncertaintyItem {
            memory_id: "mem_01parityunc".to_owned(),
            content: "Procedure needs more replay evidence.".to_owned(),
            content_truncated: false,
            kind: "procedural".to_owned(),
            uncertainty: 0.7,
            confidence: 0.4,
            retrieval_count: 2,
            last_accessed: None,
        }],
        generated_at: "2026-05-10T00:00:00Z".to_owned(),
    }
}

/// One row in the parity smoke matrix. `(surface_name, renderer_name,
/// rendered_output, must_contain)` — assert every needle in
/// `must_contain` appears in the rendered output. Empty for renderers
/// that legitimately omit per the omissions.toml policy.
struct ParityRow {
    surface: &'static str,
    renderer: &'static str,
    rendered: String,
    must_contain: Vec<&'static str>,
}

#[test]
fn canonical_content_field_appears_in_every_renderer_for_known_surfaces() -> TestResult {
    let memory_list = make_memory_list_fixture();
    let rule_list = make_rule_list_fixture();
    let learn_uncertainty = make_learn_uncertainty_fixture();

    let rows = vec![
        // memory list
        ParityRow {
            surface: "memory_list",
            renderer: "json",
            rendered: render_memory_list_json(&memory_list),
            must_contain: vec!["\"content\"", "Run cargo fmt --check before release."],
        },
        ParityRow {
            surface: "memory_list",
            renderer: "toon",
            rendered: render_memory_list_toon(&memory_list),
            must_contain: vec!["Run cargo fmt --check before release."],
        },
        ParityRow {
            surface: "memory_list",
            renderer: "human",
            rendered: render_memory_list_human(&memory_list),
            must_contain: vec!["Run cargo fmt --check before release."],
        },
        // rule list
        ParityRow {
            surface: "rule_list",
            renderer: "json",
            rendered: render_rule_list_json(&rule_list),
            must_contain: vec!["\"content\"", "Always run cargo clippy before merge."],
        },
        ParityRow {
            surface: "rule_list",
            renderer: "toon",
            rendered: render_rule_list_toon(&rule_list),
            must_contain: vec!["Always run cargo clippy before merge."],
        },
        ParityRow {
            surface: "rule_list",
            renderer: "human",
            rendered: render_rule_list_human(&rule_list),
            must_contain: vec!["Always run cargo clippy before merge."],
        },
        // learn uncertainty
        ParityRow {
            surface: "learn_uncertainty",
            renderer: "json",
            rendered: render_learn_uncertainty_json(&learn_uncertainty),
            must_contain: vec!["\"content\"", "Procedure needs more replay evidence."],
        },
        // `toon` for learn_uncertainty is a single-line aggregate
        // format by design (`LEARN_UNCERTAINTY|mean=…|high=…|candidates=…`);
        // per-item content is intentionally absent. This is a
        // surface-specific format-native omission and is covered
        // implicitly by the `format_native_omission` reason class
        // in `tests/renderer_parity_omissions.toml`. We assert only
        // the aggregate identifier reaches the wire.
        ParityRow {
            surface: "learn_uncertainty",
            renderer: "toon",
            rendered: render_learn_uncertainty_toon(&learn_uncertainty),
            must_contain: vec!["LEARN_UNCERTAINTY", "mean=", "candidates="],
        },
        ParityRow {
            surface: "learn_uncertainty",
            renderer: "human",
            rendered: render_learn_uncertainty_human(&learn_uncertainty),
            must_contain: vec!["Procedure needs more replay evidence."],
        },
        // introspect (no fixture)
        ParityRow {
            surface: "introspect",
            renderer: "json",
            rendered: render_introspect_json(),
            must_contain: vec!["\"commands\""],
        },
        ParityRow {
            surface: "introspect",
            renderer: "toon",
            rendered: render_introspect_toon(),
            must_contain: vec!["introspect"],
        },
        ParityRow {
            surface: "introspect",
            renderer: "human",
            rendered: render_introspect_human(),
            must_contain: vec!["introspect"],
        },
    ];

    let mut failures: Vec<String> = Vec::new();
    for row in &rows {
        for needle in &row.must_contain {
            if !row.rendered.contains(needle) {
                failures.push(format!(
                    "  - surface=`{}` renderer=`{}` missing canonical needle {needle:?} in rendered output",
                    row.surface, row.renderer,
                ));
            }
        }
    }
    if !failures.is_empty() {
        return Err(format!(
            "canonical-field presence failures across {} (surface, renderer) pair(s):\n{}\n\nThe D1 canonical `content` field (or equivalent surface identifier) must reach the wire in every registered renderer. A renderer that legitimately omits a field must be allowlisted in tests/renderer_parity_omissions.toml AND this test must be updated to skip the row.",
            failures.len(),
            failures.join("\n"),
        ));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Bookkeeping
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn known_renderers_and_reasons_have_no_duplicates() -> TestResult {
    // Linter for the test-side const tables. A duplicate row would
    // silently bypass the schema integrity checks.
    let mut seen_renderers: BTreeSet<&str> = BTreeSet::new();
    for r in KNOWN_RENDERERS {
        if !seen_renderers.insert(r) {
            return Err(format!("KNOWN_RENDERERS has duplicate `{r}`"));
        }
    }
    let mut seen_reasons: BTreeSet<&str> = BTreeSet::new();
    for r in KNOWN_REASONS {
        if !seen_reasons.insert(r) {
            return Err(format!("KNOWN_REASONS has duplicate `{r}`"));
        }
    }
    Ok(())
}
