use std::fs;
use std::path::{Path, PathBuf};

use ee::core::context::{
    ContextPackOptions, ContextPackOutputOptions, attach_pack_dna_to_context_response,
    run_context_pack,
};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::{
    CreateMemoryLinkInput, DbConnection, GraphSnapshotType, MemoryLinkRelation, MemoryLinkSource,
};
use ee::graph::{CentralityRefreshOptions, CentralityRefreshStatus, refresh_graph_snapshot};
use ee::models::degradation::GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE;
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

fn pack_selection_signature(
    response: &ContextResponse,
) -> Vec<(String, u32, String, Option<(u32, u32, u32)>)> {
    response
        .data
        .pack
        .items
        .iter()
        .map(|item| {
            (
                item.memory_id.to_string(),
                item.relevance.into_inner().to_bits(),
                item.why.clone(),
                item.score_breakdown.map(|breakdown| {
                    (
                        breakdown.text_score.to_bits(),
                        breakdown.ppr_score.to_bits(),
                        breakdown.combined_score.to_bits(),
                    )
                }),
            )
        })
        .collect()
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

fn assert_no_context_ppr_artifacts(workspace_path: &Path, db_path: &Path) -> TestResult {
    let connection = DbConnection::open_file(db_path).map_err(|error| error.to_string())?;
    let workspace_id = stable_workspace_id(workspace_path);
    let snapshot = connection
        .get_latest_graph_snapshot(&workspace_id, GraphSnapshotType::MemoryLinks)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "expected memory_links graph snapshot for PPR witness".to_owned())?;
    let witnesses = connection
        .list_graph_algorithm_witnesses(&workspace_id, &snapshot.id, Some("personalized_pagerank"))
        .map_err(|error| error.to_string())?;
    if !witnesses.is_empty() {
        return Err(format!(
            "disabled pack PPR must not emit personalized_pagerank witnesses, got {}",
            witnesses.len()
        ));
    }
    let results = connection
        .list_graph_algorithm_results(&workspace_id, &snapshot.id, Some("personalized_pagerank"))
        .map_err(|error| error.to_string())?;
    if !results.is_empty() {
        return Err(format!(
            "disabled pack PPR must not create personalized_pagerank cache rows, got {}",
            results.len()
        ));
    }
    connection.close().map_err(|error| error.to_string())
}

fn ppr_unavailable_snapshot_summary(
    requested: &ContextResponse,
    zero_weight: &ContextResponse,
) -> Value {
    let pack_dna = requested.data.pack_dna.as_ref().unwrap_or(&Value::Null);
    json!({
        "schema": "ee.pack.ppr.unavailable.golden.v1",
        "requestedDegradationCount": requested.data.degraded.iter().filter(|entry| entry.code == GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE).count(),
        "selectionMatchesZeroWeight": pack_selection_signature(requested) == pack_selection_signature(zero_weight),
        "scoreBreakdownCount": ppr_breakdown_count(requested),
        "whyMentionsPpr": requested.data.pack.items.iter().any(|item| item.why.contains("Personalized PageRank")),
        "packDna": {
            "hasCommunityOfMass": !pack_dna["communityOfMass"].is_null(),
            "pprNeighborCount": pack_dna["pprNeighbors"].as_array().map_or(0, Vec::len),
            "unavailableDegradationCount": pack_dna["degraded"].as_array().map_or(0, |entries| entries.iter().filter(|entry| entry["code"] == GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE).count()),
        },
    })
}

#[test]
fn requested_pack_ppr_degrades_without_changing_textual_ranking() -> TestResult {
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

    let zero_weight = run_context_pack(&context_options(workspace_path, &db_path, Some(0.0)))
        .map_err(|error| format!("zero-weight context pack failed: {error:?}"))?;
    if ppr_breakdown_count(&zero_weight) != 0 {
        return Err("ppr_weight=0 must not emit a PPR score breakdown".to_owned());
    }
    if zero_weight
        .data
        .degraded
        .iter()
        .any(|entry| entry.code == GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE)
    {
        return Err("ppr_weight=0 must remain a silent PPR no-op".to_owned());
    }
    assert_pack_item_before(
        &zero_weight,
        &seed_id,
        &neighbor_id,
        "ppr_weight=0 should preserve textual seed ranking",
    )?;

    let mut requested = run_context_pack(&context_options(workspace_path, &db_path, Some(1.0)))
        .map_err(|error| format!("requested-PPR context pack failed: {error:?}"))?;
    if pack_selection_signature(&requested) != pack_selection_signature(&zero_weight) {
        return Err(format!(
            "requested but unavailable PPR changed textual selection\nrequested={:?}\nzero={:?}",
            requested.data.pack.items, zero_weight.data.pack.items
        ));
    }
    if ppr_breakdown_count(&requested) != 0 {
        return Err("unavailable pack PPR must not emit score breakdowns".to_owned());
    }
    if requested
        .data
        .pack
        .items
        .iter()
        .any(|item| item.why.contains("Personalized PageRank"))
    {
        return Err("unavailable pack PPR must not alter item why text".to_owned());
    }
    let unavailable = requested
        .data
        .degraded
        .iter()
        .filter(|entry| entry.code == GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE)
        .collect::<Vec<_>>();
    if unavailable.len() != 1 {
        return Err(format!(
            "requested pack PPR must emit exactly one upstream-unavailable degradation: {:?}",
            requested.data.degraded
        ));
    }
    if unavailable[0].severity.as_str() != "medium"
        || !unavailable[0].message.contains("FrankenNetworkX")
        || !unavailable[0].message.contains("textual ranking")
    {
        return Err(format!(
            "pack PPR degradation must explain the truthful fallback: {:?}",
            unavailable[0]
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
    assert_no_context_ppr_artifacts(workspace_path, &db_path)?;

    attach_pack_dna_to_context_response(&db_path, &mut requested);
    let pack_dna = requested
        .data
        .pack_dna
        .as_ref()
        .ok_or_else(|| "requested Pack DNA block is absent".to_owned())?;
    if pack_dna["pprNeighbors"]
        .as_array()
        .map_or(usize::MAX, Vec::len)
        != 0
    {
        return Err(format!(
            "production Pack DNA must not expose local PPR neighbors: {pack_dna}"
        ));
    }
    if pack_dna["communityOfMass"].is_null() {
        return Err(format!(
            "disabling PPR neighbors must retain non-PPR Pack DNA explanations: {pack_dna}"
        ));
    }
    let nested_unavailable = pack_dna["degraded"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry["code"] == GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE)
                .count()
        })
        .unwrap_or(0);
    if nested_unavailable != 1 {
        return Err(format!(
            "Pack DNA must expose exactly one nested PPR unavailable degradation: {pack_dna}"
        ));
    }
    let outer_unavailable = requested
        .data
        .degraded
        .iter()
        .filter(|entry| entry.code == GRAPH_PPR_UPSTREAM_UNAVAILABLE_CODE)
        .count();
    if outer_unavailable != 1 {
        return Err(format!(
            "Pack DNA must not duplicate the top-level PPR unavailable degradation: {:?}",
            requested.data.degraded
        ));
    }
    assert_no_context_ppr_artifacts(workspace_path, &db_path)?;

    let summary =
        serde_json::to_string_pretty(&ppr_unavailable_snapshot_summary(&requested, &zero_weight))
            .map_err(|error| format!("serialize unavailable-PPR snapshot summary: {error}"))?;
    let expected = include_str!("snapshots/pack_with_ppr.snap").trim_end();
    if summary != expected {
        return Err(format!(
            "unavailable-PPR pack golden mismatch\nexpected:\n{expected}\nactual:\n{summary}"
        ));
    }

    Ok(())
}
