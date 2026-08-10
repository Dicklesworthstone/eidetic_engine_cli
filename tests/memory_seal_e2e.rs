#![allow(clippy::expect_used, clippy::unwrap_used)]
//! bd-sealed-preregistration-memory-b67be: commit-reveal sealed memories at
//! the process level.
//!
//! Drives the built `ee` binary through the full seal lifecycle the feature
//! promises: seal a protocol (content withheld, commitment recorded), fail a
//! reveal with wrong bytes (zero mutation, honest error), succeed with the
//! exact bytes (published through the revise path), refuse a second reveal,
//! surface seal state on `ee why`, and keep sealed placeholders out of packs.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const PROTOCOL: &str = "PRE-REGISTERED PROTOCOL v1: measure retrieval precision on fixture set A before reading any outcome labels.";

fn temp_workspace() -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix("ee-memory-seal-")
        .tempdir_in("/tmp")
        .map_err(|error| format!("failed to create temp workspace under /tmp: {error}"))
}

fn run_ee(workspace: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .arg("--workspace")
        .arg(workspace)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn stdout_json(output: &Output, label: &str) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{label}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{label}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_success(output: &Output, label: &str) -> TestResult {
    ensure(
        output.status.success(),
        format!(
            "{label}: ee exited {:?}; stdout: {}; stderr: {}",
            output.status.code(),
            // --json errors land on stdout as the ee.error.v2 envelope;
            // dropping stdout here once cost a debugging round trip.
            String::from_utf8_lossy(&output.stdout).trim_end(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        ),
    )
}

fn ensure_failure(output: &Output, label: &str) -> TestResult {
    ensure(
        !output.status.success(),
        format!("{label}: expected nonzero exit, got success"),
    )
}

/// The seal object may sit at the envelope top level or under `data`
/// depending on the surface; accept both.
fn seal_object<'a>(value: &'a serde_json::Value) -> Option<&'a serde_json::Value> {
    value
        .pointer("/data/seal")
        .or_else(|| value.get("seal"))
        .filter(|seal| seal.is_object())
}

fn init_and_seal(workspace: &Path) -> Result<(String, String), String> {
    let ws = workspace.to_string_lossy().to_string();
    let init = run_ee(&ws, &["init", "--json"])?;
    ensure_success(&init, "init")?;
    let sealed = run_ee(
        &ws,
        &[
            "remember", PROTOCOL, "--seal", "--level", "semantic", "--kind", "decision", "--json",
        ],
    )?;
    ensure_success(&sealed, "remember --seal")?;
    let envelope = stdout_json(&sealed, "remember --seal")?;
    let memory_id = envelope
        .pointer("/data/memoryId")
        .or_else(|| envelope.pointer("/data/memory_id"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("remember --seal: no memory id in {envelope}"))?
        .to_owned();
    let seal = seal_object(&envelope)
        .ok_or_else(|| format!("remember --seal: no seal object in {envelope}"))?;
    let commitment = seal
        .get("contentCommitment")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("remember --seal: no contentCommitment in {seal}"))?
        .to_owned();
    ensure(
        commitment.starts_with("blake3:") && commitment.len() == 71,
        format!("malformed commitment {commitment}"),
    )?;
    // The report must withhold the sealed content.
    let reported_content = envelope
        .pointer("/data/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    ensure(
        !reported_content.contains("retrieval precision"),
        "sealed remember report leaked the protocol content",
    )?;
    Ok((memory_id, commitment))
}

fn why_seal(workspace: &str, memory_id: &str) -> Result<serde_json::Value, String> {
    let why = run_ee(workspace, &["why", memory_id, "--json"])?;
    ensure_success(&why, "why")?;
    let envelope = stdout_json(&why, "why")?;
    seal_object(&envelope)
        .cloned()
        .ok_or_else(|| format!("why: no seal object for {memory_id}: {envelope}"))
}

#[test]
fn seal_reveal_lifecycle_end_to_end() -> TestResult {
    let workspace = temp_workspace()?;
    let ws = workspace.path().to_string_lossy().to_string();
    let (memory_id, commitment) = init_and_seal(workspace.path())?;

    // why: sealed and unrevealed.
    let seal = why_seal(&ws, &memory_id)?;
    ensure(
        seal.get("sealed") == Some(&serde_json::Value::Bool(true)),
        format!("why must report sealed=true before reveal: {seal}"),
    )?;

    // Mismatched reveal: refused, nothing mutated.
    let wrong = workspace.path().join("wrong.txt");
    fs::write(&wrong, "a different protocol entirely").map_err(|error| error.to_string())?;
    let mismatch = run_ee(
        &ws,
        &[
            "memory",
            "reveal",
            &memory_id,
            "--content-file",
            wrong.to_str().unwrap(),
            "--json",
        ],
    )?;
    ensure_failure(&mismatch, "mismatched reveal")?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&mismatch.stdout),
        String::from_utf8_lossy(&mismatch.stderr)
    );
    ensure(
        combined.contains("does not match"),
        format!("mismatch error must say so: {combined}"),
    )?;
    let seal = why_seal(&ws, &memory_id)?;
    ensure(
        seal.get("sealed") == Some(&serde_json::Value::Bool(true)),
        "failed reveal must not unseal",
    )?;

    // Correct reveal: verified, published through revise.
    let right = workspace.path().join("protocol.txt");
    fs::write(&right, PROTOCOL).map_err(|error| error.to_string())?;
    let reveal = run_ee(
        &ws,
        &[
            "memory",
            "reveal",
            &memory_id,
            "--content-file",
            right.to_str().unwrap(),
            "--json",
        ],
    )?;
    ensure_success(&reveal, "reveal")?;
    let envelope = stdout_json(&reveal, "reveal")?;
    ensure(
        envelope.pointer("/data/revealVerified") == Some(&serde_json::Value::Bool(true)),
        format!("reveal must verify: {envelope}"),
    )?;
    ensure(
        envelope
            .pointer("/data/contentCommitment")
            .and_then(serde_json::Value::as_str)
            == Some(commitment.as_str()),
        "reveal must echo the original commitment",
    )?;
    let revealed_id = envelope
        .pointer("/data/revealedMemoryId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("reveal: no revealedMemoryId: {envelope}"))?
        .to_owned();
    ensure(revealed_id != memory_id, "revise must mint a new memory id")?;

    // Seal row reflects the reveal.
    let seal = why_seal(&ws, &memory_id)?;
    ensure(
        seal.get("sealed") == Some(&serde_json::Value::Bool(false)),
        format!("why must report sealed=false after reveal: {seal}"),
    )?;
    ensure(
        seal.get("revealVerified") == Some(&serde_json::Value::Bool(true)),
        "why must report revealVerified=true",
    )?;

    // The revealed memory carries the protocol content.
    let why_new = run_ee(&ws, &["why", &revealed_id, "--json"])?;
    ensure_success(&why_new, "why revealed")?;
    let new_envelope = stdout_json(&why_new, "why revealed")?;
    let content = new_envelope
        .pointer("/data/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    ensure(
        content.contains("retrieval precision"),
        format!("revealed memory must carry the protocol content, got: {content}"),
    )?;

    // Second reveal: refused.
    let again = run_ee(
        &ws,
        &[
            "memory",
            "reveal",
            &memory_id,
            "--content-file",
            right.to_str().unwrap(),
            "--json",
        ],
    )?;
    ensure_failure(&again, "double reveal")
}

#[test]
fn sealed_memory_stays_out_of_packs() -> TestResult {
    let workspace = temp_workspace()?;
    let ws = workspace.path().to_string_lossy().to_string();
    let (memory_id, _) = init_and_seal(workspace.path())?;

    // An unsealed memory so the pack has something honest to serve.
    let plain = run_ee(
        &ws,
        &[
            "remember",
            "Sealed memory handling: reveal before relying on it.",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--json",
        ],
    )?;
    ensure_success(&plain, "remember plain")?;

    // Query words chosen to hit the placeholder lexically.
    let pack = run_ee(&ws, &["pack", "sealed memory reveal", "--json"])?;
    ensure_success(&pack, "pack")?;
    let stdout = String::from_utf8_lossy(&pack.stdout).to_string();
    ensure(
        !stdout.contains(&memory_id),
        "pack must not serve the sealed memory",
    )?;
    ensure(
        !stdout.contains("content committed by hash; reveal with"),
        "pack must not spend budget on the seal placeholder text",
    )
}

#[test]
fn reveal_without_seal_and_seal_guards_fail_closed() -> TestResult {
    let workspace = temp_workspace()?;
    let ws = workspace.path().to_string_lossy().to_string();
    let init = run_ee(&ws, &["init", "--json"])?;
    ensure_success(&init, "init")?;

    // Ordinary memory: reveal must refuse.
    let plain = run_ee(&ws, &["remember", "ordinary fact", "--json"])?;
    ensure_success(&plain, "remember")?;
    let envelope = stdout_json(&plain, "remember")?;
    let memory_id = envelope
        .pointer("/data/memoryId")
        .or_else(|| envelope.pointer("/data/memory_id"))
        .and_then(serde_json::Value::as_str)
        .ok_or("no memory id")?
        .to_owned();
    let file = workspace.path().join("c.txt");
    fs::write(&file, "ordinary fact").map_err(|error| error.to_string())?;
    let reveal = run_ee(
        &ws,
        &[
            "memory",
            "reveal",
            &memory_id,
            "--content-file",
            file.to_str().unwrap(),
            "--json",
        ],
    )?;
    ensure_failure(&reveal, "reveal of unsealed memory")?;

    // --seal with contradictory companions.
    for extra in [
        vec!["--reinforce"],
        vec!["--sentinel", "path_exists:README.md"],
        vec!["--global"],
    ] {
        let mut args = vec!["remember", "sealed protocol", "--seal", "--json"];
        args.extend(extra.iter().copied());
        let out = run_ee(&ws, &args)?;
        ensure_failure(&out, &format!("--seal with {extra:?} must be rejected"))?;
    }
    Ok(())
}

#[test]
fn attestation_bundle_carries_the_seal_block_offline() -> TestResult {
    // bd-2ea4a: `ee attest memory` must embed the commit-reveal material so
    // a third party verifies commitment-before-outcome without the database.
    let workspace = temp_workspace()?;
    let ws = workspace.path().to_string_lossy().to_string();
    let (memory_id, commitment) = init_and_seal(workspace.path())?;

    let attest = run_ee(&ws, &["attest", "memory", &memory_id, "--json"])?;
    ensure_success(&attest, "attest memory (sealed)")?;
    let envelope = stdout_json(&attest, "attest memory (sealed)")?;
    let bundle = envelope
        .pointer("/data/bundle")
        .ok_or_else(|| format!("attest: no bundle in {envelope}"))?;
    ensure(
        bundle
            .pointer("/schema")
            .and_then(serde_json::Value::as_str)
            == Some("ee.attestation.bundle.v2"),
        format!("bundle schema must be v2: {bundle}"),
    )?;
    ensure(
        bundle
            .pointer("/seal/contentCommitment")
            .and_then(serde_json::Value::as_str)
            == Some(commitment.as_str()),
        format!("bundle seal must carry the exact commitment: {bundle}"),
    )?;
    ensure(
        bundle
            .pointer("/seal/revealedAt")
            .is_some_and(serde_json::Value::is_null),
        "unrevealed seal must report revealedAt null",
    )?;

    // Unsealed memories must NOT carry a seal key (v1-identical shape).
    let plain = run_ee(&ws, &["remember", "an ordinary unsealed note", "--json"])?;
    ensure_success(&plain, "remember unsealed")?;
    let plain_id = stdout_json(&plain, "remember unsealed")?
        .pointer("/data/memoryId")
        .and_then(serde_json::Value::as_str)
        .ok_or("unsealed remember: no memory id")?
        .to_owned();
    let attest_plain = run_ee(&ws, &["attest", "memory", &plain_id, "--json"])?;
    ensure_success(&attest_plain, "attest memory (unsealed)")?;
    let plain_envelope = stdout_json(&attest_plain, "attest memory (unsealed)")?;
    ensure(
        plain_envelope.pointer("/data/bundle/seal").is_none(),
        format!("unsealed bundle must omit the seal key: {plain_envelope}"),
    )?;
    Ok(())
}
