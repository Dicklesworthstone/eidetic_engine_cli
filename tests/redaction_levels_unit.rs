//! K6 redaction-level unit matrix.
//!
//! This file is the executable counterpart to `docs/redaction_levels.md`:
//! five canonical levels, ordered by increasing redaction, with behavior
//! pinned for secrets, memory bodies, tags, and audit details.

use ee::core::handoff::CreateOptions as HandoffCreateOptions;
use ee::core::support_bundle::BundleOptions;
use ee::models::{
    ExportAuditRecord, ExportMemoryRecord, ExportRecord, ExportTagRecord, RedactionLevel,
};
use ee::output::jsonl_export::{
    REDACTED_PLACEHOLDER, redact_audit_record, redact_content, redact_memory_record, redact_record,
};
use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn api_key_fixture() -> String {
    let fixture = include_str!("fixtures/secrets/pre_redaction.jsonl");
    let line = fixture
        .lines()
        .find(|line| !line.trim().is_empty())
        .expect("pre_redaction fixture should contain at least one JSONL row");
    let value: JsonValue =
        serde_json::from_str(line).expect("pre_redaction fixture row should be valid JSON");
    value
        .get("content")
        .and_then(JsonValue::as_str)
        .expect("pre_redaction fixture row should contain string content")
        .to_owned()
}

fn memory_record(content: &str) -> ExportMemoryRecord {
    ExportMemoryRecord::builder()
        .memory_id("mem-redaction-matrix-001234567890")
        .workspace_id("ws-redaction-matrix")
        .level("procedural")
        .kind("rule")
        .content(content)
        .created_at("2026-05-16T00:00:00Z")
        .build()
        .expect("memory fixture has required fields")
}

fn tag_record() -> ExportRecord {
    ExportRecord::Tag(
        ExportTagRecord::builder()
            .memory_id("mem-redaction-matrix-001234567890")
            .tag("customer-secret-tag")
            .created_at("2026-05-16T00:00:00Z")
            .build()
            .expect("tag fixture has required fields"),
    )
}

fn audit_record() -> ExportAuditRecord {
    let secret = api_key_fixture();
    ExportAuditRecord::builder()
        .audit_id("audit-redaction-matrix-001234567890")
        .operation("memory.update")
        .target_type("memory")
        .target_id("mem-redaction-matrix-001234567890")
        .performed_at("2026-05-16T00:00:00Z")
        .performed_by("agent-redaction-matrix")
        .details(serde_json::json!({
            "secret": secret,
            "reason": "redaction matrix fixture"
        }))
        .build()
        .expect("audit fixture has required fields")
}

#[test]
fn pre_redaction_fixture_is_jsonl_and_carries_api_key_pattern() -> TestResult {
    let fixture = include_str!("fixtures/secrets/pre_redaction.jsonl");
    let mut rows = 0usize;
    for (line_index, line) in fixture.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        rows += 1;
        let value: JsonValue = serde_json::from_str(line).map_err(|error| {
            format!(
                "tests/fixtures/secrets/pre_redaction.jsonl line {} is not valid JSON: {error}",
                line_index + 1
            )
        })?;
        ensure(
            value.get("expected_class").and_then(JsonValue::as_str) == Some("api_key"),
            "pre_redaction fixture must name the expected api_key detector class",
        )?;
        ensure(
            value
                .get("content")
                .and_then(JsonValue::as_str)
                .is_some_and(|content| content.contains("sk-FAKE")),
            "pre_redaction fixture must carry a secret-shaped API key sample",
        )?;
    }
    ensure(
        rows > 0,
        "tests/fixtures/secrets/pre_redaction.jsonl must contain at least one fixture row",
    )
}

#[test]
fn redaction_level_vocabulary_is_canonical_five() -> TestResult {
    let levels = RedactionLevel::all()
        .iter()
        .map(|level| level.as_str())
        .collect::<Vec<_>>();
    ensure(
        levels == ["none", "minimal", "standard", "strict", "paranoid"],
        format!("unexpected redaction vocabulary/order: {levels:?}"),
    )?;

    for level in RedactionLevel::all() {
        let parsed = level
            .as_str()
            .parse::<RedactionLevel>()
            .map_err(|error| error.to_string())?;
        ensure(
            parsed == *level,
            format!("{} should parse back to itself", level.as_str()),
        )?;
    }

    ensure(
        "overly_paranoid".parse::<RedactionLevel>().is_err(),
        "unknown redaction level should be rejected",
    )
}

#[test]
fn redaction_level_behavior_matrix_matches_docs() -> TestResult {
    let secret = api_key_fixture();
    let long_body = "memory body fixture ".repeat(20);

    for level in RedactionLevel::all() {
        let redacted_secret = redact_content(&secret, *level);
        let redacted_memory = redact_memory_record(memory_record(&long_body), *level);
        let redacted_tag = redact_record(tag_record(), *level);
        let redacted_audit = redact_audit_record(audit_record(), *level);

        match *level {
            RedactionLevel::None => {
                ensure(
                    redacted_secret == secret,
                    "none should preserve secret-shaped content",
                )?;
                ensure(
                    redacted_memory.content == long_body,
                    "none should preserve memory body",
                )?;
                ensure(
                    redacted_memory.content_hash.is_none(),
                    "none should not add a memory content hash",
                )?;
                assert_tag(&redacted_tag, |tag| {
                    ensure(tag == "customer-secret-tag", "none should preserve tags")
                })?;
                ensure(
                    audit_details_string(&redacted_audit)
                        .is_some_and(|details| details.contains("sk-FAKEabc")),
                    "none should preserve audit details",
                )?;
            }
            RedactionLevel::Minimal | RedactionLevel::Standard => {
                ensure(
                    redacted_secret == REDACTED_PLACEHOLDER,
                    format!("{level}: should redact secret-shaped content"),
                )?;
                ensure(
                    redacted_memory.content == long_body,
                    format!("{level}: should preserve non-secret memory body"),
                )?;
                ensure(
                    redacted_memory.content_hash.is_none(),
                    format!("{level}: should not hash non-secret memory body"),
                )?;
                assert_tag(&redacted_tag, |tag| {
                    ensure(
                        tag == "customer-secret-tag",
                        format!("{level}: should preserve tags"),
                    )
                })?;
                assert_hash_only_audit(&redacted_audit, *level)?;
            }
            RedactionLevel::Strict => {
                ensure(
                    redacted_secret == REDACTED_PLACEHOLDER,
                    "strict should redact secret-shaped content",
                )?;
                let redacted_long = redact_memory_record(memory_record(&long_body), *level);
                ensure(
                    redacted_long.content.chars().count() == 200,
                    "strict should truncate non-secret memory bodies to 200 chars",
                )?;
                ensure(
                    redacted_long.content_hash.is_some(),
                    "strict should hash the original full body",
                )?;
                assert_tag(&redacted_tag, |tag| {
                    ensure(tag == "customer-secret-tag", "strict should preserve tags")
                })?;
                assert_hash_only_audit(&redacted_audit, *level)?;
            }
            RedactionLevel::Paranoid => {
                ensure(
                    redacted_secret == REDACTED_PLACEHOLDER,
                    "paranoid should redact secret-shaped content",
                )?;
                ensure(
                    redacted_memory.content == REDACTED_PLACEHOLDER,
                    "paranoid should replace memory content",
                )?;
                ensure(
                    redacted_memory.content_hash.is_some(),
                    "paranoid should hash the original full body",
                )?;
                assert_tag(&redacted_tag, |tag| {
                    ensure(
                        tag.starts_with("tag_") && !tag.contains("customer-secret-tag"),
                        "paranoid should hash tags",
                    )
                })?;
                ensure(
                    redacted_audit.details.is_none(),
                    "paranoid should omit audit details",
                )?;
            }
            RedactionLevel::Full => {
                return Err("legacy full level must not appear in RedactionLevel::all()".into());
            }
        }
    }

    Ok(())
}

#[test]
fn rust_surface_defaults_match_k6_documented_defaults() -> TestResult {
    ensure(
        HandoffCreateOptions::default().redaction_level == RedactionLevel::Standard,
        "ee handoff create default should be standard",
    )?;
    ensure(
        BundleOptions::default().effective_redaction_level() == RedactionLevel::Paranoid,
        "ee support bundle default should be paranoid",
    )
}

fn assert_tag(record: &ExportRecord, predicate: impl FnOnce(&str) -> TestResult) -> TestResult {
    match record {
        ExportRecord::Tag(tag) => predicate(&tag.tag),
        other => Err(format!("expected tag record, got {other:?}")),
    }
}

fn audit_details_string(record: &ExportAuditRecord) -> Option<String> {
    record.details.as_ref().map(JsonValue::to_string)
}

fn assert_hash_only_audit(record: &ExportAuditRecord, level: RedactionLevel) -> TestResult {
    let details = record
        .details
        .as_ref()
        .ok_or_else(|| format!("{level}: audit details should be hash-only"))?;
    let hash = details
        .get("hash")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("{level}: audit details missing hash"))?;
    ensure(
        hash.starts_with("blake3:"),
        format!("{level}: audit detail hash should carry blake3 prefix"),
    )?;
    ensure(
        !details.to_string().contains("sk-FAKEabc"),
        format!("{level}: audit details should not expose the original secret"),
    )
}
