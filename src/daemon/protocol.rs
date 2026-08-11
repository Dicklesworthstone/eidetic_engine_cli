//! Wire framing + envelope parsing for the daemon UDS RPC (bd-oja31).
//!
//! Wire framing:
//! ```text
//! [4-byte big-endian unsigned length] [JSON bytes]
//! ```
//!
//! The 4-byte prefix is a `u32` in network byte order. Implementations
//! MUST refuse a length above [`super::DAEMON_REQUEST_MAX_BYTES`] (for
//! inbound requests) or [`super::DAEMON_RESPONSE_MAX_BYTES`] (for
//! outbound responses) to bound peak per-connection allocation.
//!
//! JSON shape: pin the field set canonically through this module's
//! `DaemonRequest` / `DaemonResponse` types. The matching JSON Schemas
//! at `docs/schemas/ee.daemon.{request,response}.v1.json` are the
//! agent-facing contract; agents that don't speak Rust read those
//! schemas directly, but the Rust types here are the source of truth
//! for what `ee daemon` accepts/emits.

use std::io::{self, Read, Write};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::{
    DAEMON_REQUEST_MAX_BYTES, DAEMON_REQUEST_SCHEMA_V1, DAEMON_RESPONSE_MAX_BYTES,
    DAEMON_RESPONSE_SCHEMA_V1,
};

/// Method dispatch name for the warm-loaded `ee search` path.
pub const METHOD_SEARCH: &str = "ee.daemon.search";

/// Strict method-specific request schema for [`METHOD_SEARCH`].
pub const DAEMON_SEARCH_REQUEST_SCHEMA_V1: &str = "ee.daemon.search.request.v1";

/// Strict method-specific response schema for [`METHOD_SEARCH`].
pub const DAEMON_SEARCH_RESPONSE_SCHEMA_V2: &str = "ee.daemon.search.response.v2";

fn deserialize_present_json_value<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

/// Wire-level request envelope. Mirrors
/// `docs/schemas/ee.daemon.request.v1.json` exactly: `schema` is pinned
/// to the v1 constant, `request_id` is caller-chosen (ULID/UUIDv7
/// recommended), `agent_id` names the calling agent, `workspace_id`
/// optionally scopes the caller's workspace, `method` is one of the
/// dispatch names, and `params` is a method-specific JSON object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DaemonRequest {
    pub schema: String,
    pub request_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub method: String,
    // `params` is `type: object` in ee.daemon.request.v1 and optional
    // (not in the schema's `required`). Skip serializing a null so the
    // emitted envelope never carries the schema-invalid `"params":
    // null`; an absent params round-trips back to `Value::Null` via the
    // custom `Deserialize` below.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

impl DaemonRequest {
    /// Construct a request envelope with the canonical schema id
    /// already populated. Helper for tests and the CLI client.
    #[must_use]
    pub fn new(
        request_id: impl Into<String>,
        agent_id: impl Into<String>,
        method: impl Into<String>,
        params: Value,
    ) -> Self {
        Self {
            schema: DAEMON_REQUEST_SCHEMA_V1.to_owned(),
            request_id: request_id.into(),
            agent_id: agent_id.into(),
            workspace_id: None,
            method: method.into(),
            params,
        }
    }
}

// Hand-written `Deserialize` so the wire boundary enforces the
// ee.daemon.request.v1 contract that the derived deserializer ignored:
// the derive accepted unknown envelope fields and a non-object
// `params`, letting a malformed or hostile peer smuggle a shape the
// JSON Schema would reject straight into a typed `DaemonRequest`
// (bd-17g8u). The shadow `Wire` struct applies `deny_unknown_fields`
// (mirroring the schema's `additionalProperties: false`), then we
// reject a present-but-non-object `params`.
impl<'de> Deserialize<'de> for DaemonRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema: String,
            request_id: String,
            agent_id: String,
            #[serde(default)]
            workspace_id: Option<String>,
            method: String,
            #[serde(default, deserialize_with = "deserialize_present_json_value")]
            params: Option<Value>,
        }

        let wire = Wire::deserialize(deserializer)?;
        // Absent `params` (schema-optional) becomes `Value::Null`,
        // preserving the prior `#[serde(default)]` behaviour. A present
        // value MUST be an object: `null` / string / number / array all
        // violate `"params": { "type": "object" }`.
        let params = match wire.params {
            None => Value::Null,
            Some(value) if value.is_object() => value,
            Some(_) => {
                return Err(serde::de::Error::custom(
                    "ee.daemon.request.v1: `params` must be a JSON object",
                ));
            }
        };
        Ok(DaemonRequest {
            schema: wire.schema,
            request_id: wire.request_id,
            agent_id: wire.agent_id,
            workspace_id: wire.workspace_id,
            method: wire.method,
            params,
        })
    }
}

/// Wire-level error envelope inside [`DaemonResponse`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonResponseError {
    pub code: String,
    pub message: String,
}

/// Wire-level response envelope. Exactly one of `result` or `error` is
/// populated per response; the other field is serialized as `null`
/// (via `Option<...>` defaulting to `None`, then `skip_serializing_if =
/// "Option::is_none"`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DaemonResponse {
    pub schema: String,
    pub request_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<DaemonResponseError>,
    #[serde(default)]
    pub degraded_codes: Vec<String>,
}

// Hand-written `Deserialize` so the read side enforces the
// ee.daemon.response.v1 "exactly one of `result` or `error`" invariant
// that the schema documents in prose but does not structurally gate,
// and that the derived deserializer ignored entirely (bd-3a0zx). A
// hostile daemon-impersonator could otherwise wire-send BOTH a crafted
// `result` and a plausible `error`; a CLI client that branches on
// `error.is_some()` would take the safe fallback while any path that
// consumed `result` first would act on attacker-controlled JSON — a
// confused-deputy at the response layer. The shadow `Wire` struct also
// applies `deny_unknown_fields` (mirroring `additionalProperties:
// false`).
impl<'de> Deserialize<'de> for DaemonResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema: String,
            request_id: String,
            agent_id: String,
            #[serde(default)]
            workspace_id: Option<String>,
            #[serde(default)]
            result: Option<Value>,
            #[serde(default)]
            error: Option<DaemonResponseError>,
            #[serde(default)]
            degraded_codes: Vec<String>,
        }

        let wire = Wire::deserialize(deserializer)?;
        match (wire.result.is_some(), wire.error.is_some()) {
            (true, true) => {
                return Err(serde::de::Error::custom(
                    "ee.daemon.response.v1: exactly one of `result` or `error` may be set, not both",
                ));
            }
            (false, false) => {
                return Err(serde::de::Error::custom(
                    "ee.daemon.response.v1: exactly one of `result` or `error` must be set",
                ));
            }
            _ => {}
        }
        Ok(DaemonResponse {
            schema: wire.schema,
            request_id: wire.request_id,
            agent_id: wire.agent_id,
            workspace_id: wire.workspace_id,
            result: wire.result,
            error: wire.error,
            degraded_codes: wire.degraded_codes,
        })
    }
}

impl DaemonResponse {
    /// Build a success response with the supplied result value.
    #[must_use]
    pub fn ok(
        request_id: impl Into<String>,
        agent_id: impl Into<String>,
        workspace_id: Option<String>,
        result: Value,
    ) -> Self {
        Self {
            schema: DAEMON_RESPONSE_SCHEMA_V1.to_owned(),
            request_id: request_id.into(),
            agent_id: agent_id.into(),
            workspace_id,
            result: Some(result),
            error: None,
            degraded_codes: Vec::new(),
        }
    }

    /// Build an error response with the supplied code and message.
    #[must_use]
    pub fn err(
        request_id: impl Into<String>,
        agent_id: impl Into<String>,
        workspace_id: Option<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema: DAEMON_RESPONSE_SCHEMA_V1.to_owned(),
            request_id: request_id.into(),
            agent_id: agent_id.into(),
            workspace_id,
            result: None,
            error: Some(DaemonResponseError {
                code: code.into(),
                message: message.into(),
            }),
            degraded_codes: Vec::new(),
        }
    }

    /// Attach a degraded code to the response. Returns the modified
    /// response so callers can chain in a builder style.
    #[must_use]
    pub fn with_degraded(mut self, code: impl Into<String>) -> Self {
        let code = code.into();
        if !self.degraded_codes.contains(&code) {
            self.degraded_codes.push(code);
        }
        self
    }
}

/// Errors that can occur on the read side of the wire framing.
#[derive(Debug)]
pub enum FrameReadError {
    /// The peer closed the connection cleanly before sending a length
    /// prefix. Distinct from `Io` so the accept loop can break without
    /// logging a misleading error.
    Eof,
    /// The peer announced a payload larger than the configured cap.
    /// Defends against unbounded `Vec` allocation.
    TooLarge { announced: u32, max: usize },
    /// A short read happened mid-frame (the connection died after the
    /// length prefix but before the full payload arrived).
    Truncated { expected: u32, got: usize },
    /// Underlying `io::Error`, including timeouts.
    Io(io::Error),
    /// The framed payload was not valid UTF-8 JSON.
    Decode(serde_json::Error),
}

impl std::fmt::Display for FrameReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Eof => formatter.write_str("peer closed connection before sending a frame"),
            Self::TooLarge { announced, max } => write!(
                formatter,
                "request frame announced {announced} bytes which exceeds the {max}-byte cap"
            ),
            Self::Truncated { expected, got } => write!(
                formatter,
                "request frame truncated after {got} of {expected} announced bytes"
            ),
            Self::Io(source) => write!(formatter, "io error reading frame: {source}"),
            Self::Decode(source) => write!(formatter, "frame payload was not valid JSON: {source}"),
        }
    }
}

impl std::error::Error for FrameReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Decode(source) => Some(source),
            Self::Eof | Self::TooLarge { .. } | Self::Truncated { .. } => None,
        }
    }
}

impl From<io::Error> for FrameReadError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

/// Initial body-buffer capacity for an inbound frame. The 4-byte length
/// prefix is attacker-controlled, so [`read_request`] MUST NOT
/// pre-allocate the announced size: a slow-loris client could announce
/// [`super::DAEMON_REQUEST_MAX_BYTES`] (4 MiB) and then stall, pinning a
/// zero-filled 4 MiB heap buffer per connection for the full read
/// timeout. Instead the body buffer starts at this small cap and grows
/// only as real bytes arrive, so peak per-connection allocation tracks
/// bytes actually delivered rather than bytes announced. bd-26jks.
const REQUEST_BODY_PREALLOC_CAP: usize = 8 * 1024;

/// Chunk size for the streaming body read. Bounds the transient stack
/// copy and the granularity at which the body buffer grows.
const REQUEST_BODY_READ_CHUNK: usize = 8 * 1024;

/// Read one length-prefixed JSON request from `reader`. Returns `Err`
/// at EOF (`FrameReadError::Eof`), on length overflow
/// (`FrameReadError::TooLarge`), on short reads
/// (`FrameReadError::Truncated`), on raw I/O errors
/// (`FrameReadError::Io`), and on JSON decode failures
/// (`FrameReadError::Decode`). The cap is per-frame; the caller is
/// free to read multiple frames on the same reader by re-entering this
/// function.
///
/// The body is streamed in [`REQUEST_BODY_READ_CHUNK`]-sized reads and
/// the buffer grows incrementally, so a client that announces a large
/// frame but trickles (or never sends) the body holds at most the bytes
/// it actually delivered — not the announced size. The announced length
/// is still hard-capped at [`super::DAEMON_REQUEST_MAX_BYTES`] before
/// the read begins. bd-26jks.
pub fn read_request<R: Read>(reader: &mut R) -> Result<DaemonRequest, FrameReadError> {
    let mut length_prefix = [0_u8; 4];
    match reader.read_exact(&mut length_prefix) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(FrameReadError::Eof);
        }
        Err(error) => return Err(FrameReadError::Io(error)),
    }
    let announced = u32::from_be_bytes(length_prefix);
    let announced_usize = usize::try_from(announced).map_err(|_| FrameReadError::TooLarge {
        announced,
        max: DAEMON_REQUEST_MAX_BYTES,
    })?;
    if announced_usize > DAEMON_REQUEST_MAX_BYTES {
        return Err(FrameReadError::TooLarge {
            announced,
            max: DAEMON_REQUEST_MAX_BYTES,
        });
    }
    // Stream the body in bounded chunks so a hostile (or merely stuck)
    // client that announces up to DAEMON_REQUEST_MAX_BYTES but trickles
    // the body cannot pin the announced size in heap. The buffer grows
    // only as bytes actually arrive; peak allocation tracks delivered
    // bytes, not announced bytes. bd-26jks.
    let mut buffer = Vec::with_capacity(announced_usize.min(REQUEST_BODY_PREALLOC_CAP));
    let mut chunk = [0_u8; REQUEST_BODY_READ_CHUNK];
    while buffer.len() < announced_usize {
        let remaining = announced_usize - buffer.len();
        let want = remaining.min(REQUEST_BODY_READ_CHUNK);
        match reader.read(&mut chunk[..want]) {
            // Peer closed mid-frame before delivering the announced
            // bytes. Report how many actually arrived so the caller can
            // tell a stalled slow-loris from a clean EOF.
            Ok(0) => {
                return Err(FrameReadError::Truncated {
                    expected: announced,
                    got: buffer.len(),
                });
            }
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(FrameReadError::Io(error)),
        }
    }
    let request: DaemonRequest = serde_json::from_slice(&buffer).map_err(FrameReadError::Decode)?;
    Ok(request)
}

/// Errors that can occur on the write side of the wire framing.
#[derive(Debug)]
pub enum FrameWriteError {
    /// The serialized response exceeded
    /// [`super::DAEMON_RESPONSE_MAX_BYTES`]. Methods that would
    /// produce a larger response MUST truncate before serialization.
    TooLarge { actual: usize, max: usize },
    /// Underlying `io::Error`, including timeouts.
    Io(io::Error),
    /// The response failed to serialize (a `Value::Number` with a NaN
    /// payload, for instance, would trip this).
    Encode(serde_json::Error),
}

impl std::fmt::Display for FrameWriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { actual, max } => write!(
                formatter,
                "response frame is {actual} bytes which exceeds the {max}-byte cap"
            ),
            Self::Io(source) => write!(formatter, "io error writing frame: {source}"),
            Self::Encode(source) => {
                write!(formatter, "response failed to encode as JSON: {source}")
            }
        }
    }
}

impl std::error::Error for FrameWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Encode(source) => Some(source),
            Self::TooLarge { .. } => None,
        }
    }
}

impl From<io::Error> for FrameWriteError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

/// Serialize and write one length-prefixed JSON response to `writer`.
/// Refuses to send a response above
/// [`super::DAEMON_RESPONSE_MAX_BYTES`] (returns `TooLarge`); callers
/// MUST truncate or split before this point.
pub fn write_response<W: Write>(
    writer: &mut W,
    response: &DaemonResponse,
) -> Result<(), FrameWriteError> {
    let body = serde_json::to_vec(response).map_err(FrameWriteError::Encode)?;
    if body.len() > DAEMON_RESPONSE_MAX_BYTES {
        return Err(FrameWriteError::TooLarge {
            actual: body.len(),
            max: DAEMON_RESPONSE_MAX_BYTES,
        });
    }
    let length = u32::try_from(body.len()).map_err(|_| FrameWriteError::TooLarge {
        actual: body.len(),
        max: DAEMON_RESPONSE_MAX_BYTES,
    })?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip_echo_request_through_read_request() {
        let request = DaemonRequest::new(
            "req-echo-001",
            "agent-protocol-test",
            "ee.daemon.echo",
            serde_json::json!({"hello": "world", "n": 42}),
        );
        let body = serde_json::to_vec(&request).expect("request must serialize");
        let length = u32::try_from(body.len()).expect("body length must fit u32");
        let mut buffer = Vec::with_capacity(4 + body.len());
        buffer.extend_from_slice(&length.to_be_bytes());
        buffer.extend_from_slice(&body);
        let mut cursor = Cursor::new(buffer);
        let parsed = read_request(&mut cursor).expect("frame must parse");
        assert_eq!(parsed, request);
    }

    #[test]
    fn read_request_refuses_oversize_length_prefix() {
        // Announce a 4 GiB payload — well above DAEMON_REQUEST_MAX_BYTES.
        // The reader must refuse without allocating the buffer.
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&u32::MAX.to_be_bytes());
        let mut cursor = Cursor::new(buffer);
        let error = read_request(&mut cursor).expect_err("oversized prefix must be refused");
        match error {
            FrameReadError::TooLarge { announced, max } => {
                assert_eq!(announced, u32::MAX);
                assert_eq!(max, DAEMON_REQUEST_MAX_BYTES);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn read_request_truncated_body_reports_delivered_bytes() {
        // Slow-loris shape (bd-26jks): announce a frame far larger than
        // REQUEST_BODY_PREALLOC_CAP, then deliver only a few body bytes
        // before EOF. The reader must NOT pre-allocate the announced
        // size; it must surface `Truncated` with the count of bytes that
        // actually arrived (not the old hard-coded `got: 0`).
        let announced: u32 = 1_048_576; // 1 MiB announced
        let delivered: &[u8] = b"{\"partial\":";
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&announced.to_be_bytes());
        buffer.extend_from_slice(delivered);
        let mut cursor = Cursor::new(buffer);
        let error = read_request(&mut cursor).expect_err("truncated frame must be refused");
        match error {
            FrameReadError::Truncated { expected, got } => {
                assert_eq!(expected, announced);
                assert_eq!(got, delivered.len());
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn read_request_chunked_body_spanning_multiple_reads_parses() {
        // A legitimate body larger than REQUEST_BODY_READ_CHUNK must
        // round-trip through the streaming read loop unchanged, proving
        // the incremental-growth path reassembles the frame correctly.
        let filler = "x".repeat(REQUEST_BODY_READ_CHUNK * 3);
        let request = DaemonRequest::new(
            "req-chunked-001",
            "agent-protocol-test",
            "ee.daemon.echo",
            serde_json::json!({ "blob": filler }),
        );
        let body = serde_json::to_vec(&request).expect("request must serialize");
        assert!(
            body.len() > REQUEST_BODY_READ_CHUNK,
            "body must exceed one read chunk to exercise the loop"
        );
        let length = u32::try_from(body.len()).expect("body length must fit u32");
        let mut buffer = Vec::with_capacity(4 + body.len());
        buffer.extend_from_slice(&length.to_be_bytes());
        buffer.extend_from_slice(&body);
        let mut cursor = Cursor::new(buffer);
        let parsed = read_request(&mut cursor).expect("multi-chunk frame must parse");
        assert_eq!(parsed, request);
    }

    #[test]
    fn read_request_eof_before_prefix_yields_eof() {
        // An empty connection (peer closed before sending anything)
        // must surface as `Eof`, NOT as a generic `Io` error. The
        // accept loop relies on this distinction to break cleanly
        // without logging a misleading message.
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let error = read_request(&mut cursor).expect_err("empty stream must EOF");
        assert!(matches!(error, FrameReadError::Eof), "got {error:?}");
    }

    #[test]
    fn read_request_decode_error_surfaces_decode_variant() {
        // Frame the JSON body as a length-prefix + garbage bytes. The
        // length-prefix check passes (under the cap), the read
        // succeeds, and serde_json fails to parse — that error must
        // round-trip as `Decode` so the server can return a
        // `daemon_request_decode_failed` envelope.
        let garbage = b"not even close to valid JSON";
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&(garbage.len() as u32).to_be_bytes());
        buffer.extend_from_slice(garbage);
        let mut cursor = Cursor::new(buffer);
        let error = read_request(&mut cursor).expect_err("garbage body must fail decode");
        assert!(matches!(error, FrameReadError::Decode(_)), "got {error:?}");
    }

    #[test]
    fn write_response_emits_length_prefixed_frame() {
        let response = DaemonResponse::ok(
            "req-echo-001",
            "agent-protocol-test",
            Some("workspace-protocol-test".to_owned()),
            serde_json::json!({"hello": "world"}),
        );
        let mut buffer = Vec::new();
        write_response(&mut buffer, &response).expect("response must write");
        assert!(buffer.len() > 4, "frame must include length + body");
        let announced = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
        assert_eq!(announced, buffer.len() - 4);
        let parsed: DaemonResponse =
            serde_json::from_slice(&buffer[4..]).expect("body must round-trip as JSON");
        assert_eq!(parsed, response);
    }

    #[test]
    fn write_response_refuses_oversize_payload() {
        // Build a response whose serialized form deliberately exceeds
        // DAEMON_RESPONSE_MAX_BYTES. The writer must refuse rather
        // than emit a frame that downstream clients would reject.
        let huge_payload = "x".repeat(DAEMON_RESPONSE_MAX_BYTES + 1);
        let response = DaemonResponse::ok(
            "req-too-large",
            "agent-protocol-test",
            None,
            Value::String(huge_payload),
        );
        let mut buffer = Vec::new();
        let error =
            write_response(&mut buffer, &response).expect_err("oversize response must be refused");
        assert!(
            matches!(error, FrameWriteError::TooLarge { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn response_with_degraded_preserves_order_and_dedups() {
        let archived_daemon_code = "archived_daemon_code";
        let response = DaemonResponse::err(
            "req-stub-001",
            "agent-protocol-test",
            None,
            archived_daemon_code,
            "stub",
        )
        .with_degraded("alpha")
        .with_degraded("beta")
        .with_degraded("alpha")
        .with_degraded(archived_daemon_code)
        .with_degraded(archived_daemon_code);
        assert_eq!(
            response.degraded_codes,
            vec![
                "alpha".to_owned(),
                "beta".to_owned(),
                archived_daemon_code.to_owned(),
            ]
        );
    }

    #[test]
    fn response_serializes_only_one_of_result_or_error() {
        // The schema invariant: exactly one of `result` / `error` is
        // populated per response. Test the serialization shape so the
        // wire bytes match the JSON Schema in
        // `docs/schemas/ee.daemon.response.v1.json`.
        let ok_response = DaemonResponse::ok(
            "req-ok",
            "agent-protocol-test",
            Some("workspace-ok".to_owned()),
            serde_json::json!({"k": "v"}),
        );
        let ok_serialized = serde_json::to_value(&ok_response).expect("ok must serialize");
        assert_eq!(
            ok_serialized.get("agent_id").and_then(Value::as_str),
            Some("agent-protocol-test")
        );
        assert_eq!(
            ok_serialized.get("workspace_id").and_then(Value::as_str),
            Some("workspace-ok")
        );
        assert!(ok_serialized.get("result").is_some());
        assert!(
            ok_serialized.get("error").is_none(),
            "got {ok_serialized:?}"
        );

        let err_response =
            DaemonResponse::err("req-err", "agent-protocol-test", None, "code", "msg");
        let err_serialized = serde_json::to_value(&err_response).expect("err must serialize");
        assert_eq!(
            err_serialized.get("agent_id").and_then(Value::as_str),
            Some("agent-protocol-test")
        );
        assert!(
            err_serialized.get("workspace_id").is_none(),
            "absent workspace_id must be skipped; got {err_serialized:?}"
        );
        assert!(err_serialized.get("error").is_some());
        assert!(
            err_serialized.get("result").is_none(),
            "got {err_serialized:?}"
        );
    }

    // ---- bd-17g8u: DaemonRequest deserialize-boundary contract ----

    #[test]
    fn daemon_request_rejects_non_object_params() {
        // ee.daemon.request.v1 declares `params` as type:object. A
        // present null / string / number / array must be refused at the
        // deserialize boundary rather than silently accepted and
        // dispatched. bd-17g8u.
        let cases = [
            r#"{"schema":"ee.daemon.request.v1","request_id":"r","agent_id":"agent-a","method":"ee.daemon.echo","params":null}"#,
            r#"{"schema":"ee.daemon.request.v1","request_id":"r","agent_id":"agent-a","method":"ee.daemon.echo","params":"x"}"#,
            r#"{"schema":"ee.daemon.request.v1","request_id":"r","agent_id":"agent-a","method":"ee.daemon.echo","params":[1,2]}"#,
            r#"{"schema":"ee.daemon.request.v1","request_id":"r","agent_id":"agent-a","method":"ee.daemon.echo","params":7}"#,
        ];
        for bad in cases {
            assert!(
                serde_json::from_str::<DaemonRequest>(bad).is_err(),
                "non-object params must be rejected: {bad}"
            );
        }
    }

    #[test]
    fn daemon_request_accepts_object_and_absent_params() {
        // A present object is accepted unchanged; absent params is
        // schema-valid (params is optional) and defaults to Value::Null,
        // preserving the prior behaviour.
        let with_obj = r#"{"schema":"ee.daemon.request.v1","request_id":"r","agent_id":"agent-a","workspace_id":"workspace-a","method":"ee.daemon.echo","params":{"k":1}}"#;
        let parsed = serde_json::from_str::<DaemonRequest>(with_obj).expect("object params ok");
        assert_eq!(parsed.agent_id, "agent-a");
        assert_eq!(parsed.workspace_id.as_deref(), Some("workspace-a"));
        assert_eq!(parsed.params, serde_json::json!({"k": 1}));

        let no_params = r#"{"schema":"ee.daemon.request.v1","request_id":"r","agent_id":"agent-a","method":"ee.daemon.echo"}"#;
        let parsed = serde_json::from_str::<DaemonRequest>(no_params).expect("absent params ok");
        assert_eq!(parsed.agent_id, "agent-a");
        assert_eq!(parsed.workspace_id, None);
        assert_eq!(parsed.params, Value::Null);
    }

    #[test]
    fn daemon_request_rejects_missing_agent_id() {
        let bad = r#"{"schema":"ee.daemon.request.v1","request_id":"r","method":"ee.daemon.echo","params":{}}"#;
        assert!(serde_json::from_str::<DaemonRequest>(bad).is_err());
    }

    #[test]
    fn daemon_request_rejects_unknown_envelope_field() {
        // additionalProperties:false — an unknown envelope field must be
        // refused, not silently dropped.
        let bad = r#"{"schema":"ee.daemon.request.v1","request_id":"r","agent_id":"agent-a","method":"ee.daemon.echo","params":{},"unexpected":"spoof"}"#;
        assert!(serde_json::from_str::<DaemonRequest>(bad).is_err());
    }

    // ---- bd-3a0zx: DaemonResponse deserialize-boundary contract ----

    #[test]
    fn daemon_response_rejects_both_result_and_error() {
        // The schema's "exactly one of result or error" invariant must
        // be enforced on the READ side: a both-populated response is a
        // confused-deputy / client-trust hazard. bd-3a0zx.
        let bad = r#"{"schema":"ee.daemon.response.v1","request_id":"r","agent_id":"agent-a","result":1,"error":{"code":"x","message":"y"}}"#;
        assert!(serde_json::from_str::<DaemonResponse>(bad).is_err());
    }

    #[test]
    fn daemon_response_rejects_neither_result_nor_error() {
        let bad = r#"{"schema":"ee.daemon.response.v1","request_id":"r","agent_id":"agent-a"}"#;
        assert!(serde_json::from_str::<DaemonResponse>(bad).is_err());
    }

    #[test]
    fn daemon_response_accepts_exactly_one_populated() {
        let ok = r#"{"schema":"ee.daemon.response.v1","request_id":"r","agent_id":"agent-a","workspace_id":"workspace-a","result":{"k":1}}"#;
        assert!(serde_json::from_str::<DaemonResponse>(ok).is_ok());
        let err = r#"{"schema":"ee.daemon.response.v1","request_id":"r","agent_id":"agent-a","error":{"code":"c","message":"m"}}"#;
        assert!(serde_json::from_str::<DaemonResponse>(err).is_ok());
    }

    #[test]
    fn daemon_response_rejects_missing_agent_id() {
        let bad = r#"{"schema":"ee.daemon.response.v1","request_id":"r","result":1}"#;
        assert!(serde_json::from_str::<DaemonResponse>(bad).is_err());
    }

    #[test]
    fn daemon_response_rejects_unknown_envelope_field() {
        let bad = r#"{"schema":"ee.daemon.response.v1","request_id":"r","agent_id":"agent-a","result":1,"unexpected":"leak"}"#;
        assert!(serde_json::from_str::<DaemonResponse>(bad).is_err());
    }

    #[test]
    fn daemon_response_error_rejects_unknown_field() {
        // The nested error object is additionalProperties:false too.
        let bad = r#"{"schema":"ee.daemon.response.v1","request_id":"r","agent_id":"agent-a","error":{"code":"x","message":"y","extra":1}}"#;
        assert!(serde_json::from_str::<DaemonResponse>(bad).is_err());
    }
}
