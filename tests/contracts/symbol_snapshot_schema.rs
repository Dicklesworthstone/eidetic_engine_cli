//! Contract checks for the read-only symbol snapshot schema.

use std::fs;
use std::path::PathBuf;

use ee::models::{
    KNOWN_SCHEMAS, SYMBOL_SNAPSHOT_SCHEMA_V1, SymbolGraphDegradationCode, SymbolKind,
};
use serde::Serialize;
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.symbol_snapshot.v1.json";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(relative: &str) -> Result<Value, String> {
    let path = repo_root().join(relative);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn ensure(condition: bool, context: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.into())
    }
}

fn ensure_json_str(schema: &Value, pointer: &str, expected: &str) -> TestResult {
    let actual = schema.pointer(pointer).and_then(Value::as_str);
    ensure(
        actual == Some(expected),
        format!("{pointer}: expected {expected:?}, got {actual:?}"),
    )
}

fn ensure_json_bool(schema: &Value, pointer: &str, expected: bool) -> TestResult {
    let actual = schema.pointer(pointer).and_then(Value::as_bool);
    ensure(
        actual == Some(expected),
        format!("{pointer}: expected {expected:?}, got {actual:?}"),
    )
}

fn schema_enum<'a>(schema: &'a Value, pointer: &str) -> Result<Vec<&'a str>, String> {
    schema
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{pointer}: missing enum array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| format!("{pointer}: enum value {value:?} is not a string"))
        })
        .collect()
}

fn serialized_enum_value<T>(value: T) -> Result<String, String>
where
    T: Serialize,
{
    serde_json::to_value(value)
        .map_err(|error| format!("serialize enum value: {error}"))?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| "serialized enum value was not a string".to_owned())
}

fn ensure_schema_registered() -> TestResult {
    ensure(
        KNOWN_SCHEMAS.contains(&SYMBOL_SNAPSHOT_SCHEMA_V1),
        "KNOWN_SCHEMAS missing ee.symbol_snapshot.v1",
    )?;

    let supported = ee::core::supported_schemas()
        .into_iter()
        .map(|schema| (schema.name, schema.schema))
        .collect::<Vec<_>>();
    ensure(
        supported.iter().any(|(name, schema)| {
            *name == "symbol_snapshot" && *schema == SYMBOL_SNAPSHOT_SCHEMA_V1
        }),
        "supported_schemas missing symbol_snapshot=ee.symbol_snapshot.v1",
    )
}

#[test]
fn symbol_snapshot_schema_is_documented_and_registered() -> TestResult {
    let schema = read_json(SCHEMA_PATH)?;

    ensure_json_str(
        &schema,
        "/$schema",
        "https://json-schema.org/draft/2020-12/schema",
    )?;
    ensure_json_str(
        &schema,
        "/$id",
        "https://eidetic-engine/schemas/ee.symbol_snapshot.v1.json",
    )?;
    ensure_json_str(&schema, "/title", SYMBOL_SNAPSHOT_SCHEMA_V1)?;
    ensure_json_bool(&schema, "/additionalProperties", false)?;
    ensure_json_str(
        &schema,
        "/properties/schema/const",
        SYMBOL_SNAPSHOT_SCHEMA_V1,
    )?;
    ensure_json_str(
        &schema,
        "/$defs/symbol/properties/id/pattern",
        "^sym_v1_[0-9a-f]{24}$",
    )?;
    ensure_json_str(&schema, "/$defs/blake3Hex/pattern", "^[0-9a-f]{64}$")?;

    let full_preset = schema
        .pointer("/field_presets/full")
        .and_then(Value::as_array)
        .ok_or_else(|| "field_presets.full missing".to_owned())?;
    ensure(
        full_preset.iter().any(|value| value.as_str() == Some("*")),
        "field_presets.full must include wildcard preset",
    )?;

    ensure_schema_registered()
}

#[test]
fn symbol_snapshot_schema_matches_model_enum_vocabulary() -> TestResult {
    let schema = read_json(SCHEMA_PATH)?;

    let symbol_kinds = schema_enum(&schema, "/$defs/symbol/properties/kind/enum")?;
    for kind in [
        SymbolKind::Module,
        SymbolKind::Function,
        SymbolKind::Method,
        SymbolKind::Struct,
        SymbolKind::Enum,
        SymbolKind::Trait,
        SymbolKind::Impl,
        SymbolKind::MacroInvocation,
        SymbolKind::JsonSchemaConstant,
        SymbolKind::CliCommandHandler,
    ] {
        ensure(
            symbol_kinds.contains(&kind.as_str()),
            format!("symbol kind enum missing {}", kind.as_str()),
        )?;
    }
    ensure(
        symbol_kinds.len() == 10,
        format!("symbol kind enum should have 10 values, got {symbol_kinds:?}"),
    )?;

    let degradation_codes = schema_enum(&schema, "/$defs/degradation/properties/code/enum")?;
    for code in [
        SymbolGraphDegradationCode::SourceMissing,
        SymbolGraphDegradationCode::SourceNonRegular,
        SymbolGraphDegradationCode::SourceTooLarge,
        SymbolGraphDegradationCode::SourceUnreadable,
        SymbolGraphDegradationCode::SourceUnparsable,
    ] {
        let value = serialized_enum_value(code)?;
        ensure(
            degradation_codes.contains(&value.as_str()),
            format!("degradation code enum missing {value}"),
        )?;
    }
    ensure(
        degradation_codes.len() == 5,
        format!("degradation code enum should have 5 values, got {degradation_codes:?}"),
    )
}

#[test]
fn symbol_snapshot_schema_stays_redaction_safe() -> TestResult {
    let schema = read_json(SCHEMA_PATH)?;
    let serialized =
        serde_json::to_string(&schema).map_err(|error| format!("serialize schema: {error}"))?;

    for forbidden in [
        "sourceBody",
        "sourceText",
        "rawSource",
        "sourceBytes",
        "rawBody",
        "bodyText",
        "contentText",
        "secretValue",
        "tokenValue",
        "apiKey",
    ] {
        ensure(
            !serialized.contains(forbidden),
            format!("symbol snapshot schema exposed forbidden raw-source field {forbidden:?}"),
        )?;
    }

    ensure_json_bool(&schema, "/$defs/symbol/additionalProperties", false)?;
    ensure_json_bool(&schema, "/$defs/sourceFile/additionalProperties", false)?;
    ensure_json_bool(&schema, "/$defs/degradation/additionalProperties", false)
}
