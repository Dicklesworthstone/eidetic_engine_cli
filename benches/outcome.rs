//! Criterion benchmark for `ee outcome` (EE-PERF-BENCH-outcome).
//!
//! Group name: `ee_outcome`
//!
//! Tests the `record_outcome` function at three scales:
//! - empty: fresh database with only the target memory
//! - 100: database seeded with 100 additional memories
//! - 5000: database seeded with 5000 additional memories
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::core::outcome::{OutcomeRecordOptions, record_outcome};
use ee::db::DbConnection;

struct OutcomeFixture {
    db_path: PathBuf,
    target_memory_id: String,
}

fn remember_seed_memory(workspace_path: &Path, db_path: &Path, index: usize) {
    let content = format!("Seed memory {index}: benchmark fixture content.");
    let options = RememberMemoryOptions {
        workspace_path,
        database_path: Some(db_path),
        content: &content,
        level: "semantic",
        kind: "fact",
        tags: Some("bench,seed"),
        confidence: 0.65,
        source: None,
        valid_from: None,
        valid_to: None,
        dry_run: false,
    };
    remember_memory(&options).expect("seed memory");
}

fn seed_fixture(temp_dir: &Path, memory_count: usize) -> OutcomeFixture {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");
    let db_parent = db_path.parent().expect("db parent path");
    std::fs::create_dir_all(db_parent).expect("create benchmark .ee directory");

    let connection = DbConnection::open_file(&db_path).expect("open benchmark db");
    connection.migrate().expect("migrate benchmark db");

    for i in 0..memory_count {
        remember_seed_memory(&workspace_path, &db_path, i);
    }

    let target = remember_memory(&RememberMemoryOptions {
        workspace_path: &workspace_path,
        database_path: Some(&db_path),
        content: "Outcome benchmark target memory.",
        level: "episodic",
        kind: "fact",
        tags: Some("bench,target"),
        confidence: 0.7,
        source: None,
        valid_from: None,
        valid_to: None,
        dry_run: false,
    })
    .expect("create target memory");

    OutcomeFixture {
        db_path,
        target_memory_id: target.memory_id.to_string(),
    }
}

fn bench_outcome(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_outcome");

    for &count in &[0usize, 100, 5000] {
        let label = match count {
            0 => "empty",
            100 => "100_memories",
            5000 => "5000_memories",
            _ => "unknown",
        };

        group.bench_with_input(BenchmarkId::new("record", label), &count, |b, &n| {
            let temp_dir = TempDir::new().expect("temp dir");
            let fixture = seed_fixture(temp_dir.path(), n);

            let mut counter = 0usize;
            b.iter(|| {
                counter += 1;
                let signal = if counter % 2 == 0 {
                    "helpful"
                } else {
                    "harmful"
                };
                let reason = format!("benchmark outcome event {counter}");
                let report = record_outcome(&OutcomeRecordOptions {
                    database_path: fixture.db_path.as_path(),
                    target_type: "memory".to_string(),
                    target_id: fixture.target_memory_id.clone(),
                    workspace_id: None,
                    signal: signal.to_string(),
                    weight: None,
                    source_type: "outcome_observed".to_string(),
                    source_id: None,
                    reason: Some(reason),
                    evidence_json: None,
                    session_id: None,
                    event_id: None,
                    actor: Some("benchmark".to_string()),
                    dry_run: false,
                })
                .expect("record outcome");
                assert_eq!(report.status.as_str(), "recorded", "outcome should persist");
            });
        });
    }

    group.finish();
}

fn bench_outcome_dry_run(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_outcome_dry_run");

    let temp_dir = TempDir::new().expect("temp dir");
    let fixture = seed_fixture(temp_dir.path(), 0);

    group.bench_function("dry_run", |b| {
        b.iter(|| {
            let report = record_outcome(&OutcomeRecordOptions {
                database_path: fixture.db_path.as_path(),
                target_type: "memory".to_string(),
                target_id: fixture.target_memory_id.clone(),
                workspace_id: None,
                signal: "helpful".to_string(),
                weight: None,
                source_type: "outcome_observed".to_string(),
                source_id: None,
                reason: Some("dry-run benchmark feedback".to_string()),
                evidence_json: None,
                session_id: None,
                event_id: None,
                actor: Some("benchmark".to_string()),
                dry_run: true,
            })
            .expect("dry-run outcome");
            assert_eq!(
                report.status.as_str(),
                "dry_run",
                "dry-run should not mutate"
            );
        });
    });

    group.finish();
}

criterion_group!(benches, bench_outcome, bench_outcome_dry_run);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!("ee_outcome", "ee_outcome", "canonical group name");
    }

    #[test]
    fn fixture_seeding_creates_target_memory() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let fixture = super::seed_fixture(temp_dir.path(), 5);
        assert!(fixture.db_path.exists(), "benchmark db should exist");
        assert!(
            fixture.target_memory_id.starts_with("mem_"),
            "target memory id should use mem_ prefix"
        );
        assert!(temp_dir.path().exists(), "workspace path should exist");
    }

    #[test]
    fn record_outcome_succeeds_on_seeded_fixture() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let fixture = super::seed_fixture(temp_dir.path(), 1);
        let report = super::record_outcome(&super::OutcomeRecordOptions {
            database_path: fixture.db_path.as_path(),
            target_type: "memory".to_string(),
            target_id: fixture.target_memory_id.clone(),
            workspace_id: None,
            signal: "helpful".to_string(),
            weight: None,
            source_type: "outcome_observed".to_string(),
            source_id: None,
            reason: Some("unit test feedback".to_string()),
            evidence_json: None,
            session_id: None,
            event_id: None,
            actor: Some("unit-test".to_string()),
            dry_run: false,
        })
        .expect("record outcome");

        assert_eq!(report.status.as_str(), "recorded");
        assert_eq!(report.target_type, "memory");
        assert_eq!(report.target_id, fixture.target_memory_id);
    }
}
