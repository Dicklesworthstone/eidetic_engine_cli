//! Verify the recovered memory graph against the admitted JSONL projection.
//!
//! Artifact integrity and row counts cannot detect a wrong identity, changed
//! body, moved tag, or miswired edge. Reuse the importer's typed preparation so
//! timestamp normalization, redacted IDs and trust caps have one definition.
//! This is recovery verification, not authentication and not an import retry.

use super::*;
use crate::models::DomainError;

fn mismatch(reason: &'static str) -> DomainError {
    DomainError::Import {
        message: format!(
            "Restored memory graph does not match its verified backup: {reason}; the restored store was not published"
        ),
        repair: Some(
            "Keep the backup and staged restore for inspection; retry from a complete recovery point after repairing the recovery writer."
                .to_owned(),
        ),
    }
}

fn unreadable(_: impl fmt::Display) -> DomainError {
    // Do not echo raw parser diagnostics, SQL, private paths or source content.
    mismatch("record verification could not read the required data")
}

struct ExpectedRecords {
    memories: Vec<PreparedMemory>,
    links: Vec<PreparedLink>,
    legacy_supersession_ids: BTreeSet<String>,
}

impl ExpectedRecords {
    fn from_parsed(
        parsed: &ParsedJsonlImport,
        workspace_id: &str,
        auth: &NativeAuthState,
    ) -> Result<Self, DomainError> {
        let header = parsed
            .header
            .as_ref()
            .ok_or_else(|| mismatch("missing header"))?;
        let footer = parsed
            .footer
            .as_ref()
            .ok_or_else(|| mismatch("missing footer"))?;
        if parsed.has_errors()
            || !footer.success
            || footer.total_records != u64::from(parsed.records_total)
            || footer.memory_count != parsed.memories.len() as u64
            || footer.tag_count != u64::from(parsed.tag_records)
            || footer.link_count != parsed.links.len() as u64
            || header.workspace_id.as_deref()
                != Some(
                    crate::output::jsonl_export::redact_identifier(
                        workspace_id,
                        header.redaction_level,
                    )
                    .as_str(),
                )
            || parsed
                .memories
                .iter()
                .any(|memory| Some(memory.workspace_id.as_str()) != header.workspace_id.as_deref())
        {
            return Err(mismatch(
                "incomplete, malformed or cross-workspace record stream",
            ));
        }
        let validated =
            validate_memories(parsed).map_err(|_| mismatch("invalid memory records"))?;
        let legacy_supersession_ids = revisions::legacy_supersession_ids(&validated);
        let prepared = prepare_memories_with_policy(
            parsed,
            validated,
            workspace_id,
            auth,
            NativeTrustPolicy::VerifiedBackupRestore,
        );
        if prepared.has_errors() {
            return Err(mismatch("memory records failed recovery admission"));
        }
        let links = prepare_links(parsed).map_err(|_| mismatch("invalid link records"))?;
        Ok(Self {
            memories: prepared.memories,
            links,
            legacy_supersession_ids,
        })
    }

    fn verify(&self, connection: &DbConnection, workspace_id: &str) -> Result<(), DomainError> {
        if connection
            .get_workspace(workspace_id)
            .map_err(unreadable)?
            .is_none()
        {
            return Err(mismatch("workspace identity is absent"));
        }
        // Exact IDs below plus exact totals reject substitutions and extras,
        // not merely omissions. Audits are checked by the recovery inventory.
        for (table, expected) in [
            ("memories", self.memories.len()),
            ("memory_links", self.links.len()),
        ] {
            let actual = connection.count_table_rows(table).map_err(unreadable)?;
            if u64::try_from(actual).ok() != Some(expected as u64) {
                return Err(mismatch("memory or link population differs"));
            }
        }
        for expected in &self.memories {
            let actual = connection
                .get_memory(&expected.id)
                .map_err(unreadable)?
                .ok_or_else(|| mismatch("memory identity is absent"))?;
            if reimport_conflict_issue(&actual, expected).is_some()
                || actual.confidence != expected.input.confidence
                || actual.utility != expected.input.utility
                || actual.importance != expected.input.importance
                || actual.trust_subclass != expected.input.trust_subclass
                || actual.provenance_uri != expected.input.provenance_uri
                || actual.tombstoned_at != expected.tombstoned_at
            {
                return Err(mismatch("memory fields differ"));
            }
            if connection
                .get_memory_logical_id(&expected.id)
                .map_err(unreadable)?
                .as_deref()
                != Some(expected.logical_id.as_str())
            {
                return Err(mismatch("revision-family identity differs"));
            }
            // None is an obligation too: an accidentally superseded head is
            // not a faithful restore, even when every row and body survived.
            // Legacy expiry-only rows retain their structural fallback; only
            // that explicitly identified compatibility case is ambiguous.
            if !self.legacy_supersession_ids.contains(&expected.id)
                && connection
                    .get_memory_superseded_at(&expected.id)
                    .map_err(unreadable)?
                    != expected.superseded_at
            {
                return Err(mismatch("revision supersession differs"));
            }
            let tags: BTreeSet<_> = connection
                .get_memory_tags(&expected.id)
                .map_err(unreadable)?
                .into_iter()
                .collect();
            let expected_tags: BTreeSet<_> = expected.input.tags.iter().cloned().collect();
            if tags != expected_tags {
                return Err(mismatch("memory tag ownership differs"));
            }
            if let Some(posterior) = expected.bayes_posterior
                && connection
                    .get_memory_bayes_posterior(&expected.id)
                    .map_err(unreadable)?
                    != Some(posterior)
            {
                return Err(mismatch("Bayesian evidence differs"));
            }
            let family = connection
                .get_memory_attempt_family(&expected.id)
                .map_err(unreadable)?;
            match (family.as_ref(), expected.attempt_family.as_ref()) {
                (None, None) => {}
                (Some(actual), Some(expected))
                    if actual.family_id == expected.family_id
                        && actual.declared_size == expected.declared_size
                        && actual.attempt_index == expected.attempt_index
                        && actual.disposition == expected.disposition =>
                {
                    if let Some(origin) = &expected.origin {
                        let declaration = connection
                            .get_attempt_family_declaration(workspace_id, &expected.family_id)
                            .map_err(unreadable)?;
                        if declaration.as_ref().map(|(_, origin)| origin) != Some(origin) {
                            return Err(mismatch("attempt-family declaration origin differs"));
                        }
                    }
                }
                _ => return Err(mismatch("attempt-family membership differs")),
            }
        }
        for expected in &self.links {
            let actual = connection
                .get_memory_link(&expected.id)
                .map_err(unreadable)?
                .ok_or_else(|| mismatch("link identity is absent"))?;
            if !link_matches(&actual, expected)
                || actual
                    .metadata_json
                    .as_deref()
                    .is_some_and(|text| serde_json::from_str::<JsonValue>(text).is_err())
            {
                return Err(mismatch("link endpoints or evidence fields differ"));
            }
        }
        Ok(())
    }
}

/// Call only after the manifest and copied records have been authenticated and
/// every durable recovery writer has finished, before rebuilding/publishing.
/// This checks the fields represented in JSONL, not every column of every
/// durable table. Row-count reconciliation and typed asset checks remain needed.
pub(crate) fn verify_backup_records(
    database_path: &Path,
    records_path: &Path,
    workspace_path: &Path,
    workspace_id: &str,
) -> Result<(), DomainError> {
    ensure_import_source_path_is_regular_file(records_path).map_err(unreadable)?;
    let source = read_jsonl_source_bounded(records_path).map_err(unreadable)?;
    let parsed = parse_jsonl_source(&source);
    let auth = native_import_auth_state(&parsed, workspace_path, workspace_id);
    let expected = ExpectedRecords::from_parsed(&parsed, workspace_id, &auth)?;
    let connection =
        DbConnection::open(DatabaseConfig::read_only_file(database_path.to_path_buf()))
            .map_err(unreadable)?;
    let result = verify_in_snapshot(&connection, &expected, workspace_id);
    let closed = connection.close().map(|_| ()).map_err(unreadable);
    result.and(closed)
}

fn verify_in_snapshot(
    connection: &DbConnection,
    expected: &ExpectedRecords,
    workspace_id: &str,
) -> Result<(), DomainError> {
    connection.begin_read_snapshot().map_err(unreadable)?;
    let mut snapshot = ReadSnapshot {
        connection,
        active: true,
    };
    expected.verify(connection, workspace_id)?;
    connection.commit_read_snapshot().map_err(unreadable)?;
    snapshot.active = false;
    Ok(())
}

struct ReadSnapshot<'a> {
    connection: &'a DbConnection,
    active: bool,
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.connection.rollback_read_snapshot().is_err() {
            tracing::error!(target: "ee::backup::recovery", "failed to release record verification snapshot");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CreateWorkspaceInput;
    use crate::models::WorkspaceId;

    type TestResult = Result<(), String>;

    fn records() -> Vec<JsonValue> {
        let workspace = WorkspaceId::from_uuid(Uuid::from_u128(31)).to_string();
        let first = MemoryId::from_uuid(Uuid::from_u128(1)).to_string();
        let second = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
        let memory = |id: &str, text: &str| {
            json!({
                "schema": EXPORT_MEMORY_SCHEMA_V1, "memory_id": id, "workspace_id": workspace,
                "level": "procedural", "kind": "rule", "content": text,
                "confidence": 0.9, "utility": 0.7, "importance": 0.8,
                "trust_class": "human_explicit", "trust_subclass": "reviewed",
                "created_at": "2026-04-30T00:00:00Z", "updated_at": null,
                "provenance_uri": "ee-export://fixture", "redacted": false
            })
        };
        vec![
            json!({"schema": EXPORT_HEADER_SCHEMA_V1, "format_version": 1,
                "created_at": "2026-04-30T00:00:00Z", "workspace_id": workspace,
                "workspace_path": "/source", "export_scope": "all", "redaction_level": "none",
                "record_count": 6, "ee_version": "0.15.2", "export_id": "recovery-test",
                "import_source": "native", "trust_level": "validated"}),
            memory(&first, "Café releases use a reproducible build."),
            json!({"schema": EXPORT_TAG_SCHEMA_V1, "memory_id": first, "tag": "release", "created_at": "2026-04-30T00:00:00Z"}),
            memory(&second, "Retain the original release evidence."),
            json!({"schema": EXPORT_TAG_SCHEMA_V1, "memory_id": second, "tag": "evidence", "created_at": "2026-04-30T00:00:00Z"}),
            json!({"schema": EXPORT_LINK_SCHEMA_V1,
                "link_id": MemoryLinkId::from_uuid(Uuid::from_u128(3)).to_string(),
                "source_memory_id": first, "target_memory_id": second,
                "link_type": "supports", "weight": 0.75, "created_at": "2026-04-30T00:00:01Z",
                "metadata": {"confidence": 0.5, "directed": false, "evidenceCount": 7,
                    "source": "agent", "createdBy": "release-review", "metadata": {"rationale": "observed releases"}}}),
            json!({"schema": EXPORT_FOOTER_SCHEMA_V1, "export_id": "recovery-test",
                "completed_at": "2026-04-30T00:01:00Z", "total_records": 7, "memory_count": 2,
                "link_count": 1, "tag_count": 2, "audit_count": 0, "success": true}),
        ]
    }

    fn text(records: &[JsonValue]) -> String {
        records
            .iter()
            .map(JsonValue::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    struct Fixture {
        _root: tempfile::TempDir,
        options: JsonlImportOptions,
        workspace: String,
        db: DbConnection,
    }

    impl Fixture {
        fn new(records: &[JsonValue]) -> Result<Self, String> {
            let root = tempfile::tempdir().map_err(|e| e.to_string())?;
            let path = root.path().canonicalize().map_err(|e| e.to_string())?;
            let workspace_path = path.join("workspace");
            fs::create_dir_all(workspace_path.join(".ee")).map_err(|e| e.to_string())?;
            let options = JsonlImportOptions {
                workspace_path,
                database_path: None,
                source_path: path.join("records.jsonl"),
                dry_run: false,
            };
            let workspace = records[0]["workspace_id"]
                .as_str()
                .ok_or("workspace")?
                .to_owned();
            let db = DbConnection::open_file(database_path(&options)).map_err(|e| e.to_string())?;
            db.migrate().map_err(|e| e.to_string())?;
            db.insert_workspace(
                &workspace,
                &CreateWorkspaceInput {
                    path: options.workspace_path.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .map_err(|e| e.to_string())?;
            db.close().map_err(|e| e.to_string())?;
            fs::write(&options.source_path, text(records)).map_err(|e| e.to_string())?;
            let report =
                import_verified_backup_jsonl_records(&options).map_err(|e| e.to_string())?;
            assert_eq!(report.status, "completed", "{:?}", report.issues);
            assert_eq!(report.memories_imported, 2);
            assert_eq!(report.links_imported, 1);
            let db = DbConnection::open_file(database_path(&options)).map_err(|e| e.to_string())?;
            Ok(Self {
                _root: root,
                options,
                workspace,
                db,
            })
        }

        fn expected(&self, rows: &[JsonValue]) -> Result<ExpectedRecords, DomainError> {
            ExpectedRecords::from_parsed(
                &parse_jsonl_source(&text(rows)),
                &self.workspace,
                &NativeAuthState::Unauthenticated {
                    reason: "verified backup fixture".to_owned(),
                },
            )
        }
    }

    #[test]
    fn recovered_unicode_memories_tags_and_links_match_without_promoting_trust() -> TestResult {
        let rows = records();
        let fixture = Fixture::new(&rows)?;
        let expected = fixture.expected(&rows).map_err(|e| e.to_string())?;
        assert!(
            expected
                .memories
                .iter()
                .all(|memory| memory.input.trust_class == "agent_validated")
        );
        verify_in_snapshot(&fixture.db, &expected, &fixture.workspace)
            .map_err(|e| e.to_string())?;
        let before = fixture
            .db
            .list_memories(&fixture.workspace, None, true)
            .map_err(|e| e.to_string())?;
        verify_backup_records(
            &database_path(&fixture.options),
            &fixture.options.source_path,
            &fixture.options.workspace_path,
            &fixture.workspace,
        )
        .map_err(|e| e.to_string())?;
        assert_eq!(
            before,
            fixture
                .db
                .list_memories(&fixture.workspace, None, true)
                .map_err(|e| e.to_string())?
        );
        Ok(())
    }

    #[test]
    fn equal_row_counts_cannot_hide_altered_memory_fields() -> TestResult {
        let rows = records();
        let fixture = Fixture::new(&rows)?;
        for (field, value) in [
            ("content", json!("Different recovered body.")),
            ("confidence", json!(0.1)),
            ("utility", json!(0.2)),
            ("importance", json!(0.3)),
            ("trust_subclass", json!("unreviewed")),
            ("trust_class", json!("agent_assertion")),
            ("provenance_uri", json!("ee-export://other")),
            ("created_at", json!("2026-04-29T00:00:00Z")),
            ("updated_at", json!("2026-05-01T00:00:00Z")),
            ("valid_from", json!("2026-04-29T00:00:00Z")),
            ("valid_to", json!("2027-04-30T00:00:00Z")),
            ("tombstoned_at", json!("2026-05-01T00:00:00Z")),
        ] {
            let mut changed = rows.clone();
            changed[1][field] = value;
            let expected = fixture.expected(&changed).map_err(|e| e.to_string())?;
            assert!(
                verify_in_snapshot(&fixture.db, &expected, &fixture.workspace).is_err(),
                "{field}"
            );
        }
        fixture
            .db
            .begin_read_snapshot()
            .map_err(|e| e.to_string())?;
        fixture
            .db
            .rollback_read_snapshot()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn equal_tag_totals_cannot_hide_changed_ownership() -> TestResult {
        let rows = records();
        let fixture = Fixture::new(&rows)?;
        let mut changed = rows;
        changed[2]["tag"] = json!("evidence");
        changed[4]["tag"] = json!("release");
        let expected = fixture.expected(&changed).map_err(|e| e.to_string())?;
        assert!(verify_in_snapshot(&fixture.db, &expected, &fixture.workspace).is_err());
        Ok(())
    }

    #[test]
    fn equal_edge_totals_cannot_hide_rewired_or_changed_links() -> TestResult {
        let rows = records();
        let fixture = Fixture::new(&rows)?;
        for case in 0..7 {
            let mut changed = rows.clone();
            match case {
                0 => {
                    changed[5]["source_memory_id"] = rows[3]["memory_id"].clone();
                    changed[5]["target_memory_id"] = rows[1]["memory_id"].clone();
                }
                1 => changed[5]["weight"] = json!(0.5),
                2 => changed[5]["metadata"]["confidence"] = json!(0.8),
                3 => changed[5]["metadata"]["directed"] = json!(true),
                4 => changed[5]["metadata"]["evidenceCount"] = json!(8),
                5 => {
                    changed[5]["metadata"]["metadata"] = json!({"rationale": "different evidence"})
                }
                _ => {
                    changed[5]["link_id"] =
                        json!(MemoryLinkId::from_uuid(Uuid::from_u128(4)).to_string())
                }
            }
            let expected = fixture.expected(&changed).map_err(|e| e.to_string())?;
            assert!(
                verify_in_snapshot(&fixture.db, &expected, &fixture.workspace).is_err(),
                "case {case}"
            );
        }
        Ok(())
    }

    #[test]
    fn revision_membership_is_checked_not_just_memory_population() -> TestResult {
        let rows = records();
        let fixture = Fixture::new(&rows)?;
        let mut changed = rows;
        changed[1]["superseded_by"] = changed[3]["memory_id"].clone();
        changed[3]["logical_id"] = changed[1]["memory_id"].clone();
        let expected = fixture.expected(&changed).map_err(|e| e.to_string())?;
        let error = verify_in_snapshot(&fixture.db, &expected, &fixture.workspace)
            .expect_err("different revision family");
        assert!(error.message().contains("revision"));
        Ok(())
    }

    #[test]
    fn restored_posteriors_and_attempt_family_evidence_are_checked() -> TestResult {
        let mut rows = records();
        rows[1]["bayes_alpha"] = json!(2.5);
        rows[1]["bayes_beta"] = json!(1.5);
        rows[1]["attempt_family"] = json!({"family_id": "family-recovery", "declared_size": 3,
            "attempt_index": 1, "disposition": "selected", "origin": "declared"});
        let fixture = Fixture::new(&rows)?;
        let expected = fixture.expected(&rows).map_err(|e| e.to_string())?;
        verify_in_snapshot(&fixture.db, &expected, &fixture.workspace)
            .map_err(|e| e.to_string())?;
        for case in 0..4 {
            let mut changed = rows.clone();
            match case {
                0 => changed[1]["bayes_alpha"] = json!(3.5),
                1 => changed[1]["attempt_family"]["attempt_index"] = json!(2),
                2 => changed[1]["attempt_family"]["declared_size"] = json!(4),
                _ => changed[1]["attempt_family"] = json!(null),
            }
            let expected = fixture.expected(&changed).map_err(|e| e.to_string())?;
            assert!(
                verify_in_snapshot(&fixture.db, &expected, &fixture.workspace).is_err(),
                "case {case}"
            );
        }
        Ok(())
    }

    #[test]
    fn incomplete_and_cross_workspace_streams_are_rejected() -> TestResult {
        let rows = records();
        let fixture = Fixture::new(&rows)?;
        for case in 0..4 {
            let mut changed = rows.clone();
            match case {
                0 => changed[6]["success"] = json!(false),
                1 => changed[6]["memory_count"] = json!(3),
                2 => {
                    changed[0]["workspace_id"] =
                        json!(WorkspaceId::from_uuid(Uuid::from_u128(32)).to_string())
                }
                _ => {
                    changed[1]["workspace_id"] =
                        json!(WorkspaceId::from_uuid(Uuid::from_u128(32)).to_string())
                }
            }
            assert!(fixture.expected(&changed).is_err(), "case {case}");
        }
        Ok(())
    }

    #[test]
    fn caller_snapshot_is_preserved_on_nested_verification_refusal() -> TestResult {
        let rows = records();
        let fixture = Fixture::new(&rows)?;
        let expected = fixture.expected(&rows).map_err(|e| e.to_string())?;
        fixture
            .db
            .begin_read_snapshot()
            .map_err(|e| e.to_string())?;
        assert!(verify_in_snapshot(&fixture.db, &expected, &fixture.workspace).is_err());
        fixture
            .db
            .commit_read_snapshot()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn record_mismatch_diagnostics_do_not_echo_private_evidence() -> TestResult {
        let rows = records();
        let fixture = Fixture::new(&rows)?;
        let mut changed = rows;
        changed[1]["content"] = json!("PRIVATE-CANARY content must not appear in errors.");
        let expected = fixture.expected(&changed).map_err(|e| e.to_string())?;
        let error = verify_in_snapshot(&fixture.db, &expected, &fixture.workspace)
            .expect_err("different body");
        assert!(!error.to_string().contains("PRIVATE-CANARY"));
        assert!(!error.to_string().contains(&fixture.workspace));
        Ok(())
    }

    #[test]
    fn explicit_supersession_round_trip_preserves_expiry_and_exact_headship() -> TestResult {
        let mut rows = records();
        rows[1]["superseded_at"] = json!("2026-05-02T01:00:00+01:00");
        rows[1]["superseded_by"] = rows[3]["memory_id"].clone();
        rows[1]["valid_to"] = json!("2027-12-01T00:00:00Z");
        rows[3]["logical_id"] = rows[1]["memory_id"].clone();
        rows[3]["created_at"] = json!("2026-05-02T00:00:00Z");
        rows[3]["valid_from"] = json!("2026-05-02T00:00:00Z");
        let fixture = Fixture::new(&rows)?;
        let expected = fixture.expected(&rows).map_err(|e| e.to_string())?;
        verify_in_snapshot(&fixture.db, &expected, &fixture.workspace)
            .map_err(|e| e.to_string())?;
        let prior = rows[1]["memory_id"].as_str().ok_or("prior")?;
        let head = rows[3]["memory_id"].as_str().ok_or("head")?;
        assert_eq!(
            fixture
                .db
                .get_memory_superseded_at(prior)
                .map_err(|e| e.to_string())?,
            Some("2026-05-02T00:00:00Z".to_owned())
        );
        assert_eq!(
            fixture
                .db
                .get_memory(prior)
                .map_err(|e| e.to_string())?
                .ok_or("prior")?
                .valid_to,
            Some("2027-12-01T00:00:00Z".to_owned())
        );
        assert_eq!(
            fixture
                .db
                .filter_current_memory_ids(&[prior.to_owned(), head.to_owned()])
                .map_err(|e| e.to_string())?,
            BTreeSet::from([head.to_owned()])
        );
        let before = fixture
            .db
            .list_memories(&fixture.workspace, None, true)
            .map_err(|e| e.to_string())?;
        let repeat =
            import_verified_backup_jsonl_records(&fixture.options).map_err(|e| e.to_string())?;
        assert_eq!(repeat.status, "completed", "{:?}", repeat.issues);
        assert_eq!(repeat.memories_imported, 0);
        assert_eq!(repeat.memories_skipped_duplicate, 2);
        assert_eq!(
            before,
            fixture
                .db
                .list_memories(&fixture.workspace, None, true)
                .map_err(|e| e.to_string())?
        );
        Ok(())
    }

    #[test]
    fn successor_only_archives_restore_history_without_inventing_an_expiry() -> TestResult {
        for reverse_reference in [false, true] {
            let mut rows = records();
            if reverse_reference {
                rows[3]["supersedes"] = rows[1]["memory_id"].clone();
            } else {
                rows[1]["superseded_by"] = rows[3]["memory_id"].clone();
            }
            rows[3]["logical_id"] = rows[1]["memory_id"].clone();
            rows[3]["valid_from"] = json!("2026-05-02T00:00:00Z");
            let fixture = Fixture::new(&rows)?;
            let prior = rows[1]["memory_id"].as_str().ok_or("prior")?;
            assert_eq!(
                fixture
                    .db
                    .get_memory_superseded_at(prior)
                    .map_err(|e| e.to_string())?,
                Some("2026-05-02T00:00:00Z".to_owned())
            );
            assert_eq!(
                fixture
                    .db
                    .get_memory(prior)
                    .map_err(|e| e.to_string())?
                    .ok_or("prior")?
                    .valid_to,
                None
            );
        }
        Ok(())
    }

    #[test]
    fn malformed_revision_graphs_are_rejected_before_storage_creation() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        for case in 0..5 {
            let mut rows = records();
            rows[3]["logical_id"] = rows[1]["memory_id"].clone();
            rows[1]["superseded_by"] = rows[3]["memory_id"].clone();
            match case {
                0 => rows[1]["superseded_by"] = json!("missing"),
                1 => rows[1]["superseded_by"] = rows[1]["memory_id"].clone(),
                2 => rows[3]["superseded_by"] = rows[1]["memory_id"].clone(),
                3 => rows[3]["logical_id"] = rows[3]["memory_id"].clone(),
                _ => rows[1]["superseded_at"] = json!("not-a-time"),
            }
            let options = JsonlImportOptions {
                workspace_path: root.path().join(format!("absent-{case}")),
                database_path: None,
                source_path: root.path().join(format!("case-{case}.jsonl")),
                dry_run: false,
            };
            fs::write(&options.source_path, text(&rows)).map_err(|e| e.to_string())?;
            let report =
                import_verified_backup_jsonl_records(&options).map_err(|e| e.to_string())?;
            assert_eq!(report.status, "rejected", "case {case}");
            assert!(!options.workspace_path.exists(), "case {case}");
        }
        Ok(())
    }

    #[test]
    fn redacted_revision_references_remain_resolvable_and_do_not_leak_raw_ids() -> TestResult {
        for level in [RedactionLevel::Standard, RedactionLevel::Paranoid] {
            let mut rows = records();
            rows[1]["superseded_by"] = rows[3]["memory_id"].clone();
            rows[3]["supersedes"] = rows[1]["memory_id"].clone();
            rows[3]["logical_id"] = rows[1]["memory_id"].clone();
            rows[3]["valid_from"] = json!("2026-05-02T00:00:00Z");
            // Only identifier redaction is relevant here; full paranoid body
            // redaction can produce non-importable placeholder content.
            let first = serde_json::from_value(rows[1].clone()).map_err(|e| e.to_string())?;
            let second = serde_json::from_value(rows[3].clone()).map_err(|e| e.to_string())?;
            let first = crate::output::jsonl_export::redact_memory_record(first, level);
            let second = crate::output::jsonl_export::redact_memory_record(second, level);
            assert_eq!(
                first.superseded_by.as_deref(),
                Some(second.memory_id.as_str())
            );
            assert_eq!(second.supersedes.as_deref(), Some(first.memory_id.as_str()));
            assert!(
                !serde_json::to_string(&first)
                    .map_err(|e| e.to_string())?
                    .contains(rows[3]["memory_id"].as_str().ok_or("head")?)
            );
            assert!(
                !serde_json::to_string(&second)
                    .map_err(|e| e.to_string())?
                    .contains(rows[1]["memory_id"].as_str().ok_or("prior")?)
            );
        }
        Ok(())
    }
}
