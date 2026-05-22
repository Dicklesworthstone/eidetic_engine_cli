//! bd-2pk7w: conformance harness for the canonical `ee.audit.v1` +
//! `ee.log.v1` envelope shapes in `src/obs/log_envelope.rs`.
//!
//! /multi-pass-bug-hunting pass against src/obs/. The bd-2vvz3
//! sibling harness (tests/contracts/curate_outcome_audit_schema_contract.rs)
//! pinned the audit FAMILY across memory_lifecycle / procedure /
//! preflight / hello_responder, but missed the literal `ee.audit.v1`
//! schema declared in `src/obs/log_envelope.rs:16` — which IS the
//! canonical envelope the bd-2vvz3 bead text was pointing at all
//! along. This file fills that gap and extends to its sibling
//! `ee.log.v1` log envelope, plus the closed enum vocabularies
//! that downstream consumers branch on.
//!
//! Scope:
//!   1. `AUDIT_EVENT_SCHEMA_V1` pinned to `"ee.audit.v1"` (the
//!      literal canonical form).
//!   2. `LOG_ENVELOPE_SCHEMA_V1` pinned to `"ee.log.v1"`.
//!   3. `AuditOutcome` enum's wire vocabulary pinned (success,
//!      failure, cancelled, dry_run, rollback). This is the
//!      "stable signal vocabulary" the bd-2vvz3 bead asked about
//!      but the outcome `ALLOWED_SIGNALS` was private; AuditOutcome
//!      IS public.
//!   4. `LogLevel` enum's wire vocabulary pinned (trace, debug,
//!      info, warn, error).
//!   5. `AuditEvent` constructed-then-serialized to confirm the
//!      required-field set (schema, ts, actor, action, subject,
//!      outcome, fields) every consumer leans on.
//!   6. `LogEnvelope` constructed-then-serialized to confirm
//!      schema + ts + level + target are always emitted and
//!      span_id + trace_id are omitted when None (the
//!      `skip_serializing_if = Option::is_none` contract that
//!      JSONL log scrapers depend on).
//!
//! Read-only constant + serialization pinning. No DB state, no
//! filesystem writes. Real append-only audit-trail behavior is
//! exercised by other harnesses; this file is the wire-form
//! chokepoint.

use ee::obs::log_envelope::{
    AUDIT_EVENT_SCHEMA_V1, AuditEvent, AuditOutcome, LOG_ENVELOPE_SCHEMA_V1, LogEnvelope, LogLevel,
};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

/// bd-2pk7w pin #1: the canonical audit envelope wire form.
/// Every downstream consumer (J6 fixtures, agent-side audit-trail
/// scrapers, ee why's audit lookups) grep this exact string.
#[test]
fn audit_event_schema_v1_is_ee_dot_audit_dot_v1() -> TestResult {
    if AUDIT_EVENT_SCHEMA_V1 != "ee.audit.v1" {
        return Err(format!(
            "AUDIT_EVENT_SCHEMA_V1 drifted to {AUDIT_EVENT_SCHEMA_V1:?}; the canonical \
             `ee.audit.v1` is the load-bearing audit envelope the rest of the contract \
             references. Bump to v2 in a deliberate migration if this rename is \
             intentional, and update every consumer's grep + the J6 catalog in lockstep."
        ));
    }
    Ok(())
}

/// bd-2pk7w pin #2: the canonical structured log envelope wire form.
#[test]
fn log_envelope_schema_v1_is_ee_dot_log_dot_v1() -> TestResult {
    if LOG_ENVELOPE_SCHEMA_V1 != "ee.log.v1" {
        return Err(format!(
            "LOG_ENVELOPE_SCHEMA_V1 drifted to {LOG_ENVELOPE_SCHEMA_V1:?}; `ee.log.v1` is \
             the load-bearing structured-log envelope. Bump to v2 in a deliberate \
             migration if intentional."
        ));
    }
    Ok(())
}

/// bd-2pk7w pin #3: AuditOutcome's stable wire vocabulary.
///
/// AGENTS.md severity vocabulary is a separate axis; this enum
/// carries the action-outcome classification a J6 fixture's
/// `expected_emission.outcome` field can pin against. Five values,
/// closed set.
#[test]
fn audit_outcome_vocabulary_is_closed_and_pinned() -> TestResult {
    for (variant, expected) in [
        (AuditOutcome::Success, "success"),
        (AuditOutcome::Failure, "failure"),
        (AuditOutcome::Cancelled, "cancelled"),
        (AuditOutcome::DryRun, "dry_run"),
        (AuditOutcome::Rollback, "rollback"),
    ] {
        let actual = variant.as_str();
        if actual != expected {
            return Err(format!(
                "AuditOutcome::{variant:?}.as_str() drifted: got {actual:?}, expected {expected:?}; \
                 the wire vocabulary is part of the ee.audit.v1 contract — every consumer \
                 branching on outcome strings would silently re-classify."
            ));
        }
    }
    Ok(())
}

/// bd-2pk7w pin #4: LogLevel's stable wire vocabulary.
/// Pinned independently of AuditOutcome — the two enums share a
/// pattern but evolve on separate cadences.
#[test]
fn log_level_vocabulary_is_closed_and_pinned() -> TestResult {
    for (variant, expected) in [
        (LogLevel::Trace, "trace"),
        (LogLevel::Debug, "debug"),
        (LogLevel::Info, "info"),
        (LogLevel::Warn, "warn"),
        (LogLevel::Error, "error"),
    ] {
        let actual = variant.as_str();
        if actual != expected {
            return Err(format!(
                "LogLevel::{variant:?}.as_str() drifted: got {actual:?}, expected {expected:?}; \
                 the wire vocabulary is part of the ee.log.v1 contract."
            ));
        }
    }
    Ok(())
}

/// bd-2pk7w pin #5: AuditEvent required-field set.
///
/// Build a real AuditEvent via the public constructor and assert
/// the serialized JSON object's keys match the documented schema.
/// Catches a future refactor that adds a `#[serde(skip)]` to a
/// required field, renames `actor` → `agent`, or removes the
/// `fields` map.
#[test]
fn audit_event_required_field_set_pinned() -> TestResult {
    let event = AuditEvent::new(
        "2026-05-22T20:15:00Z",
        "cc-mcp",
        "bd-2pk7w.audit.test",
        "tests/contracts/obs_log_envelope_schema_contract.rs",
        AuditOutcome::Success,
    )
    .with_field("note", json!("synthetic audit event for bd-2pk7w pinning"));
    let value =
        serde_json::to_value(&event).map_err(|error| format!("serialize AuditEvent: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "AuditEvent must serialize as a JSON object".to_string())?;
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let expected = vec![
        "action", "actor", "fields", "outcome", "schema", "subject", "ts",
    ];
    if keys != expected {
        return Err(format!(
            "AuditEvent key set drifted: got {keys:?}, expected {expected:?}; every consumer \
             of ee.audit.v1 expects these 7 keys exactly. Renames or additions need a v2 bump."
        ));
    }
    // schema field carries the canonical wire form.
    if value["schema"].as_str() != Some(AUDIT_EVENT_SCHEMA_V1) {
        return Err(format!(
            "AuditEvent.schema field disagrees with AUDIT_EVENT_SCHEMA_V1 const: \
             {} != {AUDIT_EVENT_SCHEMA_V1:?}",
            value["schema"]
        ));
    }
    // outcome is wire-form (lowercase snake_case), not Debug-derived ('Success').
    if value["outcome"].as_str() != Some("success") {
        return Err(format!(
            "AuditEvent.outcome must serialize using AuditOutcome::as_str(); got {}",
            value["outcome"]
        ));
    }
    // user-supplied field landed under `fields` (not promoted to top level).
    if value["fields"].as_object().is_none_or(|f| f.is_empty()) {
        return Err(format!(
            "AuditEvent.fields must surface user-supplied entries; got {}",
            value["fields"]
        ));
    }
    Ok(())
}

/// bd-2pk7w pin #6: LogEnvelope required-field set + optional-field
/// omission contract.
///
/// `span_id` and `trace_id` are `Option<String>` with
/// `skip_serializing_if = "Option::is_none"`. JSONL log scrapers
/// branch on field-presence, so the omit-when-None contract is
/// load-bearing — a future change that switched to emitting
/// `null` would silently break every consumer that uses
/// `obj.get("span_id").is_some()` as the trace-presence signal.
#[test]
fn log_envelope_required_and_optional_field_contract() -> TestResult {
    // Required path: no span/trace ids set.
    let bare = LogEnvelope::new("2026-05-22T20:15:00Z", LogLevel::Info, "bd-2pk7w.test");
    let bare_value =
        serde_json::to_value(&bare).map_err(|error| format!("serialize bare: {error}"))?;
    let bare_obj = bare_value
        .as_object()
        .ok_or_else(|| "LogEnvelope must serialize as object".to_string())?;
    let mut bare_keys: Vec<&str> = bare_obj.keys().map(String::as_str).collect();
    bare_keys.sort_unstable();
    let expected_bare = vec!["fields", "level", "schema", "target", "ts"];
    if bare_keys != expected_bare {
        return Err(format!(
            "LogEnvelope (no span/trace) key set drifted: got {bare_keys:?}, expected \
             {expected_bare:?}. The `skip_serializing_if = Option::is_none` contract is \
             the load-bearing signal JSONL scrapers use to detect trace-mode rows."
        ));
    }
    if bare_value["schema"].as_str() != Some(LOG_ENVELOPE_SCHEMA_V1) {
        return Err("LogEnvelope.schema must equal LOG_ENVELOPE_SCHEMA_V1".to_string());
    }
    if bare_value["level"].as_str() != Some("info") {
        return Err(format!(
            "LogEnvelope.level must be wire-form (LogLevel::as_str()); got {}",
            bare_value["level"]
        ));
    }
    // Optional path: span/trace ids set.
    let traced = LogEnvelope::new("2026-05-22T20:15:00Z", LogLevel::Warn, "bd-2pk7w.test")
        .with_span_id("span-abc")
        .with_trace_id("trace-xyz");
    let traced_value =
        serde_json::to_value(&traced).map_err(|error| format!("serialize traced: {error}"))?;
    let traced_obj = traced_value
        .as_object()
        .ok_or_else(|| "LogEnvelope traced must serialize as object".to_string())?;
    if traced_obj.get("span_id").and_then(Value::as_str) != Some("span-abc") {
        return Err(format!(
            "LogEnvelope.span_id round-trip drifted: got {:?}",
            traced_obj.get("span_id")
        ));
    }
    if traced_obj.get("trace_id").and_then(Value::as_str) != Some("trace-xyz") {
        return Err(format!(
            "LogEnvelope.trace_id round-trip drifted: got {:?}",
            traced_obj.get("trace_id")
        ));
    }
    let mut traced_keys: Vec<&str> = traced_obj.keys().map(String::as_str).collect();
    traced_keys.sort_unstable();
    let expected_traced = vec![
        "fields", "level", "schema", "span_id", "target", "trace_id", "ts",
    ];
    if traced_keys != expected_traced {
        return Err(format!(
            "LogEnvelope (traced) key set drifted: got {traced_keys:?}, expected {expected_traced:?}"
        ));
    }
    Ok(())
}

/// bd-2pk7w pin #7: `AuditEvent.to_json_line()` produces exactly
/// one trailing newline.
///
/// This is the JSONL append contract — every append must end with
/// `\n` for line-based scrapers to find the next record boundary.
/// A future refactor that switched to `serde_json::to_string_pretty`
/// or forgot the trailing newline would silently merge records.
#[test]
fn audit_event_to_json_line_ends_in_exactly_one_newline() -> TestResult {
    let event = AuditEvent::new(
        "2026-05-22T20:15:00Z",
        "cc-mcp",
        "bd-2pk7w.jsonl.test",
        "tests/contracts/obs_log_envelope_schema_contract.rs",
        AuditOutcome::DryRun,
    );
    let line = event
        .to_json_line()
        .map_err(|error| format!("to_json_line: {error}"))?;
    if !line.ends_with('\n') {
        return Err(format!(
            "AuditEvent::to_json_line() must end with '\\n' for JSONL append safety; got line \
             ending with {:?}",
            line.chars().last()
        ));
    }
    // Exactly one trailing newline — no double-newline that would create empty records.
    if line.ends_with("\n\n") {
        return Err(
            "AuditEvent::to_json_line() must end in exactly ONE '\\n', not two".to_string(),
        );
    }
    // Body parses as a single JSON object (no embedded newlines that would split the row).
    let body = line.trim_end_matches('\n');
    let parsed: Value = serde_json::from_str(body)
        .map_err(|error| format!("body must parse as a single JSON value: {error}"))?;
    if parsed["outcome"].as_str() != Some("dry_run") {
        return Err(format!(
            "round-trip outcome drifted: got {}",
            parsed["outcome"]
        ));
    }
    Ok(())
}
