#![allow(clippy::unwrap_used, clippy::expect_used)] // test code may unwrap/expect
use std::error::Error;
use std::path::Path;

use ee::core::memory_debt::{
    MEMORY_DEBT_AUDIT_WINDOW_PARTIAL_CODE, MemoryDebtDoctorOptions, MemoryDebtSnapshotOptions,
    run_memory_debt_doctor, run_memory_debt_snapshot,
};
use ee::db::{
    CreateAuditInput, CreateMemoryInput, CreateWorkspaceInput, DbConnection, generate_audit_id,
};
use ee::models::WorkspaceId;

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn seed_workspace(
    workspace: &Path,
    db_path: &Path,
) -> Result<(DbConnection, String), Box<dyn Error>> {
    let connection = DbConnection::open_file(db_path)?;
    connection.migrate()?;
    let canonical = workspace.canonicalize()?;
    let workspace_id = stable_workspace_id(&canonical);
    connection.insert_workspace(
        &workspace_id,
        &CreateWorkspaceInput {
            path: canonical.display().to_string(),
            name: Some("memory-debt-test".to_owned()),
        },
    )?;
    Ok((connection, workspace_id))
}

fn insert_memory(
    connection: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
    content: &str,
    confidence: f32,
    utility: f32,
) -> Result<(), Box<dyn Error>> {
    connection.insert_memory(
        memory_id,
        &CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence,
            utility,
            importance: 0.8,
            provenance_uri: Some("test://memory-debt".to_owned()),
            // Valid per the memories.trust_class CHECK constraint
            // (human_explicit|peer_human_attested|agent_validated|agent_assertion|
            // cass_evidence|legacy_import); the prior "untrusted" value was rejected at INSERT,
            // leaving this whole test file born-red. Orphan-debt detection keys
            // off link_count, not trust_class, so this does not change classes.
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: Some("2026-01-01T00:00:00Z".to_owned()),
            valid_to: None,
        },
    )?;
    Ok(())
}

#[test]
fn curate_doctor_total_score_reflects_full_debt_not_truncated_bd_3qagn()
-> Result<(), Box<dyn Error>> {
    // bd-3qagn: with --limit truncation, summary.totalScore must reflect the
    // FULL debt set (consistent with itemCount/classCounts), not just the
    // returned rows. Seed two orphan-debt memories and request only one.
    let temp = tempfile::tempdir()?;
    let db_path = temp.path().join("ee.db");
    let (connection, workspace_id) = seed_workspace(temp.path(), &db_path)?;
    insert_memory(
        &connection,
        &workspace_id,
        "mem_00000000000000000000000001",
        "first orphaned persisted memory for debt doctor",
        0.2,
        0.9,
    )?;
    insert_memory(
        &connection,
        &workspace_id,
        "mem_00000000000000000000000002",
        "second orphaned persisted memory for debt doctor",
        0.4,
        0.9,
    )?;
    connection.close()?;

    let mut options = MemoryDebtDoctorOptions::new(temp.path());
    options.database_path = Some(&db_path);
    options.class_filter = Some("orphan");
    options.limit = 1;
    options.now_rfc3339 = Some("2026-06-15T00:00:00Z");
    let report = run_memory_debt_doctor(&options)?;
    let data = report.data_json();

    assert_eq!(
        data["summary"]["itemCount"], 2,
        "itemCount is the full debt set"
    );
    assert_eq!(
        data["summary"]["returnedCount"], 1,
        "returnedCount is truncated"
    );
    assert_eq!(data["summary"]["truncated"], true);

    let total_score = data["summary"]["totalScore"]
        .as_f64()
        .expect("totalScore is a number");
    let returned_score = data["queue"][0]["score"]
        .as_f64()
        .expect("returned queue item has a score");
    assert!(
        total_score > returned_score,
        "totalScore ({total_score}) must include the truncated-away item, not just the returned score ({returned_score})"
    );
    Ok(())
}

#[test]
fn curate_doctor_reports_orphan_and_partial_audit_window() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let db_path = temp.path().join("ee.db");
    let (connection, workspace_id) = seed_workspace(temp.path(), &db_path)?;
    let memory_id = "mem_00000000000000000000000001";
    insert_memory(
        &connection,
        &workspace_id,
        memory_id,
        "orphaned persisted memory for debt doctor",
        0.4,
        0.9,
    )?;
    connection.insert_audit(
        &generate_audit_id(),
        &CreateAuditInput {
            workspace_id: Some(workspace_id.clone()),
            actor: Some("test".to_owned()),
            action: "memory.show".to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some(memory_id.to_owned()),
            details: Some("{\"schema\":\"test.audit.v1\"}".to_owned()),
        },
    )?;
    connection.close()?;

    let mut options = MemoryDebtDoctorOptions::new(temp.path());
    options.database_path = Some(&db_path);
    options.class_filter = Some("orphan");
    options.limit = 10;
    options.now_rfc3339 = Some("2026-06-15T00:00:00Z");
    options.audit_scan_limit = Some(1);
    let report = run_memory_debt_doctor(&options)?;
    let data = report.data_json();

    assert_eq!(data["schema"], "ee.curate.doctor.v1");
    assert_eq!(data["summary"]["itemCount"], 1);
    assert_eq!(data["queue"][0]["memoryId"], memory_id);
    assert_eq!(data["queue"][0]["class"], "orphan");
    assert_eq!(
        data["degraded"][0]["code"],
        MEMORY_DEBT_AUDIT_WINDOW_PARTIAL_CODE
    );
    assert_eq!(
        data["queue"][0]["suggestedAction"]["classifier"],
        "Actionable"
    );
    Ok(())
}

#[test]
fn memory_debt_snapshot_is_idempotent_and_trend_renders() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let db_path = temp.path().join("ee.db");
    let (connection, workspace_id) = seed_workspace(temp.path(), &db_path)?;
    insert_memory(
        &connection,
        &workspace_id,
        "mem_00000000000000000000000002",
        "memory for debt snapshot trend",
        0.5,
        0.8,
    )?;
    connection.close()?;

    let snapshot_options = MemoryDebtSnapshotOptions {
        workspace_path: temp.path(),
        database_path: Some(&db_path),
        now_rfc3339: Some("2026-06-15T00:00:00Z"),
        dry_run: false,
        limit: Some(25),
    };
    let first = run_memory_debt_snapshot(&snapshot_options)?;
    let second = run_memory_debt_snapshot(&snapshot_options)?;
    assert!(first.inserted);
    assert!(!second.inserted);
    assert_eq!(first.snapshot_day, "2026-06-15");
    assert_eq!(first.generation, second.generation);

    let mut doctor_options = MemoryDebtDoctorOptions::new(temp.path());
    doctor_options.database_path = Some(&db_path);
    doctor_options.trend = true;
    doctor_options.now_rfc3339 = Some("2026-06-15T00:00:00Z");
    let report = run_memory_debt_doctor(&doctor_options)?;
    let data = report.data_json();
    assert_eq!(data["trend"]["schema"], "ee.curate.debt_trend.v1");
    assert_eq!(data["trend"]["snapshots"][0]["snapshotDay"], "2026-06-15");
    assert_eq!(
        data["trend"]["snapshots"][0]["reportHash"],
        first.report_hash
    );
    Ok(())
}

/// Seed `count` orphan-debt memories and return the reported `summary.totalScore`.
fn orphan_debt_total_score(temp: &Path, count: u32) -> Result<f64, Box<dyn Error>> {
    let db_path = temp.join("ee.db");
    let (connection, workspace_id) = seed_workspace(temp, &db_path)?;
    for index in 1..=count {
        insert_memory(
            &connection,
            &workspace_id,
            &format!("mem_{index:026}"),
            "orphaned persisted memory for debt saturation coverage",
            0.2,
            0.9,
        )?;
    }
    connection.close()?;

    let mut options = MemoryDebtDoctorOptions::new(temp);
    options.database_path = Some(&db_path);
    options.class_filter = Some("orphan");
    options.limit = 50;
    options.now_rfc3339 = Some("2026-06-15T00:00:00Z");
    let report = run_memory_debt_doctor(&options)?;
    let data = report.data_json();
    assert_eq!(
        data["summary"]["itemCount"], count,
        "every seeded memory must land in the orphan debt queue"
    );
    Ok(data["summary"]["totalScore"]
        .as_f64()
        .expect("totalScore is a number"))
}

/// `summary.totalScore` is a SUM over the debt queue, so it must be able to
/// exceed 1.0 and must keep responding as debt grows.
///
/// REGRESSION GUARD, not a refactor test. Until 2026-10-13 the sum was passed
/// through the PER-ITEM rounding helper, which clamps to [0.0, 1.0]. Per-item
/// orphan scores here are 0.572 each (base 0.32, plus utility 0.9 * 0.12,
/// importance 0.8 * 0.10, and (1 - confidence 0.2) * 0.08), so five of them sum
/// to about 2.86 and the clamp reported exactly 1.0 instead. On any corpus with
/// three or more debt items the gauge was pinned, which is why
/// `ee curate doctor --trend` could not show debt rising or falling.
///
/// The `> 1.0` assertion is the one that discriminates. Note that the older
/// sibling test above CANNOT catch this: it seeds two items and compares the
/// total against a single item's score, a comparison the clamp still satisfies.
/// A test calibrated below the saturation point cannot fail on saturation.
#[test]
fn curate_doctor_total_score_is_not_clamped_at_one_bd_92msh() -> Result<(), Box<dyn Error>> {
    let five_temp = tempfile::tempdir()?;
    let five_total = orphan_debt_total_score(five_temp.path(), 5)?;
    assert!(
        five_total > 1.0,
        "totalScore ({five_total}) must be an unclamped sum; the per-item clamp reported exactly 1.0 for this corpus"
    );

    let one_temp = tempfile::tempdir()?;
    let one_total = orphan_debt_total_score(one_temp.path(), 1)?;
    assert!(
        five_total > one_total,
        "totalScore must keep responding as debt grows: five items ({five_total}) must exceed one ({one_total})"
    );
    Ok(())
}
