//! Memory domain validation (EE-061).
//!
//! Defines the validated value types that every durable memory must
//! produce before it reaches the database layer:
//!
//! * [`MemoryLevel`] — the four-level taxonomy (`working`, `episodic`,
//!   `semantic`, `procedural`) used for scoring tilt and packing
//!   priority.
//! * [`MemoryKind`] — the known set of memory shapes (rule, fact,
//!   decision, failure, command, convention, anti-pattern, risk,
//!   playbook step) plus a [`MemoryKind::Custom`] escape hatch for
//!   project-specific extensions.
//! * [`Tag`] — a normalized keyword that survives JSON round-trips.
//! * [`MemoryContent`] — a non-empty, length-bounded body string.
//! * [`Confidence`], [`Utility`], [`Importance`] — bounded `f32`
//!   newtypes in the unit interval `0.0..=1.0`.
//!
//! Validation never panics. Every entry point returns a typed
//! [`MemoryValidationError`] that names the offending field and value.
//! Numeric newtypes treat `NaN` and infinities as invalid; they only
//! accept finite values inside the unit interval.
//!
//! `MemoryLevel` and `MemoryKind` are stable on the wire — their
//! string forms are part of the `ee.response.v1` schema and must not
//! change without a contract bump. Their parsers accept common operator
//! spelling variants and normalize them back to the canonical strings.
//! `Tag` lowercases incoming identifiers so the canonical wire form
//! matches the canonical Rust form.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde_json::{Map as JsonMap, Value as JsonValue};

fn normalized_memory_level_token(input: &str) -> String {
    input.trim().to_ascii_lowercase()
}

fn normalized_memory_kind_token(input: &str) -> String {
    let trimmed = input.trim();
    let mut normalized = String::with_capacity(trimmed.len());
    let mut previous_was_lowercase = false;
    let mut previous_was_separator = false;

    for character in trimmed.chars() {
        match character {
            '-' | '_' => {
                if !normalized.is_empty() && !previous_was_separator {
                    normalized.push('-');
                }
                previous_was_lowercase = false;
                previous_was_separator = true;
            }
            character if character.is_ascii_uppercase() => {
                if previous_was_lowercase && !previous_was_separator {
                    normalized.push('-');
                }
                normalized.push(character.to_ascii_lowercase());
                previous_was_lowercase = false;
                previous_was_separator = false;
            }
            character => {
                normalized.push(character.to_ascii_lowercase());
                previous_was_lowercase = character.is_ascii_lowercase();
                previous_was_separator = false;
            }
        }
    }

    normalized
}

/// Maximum number of bytes accepted for a single tag.
///
/// 64 bytes covers ULIDs, kebab-case slugs, and namespaced tags
/// (`security:auth-bypass`) without being so generous that storage
/// queries blow up on malformed input.
pub const MAX_TAG_BYTES: usize = 64;

/// Maximum number of UTF-8 bytes accepted for a memory body.
///
/// 64 KiB is well above any realistic single-memory size, but small
/// enough that pathological payloads (entire log files, dumps) get a
/// typed error before they reach the index queue.
pub const MAX_CONTENT_BYTES: usize = 64 * 1024;

/// Memory levels enumerated in scoring-tilt order from least to most
/// durable.
///
/// The string form is the lowercased variant name and is stable on the
/// wire. Any future addition is a schema-bump-level change.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MemoryLevel {
    Working,
    Episodic,
    Semantic,
    Procedural,
}

impl MemoryLevel {
    /// Stable lowercase wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
        }
    }

    /// All variants in a stable, schema-aligned order.
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::Working,
            Self::Episodic,
            Self::Semantic,
            Self::Procedural,
        ]
    }
}

impl fmt::Display for MemoryLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MemoryLevel {
    type Err = MemoryValidationError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match normalized_memory_level_token(input).as_str() {
            "working" => Ok(Self::Working),
            "episodic" => Ok(Self::Episodic),
            "semantic" => Ok(Self::Semantic),
            "procedural" => Ok(Self::Procedural),
            _ => Err(MemoryValidationError::UnknownLevel {
                input: input.to_owned(),
            }),
        }
    }
}

/// Memory kinds. The first nine variants are the canonical README set;
/// [`MemoryKind::Custom`] preserves canonical project-specific
/// identifiers without losing them through round-trip.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum MemoryKind {
    Rule,
    Fact,
    Decision,
    Failure,
    Command,
    Convention,
    AntiPattern,
    Risk,
    PlaybookStep,
    Custom(String),
}

/// Names of the canonical kinds, in stable order. Useful for help text
/// and golden tests.
pub const KNOWN_MEMORY_KINDS: &[&str] = &[
    "rule",
    "fact",
    "decision",
    "failure",
    "command",
    "convention",
    "anti-pattern",
    "risk",
    "playbook-step",
];

pub const TYPED_MEMORY_FIELDS_SCHEMA_V1: &str = "ee.memory.typed_fields.v1";
pub const MAX_TYPED_MEMORY_FIELDS: usize = 4;
pub const MAX_TYPED_MEMORY_FIELD_VALUE_BYTES: usize = 4096;
pub const MAX_TYPED_MEMORY_FIELD_LIST_ITEMS: usize = 8;
pub const MAX_TYPED_MEMORY_FIELDS_JSON_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedMemoryFieldShape {
    Text,
    TextList,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TypedMemoryFieldSpec {
    name: &'static str,
    shape: TypedMemoryFieldShape,
}

const FAILURE_TYPED_MEMORY_FIELDS: &[TypedMemoryFieldSpec] = &[
    TypedMemoryFieldSpec {
        name: "cause",
        shape: TypedMemoryFieldShape::Text,
    },
    TypedMemoryFieldSpec {
        name: "regression_surface",
        shape: TypedMemoryFieldShape::Text,
    },
    TypedMemoryFieldSpec {
        name: "reverted_at_sha",
        shape: TypedMemoryFieldShape::Text,
    },
    TypedMemoryFieldSpec {
        name: "family",
        shape: TypedMemoryFieldShape::Text,
    },
];

const DECISION_TYPED_MEMORY_FIELDS: &[TypedMemoryFieldSpec] = &[
    TypedMemoryFieldSpec {
        name: "options",
        shape: TypedMemoryFieldShape::TextList,
    },
    TypedMemoryFieldSpec {
        name: "chosen",
        shape: TypedMemoryFieldShape::Text,
    },
    TypedMemoryFieldSpec {
        name: "rationale",
        shape: TypedMemoryFieldShape::Text,
    },
    TypedMemoryFieldSpec {
        name: "supersedes",
        shape: TypedMemoryFieldShape::Text,
    },
];

const COMMAND_TYPED_MEMORY_FIELDS: &[TypedMemoryFieldSpec] = &[
    TypedMemoryFieldSpec {
        name: "command",
        shape: TypedMemoryFieldShape::Text,
    },
    TypedMemoryFieldSpec {
        name: "when_to_use",
        shape: TypedMemoryFieldShape::Text,
    },
    TypedMemoryFieldSpec {
        name: "exit_meaning",
        shape: TypedMemoryFieldShape::Text,
    },
];

const RISK_TYPED_MEMORY_FIELDS: &[TypedMemoryFieldSpec] = &[
    TypedMemoryFieldSpec {
        name: "trigger",
        shape: TypedMemoryFieldShape::Text,
    },
    TypedMemoryFieldSpec {
        name: "blast_radius",
        shape: TypedMemoryFieldShape::Text,
    },
    TypedMemoryFieldSpec {
        name: "safer_alternative",
        shape: TypedMemoryFieldShape::Text,
    },
];

fn typed_memory_field_specs(kind: &MemoryKind) -> Option<&'static [TypedMemoryFieldSpec]> {
    match kind {
        MemoryKind::Failure => Some(FAILURE_TYPED_MEMORY_FIELDS),
        MemoryKind::Decision => Some(DECISION_TYPED_MEMORY_FIELDS),
        MemoryKind::Command => Some(COMMAND_TYPED_MEMORY_FIELDS),
        MemoryKind::Risk | MemoryKind::AntiPattern => Some(RISK_TYPED_MEMORY_FIELDS),
        MemoryKind::Rule
        | MemoryKind::Fact
        | MemoryKind::Convention
        | MemoryKind::PlaybookStep
        | MemoryKind::Custom(_) => None,
    }
}

fn typed_memory_field_spec(
    specs: &[TypedMemoryFieldSpec],
    field: &str,
) -> Option<TypedMemoryFieldSpec> {
    specs.iter().copied().find(|spec| spec.name == field)
}

/// Canonicalize validated typed memory fields without changing values.
///
/// This is intended for already-redacted inputs. Ingestion paths that accept
/// raw external material should call
/// [`canonicalize_typed_memory_fields_json_with_redactor`] and pass the
/// project redaction pipeline for every string field value.
pub fn canonicalize_typed_memory_fields_json(
    kind: &MemoryKind,
    raw_json: &str,
) -> Result<String, MemoryValidationError> {
    canonicalize_typed_memory_fields_json_with_redactor(kind, raw_json, str::to_owned)
}

/// Canonicalize validated typed memory fields after applying `redact` to each
/// string field value.
pub fn canonicalize_typed_memory_fields_json_with_redactor<F>(
    kind: &MemoryKind,
    raw_json: &str,
    mut redact: F,
) -> Result<String, MemoryValidationError>
where
    F: FnMut(&str) -> String,
{
    if raw_json.len() > MAX_TYPED_MEMORY_FIELDS_JSON_BYTES {
        return Err(MemoryValidationError::TypedFieldsJsonTooLarge {
            bytes: raw_json.len(),
            limit: MAX_TYPED_MEMORY_FIELDS_JSON_BYTES,
        });
    }

    let parsed: JsonValue = serde_json::from_str(raw_json).map_err(|error| {
        MemoryValidationError::InvalidTypedFieldsJson {
            message: error.to_string(),
        }
    })?;
    let fields = typed_memory_fields_object(kind, &parsed)?;
    let specs = typed_memory_field_specs(kind).ok_or_else(|| {
        MemoryValidationError::TypedFieldsUnsupportedKind {
            kind: kind.as_str().to_owned(),
        }
    })?;

    let mut canonical_fields = BTreeMap::<String, JsonValue>::new();
    for (field, value) in fields {
        if value.is_null() {
            continue;
        }
        let Some(spec) = typed_memory_field_spec(specs, field) else {
            return Err(MemoryValidationError::TypedFieldNotAllowed {
                kind: kind.as_str().to_owned(),
                field: field.to_owned(),
            });
        };
        match spec.shape {
            TypedMemoryFieldShape::Text => {
                let text =
                    value
                        .as_str()
                        .ok_or_else(|| MemoryValidationError::TypedFieldWrongType {
                            field: field.to_owned(),
                            expected: "string",
                        })?;
                let text = redact(text).trim().to_owned();
                if text.is_empty() {
                    continue;
                }
                validate_typed_memory_field_value_len(field, &text)?;
                canonical_fields.insert(field.to_owned(), JsonValue::String(text));
            }
            TypedMemoryFieldShape::TextList => {
                let values =
                    value
                        .as_array()
                        .ok_or_else(|| MemoryValidationError::TypedFieldWrongType {
                            field: field.to_owned(),
                            expected: "array of strings",
                        })?;
                if values.len() > MAX_TYPED_MEMORY_FIELD_LIST_ITEMS {
                    return Err(MemoryValidationError::TypedFieldListTooLong {
                        field: field.to_owned(),
                        count: values.len(),
                        limit: MAX_TYPED_MEMORY_FIELD_LIST_ITEMS,
                    });
                }
                let mut canonical_values = Vec::new();
                for item in values {
                    let text = item.as_str().ok_or_else(|| {
                        MemoryValidationError::TypedFieldWrongType {
                            field: field.to_owned(),
                            expected: "array of strings",
                        }
                    })?;
                    let text = redact(text).trim().to_owned();
                    if text.is_empty() {
                        continue;
                    }
                    validate_typed_memory_field_value_len(field, &text)?;
                    canonical_values.push(JsonValue::String(text));
                }
                if !canonical_values.is_empty() {
                    canonical_fields.insert(field.to_owned(), JsonValue::Array(canonical_values));
                }
            }
        }
    }

    if canonical_fields.len() > MAX_TYPED_MEMORY_FIELDS {
        return Err(MemoryValidationError::TypedFieldsTooMany {
            count: canonical_fields.len(),
            limit: MAX_TYPED_MEMORY_FIELDS,
        });
    }

    let fields: JsonMap<String, JsonValue> = canonical_fields.into_iter().collect();
    let mut envelope = JsonMap::new();
    envelope.insert(
        "schema".to_owned(),
        JsonValue::String(TYPED_MEMORY_FIELDS_SCHEMA_V1.to_owned()),
    );
    envelope.insert(
        "kind".to_owned(),
        JsonValue::String(kind.as_str().to_owned()),
    );
    envelope.insert("fields".to_owned(), JsonValue::Object(fields));
    serde_json::to_string(&JsonValue::Object(envelope)).map_err(|error| {
        MemoryValidationError::InvalidTypedFieldsJson {
            message: error.to_string(),
        }
    })
}

fn typed_memory_fields_object<'a>(
    kind: &MemoryKind,
    parsed: &'a JsonValue,
) -> Result<&'a JsonMap<String, JsonValue>, MemoryValidationError> {
    let object =
        parsed
            .as_object()
            .ok_or_else(|| MemoryValidationError::InvalidTypedFieldsJson {
                message: "typed fields must be a JSON object".to_owned(),
            })?;

    if let Some(fields) = object.get("fields") {
        if let Some(schema) = object.get("schema") {
            let schema =
                schema
                    .as_str()
                    .ok_or_else(|| MemoryValidationError::TypedFieldWrongType {
                        field: "schema".to_owned(),
                        expected: "string",
                    })?;
            if schema != TYPED_MEMORY_FIELDS_SCHEMA_V1 {
                return Err(MemoryValidationError::InvalidTypedFieldsJson {
                    message: format!("typed fields schema `{schema}` is unsupported"),
                });
            }
        }
        if let Some(actual_kind) = object.get("kind") {
            let actual_kind =
                actual_kind
                    .as_str()
                    .ok_or_else(|| MemoryValidationError::TypedFieldWrongType {
                        field: "kind".to_owned(),
                        expected: "string",
                    })?;
            let actual_kind = MemoryKind::from_str(actual_kind)?;
            if actual_kind != *kind {
                return Err(MemoryValidationError::TypedFieldsKindMismatch {
                    expected: kind.as_str().to_owned(),
                    actual: actual_kind.as_str().to_owned(),
                });
            }
        }
        return fields
            .as_object()
            .ok_or_else(|| MemoryValidationError::TypedFieldWrongType {
                field: "fields".to_owned(),
                expected: "object",
            });
    }

    Ok(object)
}

fn validate_typed_memory_field_value_len(
    field: &str,
    value: &str,
) -> Result<(), MemoryValidationError> {
    if value.len() > MAX_TYPED_MEMORY_FIELD_VALUE_BYTES {
        return Err(MemoryValidationError::TypedFieldTooLong {
            field: field.to_owned(),
            bytes: value.len(),
            limit: MAX_TYPED_MEMORY_FIELD_VALUE_BYTES,
        });
    }
    Ok(())
}

/// Extract kind-specific typed fields from the freeform memory body.
///
/// The body stays authoritative. This helper only recognizes lightweight
/// conventions that agents already write naturally, including README negative
/// evidence ledger prefixes (`family-*`, `cause-*`, `regression-*`,
/// `reverted-at-*`) and simple labeled clauses (`Family:`, `Cause:`,
/// `Options:`, `Chosen:`, `Command:`, `Exit meaning:`, etc.).
pub fn extract_typed_memory_fields_json_with_redactor<F>(
    kind: &MemoryKind,
    content: &str,
    redact: F,
) -> Result<Option<String>, MemoryValidationError>
where
    F: FnMut(&str) -> String,
{
    let fields = match kind {
        MemoryKind::Failure => extract_failure_typed_memory_fields(content),
        MemoryKind::Decision => extract_decision_typed_memory_fields(content),
        MemoryKind::Command => extract_command_typed_memory_fields(content),
        MemoryKind::Risk | MemoryKind::AntiPattern => extract_risk_typed_memory_fields(content),
        MemoryKind::Rule
        | MemoryKind::Fact
        | MemoryKind::Convention
        | MemoryKind::PlaybookStep
        | MemoryKind::Custom(_) => return Ok(None),
    };
    if fields.is_empty() {
        return Ok(None);
    }
    let raw_json = serde_json::to_string(&fields).map_err(|error| {
        MemoryValidationError::InvalidTypedFieldsJson {
            message: error.to_string(),
        }
    })?;
    canonicalize_typed_memory_fields_json_with_redactor(kind, &raw_json, redact).map(Some)
}

fn extract_failure_typed_memory_fields(content: &str) -> BTreeMap<String, JsonValue> {
    let mut fields = BTreeMap::new();
    insert_text_field(
        &mut fields,
        "cause",
        extract_prefixed_token(content, "cause-")
            .or_else(|| extract_labeled_value(content, &["cause:", "cause=", "root cause:"])),
    );
    insert_text_field(
        &mut fields,
        "regression_surface",
        extract_prefixed_token(content, "regression-").or_else(|| {
            extract_labeled_value(content, &["regression:", "regression surface:", "lost on:"])
        }),
    );
    insert_text_field(
        &mut fields,
        "reverted_at_sha",
        extract_prefixed_token(content, "reverted-at-")
            .or_else(|| extract_sha_after_any(content, &["reverted at sha", "reverted at"])),
    );
    insert_text_field(
        &mut fields,
        "family",
        extract_prefixed_token(content, "family-")
            .or_else(|| extract_labeled_value(content, &["family:", "family="])),
    );
    fields
}

fn extract_decision_typed_memory_fields(content: &str) -> BTreeMap<String, JsonValue> {
    let mut fields = BTreeMap::new();
    if let Some(options) = extract_labeled_value_allowing_commas(content, &["options:", "options="])
    {
        let options = split_text_list(&options);
        if !options.is_empty() {
            fields.insert(
                "options".to_owned(),
                JsonValue::Array(options.into_iter().map(JsonValue::String).collect()),
            );
        }
    }
    insert_text_field(
        &mut fields,
        "chosen",
        extract_labeled_value(content, &["chosen:", "chosen=", "decision:", "selected:"]),
    );
    insert_text_field(
        &mut fields,
        "rationale",
        extract_labeled_value(content, &["rationale:", "because:", "why:"]),
    );
    insert_text_field(
        &mut fields,
        "supersedes",
        extract_labeled_value(content, &["supersedes:", "supersedes="]),
    );
    fields
}

fn extract_command_typed_memory_fields(content: &str) -> BTreeMap<String, JsonValue> {
    let mut fields = BTreeMap::new();
    insert_text_field(
        &mut fields,
        "command",
        extract_labeled_line_value(content, &["command:", "cmd:"]).or_else(|| {
            extract_first_backtick_segment(content).filter(|value| looks_like_command(value))
        }),
    );
    insert_text_field(
        &mut fields,
        "when_to_use",
        extract_labeled_value(content, &["when to use:", "use when:", "when:"]),
    );
    insert_text_field(
        &mut fields,
        "exit_meaning",
        extract_labeled_value(content, &["exit meaning:", "exit codes:", "exit code:"])
            .or_else(|| extract_exit_meaning_clause(content)),
    );
    fields
}

fn extract_risk_typed_memory_fields(content: &str) -> BTreeMap<String, JsonValue> {
    let mut fields = BTreeMap::new();
    insert_text_field(
        &mut fields,
        "trigger",
        extract_labeled_value(content, &["trigger:", "trigger=", "when:"]),
    );
    insert_text_field(
        &mut fields,
        "blast_radius",
        extract_labeled_value(content, &["blast radius:", "impact:", "risk:"]),
    );
    insert_text_field(
        &mut fields,
        "safer_alternative",
        extract_labeled_value(
            content,
            &["safer alternative:", "safer:", "mitigation:", "instead:"],
        ),
    );
    fields
}

fn insert_text_field(fields: &mut BTreeMap<String, JsonValue>, field: &str, value: Option<String>) {
    if let Some(value) = value.and_then(|value| clean_typed_extracted_value(&value)) {
        fields.insert(field.to_owned(), JsonValue::String(value));
    }
}

fn extract_labeled_value(content: &str, labels: &[&str]) -> Option<String> {
    extract_labeled_value_with(content, labels, extract_clause_value)
}

fn extract_labeled_value_allowing_commas(content: &str, labels: &[&str]) -> Option<String> {
    extract_labeled_value_with(content, labels, extract_clause_value_allowing_commas)
}

fn extract_labeled_line_value(content: &str, labels: &[&str]) -> Option<String> {
    extract_labeled_value_with(content, labels, extract_line_value)
}

fn extract_labeled_value_with(
    content: &str,
    labels: &[&str],
    extractor: fn(&str) -> Option<String>,
) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    for label in labels {
        let label = label.to_ascii_lowercase();
        if let Some(start) = lower.find(&label) {
            let value_start = start + label.len();
            return extractor(&content[value_start..]);
        }
    }
    None
}

fn extract_prefixed_token(content: &str, prefix: &str) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    let start = lower.find(prefix)?;
    let token_start = start + prefix.len();
    let token = content[token_start..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect::<String>();
    clean_typed_extracted_value(&token).map(|value| value.replace('_', "-"))
}

fn extract_sha_after_any(content: &str, phrases: &[&str]) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    for phrase in phrases {
        let phrase = phrase.to_ascii_lowercase();
        if let Some(start) = lower.find(&phrase) {
            let tail = &content[start + phrase.len()..];
            if let Some(sha) = first_hexish_token(tail) {
                return Some(sha);
            }
        }
    }
    None
}

fn first_hexish_token(content: &str) -> Option<String> {
    for raw in content.split(|ch: char| !ch.is_ascii_hexdigit()) {
        if (7..=64).contains(&raw.len()) && raw.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Some(raw.to_ascii_lowercase());
        }
    }
    None
}

fn extract_exit_meaning_clause(content: &str) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    let start = lower.find("exit code ").or_else(|| lower.find("exit "))?;
    extract_clause_value(&content[start..])
}

fn extract_first_backtick_segment(content: &str) -> Option<String> {
    let start = content.find('`')?;
    let tail = &content[start + 1..];
    let end = tail.find('`')?;
    clean_typed_extracted_value(&tail[..end])
}

fn looks_like_command(value: &str) -> bool {
    value.contains(' ') || value.contains("--") || value.contains('/')
}

fn extract_clause_value(content: &str) -> Option<String> {
    extract_clause_value_inner(content, false)
}

fn extract_clause_value_allowing_commas(content: &str) -> Option<String> {
    extract_clause_value_inner(content, true)
}

fn extract_line_value(content: &str) -> Option<String> {
    let trimmed = content
        .trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, '-' | '=' | ':' | '>'));
    let mut value = String::new();
    for ch in trimmed.chars() {
        if matches!(ch, '\n' | '\r' | ';') {
            break;
        }
        value.push(ch);
    }
    clean_typed_extracted_value(&value)
}

fn extract_clause_value_inner(content: &str, allow_commas: bool) -> Option<String> {
    let trimmed = content
        .trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, '-' | '=' | ':' | '>'));
    let mut value = String::new();
    for ch in trimmed.chars() {
        if matches!(ch, '\n' | '\r' | ';' | '.') {
            break;
        }
        if ch == ',' && !allow_commas && !value.trim().is_empty() {
            break;
        }
        value.push(ch);
    }
    clean_typed_extracted_value(&value)
}

fn split_text_list(value: &str) -> Vec<String> {
    value
        .replace(" vs ", ",")
        .replace(" or ", ",")
        .replace('|', ",")
        .split(',')
        .filter_map(clean_typed_extracted_value)
        .take(MAX_TYPED_MEMORY_FIELD_LIST_ITEMS)
        .collect()
}

fn clean_typed_extracted_value(value: &str) -> Option<String> {
    let cleaned = value
        .trim()
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`' | '[' | ']' | '(' | ')' | ':'))
        .trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_owned())
    }
}

impl MemoryKind {
    /// Stable lowercase wire form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Rule => "rule",
            Self::Fact => "fact",
            Self::Decision => "decision",
            Self::Failure => "failure",
            Self::Command => "command",
            Self::Convention => "convention",
            Self::AntiPattern => "anti-pattern",
            Self::Risk => "risk",
            Self::PlaybookStep => "playbook-step",
            Self::Custom(value) => value.as_str(),
        }
    }

    /// Returns `true` if `name` parses to a known kind (not [`Custom`]).
    #[must_use]
    pub fn is_known(name: &str) -> bool {
        KNOWN_MEMORY_KINDS.contains(&normalized_memory_kind_token(name).as_str())
    }
}

impl fmt::Display for MemoryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MemoryKind {
    type Err = MemoryValidationError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let normalized = normalized_memory_kind_token(input);
        match normalized.as_str() {
            "rule" => Ok(Self::Rule),
            "fact" => Ok(Self::Fact),
            "decision" => Ok(Self::Decision),
            "failure" => Ok(Self::Failure),
            "command" => Ok(Self::Command),
            "convention" => Ok(Self::Convention),
            "anti-pattern" => Ok(Self::AntiPattern),
            "risk" => Ok(Self::Risk),
            "playbook-step" => Ok(Self::PlaybookStep),
            other => {
                if other.is_empty() {
                    return Err(MemoryValidationError::EmptyKind);
                }
                if !is_valid_kind_identifier(other) {
                    return Err(MemoryValidationError::InvalidKind {
                        input: input.to_owned(),
                    });
                }
                Ok(Self::Custom(other.to_owned()))
            }
        }
    }
}

/// Validated tag — NFC-normalized, lowercase ASCII letters (Unicode letters
/// preserve their case), 1–64 bytes.
///
/// Accepted characters (bead bd-17c65.3.3 / C3):
/// - ASCII letters `[a-zA-Z]` (lowercased to `[a-z]`)
/// - ASCII digits `[0-9]`
/// - Punctuation: `.`, `_`, `:`, `-`
/// - Unicode letters / marks / numbers (`char::is_alphanumeric()` plus marks)
///
/// Explicitly rejected: whitespace, `,`, `=`, `/`, `\`, `;`, `*`, `?`, `|`,
/// control characters. These are reserved as tag-list and path delimiters
/// across the storage / search / config layers.
///
/// Tags survive JSON round-trips byte-for-byte: a `Tag` parsed from
/// upper-case input emits its lower-case canonical form on display.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Tag(String);

impl Tag {
    /// Construct a tag from raw input.
    ///
    /// Normalization pipeline (deterministic):
    /// 1. Trim leading/trailing ASCII whitespace.
    /// 2. NFC-normalize Unicode codepoints so equivalent composed/decomposed
    ///    forms collapse to the same canonical bytes.
    /// 3. Lowercase ASCII letters (Unicode letters preserve their case to
    ///    avoid surprises in locale-dependent casing of Greek/Turkish/etc.).
    /// 4. Reject inputs over [`MAX_TAG_BYTES`] (measured AFTER normalization
    ///    since NFC may shrink length).
    /// 5. Reject inputs containing any character not in the accepted set.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryValidationError::EmptyTag`] for empty input,
    /// [`MemoryValidationError::TagTooLong`] when normalized input exceeds
    /// [`MAX_TAG_BYTES`], and [`MemoryValidationError::InvalidTag`] when
    /// the normalized form contains a rejected character.
    pub fn parse(input: &str) -> Result<Self, MemoryValidationError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(MemoryValidationError::EmptyTag);
        }
        // NFC normalize then lowercase ASCII letters. Unicode letters keep
        // their case (NFC-only) so we don't depend on locale-aware casing.
        let normalized: String = unicode_normalization::UnicodeNormalization::nfc(trimmed)
            .map(|ch| {
                if ch.is_ascii_uppercase() {
                    ch.to_ascii_lowercase()
                } else {
                    ch
                }
            })
            .collect();
        if normalized.len() > MAX_TAG_BYTES {
            return Err(MemoryValidationError::TagTooLong {
                input: input.to_owned(),
                limit: MAX_TAG_BYTES,
            });
        }
        if !is_valid_tag_str(&normalized) {
            return Err(MemoryValidationError::InvalidTag {
                input: input.to_owned(),
            });
        }
        Ok(Self(normalized))
    }

    /// Return the canonical normalized form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for Tag {
    type Err = MemoryValidationError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

/// Validated memory body. Non-empty, ≤ [`MAX_CONTENT_BYTES`].
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MemoryContent(String);

impl MemoryContent {
    /// Construct after trimming surrounding ASCII whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryValidationError::EmptyContent`] if the trimmed
    /// body is empty, and [`MemoryValidationError::ContentTooLarge`] if
    /// the input exceeds [`MAX_CONTENT_BYTES`] before trimming.
    pub fn parse(input: &str) -> Result<Self, MemoryValidationError> {
        if input.len() > MAX_CONTENT_BYTES {
            return Err(MemoryValidationError::ContentTooLarge {
                bytes: input.len(),
                limit: MAX_CONTENT_BYTES,
            });
        }
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(MemoryValidationError::EmptyContent);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Borrow the canonical body text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Return the byte length of the canonical body.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.len()
    }
}

/// Bounded score in the unit interval `0.0..=1.0`.
///
/// Wraps an `f32`; the bound is enforced at construction time.
/// Equality and ordering reuse the underlying `f32` semantics with
/// `NaN` rejected at parse time so total ordering is safe.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct UnitScore(f32);

impl UnitScore {
    /// Try to wrap `value` if it is finite and in `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryValidationError::ScoreOutOfRange`] for `NaN`,
    /// infinities, or values outside the unit interval.
    pub fn parse(value: f32) -> Result<Self, MemoryValidationError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(MemoryValidationError::ScoreOutOfRange { value });
        }
        Ok(Self(value))
    }

    /// Return the underlying `f32`.
    #[must_use]
    pub const fn into_inner(self) -> f32 {
        self.0
    }

    /// Return the lowest possible score (`0.0`).
    #[must_use]
    pub fn zero() -> Self {
        Self(0.0)
    }

    /// Return the default initial score for a freshly captured memory
    /// (`0.5`).
    #[must_use]
    pub fn neutral() -> Self {
        Self(0.5)
    }

    /// Return the maximum possible score (`1.0`).
    #[must_use]
    pub fn one() -> Self {
        Self(1.0)
    }
}

impl fmt::Display for UnitScore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:.4}", self.0)
    }
}

/// Confidence in a memory's correctness.
pub type Confidence = UnitScore;
/// Utility — how often a memory has helped agents.
pub type Utility = UnitScore;
/// Importance — operator-supplied salience boost.
pub type Importance = UnitScore;

/// Errors produced by any of the validators above.
///
/// Only `PartialEq` is derived because the [`ScoreOutOfRange`] variant
/// carries an `f32`. Comparisons against `NaN`-bearing instances do
/// not happen in practice — that path is explicitly tested below — but
/// formal `Eq` is intentionally not implied.
#[derive(Clone, Debug, PartialEq)]
pub enum MemoryValidationError {
    UnknownLevel {
        input: String,
    },
    EmptyKind,
    InvalidKind {
        input: String,
    },
    EmptyTag,
    TagTooLong {
        input: String,
        limit: usize,
    },
    InvalidTag {
        input: String,
    },
    EmptyContent,
    ContentTooLarge {
        bytes: usize,
        limit: usize,
    },
    ScoreOutOfRange {
        value: f32,
    },
    TypedFieldsUnsupportedKind {
        kind: String,
    },
    TypedFieldsJsonTooLarge {
        bytes: usize,
        limit: usize,
    },
    InvalidTypedFieldsJson {
        message: String,
    },
    TypedFieldNotAllowed {
        kind: String,
        field: String,
    },
    TypedFieldWrongType {
        field: String,
        expected: &'static str,
    },
    TypedFieldTooLong {
        field: String,
        bytes: usize,
        limit: usize,
    },
    TypedFieldListTooLong {
        field: String,
        count: usize,
        limit: usize,
    },
    TypedFieldsTooMany {
        count: usize,
        limit: usize,
    },
    TypedFieldsKindMismatch {
        expected: String,
        actual: String,
    },
}

impl fmt::Display for MemoryValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownLevel { input } => write!(
                formatter,
                "unknown memory level `{input}`; expected one of working, episodic, semantic, procedural"
            ),
            Self::EmptyKind => formatter.write_str("memory kind cannot be empty"),
            Self::InvalidKind { input } => write!(
                formatter,
                "memory kind `{input}` must normalize to [a-z][a-z0-9-]*"
            ),
            Self::EmptyTag => formatter.write_str("tag cannot be empty"),
            Self::TagTooLong { input, limit } => write!(
                formatter,
                "tag `{input}` is {} bytes; limit is {limit}",
                input.len()
            ),
            Self::InvalidTag { input } => write!(
                formatter,
                "tag `{input}` contains characters outside the accepted set (ASCII letters/digits, `.`, `_`, `:`, `-`, Unicode letters/marks/numbers); reserved delimiters such as `,` ` ` `=` `/` `\\` `;` are rejected"
            ),
            Self::EmptyContent => formatter.write_str("memory content cannot be empty after trim"),
            Self::ContentTooLarge { bytes, limit } => write!(
                formatter,
                "memory content is {bytes} bytes; limit is {limit}"
            ),
            Self::ScoreOutOfRange { value } => write!(
                formatter,
                "score {value} is outside the unit interval [0.0, 1.0]"
            ),
            Self::TypedFieldsUnsupportedKind { kind } => write!(
                formatter,
                "memory kind `{kind}` does not support typed fields"
            ),
            Self::TypedFieldsJsonTooLarge { bytes, limit } => write!(
                formatter,
                "typed memory fields JSON is {bytes} bytes; limit is {limit}"
            ),
            Self::InvalidTypedFieldsJson { message } => {
                write!(formatter, "typed memory fields JSON is invalid: {message}")
            }
            Self::TypedFieldNotAllowed { kind, field } => write!(
                formatter,
                "typed memory field `{field}` is not allowed for kind `{kind}`"
            ),
            Self::TypedFieldWrongType { field, expected } => {
                write!(formatter, "typed memory field `{field}` must be {expected}")
            }
            Self::TypedFieldTooLong {
                field,
                bytes,
                limit,
            } => write!(
                formatter,
                "typed memory field `{field}` is {bytes} bytes; limit is {limit}"
            ),
            Self::TypedFieldListTooLong {
                field,
                count,
                limit,
            } => write!(
                formatter,
                "typed memory field `{field}` has {count} items; limit is {limit}"
            ),
            Self::TypedFieldsTooMany { count, limit } => write!(
                formatter,
                "typed memory fields contain {count} populated fields; limit is {limit}"
            ),
            Self::TypedFieldsKindMismatch { expected, actual } => write!(
                formatter,
                "typed memory fields kind `{actual}` does not match memory kind `{expected}`"
            ),
        }
    }
}

impl std::error::Error for MemoryValidationError {}

/// Returns `true` if every character in `s` is acceptable in a tag.
///
/// Acceptance (per bead bd-17c65.3.3 / C3):
/// - ASCII letters `[a-zA-Z]` (case-preserved; the caller has already
///   lowercased ASCII via NFC pipeline)
/// - ASCII digits `[0-9]`
/// - Punctuation: `.` `_` `:` `-`
/// - Unicode letters, marks (combining accents), and number-category
///   codepoints — `char::is_alphanumeric()` covers most of these.
///
/// Rejection (explicit delimiters used elsewhere in the system):
/// - whitespace (`char::is_whitespace`)
/// - `,` `=` `/` `\\` `;` `*` `?` `|` `<` `>` `"` `'` `` ` `` `(` `)` `[` `]`
///   `{` `}` `@` `#` `$` `%` `^` `&` `+` `~`
/// - Control characters
fn is_valid_tag_str(s: &str) -> bool {
    s.chars().all(|ch| {
        if ch.is_ascii() {
            matches!(ch, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | ':' | '-')
        } else if ch.is_whitespace() || ch.is_control() {
            false
        } else {
            // Accept Unicode letters, marks, and numbers; reject punctuation /
            // symbols (which include the dangerous Unicode delimiters).
            ch.is_alphanumeric()
                || matches!(
                    unicode_normalization::char::canonical_combining_class(ch),
                    1..=255
                )
        }
    })
}

fn is_valid_kind_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(b'a'..=b'z') => {}
        _ => return false,
    }
    bytes.all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        Confidence, KNOWN_MEMORY_KINDS, MAX_CONTENT_BYTES, MAX_TAG_BYTES, MemoryContent,
        MemoryKind, MemoryLevel, MemoryValidationError, TYPED_MEMORY_FIELDS_SCHEMA_V1, Tag,
        UnitScore, canonicalize_typed_memory_fields_json,
        canonicalize_typed_memory_fields_json_with_redactor,
        extract_typed_memory_fields_json_with_redactor,
    };

    #[test]
    fn level_round_trip_for_every_variant() {
        for level in MemoryLevel::all() {
            let rendered = level.to_string();
            let parsed = match MemoryLevel::from_str(&rendered) {
                Ok(value) => value,
                Err(error) => panic!("level {level:?} failed to round-trip: {error:?}"), // ubs:ignore
            };
            assert_eq!(parsed, level);
        }
    }

    #[test]
    fn level_rejects_unknown_input() {
        let err = match MemoryLevel::from_str("durable") {
            Ok(value) => panic!("expected error, got Ok({value:?})"), // ubs:ignore
            Err(error) => error,
        };
        assert_eq!(
            err,
            MemoryValidationError::UnknownLevel {
                input: "durable".to_owned(),
            }
        );
    }

    #[test]
    fn level_accepts_operator_spelling_variants() {
        assert_eq!(
            MemoryLevel::from_str(" Working ").expect("trimmed level parses"),
            MemoryLevel::Working
        );
        assert_eq!(
            MemoryLevel::from_str("PROCEDURAL").expect("uppercase level parses"),
            MemoryLevel::Procedural
        );
    }

    #[test]
    fn kind_round_trip_for_every_known_variant() {
        for name in KNOWN_MEMORY_KINDS {
            let parsed = match MemoryKind::from_str(name) {
                Ok(value) => value,
                Err(error) => panic!("known kind `{name}` failed: {error:?}"), // ubs:ignore
            };
            assert_eq!(parsed.as_str(), *name);
            assert!(MemoryKind::is_known(name));
        }
    }

    #[test]
    fn kind_accepts_custom_identifier() {
        let parsed = match MemoryKind::from_str("project-rule") {
            Ok(value) => value,
            Err(error) => panic!("custom kind failed: {error:?}"), // ubs:ignore
        };
        assert!(matches!(parsed, MemoryKind::Custom(_)));
        assert_eq!(parsed.as_str(), "project-rule");
        assert!(!MemoryKind::is_known("project-rule"));
    }

    #[test]
    fn kind_accepts_operator_spelling_variants() {
        assert_eq!(
            MemoryKind::from_str(" Anti_Pattern ").expect("known alias parses"),
            MemoryKind::AntiPattern
        );
        assert_eq!(
            MemoryKind::from_str("PLAYBOOK_STEP").expect("known underscore alias parses"),
            MemoryKind::PlaybookStep
        );
        assert_eq!(
            MemoryKind::from_str("AntiPattern").expect("known PascalCase alias parses"),
            MemoryKind::AntiPattern
        );
        assert_eq!(
            MemoryKind::from_str("playbookStep").expect("known camelCase alias parses"),
            MemoryKind::PlaybookStep
        );
        assert!(MemoryKind::is_known(" Anti_Pattern "));
        assert!(MemoryKind::is_known("AntiPattern"));

        let custom = MemoryKind::from_str(" Project_Rule ").expect("custom identifier normalizes");
        assert_eq!(custom.as_str(), "project-rule");
        assert!(matches!(custom, MemoryKind::Custom(_)));

        let custom = MemoryKind::from_str(" ProjectRule ").expect("custom camelCase normalizes");
        assert_eq!(custom.as_str(), "project-rule");
        assert!(matches!(custom, MemoryKind::Custom(_)));
    }

    #[test]
    fn kind_rejects_empty_and_invalid_identifiers() {
        for input in ["", "1rule", "rule!", "ru le"] {
            let err = match MemoryKind::from_str(input) {
                Ok(value) => panic!("expected error for `{input}`, got Ok({value:?})"),
                Err(error) => error,
            };
            assert!(
                matches!(
                    err,
                    MemoryValidationError::EmptyKind | MemoryValidationError::InvalidKind { .. }
                ),
                "wrong variant for `{input}`: {err:?}"
            );
        }
    }

    #[test]
    fn typed_memory_fields_canonicalize_failure_fields() {
        let canonical = canonicalize_typed_memory_fields_json(
            &MemoryKind::Failure,
            r#"{"family":"aggressive-prefetch","cause":" stale cache ","regression_surface":null}"#,
        )
        .expect("failure fields canonicalize");
        let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");
        assert_eq!(parsed["schema"], TYPED_MEMORY_FIELDS_SCHEMA_V1);
        assert_eq!(parsed["kind"], "failure");
        assert_eq!(parsed["fields"]["cause"], "stale cache");
        assert_eq!(parsed["fields"]["family"], "aggressive-prefetch");
        assert!(parsed["fields"].get("regression_surface").is_none());
    }

    #[test]
    fn typed_memory_fields_redact_every_string_value() {
        let canonical = canonicalize_typed_memory_fields_json_with_redactor(
            &MemoryKind::Decision,
            r#"{"options":["use API_KEY=secret-value"," keep local "],"chosen":"API_KEY=secret-value"}"#,
            |value| value.replace("secret-value", "[REDACTED:test]"),
        )
        .expect("decision fields canonicalize");
        assert!(!canonical.contains("secret-value"));
        let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");
        assert_eq!(
            parsed["fields"]["options"][0],
            "use API_KEY=[REDACTED:test]"
        );
        assert_eq!(parsed["fields"]["options"][1], "keep local");
        assert_eq!(parsed["fields"]["chosen"], "API_KEY=[REDACTED:test]");
    }

    #[test]
    fn typed_memory_fields_reject_unknown_field_for_kind() {
        let err = canonicalize_typed_memory_fields_json(
            &MemoryKind::Command,
            r#"{"cause":"wrong shape"}"#,
        )
        .expect_err("wrong command field rejected");
        assert!(matches!(
            err,
            MemoryValidationError::TypedFieldNotAllowed { .. }
        ));
    }

    #[test]
    fn typed_memory_fields_extract_failure_patterns_from_body() {
        let canonical = extract_typed_memory_fields_json_with_redactor(
            &MemoryKind::Failure,
            "Tried page-level cache prefetch. Result: -8% on small-N reads. Reverted at SHA 9af3c21. Family: aggressive prefetch, third failure in this family. Cause: cache pollution. Regression surface: small-N reads.",
            str::to_owned,
        )
        .expect("failure body extracts")
        .expect("failure body has typed fields");
        let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");

        assert_eq!(parsed["schema"], TYPED_MEMORY_FIELDS_SCHEMA_V1);
        assert_eq!(parsed["kind"], "failure");
        assert_eq!(parsed["fields"]["cause"], "cache pollution");
        assert_eq!(parsed["fields"]["family"], "aggressive prefetch");
        assert_eq!(parsed["fields"]["regression_surface"], "small-N reads");
        assert_eq!(parsed["fields"]["reverted_at_sha"], "9af3c21");
    }

    #[test]
    fn typed_memory_fields_extract_decision_options_from_body() {
        let canonical = extract_typed_memory_fields_json_with_redactor(
            &MemoryKind::Decision,
            "Options: local cache, RCH remote or no-op. Chosen: RCH remote. Rationale: avoids local Cargo. Supersedes: bd-old.",
            str::to_owned,
        )
        .expect("decision body extracts")
        .expect("decision body has typed fields");
        let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");

        assert_eq!(parsed["fields"]["options"][0], "local cache");
        assert_eq!(parsed["fields"]["options"][1], "RCH remote");
        assert_eq!(parsed["fields"]["options"][2], "no-op");
        assert_eq!(parsed["fields"]["chosen"], "RCH remote");
        assert_eq!(parsed["fields"]["rationale"], "avoids local Cargo");
        assert_eq!(parsed["fields"]["supersedes"], "bd-old");
    }

    #[test]
    fn typed_memory_fields_extract_command_without_rewriting_literal() {
        let canonical = extract_typed_memory_fields_json_with_redactor(
            &MemoryKind::Command,
            "Command: ./scripts/check_local_cargo_tripwire.sh --json\nWhen to use: before remote proof\nExit code: 7 means policy denied",
            str::to_owned,
        )
        .expect("command body extracts")
        .expect("command body has typed fields");
        let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");

        assert_eq!(
            parsed["fields"]["command"],
            "./scripts/check_local_cargo_tripwire.sh --json"
        );
        assert_eq!(parsed["fields"]["when_to_use"], "before remote proof");
        assert_eq!(parsed["fields"]["exit_meaning"], "7 means policy denied");
    }

    #[test]
    fn typed_memory_fields_extract_risk_patterns_from_body() {
        let canonical = extract_typed_memory_fields_json_with_redactor(
            &MemoryKind::Risk,
            "Trigger: running local Cargo during RCH-only swarms. Blast radius: fills the internal SSD. Safer alternative: use RCH remote proof.",
            str::to_owned,
        )
        .expect("risk body extracts")
        .expect("risk body has typed fields");
        let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");

        assert_eq!(parsed["schema"], TYPED_MEMORY_FIELDS_SCHEMA_V1);
        assert_eq!(parsed["kind"], "risk");
        assert_eq!(
            parsed["fields"]["trigger"],
            "running local Cargo during RCH-only swarms"
        );
        assert_eq!(parsed["fields"]["blast_radius"], "fills the internal SSD");
        assert_eq!(
            parsed["fields"]["safer_alternative"],
            "use RCH remote proof"
        );
    }

    #[test]
    fn typed_memory_fields_do_not_fabricate_from_bare_bodies() {
        for kind in [
            MemoryKind::Failure,
            MemoryKind::Decision,
            MemoryKind::Command,
            MemoryKind::Risk,
            MemoryKind::AntiPattern,
        ] {
            let extracted = extract_typed_memory_fields_json_with_redactor(
                &kind,
                "This memory intentionally has no typed labels, no negative-evidence prefixes, and no command literal.",
                str::to_owned,
            )
            .expect("bare body extraction is allowed");

            assert_eq!(extracted, None, "kind {kind:?} fabricated typed fields");
        }
    }

    #[test]
    fn typed_memory_fields_extraction_is_idempotent_and_lossless() {
        let canonical = extract_typed_memory_fields_json_with_redactor(
            &MemoryKind::Decision,
            "Options: local cache, RCH remote. Chosen: RCH remote. Rationale: avoids local Cargo. Supersedes: mem_00000000000000000000000001.",
            str::to_owned,
        )
        .expect("decision body extracts")
        .expect("decision body has typed fields");

        let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");
        let serialized = serde_json::to_string(&parsed).expect("serialize parsed sidecar");
        let recanonicalized =
            canonicalize_typed_memory_fields_json(&MemoryKind::Decision, &serialized)
                .expect("sidecar recanonicalizes");

        assert_eq!(recanonicalized, canonical);
        assert_eq!(parsed["fields"]["options"][0], "local cache");
        assert_eq!(parsed["fields"]["options"][1], "RCH remote");
        assert_eq!(
            parsed["fields"]["supersedes"],
            "mem_00000000000000000000000001"
        );
    }

    #[test]
    fn typed_memory_fields_extract_returns_none_for_unsupported_kind() {
        let extracted = extract_typed_memory_fields_json_with_redactor(
            &MemoryKind::Rule,
            "Family: aggressive prefetch. Cause: cache pollution.",
            str::to_owned,
        )
        .expect("unsupported kind is accepted");

        assert!(extracted.is_none());
    }

    #[test]
    fn tag_lowercases_and_validates() {
        let tag = match Tag::parse("Security:Auth-Bypass") {
            Ok(value) => value,
            Err(error) => panic!("valid tag rejected: {error:?}"),
        };
        assert_eq!(tag.as_str(), "security:auth-bypass");
        assert_eq!(tag.to_string(), "security:auth-bypass");
    }

    #[test]
    fn tag_rejects_empty_and_too_long_and_invalid_bytes() {
        match Tag::parse("") {
            Ok(_) => panic!("empty tag should fail"),
            Err(MemoryValidationError::EmptyTag) => {}
            Err(other) => panic!("wrong variant: {other:?}"),
        }
        // Whitespace-only input should also reject as empty after trim.
        match Tag::parse("   ") {
            Ok(_) => panic!("whitespace-only tag should fail"),
            Err(MemoryValidationError::EmptyTag) => {}
            Err(other) => panic!("wrong variant for whitespace: {other:?}"),
        }
        let huge = "a".repeat(MAX_TAG_BYTES + 1);
        match Tag::parse(&huge) {
            Ok(_) => panic!("oversized tag should fail"),
            Err(MemoryValidationError::TagTooLong { limit, .. }) => {
                assert_eq!(limit, MAX_TAG_BYTES);
            }
            Err(other) => panic!("wrong variant: {other:?}"),
        }
        // Reserved delimiters and symbols stay rejected (C3 invariant).
        for bad in [
            "space tag",
            "slash/path",
            "emoji-🎉",
            "a,b",
            "a=b",
            "a;b",
            "a*b",
            "back\\slash",
            "pipe|tag",
            "ampers&and",
            "at@sign",
            "hash#sign",
        ] {
            match Tag::parse(bad) {
                Ok(_) => panic!("invalid tag `{bad}` should fail"),
                Err(MemoryValidationError::InvalidTag { .. }) => {}
                Err(other) => panic!("wrong variant for `{bad}`: {other:?}"),
            }
        }
    }

    #[test]
    fn tag_accepts_dots_underscores_unicode_per_c3() {
        // Bead bd-17c65.3.3 (C3): version strings, namespace paths,
        // underscores, and Unicode letters must all accept.
        let cases: &[(&str, &str)] = &[
            ("v0.1.0", "v0.1.0"),
            ("v0.2.0", "v0.2.0"),
            ("release:0.1.0", "release:0.1.0"),
            ("policy.detector", "policy.detector"),
            ("policy.secret_detector", "policy.secret_detector"),
            ("under_score", "under_score"),
            ("mixed.dots_and-dashes:01", "mixed.dots_and-dashes:01"),
            // Unicode letters (NFC-stable) — case preserved for non-ASCII.
            ("mémoire", "mémoire"),
            ("café", "café"),
            ("日本語", "日本語"),
            // Leading/trailing whitespace is trimmed.
            ("  release  ", "release"),
        ];
        for (input, expected) in cases {
            match Tag::parse(input) {
                Ok(tag) => assert_eq!(
                    tag.as_str(),
                    *expected,
                    "tag `{input}` should normalize to `{expected}`"
                ),
                Err(error) => panic!("tag `{input}` should accept, got: {error:?}"),
            }
        }
    }

    #[test]
    fn tag_nfc_normalizes_composed_and_decomposed_forms() {
        // U+00E9 (composed é) and U+0065 U+0301 (decomposed e + combining
        // acute) must produce the same canonical tag bytes.
        let composed = "café"; // U+00E9
        let decomposed = "cafe\u{0301}"; // U+0065 + U+0301
        let parsed_composed = match Tag::parse(composed) {
            Ok(tag) => tag,
            Err(error) => panic!("composed accepts: {error:?}"),
        };
        let parsed_decomposed = match Tag::parse(decomposed) {
            Ok(tag) => tag,
            Err(error) => panic!("decomposed accepts: {error:?}"),
        };
        assert_eq!(
            parsed_composed.as_str(),
            parsed_decomposed.as_str(),
            "NFC should collapse composed and decomposed forms"
        );
    }

    #[test]
    fn tag_ascii_uppercase_lowercases_unicode_preserves_case() {
        // ASCII letters lowercase; Unicode case-preserving (no locale dep).
        let ascii = match Tag::parse("RELEASE") {
            Ok(tag) => tag,
            Err(error) => panic!("upper-ASCII accepts: {error:?}"),
        };
        assert_eq!(ascii.as_str(), "release");
        let unicode = match Tag::parse("MÉMOIRE") {
            Ok(tag) => tag,
            Err(error) => panic!("upper-unicode accepts: {error:?}"),
        };
        // We preserve Unicode case to avoid locale-dependent casing pitfalls.
        // The leading 'M' lowercases (it's ASCII) but 'É' stays uppercase.
        assert!(
            unicode.as_str().starts_with("mÉ") || unicode.as_str().starts_with("mé"),
            "got: {}",
            unicode.as_str()
        );
    }

    #[test]
    fn tag_ordering_is_by_canonical_form() {
        let upper = match Tag::parse("Z-tag") {
            Ok(value) => value,
            Err(error) => panic!("{error:?}"),
        };
        let lower = match Tag::parse("a-tag") {
            Ok(value) => value,
            Err(error) => panic!("{error:?}"),
        };
        let mut tags = [upper, lower];
        tags.sort();
        assert_eq!(tags[0].as_str(), "a-tag");
        assert_eq!(tags[1].as_str(), "z-tag");
    }

    #[test]
    fn content_trims_and_rejects_empty() {
        let content = match MemoryContent::parse("  hello world  \n") {
            Ok(value) => value,
            Err(error) => panic!("valid content rejected: {error:?}"),
        };
        assert_eq!(content.as_str(), "hello world");

        for input in ["", "    ", "\n\t  \r\n"] {
            match MemoryContent::parse(input) {
                Ok(_) => panic!("empty/whitespace content should fail for `{input}`"),
                Err(MemoryValidationError::EmptyContent) => {}
                Err(other) => panic!("wrong variant: {other:?}"),
            }
        }
    }

    #[test]
    fn content_rejects_oversized_input() {
        let huge = "x".repeat(MAX_CONTENT_BYTES + 1);
        match MemoryContent::parse(&huge) {
            Ok(_) => panic!("oversized content should fail"),
            Err(MemoryValidationError::ContentTooLarge { limit, .. }) => {
                assert_eq!(limit, MAX_CONTENT_BYTES);
            }
            Err(other) => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn content_byte_len_matches_canonical_form() {
        let content = match MemoryContent::parse("  abc  ") {
            Ok(value) => value,
            Err(error) => panic!("{error:?}"),
        };
        assert_eq!(content.byte_len(), 3);
    }

    #[test]
    fn unit_score_accepts_unit_interval_endpoints() {
        for value in [0.0_f32, 0.5, 1.0] {
            match UnitScore::parse(value) {
                Ok(score) => assert!((score.into_inner() - value).abs() < f32::EPSILON),
                Err(error) => panic!("{value} rejected: {error:?}"),
            }
        }
    }

    #[test]
    fn unit_score_rejects_non_finite_and_out_of_range() {
        for value in [
            -0.001_f32,
            1.001,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            match UnitScore::parse(value) {
                Ok(score) => panic!("{value} accepted: {score:?}"),
                Err(MemoryValidationError::ScoreOutOfRange { .. }) => {}
                Err(other) => panic!("wrong variant: {other:?}"),
            }
        }
    }

    #[test]
    fn unit_score_constants_are_in_range() {
        assert_eq!(UnitScore::zero().into_inner(), 0.0);
        assert_eq!(UnitScore::neutral().into_inner(), 0.5);
        assert_eq!(UnitScore::one().into_inner(), 1.0);
    }

    #[test]
    fn confidence_alias_matches_unit_score() {
        let confidence: Confidence = match Confidence::parse(0.7) {
            Ok(value) => value,
            Err(error) => panic!("{error:?}"),
        };
        assert_eq!(confidence.into_inner(), 0.7);
    }

    #[test]
    fn known_memory_kinds_constant_matches_enum_strings() {
        let from_enum = [
            MemoryKind::Rule,
            MemoryKind::Fact,
            MemoryKind::Decision,
            MemoryKind::Failure,
            MemoryKind::Command,
            MemoryKind::Convention,
            MemoryKind::AntiPattern,
            MemoryKind::Risk,
            MemoryKind::PlaybookStep,
        ];
        let from_enum: Vec<&str> = from_enum.iter().map(MemoryKind::as_str).collect();
        let from_const: Vec<&str> = KNOWN_MEMORY_KINDS.to_vec();
        assert_eq!(from_enum, from_const);
    }
}
