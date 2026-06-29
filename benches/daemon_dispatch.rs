//! Criterion benchmark for pure daemon dispatch overhead (bd-26qrk).
//!
//! Group name: `daemon_dispatch`
//!
//! This isolates the method table from UDS framing so future daemon
//! context work can spot dispatch regressions independently from socket
//! and JSON frame costs.

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ee::daemon::protocol::DaemonRequest;
use ee::daemon::server::{METHOD_CAPABILITIES, METHOD_CONTEXT, METHOD_ECHO, dispatch};
use serde_json::json;
use std::hint::black_box;

const BENCH_GROUP_NAME: &str = "daemon_dispatch";
const BENCH_AGENT_ID: &str = "bench-agent";
const BENCH_WORKSPACE_ID: &str = "workspace-bench";

fn request(label: &str, method: &str) -> DaemonRequest {
    let mut request = DaemonRequest::new(
        format!("bench-dispatch-{label}"),
        BENCH_AGENT_ID,
        method,
        json!({
            "task": "measure pure daemon dispatch",
            "label": label,
        }),
    );
    request.workspace_id = Some(BENCH_WORKSPACE_ID.to_owned());
    request
}

fn schema_mismatch_request() -> DaemonRequest {
    let mut request = request("schema_mismatch", METHOD_ECHO);
    request.schema = "ee.daemon.request.v999".to_owned();
    request
}

fn bench_daemon_dispatch(criterion: &mut Criterion) {
    let cases = [
        ("capabilities", request("capabilities", METHOD_CAPABILITIES)),
        (
            "echo_disabled_default",
            request("echo_disabled", METHOD_ECHO),
        ),
        ("context_stub", request("context_stub", METHOD_CONTEXT)),
        (
            "unknown_method",
            request("unknown_method", "ee.daemon.unknown"),
        ),
        ("schema_mismatch", schema_mismatch_request()),
    ];

    let mut group = criterion.benchmark_group(BENCH_GROUP_NAME);
    group.measurement_time(Duration::from_secs(2));
    group.throughput(Throughput::Elements(1));

    for (label, request) in cases {
        group.bench_with_input(
            BenchmarkId::new("dispatch", label),
            &request,
            |bench, request| {
                bench.iter(|| {
                    let response = dispatch(black_box(request));
                    black_box(response);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_daemon_dispatch);
criterion_main!(benches);
