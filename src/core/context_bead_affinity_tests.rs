//! Source-backed tests for opt-in ranking, identity, fallback and read bounds.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
use crate::models::{MemoryId, ProvenanceUri, UnitScore};
use crate::pack::{PackCandidateInput, PackProvenance, PackSection};

fn options(workspace: &Path) -> ContextPackOptions {
    ContextPackOptions {
        task_paths: Vec::new(),
        workspace_path: workspace.to_owned(),
        database_path: None,
        index_dir: None,
        query: "emberstone release".to_owned(),
        bead_id: None,
        speed: crate::search::SpeedMode::Instant,
        source_mode: SearchSourceMode::LexicalOnly,
        strict_source_mode: false,
        filters: crate::models::QueryFilters::default(),
        profile: Some(ContextPackProfile::Balanced),
        max_tokens: Some(1000),
        candidate_pool: Some(20),
        max_results: Some(1),
        include_tombstoned: false,
        as_of: Some("2030-01-01T00:00:00Z".parse().unwrap()),
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: None,
        redaction_level: crate::models::RedactionLevel::Minimal,
        memory_scope: MemoryScope::Workspace,
        strict_scope: false,
        ppr_weight: Some(0.0),
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: 0,
        task_lens: None,
        require_fresh_sentinels: false,
        output_options: ContextPackOutputOptions::default(),
        persist_pack: false,
        baseline_write: None,
        no_lod: true,
    }
}

fn write_beads(workspace: &Path, jsonl: &str) {
    fs::create_dir_all(workspace.join(".beads")).unwrap();
    fs::write(workspace.join(".beads/issues.jsonl"), jsonl).unwrap();
}

fn resolve_id(workspace: &Path, id: &str) -> ResolvedBeadAffinity {
    let mut opts = options(workspace);
    opts.bead_id = Some(id.to_owned());
    resolve(&opts, &mut Vec::new()).unwrap()
}

fn candidate(seed: u128, content: &str, relevance: f32) -> PackCandidate {
    PackCandidate::new(PackCandidateInput {
        memory_id: MemoryId::from_uuid(uuid::Uuid::from_u128(seed)),
        section: PackSection::Evidence,
        content: content.to_owned(),
        estimated_tokens: 20,
        relevance: UnitScore::parse(relevance).unwrap(),
        utility: UnitScore::neutral(),
        provenance: vec![
            PackProvenance::new(
                ProvenanceUri::from_str("manual://bead-context-test").unwrap(),
                "Explicit fixture memory",
            )
            .unwrap(),
        ],
        why: "source-admitted candidate".to_owned(),
    })
    .unwrap()
}

struct Fixture {
    _directory: tempfile::TempDir,
    _embedder: crate::core::index::TestWorkspaceEmbedderStackGuard,
    workspace: PathBuf,
    workspace_id: String,
    db: DbConnection,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().canonicalize().unwrap();
        fs::create_dir(workspace.join(".ee")).unwrap();
        let db = DbConnection::open_file(workspace.join(".ee/ee.db")).unwrap();
        db.migrate().unwrap();
        let workspace_id = crate::core::workspace::stable_workspace_id(&workspace);
        db.insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
        let embedder = crate::core::index::install_test_hash_workspace_embedder(&workspace_id);
        Self {
            _directory: directory,
            _embedder: embedder,
            workspace,
            workspace_id,
            db,
        }
    }

    fn memory(&self, seed: u128, content: &str, tag: &str) -> MemoryId {
        let id = MemoryId::from_uuid(uuid::Uuid::from_u128(seed));
        self.db
            .insert_memory(
                &id.to_string(),
                &CreateMemoryInput {
                    workspace_id: self.workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
                    confidence: 1.0,
                    utility: 1.0,
                    importance: 0.5,
                    provenance_uri: Some("manual://bead-context-test".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: vec![tag.to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .unwrap();
        id
    }
}

#[test]
fn request_is_opt_in_and_rejects_unbound_candidate_pagination() {
    let temp = tempfile::tempdir().unwrap();
    let mut opts = options(temp.path());
    // There is deliberately no tracker or database. The absent selector
    // performs no read, produces no summary, and binds no new request field.
    let mut degraded = Vec::new();
    assert!(resolve(&opts, &mut degraded).is_none());
    assert!(degraded.is_empty());
    assert!(
        context_request_from_options(&opts)
            .unwrap()
            .bead_affinity_identity
            .is_none()
    );
    opts.bead_id = Some("bd-storage".to_owned());
    assert_eq!(
        context_pack_l2_bypass_reason(&opts, &opts.filters),
        Some("bead_context_requires_live_snapshot")
    );
    opts.pagination = Some(ContextPagination {
        limit: 1,
        ..ContextPagination::default()
    });
    assert!(
        validate_request(&opts)
            .unwrap_err()
            .to_string()
            .contains("pagination")
    );
    opts.pagination = None;
    for bad in ["", "  ", "bd-bad\nline"] {
        opts.bead_id = Some(bad.to_owned());
        assert!(validate_request(&opts).is_err());
    }
}

#[test]
fn identity_tracks_only_normalized_selected_scoring_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().canonicalize().unwrap();
    write_beads(
        &workspace,
        "{\"id\":\"bd-storage\",\"title\":\"Cedar storage\",\"labels\":[\"cache-index\"]}\n",
    );
    let first = resolve_id(&workspace, "bd-storage");
    assert!(first.bead.is_some());
    write_beads(
        &workspace,
        concat!(
            "{\"id\":\"bd-unrelated\",\"title\":\"Changed independently\"}\n",
            "{\"id\":\"bd-storage\",\"title\":\"STORAGE, cedar!\",\"labels\":[\"index\",\"cache\",\"cache\"],\"status\":\"in_progress\"}\n",
        ),
    );
    assert_eq!(
        first.identity,
        resolve_id(&workspace, "bd-storage").identity
    );
    write_beads(
        &workspace,
        "{\"id\":\"bd-storage\",\"title\":\"Network routing\",\"labels\":[\"network\"]}\n",
    );
    assert_ne!(
        first.identity,
        resolve_id(&workspace, "bd-storage").identity
    );
    write_beads(
        &workspace,
        "{\"id\":\"bd-other\",\"title\":\"Cedar storage\",\"labels\":[\"cache-index\"]}\n",
    );
    assert_ne!(first.identity, resolve_id(&workspace, "bd-other").identity);
}

#[test]
fn actual_float_score_change_never_exceeds_policy_cap() {
    for step in 0..=10000 {
        let base = step as f32 / 10000.0;
        for weight in [0.0, 0.0125, 0.025, 0.0375, 0.05, 1.0] {
            let adjusted = bounded_bonus(base, weight);
            let delta = f64::from(adjusted) - f64::from(base);
            assert!((0.0..=BEAD_AFFINITY_BIAS_CAP).contains(&delta));
            assert!(adjusted <= 1.0);
        }
        assert_eq!(bounded_bonus(base, f64::NAN), base);
    }
}

#[test]
fn tracker_reads_are_bounded_and_missing_or_malformed_records_fall_back() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().canonicalize().unwrap();
    let missing = resolve_id(&workspace, "bd-storage");
    assert!(missing.bead.is_none());
    assert_eq!(missing.degraded[0].code, BEAD_AFFINITY_UNAVAILABLE_CODE);
    write_beads(&workspace, "{\"id\":\"bd-other\"}\n");
    let missing_id = resolve_id(&workspace, "bd-storage");
    assert_eq!(missing_id.degraded[0].code, "bead_affinity_lookup_failed");
    assert!(read_bead_contents(&workspace.join(".beads/issues.jsonl"), 4).is_err());
    write_beads(&workspace, "{\"id\":\"bd-storage\"}\nnot-json\n");
    assert!(resolve_id(&workspace, "bd-storage").bead.is_none());
}

#[cfg(unix)]
#[test]
fn tracker_symlinks_are_never_followed() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().canonicalize().unwrap();
    fs::create_dir(workspace.join(".beads")).unwrap();
    let outside = workspace.join("outside.jsonl");
    fs::write(&outside, "{\"id\":\"bd-storage\"}\n").unwrap();
    std::os::unix::fs::symlink(outside, workspace.join(".beads/issues.jsonl")).unwrap();
    let resolved = resolve_id(&workspace, "bd-storage");
    assert!(resolved.bead.is_none());
    assert_eq!(resolved.degraded[0].code, BEAD_AFFINITY_UNAVAILABLE_CODE);
}

#[test]
fn affinity_uses_live_tags_and_admitted_content_without_borrowing_nonlocal_signals() {
    let fixture = Fixture::new();
    let local = fixture.memory(0xb01, "Cedar storage guidance.", "cache-index");
    let nonlocal = fixture.memory(0xb02, "Cedar storage guidance.", "cache-index");
    write_beads(
        &fixture.workspace,
        "{\"id\":\"bd-storage\",\"title\":\"Cedar\",\"description\":\"storage\",\"labels\":[\"cache-index\"]}\n",
    );
    let context = resolve_id(&fixture.workspace, "bd-storage");
    let mut candidates = vec![
        candidate(0xb01, "Cedar storage guidance.", 0.5),
        candidate(0xb02, "Cedar storage guidance.", 0.5),
    ];
    candidates[0].section = PackSection::Failures;
    candidates[1].origin = Some(crate::pack::PackItemOrigin {
        lane: crate::pack::PackItemOrigin::GLOBAL_LANE.to_owned(),
        workspace_id: "separate-store".to_owned(),
    });
    let before = candidates.clone();
    let mut degraded = Vec::new();
    let summary = apply(
        &context,
        &fixture.db,
        &mut candidates,
        Some(7),
        &mut degraded,
    );
    assert!(degraded.is_empty());
    assert_eq!(candidates[0].memory_id, local);
    assert!(candidates[0].relevance > before[0].relevance);
    assert_eq!(candidates[0].section, PackSection::Failures);
    assert_eq!(candidates[0].content, before[0].content);
    assert_eq!(candidates[1].memory_id, nonlocal);
    assert_eq!(candidates[1], before[1]);
    assert_eq!(summary["memoryBiasApplied"], 1);
    assert_eq!(summary["matchedFeatureCount"], 3);
    assert_eq!(
        summary["topBiases"][0]["features"],
        serde_json::json!(["label", "titleToken", "descriptionToken"])
    );
    assert!(summary["determinismKey"]["basePackHash"].is_null());
    assert_eq!(
        summary["determinismKey"]["beadContextHash"],
        context.identity
    );
    assert!(!summary.to_string().contains("Cedar storage guidance"));

    // A missing tag table is unavailable evidence, never observed no-overlap.
    let unreadable = DbConnection::open_memory().unwrap();
    let mut unchanged = before.clone();
    let summary = apply(&context, &unreadable, &mut unchanged, None, &mut degraded);
    assert_eq!(unchanged, before);
    assert_eq!(summary["memoryBiasApplied"], 0);
    assert_eq!(summary["coldStart"], false);
    assert_eq!(
        summary["degraded"][0]["code"],
        BEAD_AFFINITY_UNAVAILABLE_CODE
    );
    assert!(summary["determinismKey"]["workspaceGeneration"].is_null());
}

#[test]
fn request_identity_binds_bead_inputs_even_when_selection_is_unchanged() {
    let mut request = ContextRequest::from_query("same selected content").unwrap();
    let draft = crate::pack::assemble_draft(
        request.query.clone(),
        request.budget,
        vec![candidate(0xb10, "Unchanged memory.", 0.8)],
    )
    .unwrap();
    let hash = |request: &ContextRequest| {
        compute_pack_hash_with_output_options_coordination_and_snapshot(
            request,
            &draft,
            &[],
            ContextPackOutputOptions::default(),
            None,
            Some(7),
        )
    };
    let baseline = hash(&request);
    request.bead_affinity_identity = Some("blake3:bead-context-a".to_owned());
    let first = hash(&request);
    assert_ne!(first, baseline);
    request.bead_affinity_identity = Some("blake3:bead-context-b".to_owned());
    assert_ne!(hash(&request), first);
    request.bead_affinity_identity = None;
    assert_eq!(hash(&request), baseline);
}

#[cfg(all(unix, feature = "lexical-bm25"))]
#[test]
fn shared_context_path_selects_bead_relevant_memories_and_observes_tracker_edits() {
    let fixture = Fixture::new();
    let storage = fixture.memory(
        0xb21,
        "Emberstone release planning covers cedar storage indexes.",
        "storage",
    );
    let network = fixture.memory(
        0xb22,
        "Emberstone release planning checks maple network routes.",
        "network",
    );
    write_beads(
        &fixture.workspace,
        concat!(
            "{\"id\":\"bd-storage\",\"labels\":[\"storage\"]}\n",
            "{\"id\":\"bd-network\",\"labels\":[\"network\"]}\n",
            "{\"id\":\"bd-cold\",\"labels\":[\"astrometry\"]}\n",
        ),
    );
    let database_path = fixture.workspace.join(".ee/ee.db");
    let indexed = crate::core::index::rebuild_index(&crate::core::index::IndexRebuildOptions {
        workspace_path: fixture.workspace.clone(),
        database_path: Some(database_path.clone()),
        index_dir: None,
        dry_run: false,
    })
    .unwrap();
    assert_eq!(
        indexed.status,
        crate::core::index::IndexRebuildStatus::Success
    );
    let mut opts = options(&fixture.workspace);
    opts.database_path = Some(database_path);
    let baseline = run_context_pack(&opts).unwrap();
    assert_eq!(baseline.data.pack.items.len(), 1);
    assert!(baseline.data.bead_affinity.is_none());
    let baseline_json: serde_json::Value =
        serde_json::from_str(&crate::output::render_context_response_json(&baseline)).unwrap();
    assert!(baseline_json["data"]["pack"].get("beadAffinity").is_none());

    opts.bead_id = Some("bd-storage".to_owned());
    let storage_pack = run_context_pack(&opts).unwrap();
    assert_eq!(storage_pack.data.pack.items[0].memory_id, storage);
    assert_eq!(
        storage_pack.data.bead_affinity.as_ref().unwrap()["memoryBiasApplied"],
        1
    );
    let repeated = run_context_pack(&opts).unwrap();
    assert_eq!(repeated.data.pack.items, storage_pack.data.pack.items);
    assert_eq!(repeated.data.pack.hash, storage_pack.data.pack.hash);
    opts.bead_id = Some("bd-network".to_owned());
    let network_pack = run_context_pack(&opts).unwrap();
    assert_eq!(network_pack.data.pack.items[0].memory_id, network);
    assert_ne!(network_pack.data.pack.hash, storage_pack.data.pack.hash);
    let network_json: serde_json::Value =
        serde_json::from_str(&crate::output::render_context_response_json(&network_pack)).unwrap();
    assert_eq!(
        network_json["data"]["pack"]["beadAffinity"]["beadId"],
        "bd-network"
    );

    opts.bead_id = Some("bd-cold".to_owned());
    let cold = run_context_pack(&opts).unwrap();
    assert_eq!(cold.data.pack.items, baseline.data.pack.items);
    assert_eq!(cold.data.bead_affinity.as_ref().unwrap()["coldStart"], true);
    assert!(
        cold.data
            .degraded
            .iter()
            .any(|entry| entry.code == BEAD_AFFINITY_COLD_START_CODE)
    );

    // Same bead ID, changed scoring tokens: a fresh request must change its
    // selected memory and identity despite unchanged DB/index generations.
    write_beads(
        &fixture.workspace,
        "{\"id\":\"bd-storage\",\"labels\":[\"network\"]}\n",
    );
    opts.bead_id = Some("bd-storage".to_owned());
    let edited = run_context_pack(&opts).unwrap();
    assert_eq!(edited.data.pack.items[0].memory_id, network);
    assert_ne!(
        edited.data.request.bead_affinity_identity,
        storage_pack.data.request.bead_affinity_identity
    );
    assert_ne!(edited.data.pack.hash, storage_pack.data.pack.hash);
    opts.bead_id = None;
    let unaffected = run_context_pack(&opts).unwrap();
    assert_eq!(unaffected.data.pack.items, baseline.data.pack.items);
    assert_eq!(unaffected.data.pack.hash, baseline.data.pack.hash);
}
