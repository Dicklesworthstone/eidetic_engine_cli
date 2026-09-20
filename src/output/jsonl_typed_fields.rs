//! Shape-preserving redaction of authenticated typed memory sidecars.

use crate::models::memory::canonicalize_typed_memory_fields_json;
use crate::models::{MemoryId, MemoryKind, RedactionLevel};
use serde_json::Value;
use std::io;

pub(super) fn canonical(kind: &str, value: &Value) -> io::Result<Value> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid typed memory fields");
    let kind: MemoryKind = kind.parse().map_err(|_| invalid())?;
    let text =
        canonicalize_typed_memory_fields_json(&kind, &value.to_string()).map_err(|_| invalid())?;
    serde_json::from_str(&text).map_err(|_| invalid())
}

pub(super) fn redact(value: &mut Value, kind: &str, level: RedactionLevel) {
    // This public redaction path may be called without the writer. Invalid
    // values become an unmistakably invalid, content-free sentinel. The
    // writer validates both before and after redaction and cannot emit it.
    let Ok(mut normalized) = canonical(kind, value) else {
        *value = Value::String("[INVALID TYPED FIELDS]".to_owned());
        return;
    };
    if let Some(fields) = normalized.get_mut("fields").and_then(Value::as_object_mut) {
        for (name, field) in fields {
            match field {
                // Like primary-record timestamps, the validated revisit time
                // is structural scheduling data, not free-form private prose.
                Value::String(_) if kind == "decision" && name == "revisit_by" => {}
                Value::String(text)
                    if kind == "decision"
                        && name == "supersedes"
                        && text.parse::<MemoryId>().is_ok() =>
                {
                    *text = super::redact_identifier(text, level);
                }
                Value::String(text) => *text = super::redact_content(text, level),
                Value::Array(items) => {
                    for item in items {
                        if let Value::String(text) = item {
                            *text = super::redact_content(text, level);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    *value = normalized;
}

#[cfg(test)]
#[path = "jsonl_typed_fields_tests.rs"]
mod tests;
