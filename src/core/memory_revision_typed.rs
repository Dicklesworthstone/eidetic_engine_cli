//! Preserve first-class typed memory data through immutable revisions.
//!
//! A prose or metadata edit must not turn a structured decision, failure, or
//! rule into an untyped row. Same-kind revisions inherit the validated sidecar;
//! recognized fields explicitly present in replacement prose override it.
//! Changing kind starts a destination-kind projection, never a mismatched copy.
//! Preparation and dry runs are read-only. The transaction rechecks both source
//! projections before writing and persists the sidecar before indexing.

use std::str::FromStr;

use crate::db::{DbConnection, DbError, DbOperation, StoredMemory};
use crate::models::memory::{
    canonicalize_typed_memory_fields_json, extract_typed_memory_fields_json_with_redactor,
    merge_typed_memory_fields_json,
};
use crate::models::{MemoryContent, MemoryKind};

pub(super) struct Prepared {
    source_json: Option<String>,
    projected_json: Option<String>,
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
        let body = content.unwrap_or(&original.content);
        MemoryContent::parse(body).map_err(|error| format!("Invalid revision content: {error}"))?;
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
            projected_json,
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
        {
            return Err(DbError::MalformedRow {
                operation: DbOperation::Execute,
                message: "Original memory changed during revision preparation; retry against current state".to_owned(),
            });
        }
        Ok(())
    }

    pub(super) fn apply(&self, db: &DbConnection, new_id: &str) -> crate::db::Result<()> {
        if let Some(json) = &self.projected_json
            && !db.set_memory_typed_fields_json(new_id, Some(json))?
        {
            return Err(DbError::MalformedRow {
                operation: DbOperation::Execute,
                message: "New revision could not retain typed fields; revision was not written"
                    .to_owned(),
            });
        }
        Ok(())
    }
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
}
