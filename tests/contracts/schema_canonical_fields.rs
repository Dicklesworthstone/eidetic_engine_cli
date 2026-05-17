//! D4 — Schema-drift audit for canonical JSON field names (bd-17c65.4.4).
//!
//! Walks the JSON output of every scanned `ee` surface looking for forbidden
//! field-name synonyms (e.g. `content_preview` / `memoryText` / `ws_path` when
//! D1 settled on `content` / `workspace_path`). Fails CI with an actionable
//! rename suggestion the moment a surface drifts.
//!
//! Why this exists: D1 (bd-17c65.4.1, commit 0b6474f) settled the canonical
//! field map across memory list, rule list, learn uncertainty, and why; D2
//! (bd-17c65.4.2, commit bcc7611) added JSON/Markdown render parity. Both
//! are point-in-time normalizations. Without an automated drift gate, the
//! next surface to add a list view ships with `content_preview` again and
//! we re-litigate D1 every milestone. This test is the gate.
//!
//! Scope (per the bead acceptance):
//! - Canonical field map defined in `CANONICAL_FIELDS` (logical concept →
//!   (canonical key, forbidden synonyms)).
//! - Recursive whole-tree scanner that flags forbidden-synonym occurrences
//!   anywhere in a response — list items, nested envelopes, command
//!   manifests, all of it.
//! - Applied to the surfaces D1 normalized (memory list, rule list, learn
//!   uncertainty) plus `ee introspect` (the command manifest itself).
//! - Positive-control test confirms the scanner actually catches a hand-
//!   constructed `content_preview` regression.
//!
//! Forbidden-synonym selection policy: only high-confidence drift markers
//! (e.g. `content_preview` is unambiguously a D1 regression). Two cases
//! are deliberately kept OFF the forbidden list to keep false positives at
//! zero:
//!
//!   1. **Ambiguous short names** that legitimately appear in unrelated
//!      contexts (`type`, `name`, `body` as in HTTP body).
//!
//!   2. **Pure case variants of the canonical key.** Several surfaces use
//!      `#[serde(rename_all = "camelCase")]` at the envelope level
//!      (`RuleListReport`, retrieval responses) and emit `workspacePath`
//!      where the canonical is `workspace_path`. That's a style policy
//!      separately enforced by `retrieval_field_naming.rs` —
//!      `schema_canonical_fields.rs` only flags *different-word* drift
//!      (`wsPath`, `memoryText`, `createdTs`), not case re-rendering.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::core::learn::{LEARN_UNCERTAINTY_SCHEMA_V1, LearnUncertaintyReport, UncertaintyItem};
use ee::core::memory::{MemoryListFilter, MemoryListReport, MemorySummary};
use ee::core::profile::{OperatingProfile, RuntimeProfileReport};
use ee::core::rule::{RuleEvidence, RuleLifecycle, RuleListFilter, RuleListReport, RuleSummary};
use ee::core::search::{ScoreSource, SearchHit, SearchReport, SearchSourceMode, SearchStatus};
use ee::models::{MemoryScope, MemoryScopeStats};
use ee::output::{
    render_introspect_json, render_learn_uncertainty_json, render_memory_list_json,
    render_rule_list_json,
};
use serde_json::Value;

type TestResult = Result<(), String>;

/// Canonical field map: each entry is `(logical_concept, canonical_key,
/// forbidden_synonyms)`. The scanner reports drift whenever any forbidden
/// synonym appears as an object key anywhere in a response tree.
///
/// **How to extend:** add a new row when D1+ settles a new canonical name,
/// or when a regression review uncovers a new synonym that snuck back in.
/// The bar for a forbidden entry is "no legitimate use elsewhere in the ee
/// JSON surface" — see the policy note in the module-level doc.
const CANONICAL_FIELDS: &[(&str, &str, &[&str])] = &[
    // Memory body text — the most-renamed field in the codebase. D1's
    // anchor concept. `content_preview` is the dominant pre-D1 spelling.
    (
        "memory body text",
        "content",
        &[
            "content_preview",
            "contentPreview",
            "memory_text",
            "memoryText",
            "body_text",
            "bodyText",
            "raw_content",
            "rawContent",
        ],
    ),
    // Truncation indicator that pairs with `content` on list/preview
    // surfaces. Snake_case in raw structs; camelCase variants (e.g.
    // `contentTruncated` on `RuleSummary`) are intentional and not in the
    // forbidden list.
    (
        "memory content truncation flag",
        "content_truncated",
        &[
            "is_truncated",
            "isTruncated",
            "was_truncated",
            "wasTruncated",
            "truncated_flag",
            "truncatedFlag",
        ],
    ),
    // Creation timestamp. RFC3339 string per AGENTS.md CLI output rules.
    (
        "creation timestamp",
        "created_at",
        &[
            "created_ts",
            "createdTs",
            "creation_ts",
            "creationTs",
            "create_time",
            "createTime",
            "creation_time",
            "creationTime",
            "created_time",
            "createdTime",
            "ts_created",
            "tsCreated",
        ],
    ),
    // Update timestamp.
    (
        "update timestamp",
        "updated_at",
        &[
            "updated_ts",
            "updatedTs",
            "modified_at",
            "modifiedAt",
            "modify_time",
            "modifyTime",
            "update_time",
            "updateTime",
            "modified_time",
            "modifiedTime",
            "ts_updated",
            "tsUpdated",
        ],
    ),
    // Workspace identifier — D1 fixed several surfaces that shipped `ws_id`
    // or bare `workspace`.
    (
        "workspace identifier",
        "workspace_id",
        &["ws_id", "wsId", "workspaceID", "workspaceid"],
    ),
    // Workspace filesystem path. `workspacePath` (pure camelCase variant)
    // is NOT forbidden because envelopes with serde `rename_all =
    // "camelCase"` legitimately emit that form (style is governed by
    // `retrieval_field_naming.rs`). Only different-word abbreviations are
    // listed.
    (
        "workspace filesystem path",
        "workspace_path",
        &["ws_path", "wsPath", "workspaceFsPath", "workspaceFspath"],
    ),
    // Provenance pointer (where this memory came from). Several pre-D1
    // surfaces shipped `source_uri`.
    (
        "provenance pointer URI",
        "provenance_uri",
        &[
            "provenance_url",
            "provenanceUrl",
            "source_uri",
            "sourceUri",
            "src_uri",
            "srcUri",
        ],
    ),
];

/// A single drift finding: a forbidden synonym appeared at a specific JSON
/// path within a scanned surface.
#[derive(Debug)]
struct DriftFinding {
    /// JSON pointer path like `/data/memories/0/content_preview`.
    json_path: String,
    /// The offending key as it appeared in the JSON.
    offending_key: String,
    /// Human-readable concept (e.g. "memory body text").
    logical_concept: String,
    /// What the key should have been (e.g. "content").
    canonical_key: String,
}

impl DriftFinding {
    fn render(&self) -> String {
        format!(
            "  • at `{path}` — key `{offender}` is a forbidden synonym for `{canonical}` ({concept}).\n    fix: rename `{offender}` → `{canonical}` (preserve value, add `{canonical}_truncated: true` if the value was elided)",
            path = self.json_path,
            offender = self.offending_key,
            canonical = self.canonical_key,
            concept = self.logical_concept,
        )
    }
}

/// Lookup table built from `CANONICAL_FIELDS` for O(1) per-key checks.
struct ForbiddenIndex {
    /// Maps `forbidden_key` → `(logical_concept, canonical_key)`.
    entries: Vec<(&'static str, &'static str, &'static str)>,
}

impl ForbiddenIndex {
    fn build() -> Self {
        let mut entries: Vec<(&'static str, &'static str, &'static str)> = Vec::new();
        for (logical, canonical, forbidden) in CANONICAL_FIELDS {
            for &bad in *forbidden {
                entries.push((bad, *logical, *canonical));
            }
        }
        Self { entries }
    }

    fn lookup(&self, key: &str) -> Option<(&'static str, &'static str)> {
        for (bad, logical, canonical) in &self.entries {
            if *bad == key {
                return Some((*logical, *canonical));
            }
        }
        None
    }
}

/// Recursively walk `value`, accumulating drift findings into `out`.
///
/// `path` is the JSON pointer prefix accumulated so far (RFC 6901 style:
/// `/data/memories/0`); pass `""` at the top level.
fn scan_for_drift(value: &Value, path: &str, index: &ForbiddenIndex, out: &mut Vec<DriftFinding>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if let Some((logical, canonical)) = index.lookup(key) {
                    out.push(DriftFinding {
                        json_path: format!("{path}/{key}"),
                        offending_key: key.clone(),
                        logical_concept: logical.to_owned(),
                        canonical_key: canonical.to_owned(),
                    });
                }
                let child_path = format!("{path}/{key}");
                scan_for_drift(child, &child_path, index, out);
            }
        }
        Value::Array(arr) => {
            for (i, child) in arr.iter().enumerate() {
                let child_path = format!("{path}/{i}");
                scan_for_drift(child, &child_path, index, out);
            }
        }
        _ => {}
    }
}

/// Parse `json_text`, scan it, and return `Ok` if no drift is found.
/// On failure, render an actionable multi-line error listing every offender
/// with its path and the suggested rename.
fn assert_no_drift(surface_name: &str, json_text: &str) -> TestResult {
    let value: Value = serde_json::from_str(json_text)
        .map_err(|e| format!("[{surface_name}] JSON did not parse: {e}"))?;
    let index = ForbiddenIndex::build();
    let mut findings: Vec<DriftFinding> = Vec::new();
    scan_for_drift(&value, "", &index, &mut findings);
    if findings.is_empty() {
        return Ok(());
    }
    let mut msg = format!(
        "[{surface_name}] schema-drift audit found {n} forbidden field-name synonym(s):\n",
        n = findings.len(),
    );
    for f in &findings {
        msg.push_str(&f.render());
        msg.push('\n');
    }
    msg.push_str(
        "\nWhy this fails CI: D1 (bd-17c65.4.1) settled the canonical field names listed above. A new surface that uses a different spelling forces every agent consumer to maintain a field-name translation map. The fix is mechanical: rename the offending key to its canonical form. If the synonym is legitimate in this context (rare; see policy note in tests/contracts/schema_canonical_fields.rs), remove it from CANONICAL_FIELDS' forbidden list with a justification comment.",
    );
    Err(msg)
}

// ─────────────────────────────────────────────────────────────────────────
// Scanner unit tests (positive + negative controls for the gate itself)
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn scanner_catches_hand_constructed_content_preview_drift() -> TestResult {
    // Synthesize a response that ships `content_preview` instead of the
    // canonical `content` — this is the exact regression D1 fixed and the
    // gate must catch it on sight.
    let drifty = serde_json::json!({
        "schema": "ee.synthetic.v1",
        "data": {
            "memories": [
                {
                    "id": "mem_01drift",
                    "content_preview": "should be `content` per D1",
                    "level": "procedural",
                    "kind": "rule"
                }
            ]
        }
    });
    let result = assert_no_drift("synthetic-drifty", &drifty.to_string());
    let err = match result {
        Err(e) => e,
        Ok(()) => {
            return Err("scanner failed to catch synthetic content_preview drift".to_string());
        }
    };
    if !err.contains("content_preview") {
        return Err(format!("error did not mention offending key: {err}"));
    }
    if !err.contains("content") {
        return Err(format!("error did not suggest canonical rename: {err}"));
    }
    if !err.contains("/data/memories/0/content_preview") {
        return Err(format!("error did not include JSON pointer path: {err}"));
    }
    Ok(())
}

#[test]
fn scanner_catches_all_forbidden_synonyms_across_one_response() -> TestResult {
    // Build a response that includes every category of forbidden synonym
    // simultaneously and assert each one is reported. This is the
    // backstop that prevents a future refactor from silently neutering
    // half the canonical map.
    let kitchen_sink = serde_json::json!({
        "memories": [{
            "memoryText": "drift 1",
            "createdTs": "2026-05-12T00:00:00Z",
            "modifiedAt": "2026-05-12T00:00:00Z",
            "wsId": "ws_x",
            "wsPath": "/tmp/ws_x",
            "sourceUri": "file:///x",
            "isTruncated": true
        }]
    });
    let result = assert_no_drift("synthetic-kitchen-sink", &kitchen_sink.to_string());
    let err = match result {
        Err(e) => e,
        Ok(()) => return Err("scanner missed kitchen-sink drift response".to_string()),
    };
    // Each canonical concept must surface at least one finding.
    for needle in [
        "memoryText",
        "createdTs",
        "modifiedAt",
        "wsId",
        "wsPath",
        "sourceUri",
        "isTruncated",
    ] {
        if !err.contains(needle) {
            return Err(format!(
                "scanner did not report `{needle}` in kitchen-sink output:\n{err}"
            ));
        }
    }
    Ok(())
}

#[test]
fn scanner_passes_known_canonical_response() -> TestResult {
    // The mirror of `scanner_catches_*`: a hand-built response that uses
    // every canonical key correctly must pass cleanly.
    let canonical = serde_json::json!({
        "schema": "ee.synthetic.v1",
        "data": {
            "memories": [{
                "id": "mem_01clean",
                "content": "canonical body",
                "content_truncated": false,
                "level": "procedural",
                "kind": "rule",
                "workspace_id": "ws_x",
                "workspace_path": "/tmp/ws_x",
                "provenance_uri": "file:///x",
                "created_at": "2026-05-12T00:00:00Z",
                "updated_at": "2026-05-12T00:00:00Z"
            }]
        }
    });
    assert_no_drift("synthetic-canonical", &canonical.to_string())
}

// ─────────────────────────────────────────────────────────────────────────
// Real surfaces — every render_*_json function the bead acceptance lists,
// fed a minimal fixture and scanned end-to-end.
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn memory_list_surface_has_no_field_name_drift() -> TestResult {
    let report = MemoryListReport::success(
        vec![MemorySummary {
            id: "mem_01canon".to_owned(),
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
    );
    assert_no_drift("memory list", &render_memory_list_json(&report))
}

#[test]
fn rule_list_surface_has_no_field_name_drift() -> TestResult {
    let summary = RuleSummary {
        id: "rule_01canon".to_owned(),
        content: "Run cargo clippy before merge.".to_owned(),
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
    let report = RuleListReport {
        schema: ee::core::rule::RULE_LIST_SCHEMA_V1,
        command: "rule list",
        version: env!("CARGO_PKG_VERSION"),
        workspace_id: "ws_test".to_owned(),
        workspace_path: "/tmp/ws_test".to_owned(),
        database_path: "/tmp/ws_test/ee.db".to_owned(),
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
    };
    assert_no_drift("rule list", &render_rule_list_json(&report))
}

#[test]
fn learn_uncertainty_surface_has_no_field_name_drift() -> TestResult {
    let report = LearnUncertaintyReport {
        schema: LEARN_UNCERTAINTY_SCHEMA_V1.to_owned(),
        mean_uncertainty: 0.7,
        high_uncertainty_count: 1,
        sampling_candidates: 1,
        items: vec![UncertaintyItem {
            memory_id: "mem_01uncertain".to_owned(),
            content: "Procedure needs more replay evidence.".to_owned(),
            content_truncated: false,
            kind: "procedural".to_owned(),
            uncertainty: 0.7,
            confidence: 0.4,
            retrieval_count: 2,
            last_accessed: None,
        }],
        generated_at: "2026-05-10T00:00:00Z".to_owned(),
    };
    assert_no_drift("learn uncertainty", &render_learn_uncertainty_json(&report))
}

#[test]
fn search_surface_normalizes_legacy_content_preview_metadata() -> TestResult {
    let report = SearchReport {
        status: SearchStatus::Success,
        query: "format before release".to_owned(),
        requested_limit: 1,
        results: vec![SearchHit {
            doc_id: "mem_01search".to_owned(),
            score: 0.91,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.91),
            rerank_score: None,
            metadata: Some(serde_json::json!({
                "contentPreview": "Run cargo fmt --check before release.",
                "created_at": "2026-05-10T00:00:00Z",
            })),
            explanation: None,
        }],
        elapsed_ms: 1.0,
        errors: Vec::new(),
        degraded: Vec::new(),
        runtime_profile: RuntimeProfileReport::for_profile(
            OperatingProfile::Workstation,
            "test_fixture",
        ),
        relevance_floor_applied: None,
        candidates_below_floor: 0,
        source_mode_requested: SearchSourceMode::Hybrid,
        source_mode_applied: SearchSourceMode::Hybrid,
        source_mode_fallback: false,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        scope_stats: MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0),
    };
    assert_no_drift("search", &report.data_json().to_string())
}

#[test]
fn introspect_command_manifest_has_no_field_name_drift() -> TestResult {
    // No fixture required — `render_introspect_json()` enumerates every
    // command in COMMAND_MANIFEST, so this single scan covers the
    // self-description surface for every subcommand at once.
    assert_no_drift("introspect", &render_introspect_json())
}

// ─────────────────────────────────────────────────────────────────────────
// Map integrity: catch authoring bugs in CANONICAL_FIELDS itself
// (e.g. listing the canonical key as one of its own forbidden synonyms).
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn canonical_map_does_not_forbid_canonical_keys() -> TestResult {
    for (concept, canonical, forbidden) in CANONICAL_FIELDS {
        for &bad in *forbidden {
            if bad == *canonical {
                return Err(format!(
                    "CANONICAL_FIELDS row for `{concept}` lists `{canonical}` as a forbidden synonym for itself"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn canonical_map_has_no_duplicate_forbidden_entries() -> TestResult {
    // A synonym claimed by two different canonical concepts would make
    // drift errors ambiguous ("did you mean `content` or `created_at`?").
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for (_, canonical, forbidden) in CANONICAL_FIELDS {
        for &bad in *forbidden {
            for (prior_bad, prior_canonical) in &seen {
                if *prior_bad == bad {
                    return Err(format!(
                        "synonym `{bad}` is claimed by both `{prior_canonical}` and `{canonical}` — split into distinct canonical concepts or pick one owner"
                    ));
                }
            }
            seen.push((bad, *canonical));
        }
    }
    Ok(())
}
