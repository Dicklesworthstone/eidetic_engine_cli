//! Criterion benchmark for per-agent context profile reads (bd-1prrl.2.5).
//!
//! Group name: `agent_profile`

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ee::db::{
    CreateMemoryInput, CreateWorkspaceInput, DbConnection, UpsertAgentContextProfileInput,
};
use ee::models::{AgentContextProfileCounts, WorkspaceId};

const BENCH_GROUP_NAME: &str = "agent_profile";
const FIXTURE_MEMORY_COUNT: usize = 1_000;
const QUICK_MEASURE_ITERS: usize = 31;
const PROFILE_READ_P50_BUDGET_MS: f64 = 2.0;
const AGENT_NAME: &str = "AgentProfileBench";

fn retained_workspace(prefix: &str) -> PathBuf {
    let mut root = std::env::var("EE_BENCH_TMPDIR")
        .or_else(|_| std::env::var("TMPDIR"))
        .unwrap_or_else(|_| "/tmp".to_string());
    if root.starts_with("/Volumes/") {
        root = "/tmp".to_string();
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos();
    let path = PathBuf::from(format!(
        "{}/{}-{}-{nanos}",
        root.trim_end_matches('/'),
        prefix,
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create retained benchmark workspace");
    path
}

fn stable_workspace_id(workspace: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", workspace.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

struct AgentProfileFixture {
    db: DbConnection,
    workspace_id: String,
}

fn seed_fixture(memory_count: usize) -> AgentProfileFixture {
    let workspace = retained_workspace("ee-agent-profile-bench");
    let db_path = workspace.join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee");
    let db = DbConnection::open_file(&db_path).expect("open bench db");
    db.migrate().expect("migrate bench db");
    let workspace_id = stable_workspace_id(&workspace);
    db.insert_workspace(
        &workspace_id,
        &CreateWorkspaceInput {
            path: workspace.to_string_lossy().into_owned(),
            name: Some("agent-profile-bench".to_string()),
        },
    )
    .expect("insert workspace");

    for index in 0..memory_count {
        let memory_id = format!("mem_agent_profile_bench_{index:06}");
        db.insert_memory(
            &memory_id,
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "procedural".to_string(),
                kind: "rule".to_string(),
                content: format!("agent profile benchmark memory {index}"),
                workflow_id: None,
                confidence: 0.75,
                utility: 0.75,
                importance: 0.75,
                provenance_uri: None,
                trust_class: "agent_validated".to_string(),
                trust_subclass: Some("agent-profile-bench".to_string()),
                tags: vec!["bench".to_string(), "agent-profile".to_string()],
                valid_from: None,
                valid_to: None,
            },
        )
        .expect("insert memory");
        let counts = if index % 2 == 0 {
            AgentContextProfileCounts::new(10, 0, 0)
        } else {
            AgentContextProfileCounts::new(0, 10, 0)
        };
        db.upsert_agent_context_profile_event(&UpsertAgentContextProfileInput {
            workspace_id: workspace_id.clone(),
            agent_name: AGENT_NAME.to_string(),
            memory_id,
            counts_delta: counts,
            last_seen_at: Some("2026-05-16T00:00:00Z".to_string()),
            weight_cached: counts.bias().weight,
        })
        .expect("upsert agent profile");
    }

    AgentProfileFixture { db, workspace_id }
}

fn profile_read_once(fixture: &AgentProfileFixture) -> f64 {
    let start = Instant::now();
    let rows = fixture
        .db
        .list_agent_context_profiles_for_pack(&fixture.workspace_id, AGENT_NAME)
        .expect("list agent profiles");
    black_box(rows);
    start.elapsed().as_secs_f64() * 1000.0
}

fn percentile(sorted_samples: &[f64], percentile: f64) -> f64 {
    let last_index = sorted_samples.len() - 1;
    sorted_samples[(percentile * last_index as f64).round() as usize]
}

fn quick_p50_ms() -> f64 {
    let fixture = seed_fixture(FIXTURE_MEMORY_COUNT);
    let mut samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for _ in 0..QUICK_MEASURE_ITERS {
        samples.push(profile_read_once(&fixture));
    }
    samples.sort_by(|left, right| left.total_cmp(right));
    percentile(&samples, 0.50)
}

fn compare_only_mode_enabled() -> bool {
    std::env::var("EE_BENCH_COMPARE_ONLY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn bench_agent_profile(c: &mut Criterion) {
    if compare_only_mode_enabled() {
        let p50_ms = quick_p50_ms();
        assert!(
            p50_ms <= PROFILE_READ_P50_BUDGET_MS,
            "agent profile read p50 above budget: {:.3}ms > {:.3}ms",
            p50_ms,
            PROFILE_READ_P50_BUDGET_MS
        );
        return;
    }

    let fixture = seed_fixture(FIXTURE_MEMORY_COUNT);
    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    group.sample_size(10);
    group.bench_function("list_1000_profiles", |b| {
        b.iter(|| black_box(profile_read_once(&fixture)));
    });
    group.finish();
}

criterion_group!(benches, bench_agent_profile);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(super::BENCH_GROUP_NAME, "agent_profile");
    }

    #[test]
    fn profile_read_budget_matches_acceptance() {
        assert!((super::PROFILE_READ_P50_BUDGET_MS - 2.0).abs() < f64::EPSILON);
    }
}
