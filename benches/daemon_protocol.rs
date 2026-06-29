//! Criterion benchmark for daemon wire framing overhead (bd-26qrk).
//!
//! Group name: `daemon_protocol`
//!
//! The protocol path runs twice for every daemon RPC: one inbound
//! request frame and one outbound response frame. Keep serialized
//! fixtures outside measured closures so the benchmark tracks frame
//! parsing/encoding work, not JSON fixture construction.

use std::io::Cursor;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ee::daemon::protocol::{DaemonRequest, DaemonResponse, read_request, write_response};
use ee::daemon::server::METHOD_ECHO;
use serde_json::json;
use std::hint::black_box;

const BENCH_GROUP_NAME: &str = "daemon_protocol";
const BENCH_AGENT_ID: &str = "bench-agent";
const BENCH_WORKSPACE_ID: &str = "workspace-bench";
const NEAR_FRAME_CAP_PAYLOAD_BYTES: usize = 3_900_000;

fn request_with_payload(label: &str, payload_bytes: usize) -> DaemonRequest {
    let mut request = DaemonRequest::new(
        format!("bench-read-{label}"),
        BENCH_AGENT_ID,
        METHOD_ECHO,
        json!({
            "label": label,
            "payload": "x".repeat(payload_bytes),
        }),
    );
    request.workspace_id = Some(BENCH_WORKSPACE_ID.to_owned());
    request
}

fn response_with_payload(label: &str, payload_bytes: usize) -> DaemonResponse {
    DaemonResponse::ok(
        format!("bench-write-{label}"),
        BENCH_AGENT_ID,
        Some(BENCH_WORKSPACE_ID.to_owned()),
        json!({
            "label": label,
            "payload": "x".repeat(payload_bytes),
        }),
    )
}

fn framed_request(request: &DaemonRequest) -> Vec<u8> {
    let body = serialize_request(request);
    let length = usize_to_u32(body.len(), "benchmark request body must fit u32");
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body);
    frame
}

fn serialize_request(request: &DaemonRequest) -> Vec<u8> {
    match serde_json::to_vec(request) {
        Ok(body) => body,
        Err(error) => panic!("benchmark request must serialize: {error}"),
    }
}

fn serialize_response(response: &DaemonResponse) -> Vec<u8> {
    match serde_json::to_vec(response) {
        Ok(body) => body,
        Err(error) => panic!("benchmark response must serialize: {error}"),
    }
}

fn usize_to_u32(value: usize, context: &str) -> u32 {
    match u32::try_from(value) {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error}"),
    }
}

fn usize_to_u64(value: usize, context: &str) -> u64 {
    match u64::try_from(value) {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error}"),
    }
}

fn bench_daemon_protocol(criterion: &mut Criterion) {
    let request_cases = [
        ("64b", framed_request(&request_with_payload("64b", 64))),
        ("1kib", framed_request(&request_with_payload("1kib", 1024))),
        (
            "64kib",
            framed_request(&request_with_payload("64kib", 64 * 1024)),
        ),
        (
            "1mib",
            framed_request(&request_with_payload("1mib", 1024 * 1024)),
        ),
        (
            "near_cap",
            framed_request(&request_with_payload(
                "near_cap",
                NEAR_FRAME_CAP_PAYLOAD_BYTES,
            )),
        ),
    ];
    let response_cases = [
        ("64b", response_with_payload("64b", 64)),
        ("1kib", response_with_payload("1kib", 1024)),
        ("64kib", response_with_payload("64kib", 64 * 1024)),
        ("1mib", response_with_payload("1mib", 1024 * 1024)),
        (
            "near_cap",
            response_with_payload("near_cap", NEAR_FRAME_CAP_PAYLOAD_BYTES),
        ),
    ];

    let mut group = criterion.benchmark_group(BENCH_GROUP_NAME);
    group.measurement_time(Duration::from_secs(2));

    for (label, frame) in request_cases {
        let body_bytes = usize_to_u64(
            frame.len().saturating_sub(4),
            "benchmark request frame length must fit u64",
        );
        group.throughput(Throughput::Bytes(body_bytes));
        group.bench_with_input(
            BenchmarkId::new("read_request", label),
            &frame,
            |bench, frame| {
                bench.iter(|| {
                    let mut cursor = Cursor::new(black_box(frame.as_slice()));
                    let request = match read_request(&mut cursor) {
                        Ok(request) => request,
                        Err(error) => panic!("benchmark frame must parse: {error}"),
                    };
                    black_box(request);
                });
            },
        );
    }

    for (label, response) in response_cases {
        let body_bytes = usize_to_u64(
            serialize_response(&response).len(),
            "benchmark response length must fit u64",
        );
        group.throughput(Throughput::Bytes(body_bytes));
        group.bench_with_input(
            BenchmarkId::new("write_response", label),
            &response,
            |bench, response| {
                bench.iter_batched(
                    Vec::new,
                    |mut writer| {
                        if let Err(error) = write_response(&mut writer, black_box(response)) {
                            panic!("benchmark response must write: {error}");
                        }
                        black_box(writer);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_daemon_protocol);
criterion_main!(benches);
