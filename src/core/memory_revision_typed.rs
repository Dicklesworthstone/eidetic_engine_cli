//! Preserve first-class memory data and guards through immutable revisions.
//!
//! A prose or metadata edit must not turn a structured decision, failure, or
//! rule into an untyped row. Same-kind revisions inherit the validated sidecar;
//! recognized fields explicitly present in replacement prose override it.
//! Changing kind starts a destination-kind projection, never a mismatched copy.
//! Preparation and dry runs are read-only. The transaction rechecks both source
//! projections before writing and persists the sidecar before indexing.
//! Sentinel specs follow the revision with freshly bound identities. Results
//! remain evidence about the prior revision: a new revision needs a new check.

use std::path::Path;
use std::str::FromStr;

use crate::db::{
    CreateAuditInput, DbConnection, DbError, DbOperation, StoredMemory, audit_actions,
    generate_audit_id,
};
use crate::models::memory::{
    canonicalize_typed_memory_fields_json, extract_typed_memory_fields_json_with_redactor,
    merge_typed_memory_fields_json,
};
use crate::models::{
    CreateMemorySentinelSpecInput, MemoryContent, MemoryKind, MemorySentinelSpec,
    StoredMemorySentinelSpec,
};

pub(super) struct Prepared {
    source_json: Option<String>,
    source_sentinels: Vec<StoredMemorySentinelSpec>,
    projected_json: Option<String>,
    policy_bypass: Option<super::RememberPolicyBypassReport>,
    workspace_id: String,
    source_id: String,
    pub(super) changed: bool,
}

impl Prepared {
    pub(super) fn prepare(
        db: &DbConnection,
        original: &StoredMemory,
        content: Option<&str>,
        kind: Option<&str>,
    ) -> Result<Self, String> {
        let source_kind = MemoryKind::from_str(&original.kind)
            .map_err(|_| "Original memory kind is invalid; revision was not written".to_owned())?;
        let target_kind = MemoryKind::from_str(kind.unwrap_or(&original.kind))
            .map_err(|_| "Revision memory kind is invalid; revision was not written".to_owned())?;
        let source_json = db.get_memory_typed_fields_json(&original.id).map_err(|_| {
            "Could not read original typed fields; revision was not written".to_owned()
        })?;
        let canonical_source = source_json
            .as_deref()
            .map(|raw| canonicalize_typed_memory_fields_json(&source_kind, raw))
            .transpose()
            .map_err(|_| {
                "Original typed fields are invalid; revision was not written".to_owned()
            })?;
        let source_sentinels = db.list_memory_sentinel_specs(&original.id).map_err(|_| {
            "Could not read original sentinel guards; revision was not written".to_owned()
        })?;
        for spec in &source_sentinels {
            let canonical = MemorySentinelSpec::new(sentinel_input(spec, &original.id))
                .map_err(|_| invalid_sentinel_message().to_owned())?;
            if spec.memory_id != original.id
                || canonical.spec_hash != spec.spec_hash
                || canonical.safety_class != spec.safety_class
            {
                return Err(invalid_sentinel_message().to_owned());
            }
        }
        let body = content.unwrap_or(&original.content);
        MemoryContent::parse(body).map_err(|error| format!("Invalid revision content: {error}"))?;
        // Revisions are another write ingress, not a policy exception. Reuse
        // the same detector and configured allow contract as remember, before
        // either preview or mutation. An unchanged, previously stored body is
        // not newly admitted merely because its tags or confidence changed.
        let policy_bypass = if body != original.content
            && crate::policy::redact_secret_like_content(body).redacted
        {
            let workspace = db
                .get_workspace(&original.workspace_id)
                .map_err(|_| "Could not resolve revision policy workspace".to_owned())?
                .ok_or_else(|| "Revision policy workspace is missing".to_owned())?;
            super::validate_remember_policy(body, Path::new(&workspace.path), false)
                .map_err(|error| error.message())?
        } else {
            None
        };
        let inherited = (source_kind == target_kind)
            .then_some(canonical_source.as_deref())
            .flatten();
        let extracted = if body != original.content || source_kind != target_kind {
            extract_typed_memory_fields_json_with_redactor(&target_kind, body, |text| {
                crate::policy::redact_secret_like_content(text).content
            })
            .map_err(|_| "Revision typed fields are invalid; revision was not written".to_owned())?
        } else {
            None
        };
        // Explicit replacement-body fields win; absent fields are not a request
        // to erase independent structured data supplied with remember --field.
        let projected_json =
            merge_typed_memory_fields_json(&target_kind, inherited, extracted.as_deref()).map_err(
                |_| "Cannot project revision typed fields; revision was not written".to_owned(),
            )?;
        let changed = canonical_source != projected_json;
        Ok(Self {
            source_json,
            source_sentinels,
            projected_json,
            policy_bypass,
            workspace_id: original.workspace_id.clone(),
            source_id: original.id.clone(),
            changed,
        })
    }

    pub(super) fn check_source(
        &self,
        db: &DbConnection,
        original: &StoredMemory,
    ) -> crate::db::Result<()> {
        if db.get_memory(&original.id)?.as_ref() != Some(original)
            || db.get_memory_typed_fields_json(&original.id)? != self.source_json
            || db.list_memory_sentinel_specs(&original.id)? != self.source_sentinels
        {
            return Err(DbError::MalformedRow {
                operation: DbOperation::Execute,
                message: "Original memory changed during revision preparation; retry against current state".to_owned(),
            });
        }
        Ok(())
    }

    pub(super) fn apply(
        &self,
        db: &DbConnection,
        new_id: &str,
        recorded_at: chrono::DateTime<chrono::Utc>,
    ) -> crate::db::Result<()> {
        // Hashes bind the memory ID, so copying a row or its prior pass/fail
        // result would misattribute evidence. Rebuild each immutable guard and
        // let the next explicit sentinel check establish this revision's state.
        for source in &self.source_sentinels {
            let rebound =
                MemorySentinelSpec::new(sentinel_input(source, new_id)).map_err(|_| {
                    DbError::MalformedRow {
                        operation: DbOperation::Execute,
                        message: invalid_sentinel_message().to_owned(),
                    }
                })?;
            db.upsert_memory_sentinel_spec(&rebound)?;
        }
        if let Some(json) = &self.projected_json
            && !db.set_memory_typed_fields_json_at(new_id, Some(json), recorded_at)?
        {
            return Err(DbError::MalformedRow {
                operation: DbOperation::Execute,
                message: "New revision could not retain typed fields; revision was not written"
                    .to_owned(),
            });
        }
        if let Some(bypass) = &self.policy_bypass {
            let audit_id = generate_audit_id();
            let bypass = bypass.clone().with_audit_id(audit_id.clone());
            db.insert_audit(
                &audit_id,
                &CreateAuditInput {
                    workspace_id: Some(self.workspace_id.clone()),
                    actor: Some("ee memory revise".to_owned()),
                    action: audit_actions::POLICY_BYPASS.to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some(new_id.to_owned()),
                    details: Some(
                        serde_json::json!({
                            "schema": "ee.audit.policy_bypass.v1",
                            "command": "ee memory revise",
                            "originalMemoryId": &self.source_id,
                            "policyBypass": super::policy_bypass_audit_json(&bypass),
                        })
                        .to_string(),
                    ),
                },
            )?;
        }
        Ok(())
    }
}

fn sentinel_input(
    source: &StoredMemorySentinelSpec,
    memory_id: &str,
) -> CreateMemorySentinelSpecInput {
    CreateMemorySentinelSpecInput {
        memory_id: memory_id.to_owned(),
        sentinel_kind: source.sentinel_kind,
        polarity: source.polarity,
        target: source.target.clone(),
        expected_predicate: Some(source.expected_predicate.clone()),
        provenance: source.provenance.clone(),
        stale_threshold_seconds: source.stale_threshold_seconds,
    }
}

fn invalid_sentinel_message() -> &'static str {
    "Original sentinel guards are invalid; revision was not written"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::memory::{ReviseMemoryOptions, ReviseReason, revise_memory};
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
    use crate::models::{MemoryId, WorkspaceId};
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use uuid::Uuid;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture(
        kind: &str,
        fields: Value,
    ) -> Result<(tempfile::TempDir, PathBuf, String), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let workspace = temporary.path().canonicalize()?;
        std::fs::create_dir(workspace.join(".ee"))?;
        let path = workspace.join(".ee/ee.db");
        let db = DbConnection::open_file(&path)?;
        db.migrate()?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(0x7654)).to_string();
        let memory_id = MemoryId::from_uuid(Uuid::from_u128(0x9876)).to_string();
        db.insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: None,
            },
        )?;
        db.insert_memory(
            &memory_id,
            &CreateMemoryInput {
                workspace_id,
                level: "semantic".into(),
                kind: kind.into(),
                content: "Retain the structured release choice through later editorial changes."
                    .into(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some("manual://revision-fixture".into()),
                trust_class: "agent_validated".into(),
                trust_subclass: None,
                tags: vec!["typed-revision".into()],
                valid_from: None,
                valid_to: None,
            },
        )?;
        assert!(db.set_memory_typed_fields_json(&memory_id, Some(&fields.to_string()))?);
        db.close()?;
        Ok((temporary, path, memory_id))
    }

    fn options<'a>(path: &'a std::path::Path, id: &'a str) -> ReviseMemoryOptions<'a> {
        ReviseMemoryOptions {
            database_path: path,
            original_memory_id: id,
            content: None,
            level: None,
            kind: None,
            confidence: None,
            tags: None,
            provenance_uri: None,
            reason: ReviseReason::Refinement,
            actor: Some("typed-revision-test"),
            dry_run: false,
        }
    }

    fn fields(db: &DbConnection, id: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let raw = db
            .get_memory_typed_fields_json(id)?
            .ok_or("typed fields missing")?;
        Ok(serde_json::from_str::<Value>(&raw)?["fields"].clone())
    }

    fn seed_sentinels(db: &DbConnection, memory_id: &str) -> TestResult {
        use crate::models::{
            MemorySentinelPolarity, MemorySentinelResult, MemorySentinelResultInput,
            MemorySentinelResultStatus,
        };

        for (raw, polarity) in [
            ("path_exists:Cargo.toml", MemorySentinelPolarity::Gate),
            ("path_exists:ready.marker", MemorySentinelPolarity::Revive),
        ] {
            let spec = MemorySentinelSpec::from_raw(
                memory_id,
                raw,
                polarity,
                None,
                "manual://revision-guard",
                Some(600),
            )?;
            db.upsert_memory_sentinel_spec(&spec)?;
            db.insert_memory_sentinel_result(&MemorySentinelResult::new(
                MemorySentinelResultInput {
                    spec_hash: spec.spec_hash,
                    status: MemorySentinelResultStatus::Pass,
                    checked_at: chrono::Utc::now().to_rfc3339(),
                    evidence_summary: "Observed against the original revision.".to_owned(),
                    stale_threshold_seconds: Some(600),
                },
            )?)?;
        }
        Ok(())
    }

    #[test]
    fn revisions_preserve_gate_and_revival_specs_without_reusing_old_check_results() -> TestResult {
        let (_temp, path, source) = fixture("decision", json!({"chosen":"SQLite"}))?;
        let db = DbConnection::open_file(&path)?;
        seed_sentinels(&db, &source)?;
        let original_specs = db.list_memory_sentinel_specs(&source)?;
        let original_results = db.latest_memory_sentinel_results_for_memory(&source)?;
        let workspace = db
            .get_memory(&source)?
            .ok_or("source missing")?
            .workspace_id;
        db.close()?;
        let mut request = options(&path, &source);
        request.tags = Some(vec!["reviewed".into()]);
        let report = revise_memory(&request);
        assert!(report.success, "{:?}", report.error);
        let first_id = report.new_id.as_deref().ok_or("new revision missing")?;
        let db = DbConnection::open_file(&path)?;
        let first_specs = db.list_memory_sentinel_specs(first_id)?;
        assert_eq!(first_specs.len(), original_specs.len());
        for original in &original_specs {
            let expected = MemorySentinelSpec::new(sentinel_input(original, first_id))?;
            let actual = first_specs
                .iter()
                .find(|spec| spec.spec_hash == expected.spec_hash)
                .ok_or("rebound sentinel missing")?;
            assert_eq!(actual.memory_id, first_id);
            assert_eq!(actual.polarity, original.polarity);
            assert_eq!(actual.target, original.target);
            assert_eq!(actual.expected_predicate, original.expected_predicate);
            assert_eq!(actual.safety_class, original.safety_class);
            assert_eq!(actual.provenance, original.provenance);
            assert_eq!(actual.stale_threshold_seconds, Some(600));
            assert_ne!(actual.spec_hash, original.spec_hash);
            assert!(
                db.latest_memory_sentinel_result(&actual.spec_hash)?
                    .is_none()
            );
        }
        assert_eq!(db.list_memory_sentinel_specs(&source)?, original_specs);
        assert_eq!(
            db.latest_memory_sentinel_results_for_memory(&source)?,
            original_results
        );
        let revival = db.list_current_memory_revival_specs_bounded(
            &workspace,
            &chrono::Utc::now().to_rfc3339(),
            20,
        )?;
        assert_eq!(revival.total_count, 1);
        assert_eq!(revival.specs[0].memory_id, first_id);
        db.close()?;

        let mut second = options(&path, first_id);
        second.content = Some("Chosen: SQLite\nRationale: Retain local storage while guards hold.");
        let report = revise_memory(&second);
        assert!(report.success, "{:?}", report.error);
        let second_id = report.new_id.as_deref().ok_or("second revision missing")?;
        let db = DbConnection::open_file(&path)?;
        let second_specs = db.list_memory_sentinel_specs(second_id)?;
        assert_eq!(second_specs.len(), original_specs.len());
        for original in &original_specs {
            let expected = MemorySentinelSpec::new(sentinel_input(original, second_id))?;
            assert!(
                second_specs
                    .iter()
                    .any(|spec| spec.spec_hash == expected.spec_hash)
            );
        }
        assert_eq!(db.list_memory_sentinel_specs(first_id)?, first_specs);
        assert!(
            db.latest_memory_sentinel_results_for_memory(second_id)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn sentinel_revision_preview_and_failed_commit_preserve_the_complete_original_state()
    -> TestResult {
        use crate::core::memory::{UnchangedRevisionPolicy, revise_memory_with_transaction_hook};

        let (_temp, path, source) = fixture("decision", json!({"chosen":"SQLite"}))?;
        let db = DbConnection::open_file(&path)?;
        seed_sentinels(&db, &source)?;
        let original = db.get_memory(&source)?;
        let specs = db.list_memory_sentinel_specs(&source)?;
        let results = db.latest_memory_sentinel_results_for_memory(&source)?;
        let audits = db.count_table_rows("audit_log")?;
        let jobs = db.count_table_rows("search_index_jobs")?;
        db.close()?;
        let mut request = options(&path, &source);
        request.tags = Some(vec!["reviewed".into()]);
        request.dry_run = true;
        let preview = revise_memory(&request);
        assert!(preview.success && preview.dry_run);
        let db = DbConnection::open_file(&path)?;
        assert_eq!(db.count_memory_chain(&source)?, 1);
        assert_eq!(db.count_table_rows("memory_sentinel_specs")?, 2);
        assert_eq!(db.count_table_rows("audit_log")?, audits);
        assert_eq!(db.count_table_rows("search_index_jobs")?, jobs);
        db.close()?;

        request.dry_run = false;
        let mut attempted_id = None;
        let rejected = revise_memory_with_transaction_hook(
            &request,
            UnchangedRevisionPolicy::Reject,
            |db, context| {
                attempted_id = Some(context.new_id.clone());
                assert_eq!(db.list_memory_sentinel_specs(&context.new_id)?.len(), 2);
                assert!(
                    db.latest_memory_sentinel_results_for_memory(&context.new_id)?
                        .is_empty()
                );
                Err(DbError::MalformedRow {
                    operation: DbOperation::Execute,
                    message: "Planted failure after sentinel inheritance".to_owned(),
                })
            },
        );
        assert!(!rejected.success);
        let db = DbConnection::open_file(&path)?;
        let attempted_id = attempted_id.ok_or("transaction hook never ran")?;
        assert!(db.get_memory(&attempted_id)?.is_none());
        assert!(db.list_memory_sentinel_specs(&attempted_id)?.is_empty());
        assert_eq!(db.get_memory(&source)?, original);
        assert_eq!(db.list_memory_sentinel_specs(&source)?, specs);
        assert_eq!(
            db.latest_memory_sentinel_results_for_memory(&source)?,
            results
        );
        assert!(db.get_memory_superseded_at(&source)?.is_none());
        assert_eq!(db.count_table_rows("audit_log")?, audits);
        assert_eq!(db.count_table_rows("search_index_jobs")?, jobs);
        Ok(())
    }

    #[test]
    fn invalid_or_changed_sentinel_authority_cannot_be_erased_by_revision() -> TestResult {
        use crate::models::MemorySentinelPolarity;

        let (_temp, path, source) = fixture("decision", json!({"chosen":"SQLite"}))?;
        let db = DbConnection::open_file(&path)?;
        let original = db.get_memory(&source)?.ok_or("source missing")?;
        let prepared = Prepared::prepare(&db, &original, Some("Clarify release guidance."), None)?;
        seed_sentinels(&db, &source)?;
        assert!(
            prepared.check_source(&db, &original).is_err(),
            "a newly attached guard must win"
        );
        let prepared = Prepared::prepare(&db, &original, Some("Clarify release guidance."), None)?;
        let additional = MemorySentinelSpec::from_raw(
            &source,
            "path_exists:extra.marker",
            MemorySentinelPolarity::Gate,
            None,
            "manual://revision-guard",
            Some(60),
        )?;
        db.upsert_memory_sentinel_spec(&additional)?;
        assert!(prepared.check_source(&db, &original).is_err());
        db.execute_raw("UPDATE memory_sentinel_specs SET target = 'private-sentinel-target' WHERE polarity = 'gate'")?;
        db.close()?;
        for dry_run in [true, false] {
            let mut request = options(&path, &source);
            request.tags = Some(vec!["reviewed".into()]);
            request.dry_run = dry_run;
            let rejected = revise_memory(&request);
            assert!(!rejected.success);
            let error = rejected.error.ok_or("sentinel validation error missing")?;
            assert!(error.contains("sentinel"));
            assert!(!error.contains("private-sentinel-target"));
        }
        let db = DbConnection::open_file(&path)?;
        assert_eq!(db.count_memory_chain(&source)?, 1);
        assert!(db.get_memory_superseded_at(&source)?.is_none());
        assert_eq!(db.count_table_rows("memory_sentinel_specs")?, 3);
        assert_eq!(db.count_table_rows("memory_sentinel_results")?, 2);
        Ok(())
    }

    #[test]
    fn metadata_revision_keeps_explicit_structured_decision_and_source_immutable() -> TestResult {
        let expected = json!({"chosen":"SQLite", "options":["SQLite","Postgres"], "rationale":"Offline operation", "revisit_by":"2027-01-01T00:00:00.123456789Z"});
        let (_temp, path, source) = fixture("decision", expected.clone())?;
        let db = DbConnection::open_file(&path)?;
        let source_json = db.get_memory_typed_fields_json(&source)?;
        db.close()?;
        let mut request = options(&path, &source);
        request.tags = Some(vec!["reviewed".into()]);
        let report = revise_memory(&request);
        assert!(report.success, "{:?}", report.error);
        let new_id = report.new_id.as_deref().ok_or("new revision missing")?;
        let db = DbConnection::open_file(&path)?;
        assert_eq!(fields(&db, new_id)?, expected);
        assert_eq!(db.get_memory_typed_fields_json(&source)?, source_json);
        assert_eq!(
            db.get_memory_logical_id(new_id)?.as_deref(),
            Some(source.as_str())
        );
        assert!(db.get_memory_superseded_at(&source)?.is_some());
        assert_eq!(db.get_memory_tags(new_id)?, ["reviewed"]);
        let revised = db.get_memory(new_id)?.ok_or("new revision missing")?;
        let cutoff = db
            .get_memory_superseded_at(&source)?
            .ok_or("revision cutoff missing")?;
        let cutoff = chrono::DateTime::parse_from_rfc3339(&cutoff)?;
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(&revised.created_at)?,
            cutoff
        );
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(&revised.updated_at)?,
            cutoff,
            "inherited typed fields must not turn the revision into a later edit"
        );
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(
                revised
                    .valid_from
                    .as_deref()
                    .ok_or("revision start missing")?
            )?,
            cutoff
        );
        Ok(())
    }

    #[test]
    fn replacement_fields_override_inherited_values_without_erasing_unmentioned_data() -> TestResult
    {
        let (_temp, path, source) = fixture(
            "decision",
            json!({"chosen":"SQLite", "options":["SQLite","Postgres"], "rationale":"Offline operation"}),
        )?;
        let mut request = options(&path, &source);
        request.content = Some("Chosen: Postgres\nRationale: Shared writer coordination");
        let report = revise_memory(&request);
        assert!(report.success, "{:?}", report.error);
        assert!(
            report
                .changed_fields
                .iter()
                .any(|field| field == "typed_fields")
        );
        let db = DbConnection::open_file(&path)?;
        let projected = fields(&db, report.new_id.as_deref().ok_or("new revision missing")?)?;
        assert_eq!(projected["chosen"], "Postgres");
        assert_eq!(projected["options"], json!(["SQLite", "Postgres"]));
        assert_eq!(projected["rationale"], "Shared writer coordination");
        assert_eq!(fields(&db, &source)?["chosen"], "SQLite");
        Ok(())
    }

    #[test]
    fn kind_change_projects_destination_fields_instead_of_copying_foreign_shape() -> TestResult {
        let (_temp, path, source) = fixture("decision", json!({"chosen":"SQLite"}))?;
        let mut request = options(&path, &source);
        request.kind = Some("failure");
        request.content = Some("Cause: Missing deployment guard\nFamily: Release regression");
        let report = revise_memory(&request);
        assert!(report.success, "{:?}", report.error);
        let db = DbConnection::open_file(&path)?;
        let projected = fields(&db, report.new_id.as_deref().ok_or("new revision missing")?)?;
        assert_eq!(projected["cause"], "Missing deployment guard");
        assert!(projected.get("chosen").is_none());
        assert_eq!(fields(&db, &source)?["chosen"], "SQLite");
        Ok(())
    }

    #[test]
    fn dry_run_validates_the_same_projection_and_does_not_supersede_source() -> TestResult {
        let (_temp, path, source) = fixture("decision", json!({"chosen":"SQLite"}))?;
        let mut request = options(&path, &source);
        request.content = Some("Chosen: Postgres");
        request.dry_run = true;
        let report = revise_memory(&request);
        assert!(report.success && report.dry_run);
        assert!(
            report
                .changed_fields
                .iter()
                .any(|field| field == "typed_fields")
        );
        let db = DbConnection::open_file(&path)?;
        assert_eq!(db.count_memory_chain(&source)?, 1);
        assert!(db.get_memory_superseded_at(&source)?.is_none());
        assert_eq!(fields(&db, &source)?["chosen"], "SQLite");
        db.close()?;
        request.content = Some("Chosen: Postgres\nRevisit by: invalid-date");
        assert!(!revise_memory(&request).success);
        request.dry_run = false;
        assert!(!revise_memory(&request).success);
        let db = DbConnection::open_file(&path)?;
        assert_eq!(db.count_memory_chain(&source)?, 1);
        assert!(db.get_memory_superseded_at(&source)?.is_none());
        Ok(())
    }

    #[test]
    fn changed_source_sidecar_cannot_be_overwritten_by_a_stale_projection() -> TestResult {
        let (_temp, path, source) = fixture("decision", json!({"chosen":"SQLite"}))?;
        let db = DbConnection::open_file(&path)?;
        let original = db.get_memory(&source)?.ok_or("source missing")?;
        let prepared = Prepared::prepare(&db, &original, Some("Clarify release guidance."), None)?;
        db.set_memory_typed_fields_json(&source, Some(r#"{"chosen":"Postgres"}"#))?;
        assert!(prepared.check_source(&db, &original).is_err());
        assert_eq!(fields(&db, &source)?["chosen"], "Postgres");
        assert_eq!(db.count_memory_chain(&source)?, 1);
        Ok(())
    }

    #[test]
    fn revised_secret_content_is_rejected_in_preview_and_apply_without_leaking() -> TestResult {
        let (_temp, path, source) = fixture("decision", json!({"chosen":"SQLite"}))?;
        let credential = format!("ghp_{}", "Q".repeat(36));
        let body = format!("The credential is {credential}.");
        assert!(crate::policy::redact_secret_like_content(&body).redacted);
        let db = DbConnection::open_file(&path)?;
        let original = db.get_memory(&source)?;
        let audit_count = db.count_table_rows("audit_log")?;
        let jobs = db.count_table_rows("search_index_jobs")?;
        db.close()?;
        for dry_run in [true, false] {
            let mut request = options(&path, &source);
            request.content = Some(&body);
            request.dry_run = dry_run;
            let report = revise_memory(&request);
            assert!(!report.success);
            let error = report.error.as_deref().ok_or("policy error missing")?;
            assert!(error.contains("secrets"));
            assert!(!error.contains(&credential));
        }
        let db = DbConnection::open_file(&path)?;
        assert_eq!(db.get_memory(&source)?, original);
        assert_eq!(db.count_memory_chain(&source)?, 1);
        assert!(db.get_memory_superseded_at(&source)?.is_none());
        assert_eq!(fields(&db, &source)?["chosen"], "SQLite");
        assert_eq!(db.count_table_rows("audit_log")?, audit_count);
        assert_eq!(db.count_table_rows("search_index_jobs")?, jobs);
        Ok(())
    }

    #[test]
    fn configured_revision_exception_is_audited_atomically_and_rollback_removes_it() -> TestResult {
        use crate::core::memory::{UnchangedRevisionPolicy, revise_memory_with_transaction_hook};

        let (_temp, path, source) = fixture("decision", json!({"chosen":"SQLite"}))?;
        let config = path
            .parent()
            .ok_or("config directory missing")?
            .join("config.toml");
        std::fs::write(
            &config,
            "[policy.secret_detector]\nallow_phrases = [\"OAuth refresh token\"]\n",
        )?;
        let body = "OAuth refresh token fixture uses API_KEY=sk-FAKEabc123def456ghi789jkl012 for documentation.";
        assert!(crate::policy::redact_secret_like_content(body).redacted);
        let mut request = options(&path, &source);
        request.content = Some(body);
        let db = DbConnection::open_file(&path)?;
        let audits_before = db.count_table_rows("audit_log")?;
        db.close()?;
        request.dry_run = true;
        assert!(revise_memory(&request).success);
        request.dry_run = false;
        // The hook observes the new revision and its exception audit inside
        // the same transaction, then forces rollback after both were written.
        let failed = revise_memory_with_transaction_hook(
            &request,
            UnchangedRevisionPolicy::Reject,
            |db, context| {
                let audits = db.list_audit_by_target("memory", &context.new_id, None)?;
                assert_eq!(
                    audits
                        .iter()
                        .filter(|row| row.action == audit_actions::POLICY_BYPASS)
                        .count(),
                    1
                );
                assert!(db.get_memory_typed_fields_json(&context.new_id)?.is_some());
                Err(DbError::MalformedRow {
                    operation: DbOperation::Execute,
                    message: "Planted failure after revision policy admission".to_owned(),
                })
            },
        );
        assert!(!failed.success);
        let db = DbConnection::open_file(&path)?;
        assert_eq!(db.count_table_rows("audit_log")?, audits_before);
        assert_eq!(db.count_memory_chain(&source)?, 1);
        assert!(db.get_memory_superseded_at(&source)?.is_none());
        db.close()?;
        let report = revise_memory(&request);
        assert!(report.success, "{:?}", report.error);
        let new_id = report.new_id.as_deref().ok_or("new revision missing")?;
        let db = DbConnection::open_file(&path)?;
        assert_eq!(
            db.get_memory(new_id)?.ok_or("revision missing")?.content,
            body
        );
        assert_eq!(fields(&db, new_id)?["chosen"], "SQLite");
        let audits = db.list_audit_by_target("memory", new_id, None)?;
        let exceptions: Vec<_> = audits
            .iter()
            .filter(|row| row.action == audit_actions::POLICY_BYPASS)
            .collect();
        assert_eq!(exceptions.len(), 1);
        let details: Value = serde_json::from_str(
            exceptions[0]
                .details
                .as_deref()
                .ok_or("exception detail missing")?,
        )?;
        assert_eq!(details["command"], "ee memory revise");
        assert_eq!(details["originalMemoryId"], source);
        assert_eq!(details["policyBypass"]["kind"], "config_phrase");
        assert_eq!(details["policyBypass"]["auditId"], exceptions[0].id);
        db.close()?;
        // No new body is admitted by a metadata edit. Historical authorized
        // content remains editable even after its allow phrase is removed.
        std::fs::write(&config, "[policy.secret_detector]\nallow_phrases = []\n")?;
        let mut metadata = options(&path, new_id);
        metadata.tags = Some(vec!["reviewed".into()]);
        let edited = revise_memory(&metadata);
        assert!(edited.success, "{:?}", edited.error);
        let db = DbConnection::open_file(&path)?;
        let metadata_id = edited
            .new_id
            .as_deref()
            .ok_or("metadata revision missing")?;
        assert_eq!(
            db.get_memory(metadata_id)?
                .ok_or("metadata revision missing")?
                .content,
            body
        );
        assert!(
            !db.list_audit_by_target("memory", metadata_id, None)?
                .iter()
                .any(|row| row.action == audit_actions::POLICY_BYPASS)
        );
        Ok(())
    }
}
