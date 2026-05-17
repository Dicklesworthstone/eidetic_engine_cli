//! Criterion benchmark for `ee context --ppr-weight` (bd-ov09.4).
//!
//! Group name: `ee_context_with_ppr`
//!
//! Measures the same 1k-memory fixture with PPR disabled and enabled so the
//! bench job can track the PPR rerank overhead budget separately from the base
//! context-pack SLO.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::core::context::{
    ContextPackOptions, ContextPackOutputOptions, run_context_pack,
    run_context_pack_with_performance,
};
use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::db::{
    CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, DbConnection,
    MemoryLinkRelation, MemoryLinkSource,
};
use ee::graph::{CentralityRefreshOptions, CentralityRefreshStatus, refresh_graph_snapshot};
use ee::models::{MemoryScope, RedactionLevel, WorkspaceId};
use ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS;
use ee::search::SpeedMode;

const BENCH_GROUP_NAME: &str = "ee_context_with_ppr";
const MEMORY_COUNT: usize = 1_000;
#[allow(dead_code)]
const PPR_OVERHEAD_P50_BUDGET_MS: f64 = 30.0;

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn ensure_workspace_row(connection: &DbConnection, workspace_path: &Path) {
    let input = CreateWorkspaceInput {
        path: workspace_path.to_string_lossy().into_owned(),
        name: Some("context-with-ppr-bench".to_owned()),
    };
    connection
        .insert_workspace(&stable_workspace_id(workspace_path), &input)
        .expect("insert benchmark workspace row");
}

fn seed_benchmark_fixture(temp_dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");
    let index_dir = workspace_path.join(".ee").join("index");
    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");
    std::fs::write(
        workspace_path.join(".ee").join("config.toml"),
        "[graph.feature.ppr]\nenabled = true\n",
    )
    .expect("enable PPR benchmark feature flag");

    let connection = DbConnection::open_file(&db_path).expect("open benchmark db");
    connection.migrate().expect("migrate benchmark db");
    ensure_workspace_row(&connection, &workspace_path);
    let workspace_id = stable_workspace_id(&workspace_path);

    for index in 0..MEMORY_COUNT {
        let content = if index < 200 {
            format!(
                "PPR context benchmark primary memory {index:04}: structural reranking release seed evidence."
            )
        } else {
            format!(
                "PPR context benchmark background memory {index:04}: linked graph filler outside the query cohort."
            )
        };
        connection
            .insert_memory(
                &format!("mem_ppr_context_bench_{index:08}"),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content,
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.8,
                    importance: 0.8,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: Some("context-ppr-bench".to_owned()),
                    tags: vec!["bench".to_owned(), "context".to_owned(), "ppr".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("insert benchmark memory");
    }

    for index in 0..(MEMORY_COUNT - 1) {
        connection
            .insert_memory_link(
                &format!("link_{index:026}"),
                &CreateMemoryLinkInput {
                    src_memory_id: format!("mem_ppr_context_bench_{index:08}"),
                    dst_memory_id: format!("mem_ppr_context_bench_{:08}", index + 1),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("context-with-ppr-bench".to_owned()),
                    metadata_json: None,
                },
            )
            .expect("insert benchmark memory link");
    }

    let centrality = refresh_graph_snapshot(
        &connection,
        &workspace_id,
        &CentralityRefreshOptions::default(),
    )
    .expect("refresh benchmark centrality snapshot");
    assert_eq!(
        centrality.centrality.status,
        CentralityRefreshStatus::Refreshed
    );
    assert!(
        centrality.snapshot.is_some(),
        "benchmark fixture should persist memory_links graph snapshot"
    );

    let rebuild = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace_path.clone(),
        database_path: Some(db_path.clone()),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
    })
    .expect("rebuild benchmark index");
    assert_eq!(rebuild.status, IndexRebuildStatus::Success);
    assert_eq!(rebuild.memories_indexed as usize, MEMORY_COUNT);

    (workspace_path, db_path, index_dir)
}

fn options(
    workspace_path: &Path,
    db_path: &Path,
    index_dir: &Path,
    ppr_weight: Option<f32>,
) -> ContextPackOptions {
    ContextPackOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: Some(index_dir.to_path_buf()),
        query: "structural reranking release seed".to_owned(),
        speed: SpeedMode::Default,
        filters: Default::default(),
        profile: None,
        max_tokens: Some(4000),
        candidate_pool: Some(200),
        max_results: None,
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        redaction_level: RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: ContextPackOutputOptions::default(),
    }
}

fn timing_ms(performance: &serde_json::Value, name: &str) -> f64 {
    performance["data"]["timings"]
        .as_array()
        .and_then(|timings| {
            timings
                .iter()
                .find(|timing| timing["name"].as_str() == Some(name))
        })
        .and_then(|timing| timing["elapsedMs"].as_f64())
        .unwrap_or(0.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct StageDiagnostics {
    total_ms: f64,
    search_ms: f64,
    candidate_resolution_ms: f64,
    ppr_rerank_ms: f64,
    pack_assembly_ms: f64,
    pack_persistence_ms: f64,
}

impl StageDiagnostics {
    fn from_performance(performance: &serde_json::Value) -> Self {
        Self {
            total_ms: timing_ms(performance, "total"),
            search_ms: timing_ms(performance, "search"),
            candidate_resolution_ms: timing_ms(performance, "candidateResolution"),
            ppr_rerank_ms: timing_ms(performance, "pprRerank"),
            pack_assembly_ms: timing_ms(performance, "packAssembly"),
            pack_persistence_ms: timing_ms(performance, "packPersistence"),
        }
    }

    fn tracked_ms(self) -> f64 {
        self.search_ms
            + self.candidate_resolution_ms
            + self.ppr_rerank_ms
            + self.pack_assembly_ms
            + self.pack_persistence_ms
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OverheadAttribution {
    total_delta_ms: f64,
    search_delta_ms: f64,
    candidate_resolution_delta_ms: f64,
    ppr_rerank_delta_ms: f64,
    pack_assembly_delta_ms: f64,
    pack_persistence_delta_ms: f64,
    residual_delta_ms: f64,
}

impl OverheadAttribution {
    fn from_diagnostics(base: StageDiagnostics, ppr: StageDiagnostics) -> Self {
        let total_delta_ms = ppr.total_ms - base.total_ms;
        let search_delta_ms = ppr.search_ms - base.search_ms;
        let candidate_resolution_delta_ms =
            ppr.candidate_resolution_ms - base.candidate_resolution_ms;
        let ppr_rerank_delta_ms = ppr.ppr_rerank_ms - base.ppr_rerank_ms;
        let pack_assembly_delta_ms = ppr.pack_assembly_ms - base.pack_assembly_ms;
        let pack_persistence_delta_ms = ppr.pack_persistence_ms - base.pack_persistence_ms;
        let tracked_delta_ms = ppr.tracked_ms() - base.tracked_ms();
        let residual_delta_ms = total_delta_ms - tracked_delta_ms;
        Self {
            total_delta_ms,
            search_delta_ms,
            candidate_resolution_delta_ms,
            ppr_rerank_delta_ms,
            pack_assembly_delta_ms,
            pack_persistence_delta_ms,
            residual_delta_ms,
        }
    }

    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "schema": "ee.bench.context_with_ppr.attribution.v1",
            "budgetMs": PPR_OVERHEAD_P50_BUDGET_MS,
            "deltasMs": {
                "total": self.total_delta_ms,
                "search": self.search_delta_ms,
                "candidateResolution": self.candidate_resolution_delta_ms,
                "pprRerank": self.ppr_rerank_delta_ms,
                "packAssembly": self.pack_assembly_delta_ms,
                "packPersistence": self.pack_persistence_delta_ms,
                "residual": self.residual_delta_ms,
            },
            "budgetStatus": {
                "total": budget_status(self.total_delta_ms),
                "pprRerank": budget_status(self.ppr_rerank_delta_ms),
            },
        })
    }
}

fn budget_status(value_ms: f64) -> &'static str {
    if value_ms <= PPR_OVERHEAD_P50_BUDGET_MS {
        "within_budget"
    } else {
        "over_budget"
    }
}

fn emit_stage_diagnostics(workspace_path: &Path, db_path: &Path, index_dir: &Path) {
    let mut base_diagnostics = None;
    let mut ppr_diagnostics = None;

    for (label, ppr_weight) in [
        ("base_1000_memories", None),
        ("ppr_1000_memories", Some(0.5)),
    ] {
        let run = run_context_pack_with_performance(
            &options(workspace_path, db_path, index_dir, ppr_weight),
            "context",
        )
        .expect("context pack stage diagnostics");
        let diagnostics = StageDiagnostics::from_performance(&run.performance);
        println!(
            "ppr_stage_diagnostics label={label} total_ms={:.3} search_ms={:.3} candidate_resolution_ms={:.3} ppr_rerank_ms={:.3} pack_assembly_ms={:.3} pack_persistence_ms={:.3}",
            diagnostics.total_ms,
            diagnostics.search_ms,
            diagnostics.candidate_resolution_ms,
            diagnostics.ppr_rerank_ms,
            diagnostics.pack_assembly_ms,
            diagnostics.pack_persistence_ms,
        );
        if ppr_weight.is_some() {
            ppr_diagnostics = Some(diagnostics);
        } else {
            base_diagnostics = Some(diagnostics);
        }
        black_box(run.response.data.pack.hash);
    }

    if let (Some(base), Some(ppr)) = (base_diagnostics, ppr_diagnostics) {
        let attribution = OverheadAttribution::from_diagnostics(base, ppr);
        println!("ppr_overhead_attribution_json {}", attribution.to_json());
        println!(
            "ppr_overhead_attribution total_delta_ms={:.3} search_delta_ms={:.3} candidate_resolution_delta_ms={:.3} ppr_rerank_delta_ms={:.3} pack_assembly_delta_ms={:.3} pack_persistence_delta_ms={:.3} residual_delta_ms={:.3} budget_ms={PPR_OVERHEAD_P50_BUDGET_MS:.3} total_budget_status={} ppr_rerank_budget_status={}",
            attribution.total_delta_ms,
            attribution.search_delta_ms,
            attribution.candidate_resolution_delta_ms,
            attribution.ppr_rerank_delta_ms,
            attribution.pack_assembly_delta_ms,
            attribution.pack_persistence_delta_ms,
            attribution.residual_delta_ms,
            budget_status(attribution.total_delta_ms),
            budget_status(attribution.ppr_rerank_delta_ms),
        );
    }
}

fn bench_context_with_ppr(c: &mut Criterion) {
    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    let temp_dir = TempDir::new().expect("temp dir");
    let (workspace_path, db_path, index_dir) = seed_benchmark_fixture(temp_dir.path());

    for (label, ppr_weight) in [
        ("base_1000_memories", None),
        ("ppr_1000_memories", Some(0.5)),
    ] {
        group.bench_with_input(
            BenchmarkId::new("context_pack", label),
            &ppr_weight,
            |b, weight| {
                b.iter(|| {
                    let response =
                        run_context_pack(&options(&workspace_path, &db_path, &index_dir, *weight))
                            .expect("context pack");
                    black_box(response.data.pack.hash);
                });
            },
        );
    }

    group.finish();
    emit_stage_diagnostics(&workspace_path, &db_path, &index_dir);
}

criterion_group!(benches, bench_context_with_ppr);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "expected {actual} to equal {expected}"
        );
    }

    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(super::BENCH_GROUP_NAME, "ee_context_with_ppr");
        assert_eq!(super::MEMORY_COUNT, 1_000);
        assert_eq!(super::PPR_OVERHEAD_P50_BUDGET_MS, 30.0);
    }

    #[test]
    fn timing_ms_extracts_named_stage() {
        let performance = serde_json::json!({
            "data": {
                "timings": [
                    { "name": "search", "elapsedMs": 12.5 },
                    { "name": "pprRerank", "elapsedMs": 7.25 }
                ]
            }
        });

        assert_close(super::timing_ms(&performance, "pprRerank"), 7.25);
        assert_close(super::timing_ms(&performance, "missing"), 0.0);
    }

    #[test]
    fn stage_diagnostics_tracks_named_context_stages() {
        let performance = serde_json::json!({
            "data": {
                "timings": [
                    { "name": "total", "elapsedMs": 100.0 },
                    { "name": "search", "elapsedMs": 80.0 },
                    { "name": "candidateResolution", "elapsedMs": 2.0 },
                    { "name": "pprRerank", "elapsedMs": 7.0 },
                    { "name": "packAssembly", "elapsedMs": 3.0 },
                    { "name": "packPersistence", "elapsedMs": 5.0 }
                ]
            }
        });

        let diagnostics = super::StageDiagnostics::from_performance(&performance);

        assert_close(diagnostics.total_ms, 100.0);
        assert_close(diagnostics.tracked_ms(), 97.0);
        assert_eq!(super::budget_status(7.0), "within_budget");
        assert_eq!(super::budget_status(31.0), "over_budget");
    }

    #[test]
    fn overhead_attribution_json_preserves_stage_deltas_and_budget_status() {
        let base = super::StageDiagnostics {
            total_ms: 100.0,
            search_ms: 80.0,
            candidate_resolution_ms: 2.0,
            ppr_rerank_ms: 0.0,
            pack_assembly_ms: 3.0,
            pack_persistence_ms: 5.0,
        };
        let ppr = super::StageDiagnostics {
            total_ms: 135.0,
            search_ms: 82.0,
            candidate_resolution_ms: 2.5,
            ppr_rerank_ms: 7.0,
            pack_assembly_ms: 3.25,
            pack_persistence_ms: 6.0,
        };

        let attribution = super::OverheadAttribution::from_diagnostics(base, ppr);
        let json = attribution.to_json();

        assert_eq!(json["schema"], "ee.bench.context_with_ppr.attribution.v1");
        assert_close(json["deltasMs"]["total"].as_f64().unwrap(), 35.0);
        assert_close(json["deltasMs"]["search"].as_f64().unwrap(), 2.0);
        assert_close(
            json["deltasMs"]["candidateResolution"].as_f64().unwrap(),
            0.5,
        );
        assert_close(json["deltasMs"]["pprRerank"].as_f64().unwrap(), 7.0);
        assert_close(json["deltasMs"]["packAssembly"].as_f64().unwrap(), 0.25);
        assert_close(json["deltasMs"]["packPersistence"].as_f64().unwrap(), 1.0);
        assert_close(json["deltasMs"]["residual"].as_f64().unwrap(), 24.25);
        assert_eq!(json["budgetStatus"]["total"], "over_budget");
        assert_eq!(json["budgetStatus"]["pprRerank"], "within_budget");
    }
}
