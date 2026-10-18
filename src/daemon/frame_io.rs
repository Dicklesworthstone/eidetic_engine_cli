//! Cooperative whole-frame deadlines for daemon protocol transfers.
//!
//! A socket timeout applies to an individual system call, not an entire frame.
//! Observe one deadline and the caller's existing cancellation context across
//! the prefix, body, retries, encoding/decoding and final flush. Generic Read /
//! Write cannot preempt a blocking call: socket owners must still install their
//! transport timeouts. This never creates a task, context, thread or timer.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use super::{
    DAEMON_REQUEST_MAX_BYTES, DAEMON_RESPONSE_MAX_BYTES, DaemonRequest, DaemonResponse,
    FrameReadError, FrameWriteError, REQUEST_BODY_PREALLOC_CAP, REQUEST_BODY_READ_CHUNK,
};

fn frame_checkpoint(limit: Duration) -> impl FnMut() -> io::Result<()> {
    let request = asupersync::Cx::current();
    let started = Instant::now();
    move || {
        // A caller cancellation wins over an expired local frame deadline.
        // Never expose its potentially private reason on this transport path.
        if request.as_ref().is_some_and(|cx| cx.checkpoint().is_err()) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "daemon frame request cancelled",
            ));
        }
        if started.elapsed() >= limit {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "daemon frame transfer deadline exceeded",
            ));
        }
        Ok(())
    }
}

/// Retry only Interrupted from the transport, never a failed checkpoint.
/// Check AFTER every operation too: even a complete late result is not success.
fn retry_io<T>(
    checkpoint: &mut impl FnMut() -> io::Result<()>,
    mut operation: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    loop {
        checkpoint()?;
        let result = operation();
        checkpoint()?;
        match result {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

pub(super) fn read_request<R: Read>(reader: &mut R) -> Result<DaemonRequest, FrameReadError> {
    read_with_checkpoint(
        reader,
        &mut frame_checkpoint(super::super::DAEMON_DEFAULT_RPC_TIMEOUT),
    )
}

fn read_with_checkpoint<R: Read>(
    reader: &mut R,
    checkpoint: &mut impl FnMut() -> io::Result<()>,
) -> Result<DaemonRequest, FrameReadError> {
    let mut prefix = [0_u8; 4];
    let mut filled = 0;
    while filled < prefix.len() {
        let count = retry_io(checkpoint, || reader.read(&mut prefix[filled..]))?;
        if count == 0 {
            return Err(if filled == 0 {
                FrameReadError::Eof
            } else {
                FrameReadError::Truncated {
                    expected: 4,
                    got: filled,
                }
            });
        }
        if count > prefix.len() - filled {
            return Err(invalid_read_count().into());
        }
        filled += count;
    }
    let announced = u32::from_be_bytes(prefix);
    let length = usize::try_from(announced).map_err(|_| FrameReadError::TooLarge {
        announced,
        max: DAEMON_REQUEST_MAX_BYTES,
    })?;
    if length > DAEMON_REQUEST_MAX_BYTES {
        return Err(FrameReadError::TooLarge {
            announced,
            max: DAEMON_REQUEST_MAX_BYTES,
        });
    }
    checkpoint()?;
    let mut buffer = Vec::with_capacity(length.min(REQUEST_BODY_PREALLOC_CAP));
    let mut chunk = [0_u8; REQUEST_BODY_READ_CHUNK];
    while buffer.len() < length {
        let want = (length - buffer.len()).min(chunk.len());
        let count = retry_io(checkpoint, || reader.read(&mut chunk[..want]))?;
        if count == 0 {
            return Err(FrameReadError::Truncated {
                expected: announced,
                got: buffer.len(),
            });
        }
        if count > want {
            return Err(invalid_read_count().into());
        }
        buffer.extend_from_slice(&chunk[..count]);
    }
    checkpoint()?;
    let request = serde_json::from_slice(&buffer);
    checkpoint()?;
    request.map_err(FrameReadError::Decode)
}

fn invalid_read_count() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid daemon frame read count",
    )
}

pub(super) fn write_response<W: Write>(
    writer: &mut W,
    response: &DaemonResponse,
) -> Result<(), FrameWriteError> {
    write_with_checkpoint(
        writer,
        response,
        &mut frame_checkpoint(super::super::DAEMON_DEFAULT_RPC_TIMEOUT),
    )
}

fn write_with_checkpoint<W: Write>(
    writer: &mut W,
    response: &DaemonResponse,
    checkpoint: &mut impl FnMut() -> io::Result<()>,
) -> Result<(), FrameWriteError> {
    let body = encode_response(response, DAEMON_RESPONSE_MAX_BYTES, checkpoint)?;
    let length = u32::try_from(body.len()).map_err(|_| FrameWriteError::TooLarge {
        actual: body.len(),
        max: DAEMON_RESPONSE_MAX_BYTES,
    })?;
    write_all_checked(writer, &length.to_be_bytes(), checkpoint)?;
    write_all_checked(writer, &body, checkpoint)?;
    retry_io(checkpoint, || writer.flush())?;
    Ok(())
}

/// Retain at most the allowed encoded bytes. Once oversized, count the rest
/// without keeping it so TooLarge.actual remains the exact serialized size.
/// This avoids allocating a complete rejected JSON copy, including escaping
/// expansion. Counting still observes the SAME frame checkpoint; a single
/// serializer operation cannot be preempted. This is not a bound on the source
/// Value's allocations, allocator overhead, or the caller's total memory.
struct ResponseBuffer<'a> {
    bytes: Vec<u8>,
    actual: usize,
    limit: usize,
    checkpoint: &'a mut dyn FnMut() -> io::Result<()>,
    failure: Option<io::Error>,
}

impl<'a> ResponseBuffer<'a> {
    fn new(limit: usize, checkpoint: &'a mut dyn FnMut() -> io::Result<()>) -> Self {
        Self {
            bytes: Vec::new(),
            actual: 0,
            limit,
            checkpoint,
            failure: None,
        }
    }
}

impl Write for ResponseBuffer<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(io::Error::other("daemon response encoding already failed"));
        }
        let result = (|| {
            (self.checkpoint)()?;
            let next = self.actual.checked_add(bytes.len()).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "daemon response byte count overflow",
                )
            })?;
            if next <= self.limit {
                if next > self.bytes.capacity() {
                    // Ask for bounded geometric growth rather than reallocating
                    // on every JSON token or reserving an oversized input slice.
                    let capacity = next
                        .max(self.bytes.capacity().saturating_mul(2))
                        .max(REQUEST_BODY_PREALLOC_CAP.min(self.limit))
                        .min(self.limit);
                    self.bytes
                        .try_reserve_exact(capacity - self.bytes.len())
                        .map_err(|_| {
                            io::Error::other("unable to allocate bounded daemon response buffer")
                        })?;
                }
                self.bytes.extend_from_slice(bytes);
            } else {
                // Never expose this rejected prefix to the transport. Retained
                // capacity is bounded by earlier admitted allocation requests.
                self.bytes.clear();
            }
            self.actual = next;
            (self.checkpoint)()?;
            Ok(bytes.len())
        })();
        match result {
            Ok(count) => Ok(count),
            Err(error) => {
                // serde_json wraps writer errors. Preserve the original kind
                // separately so cancellation/deadlines stay transport errors.
                self.failure = Some(error);
                Err(io::Error::other("daemon response encoding interrupted"))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_response(
    response: &DaemonResponse,
    limit: usize,
    checkpoint: &mut impl FnMut() -> io::Result<()>,
) -> Result<Vec<u8>, FrameWriteError> {
    checkpoint()?;
    let (encoded, bytes, actual, failure) = {
        let mut buffer = ResponseBuffer::new(limit, checkpoint);
        let encoded = serde_json::to_writer(&mut buffer, response);
        (encoded, buffer.bytes, buffer.actual, buffer.failure)
    };
    checkpoint()?;
    if let Some(error) = failure {
        return Err(FrameWriteError::Io(error));
    }
    encoded.map_err(FrameWriteError::Encode)?;
    if actual > limit {
        return Err(FrameWriteError::TooLarge { actual, max: limit });
    }
    Ok(bytes)
}

fn write_all_checked<W: Write>(
    writer: &mut W,
    mut bytes: &[u8],
    checkpoint: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        let want = bytes.len().min(REQUEST_BODY_READ_CHUNK);
        let count = retry_io(checkpoint, || writer.write(&bytes[..want]))?;
        if count == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        if count > want {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid daemon frame write count",
            ));
        }
        bytes = &bytes[count..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Cursor;

    fn request_frame() -> (DaemonRequest, Vec<u8>) {
        let request = DaemonRequest::new(
            "frame-budget",
            "agent",
            "ee.daemon.echo",
            serde_json::json!({"text": "資料 café", "number": 7}),
        );
        let body = serde_json::to_vec(&request).expect("request bytes");
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend(body);
        (request, frame)
    }

    struct SteppedReader<'a> {
        bytes: Cursor<Vec<u8>>,
        steps: &'a Cell<usize>,
        width: usize,
    }

    impl Read for SteppedReader<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let want = bytes.len().min(self.width);
            let read = self.bytes.read(&mut bytes[..want]);
            self.steps.set(self.steps.get() + 1);
            read
        }
    }

    fn stopped(kind: io::ErrorKind) -> io::Error {
        io::Error::new(kind, "test transfer stopped")
    }

    #[test]
    fn prefix_and_body_spend_the_same_budget_and_late_completion_is_refused() {
        let (_, bytes) = request_frame();
        for (width, stop_after) in [(1, 3), (1, 9), (usize::MAX, 2)] {
            let steps = Cell::new(0);
            let mut reader = SteppedReader {
                bytes: Cursor::new(bytes.clone()),
                steps: &steps,
                width,
            };
            let result = read_with_checkpoint(&mut reader, &mut || {
                if steps.get() >= stop_after {
                    Err(stopped(io::ErrorKind::TimedOut))
                } else {
                    Ok(())
                }
            });
            assert!(
                matches!(result, Err(FrameReadError::Io(error)) if error.kind() == io::ErrorKind::TimedOut)
            );
            assert_eq!(steps.get(), stop_after);
            if width == usize::MAX {
                assert_eq!(reader.bytes.position(), bytes.len() as u64);
            }
        }
    }

    #[test]
    fn interrupted_transport_retries_stop_but_interrupted_checkpoints_are_terminal() {
        for kind in [io::ErrorKind::TimedOut, io::ErrorKind::Interrupted] {
            let attempts = Cell::new(0);
            let result = retry_io(
                &mut || {
                    if attempts.get() == 3 {
                        Err(stopped(kind))
                    } else {
                        Ok(())
                    }
                },
                || -> io::Result<()> {
                    attempts.set(attempts.get() + 1);
                    Err(stopped(io::ErrorKind::Interrupted))
                },
            );
            assert_eq!(result.expect_err("stop retry loop").kind(), kind);
            assert_eq!(attempts.get(), 3);
        }
        let result = retry_io(
            &mut || Err(stopped(io::ErrorKind::Interrupted)),
            || -> io::Result<()> { panic!("cancelled transfer must not call transport") },
        );
        assert_eq!(
            result.expect_err("cancelled before IO").kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn truncated_prefix_is_distinct_from_clean_eof_and_body_counts_remain_exact() {
        for count in 0..4 {
            let result = read_with_checkpoint(&mut Cursor::new(vec![0; count]), &mut || Ok(()));
            if count == 0 {
                assert!(matches!(result, Err(FrameReadError::Eof)));
            } else {
                assert!(
                    matches!(result, Err(FrameReadError::Truncated {expected: 4, got}) if got == count)
                );
            }
        }
        let mut bytes = 20_u32.to_be_bytes().to_vec();
        bytes.extend(b"abc");
        assert!(matches!(
            read_with_checkpoint(&mut Cursor::new(bytes), &mut || Ok(())),
            Err(FrameReadError::Truncated {
                expected: 20,
                got: 3
            })
        ));
    }

    #[test]
    fn successful_partial_reads_preserve_the_next_frame_and_exact_json() {
        let (request, frame) = request_frame();
        let steps = Cell::new(0);
        let mut bytes = frame.clone();
        bytes.extend(&frame);
        let mut reader = SteppedReader {
            bytes: Cursor::new(bytes),
            steps: &steps,
            width: 3,
        };
        for expected_position in [frame.len(), frame.len() * 2] {
            assert_eq!(
                read_with_checkpoint(&mut reader, &mut || Ok(())).expect("valid frame"),
                request
            );
            assert_eq!(reader.bytes.position(), expected_position as u64);
        }
        assert!(steps.get() > 4);
    }

    struct SteppedWriter<'a> {
        bytes: Vec<u8>,
        steps: &'a Cell<usize>,
        width: usize,
        flush_error: bool,
        flushes: usize,
    }

    impl Write for SteppedWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = bytes.len().min(self.width);
            self.bytes.extend_from_slice(&bytes[..count]);
            self.steps.set(self.steps.get() + 1);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.flush_error {
                Err(stopped(io::ErrorKind::BrokenPipe))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn response_partial_writes_preserve_wire_bytes_and_failed_flush_is_not_delivery() {
        let response =
            DaemonResponse::ok("r", "agent", None, serde_json::json!({"message": "資料"}));
        let body = serde_json::to_vec(&response).expect("response bytes");
        let mut expected = (body.len() as u32).to_be_bytes().to_vec();
        expected.extend(body);
        for flush_error in [false, true] {
            let steps = Cell::new(0);
            let mut writer = SteppedWriter {
                bytes: Vec::new(),
                steps: &steps,
                width: 3,
                flush_error,
                flushes: 0,
            };
            let result = write_with_checkpoint(&mut writer, &response, &mut || Ok(()));
            if flush_error {
                assert!(
                    matches!(result, Err(FrameWriteError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe)
                );
            } else {
                assert!(result.is_ok());
            }
            assert_eq!(writer.bytes, expected);
            assert_eq!(writer.flushes, 1);
            assert!(steps.get() > 2);
        }
    }

    #[test]
    fn response_deadline_stops_before_the_next_write_and_zero_writes_are_errors() {
        let response =
            DaemonResponse::ok("r", "agent", None, serde_json::json!({"message": "body"}));
        for width in [0, 1, 4] {
            let steps = Cell::new(0);
            let mut writer = SteppedWriter {
                bytes: Vec::new(),
                steps: &steps,
                width,
                flush_error: false,
                flushes: 0,
            };
            let result = write_with_checkpoint(&mut writer, &response, &mut || {
                if steps.get() == 2 {
                    Err(stopped(io::ErrorKind::TimedOut))
                } else {
                    Ok(())
                }
            });
            let expected = if width == 0 {
                io::ErrorKind::WriteZero
            } else {
                io::ErrorKind::TimedOut
            };
            assert!(matches!(result, Err(FrameWriteError::Io(error)) if error.kind() == expected));
            assert_eq!(steps.get(), if width == 0 { 1 } else { 2 });
            assert_eq!(writer.flushes, 0);
        }
    }

    #[test]
    fn expired_frame_budget_refuses_io_without_waiting_for_a_socket_timeout() {
        let error = frame_checkpoint(Duration::ZERO)().expect_err("zero remaining time");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn bounded_encoding_matches_native_json_and_exact_oversize_counts() {
        let success = DaemonResponse::ok(
            "r",
            "agent",
            Some("workspace".to_owned()),
            serde_json::json!({"text": "資料 café\n\0\"\\", "values": [null, true, u64::MAX]}),
        )
        .with_degraded("diagnostic")
        .with_search_advisory_delivery("workspace", 19, true);
        let failure = DaemonResponse::err("r", "agent", None, "failure", "quoted \"message\"");
        for response in [success, failure] {
            let expected = serde_json::to_vec(&response).expect("native serialization");
            for limit in [
                expected.len(),
                expected.len() + 1,
                DAEMON_RESPONSE_MAX_BYTES,
            ] {
                assert_eq!(
                    encode_response(&response, limit, &mut || Ok(())).expect("fits"),
                    expected
                );
            }
            for limit in [0, 1, expected.len() - 1] {
                assert!(matches!(encode_response(&response, limit, &mut || Ok(())),
                    Err(FrameWriteError::TooLarge {actual, max}) if actual == expected.len() && max == limit));
            }
        }
    }

    #[test]
    fn exact_wire_limit_is_inclusive_and_oversize_never_writes_a_prefix() {
        let mut response =
            DaemonResponse::ok("r", "agent", None, serde_json::Value::String(String::new()));
        let overhead = serde_json::to_vec(&response).expect("empty response").len();
        response.result = Some(serde_json::Value::String(
            "x".repeat(DAEMON_RESPONSE_MAX_BYTES - overhead),
        ));
        let mut wire = Vec::new();
        write_with_checkpoint(&mut wire, &response, &mut || Ok(())).expect("inclusive cap");
        assert_eq!(wire.len(), DAEMON_RESPONSE_MAX_BYTES + 4);
        assert_eq!(
            &wire[..4],
            &(DAEMON_RESPONSE_MAX_BYTES as u32).to_be_bytes()
        );
        assert_eq!(
            serde_json::from_slice::<DaemonResponse>(&wire[4..]).expect("complete JSON"),
            response
        );
        if let Some(serde_json::Value::String(text)) = &mut response.result {
            text.push('x');
        }
        let mut rejected = vec![17, 23];
        assert!(
            matches!(write_with_checkpoint(&mut rejected, &response, &mut || Ok(())),
            Err(FrameWriteError::TooLarge {actual, max}) if actual == DAEMON_RESPONSE_MAX_BYTES + 1 && max == DAEMON_RESPONSE_MAX_BYTES)
        );
        assert_eq!(rejected, [17, 23]);
    }

    #[test]
    fn escaped_oversize_counts_every_byte_without_retaining_the_encoded_tail() {
        let response = DaemonResponse::ok(
            "r",
            "agent",
            None,
            serde_json::Value::String("\0".repeat(8192)),
        );
        let expected = serde_json::to_vec(&response).expect("native escaped bytes");
        assert!(expected.len() > 8192 * 5);
        let mut checkpoint = || Ok(());
        let mut buffer = ResponseBuffer::new(1024, &mut checkpoint);
        serde_json::to_writer(&mut buffer, &response).expect("bounded counting");
        assert_eq!(buffer.actual, expected.len());
        assert!(buffer.bytes.is_empty());
        assert!(buffer.failure.is_none());
    }

    #[test]
    fn encoding_cancellation_retains_its_io_kind_without_touching_the_transport() {
        let response = DaemonResponse::ok(
            "r",
            "agent",
            None,
            serde_json::json!({"body": "x".repeat(8192)}),
        );
        for kind in [io::ErrorKind::TimedOut, io::ErrorKind::Interrupted] {
            let mut checks = 0;
            let steps = Cell::new(0);
            let mut writer = SteppedWriter {
                bytes: Vec::new(),
                steps: &steps,
                width: 3,
                flush_error: false,
                flushes: 0,
            };
            // A one-shot checkpoint error must survive serde_json's wrapper,
            // even though the final post-encoding checkpoint succeeds.
            let result = write_with_checkpoint(&mut writer, &response, &mut || {
                checks += 1;
                if checks == 3 {
                    Err(stopped(kind))
                } else {
                    Ok(())
                }
            });
            assert!(matches!(result, Err(FrameWriteError::Io(error)) if error.kind() == kind));
            assert_eq!(checks, 4);
            assert_eq!(steps.get(), 0);
            assert!(writer.bytes.is_empty());
            assert_eq!(writer.flushes, 0);
        }
    }

    #[test]
    fn response_buffer_bounds_retention_independently_of_serializer_chunk_sizes() {
        let input = (0..300).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        for limit in [0, 1, 7, 31, 299, 300] {
            for width in [1, 2, 7, 31, 32, 100, 300] {
                let mut checkpoint = || Ok(());
                let mut buffer = ResponseBuffer::new(limit, &mut checkpoint);
                let mut written = 0;
                for chunk in input.chunks(width) {
                    assert_eq!(buffer.write(chunk).expect("count bytes"), chunk.len());
                    written += chunk.len();
                    assert_eq!(buffer.actual, written);
                    assert!(buffer.bytes.len() <= limit);
                    if written <= limit {
                        assert_eq!(buffer.bytes, input[..written]);
                    } else {
                        assert!(buffer.bytes.is_empty());
                    }
                }
                assert_eq!(buffer.actual, input.len());
            }
        }
    }

    #[test]
    fn response_count_overflow_is_terminal_and_cannot_wrap_into_admission() {
        let mut checkpoint = || Ok(());
        let mut buffer = ResponseBuffer::new(16, &mut checkpoint);
        buffer.actual = usize::MAX;
        assert!(buffer.write(b"x").is_err());
        assert_eq!(buffer.actual, usize::MAX);
        assert_eq!(
            buffer.failure.as_ref().expect("count overflow").kind(),
            io::ErrorKind::InvalidData
        );
        assert!(buffer.bytes.is_empty());
        assert!(buffer.write(b"").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn public_framing_roundtrips_over_real_unix_streams() -> io::Result<()> {
        use std::os::unix::net::UnixStream;
        let (mut client, mut server) = UnixStream::pair()?;
        for socket in [&client, &server] {
            socket.set_read_timeout(Some(Duration::from_secs(1)))?;
            socket.set_write_timeout(Some(Duration::from_secs(1)))?;
        }
        let (request, frame) = request_frame();
        client.write_all(&frame)?;
        assert_eq!(
            super::super::read_request(&mut server).expect("native request"),
            request
        );
        let response = DaemonResponse::ok("frame-budget", "agent", None, request.params);
        super::super::write_response(&mut server, &response).expect("native response");
        let mut prefix = [0; 4];
        client.read_exact(&mut prefix)?;
        let mut body = vec![0; u32::from_be_bytes(prefix) as usize];
        client.read_exact(&mut body)?;
        assert_eq!(
            serde_json::from_slice::<DaemonResponse>(&body).expect("wire response"),
            response
        );
        Ok(())
    }
}
