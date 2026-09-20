//! Source-snapshot admission for every diagnostic arm, not only final hits.
//!
//! A raw rank is still an observation of a memory. Filtering only `final` can
//! expose superseded, tombstoned, expired, sealed or out-of-scope IDs in
//! preFusion/fusion. Revision identity and author expiry are separate gates.
//! Keep allowed below-floor candidates and their original rank/score; visibility
//! is not a relevance floor, and removing a row must not invent a new ranking.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    DbConnection, DiagSearchSyncResult, MeshQueryVisibility, ScoreSource, SearchDegradation,
    SearchError, SearchHit, SearchOptions, apply_memory_scope_visibility,
    apply_tombstone_visibility_collecting, mesh_query_visibility, search_checkpoint,
};

fn admission_error() -> SearchError {
    SearchError::Index(
        "Diagnostic memory admission could not be verified in the source snapshot; results withheld"
            .to_owned(),
    )
}

pub(super) fn admit_memories(
    cx: &asupersync::Cx,
    options: &SearchOptions,
    diag: &mut DiagSearchSyncResult,
    degraded: &mut Vec<SearchDegradation>,
    read_connection: Option<&DbConnection>,
) -> Result<(), SearchError> {
    search_checkpoint(cx)?;
    let Some(connection) = read_connection else {
        // Preserve deliberately database-free synthetic index diagnostics.
        // Production database-backed diagnostics always supply a pinned reader.
        return Ok(());
    };
    let workspace = crate::core::workspace::addressed_workspace_row(
        connection,
        &options.workspace_path,
        &options.resolve_database_path(),
    )
    .map_err(|error| SearchError::WorkspaceBinding(Box::new(error)))?
    .ok_or_else(admission_error)?;
    let ids: BTreeSet<String> = diag
        .pre_fusion
        .lexical
        .results
        .iter()
        .chain(&diag.pre_fusion.semantic_fast.results)
        .map(|hit| hit.doc_id.as_str())
        .chain(
            diag.fusion
                .per_doc_contribution
                .iter()
                .map(|hit| hit.doc_id.as_str()),
        )
        .chain(diag.final_hits.iter().map(|hit| hit.doc_id.as_str()))
        .filter(|id| id.starts_with("mem_"))
        .map(str::to_owned)
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    // Fail closed here, before invoking shared best-effort presentation helpers.
    // Only candidate IDs are loaded; there is no corpus-wide fallback scan.
    let rows = connection
        .get_memories_batch(&refs)
        .map_err(|_| admission_error())?;
    let final_by_id: BTreeMap<&str, &SearchHit> = diag
        .final_hits
        .iter()
        .map(|hit| (hit.doc_id.as_str(), hit))
        .collect();
    let mut candidates = Vec::with_capacity(ids.len());
    let mut unbound = 0;
    for id in &ids {
        search_checkpoint(cx)?;
        let Some(row) = rows.get(id).filter(|row| row.workspace_id == workspace.id) else {
            unbound += 1;
            continue;
        };
        // Source truth is mandatory even when a stale index contains plaintext
        // instead of the seal placeholder. A failed seal lookup cannot authorize
        // a diagnostic read of material the canonical projection omits.
        if connection
            .get_memory_seal(&row.id)
            .map_err(|_| admission_error())?
            .is_some_and(|seal| seal.is_sealed())
        {
            continue;
        }
        let mut hit = final_by_id.get(id.as_str()).map_or_else(
            || SearchHit {
                doc_id: id.clone(),
                score: 0.0,
                source: ScoreSource::Lexical,
                fast_score: None,
                quality_score: None,
                lexical_score: None,
                rerank_score: None,
                metadata: None,
                explanation: None,
            },
            |hit| (**hit).clone(),
        );
        if hit.metadata.is_none() {
            hit.metadata = diag.candidate_metadata.get(id).cloned();
        }
        candidates.push(hit);
    }
    let mut admission_degraded = Vec::new();
    let candidates = super::rule_admission::memory_revisions::admit_hits(
        options,
        candidates,
        &mut admission_degraded,
        Some(connection),
    );
    let candidates = apply_tombstone_visibility_collecting(
        options,
        candidates,
        &mut admission_degraded,
        Some(connection),
        None,
    );
    let (candidates, _) = apply_memory_scope_visibility(
        options,
        candidates,
        &mut admission_degraded,
        Some(connection),
    );
    if admission_degraded.iter().any(|entry| {
        entry.code == super::rule_admission::memory_revisions::UNAVAILABLE
            || matches!(
                entry.code.as_str(),
                "tombstone_visibility_unavailable" | "scope_metadata_unavailable"
            )
    }) {
        return Err(admission_error());
    }
    // Admission only: the final path applies mesh trust adjustment once, while
    // raw arm scores and RRF contributions retain their original math.
    let admitted: BTreeSet<String> = candidates
        .into_iter()
        .filter(|hit| {
            !matches!(
                mesh_query_visibility(hit.metadata.as_ref()),
                MeshQueryVisibility::Blocked
            )
        })
        .map(|hit| hit.doc_id)
        .collect();
    search_checkpoint(cx)?;
    let visible = |id: &str| !id.starts_with("mem_") || admitted.contains(id);
    diag.pre_fusion
        .lexical
        .results
        .retain(|hit| visible(&hit.doc_id));
    diag.pre_fusion
        .semantic_fast
        .results
        .retain(|hit| visible(&hit.doc_id));
    diag.fusion
        .per_doc_contribution
        .retain(|hit| visible(&hit.doc_id));
    diag.final_hits.retain(|hit| visible(&hit.doc_id));
    if unbound > 0 {
        admission_degraded.push(SearchDegradation::orphaned_index_rows_filtered(unbound));
    }
    for entry in admission_degraded {
        if !degraded.contains(&entry) {
            degraded.push(entry);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "lexical-bm25")]
    use super::super::run_diag_search;
    #[cfg(all(unix, feature = "lexical-bm25"))]
    use super::super::run_diag_search_in_snapshot;
    use super::super::{
        FusionContribution, FusionDiagnostics, PreFusionDiagnostics, SearchArmDiagnostics,
        SearchArmHit, SearchDedupMode, SearchSourceMode, SpeedMode,
    };
    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
    use crate::models::MemoryScope;
    use serde_json::json;
    #[cfg(feature = "lexical-bm25")]
    use std::path::{Path, PathBuf};
    #[cfg(feature = "lexical-bm25")]
    use std::sync::Arc;
    #[cfg(feature = "lexical-bm25")]
    use std::time::Duration;

    type TestResult = Result<(), String>;
    const WORKSPACE: &str = "wsp_00000000000000000000000001";
    const OTHER_WORKSPACE: &str = "wsp_00000000000000000000000002";
    const VISIBLE: &str = "mem_00000000000000000000000001";
    const HIDDEN: &str = "mem_00000000000000000000000002";
    const PHRASE: &str = "quartz snapshot diagnostic recovery preserves evidence";

    fn input(workspace: &str, text: &str) -> CreateMemoryInput {
        CreateMemoryInput {
            workspace_id: workspace.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: text.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.7,
            importance: 0.6,
            provenance_uri: Some("manual://diagnostic-snapshot".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            // The fixture queries at a fixed August instant. An ambient
            // creation-time default turns its visible control into a future row.
            valid_from: Some("2026-01-01T00:00:00Z".to_owned()),
            valid_to: None,
        }
    }

    fn fixture() -> Result<(tempfile::TempDir, SearchOptions, DbConnection), String> {
        let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let root = temp.path().canonicalize().map_err(|e| e.to_string())?;
        std::fs::create_dir(root.join(".ee")).map_err(|e| e.to_string())?;
        let database = root.join(".ee/ee.db");
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        db.migrate().map_err(|e| e.to_string())?;
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: root.display().to_string(),
                name: Some("diagnostic-snapshot".to_owned()),
            },
        )
        .map_err(|e| e.to_string())?;
        let options = SearchOptions {
            workspace_path: root.clone(),
            database_path: Some(database),
            index_dir: Some(root.join(".ee/index")),
            query: PHRASE.to_owned(),
            limit: 5,
            speed: SpeedMode::Default,
            explain: true,
            as_of: Some(
                chrono::DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
                    .map_err(|e| e.to_string())?
                    .with_timezone(&chrono::Utc),
            ),
            include_tombstoned: false,
            include_expired: false,
            include_future: false,
            include_stale: false,
            relevance_floor: Some(0.0),
            dedup_mode: SearchDedupMode::DocId,
            source_mode: SearchSourceMode::LexicalOnly,
            strict_source_mode: false,
            memory_scope: MemoryScope::Workspace,
            strict_scope: false,
        };
        Ok((temp, options, db))
    }

    fn hit(id: &str) -> SearchHit {
        SearchHit {
            doc_id: id.to_owned(),
            score: 0.01,
            source: ScoreSource::Lexical,
            fast_score: None,
            quality_score: None,
            lexical_score: Some(0.01),
            rerank_score: None,
            metadata: None,
            explanation: None,
        }
    }

    fn diagnostic(ids: &[&str], final_ids: &[&str]) -> DiagSearchSyncResult {
        let results: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| SearchArmHit {
                doc_id: (*id).to_owned(),
                rank: index + 1,
                raw_score: 1.0 / (index + 1) as f32,
            })
            .collect();
        let arm = SearchArmDiagnostics {
            available: true,
            score_scale: "fixture",
            elapsed_ms: 0.0,
            results,
            error: None,
        };
        DiagSearchSyncResult {
            candidate_metadata: BTreeMap::new(),
            pre_fusion: PreFusionDiagnostics {
                lexical: arm.clone(),
                semantic_fast: arm,
            },
            fusion: FusionDiagnostics {
                algorithm: "reciprocal_rank_fusion",
                rrf_k: 60.0,
                elapsed_ms: 0.0,
                per_doc_contribution: ids
                    .iter()
                    .enumerate()
                    .map(|(rank, id)| FusionContribution {
                        doc_id: (*id).to_owned(),
                        lexical_rank: Some(rank + 1),
                        semantic_rank: Some(rank + 1),
                        lexical_contribution: Some(0.001),
                        semantic_contribution: Some(0.002),
                        fused_score: 0.003,
                    })
                    .collect(),
            },
            final_hits: final_ids.iter().map(|id| hit(id)).collect(),
            final_elapsed_ms: 0.0,
            errors: Vec::new(),
        }
    }

    fn assert_arms(diag: &DiagSearchSyncResult, expected: &[&str]) {
        for arm in [&diag.pre_fusion.lexical, &diag.pre_fusion.semantic_fast] {
            assert_eq!(
                arm.results
                    .iter()
                    .map(|hit| hit.doc_id.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
        }
        assert_eq!(
            diag.fusion
                .per_doc_contribution
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
    }

    fn revision_fixture() -> Result<(tempfile::TempDir, SearchOptions, DbConnection), String> {
        let (temp, options, db) = fixture()?;
        for (id, created, text) in [
            (
                HIDDEN,
                "2026-05-01T00:00:00Z",
                "Original diagnostic evidence.",
            ),
            (VISIBLE, "2026-06-01T00:00:00Z", PHRASE),
        ] {
            let mut record = input(WORKSPACE, text);
            record.valid_from = Some(created.to_owned());
            record.valid_to = Some("2099-01-01T00:00:00Z".to_owned());
            db.insert_memory_with_timestamps(id, &record, created, created, HIDDEN)
                .map_err(|e| e.to_string())?;
        }
        assert!(
            db.restore_imported_memory_supersession(HIDDEN, "2026-06-01T00:00:00Z")
                .map_err(|e| e.to_string())?
        );
        Ok((temp, options, db))
    }

    #[test]
    fn diagnostic_revision_admission_removes_history_from_every_arm_without_rewriting_scores()
    -> TestResult {
        let (_temp, mut options, db) = revision_fixture()?;
        options.include_expired = true;
        options.include_stale = true;
        options.include_tombstoned = true;
        options.relevance_floor = Some(1.0);
        let before = db
            .list_memories(WORKSPACE, None, true)
            .map_err(|e| e.to_string())?;
        for final_ids in [&[HIDDEN][..], &[HIDDEN, VISIBLE][..]] {
            let mut diag = diagnostic(&[HIDDEN, VISIBLE], final_ids);
            diag.candidate_metadata.insert(
                HIDDEN.to_owned(),
                json!({"superseded_at": null, "current_revision": true}),
            );
            let raw = diag.pre_fusion.lexical.results[1].clone();
            let mut degraded = Vec::new();
            admit_memories(
                &asupersync::Cx::for_testing(),
                &options,
                &mut diag,
                &mut degraded,
                Some(&db),
            )
            .map_err(|e| e.to_string())?;
            assert_arms(&diag, &[VISIBLE]);
            assert_eq!(diag.pre_fusion.lexical.results[0].rank, raw.rank);
            assert_eq!(
                diag.pre_fusion.lexical.results[0].raw_score.to_bits(),
                raw.raw_score.to_bits()
            );
            assert_eq!(diag.pre_fusion.semantic_fast.results[0].rank, 2);
            assert_eq!(diag.fusion.per_doc_contribution[0].lexical_rank, Some(2));
            assert_eq!(
                diag.fusion.per_doc_contribution[0].fused_score.to_bits(),
                0.003_f64.to_bits()
            );
            assert_eq!(
                diag.final_hits
                    .iter()
                    .map(|hit| hit.doc_id.as_str())
                    .collect::<Vec<_>>(),
                final_ids
                    .iter()
                    .copied()
                    .filter(|id| *id == VISIBLE)
                    .collect::<Vec<_>>(),
            );
            assert!(
                degraded
                    .iter()
                    .any(|entry| entry.code == "superseded_revision_filtered")
            );
            assert!(!degraded.iter().any(|entry| entry.message.contains(HIDDEN)));
        }
        assert_eq!(
            db.list_memories(WORKSPACE, None, true)
                .map_err(|e| e.to_string())?,
            before
        );
        Ok(())
    }

    #[test]
    fn diagnostic_history_changes_at_the_exact_supersession_instant() -> TestResult {
        let (_temp, mut options, db) = revision_fixture()?;
        db.begin_read_snapshot().map_err(|e| e.to_string())?;
        for (reference, expected, rank) in [
            ("2026-05-15T00:00:00Z", HIDDEN, 1),
            ("2026-05-31T23:59:59.999999999Z", HIDDEN, 1),
            ("2026-06-01T01:00:00+01:00", VISIBLE, 2),
        ] {
            options.as_of = Some(
                chrono::DateTime::parse_from_rfc3339(reference)
                    .map_err(|e| e.to_string())?
                    .with_timezone(&chrono::Utc),
            );
            let mut diag = diagnostic(&[HIDDEN, VISIBLE], &[HIDDEN, VISIBLE]);
            admit_memories(
                &asupersync::Cx::for_testing(),
                &options,
                &mut diag,
                &mut Vec::new(),
                Some(&db),
            )
            .map_err(|e| e.to_string())?;
            assert_arms(&diag, &[expected]);
            assert_eq!(diag.final_hits.len(), 1);
            assert_eq!(diag.final_hits[0].doc_id, expected);
            assert_eq!(diag.pre_fusion.lexical.results[0].rank, rank);
        }
        db.commit_read_snapshot().map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn malformed_diagnostic_revision_authority_fails_without_echoing_source_values() -> TestResult {
        let (_temp, options, db) = revision_fixture()?;
        db.execute_raw(&format!(
            "UPDATE memories SET superseded_at = 'PRIVATE_DIAGNOSTIC_REVISION' WHERE id = '{VISIBLE}'"
        ))
        .map_err(|e| e.to_string())?;
        let mut diag = diagnostic(&[VISIBLE], &[VISIBLE]);
        let error = admit_memories(
            &asupersync::Cx::for_testing(),
            &options,
            &mut diag,
            &mut Vec::new(),
            Some(&db),
        )
        .expect_err("unverified raw ranks must not be returned");
        assert!(error.to_string().contains("source snapshot"));
        assert!(!error.to_string().contains("PRIVATE_DIAGNOSTIC_REVISION"));
        assert!(!error.to_string().contains(VISIBLE));
        Ok(())
    }

    #[test]
    fn diagnostic_admission_filters_every_arm_without_reranking_or_applying_the_floor() -> TestResult
    {
        let (_temp, mut options, db) = fixture()?;
        db.insert_memory(VISIBLE, &input(WORKSPACE, PHRASE))
            .map_err(|e| e.to_string())?;
        db.insert_memory(HIDDEN, &input(WORKSPACE, "private tombstoned evidence"))
            .map_err(|e| e.to_string())?;
        assert!(db.tombstone_memory(HIDDEN).map_err(|e| e.to_string())?);
        let expired = "mem_00000000000000000000000003";
        let future = "mem_00000000000000000000000004";
        let mut record = input(WORKSPACE, "private expired evidence");
        record.valid_to = Some("2026-01-01T00:00:00Z".to_owned());
        db.insert_memory(expired, &record)
            .map_err(|e| e.to_string())?;
        record.valid_to = None;
        record.valid_from = Some("2030-01-01T00:00:00Z".to_owned());
        db.insert_memory(future, &record)
            .map_err(|e| e.to_string())?;
        let foreign = "mem_00000000000000000000000005";
        db.insert_workspace(
            OTHER_WORKSPACE,
            &CreateWorkspaceInput {
                path: options.workspace_path.join("other").display().to_string(),
                name: None,
            },
        )
        .map_err(|e| e.to_string())?;
        db.insert_memory(foreign, &input(OTHER_WORKSPACE, "private foreign evidence"))
            .map_err(|e| e.to_string())?;
        let mut diag = diagnostic(
            &[HIDDEN, expired, future, foreign, "mem_absent", VISIBLE],
            &[HIDDEN],
        );
        let raw = diag
            .pre_fusion
            .lexical
            .results
            .last()
            .expect("visible raw hit")
            .clone();
        options.relevance_floor = Some(1.0);
        let mut degraded = Vec::new();
        admit_memories(
            &asupersync::Cx::for_testing(),
            &options,
            &mut diag,
            &mut degraded,
            Some(&db),
        )
        .map_err(|e| e.to_string())?;
        assert_arms(&diag, &[VISIBLE]);
        assert!(
            diag.final_hits.is_empty(),
            "raw-only admissible evidence must remain diagnosable"
        );
        assert_eq!(diag.pre_fusion.lexical.results[0].rank, raw.rank);
        assert_eq!(
            diag.pre_fusion.lexical.results[0].raw_score.to_bits(),
            raw.raw_score.to_bits()
        );
        assert_eq!(diag.fusion.per_doc_contribution[0].lexical_rank, Some(6));
        assert_eq!(
            diag.fusion.per_doc_contribution[0].fused_score.to_bits(),
            0.003_f64.to_bits()
        );
        assert!(
            degraded
                .iter()
                .any(|entry| entry.code == "tombstoned_filtered")
        );
        Ok(())
    }

    #[test]
    fn diagnostic_admission_obeys_explicit_history_flags_and_authoritative_global_tags()
    -> TestResult {
        let (_temp, mut options, db) = fixture()?;
        let mut record = input(WORKSPACE, PHRASE);
        record.valid_to = Some("2026-01-01T00:00:00Z".to_owned());
        record.tags = vec![crate::models::GLOBAL_MEMORY_SCOPE_TAG.to_owned()];
        db.insert_memory(VISIBLE, &record)
            .map_err(|e| e.to_string())?;
        assert!(db.tombstone_memory(VISIBLE).map_err(|e| e.to_string())?);
        db.insert_memory(HIDDEN, &input(WORKSPACE, "local-only evidence"))
            .map_err(|e| e.to_string())?;
        options.include_tombstoned = true;
        options.include_expired = true;
        options.memory_scope = MemoryScope::Global;
        let mut diag = diagnostic(&[HIDDEN, VISIBLE], &[HIDDEN, VISIBLE]);
        // A stale index's tag is not authority to promote a local memory to global.
        diag.candidate_metadata.insert(
            HIDDEN.to_owned(),
            json!({"tags": [crate::models::GLOBAL_MEMORY_SCOPE_TAG]}),
        );
        admit_memories(
            &asupersync::Cx::for_testing(),
            &options,
            &mut diag,
            &mut Vec::new(),
            Some(&db),
        )
        .map_err(|e| e.to_string())?;
        assert_arms(&diag, &[VISIBLE]);
        assert_eq!(
            diag.final_hits
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            [VISIBLE]
        );
        Ok(())
    }

    #[test]
    fn diagnostic_admission_checks_raw_only_seals_validity_metadata_and_mesh_denial() -> TestResult
    {
        let (_temp, mut options, db) = fixture()?;
        let sealed = "mem_00000000000000000000000006";
        let stale = "mem_00000000000000000000000007";
        let mesh = "mem_00000000000000000000000008";
        for id in [VISIBLE, sealed, stale, mesh] {
            db.insert_memory(id, &input(WORKSPACE, PHRASE))
                .map_err(|e| e.to_string())?;
        }
        db.insert_memory_seal(
            sealed,
            &crate::models::memory_seal_commitment(PHRASE.as_bytes()),
            "2026-07-01T00:00:00Z",
        )
        .map_err(|e| e.to_string())?;
        let make_diag = || {
            let mut diag = diagnostic(&[sealed, stale, mesh, VISIBLE], &[]);
            diag.candidate_metadata
                .insert(stale.to_owned(), json!({"validityStatus": "stale"}));
            diag.candidate_metadata
                .insert(mesh.to_owned(), json!({"workspaceScopeDecision": "deny"}));
            diag
        };
        let mut diag = make_diag();
        admit_memories(
            &asupersync::Cx::for_testing(),
            &options,
            &mut diag,
            &mut Vec::new(),
            Some(&db),
        )
        .map_err(|e| e.to_string())?;
        assert_arms(&diag, &[VISIBLE]);
        options.include_stale = true;
        let mut diag = make_diag();
        admit_memories(
            &asupersync::Cx::for_testing(),
            &options,
            &mut diag,
            &mut Vec::new(),
            Some(&db),
        )
        .map_err(|e| e.to_string())?;
        assert_arms(&diag, &[stale, VISIBLE]);
        Ok(())
    }

    #[test]
    fn diagnostic_admission_preserves_verified_reveals_without_reviving_closed_seals() -> TestResult
    {
        let (_temp, options, db) = fixture()?;
        db.insert_memory(VISIBLE, &input(WORKSPACE, PHRASE))
            .map_err(|e| e.to_string())?;
        db.insert_memory(HIDDEN, &input(WORKSPACE, PHRASE))
            .map_err(|e| e.to_string())?;
        for id in [VISIBLE, HIDDEN] {
            db.insert_memory_seal(
                id,
                &crate::models::memory_seal_commitment(PHRASE.as_bytes()),
                "2026-07-01T00:00:00Z",
            )
            .map_err(|e| e.to_string())?;
        }
        let mut closed = diagnostic(&[HIDDEN, VISIBLE], &[HIDDEN, VISIBLE]);
        admit_memories(
            &asupersync::Cx::for_testing(),
            &options,
            &mut closed,
            &mut Vec::new(),
            Some(&db),
        )
        .map_err(|e| e.to_string())?;
        assert_arms(&closed, &[]);
        assert!(closed.final_hits.is_empty());
        assert!(
            db.mark_memory_seal_revealed(VISIBLE, "2026-07-02T00:00:00Z")
                .map_err(|e| e.to_string())?
        );
        let mut revealed = diagnostic(&[HIDDEN, VISIBLE], &[HIDDEN, VISIBLE]);
        admit_memories(
            &asupersync::Cx::for_testing(),
            &options,
            &mut revealed,
            &mut Vec::new(),
            Some(&db),
        )
        .map_err(|e| e.to_string())?;
        assert_arms(&revealed, &[VISIBLE]);
        assert_eq!(
            revealed
                .final_hits
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            [VISIBLE]
        );
        assert_eq!(revealed.pre_fusion.lexical.results[0].rank, 2);
        assert!(
            db.get_memory_seal(HIDDEN)
                .map_err(|e| e.to_string())?
                .ok_or("closed seal")?
                .is_sealed()
        );
        Ok(())
    }

    #[test]
    fn diagnostic_admission_withholds_on_source_failure_and_preserves_cancellation() -> TestResult {
        let (_temp, options, db) = fixture()?;
        db.insert_memory(VISIBLE, &input(WORKSPACE, PHRASE))
            .map_err(|e| e.to_string())?;
        db.execute_raw("ALTER TABLE memory_seals RENAME TO unavailable_memory_seals")
            .map_err(|e| e.to_string())?;
        let mut diag = diagnostic(&[VISIBLE], &[VISIBLE]);
        let result = admit_memories(
            &asupersync::Cx::for_testing(),
            &options,
            &mut diag,
            &mut Vec::new(),
            Some(&db),
        );
        assert!(
            matches!(result, Err(SearchError::Index(message)) if message.contains("withheld") && !message.contains(VISIBLE))
        );
        let cx = asupersync::Cx::for_testing();
        cx.set_cancel_reason(asupersync::CancelReason::user(
            "cancel diagnostic admission",
        ));
        assert!(
            matches!(admit_memories(&cx, &options, &mut diag, &mut Vec::new(), Some(&db)),
            Err(SearchError::Cancelled(reason)) if reason.message.as_deref() == Some("cancel diagnostic admission"))
        );
        Ok(())
    }

    #[test]
    fn diagnostic_evidence_uses_admitted_source_content_not_stale_index_metadata() -> TestResult {
        use crate::db::{CreateEvidenceSpanInput, CreateSessionInput, EvidenceProducerKind};
        use crate::models::{EvidenceId, SessionId};
        let (_temp, options, db) = fixture()?;
        let session_id = SessionId::from_uuid(uuid::Uuid::from_u128(0x5a_001)).to_string();
        let evidence_id = EvidenceId::from_uuid(uuid::Uuid::from_u128(0x5a_002)).to_string();
        let hash = format!("blake3:{}", blake3::hash(PHRASE.as_bytes()).to_hex());
        db.insert_session(
            &session_id,
            &CreateSessionInput {
                workspace_id: WORKSPACE.to_owned(),
                cass_session_id: "diagnostic-source-session".to_owned(),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: Some("2026-07-01T00:00:00Z".to_owned()),
                ended_at: None,
                message_count: 1,
                token_count: Some(10),
                content_hash: hash.clone(),
                metadata_json: None,
            },
        )
        .map_err(|e| e.to_string())?;
        db.insert_evidence_span(
            &evidence_id,
            &CreateEvidenceSpanInput {
                workspace_id: WORKSPACE.to_owned(),
                session_id,
                memory_id: None,
                producer_kind: EvidenceProducerKind::CassImport,
                cass_span_id: "diagnostic-source-span".to_owned(),
                span_kind: "message".to_owned(),
                start_line: 1,
                end_line: 1,
                start_byte: None,
                end_byte: None,
                role: Some("assistant".to_owned()),
                excerpt: PHRASE.to_owned(),
                content_hash: hash,
                metadata_json: None,
                inherited_redaction_classes: Vec::new(),
            },
        )
        .map_err(|e| e.to_string())?;
        let span = db
            .get_evidence_span(&evidence_id)
            .map_err(|e| e.to_string())?
            .ok_or("evidence")?;
        let expected = super::super::canonical_evidence_search_metadata(&span);
        let mut diag = diagnostic(&[&evidence_id], &[&evidence_id]);
        diag.final_hits[0].metadata = Some(json!({"content": "private obsolete indexed excerpt"}));
        super::super::apply_live_evidence_visibility_to_diag(
            &options,
            &mut diag,
            &mut Vec::new(),
            Some(&db),
        );
        assert_arms(&diag, &[&evidence_id]);
        assert_eq!(
            diag.final_hits.len(),
            1,
            "the positive evidence fixture must be admitted"
        );
        assert_eq!(diag.final_hits[0].metadata.as_ref(), Some(&expected));
        assert!(
            !serde_json::to_string(&diag.final_hits)
                .map_err(|e| e.to_string())?
                .contains("private obsolete")
        );
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    async fn build_index(cx: &asupersync::Cx, dir: &Path, generation: u64) -> TestResult {
        use crate::search::{
            Embedder, EmbedderStack, HashEmbedder, IndexBuilder, IndexableDocument,
        };
        let docs = vec![IndexableDocument::new(VISIBLE, PHRASE)];
        IndexBuilder::new(dir)
            .with_embedder_stack(EmbedderStack::from_parts(
                Arc::new(HashEmbedder::default_256()) as Arc<dyn Embedder>,
                None,
            ))
            .add_documents(docs.clone())
            .build(cx)
            .await
            .map_err(|e| e.to_string())?;
        crate::core::index::build_lexical_tier(cx, dir, &docs)
            .await
            .map_err(|e| e.to_string())?;
        crate::core::index::write_memory_eval_index_metadata_for_generation(dir, generation, 1)
            .map_err(|e| e.to_string())
    }

    #[cfg(feature = "lexical-bm25")]
    fn index_bytes(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
        let mut files = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
                    pending.push(entry.map_err(|e| e.to_string())?.path());
                }
            } else {
                files.insert(
                    path.clone(),
                    std::fs::read(path).map_err(|e| e.to_string())?,
                );
            }
        }
        Ok(files)
    }

    #[cfg(all(unix, feature = "lexical-bm25"))]
    #[test]
    fn diagnostic_snapshot_recovers_missing_and_corrupt_live_tiers_through_public_entry()
    -> TestResult {
        for corruption in ["missing", "vector", "lexical"] {
            let (_temp, options, db) = fixture()?;
            db.insert_memory(VISIBLE, &input(WORKSPACE, PHRASE))
                .map_err(|e| e.to_string())?;
            let generation = db
                .get_workspace_generation(WORKSPACE)
                .map_err(|e| e.to_string())?
                .ok_or("generation")?;
            db.close().map_err(|e| e.to_string())?;
            let index = options.resolve_index_dir();
            let retained = index.parent().ok_or("index parent")?.join("index.previous");
            let (retained_ref, index_ref) = (&retained, &index);
            crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
                build_index(&cx, retained_ref, generation).await?;
                if corruption != "missing" {
                    build_index(&cx, index_ref, generation).await?;
                    if corruption == "vector" {
                        std::fs::write(index_ref.join("vector.fast.idx"), b"corrupt")
                            .map_err(|e| e.to_string())?;
                    } else {
                        // Keep the files for the byte-preservation assertion.
                        std::fs::rename(
                            index_ref.join("lexical"),
                            index_ref.join("broken-lexical"),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                }
                Ok::<(), String>(())
            })
            .map_err(|e| e.to_string())??;
            let before_retained = index_bytes(&retained)?;
            let report = run_diag_search(&options).map_err(|e| format!("{corruption}: {e}"))?;
            let serialized = report.data_json();
            assert_eq!(
                serialized["final"]["indexFreshness"]["dbGeneration"],
                generation
            );
            assert_eq!(
                serialized["final"]["indexFreshness"]["indexGeneration"],
                generation
            );
            assert_eq!(serialized["final"]["indexFreshness"]["stale"], false);
            assert_eq!(
                report
                    .final_report
                    .results
                    .iter()
                    .map(|hit| hit.doc_id.as_str())
                    .collect::<Vec<_>>(),
                [VISIBLE]
            );
            assert_eq!(report.pre_fusion.lexical.results[0].doc_id, VISIBLE);
            assert_eq!(
                report
                    .final_report
                    .index_freshness
                    .and_then(|state| state.index_generation),
                Some(generation)
            );
            assert_eq!(index_bytes(&retained)?, before_retained);
            if corruption == "missing" {
                assert!(!index.exists());
            }
        }
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn diagnostic_public_entry_removes_live_tombstones_from_raw_rankings_without_index_repair()
    -> TestResult {
        let (_temp, options, db) = fixture()?;
        db.insert_memory(VISIBLE, &input(WORKSPACE, PHRASE))
            .map_err(|e| e.to_string())?;
        let generation = db
            .get_workspace_generation(WORKSPACE)
            .map_err(|e| e.to_string())?
            .ok_or("generation")?;
        let index = options.resolve_index_dir();
        let index_ref = &index;
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            build_index(&cx, index_ref, generation).await
        })
        .map_err(|e| e.to_string())??;
        db.close().map_err(|e| e.to_string())?;
        let baseline = run_diag_search(&options).map_err(|e| e.to_string())?;
        assert!(
            baseline
                .pre_fusion
                .lexical
                .results
                .iter()
                .any(|hit| hit.doc_id == VISIBLE)
        );
        let writer =
            DbConnection::open_file(&options.resolve_database_path()).map_err(|e| e.to_string())?;
        assert!(
            writer
                .tombstone_memory(VISIBLE)
                .map_err(|e| e.to_string())?
        );
        writer.close().map_err(|e| e.to_string())?;
        let before = index_bytes(&index)?;
        let report = run_diag_search(&options).map_err(|e| e.to_string())?;
        assert!(report.pre_fusion.lexical.results.is_empty());
        assert!(report.pre_fusion.semantic_fast.results.is_empty());
        assert!(report.fusion.per_doc_contribution.is_empty());
        assert!(report.final_report.results.is_empty());
        assert!(!report.data_json().to_string().contains(VISIBLE));
        assert_eq!(index_bytes(&index)?, before);
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn diagnostic_public_revision_cutoff_uses_live_source_without_erasing_indexed_history()
    -> TestResult {
        let (_temp, mut options, db) = fixture()?;
        let mut record = input(WORKSPACE, PHRASE);
        record.valid_from = Some("2026-05-01T00:00:00Z".to_owned());
        record.valid_to = Some("2099-01-01T00:00:00Z".to_owned());
        db.insert_memory_with_timestamps(
            VISIBLE,
            &record,
            "2026-05-01T00:00:00Z",
            "2026-05-01T00:00:00Z",
            VISIBLE,
        )
        .map_err(|e| e.to_string())?;
        let generation = db
            .get_workspace_generation(WORKSPACE)
            .map_err(|e| e.to_string())?
            .ok_or("generation")?;
        let index = options.resolve_index_dir();
        let index_ref = &index;
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            build_index(&cx, index_ref, generation).await
        })
        .map_err(|e| e.to_string())??;
        db.close().map_err(|e| e.to_string())?;
        let baseline = run_diag_search(&options).map_err(|e| e.to_string())?;
        assert_eq!(baseline.final_report.results.len(), 1);
        assert_eq!(baseline.final_report.results[0].doc_id, VISIBLE);
        assert!(
            baseline
                .pre_fusion
                .lexical
                .results
                .iter()
                .any(|hit| hit.doc_id == VISIBLE)
        );
        let writer =
            DbConnection::open_file(&options.resolve_database_path()).map_err(|e| e.to_string())?;
        assert!(
            writer
                .restore_imported_memory_supersession(VISIBLE, "2026-07-01T00:00:00Z")
                .map_err(|e| e.to_string())?
        );
        writer.close().map_err(|e| e.to_string())?;
        let before = index_bytes(&index)?;
        let current = run_diag_search(&options).map_err(|e| e.to_string())?;
        assert!(current.pre_fusion.lexical.results.is_empty());
        assert!(current.pre_fusion.semantic_fast.results.is_empty());
        assert!(current.fusion.per_doc_contribution.is_empty());
        assert!(current.final_report.results.is_empty());
        assert!(!current.data_json().to_string().contains(VISIBLE));
        options.as_of = Some(
            chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
                .map_err(|e| e.to_string())?
                .with_timezone(&chrono::Utc),
        );
        let historical = run_diag_search(&options).map_err(|e| e.to_string())?;
        assert_eq!(historical.final_report.results.len(), 1);
        assert_eq!(historical.final_report.results[0].doc_id, VISIBLE);
        assert!(
            historical
                .pre_fusion
                .lexical
                .results
                .iter()
                .any(|hit| hit.doc_id == VISIBLE)
        );
        assert_eq!(index_bytes(&index)?, before);
        Ok(())
    }

    #[cfg(all(unix, feature = "lexical-bm25"))]
    #[test]
    fn diagnostic_snapshot_pins_source_visibility_and_selects_its_retained_generation() -> TestResult
    {
        use crate::db::DatabaseConfig;
        use crate::db::read_pool::{PoolConfig, registered_process_read_pool};
        let (_temp, options, db) = fixture()?;
        db.insert_memory(VISIBLE, &input(WORKSPACE, PHRASE))
            .map_err(|e| e.to_string())?;
        let generation = db
            .get_workspace_generation(WORKSPACE)
            .map_err(|e| e.to_string())?
            .ok_or("generation")?;
        db.close().map_err(|e| e.to_string())?;
        let index = options.resolve_index_dir();
        let retained = index.parent().ok_or("index parent")?.join("index.previous");
        let (retained_ref, index_ref) = (&retained, &index);
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            build_index(&cx, retained_ref, generation).await?;
            build_index(&cx, index_ref, generation + 1).await
        })
        .map_err(|e| e.to_string())??;
        let pool = registered_process_read_pool(
            DatabaseConfig::file(options.resolve_database_path()),
            PoolConfig::default_single(),
        );
        let snapshot = pool.pin_snapshot().map_err(|e| e.to_string())?;
        let connection = snapshot.checked_connection().map_err(|e| e.to_string())?;
        assert_eq!(
            connection
                .get_workspace_generation(WORKSPACE)
                .map_err(|e| e.to_string())?,
            Some(generation)
        );
        let writer =
            DbConnection::open_file(&options.resolve_database_path()).map_err(|e| e.to_string())?;
        assert!(
            writer
                .tombstone_memory(VISIBLE)
                .map_err(|e| e.to_string())?
        );
        writer.close().map_err(|e| e.to_string())?;
        let options_ref = &options;
        let report = crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            run_diag_search_in_snapshot(&cx, options_ref, None, true, Some(connection)).await
        })
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        assert_eq!(
            report
                .final_report
                .results
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            [VISIBLE]
        );
        let freshness = report.final_report.index_freshness.ok_or("freshness")?;
        assert_eq!(freshness.db_generation, Some(generation));
        assert_eq!(freshness.index_generation, Some(generation));
        assert!(!freshness.stale);
        snapshot.commit().map_err(|e| e.to_string())?;
        let next = run_diag_search(&options).map_err(|e| e.to_string())?;
        assert!(next.pre_fusion.lexical.results.is_empty());
        assert!(next.final_report.results.is_empty());
        Ok(())
    }

    #[test]
    fn global_scope_accepts_separate_store_admission_but_not_index_claims() -> TestResult {
        use super::super::apply_memory_scope_visibility_with_metadata_mode_collecting;
        use crate::core::global_store::{
            GlobalStorePaths, open_or_create_global_store, read_global_store_memories,
        };
        let (_temp, mut options, db) = fixture()?;
        db.insert_memory(VISIBLE, &input(WORKSPACE, "workspace-only source"))
            .map_err(|e| e.to_string())?;
        let paths = GlobalStorePaths::from_root(&options.workspace_path.join("user-global"));
        let (global, workspace) = open_or_create_global_store(&paths)?;
        global
            .insert_memory(
                HIDDEN,
                &input(&workspace, "actual global source without local tags"),
            )
            .map_err(|e| e.to_string())?;
        global.close().map_err(|e| e.to_string())?;
        let source_rows: BTreeMap<_, _> = read_global_store_memories(&paths, false)?
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        let admitted: BTreeSet<_> = source_rows.keys().cloned().collect();
        options.memory_scope = MemoryScope::Global;
        let hits: Vec<_> = [VISIBLE, HIDDEN, "mem_index_only_claim"].into_iter().map(|id| {
            let mut hit = hit(id);
            hit.metadata = Some(json!({"storeLane": "global", "tags": [crate::models::GLOBAL_MEMORY_SCOPE_TAG]}));
            hit
        }).collect();
        let mut preloaded = source_rows.clone();
        let (scoped, stats) = apply_memory_scope_visibility_with_metadata_mode_collecting(
            &options,
            hits.clone(),
            &mut Vec::new(),
            Some(&db),
            true,
            Some(&mut preloaded),
            Some(&admitted),
        );
        assert_eq!(
            scoped
                .iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            [HIDDEN]
        );
        assert_eq!(stats.candidates_total, 3);
        assert_eq!(stats.candidates_in_scope, 1);
        assert_eq!(stats.candidates_excluded_by_scope, 2);
        // Source-row bytes alone are not a declaration of their store lane.
        let mut preloaded = source_rows.clone();
        let (without_admission, _) = apply_memory_scope_visibility_with_metadata_mode_collecting(
            &options,
            hits.clone(),
            &mut Vec::new(),
            Some(&db),
            true,
            Some(&mut preloaded),
            None,
        );
        assert!(without_admission.is_empty());
        // A membership set alone cannot replace an authoritative memory row.
        let (without_rows, _) = apply_memory_scope_visibility_with_metadata_mode_collecting(
            &options,
            hits,
            &mut Vec::new(),
            Some(&db),
            true,
            None,
            Some(&admitted),
        );
        assert!(without_rows.is_empty());
        assert_eq!(
            db.get_memory_tags(VISIBLE).map_err(|e| e.to_string())?,
            Vec::<String>::new()
        );
        assert_eq!(
            read_global_store_memories(&paths, false)?.len(),
            source_rows.len()
        );
        Ok(())
    }

    #[test]
    fn global_scope_uses_local_source_identity_when_store_ids_collide() -> TestResult {
        use super::super::apply_memory_scope_visibility_with_metadata_mode_collecting;
        use crate::core::global_store::{
            GlobalStorePaths, open_or_create_global_store, read_global_store_memories,
        };
        let (_temp, mut options, db) = fixture()?;
        db.insert_memory(HIDDEN, &input(WORKSPACE, "local identity is not global"))
            .map_err(|e| e.to_string())?;
        let paths = GlobalStorePaths::from_root(&options.workspace_path.join("user-global"));
        let (global, workspace) = open_or_create_global_store(&paths)?;
        global
            .insert_memory(
                HIDDEN,
                &input(&workspace, "different global identity with the same ID"),
            )
            .map_err(|e| e.to_string())?;
        global.close().map_err(|e| e.to_string())?;
        let mut source_rows: BTreeMap<_, _> = read_global_store_memories(&paths, false)?
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        let admitted: BTreeSet<_> = source_rows.keys().cloned().collect();
        options.memory_scope = MemoryScope::Global;
        let mut hit = hit(HIDDEN);
        hit.metadata =
            Some(json!({"storeLane": "global", "tags": [crate::models::GLOBAL_MEMORY_SCOPE_TAG]}));
        let (scoped, stats) = apply_memory_scope_visibility_with_metadata_mode_collecting(
            &options,
            vec![hit],
            &mut Vec::new(),
            Some(&db),
            true,
            Some(&mut source_rows),
            Some(&admitted),
        );
        assert!(
            scoped.is_empty(),
            "global source membership must not overwrite a local identity"
        );
        assert_eq!(stats.candidates_excluded_by_scope, 1);
        assert_eq!(
            db.get_memory(HIDDEN)
                .map_err(|e| e.to_string())?
                .ok_or("local source")?
                .content,
            "local identity is not global"
        );
        Ok(())
    }
}
