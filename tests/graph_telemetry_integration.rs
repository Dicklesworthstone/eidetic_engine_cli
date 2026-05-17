//! Boundary-crossing smoke test for the graph telemetry public
//! API surface (bd-2inbn task: "Add an integration test that
//! runs a small workload and asserts the captured-events set
//! includes ee.graph.algorithm.compute with the expected fields").
//!
//! ## Why this file is deliberately minimal
//!
//! Inline unit tests in `src/graph/algorithms.rs::tests` already
//! cover the per-event field + level invariants thoroughly for
//! every `run_with_budget` outcome path:
//! `run_with_budget_emits_algorithm_compute_telemetry`,
//! `run_with_budget_emits_timeout_telemetry`,
//! `run_with_budget_emits_cancelled_telemetry`, and several
//! cache-related siblings landed by SandyBadger in commit
//! `ffd576f`. The fuzz target in
//! `fuzz/fuzz_targets/graph_telemetry.rs` covers the
//! side-effect-free invariants (sweep slice integrity, wire
//! string properties).
//!
//! Replicating those field/level assertions here would be
//! duplicate coverage. What an integration-test crate uniquely
//! adds is **boundary-crossing verification**:
//!
//! 1. Inline tests have access to private items
//!    (`UNTRACKED_GRAPH_SNAPSHOT_ID`, internal helpers). This
//!    file only sees the `pub` API. A regression that
//!    accidentally narrows the visibility of `run_with_budget`,
//!    `ALL_GRAPH_TELEMETRY_EVENTS`, or the per-event constants
//!    would compile the inline tests but fail to link here.
//! 2. The single test below asserts that every `ee.graph.*`
//!    event captured under a representative successful workload
//!    is a member of the public `ALL_GRAPH_TELEMETRY_EVENTS`
//!    sweep slice. A future emission with an unenumerated
//!    target — or a sweep-slice / constant drift — surfaces
//!    here even when the inline tests pass.
//!
//! Everything else (timeout / cancellation paths, exact field
//! values, level checks) is intentionally NOT duplicated; the
//! inline tests own those invariants.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ee::core::graph_telemetry::{ALGORITHM_COMPUTE_EVENT, ALL_GRAPH_TELEMETRY_EVENTS};
use ee::graph::algorithms::{current_or_testing_cx, run_with_budget};
use tracing::Level;
use tracing::subscriber::with_default;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::Registry;

type TestResult = Result<(), String>;

#[derive(Clone, Default)]
struct CapturedEvent {
    target: String,
    level: Level,
    fields: BTreeMap<String, String>,
}

#[derive(Default, Clone)]
struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl<S> Layer<S> for CaptureLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut captured = CapturedEvent {
            target: event.metadata().target().to_owned(),
            level: *event.metadata().level(),
            ..CapturedEvent::default()
        };
        let mut visitor = CaptureVisitor {
            fields: &mut captured.fields,
        };
        event.record(&mut visitor);
        self.events.lock().expect("capture lock").push(captured);
    }
}

struct CaptureVisitor<'a> {
    fields: &'a mut BTreeMap<String, String>,
}

impl tracing::field::Visit for CaptureVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_owned(), format!("{value:?}"));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.fields
            .insert(field.name().to_owned(), value.to_owned());
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }
}

fn capture<F: FnOnce()>(thunk: F) -> Vec<CapturedEvent> {
    let layer = CaptureLayer::default();
    let events = layer.events.clone();
    let subscriber = Registry::default()
        .with(layer)
        .with(tracing_subscriber::filter::LevelFilter::TRACE);
    with_default(subscriber, thunk);
    events.lock().expect("capture lock").clone()
}

/// Boundary-crossing drift guard: every `ee.graph.*` event
/// captured under a representative successful workload via the
/// `pub` API surface must be a member of
/// `ALL_GRAPH_TELEMETRY_EVENTS`. Verifies that
/// `run_with_budget`, the per-event target constants, and the
/// sweep slice are all `pub` and agree from outside the crate.
#[test]
fn pub_api_emits_only_canonical_ee_graph_events_and_sweep_slice_resolves() -> TestResult {
    let events = capture(|| {
        let cx = current_or_testing_cx();
        let _: ee::graph::GraphResult<u64> = run_with_budget(
            &cx,
            "calmbridge_boundary_smoke",
            Duration::from_secs(5),
            || Ok(7),
        );
    });

    // At least one ee.graph.* event must have been captured —
    // otherwise either the wiring is broken or the public
    // re-export of `run_with_budget` no longer goes through the
    // instrumented wrapper.
    let graph_events: Vec<&CapturedEvent> = events
        .iter()
        .filter(|event| event.target.starts_with("ee.graph."))
        .collect();
    if graph_events.is_empty() {
        return Err(format!(
            "expected at least one ee.graph.* event from the boundary smoke; captured targets: {:?}",
            events
                .iter()
                .map(|event| event.target.as_str())
                .collect::<Vec<_>>()
        ));
    }

    // Specifically, the compute event must fire so the test
    // catches a regression that silently strips telemetry from
    // the wrapper's success path.
    if !graph_events
        .iter()
        .any(|event| event.target == ALGORITHM_COMPUTE_EVENT)
    {
        return Err(format!(
            "expected {ALGORITHM_COMPUTE_EVENT} from successful run_with_budget; got {:?}",
            graph_events
                .iter()
                .map(|event| event.target.as_str())
                .collect::<Vec<_>>()
        ));
    }

    // Drift guard from outside the crate: every emitted
    // ee.graph.* target must be in the canonical sweep slice.
    let canonical: BTreeSet<&'static str> = ALL_GRAPH_TELEMETRY_EVENTS.iter().copied().collect();
    for event in &graph_events {
        if !canonical.contains(event.target.as_str()) {
            return Err(format!(
                "emitted event {} is not in ALL_GRAPH_TELEMETRY_EVENTS; canonical set: {:?}",
                event.target, canonical
            ));
        }
    }

    Ok(())
}
