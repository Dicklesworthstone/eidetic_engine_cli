//! Audit-lane public contract constants and in-process queue primitives.
//!
//! The database writer and foreground call-site integration land in later
//! `bd-wp5ac` slices. This module owns the bounded producer lane and drain
//! semantics that those call sites will use.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use asupersync::channel::mpsc::{self, RecvError, SendError};

use crate::db::{CreateAuditInput, DbConnection};

pub const AUDIT_LANE_SCHEMA_V1: &str = "ee.audit_lane.v1";
pub const AUDIT_LANE_SOURCE_LABEL: &str = "audit_lane";

pub const AUDIT_BACKPRESSURE_CODE: &str = "audit_backpressure";
pub const AUDIT_LANE_SHUTDOWN_DRAIN_TIMEOUT_CODE: &str = "audit_lane_shutdown_drain_timeout";

const DEFAULT_AUDIT_LANE_CAPACITY: usize = 1024;
const DEFAULT_AUDIT_LANE_BATCH_SIZE: usize = 64;
const DEFAULT_AUDIT_LANE_SHUTDOWN_EVENT_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditLanePhase {
    Enqueue,
    Drain,
    BatchCommit,
    Shutdown,
    Backpressure,
}

impl AuditLanePhase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enqueue => "enqueue",
            Self::Drain => "drain",
            Self::BatchCommit => "batch_commit",
            Self::Shutdown => "shutdown",
            Self::Backpressure => "backpressure",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Enqueue,
            Self::Drain,
            Self::BatchCommit,
            Self::Shutdown,
            Self::Backpressure,
        ]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditEvent {
    pub audit_id: String,
    pub workspace_id: String,
    pub actor: Option<String>,
    pub request_id: Option<String>,
    pub audit_seq: u64,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub details: Option<String>,
}

impl AuditEvent {
    #[must_use]
    pub fn new(workspace_id: impl Into<String>, audit_seq: u64, action: impl Into<String>) -> Self {
        Self {
            audit_id: format!("audit_lane_{audit_seq:020}"),
            workspace_id: workspace_id.into(),
            actor: None,
            request_id: None,
            audit_seq,
            action: action.into(),
            target_type: None,
            target_id: None,
            details: None,
        }
    }

    #[must_use]
    pub fn from_audit_input(
        audit_id: impl Into<String>,
        audit_seq: u64,
        input: &CreateAuditInput,
    ) -> Self {
        Self {
            audit_id: audit_id.into(),
            workspace_id: input.workspace_id.clone().unwrap_or_default(),
            actor: input.actor.clone(),
            request_id: None,
            audit_seq,
            action: input.action.clone(),
            target_type: input.target_type.clone(),
            target_id: input.target_id.clone(),
            details: input.details.clone(),
        }
    }

    #[must_use]
    pub fn to_audit_input(&self) -> CreateAuditInput {
        CreateAuditInput {
            workspace_id: (!self.workspace_id.is_empty()).then(|| self.workspace_id.clone()),
            actor: self.actor.clone(),
            action: self.action.clone(),
            target_type: self.target_type.clone(),
            target_id: self.target_id.clone(),
            details: self.details.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuditLaneConfig {
    pub capacity: usize,
    pub batch_size: usize,
    pub shutdown_event_limit: usize,
}

impl Default for AuditLaneConfig {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_AUDIT_LANE_CAPACITY,
            batch_size: DEFAULT_AUDIT_LANE_BATCH_SIZE,
            shutdown_event_limit: DEFAULT_AUDIT_LANE_SHUTDOWN_EVENT_LIMIT,
        }
    }
}

impl AuditLaneConfig {
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            capacity: self.capacity.max(1),
            batch_size: self.batch_size.max(1),
            shutdown_event_limit: self.shutdown_event_limit,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditEnqueueResult {
    Enqueued {
        audit_seq: u64,
        pending_events: u64,
    },
    Backpressure {
        code: &'static str,
        event: AuditEvent,
        capacity: usize,
        pending_events: u64,
    },
    Closed {
        event: AuditEvent,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditLaneDrainReport {
    pub phase: AuditLanePhase,
    pub drained_events: u64,
    pub batches: u64,
    pub pending_events: u64,
    pub degraded_codes: Vec<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditEmissionPath {
    Enqueued,
    DirectDisabled,
    DirectFallback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditEmissionReport {
    pub audit_seq: u64,
    pub path: AuditEmissionPath,
    pub pending_events: u64,
    pub degraded_codes: Vec<&'static str>,
}

pub fn emit_with_direct_fallback<E, F>(
    lane: Option<&AuditLaneHandle>,
    event: AuditEvent,
    direct_insert: F,
) -> Result<AuditEmissionReport, E>
where
    F: FnOnce(&AuditEvent) -> Result<(), E>,
{
    let audit_seq = event.audit_seq;
    let Some(lane) = lane else {
        direct_insert(&event)?;
        return Ok(AuditEmissionReport {
            audit_seq,
            path: AuditEmissionPath::DirectDisabled,
            pending_events: 0,
            degraded_codes: Vec::new(),
        });
    };

    match lane.enqueue(event) {
        AuditEnqueueResult::Enqueued {
            audit_seq,
            pending_events,
        } => Ok(AuditEmissionReport {
            audit_seq,
            path: AuditEmissionPath::Enqueued,
            pending_events,
            degraded_codes: Vec::new(),
        }),
        AuditEnqueueResult::Backpressure {
            code,
            event,
            pending_events,
            ..
        } => {
            direct_insert(&event)?;
            Ok(AuditEmissionReport {
                audit_seq,
                path: AuditEmissionPath::DirectFallback,
                pending_events,
                degraded_codes: vec![code],
            })
        }
        AuditEnqueueResult::Closed { event } => {
            direct_insert(&event)?;
            Ok(AuditEmissionReport {
                audit_seq,
                path: AuditEmissionPath::DirectFallback,
                pending_events: lane.pending_events(),
                degraded_codes: Vec::new(),
            })
        }
    }
}

pub fn insert_audit_event(connection: &DbConnection, event: &AuditEvent) -> crate::db::Result<()> {
    connection.insert_audit(&event.audit_id, &event.to_audit_input())
}

pub fn insert_audit_event_batch(
    connection: &DbConnection,
    events: &[AuditEvent],
) -> crate::db::Result<()> {
    let entries = events
        .iter()
        .map(|event| (event.audit_id.clone(), event.to_audit_input()))
        .collect::<Vec<_>>();
    connection.insert_audit_batch(&entries)
}

#[derive(Debug, Default)]
struct AuditLaneCounters {
    accepted: AtomicU64,
    drained: AtomicU64,
}

impl AuditLaneCounters {
    fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Acquire)
    }

    fn drained(&self) -> u64 {
        self.drained.load(Ordering::Acquire)
    }

    fn pending(&self) -> u64 {
        self.accepted().saturating_sub(self.drained())
    }
}

#[derive(Clone, Debug)]
pub struct AuditLaneHandle {
    sender: mpsc::Sender<AuditEvent>,
    counters: Arc<AuditLaneCounters>,
    capacity: usize,
}

impl AuditLaneHandle {
    #[must_use]
    pub fn enqueue(&self, event: AuditEvent) -> AuditEnqueueResult {
        let audit_seq = event.audit_seq;
        match self.sender.try_send(event) {
            Ok(()) => {
                self.counters.accepted.fetch_add(1, Ordering::AcqRel);
                AuditEnqueueResult::Enqueued {
                    audit_seq,
                    pending_events: self.counters.pending(),
                }
            }
            Err(SendError::Full(event)) => AuditEnqueueResult::Backpressure {
                code: AUDIT_BACKPRESSURE_CODE,
                event,
                capacity: self.capacity,
                pending_events: self.counters.pending(),
            },
            Err(SendError::Disconnected(event) | SendError::Cancelled(event)) => {
                AuditEnqueueResult::Closed { event }
            }
        }
    }

    #[must_use]
    pub fn pending_events(&self) -> u64 {
        self.counters.pending()
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
}

#[derive(Debug)]
pub struct AuditLane {
    receiver: mpsc::Receiver<AuditEvent>,
    counters: Arc<AuditLaneCounters>,
    config: AuditLaneConfig,
}

impl AuditLane {
    #[must_use]
    pub fn new(config: AuditLaneConfig) -> (AuditLaneHandle, Self) {
        let config = config.normalized();
        let counters = Arc::new(AuditLaneCounters::default());
        let (sender, receiver) = mpsc::channel(config.capacity);
        let handle = AuditLaneHandle {
            sender,
            counters: Arc::clone(&counters),
            capacity: config.capacity,
        };
        let lane = Self {
            receiver,
            counters,
            config,
        };
        (handle, lane)
    }

    pub fn drain_available<F>(&mut self, sink: F) -> AuditLaneDrainReport
    where
        F: FnMut(&[AuditEvent]),
    {
        self.drain_with_limit(AuditLanePhase::Drain, None, sink)
    }

    pub fn shutdown_drain<F>(&mut self, sink: F) -> AuditLaneDrainReport
    where
        F: FnMut(&[AuditEvent]),
    {
        self.receiver.close();
        self.drain_with_limit(
            AuditLanePhase::Shutdown,
            Some(self.config.shutdown_event_limit as u64),
            sink,
        )
    }

    fn drain_with_limit<F>(
        &mut self,
        phase: AuditLanePhase,
        max_events: Option<u64>,
        mut sink: F,
    ) -> AuditLaneDrainReport
    where
        F: FnMut(&[AuditEvent]),
    {
        let mut batch = Vec::with_capacity(self.config.batch_size);
        let mut drained_events = 0_u64;
        let mut batches = 0_u64;

        while max_events.is_none_or(|limit| drained_events < limit) {
            match self.receiver.try_recv() {
                Ok(event) => {
                    drained_events = drained_events.saturating_add(1);
                    batch.push(event);
                    if batch.len() == self.config.batch_size {
                        sink(&batch);
                        batch.clear();
                        batches = batches.saturating_add(1);
                    }
                }
                Err(RecvError::Empty | RecvError::Disconnected | RecvError::Cancelled) => break,
            }
        }

        if !batch.is_empty() {
            sink(&batch);
            batches = batches.saturating_add(1);
        }

        self.counters
            .drained
            .fetch_add(drained_events, Ordering::AcqRel);
        let pending_events = self.counters.pending();
        let degraded_codes = if phase == AuditLanePhase::Shutdown && pending_events > 0 {
            vec![AUDIT_LANE_SHUTDOWN_DRAIN_TIMEOUT_CODE]
        } else {
            Vec::new()
        };

        AuditLaneDrainReport {
            phase,
            drained_events,
            batches,
            pending_events,
            degraded_codes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::audit::{AuditVerifyOptions, verify_audit};
    use proptest::prelude::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn audit_lane_phase_order_is_schema_order() {
        let phases: Vec<&str> = AuditLanePhase::all()
            .iter()
            .map(|phase| phase.as_str())
            .collect();
        assert_eq!(
            phases,
            vec![
                "enqueue",
                "drain",
                "batch_commit",
                "shutdown",
                "backpressure"
            ]
        );
    }

    #[test]
    fn audit_lane_degraded_codes_are_snake_case() {
        for code in [
            AUDIT_BACKPRESSURE_CODE,
            AUDIT_LANE_SHUTDOWN_DRAIN_TIMEOUT_CODE,
        ] {
            assert!(
                code.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
                "{code} must stay fixture-compatible snake_case"
            );
        }
    }

    #[test]
    fn enqueue_and_drain_preserves_workspace_order() {
        let config = AuditLaneConfig {
            capacity: 8,
            batch_size: 8,
            shutdown_event_limit: 8,
        };
        let (handle, mut lane) = AuditLane::new(config);

        assert!(matches!(
            handle.enqueue(AuditEvent::new("workspace-a", 1, "memory.create")),
            AuditEnqueueResult::Enqueued {
                audit_seq: 1,
                pending_events: 1
            }
        ));
        let mut workspace_b = AuditEvent::new("workspace-b", 1, "memory.create");
        workspace_b.request_id = Some("req-b".to_owned());
        assert!(matches!(
            handle.enqueue(workspace_b),
            AuditEnqueueResult::Enqueued {
                audit_seq: 1,
                pending_events: 2
            }
        ));
        assert!(matches!(
            handle.enqueue(AuditEvent::new("workspace-a", 2, "memory.update")),
            AuditEnqueueResult::Enqueued {
                audit_seq: 2,
                pending_events: 3
            }
        ));

        let mut drained = Vec::new();
        let report = lane.drain_available(|batch| drained.extend_from_slice(batch));

        assert_eq!(report.drained_events, 3);
        assert_eq!(report.batches, 1);
        assert_eq!(report.pending_events, 0);
        let workspace_a: Vec<u64> = drained
            .iter()
            .filter(|event| event.workspace_id == "workspace-a")
            .map(|event| event.audit_seq)
            .collect();
        assert_eq!(workspace_a, vec![1, 2]);
        assert_eq!(
            drained
                .iter()
                .map(|event| event.workspace_id.as_str())
                .collect::<Vec<_>>(),
            vec!["workspace-a", "workspace-b", "workspace-a"]
        );
    }

    #[test]
    fn enqueue_reports_backpressure_when_full() {
        let config = AuditLaneConfig {
            capacity: 1,
            batch_size: 1,
            shutdown_event_limit: 1,
        };
        let (handle, _lane) = AuditLane::new(config);

        assert!(matches!(
            handle.enqueue(AuditEvent::new("workspace-a", 1, "memory.create")),
            AuditEnqueueResult::Enqueued { .. }
        ));
        let rejected = handle.enqueue(AuditEvent::new("workspace-a", 2, "memory.update"));

        assert_eq!(
            rejected,
            AuditEnqueueResult::Backpressure {
                code: AUDIT_BACKPRESSURE_CODE,
                event: AuditEvent::new("workspace-a", 2, "memory.update"),
                capacity: 1,
                pending_events: 1,
            }
        );
        assert_eq!(handle.pending_events(), 1);
    }

    #[test]
    fn emit_with_direct_fallback_uses_direct_insert_when_lane_disabled() -> Result<(), String> {
        let mut inserted = Vec::new();
        let report = emit_with_direct_fallback(
            None,
            AuditEvent::new("workspace-a", 7, "memory.create"),
            |event| {
                inserted.push(event.audit_seq);
                Ok::<(), String>(())
            },
        )?;

        assert_eq!(
            report,
            AuditEmissionReport {
                audit_seq: 7,
                path: AuditEmissionPath::DirectDisabled,
                pending_events: 0,
                degraded_codes: Vec::new(),
            }
        );
        assert_eq!(inserted, vec![7]);
        Ok(())
    }

    #[test]
    fn emit_with_direct_fallback_enqueues_when_lane_available() -> Result<(), String> {
        let config = AuditLaneConfig {
            capacity: 2,
            batch_size: 2,
            shutdown_event_limit: 2,
        };
        let (handle, mut lane) = AuditLane::new(config);
        let mut direct_insert_count = 0;

        let report = emit_with_direct_fallback(
            Some(&handle),
            AuditEvent::new("workspace-a", 8, "memory.create"),
            |_| {
                direct_insert_count += 1;
                Ok::<(), String>(())
            },
        )?;

        assert_eq!(
            report,
            AuditEmissionReport {
                audit_seq: 8,
                path: AuditEmissionPath::Enqueued,
                pending_events: 1,
                degraded_codes: Vec::new(),
            }
        );
        assert_eq!(direct_insert_count, 0);

        let mut drained = Vec::new();
        lane.drain_available(|batch| drained.extend_from_slice(batch));
        assert_eq!(drained[0].audit_seq, 8);
        Ok(())
    }

    #[test]
    fn emit_with_direct_fallback_reports_backpressure_and_direct_inserts() -> Result<(), String> {
        let config = AuditLaneConfig {
            capacity: 1,
            batch_size: 1,
            shutdown_event_limit: 1,
        };
        let (handle, _lane) = AuditLane::new(config);
        assert!(matches!(
            handle.enqueue(AuditEvent::new("workspace-a", 9, "memory.create")),
            AuditEnqueueResult::Enqueued { .. }
        ));
        let mut inserted = Vec::new();

        let report = emit_with_direct_fallback(
            Some(&handle),
            AuditEvent::new("workspace-a", 10, "memory.update"),
            |event| {
                inserted.push((event.audit_seq, event.action.clone()));
                Ok::<(), String>(())
            },
        )?;

        assert_eq!(
            report,
            AuditEmissionReport {
                audit_seq: 10,
                path: AuditEmissionPath::DirectFallback,
                pending_events: 1,
                degraded_codes: vec![AUDIT_BACKPRESSURE_CODE],
            }
        );
        assert_eq!(inserted, vec![(10, "memory.update".to_owned())]);
        Ok(())
    }

    #[test]
    fn audit_event_round_trips_db_audit_input() {
        let input = CreateAuditInput {
            workspace_id: Some("workspace-a".to_owned()),
            actor: Some("ee remember".to_owned()),
            action: "memory.create".to_owned(),
            target_type: Some("memory".to_owned()),
            target_id: Some("mem_a".to_owned()),
            details: Some("{\"schema\":\"test\"}".to_owned()),
        };

        let event = AuditEvent::from_audit_input("audit_a", 11, &input);
        let rendered = event.to_audit_input();

        assert_eq!(event.audit_id, "audit_a");
        assert_eq!(event.audit_seq, 11);
        assert_eq!(rendered.workspace_id, input.workspace_id);
        assert_eq!(rendered.actor, input.actor);
        assert_eq!(rendered.action, input.action);
        assert_eq!(rendered.target_type, input.target_type);
        assert_eq!(rendered.target_id, input.target_id);
        assert_eq!(rendered.details, input.details);
    }

    #[test]
    fn insert_audit_event_batch_uses_db_batch_writer() -> Result<(), String> {
        const WORKSPACE_ID: &str = "wsp_01234567890123456789012345";
        const FIRST_AUDIT_ID: &str = "audit_00000000000000000000000000000001";
        const SECOND_AUDIT_ID: &str = "audit_00000000000000000000000000000002";
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/audit-lane-batch-test".to_owned(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        let first = AuditEvent::from_audit_input(
            FIRST_AUDIT_ID,
            1,
            &CreateAuditInput {
                workspace_id: Some(WORKSPACE_ID.to_owned()),
                actor: Some("ee remember".to_owned()),
                action: "memory.create".to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some("mem_a".to_owned()),
                details: None,
            },
        );
        let second = AuditEvent::from_audit_input(
            SECOND_AUDIT_ID,
            2,
            &CreateAuditInput {
                workspace_id: Some(WORKSPACE_ID.to_owned()),
                actor: Some("ee remember".to_owned()),
                action: "memory.update".to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some("mem_a".to_owned()),
                details: None,
            },
        );

        insert_audit_event_batch(&connection, &[first, second])
            .map_err(|error| error.to_string())?;

        let first = connection
            .get_audit(FIRST_AUDIT_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "first audit row missing".to_owned())?;
        let second = connection
            .get_audit(SECOND_AUDIT_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "second audit row missing".to_owned())?;
        assert_eq!(first.action, "memory.create");
        assert_eq!(second.action, "memory.update");
        assert_eq!(second.prev_row_hash, first.this_row_hash);
        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn concurrent_producers_batch_commit_without_loss_or_chain_break() -> Result<(), String> {
        const WORKSPACE_ID: &str = "wsp_01234567890123456789012345";
        const PRODUCER_COUNT: usize = 64;
        const EVENTS_PER_PRODUCER: usize = 2;
        const EVENT_COUNT: usize = PRODUCER_COUNT * EVENTS_PER_PRODUCER;

        let config = AuditLaneConfig {
            capacity: EVENT_COUNT,
            batch_size: 64,
            shutdown_event_limit: EVENT_COUNT,
        };
        let (handle, mut lane) = AuditLane::new(config);
        let start = Arc::new(Barrier::new(PRODUCER_COUNT));
        let mut producers = Vec::with_capacity(PRODUCER_COUNT);

        for producer_id in 0..PRODUCER_COUNT {
            let producer_handle = handle.clone();
            let producer_start = Arc::clone(&start);
            producers.push(thread::spawn(move || -> Result<Vec<Duration>, String> {
                producer_start.wait();
                let mut enqueue_latencies = Vec::with_capacity(EVENTS_PER_PRODUCER);
                for event_index in 0..EVENTS_PER_PRODUCER {
                    let seq = (producer_id * EVENTS_PER_PRODUCER + event_index + 1) as u64;
                    let input = CreateAuditInput {
                        workspace_id: Some(WORKSPACE_ID.to_owned()),
                        actor: Some(format!("producer-{producer_id:02}")),
                        action: "memory.create".to_owned(),
                        target_type: Some("memory".to_owned()),
                        target_id: Some(format!("mem_{seq:026}")),
                        details: Some(format!(
                            "{{\"producer\":{producer_id},\"event\":{event_index}}}"
                        )),
                    };
                    let event =
                        AuditEvent::from_audit_input(format!("audit_{seq:032x}"), seq, &input);
                    let started_at = Instant::now();
                    let result = producer_handle.enqueue(event);
                    enqueue_latencies.push(started_at.elapsed());
                    match result {
                        AuditEnqueueResult::Enqueued { audit_seq, .. } if audit_seq == seq => {}
                        other => {
                            return Err(format!(
                                "producer {producer_id} event {event_index} enqueue returned {other:?}"
                            ));
                        }
                    }
                }
                Ok(enqueue_latencies)
            }));
        }

        let mut enqueue_latencies = Vec::with_capacity(EVENT_COUNT);
        for producer in producers {
            enqueue_latencies.extend(
                producer
                    .join()
                    .map_err(|_| "producer thread panicked".to_owned())??,
            );
        }
        enqueue_latencies.sort_unstable();
        let p99_index = ((enqueue_latencies.len() * 99).div_ceil(100)).saturating_sub(1);
        let p99_enqueue_latency = enqueue_latencies
            .get(p99_index)
            .copied()
            .ok_or_else(|| "missing enqueue latency sample".to_owned())?;
        assert!(
            p99_enqueue_latency <= Duration::from_millis(2),
            "p99 foreground enqueue latency {p99_enqueue_latency:?} exceeded 2ms budget"
        );

        let mut drained = Vec::with_capacity(EVENT_COUNT);
        let report = lane.drain_available(|batch| drained.extend_from_slice(batch));

        assert_eq!(report.drained_events, EVENT_COUNT as u64);
        assert_eq!(report.batches, 2);
        assert_eq!(report.pending_events, 0);
        assert!(report.degraded_codes.is_empty());
        assert_eq!(handle.pending_events(), 0);
        assert_eq!(drained.len(), EVENT_COUNT);

        let mut drained_seqs = drained
            .iter()
            .map(|event| event.audit_seq)
            .collect::<Vec<_>>();
        drained_seqs.sort_unstable();
        assert_eq!(drained_seqs, (1..=EVENT_COUNT as u64).collect::<Vec<_>>());

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/audit-lane-concurrent-producer-test".to_owned(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        insert_audit_event_batch(&connection, &drained).map_err(|error| error.to_string())?;

        let stored = drained
            .iter()
            .map(|event| {
                connection
                    .get_audit(&event.audit_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("audit row {} missing", event.audit_id))
            })
            .collect::<Result<Vec<_>, String>>()?;

        assert!(stored[0].prev_row_hash.is_none());
        for pair in stored.windows(2) {
            assert_eq!(pair[1].prev_row_hash, pair[0].this_row_hash);
        }

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn file_backed_batch_survives_reopen_and_audit_verify() -> Result<(), String> {
        const WORKSPACE_ID: &str = "wsp_01234567890123456789012345";
        const EVENT_COUNT: usize = 4;
        let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
        let workspace = tempdir.path().join("workspace");
        let ee_dir = workspace.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| format!("create .ee dir: {error}"))?;
        let database_path = ee_dir.join("ee.db");

        let config = AuditLaneConfig {
            capacity: EVENT_COUNT,
            batch_size: 2,
            shutdown_event_limit: EVENT_COUNT,
        };
        let (handle, mut lane) = AuditLane::new(config);
        for seq in 1..=EVENT_COUNT as u64 {
            let input = CreateAuditInput {
                workspace_id: Some(WORKSPACE_ID.to_owned()),
                actor: Some(format!("restart-producer-{seq}")),
                action: "memory.create".to_owned(),
                target_type: Some("memory".to_owned()),
                target_id: Some(format!("mem_restart_{seq:014}")),
                details: Some(format!("{{\"restart_seq\":{seq}}}")),
            };
            let event = AuditEvent::from_audit_input(format!("audit_{seq:032x}"), seq, &input);
            assert!(matches!(
                handle.enqueue(event),
                AuditEnqueueResult::Enqueued { audit_seq, .. } if audit_seq == seq
            ));
        }

        let mut drained = Vec::with_capacity(EVENT_COUNT);
        let drain_report = lane.shutdown_drain(|batch| drained.extend_from_slice(batch));
        assert_eq!(drain_report.drained_events, EVENT_COUNT as u64);
        assert_eq!(drain_report.batches, 2);
        assert_eq!(drain_report.pending_events, 0);
        assert!(drain_report.degraded_codes.is_empty());

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("audit-lane-restart-test".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        insert_audit_event_batch(&connection, &drained).map_err(|error| error.to_string())?;
        let last_before_reopen = connection
            .get_audit("audit_00000000000000000000000000000004")
            .map_err(|error| error.to_string())?
            .and_then(|entry| entry.this_row_hash)
            .ok_or_else(|| "last audit row hash missing before reopen".to_owned())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = verify_audit(&AuditVerifyOptions {
            workspace,
            database_path: Some(database_path),
            since: None,
            until: None,
        })
        .map_err(|error| error.to_string())?;

        assert!(
            report.integrity_ok,
            "audit verify issues: {:?}",
            report.issues
        );
        assert_eq!(report.rows, EVENT_COUNT as u32);
        assert_eq!(report.last_hash, Some(last_before_reopen));
        assert!(report.first_break.is_none());
        assert!(report.issues.is_empty());
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn randomized_producer_rates_commit_without_loss_or_chain_break(
            producer_event_counts in prop::collection::vec(1_usize..=6, 1..=12),
            batch_size in 1_usize..=16,
        ) {
            let stored = run_randomized_producer_rate_case(producer_event_counts, batch_size)
                .map_err(TestCaseError::fail)?;

            prop_assert!(stored[0].prev_row_hash.is_none());
            for pair in stored.windows(2) {
                prop_assert_eq!(&pair[1].prev_row_hash, &pair[0].this_row_hash);
            }
        }
    }

    fn run_randomized_producer_rate_case(
        producer_event_counts: Vec<usize>,
        batch_size: usize,
    ) -> Result<Vec<crate::db::StoredAuditEntry>, String> {
        const WORKSPACE_ID: &str = "wsp_01234567890123456789012345";
        let event_count: usize = producer_event_counts.iter().sum();
        let config = AuditLaneConfig {
            capacity: event_count,
            batch_size,
            shutdown_event_limit: event_count,
        };
        let (handle, mut lane) = AuditLane::new(config);
        let start = Arc::new(Barrier::new(producer_event_counts.len()));
        let mut next_seq = 1_u64;
        let mut producers = Vec::with_capacity(producer_event_counts.len());

        for (producer_id, events_for_producer) in producer_event_counts.into_iter().enumerate() {
            let producer_handle = handle.clone();
            let producer_start = Arc::clone(&start);
            let first_seq = next_seq;
            next_seq = next_seq.saturating_add(events_for_producer as u64);
            producers.push(thread::spawn(move || -> Result<(), String> {
                producer_start.wait();
                for offset in 0..events_for_producer {
                    let seq = first_seq + offset as u64;
                    let input = CreateAuditInput {
                        workspace_id: Some(WORKSPACE_ID.to_owned()),
                        actor: Some(format!("property-producer-{producer_id:02}")),
                        action: "memory.create".to_owned(),
                        target_type: Some("memory".to_owned()),
                        target_id: Some(format!("mem_property_{seq:014}")),
                        details: Some(format!(
                            "{{\"producer\":{producer_id},\"offset\":{offset},\"batchSize\":{batch_size}}}"
                        )),
                    };
                    let event =
                        AuditEvent::from_audit_input(format!("audit_{seq:032x}"), seq, &input);
                    match producer_handle.enqueue(event) {
                        AuditEnqueueResult::Enqueued { audit_seq, .. } if audit_seq == seq => {}
                        other => {
                            return Err(format!(
                                "producer {producer_id} offset {offset} enqueue returned {other:?}"
                            ));
                        }
                    }
                }
                Ok(())
            }));
        }

        for producer in producers {
            producer
                .join()
                .map_err(|_| "property producer thread panicked".to_owned())??;
        }

        let mut drained = Vec::with_capacity(event_count);
        let report = lane.shutdown_drain(|batch| drained.extend_from_slice(batch));
        if report.drained_events != event_count as u64 {
            return Err(format!(
                "drained {} events, expected {event_count}",
                report.drained_events
            ));
        }
        if report.pending_events != 0 {
            return Err(format!(
                "drain left {} pending events",
                report.pending_events
            ));
        }
        if !report.degraded_codes.is_empty() {
            return Err(format!(
                "drain degraded unexpectedly: {:?}",
                report.degraded_codes
            ));
        }

        let mut drained_seqs = drained
            .iter()
            .map(|event| event.audit_seq)
            .collect::<Vec<_>>();
        drained_seqs.sort_unstable();
        let expected_seqs = (1..=event_count as u64).collect::<Vec<_>>();
        if drained_seqs != expected_seqs {
            return Err(format!(
                "drained sequence set mismatch: got {drained_seqs:?}, expected {expected_seqs:?}"
            ));
        }

        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/audit-lane-property-test".to_owned(),
                    name: None,
                },
            )
            .map_err(|error| error.to_string())?;
        insert_audit_event_batch(&connection, &drained).map_err(|error| error.to_string())?;
        let stored = drained
            .iter()
            .map(|event| {
                connection
                    .get_audit(&event.audit_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("audit row {} missing", event.audit_id))
            })
            .collect::<Result<Vec<_>, String>>()?;
        connection.close().map_err(|error| error.to_string())?;
        Ok(stored)
    }

    #[test]
    fn drain_honors_batch_boundaries() {
        let config = AuditLaneConfig {
            capacity: 8,
            batch_size: 2,
            shutdown_event_limit: 8,
        };
        let (handle, mut lane) = AuditLane::new(config);
        for seq in 1..=5 {
            assert!(matches!(
                handle.enqueue(AuditEvent::new("workspace-a", seq, "memory.create")),
                AuditEnqueueResult::Enqueued { .. }
            ));
        }

        let mut batch_sizes = Vec::new();
        let report = lane.drain_available(|batch| batch_sizes.push(batch.len()));

        assert_eq!(report.drained_events, 5);
        assert_eq!(report.batches, 3);
        assert_eq!(batch_sizes, vec![2, 2, 1]);
        assert!(report.degraded_codes.is_empty());
    }

    #[test]
    fn shutdown_drain_reports_timeout_when_budget_leaves_pending_events() {
        let config = AuditLaneConfig {
            capacity: 4,
            batch_size: 2,
            shutdown_event_limit: 1,
        };
        let (handle, mut lane) = AuditLane::new(config);
        for seq in 1..=3 {
            assert!(matches!(
                handle.enqueue(AuditEvent::new("workspace-a", seq, "memory.create")),
                AuditEnqueueResult::Enqueued { .. }
            ));
        }

        let mut drained = Vec::new();
        let report = lane.shutdown_drain(|batch| drained.extend_from_slice(batch));

        assert!(handle.is_closed());
        assert_eq!(report.phase, AuditLanePhase::Shutdown);
        assert_eq!(report.drained_events, 1);
        assert_eq!(report.pending_events, 2);
        assert_eq!(
            report.degraded_codes,
            vec![AUDIT_LANE_SHUTDOWN_DRAIN_TIMEOUT_CODE]
        );
        assert_eq!(drained[0].audit_seq, 1);
        assert!(matches!(
            handle.enqueue(AuditEvent::new("workspace-a", 4, "memory.create")),
            AuditEnqueueResult::Closed { .. }
        ));
    }
}
