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
pub const TYPED_MEMORY_FIELDS_SCHEMA_V2: &str = "ee.memory.typed_fields.v2";
pub const TYPED_MEMORY_FIELD_METADATA_PREFIX: &str = "typed_field.";
pub const MAX_TYPED_MEMORY_FIELDS: usize = 8;
pub const MAX_TYPED_MEMORY_FIELD_VALUE_BYTES: usize = 4096;
pub const MAX_TYPED_MEMORY_FIELD_LIST_ITEMS: usize = 8;
pub const MAX_TYPED_MEMORY_FIELDS_JSON_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedMemoryFieldShape {
    Text,
    TextList,
    Rfc3339,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TypedMemoryFieldSpec {
    name: &'static str,
    shape: TypedMemoryFieldShape,
    indexed: bool,
}

const FAILURE_TYPED_MEMORY_FIELDS: &[TypedMemoryFieldSpec] = &[
    TypedMemoryFieldSpec {
        name: "cause",
        shape: TypedMemoryFieldShape::Text,
        indexed: true,
    },
    TypedMemoryFieldSpec {
        name: "regression_surface",
        shape: TypedMemoryFieldShape::Text,
        indexed: false,
    },
    TypedMemoryFieldSpec {
        name: "reverted_at_sha",
        shape: TypedMemoryFieldShape::Text,
        indexed: false,
    },
    TypedMemoryFieldSpec {
        name: "family",
        shape: TypedMemoryFieldShape::Text,
        indexed: true,
    },
];

const DECISION_TYPED_MEMORY_FIELDS: &[TypedMemoryFieldSpec] = &[
    TypedMemoryFieldSpec {
        name: "options",
        shape: TypedMemoryFieldShape::TextList,
        indexed: false,
    },
    TypedMemoryFieldSpec {
        name: "chosen",
        shape: TypedMemoryFieldShape::Text,
        indexed: true,
    },
    TypedMemoryFieldSpec {
        name: "rationale",
        shape: TypedMemoryFieldShape::Text,
        indexed: false,
    },
    TypedMemoryFieldSpec {
        name: "supersedes",
        shape: TypedMemoryFieldShape::Text,
        indexed: true,
    },
    TypedMemoryFieldSpec {
        name: "revisit_by",
        shape: TypedMemoryFieldShape::Rfc3339,
        indexed: false,
    },
];

const COMMAND_TYPED_MEMORY_FIELDS: &[TypedMemoryFieldSpec] = &[
    TypedMemoryFieldSpec {
        name: "command",
        shape: TypedMemoryFieldShape::Text,
        indexed: true,
    },
    TypedMemoryFieldSpec {
        name: "when_to_use",
        shape: TypedMemoryFieldShape::Text,
        indexed: false,
    },
    TypedMemoryFieldSpec {
        name: "exit_meaning",
        shape: TypedMemoryFieldShape::Text,
        indexed: false,
    },
];

const RISK_TYPED_MEMORY_FIELDS: &[TypedMemoryFieldSpec] = &[
    TypedMemoryFieldSpec {
        name: "trigger",
        shape: TypedMemoryFieldShape::Text,
        indexed: false,
    },
    TypedMemoryFieldSpec {
        name: "blast_radius",
        shape: TypedMemoryFieldShape::Text,
        indexed: false,
    },
    TypedMemoryFieldSpec {
        name: "safer_alternative",
        shape: TypedMemoryFieldShape::Text,
        indexed: false,
    },
];

const RULE_TYPED_MEMORY_FIELDS: &[TypedMemoryFieldSpec] = &[
    TypedMemoryFieldSpec {
        name: "condition",
        shape: TypedMemoryFieldShape::Text,
        indexed: true,
    },
    TypedMemoryFieldSpec {
        name: "action",
        shape: TypedMemoryFieldShape::Text,
        indexed: false,
    },
    TypedMemoryFieldSpec {
        name: "exceptions",
        shape: TypedMemoryFieldShape::TextList,
        indexed: false,
    },
];

const CONVENTION_TYPED_MEMORY_FIELDS: &[TypedMemoryFieldSpec] = &[
    TypedMemoryFieldSpec {
        name: "scope",
        shape: TypedMemoryFieldShape::Text,
        indexed: true,
    },
    TypedMemoryFieldSpec {
        name: "pattern",
        shape: TypedMemoryFieldShape::Text,
        indexed: false,
    },
];

fn typed_memory_field_specs(kind: &MemoryKind) -> Option<&'static [TypedMemoryFieldSpec]> {
    match kind {
        MemoryKind::Rule => Some(RULE_TYPED_MEMORY_FIELDS),
        MemoryKind::Failure => Some(FAILURE_TYPED_MEMORY_FIELDS),
        MemoryKind::Decision => Some(DECISION_TYPED_MEMORY_FIELDS),
        MemoryKind::Command => Some(COMMAND_TYPED_MEMORY_FIELDS),
        MemoryKind::Convention => Some(CONVENTION_TYPED_MEMORY_FIELDS),
        MemoryKind::Risk | MemoryKind::AntiPattern => Some(RISK_TYPED_MEMORY_FIELDS),
        MemoryKind::Fact | MemoryKind::PlaybookStep | MemoryKind::Custom(_) => None,
    }
}

fn typed_memory_field_spec(
    specs: &[TypedMemoryFieldSpec],
    field: &str,
) -> Option<TypedMemoryFieldSpec> {
    specs.iter().copied().find(|spec| spec.name == field)
}

fn typed_memory_valid_field_names(specs: &[TypedMemoryFieldSpec]) -> Vec<String> {
    specs.iter().map(|spec| spec.name.to_owned()).collect()
}

/// Return the canonical registry field names accepted for a memory kind.
///
/// Kinds without a v2 typed sidecar return an empty list. This is the same
/// vocabulary published by `ee.memory.typed_fields.v2` and used in structured
/// validation errors.
#[must_use]
pub fn typed_memory_field_names(kind: &MemoryKind) -> Vec<String> {
    typed_memory_field_specs(kind)
        .map(typed_memory_valid_field_names)
        .unwrap_or_default()
}

/// Normalize a typed-memory field name exactly as `ee search --field` does.
///
/// CLI callers may use kebab-case for convenience; persisted sidecars always
/// use lowercase snake_case registry names.
pub fn normalize_typed_memory_field_name(raw: &str) -> Result<String, String> {
    let field = raw.trim().replace('-', "_");
    if field.is_empty() {
        return Err("typed memory field name must not be empty".to_owned());
    }
    if field
        .bytes()
        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
    {
        Ok(field)
    } else {
        Err(format!(
            "typed memory field name `{}` must be lowercase snake_case",
            raw.trim()
        ))
    }
}

/// Convert repeatable `NAME=VALUE` assignments into one canonical v2 sidecar.
///
/// Registry text-list fields accumulate repeated assignments in argument
/// order. Scalar fields must be assigned at most once. Values may contain
/// additional `=` characters; `~` and `^` are search-only operators.
pub fn canonicalize_typed_memory_field_assignments_json_with_redactor<F>(
    kind: &MemoryKind,
    assignments: &[String],
    redact: F,
) -> Result<Option<String>, MemoryValidationError>
where
    F: FnMut(&str) -> String,
{
    if assignments.is_empty() {
        return Ok(None);
    }

    let specs = typed_memory_field_specs(kind).ok_or_else(|| {
        MemoryValidationError::TypedFieldsUnsupportedKind {
            kind: kind.as_str().to_owned(),
        }
    })?;
    let mut fields = BTreeMap::<String, JsonValue>::new();
    for assignment in assignments {
        let Some((raw_field, raw_value)) = assignment.split_once('=') else {
            return Err(MemoryValidationError::InvalidTypedFieldsJson {
                message: format!(
                    "typed field assignment `{assignment}` must use NAME=VALUE; `~` and `^` are search-only operators"
                ),
            });
        };
        let field = normalize_typed_memory_field_name(raw_field).map_err(|reason| {
            MemoryValidationError::TypedFieldInvalid {
                field: raw_field.trim().to_owned(),
                reason,
            }
        })?;
        let value = raw_value.trim();
        if value.is_empty() {
            return Err(MemoryValidationError::TypedFieldInvalid {
                field,
                reason: "assignment value must not be empty".to_owned(),
            });
        }
        let Some(spec) = typed_memory_field_spec(specs, &field) else {
            return Err(MemoryValidationError::TypedFieldNotAllowed {
                kind: kind.as_str().to_owned(),
                field,
                valid_fields: typed_memory_valid_field_names(specs),
            });
        };
        match spec.shape {
            TypedMemoryFieldShape::TextList => {
                let values = fields
                    .entry(field)
                    .or_insert_with(|| JsonValue::Array(Vec::new()));
                let values = values.as_array_mut().ok_or_else(|| {
                    MemoryValidationError::InvalidTypedFieldsJson {
                        message: "typed list assignment accumulator was not an array".to_owned(),
                    }
                })?;
                values.push(JsonValue::String(value.to_owned()));
            }
            TypedMemoryFieldShape::Text | TypedMemoryFieldShape::Rfc3339 => {
                if fields
                    .insert(field.clone(), JsonValue::String(value.to_owned()))
                    .is_some()
                {
                    return Err(MemoryValidationError::TypedFieldInvalid {
                        field,
                        reason: "scalar field was assigned more than once".to_owned(),
                    });
                }
            }
        }
    }

    let raw_json = serde_json::to_string(&fields).map_err(|error| {
        MemoryValidationError::InvalidTypedFieldsJson {
            message: error.to_string(),
        }
    })?;
    canonicalize_typed_memory_fields_json_with_redactor(kind, &raw_json, redact).map(Some)
}

/// Merge body-extracted and explicitly assigned sidecars.
///
/// Explicit assignments win when the same registry field was also extracted
/// from the body. Both inputs must already be canonical and redacted.
pub fn merge_typed_memory_fields_json(
    kind: &MemoryKind,
    extracted_json: Option<&str>,
    explicit_json: Option<&str>,
) -> Result<Option<String>, MemoryValidationError> {
    let mut fields = match extracted_json {
        Some(raw) => typed_memory_fields_from_json(kind, raw)?,
        None => BTreeMap::new(),
    };
    if let Some(raw) = explicit_json {
        fields.extend(typed_memory_fields_from_json(kind, raw)?);
    }
    if fields.is_empty() {
        return Ok(None);
    }
    let raw_json = serde_json::to_string(&fields).map_err(|error| {
        MemoryValidationError::InvalidTypedFieldsJson {
            message: error.to_string(),
        }
    })?;
    canonicalize_typed_memory_fields_json(kind, &raw_json).map(Some)
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
                valid_fields: typed_memory_valid_field_names(specs),
            });
        };
        match spec.shape {
            TypedMemoryFieldShape::Text | TypedMemoryFieldShape::Rfc3339 => {
                let text =
                    value
                        .as_str()
                        .ok_or_else(|| MemoryValidationError::TypedFieldWrongType {
                            field: field.to_owned(),
                            expected: spec.shape.expected_type(),
                        })?;
                let text = redact(text).trim().to_owned();
                if text.is_empty() {
                    continue;
                }
                validate_typed_memory_field_value_len(field, &text)?;
                if spec.shape == TypedMemoryFieldShape::Rfc3339 {
                    validate_typed_memory_field_rfc3339(field, &text)?;
                }
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
        JsonValue::String(TYPED_MEMORY_FIELDS_SCHEMA_V2.to_owned()),
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
            if schema != TYPED_MEMORY_FIELDS_SCHEMA_V1 && schema != TYPED_MEMORY_FIELDS_SCHEMA_V2 {
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

impl TypedMemoryFieldShape {
    const fn expected_type(self) -> &'static str {
        match self {
            Self::Text => "string",
            Self::TextList => "array of strings",
            Self::Rfc3339 => "RFC 3339 timestamp string",
        }
    }
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

fn validate_typed_memory_field_rfc3339(
    field: &str,
    value: &str,
) -> Result<(), MemoryValidationError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|error| MemoryValidationError::TypedFieldInvalid {
            field: field.to_owned(),
            reason: format!("expected RFC 3339 timestamp ({error})"),
        })
}

/// Return a validated, canonical field map from raw or enveloped typed-field JSON.
pub fn typed_memory_fields_from_json(
    kind: &MemoryKind,
    raw_json: &str,
) -> Result<BTreeMap<String, JsonValue>, MemoryValidationError> {
    let canonical = canonicalize_typed_memory_fields_json(kind, raw_json)?;
    let parsed: JsonValue = serde_json::from_str(&canonical).map_err(|error| {
        MemoryValidationError::InvalidTypedFieldsJson {
            message: error.to_string(),
        }
    })?;
    let fields = typed_memory_fields_object(kind, &parsed)?;
    Ok(fields
        .iter()
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect())
}

/// Return the registry-indexed typed fields as canonical document metadata.
pub fn typed_memory_index_metadata_from_json(
    kind: &MemoryKind,
    raw_json: &str,
) -> Result<BTreeMap<String, String>, MemoryValidationError> {
    let fields = typed_memory_fields_from_json(kind, raw_json)?;
    let specs = typed_memory_field_specs(kind).ok_or_else(|| {
        MemoryValidationError::TypedFieldsUnsupportedKind {
            kind: kind.as_str().to_owned(),
        }
    })?;
    let mut metadata = BTreeMap::new();
    for (field, value) in fields {
        let Some(spec) = typed_memory_field_spec(specs, &field) else {
            continue;
        };
        if !spec.indexed {
            continue;
        }
        let Some(value) = typed_memory_index_metadata_value(&value) else {
            continue;
        };
        metadata.insert(
            format!("{TYPED_MEMORY_FIELD_METADATA_PREFIX}{field}"),
            value,
        );
    }
    Ok(metadata)
}

fn typed_memory_index_metadata_value(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(value) => Some(value.clone()),
        JsonValue::Array(values) => {
            let joined = values
                .iter()
                .filter_map(JsonValue::as_str)
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
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
        MemoryKind::Rule => extract_rule_typed_memory_fields(content),
        MemoryKind::Convention => extract_convention_typed_memory_fields(content),
        MemoryKind::Risk | MemoryKind::AntiPattern => extract_risk_typed_memory_fields(content),
        MemoryKind::Fact | MemoryKind::PlaybookStep | MemoryKind::Custom(_) => return Ok(None),
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
    insert_text_field(
        &mut fields,
        "revisit_by",
        extract_labeled_value(content, &["revisit by:", "revisit_by:", "revisit-by:"]),
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

fn extract_rule_typed_memory_fields(content: &str) -> BTreeMap<String, JsonValue> {
    let mut fields = BTreeMap::new();
    insert_text_field(
        &mut fields,
        "condition",
        extract_labeled_value(content, &["condition:", "condition=", "when:", "if:"]),
    );
    insert_text_field(
        &mut fields,
        "action",
        extract_labeled_value(content, &["action:", "action=", "then:", "do:"]),
    );
    if let Some(exceptions) =
        extract_labeled_value_allowing_commas(content, &["exceptions:", "except:"])
    {
        let exceptions = split_text_list(&exceptions);
        if !exceptions.is_empty() {
            fields.insert(
                "exceptions".to_owned(),
                JsonValue::Array(exceptions.into_iter().map(JsonValue::String).collect()),
            );
        }
    }
    fields
}

fn extract_convention_typed_memory_fields(content: &str) -> BTreeMap<String, JsonValue> {
    let mut fields = BTreeMap::new();
    insert_text_field(
        &mut fields,
        "scope",
        extract_labeled_value(content, &["scope:", "scope=", "applies to:", "where:"]),
    );
    insert_text_field(
        &mut fields,
        "pattern",
        extract_labeled_value(content, &["pattern:", "pattern=", "convention:", "style:"]),
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
        valid_fields: Vec<String>,
    },
    TypedFieldWrongType {
        field: String,
        expected: &'static str,
    },
    TypedFieldInvalid {
        field: String,
        reason: String,
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
            Self::TypedFieldNotAllowed {
                kind,
                field,
                valid_fields,
            } => write!(
                formatter,
                "typed memory field `{field}` is not allowed for kind `{kind}`; valid fields: {}",
                valid_fields.join(", ")
            ),
            Self::TypedFieldWrongType { field, expected } => {
                write!(formatter, "typed memory field `{field}` must be {expected}")
            }
            Self::TypedFieldInvalid { field, reason } => {
                write!(
                    formatter,
                    "typed memory field `{field}` is invalid: {reason}"
                )
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

    use proptest::prelude::*;
    use proptest::test_runner::{Config as ProptestConfig, TestCaseError};

    use super::{
        Confidence, KNOWN_MEMORY_KINDS, MAX_CONTENT_BYTES, MAX_TAG_BYTES,
        MAX_TYPED_MEMORY_FIELD_LIST_ITEMS, MAX_TYPED_MEMORY_FIELD_VALUE_BYTES,
        MAX_TYPED_MEMORY_FIELDS, MemoryContent, MemoryKind, MemoryLevel, MemoryValidationError,
        TYPED_MEMORY_FIELDS_SCHEMA_V2, Tag, UnitScore,
        canonicalize_typed_memory_field_assignments_json_with_redactor,
        canonicalize_typed_memory_fields_json, canonicalize_typed_memory_fields_json_with_redactor,
        extract_typed_memory_fields_json_with_redactor, merge_typed_memory_fields_json,
        typed_memory_fields_from_json, typed_memory_index_metadata_from_json,
    };

    fn decision_field_text_strategy() -> impl Strategy<Value = String> {
        let ascii_chunk = proptest::string::string_regex("[A-Za-z0-9][A-Za-z0-9_-]{0,7}")
            .expect("test regex is valid");
        let atom = prop_oneof![
            ascii_chunk,
            prop::sample::select(vec![
                "=", "~", "^", "/", ".", ":", " ", "delta", "tokyo", "cafe", "RCH", "remote",
                "local", "cargo", "東京", "δ", "é"
            ])
            .prop_map(str::to_owned),
        ];
        prop::collection::vec(atom, 1..16).prop_filter_map(
            "non-empty, bounded text value without trimming loss",
            |parts| {
                let value = parts.concat();
                if value.trim() != value || value.is_empty() {
                    return None;
                }
                if value.len() > MAX_TYPED_MEMORY_FIELD_VALUE_BYTES {
                    return None;
                }
                Some(value)
            },
        )
    }

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
        assert_eq!(parsed["schema"], TYPED_MEMORY_FIELDS_SCHEMA_V2);
        assert_eq!(parsed["kind"], "failure");
        assert_eq!(parsed["fields"]["cause"], "stale cache");
        assert_eq!(parsed["fields"]["family"], "aggressive-prefetch");
        assert!(parsed["fields"].get("regression_surface").is_none());
    }

    #[test]
    fn typed_memory_fields_accept_v1_sidecars_and_emit_v2() {
        let canonical = canonicalize_typed_memory_fields_json(
            &MemoryKind::Decision,
            r#"{"schema":"ee.memory.typed_fields.v1","kind":"decision","fields":{"options":["local","remote"],"chosen":"remote","supersedes":"mem_old"}}"#,
        )
        .expect("v1 sidecar canonicalizes through v2 registry");
        let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");

        assert_eq!(parsed["schema"], TYPED_MEMORY_FIELDS_SCHEMA_V2);
        assert_eq!(parsed["kind"], "decision");
        assert_eq!(parsed["fields"]["options"][0], "local");
        assert_eq!(parsed["fields"]["chosen"], "remote");
        assert_eq!(parsed["fields"]["supersedes"], "mem_old");
    }

    #[test]
    fn typed_memory_fields_validate_decision_revisit_by_rfc3339() {
        let canonical = canonicalize_typed_memory_fields_json(
            &MemoryKind::Decision,
            r#"{"chosen":"RCH remote","revisit_by":"2026-07-01T12:00:00Z"}"#,
        )
        .expect("decision revisit timestamp canonicalizes");
        let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");
        assert_eq!(parsed["fields"]["revisit_by"], "2026-07-01T12:00:00Z");

        let err = canonicalize_typed_memory_fields_json(
            &MemoryKind::Decision,
            r#"{"chosen":"RCH remote","revisit_by":"next Tuesday"}"#,
        )
        .expect_err("natural language revisit timestamp is invalid");
        assert!(matches!(
            err,
            MemoryValidationError::TypedFieldInvalid { field, .. } if field == "revisit_by"
        ));
    }

    #[test]
    fn typed_memory_fields_canonicalize_rule_and_convention_fields() {
        let rule = canonicalize_typed_memory_fields_json(
            &MemoryKind::Rule,
            r#"{"condition":"release prep","action":"run remote proof","exceptions":["docs only","read-only review"]}"#,
        )
        .expect("rule fields canonicalize");
        let rule: serde_json::Value = serde_json::from_str(&rule).expect("rule JSON");
        assert_eq!(rule["schema"], TYPED_MEMORY_FIELDS_SCHEMA_V2);
        assert_eq!(rule["kind"], "rule");
        assert_eq!(rule["fields"]["condition"], "release prep");
        assert_eq!(rule["fields"]["action"], "run remote proof");
        assert_eq!(rule["fields"]["exceptions"][1], "read-only review");

        let convention = canonicalize_typed_memory_fields_json(
            &MemoryKind::Convention,
            r#"{"scope":"Rust CLI tests","pattern":"inline module tests near implementation"}"#,
        )
        .expect("convention fields canonicalize");
        let convention: serde_json::Value =
            serde_json::from_str(&convention).expect("convention JSON");
        assert_eq!(convention["kind"], "convention");
        assert_eq!(convention["fields"]["scope"], "Rust CLI tests");
        assert_eq!(
            convention["fields"]["pattern"],
            "inline module tests near implementation"
        );
    }

    #[test]
    fn typed_memory_fields_reject_unknown_field_with_valid_field_list() {
        let err = canonicalize_typed_memory_fields_json(
            &MemoryKind::Rule,
            r#"{"family":"wrong vocabulary"}"#,
        )
        .expect_err("unknown rule field rejected");
        match err {
            MemoryValidationError::TypedFieldNotAllowed {
                field,
                valid_fields,
                ..
            } => {
                assert_eq!(field, "family");
                assert_eq!(
                    valid_fields,
                    vec![
                        "condition".to_owned(),
                        "action".to_owned(),
                        "exceptions".to_owned()
                    ]
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn typed_memory_fields_enforce_v2_field_count_bound() {
        let canonical = canonicalize_typed_memory_fields_json(
            &MemoryKind::Decision,
            r#"{"options":["local","remote"],"chosen":"remote","rationale":"keeps SSD cold","supersedes":"mem_old","revisit_by":"2026-07-01T12:00:00Z"}"#,
        )
        .expect("decision with five fields fits v2 bound");
        let parsed: serde_json::Value = serde_json::from_str(&canonical).expect("canonical JSON");
        assert_eq!(
            parsed["fields"].as_object().expect("fields object").len(),
            5
        );
        assert_eq!(MAX_TYPED_MEMORY_FIELDS, 8);
    }

    #[test]
    fn typed_memory_fields_index_metadata_uses_registry_flags() {
        let metadata = typed_memory_index_metadata_from_json(
            &MemoryKind::Decision,
            r#"{"chosen":"RCH remote","rationale":"avoid local cargo","supersedes":"mem_old","revisit_by":"2026-07-01T12:00:00Z"}"#,
        )
        .expect("index metadata extracts");

        assert_eq!(
            metadata.get("typed_field.chosen"),
            Some(&"RCH remote".to_owned())
        );
        assert_eq!(
            metadata.get("typed_field.supersedes"),
            Some(&"mem_old".to_owned())
        );
        assert!(!metadata.contains_key("typed_field.rationale"));
        assert!(!metadata.contains_key("typed_field.revisit_by"));
    }

    #[test]
    fn typed_memory_fields_preserve_operator_and_max_byte_values() {
        let max_value = "x".repeat(MAX_TYPED_MEMORY_FIELD_VALUE_BYTES);
        let canonical = canonicalize_typed_memory_fields_json(
            &MemoryKind::Decision,
            &serde_json::json!({
                "chosen": "RCH=remote/worker~hz2^prefix|safe",
                "rationale": max_value,
                "supersedes": "mem_01234567890123456789012345",
                "options": ["local Cargo", "RCH=remote/worker~hz2^prefix|safe"]
            })
            .to_string(),
        )
        .expect("operator and max-byte decision fields canonicalize");
        let fields = typed_memory_fields_from_json(&MemoryKind::Decision, &canonical)
            .expect("canonical sidecar parses");

        assert_eq!(
            fields.get("chosen").and_then(serde_json::Value::as_str),
            Some("RCH=remote/worker~hz2^prefix|safe")
        );
        assert_eq!(
            fields.get("rationale").and_then(serde_json::Value::as_str),
            Some(max_value.as_str())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        #[test]
        fn typed_memory_fields_property_decision_values_round_trip_byte_identically(
            chosen in decision_field_text_strategy(),
            rationale in decision_field_text_strategy(),
            supersedes in decision_field_text_strategy(),
            options in prop::collection::vec(
                decision_field_text_strategy(),
                1..=MAX_TYPED_MEMORY_FIELD_LIST_ITEMS,
            ),
        ) {
            let raw = serde_json::json!({
                "chosen": chosen,
                "rationale": rationale,
                "supersedes": supersedes,
                "options": options,
            });
            let canonical = canonicalize_typed_memory_fields_json(
                &MemoryKind::Decision,
                &raw.to_string(),
            )
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
            let fields = typed_memory_fields_from_json(&MemoryKind::Decision, &canonical)
                .map_err(|error| TestCaseError::fail(error.to_string()))?;

            prop_assert_eq!(
                fields.get("chosen").and_then(serde_json::Value::as_str),
                raw["chosen"].as_str()
            );
            prop_assert_eq!(
                fields.get("rationale").and_then(serde_json::Value::as_str),
                raw["rationale"].as_str()
            );
            prop_assert_eq!(
                fields.get("supersedes").and_then(serde_json::Value::as_str),
                raw["supersedes"].as_str()
            );

            let actual_options = fields
                .get("options")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| TestCaseError::fail("options must round-trip as an array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| TestCaseError::fail("option must round-trip as text"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let expected_options = raw["options"]
                .as_array()
                .ok_or_else(|| TestCaseError::fail("raw options must be an array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| TestCaseError::fail("raw option must be text"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            prop_assert_eq!(actual_options, expected_options);

            let recanonicalized = canonicalize_typed_memory_fields_json(
                &MemoryKind::Decision,
                &canonical,
            )
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(recanonicalized, canonical);
        }
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
    fn typed_memory_field_assignments_normalize_and_accumulate_lists() {
        let assignments = vec![
            "chosen=RCH remote".to_owned(),
            "options=local Cargo".to_owned(),
            "options=RCH remote=verified".to_owned(),
            "revisit-by=2026-09-15T00:00:00Z".to_owned(),
        ];
        let canonical = canonicalize_typed_memory_field_assignments_json_with_redactor(
            &MemoryKind::Decision,
            &assignments,
            str::to_owned,
        )
        .expect("explicit decision fields validate")
        .expect("explicit fields produce a sidecar");
        let parsed: serde_json::Value =
            serde_json::from_str(&canonical).expect("canonical assignment JSON");

        assert_eq!(parsed["fields"]["chosen"], "RCH remote");
        assert_eq!(parsed["fields"]["options"][0], "local Cargo");
        assert_eq!(parsed["fields"]["options"][1], "RCH remote=verified");
        assert_eq!(parsed["fields"]["revisit_by"], "2026-09-15T00:00:00Z");
    }

    #[test]
    fn typed_memory_field_assignments_reject_ambiguous_scalars_and_search_operators() {
        let duplicate = canonicalize_typed_memory_field_assignments_json_with_redactor(
            &MemoryKind::Failure,
            &["family=one".to_owned(), "family=two".to_owned()],
            str::to_owned,
        )
        .expect_err("duplicate scalar assignment is ambiguous");
        assert!(matches!(
            duplicate,
            MemoryValidationError::TypedFieldInvalid { field, .. } if field == "family"
        ));

        let search_operator = canonicalize_typed_memory_field_assignments_json_with_redactor(
            &MemoryKind::Failure,
            &["family~prefetch".to_owned()],
            str::to_owned,
        )
        .expect_err("search operator is not a write assignment");
        assert!(matches!(
            search_operator,
            MemoryValidationError::InvalidTypedFieldsJson { .. }
        ));
    }

    #[test]
    fn explicit_typed_fields_override_body_extraction() {
        let extracted = extract_typed_memory_fields_json_with_redactor(
            &MemoryKind::Failure,
            "Family: extracted family. Cause: extracted cause.",
            str::to_owned,
        )
        .expect("body fields extract")
        .expect("body has fields");
        let explicit = canonicalize_typed_memory_field_assignments_json_with_redactor(
            &MemoryKind::Failure,
            &["family=explicit family".to_owned()],
            str::to_owned,
        )
        .expect("explicit field validates")
        .expect("explicit field exists");
        let merged =
            merge_typed_memory_fields_json(&MemoryKind::Failure, Some(&extracted), Some(&explicit))
                .expect("sidecars merge")
                .expect("merged sidecar exists");
        let parsed: serde_json::Value = serde_json::from_str(&merged).expect("merged sidecar JSON");

        assert_eq!(parsed["fields"]["family"], "explicit family");
        assert_eq!(parsed["fields"]["cause"], "extracted cause");
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

        assert_eq!(parsed["schema"], TYPED_MEMORY_FIELDS_SCHEMA_V2);
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
            "Options: local cache, RCH remote or no-op. Chosen: RCH remote. Rationale: avoids local Cargo. Supersedes: bd-old. Revisit by: 2026-07-01T12:00:00Z.",
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
        assert_eq!(parsed["fields"]["revisit_by"], "2026-07-01T12:00:00Z");
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

        assert_eq!(parsed["schema"], TYPED_MEMORY_FIELDS_SCHEMA_V2);
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
    fn typed_memory_fields_extract_rule_and_convention_patterns_from_body() {
        let rule = extract_typed_memory_fields_json_with_redactor(
            &MemoryKind::Rule,
            "Condition: release prep. Action: run scripts/rch_verify.sh. Exceptions: docs only, read-only review.",
            str::to_owned,
        )
        .expect("rule body extracts")
        .expect("rule body has typed fields");
        let rule: serde_json::Value = serde_json::from_str(&rule).expect("rule JSON");
        assert_eq!(rule["fields"]["condition"], "release prep");
        assert_eq!(rule["fields"]["action"], "run scripts/rch_verify");
        assert_eq!(rule["fields"]["exceptions"][0], "docs only");
        assert_eq!(rule["fields"]["exceptions"][1], "read-only review");

        let convention = extract_typed_memory_fields_json_with_redactor(
            &MemoryKind::Convention,
            "Scope: Rust CLI tests. Pattern: keep typed-field tests next to registry code.",
            str::to_owned,
        )
        .expect("convention body extracts")
        .expect("convention body has typed fields");
        let convention: serde_json::Value =
            serde_json::from_str(&convention).expect("convention JSON");
        assert_eq!(convention["fields"]["scope"], "Rust CLI tests");
        assert_eq!(
            convention["fields"]["pattern"],
            "keep typed-field tests next to registry code"
        );
    }

    #[test]
    fn typed_memory_fields_do_not_fabricate_from_bare_bodies() {
        for kind in [
            MemoryKind::Failure,
            MemoryKind::Decision,
            MemoryKind::Command,
            MemoryKind::Rule,
            MemoryKind::Convention,
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
