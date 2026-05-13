#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn ee_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn artifact_root() -> PathBuf {
    option_env!("CARGO_TARGET_TMPDIR").map_or_else(
        || std::env::temp_dir().join("ee-verification-ingestion-unit"),
        PathBuf::from,
    )
}

fn unique_dir(name: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before epoch: {error}"))?
        .as_nanos();
    let dir = artifact_root().join(format!("{name}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("mkdir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn run_ee(workspace: &Path, args: &[&str]) -> Result<JsonValue, String> {
    let output = Command::new(ee_bin())
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .args(args)
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!(
            "ee {} failed (exit {:?})\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse JSON from ee {}: {error}", args.join(" ")))
}

fn json_str<'a>(value: &'a JsonValue, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("{context}: missing string at {pointer}"))
}

fn json_bool(value: &JsonValue, pointer: &str, context: &str) -> Result<bool, String> {
    value
        .pointer(pointer)
        .and_then(JsonValue::as_bool)
        .ok_or_else(|| format!("{context}: missing bool at {pointer}"))
}

fn write_evidence(
    root: &Path,
    name: &str,
    record: &ee::models::VerificationEvidenceRecord,
) -> Result<PathBuf, String> {
    let path = root.join(name);
    let json = serde_json::to_string(record)
        .map_err(|error| format!("serialize verification evidence: {error}"))?;
    fs::write(&path, json).map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(path)
}

fn remember_fixture_memory(workspace: &Path) -> Result<String, String> {
    let report = run_ee(
        workspace,
        &[
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--no-propose-candidates",
            "verification ingestion unit memory",
        ],
    )?;
    json_str(&report, "/data/memory_id", "remember report").map(str::to_owned)
}

#[test]
fn verification_ingest_is_idempotent_and_attaches_to_why() -> TestResult {
    let root = unique_dir("verification-ingest")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| format!("mkdir workspace: {error}"))?;
    run_ee(&workspace, &["init"])?;
    let memory_id = remember_fixture_memory(&workspace)?;

    let evidence = ee::models::sample_verification_evidence_records()
        .into_iter()
        .next()
        .ok_or_else(|| "sample verification evidence exists".to_owned())?;
    let evidence_path = write_evidence(&root, "pass-evidence.json", &evidence)?;
    let evidence_path = evidence_path.to_string_lossy().into_owned();

    let ingest = run_ee(
        &workspace,
        &[
            "verification",
            "ingest",
            "--file",
            &evidence_path,
            "--target-type",
            "memory",
            "--target-id",
            &memory_id,
            "--actor",
            "verification_ingestion_unit",
        ],
    )?;
    assert_eq!(
        json_str(&ingest, "/data/command", "ingest")?,
        "verification ingest"
    );
    assert_eq!(
        json_str(&ingest, "/data/targetId", "ingest")?,
        memory_id.as_str()
    );
    assert!(json_bool(&ingest, "/data/persisted", "ingest")?);
    assert!(!json_bool(&ingest, "/data/replayed", "ingest")?);
    assert!(
        json_str(&ingest, "/data/contentHash", "ingest")?.starts_with("blake3:"),
        "ingest should expose a stable content hash"
    );

    let replay = run_ee(
        &workspace,
        &[
            "verification",
            "ingest",
            "--file",
            &evidence_path,
            "--target-type",
            "memory",
            "--target-id",
            &memory_id,
            "--actor",
            "verification_ingestion_unit",
        ],
    )?;
    assert!(!json_bool(&replay, "/data/persisted", "replay")?);
    assert!(json_bool(&replay, "/data/replayed", "replay")?);
    assert_eq!(
        json_str(&replay, "/data/degradations/0", "replay")?,
        "degraded.verification_idempotent_replay"
    );
    assert_eq!(
        json_str(&replay, "/data/auditId", "replay")?,
        json_str(&ingest, "/data/auditId", "ingest")?
    );

    let why = run_ee(&workspace, &["why", &memory_id])?;
    let evidence_id = json_str(
        &why,
        "/data/verificationEvidence/0/verificationId",
        "why verification evidence",
    )?;
    assert_eq!(evidence_id, evidence.verification_id);
    Ok(())
}

#[test]
fn closure_guidance_rejects_ingested_fallback_evidence() -> TestResult {
    let root = unique_dir("verification-closure")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| format!("mkdir workspace: {error}"))?;
    run_ee(&workspace, &["init"])?;
    let memory_id = remember_fixture_memory(&workspace)?;

    let fallback = ee::models::sample_verification_evidence_records()
        .into_iter()
        .find(|record| record.status == ee::models::VerificationStatus::FallbackDetected)
        .ok_or_else(|| "sample fallback evidence exists".to_owned())?;
    let fallback_path = write_evidence(&root, "fallback-evidence.json", &fallback)?;
    let fallback_path = fallback_path.to_string_lossy().into_owned();

    run_ee(
        &workspace,
        &[
            "verification",
            "ingest",
            "--file",
            &fallback_path,
            "--target-type",
            "memory",
            "--target-id",
            &memory_id,
            "--actor",
            "verification_ingestion_unit",
        ],
    )?;

    let guidance = run_ee(
        &workspace,
        &[
            "verification",
            "closure-guidance",
            "--bead-id",
            fallback.bead_id.as_deref().unwrap_or("bd-example"),
            "--require-rch-cargo",
        ],
    )?;
    assert!(!json_bool(
        &guidance,
        "/data/guidance/canClose",
        "closure guidance"
    )?);
    let reasons = guidance
        .pointer("/data/guidance/rejectedReasons")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "closure guidance: missing rejected reasons".to_owned())?;
    assert!(
        reasons.iter().any(|reason| reason
            .as_str()
            .is_some_and(|reason| reason.contains("local fallback"))),
        "closure guidance should explain local fallback rejection: {reasons:?}"
    );
    Ok(())
}
