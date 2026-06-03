//! Contract coverage for `ee.pack.compression_manifest.v1`.
//!
//! The manifest is the boundary that lets pack compression stay a transport
//! optimization: canonical pack/replay hashes continue to refer to
//! uncompressed content, and readers can fall back without changing selected
//! pack items. These tests pin the schema fields that carry those invariants.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use ee::cache::pack_compression::{
    PACK_COMPRESSION_MANIFEST_SCHEMA_V1, PACK_COMPRESSION_TRAINING_ALGORITHM_V1,
};
use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.pack.compression_manifest.v1.json";
const SCHEMA_ID: &str = "https://eidetic-engine/schemas/ee.pack.compression_manifest.v1.json";

const REQUIRED_TOP_LEVEL: &[&str] = &[
    "schema",
    "manifestId",
    "createdAt",
    "codec",
    "compressionLevel",
    "dictionary",
    "artifact",
    "canonical",
    "corpusWindow",
    "compatibility",
    "storage",
    "fallback",
    "redactionStatus",
];

const REQUIRED_DICTIONARY: &[&str] = &[
    "dictionaryId",
    "dictionaryByteHash",
    "dictionarySourceHash",
    "dictionaryBytes",
    "trainingAlgorithm",
];

const REQUIRED_ARTIFACT: &[&str] = &[
    "kind",
    "uncompressedByteHash",
    "compressedByteHash",
    "uncompressedBytes",
    "compressedBytes",
];

const REQUIRED_CANONICAL: &[&str] = &[
    "hashAlgorithm",
    "byteEncoding",
    "manifestIdentity",
    "packHashMode",
    "ledgerHashMode",
    "binaryContentHashMode",
    "sortOrder",
];

const REQUIRED_SORT_ORDER: &[&str] = &["objectKeys", "arrays"];

const REQUIRED_CORPUS_WINDOW: &[&str] = &[
    "sourceKind",
    "workspaceId",
    "fromGeneration",
    "toGeneration",
    "sampleCount",
    "sampleHash",
];

const REQUIRED_COMPATIBILITY: &[&str] = &[
    "minReaderVersion",
    "transparentFallbackRequired",
    "preservesPackHash",
    "preservesLedgerHash",
    "readerMayIgnoreManifest",
    "writerMustKeepUncompressedRecovery",
];

const REQUIRED_STORAGE: &[&str] = &[
    "authority",
    "sidecarPath",
    "dbReference",
    "publish",
    "rollback",
];

const REQUIRED_DB_REFERENCE: &[&str] = &["mode", "table", "columns"];

const REQUIRED_FALLBACK: &[&str] = &[
    "missingManifest",
    "corruptManifest",
    "missingDictionary",
    "staleDictionary",
    "hashMismatch",
    "unsupportedCodec",
];

const REQUIRED_FALLBACK_CASE: &[&str] = &[
    "degradedCode",
    "severity",
    "behavior",
    "selectedItemsUnaffected",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn load_schema() -> Result<Value, String> {
    let path = repo_root().join(SCHEMA_PATH);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn collect_string_set(node: &Value, ctx: &str) -> Result<BTreeSet<String>, String> {
    let array = node
        .as_array()
        .ok_or_else(|| format!("{ctx}: expected array, got {node}"))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: non-string entry {value}"))
        })
        .collect()
}

fn require_exact_required(
    schema: &Value,
    pointer: &str,
    expected: &[&str],
    label: &str,
) -> TestResult {
    let required = collect_string_set(schema.pointer(pointer).unwrap_or(&Value::Null), label)?;
    let want: BTreeSet<String> = expected.iter().map(|field| (*field).to_owned()).collect();
    ensure(
        required == want,
        format!("{label} drifted from exact required set; expected {want:?}, got {required:?}"),
    )
}

fn require_value_fields(value: &Value, expected: &[&str], label: &str) -> TestResult {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{label}: expected object, got {value}"))?;
    for field in expected {
        ensure(
            object.contains_key(*field),
            format!("{label} missing required example field `{field}`"),
        )?;
    }
    Ok(())
}

fn example<'a>(schema: &'a Value, pointer: &str) -> Result<&'a Value, String> {
    schema
        .pointer(pointer)
        .ok_or_else(|| format!("schema example pointer {pointer} missing"))
}

#[test]
fn pack_compression_manifest_schema_identity_matches_rust_constant() -> TestResult {
    let schema = load_schema()?;
    ensure(
        schema["$id"] == SCHEMA_ID,
        format!("expected $id={SCHEMA_ID}; got {}", schema["$id"]),
    )?;
    ensure(
        schema["title"] == PACK_COMPRESSION_MANIFEST_SCHEMA_V1,
        format!(
            "title must match Rust constant {}; got {}",
            PACK_COMPRESSION_MANIFEST_SCHEMA_V1, schema["title"]
        ),
    )?;
    ensure(
        schema["properties"]["schema"]["const"] == PACK_COMPRESSION_MANIFEST_SCHEMA_V1,
        "properties.schema.const must match the Rust schema constant",
    )?;
    ensure(
        schema["additionalProperties"] == Value::Bool(false),
        "pack compression manifest top-level schema must be closed",
    )?;
    require_exact_required(
        &schema,
        "/required",
        REQUIRED_TOP_LEVEL,
        "manifest.required",
    )
}

#[test]
fn pack_compression_manifest_nested_required_sets_are_exact() -> TestResult {
    let schema = load_schema()?;
    for (pointer, expected, label) in [
        (
            "/properties/dictionary/required",
            REQUIRED_DICTIONARY,
            "dictionary.required",
        ),
        (
            "/properties/artifact/required",
            REQUIRED_ARTIFACT,
            "artifact.required",
        ),
        (
            "/properties/canonical/required",
            REQUIRED_CANONICAL,
            "canonical.required",
        ),
        (
            "/properties/canonical/properties/sortOrder/required",
            REQUIRED_SORT_ORDER,
            "canonical.sortOrder.required",
        ),
        (
            "/properties/corpusWindow/required",
            REQUIRED_CORPUS_WINDOW,
            "corpusWindow.required",
        ),
        (
            "/properties/compatibility/required",
            REQUIRED_COMPATIBILITY,
            "compatibility.required",
        ),
        (
            "/properties/storage/required",
            REQUIRED_STORAGE,
            "storage.required",
        ),
        (
            "/properties/storage/properties/dbReference/required",
            REQUIRED_DB_REFERENCE,
            "storage.dbReference.required",
        ),
        (
            "/properties/fallback/required",
            REQUIRED_FALLBACK,
            "fallback.required",
        ),
        (
            "/$defs/fallbackCase/required",
            REQUIRED_FALLBACK_CASE,
            "fallbackCase.required",
        ),
    ] {
        require_exact_required(&schema, pointer, expected, label)?;
    }
    Ok(())
}

#[test]
fn pack_compression_manifest_example_covers_required_shape() -> TestResult {
    let schema = load_schema()?;
    let root = example(&schema, "/examples/0")?;
    require_value_fields(root, REQUIRED_TOP_LEVEL, "example")?;
    require_value_fields(
        example(&schema, "/examples/0/dictionary")?,
        REQUIRED_DICTIONARY,
        "example.dictionary",
    )?;
    require_value_fields(
        example(&schema, "/examples/0/artifact")?,
        REQUIRED_ARTIFACT,
        "example.artifact",
    )?;
    require_value_fields(
        example(&schema, "/examples/0/canonical")?,
        REQUIRED_CANONICAL,
        "example.canonical",
    )?;
    require_value_fields(
        example(&schema, "/examples/0/canonical/sortOrder")?,
        REQUIRED_SORT_ORDER,
        "example.canonical.sortOrder",
    )?;
    require_value_fields(
        example(&schema, "/examples/0/corpusWindow")?,
        REQUIRED_CORPUS_WINDOW,
        "example.corpusWindow",
    )?;
    require_value_fields(
        example(&schema, "/examples/0/compatibility")?,
        REQUIRED_COMPATIBILITY,
        "example.compatibility",
    )?;
    require_value_fields(
        example(&schema, "/examples/0/storage")?,
        REQUIRED_STORAGE,
        "example.storage",
    )?;
    require_value_fields(
        example(&schema, "/examples/0/storage/dbReference")?,
        REQUIRED_DB_REFERENCE,
        "example.storage.dbReference",
    )?;
    require_value_fields(
        example(&schema, "/examples/0/fallback")?,
        REQUIRED_FALLBACK,
        "example.fallback",
    )?;
    for fallback_case in REQUIRED_FALLBACK {
        require_value_fields(
            example(&schema, &format!("/examples/0/fallback/{fallback_case}"))?,
            REQUIRED_FALLBACK_CASE,
            &format!("example.fallback.{fallback_case}"),
        )?;
    }
    Ok(())
}

#[test]
fn pack_compression_manifest_pins_hash_preserving_fallback_contract() -> TestResult {
    let schema = load_schema()?;
    let example = example(&schema, "/examples/0")?;
    ensure(
        example["dictionary"]["trainingAlgorithm"] == PACK_COMPRESSION_TRAINING_ALGORITHM_V1,
        "example trainingAlgorithm must match the Rust training algorithm constant",
    )?;
    ensure(
        example["canonical"]["packHashMode"] == "uncompressed_pack_content_components",
        "pack hashes must stay tied to uncompressed pack content",
    )?;
    ensure(
        example["canonical"]["ledgerHashMode"] == "uncompressed_replay_ledger_json",
        "replay ledger hashes must stay tied to uncompressed ledger JSON",
    )?;
    ensure(
        example["compatibility"]["transparentFallbackRequired"] == Value::Bool(true),
        "manifest readers must require transparent fallback",
    )?;
    ensure(
        example["compatibility"]["preservesPackHash"] == Value::Bool(true),
        "compression must preserve canonical pack hashes",
    )?;
    ensure(
        example["compatibility"]["preservesLedgerHash"] == Value::Bool(true),
        "compression must preserve replay ledger hashes",
    )?;
    ensure(
        example["compatibility"]["writerMustKeepUncompressedRecovery"] == Value::Bool(true),
        "writers must retain uncompressed recovery material",
    )?;
    ensure(
        example["fallback"]["hashMismatch"]["behavior"] == "reject_compressed_artifact",
        "hash mismatch must reject compressed artifacts rather than mutating pack selection",
    )?;
    ensure(
        example["fallback"]["hashMismatch"]["selectedItemsUnaffected"] == Value::Bool(true),
        "hash mismatch fallback must not change selected pack items",
    )
}
