//! Production telemetry helpers for graph operations (bd-bife.22).
//!
//! `ee` runs in production at user workloads where the operator
//! needs visibility into which graph algorithms are slow, how often
//! the algorithm-result cache hits, and whether work is hitting the
//! `run_with_budget` timeout. We expose that visibility through
//! well-known `tracing` event targets plus a tiny structured-event
//! API so call sites can emit a stable shape without re-typing field
//! names.
//!
//! ## Decision (per bd-bife.22)
//!
//! - **Tracing events, not Prometheus.** The project already uses
//!   `tracing` + `tracing-subscriber`; we expose well-known target
//!   names at well-known levels and let the operator pipe them into
//!   any exporter they like (`tracing-subscriber::fmt`,
//!   `tracing-opentelemetry`, …).
//! - **Opt-in by level.** Every event respects the live subscriber's
//!   level filter; no extra env knobs are required beyond
//!   `EE_TRACE` / `RUST_LOG`.
//! - **Stable target names.** The constants in this module are the
//!   canonical event-name strings; `docs/observability/graph-telemetry.md`
//!   pins the same names so a query language can match against them
//!   without traversing the source.
//!
//! ## Event taxonomy
//!
//! | Target | Level | When |
//! | --- | --- | --- |
//! | [`SNAPSHOT_REFRESH_EVENT`]    | `info`  | A persisted graph snapshot has just been rebuilt. |
//! | [`ALGORITHM_COMPUTE_EVENT`]   | `debug` | One algorithm invocation completed (either fresh or cache-served). |
//! | [`ALGORITHM_TIMEOUT_EVENT`]   | `warn`  | An algorithm invocation hit its `run_with_budget` ceiling. |
//! | [`ALGORITHM_CANCELLED_EVENT`] | `info`  | An algorithm invocation was cancelled via the `Cx` cancellation path. |
//! | [`CACHE_HIT_EVENT`]           | `trace` | Algorithm-result cache served a request without recomputation. |
//! | [`CACHE_MISS_EVENT`]          | `trace` | Algorithm-result cache missed and recomputation followed. |
//! | [`CACHE_EVICT_EVENT`]         | `debug` | One or more cache rows were evicted by the maintenance path. |
//!
//! All fields use snake_case to match
//! `docs/observability/tracing_field_convention.md`. The emission
//! helpers below take typed parameter structs (`Copy` where the
//! payload allows it) so call sites cannot accidentally swap two
//! `usize` arguments at the call site.
//!
//! ## Wiring boundary
//!
//! This module is intentionally call-site-agnostic: it defines the
//! constants and the emission helpers but does not insert them into
//! the F2 wrappers or F1 snapshot refresh. Routing the helpers into
//! the live algorithm/snapshot paths is tracked separately so this
//! slice can land clean against an uncontested file boundary.

/// `tracing` event target emitted when a persisted graph snapshot
/// has just been rebuilt (whether the build was triggered manually
/// via `ee graph snapshot refresh` or automatically by a steward
/// job). INFO level.
pub const SNAPSHOT_REFRESH_EVENT: &str = "ee.graph.snapshot.refresh";

/// Emitted at the end of every algorithm invocation, whether the
/// result came from a fresh compute or the result cache. DEBUG
/// level so heavy workloads do not flood operator logs at default
/// filters.
pub const ALGORITHM_COMPUTE_EVENT: &str = "ee.graph.algorithm.compute";

/// Emitted when an algorithm invocation hits its `run_with_budget`
/// ceiling and is terminated with a timeout outcome. WARN level
/// since this represents user-visible degradation.
pub const ALGORITHM_TIMEOUT_EVENT: &str = "ee.graph.algorithm.timeout";

/// Emitted when an algorithm invocation is cancelled via the `Cx`
/// cancellation path (typically caller-driven, e.g. SIGINT during
/// `ee context`). INFO level — cancellation is expected behavior,
/// not a fault.
pub const ALGORITHM_CANCELLED_EVENT: &str = "ee.graph.algorithm.cancelled";

/// Emitted when the algorithm-result cache served a request without
/// recomputation. TRACE level — fires on every hot-path read and
/// would otherwise dominate operator logs.
pub const CACHE_HIT_EVENT: &str = "ee.graph.cache.hit";

/// Emitted when the algorithm-result cache missed and the algorithm
/// recomputed. TRACE level (paired with `CACHE_HIT_EVENT` so an
/// operator querying both can compute the hit rate without sampling
/// bias).
pub const CACHE_MISS_EVENT: &str = "ee.graph.cache.miss";

/// Emitted when one or more cache rows are evicted by the
/// maintenance path. DEBUG level so an operator can see the
/// background churn without enabling TRACE.
pub const CACHE_EVICT_EVENT: &str = "ee.graph.cache.evict";

/// Canonical list of every telemetry event target this module
/// defines. The documentation generator and the subscriber-test
/// fixture both walk this slice so new events automatically gain
/// coverage; nothing else needs to be re-edited.
pub const ALL_GRAPH_TELEMETRY_EVENTS: &[&str] = &[
    SNAPSHOT_REFRESH_EVENT,
    ALGORITHM_COMPUTE_EVENT,
    ALGORITHM_TIMEOUT_EVENT,
    ALGORITHM_CANCELLED_EVENT,
    CACHE_HIT_EVENT,
    CACHE_MISS_EVENT,
    CACHE_EVICT_EVENT,
];

/// Reason a cache eviction fired. Mirrored as the `reason` field on
/// [`CACHE_EVICT_EVENT`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheEvictReason {
    /// Row aged past the configured retention TTL.
    TtlExpired,
    /// The snapshot the row was tied to has been archived.
    SnapshotArchived,
    /// Operator invoked the maintenance command explicitly.
    OperatorRequest,
}

impl CacheEvictReason {
    /// Stable wire string used in the emitted `reason` field. Lower
    /// snake_case so log-query languages can pattern-match without
    /// case folding.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TtlExpired => "ttl_expired",
            Self::SnapshotArchived => "snapshot_archived",
            Self::OperatorRequest => "operator_request",
        }
    }
}

/// Snapshot-refresh event payload (target [`SNAPSHOT_REFRESH_EVENT`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotRefreshEvent<'a> {
    pub graph_type: &'a str,
    pub snapshot_version: u64,
    pub build_ms: u64,
    pub node_count: usize,
    pub edge_count: usize,
    pub lock_wait_ms: u64,
}

/// Per-invocation algorithm-compute event payload (target
/// [`ALGORITHM_COMPUTE_EVENT`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlgorithmComputeEvent<'a> {
    pub algorithm: &'a str,
    pub snapshot_id: &'a str,
    pub params_hash: &'a str,
    pub elapsed_ms: u64,
    pub cache_hit: bool,
    pub sampling_used: bool,
}

/// Algorithm-timeout event payload (target [`ALGORITHM_TIMEOUT_EVENT`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlgorithmTimeoutEvent<'a> {
    pub algorithm: &'a str,
    pub snapshot_id: &'a str,
    pub budget_ms: u64,
    pub elapsed_ms: u64,
}

/// Algorithm-cancellation event payload (target
/// [`ALGORITHM_CANCELLED_EVENT`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlgorithmCancelledEvent<'a> {
    pub algorithm: &'a str,
    pub elapsed_ms: u64,
}

/// Cache hit/miss event payload (targets [`CACHE_HIT_EVENT`] /
/// [`CACHE_MISS_EVENT`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheOutcomeEvent<'a> {
    pub algorithm: &'a str,
    pub params_hash: &'a str,
}

/// Cache-eviction event payload (target [`CACHE_EVICT_EVENT`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheEvictEvent {
    pub reason: CacheEvictReason,
    pub count: u32,
}

/// Emit [`SNAPSHOT_REFRESH_EVENT`] at INFO level.
pub fn emit_snapshot_refresh(event: SnapshotRefreshEvent<'_>) {
    tracing::info!(
        target: SNAPSHOT_REFRESH_EVENT,
        graph_type = event.graph_type,
        snapshot_version = event.snapshot_version,
        build_ms = event.build_ms,
        node_count = event.node_count,
        edge_count = event.edge_count,
        lock_wait_ms = event.lock_wait_ms,
    );
}

/// Emit [`ALGORITHM_COMPUTE_EVENT`] at DEBUG level.
pub fn emit_algorithm_compute(event: AlgorithmComputeEvent<'_>) {
    tracing::debug!(
        target: ALGORITHM_COMPUTE_EVENT,
        algorithm = event.algorithm,
        snapshot_id = event.snapshot_id,
        params_hash = event.params_hash,
        elapsed_ms = event.elapsed_ms,
        cache_hit = event.cache_hit,
        sampling_used = event.sampling_used,
    );
}

/// Emit [`ALGORITHM_TIMEOUT_EVENT`] at WARN level.
pub fn emit_algorithm_timeout(event: AlgorithmTimeoutEvent<'_>) {
    tracing::warn!(
        target: ALGORITHM_TIMEOUT_EVENT,
        algorithm = event.algorithm,
        snapshot_id = event.snapshot_id,
        budget_ms = event.budget_ms,
        elapsed_ms = event.elapsed_ms,
    );
}

/// Emit [`ALGORITHM_CANCELLED_EVENT`] at INFO level.
pub fn emit_algorithm_cancelled(event: AlgorithmCancelledEvent<'_>) {
    tracing::info!(
        target: ALGORITHM_CANCELLED_EVENT,
        algorithm = event.algorithm,
        elapsed_ms = event.elapsed_ms,
    );
}

/// Emit [`CACHE_HIT_EVENT`] at TRACE level.
pub fn emit_cache_hit(event: CacheOutcomeEvent<'_>) {
    tracing::trace!(
        target: CACHE_HIT_EVENT,
        algorithm = event.algorithm,
        params_hash = event.params_hash,
    );
}

/// Emit [`CACHE_MISS_EVENT`] at TRACE level.
pub fn emit_cache_miss(event: CacheOutcomeEvent<'_>) {
    tracing::trace!(
        target: CACHE_MISS_EVENT,
        algorithm = event.algorithm,
        params_hash = event.params_hash,
    );
}

/// Emit [`CACHE_EVICT_EVENT`] at DEBUG level.
pub fn emit_cache_evict(event: CacheEvictEvent) {
    tracing::debug!(
        target: CACHE_EVICT_EVENT,
        reason = event.reason.as_str(),
        count = event.count,
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::Level;
    use tracing::subscriber::with_default;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::registry::Registry;

    use super::*;

    #[derive(Default)]
    struct CapturedEvent {
        target: String,
        level: Level,
        fields: std::collections::BTreeMap<String, String>,
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
        fields: &'a mut std::collections::BTreeMap<String, String>,
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
        // TRACE filter so even the trace-level cache events show up.
        let subscriber = Registry::default()
            .with(layer)
            .with(tracing_subscriber::filter::LevelFilter::TRACE);
        with_default(subscriber, thunk);
        let guard = events.lock().expect("capture lock");
        guard.iter().map(clone_captured).collect()
    }

    fn clone_captured(captured: &CapturedEvent) -> CapturedEvent {
        CapturedEvent {
            target: captured.target.clone(),
            level: captured.level,
            fields: captured.fields.clone(),
        }
    }

    fn find<'a>(events: &'a [CapturedEvent], target: &str) -> Option<&'a CapturedEvent> {
        events.iter().find(|e| e.target == target)
    }

    /// Every constant in `ALL_GRAPH_TELEMETRY_EVENTS` carries the
    /// `ee.graph.` prefix, so a query-language filter
    /// `target ~= '^ee\.graph\.'` reliably scopes to graph
    /// telemetry without per-event aliases.
    #[test]
    fn all_event_targets_share_the_ee_graph_prefix() {
        for event in ALL_GRAPH_TELEMETRY_EVENTS {
            assert!(
                event.starts_with("ee.graph."),
                "{event} must start with ee.graph."
            );
        }
        assert_eq!(
            ALL_GRAPH_TELEMETRY_EVENTS.len(),
            7,
            "bd-bife.22 declares seven graph telemetry events"
        );
    }

    /// Every event name in the canonical list is unique — duplicates
    /// would silently merge in any query engine.
    #[test]
    fn all_event_targets_are_unique() {
        let mut seen: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
        for event in ALL_GRAPH_TELEMETRY_EVENTS {
            assert!(seen.insert(event), "duplicate event target {event}");
        }
    }

    #[test]
    fn snapshot_refresh_emits_documented_fields_at_info_level() {
        let events = capture(|| {
            emit_snapshot_refresh(SnapshotRefreshEvent {
                graph_type: "memory_links",
                snapshot_version: 42,
                build_ms: 18,
                node_count: 14_000,
                edge_count: 35_000,
                lock_wait_ms: 3,
            });
        });
        let event = find(&events, SNAPSHOT_REFRESH_EVENT).expect("snapshot refresh emitted");
        assert_eq!(event.level, Level::INFO);
        assert_eq!(
            event.fields.get("graph_type").map(String::as_str),
            Some("memory_links")
        );
        assert_eq!(
            event.fields.get("snapshot_version").map(String::as_str),
            Some("42")
        );
        assert_eq!(event.fields.get("build_ms").map(String::as_str), Some("18"));
        assert_eq!(
            event.fields.get("node_count").map(String::as_str),
            Some("14000")
        );
        assert_eq!(
            event.fields.get("edge_count").map(String::as_str),
            Some("35000")
        );
        assert_eq!(
            event.fields.get("lock_wait_ms").map(String::as_str),
            Some("3")
        );
    }

    #[test]
    fn algorithm_compute_emits_at_debug_level_with_cache_hit_flag() {
        let events = capture(|| {
            emit_algorithm_compute(AlgorithmComputeEvent {
                algorithm: "personalized_pagerank",
                snapshot_id: "snap_01HX",
                params_hash: "blake3:abcd",
                elapsed_ms: 27,
                cache_hit: true,
                sampling_used: false,
            });
        });
        let event = find(&events, ALGORITHM_COMPUTE_EVENT).expect("algorithm compute emitted");
        assert_eq!(event.level, Level::DEBUG);
        assert_eq!(
            event.fields.get("algorithm").map(String::as_str),
            Some("personalized_pagerank")
        );
        assert_eq!(
            event.fields.get("snapshot_id").map(String::as_str),
            Some("snap_01HX")
        );
        assert_eq!(
            event.fields.get("params_hash").map(String::as_str),
            Some("blake3:abcd")
        );
        assert_eq!(
            event.fields.get("elapsed_ms").map(String::as_str),
            Some("27")
        );
        assert_eq!(
            event.fields.get("cache_hit").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            event.fields.get("sampling_used").map(String::as_str),
            Some("false")
        );
    }

    #[test]
    fn algorithm_timeout_emits_at_warn_level() {
        let events = capture(|| {
            emit_algorithm_timeout(AlgorithmTimeoutEvent {
                algorithm: "louvain",
                snapshot_id: "snap_01HY",
                budget_ms: 200,
                elapsed_ms: 210,
            });
        });
        let event = find(&events, ALGORITHM_TIMEOUT_EVENT).expect("algorithm timeout emitted");
        assert_eq!(event.level, Level::WARN);
        assert_eq!(
            event.fields.get("algorithm").map(String::as_str),
            Some("louvain")
        );
        assert_eq!(
            event.fields.get("budget_ms").map(String::as_str),
            Some("200")
        );
        assert_eq!(
            event.fields.get("elapsed_ms").map(String::as_str),
            Some("210")
        );
    }

    #[test]
    fn algorithm_cancelled_emits_at_info_level() {
        let events = capture(|| {
            emit_algorithm_cancelled(AlgorithmCancelledEvent {
                algorithm: "betweenness",
                elapsed_ms: 9,
            });
        });
        let event = find(&events, ALGORITHM_CANCELLED_EVENT).expect("algorithm cancelled emitted");
        assert_eq!(event.level, Level::INFO);
        assert_eq!(
            event.fields.get("algorithm").map(String::as_str),
            Some("betweenness")
        );
        assert_eq!(
            event.fields.get("elapsed_ms").map(String::as_str),
            Some("9")
        );
    }

    #[test]
    fn cache_hit_and_miss_both_emit_at_trace_level() {
        let events = capture(|| {
            emit_cache_hit(CacheOutcomeEvent {
                algorithm: "ppr",
                params_hash: "blake3:cafe",
            });
            emit_cache_miss(CacheOutcomeEvent {
                algorithm: "ppr",
                params_hash: "blake3:beef",
            });
        });
        let hit = find(&events, CACHE_HIT_EVENT).expect("cache hit emitted");
        let miss = find(&events, CACHE_MISS_EVENT).expect("cache miss emitted");
        assert_eq!(hit.level, Level::TRACE);
        assert_eq!(miss.level, Level::TRACE);
        assert_eq!(
            hit.fields.get("params_hash").map(String::as_str),
            Some("blake3:cafe")
        );
        assert_eq!(
            miss.fields.get("params_hash").map(String::as_str),
            Some("blake3:beef")
        );
    }

    #[test]
    fn cache_evict_emits_at_debug_level_with_wire_reason_string() {
        let events = capture(|| {
            emit_cache_evict(CacheEvictEvent {
                reason: CacheEvictReason::TtlExpired,
                count: 12,
            });
            emit_cache_evict(CacheEvictEvent {
                reason: CacheEvictReason::SnapshotArchived,
                count: 1,
            });
            emit_cache_evict(CacheEvictEvent {
                reason: CacheEvictReason::OperatorRequest,
                count: 47,
            });
        });
        let evicts: Vec<_> = events
            .iter()
            .filter(|e| e.target == CACHE_EVICT_EVENT)
            .collect();
        assert_eq!(evicts.len(), 3);
        for evict in &evicts {
            assert_eq!(evict.level, Level::DEBUG);
        }
        assert_eq!(
            evicts[0].fields.get("reason").map(String::as_str),
            Some("ttl_expired")
        );
        assert_eq!(
            evicts[0].fields.get("count").map(String::as_str),
            Some("12")
        );
        assert_eq!(
            evicts[1].fields.get("reason").map(String::as_str),
            Some("snapshot_archived")
        );
        assert_eq!(
            evicts[2].fields.get("reason").map(String::as_str),
            Some("operator_request")
        );
    }

    /// Subscriber-coverage proof: a single `with_default` scope that
    /// fires every one of the seven canonical events sees all of
    /// them by target name. This is the load-bearing verification
    /// the bead asks for ("Subscriber test green").
    #[test]
    fn subscriber_can_capture_every_canonical_graph_telemetry_event() {
        let events = capture(|| {
            emit_snapshot_refresh(SnapshotRefreshEvent {
                graph_type: "memory_links",
                snapshot_version: 1,
                build_ms: 1,
                node_count: 1,
                edge_count: 1,
                lock_wait_ms: 0,
            });
            emit_algorithm_compute(AlgorithmComputeEvent {
                algorithm: "a",
                snapshot_id: "s",
                params_hash: "p",
                elapsed_ms: 1,
                cache_hit: false,
                sampling_used: false,
            });
            emit_algorithm_timeout(AlgorithmTimeoutEvent {
                algorithm: "a",
                snapshot_id: "s",
                budget_ms: 1,
                elapsed_ms: 2,
            });
            emit_algorithm_cancelled(AlgorithmCancelledEvent {
                algorithm: "a",
                elapsed_ms: 1,
            });
            emit_cache_hit(CacheOutcomeEvent {
                algorithm: "a",
                params_hash: "p",
            });
            emit_cache_miss(CacheOutcomeEvent {
                algorithm: "a",
                params_hash: "p",
            });
            emit_cache_evict(CacheEvictEvent {
                reason: CacheEvictReason::TtlExpired,
                count: 1,
            });
        });
        let observed: std::collections::BTreeSet<&str> =
            events.iter().map(|e| e.target.as_str()).collect();
        for event in ALL_GRAPH_TELEMETRY_EVENTS {
            assert!(
                observed.contains(event),
                "missing event target {event} in captured set: {observed:?}"
            );
        }
    }

    /// Each `CacheEvictReason::as_str` value is unique and matches
    /// the documented wire format (lower snake_case, no whitespace).
    #[test]
    fn cache_evict_reason_wire_strings_are_unique_and_snake_case() {
        let strings = [
            CacheEvictReason::TtlExpired.as_str(),
            CacheEvictReason::SnapshotArchived.as_str(),
            CacheEvictReason::OperatorRequest.as_str(),
        ];
        for string in strings {
            assert!(
                string.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{string} must be lower snake_case"
            );
            assert!(!string.is_empty());
        }
        let unique: std::collections::BTreeSet<&str> = strings.iter().copied().collect();
        assert_eq!(unique.len(), strings.len(), "wire strings must be unique");
    }
}
