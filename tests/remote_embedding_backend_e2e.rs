//! HTTP transport coverage for the OpenAI-compatible remote embedding backend
//! (GH #34), driven against a synthetic loopback server.
//!
//! The stub is a plain `std::net::TcpListener` on a loopback ephemeral port
//! speaking just enough HTTP/1.1 to be a `/v1/embeddings` endpoint. Nothing
//! here reaches an external provider or executes a model; each test owns its
//! own server and port.

// These expects are test setup and outcome assertions, as allowed by the
// repository's test lint policy.
#![allow(clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ee::core::remote_embed::{
    RemoteApiEmbedder, RemoteEmbedError, RemoteEmbedSettings, probe_dimension,
    probe_dimension_blocking_with_timeout,
};
use frankensearch::{Embedder, SearchError};

/// How a stub server should answer one request.
#[derive(Clone)]
enum StubBehavior {
    /// Answer 200 with `dimension`-wide vectors, one per input.
    Embeddings { dimension: usize },
    /// Reverse each response and encode the input's numeric identity in its
    /// vector. Optionally corrupt a later HTTP chunk's first input index.
    IndexedInputs { corrupt_request: Option<usize> },
    /// Answer with a fixed status and body.
    Raw { status: u16, body: String },
    /// Attempt to move the request to another endpoint.
    Redirect { status: u16, location: String },
    /// Send only non-success headers, then wait for the client to disconnect.
    ErrorHeaders { status: u16 },
    /// Read the request, then never answer.
    Hang,
    /// Send response headers and optionally a body prefix, then stop sending.
    HangAfterHeaders { partial_body: bool },
}

/// A single-purpose local `/v1/embeddings` server.
struct StubServer {
    base_url: String,
    /// `Authorization` header values observed, in arrival order.
    seen_auth: Arc<Mutex<Vec<Option<String>>>>,
    /// Request bodies observed, in arrival order.
    seen_bodies: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl StubServer {
    fn start(behavior: StubBehavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let seen_bodies = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread_auth = Arc::clone(&seen_auth);
        let thread_bodies = Arc::clone(&seen_bodies);
        let thread_stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(stream) = stream else { return };
                handle_connection(stream, &behavior, &thread_auth, &thread_bodies);
            }
        });

        Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            seen_auth,
            seen_bodies,
            stop,
        }
    }

    fn settings(
        &self,
        model: &str,
        api_key: Option<&str>,
        dimension: Option<&str>,
    ) -> RemoteEmbedSettings {
        RemoteEmbedSettings::new(Some(&self.base_url), Some(model), api_key, dimension)
            .expect("valid stub settings")
    }

    fn observed_auth(&self) -> Vec<Option<String>> {
        self.seen_auth.lock().expect("auth lock").clone()
    }

    fn observed_bodies(&self) -> Vec<String> {
        self.seen_bodies.lock().expect("body lock").clone()
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock the accept loop so the thread can observe the stop flag.
        let _ = TcpStream::connect(
            self.base_url
                .trim_start_matches("http://")
                .trim_end_matches("/v1"),
        );
    }
}

fn handle_connection(
    mut stream: TcpStream,
    behavior: &StubBehavior,
    seen_auth: &Arc<Mutex<Vec<Option<String>>>>,
    seen_bodies: &Arc<Mutex<Vec<String>>>,
) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut authorization = None;
    let mut content_length = 0usize;

    // Request line, then headers up to the blank line.
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            return;
        }
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().unwrap_or(0);
        }
    }

    let mut body = vec![0_u8; content_length];
    if content_length > 0 && reader.read_exact(&mut body).is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    seen_auth.lock().expect("auth lock").push(authorization);
    let request_number = {
        let mut bodies = seen_bodies.lock().expect("body lock");
        bodies.push(body.clone());
        bodies.len()
    };

    let (status, payload) = match behavior {
        StubBehavior::Redirect { status, location } => {
            let response = format!(
                "HTTP/1.1 {status} Redirect\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).expect("redirect");
            let _ = stream.shutdown(Shutdown::Write);
            return;
        }
        StubBehavior::ErrorHeaders { status } => {
            let response = format!(
                "HTTP/1.1 {status} Error\r\ncontent-length: 1024\r\nconnection: close\r\n\r\n"
            );
            stream
                .write_all(response.as_bytes())
                .expect("error headers");
            stream.flush().expect("flush error headers");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("bounded server teardown");
            let _ = stream.read(&mut [0_u8; 1]);
            return;
        }
        StubBehavior::IndexedInputs { corrupt_request } => {
            let parsed: serde_json::Value = serde_json::from_str(&body).expect("request JSON");
            let inputs = parsed["input"].as_array().expect("input array");
            let mut data: Vec<_> = inputs
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    let value: u16 = input
                        .as_str()
                        .expect("input text")
                        .parse()
                        .expect("ordinal");
                    serde_json::json!({"index": index, "embedding": [value]})
                })
                .collect();
            if *corrupt_request == Some(request_number) {
                data[0]["index"] = serde_json::Value::Null;
            }
            data.reverse();
            (200, serde_json::json!({"data": data}).to_string())
        }
        StubBehavior::Hang => {
            // Hold the connection open without answering. The client's timeout
            // is what must end this exchange.
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
        StubBehavior::HangAfterHeaders { partial_body } => {
            let headers = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 1024\r\nconnection: close\r\n\r\n";
            stream.write_all(headers).expect("write response headers");
            if *partial_body {
                stream.write_all(b"{\"data\":[").expect("write body prefix");
            }
            stream.flush().expect("flush incomplete response");
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
        StubBehavior::Raw { status, body } => (*status, body.clone()),
        StubBehavior::Embeddings { dimension } => {
            let count = count_inputs(&body);
            let entries: Vec<String> = (0..count)
                .map(|index| {
                    let values: Vec<String> = (0..*dimension)
                        .map(|component| format!("{}", (component % 7) as f64 * 0.125))
                        .collect();
                    format!(
                        "{{\"object\":\"embedding\",\"index\":{index},\"embedding\":[{}]}}",
                        values.join(",")
                    )
                })
                .collect();
            (
                200,
                format!("{{\"object\":\"list\",\"data\":[{}]}}", entries.join(",")),
            )
        }
    };

    let response = format!(
        "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

/// Count the entries in the request's `"input": [...]` array.
///
/// A deliberately small parser: the request body is written by ee's own
/// serializer, so it is always a flat array of JSON strings.
fn count_inputs(body: &str) -> usize {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(parsed) => parsed,
        Err(_) => return 1,
    };
    parsed
        .get("input")
        .and_then(serde_json::Value::as_array)
        .map_or(1, Vec::len)
}

/// Run one embedder call on its own runtime, mirroring how the CLI drives it.
fn embed_one(embedder: &RemoteApiEmbedder, text: &str) -> Result<Vec<f32>, String> {
    ee::core::run_cli_with_cx(Duration::from_secs(20), |cx| async move {
        embedder.embed(&cx, text).await
    })
    .map_err(|error| format!("runtime error: {error}"))?
    .map_err(|error| error.to_string())
}

fn embed_many(embedder: &RemoteApiEmbedder, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
    ee::core::run_cli_with_cx(Duration::from_secs(20), |cx| async move {
        embedder.embed_batch(&cx, texts).await
    })
    .map_err(|error| format!("runtime error: {error}"))?
    .map_err(|error| error.to_string())
}

#[test]
fn probe_discovers_the_dimension_from_the_first_response() {
    let server = StubServer::start(StubBehavior::Embeddings { dimension: 384 });
    let settings = server.settings("all-minilm", None, None);

    let dimension = probe_dimension_blocking_with_timeout(&settings, Duration::from_secs(20))
        .expect("probe succeeds");

    assert_eq!(dimension, 384);
    assert_eq!(
        server.observed_auth(),
        vec![None],
        "no api key was configured, so no Authorization header may be sent"
    );
}

#[test]
fn embedding_returns_vectors_of_the_configured_dimension() {
    let server = StubServer::start(StubBehavior::Embeddings { dimension: 384 });
    let settings = server.settings("all-minilm", None, Some("384"));
    let embedder = RemoteApiEmbedder::with_dimension(settings, 384);

    let vector = embed_one(&embedder, "hello").expect("embed succeeds");

    assert_eq!(vector.len(), 384);
    assert_eq!(embedder.dimension(), 384);
    assert_eq!(embedder.id(), "remote-api:all-minilm");
    assert!(embedder.is_semantic());
}

#[test]
fn batching_preserves_input_order_and_count() {
    let server = StubServer::start(StubBehavior::Embeddings { dimension: 8 });
    let settings = server.settings("all-minilm", None, Some("8"));
    let embedder = RemoteApiEmbedder::with_dimension(settings, 8);

    let vectors = embed_many(&embedder, &["one", "two", "three"]).expect("batch succeeds");

    assert_eq!(vectors.len(), 3);
    assert!(vectors.iter().all(|vector| vector.len() == 8));
    let bodies = server.observed_bodies();
    assert_eq!(bodies.len(), 1, "three short inputs fit in one request");
    assert!(bodies[0].contains("\"three\""), "{}", bodies[0]);
    assert!(
        bodies[0].contains("\"encoding_format\":\"float\""),
        "{}",
        bodies[0]
    );
}

#[test]
fn the_api_key_is_sent_as_a_bearer_token() {
    let server = StubServer::start(StubBehavior::Embeddings { dimension: 4 });
    let settings = server.settings("all-minilm", Some("sk-test-key"), Some("4"));
    let embedder = RemoteApiEmbedder::with_dimension(settings, 4);

    embed_one(&embedder, "hello").expect("embed succeeds");

    assert_eq!(
        server.observed_auth(),
        vec![Some("Bearer sk-test-key".to_owned())]
    );
}

#[test]
fn a_dimension_change_under_a_configured_index_is_refused() {
    // The endpoint now serves 512d vectors, but this index was built at 384d.
    let server = StubServer::start(StubBehavior::Embeddings { dimension: 512 });
    let settings = server.settings("all-minilm", None, Some("384"));
    let embedder = RemoteApiEmbedder::with_dimension(settings, 384);

    let error = ee::core::run_cli_with_cx(Duration::from_secs(20), |cx| async move {
        embedder.embed(&cx, "hello").await
    })
    .expect("runtime")
    .expect_err("dimension mismatch must be refused");
    assert!(matches!(
        error,
        SearchError::DimensionMismatch {
            expected: 384,
            found: 512
        }
    ));
}

#[test]
fn a_non_success_status_is_reported_rather_than_parsed() {
    let server = StubServer::start(StubBehavior::Raw {
        status: 404,
        body: "{\"error\":{\"message\":\"model not found\"}}".to_owned(),
    });
    let settings = server.settings("missing-model", None, Some("384"));
    let embedder = RemoteApiEmbedder::with_dimension(settings, 384);

    let error = embed_one(&embedder, "hello").expect_err("404 must fail");

    assert!(error.contains("404"), "{error}");
}

#[test]
fn a_malformed_body_is_reported_rather_than_indexed() {
    let server = StubServer::start(StubBehavior::Raw {
        status: 200,
        body: "{\"data\":[{\"index\":0}]}".to_owned(),
    });
    let settings = server.settings("all-minilm", None, Some("4"));
    let embedder = RemoteApiEmbedder::with_dimension(settings, 4);

    let error = embed_one(&embedder, "hello").expect_err("missing embedding must fail");

    assert!(error.contains("malformed"), "{error}");
}

#[test]
fn a_stalled_endpoint_times_out_instead_of_hanging_forever() {
    let server = StubServer::start(StubBehavior::Hang);
    let settings = server.settings("all-minilm", None, Some("384"));
    let embedder = RemoteApiEmbedder::with_dimension(settings, 384)
        .with_request_timeout(Duration::from_millis(300));

    let started = std::time::Instant::now();
    let error = embed_one(&embedder, "hello").expect_err("a stalled endpoint must time out");
    let elapsed = started.elapsed();

    assert!(
        error.contains("unreachable"),
        "timeout must surface as unreachable: {error}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "timeout took {elapsed:?}, which means the bound did not apply"
    );
}

#[test]
fn a_stalled_response_body_obeys_the_request_deadline() {
    for partial_body in [false, true] {
        let server = StubServer::start(StubBehavior::HangAfterHeaders { partial_body });
        let settings = server.settings("all-minilm", None, Some("384"));
        let embedder = RemoteApiEmbedder::with_dimension(settings, 384)
            .with_request_timeout(Duration::from_millis(300));

        let started = std::time::Instant::now();
        let error =
            embed_one(&embedder, "hello").expect_err("an incomplete response must time out");
        let elapsed = started.elapsed();

        assert_eq!(server.observed_bodies().len(), 1);
        assert!(
            error.contains("unreachable") && error.contains("no complete response"),
            "partial_body={partial_body}: expected the exchange deadline, got {error}"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "partial_body={partial_body}: body read exceeded the request deadline: {elapsed:?}"
        );
    }
}

#[test]
fn an_unbound_port_is_reported_as_unreachable() {
    // Bind and immediately drop, so the port is almost certainly free.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    let settings = RemoteEmbedSettings::new(
        Some(&format!("http://127.0.0.1:{port}/v1")),
        Some("all-minilm"),
        None,
        None,
    )
    .expect("valid settings");

    let error = probe_dimension_blocking_with_timeout(&settings, Duration::from_secs(5))
        .expect_err("nothing is listening");

    assert_eq!(error.code(), "remote_embed_unreachable", "{error}");
}

#[test]
fn large_batches_restore_input_identity_across_reversed_http_chunks() {
    let server = StubServer::start(StubBehavior::IndexedInputs {
        corrupt_request: None,
    });
    let settings = server.settings("ordinal-model", None, Some("1"));
    let embedder = RemoteApiEmbedder::with_dimension(settings, 1);
    let inputs: Vec<_> = (0..513_u16).map(|ordinal| ordinal.to_string()).collect();
    let texts: Vec<_> = inputs.iter().map(String::as_str).collect();
    let expected: Vec<_> = (0..513_u16)
        .map(|ordinal| vec![f32::from(ordinal)])
        .collect();

    assert_eq!(
        embed_many(&embedder, &texts).expect("three chunks"),
        expected
    );
    let sizes: Vec<_> = server
        .observed_bodies()
        .iter()
        .map(|body| count_inputs(body))
        .collect();
    assert_eq!(sizes, [256, 256, 1]);
}

#[test]
fn a_malformed_later_chunk_aborts_without_partial_vectors_or_further_requests() {
    let server = StubServer::start(StubBehavior::IndexedInputs {
        corrupt_request: Some(2),
    });
    let settings = server.settings("ordinal-model", None, Some("1"));
    let embedder = RemoteApiEmbedder::with_dimension(settings, 1);
    let inputs: Vec<_> = (0..513_u16).map(|ordinal| ordinal.to_string()).collect();
    let texts: Vec<_> = inputs.iter().map(String::as_str).collect();

    let error = ee::core::run_cli_with_cx(Duration::from_secs(20), |cx| async move {
        embedder.embed_batch(&cx, &texts).await
    })
    .expect("runtime")
    .expect_err("partial success must not become a batch result");
    assert!(matches!(error, SearchError::InvalidConfig { .. }));
    assert_eq!(server.observed_bodies().len(), 2);
}

#[test]
fn malformed_response_indexes_are_rejected_by_the_live_embedder() {
    for index in [
        serde_json::json!(-1),
        serde_json::json!(null),
        serde_json::json!(0.5),
        serde_json::json!("private-index"),
    ] {
        let server = StubServer::start(StubBehavior::Raw {
            status: 200,
            body: serde_json::json!({
                "data": [{"index": index, "embedding": [1.0]}]
            })
            .to_string(),
        });
        let embedder =
            RemoteApiEmbedder::with_dimension(server.settings("model", None, Some("1")), 1);
        let error = ee::core::run_cli_with_cx(Duration::from_secs(20), |cx| async move {
            embedder.embed(&cx, "private-source-text").await
        })
        .expect("runtime")
        .expect_err("invalid response mapping");
        assert!(!error.to_string().contains("private-index"));
        assert!(!error.to_string().contains("private-source-text"));
        assert!(matches!(error, SearchError::InvalidConfig { .. }));
        assert_eq!(server.observed_bodies().len(), 1);
    }
}

#[test]
fn redirects_do_not_resubmit_source_content_or_credentials() {
    for status in [301, 302, 303, 307, 308] {
        let destination = StubServer::start(StubBehavior::Embeddings { dimension: 4 });
        let server = StubServer::start(StubBehavior::Redirect {
            status,
            location: format!("{}/embeddings", destination.base_url),
        });
        let settings = server.settings("model", Some("private-api-token"), Some("4"));
        let embedder = RemoteApiEmbedder::with_dimension(settings, 4);
        let error = embed_one(&embedder, "private-workspace-content")
            .expect_err("redirect must not authorize a new endpoint");
        assert!(error.contains(&status.to_string()), "{error}");
        assert!(!error.contains("private-api-token"));
        assert!(!error.contains("private-workspace-content"));
        assert_eq!(server.observed_bodies().len(), 1);
        assert!(destination.observed_bodies().is_empty());
        assert!(destination.observed_auth().is_empty());
    }
}

#[test]
fn error_status_is_reported_without_waiting_for_its_body() {
    for status in [401, 503] {
        let server = StubServer::start(StubBehavior::ErrorHeaders { status });
        let settings = server.settings("model", None, Some("4"));
        let embedder = RemoteApiEmbedder::with_dimension(settings, 4)
            .with_request_timeout(Duration::from_millis(300));
        let error =
            embed_one(&embedder, "private-workspace-content").expect_err("non-success response");
        assert!(error.contains(&status.to_string()), "{error}");
        assert!(!error.contains("no complete response"), "{error}");
        assert_eq!(server.observed_bodies().len(), 1);
    }
}

#[test]
fn cancelled_requests_send_nothing_and_keep_the_cancellation_error_class() {
    let server = StubServer::start(StubBehavior::Embeddings { dimension: 4 });
    let settings = server.settings("model", Some("private-api-token"), Some("4"));
    let embedder = RemoteApiEmbedder::with_dimension(settings.clone(), 4);
    ee::core::run_cli_with_cx(Duration::from_secs(20), |_runtime_cx| async move {
        let cx = asupersync::Cx::for_testing();
        cx.set_cancel_reason(asupersync::CancelReason::user("private-cancel-reason"));
        let single = embedder.embed(&cx, "private-source-text").await;
        let batch = embedder.embed_batch(&cx, &["one", "two"]).await;
        for error in [
            single.expect_err("cancelled single"),
            batch.expect_err("cancelled batch"),
        ] {
            assert!(!error.to_string().contains("private-cancel-reason"));
            assert!(matches!(error, SearchError::Cancelled { .. }));
        }
        assert!(matches!(
            probe_dimension(&cx, &settings, Duration::from_secs(1)).await,
            Err(RemoteEmbedError::Cancelled)
        ));
        assert!(matches!(
            RemoteApiEmbedder::resolve(&cx, settings).await,
            Err(RemoteEmbedError::Cancelled)
        ));
    })
    .expect("runtime");
    assert!(server.observed_bodies().is_empty());
    assert!(server.observed_auth().is_empty());
}

#[test]
fn empty_batches_do_not_contact_the_endpoint() {
    let server = StubServer::start(StubBehavior::Embeddings { dimension: 4 });
    let settings = server.settings("model", None, Some("4"));
    let embedder = RemoteApiEmbedder::with_dimension(settings, 4);
    assert!(embed_many(&embedder, &[]).expect("empty batch").is_empty());
    assert!(server.observed_bodies().is_empty());
}
