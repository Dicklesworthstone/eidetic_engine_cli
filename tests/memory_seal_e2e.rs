#![allow(clippy::expect_used, clippy::unwrap_used)]
//! bd-sealed-preregistration-memory-b67be: commit-reveal sealed memories at
//! the process level.
//!
//! Drives the built `ee` binary through the full seal lifecycle the feature
//! promises: seal a protocol (content withheld, commitment recorded), fail a
//! reveal with wrong bytes (zero mutation, honest error), succeed with the
//! exact bytes (published through the revise path), refuse a second reveal,
//! surface seal state on `ee why`, and keep sealed placeholders out of the
//! real index, search results, and packs.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

type TestResult = Result<(), String>;

const PROTOCOL: &str = "PRE-REGISTERED PROTOCOL v1: measure retrieval precision on fixture set A before reading any outcome labels.";
const PLACEHOLDER: &str =
    "[sealed memory: content committed by hash; reveal with `ee memory reveal`]";

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

fn run_ee_with_stdin(workspace: &str, args: &[&str], stdin: &[u8]) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .args(args)
        .arg("--workspace")
        .arg(workspace)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to spawn ee {}: {error}", args.join(" ")))?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("ee {} did not expose piped stdin", args.join(" ")))?;
    child_stdin
        .write_all(stdin)
        .map_err(|error| format!("failed to write bounded ee stdin: {error}"))?;
    drop(child_stdin);
    child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for ee {}: {error}", args.join(" ")))
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
fn sealed_memory_stays_out_of_search_and_packs() -> TestResult {
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
    let plain_id = stdout_json(&plain, "remember plain")?
        .pointer("/data/memoryId")
        .and_then(serde_json::Value::as_str)
        .ok_or("remember plain: no memory id")?
        .to_owned();

    // The bytes alone do not confer seal status. This exact-content control
    // catches the former false-positive where index/search treated the public
    // placeholder string as authoritative instead of consulting memory_seals.
    let placeholder_control = run_ee(&ws, &["remember", PLACEHOLDER, "--json"])?;
    ensure_success(&placeholder_control, "remember exact-placeholder control")?;
    let placeholder_control_id =
        stdout_json(&placeholder_control, "remember exact-placeholder control")?
            .pointer("/data/memoryId")
            .and_then(serde_json::Value::as_str)
            .ok_or("remember exact-placeholder control: no memory id")?
            .to_owned();

    // Both ordinary memories are real index documents. The sealed row is
    // storage metadata until reveal, regardless of identical public bytes.
    let index_rebuild = run_ee(&ws, &["index", "rebuild", "--json"])?;
    ensure_success(&index_rebuild, "index rebuild")?;
    let index_envelope = stdout_json(&index_rebuild, "index rebuild")?;
    ensure(
        index_envelope
            .pointer("/data/memories_indexed")
            .and_then(serde_json::Value::as_u64)
            == Some(2),
        format!("both unsealed memories and no sealed row must be indexed: {index_envelope}"),
    )?;

    let search = run_ee(
        &ws,
        &[
            "search",
            "sealed memory reveal",
            "--limit",
            "100",
            "--source-mode",
            "lexical_only",
            "--strict-source-mode",
            "--json",
        ],
    )?;
    ensure_success(&search, "search")?;
    let search_stdout = String::from_utf8_lossy(&search.stdout).to_string();
    ensure(
        search_stdout.contains(&plain_id),
        "search control memory must remain discoverable",
    )?;
    ensure(
        search_stdout.contains(&placeholder_control_id),
        "ordinary memory equal to the placeholder text must remain discoverable",
    )?;
    ensure(
        !search_stdout.contains(&memory_id),
        "search must not return the sealed memory",
    )?;
    ensure(
        search_stdout.contains(PLACEHOLDER),
        "search must preserve exact placeholder bytes on an ordinary memory",
    )?;
    ensure(
        !search_stdout.contains(PROTOCOL),
        "search must not expose the unrevealed protocol bytes",
    )?;

    // Query words chosen to hit the placeholder lexically.
    let pack = run_ee(&ws, &["pack", "sealed memory reveal", "--json"])?;
    ensure_success(&pack, "pack")?;
    let stdout = String::from_utf8_lossy(&pack.stdout).to_string();
    ensure(
        !stdout.contains(&memory_id),
        "pack must not serve the sealed memory",
    )?;
    ensure(
        !stdout.contains(PROTOCOL),
        "pack must not expose the unrevealed protocol bytes",
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

    // Every single-memory companion that consumes, predicates over, scopes, or
    // deduplicates content must reject --seal before it can mutate storage.
    for (extra, expected) in [
        (vec!["--reinforce"], "--reinforce"),
        (
            vec!["--sentinel", "path_exists:README.md"],
            "--sentinel/--revive-when",
        ),
        (
            vec!["--revive-when", "path_exists:README.md"],
            "--sentinel/--revive-when",
        ),
        (
            vec!["--idempotency-key", "sealed-guard-key"],
            "--idempotency-key",
        ),
        (vec!["--global"], "--global"),
    ] {
        let mut args = vec!["remember", "sealed protocol", "--seal", "--json"];
        args.extend(extra.iter().copied());
        let out = run_ee(&ws, &args)?;
        ensure_failure(&out, &format!("--seal with {extra:?} must be rejected"))?;
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        ensure(
            output.contains(expected),
            format!("--seal with {extra:?} failed for the wrong reason: {output}"),
        )?;
    }

    for extra in [
        vec!["--from-commit", "HEAD"],
        vec!["--from-diff", "HEAD"],
        vec!["--from-worktree"],
    ] {
        let mut args = vec!["remember", "--seal", "--json"];
        args.extend(extra.iter().copied());
        let out = run_ee(&ws, &args)?;
        ensure_failure(&out, &format!("--seal with {extra:?} must be rejected"))?;
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        ensure(
            output.contains("--seal cannot be combined with git capture modes"),
            format!("--seal with {extra:?} failed for the wrong reason: {output}"),
        )?;
    }

    let batch = run_ee_with_stdin(
        &ws,
        &["remember", "--seal", "--batch", "--stdin", "--json"],
        br#"{"content":"sealed batch protocol"}
"#,
    )?;
    ensure_failure(&batch, "--seal with --batch --stdin must be rejected")?;
    let batch_output = format!(
        "{}{}",
        String::from_utf8_lossy(&batch.stdout),
        String::from_utf8_lossy(&batch.stderr)
    );
    ensure(
        batch_output.contains("--seal applies to single-memory mode"),
        format!("--seal with batch failed for the wrong reason: {batch_output}"),
    )?;
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

    // Reveal publishes a new immutable memory revision. Attesting either the
    // original row or the live revision must resolve to the same seal lineage.
    let content_file = workspace.path().join("attested-protocol.txt");
    fs::write(&content_file, PROTOCOL).map_err(|error| error.to_string())?;
    let reveal = run_ee(
        &ws,
        &[
            "memory",
            "reveal",
            &memory_id,
            "--content-file",
            content_file.to_str().unwrap(),
            "--json",
        ],
    )?;
    ensure_success(&reveal, "reveal before post-reveal attest")?;
    let reveal_envelope = stdout_json(&reveal, "reveal before post-reveal attest")?;
    let revealed_id = reveal_envelope
        .pointer("/data/revealedMemoryId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("reveal omitted revised memory id: {reveal_envelope}"))?
        .to_owned();
    let revealed_at = reveal_envelope
        .pointer("/data/revealedAt")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("reveal omitted revealedAt: {reveal_envelope}"))?;
    ensure(
        reveal_envelope
            .pointer("/data/revisionGroupId")
            .and_then(serde_json::Value::as_str)
            == Some(memory_id.as_str()),
        format!("reveal must retain the sealed memory as logical root: {reveal_envelope}"),
    )?;
    ensure(
        reveal_envelope
            .pointer("/data/revisionNumber")
            .and_then(serde_json::Value::as_u64)
            == Some(2),
        format!("first reveal must publish revision two: {reveal_envelope}"),
    )?;

    let attest_original = run_ee(&ws, &["attest", "memory", &memory_id, "--json"])?;
    ensure_success(&attest_original, "attest original memory after reveal")?;
    let original_envelope = stdout_json(&attest_original, "attest original memory after reveal")?;
    let original_seal = original_envelope
        .pointer("/data/bundle/seal")
        .ok_or_else(|| format!("post-reveal original attest omitted seal: {original_envelope}"))?;

    let attest_revised = run_ee(&ws, &["attest", "memory", &revealed_id, "--json"])?;
    ensure_success(&attest_revised, "attest revised memory after reveal")?;
    let revised_envelope = stdout_json(&attest_revised, "attest revised memory after reveal")?;
    let revised_bundle = revised_envelope
        .pointer("/data/bundle")
        .ok_or_else(|| format!("post-reveal revised attest omitted bundle: {revised_envelope}"))?;
    let revised_seal = revised_bundle
        .pointer("/seal")
        .ok_or_else(|| format!("post-reveal revised attest omitted seal: {revised_bundle}"))?;
    ensure(
        revised_bundle
            .pointer("/subject/id")
            .and_then(serde_json::Value::as_str)
            == Some(revealed_id.as_str()),
        format!("attestation subject must remain the revised memory: {revised_bundle}"),
    )?;
    ensure(
        revised_seal == original_seal,
        format!("original and revised attestations must share one seal: {revised_bundle}"),
    )?;
    ensure(
        revised_seal
            .pointer("/contentCommitment")
            .and_then(serde_json::Value::as_str)
            == Some(commitment.as_str()),
        format!("revised attestation changed the commitment: {revised_seal}"),
    )?;
    ensure(
        revised_seal
            .pointer("/revealedAt")
            .and_then(serde_json::Value::as_str)
            == Some(revealed_at),
        format!("revised attestation must carry the exact reveal timestamp: {revised_seal}"),
    )?;
    ensure(
        revised_seal.pointer("/revealVerified") == Some(&serde_json::Value::Bool(true)),
        format!("revised attestation must report a verified reveal: {revised_seal}"),
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
