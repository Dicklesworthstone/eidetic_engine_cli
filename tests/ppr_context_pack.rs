use std::fs;
use std::path::{Path, PathBuf};

use ee::core::context::{ContextPackOptions, ContextPackOutputOptions, run_context_pack};
use ee::core::init::{InitOptions, init_workspace};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::{
    CreateMemoryLinkInput, DbConnection, GraphSnapshotType, MemoryLinkRelation, MemoryLinkSource,
};
use ee::graph::{CentralityRefreshOptions, CentralityRefreshStatus, refresh_graph_snapshot};
use ee::models::{MemoryScope, WorkspaceId};
use ee::pack::ContextResponse;
use ee::search::SpeedMode;
use serde_json::{Value, json};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, String>;

/// The stand-down code bd-reality-core-convergence-1azkt.82 retired. Nothing
/// may emit it now that FrankenNetworkX computes personalized PageRank.
const RETIRED_PPR_UPSTREAM_CODE: &str = "graph_ppr_upstream_unavailable";

fn db_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ee").join("ee.db")
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn remember_fixture(workspace_path: &Path, db_path: &Path, content: &str) -> TestResult<String> {
    let report = remember_memory(&RememberMemoryOptions {
        workspace_path,
        database_path: Some(db_path),
        content,
        workflow_id: None,
        level: "semantic",
        kind: "note",
        tags: Some("ppr,context,golden"),
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

fn insert_support_link(
    workspace_path: &Path,
    db_path: &Path,
    seed_id: &str,
    neighbor_id: &str,
) -> TestResult {
    let connection = DbConnection::open_file(db_path).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000100401",
            &CreateMemoryLinkInput {
                src_memory_id: seed_id.to_owned(),
                dst_memory_id: neighbor_id.to_owned(),
                relation: MemoryLinkRelation::Supports,
                weight: 1.0,
                confidence: 1.0,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Agent,
                created_by: Some("ppr-context-pack-test".to_owned()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;

    let refresh = refresh_graph_snapshot(
        &connection,
        &stable_workspace_id(workspace_path),
        &CentralityRefreshOptions::default(),
    )
    .map_err(|error| error.to_string())?;
    if refresh.centrality.status != CentralityRefreshStatus::Refreshed {
        return Err(format!(
            "expected refreshed centrality snapshot, got {:?}",
            refresh.centrality.status
        ));
    }
    if refresh.snapshot.is_none() {
        return Err("expected persisted memory_links graph snapshot".to_owned());
    }
    connection.close().map_err(|error| error.to_string())
}

fn enable_ppr_feature(workspace_path: &Path) -> TestResult {
    let config_dir = workspace_path.join(".ee");
    fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
    fs::write(
        config_dir.join("config.toml"),
        "[graph.feature.ppr]\nenabled = true\n\n[graph.feature.proximity]\nenabled = true\n\n[graph.feature.pack_dna]\nenabled = true\n",
    )
    .map_err(|error| error.to_string())
}

fn context_options(
    workspace_path: &Path,
    db_path: &Path,
    ppr_weight: Option<f32>,
) -> ContextPackOptions {
    ContextPackOptions {
        bead_id: None,
        task_paths: Vec::new(),
        task_lens: None,
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: None,
        query: "structural reranking release seed".to_owned(),
        speed: SpeedMode::Default,
        source_mode: ee::core::search::SearchSourceMode::Hybrid,
        strict_source_mode: false,
        filters: Default::default(),
        profile: None,
        max_tokens: Some(1000),
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
        no_global: false,
        strict_scope: false,
        ppr_weight,
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: ContextPackOutputOptions::default(),
        persist_pack: true,
        baseline_write: None,
        no_lod: false,
        require_fresh_sentinels: false,
    }
}

fn ppr_breakdown_count(response: &ContextResponse) -> usize {
    response
        .data
        .pack
        .items
        .iter()
        .filter(|item| item.score_breakdown.is_some())
        .count()
}

fn assert_pack_item_before(
    response: &ContextResponse,
    earlier_memory_id_raw: &str,
    later_memory_id_raw: &str,
    context: &str,
) -> TestResult {
    let rank = |memory_id: &str| {
        response
            .data
            .pack
            .items
            .iter()
            .position(|item| item.memory_id.to_string() == memory_id)
    };
    let earlier_rank = rank(earlier_memory_id_raw).ok_or_else(|| {
        format!(
            "{context}: expected earlier memory {earlier_memory_id_raw} in pack items: {:?}",
            response.data.pack.items
        )
    })?;
    let later_rank = rank(later_memory_id_raw).ok_or_else(|| {
        format!(
            "{context}: expected later memory {later_memory_id_raw} in pack items: {:?}",
            response.data.pack.items
        )
    })?;
    if earlier_rank >= later_rank {
        return Err(format!(
            "{context}: expected {earlier_memory_id_raw} rank {earlier_rank} before {later_memory_id_raw} rank {later_rank}"
        ));
    }
    Ok(())
}

/// `ee context` is a read-only command, so the rerank must leave the store as it
/// found it. Until the result cache skipped its row on a read-only connection,
/// that row write failed and took the whole rerank down with it: the old
/// "exactly one witness" assertion here was never reached.
fn assert_context_ppr_wrote_nothing(workspace_path: &Path, db_path: &Path) -> TestResult {
    let connection = DbConnection::open_file(db_path).map_err(|error| error.to_string())?;
    let workspace_id = stable_workspace_id(workspace_path);
    let snapshot = connection
        .get_latest_graph_snapshot(&workspace_id, GraphSnapshotType::MemoryLinks)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "expected memory_links graph snapshot for the PPR rerank".to_owned())?;
    let witnesses = connection
        .list_graph_algorithm_witnesses(&workspace_id, &snapshot.id, Some("personalized_pagerank"))
        .map_err(|error| error.to_string())?;
    let cached = connection
        .list_graph_algorithm_results(&workspace_id, &snapshot.id, Some("personalized_pagerank"))
        .map_err(|error| error.to_string())?;
    if !witnesses.is_empty() || !cached.is_empty() {
        return Err(format!(
            "a read-only context pack wrote {} personalized_pagerank witness(es) and {} cache row(s)",
            witnesses.len(),
            cached.len()
        ));
    }
    connection.close().map_err(|error| error.to_string())
}

fn ppr_snapshot_summary(response: &ContextResponse) -> Value {
    let items = response
        .data
        .pack
        .items
        .iter()
        .filter_map(|item| {
            let breakdown = item.score_breakdown?;
            let text_score = f64::from(breakdown.text_score);
            let ppr_score = f64::from(breakdown.ppr_score);
            let combined_score = f64::from(breakdown.combined_score);
            Some(json!({
                "combinedEqualsPpr": (combined_score - ppr_score).abs() < 0.000001,
                "content": item.content,
                "pprScorePositive": ppr_score > 0.0,
                "scoreBreakdownKeys": ["combinedScore", "pprScore", "textScore"],
                "textScorePositive": text_score > 0.0,
                "whyMentionsPpr": item.why.contains("Personalized PageRank"),
            }))
        })
        .collect::<Vec<_>>();

    json!({
        "schema": "ee.pack.ppr.golden.v1",
        "pprItemCount": items.len(),
        "items": items,
    })
}

fn retired_ppr_code_count(response: &ContextResponse) -> usize {
    let pack_dna_count = response
        .data
        .pack_dna
        .as_ref()
        .and_then(|pack_dna| pack_dna["degraded"].as_array())
        .map_or(0, |entries| {
            entries
                .iter()
                .filter(|entry| entry["code"] == RETIRED_PPR_UPSTREAM_CODE)
                .count()
        });
    response
        .data
        .degraded
        .iter()
        .filter(|entry| entry.code == RETIRED_PPR_UPSTREAM_CODE)
        .count()
        + pack_dna_count
}

/// bd-reality-core-convergence-1azkt.82 restores the contract .15 stood down:
/// fnx personalized PageRank reranks the pack and Pack DNA ranks PPR
/// neighbors again.
///
/// Why weight 1 promotes the neighbor: the only link is seed -> neighbor, and
/// fnx (damping d = 0.85) returns dangling mass to the personalization, so
/// the neighbor scores K(p_n + d p_s) against the seed's K p_s. It therefore
/// leads exactly when its search score exceeds (1 - d) = 0.15 of the seed's.
/// It matches three of the query's four terms, so a red here means it lost
/// its search hit rather than that the kernel changed.
#[test]
fn context_pack_with_ppr_emits_score_breakdown_and_matches_golden() -> TestResult {
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace = temp_dir
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let workspace_path = workspace.as_path();
    let db_path = db_path(workspace_path);
    let init = init_workspace(&InitOptions {
        workspace_path: workspace.clone(),
        dry_run: false,
        repair_plan: false,
        force: false,
        allow_symlink: false,
        skip_boilerplate: true,
    });
    if !init.status.is_success() {
        return Err(format!("initialize PPR fixture workspace failed: {init:?}"));
    }

    let seed_id = remember_fixture(
        workspace_path,
        &db_path,
        "PPR golden fixture structural reranking release seed memory.",
    )?;
    let neighbor_id = remember_fixture(
        workspace_path,
        &db_path,
        "PPR golden fixture structural reranking release neighbor memory.",
    )?;
    let _baseline_id = remember_fixture(
        workspace_path,
        &db_path,
        "PPR golden fixture structural reranking release baseline memory.",
    )?;
    enable_ppr_feature(workspace_path)?;
    insert_support_link(workspace_path, &db_path, &seed_id, &neighbor_id)?;

    // Weight 1 runs first, so the no-write check below covers the run that
    // reaches the rerank; the zero-weight run never does.
    let mut requested_options = context_options(workspace_path, &db_path, Some(1.0));
    requested_options.output_options = requested_options.output_options.with_pack_dna(true);
    let requested = run_context_pack(&requested_options)
        .map_err(|error| format!("context pack with PPR failed: {error:?}"))?;
    if ppr_breakdown_count(&requested) == 0 {
        return Err(format!(
            "PPR score breakdown missing from context pack: degraded={:?} items={:?}",
            requested.data.degraded, requested.data.pack.items
        ));
    }
    assert_pack_item_before(
        &requested,
        &neighbor_id,
        &seed_id,
        "ppr_weight=1 should visibly promote the graph-linked neighbor",
    )?;
    assert_context_ppr_wrote_nothing(workspace_path, &db_path)?;
    if retired_ppr_code_count(&requested) != 0 {
        return Err(format!(
            "{RETIRED_PPR_UPSTREAM_CODE} is retired and must not be emitted: {:?}",
            requested.data.degraded
        ));
    }

    let neighbor_proximity = requested
        .data
        .pack
        .items
        .iter()
        .find(|item| item.memory_id.to_string() == neighbor_id)
        .and_then(|item| item.proximity_to_seed)
        .ok_or_else(|| "neighbor item missing proximityToSeed".to_owned())?;
    if neighbor_proximity < 1.0 {
        return Err(format!(
            "neighbor proximityToSeed should reflect the seeded support link; got {neighbor_proximity}"
        ));
    }

    // Every linked memory is in this pack, so each one is a Pack DNA seed and
    // none can be a PPR neighbor; the non-empty default is pinned in
    // graph::pack_dna. What this proves is that the production path ran
    // without a stand-down: any neighbor it reports lies outside the pack.
    let pack_dna = requested
        .data
        .pack_dna
        .as_ref()
        .ok_or_else(|| "requested Pack DNA block is absent".to_owned())?;
    if pack_dna["communityOfMass"].is_null() {
        return Err(format!(
            "Pack DNA must keep its community of mass beside PPR: {pack_dna}"
        ));
    }
    let pack_ids = requested
        .data
        .pack
        .items
        .iter()
        .map(|item| item.memory_id.to_string())
        .collect::<Vec<_>>();
    let neighbors = pack_dna["pprNeighbors"]
        .as_array()
        .ok_or_else(|| format!("Pack DNA pprNeighbors must be an array: {pack_dna}"))?;
    if !neighbors.iter().all(|neighbor| {
        neighbor["memoryId"]
            .as_str()
            .is_some_and(|memory_id| !pack_ids.iter().any(|id| id == memory_id))
            && neighbor["score"]
                .as_f64()
                .is_some_and(|score| score.is_finite() && score > 0.0)
    }) {
        return Err(format!(
            "Pack DNA PPR neighbors must lie outside the pack with positive rank: {pack_dna}"
        ));
    }

    let summary = serde_json::to_string_pretty(&ppr_snapshot_summary(&requested))
        .map_err(|error| format!("serialize PPR snapshot summary: {error}"))?;
    let expected = include_str!("snapshots/pack_with_ppr.snap").trim_end();
    if summary != expected {
        return Err(format!(
            "PPR pack golden mismatch\nexpected:\n{expected}\nactual:\n{summary}"
        ));
    }

    let zero_weight = run_context_pack(&context_options(workspace_path, &db_path, Some(0.0)))
        .map_err(|error| format!("zero-weight context pack failed: {error:?}"))?;
    if ppr_breakdown_count(&zero_weight) != 0 {
        return Err("PPR score breakdown should be absent when ppr_weight=0".to_owned());
    }
    if retired_ppr_code_count(&zero_weight) != 0 {
        return Err("ppr_weight=0 must remain a silent PPR no-op".to_owned());
    }
    assert_pack_item_before(
        &zero_weight,
        &seed_id,
        &neighbor_id,
        "ppr_weight=0 should preserve the textual seed ahead of its graph neighbor",
    )?;

    Ok(())
}
