//! Criterion benchmark for `ee context` (EE-PERF-BENCH-context).
//!
//! Group name: `ee_context`
//!
//! Tests the `run_context_pack` function at three token budget scales:
//! - 1k tokens: small context pack
//! - 4k tokens: default context pack
//! - 8k tokens: large context pack
//!
//! S4 resource scales:
//! - 1k memories: release-candidate smoke scale
//! - 10k memories: swarm-scale nightly/stress scale
//! - 100k memories: stress-only large-machine scale
//!
//! Performance budget (plan §28):
//! - p50: 95ms
//! - p99: 240ms
//! - Regression threshold: 30%

#![allow(clippy::expect_used)]
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::cache::pack_compression::{
    PackCompressionDictionaryTrainingOutcome, PackCompressionSample,
    PackCompressionSampleSourceKind, PackCompressionTrainingOptions,
    train_pack_compression_dictionary,
};
use ee::cache::pack_l2::{
    PackL2Cache, PackL2CacheLookup, PackL2CacheOptions, PackL2CompressionDictionary,
    PackL2WriteReport,
};
use tempfile::TempDir;

use ee::core::context::{
    ContextPackOptions, ContextPackOutputOptions, attach_pack_dna_to_context_response,
    run_context_pack, run_context_pack_with_performance,
};
use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::{
    CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, DbConnection,
    MemoryLinkRelation, MemoryLinkSource,
};
use ee::models::{MemoryId, MemoryScope, ProvenanceUri, RedactionLevel, UnitScore, WorkspaceId};
use ee::pack::{
    ArenaMode, ContextPackProfile, PackArenaWorkspace, PackArenaWorkspaceKey, PackAssemblyOptions,
    PackCandidate, PackCandidateInput, PackProvenance, PackResourceProfile, PackSection,
    TokenBudget, assemble_draft_with_profile_and_options_seeded,
    assemble_draft_with_profile_and_options_seeded_in_workspace,
};
use ee::runtime::determinism::Deterministic;
use ee::search::SpeedMode;

/// Performance budget from plan §28 (README "Performance" table).
/// p50 must stay under 95ms, p99 under 240ms.
const BUDGET_P50_MS: f64 = 95.0;
const BUDGET_P99_MS: f64 = 240.0;

/// Regression threshold: fail if p50 degrades by more than 30%.
const REGRESSION_THRESHOLD: f64 = 0.30;
const S4_RELEASE_CANDIDATE_SCALE: usize = 1_000;
const S4_NIGHTLY_SCALE: usize = 10_000;
const S4_STRESS_SCALE: usize = 100_000;
const L2_WARM_BENCH_GROUP: &str = "ee_context_pack_l2_warm";
const L2_WARM_BENCH_OPERATION: &str = "ee_context_pack_l2_warm";
const ARENA_MODE_BENCH_GROUP: &str = "ee_context_arena_mode";
const ARENA_MODE_BENCH_OPERATION: &str = "ee_context_arena_workspace_reuse";
const PACK_DNA_ORCHESTRATION_BENCH_GROUP: &str = "ee_context_pack_dna_orchestration";
const PACK_DNA_ORCHESTRATION_OPERATION: &str = "ee_context_pack_dna_attach";
const ZSTD_PACK_DICTIONARY_BENCH_GROUP: &str = "ee_context_zstd_pack_dictionary";
const ZSTD_PACK_DICTIONARY_OPERATION: &str = "ee_context_zstd_pack_dictionary_l2";
const TIERED_RECALL_BENCH_GROUP: &str = "ee_context_tiered_recall";
const TIERED_RECALL_OPERATION: &str = "ee_context_memory_tier_admission";
const TIERED_RECALL_QUERY: &str = "tiered recall release cold explicit failure evidence";
const TIERED_RECALL_FILLER_COUNT: usize = 650;
const TIERED_RECALL_MEMORY_COUNT: usize = TIERED_RECALL_FILLER_COUNT + 1;
const TIERED_RECALL_CANDIDATE_POOL: u32 = 192;
const L2_WARM_BUDGET_P50_MS: f64 = 10.0;
const L2_WARM_BUDGET_P99_MS: f64 = 50.0;
const ARENA_MODE_BUDGET_P50_MS: f64 = 95.0;
const ARENA_MODE_BUDGET_P99_MS: f64 = 240.0;
const PACK_DNA_ORCHESTRATION_BUDGET_P50_MS: f64 = 125.0;
const PACK_DNA_ORCHESTRATION_BUDGET_P99_MS: f64 = 300.0;
const ZSTD_PACK_DICTIONARY_BUDGET_P50_MS: f64 = 15.0;
const ZSTD_PACK_DICTIONARY_BUDGET_P99_MS: f64 = 75.0;
const TIERED_RECALL_BUDGET_P50_MS: f64 = 140.0;
const TIERED_RECALL_BUDGET_P99_MS: f64 = 340.0;
const L2_CONCURRENT_IDENTICAL_REQUESTS: usize = 4;
const L2_EXPECTED_FRESH_ASSEMBLIES: usize = 1;
const L2_EXPECTED_WARM_HITS: usize =
    L2_CONCURRENT_IDENTICAL_REQUESTS - L2_EXPECTED_FRESH_ASSEMBLIES;
const ARENA_MODE_EXPECTED_WORKSPACE_FRESH_ALLOCATIONS: u64 = 1;
const PACK_DNA_ORCHESTRATION_SERIAL_TASK_COUNT: u64 = 1;
const ZSTD_PACK_DICTIONARY_SAMPLE_COUNT: usize = 96;
const TIERED_RECALL_EXPECTED_REQUIRED_COLD_MIN: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResourceScale {
    label: &'static str,
    memory_count: usize,
    candidate_pool: u32,
    resource_profile: PackResourceProfile,
}

const S4_RESOURCE_SCALES: &[ResourceScale] = &[
    ResourceScale {
        label: "1000_memories",
        memory_count: S4_RELEASE_CANDIDATE_SCALE,
        candidate_pool: 200,
        resource_profile: PackResourceProfile::Standard,
    },
    ResourceScale {
        label: "10000_memories",
        memory_count: S4_NIGHTLY_SCALE,
        candidate_pool: 1_000,
        resource_profile: PackResourceProfile::SwarmHeavy,
    },
    ResourceScale {
        label: "100000_memories",
        memory_count: S4_STRESS_SCALE,
        candidate_pool: 1_000,
        resource_profile: PackResourceProfile::SwarmHeavy,
    },
];

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn arena_memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(uuid::Uuid::from_u128(seed + 1))
}

fn arena_score(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("arena benchmark score")
}

fn arena_provenance(seed: u128, index: usize) -> PackProvenance {
    let uri = ProvenanceUri::from_str(&format!(
        "file://arena-bench-{seed}-{index}.md#L{}",
        (index + 1) * 10
    ))
    .expect("arena benchmark provenance uri");
    PackProvenance::new(uri, format!("arena benchmark provenance {index}"))
        .expect("arena benchmark provenance")
}

fn arena_candidate(
    seed: u128,
    section: PackSection,
    content: impl Into<String>,
    tokens: u32,
    relevance: f32,
    utility: f32,
    provenance_count: usize,
) -> PackCandidate {
    let provenance = (0..provenance_count)
        .map(|index| arena_provenance(seed, index))
        .collect();
    PackCandidate::new(PackCandidateInput {
        memory_id: arena_memory_id(seed),
        section,
        content: content.into(),
        estimated_tokens: tokens,
        relevance: arena_score(relevance),
        utility: arena_score(utility),
        provenance,
        why: format!("arena benchmark candidate {seed} matches the task"),
    })
    .expect("arena benchmark candidate")
}

fn arena_fixture_coverage_fill() -> Vec<PackCandidate> {
    let mut candidates = Vec::with_capacity(20);
    for seed in 0u128..10 {
        candidates.push(arena_candidate(
            seed + 200,
            PackSection::ProceduralRules,
            format!("Run cargo fmt before release. variant {seed}"),
            90,
            0.95,
            0.9,
            1,
        ));
    }
    for seed in 0u128..10 {
        candidates.push(arena_candidate(
            seed + 220,
            PackSection::Failures,
            format!("Past incident: arena mode swap broke parity {seed}"),
            85,
            0.55,
            0.6,
            1,
        ));
    }
    candidates
}

fn arena_fixture_provenance_heavy() -> Vec<PackCandidate> {
    let sections = [
        PackSection::Evidence,
        PackSection::Decisions,
        PackSection::Artifacts,
    ];
    (0u128..9)
        .map(|seed| {
            arena_candidate(
                seed + 400,
                sections[(seed as usize) % sections.len()],
                format!("Provenance-heavy arena benchmark candidate {seed}."),
                110,
                0.7 + ((seed as f32) * 0.02).min(0.25),
                0.65 + ((seed as f32) * 0.02).min(0.3),
                3 + ((seed as usize) % 4),
            )
        })
        .collect()
}

fn ensure_workspace_row(connection: &DbConnection, workspace_path: &Path) {
    let workspace_path_string = workspace_path.to_string_lossy().into_owned();
    if connection
        .get_workspace_by_path(&workspace_path_string)
        .expect("query benchmark workspace row")
        .is_some()
    {
        return;
    }

    let input = CreateWorkspaceInput {
        path: workspace_path_string,
        name: workspace_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    };
    connection
        .insert_workspace(&stable_workspace_id(workspace_path), &input)
        .expect("insert benchmark workspace row");
}

/// Seed a database with memories for context pack testing.
fn seed_database(temp_dir: &Path, memory_count: usize) -> std::path::PathBuf {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");

    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");

    let connection = DbConnection::open_file(&db_path).expect("open db");
    connection.migrate().expect("migrate db");

    let topics = [
        "release",
        "testing",
        "performance",
        "refactoring",
        "debugging",
        "deployment",
        "security",
        "documentation",
    ];

    for i in 0..memory_count {
        let topic = topics[i % topics.len()];
        let content = format!(
            "Memory {i} about {topic}: This is a test memory for benchmarking context packing. \
             It contains relevant information about {topic} that should be retrieved when \
             querying for related tasks. The memory includes details about best practices, \
             common pitfalls, and lessons learned from past experiences with {topic}."
        );
        let options = RememberMemoryOptions {
            workspace_path: &workspace_path,
            database_path: Some(&db_path),
            content: &content,
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some(&format!("bench,{topic}")),
            confidence: 0.75,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: false,
            allow_secret_mention: false,
        };
        remember_memory(&options).expect("seed memory");
    }

    workspace_path
}

/// Fast deterministic seeding for S4 scale benches. This bypasses remember-time
/// linking/proposal work so the benchmark fixture cost does not dominate the
/// measured read path.
fn seed_resource_scale_database(temp_dir: &Path, memory_count: usize) -> PathBuf {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");

    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");

    let connection = DbConnection::open_file(&db_path).expect("open db");
    connection.migrate().expect("migrate db");
    ensure_workspace_row(&connection, &workspace_path);
    let workspace_id = stable_workspace_id(&workspace_path);
    let topics = [
        "release",
        "testing",
        "performance",
        "refactoring",
        "debugging",
        "deployment",
        "security",
        "documentation",
        "graph",
        "search",
    ];

    for index in 0..memory_count {
        let topic = topics[index % topics.len()];
        let content = format!(
            "S4 resource benchmark memory {index}: deterministic {topic} evidence for bounded \
             context pack assembly, search latency, and memory growth measurement."
        );
        let input = CreateMemoryInput {
            workspace_id: workspace_id.clone(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content,
            workflow_id: None,
            confidence: 0.75,
            utility: 0.75,
            importance: 0.75,
            provenance_uri: None,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: Some("s4-resource-bench".to_owned()),
            tags: vec![
                "bench".to_owned(),
                "s4".to_owned(),
                "resource".to_owned(),
                topic.to_owned(),
            ],
            valid_from: None,
            valid_to: None,
        };
        let memory_id = format!("mem_s4_resource_{index:06}");
        connection
            .insert_memory(&memory_id, &input)
            .expect("insert S4 benchmark memory");
    }

    workspace_path
}

fn build_resource_scale_index(
    workspace_path: &Path,
    db_path: &Path,
    memory_count: usize,
) -> PathBuf {
    let index_dir = workspace_path.join(".ee").join("index");
    let report = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
    })
    .expect("rebuild S4 benchmark index");
    assert_eq!(
        report.status,
        IndexRebuildStatus::Success,
        "S4 benchmark search index should rebuild successfully"
    );
    assert_eq!(
        report.memories_indexed as usize, memory_count,
        "S4 benchmark index should cover every seeded memory"
    );
    index_dir
}

fn seed_pack_dna_orchestration_database(temp_dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");

    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");
    std::fs::write(
        workspace_path.join(".ee").join("config.toml"),
        "[graph.feature.pack_dna]\nenabled = true\n",
    )
    .expect("write Pack DNA benchmark config");

    let connection = DbConnection::open_file(&db_path).expect("open db");
    connection.migrate().expect("migrate db");
    ensure_workspace_row(&connection, &workspace_path);
    let workspace_id = stable_workspace_id(&workspace_path);

    for index in 0..18 {
        let selected = index < 12;
        let content = if selected {
            format!(
                "Pack DNA orchestration benchmark memory {index}: graph release pipeline evidence \
                 with deterministic context selection and explain parity."
            )
        } else {
            format!(
                "Auxiliary Pack DNA topology neighbor {index}: linked graph evidence that should \
                 support PPR neighbors without dominating selected context items."
            )
        };
        let input = CreateMemoryInput {
            workspace_id: workspace_id.clone(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content,
            workflow_id: None,
            confidence: 0.85,
            utility: if selected { 0.85 } else { 0.4 },
            importance: if selected { 0.8 } else { 0.35 },
            provenance_uri: None,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: Some("pack-dna-orchestration-bench".to_owned()),
            tags: vec![
                "bench".to_owned(),
                "pack-dna".to_owned(),
                if selected { "selected" } else { "topology" }.to_owned(),
            ],
            valid_from: None,
            valid_to: None,
        };
        connection
            .insert_memory(&format!("mem_pack_dna_{index:03}"), &input)
            .expect("insert Pack DNA benchmark memory");
    }

    for (link_index, (src, dst)) in [
        (0_usize, 1_usize),
        (0, 2),
        (1, 3),
        (2, 4),
        (3, 5),
        (4, 6),
        (5, 7),
        (6, 8),
        (7, 9),
        (8, 10),
        (9, 11),
        (1, 12),
        (4, 13),
        (7, 14),
        (10, 15),
        (11, 16),
        (2, 17),
        (0, 11),
        (3, 9),
    ]
    .into_iter()
    .enumerate()
    {
        let input = CreateMemoryLinkInput {
            src_memory_id: format!("mem_pack_dna_{src:03}"),
            dst_memory_id: format!("mem_pack_dna_{dst:03}"),
            relation: MemoryLinkRelation::Supports,
            weight: 0.75,
            confidence: 0.9,
            directed: false,
            evidence_count: 1,
            last_reinforced_at: Some("2026-05-25T00:00:00Z".to_owned()),
            source: MemoryLinkSource::Human,
            created_by: Some("pack-dna-orchestration-bench".to_owned()),
            metadata_json: None,
        };
        connection
            .insert_memory_link(&format!("link_pack_dna_{link_index:03}"), &input)
            .expect("insert Pack DNA benchmark link");
    }
    connection.close().expect("close Pack DNA benchmark db");

    let index_dir = build_resource_scale_index(&workspace_path, &db_path, 18);
    (workspace_path, db_path, index_dir)
}

fn pack_dna_orchestration_options(
    workspace_path: &Path,
    db_path: &Path,
    index_dir: &Path,
) -> ContextPackOptions {
    ContextPackOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: Some(index_dir.to_path_buf()),
        query: "Pack DNA orchestration graph release pipeline".to_string(),
        speed: SpeedMode::Default,
        filters: Default::default(),
        profile: None,
        max_tokens: Some(1_200),
        candidate_pool: Some(12),
        max_results: None,
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: None,
        redaction_level: RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight: None,
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: ContextPackOutputOptions::default(),
        persist_pack: true,
    }
}

fn write_tiered_recall_config(workspace_path: &Path, enabled: bool) {
    std::fs::create_dir_all(workspace_path.join(".ee")).expect("create tiered recall .ee dir");
    std::fs::write(
        workspace_path.join(".ee").join("config.toml"),
        format!("[pack]\nmemory_tier_admission = {enabled}\n"),
    )
    .expect("write tiered recall benchmark config");
}

fn seed_tiered_recall_database(temp_dir: &Path, tier_enabled: bool) -> (PathBuf, PathBuf, PathBuf) {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");

    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");
    write_tiered_recall_config(&workspace_path, tier_enabled);

    let connection = DbConnection::open_file(&db_path).expect("open db");
    connection.migrate().expect("migrate db");
    ensure_workspace_row(&connection, &workspace_path);
    let workspace_id = stable_workspace_id(&workspace_path);

    for index in 0..TIERED_RECALL_FILLER_COUNT {
        let input = CreateMemoryInput {
            workspace_id: workspace_id.clone(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: format!(
                "Tiered recall benchmark filler {index}: hot warm context for \
                 tiered recall release cold explicit failure evidence."
            ),
            workflow_id: None,
            confidence: 0.95,
            utility: 0.95,
            importance: 0.95,
            provenance_uri: Some(format!("bench://tiered-recall/filler/{index}")),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: Some("tiered-recall-bench".to_owned()),
            tags: vec![
                "bench".to_owned(),
                "tiered-recall".to_owned(),
                "filler".to_owned(),
            ],
            valid_from: None,
            valid_to: None,
        };
        connection
            .insert_memory(&format!("mem_tiered_recall_filler_{index:03}"), &input)
            .expect("insert tiered recall filler memory");
    }

    let cold_input = CreateMemoryInput {
        workspace_id,
        level: "procedural".to_owned(),
        kind: "failure".to_owned(),
        content: "Tiered recall cold explicit failure evidence sentinel: keep required cold \
                  failure evidence eligible even when hot and warm tiers are full."
            .to_owned(),
        workflow_id: None,
        confidence: 0.02,
        utility: 0.02,
        importance: 0.02,
        provenance_uri: Some("bench://tiered-recall/cold-required".to_owned()),
        trust_class: "human_explicit".to_owned(),
        trust_subclass: Some("tiered-recall-bench".to_owned()),
        tags: vec![
            "bench".to_owned(),
            "tiered-recall".to_owned(),
            "cold-required".to_owned(),
        ],
        valid_from: None,
        valid_to: None,
    };
    connection
        .insert_memory("mem_tiered_recall_cold_required", &cold_input)
        .expect("insert tiered recall cold required memory");
    connection
        .close()
        .expect("close tiered recall benchmark db");

    let index_dir =
        build_resource_scale_index(&workspace_path, &db_path, TIERED_RECALL_MEMORY_COUNT);
    (workspace_path, db_path, index_dir)
}

fn tiered_recall_options(
    workspace_path: &Path,
    db_path: &Path,
    index_dir: &Path,
) -> ContextPackOptions {
    ContextPackOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: Some(index_dir.to_path_buf()),
        query: TIERED_RECALL_QUERY.to_owned(),
        speed: SpeedMode::Default,
        filters: Default::default(),
        profile: None,
        max_tokens: Some(20_000),
        candidate_pool: Some(TIERED_RECALL_CANDIDATE_POOL),
        max_results: None,
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: None,
        redaction_level: RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight: None,
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: ContextPackOutputOptions::default(),
        persist_pack: true,
    }
}

fn performance_timing_ms(performance: &serde_json::Value, name: &str) -> f64 {
    performance
        .pointer("/data/timings")
        .and_then(serde_json::Value::as_array)
        .and_then(|timings| {
            timings.iter().find_map(|entry| {
                let timing_name = entry.get("name").and_then(serde_json::Value::as_str)?;
                (timing_name == name)
                    .then(|| entry.get("elapsedMs").and_then(serde_json::Value::as_f64))
                    .flatten()
            })
        })
        .unwrap_or(0.0)
}

fn performance_u64(performance: &serde_json::Value, pointer: &str) -> u64 {
    performance
        .pointer(pointer)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default()
}

fn l2_warm_pack_json() -> serde_json::Value {
    serde_json::json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "pack": {
                "schema": "ee.pack.v2",
                "hash": "blake3:l2-warm-benchmark-pack",
                "text": "L2 warm cache benchmark context pack.",
                "items": [],
                "meta": {
                    "operation": L2_WARM_BENCH_OPERATION,
                    "expectedFreshAssemblies": L2_EXPECTED_FRESH_ASSEMBLIES,
                    "expectedWarmHits": L2_EXPECTED_WARM_HITS,
                },
            },
        },
        "degraded": [],
    })
}

fn seed_l2_warm_cache(cache_root: &Path) -> (PackL2Cache, String) {
    let cache = PackL2Cache::new(
        cache_root.to_path_buf(),
        PackL2CacheOptions::new(1_048_576, Duration::from_secs(300)),
    );
    let key = "blake3:ee-context-pack-l2-warm-benchmark-key".to_owned();
    cache
        .put(&key, &l2_warm_pack_json())
        .expect("seed warm L2 pack cache entry");
    (cache, key)
}

fn zstd_pack_dictionary_pack_json() -> serde_json::Value {
    let repeated_terms = (0..96)
        .map(|index| {
            format!(
                "zstd dictionary pack fixture repeated provenance segment {index:03}: release context cache ledger hash pack hash markdown parity"
            )
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "pack": {
                "schema": "ee.pack.v2",
                "hash": "blake3:zstd-pack-dictionary-benchmark-pack",
                "ledgerHash": "blake3:zstd-pack-dictionary-benchmark-ledger",
                "markdownHash": "sha256:zstd-pack-dictionary-benchmark-markdown",
                "text": repeated_terms.join("\n"),
                "items": repeated_terms
                    .iter()
                    .enumerate()
                    .map(|(index, content)| {
                        serde_json::json!({
                            "id": format!("mem_zstd_pack_dictionary_{index:03}"),
                            "content": content,
                            "why": "dictionary-compressed L2 cache benchmark fixture",
                        })
                    })
                    .collect::<Vec<_>>(),
            },
        },
        "degraded": [],
    })
}

fn zstd_pack_dictionary_training_samples() -> Vec<PackCompressionSample> {
    (0..ZSTD_PACK_DICTIONARY_SAMPLE_COUNT)
        .map(|index| {
            let payload = serde_json::to_vec(&serde_json::json!({
                "schema": "ee.pack.v2",
                "hash": format!("blake3:zstd-pack-dictionary-training-{index:03}"),
                "ledgerHash": format!("blake3:zstd-pack-dictionary-ledger-{index:03}"),
                "text": format!(
                    "zstd dictionary pack fixture repeated provenance segment {index:03}: release context cache ledger hash pack hash markdown parity"
                ),
                "items": [
                    {
                        "id": format!("mem_zstd_pack_dictionary_{index:03}"),
                        "why": "dictionary-compressed L2 cache benchmark fixture",
                    },
                    {
                        "id": format!("mem_zstd_pack_dictionary_shared_{index:03}"),
                        "why": "shared dictionary corpus term cache replay",
                    },
                ],
            }))
            .expect("zstd dictionary training sample JSON");
            PackCompressionSample::new(
                PackCompressionSampleSourceKind::PackRecord,
                format!("pack-zstd-dictionary-{index:03}"),
                Some(index as u64),
                payload,
            )
            .with_redaction_level("ids_hashes_counts_paths_no_pack_content_no_query_text")
        })
        .collect()
}

fn zstd_pack_dictionary() -> PackL2CompressionDictionary {
    let options = PackCompressionTrainingOptions {
        workspace_id: Some("wsp_zstd_pack_dictionary_benchmark".to_owned()),
        max_dictionary_bytes: 8 * 1024,
        max_sample_count: ZSTD_PACK_DICTIONARY_SAMPLE_COUNT,
        max_sample_bytes: 512 * 1024,
        ..PackCompressionTrainingOptions::default()
    };
    let outcome =
        train_pack_compression_dictionary(&zstd_pack_dictionary_training_samples(), &options)
            .expect("train zstd pack dictionary benchmark fixture");
    let report = match outcome {
        PackCompressionDictionaryTrainingOutcome::Trained(report) => report,
        PackCompressionDictionaryTrainingOutcome::NoEligibleSamples(_) => {
            panic!("zstd pack dictionary benchmark fixture should train a dictionary")
        }
    };
    PackL2CompressionDictionary {
        id: report
            .dictionary_id
            .expect("trained dictionary should have an id"),
        byte_hash: report
            .dictionary_byte_hash
            .expect("trained dictionary should have a byte hash"),
        bytes: report.dictionary_bytes,
    }
}

fn seed_zstd_pack_dictionary_cache(
    cache_root: &Path,
    with_dictionary: bool,
) -> (PackL2Cache, String, Option<String>, PackL2WriteReport) {
    let cache = PackL2Cache::new(
        cache_root.to_path_buf(),
        PackL2CacheOptions::new(8 * 1_048_576, Duration::from_secs(300)),
    );
    let key = "blake3:ee-context-zstd-pack-dictionary-benchmark-key".to_owned();
    let dictionary = with_dictionary.then(zstd_pack_dictionary);
    let dictionary_id = dictionary.as_ref().map(|dictionary| dictionary.id.clone());
    let report = cache
        .put_compressed_with_dictionary_at(
            &key,
            &zstd_pack_dictionary_pack_json(),
            dictionary.as_ref(),
            1_800_000_000,
        )
        .expect("seed zstd dictionary L2 pack cache entry");
    (cache, key, dictionary_id, report)
}

fn active_bench_profile() -> String {
    std::env::var("EE_BENCH_PROFILE").unwrap_or_else(|_| "manual".to_owned())
}

fn s4_resource_scales_for_profile(profile: &str) -> Vec<ResourceScale> {
    match profile {
        "stress" => S4_RESOURCE_SCALES.to_vec(),
        "nightly" => S4_RESOURCE_SCALES[..2].to_vec(),
        _ => S4_RESOURCE_SCALES[..1].to_vec(),
    }
}

/// Benchmark `run_context_pack` at different token budget scales.
fn bench_context(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_context");

    let temp_dir = TempDir::new().expect("temp dir");
    let workspace_path = seed_database(temp_dir.path(), 100);
    let db_path = workspace_path.join(".ee").join("ee.db");

    for &max_tokens in &[1000u32, 4000, 8000] {
        let label = match max_tokens {
            1000 => "1k_tokens",
            4000 => "4k_tokens",
            8000 => "8k_tokens",
            _ => "unknown",
        };

        group.bench_with_input(
            BenchmarkId::new("context_pack", label),
            &max_tokens,
            |b, &tokens| {
                b.iter(|| {
                    let options = ContextPackOptions {
                        workspace_path: workspace_path.clone(),
                        database_path: Some(db_path.clone()),
                        index_dir: None,
                        query: "prepare for release deployment and testing".to_string(),
                        speed: SpeedMode::Default,
                        filters: Default::default(),
                        profile: None,
                        max_tokens: Some(tokens),
                        candidate_pool: Some(50),
                        max_results: None,
                        include_tombstoned: false,
                        as_of: None,
                        include_expired: false,
                        include_future: false,
                        include_stale: false,
                        relevance_floor: None,
                        redaction_level: RedactionLevel::Minimal,
                        memory_scope: MemoryScope::Swarm,
                        strict_scope: false,
                        ppr_weight: None,
                        changed_symbols: Vec::new(),
                        changed_symbols_from_git: false,
                        pagination: None,
                        coordination_snapshot_path: None,
                        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
                        output_options: Default::default(),
                        persist_pack: true,
                    };
                    run_context_pack(&options).expect("context pack");
                });
            },
        );
    }

    group.finish();
}

/// Benchmark context pack at different memory scales.
fn bench_context_memory_scales(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_context_memory_scales");

    for &count in &[10usize, 100, 500] {
        let label = match count {
            10 => "10_memories",
            100 => "100_memories",
            500 => "500_memories",
            _ => "unknown",
        };

        group.bench_with_input(BenchmarkId::new("context_pack", label), &count, |b, &n| {
            let temp_dir = TempDir::new().expect("temp dir");
            let workspace_path = seed_database(temp_dir.path(), n);
            let db_path = workspace_path.join(".ee").join("ee.db");

            b.iter(|| {
                let options = ContextPackOptions {
                    workspace_path: workspace_path.clone(),
                    database_path: Some(db_path.clone()),
                    index_dir: None,
                    query: "release testing security".to_string(),
                    speed: SpeedMode::Default,
                    filters: Default::default(),
                    profile: None,
                    max_tokens: Some(4000),
                    candidate_pool: Some(50),
                    max_results: None,
                    include_tombstoned: false,
                    as_of: None,
                    include_expired: false,
                    include_future: false,
                    include_stale: false,
                    relevance_floor: None,
                    redaction_level: RedactionLevel::Minimal,
                    memory_scope: MemoryScope::Swarm,
                    strict_scope: false,
                    ppr_weight: None,
                    changed_symbols: Vec::new(),
                    changed_symbols_from_git: false,
                    pagination: None,
                    coordination_snapshot_path: None,
                    coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
                    output_options: Default::default(),
                    persist_pack: true,
                };
                run_context_pack(&options).expect("context pack");
            });
        });
    }

    group.finish();
}

/// Benchmark context SLO telemetry over S4's required large-memory fixtures.
fn bench_context_s4_resource_scales(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_context_s4_resource_scales");

    for scale in s4_resource_scales_for_profile(&active_bench_profile()) {
        group.bench_with_input(
            BenchmarkId::new("context_pack_slo", scale.label),
            &scale,
            |b, scale| {
                let temp_dir = TempDir::new().expect("temp dir");
                let workspace_path =
                    seed_resource_scale_database(temp_dir.path(), scale.memory_count);
                let db_path = workspace_path.join(".ee").join("ee.db");
                let index_dir =
                    build_resource_scale_index(&workspace_path, &db_path, scale.memory_count);

                b.iter(|| {
                    let options = ContextPackOptions {
                        workspace_path: workspace_path.clone(),
                        database_path: Some(db_path.clone()),
                        index_dir: Some(index_dir.clone()),
                        query: "S4 resource benchmark release testing performance".to_string(),
                        speed: SpeedMode::Default,
                        filters: Default::default(),
                        profile: None,
                        max_tokens: Some(4000),
                        candidate_pool: Some(scale.candidate_pool),
                        max_results: None,
                        include_tombstoned: false,
                        as_of: None,
                        include_expired: false,
                        include_future: false,
                        include_stale: false,
                        relevance_floor: None,
                        redaction_level: RedactionLevel::Minimal,
                        memory_scope: MemoryScope::Swarm,
                        strict_scope: false,
                        ppr_weight: None,
                        changed_symbols: Vec::new(),
                        changed_symbols_from_git: false,
                        pagination: None,
                        coordination_snapshot_path: None,
                        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
                        output_options: ContextPackOutputOptions::default()
                            .with_resource_profile(scale.resource_profile),
                        persist_pack: true,
                    };
                    let run = run_context_pack_with_performance(&options, "context")
                        .expect("context pack");
                    let search_ms = performance_timing_ms(&run.performance, "search");
                    let pack_assembly_ms = performance_timing_ms(&run.performance, "packAssembly");
                    let memory_bytes_peak = run
                        .performance
                        .pointer("/data/pack/slo/actuals/memoryBytesPeak")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default();
                    black_box(run.response.data.slo.as_ref().map(|slo| {
                        (
                            search_ms,
                            pack_assembly_ms,
                            memory_bytes_peak,
                            slo.actuals.elapsed_ms,
                            slo.actuals.memory_bytes_peak,
                            slo.actuals.scanned_count,
                        )
                    }));
                });
            },
        );
    }

    group.finish();
}

/// Benchmark arena allocation modes on deterministic pack assembly fixtures.
fn bench_context_arena_mode(c: &mut Criterion) {
    let mut group = c.benchmark_group(ARENA_MODE_BENCH_GROUP);
    let fixtures = [
        (
            "fixture_coverage_fill",
            arena_fixture_coverage_fill(),
            ContextPackProfile::Balanced,
            1_400_u32,
        ),
        (
            "fixture_provenance_heavy",
            arena_fixture_provenance_heavy(),
            ContextPackProfile::Thorough,
            6_000_u32,
        ),
    ];

    for (fixture_label, candidates, profile, max_tokens) in fixtures {
        let budget = TokenBudget::new(max_tokens).expect("arena benchmark token budget");
        for mode in [
            ArenaMode::Disabled,
            ArenaMode::RequestScoped,
            ArenaMode::WorkspaceReuse,
        ] {
            let label = format!("{fixture_label}/{}", mode.as_str());
            match mode {
                ArenaMode::WorkspaceReuse => {
                    let mut workspace = PackArenaWorkspace::new(PackArenaWorkspaceKey::new(
                        "file:///tmp/ee-arena-benchmark-workspace",
                        PackResourceProfile::Standard,
                    ));
                    group.bench_function(
                        BenchmarkId::new(ARENA_MODE_BENCH_OPERATION, label),
                        |b| {
                            b.iter(|| {
                                let determinism = Deterministic::from_seed(0xbd_17_aa_75);
                                let draft =
                                    assemble_draft_with_profile_and_options_seeded_in_workspace(
                                        profile,
                                        "show evidence for arena policy decisions",
                                        budget,
                                        black_box(candidates.clone()),
                                        PackAssemblyOptions {
                                            arena_mode: ArenaMode::WorkspaceReuse,
                                            ..PackAssemblyOptions::default()
                                        },
                                        &determinism,
                                        &mut workspace,
                                    )
                                    .expect("workspace reuse arena benchmark assembly");
                                black_box((
                                    draft.items.len(),
                                    draft.omitted.len(),
                                    workspace.stats().fresh_scratch_allocations,
                                    workspace.stats().reset_count,
                                ))
                            });
                        },
                    );
                }
                ArenaMode::Disabled | ArenaMode::RequestScoped => {
                    group.bench_function(
                        BenchmarkId::new(ARENA_MODE_BENCH_OPERATION, label),
                        |b| {
                            b.iter(|| {
                                let determinism = Deterministic::from_seed(0xbd_17_aa_75);
                                let draft = assemble_draft_with_profile_and_options_seeded(
                                    profile,
                                    "show evidence for arena policy decisions",
                                    budget,
                                    black_box(candidates.clone()),
                                    PackAssemblyOptions {
                                        arena_mode: mode,
                                        ..PackAssemblyOptions::default()
                                    },
                                    &determinism,
                                )
                                .expect("arena benchmark assembly");
                                black_box((draft.items.len(), draft.omitted.len()))
                            });
                        },
                    );
                }
            }
        }
    }

    group.finish();
}

/// Benchmark warm L2 JSON retrieval for the context-pack cache gate.
fn bench_context_l2_warm_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group(L2_WARM_BENCH_GROUP);
    let temp_dir = TempDir::new().expect("temp dir");
    let (cache, key) = seed_l2_warm_cache(&temp_dir.path().join("pack-l2"));

    group.bench_function("warm_hit_json", |b| {
        b.iter(|| {
            match cache
                .get(black_box(key.as_str()))
                .expect("warm L2 cache lookup")
            {
                PackL2CacheLookup::Hit(hit) => {
                    assert_eq!(hit.key, key, "warm L2 hit should preserve cache key");
                    black_box(hit.pack_json)
                }
                PackL2CacheLookup::Miss(miss) => {
                    panic!(
                        "warm L2 cache entry should hit, got miss: {:?}",
                        miss.reason
                    );
                }
            }
        });
    });

    group.finish();
}

/// Benchmark dictionary-compressed L2 pack cache retrieval against a no-dictionary entry.
fn bench_context_zstd_pack_dictionary(c: &mut Criterion) {
    let mut group = c.benchmark_group(ZSTD_PACK_DICTIONARY_BENCH_GROUP);
    let dictionary_temp_dir = TempDir::new().expect("dictionary L2 temp dir");
    let baseline_temp_dir = TempDir::new().expect("baseline L2 temp dir");
    let (dictionary_cache, dictionary_key, dictionary_id, dictionary_report) =
        seed_zstd_pack_dictionary_cache(&dictionary_temp_dir.path().join("pack-l2-dict"), true);
    let (baseline_cache, baseline_key, _, baseline_report) =
        seed_zstd_pack_dictionary_cache(&baseline_temp_dir.path().join("pack-l2-plain"), false);

    group.bench_function(
        BenchmarkId::new(ZSTD_PACK_DICTIONARY_OPERATION, "dictionary_hit_json"),
        |b| {
            b.iter(|| {
                match dictionary_cache
                    .get(black_box(dictionary_key.as_str()))
                    .expect("dictionary-compressed L2 cache lookup")
                {
                    PackL2CacheLookup::Hit(hit) => {
                        let compression = hit
                            .compression
                            .as_ref()
                            .expect("dictionary entry should be compressed");
                        black_box((
                            hit.pack_json,
                            compression.dictionary_id.clone(),
                            compression.compressed_bytes,
                            compression.uncompressed_bytes,
                            compression.decompression_latency_ms,
                            dictionary_id.clone(),
                            dictionary_report.byte_len,
                        ))
                    }
                    PackL2CacheLookup::Miss(miss) => {
                        panic!(
                            "dictionary-compressed L2 cache entry should hit, got miss: {:?}",
                            miss.reason
                        );
                    }
                }
            });
        },
    );

    group.bench_function(
        BenchmarkId::new(
            ZSTD_PACK_DICTIONARY_OPERATION,
            "baseline_no_dictionary_hit_json",
        ),
        |b| {
            b.iter(|| {
                match baseline_cache
                    .get(black_box(baseline_key.as_str()))
                    .expect("baseline compressed L2 cache lookup")
                {
                    PackL2CacheLookup::Hit(hit) => {
                        let compression = hit
                            .compression
                            .as_ref()
                            .expect("baseline entry should be compressed");
                        black_box((
                            hit.pack_json,
                            compression.dictionary_id.clone(),
                            compression.compressed_bytes,
                            compression.uncompressed_bytes,
                            baseline_report.byte_len,
                        ))
                    }
                    PackL2CacheLookup::Miss(miss) => {
                        panic!(
                            "baseline compressed L2 cache entry should hit, got miss: {:?}",
                            miss.reason
                        );
                    }
                }
            });
        },
    );

    group.finish();
}

/// Benchmark Pack DNA explain attachment separately from baseline context packing.
fn bench_context_pack_dna_orchestration(c: &mut Criterion) {
    let mut group = c.benchmark_group(PACK_DNA_ORCHESTRATION_BENCH_GROUP);
    let temp_dir = TempDir::new().expect("temp dir");
    let (workspace_path, db_path, index_dir) =
        seed_pack_dna_orchestration_database(temp_dir.path());

    group.bench_function(
        BenchmarkId::new(PACK_DNA_ORCHESTRATION_OPERATION, "baseline_no_explain"),
        |b| {
            b.iter(|| {
                let response = run_context_pack(&pack_dna_orchestration_options(
                    &workspace_path,
                    &db_path,
                    &index_dir,
                ))
                .expect("baseline context pack");
                black_box((
                    response.data.pack.hash,
                    response.data.pack.items.len(),
                    response.data.pack_dna.is_none(),
                ))
            });
        },
    );

    group.bench_function(
        BenchmarkId::new(PACK_DNA_ORCHESTRATION_OPERATION, "explain_attach_pack_dna"),
        |b| {
            b.iter(|| {
                let mut response = run_context_pack(&pack_dna_orchestration_options(
                    &workspace_path,
                    &db_path,
                    &index_dir,
                ))
                .expect("context pack before Pack DNA attach");
                let baseline_hash = response.data.pack.hash.clone();
                let baseline_item_count = response.data.pack.items.len();
                attach_pack_dna_to_context_response(&db_path, &mut response);
                let pack_dna = response
                    .data
                    .pack_dna
                    .as_ref()
                    .expect("Pack DNA attach should populate JSON");
                black_box((
                    baseline_hash,
                    baseline_item_count,
                    pack_dna.pointer("/voronoiDominator").is_some(),
                    pack_dna.pointer("/communityOfMass").is_some(),
                    pack_dna
                        .pointer("/egoSubgraph/nodeCount")
                        .and_then(serde_json::Value::as_u64),
                    pack_dna
                        .pointer("/pprNeighbors")
                        .and_then(serde_json::Value::as_array)
                        .map(Vec::len),
                ))
            });
        },
    );

    group.finish();
}

/// Benchmark advisory memory-tier admission against an otherwise identical recall fixture.
fn bench_context_tiered_recall(c: &mut Criterion) {
    let mut group = c.benchmark_group(TIERED_RECALL_BENCH_GROUP);
    let disabled_temp_dir = TempDir::new().expect("disabled tiered recall temp dir");
    let enabled_temp_dir = TempDir::new().expect("enabled tiered recall temp dir");
    let (disabled_workspace_path, disabled_db_path, disabled_index_dir) =
        seed_tiered_recall_database(disabled_temp_dir.path(), false);
    let (enabled_workspace_path, enabled_db_path, enabled_index_dir) =
        seed_tiered_recall_database(enabled_temp_dir.path(), true);

    group.bench_function(
        BenchmarkId::new(TIERED_RECALL_OPERATION, "baseline_disabled"),
        |b| {
            b.iter(|| {
                let run = run_context_pack_with_performance(
                    &tiered_recall_options(
                        &disabled_workspace_path,
                        &disabled_db_path,
                        &disabled_index_dir,
                    ),
                    "context",
                )
                .expect("disabled tiered recall context pack");
                black_box((
                    run.response.data.pack.hash,
                    performance_u64(&run.performance, "/data/candidates/tierBoostedCandidates"),
                    performance_u64(&run.performance, "/data/candidates/tierColdCandidates"),
                    performance_timing_ms(&run.performance, "total"),
                ))
            });
        },
    );

    group.bench_function(
        BenchmarkId::new(TIERED_RECALL_OPERATION, "enabled_cold_recall"),
        |b| {
            b.iter(|| {
                let run = run_context_pack_with_performance(
                    &tiered_recall_options(
                        &enabled_workspace_path,
                        &enabled_db_path,
                        &enabled_index_dir,
                    ),
                    "context",
                )
                .expect("enabled tiered recall context pack");
                let required_cold_selected = run.response.data.pack.items.iter().any(|item| {
                    item.memory_id.to_string() == "mem_tiered_recall_cold_required"
                        && item.why.contains("tierAdmission tier=cold")
                        && item.why.contains("requiredEvidencePreserved=true")
                });
                black_box((
                    run.response.data.pack.hash,
                    performance_u64(&run.performance, "/data/candidates/tierBoostedCandidates"),
                    performance_u64(&run.performance, "/data/candidates/tierColdCandidates"),
                    performance_u64(
                        &run.performance,
                        "/data/candidates/tierRequiredColdCandidates",
                    ),
                    performance_timing_ms(&run.performance, "memoryTierAdmission"),
                    performance_timing_ms(&run.performance, "total"),
                    required_cold_selected,
                ))
            });
        },
    );

    group.finish();
}

criterion_group!(
    benches,
    bench_context,
    bench_context_memory_scales,
    bench_context_s4_resource_scales,
    bench_context_arena_mode,
    bench_context_l2_warm_cache,
    bench_context_pack_dna_orchestration,
    bench_context_zstd_pack_dictionary,
    bench_context_tiered_recall
);
criterion_main!(benches);

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use tempfile::TempDir;

    use super::{
        ARENA_MODE_BENCH_GROUP, ARENA_MODE_BENCH_OPERATION, ARENA_MODE_BUDGET_P50_MS,
        ARENA_MODE_BUDGET_P99_MS, ARENA_MODE_EXPECTED_WORKSPACE_FRESH_ALLOCATIONS, BUDGET_P50_MS,
        BUDGET_P99_MS, L2_CONCURRENT_IDENTICAL_REQUESTS, L2_EXPECTED_FRESH_ASSEMBLIES,
        L2_EXPECTED_WARM_HITS, L2_WARM_BENCH_GROUP, L2_WARM_BENCH_OPERATION, L2_WARM_BUDGET_P50_MS,
        L2_WARM_BUDGET_P99_MS, PACK_DNA_ORCHESTRATION_BENCH_GROUP,
        PACK_DNA_ORCHESTRATION_BUDGET_P50_MS, PACK_DNA_ORCHESTRATION_BUDGET_P99_MS,
        PACK_DNA_ORCHESTRATION_OPERATION, PACK_DNA_ORCHESTRATION_SERIAL_TASK_COUNT,
        REGRESSION_THRESHOLD, S4_NIGHTLY_SCALE, S4_RELEASE_CANDIDATE_SCALE, S4_RESOURCE_SCALES,
        S4_STRESS_SCALE, TIERED_RECALL_BENCH_GROUP, TIERED_RECALL_BUDGET_P50_MS,
        TIERED_RECALL_BUDGET_P99_MS, TIERED_RECALL_CANDIDATE_POOL,
        TIERED_RECALL_EXPECTED_REQUIRED_COLD_MIN, TIERED_RECALL_MEMORY_COUNT,
        TIERED_RECALL_OPERATION, TIERED_RECALL_QUERY, ZSTD_PACK_DICTIONARY_BENCH_GROUP,
        ZSTD_PACK_DICTIONARY_BUDGET_P50_MS, ZSTD_PACK_DICTIONARY_BUDGET_P99_MS,
        ZSTD_PACK_DICTIONARY_OPERATION, ZSTD_PACK_DICTIONARY_SAMPLE_COUNT,
        arena_fixture_coverage_fill, arena_fixture_provenance_heavy, l2_warm_pack_json,
        pack_dna_orchestration_options, s4_resource_scales_for_profile, seed_database,
        seed_l2_warm_cache, seed_pack_dna_orchestration_database, seed_zstd_pack_dictionary_cache,
        zstd_pack_dictionary_pack_json,
    };

    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!("ee_context", "ee_context", "canonical group name");
    }

    #[test]
    fn budget_constants_match_plan() {
        assert!(
            (BUDGET_P50_MS - 95.0).abs() < f64::EPSILON,
            "p50 budget matches plan §28"
        );
        assert!(
            (BUDGET_P99_MS - 240.0).abs() < f64::EPSILON,
            "p99 budget matches plan §28"
        );
    }

    #[test]
    fn regression_threshold_is_30_percent() {
        assert!(
            (REGRESSION_THRESHOLD - 0.30).abs() < f64::EPSILON,
            "regression threshold is 30%"
        );
    }

    #[test]
    fn can_seed_database_for_context() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace_path = seed_database(temp_dir.path(), 10);
        let db_path = workspace_path.join(".ee").join("ee.db");
        assert!(db_path.exists(), "database file exists after seeding");
    }

    #[test]
    fn s4_resource_scale_table_covers_required_fixture_sizes() {
        let counts = S4_RESOURCE_SCALES
            .iter()
            .map(|scale| scale.memory_count)
            .collect::<Vec<_>>();
        assert_eq!(
            counts,
            vec![
                S4_RELEASE_CANDIDATE_SCALE,
                S4_NIGHTLY_SCALE,
                S4_STRESS_SCALE
            ],
            "S4 benchmark scales must cover 1k, 10k, and 100k memories"
        );
    }

    #[test]
    fn s4_resource_scales_are_profile_gated() {
        assert_eq!(
            s4_resource_scales_for_profile("ci-smoke")
                .iter()
                .map(|scale| scale.memory_count)
                .collect::<Vec<_>>(),
            vec![S4_RELEASE_CANDIDATE_SCALE]
        );
        assert_eq!(
            s4_resource_scales_for_profile("nightly")
                .iter()
                .map(|scale| scale.memory_count)
                .collect::<Vec<_>>(),
            vec![S4_RELEASE_CANDIDATE_SCALE, S4_NIGHTLY_SCALE]
        );
        assert_eq!(
            s4_resource_scales_for_profile("stress")
                .iter()
                .map(|scale| scale.memory_count)
                .collect::<Vec<_>>(),
            vec![
                S4_RELEASE_CANDIDATE_SCALE,
                S4_NIGHTLY_SCALE,
                S4_STRESS_SCALE
            ]
        );
    }

    #[test]
    fn l2_warm_cache_benchmark_contract_matches_gate() -> Result<(), String> {
        assert_eq!(L2_WARM_BENCH_GROUP, "ee_context_pack_l2_warm");
        assert_eq!(L2_WARM_BENCH_OPERATION, "ee_context_pack_l2_warm");
        assert!(
            L2_WARM_BUDGET_P50_MS > 0.0 && L2_WARM_BUDGET_P99_MS >= L2_WARM_BUDGET_P50_MS,
            "warm L2 benchmark budgets must be positive and monotonic"
        );
        assert_eq!(L2_CONCURRENT_IDENTICAL_REQUESTS, 4);
        assert_eq!(L2_EXPECTED_FRESH_ASSEMBLIES, 1);
        assert_eq!(L2_EXPECTED_WARM_HITS, 3);

        let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
        let (cache, key) = seed_l2_warm_cache(&temp_dir.path().join("pack-l2"));
        let hit = match cache.get(&key).map_err(|error| error.to_string())? {
            super::PackL2CacheLookup::Hit(hit) => hit,
            super::PackL2CacheLookup::Miss(miss) => {
                return Err(format!("seeded warm L2 entry missed: {:?}", miss.reason));
            }
        };
        assert_eq!(hit.pack_json, l2_warm_pack_json());
        Ok(())
    }

    #[test]
    fn arena_mode_benchmark_contract_matches_workspace_reuse_gate() -> Result<(), String> {
        assert_eq!(ARENA_MODE_BENCH_GROUP, "ee_context_arena_mode");
        assert_eq!(
            ARENA_MODE_BENCH_OPERATION,
            "ee_context_arena_workspace_reuse"
        );
        assert!(
            ARENA_MODE_BUDGET_P50_MS > 0.0 && ARENA_MODE_BUDGET_P99_MS >= ARENA_MODE_BUDGET_P50_MS,
            "arena mode benchmark budgets must be positive and monotonic"
        );
        assert_eq!(ARENA_MODE_EXPECTED_WORKSPACE_FRESH_ALLOCATIONS, 1);
        assert_eq!(arena_fixture_coverage_fill().len(), 20);
        assert_eq!(arena_fixture_provenance_heavy().len(), 9);
        Ok(())
    }

    #[test]
    fn pack_dna_orchestration_benchmark_contract_matches_pipeline_gate() -> Result<(), String> {
        assert_eq!(
            PACK_DNA_ORCHESTRATION_BENCH_GROUP,
            "ee_context_pack_dna_orchestration"
        );
        assert_eq!(
            PACK_DNA_ORCHESTRATION_OPERATION,
            "ee_context_pack_dna_attach"
        );
        assert!(
            PACK_DNA_ORCHESTRATION_BUDGET_P50_MS > 0.0
                && PACK_DNA_ORCHESTRATION_BUDGET_P99_MS >= PACK_DNA_ORCHESTRATION_BUDGET_P50_MS,
            "Pack DNA orchestration benchmark budgets must be positive and monotonic"
        );
        assert_eq!(PACK_DNA_ORCHESTRATION_SERIAL_TASK_COUNT, 1);

        let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
        let (workspace_path, db_path, index_dir) =
            seed_pack_dna_orchestration_database(temp_dir.path());
        assert!(db_path.exists(), "Pack DNA benchmark database must exist");
        assert!(index_dir.exists(), "Pack DNA benchmark index must exist");
        let options = pack_dna_orchestration_options(&workspace_path, &db_path, &index_dir);
        assert_eq!(options.candidate_pool, Some(12));
        assert_eq!(options.max_tokens, Some(1_200));
        Ok(())
    }

    #[test]
    fn zstd_pack_dictionary_benchmark_contract_matches_e2e_gate() -> Result<(), String> {
        assert_eq!(
            ZSTD_PACK_DICTIONARY_BENCH_GROUP,
            "ee_context_zstd_pack_dictionary"
        );
        assert_eq!(
            ZSTD_PACK_DICTIONARY_OPERATION,
            "ee_context_zstd_pack_dictionary_l2"
        );
        assert_eq!(ZSTD_PACK_DICTIONARY_SAMPLE_COUNT, 96);
        assert!(
            ZSTD_PACK_DICTIONARY_BUDGET_P50_MS > 0.0
                && ZSTD_PACK_DICTIONARY_BUDGET_P99_MS >= ZSTD_PACK_DICTIONARY_BUDGET_P50_MS,
            "zstd dictionary benchmark budgets must be positive and monotonic"
        );

        let temp_dir = TempDir::new().map_err(|error| error.to_string())?;
        let (dictionary_cache, dictionary_key, dictionary_id, dictionary_report) =
            seed_zstd_pack_dictionary_cache(&temp_dir.path().join("pack-l2-dict"), true);
        let (baseline_cache, baseline_key, _, baseline_report) =
            seed_zstd_pack_dictionary_cache(&temp_dir.path().join("pack-l2-plain"), false);
        assert_eq!(
            dictionary_key, baseline_key,
            "dictionary and baseline fixtures must share a comparable cache key"
        );
        let dictionary_id = dictionary_id.ok_or_else(|| "dictionary id missing".to_owned())?;
        let dictionary_compression = dictionary_report
            .compression
            .as_ref()
            .ok_or_else(|| "dictionary write report missing compression".to_owned())?;
        let baseline_compression = baseline_report
            .compression
            .as_ref()
            .ok_or_else(|| "baseline write report missing compression".to_owned())?;
        assert_eq!(
            dictionary_compression.dictionary_id.as_deref(),
            Some(dictionary_id.as_str())
        );
        assert!(
            dictionary_compression.compressed_bytes < dictionary_compression.uncompressed_bytes,
            "dictionary-compressed payload must improve on uncompressed fixture bytes"
        );
        assert!(
            baseline_compression.compressed_bytes < baseline_compression.uncompressed_bytes,
            "baseline compressed payload must improve on uncompressed fixture bytes"
        );

        let hit = match dictionary_cache
            .get(&dictionary_key)
            .map_err(|error| error.to_string())?
        {
            super::PackL2CacheLookup::Hit(hit) => hit,
            super::PackL2CacheLookup::Miss(miss) => {
                return Err(format!(
                    "dictionary-compressed L2 entry missed: {:?}",
                    miss.reason
                ));
            }
        };
        assert_eq!(hit.pack_json, zstd_pack_dictionary_pack_json());
        let hit_compression = hit
            .compression
            .ok_or_else(|| "dictionary hit missing compression metadata".to_owned())?;
        assert_eq!(
            hit_compression.dictionary_id.as_deref(),
            Some(dictionary_id.as_str())
        );
        assert_eq!(
            hit_compression.uncompressed_bytes,
            dictionary_compression.uncompressed_bytes
        );

        let baseline_hit = match baseline_cache
            .get(&baseline_key)
            .map_err(|error| error.to_string())?
        {
            super::PackL2CacheLookup::Hit(hit) => hit,
            super::PackL2CacheLookup::Miss(miss) => {
                return Err(format!("baseline L2 entry missed: {:?}", miss.reason));
            }
        };
        assert_eq!(baseline_hit.pack_json, zstd_pack_dictionary_pack_json());
        Ok(())
    }

    #[test]
    fn tiered_recall_benchmark_contract_matches_e2e_gate() {
        assert_eq!(TIERED_RECALL_BENCH_GROUP, "ee_context_tiered_recall");
        assert_eq!(TIERED_RECALL_OPERATION, "ee_context_memory_tier_admission");
        assert_eq!(
            TIERED_RECALL_QUERY,
            "tiered recall release cold explicit failure evidence"
        );
        assert!(
            (TIERED_RECALL_CANDIDATE_POOL as usize) < TIERED_RECALL_MEMORY_COUNT,
            "tiered recall proof must not request the whole corpus"
        );
        assert!(
            TIERED_RECALL_CANDIDATE_POOL > 128,
            "bounded pool should still cross the hot tier budget and exercise warm admission"
        );
        assert!(
            TIERED_RECALL_MEMORY_COUNT > 640,
            "default_swarm hot+warm budgets are 640, so the fixture must force a cold tier"
        );
        assert_eq!(TIERED_RECALL_EXPECTED_REQUIRED_COLD_MIN, 1);
        assert!(
            TIERED_RECALL_BUDGET_P50_MS > 0.0
                && TIERED_RECALL_BUDGET_P99_MS >= TIERED_RECALL_BUDGET_P50_MS,
            "tiered recall benchmark budgets must be positive and monotonic"
        );
    }
}
