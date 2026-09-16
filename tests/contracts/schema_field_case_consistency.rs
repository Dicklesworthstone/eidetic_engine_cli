//! Envelope field-case consistency gate
//! (bd-cli-surface-consistency-cluster-1jnu1 item 1).
//!
//! The 2026-08-26 field campaign reported that `ee` envelopes "mix cases
//! within a single object" and asked for a walker that checks every envelope
//! for case consistency. This is that walker, plus the measurement the report
//! did not have.
//!
//! **The convention is already settled: camelCase.**
//! `tests/contracts/retrieval_field_naming.rs` (EE-FIELD-NAMING-001) has
//! enforced camelCase at every depth for context/search/why since
//! `eidetic_engine_cli-fbmq`. What was missing is *coverage*: that gate only
//! walks three retrieval surfaces, so drift on any other surface was
//! invisible. This gate walks all of `docs/schemas/`, which is the declared
//! field list for every machine-facing surface. Combined with the emission
//! drift gate in `tests/contracts/schema_drift.rs` (which validates real
//! responses against these schemas), schema-level case consistency implies
//! emission-level case consistency for every schema'd surface.
//!
//! **What the measurement actually found.** Scanning 236 schema files and
//! 1,427 property objects yields 11 objects that mix snake_case and camelCase
//! keys — caused by only four distinct field names, two of which are pinned
//! contracts rather than drift:
//!
//! | Field | Objects | Status |
//! |---|---|---|
//! | `embed_backend` | 5 | Contractual. `retrieval_field_naming.rs` documents it as "the documented root `embed_backend` token in the public retrieval schemas". |
//! | `content_truncated` | 4 | Contractual. AGENTS.md pins it in the response-envelope contract ("List views may set `content_truncated: true`"), and `canonical_content_field.rs` tests it. |
//! | `is_tombstoned` | 1 | Real drift. |
//! | `read_pool` | 1 | Real drift. |
//!
//! So the honest residual is **two field names**, not a systemic problem.
//! They are listed in `KNOWN_CASE_DRIFT` below and must shrink, never grow.
//!
//! This gate deliberately flags only *mixing within a single object*, not
//! snake_case on its own. A fully snake_case envelope (`ee memory list`) is
//! self-consistent and readable; an object that spells two of its own keys two
//! different ways is what forces a consumer to guess. Whole-surface camelCase
//! migration is a separate contract break, not this test's job.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

type TestResult = Result<(), String>;

/// snake_case tokens that are pinned by a published contract and are expected
/// to keep appearing next to camelCase siblings. Each entry needs a reason.
const CONTRACTUAL_SNAKE_TOKENS: &[(&str, &str)] = &[
    (
        "embed_backend",
        "EE-FIELD-NAMING-001: the documented root retrieval-backend token, \
         explicitly exempted in tests/contracts/retrieval_field_naming.rs",
    ),
    (
        "content_truncated",
        "AGENTS.md response-envelope contract: `content` is canonical and list \
         views mark elision with `content_truncated`; pinned by \
         tests/contracts/canonical_content_field.rs",
    ),
];

/// Real case drift, tracked so it shrinks. Removing the drift must also remove
/// the entry — `known_case_drift_entries_are_all_still_present` fails if an
/// entry goes stale, so this list cannot rot into a permanent excuse.
const KNOWN_CASE_DRIFT: &[(&str, &str)] = &[
    (
        "is_tombstoned",
        "ee.memory.show.v1.json /properties/data, alongside camelCase `memoryId`",
    ),
    (
        "read_pool",
        "ee.status.v1.json /properties/data, alongside 16 camelCase siblings",
    ),
];

#[derive(Debug)]
struct MixedObject {
    schema: String,
    pointer: String,
    snake_keys: Vec<String>,
}

fn is_snake_case(key: &str) -> bool {
    key.starts_with(|c: char| c.is_ascii_lowercase())
        && key.contains('_')
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_camel_case(key: &str) -> bool {
    key.starts_with(|c: char| c.is_ascii_lowercase())
        && !key.contains('_')
        && key.chars().any(|c| c.is_ascii_uppercase())
        && key.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Walk a schema document, reporting every `properties` object whose own keys
/// mix the two conventions.
fn collect_mixed_objects(schema: &str, node: &Value, pointer: &str, out: &mut Vec<MixedObject>) {
    match node {
        Value::Object(map) => {
            if let Some(Value::Object(properties)) = map.get("properties") {
                let snake_keys: Vec<String> = properties
                    .keys()
                    .filter(|key| is_snake_case(key))
                    .cloned()
                    .collect();
                let has_camel = properties.keys().any(|key| is_camel_case(key));
                if has_camel && !snake_keys.is_empty() {
                    out.push(MixedObject {
                        schema: schema.to_owned(),
                        pointer: if pointer.is_empty() {
                            "/".to_owned()
                        } else {
                            pointer.to_owned()
                        },
                        snake_keys,
                    });
                }
            }
            for (key, child) in map {
                collect_mixed_objects(schema, child, &format!("{pointer}/{key}"), out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_mixed_objects(schema, child, &format!("{pointer}/{index}"), out);
            }
        }
        _ => {}
    }
}

fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("schemas")
}

fn scan_declared_schemas() -> Result<Vec<MixedObject>, String> {
    let dir = schema_dir();
    let entries =
        fs::read_dir(&dir).map_err(|error| format!("failed to read {}: {error}", dir.display()))?;
    // Sort so failure output is deterministic regardless of directory order.
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("failed to read a schema entry: {error}"))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut mixed = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("schema file name is not UTF-8: {}", path.display()))?
            .to_owned();
        let text =
            fs::read_to_string(&path).map_err(|error| format!("failed to read {name}: {error}"))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| format!("{name} is not valid JSON: {error}"))?;
        collect_mixed_objects(&name, &value, "", &mut mixed);
    }
    Ok(mixed)
}

fn allowed_token(token: &str) -> bool {
    CONTRACTUAL_SNAKE_TOKENS
        .iter()
        .any(|(name, _)| *name == token)
        || KNOWN_CASE_DRIFT.iter().any(|(name, _)| *name == token)
}

/// The gate: no envelope object may mix snake_case and camelCase keys through
/// a field name that is neither a pinned contract nor tracked drift.
#[test]
fn no_new_field_case_mixing_in_declared_schemas() -> TestResult {
    let mixed = scan_declared_schemas()?;
    let mut violations = Vec::new();
    for object in &mixed {
        for key in &object.snake_keys {
            if !allowed_token(key) {
                violations.push(format!(
                    "{} {} mixes cases via snake_case key `{key}`",
                    object.schema, object.pointer
                ));
            }
        }
    }
    if violations.is_empty() {
        return Ok(());
    }
    Err(format!(
        "New envelope field-case mixing ({} occurrence(s)). The convention is \
         camelCase (EE-FIELD-NAMING-001). Rename the field, or — if it is a \
         deliberate published contract — add it to CONTRACTUAL_SNAKE_TOKENS \
         with a reason:\n  {}",
        violations.len(),
        violations.join("\n  ")
    ))
}

/// An allowlist that outlives the problem it documents is a lie. If drift is
/// repaired, this fails until the stale entry is deleted.
#[test]
fn known_case_drift_entries_are_all_still_present() -> TestResult {
    let mixed = scan_declared_schemas()?;
    let observed: BTreeSet<&str> = mixed
        .iter()
        .flat_map(|object| object.snake_keys.iter().map(String::as_str))
        .collect();
    let stale: Vec<&str> = KNOWN_CASE_DRIFT
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !observed.contains(name))
        .collect();
    if stale.is_empty() {
        return Ok(());
    }
    Err(format!(
        "KNOWN_CASE_DRIFT lists field(s) that no longer mix cases: {}. \
         The drift was fixed — delete the entry so the list keeps shrinking.",
        stale.join(", ")
    ))
}

/// Same for the contractual list, so a removed contract cannot silently keep
/// granting an exemption to a future field of the same name.
#[test]
fn contractual_snake_tokens_are_all_still_present() -> TestResult {
    let mixed = scan_declared_schemas()?;
    let observed: BTreeSet<&str> = mixed
        .iter()
        .flat_map(|object| object.snake_keys.iter().map(String::as_str))
        .collect();
    let stale: Vec<&str> = CONTRACTUAL_SNAKE_TOKENS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !observed.contains(name))
        .collect();
    if stale.is_empty() {
        return Ok(());
    }
    Err(format!(
        "CONTRACTUAL_SNAKE_TOKENS lists token(s) that no longer appear beside \
         camelCase siblings: {}. Delete the entry rather than leaving a \
         standing exemption.",
        stale.join(", ")
    ))
}

/// Positive control: the walker must actually detect mixing, so a green run
/// means "no mixing found", not "the scanner silently matched nothing".
#[test]
fn walker_detects_a_hand_constructed_mixed_object() -> TestResult {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "data": {
                "type": "object",
                "properties": {
                    "camelCaseField": {"type": "string"},
                    "snake_case_field": {"type": "string"},
                    "single": {"type": "string"}
                }
            }
        }
    });
    let mut mixed = Vec::new();
    collect_mixed_objects("fixture.json", &schema, "", &mut mixed);

    let found = mixed
        .iter()
        .find(|object| object.pointer == "/properties/data")
        .ok_or_else(|| format!("walker missed the planted mixed object: {mixed:?}"))?;
    if found.snake_keys != vec!["snake_case_field".to_owned()] {
        return Err(format!(
            "walker should report exactly the snake_case key, got {:?}",
            found.snake_keys
        ));
    }
    Ok(())
}

/// Negative control: a self-consistent object must not be flagged. `ee memory
/// list` rows are entirely snake_case and are legitimate.
#[test]
fn walker_ignores_self_consistent_objects() -> TestResult {
    let schema = serde_json::json!({
        "properties": {
            "all_snake": {"type": "string"},
            "still_snake": {"type": "string"}
        }
    });
    let mut mixed = Vec::new();
    collect_mixed_objects("fixture.json", &schema, "", &mut mixed);
    if mixed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "an all-snake_case object must not be reported as mixed: {mixed:?}"
        ))
    }
}

/// Guard the classifiers themselves; they decide every verdict above.
#[test]
fn case_classifiers_agree_with_the_documented_convention() -> TestResult {
    for key in [
        "embed_backend",
        "content_truncated",
        "is_tombstoned",
        "read_pool",
    ] {
        if !is_snake_case(key) || is_camel_case(key) {
            return Err(format!("`{key}` should classify as snake_case only"));
        }
    }
    for key in ["memoryId", "contentRedacted", "elapsedMs", "docId"] {
        if !is_camel_case(key) || is_snake_case(key) {
            return Err(format!("`{key}` should classify as camelCase only"));
        }
    }
    // Single lowercase words belong to neither convention and must never make
    // an object look mixed on their own.
    for key in ["command", "version", "schema", "data"] {
        if is_camel_case(key) || is_snake_case(key) {
            return Err(format!("`{key}` must classify as neither convention"));
        }
    }
    Ok(())
}
