//! End-to-end coverage for the OpenAI-compatible remote embedding backend
//! (GH #34), driven against a real local HTTP server rather than a mock.
//!
//! The stub is a plain `std::net::TcpListener` on a loopback ephemeral port
//! speaking just enough HTTP/1.1 to be a `/v1/embeddings` endpoint. Nothing
//! here reaches the network, and each test owns its own server and port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ee::core::remote_embed::{
    RemoteApiEmbedder, RemoteEmbedSettings, probe_dimension_blocking_with_timeout,
};
use frankensearch::Embedder;

/// How a stub server should answer one request.
#[derive(Clone)]
enum StubBehavior {
    /// Answer 200 with `dimension`-wide vectors, one per input.
    Embeddings { dimension: usize },
    /// Answer with a fixed status and body.
    Raw { status: u16, body: String },
    /// Read the request, then never answer.
    Hang,
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

    fn settings(&self, model: &str, api_key: Option<&str>, dimension: Option<&str>) -> RemoteEmbedSettings {
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
    let input_count = body.matches("\"").count(); // placeholder, refined below
    let _ = input_count;

    seen_auth.lock().expect("auth lock").push(authorization);
    seen_bodies.lock().expect("body lock").push(body.clone());

    let (status, payload) = match behavior {
        StubBehavior::Hang => {
            // Hold the connection open without answering. The client's timeout
            // is what must end this exchange.
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

    let error = embed_one(&embedder, "hello").expect_err("dimension mismatch must be refused");

    assert!(
        error.contains("384") && error.contains("512"),
        "error must name both dimensions: {error}"
    );
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
    let embedder =
        RemoteApiEmbedder::with_dimension(settings, 384).with_request_timeout(Duration::from_millis(300));

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
