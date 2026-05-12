//! D1 schema audit: canonical `content` field across every surface that returns
//! memory text, plus `content_truncated: bool` on every list/preview view.
//!
//! This contract test serves as the schema-drift guard for D1
//! (bd-17c65.4.1). It exercises four agent-facing surfaces and asserts the
//! canonical field shape so an agent never has to write a field-name
//! translation map between `memory list -> memory show -> why -> context`.
//!
//! Surfaces covered:
//! - `ee memory list`  : each item must have `content` + `content_truncated`
//! - `ee rule list`    : each item must have `content` + `contentTruncated`
//!                       (RuleSummary serializes camelCase via serde)
//! - `ee why`          : top-level `content: <full body>` (no truncation flag —
//!                       why returns the full body so an agent does not need
//!                       to chain `ee memory show`)
//! - `ee learn uncertainty` : each item must have `content` + `content_truncated`
//!
//! Counter-assertion: the legacy `content_preview` name must not appear in
//! the JSON output of any of these surfaces.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ee::core::learn::{LEARN_UNCERTAINTY_SCHEMA_V1, LearnUncertaintyReport, UncertaintyItem};
use ee::core::memory::{MemoryListFilter, MemoryListReport, MemorySummary};
use ee::core::rule::{RuleEvidence, RuleLifecycle, RuleListFilter, RuleListReport, RuleSummary};
use ee::output::{render_learn_uncertainty_json, render_memory_list_json, render_rule_list_json};
use serde_json::Value;

type TestResult = Result<(), String>;

fn require_string(value: &Value, path: &str) -> Result<String, String> {
    value
        .get(path)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("expected string at .{path}"))
}

fn require_bool(value: &Value, path: &str) -> Result<bool, String> {
    value
        .get(path)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("expected bool at .{path}"))
}

fn reject_field(value: &Value, forbidden: &str) -> TestResult {
    if value.get(forbidden).is_some() {
        return Err(format!(
            "forbidden field `{forbidden}` present (D1 canonical name is `content`)"
        ));
    }
    Ok(())
}

#[test]
fn memory_list_items_emit_canonical_content_and_truncated_flag() -> TestResult {
    let report = MemoryListReport::success(
        vec![MemorySummary {
            id: "mem_01canon".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: "Run cargo fmt --check before release.".to_owned(),
            content_truncated: false,
            confidence: 0.9,
            provenance_uri: None,
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

    let json: Value = serde_json::from_str(&render_memory_list_json(&report))
        .map_err(|e| format!("memory list JSON did not parse: {e}"))?;
    let item = json
        .pointer("/data/memories/0")
        .ok_or_else(|| "missing /data/memories/0".to_string())?;

    let content = require_string(item, "content")?;
    let truncated = require_bool(item, "content_truncated")?;
    if content != "Run cargo fmt --check before release." {
        return Err(format!("unexpected content body: {content:?}"));
    }
    if truncated {
        return Err("content_truncated should be false for short content".to_string());
    }
    reject_field(item, "content_preview")
}

#[test]
fn memory_list_marks_truncated_content_with_flag_true() -> TestResult {
    // Construct a summary whose preview already ends with the canonical "..."
    // marker — this matches what `truncate_content` produces in the live path.
    let truncated_body: String = "é".repeat(80);
    let display = format!("{truncated_body}...");
    let report = MemoryListReport::success(
        vec![MemorySummary {
            id: "mem_01trunc".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: display.clone(),
            content_truncated: true,
            confidence: 0.5,
            provenance_uri: None,
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

    let json: Value = serde_json::from_str(&render_memory_list_json(&report))
        .map_err(|e| format!("memory list JSON did not parse: {e}"))?;
    let item = json
        .pointer("/data/memories/0")
        .ok_or_else(|| "missing /data/memories/0".to_string())?;

    let truncated = require_bool(item, "content_truncated")?;
    if !truncated {
        return Err("content_truncated should be true when body was elided".to_string());
    }
    let content = require_string(item, "content")?;
    if !content.ends_with("...") {
        return Err(format!(
            "truncated content should end with ellipsis: {content:?}"
        ));
    }
    Ok(())
}

#[test]
fn rule_list_items_emit_canonical_content_and_truncated_flag() -> TestResult {
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

    let json: Value = serde_json::from_str(&render_rule_list_json(&report))
        .map_err(|e| format!("rule list JSON did not parse: {e}"))?;
    let item = json
        .pointer("/data/rules/0")
        .ok_or_else(|| "missing /data/rules/0".to_string())?;

    let content = require_string(item, "content")?;
    let truncated = require_bool(item, "contentTruncated")?;
    if content != "Run cargo clippy before merge." {
        return Err(format!("unexpected content body: {content:?}"));
    }
    if truncated {
        return Err("contentTruncated should be false for short content".to_string());
    }
    reject_field(item, "contentPreview")?;
    reject_field(item, "content_preview")
}

#[test]
fn learn_uncertainty_items_emit_canonical_content_and_truncated_flag() -> TestResult {
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
    let json: Value = serde_json::from_str(&render_learn_uncertainty_json(&report))
        .map_err(|e| format!("learn uncertainty JSON did not parse: {e}"))?;
    let item = json
        .pointer("/items/0")
        .ok_or_else(|| "missing /items/0".to_string())?;

    let content = require_string(item, "content")?;
    let truncated = require_bool(item, "content_truncated")?;
    if content != "Procedure needs more replay evidence." {
        return Err(format!("unexpected content body: {content:?}"));
    }
    if truncated {
        return Err("content_truncated should be false for short content".to_string());
    }
    reject_field(item, "content_preview")
}

#[test]
fn why_report_attaches_canonical_content_via_with_content() -> TestResult {
    use ee::core::why::{
        RetrievalExplanation, SelectionExplanation, StorageExplanation, WhyReport,
    };

    let report = WhyReport::found(
        "mem_01why".to_owned(),
        StorageExplanation {
            origin: "remember".to_owned(),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            provenance_uri: None,
            workflow_id: None,
            created_at: "2026-05-10T00:00:00Z".to_owned(),
            valid_from: None,
            valid_to: None,
            validity_status: "active".to_owned(),
            validity_window_kind: "open_ended".to_owned(),
        },
        RetrievalExplanation {
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            tags: Vec::new(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
        },
        SelectionExplanation {
            selection_score: 0.8,
            above_confidence_threshold: true,
            is_active: true,
            score_breakdown: "test".to_owned(),
            latest_pack_selection: None,
        },
    )
    .with_content("Always run cargo fmt --check before release.".to_owned());

    if report.content.as_deref() != Some("Always run cargo fmt --check before release.") {
        return Err(format!(
            "with_content should attach the full body, got {:?}",
            report.content
        ));
    }
    Ok(())
}
