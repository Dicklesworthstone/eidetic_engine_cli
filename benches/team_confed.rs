//! Criterion profile for team-confed join, pair-key, and admission caps (T6.5).
//!
//! Group name: `ee_team_confed`

#![allow(clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tempfile::TempDir;

use ee::db::{CreateWorkspaceInput, DbConnection};
use ee::mesh::admission::{
    MeshAdmissionRequest, MeshAdmissionRequestKind, MeshPeerAdmissionState, decide_admission,
};
use ee::mesh::team::{
    create_local_team, derive_team_pair_key, enroll_team_pair_peer, team_confed_budget_profile,
};

fn bench_team_confed(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_team_confed");

    group.bench_function("derive_pair_key", |bench| {
        bench.iter(|| {
            black_box(derive_team_pair_key(
                "secret",
                "team_analysts",
                "invite_a",
                "node_joiner0000000000000000000001",
                "node_origin0000000000000000000001",
                "nonce_j",
                "nonce_i",
            ))
        });
    });

    group.bench_function("admission_event_batch_at_cap", |bench| {
        let limits = ee::mesh::admission::MeshAdmissionLimits::conservative_default();
        let state = MeshPeerAdmissionState::new("peer_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let request = MeshAdmissionRequest::new(
            "peer_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            MeshAdmissionRequestKind::EventBatch,
            0,
        )
        .with_event_count(limits.max_event_batch_count);
        bench.iter(|| black_box(decide_admission(limits, &state, &request)));
    });

    group.bench_function("admission_body_fetch_at_cap", |bench| {
        let limits = ee::mesh::admission::MeshAdmissionLimits::conservative_default();
        let state = MeshPeerAdmissionState::new("peer_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let request = MeshAdmissionRequest::new(
            "peer_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            MeshAdmissionRequestKind::BodyFetch,
            0,
        )
        .with_body_fetch_bytes(limits.max_body_fetch_bytes);
        bench.iter(|| black_box(decide_admission(limits, &state, &request)));
    });

    group.bench_function("create_and_enroll", |bench| {
        bench.iter(|| {
            let dir = TempDir::new().expect("temp");
            let database = dir.path().join("ee.db");
            let connection = DbConnection::open_file(&database).expect("open");
            connection.migrate().expect("migrate");
            connection
                .insert_workspace(
                    "wsp_persistfixture000000000001",
                    &CreateWorkspaceInput {
                        path: dir.path().display().to_string(),
                        name: Some("bench".to_owned()),
                    },
                )
                .expect("workspace");
            let created = create_local_team(
                &connection,
                "wsp_persistfixture000000000001",
                "Analysts",
                "2026-08-15T00:00:00Z",
            )
            .expect("create");
            let handle = enroll_team_pair_peer(
                &connection,
                "wsp_persistfixture000000000001",
                &created.team.team_id,
                "node_joiner0000000000000000000001",
                "Priya",
                "127.0.0.1",
                created.team.hello_port,
                "2026-08-15T00:01:00Z",
                "wsp_joinworkspace0000000000001",
            )
            .expect("enroll");
            black_box((handle, team_confed_budget_profile()));
        });
    });

    group.finish();
}

criterion_group!(benches, bench_team_confed);
criterion_main!(benches);
