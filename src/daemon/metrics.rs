//! Metrics-collection seam for the daemon dispatch path (bd-3vkyp).
//!
//! `dispatch` is an allocation-free `match` whose whole frame-read →
//! dispatch → frame-write cycle runs in microseconds for
//! `ee.daemon.echo` — that hot path IS the daemon's value-add over
//! cold-start (bd-ob21s). The risk this module forecloses: the
//! "natural" way to add per-method counters/histograms later is to
//! sprinkle `tracing::info!` / `metrics::histogram!` inline at the
//! entry and exit of every dispatch arm. Each such call costs an event
//! allocation, a timestamp read, a formatter call, and a subscriber
//! filter check — ~1-5µs that, at 10k RPC/s, eats tens of ms/s of the
//! steady-state hot path and shrinks the cold-start amortization
//! window the daemon exists to provide.
//!
//! Instead, observability enters through ONE seam: a
//! [`DaemonMetricsCollector`] trait threaded around dispatch via
//! [`instrument_dispatch`]. A release build wires the zero-cost
//! [`NoopMetricsCollector`] (its trait body is empty, so a concrete
//! call monomorphizes to nothing); a perf-investigation build swaps in
//! a real collector (Prometheus, an in-process histogram, a 1-in-N
//! sampler) at the construction site WITHOUT touching `dispatch` or any
//! match arm. Adding a new per-method counter then never requires a
//! hot-path recompile of the dispatch table.
//!
//! This module ships the seam only; `dispatch` itself stays a pure
//! `&DaemonRequest -> DaemonResponse` function so the unit tests that
//! pin the dispatch table keep exercising it without a collector. The
//! live accept path routes through [`instrument_dispatch`] with a
//! [`NoopMetricsCollector`], so a future collector plugs in at that one
//! call site.

use std::time::{Duration, Instant};

use super::protocol::DaemonResponse;

/// Classification of a dispatch result, recorded alongside the method
/// name and elapsed time. Kept deliberately coarse (the wire envelope's
/// `error.code` carries the specific failure code) so a collector can
/// maintain a small fixed set of per-outcome series without a
/// per-error-code cardinality explosion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchOutcome {
    /// The response carried a `result` and no `error`.
    Success,
    /// The response carried a structured `error` envelope (unknown
    /// method, schema mismatch, decode failure, handler panic, …).
    Error,
    /// The response succeeded but flagged one or more `degraded_codes`
    /// (e.g. the `ee.daemon.context` warm-load stub). Distinguished
    /// from `Success` so a collector can alert on a rising degraded
    /// rate without conflating it with hard errors.
    Degraded,
}

impl DispatchOutcome {
    /// Derive the coarse outcome from a finished [`DaemonResponse`].
    /// An `error` envelope dominates (it is always `Error`); otherwise a
    /// non-empty `degraded_codes` list is `Degraded`; otherwise
    /// `Success`.
    #[must_use]
    pub fn from_response(response: &DaemonResponse) -> Self {
        if response.error.is_some() {
            Self::Error
        } else if response.degraded_codes.is_empty() {
            Self::Success
        } else {
            Self::Degraded
        }
    }

    /// Stable lowercase label for a metrics series / log field. Never
    /// changes once shipped (collectors key series off it).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Degraded => "degraded",
        }
    }
}

/// Sink for per-dispatch observability. Implementors record one sample
/// per dispatched request: the method name, the coarse
/// [`DispatchOutcome`], and the wall-clock time spent inside dispatch.
///
/// `record_dispatch` runs on the hot path, so implementors MUST keep it
/// cheap and non-blocking (atomic increments, lock-free histograms, or
/// a bounded channel send) — never synchronous I/O. A sampler that
/// records only 1-in-N calls belongs INSIDE the implementor, not in the
/// dispatch path.
pub trait DaemonMetricsCollector: Send + Sync {
    /// Record one completed dispatch.
    fn record_dispatch(&self, method: &str, outcome: DispatchOutcome, elapsed: Duration);
}

/// Zero-cost collector wired into release builds. Every method body is
/// empty, so a monomorphized call compiles to nothing and a `&dyn`
/// call is a single (predicted) vtable hop to a `ret`. This is the
/// default the live accept path passes so the seam is always present
/// without adding steady-state cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopMetricsCollector;

impl DaemonMetricsCollector for NoopMetricsCollector {
    #[inline]
    fn record_dispatch(&self, _method: &str, _outcome: DispatchOutcome, _elapsed: Duration) {}
}

/// Time a dispatch call and report it to `collector`, returning the
/// response unchanged. This is the single seam the live accept path
/// uses: `instrument_dispatch(&req.method, collector, || dispatch(&req))`.
/// Swapping `collector` from [`NoopMetricsCollector`] to a real sink is
/// the entire change needed to turn on per-method metrics — no edit to
/// `dispatch` or its match arms, hence no hot-path recompile of the
/// dispatch table.
pub fn instrument_dispatch<F>(
    method: &str,
    collector: &dyn DaemonMetricsCollector,
    dispatch_fn: F,
) -> DaemonResponse
where
    F: FnOnce() -> DaemonResponse,
{
    let start = Instant::now();
    let response = dispatch_fn();
    let elapsed = start.elapsed();
    collector.record_dispatch(method, DispatchOutcome::from_response(&response), elapsed);
    response
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::super::protocol::{DaemonResponse, DaemonResponseError};
    use super::{
        DaemonMetricsCollector, DispatchOutcome, NoopMetricsCollector, instrument_dispatch,
    };

    /// Test collector that captures every recorded sample so the seam's
    /// behavior can be asserted without a real metrics backend.
    #[derive(Default)]
    struct CapturingCollector {
        samples: Mutex<Vec<(String, DispatchOutcome)>>,
    }

    impl DaemonMetricsCollector for CapturingCollector {
        fn record_dispatch(&self, method: &str, outcome: DispatchOutcome, _elapsed: Duration) {
            self.samples
                .lock()
                .expect("capturing collector mutex must not be poisoned")
                .push((method.to_owned(), outcome));
        }
    }

    fn ok_response() -> DaemonResponse {
        DaemonResponse::ok(
            "req-1",
            "agent-metrics-test",
            None,
            serde_json::json!({"ok": true}),
        )
    }

    fn error_response() -> DaemonResponse {
        DaemonResponse {
            schema: super::super::DAEMON_RESPONSE_SCHEMA_V1.to_owned(),
            request_id: "req-2".to_owned(),
            agent_id: "agent-metrics-test".to_owned(),
            workspace_id: None,
            result: None,
            error: Some(DaemonResponseError {
                code: "daemon_unknown_method".to_owned(),
                message: "nope".to_owned(),
            }),
            degraded_codes: Vec::new(),
        }
    }

    fn degraded_response() -> DaemonResponse {
        DaemonResponse::ok("req-3", "agent-metrics-test", None, serde_json::Value::Null)
            .with_degraded("daemon_overloaded")
    }

    #[test]
    fn outcome_classifies_success_error_and_degraded() {
        assert_eq!(
            DispatchOutcome::from_response(&ok_response()),
            DispatchOutcome::Success
        );
        assert_eq!(
            DispatchOutcome::from_response(&error_response()),
            DispatchOutcome::Error
        );
        assert_eq!(
            DispatchOutcome::from_response(&degraded_response()),
            DispatchOutcome::Degraded
        );
    }

    #[test]
    fn error_dominates_even_when_degraded_codes_present() {
        // A response that somehow carried both an error and a degraded
        // code is classified Error — the hard failure is the headline.
        let mut response = error_response();
        response.degraded_codes.push("daemon_overloaded".to_owned());
        assert_eq!(
            DispatchOutcome::from_response(&response),
            DispatchOutcome::Error
        );
    }

    #[test]
    fn instrument_dispatch_records_method_and_outcome_and_returns_response() {
        let collector = CapturingCollector::default();
        let response = instrument_dispatch("ee.daemon.echo", &collector, ok_response);
        // The response is passed through unchanged.
        assert_eq!(response.request_id, "req-1");
        assert!(response.error.is_none());
        // Exactly one sample was recorded, with the right method + outcome.
        let samples = collector.samples.lock().unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].0, "ee.daemon.echo");
        assert_eq!(samples[0].1, DispatchOutcome::Success);
    }

    #[test]
    fn noop_collector_records_nothing_observable_and_passes_response_through() {
        // The Noop path must not alter the response and must be safe to
        // call on the hot path. We can only assert the response is
        // returned unchanged (the Noop body is intentionally empty).
        let response = instrument_dispatch(
            "ee.daemon.context",
            &NoopMetricsCollector,
            degraded_response,
        );
        assert_eq!(response.request_id, "req-3");
        assert!(
            response
                .degraded_codes
                .contains(&"daemon_overloaded".to_owned())
        );
    }

    #[test]
    fn outcome_labels_are_stable() {
        assert_eq!(DispatchOutcome::Success.label(), "success");
        assert_eq!(DispatchOutcome::Error.label(), "error");
        assert_eq!(DispatchOutcome::Degraded.label(), "degraded");
    }
}
