#![allow(clippy::useless_format)]
//! N4.1 (bd-17c65.14.4.1) — validate the static randomness audit
//! inventory at `tests/randomness_inventory.json`.
//!
//! The audit is produced by `scripts/audit_randomness_sources.sh` and
//! is the input to N4.2 (Deterministic<Seed> token design). N4.2's
//! bead body cites this inventory by `rows_content_hash` before
//! opening implementation work.
//!
//! Contracts (each test):
//!
//! 1. Schema field equals `ee.audit.randomness_inventory.v1`.
//! 2. `count_total` matches the actual length of `rows`.
//! 3. Every row has `randomness_kind` from the enum set
//!    {rng, systemtime, hashmap_iter, env, filesystem_order, ulid_clock}.
//! 4. Every row has `severity` from the enum set
//!    {deterministic_today, latent_risk, confirmed_drift}.
//! 5. Every row has `proposed_remediation` from the enum set
//!    {capability_token, sort_iter, inject_clock, env_var, manual_sort}.
//! 6. Inventory is non-empty (count_total > 0) — zero findings would
//!    be suspicious in a non-trivial codebase.
//! 7. At least one row has severity `latent_risk` OR
//!    `confirmed_drift` — the audit must be non-trivial.
//! 8. `rows_content_hash` is present and has the expected
//!    `blake3-ish:<hex>` prefix shape — N4.2 cites it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use serde_json::Value;

type TestResult = Result<(), String>;

const INVENTORY: &str = include_str!("../tests/randomness_inventory.json");

fn parse_inventory() -> Result<Value, String> {
    serde_json::from_str(INVENTORY).map_err(|error| format!("parse inventory: {error}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn schema_pin_is_v1() -> TestResult {
    let inv = parse_inventory()?;
    let schema = inv
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "inventory: missing top-level `schema` field".to_string())?;
    ensure(
        schema == "ee.audit.randomness_inventory.v1",
        format!(
            "inventory: schema is `{schema}`; expected \
             `ee.audit.randomness_inventory.v1`. Re-run \
             `scripts/audit_randomness_sources.sh` OR file a follow-up \
             bead to bump the schema version with a documented migration."
        ),
    )
}

#[test]
fn count_total_matches_rows_length() -> TestResult {
    let inv = parse_inventory()?;
    let declared = inv
        .get("count_total")
        .and_then(Value::as_u64)
        .ok_or_else(|| "inventory: missing or non-integer `count_total`".to_string())?;
    let rows_len = inv
        .get("rows")
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| "inventory: missing `rows` array".to_string())?;
    ensure(
        declared as usize == rows_len,
        format!(
            "inventory: count_total {declared} does not match rows.length \
             {rows_len}. Re-run `scripts/audit_randomness_sources.sh`."
        ),
    )
}

#[test]
fn every_row_has_valid_randomness_kind() -> TestResult {
    let allowed: BTreeSet<&str> = [
        "rng",
        "systemtime",
        "hashmap_iter",
        "env",
        "filesystem_order",
        "ulid_clock",
    ]
    .into_iter()
    .collect();
    let inv = parse_inventory()?;
    let rows = inv
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| "inventory: missing `rows` array".to_string())?;
    let mut violations: Vec<String> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let kind = row
            .get("randomness_kind")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("inventory row {i}: missing `randomness_kind`"))?;
        if !allowed.contains(kind) {
            violations.push(format!(
                "row {i} (fn_path=`{}`): randomness_kind `{kind}` not in {:?}",
                row.get("fn_path")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>"),
                allowed
            ));
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} row(s) have invalid randomness_kind:\n  - {}",
            violations.len(),
            violations.join("\n  - ")
        ))
    }
}

#[test]
fn every_row_has_valid_severity() -> TestResult {
    let allowed: BTreeSet<&str> = ["deterministic_today", "latent_risk", "confirmed_drift"]
        .into_iter()
        .collect();
    let inv = parse_inventory()?;
    let rows = inv
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| "inventory: missing `rows` array".to_string())?;
    let mut violations: Vec<String> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let sev = row
            .get("severity")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("inventory row {i}: missing `severity`"))?;
        if !allowed.contains(sev) {
            violations.push(format!(
                "row {i} (fn_path=`{}`): severity `{sev}` not in {:?}",
                row.get("fn_path")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>"),
                allowed
            ));
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} row(s) have invalid severity:\n  - {}",
            violations.len(),
            violations.join("\n  - ")
        ))
    }
}

#[test]
fn every_row_has_valid_proposed_remediation() -> TestResult {
    let allowed: BTreeSet<&str> = [
        "capability_token",
        "sort_iter",
        "inject_clock",
        "env_var",
        "manual_sort",
    ]
    .into_iter()
    .collect();
    let inv = parse_inventory()?;
    let rows = inv
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| "inventory: missing `rows` array".to_string())?;
    let mut violations: Vec<String> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let rem = row
            .get("proposed_remediation")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("inventory row {i}: missing `proposed_remediation`"))?;
        if !allowed.contains(rem) {
            violations.push(format!(
                "row {i} (fn_path=`{}`): proposed_remediation `{rem}` not in {:?}",
                row.get("fn_path")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>"),
                allowed
            ));
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} row(s) have invalid proposed_remediation:\n  - {}",
            violations.len(),
            violations.join("\n  - ")
        ))
    }
}

#[test]
fn inventory_is_non_empty() -> TestResult {
    let inv = parse_inventory()?;
    let total = inv
        .get("count_total")
        .and_then(Value::as_u64)
        .ok_or_else(|| "inventory: missing `count_total`".to_string())?;
    ensure(
        total > 0,
        format!(
            "inventory: count_total = {total}; expected > 0. A zero-finding \
             audit in a non-trivial codebase is suspicious — either the audit \
             script's patterns are mis-tuned or src/ shrunk dramatically. \
             Re-run `scripts/audit_randomness_sources.sh` and investigate."
        ),
    )
}

#[test]
fn audit_is_non_trivial() -> TestResult {
    let inv = parse_inventory()?;
    let rows = inv
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| "inventory: missing `rows` array".to_string())?;
    let elevated = rows
        .iter()
        .filter(|row| {
            row.get("severity")
                .and_then(Value::as_str)
                .is_some_and(|s| s == "latent_risk" || s == "confirmed_drift")
        })
        .count();
    ensure(
        elevated > 0,
        format!(
            "inventory: zero rows have severity `latent_risk` or \
             `confirmed_drift`; the audit is trivial (only \
             deterministic_today entries). Either the codebase has \
             genuinely eliminated all ambient randomness (verify by \
             reading the inventory) or the script's classification is \
             broken."
        ),
    )
}

#[test]
fn rows_content_hash_is_present_and_well_formed() -> TestResult {
    let inv = parse_inventory()?;
    let hash = inv
        .get("rows_content_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| "inventory: missing `rows_content_hash`".to_string())?;
    ensure(
        hash.starts_with("blake3-ish:"),
        format!(
            "inventory: rows_content_hash `{hash}` does not start with \
             `blake3-ish:` prefix. N4.2 cites this hash by prefix-and-hex; \
             a different prefix is OK only if N4.2's citation contract \
             updates in the same PR."
        ),
    )?;
    let hex_part = &hash["blake3-ish:".len()..];
    ensure(
        hex_part.len() == 64,
        format!(
            "inventory: rows_content_hash hex part has length {}; expected 64 \
             format!(SHA-256 hex). The script uses shasum -a 256 as a blake3 \
             stand-in; bumping the hash algorithm requires updating both the \
             script and this test in the same PR.",
            hex_part.len()
        ),
    )?;
    ensure(
        hex_part.chars().all(|c| c.is_ascii_hexdigit()),
        format!("inventory: rows_content_hash hex part contains non-hex characters: `{hex_part}`"),
    )
}

#[test]
fn summary_aggregates_match_rows() -> TestResult {
    let inv = parse_inventory()?;
    let rows = inv
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| "inventory: missing `rows` array".to_string())?;
    let summary = inv
        .get("summary")
        .ok_or_else(|| "inventory: missing `summary` object".to_string())?;

    // Sum the by_severity buckets and assert they match the row count.
    let by_severity = summary
        .get("by_severity")
        .and_then(Value::as_object)
        .ok_or_else(|| "summary: missing by_severity".to_string())?;
    let severity_total: u64 = by_severity.values().filter_map(Value::as_u64).sum();
    ensure(
        severity_total as usize == rows.len(),
        format!(
            "summary.by_severity sums to {severity_total} but rows.length is \
             {}. Re-run the audit script.",
            rows.len()
        ),
    )?;

    let by_kind = summary
        .get("by_kind")
        .and_then(Value::as_object)
        .ok_or_else(|| "summary: missing by_kind".to_string())?;
    let kind_total: u64 = by_kind.values().filter_map(Value::as_u64).sum();
    ensure(
        kind_total as usize == rows.len(),
        format!(
            "summary.by_kind sums to {kind_total} but rows.length is \
             {}. Re-run the audit script.",
            rows.len()
        ),
    )
}
