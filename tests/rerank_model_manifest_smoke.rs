//! Smoke coverage for the bundled rerank model manifest (bd-17c65.14.8).

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn is_hex_hash_64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[test]
fn rerank_model_manifest_has_stable_fetch_contract() -> TestResult {
    let manifest: Value =
        serde_json::from_str(include_str!("../src/data/rerank_model_manifest.json"))
            .map_err(|error| format!("parse manifest: {error}"))?;

    ensure(
        manifest.get("schema").and_then(Value::as_str) == Some("ee.model_manifest.v1"),
        "manifest schema",
    )?;
    ensure(
        manifest
            .get("model_id")
            .and_then(Value::as_str)
            .is_some_and(|model_id| model_id.starts_with("rerank-default-v")),
        "model_id should be a versioned rerank-default ID",
    )?;
    ensure(
        manifest
            .get("hash_blake3")
            .and_then(Value::as_str)
            .is_some_and(is_hex_hash_64),
        "hash_blake3 should be 64 hex characters",
    )?;
    ensure(
        manifest
            .get("hash_sha256")
            .and_then(Value::as_str)
            .is_some_and(is_hex_hash_64),
        "hash_sha256 should be 64 hex characters",
    )?;
    ensure(
        manifest
            .get("content_length_bytes")
            .and_then(Value::as_u64)
            .is_some_and(|length| length > 0),
        "content_length_bytes should be positive",
    )?;
    ensure(
        manifest
            .get("source_uri")
            .and_then(Value::as_str)
            .is_some_and(|source| source.starts_with("https://")),
        "source_uri should be HTTPS",
    )?;
    let fallback_sources = manifest
        .get("fallback_source_uris")
        .and_then(Value::as_array)
        .ok_or_else(|| "fallback_source_uris should be an array".to_string())?;
    ensure(
        !fallback_sources.is_empty()
            && fallback_sources.iter().all(|source| {
                source
                    .as_str()
                    .is_some_and(|uri| uri.starts_with("https://"))
            }),
        "fallback_source_uris should be non-empty HTTPS URIs",
    )?;
    ensure(
        manifest
            .pointer("/inference_dimensions/input_max_tokens")
            .and_then(Value::as_u64)
            .is_some_and(|tokens| tokens > 0)
            && manifest
                .pointer("/inference_dimensions/output_dimension")
                .and_then(Value::as_u64)
                .is_some_and(|dimension| dimension > 0),
        "inference_dimensions should be positive",
    )?;
    ensure(
        manifest
            .pointer("/signed_attestation/sigstore_bundle")
            .and_then(Value::as_str)
            == Some("src/data/rerank_model_manifest.sigstore.json"),
        "signed_attestation should point at the committed sigstore bundle path",
    )
}
