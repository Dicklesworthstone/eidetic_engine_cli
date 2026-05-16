use std::fs;
use std::path::{Path, PathBuf};

use ee::core::context::{ContextPackOptions, ContextPackOutputOptions, run_context_pack};
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
        "[graph.feature.ppr]\nenabled = true\n\n[graph.feature.proximity]\nenabled = true\n",
    )
    .map_err(|error| error.to_string())
}

fn context_options(
    workspace_path: &Path,
    db_path: &Path,
    ppr_weight: Option<f32>,
) -> ContextPackOptions {
    ContextPackOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: None,
        query: "structural reranking release seed".to_owned(),
        speed: SpeedMode::Default,
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
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: ContextPackOutputOptions::default(),
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

fn assert_context_ppr_witness(workspace_path: &Path, db_path: &Path) -> TestResult {
    let connection = DbConnection::open_file(db_path).map_err(|error| error.to_string())?;
    let workspace_id = stable_workspace_id(workspace_path);
    let snapshot = connection
        .get_latest_graph_snapshot(&workspace_id, GraphSnapshotType::MemoryLinks)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "expected memory_links graph snapshot for PPR witness".to_owned())?;
    let witnesses = connection
        .list_graph_algorithm_witnesses(&workspace_id, &snapshot.id, Some("personalized_pagerank"))
        .map_err(|error| error.to_string())?;
    if witnesses.len() != 1 {
        return Err(format!(
            "context PPR rerank should emit exactly one personalized_pagerank witness, got {}",
            witnesses.len()
        ));
    }

    let params: Value = serde_json::from_str(&witnesses[0].params_json)
        .map_err(|error| format!("PPR witness params must be JSON: {error}"))?;
    if params["seedCount"].as_u64().unwrap_or(0) == 0 {
        return Err(format!(
            "PPR witness params must record a non-empty seed set: {params}"
        ));
    }
    connection.close().map_err(|error| error.to_string())
}

#[test]
fn context_pack_with_ppr_emits_score_breakdown_and_matches_golden() -> TestResult {
    let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
    let workspace_path = temp_dir.path();
    let db_path = db_path(workspace_path);
    fs::create_dir_all(db_path.parent().ok_or("missing db parent")?)
        .map_err(|error| error.to_string())?;

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

    let response = run_context_pack(&context_options(workspace_path, &db_path, Some(1.0)))
        .map_err(|error| format!("context pack with PPR failed: {error:?}"))?;
    if ppr_breakdown_count(&response) == 0 {
        return Err(format!(
            "PPR score breakdown missing from context pack: {:?}",
            response.data.pack.items
        ));
    }
    assert_context_ppr_witness(workspace_path, &db_path)?;
    let neighbor_proximity = response
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

    let summary = serde_json::to_string_pretty(&ppr_snapshot_summary(&response))
        .map_err(|error| format!("serialize PPR snapshot summary: {error}"))?;
    let expected = include_str!("snapshots/pack_with_ppr.snap").trim_end();
    if summary != expected {
        return Err(format!(
            "PPR pack golden mismatch\nexpected:\n{expected}\nactual:\n{summary}"
        ));
    }

    let base_response = run_context_pack(&context_options(workspace_path, &db_path, Some(0.0)))
        .map_err(|error| format!("context pack without PPR failed: {error:?}"))?;
    if ppr_breakdown_count(&base_response) != 0 {
        return Err("PPR score breakdown should be absent when ppr_weight=0".to_owned());
    }

    Ok(())
}
