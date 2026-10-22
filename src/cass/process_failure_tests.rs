//! First-failure precedence at the stdout handoff used by CASS import.
//!
//! Drive the real drain function with deterministic queued events. A view's
//! aggregate-budget callback can fail before the reader observes an oversized
//! physical line. Only the latter is a recoverable session refusal, so cleanup
//! must not replace the callback's earlier fatal diagnosis.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::{
    CASS_STDOUT_DRAIN_BATCH_LINES, CASS_VIEW_STDOUT_LINE_MAX_BYTES, CassError, CassStdoutLine,
    drain_available_stdout_lines,
};
use crate::cass::CassImportError;

type LineEvent = Result<CassStdoutLine, CassError>;

#[derive(Default)]
struct DrainState {
    done: bool,
    line_count: usize,
    bytes_seen: usize,
    peak_line_bytes: usize,
    stream_error: Option<CassError>,
    handler_error: Option<CassImportError>,
    pending_line: Option<LineEvent>,
}

impl DrainState {
    fn drain(
        &mut self,
        receiver: &Receiver<LineEvent>,
        mut handle: impl FnMut(String) -> Result<(), CassImportError>,
    ) -> bool {
        drain_available_stdout_lines(
            receiver,
            &mut self.done,
            &mut self.line_count,
            &mut self.bytes_seen,
            &mut self.peak_line_bytes,
            &mut self.stream_error,
            &mut self.handler_error,
            &mut self.pending_line,
            &mut handle,
        )
    }

    fn drain_counting_handlers(
        &mut self,
        receiver: &Receiver<LineEvent>,
        handled: &mut usize,
    ) -> bool {
        self.drain(receiver, |_| {
            *handled += 1;
            Ok(())
        })
    }
}

fn line(text: &str) -> LineEvent {
    Ok(CassStdoutLine {
        text: text.to_owned(),
        delimiter_bytes: 1,
    })
}

fn view_line_limit() -> CassError {
    CassError::StdoutLineTooLong {
        limit_bytes: CASS_VIEW_STDOUT_LINE_MAX_BYTES,
    }
}

fn aggregate_limit() -> CassImportError {
    // The production view callback returns this typed error. Injecting it at
    // the handoff avoids a 100 MiB allocation and scheduling-dependent race.
    CassImportError::InvalidJson {
        source: "view",
        message: "view JSON output exceeds 104857600 byte limit".to_owned(),
    }
}

fn assert_aggregate_limit(state: &DrainState) {
    assert!(
        matches!(
            state.handler_error.as_ref(),
            Some(CassImportError::InvalidJson { source: "view", message })
                if message == "view JSON output exceeds 104857600 byte limit"
        ),
        "the import callback's fatal aggregate limit must remain the primary error"
    );
    assert!(
        state.stream_error.is_none(),
        "a later recoverable reader error must not outrank the fatal callback error"
    );
}

#[test]
fn callback_aggregate_limit_precedes_later_view_cap_through_pending_cleanup() {
    for first_pending in [false, true] {
        let (sender, receiver) = mpsc::channel();
        let mut state = DrainState::default();
        if first_pending {
            state.pending_line = Some(line("rejected"));
        } else {
            sender.send(line("rejected")).unwrap();
        }
        sender.send(Err(view_line_limit())).unwrap();
        for _ in 0..(2 * CASS_STDOUT_DRAIN_BATCH_LINES + 1) {
            sender.send(line("buffered tail")).unwrap();
        }
        drop(sender);

        let mut handled = 0;
        state.drain(&receiver, |_| {
            handled += 1;
            Err(aggregate_limit())
        });
        assert_aggregate_limit(&state);
        assert_eq!(handled, 1);
        assert_eq!(state.line_count, 1);
        assert_eq!(state.bytes_seen, "rejected\n".len());
        assert!(!state.done);
        assert!(state.pending_line.is_none());

        // Failure must yield before touching the next event. Reinsert that
        // event through the pending slot used by the supervising receive loop.
        let next = receiver
            .try_recv()
            .expect("supervision must regain control before the later view cap");
        assert_eq!(next.as_ref().err(), Some(&view_line_limit()));
        state.pending_line = Some(next);

        assert!(state.drain_counting_handlers(&receiver, &mut handled));
        assert_aggregate_limit(&state);
        assert_eq!(handled, 1);
        assert!(!state.done, "cleanup must remain bounded to one batch");
        assert!(state.pending_line.is_none());

        assert!(state.drain_counting_handlers(&receiver, &mut handled));
        assert_aggregate_limit(&state);
        assert_eq!(handled, 1);
        assert!(!state.done, "a second full batch still leaves a tail");

        assert!(!state.drain_counting_handlers(&receiver, &mut handled));
        assert_aggregate_limit(&state);
        assert_eq!(handled, 1, "cleanup must never invoke the callback");
        assert!(state.done, "cleanup must consume the remaining channel");
        assert!(matches!(
            receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }
}

#[test]
fn accounting_overflow_precedes_later_view_cap_without_partial_stats() {
    for (line_count, bytes_seen, message) in [
        (
            usize::MAX,
            7,
            "cass subprocess stdout line count overflowed",
        ),
        (
            7,
            usize::MAX,
            "cass subprocess stdout byte count overflowed",
        ),
    ] {
        for first_pending in [false, true] {
            let (sender, receiver) = mpsc::channel();
            let mut state = DrainState {
                line_count,
                bytes_seen,
                peak_line_bytes: 3,
                ..DrainState::default()
            };
            if first_pending {
                state.pending_line = Some(line("would overflow"));
            } else {
                sender.send(line("would overflow")).unwrap();
            }
            sender.send(Err(view_line_limit())).unwrap();
            sender.send(line("buffered tail")).unwrap();
            drop(sender);

            let expected = CassError::Io {
                message: message.to_owned(),
            };
            let mut handled = 0;
            state.drain_counting_handlers(&receiver, &mut handled);
            assert_eq!(state.stream_error.as_ref(), Some(&expected));
            assert!(state.handler_error.is_none());
            assert_eq!(handled, 0, "accounting rejects before the callback");
            assert_eq!(state.line_count, line_count);
            assert_eq!(state.bytes_seen, bytes_seen);
            assert_eq!(state.peak_line_bytes, 3);
            assert!(!state.done);

            let next = receiver
                .try_recv()
                .expect("accounting failure must yield before the later reader error");
            assert_eq!(next.as_ref().err(), Some(&view_line_limit()));
            state.pending_line = Some(next);

            assert!(!state.drain_counting_handlers(&receiver, &mut handled));
            assert_eq!(state.stream_error.as_ref(), Some(&expected));
            assert!(state.handler_error.is_none());
            assert_eq!(handled, 0);
            assert!(state.done, "failed accounting must not wedge cleanup");
            assert!(state.pending_line.is_none());
            assert!(matches!(
                receiver.try_recv(),
                Err(TryRecvError::Disconnected)
            ));
        }
    }
}

#[test]
fn first_view_cap_yields_and_later_buffered_events_never_reach_handler() {
    for first_pending in [false, true] {
        let (sender, receiver) = mpsc::channel();
        let mut state = DrainState::default();
        if first_pending {
            state.pending_line = Some(Err(view_line_limit()));
        } else {
            sender.send(Err(view_line_limit())).unwrap();
        }
        for _ in 0..(CASS_STDOUT_DRAIN_BATCH_LINES + 1) {
            sender.send(line("buffered tail")).unwrap();
        }
        sender
            .send(Err(CassError::Io {
                message: "later reader failure".to_owned(),
            }))
            .unwrap();
        drop(sender);

        let expected = view_line_limit();
        let mut handled = 0;
        state.drain_counting_handlers(&receiver, &mut handled);
        assert_eq!(state.stream_error.as_ref(), Some(&expected));
        assert!(state.handler_error.is_none());
        assert_eq!(handled, 0);
        assert_eq!(state.line_count, 0, "yield before accounting a later line");
        assert_eq!(state.bytes_seen, 0);
        assert!(!state.done);

        let next = receiver
            .try_recv()
            .expect("the first buffered line must remain for cleanup");
        assert_eq!(next.as_ref().unwrap().text, "buffered tail");
        state.pending_line = Some(next);

        assert!(state.drain_counting_handlers(&receiver, &mut handled));
        assert_eq!(state.stream_error.as_ref(), Some(&expected));
        assert!(state.handler_error.is_none());
        assert_eq!(handled, 0);
        assert!(!state.done);

        assert!(!state.drain_counting_handlers(&receiver, &mut handled));
        assert_eq!(state.stream_error.as_ref(), Some(&expected));
        assert!(state.handler_error.is_none());
        assert_eq!(handled, 0);
        assert!(state.done);
        assert!(state.pending_line.is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }
}

#[test]
fn successful_pending_line_still_counts_toward_the_finite_drain_batch() {
    let (sender, receiver) = mpsc::channel();
    let mut state = DrainState {
        pending_line: Some(line("x")),
        ..DrainState::default()
    };
    for _ in 0..CASS_STDOUT_DRAIN_BATCH_LINES {
        sender.send(line("x")).unwrap();
    }
    drop(sender);

    let mut handled = 0;
    assert!(state.drain_counting_handlers(&receiver, &mut handled));
    assert_eq!(handled, CASS_STDOUT_DRAIN_BATCH_LINES);
    assert_eq!(state.line_count, CASS_STDOUT_DRAIN_BATCH_LINES);
    assert_eq!(state.bytes_seen, 2 * CASS_STDOUT_DRAIN_BATCH_LINES);
    assert!(!state.done);
    assert!(state.stream_error.is_none());
    assert!(state.handler_error.is_none());
    assert!(state.pending_line.is_none());

    assert!(!state.drain_counting_handlers(&receiver, &mut handled));
    assert_eq!(handled, CASS_STDOUT_DRAIN_BATCH_LINES + 1);
    assert_eq!(state.line_count, CASS_STDOUT_DRAIN_BATCH_LINES + 1);
    assert_eq!(state.bytes_seen, 2 * (CASS_STDOUT_DRAIN_BATCH_LINES + 1));
    assert!(state.done);
    assert!(state.stream_error.is_none());
    assert!(state.handler_error.is_none());
    assert!(matches!(
        receiver.try_recv(),
        Err(TryRecvError::Disconnected)
    ));
}
