//! M4 contract test (eidetic_engine_cli bd-17c65.13.5).
//!
//! Asserts the stale-snapshot detection surface added in M4:
//!
//! - `ee handoff resume` always emits `stale_snapshot` in the report
//!   when the capsule embeds a `workspace_state_hash` (which it does
//!   after M3 / bd-1xkxi).
//! - When the workspace hasn't changed between capture and resume,
//!   `stale_snapshot.drift_detected` is `false` and no
//!   `handoff_snapshot_stale` degradation fires.
//! - When the workspace memory set changes between capture and
//!   resume, `drift_detected` flips to `true` and the degradation
//!   fires with severity that signals the agent to refresh.
//! - With `require_fresh: true` set, drift converts the resume into
//!   a `UnsatisfiedDegradedMode` error (exit code 6) so callers in
//!   strict mode never silently consume stale context.
//! - Per-signal counts (`memories_added_since`,
//!   `memories_expired_since`, `memories_revised_since`) are measured
//!   from the capsule's embedded memory snapshot.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use chrono::Utc;
use ee::config::ConfigFile;
use ee::core::handoff::{
    CapsuleProfile, CreateOptions, HANDOFF_SNAPSHOT_STALE_CODE, HandoffStaleThresholds,
    ResumeOptions, create_handoff, resume_handoff,
};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::{ApplyMemoryCurationInput, DbConnection};
use ee::output::render_handoff_resume_json;
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

struct CapsuleFixture {
    _dir: TempDir,
    workspace: PathBuf,
    capsule_path: PathBuf,
    seed_memory_id: String,
}

fn base_remember_options<'a>(workspace: &'a Path, database: &'a Path) -> RememberMemoryOptions<'a> {
    RememberMemoryOptions {
        workspace_path: workspace,
        database_path: Some(database),
        content: "",
        workflow_id: None,
        level: "semantic",
        kind: "fact",
        tags: None,
        confidence: 0.85,
        source: None,
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: true,
        propose_candidates: false,
    }
}

fn build_capsule_with_seeds() -> Result<CapsuleFixture, String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = dir.path().to_path_buf();
    let db_path = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    drop(conn);

    let mut opts = base_remember_options(&workspace, &db_path);
    opts.content = "Always run cargo fmt --check before tagging a release.";
    opts.level = "procedural";
    opts.kind = "rule";
    opts.tags = Some("release");
    let seed = remember_memory(&opts).map_err(|error| format!("remember seed: {error:?}"))?;

    let capsule_path = workspace.join("out").join("capsule.json");
    std::fs::create_dir_all(capsule_path.parent().expect("out parent"))
        .map_err(|error| format!("mkdir out: {error}"))?;
    create_handoff(&CreateOptions {
        workspace: workspace.clone(),
        output: capsule_path.clone(),
        profile: CapsuleProfile::Resume,
        since: None,
        dry_run: false,
        task_frame_id: None,
        bind_to_machine: false,
        machine_salt_path: None,
    })
    .map_err(|error| format!("create_handoff: {error:?}"))?;

    Ok(CapsuleFixture {
        _dir: dir,
        workspace,
        capsule_path,
        seed_memory_id: seed.memory_id.to_string(),
    })
}

fn base_resume_options(fixture: &CapsuleFixture) -> ResumeOptions {
    ResumeOptions {
        path: fixture.capsule_path.clone(),
        use_latest: false,
        workspace: fixture.workspace.clone(),
        max_sections: None,
        task_frame_id: None,
        bound_workspace_id: None,
        bound_workspace_identity: None,
        include_prompt_fragment: false,
        require_fresh: false,
        insecure_skip_hmac: false,
        machine_salt_path: None,
    }
}

fn remember_extra_memory(fixture: &CapsuleFixture, ordinal: usize) -> TestResult {
    let db_path = fixture.workspace.join(".ee").join("ee.db");
    let mut opts = base_remember_options(&fixture.workspace, &db_path);
    let content = format!(
        "Newly seeded handoff stale snapshot memory {ordinal} that perturbs workspace state."
    );
    opts.content = &content;
    opts.level = "episodic";
    opts.kind = "fact";
    opts.tags = Some("bench,perturbation");
    remember_memory(&opts)
        .map(|_| ())
        .map_err(|error| format!("remember extra {ordinal}: {error:?}"))
}

fn stale_snapshot_json(report: &ee::core::handoff::ResumeReport) -> Result<Value, String> {
    let stale = report
        .stale_snapshot
        .as_ref()
        .ok_or_else(|| "stale_snapshot missing".to_string())?;
    serde_json::to_value(stale).map_err(|error| format!("serialize stale_snapshot: {error}"))
}

fn normalized_stale_snapshot_json(
    report: &ee::core::handoff::ResumeReport,
) -> Result<Value, String> {
    let mut value = stale_snapshot_json(report)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "stale_snapshot should serialize as object".to_string())?;
    object.insert(
        "computed_at".to_string(),
        Value::String("TIMESTAMP".to_string()),
    );
    object.insert(
        "captured_state_hash".to_string(),
        Value::String("HASH_CAPTURED".to_string()),
    );
    object.insert(
        "current_state_hash".to_string(),
        Value::String("HASH_CURRENT".to_string()),
    );
    Ok(value)
}

fn resume_json_without_stale_snapshot(
    report: &ee::core::handoff::ResumeReport,
) -> Result<String, String> {
    let mut value: Value = serde_json::from_str(&render_handoff_resume_json(report))
        .map_err(|error| format!("rendered resume JSON must parse: {error}"))?;
    value
        .as_object_mut()
        .ok_or_else(|| "resume JSON should be an object".to_string())?
        .remove("stale_snapshot");
    serde_json::to_string(&value).map_err(|error| format!("serialize baseline JSON: {error}"))
}

#[test]
fn stale_snapshot_is_present_and_clean_for_unchanged_workspace() -> TestResult {
    let fixture = build_capsule_with_seeds()?;
    let report = resume_handoff(&base_resume_options(&fixture))
        .map_err(|error| format!("resume_handoff: {error:?}"))?;

    let stale = report
        .stale_snapshot
        .as_ref()
        .ok_or_else(|| "stale_snapshot must be present after M3+M4 land".to_string())?;
    if stale.captured_state_hash.is_none() {
        return Err("captured_state_hash must populate from the capsule body".to_string());
    }
    if stale.current_state_hash.is_none() {
        return Err("current_state_hash must populate from the live database".to_string());
    }
    if stale.drift_detected {
        return Err(format!(
            "unchanged workspace should not show drift; captured={:?} current={:?}",
            stale.captured_state_hash, stale.current_state_hash
        ));
    }
    if stale.memories_added_since != Some(0) {
        return Err(format!(
            "clean resume should measure zero added memories, got {:?}",
            stale.memories_added_since
        ));
    }
    if stale.memories_expired_since != Some(0) {
        return Err(format!(
            "clean resume should measure zero expired memories, got {:?}",
            stale.memories_expired_since
        ));
    }
    if stale.memories_revised_since != Some(0) {
        return Err(format!(
            "clean resume should measure zero revised memories, got {:?}",
            stale.memories_revised_since
        ));
    }
    if stale.content_drift_score != Some(0.0) {
        return Err(format!(
            "clean resume should have zero content drift score, got {:?}",
            stale.content_drift_score
        ));
    }
    if stale.computed_at.trim().is_empty() {
        return Err("stale_snapshot computed_at must be populated".to_string());
    }
    if report
        .degradations
        .iter()
        .any(|d| d.code.as_str() == "handoff_snapshot_stale")
    {
        return Err("handoff_snapshot_stale must not fire when nothing changed".to_string());
    }
    let rendered: serde_json::Value = serde_json::from_str(&render_handoff_resume_json(&report))
        .map_err(|error| format!("rendered resume JSON must parse: {error}"))?;
    if rendered
        .get("stale_snapshot")
        .and_then(|snapshot| snapshot.get("memories_added_since"))
        .and_then(serde_json::Value::as_u64)
        != Some(0)
    {
        return Err(format!(
            "rendered resume JSON must expose measured stale_snapshot; got {rendered}"
        ));
    }
    Ok(())
}

#[test]
fn stale_snapshot_subtree_matches_golden() -> TestResult {
    let fixture = build_capsule_with_seeds()?;
    remember_extra_memory(&fixture, 1)?;
    let report = resume_handoff(&base_resume_options(&fixture))
        .map_err(|error| format!("resume_handoff: {error:?}"))?;
    let normalized = normalized_stale_snapshot_json(&report)?;

    insta::assert_json_snapshot!("handoff_resume_stale_snapshot", normalized);
    Ok(())
}

#[test]
fn handoff_resume_stale_snapshot_size_budget_is_bounded() -> TestResult {
    let fixture = build_capsule_with_seeds()?;
    let mut options = base_resume_options(&fixture);
    options.include_prompt_fragment = true;
    let report = resume_handoff(&options).map_err(|error| format!("resume_handoff: {error:?}"))?;
    let with_stale = render_handoff_resume_json(&report).len();
    let without_stale = resume_json_without_stale_snapshot(&report)?.len();
    let overhead = with_stale.saturating_sub(without_stale);
    let budget = without_stale.div_ceil(12);
    if overhead > budget {
        return Err(format!(
            "stale_snapshot JSON overhead should stay within 8.34% budget; \
             with={with_stale} without={without_stale} overhead={overhead} budget={budget}"
        ));
    }
    Ok(())
}

#[test]
fn stale_snapshot_flips_to_drift_when_memory_set_changes() -> TestResult {
    let fixture = build_capsule_with_seeds()?;

    // Mutate the workspace between capture and resume by adding a new
    // memory. With a one-memory captured set this also crosses the
    // default content_drift_score threshold.
    remember_extra_memory(&fixture, 1)?;

    let report = resume_handoff(&base_resume_options(&fixture))
        .map_err(|error| format!("resume_handoff: {error:?}"))?;
    let stale = report
        .stale_snapshot
        .as_ref()
        .ok_or_else(|| "stale_snapshot missing after drift".to_string())?;
    if !stale.drift_detected {
        return Err(format!(
            "drift must be detected after adding a memory; captured={:?} current={:?}",
            stale.captured_state_hash, stale.current_state_hash
        ));
    }
    if stale.memories_added_since != Some(1) {
        return Err(format!(
            "added memory count should be 1 after perturbation, got {:?}",
            stale.memories_added_since
        ));
    }
    if stale
        .content_drift_score
        .is_none_or(|score| score <= 0.0 || score > 1.0)
    {
        return Err(format!(
            "content drift score should be in (0, 1], got {:?}",
            stale.content_drift_score
        ));
    }
    let degradation = report
        .degradations
        .iter()
        .find(|d| d.code.as_str() == HANDOFF_SNAPSHOT_STALE_CODE);
    if degradation.is_none() {
        return Err(format!(
            "handoff_snapshot_stale must fire when drift crosses default thresholds; got degradations={:?}",
            report
                .degradations
                .iter()
                .map(|d| d.code.as_str())
                .collect::<Vec<_>>()
        ));
    }
    if degradation.and_then(|d| d.severity.as_deref()) != Some("medium") {
        return Err(format!(
            "default drift breach should have medium severity, got {:?}",
            report.degradations
        ));
    }
    Ok(())
}

#[test]
fn stale_threshold_defaults_and_config_override_gate_degradation() -> TestResult {
    let defaults = HandoffStaleThresholds::default();
    if defaults.memories_added != 20
        || !defaults.any_expired_in_pack
        || defaults.content_drift_score != 0.15
        || defaults.memories_revised != 0
    {
        return Err(format!("unexpected stale threshold defaults: {defaults:?}"));
    }

    let parsed = ConfigFile::parse(
        r#"
[handoff.stale_threshold]
memories_added = 0
any_expired_in_pack = true
content_drift_score = 1.0
memories_revised = 0
"#,
    )
    .map_err(|error| format!("handoff config should parse: {error}"))?;
    let configured = HandoffStaleThresholds::from_config(&parsed.handoff.stale_threshold);
    if configured.memories_added != 0 || configured.content_drift_score != 1.0 {
        return Err(format!(
            "handoff config override did not apply: {configured:?}"
        ));
    }

    let fixture = build_capsule_with_seeds()?;
    let config_path = fixture.workspace.join(".ee").join("config.toml");
    std::fs::write(
        &config_path,
        "[handoff.stale_threshold]\nmemories_added = 0\ncontent_drift_score = 1.0\n",
    )
    .map_err(|error| format!("write config: {error}"))?;
    remember_extra_memory(&fixture, 1)?;

    let report = resume_handoff(&base_resume_options(&fixture))
        .map_err(|error| format!("resume_handoff: {error:?}"))?;
    let degradation = report
        .degradations
        .iter()
        .find(|d| d.code.as_str() == HANDOFF_SNAPSHOT_STALE_CODE)
        .ok_or_else(|| "config override should make one added memory breach".to_string())?;
    if degradation.severity.as_deref() != Some("medium") {
        return Err(format!(
            "added-memory breach should have medium severity, got {:?}",
            degradation.severity
        ));
    }
    Ok(())
}

#[test]
fn require_fresh_converts_drift_into_error() -> TestResult {
    let fixture = build_capsule_with_seeds()?;
    let db_path = fixture.workspace.join(".ee").join("ee.db");
    let mut opts = base_remember_options(&fixture.workspace, &db_path);
    opts.content = "Drift-trigger memory under require_fresh mode.";
    opts.tags = Some("strict");
    remember_memory(&opts).map_err(|error| format!("remember new: {error:?}"))?;

    let mut options = base_resume_options(&fixture);
    options.require_fresh = true;

    let result = resume_handoff(&options);
    match result {
        Ok(report) => Err(format!(
            "require_fresh=true on a drifted capsule must error, not return Ok; \
             drift_detected={:?}",
            report.stale_snapshot.as_ref().map(|s| s.drift_detected)
        )),
        Err(error) => {
            // The error must signal degraded-but-could-not-satisfy
            // semantics (AGENTS.md exit code 6). Check the code path
            // by sniffing the variant via Debug — coupling to the
            // exact variant name is acceptable here because changing
            // the variant is a public API change that should break
            // this test.
            let debug = format!("{error:?}");
            if !debug.contains("UnsatisfiedDegradedMode") {
                return Err(format!(
                    "require_fresh drift must map to UnsatisfiedDegradedMode (exit 6); got {debug}"
                ));
            }
            Ok(())
        }
    }
}

#[test]
fn require_fresh_does_not_error_on_clean_resume() -> TestResult {
    // Symmetry check: when there's no drift, require_fresh=true must
    // still succeed. Without this, callers in strict mode could not
    // ever consume a capsule, defeating the purpose.
    let fixture = build_capsule_with_seeds()?;
    let mut options = base_resume_options(&fixture);
    options.require_fresh = true;
    let report = resume_handoff(&options).map_err(|error| {
        format!("require_fresh=true on a clean resume must succeed; got error: {error:?}")
    })?;
    if let Some(stale) = &report.stale_snapshot {
        if stale.drift_detected {
            return Err("clean resume must not show drift".to_string());
        }
    }
    Ok(())
}

#[test]
fn stale_snapshot_counts_expired_captured_memory() -> TestResult {
    let fixture = build_capsule_with_seeds()?;
    let db_path = fixture.workspace.join(".ee").join("ee.db");
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    let expired_at = Utc::now().to_rfc3339();
    conn.expire_memory_valid_to(&fixture.seed_memory_id, &expired_at)
        .map_err(|error| format!("expire memory: {error}"))?;

    let report = resume_handoff(&base_resume_options(&fixture))
        .map_err(|error| format!("resume_handoff: {error:?}"))?;
    let stale = report
        .stale_snapshot
        .as_ref()
        .ok_or_else(|| "stale_snapshot missing".to_string())?;
    if !stale.drift_detected {
        return Err("expired captured memory should trigger drift".to_string());
    }
    if stale.memories_expired_since != Some(1) {
        return Err(format!(
            "expired memory count should be 1, got {:?}",
            stale.memories_expired_since
        ));
    }
    if stale.memories_added_since != Some(0) {
        return Err(format!(
            "expiring an existing memory should not count as added, got {:?}",
            stale.memories_added_since
        ));
    }
    if stale.memories_revised_since != Some(0) {
        return Err(format!(
            "validity expiration should not count as content revision, got {:?}",
            stale.memories_revised_since
        ));
    }
    if stale.repair_hints.is_empty() {
        return Err("drifted stale_snapshot should include repair hints".to_string());
    }
    let degradation = report
        .degradations
        .iter()
        .find(|d| d.code.as_str() == HANDOFF_SNAPSHOT_STALE_CODE)
        .ok_or_else(|| "expired captured memory should breach stale threshold".to_string())?;
    if degradation.severity.as_deref() != Some("high") {
        return Err(format!(
            "expired captured memory should have high severity, got {:?}",
            degradation.severity
        ));
    }
    Ok(())
}

#[test]
fn stale_snapshot_counts_revised_captured_memory() -> TestResult {
    let fixture = build_capsule_with_seeds()?;
    let db_path = fixture.workspace.join(".ee").join("ee.db");
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    let original = conn
        .get_memory(&fixture.seed_memory_id)
        .map_err(|error| format!("load seed memory: {error}"))?
        .ok_or_else(|| "seed memory should exist".to_string())?;
    conn.apply_memory_curation_update(
        &fixture.seed_memory_id,
        &ApplyMemoryCurationInput {
            workspace_id: original.workspace_id,
            content: "Always run cargo fmt --check and cargo clippy before tagging a release."
                .to_string(),
            confidence: original.confidence,
            trust_class: original.trust_class,
        },
    )
    .map_err(|error| format!("revise seed memory content: {error}"))?;

    let report = resume_handoff(&base_resume_options(&fixture))
        .map_err(|error| format!("resume_handoff: {error:?}"))?;
    let stale = report
        .stale_snapshot
        .as_ref()
        .ok_or_else(|| "stale_snapshot missing".to_string())?;
    if !stale.drift_detected {
        return Err("revised captured memory should trigger drift".to_string());
    }
    if stale.memories_revised_since != Some(1) {
        return Err(format!(
            "revised memory count should be 1, got {:?}",
            stale.memories_revised_since
        ));
    }
    if stale.memories_added_since != Some(0) || stale.memories_expired_since != Some(0) {
        return Err(format!(
            "revision should not count as add/expire, got added={:?} expired={:?}",
            stale.memories_added_since, stale.memories_expired_since
        ));
    }
    let degradation = report
        .degradations
        .iter()
        .find(|d| d.code.as_str() == HANDOFF_SNAPSHOT_STALE_CODE)
        .ok_or_else(|| "revised captured memory should breach stale threshold".to_string())?;
    if degradation.severity.as_deref() != Some("low") {
        return Err(format!(
            "revised captured memory should have low severity, got {:?}",
            degradation.severity
        ));
    }
    Ok(())
}
