#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ee::core::verify::{VerificationRecordOptions, record_verification_evidence};
use ee::core::why::{WhyDegradation, WhyOptions, explain_memory};
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::{VerificationStatus, WorkspaceId, sample_verification_evidence_records};
use serde_json::json;

type TestResult = Result<(), String>;

fn artifact_root() -> PathBuf {
    option_env!("CARGO_TARGET_TMPDIR").map_or_else(
        || std::env::temp_dir().join("ee-verification-ledger-lookup-unit"),
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

fn open_database(database_path: &Path) -> Result<DbConnection, String> {
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    }
    let connection = DbConnection::open_file(database_path)
        .map_err(|error| format!("open {}: {error}", database_path.display()))?;
    connection
        .migrate()
        .map_err(|error| format!("migrate {}: {error}", database_path.display()))?;
    Ok(connection)
}

fn insert_verification_memory(
    connection: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
) -> Result<(), String> {
    connection
        .insert_memory(
            memory_id,
            &CreateMemoryInput {
                workspace_id: workspace_id.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: "Run verification gates before closing beads.".to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.7,
                importance: 0.6,
                provenance_uri: None,
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: None,
                tags: vec!["verification".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| format!("insert memory {memory_id}: {error}"))
}

fn why_projection(
    report: &ee::core::why::WhyReport,
    degradations: &[WhyDegradation],
) -> serde_json::Value {
    json!({
        "command": "why",
        "memoryId": report.memory_id,
        "found": report.found,
        "verificationEvidence": report.verification_evidence,
        "degraded": degradations.iter().map(|degradation| {
            json!({
                "code": degradation.code,
                "severity": degradation.severity,
                "message": degradation.message,
                "repair": degradation.repair,
            })
        }).collect::<Vec<_>>(),
    })
}

#[test]
fn seeded_verification_ledger_attaches_to_why_projection_golden() -> TestResult {
    let root = unique_dir("verification-ledger-attached")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| format!("mkdir workspace: {error}"))?;
    let database_path = workspace.join(".ee").join("ee.db");
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    }
    let memory_id = "mem_verifyledger00000000000001";
    let evidence = sample_verification_evidence_records()
        .into_iter()
        .find(|record| record.status == VerificationStatus::Passed)
        .ok_or_else(|| "sample pass evidence exists".to_owned())?;

    let record_report = record_verification_evidence(VerificationRecordOptions {
        database_path: &database_path,
        workspace_path: &workspace,
        target_type: "memory",
        target_id: memory_id,
        actor: Some("verification_ledger_lookup_unit"),
        evidence,
    })
    .map_err(|error| error.to_string())?;

    let connection = open_database(&database_path)?;
    insert_verification_memory(&connection, &record_report.workspace_id, memory_id)?;

    let report = explain_memory(&WhyOptions {
        database_path: &database_path,
        memory_id,
        confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
    });

    if !report.found {
        return Err("why should find seeded memory".to_owned());
    }
    if report.verification_evidence.len() != 1 {
        return Err(format!(
            "why should attach one verification record, got {}",
            report.verification_evidence.len()
        ));
    }

    let projection = why_projection(&report, &[]);
    let actual = serde_json::to_string(&projection)
        .map_err(|error| format!("serialize why projection: {error}"))?;
    let golden_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden/why/verification_evidence_attached.json.golden");
    let expected = fs::read_to_string(&golden_path)
        .map_err(|error| format!("read {}: {error}", golden_path.display()))?;
    if actual != expected.trim_end_matches('\n') {
        return Err(format!(
            "why verification golden mismatch\nexpected: {}\nactual:   {}",
            expected.trim_end_matches('\n'),
            actual
        ));
    }
    Ok(())
}

#[test]
fn missing_verification_ledger_row_degrades_for_verification_memory() -> TestResult {
    let root = unique_dir("verification-ledger-missing")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| format!("mkdir workspace: {error}"))?;
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = open_database(&database_path)?;
    let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(71_002)).to_string();
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: Some("workspace".to_owned()),
            },
        )
        .map_err(|error| format!("insert workspace: {error}"))?;

    let memory_id = "mem_verifymissing0000000000001";
    insert_verification_memory(&connection, &workspace_id, memory_id)?;

    let report = explain_memory(&WhyOptions {
        database_path: &database_path,
        memory_id,
        confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
    });

    if !report.verification_evidence.is_empty() {
        return Err("missing ledger row should not synthesize evidence".to_owned());
    }
    if !report
        .degraded
        .iter()
        .any(|degradation| degradation.code == "verification_evidence_not_found")
    {
        return Err(format!(
            "expected verification_evidence_not_found degradation, got {:?}",
            report
                .degraded
                .iter()
                .map(|degradation| degradation.code)
                .collect::<Vec<_>>()
        ));
    }
    Ok(())
}
