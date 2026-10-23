//! Property / contract tests for the explicit-evidence contradiction detector
//! (bd-1n0np.7.6) over the landed pub core in
//! `ee::core::contradiction_detect::detect_explicit_contradictions`
//! (bd-1n0np.7.2 detection core, d010a42f).
//!
//! The in-module tests cover canonical-pair normalization, a reversed-order
//! determinism point, and the fuzzy-skip flag on empty input. These lock the
//! load-bearing input contract more broadly, independent of whether k-truss +
//! Louvain forms a cluster for any given small input:
//! - empty input yields an empty, non-failing report;
//! - `explicit_edge_count` counts DISTINCT canonical pairs (undirected dedup,
//!   multi-signal collapse);
//! - self-loops and blank endpoints are dropped, never counted;
//! - the fuzzy near-conflict pass is reported skipped, never silently widened;
//! - the whole report is deterministic across edge orderings;
//! - ranked clusters (when any form) are ordered most-urgent-first;
//! - CLI reads admit only visible sources in the addressed workspace, redact
//!   sensitive bodies, and fail closed when read authority is unavailable.

use std::path::{Path, PathBuf};

use ee::core::context::{ContextPackOptions, ContextPackOutputOptions, run_context_pack};
use ee::core::contradiction_detect::{
    ConflictEdge, ContradictionDetectionConfig, ExplicitConflictSignal,
    detect_explicit_contradictions,
};
use ee::core::init::{InitOptions, InitStatus, init_workspace};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::{CreateMemoryLinkInput, DbConnection, MemoryLinkRelation, MemoryLinkSource};
use ee::models::MemoryScope;
use ee::search::SpeedMode;
use tempfile::TempDir;

#[path = "support/isolated_ee.rs"]
mod isolated_ee;

type TestResult<T = ()> = Result<T, String>;

fn db_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ee").join("ee.db")
}

fn edge(a: &str, b: &str, signal: ExplicitConflictSignal) -> ConflictEdge {
    ConflictEdge::new(a, b, signal)
}

fn remember_pack_guard_fixture(
    workspace_path: &Path,
    database_path: &Path,
    content: &str,
) -> TestResult<String> {
    let report = remember_memory(&RememberMemoryOptions {
        workspace_path,
        database_path: Some(database_path),
        content,
        workflow_id: None,
        level: "semantic",
        kind: "note",
        tags: Some("contradiction,pack-guard"),
        confidence: 0.9,
        source: None,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: false,
        propose_candidates: false,
        allow_secret_mention: false,
    })
    .map_err(|error| format!("remember fixture memory failed: {error:?}"))?;
    Ok(report.memory_id.to_string())
}

fn insert_contradiction_link(database_path: &Path, first: &str, second: &str) -> TestResult {
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000070009",
            &CreateMemoryLinkInput {
                src_memory_id: first.to_owned(),
                dst_memory_id: second.to_owned(),
                relation: MemoryLinkRelation::Contradicts,
                weight: 1.0,
                confidence: 1.0,
                directed: false,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Agent,
                created_by: Some("bd-1n0np.7.9-test".to_owned()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())
}

fn pack_guard_options(
    workspace_path: &Path,
    database_path: &Path,
    task: &str,
) -> ContextPackOptions {
    ContextPackOptions {
        task_paths: Vec::new(),
        task_lens: None,
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(database_path.to_path_buf()),
        index_dir: None,
        query: task.to_owned(),
        speed: SpeedMode::Default,
        source_mode: ee::core::search::SearchSourceMode::Hybrid,
        strict_source_mode: false,
        filters: Default::default(),
        profile: None,
        max_tokens: Some(2000),
        candidate_pool: Some(20),
        max_results: None,
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: None,
        redaction_level: ee::models::RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight: None,
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: ContextPackOutputOptions::default(),
        persist_pack: false,
        baseline_write: None,
        no_lod: false,
        require_fresh_sentinels: false,
    }
}

#[test]
fn production_pack_suppresses_one_side_of_explicit_contradiction() -> TestResult {
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace_path = temp_dir
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let database_path = db_path(&workspace_path);
    let initialized = init_workspace(&InitOptions {
        workspace_path: workspace_path.clone(),
        dry_run: false,
        repair_plan: false,
        force: false,
        allow_symlink: false,
        skip_boilerplate: true,
    });
    assert_eq!(
        initialized.status,
        InitStatus::Created,
        "contradiction pack fixture must initialize successfully: {:?}",
        initialized.action_errors,
    );
    assert!(
        initialized.action_errors.is_empty(),
        "contradiction pack fixture cannot ignore initialization errors: {:?}",
        initialized.action_errors,
    );

    let first = remember_pack_guard_fixture(
        &workspace_path,
        &database_path,
        "Network retry policy: always retry transient failed network calls exactly three times before returning an error.",
    )?;
    let second = remember_pack_guard_fixture(
        &workspace_path,
        &database_path,
        "Network retry policy: never retry transient failed network calls; fail fast immediately instead.",
    )?;
    insert_contradiction_link(&database_path, &first, &second)?;

    let response = run_context_pack(&pack_guard_options(
        &workspace_path,
        &database_path,
        "network retry policy transient failed network calls",
    ))
    .map_err(|error| format!("run_context_pack failed: {error:?}"))?;
    let selected_conflict_sides = response
        .data
        .pack
        .items
        .iter()
        .filter(|item| {
            let memory_id = item.memory_id.to_string();
            memory_id == first || memory_id == second
        })
        .count();
    assert_eq!(
        selected_conflict_sides, 1,
        "production pack should retain exactly one unresolved contradiction side"
    );
    let suppressed = response.data.pack.omitted.iter().find(|omission| {
        let memory_id = omission.memory_id.to_string();
        (memory_id == first || memory_id == second)
            && omission.reason.as_str() == "contradiction_suppressed"
    });
    assert!(
        suppressed.is_some(),
        "production pack should record the suppressed side as contradiction_suppressed"
    );
    assert_eq!(
        response.data.pack.used_tokens,
        response
            .data
            .pack
            .items
            .iter()
            .map(|item| item.estimated_tokens)
            .sum::<u32>(),
        "production pack token accounting must reflect post-guard selected items"
    );
    Ok(())
}

#[test]
fn empty_edges_yield_an_empty_non_failing_report() {
    let report = detect_explicit_contradictions(&[], ContradictionDetectionConfig::default());
    assert!(report.clusters.is_empty());
    assert_eq!(report.explicit_edge_count, 0);
    assert!(!report.fuzzy_near_conflict_skipped);
}

mod conflict_read_admission {
    use std::process::Output;

    use super::{TestResult, isolated_ee};
    use ee::core::workspace::stable_workspace_id;
    use ee::db::{
        CreateFeedbackQuarantineInput, CreateMemoryInput, CreateMemoryLinkInput,
        CreateWorkspaceInput, DbConnection, MemoryLinkRelation, MemoryLinkSource,
    };
    use ee::models::{MemoryId, WorkspaceId};
    use serde_json::Value;
    use tempfile::TempDir;

    const TIME: &str = "2020-01-01T00:00:00Z";
    type StoredState = Vec<Vec<Vec<(String, sqlmodel_core::Value)>>>;

    struct Fixture {
        db: DbConnection,
        root: TempDir,
        workspace: String,
        other: String,
    }

    impl Fixture {
        fn new() -> TestResult<Self> {
            let root = TempDir::new().map_err(|error| error.to_string())?;
            let path = root
                .path()
                .canonicalize()
                .map_err(|error| error.to_string())?;
            std::fs::create_dir_all(path.join(".ee")).map_err(|error| error.to_string())?;
            std::fs::write(
                path.join(".ee/config.toml"),
                "[memory]\ninclude_global = false\n",
            )
            .map_err(|error| error.to_string())?;
            let db = DbConnection::open_file(path.join(".ee/ee.db"))
                .map_err(|error| error.to_string())?;
            db.migrate().map_err(|error| error.to_string())?;
            let workspace = stable_workspace_id(&path);
            let other = WorkspaceId::from_uuid(uuid::Uuid::from_u128(71000)).to_string();
            for (id, directory) in [(&workspace, path.clone()), (&other, path.join("other"))] {
                db.insert_workspace(
                    id,
                    &CreateWorkspaceInput {
                        path: directory.to_string_lossy().into_owned(),
                        name: None,
                    },
                )
                .map_err(|error| error.to_string())?;
            }
            Ok(Self {
                db,
                root,
                workspace,
                other,
            })
        }

        fn memory(&self, number: u128, workspace: &str, content: &str) -> TestResult<String> {
            let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
            self.db
                .insert_memory(
                    &id,
                    &CreateMemoryInput {
                        workspace_id: workspace.to_owned(),
                        level: "semantic".to_owned(),
                        kind: "fact".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: Some("manual://conflict-read-admission".to_owned()),
                        trust_class: "agent_assertion".to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: Some(TIME.to_owned()),
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
            Ok(id)
        }

        fn link(&self, number: u32, first: &str, second: &str) -> TestResult {
            self.db
                .insert_memory_link(
                    &format!("link_{number:026}"),
                    &CreateMemoryLinkInput {
                        src_memory_id: first.to_owned(),
                        dst_memory_id: second.to_owned(),
                        relation: MemoryLinkRelation::Contradicts,
                        weight: 1.0,
                        confidence: 1.0,
                        directed: false,
                        evidence_count: 1,
                        last_reinforced_at: None,
                        source: MemoryLinkSource::Agent,
                        created_by: Some("conflict-read-test".to_owned()),
                        metadata_json: None,
                    },
                )
                .map_err(|error| error.to_string())
        }

        fn hold(
            &self,
            number: u32,
            workspace: &str,
            target_type: &str,
            memory: &str,
        ) -> TestResult<String> {
            let id = format!("fq_{number:026}");
            self.db
                .insert_feedback_quarantine(
                    &id,
                    &CreateFeedbackQuarantineInput {
                        workspace_id: workspace.to_owned(),
                        source_id: "conflict-read-review".to_owned(),
                        target_type: target_type.to_owned(),
                        target_id: memory.to_owned(),
                        signal: "harmful".to_owned(),
                        weight: 1.0,
                        source_type: "outcome_observed".to_owned(),
                        proposed_event_id: None,
                        recorded_at: TIME.to_owned(),
                        reason: "Review this source before serving it.".to_owned(),
                        event_reason: None,
                        evidence_json: None,
                        session_id: None,
                        raw_event_hash: format!("blake3:{}", "a".repeat(64)),
                    },
                )
                .map_err(|error| error.to_string())?;
            Ok(id)
        }

        fn run(&self, arguments: &[&str]) -> TestResult<Output> {
            isolated_ee::isolated_ee_command(&self.root.path().join("client"))?
                .arg("conflict")
                .args(arguments)
                .arg("--workspace")
                .arg(self.root.path())
                .arg("--json")
                .output()
                .map_err(|error| format!("run conflict {arguments:?}: {error}"))
        }

        fn data(&self, arguments: &[&str]) -> TestResult<Value> {
            let output = self.run(arguments)?;
            if !output.status.success() {
                return Err(format!(
                    "conflict {arguments:?} failed: status={:?}; stdout={}; stderr={}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                ));
            }
            let envelope: Value = serde_json::from_slice(&output.stdout)
                .map_err(|error| format!("conflict JSON: {error}"))?;
            assert_eq!(envelope["success"], true);
            assert_eq!(envelope["data"]["schema"], "ee.conflict.v1");
            assert!(envelope["data"]["pairs"].is_array());
            assert!(envelope["data"]["clusters"].is_array());
            Ok(envelope["data"].clone())
        }

        fn state(&self) -> TestResult<StoredState> {
            [
                "memories",
                "memory_tags",
                "memory_links",
                "audit_log",
                "search_index_jobs",
            ]
            .into_iter()
            .map(|table| {
                self.db
                    .query(&format!("SELECT * FROM {table} ORDER BY 1, 2"), &[])
                    .map(|rows| {
                        rows.into_iter()
                            .map(|row| {
                                row.iter()
                                    .map(|(name, value)| (name.to_owned(), value.clone()))
                                    .collect()
                            })
                            .collect()
                    })
                    .map_err(|error| error.to_string())
            })
            .collect()
        }
    }

    fn assert_absent(data: &Value, excluded: &[&str]) {
        let text = data.to_string();
        for private in excluded {
            assert!(!text.contains(private), "conflict data disclosed {private}");
        }
    }

    #[test]
    fn conflict_cli_reads_hide_sealed_held_and_placeholder_sources() -> TestResult {
        let fixture = Fixture::new()?;
        let secret = format!("ghp_{}", "Q".repeat(36));
        let first = fixture.memory(
            71001,
            &fixture.workspace,
            &format!("Public setting A; {secret}"),
        )?;
        let second = fixture.memory(71002, &fixture.workspace, "Public setting B")?;
        let third = fixture.memory(71003, &fixture.workspace, "Public setting C")?;
        fixture.link(71001, &first, &second)?;
        fixture.link(71002, &second, &third)?;
        fixture.link(71003, &third, &first)?;
        let sealed = fixture.memory(71101, &fixture.workspace, "SEALED-CONFLICT-CANARY")?;
        let held = fixture.memory(71102, &fixture.workspace, "HELD-CONFLICT-CANARY")?;
        let placeholder = fixture.memory(
            71103,
            &fixture.workspace,
            ee::models::MEMORY_SEAL_PLACEHOLDER_CONTENT,
        )?;
        for (number, hidden) in [(71101, &sealed), (71102, &held), (71103, &placeholder)] {
            fixture.link(number, &first, hidden)?;
            fixture.link(number + 10, &second, hidden)?;
        }
        fixture
            .db
            .insert_memory_seal(&sealed, &format!("blake3:{}", "a".repeat(64)), TIME)
            .map_err(|error| error.to_string())?;
        let hold = fixture.hold(71102, &fixture.workspace, "memory", &held)?;
        // Unrelated authority must not quarantine an otherwise visible memory.
        fixture.hold(71104, &fixture.other, "memory", &first)?;
        fixture.hold(71105, &fixture.workspace, "rule", &first)?;
        let before = fixture.state()?;
        for command in [
            &["list"][..],
            &["cluster"][..],
            &["explain", first.as_str()][..],
        ] {
            let data = fixture.data(command)?;
            let expected_pairs = if command[0] == "explain" { 2 } else { 3 };
            assert_eq!(data["pairs"].as_array().map(Vec::len), Some(expected_pairs),);
            assert_eq!(data["explicitEdgeCount"], expected_pairs);
            assert_absent(
                &data,
                &[
                    &sealed,
                    &held,
                    &placeholder,
                    "SEALED-CONFLICT-CANARY",
                    "HELD-CONFLICT-CANARY",
                    &secret,
                ],
            );
            assert!(data.to_string().contains(&first));
        }
        for hidden in [&sealed, &held, &placeholder] {
            let data = fixture.data(&["explain", hidden])?;
            assert_eq!(data["pairs"], serde_json::json!([]));
            assert_eq!(data["clusters"], serde_json::json!([]));
            assert_absent(&data, &[hidden]);
        }
        assert_eq!(
            fixture.state()?,
            before,
            "conflict reads must not rewrite source or audit state",
        );
        assert!(
            fixture
                .db
                .mark_memory_seal_revealed(&sealed, TIME)
                .map_err(|error| error.to_string())?,
        );
        assert!(
            fixture
                .db
                .update_feedback_quarantine_status(&hold, "released", Some("operator"), None)
                .map_err(|error| error.to_string())?,
        );
        let restored = fixture.data(&["list"])?;
        assert_eq!(restored["pairs"].as_array().map(Vec::len), Some(7));
        assert_eq!(restored["explicitEdgeCount"], 7);
        assert!(restored.to_string().contains("SEALED-CONFLICT-CANARY"));
        assert!(restored.to_string().contains("HELD-CONFLICT-CANARY"));
        assert_absent(&restored, &[&placeholder, &secret]);
        Ok(())
    }

    #[test]
    fn conflict_cli_reads_exclude_foreign_and_cross_workspace_pairs() -> TestResult {
        let fixture = Fixture::new()?;
        let first = fixture.memory(71201, &fixture.workspace, "Local setting A")?;
        let second = fixture.memory(71202, &fixture.workspace, "Local setting B")?;
        let foreign_a = fixture.memory(71203, &fixture.other, "FOREIGN-CONFLICT-A")?;
        let foreign_b = fixture.memory(71204, &fixture.other, "FOREIGN-CONFLICT-B")?;
        let foreign_c = fixture.memory(71205, &fixture.other, "FOREIGN-CONFLICT-C")?;
        fixture.link(71201, &first, &second)?;
        fixture.link(71202, &foreign_a, &foreign_b)?;
        fixture.link(71203, &first, &foreign_a)?;
        fixture.link(71204, &foreign_b, &foreign_c)?;
        fixture.link(71205, &foreign_c, &foreign_a)?;
        let before = fixture.state()?;
        for command in [
            &["list"][..],
            &["cluster"][..],
            &["explain", first.as_str()][..],
        ] {
            let data = fixture.data(command)?;
            assert_eq!(data["pairs"].as_array().map(Vec::len), Some(1));
            assert_eq!(data["explicitEdgeCount"], 1);
            assert_absent(
                &data,
                &[
                    &foreign_a,
                    &foreign_b,
                    &foreign_c,
                    "FOREIGN-CONFLICT-A",
                    "FOREIGN-CONFLICT-B",
                    "FOREIGN-CONFLICT-C",
                ],
            );
            assert!(data.to_string().contains(&first));
            assert!(data.to_string().contains(&second));
        }
        let data = fixture.data(&["explain", &foreign_a])?;
        assert_eq!(data["pairs"], serde_json::json!([]));
        assert_eq!(data["clusters"], serde_json::json!([]));
        assert_absent(&data, &[&foreign_a, &foreign_b, &foreign_c]);
        assert_eq!(fixture.state()?, before);
        Ok(())
    }

    #[test]
    fn conflict_cli_reads_withhold_all_pairs_when_authority_cannot_be_read() -> TestResult {
        for table in ["memory_seals", "feedback_quarantine"] {
            let fixture = Fixture::new()?;
            let first = fixture.memory(71301, &fixture.workspace, "AUTHORITY-UNAVAILABLE-A")?;
            let second = fixture.memory(71302, &fixture.workspace, "AUTHORITY-UNAVAILABLE-B")?;
            fixture.link(71301, &first, &second)?;
            fixture
                .db
                .execute_raw(&format!(
                    "ALTER TABLE {table} RENAME TO unavailable_{table}"
                ))
                .map_err(|error| error.to_string())?;
            let before = fixture.state()?;
            for command in [&["list"][..], &["cluster"][..]] {
                let output = fixture.run(command)?;
                let envelope: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
                    format!(
                        "authority failure JSON: {error}; status={:?}; stdout={}; stderr={}",
                        output.status.code(),
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr),
                    )
                })?;
                assert_absent(
                    &envelope,
                    &[
                        &first,
                        &second,
                        "AUTHORITY-UNAVAILABLE-A",
                        "AUTHORITY-UNAVAILABLE-B",
                    ],
                );
                if output.status.success() {
                    assert_eq!(envelope["data"]["pairs"], serde_json::json!([]));
                    assert_eq!(envelope["data"]["clusters"], serde_json::json!([]));
                    assert!(
                        envelope["data"]["degraded"]
                            .as_array()
                            .is_some_and(|notes| !notes.is_empty()),
                    );
                } else {
                    assert_eq!(envelope["schema"], "ee.error.v2");
                    assert!(envelope["error"]["code"].is_string());
                }
            }
            assert_eq!(fixture.state()?, before);
        }
        Ok(())
    }
}

#[test]
fn explicit_edge_count_dedups_undirected_and_multi_signal_pairs() {
    // (a,b) appears three ways — reversed direction and a second signal — but is
    // one undirected pair. (c,d) is a distinct pair. Count must be 2.
    let edges = vec![
        edge("mem_a", "mem_b", ExplicitConflictSignal::ContradictionLink),
        edge("mem_b", "mem_a", ExplicitConflictSignal::ContradictionLink),
        edge("mem_a", "mem_b", ExplicitConflictSignal::Supersession),
        edge("mem_c", "mem_d", ExplicitConflictSignal::TrustOutcomeSplit),
    ];
    let report = detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
    assert_eq!(
        report.explicit_edge_count, 2,
        "two distinct undirected pairs after dedup"
    );
}

#[test]
fn self_loops_and_blank_endpoints_are_dropped() {
    let edges = vec![
        edge("mem_a", "mem_a", ExplicitConflictSignal::ContradictionLink), // self-loop
        edge("   ", "mem_b", ExplicitConflictSignal::ContradictionLink),   // blank a
        edge("mem_x", "", ExplicitConflictSignal::Supersession),           // blank b
        edge(
            "  mem_a  ",
            "mem_b",
            ExplicitConflictSignal::ContradictionLink,
        ), // trims to (a,b)
    ];
    let report = detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
    assert_eq!(
        report.explicit_edge_count, 1,
        "only the trimmed (mem_a, mem_b) pair survives"
    );
}

#[test]
fn fuzzy_pass_is_reported_skipped_never_silently_widened() {
    let edges = vec![edge(
        "mem_a",
        "mem_b",
        ExplicitConflictSignal::ContradictionLink,
    )];
    let with_fuzzy = ContradictionDetectionConfig {
        include_fuzzy_near_conflict: true,
        ..ContradictionDetectionConfig::default()
    };
    let report = detect_explicit_contradictions(&edges, with_fuzzy);
    assert!(
        report.fuzzy_near_conflict_skipped,
        "opting into fuzzy must surface a skipped flag (v1 defers it), never widen silently"
    );

    let report_default =
        detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
    assert!(!report_default.fuzzy_near_conflict_skipped);
}

#[test]
fn report_is_deterministic_across_edge_orderings() {
    let base = vec![
        edge("mem_a", "mem_b", ExplicitConflictSignal::ContradictionLink),
        edge("mem_b", "mem_c", ExplicitConflictSignal::ContradictionLink),
        edge("mem_a", "mem_c", ExplicitConflictSignal::Supersession),
        edge("mem_c", "mem_d", ExplicitConflictSignal::DuplicateDivergent),
    ];
    let mut reversed = base.clone();
    reversed.reverse();
    let mut rotated = base.clone();
    rotated.rotate_left(2);

    let config = ContradictionDetectionConfig::default();
    let canonical = detect_explicit_contradictions(&base, config);
    assert_eq!(
        canonical,
        detect_explicit_contradictions(&reversed, config),
        "reversed edge order must yield an identical report"
    );
    assert_eq!(
        canonical,
        detect_explicit_contradictions(&rotated, config),
        "rotated edge order must yield an identical report"
    );
}

#[test]
fn ranked_clusters_are_ordered_most_urgent_first() {
    // A small densely-linked set; whatever clusters form must be sorted by
    // non-increasing rank_score (deterministic urgency ordering).
    let edges = vec![
        edge("mem_a", "mem_b", ExplicitConflictSignal::ContradictionLink),
        edge("mem_b", "mem_c", ExplicitConflictSignal::ContradictionLink),
        edge("mem_a", "mem_c", ExplicitConflictSignal::ContradictionLink),
        edge("mem_c", "mem_d", ExplicitConflictSignal::ContradictionLink),
        edge("mem_d", "mem_e", ExplicitConflictSignal::ContradictionLink),
        edge("mem_c", "mem_e", ExplicitConflictSignal::ContradictionLink),
    ];
    let report = detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
    for pair in report.clusters.windows(2) {
        assert!(
            pair[0].rank_score >= pair[1].rank_score,
            "clusters must be sorted most-urgent (highest rank_score) first"
        );
    }
}
