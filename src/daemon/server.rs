//! Unix-domain socket accept loop + per-connection dispatcher
//! (bd-oja31 skeleton). Wraps the framing in
//! [`super::protocol`] with the seed dispatch table for
//! `ee.daemon.echo` and the `ee.daemon.context` stub.
//!
//! Threading: each accepted connection is handled on a
//! `std::thread::spawn` worker. A shutdown signal (an
//! `Arc<AtomicBool>`) is checked between accepts; the accept loop
//! breaks on the next iteration when the signal flips. A follow-up
//! slice will wrap this with Asupersync supervision; the wire framing
//! and dispatch table are stable across that refactor.
//!
//! Platform: this module is `#[cfg(unix)]`; Windows builds skip it and
//! the CLI handler short-circuits with
//! [`super::DaemonStartError::PlatformUnsupported`].

#![cfg(unix)]

use std::fs;
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;

use super::protocol::{
    DaemonRequest, DaemonResponse, FrameReadError, FrameWriteError, read_request, write_response,
};
use super::{
    DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE, DAEMON_DEFAULT_RPC_TIMEOUT, DaemonStartError,
};

/// Method dispatch name for the round-trip integrity check.
pub const METHOD_ECHO: &str = "ee.daemon.echo";

/// Method dispatch name for the warm-loaded `ee context` stub.
pub const METHOD_CONTEXT: &str = "ee.daemon.context";

/// Error code returned when a request's `method` field does not match
/// any registered handler.
pub const DAEMON_UNKNOWN_METHOD_CODE: &str = "daemon_unknown_method";

/// Error code returned when the request envelope failed to decode
/// (malformed JSON, missing required field, schema mismatch).
pub const DAEMON_REQUEST_DECODE_FAILED_CODE: &str = "daemon_request_decode_failed";

/// Error code returned when the request envelope's `schema` field does
/// not match [`super::DAEMON_REQUEST_SCHEMA_V1`].
pub const DAEMON_REQUEST_SCHEMA_MISMATCH_CODE: &str = "daemon_request_schema_mismatch";

/// Handle returned by [`start_server`]. Holds the accept-loop thread
/// and the shutdown signal; dropping it does NOT stop the server
/// (callers must call [`DaemonServerHandle::shutdown`] explicitly so
/// the socket file is unlinked deterministically).
#[derive(Debug)]
pub struct DaemonServerHandle {
    socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl DaemonServerHandle {
    /// Return the bound socket path for status surfaces / tests.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Signal the accept loop to stop and wait for it to drain. Also
    /// removes the socket file from the filesystem so a subsequent
    /// `start_server` call against the same path does not need a
    /// manual cleanup step.
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the accept loop by connecting to the socket from the
        // current process; the loop checks the shutdown flag between
        // accepts but blocks inside `accept()` itself. The connect-
        // and-immediately-drop pattern unblocks the listener cheaply.
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(handle) = self.accept_thread.take() {
            handle
                .join()
                .map_err(|_| io::Error::other("daemon accept thread panicked"))?;
        }
        if self.socket_path.exists() {
            fs::remove_file(&self.socket_path)?;
        }
        Ok(())
    }
}

impl Drop for DaemonServerHandle {
    fn drop(&mut self) {
        // Best-effort cleanup: signal shutdown and join the accept
        // thread so the socket file is unlinked even if the caller
        // forgot to call `shutdown`. Errors are intentionally
        // swallowed in Drop; the explicit `shutdown` method surfaces
        // them when callers care.
        let _ = self.shutdown();
    }
}

/// Bind a UDS at `socket_path` and spawn the accept loop. Returns a
/// handle the caller can use to query the bound path and to signal
/// shutdown. The accept loop runs until [`DaemonServerHandle::shutdown`]
/// is called or the handle is dropped.
///
/// The function refuses to overwrite a non-socket file at
/// `socket_path`; the operator must remove a stale path explicitly so
/// the daemon never silently truncates a regular file someone
/// accidentally pointed it at.
pub fn start_server(
    socket_path: impl Into<PathBuf>,
) -> Result<DaemonServerHandle, DaemonStartError> {
    let socket_path = socket_path.into();

    if let Some(parent) = socket_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| DaemonStartError::SocketDirCreate {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    match fs::symlink_metadata(&socket_path) {
        Ok(metadata) => {
            // The path exists. Only proceed if it is itself a socket
            // (a stale daemon left it behind); refuse anything else.
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                if !metadata.file_type().is_socket() {
                    return Err(DaemonStartError::SocketPathOccupied { path: socket_path });
                }
            }
            #[cfg(not(unix))]
            {
                let _ = metadata;
                return Err(DaemonStartError::PlatformUnsupported);
            }
            // Stale socket: unlink so bind succeeds.
            fs::remove_file(&socket_path).map_err(|source| DaemonStartError::Bind {
                path: socket_path.clone(),
                source,
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(DaemonStartError::Bind {
                path: socket_path,
                source,
            });
        }
    }

    let listener = UnixListener::bind(&socket_path).map_err(|source| DaemonStartError::Bind {
        path: socket_path.clone(),
        source,
    })?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_in_thread = Arc::clone(&shutdown);
    let listener_path_in_thread = socket_path.clone();

    let accept_thread = thread::Builder::new()
        .name("ee-daemon-accept".to_owned())
        .spawn(move || {
            run_accept_loop(listener, listener_path_in_thread, shutdown_in_thread);
        })
        .map_err(|source| DaemonStartError::Bind {
            path: socket_path.clone(),
            source,
        })?;

    Ok(DaemonServerHandle {
        socket_path,
        shutdown,
        accept_thread: Some(accept_thread),
    })
}

fn run_accept_loop(listener: UnixListener, socket_path: PathBuf, shutdown: Arc<AtomicBool>) {
    let _ = socket_path; // reserved for future tracing.
    for incoming in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match incoming {
            Ok(stream) => {
                let _ = thread::Builder::new()
                    .name("ee-daemon-conn".to_owned())
                    .spawn(move || {
                        handle_connection(stream);
                    });
            }
            Err(error) => {
                // Accept errors that are not transient bring down the
                // accept loop; the next `start_server` call after the
                // operator fixes the cause will bind fresh.
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }
}

fn handle_connection(mut stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(DAEMON_DEFAULT_RPC_TIMEOUT));
    let _ = stream.set_write_timeout(Some(DAEMON_DEFAULT_RPC_TIMEOUT));

    // Skeleton: one request per accepted connection. A follow-up
    // multiplexing slice will loop here so a single client can run
    // many RPCs over the same socket; the framing already supports
    // that because each frame is self-contained.
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(FrameReadError::Eof) => return,
        Err(other) => {
            let response = DaemonResponse::err(
                "<unknown>",
                DAEMON_REQUEST_DECODE_FAILED_CODE,
                other.to_string(),
            );
            let _ = write_response(&mut stream, &response);
            return;
        }
    };

    let response = dispatch(&request);
    let _ = write_response(&mut stream, &response);
}

/// Pure dispatch: map a parsed [`DaemonRequest`] to a
/// [`DaemonResponse`]. Exposed for unit tests that exercise the
/// dispatch table without paying for a UDS round-trip.
#[must_use]
pub fn dispatch(request: &DaemonRequest) -> DaemonResponse {
    if request.schema != super::DAEMON_REQUEST_SCHEMA_V1 {
        return DaemonResponse::err(
            request.request_id.clone(),
            DAEMON_REQUEST_SCHEMA_MISMATCH_CODE,
            format!(
                "expected schema `{}`, got `{}`",
                super::DAEMON_REQUEST_SCHEMA_V1,
                request.schema,
            ),
        );
    }

    match request.method.as_str() {
        METHOD_ECHO => DaemonResponse::ok(request.request_id.clone(), request.params.clone()),
        METHOD_CONTEXT => DaemonResponse::err(
            request.request_id.clone(),
            DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE,
            "ee.daemon.context is a stub until the ANN warm-load slice ships; \
             the CLI client should fall back to the in-process `ee context` path.",
        )
        .with_degraded(DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE),
        other => DaemonResponse::err(
            request.request_id.clone(),
            DAEMON_UNKNOWN_METHOD_CODE,
            format!("unknown daemon method `{other}`"),
        ),
    }
}

/// Open a UDS client connection to a running daemon and send exactly
/// one request, returning the parsed response. The CLI client uses
/// this for `ee daemon status` round-trip checks and for any future
/// hot-path proxy calls.
pub fn client_round_trip(
    socket_path: &Path,
    request: &DaemonRequest,
) -> Result<DaemonResponse, ClientError> {
    let mut stream = UnixStream::connect(socket_path).map_err(ClientError::Connect)?;
    stream
        .set_read_timeout(Some(DAEMON_DEFAULT_RPC_TIMEOUT))
        .map_err(ClientError::Io)?;
    stream
        .set_write_timeout(Some(DAEMON_DEFAULT_RPC_TIMEOUT))
        .map_err(ClientError::Io)?;

    let body = serde_json::to_vec(request).map_err(ClientError::Encode)?;
    use std::io::Write;
    let length = u32::try_from(body.len())
        .map_err(|_| ClientError::RequestTooLarge { actual: body.len() })?;
    stream
        .write_all(&length.to_be_bytes())
        .map_err(ClientError::Io)?;
    stream.write_all(&body).map_err(ClientError::Io)?;
    stream.flush().map_err(ClientError::Io)?;

    // Read the response with the same frame shape.
    let mut response_prefix = [0_u8; 4];
    use std::io::Read;
    stream
        .read_exact(&mut response_prefix)
        .map_err(ClientError::Io)?;
    let announced = u32::from_be_bytes(response_prefix);
    let announced_usize =
        usize::try_from(announced).map_err(|_| ClientError::ResponseTooLarge { announced })?;
    if announced_usize > super::DAEMON_RESPONSE_MAX_BYTES {
        return Err(ClientError::ResponseTooLarge { announced });
    }
    let mut buffer = vec![0_u8; announced_usize];
    stream.read_exact(&mut buffer).map_err(ClientError::Io)?;
    let response: DaemonResponse = serde_json::from_slice(&buffer).map_err(ClientError::Decode)?;
    Ok(response)
}

/// Errors that can occur on the client side of a UDS round-trip.
#[derive(Debug)]
pub enum ClientError {
    Connect(io::Error),
    Io(io::Error),
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    RequestTooLarge { actual: usize },
    ResponseTooLarge { announced: u32 },
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(source) => {
                write!(formatter, "failed to connect to daemon socket: {source}")
            }
            Self::Io(source) => write!(formatter, "io error during daemon round-trip: {source}"),
            Self::Encode(source) => write!(formatter, "failed to encode daemon request: {source}"),
            Self::Decode(source) => write!(formatter, "failed to decode daemon response: {source}"),
            Self::RequestTooLarge { actual } => write!(
                formatter,
                "daemon request is {actual} bytes which exceeds the {}-byte cap",
                super::DAEMON_REQUEST_MAX_BYTES
            ),
            Self::ResponseTooLarge { announced } => write!(
                formatter,
                "daemon response announced {announced} bytes which exceeds the {}-byte cap",
                super::DAEMON_RESPONSE_MAX_BYTES
            ),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect(source) | Self::Io(source) => Some(source),
            Self::Encode(source) | Self::Decode(source) => Some(source),
            Self::RequestTooLarge { .. } | Self::ResponseTooLarge { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_echo_returns_params_unchanged() {
        let params = serde_json::json!({"k": "v", "n": 7});
        let request = DaemonRequest::new("req-echo-001", METHOD_ECHO, params.clone());
        let response = dispatch(&request);
        assert_eq!(response.request_id, "req-echo-001");
        assert_eq!(response.result, Some(params));
        assert!(response.error.is_none());
        assert!(response.degraded_codes.is_empty());
    }

    #[test]
    fn dispatch_context_returns_warmload_not_yet_implemented() {
        let request = DaemonRequest::new(
            "req-ctx-001",
            METHOD_CONTEXT,
            serde_json::json!({"task": "ship daemon"}),
        );
        let response = dispatch(&request);
        assert!(response.result.is_none());
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE);
        assert!(
            response
                .degraded_codes
                .contains(&DAEMON_ANN_WARMLOAD_NOT_YET_IMPLEMENTED_CODE.to_owned())
        );
    }

    #[test]
    fn dispatch_unknown_method_returns_unknown_method_code() {
        let request = DaemonRequest::new("req-unk-001", "ee.daemon.nope", Value::Null);
        let response = dispatch(&request);
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_UNKNOWN_METHOD_CODE);
    }

    #[test]
    fn dispatch_schema_mismatch_returns_schema_mismatch_code() {
        let bogus = DaemonRequest {
            schema: "ee.daemon.request.v0_wrong".to_owned(),
            request_id: "req-schema-001".to_owned(),
            method: METHOD_ECHO.to_owned(),
            params: Value::Null,
        };
        let response = dispatch(&bogus);
        let error = response.error.as_ref().expect("must have error");
        assert_eq!(error.code, DAEMON_REQUEST_SCHEMA_MISMATCH_CODE);
    }

    #[test]
    fn start_server_then_client_round_trip_echo() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket_path = temp.path().join("ee-daemon-test.sock");
        let mut handle = start_server(&socket_path).expect("server must start");
        // Give the accept thread a moment to enter the listen state.
        thread::sleep(Duration::from_millis(50));

        let request = DaemonRequest::new(
            "req-roundtrip-001",
            METHOD_ECHO,
            serde_json::json!({"ping": "pong"}),
        );
        let response = client_round_trip(handle.socket_path(), &request).expect("round-trip");
        assert_eq!(response.request_id, "req-roundtrip-001");
        assert_eq!(response.result, Some(serde_json::json!({"ping": "pong"})));

        handle.shutdown().expect("shutdown");
        // After shutdown, the socket file must be gone so a subsequent
        // start_server against the same path does not need manual
        // cleanup.
        assert!(!socket_path.exists(), "socket file must be unlinked");
    }

    #[test]
    fn start_server_refuses_non_socket_existing_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("not-a-socket");
        fs::write(&path, b"i am a regular file").expect("write");
        let error = start_server(&path).expect_err("must refuse non-socket");
        assert!(matches!(error, DaemonStartError::SocketPathOccupied { .. }));
        // The regular file must still exist after the refused start;
        // the daemon must not silently overwrite arbitrary paths.
        assert!(path.exists());
    }
}
