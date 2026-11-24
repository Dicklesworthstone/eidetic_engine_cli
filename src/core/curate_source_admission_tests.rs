use super::super as curate_core;
use super::*;

struct SourceFixture {
    directory: tempfile::TempDir,
    database: std::path::PathBuf,
    connection: DbConnection,
    workspace_id: String,
    memory_id: String,
    evidence_id: String,
    candidate_id: String,
    key_config: ReflectionHmacKeyConfig,
}

impl SourceFixture {
    fn new(seed: u128) -> Result<Self, String> {
        let directory = tempfile::tempdir_in("/tmp").map_err(|error| error.to_string())?;
        let database = directory.path().join("ee.db");
        let workspace_id = test_workspace_id(directory.path());
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
        let evidence_id = evidence_id(seed + 1);
        let candidate_id = curate_id(seed + 2);
        let connection = seed_create_derived_candidate_database(
            &database,
            directory.path(),
            &workspace_id,
            &memory_id,
            &evidence_id,
            &candidate_id,
            None,
            None,
            None,
        )?;
        let key_path = directory.path().join("reflect.key");
        fs::write(&key_path, b"curation-source-authority-test-key")
            .map_err(|error| error.to_string())?;
        Ok(Self {
            directory,
            database,
            connection,
            workspace_id,
            memory_id,
            evidence_id,
            candidate_id,
            key_config: ReflectionHmacKeyConfig::new(
                Some("source-authority-test".to_owned()),
                Some(key_path),
            ),
        })
    }

    fn derive(&self, dry_run: bool) -> Result<curate_core::ProposeDerivedReport, DomainError> {
        curate_core::propose_derived_candidate(&curate_core::ProposeDerivedOptions {
            workspace_path: self.directory.path(),
            database_path: Some(&self.database),
            source_memory_ids: std::slice::from_ref(&self.memory_id),
            source_evidence_span_ids: &[],
            level: "semantic",
            kind: "summary",
            content: "A historical summary of the selected source memory.",
            tags: &[],
            confidence: 0.6,
            utility: None,
            importance: None,
            valid_from: None,
            valid_to: None,
            producer_kind: None,
            producer_model: None,
            producer_note: None,
            dry_run,
        })
    }

    fn reflect(
        &self,
        dry_run: bool,
        untyped_source: bool,
    ) -> Result<curate_core::ReflectionProposeReport, DomainError> {
        let ids = std::slice::from_ref(&self.memory_id);
        propose_reflection_request(&ReflectionProposeOptions {
            workspace_path: self.directory.path(),
            database_path: Some(&self.database),
            reflection_kind: "gaps",
            gaps_only: false,
            source_ids: if untyped_source { ids } else { &[] },
            source_memory_ids: if untyped_source { &[] } else { ids },
            source_evidence_span_ids: &[],
            created_at: Some("2026-05-24T00:00:00Z"),
            limits: ReflectionSourcePackageLimits::default(),
            dry_run,
            hmac_key_config: Some(self.key_config.clone()),
            lifecycle_config: Some(
                ReflectionRequestLifecycleConfig::new(3600, 60)
                    .expect("valid reflection lifecycle"),
            ),
        })
    }

    fn candidate_count(&self) -> Result<usize, String> {
        self.connection
            .list_curation_candidates(&self.workspace_id, None, None, None)
            .map(|rows| rows.len())
            .map_err(|error| error.to_string())
    }

    fn source_body(&self) -> Result<String, String> {
        self.connection
            .get_memory(&self.memory_id)
            .map_err(|error| error.to_string())?
            .map(|memory| memory.content)
            .ok_or_else(|| "missing source memory".to_owned())
    }

    fn withdraw(&self, posture: &str) -> TestResult {
        match posture {
            "sealed" => self.connection.insert_memory_seal(
                &self.memory_id,
                &crate::models::memory_seal_commitment(self.source_body()?.as_bytes()),
                "2026-05-24T00:05:00Z",
            ),
            "held" => self.insert_hold("memory", &self.memory_id),
            _ => return Err(format!("unknown source posture {posture}")),
        }
        .map_err(|error| error.to_string())
    }

    fn insert_hold(&self, target_type: &str, target_id: &str) -> crate::db::Result<()> {
        self.connection.insert_feedback_quarantine(
            &format!("fq_{:026}", 91),
            &crate::db::CreateFeedbackQuarantineInput {
                workspace_id: self.workspace_id.clone(),
                source_id: "source-authority-test".to_owned(),
                target_type: target_type.to_owned(),
                target_id: target_id.to_owned(),
                signal: "harmful".to_owned(),
                weight: 1.0,
                source_type: "automated_check".to_owned(),
                proposed_event_id: None,
                recorded_at: "2026-05-24T00:05:00Z".to_owned(),
                reason: "PRIVATE_REVIEW_REASON_NOT_FOR_SOURCE_ERRORS".to_owned(),
                event_reason: None,
                evidence_json: None,
                session_id: None,
                raw_event_hash: format!("blake3:{}", "a".repeat(64)),
            },
        )
    }

    fn release(&self, posture: &str) -> TestResult {
        match posture {
            "sealed" => {
                assert!(
                    self.connection
                        .mark_memory_seal_revealed(&self.memory_id, "2026-05-24T00:06:00Z",)
                        .map_err(|error| error.to_string())?
                );
            }
            "held" => self
                .connection
                .execute_raw("UPDATE feedback_quarantine SET status = 'rejected'")
                .map_err(|error| error.to_string())?,
            _ => return Err(format!("unknown source posture {posture}")),
        }
        Ok(())
    }
}

fn posture_code(posture: &str) -> &'static str {
    match posture {
        "sealed" => "derived_source_memory_sealed",
        "held" => "derived_source_memory_review_pending",
        "unavailable" => "derived_source_memory_authority_unavailable",
        _ => panic!("unknown source posture"),
    }
}

fn assert_private_refusal(message: &str, body: &str, code: &str) {
    assert!(message.contains(code), "{message}");
    assert!(!message.contains(body), "source body escaped: {message}");
    assert!(!message.contains("PRIVATE_REVIEW_REASON_NOT_FOR_SOURCE_ERRORS"));
}

#[test]
fn derived_and_reflection_proposals_reject_live_source_withdrawal() -> TestResult {
    for posture in ["sealed", "held"] {
        let fixture = SourceFixture::new(0x73_100)?;
        let body = fixture.source_body()?;
        fixture.withdraw(posture)?;
        // Retained plaintext and its content hash are unchanged. Sidecars,
        // rather than placeholder spelling, must decide new disclosure.
        assert_eq!(fixture.source_body()?, body);
        for dry_run in [true, false] {
            let error = fixture
                .derive(dry_run)
                .expect_err("withheld derivation source");
            assert_private_refusal(&error.message(), &body, posture_code(posture));
            for untyped_source in [true, false] {
                let error = fixture
                    .reflect(dry_run, untyped_source)
                    .expect_err("withheld reflection source");
                assert_private_refusal(&error.message(), &body, posture_code(posture));
            }
        }
        assert_eq!(fixture.candidate_count()?, 1, "no proposal may be inserted");
        assert!(
            fixture
                .connection
                .query("SELECT request_id FROM reflection_request_ledger", &[],)
                .map_err(|error| error.to_string())?
                .is_empty()
        );
    }
    Ok(())
}

#[test]
fn derived_apply_rechecks_authority_after_approval_and_planning() -> TestResult {
    for posture in ["sealed", "held"] {
        let fixture = SourceFixture::new(0x73_200)?;
        let body = fixture.source_body()?;
        validate_curation_candidate(&curate_core::CurateValidateOptions {
            workspace_path: fixture.directory.path(),
            database_path: Some(&fixture.database),
            candidate_id: &fixture.candidate_id,
            actor: Some("source-authority-test"),
            dry_run: false,
        })
        .map_err(|error| error.message())?;
        let stored = fixture
            .connection
            .get_curation_candidate(&fixture.workspace_id, &fixture.candidate_id)
            .map_err(|error| error.to_string())?
            .ok_or("missing candidate")?;
        let now = chrono::Utc::now().to_rfc3339();
        let planned = curate_core::evaluate_create_derived_candidate_for_apply(
            &fixture.connection,
            &stored,
            &now,
            true,
        );
        assert!(planned.should_persist, "{:?}", planned.application.errors);
        fixture.withdraw(posture)?;

        let error = curate_core::persist_candidate_application(
            &fixture.connection,
            &fixture.workspace_id,
            &stored,
            &planned,
            None,
            &now,
            "source-authority-test",
        )
        .expect_err("writer must reject withdrawal after planning");
        assert_private_refusal(&error.message(), &body, posture_code(posture));
        let report = apply_curation_candidate(&curate_core::CurateApplyOptions {
            workspace_path: fixture.directory.path(),
            database_path: Some(&fixture.database),
            candidate_id: &fixture.candidate_id,
            actor: Some("source-authority-test"),
            dry_run: false,
            allow_tombstone_load_bearing: false,
        })
        .map_err(|error| error.message())?;
        assert_eq!(report.application.status, "blocked");
        assert!(
            report
                .application
                .errors
                .iter()
                .any(|issue| issue.code == posture_code(posture))
        );
        assert!(!report.mutation.persisted);
        assert_no_create_derived_apply_side_effects(
            &fixture.connection,
            &fixture.workspace_id,
            &fixture.candidate_id,
            &fixture.memory_id,
            &fixture.evidence_id,
            None,
            1,
            posture,
        )?;
    }
    Ok(())
}

#[test]
fn reflection_ingest_rechecks_source_authority_but_preserves_accepted_replays() -> TestResult {
    for posture in ["sealed", "held"] {
        let fixture = SourceFixture::new(0x73_300)?;
        let body = fixture.source_body()?;
        let proposed = fixture
            .reflect(false, false)
            .map_err(|error| error.message())?;
        let mut kind_fields = serde_json::Map::new();
        kind_fields.insert(
            "knowledgeGaps".to_owned(),
            serde_json::json!([{
                "topic": "source admission",
                "question": "Which current authority must a new reflection recheck?"
            }]),
        );
        let result = ReflectionResultArtifact {
            schema: REFLECTION_RESULT_SCHEMA.to_owned(),
            request_id: proposed.request_id.clone(),
            request_hash: proposed.request_hash.clone(),
            challenge: proposed
                .request
                .challenge
                .clone()
                .ok_or("missing challenge")?,
            producer: ReflectionResultProducer {
                kind: "agent_harness".to_owned(),
                id: "source-authority-test".to_owned(),
                version: None,
                extra: BTreeMap::new(),
            },
            reflection_kind: "gaps".to_owned(),
            cited_source_ids: vec![fixture.memory_id.clone()],
            body: "The cited source leaves an unresolved gap about live source authority."
                .to_owned(),
            kind_fields,
            self_reported_confidence: 0.7,
        };
        let json = serde_json::to_string(&result).map_err(|error| error.to_string())?;
        let options = ReflectionIngestOptions {
            workspace_path: fixture.directory.path(),
            database_path: Some(&fixture.database),
            result_json: &json,
            consumed_at: Some("2026-05-24T00:30:00Z"),
            dry_run: false,
            gaps_only: false,
            hmac_key_config: Some(fixture.key_config.clone()),
        };
        let dry = ingest_reflection_result(&ReflectionIngestOptions {
            dry_run: true,
            ..options.clone()
        })
        .map_err(|error| error.message())?;
        assert_eq!(dry.outcome, "would_insert");
        let material = reflection_request_ledger_material(&proposed.request)
            .map_err(|error| error.to_string())?;
        let key = fixture
            .key_config
            .load_key_material()
            .map_err(|error| error.to_string())?;
        let decision = crate::curate::reflection_result_ingest_decision_from_ledger(
            &material,
            &result,
            ReflectionResultReplayGate::Pending,
            Some(&key),
            "2026-05-24T00:30:00Z",
        )
        .map_err(|error| error.to_string())?;
        fixture.withdraw(posture)?;

        // The decision was already authorized. The storage callback must
        // reject the intervening withdrawal inside its writer transaction.
        let error = persist_reflection_result_ingest_decision(
            &fixture.connection,
            &fixture.workspace_id,
            &proposed.request_id,
            &decision,
            "2026-05-24T00:30:00Z",
        )
        .expect_err("new result cannot use withdrawn source after preflight");
        assert_private_refusal(&error.message(), &body, posture_code(posture));
        let error = ingest_reflection_result(&options).expect_err("preflight must also reject");
        assert_private_refusal(&error.message(), &body, posture_code(posture));
        assert_eq!(fixture.candidate_count()?, 1);
        let ledger = fixture
            .connection
            .get_reflection_request_ledger(&fixture.workspace_id, &proposed.request_id)
            .map_err(|error| error.to_string())?
            .ok_or("missing request ledger")?;
        assert_eq!(ledger.status, "pending");
        assert!(ledger.consumed_candidate_id.is_none());

        fixture.release(posture)?;
        let accepted = ingest_reflection_result(&options).map_err(|error| error.message())?;
        assert_eq!(accepted.outcome, "inserted");
        assert_eq!(fixture.candidate_count()?, 2);
        let withdraw_again = if posture == "sealed" {
            "UPDATE memory_seals SET revealed_at = NULL, reveal_verified = NULL"
        } else {
            "UPDATE feedback_quarantine SET status = 'pending'"
        };
        fixture
            .connection
            .execute_raw(withdraw_again)
            .map_err(|error| error.to_string())?;
        let receipt = ingest_reflection_result(&options).map_err(|error| error.message())?;
        assert_eq!(receipt.outcome, "idempotent_replay");
        assert_eq!(receipt.candidate_id, accepted.candidate_id);
        assert_eq!(receipt.validation.source_lock, "not_checked");
        // Every source read now fails, proving a completed replay is only a
        // receipt lookup, including the DB's stale-CreateCandidate replay path.
        fixture
            .connection
            .execute_raw("ALTER TABLE memory_seals RENAME TO withheld_seals")
            .map_err(|error| error.to_string())?;
        let replay = persist_reflection_result_ingest_decision(
            &fixture.connection,
            &fixture.workspace_id,
            &proposed.request_id,
            &decision,
            "2026-05-24T00:31:00Z",
        )
        .map_err(|error| error.message())?;
        assert_eq!(
            replay,
            ReflectionResultDurableIngestOutcome::IdempotentReplay {
                candidate_id: accepted.candidate_id.ok_or("accepted candidate missing")?,
            }
        );
        assert_eq!(fixture.candidate_count()?, 2);
    }
    Ok(())
}

#[test]
fn historical_revealed_and_review_released_sources_remain_eligible() -> TestResult {
    let fixture = SourceFixture::new(0x73_400)?;
    fixture.connection.execute_raw(&format!(
        "UPDATE memories SET valid_from = '2000-01-01T00:00:00Z', valid_to = '2001-01-01T00:00:00Z', superseded_at = '2001-01-01T00:00:00Z' WHERE id = '{}'",
        fixture.memory_id,
    )).map_err(|error| error.to_string())?;
    fixture.withdraw("sealed")?;
    fixture.release("sealed")?;
    fixture.withdraw("held")?;
    fixture.release("held")?;
    // A rule-target review is not a memory hold even if its raw id collides.
    fixture
        .connection
        .execute_raw("UPDATE feedback_quarantine SET status = 'pending', target_type = 'rule'")
        .map_err(|error| error.to_string())?;
    assert!(
        !fixture
            .derive(true)
            .map_err(|error| error.message())?
            .persisted
    );
    let reflection = fixture
        .reflect(true, false)
        .map_err(|error| error.message())?;
    let serialized = serde_json::to_string(&reflection).map_err(|error| error.to_string())?;
    assert!(serialized.contains(&fixture.source_body()?));
    let validation = validate_curation_candidate(&curate_core::CurateValidateOptions {
        workspace_path: fixture.directory.path(),
        database_path: Some(&fixture.database),
        candidate_id: &fixture.candidate_id,
        actor: Some("source-authority-test"),
        dry_run: true,
    })
    .map_err(|error| error.message())?;
    assert_eq!(validation.validation.status, "passed");
    Ok(())
}

#[test]
fn derivation_source_authority_fails_closed_without_releasing_owned_transaction() -> TestResult {
    let fixture = SourceFixture::new(0x73_500)?;
    fixture
        .connection
        .execute_raw("ALTER TABLE feedback_quarantine RENAME TO unavailable_reviews")
        .map_err(|error| error.to_string())?;
    curate_core::persist_curation_transaction(
        &fixture.connection,
        "test authority snapshot",
        || {
            let memory = fixture
                .connection
                .get_memory(&fixture.memory_id)
                .map_err(|error| DomainError::Storage {
                    message: error.to_string(),
                    repair: None,
                })?
                .ok_or_else(|| DomainError::Storage {
                    message: "missing source".to_owned(),
                    repair: None,
                })?;
            let failure = curate_core::source_admission::with_snapshot(&fixture.connection, || {
                curate_core::source_admission::require_memory_authority(
                    &fixture.connection,
                    &memory,
                )
            })
            .expect_err("unreadable authority is not an unsealed/unheld source");
            assert!(
                failure
                    .message()
                    .contains("derived_source_memory_authority_unavailable")
            );
            // If the borrowed snapshot rolled back this transaction, its owner's
            // commit below would fail. Keep a real write in the surrounding scope.
            fixture
                .connection
                .add_memory_tags(&fixture.memory_id, &["snapshot-owner".to_owned()])
                .map_err(|error| DomainError::Storage {
                    message: error.to_string(),
                    repair: None,
                })?;
            Ok(())
        },
    )
    .map_err(|error| error.message())?;
    assert!(
        fixture
            .connection
            .get_memory_tags(&fixture.memory_id)
            .map_err(|error| error.to_string())?
            .contains(&"snapshot-owner".to_owned())
    );
    Ok(())
}

struct ProceduralFixture {
    directory: tempfile::TempDir,
    database: std::path::PathBuf,
    connection: DbConnection,
    workspace_id: String,
    memory_id: String,
    candidate_id: String,
    as_recipe: bool,
}

impl ProceduralFixture {
    fn new(kind: &str, as_recipe: bool) -> Result<Self, String> {
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        fs::create_dir(directory.path().join(".ee")).map_err(|error| error.to_string())?;
        let database = directory.path().join("ee.db");
        let workspace_id = test_workspace_id(directory.path());
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x73_600)).to_string();
        let candidate_id = curate_id(0x73_601);
        let connection = seed_candidate_database(
            &database,
            &workspace_id,
            &memory_id,
            &candidate_id,
            kind,
            Some("approved"),
            Some("Verify the signed release artifact before deployment."),
        )?;
        Ok(Self {
            directory,
            database,
            connection,
            workspace_id,
            memory_id,
            candidate_id,
            as_recipe,
        })
    }

    fn apply(&self, dry_run: bool) -> Result<curate_core::CurateApplyReport, DomainError> {
        let options = curate_core::CurateApplyOptions {
            workspace_path: self.directory.path(),
            database_path: Some(&self.database),
            candidate_id: &self.candidate_id,
            actor: Some("procedural-source-authority-test"),
            dry_run,
            allow_tombstone_load_bearing: false,
        };
        if self.as_recipe {
            curate_core::apply_curation_candidate_as_recipe(
                &options,
                "Verified release",
                "Before deploying a release",
            )
        } else {
            curate_core::apply_curation_candidate(&options)
        }
    }

    fn withdraw(&self, id: &str, posture: &str) -> TestResult {
        match posture {
            "sealed" => self.connection.insert_memory_seal(
                id,
                &crate::models::memory_seal_commitment(b"retained procedural evidence"),
                "2026-05-24T00:05:00Z",
            ),
            "held" => self.connection.insert_feedback_quarantine(
                &format!("fq_{:026}", 92),
                &crate::db::CreateFeedbackQuarantineInput {
                    workspace_id: self.workspace_id.clone(),
                    source_id: "procedural-source-authority-test".to_owned(),
                    target_type: "memory".to_owned(),
                    target_id: id.to_owned(),
                    signal: "harmful".to_owned(),
                    weight: 1.0,
                    source_type: "automated_check".to_owned(),
                    proposed_event_id: None,
                    recorded_at: "2026-05-24T00:05:00Z".to_owned(),
                    reason: "PRIVATE_REVIEW_REASON_NOT_FOR_SOURCE_ERRORS".to_owned(),
                    event_reason: None,
                    evidence_json: None,
                    session_id: None,
                    raw_event_hash: format!("blake3:{}", "b".repeat(64)),
                },
            ),
            "unavailable" => self
                .connection
                .execute_raw("ALTER TABLE memory_seals RENAME TO unavailable_procedural_seals"),
            _ => return Err(format!("unknown source posture {posture}")),
        }
        .map_err(|error| error.to_string())
    }

    fn counts(&self) -> Result<Vec<i64>, String> {
        [
            "memories",
            "curation_candidates",
            "procedural_rules",
            "procedures",
            "procedure_events",
            "plan_recipes",
            "search_index_jobs",
            "audit_log",
        ]
        .into_iter()
        .map(|table| {
            self.connection
                .count_table_rows(table)
                .map_err(|error| error.to_string())
        })
        .collect()
    }
}

#[test]
fn procedural_apply_rechecks_withdrawn_sources_without_creating_outputs() -> TestResult {
    for (kind, as_recipe) in [
        ("rule", false),
        ("anti_pattern_proposal", false),
        ("procedure", false),
        ("rule", true),
        ("procedure", true),
    ] {
        for posture in ["sealed", "held", "unavailable"] {
            let fixture = ProceduralFixture::new(kind, as_recipe)?;
            let preview = fixture.apply(true).map_err(|error| error.message())?;
            assert_eq!(preview.application.status, "would_apply");
            let stored = fixture
                .connection
                .get_curation_candidate(&fixture.workspace_id, &fixture.candidate_id)
                .map_err(|error| error.to_string())?
                .ok_or("missing procedural candidate")?;
            let target = fixture
                .connection
                .get_memory(&fixture.memory_id)
                .map_err(|error| error.to_string())?
                .ok_or("missing procedural source")?;
            let now = chrono::Utc::now().to_rfc3339();
            let mut planned = curate_core::evaluate_candidate_for_apply(
                &stored,
                Some(&target),
                &curate_core::ConsolidateAbsorbResolution::LegacyUpdate,
                &now,
            );
            assert!(planned.should_persist, "{:?}", planned.application.errors);
            let recipe = as_recipe.then(|| {
                planned.rule_create = None;
                planned.procedure_create = None;
                planned.target_after = planned.target_before.clone();
                crate::db::StoredPlanRecipe {
                    id: curate_core::candidate_recipe_id(&stored),
                    workspace_id: fixture.workspace_id.clone(),
                    name: "Verified release".to_owned(),
                    when_to_use: "Before deploying a release".to_owned(),
                    steps_json: serde_json::json!([stored.proposed_content]).to_string(),
                    evidence_uris_json: serde_json::json!([
                        format!("ee://curation-candidate/{}", stored.id),
                        format!("ee://memory/{}", fixture.memory_id),
                    ])
                    .to_string(),
                    maturity: "draft".to_owned(),
                    confidence: 0.0,
                    helpful_count: 0,
                    harmful_count: 0,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    last_recommended_at: None,
                }
            });
            fixture.withdraw(&fixture.memory_id, posture)?;
            // Seal/review sidecars change; the prepared target row does not.
            assert_eq!(
                fixture
                    .connection
                    .get_memory(&fixture.memory_id)
                    .map_err(|error| error.to_string())?,
                Some(target.clone()),
            );
            let counts = fixture.counts()?;
            for dry_run in [true, false] {
                let error = fixture
                    .apply(dry_run)
                    .expect_err("source withdrawal must block public procedural apply");
                assert_private_refusal(&error.message(), &target.content, posture_code(posture));
                assert!(
                    !error
                        .message()
                        .contains(stored.proposed_content.as_deref().ok_or("proposal")?)
                );
            }
            let error = curate_core::persist_candidate_application(
                &fixture.connection,
                &fixture.workspace_id,
                &stored,
                &planned,
                recipe.as_ref(),
                &now,
                "procedural-source-authority-test",
            )
            .expect_err("source withdrawal after planning must block the writer");
            assert_private_refusal(&error.message(), &target.content, posture_code(posture));
            assert_eq!(
                fixture.counts()?,
                counts,
                "{kind}, recipe={as_recipe}, {posture}"
            );
            assert_eq!(
                fixture
                    .connection
                    .get_memory(&fixture.memory_id)
                    .map_err(|error| error.to_string())?,
                Some(target),
            );
            assert_eq!(
                fixture
                    .connection
                    .get_curation_candidate(&fixture.workspace_id, &fixture.candidate_id)
                    .map_err(|error| error.to_string())?,
                Some(stored),
            );
        }
    }
    Ok(())
}

#[test]
fn procedural_apply_checks_cluster_sources_beyond_the_target() -> TestResult {
    for posture in ["sealed", "held"] {
        let fixture = ProceduralFixture::new("rule", false)?;
        let second_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x73_602)).to_string();
        fixture
            .connection
            .insert_memory(
                &second_id,
                &CreateMemoryInput {
                    workspace_id: fixture.workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: "Corroborating release evidence kept out of source errors.".to_owned(),
                    workflow_id: None,
                    confidence: 0.7,
                    utility: 0.6,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "agent_validated".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        bind_procedural_source_audit(&fixture, &second_id)?;
        assert_eq!(
            fixture
                .apply(true)
                .map_err(|error| error.message())?
                .application
                .status,
            "would_apply"
        );
        fixture.withdraw(&second_id, posture)?;
        let counts = fixture.counts()?;
        let error = fixture
            .apply(false)
            .expect_err("a cluster source was withdrawn");
        assert_private_refusal(
            &error.message(),
            "Corroborating release evidence kept out of source errors.",
            posture_code(posture),
        );
        assert_eq!(fixture.counts()?, counts);
    }
    Ok(())
}

#[test]
fn applied_procedural_outputs_remain_independent_of_later_source_withdrawal() -> TestResult {
    for (kind, as_recipe) in [
        ("rule", false),
        ("anti_pattern_proposal", false),
        ("procedure", false),
        ("rule", true),
        ("procedure", true),
    ] {
        let fixture = ProceduralFixture::new(kind, as_recipe)?;
        let accepted = fixture.apply(false).map_err(|error| error.message())?;
        assert_eq!(accepted.application.status, "applied");
        assert!(accepted.durable_mutation);
        let before = fixture.counts()?;
        fixture.withdraw(&fixture.memory_id, "sealed")?;
        fixture.withdraw(&fixture.memory_id, "held")?;
        let replay = fixture.apply(false).map_err(|error| error.message())?;
        assert_eq!(replay.application.status, "already_applied");
        assert!(!replay.durable_mutation);
        // Source revocation does not delete or remint an independent rule,
        // procedure or recipe. Derived index recovery remains the normal lane.
        assert_eq!(&fixture.counts()?[..6], &before[..6]);
        assert_eq!(
            fixture
                .connection
                .get_curation_candidate(&fixture.workspace_id, &fixture.candidate_id)
                .map_err(|error| error.to_string())?
                .ok_or("missing applied candidate")?
                .status,
            "applied"
        );
    }
    Ok(())
}

fn bind_procedural_source_audit(fixture: &ProceduralFixture, source_id: &str) -> TestResult {
    let stored = fixture
        .connection
        .get_curation_candidate(&fixture.workspace_id, &fixture.candidate_id)
        .map_err(|error| error.to_string())?
        .ok_or("missing procedural candidate")?;
    fixture
        .connection
        .insert_audit(
            &crate::db::generate_audit_id(),
            &crate::db::CreateAuditInput {
                workspace_id: Some(fixture.workspace_id.clone()),
                actor: Some("learn.experiment.propose".to_owned()),
                action: audit_actions::CURATION_CANDIDATE_CREATE.to_owned(),
                target_type: Some("curation_candidate".to_owned()),
                target_id: Some(stored.id.clone()),
                details: Some(
                    serde_json::json!({
                        "schema": "ee.audit.curation_candidate_create.v1",
                        "proposalSource": "learn.experiment.propose",
                        "workspaceId": fixture.workspace_id,
                        "candidateId": stored.id,
                        "candidateType": stored.candidate_type,
                        "sourceType": stored.source_type,
                        "sourceId": stored.source_id,
                        "targetMemoryId": stored.target_memory_id,
                        "proposedContentHash": blake3::hash(
                            stored.proposed_content.as_deref().unwrap_or_default().as_bytes()
                        ).to_hex().to_string(),
                        "sourceRefs": [{
                            "kind": "memory",
                            "id": source_id,
                            "contentHash": format!("blake3:{}", blake3::hash(
                                b"Corroborating release evidence kept out of source errors."
                            ).to_hex()),
                        }],
                    })
                    .to_string(),
                ),
            },
        )
        .map_err(|error| error.to_string())
}

#[test]
fn procedural_source_admission_keeps_unavailable_audit_claims_visible() -> TestResult {
    for (posture, code) in [
        ("missing", "derived_source_memory_missing"),
        ("foreign", "derived_source_workspace_mismatch"),
        ("tombstoned", "derived_source_memory_tombstoned"),
    ] {
        let fixture = ProceduralFixture::new("rule", false)?;
        let source_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x73_603)).to_string();
        if posture != "missing" {
            let source_workspace = if posture == "foreign" {
                let foreign_id =
                    crate::models::WorkspaceId::from_uuid(uuid::Uuid::from_u128(0x73_604))
                        .to_string();
                fixture
                    .connection
                    .insert_workspace(
                        &foreign_id,
                        &CreateWorkspaceInput {
                            path: "/procedural-source-other-workspace".to_owned(),
                            name: None,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                foreign_id
            } else {
                fixture.workspace_id.clone()
            };
            fixture
                .connection
                .insert_memory(
                    &source_id,
                    &CreateMemoryInput {
                        workspace_id: source_workspace,
                        level: "semantic".to_owned(),
                        kind: "note".to_owned(),
                        content: "Corroborating release evidence kept out of source errors."
                            .to_owned(),
                        workflow_id: None,
                        confidence: 0.7,
                        utility: 0.6,
                        importance: 0.5,
                        provenance_uri: None,
                        trust_class: "agent_validated".to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
            if posture == "tombstoned" {
                assert!(
                    fixture
                        .connection
                        .tombstone_memory(&source_id)
                        .map_err(|error| error.to_string())?
                );
            }
        }
        // The claimed ID remains in authenticated creation provenance even
        // when the normal publication helper would omit a missing/foreign row.
        bind_procedural_source_audit(&fixture, &source_id)?;
        let counts = fixture.counts()?;
        for dry_run in [true, false] {
            let error = fixture
                .apply(dry_run)
                .expect_err("all claimed source memories need authority");
            assert_private_refusal(
                &error.message(),
                "Corroborating release evidence kept out of source errors.",
                code,
            );
        }
        assert_eq!(fixture.counts()?, counts);
    }
    Ok(())
}

#[test]
fn procedural_source_admission_accepts_history_and_non_memory_review_targets() -> TestResult {
    let fixture = ProceduralFixture::new("rule", false)?;
    fixture
        .connection
        .execute_raw(&format!(
            "UPDATE memories SET valid_from = '2000-01-01T00:00:00Z', valid_to = '2001-01-01T00:00:00Z', superseded_at = '2001-01-01T00:00:00Z' WHERE id = '{}'",
            fixture.memory_id,
        ))
        .map_err(|error| error.to_string())?;
    fixture.withdraw(&fixture.memory_id, "sealed")?;
    assert!(
        fixture
            .connection
            .mark_memory_seal_revealed(&fixture.memory_id, "2026-05-24T00:06:00Z")
            .map_err(|error| error.to_string())?
    );
    fixture.withdraw(&fixture.memory_id, "held")?;
    fixture
        .connection
        .execute_raw("UPDATE feedback_quarantine SET target_type = 'rule'")
        .map_err(|error| error.to_string())?;
    // ADR 0043 permits historical, hash-locked evidence. A rule-target review
    // whose raw ID happens to match is not a pending review of this memory.
    let preview = fixture.apply(true).map_err(|error| error.message())?;
    assert_eq!(preview.application.status, "would_apply");
    assert!(!preview.durable_mutation);
    Ok(())
}

fn use_revision_source(fixture: &mut SourceFixture, logical_id: &str) -> TestResult {
    let original = fixture
        .connection
        .get_memory(&fixture.memory_id)
        .map_err(|error| error.to_string())?
        .ok_or("missing source root")?;
    let revision_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x73_a00)).to_string();
    fixture
        .connection
        .insert_memory_revision(
            &revision_id,
            logical_id,
            &CreateMemoryInput {
                workspace_id: original.workspace_id,
                level: original.level,
                kind: original.kind,
                content: original.content,
                workflow_id: original.workflow_id,
                confidence: original.confidence,
                utility: original.utility,
                importance: original.importance,
                provenance_uri: original.provenance_uri,
                trust_class: original.trust_class,
                trust_subclass: original.trust_subclass,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| error.to_string())?;
    fixture.memory_id = revision_id;
    Ok(())
}

#[test]
fn inherited_seals_guard_derivation_reflection_and_batched_source_selection() -> TestResult {
    let mut fixture = SourceFixture::new(0x73_900)?;
    let root_id = fixture.memory_id.clone();
    fixture.withdraw("sealed")?;
    use_revision_source(&mut fixture, &root_id)?;
    let body = fixture.source_body()?;
    let ids = [fixture.memory_id.as_str()];
    assert!(
        fixture
            .connection
            .get_memory_seal(&fixture.memory_id)
            .map_err(|error| error.to_string())?
            .is_none(),
        "the source has no direct sidecar; admission must follow its root"
    );
    let withheld = curate_core::source_admission::withheld_memory_ids(
        &fixture.connection,
        &fixture.workspace_id,
        &ids,
    )
    .map_err(|issue| issue.message)?;
    assert_eq!(withheld, [fixture.memory_id.clone()].into_iter().collect());
    assert!(!withheld.contains(&root_id), "mask the requested revision");
    for dry_run in [true, false] {
        let error = fixture.derive(dry_run).expect_err("inherited closed seal");
        assert_private_refusal(&error.message(), &body, posture_code("sealed"));
        for untyped_source in [true, false] {
            let error = fixture
                .reflect(dry_run, untyped_source)
                .expect_err("reflection must obey the same inherited seal");
            assert_private_refusal(&error.message(), &body, posture_code("sealed"));
        }
    }
    assert_eq!(fixture.candidate_count()?, 1);
    assert!(
        fixture
            .connection
            .query("SELECT request_id FROM reflection_request_ledger", &[])
            .map_err(|error| error.to_string())?
            .is_empty()
    );

    assert!(
        fixture
            .connection
            .mark_memory_seal_revealed(&root_id, "2026-05-24T00:06:00Z")
            .map_err(|error| error.to_string())?
    );
    fixture.derive(true).map_err(|error| error.message())?;
    fixture.reflect(true, false).map_err(|error| error.message())?;
    assert!(
        curate_core::source_admission::withheld_memory_ids(
            &fixture.connection,
            &fixture.workspace_id,
            &ids,
        )
        .map_err(|issue| issue.message)?
        .is_empty()
    );

    // A directly closed revision stays withheld even if the inherited seal
    // is already revealed. Whole sidecars, not individual fields, decide.
    fixture.withdraw("sealed")?;
    let error = fixture
        .derive(true)
        .expect_err("direct seal overrides root reveal");
    assert_private_refusal(&error.message(), &body, posture_code("sealed"));
    let withheld = curate_core::source_admission::withheld_memory_ids(
        &fixture.connection,
        &fixture.workspace_id,
        &ids,
    )
    .map_err(|issue| issue.message)?;
    assert_eq!(withheld, [fixture.memory_id.clone()].into_iter().collect());
    Ok(())
}

#[test]
fn broken_source_lineage_is_unavailable_even_with_a_direct_verified_reveal() -> TestResult {
    for direct_reveal in [false, true] {
        let mut fixture = SourceFixture::new(0x73_b00)?;
        let missing_root = MemoryId::from_uuid(uuid::Uuid::from_u128(0x73_b10)).to_string();
        use_revision_source(&mut fixture, &missing_root)?;
        let body = fixture.source_body()?;
        if direct_reveal {
            fixture.withdraw("sealed")?;
            fixture.release("sealed")?;
        }
        let before = fixture
            .connection
            .logical_table_digests()
            .map_err(|error| error.to_string())?;
        for dry_run in [true, false] {
            let error = fixture.derive(dry_run).expect_err("broken source lineage");
            assert_private_refusal(&error.message(), &body, posture_code("unavailable"));
            for untyped_source in [true, false] {
                let error = fixture
                    .reflect(dry_run, untyped_source)
                    .expect_err("broken reflection source lineage");
                assert_private_refusal(&error.message(), &body, posture_code("unavailable"));
            }
        }
        let issue = curate_core::source_admission::withheld_memory_ids(
            &fixture.connection,
            &fixture.workspace_id,
            &[fixture.memory_id.as_str()],
        )
        .expect_err("broken batch authority must not become an empty mask");
        assert_private_refusal(&issue.message, &body, "current memory source seal");
        assert_eq!(issue.code, posture_code("unavailable"));
        assert_eq!(
            fixture
                .connection
                .logical_table_digests()
                .map_err(|error| error.to_string())?,
            before,
            "refused proposals and previews must preserve all durable state"
        );
    }
    Ok(())
}
