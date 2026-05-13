//! Criterion benchmark for `ee init` clean workspace initialization (J9).
//!
//! Group name: `ee_workspace_init`

#![allow(clippy::expect_used)]

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::core::init::{InitOptions, InitStatus, init_workspace};

const BUDGET_P50_MS: f64 = 100.0;
const BUDGET_P99_MS: f64 = 250.0;
const REGRESSION_THRESHOLD_P50_PCT: f64 = 30.0;
const REGRESSION_THRESHOLD_P99_PCT: f64 = 50.0;

fn bench_workspace_init(c: &mut Criterion) {
    black_box((
        BUDGET_P50_MS,
        BUDGET_P99_MS,
        REGRESSION_THRESHOLD_P50_PCT,
        REGRESSION_THRESHOLD_P99_PCT,
    ));
    let mut group = c.benchmark_group("ee_workspace_init");

    group.bench_function("clean_workspace", |bench| {
        let root = TempDir::new().expect("temp dir");
        let mut iteration = 0_u64;
        bench.iter(|| {
            iteration = iteration.saturating_add(1);
            let workspace_path = root.path().join(format!("workspace_{iteration:06}"));
            std::fs::create_dir_all(&workspace_path).expect("create benchmark workspace");
            let report = init_workspace(&InitOptions {
                workspace_path,
                dry_run: false,
                repair_plan: false,
                force: false,
                allow_symlink: false,
                skip_boilerplate: true,
            });
            assert_eq!(
                report.status,
                InitStatus::Created,
                "clean workspace init should create ee artifacts"
            );
            black_box(report.actions.len());
        });
    });

    group.finish();
}

criterion_group!(benches, bench_workspace_init);
criterion_main!(benches);
