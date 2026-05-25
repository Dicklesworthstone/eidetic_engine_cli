// bd-2dek9: reflection handshake security and determinism contracts.
//
// Pure pin-tests against ee::curate's reflection request/result/HMAC
// surface. Built on the substrate landed by bd-ogqf6 (request ledger +
// HMAC keys + replay protection) and bd-3dw0l (gaps-only propose/ingest).
// This file does NOT touch src/core/reflect.rs or src/core/curate.rs —
// it pins externally observable invariants so future drift trips a
// focused failure rather than silently changing the protocol.

#![forbid(unsafe_code)]

use std::fs;
use std::str::FromStr;

use ee::curate::{
    DerivationSourceKind, DerivationSourceRef, ReflectionChallengeBinding,
    ReflectionChallengeError, ReflectionHmacKeyMaterial, ReflectionKind,
    ReflectionResultValidationError, ReflectionSourceInput, ReflectionSourcePackageLimits,
    attach_reflection_request_challenge, build_reflection_request_artifact,
    build_reflection_request_challenge, build_reflection_source_package,
    canonical_reflection_request_artifact_json, reflection_request_ledger_material,
    validate_reflection_request_artifact, validate_reflection_request_matches_ledger_material,
    validate_reflection_result_artifact, verify_reflection_request_challenge,
};

type TestResult = Result<(), String>;

// ---- Fixtures ---------------------------------------------------------

const WORKSPACE_ID: &str = "fixture-workspace";
const REFLECTION_KIND: &str = "gaps"; // ReflectionKind::Gaps; gaps-only is the v1 propose/ingest surface.
const KEY_ID: &str = "test-key-1";
const KEY_MATERIAL: &[u8] = b"test-hmac-key-material-32-bytes-long";
const ALT_KEY_MATERIAL: &[u8] = b"different-hmac-key-material-also-32b";
const CREATED_AT: &str = "2026-05-25T17:00:00Z";
const EXPIRES_AT: &str = "2026-05-25T17:15:00Z";

fn source_a() -> ReflectionSourceInput {
    ReflectionSourceInput::new(
        DerivationSourceRef::new(
            DerivationSourceKind::Memory,
            "mem_aaaaaaaaaaaaaaaaaaaaaaaaaa1",
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        "alpha memory content body for reflection contract test",
        None,
    )
}

fn source_b() -> ReflectionSourceInput {
    ReflectionSourceInput::new(
        DerivationSourceRef::new(
            DerivationSourceKind::Memory,
            "mem_bbbbbbbbbbbbbbbbbbbbbbbbbb2",
            "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
        "beta memory content body for reflection contract test",
        None,
    )
}

fn build_request_unsealed() -> Result<ee::curate::ReflectionRequestArtifact, String> {
    let sources = vec![source_a(), source_b()];
    let package =
        build_reflection_source_package(&sources, ReflectionSourcePackageLimits::default())
            .map_err(|e| format!("source package: {e}"))?;
    build_reflection_request_artifact(WORKSPACE_ID, REFLECTION_KIND, package)
        .map_err(|e| format!("artifact: {e}"))
}

fn build_request_sealed() -> Result<ee::curate::ReflectionRequestArtifact, String> {
    let artifact = build_request_unsealed()?;
    attach_reflection_request_challenge(artifact, CREATED_AT, EXPIRES_AT, KEY_ID, KEY_MATERIAL)
        .map_err(|e| format!("challenge: {e}"))
}

// ---- Determinism ------------------------------------------------------

#[test]
fn request_hash_is_stable_for_identical_canonical_inputs() -> TestResult {
    let a = build_request_unsealed()?;
    let b = build_request_unsealed()?;
    if a.request_hash != b.request_hash {
        return Err(format!(
            "requestHash drifted across identical inputs: a={}, b={}",
            a.request_hash, b.request_hash
        ));
    }
    if a.request_id != b.request_id {
        return Err("requestId drifted across identical canonical inputs".into());
    }
    Ok(())
}

#[test]
fn request_hash_excludes_volatile_lifecycle_fields() -> TestResult {
    let bare = build_request_unsealed()?;
    let sealed = build_request_sealed()?;
    if sealed.request_hash != bare.request_hash {
        return Err(format!(
            "requestHash must not depend on createdAt/expiresAt/challenge; bare={}, sealed={}",
            bare.request_hash, sealed.request_hash
        ));
    }
    if sealed.created_at.is_none() || sealed.expires_at.is_none() || sealed.challenge.is_none() {
        return Err("sealed artifact missing volatile-but-present lifecycle fields".into());
    }
    Ok(())
}

#[test]
fn request_hash_differs_when_workspace_kind_or_sources_change() -> TestResult {
    let baseline = build_request_unsealed()?;

    let alt_workspace = {
        let pkg = build_reflection_source_package(
            &[source_a(), source_b()],
            ReflectionSourcePackageLimits::default(),
        )
        .map_err(|e| e.to_string())?;
        build_reflection_request_artifact("OTHER-workspace", REFLECTION_KIND, pkg)
            .map_err(|e| e.to_string())?
    };
    if alt_workspace.request_hash == baseline.request_hash {
        return Err("requestHash unchanged after workspace change".into());
    }

    let alt_kind = {
        let pkg = build_reflection_source_package(
            &[source_a(), source_b()],
            ReflectionSourcePackageLimits::default(),
        )
        .map_err(|e| e.to_string())?;
        build_reflection_request_artifact(
            WORKSPACE_ID,
            ReflectionKind::ProceduralExtract.as_str(),
            pkg,
        )
        .map_err(|e| e.to_string())?
    };
    if alt_kind.request_hash == baseline.request_hash {
        return Err("requestHash unchanged after reflectionKind change".into());
    }

    let alt_sources = {
        let pkg = build_reflection_source_package(
            &[source_a()],
            ReflectionSourcePackageLimits::default(),
        )
        .map_err(|e| e.to_string())?;
        build_reflection_request_artifact(WORKSPACE_ID, REFLECTION_KIND, pkg)
            .map_err(|e| e.to_string())?
    };
    if alt_sources.request_hash == baseline.request_hash {
        return Err("requestHash unchanged after sources change".into());
    }

    Ok(())
}

#[test]
fn canonical_request_json_round_trips_through_serde() -> TestResult {
    let artifact = build_request_sealed()?;
    let json = canonical_reflection_request_artifact_json(&artifact).map_err(|e| e.to_string())?;
    let parsed: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("canonical JSON is not valid: {e}"))?;
    if parsed.get("requestId").and_then(|v| v.as_str()) != Some(artifact.request_id.as_str()) {
        return Err("requestId round-trip differs from artifact".into());
    }
    if parsed.get("requestHash").and_then(|v| v.as_str()) != Some(artifact.request_hash.as_str()) {
        return Err("requestHash round-trip differs from artifact".into());
    }
    Ok(())
}

// ---- HMAC bind set ---------------------------------------------------

fn binding<'a>(
    request_id: &'a str,
    request_hash: &'a str,
    workspace_id: &'a str,
    reflection_kind: &'a str,
    source_package_hash: &'a str,
    source_content_hashes: &'a [&'a str],
    response_schema_hash: &'a str,
    expires_at: &'a str,
    key_id: &'a str,
) -> ReflectionChallengeBinding<'a> {
    ReflectionChallengeBinding {
        request_id,
        request_hash,
        workspace_id,
        reflection_kind,
        source_package_hash,
        source_content_hashes,
        response_schema_hash,
        expires_at,
        key_id,
    }
}

const CANONICAL_REQUEST_ID: &str = "rq_canonical";
const CANONICAL_REQUEST_HASH: &str =
    "blake3:1111111111111111111111111111111111111111111111111111111111111111";
const CANONICAL_PKG_HASH: &str =
    "blake3:2222222222222222222222222222222222222222222222222222222222222222";
const CANONICAL_RESP_HASH: &str =
    "blake3:3333333333333333333333333333333333333333333333333333333333333333";

#[test]
fn hmac_binding_changes_when_any_bound_field_changes() -> TestResult {
    let source_hashes_base: [&str; 2] = [
        "blake3:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        "blake3:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
    ];

    let base = build_reflection_request_challenge(
        binding(
            CANONICAL_REQUEST_ID,
            CANONICAL_REQUEST_HASH,
            WORKSPACE_ID,
            REFLECTION_KIND,
            CANONICAL_PKG_HASH,
            &source_hashes_base,
            CANONICAL_RESP_HASH,
            EXPIRES_AT,
            KEY_ID,
        ),
        KEY_MATERIAL,
    )
    .map_err(|e| e.to_string())?;

    // Mutate each bound field one at a time; HMAC must change.
    let mutations: Vec<(&'static str, ReflectionChallengeBinding<'_>)> = vec![
        (
            "requestId",
            binding(
                "rq_other",
                CANONICAL_REQUEST_HASH,
                WORKSPACE_ID,
                REFLECTION_KIND,
                CANONICAL_PKG_HASH,
                &source_hashes_base,
                CANONICAL_RESP_HASH,
                EXPIRES_AT,
                KEY_ID,
            ),
        ),
        (
            "requestHash",
            binding(
                CANONICAL_REQUEST_ID,
                "blake3:9999999999999999999999999999999999999999999999999999999999999999",
                WORKSPACE_ID,
                REFLECTION_KIND,
                CANONICAL_PKG_HASH,
                &source_hashes_base,
                CANONICAL_RESP_HASH,
                EXPIRES_AT,
                KEY_ID,
            ),
        ),
        (
            "workspaceId",
            binding(
                CANONICAL_REQUEST_ID,
                CANONICAL_REQUEST_HASH,
                "OTHER",
                REFLECTION_KIND,
                CANONICAL_PKG_HASH,
                &source_hashes_base,
                CANONICAL_RESP_HASH,
                EXPIRES_AT,
                KEY_ID,
            ),
        ),
        (
            "reflectionKind",
            binding(
                CANONICAL_REQUEST_ID,
                CANONICAL_REQUEST_HASH,
                WORKSPACE_ID,
                "procedural_extract",
                CANONICAL_PKG_HASH,
                &source_hashes_base,
                CANONICAL_RESP_HASH,
                EXPIRES_AT,
                KEY_ID,
            ),
        ),
        (
            "sourcePackageHash",
            binding(
                CANONICAL_REQUEST_ID,
                CANONICAL_REQUEST_HASH,
                WORKSPACE_ID,
                REFLECTION_KIND,
                "blake3:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                &source_hashes_base,
                CANONICAL_RESP_HASH,
                EXPIRES_AT,
                KEY_ID,
            ),
        ),
        (
            "responseSchemaHash",
            binding(
                CANONICAL_REQUEST_ID,
                CANONICAL_REQUEST_HASH,
                WORKSPACE_ID,
                REFLECTION_KIND,
                CANONICAL_PKG_HASH,
                &source_hashes_base,
                "blake3:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                EXPIRES_AT,
                KEY_ID,
            ),
        ),
        (
            "expiresAt",
            binding(
                CANONICAL_REQUEST_ID,
                CANONICAL_REQUEST_HASH,
                WORKSPACE_ID,
                REFLECTION_KIND,
                CANONICAL_PKG_HASH,
                &source_hashes_base,
                CANONICAL_RESP_HASH,
                "2099-12-31T23:59:59Z",
                KEY_ID,
            ),
        ),
        (
            "keyId",
            binding(
                CANONICAL_REQUEST_ID,
                CANONICAL_REQUEST_HASH,
                WORKSPACE_ID,
                REFLECTION_KIND,
                CANONICAL_PKG_HASH,
                &source_hashes_base,
                CANONICAL_RESP_HASH,
                EXPIRES_AT,
                "rotated-key-2",
            ),
        ),
    ];

    for (field, mutated) in mutations {
        let challenge =
            build_reflection_request_challenge(mutated, KEY_MATERIAL).map_err(|e| e.to_string())?;
        if challenge.hmac == base.hmac {
            return Err(format!(
                "HMAC unchanged after mutating bound field `{field}`; bind set is incomplete"
            ));
        }
    }

    // Source content hashes are also bound. Permuting them must change the HMAC.
    let permuted_sources: [&str; 2] = [source_hashes_base[1], source_hashes_base[0]];
    let permuted = build_reflection_request_challenge(
        binding(
            CANONICAL_REQUEST_ID,
            CANONICAL_REQUEST_HASH,
            WORKSPACE_ID,
            REFLECTION_KIND,
            CANONICAL_PKG_HASH,
            &permuted_sources,
            CANONICAL_RESP_HASH,
            EXPIRES_AT,
            KEY_ID,
        ),
        KEY_MATERIAL,
    )
    .map_err(|e| e.to_string())?;
    if permuted.hmac == base.hmac {
        return Err("HMAC unchanged after permuting source content hashes".into());
    }

    Ok(())
}

#[test]
fn hmac_verification_round_trips_with_correct_key() -> TestResult {
    let source_hashes: [&str; 1] =
        ["blake3:0000000000000000000000000000000000000000000000000000000000000001"];
    let b = binding(
        CANONICAL_REQUEST_ID,
        CANONICAL_REQUEST_HASH,
        WORKSPACE_ID,
        REFLECTION_KIND,
        CANONICAL_PKG_HASH,
        &source_hashes,
        CANONICAL_RESP_HASH,
        EXPIRES_AT,
        KEY_ID,
    );
    let challenge =
        build_reflection_request_challenge(b, KEY_MATERIAL).map_err(|e| e.to_string())?;
    verify_reflection_request_challenge(b, KEY_MATERIAL, &challenge)
        .map_err(|e| format!("verification failed for matching key: {e}"))?;
    Ok(())
}

#[test]
fn hmac_verification_fails_with_wrong_key_material() -> TestResult {
    let source_hashes: [&str; 1] =
        ["blake3:0000000000000000000000000000000000000000000000000000000000000002"];
    let b = binding(
        CANONICAL_REQUEST_ID,
        CANONICAL_REQUEST_HASH,
        WORKSPACE_ID,
        REFLECTION_KIND,
        CANONICAL_PKG_HASH,
        &source_hashes,
        CANONICAL_RESP_HASH,
        EXPIRES_AT,
        KEY_ID,
    );
    let challenge =
        build_reflection_request_challenge(b, KEY_MATERIAL).map_err(|e| e.to_string())?;
    match verify_reflection_request_challenge(b, ALT_KEY_MATERIAL, &challenge) {
        Err(ReflectionChallengeError::ChallengeHmacMismatch) => Ok(()),
        other => Err(format!(
            "expected ChallengeHmacMismatch with wrong key, got {other:?}"
        )),
    }
}

#[test]
fn hmac_verification_fails_when_challenge_key_id_drifts() -> TestResult {
    let source_hashes: [&str; 1] =
        ["blake3:0000000000000000000000000000000000000000000000000000000000000003"];
    let b = binding(
        CANONICAL_REQUEST_ID,
        CANONICAL_REQUEST_HASH,
        WORKSPACE_ID,
        REFLECTION_KIND,
        CANONICAL_PKG_HASH,
        &source_hashes,
        CANONICAL_RESP_HASH,
        EXPIRES_AT,
        KEY_ID,
    );
    let mut challenge =
        build_reflection_request_challenge(b, KEY_MATERIAL).map_err(|e| e.to_string())?;
    challenge.key_id = "rotated-key-but-binding-still-says-original".to_owned();
    match verify_reflection_request_challenge(b, KEY_MATERIAL, &challenge) {
        Err(ReflectionChallengeError::ChallengeKeyMismatch { .. }) => Ok(()),
        other => Err(format!(
            "expected ChallengeKeyMismatch when challenge.key_id drifts, got {other:?}"
        )),
    }
}

#[test]
fn hmac_verification_fails_when_algorithm_drifts() -> TestResult {
    let source_hashes: [&str; 1] =
        ["blake3:0000000000000000000000000000000000000000000000000000000000000004"];
    let b = binding(
        CANONICAL_REQUEST_ID,
        CANONICAL_REQUEST_HASH,
        WORKSPACE_ID,
        REFLECTION_KIND,
        CANONICAL_PKG_HASH,
        &source_hashes,
        CANONICAL_RESP_HASH,
        EXPIRES_AT,
        KEY_ID,
    );
    let mut challenge =
        build_reflection_request_challenge(b, KEY_MATERIAL).map_err(|e| e.to_string())?;
    challenge.algorithm = "hmac-md5".to_owned();
    match verify_reflection_request_challenge(b, KEY_MATERIAL, &challenge) {
        Err(ReflectionChallengeError::ChallengeAlgorithmMismatch { .. }) => Ok(()),
        other => Err(format!(
            "expected ChallengeAlgorithmMismatch when algorithm drifts, got {other:?}"
        )),
    }
}

// ---- Ledger material parity & lifecycle ------------------------------

#[test]
fn ledger_material_matches_sealed_artifact() -> TestResult {
    let artifact = build_request_sealed()?;
    let material = reflection_request_ledger_material(&artifact).map_err(|e| e.to_string())?;
    validate_reflection_request_matches_ledger_material(&artifact, &material)
        .map_err(|e| format!("ledger material mismatch on sealed artifact: {e:?}"))?;
    Ok(())
}

#[test]
fn ledger_material_mismatch_detected_on_workspace_drift() -> TestResult {
    let artifact = build_request_sealed()?;
    let mut material = reflection_request_ledger_material(&artifact).map_err(|e| e.to_string())?;
    material.workspace_id = "TAMPERED-workspace".to_owned();
    match validate_reflection_request_matches_ledger_material(&artifact, &material) {
        Err(_) => Ok(()),
        Ok(()) => Err("validator accepted artifact with tampered ledger workspace_id".into()),
    }
}

#[test]
fn request_artifact_validation_rejects_expiry_before_creation() -> TestResult {
    let artifact = build_request_unsealed()?;
    // Manually populate inverted lifecycle WITHOUT a challenge so the lifecycle
    // check runs first.
    let mut artifact = artifact;
    artifact.created_at = Some(EXPIRES_AT.to_owned());
    artifact.expires_at = Some(CREATED_AT.to_owned());
    match validate_reflection_request_artifact(&artifact) {
        Err(e) => {
            let display = format!("{e}");
            if !display.contains("expiresAt") && !display.contains("expiry") {
                return Err(format!("expected expiresAt diagnostic, got: {display}"));
            }
            Ok(())
        }
        Ok(()) => Err("validator accepted artifact whose expiry precedes creation".into()),
    }
}

// ---- Result fail-closed paths ----------------------------------------

#[test]
fn result_validation_rejects_request_expired() -> TestResult {
    let request = build_request_sealed()?;
    // canonical fake result that matches request identity/challenge
    let result = ee::curate::ReflectionResultArtifact {
        schema: "ee.reflect.result.v1".to_owned(),
        request_id: request.request_id.clone(),
        request_hash: request.request_hash.clone(),
        challenge: request
            .challenge
            .clone()
            .ok_or_else(|| "sealed artifact must carry a challenge".to_owned())?,
        producer: ee::curate::ReflectionResultProducer {
            kind: "test_producer".to_owned(),
            id: "fixture".to_owned(),
            version: Some("1.0".to_owned()),
            extra: Default::default(),
        },
        reflection_kind: request.reflection_kind.clone(),
        cited_source_ids: request
            .source_package
            .sources
            .iter()
            .map(|entry| entry.id.clone())
            .collect(),
        body: "fixture reflection result body".to_owned(),
        kind_fields: serde_json::Map::new(),
        self_reported_confidence: 0.5,
    };
    // now > expires_at → expired
    let now = "2099-12-31T23:59:59Z";
    match validate_reflection_result_artifact(&request, &result, KEY_MATERIAL, now) {
        Err(ReflectionResultValidationError::RequestExpired { .. }) => Ok(()),
        other => Err(format!("expected RequestExpired, got {other:?}")),
    }
}

// ---- Redacted diagnostic output --------------------------------------

#[test]
fn hmac_key_material_debug_is_redacted() -> TestResult {
    let key = ReflectionHmacKeyMaterial::new(KEY_ID, KEY_MATERIAL)
        .map_err(|e| format!("key material construction failed: {e}"))?;
    let dbg = format!("{key:?}");
    if dbg.contains(std::str::from_utf8(KEY_MATERIAL).unwrap_or("")) {
        return Err(format!(
            "Debug output of ReflectionHmacKeyMaterial leaks raw key bytes: {dbg}"
        ));
    }
    if !dbg.contains("<redacted>") {
        return Err(format!(
            "Debug output of ReflectionHmacKeyMaterial must mark key_material as redacted; got: {dbg}"
        ));
    }
    if !dbg.contains(KEY_ID) {
        return Err(format!(
            "Debug output of ReflectionHmacKeyMaterial should keep non-secret key_id; got: {dbg}"
        ));
    }
    Ok(())
}

// ---- ReflectionKind parser is closed over the v1 vocabulary ---------

#[test]
fn reflection_kind_round_trips_for_documented_v1_kinds() -> TestResult {
    let all = [
        ReflectionKind::Summary,
        ReflectionKind::Insight,
        ReflectionKind::Gaps,
        ReflectionKind::Strengths,
        ReflectionKind::Question,
        ReflectionKind::Plan,
        ReflectionKind::SummaryInsightStrengths,
        ReflectionKind::PlanKinds,
        ReflectionKind::ProceduralExtract,
        ReflectionKind::ContradictionResolve,
    ];
    for kind in all {
        let parsed = ReflectionKind::from_str(kind.as_str())
            .map_err(|e| format!("ReflectionKind `{kind:?}` failed round-trip: {e}"))?;
        if parsed != kind {
            return Err(format!(
                "ReflectionKind round-trip differs: input={kind:?}, parsed={parsed:?}"
            ));
        }
    }
    // unknown kind must fail
    if ReflectionKind::from_str("not_a_real_kind").is_ok() {
        return Err("ReflectionKind accepted an unknown variant".into());
    }
    Ok(())
}

// ---- Forbidden-dependency guard for the reflection surface ----------
//
// AGENTS.md forbids tokio/async-std/smol/rusqlite/petgraph/hyper/axum/
// tower/reqwest/sqlx/diesel/sea-orm in the ee dep tree. The reflection
// handshake is a deterministic in-process protocol; if any of these
// crates show up in Cargo.lock under names the reflect surface owns,
// something has regressed. The general forbidden-deps audit lives at
// tests/forbidden_deps.rs; this is a reflection-scoped cross-check
// proving no NEW forbidden dep can land via a reflect-only PR.

#[test]
fn cargo_lock_remains_free_of_forbidden_runtime_and_http_deps() -> TestResult {
    let lock =
        fs::read_to_string("Cargo.lock").map_err(|e| format!("failed to read Cargo.lock: {e}"))?;
    // Match `name = "..."` at column 0 to avoid false hits on description strings.
    let forbidden = [
        "tokio",
        "tokio-util",
        "async-std",
        "smol",
        "rusqlite",
        "sqlx",
        "diesel",
        "sea-orm",
        "petgraph",
        "hyper",
        "axum",
        "tower",
        "reqwest",
    ];
    let mut hits = Vec::new();
    for crate_name in forbidden {
        let needle = format!("name = \"{crate_name}\"");
        if lock.contains(&needle) {
            hits.push(crate_name);
        }
    }
    if !hits.is_empty() {
        return Err(format!(
            "forbidden dependencies present in Cargo.lock: {hits:?}; reflect surface must remain Tokio/HTTP-free per AGENTS.md"
        ));
    }
    Ok(())
}
