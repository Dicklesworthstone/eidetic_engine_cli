use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type TestResult = Result<(), String>;

fn repo_file(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn read_repo_file(path: &str) -> Result<String, String> {
    fs::read_to_string(repo_file(path)).map_err(|error| format!("failed to read `{path}`: {error}"))
}

fn tempdir() -> Result<tempfile::TempDir, String> {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_file("target/tmp/e2e_artifact_manifest_contract"));
    fs::create_dir_all(&base)
        .map_err(|error| format!("failed to create temp base `{}`: {error}", base.display()))?;
    tempfile::Builder::new()
        .prefix("artifact-manifest-")
        .tempdir_in(&base)
        .map_err(|error| format!("failed to create tempdir in `{}`: {error}", base.display()))
}

fn write_fake_binary(path: &Path) -> Result<(), String> {
    let source = r#"#!/usr/bin/env bash
set -uo pipefail
if [ "${1:-}" = "--version" ]; then
    printf 'ee 0.0.0-artifact-manifest-test\n'
    exit 0
fi
printf 'unexpected fake binary invocation: %s\n' "$*" >&2
exit 64
"#;
    fs::write(path, source).map_err(|error| format!("failed to write fake binary: {error}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|error| format!("failed to stat fake binary: {error}"))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("failed to chmod fake binary: {error}"))?;
    }

    Ok(())
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read `{}`: {error}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<Value>(line).map_err(|error| {
                format!(
                    "{}:{} invalid JSON: {error}: {line}",
                    path.display(),
                    index + 1
                )
            })
        })
        .collect()
}

fn field<'a>(event: &'a Value, key: &str) -> Result<&'a str, String> {
    event
        .pointer(&format!("/fields/{key}"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("artifact_manifest missing fields.{key}: {event}"))
}

fn assert_hash(value: &str, key: &str) -> TestResult {
    if (value.starts_with("blake3:") || value.starts_with("sha256:")) && value.len() >= 71 {
        Ok(())
    } else {
        Err(format!("{key} is not a supported hash: {value}"))
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("failed to read `{}` for hashing: {error}", path.display()))?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity("sha256:".len() + (digest.len() * 2));
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(encoded, "{byte:02x}").map_err(|error| error.to_string())?;
    }
    Ok(encoded)
}

fn repeated_hash(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn remote_artifact_verification_report(binary_hash: &str) -> Value {
    json!({
        "schema": "ee.remote_build_artifact_manifest.verification.v1",
        "status": "verified",
        "accepted": true,
        "artifactName": "ee-macos-arm64",
        "artifactId": "9001",
        "repository": "Dicklesworthstone/eidetic_engine_cli",
        "workflow": "macos-ee-artifact.yml",
        "runId": "8001",
        "runAttempt": 1,
        "sourceCommit": "1".repeat(40),
        "gitTree": "2".repeat(40),
        "manifestHash": repeated_hash('3'),
        "buildCommandHash": repeated_hash('4'),
        "effectiveInputHash": repeated_hash('5'),
        "provenanceHash": repeated_hash('6'),
        "target": "aarch64-apple-darwin",
        "profile": "release",
        "binaryHash": binary_hash,
        "archiveHash": repeated_hash('7'),
        "archiveSizeBytes": 4096,
        "checksumStatus": "verified",
        "probeStatus": "passed",
        "probes": [
            {
                "id": "version_json",
                "argvHash": repeated_hash('8'),
                "exitCode": 0,
                "stdoutHash": repeated_hash('9'),
                "stderrHash": repeated_hash('a'),
                "status": "passed",
                "semanticAssertions": []
            },
            {
                "id": "environment_attestation_help",
                "argvHash": repeated_hash('b'),
                "exitCode": 0,
                "stdoutHash": repeated_hash('c'),
                "stderrHash": repeated_hash('d'),
                "status": "passed",
                "semanticAssertions": []
            }
        ],
        "rejections": [],
        "rawOutputIncluded": false,
        "verificationHash": repeated_hash('e')
    })
}

#[test]
fn schema_registers_artifact_manifest_event_kind() -> TestResult {
    let schema_text = read_repo_file("docs/schemas/test_event_v1.json")?;
    let schema: Value = serde_json::from_str(&schema_text)
        .map_err(|error| format!("schema is invalid JSON: {error}"))?;
    let kinds = schema
        .pointer("/properties/kind/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema missing kind enum".to_owned())?;
    if !kinds.iter().any(|kind| kind == "artifact_manifest") {
        return Err("schema kind enum missing artifact_manifest".to_owned());
    }
    let required = schema
        .pointer("/allOf")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema missing allOf".to_owned())?
        .iter()
        .find(|entry| {
            entry.pointer("/if/properties/kind/const")
                == Some(&Value::String("artifact_manifest".to_owned()))
        })
        .and_then(|entry| entry.pointer("/then/properties/fields/required"))
        .and_then(Value::as_array)
        .ok_or_else(|| "schema missing artifact_manifest required fields".to_owned())?;
    for key in [
        "manifest_schema",
        "binary_path",
        "binary_hash",
        "binary_hash_status",
        "source_hash",
        "command_hash",
        "execution_substrate",
        "target_directory",
        "log_path",
        "retention_manifest_path",
        "artifact_manifest_hash",
    ] {
        if !required.iter().any(|value| value == key) {
            return Err(format!(
                "artifact_manifest schema missing required field {key}"
            ));
        }
    }

    let v2_contract = schema
        .pointer("/allOf")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema missing allOf".to_owned())?
        .iter()
        .find(|entry| {
            entry.pointer("/if/properties/fields/properties/manifest_schema/const")
                == Some(&Value::String("ee.test_artifact_manifest.v2".to_owned()))
        })
        .ok_or_else(|| "schema missing artifact_manifest v2 conditional".to_owned())?;
    let v2_required = v2_contract
        .pointer("/then/properties/fields/required")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema missing artifact_manifest v2 required fields".to_owned())?;
    for key in [
        "remote_artifact_attestation",
        "source_commit",
        "git_tree",
        "build_command_hash",
        "effective_input_hash",
        "provenance_hash",
        "archive_hash",
        "verification_hash",
    ] {
        if !v2_required.iter().any(|value| value == key) {
            return Err(format!(
                "artifact_manifest v2 schema missing required field {key}"
            ));
        }
    }
    if v2_contract.pointer(
        "/then/properties/fields/properties/remote_artifact_attestation/properties/schema/const",
    ) != Some(&Value::String(
        "ee.remote_build_artifact_manifest.verification.v1".to_owned(),
    )) {
        return Err("artifact_manifest v2 schema does not bind the attestation schema".to_owned());
    }
    Ok(())
}

#[test]
fn bash_harness_emits_command_artifact_manifest() -> TestResult {
    let tmp = tempdir()?;
    let fake_binary = tmp.path().join("fake-ee");
    let log_path = tmp.path().join("j1.jsonl");
    let target_dir = tmp.path().join("target-dir");
    let retention_manifest = tmp.path().join("retention.json");
    write_fake_binary(&fake_binary)?;

    let script = format!(
        r#"
set -uo pipefail
source "{}/scripts/lib/e2e_logger.sh"
e2e_log_start "artifact_manifest_smoke"
e2e_log_command "{}" --version >/dev/null
e2e_log_end
"#,
        env!("CARGO_MANIFEST_DIR"),
        fake_binary.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("EE_TEST_LOG_PATH", &log_path)
        .env("EE_E2E_KEEP_ARTIFACTS", "1")
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("EPIC_RETENTION_MANIFEST", &retention_manifest)
        .env_remove("EE_REMOTE_ARTIFACT_VERIFICATION_REPORT")
        .output()
        .map_err(|error| format!("failed to run bash harness: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "bash harness failed: status={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let events = read_jsonl(&log_path)?;
    let manifest = events
        .iter()
        .find(|event| event["kind"] == "artifact_manifest")
        .ok_or_else(|| format!("no artifact_manifest event in {events:?}"))?;

    if manifest["schema"] != "ee.test_event.v1" {
        return Err(format!("unexpected schema: {}", manifest["schema"]));
    }
    if field(manifest, "manifest_schema")? != "ee.test_artifact_manifest.v1" {
        return Err("manifest_schema mismatch".to_owned());
    }
    if field(manifest, "phase")? != "command_end" {
        return Err(format!("unexpected phase: {}", field(manifest, "phase")?));
    }
    if field(manifest, "binary_path")? != fake_binary.to_string_lossy() {
        return Err(format!(
            "binary_path mismatch: {}",
            field(manifest, "binary_path")?
        ));
    }
    if field(manifest, "binary_hash_status")? != "available" {
        return Err(format!(
            "expected available binary hash, got {}",
            field(manifest, "binary_hash_status")?
        ));
    }
    assert_hash(field(manifest, "binary_hash")?, "binary_hash")?;
    assert_hash(field(manifest, "source_hash")?, "source_hash")?;
    assert_hash(field(manifest, "command_hash")?, "command_hash")?;
    assert_hash(
        field(manifest, "artifact_manifest_hash")?,
        "artifact_manifest_hash",
    )?;
    if field(manifest, "execution_substrate")? != "local" {
        return Err(format!(
            "unexpected execution substrate: {}",
            field(manifest, "execution_substrate")?
        ));
    }
    if field(manifest, "target_directory")? != target_dir.to_string_lossy() {
        return Err(format!(
            "target_directory mismatch: {}",
            field(manifest, "target_directory")?
        ));
    }
    if field(manifest, "log_path")? != log_path.to_string_lossy() {
        return Err(format!(
            "log_path mismatch: {}",
            field(manifest, "log_path")?
        ));
    }
    if field(manifest, "retention_manifest_path")? != retention_manifest.to_string_lossy() {
        return Err(format!(
            "retention_manifest_path mismatch: {}",
            field(manifest, "retention_manifest_path")?
        ));
    }
    Ok(())
}

#[test]
fn bash_harness_warns_when_manifest_binary_hash_is_missing() -> TestResult {
    let tmp = tempdir()?;
    let log_path = tmp.path().join("j1.jsonl");
    let missing_binary = tmp.path().join("missing-ee");

    let script = format!(
        r#"
set -uo pipefail
source "{}/scripts/lib/e2e_logger.sh"
e2e_log_start "artifact_manifest_missing_binary"
e2e_log_artifact_manifest "manual_probe" "{}" --version
e2e_log_end
"#,
        env!("CARGO_MANIFEST_DIR"),
        missing_binary.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("EE_TEST_LOG_PATH", &log_path)
        .env("EE_E2E_KEEP_ARTIFACTS", "1")
        .env_remove("EE_REMOTE_ARTIFACT_VERIFICATION_REPORT")
        .output()
        .map_err(|error| format!("failed to run bash harness: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "bash harness failed: status={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let events = read_jsonl(&log_path)?;
    let manifest = events
        .iter()
        .find(|event| event["kind"] == "artifact_manifest")
        .ok_or_else(|| format!("no artifact_manifest event in {events:?}"))?;
    if field(manifest, "binary_hash")? != "unavailable" {
        return Err(format!(
            "missing binary hash should be unavailable, got {}",
            field(manifest, "binary_hash")?
        ));
    }
    if field(manifest, "binary_hash_status")? != "not_file" {
        return Err(format!(
            "missing binary should report not_file, got {}",
            field(manifest, "binary_hash_status")?
        ));
    }
    assert_hash(field(manifest, "command_hash")?, "command_hash")?;
    assert_hash(
        field(manifest, "artifact_manifest_hash")?,
        "artifact_manifest_hash",
    )?;
    Ok(())
}

#[test]
fn bash_harness_emits_verified_remote_artifact_manifest_v2() -> TestResult {
    let tmp = tempdir()?;
    let fake_binary = tmp.path().join("fake-ee");
    let verification_path = tmp.path().join("artifact-verification.json");
    let log_path = tmp.path().join("j1.jsonl");
    write_fake_binary(&fake_binary)?;
    let binary_hash = sha256_file(&fake_binary)?;
    let report = remote_artifact_verification_report(&binary_hash);
    fs::write(
        &verification_path,
        serde_json::to_vec_pretty(&report)
            .map_err(|error| format!("failed to encode verification report: {error}"))?,
    )
    .map_err(|error| format!("failed to write verification report: {error}"))?;

    let script = format!(
        r#"
set -uo pipefail
source "{}/scripts/lib/e2e_logger.sh"
e2e_log_start "artifact_manifest_v2"
e2e_log_artifact_manifest "remote_probe" "{}" --version
e2e_log_end
"#,
        env!("CARGO_MANIFEST_DIR"),
        fake_binary.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("EE_TEST_LOG_PATH", &log_path)
        .env("EE_REMOTE_ARTIFACT_VERIFICATION_REPORT", &verification_path)
        .env("EE_TEST_EXECUTION_SUBSTRATE", "remote_artifact")
        .output()
        .map_err(|error| format!("failed to run bash harness: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "bash harness failed: status={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let events = read_jsonl(&log_path)?;
    let manifest = events
        .iter()
        .find(|event| event["kind"] == "artifact_manifest")
        .ok_or_else(|| format!("no artifact_manifest event in {events:?}"))?;
    if field(manifest, "manifest_schema")? != "ee.test_artifact_manifest.v2" {
        return Err("manifest_schema mismatch".to_owned());
    }
    if field(manifest, "binary_hash")? != binary_hash {
        return Err(format!(
            "binary_hash did not come from the verified report: {}",
            field(manifest, "binary_hash")?
        ));
    }
    let git_tree = report["gitTree"]
        .as_str()
        .ok_or_else(|| "test report missing gitTree".to_owned())?;
    if field(manifest, "source_hash")? != format!("git_tree:{git_tree}") {
        return Err(format!(
            "source_hash did not bind the verified tree: {}",
            field(manifest, "source_hash")?
        ));
    }
    let manifest_hash = report["manifestHash"]
        .as_str()
        .ok_or_else(|| "test report missing manifestHash".to_owned())?;
    if field(manifest, "artifact_manifest_hash")? != manifest_hash {
        return Err(format!(
            "artifact_manifest_hash did not come from the verified report: {}",
            field(manifest, "artifact_manifest_hash")?
        ));
    }
    for (event_key, report_key) in [
        ("source_commit", "sourceCommit"),
        ("git_tree", "gitTree"),
        ("build_command_hash", "buildCommandHash"),
        ("effective_input_hash", "effectiveInputHash"),
        ("provenance_hash", "provenanceHash"),
        ("archive_hash", "archiveHash"),
        ("verification_hash", "verificationHash"),
    ] {
        let expected = report[report_key]
            .as_str()
            .ok_or_else(|| format!("test report missing {report_key}"))?;
        if field(manifest, event_key)? != expected {
            return Err(format!(
                "fields.{event_key} did not mirror report.{report_key}"
            ));
        }
    }
    if manifest.pointer("/fields/remote_artifact_attestation") != Some(&report) {
        return Err("nested remote artifact attestation was not preserved".to_owned());
    }
    Ok(())
}

#[test]
fn bash_harness_refuses_v2_when_exercised_binary_does_not_match_report() -> TestResult {
    let tmp = tempdir()?;
    let fake_binary = tmp.path().join("fake-ee");
    let verification_path = tmp.path().join("artifact-verification.json");
    let log_path = tmp.path().join("j1.jsonl");
    write_fake_binary(&fake_binary)?;
    let report = remote_artifact_verification_report(&repeated_hash('f'));
    fs::write(
        &verification_path,
        serde_json::to_vec_pretty(&report)
            .map_err(|error| format!("failed to encode verification report: {error}"))?,
    )
    .map_err(|error| format!("failed to write verification report: {error}"))?;

    let script = format!(
        r#"
set -uo pipefail
source "{}/scripts/lib/e2e_logger.sh"
e2e_log_start "artifact_manifest_v2_mismatch"
if e2e_log_artifact_manifest "remote_probe" "{}" --version; then
    exit 70
fi
e2e_log_end
"#,
        env!("CARGO_MANIFEST_DIR"),
        fake_binary.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("EE_TEST_LOG_PATH", &log_path)
        .env("EE_REMOTE_ARTIFACT_VERIFICATION_REPORT", &verification_path)
        .output()
        .map_err(|error| format!("failed to run bash harness: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "bash harness failed: status={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.contains("exercised binary hash does not match") {
        return Err(format!("missing binary mismatch diagnostic: {stderr}"));
    }
    let events = read_jsonl(&log_path)?;
    if events
        .iter()
        .any(|event| event["kind"] == "artifact_manifest")
    {
        return Err("mismatched binary emitted an artifact_manifest event".to_owned());
    }
    Ok(())
}

#[test]
fn artifact_manifest_hashing_avoids_temp_file_cleanup_path() -> TestResult {
    let source = read_repo_file("scripts/lib/e2e_logger.sh")?;
    let hash_string_start = source
        .find("_e2e_hash_string()")
        .ok_or_else(|| "missing _e2e_hash_string".to_owned())?;
    let emit_start = source
        .find("# Emit a single JSON-line event")
        .ok_or_else(|| "missing emit-event marker".to_owned())?;
    let hash_string_body = &source[hash_string_start..emit_start];
    if hash_string_body.contains("mktemp") || hash_string_body.contains("rm -f") {
        return Err("_e2e_hash_string must hash from stdin without temp cleanup".to_owned());
    }
    Ok(())
}
