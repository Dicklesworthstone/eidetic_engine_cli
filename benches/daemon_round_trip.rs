//! Criterion benchmark for daemon dispatch and UDS round-trip overhead.
//!
//! Group name: `ee_daemon_round_trip`
//!
//! This is the scaffold baseline for bd-ob21s. It measures the daemon
//! overhead that every future warm-loaded `ee context` call must beat:
//! pure dispatch, and, on Linux where the daemon's safe peer credential
//! gate is currently implemented, request/response framing, UDS connect,
//! and client-side serialization. The real cold-start-vs-daemon context
//! comparison is intentionally deferred until `ee.daemon.context` stops
//! returning the warm-load-not-implemented degradation.

#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use criterion::{Criterion, black_box, criterion_group, criterion_main};
#[cfg(unix)]
use ee::daemon::protocol::DaemonRequest;
#[cfg(unix)]
use ee::daemon::server::{METHOD_CONTEXT, METHOD_ECHO, dispatch};
#[cfg(unix)]
use serde_json::json;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use tempfile::TempDir;

#[cfg(unix)]
const BENCH_GROUP_NAME: &str = "ee_daemon_round_trip";
#[cfg(unix)]
const BENCH_AGENT_ID: &str = "bench-agent";
#[cfg(unix)]
const BENCH_WORKSPACE_ID: &str = "workspace-bench";

#[cfg(target_os = "linux")]
struct RunningDaemon {
    _tempdir: TempDir,
    socket_path: PathBuf,
    handle: ee::daemon::server::DaemonServerHandle,
}

#[cfg(target_os = "linux")]
impl RunningDaemon {
    fn start() -> Self {
        let tempdir = match TempDir::new() {
            Ok(tempdir) => tempdir,
            Err(error) => panic!("daemon benchmark tempdir: {error}"),
        };
        let socket_path = tempdir.path().join("ee-daemon-bench.sock");
        let handle = match ee::daemon::server::start_server(&socket_path) {
            Ok(handle) => handle,
            Err(error) => panic!("start daemon benchmark server: {error}"),
        };
        Self {
            _tempdir: tempdir,
            socket_path,
            handle,
        }
    }

    fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

#[cfg(target_os = "linux")]
impl Drop for RunningDaemon {
    fn drop(&mut self) {
        let _ = self.handle.shutdown();
    }
}

#[cfg(unix)]
fn echo_request() -> DaemonRequest {
    let mut request = DaemonRequest::new(
        "bench-echo-0001",
        BENCH_AGENT_ID,
        METHOD_ECHO,
        json!({
            "message": "daemon round-trip benchmark",
            "workspace": BENCH_WORKSPACE_ID,
        }),
    );
    request.workspace_id = Some(BENCH_WORKSPACE_ID.to_owned());
    request
}

#[cfg(unix)]
fn context_stub_request() -> DaemonRequest {
    let mut request = DaemonRequest::new(
        "bench-context-0001",
        BENCH_AGENT_ID,
        METHOD_CONTEXT,
        json!({
            "task": "measure daemon context stub overhead",
            "maxTokens": 4000,
        }),
    );
    request.workspace_id = Some(BENCH_WORKSPACE_ID.to_owned());
    request
}

#[cfg(unix)]
fn bench_daemon_round_trip(criterion: &mut Criterion) {
    let echo = echo_request();
    let context_stub = context_stub_request();
    let mut group = criterion.benchmark_group(BENCH_GROUP_NAME);
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("dispatch_echo_disabled_default", |bench| {
        bench.iter(|| {
            let response = dispatch(black_box(&echo));
            black_box(response);
        });
    });

    group.bench_function("dispatch_context_stub", |bench| {
        bench.iter(|| {
            let response = dispatch(black_box(&context_stub));
            black_box(response);
        });
    });

    bench_live_socket_round_trip(&mut group, &echo, &context_stub);

    group.finish();
}

#[cfg(target_os = "linux")]
fn bench_live_socket_round_trip(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    echo: &DaemonRequest,
    context_stub: &DaemonRequest,
) {
    use ee::daemon::server::client_round_trip;

    let daemon = RunningDaemon::start();
    for (label, request) in [
        ("echo_disabled_default", echo),
        ("context_stub", context_stub),
    ] {
        group.bench_with_input(
            criterion::BenchmarkId::new("client_round_trip", label),
            request,
            |bench, request| {
                bench.iter(|| {
                    let response = match client_round_trip(
                        black_box(daemon.socket_path()),
                        black_box(request),
                    ) {
                        Ok(response) => response,
                        Err(error) => panic!("daemon benchmark round-trip: {error}"),
                    };
                    black_box(response);
                });
            },
        );
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn bench_live_socket_round_trip(
    _group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    _echo: &DaemonRequest,
    _context_stub: &DaemonRequest,
) {
}

#[cfg(unix)]
criterion_group!(benches, bench_daemon_round_trip);
#[cfg(unix)]
criterion_main!(benches);

#[cfg(not(unix))]
fn main() {}
