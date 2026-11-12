//! Real-store discovery, review and apply, including source drift after the
//! apply plan has already been prepared.

use super::*;
use crate::core::init::{InitOptions, init_workspace};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Workspace {
    _temp: tempfile::TempDir,
    path: PathBuf,
    database: PathBuf,
    id: String,
    db: DbConnection,
}

impl Workspace {
    fn new() -> TestResult<Self> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().canonicalize()?;
        let report = init_workspace(&InitOptions {
            workspace_path: path.clone(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        });
        assert!(report.status.is_success(), "{report:?}");
        let db = DbConnection::open_file(&report.database_path)?;
        let workspace = db
            .get_workspace_by_path(&path.to_string_lossy())?
            .ok_or("workspace")?;
        Ok(Self {
            _temp: temp,
            path,
            database: report.database_path,
            id: workspace.id,
            db,
        })
    }

    fn memory(&self, seed: u128, body: &str, kind: &str, tags: &[&str]) -> TestResult<String> {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
        self.db.insert_memory(
            &id,
            &CreateMemoryInput {
                workspace_id: self.id.clone(),
                level: "episodic".to_owned(),
                kind: kind.to_owned(),
                content: body.to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: None,
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: None,
                tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
                valid_from: None,
                valid_to: None,
            },
        )?;
        Ok(id)
    }

    fn sources(&self, start: u128) -> TestResult<Vec<String>> {
        ["alpha", "beta", "gamma"]
            .iter()
            .enumerate()
            .map(|(index, label)| {
                self.memory(
                    start + index as u128,
                    &observation(label),
                    "fact",
                    &["cache"],
                )
            })
            .collect()
    }

    fn discover(&self, persist: bool, target: Option<&str>) -> TestResult<CurateCandidatesReport> {
        Ok(list_curation_candidates(&CurateCandidatesOptions {
            workspace_path: &self.path,
            database_path: Some(&self.database),
            candidate_type: Some("summary"),
            status: Some("pending"),
            target_memory_id: target,
            limit: 100,
            offset: 0,
            sort: "review_state",
            group_duplicates: false,
            propose: persist,
        })?)
    }

    fn approve(&self, id: &str) -> TestResult<StoredCurationCandidate> {
        let report = validate_curation_candidate(&CurateValidateOptions {
            workspace_path: &self.path,
            database_path: Some(&self.database),
            candidate_id: id,
            actor: Some("summary-test"),
            dry_run: false,
        })?;
        assert!(report.validation.errors.is_empty(), "{report:?}");
        let candidate = self
            .db
            .get_curation_candidate(&self.id, id)?
            .ok_or("candidate")?;
        assert_eq!(candidate.status, "approved");
        Ok(candidate)
    }

    fn apply(&self, id: &str) -> TestResult<CurateApplyReport> {
        Ok(apply_curation_candidate(&CurateApplyOptions {
            workspace_path: &self.path,
            database_path: Some(&self.database),
            candidate_id: id,
            actor: Some("summary-test"),
            dry_run: false,
            allow_tombstone_load_bearing: false,
        })?)
    }

    fn counts(&self) -> TestResult<(usize, usize, usize, usize)> {
        Ok((
            self.db.list_memories(&self.id, None, true)?.len(),
            self.db
                .list_curation_candidates(&self.id, None, None, None)?
                .len(),
            self.db.list_audit_entries(Some(&self.id), None)?.len(),
            self.db.list_search_index_jobs(&self.id, None)?.len(),
        ))
    }

    fn conflict(&self, left: &str, right: &str) -> TestResult {
        self.db.insert_memory_link(
            &format!("link_{}", "0".repeat(26)),
            &CreateMemoryLinkInput {
                src_memory_id: left.to_owned(),
                dst_memory_id: right.to_owned(),
                relation: MemoryLinkRelation::Contradicts,
                weight: 1.0,
                confidence: 1.0,
                directed: false,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Human,
                created_by: Some("summary-test".to_owned()),
                metadata_json: None,
            },
        )?;
        Ok(())
    }
}

fn observation(label: &str) -> String {
    format!(
        "Cache recovery rebuilt the pending search index from the local durable snapshot before serving requests during the {label} rehearsal."
    )
}

#[test]
fn episodic_summary_preview_propose_validate_apply_preserves_sources_and_lineage() -> TestResult {
    let workspace = Workspace::new()?;
    let ids = workspace.sources(100)?;
    workspace.memory(
        900,
        "Orchid irrigation requires warm rainwater and filtered sunlight.",
        "fact",
        &["plants"],
    )?;
    let before_sources = workspace.db.list_memories(&workspace.id, None, true)?;
    let before = workspace.counts()?;
    let preview = workspace.discover(false, None)?;
    assert_eq!(preview.candidates.len(), 1, "{preview:?}");
    assert_eq!(preview.filter.candidate_type.as_deref(), Some("summary"));
    assert!(!preview.durable_mutation);
    assert_eq!(workspace.counts()?, before);
    let candidate = &preview.candidates[0];
    assert_eq!(candidate.proposed_level.as_deref(), Some("semantic"));
    assert_eq!(candidate.proposed_kind.as_deref(), Some("summary"));
    assert!(!candidate.requires_validate && !candidate.requires_apply);
    assert!(candidate.next_action.contains("--type summary --propose"));

    let proposed = workspace.discover(true, None)?;
    assert!(proposed.durable_mutation);
    assert_eq!(proposed.candidates[0].id, candidate.id);
    let after_propose = workspace.counts()?;
    let repeat = workspace.discover(true, None)?;
    assert!(!repeat.durable_mutation);
    assert_eq!(workspace.counts()?, after_propose);
    let stored = workspace
        .db
        .get_curation_candidate(&workspace.id, &candidate.id)?
        .ok_or("candidate")?;
    let metadata = parse_derivation_metadata(&stored).map_err(|issue| issue.message)?;
    let coverage = metadata
        .producer
        .producer_payload
        .as_ref()
        .and_then(|payload| payload["sourceCoverage"].as_array())
        .ok_or("coverage")?;
    assert_eq!(coverage.len(), 3);
    for entry in coverage {
        let id = entry["memoryId"].as_str().ok_or("source id")?;
        assert!(ids.iter().any(|source| source == id));
        let source = workspace.db.get_memory(id)?.ok_or("source")?;
        let start = usize::try_from(entry["byteStart"].as_u64().ok_or("start")?)?;
        let end = usize::try_from(entry["byteEnd"].as_u64().ok_or("end")?)?;
        let excerpt = source.content.get(start..end).ok_or("UTF-8 span")?;
        assert_eq!(
            entry["excerptHash"].as_str(),
            Some(memory_content_hash(excerpt).as_str())
        );
        assert!(
            stored
                .proposed_content
                .as_deref()
                .ok_or("body")?
                .contains(excerpt)
        );
    }
    workspace.approve(&candidate.id)?;
    let applied = workspace.apply(&candidate.id)?;
    assert!(applied.mutation.persisted, "{applied:?}");
    let created_id = applied
        .application
        .created_memory_id
        .as_deref()
        .ok_or("derived memory")?;
    let created = workspace.db.get_memory(created_id)?.ok_or("derived row")?;
    assert_eq!(
        (&created.level[..], &created.kind[..]),
        ("semantic", "summary")
    );
    assert_eq!(created.trust_class, "agent_assertion");
    assert_eq!(created.confidence, CONFIDENCE_CAP);
    assert_eq!(workspace.db.get_memory_tags(created_id)?, vec!["cache"]);
    let links = workspace
        .db
        .list_memory_links_for_memory(created_id, Some(MemoryLinkRelation::DerivedFrom))?;
    assert_eq!(links.len(), ids.len());
    assert_eq!(
        links
            .iter()
            .map(|link| link.dst_memory_id.clone())
            .collect::<BTreeSet<_>>(),
        ids.into_iter().collect()
    );
    for original in before_sources {
        assert_eq!(
            workspace.db.get_memory(&original.id)?.as_ref(),
            Some(&original)
        );
    }
    assert_eq!(
        workspace
            .db
            .list_search_index_jobs(&workspace.id, None)?
            .len(),
        1
    );
    let after_apply = workspace.counts()?;
    let replay = workspace.apply(&candidate.id)?;
    assert!(!replay.mutation.persisted);
    assert_eq!(
        replay.application.created_memory_id.as_deref(),
        Some(created_id)
    );
    assert_eq!(workspace.counts()?, after_apply);
    assert!(workspace.discover(false, None)?.candidates.is_empty());
    assert!(!workspace.discover(true, None)?.durable_mutation);
    assert_eq!(workspace.counts()?, after_apply);
    Ok(())
}

#[test]
fn episodic_summary_fixture_is_topically_coherent_without_detected_opposition() -> TestResult {
    let workspace = Workspace::new()?;
    let ids = workspace.sources(100)?;
    let refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
    let mut metadata = workspace
        .db
        .memory_consolidation_metadata_in_current_snapshot(&refs)?;
    let sources = ids
        .iter()
        .map(|id| {
            Ok(Source {
                memory: workspace.db.get_memory(id)?.ok_or("source")?,
                metadata: metadata.remove(id).ok_or("metadata")?,
            })
        })
        .collect::<TestResult<Vec<_>>>()?;
    let clusters = clusters(&sources)?;
    assert_eq!(clusters.len(), 1, "{clusters:?}");
    assert!(clusters[0].accepted);
    assert!(clusters[0].average_internal_similarity >= TOPICAL_THRESHOLD);
    assert!(!conflicting(&workspace.db, &sources)?);
    Ok(())
}

#[test]
fn episodic_summary_source_coverage_replaces_only_the_generic_prose_gate() -> TestResult {
    let workspace = Workspace::new()?;
    workspace.sources(100)?;
    let report = workspace.discover(true, None)?;
    let stored = workspace
        .db
        .get_curation_candidate(
            &workspace.id,
            &report.candidates.first().ok_or("proposal")?.id,
        )?
        .ok_or("candidate")?;
    assert!(
        !crate::curate::specificity_score(stored.proposed_content.as_deref().ok_or("body")?)
            .passes_threshold
    );
    let now = Utc::now().to_rfc3339();
    assert!(
        evaluate_create_derived_candidate_for_validation(&workspace.db, &stored, &now, true)
            .validation
            .errors
            .is_empty()
    );

    let mut generic = stored.clone();
    let mut metadata = parse_derivation_metadata(&generic).map_err(|issue| issue.message)?;
    metadata.producer.producer = "manual_summary".to_owned();
    generic.derivation_metadata_json = Some(canonical_derivation_metadata_json(&metadata)?);
    generic.source_id = Some("manual_summary".to_owned());
    let rejected =
        evaluate_create_derived_candidate_for_validation(&workspace.db, &generic, &now, true);
    assert!(
        rejected
            .validation
            .errors
            .iter()
            .any(|issue| issue.code == crate::curate::CANDIDATE_TOO_GENERIC_CODE)
    );

    // A reserved source label alone cannot borrow the native exception.
    generic.source_id = Some(PRODUCER.to_owned());
    let spoofed =
        evaluate_create_derived_candidate_for_validation(&workspace.db, &generic, &now, true);
    assert!(
        spoofed
            .validation
            .errors
            .iter()
            .any(|issue| issue.code == "episodic_summary_source_changed")
    );

    let mut invented = stored.clone();
    invented.proposed_content =
        Some("The cache recovery observation proves every deployment will succeed.".to_owned());
    assert!(
        evaluate_create_derived_candidate_for_validation(&workspace.db, &invented, &now, true)
            .validation
            .errors
            .iter()
            .any(|issue| issue.code == "episodic_summary_source_changed")
    );

    let mut insufficient = stored.clone();
    let mut refs = parse_derivation_source_refs(&insufficient).map_err(|issue| issue.message)?;
    refs.pop();
    insufficient.derivation_source_refs_json = Some(canonical_derivation_source_refs_json(&refs)?);
    assert!(
        evaluate_create_derived_candidate_for_validation(&workspace.db, &insufficient, &now, true)
            .validation
            .errors
            .iter()
            .any(|issue| issue.code == "episodic_summary_source_changed")
    );
    Ok(())
}

#[test]
fn episodic_summary_rejected_candidate_is_not_recreated() -> TestResult {
    let workspace = Workspace::new()?;
    workspace.sources(100)?;
    let proposed = workspace.discover(true, None)?;
    let id = &proposed.candidates.first().ok_or("proposal")?.id;
    review_curation_candidate(&CurateReviewOptions {
        workspace_path: &workspace.path,
        database_path: Some(&workspace.database),
        candidate_id: id,
        action: CurateReviewAction::Reject,
        actor: Some("summary-test"),
        dry_run: false,
        snoozed_until: None,
        reason: Some("Keep individual observations"),
        merge_into_candidate_id: None,
    })?;
    let before = workspace.counts()?;
    assert!(workspace.discover(false, None)?.candidates.is_empty());
    assert!(workspace.discover(true, None)?.candidates.is_empty());
    assert_eq!(workspace.counts()?, before);
    assert_eq!(
        workspace
            .db
            .get_curation_candidate(&workspace.id, id)?
            .ok_or("candidate")?
            .status,
        "rejected"
    );
    Ok(())
}

#[test]
fn episodic_summary_intersects_applicability_and_respects_weakest_confidence() -> TestResult {
    let workspace = Workspace::new()?;
    let ids = workspace.sources(100)?;
    for (id, start, end) in [
        (&ids[0], "2000-01-01T00:00:00Z", "2100-01-01T00:00:00Z"),
        (&ids[1], "2001-01-01T00:00:00Z", "2101-01-01T00:00:00Z"),
        (&ids[2], "2002-01-01T00:00:00Z", "2102-01-01T00:00:00Z"),
    ] {
        workspace.db.set_memory_validity_for_diagnostic(
            id,
            Some(start),
            Some(end),
            false,
            false,
        )?;
    }
    workspace.db.apply_memory_curation_update(
        &ids[1],
        &ApplyMemoryCurationInput {
            workspace_id: workspace.id.clone(),
            content: observation("beta"),
            confidence: 0.42,
            trust_class: "agent_assertion".to_owned(),
        },
    )?;
    let proposed = workspace.discover(true, None)?;
    let candidate = workspace
        .db
        .get_curation_candidate(
            &workspace.id,
            &proposed.candidates.first().ok_or("proposal")?.id,
        )?
        .ok_or("candidate")?;
    let metadata = parse_derivation_metadata(&candidate).map_err(|issue| issue.message)?;
    assert_eq!(
        metadata.memory_spec.valid_from.as_deref(),
        Some("2002-01-01T00:00:00Z")
    );
    assert_eq!(
        metadata.memory_spec.valid_to.as_deref(),
        Some("2100-01-01T00:00:00Z")
    );
    assert_eq!(metadata.memory_spec.confidence, Some(0.42));
    workspace.approve(&candidate.id)?;
    let result = workspace.apply(&candidate.id)?;
    let created = workspace
        .db
        .get_memory(
            result
                .application
                .created_memory_id
                .as_deref()
                .ok_or("created memory")?,
        )?
        .ok_or("row")?;
    assert_eq!(created.valid_from, metadata.memory_spec.valid_from);
    assert_eq!(created.valid_to, metadata.memory_spec.valid_to);
    assert_eq!(created.confidence, 0.42);
    Ok(())
}

#[test]
fn episodic_summary_requires_distinct_compatible_sources_and_current_authority() -> TestResult {
    let workspace = Workspace::new()?;
    for seed in 1..=4 {
        workspace.memory(seed, &observation("same"), "fact", &["cache"])?;
    }
    assert!(workspace.discover(false, None)?.candidates.is_empty());
    workspace.memory(10, &observation("beta"), "fact", &["other-scope"])?;
    workspace.memory(11, &observation("gamma"), "decision", &["cache"])?;
    assert!(workspace.discover(false, None)?.candidates.is_empty());
    let ids = workspace.sources(100)?;
    workspace.db.insert_memory_seal(
        &ids[1],
        &memory_content_hash(&observation("beta")),
        &Utc::now().to_rfc3339(),
    )?;
    workspace.db.set_memory_validity_for_diagnostic(
        &ids[2],
        None,
        Some("2000-01-01T00:00:00Z"),
        false,
        false,
    )?;
    assert!(workspace.discover(false, None)?.candidates.is_empty());
    Ok(())
}

#[test]
fn episodic_summary_abstains_for_explicit_and_inferred_conflicts() -> TestResult {
    let workspace = Workspace::new()?;
    let ids = workspace.sources(100)?;
    workspace.conflict(&ids[0], &ids[1])?;
    assert!(workspace.discover(false, None)?.candidates.is_empty());

    let inferred = Workspace::new()?;
    for (index, body) in [
        "Cache recovery must rebuild the pending search index from the local durable snapshot before serving production requests.",
        "Cache recovery must not rebuild the pending search index from the local durable snapshot before serving production requests.",
        "Cache recovery should rebuild the pending search index from the local durable snapshot before serving production requests.",
    ].into_iter().enumerate() { inferred.memory(index as u128 + 1, body, "fact", &["cache"])?; }
    assert!(inferred.discover(false, None)?.candidates.is_empty());
    assert!(texts_conflict(
        "The database port is 5432.",
        "The database port is 6432."
    ));
    assert!(texts_conflict(
        "Run cargo fmt before cargo test.",
        "Run cargo test before cargo fmt."
    ));
    Ok(())
}

#[test]
fn episodic_summary_apply_rechecks_scope_expiry_and_conflicts_after_approval() -> TestResult {
    for drift in [
        "tag",
        "expiry",
        "conflict",
        "seal",
        "content",
        "tombstone",
        "hold",
    ] {
        let workspace = Workspace::new()?;
        let ids = workspace.sources(100)?;
        let proposed = workspace.discover(true, None)?;
        let candidate_id = &proposed.candidates.first().ok_or("proposal")?.id;
        workspace.approve(candidate_id)?;
        match drift {
            "tag" => workspace
                .db
                .add_memory_tags(&ids[0], &["restricted".to_owned()])?,
            "expiry" => {
                workspace.db.set_memory_validity_for_diagnostic(
                    &ids[0],
                    None,
                    Some("2000-01-01T00:00:00Z"),
                    false,
                    false,
                )?;
            }
            "conflict" => workspace.conflict(&ids[0], &ids[1])?,
            "seal" => workspace.db.insert_memory_seal(
                &ids[0],
                &memory_content_hash(&observation("alpha")),
                &Utc::now().to_rfc3339(),
            )?,
            "content" => {
                workspace.db.apply_memory_curation_update(
                    &ids[0],
                    &ApplyMemoryCurationInput {
                        workspace_id: workspace.id.clone(),
                        content: observation("changed"),
                        confidence: 0.8,
                        trust_class: "agent_assertion".to_owned(),
                    },
                )?;
            }
            "tombstone" => {
                workspace.db.tombstone_memory(&ids[0])?;
            }
            "hold" => {
                workspace.db.insert_feedback_quarantine(
                    &format!("fq_{}", "0".repeat(26)),
                    &crate::db::CreateFeedbackQuarantineInput {
                        workspace_id: workspace.id.clone(),
                        source_id: "summary-review".to_owned(),
                        target_type: "memory".to_owned(),
                        target_id: ids[0].clone(),
                        signal: "harmful".to_owned(),
                        weight: 1.0,
                        source_type: "human_explicit".to_owned(),
                        proposed_event_id: None,
                        recorded_at: Utc::now().to_rfc3339(),
                        reason: "Review source applicability".to_owned(),
                        event_reason: None,
                        evidence_json: None,
                        session_id: None,
                        raw_event_hash: "blake3:summary-review".to_owned(),
                    },
                )?;
            }
            _ => unreachable!(),
        }
        let before = workspace.counts()?;
        let report = workspace.apply(candidate_id)?;
        assert_eq!(report.application.status, "blocked", "{drift}: {report:?}");
        assert!(!report.mutation.persisted);
        assert!(
            report
                .application
                .errors
                .iter()
                .any(|issue| issue.code == "episodic_summary_source_changed")
        );
        assert_eq!(workspace.counts()?, before);
    }
    Ok(())
}

#[test]
fn episodic_summary_writer_revalidation_blocks_a_drifted_prepared_plan() -> TestResult {
    let workspace = Workspace::new()?;
    let ids = workspace.sources(100)?;
    let proposed = workspace.discover(true, None)?;
    let candidate = workspace.approve(&proposed.candidates.first().ok_or("proposal")?.id)?;
    let now = Utc::now().to_rfc3339();
    let plan = evaluate_create_derived_candidate_for_apply(&workspace.db, &candidate, &now, true);
    let derived = plan
        .derived_create
        .as_ref()
        .ok_or("prepared derived plan")?;
    workspace
        .db
        .add_memory_tags(&ids[0], &["new-scope".to_owned()])?;
    let before = workspace.counts()?;
    let result = workspace.db.with_transaction_error(|| {
        persist_create_derived_candidate_application_inner(
            &workspace.db,
            &workspace.id,
            &candidate,
            derived,
            &now,
            "summary-test",
        )
        .map_err(CandidateProposalTransactionError)
    });
    let error = result.err().ok_or("writer should reject changed scope")?;
    assert!(
        error
            .0
            .message()
            .contains("episodic_summary_source_changed")
    );
    assert_eq!(workspace.counts()?, before);
    assert!(workspace.db.get_memory(&derived.memory_id)?.is_none());
    Ok(())
}

#[test]
fn episodic_summary_bounded_discovery_reports_limits_and_targets_later_sources() -> TestResult {
    let workspace = Workspace::new()?;
    for seed in 1..=(MAX_SOURCES as u128 + 1) {
        workspace.memory(
            seed,
            "Unrelated irrigation observation.",
            "observation",
            &["garden"],
        )?;
    }
    let ids = workspace.sources(1000)?;
    let preview = workspace.discover(false, None)?;
    assert!(preview.candidates.is_empty());
    assert_eq!(preview.degraded.len(), 1);
    assert_eq!(preview.degraded[0].code, SOURCE_LIMIT_CODE);
    assert!(
        preview.degraded[0]
            .message
            .contains("discovery is incomplete")
    );
    let targeted = workspace.discover(false, Some(&ids[2]))?;
    assert_eq!(targeted.candidates.len(), 1, "{targeted:?}");
    assert!(targeted.degraded.is_empty());
    let proposed = workspace.discover(true, Some(&ids[2]))?;
    assert_eq!(proposed.candidates[0].id, targeted.candidates[0].id);
    Ok(())
}

#[test]
fn episodic_summary_cancellation_never_creates_partial_proposals() -> Result<(), String> {
    let workspace = Workspace::new().map_err(|error| error.to_string())?;
    workspace.sources(100).map_err(|error| error.to_string())?;
    let before = workspace.counts().map_err(|error| error.to_string())?;
    let active_workspace = &workspace;
    crate::core::run_cli_with_cx(std::time::Duration::from_secs(10), |cx| async move {
        cx.set_cancel_reason(asupersync::CancelReason::user("stop summary discovery"));
        assert!(active_workspace.discover(true, None).is_err());
    })
    .map_err(|error| error.to_string())?;
    assert_eq!(
        workspace.counts().map_err(|error| error.to_string())?,
        before
    );
    Ok(())
}
