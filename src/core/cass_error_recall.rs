//! Feed imported CASS failure->fix arcs into the error-fingerprint store
//! (bd-reality-core-convergence-1azkt.60).
//!
//! `ee diagnose-error` and `ee pack --error-log` recall an error class by its
//! layered fingerprint key (ADR 0057). Until now only a manual
//! `ee diagnose-error --record` populated that store, while every imported
//! session carried real tool failures and the fixes that followed them. This
//! derivation reads one imported session in transcript order, pairs each
//! failing tool result with a later completed retry of the same invocation
//! and execution context, and records:
//!
//! - the failure's fingerprint (key and masked signatures only, never the raw
//!   log), for every structured failure;
//! - when the failure was resolved, a `repair`/`helpful` link to the admitted
//!   assistant turns between the failure and its verifying success, and a
//!   `proof` link to that success.
//!
//! Derivation policy (shared with .45, .46 and .59): tool records are
//! quarantined from retrieval for exposure, not secrecy (class A), and may
//! feed this derivation. A record quarantined for instruction risk (class B)
//! never does. Repair targets are always retrieval-admitted turns, so recall
//! points at text an agent can actually be shown. Every link names the failing
//! span as its `evidence_ref`. Recording is idempotent: fingerprints upsert
//! and link ids are deterministic over (workspace, key, kind, target, outcome).

use std::collections::BTreeSet;

use serde_json::Value;

use crate::core::error_diagnosis::{ErrorRepairLinkRecording, record_error_repair_links};
use crate::core::error_recall::{
    CanonicalDiagnostic, from_cargo, from_ee_error, from_rch_blocker, from_rustc,
};
use crate::db::{DbConnection, Result, StoredEvidenceSpan, StoredSession};

#[path = "cass_error_recall_outcome.rs"]
mod outcome;

/// Actor recorded on CASS-derived repair links.
pub const CASS_ERROR_RECALL_ACTOR: &str = "ee import cass";
/// Version of failure extraction, independent of the incident-card renderer.
pub const CASS_ERROR_RECALL_DERIVATION: &str = "cass_error_recall.v2";

/// Tool output beyond this many bytes is not scanned for diagnostics.
const MAX_SCANNED_OUTPUT_BYTES: usize = 64 * 1024;
/// Distinct diagnostics recorded from one failing tool result.
const MAX_DIAGNOSTICS_PER_FAILURE: usize = 4;
/// Unresolved failures remembered while walking one session.
const MAX_PENDING_FAILURES: usize = 32;
/// Admitted assistant turns linked as the repair of one failure.
const MAX_REPAIR_TURNS: usize = 2;

/// What one session contributed to the error-fingerprint store.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CassErrorRecallReport {
    pub failures_seen: u32,
    pub fingerprints_recorded: u32,
    pub resolved_failures: u32,
    pub repair_links_recorded: u32,
    /// Derived incident cards written by this run (ADR 0091).
    pub incident_cards_recorded: u32,
}

/// Derive and record error fingerprints and repair links for one imported
/// session. Runs in one transaction; a rerun records nothing new.
///
/// # Errors
///
/// Propagates database errors from reading the session or recording links.
pub fn record_session_error_recall(
    connection: &DbConnection,
    workspace_id: &str,
    session_id: &str,
) -> Result<CassErrorRecallReport> {
    let Some(session) = connection.get_session(session_id)? else {
        return Ok(CassErrorRecallReport::default());
    };
    if session.workspace_id != workspace_id {
        return Ok(CassErrorRecallReport::default());
    }
    let spans = connection
        .list_evidence_spans_for_session(session_id)?
        .into_iter()
        .filter(|span| !span.is_derived_incident_card())
        .collect::<Vec<_>>();
    let arcs = session_failure_arcs(workspace_id, &session, &spans);
    let mut report = CassErrorRecallReport::default();
    if arcs.is_empty() {
        return Ok(report);
    }
    connection.with_transaction(|| {
        for arc in &arcs {
            report.failures_seen = report.failures_seen.saturating_add(1);
            if arc.resolution.is_some() {
                report.resolved_failures = report.resolved_failures.saturating_add(1);
            }
            let recording = arc.recording();
            for diagnostic in &arc.diagnostics {
                let links =
                    record_error_repair_links(connection, workspace_id, diagnostic, &recording)?;
                report.fingerprints_recorded = report.fingerprints_recorded.saturating_add(1);
                let recorded_here = links
                    .iter()
                    .filter(|link| link.evidence_ref.as_deref() == Some(arc.failure_id.as_str()))
                    .count();
                report.repair_links_recorded = report
                    .repair_links_recorded
                    .saturating_add(u32::try_from(recorded_here).unwrap_or(u32::MAX));
            }
            let Some(card) = crate::core::incident_card::draft_incident_card(
                workspace_id,
                session_id,
                &spans,
                arc,
            ) else {
                continue;
            };
            if connection.get_evidence_span(&card.id)?.is_none() {
                connection.insert_evidence_span(&card.id, &card.input)?;
                report.incident_cards_recorded = report.incident_cards_recorded.saturating_add(1);
            }
            // The card is the compact form of this arc's repair: recall of the
            // error class surfaces it, and its failing span stays its anchor.
            let card_recording = ErrorRepairLinkRecording {
                helpful_repairs: vec![card.id],
                created_by: Some(crate::core::incident_card::INCIDENT_CARD_ACTOR.to_owned()),
                evidence_ref: Some(arc.failure_id.clone()),
                ..ErrorRepairLinkRecording::default()
            };
            for diagnostic in &arc.diagnostics {
                record_error_repair_links(connection, workspace_id, diagnostic, &card_recording)?;
            }
        }
        Ok(())
    })?;
    Ok(report)
}

/// One failing tool result, its diagnostics, and how it was resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FailureArc {
    pub failure_id: String,
    /// The tool call that ran the failing command.
    pub attempt_id: String,
    pub family: CommandFamily,
    /// The first diagnostic line of the failing output (and its source
    /// location), secret-redacted but otherwise raw tool output: a derivation
    /// that shows it must screen it again.
    pub symptom: Option<String>,
    pub diagnostics: Vec<CanonicalDiagnostic>,
    pub resolution: Option<Resolution>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Resolution {
    pub proof_id: String,
    pub proof_family: CommandFamily,
    /// Admitted repair explanations between the failure and the verifying
    /// call, newest first. Messages after that call are not verified by it.
    pub repair_ids: Vec<String>,
}

impl FailureArc {
    fn recording(&self) -> ErrorRepairLinkRecording {
        let (helpful_repairs, proof_links) = self.resolution.as_ref().map_or_else(
            || (Vec::new(), Vec::new()),
            |resolution| {
                (
                    resolution.repair_ids.clone(),
                    vec![resolution.proof_id.clone()],
                )
            },
        );
        ErrorRepairLinkRecording {
            helpful_repairs,
            proof_links,
            created_by: Some(CASS_ERROR_RECALL_ACTOR.to_owned()),
            evidence_ref: Some(self.failure_id.clone()),
            ..ErrorRepairLinkRecording::default()
        }
    }
}

struct PendingFailure {
    index: usize,
    failure_id: String,
    attempt_id: String,
    family: CommandFamily,
    symptom: Option<String>,
    diagnostics: Vec<CanonicalDiagnostic>,
}

impl PendingFailure {
    fn into_arc(self, resolution: Option<Resolution>) -> FailureArc {
        FailureArc {
            failure_id: self.failure_id,
            attempt_id: self.attempt_id,
            family: self.family,
            symptom: self.symptom,
            diagnostics: self.diagnostics,
            resolution,
        }
    }
}

/// An exec invocation survives empty-input polls, but its launch time and
/// scope never become those of the polling request. Buffered output is bounded
/// and only used for derivation; source rows and their hashes are unchanged.
struct InFlightInvocation {
    family: CommandFamily,
    call_index: usize,
    request_index: usize,
    can_poll: bool,
    process_id: Option<i32>,
    output: ToolOutput,
    failure_recorded: bool,
}

impl InFlightInvocation {
    fn append(&mut self, chunk: ToolOutput) {
        let overflow =
            self.output.text.len().saturating_add(chunk.text.len()) > MAX_SCANNED_OUTPUT_BYTES;
        let veto = self.output.failed() || chunk.failed() || overflow;
        let mut end = chunk
            .text
            .len()
            .min(MAX_SCANNED_OUTPUT_BYTES.saturating_sub(self.output.text.len()));
        while !chunk.text.is_char_boundary(end) {
            end -= 1;
        }
        // Chunks are contiguous stdout/stderr, not separate messages. Inserting
        // a newline could conceal a failure/instruction split across reads.
        self.output.text.push_str(&chunk.text[..end]);
        self.output.is_error = if veto { Some(true) } else { chunk.is_error };
        self.output.exit_code = chunk.exit_code;
    }
}

struct InvocationObservation {
    family: CommandFamily,
    call_index: usize,
    output: ToolOutput,
    new_failure: bool,
    complete: bool,
}

#[derive(Default)]
struct InvocationLedger {
    calls: std::collections::HashMap<String, InFlightInvocation>,
    running: std::collections::BTreeMap<i32, InFlightInvocation>,
    seen_calls: BTreeSet<String>,
    seen_processes: BTreeSet<i32>,
    invalid_processes: BTreeSet<i32>,
}

impl InvocationLedger {
    fn invalidate(&mut self, process_id: i32) {
        self.invalid_processes.insert(process_id);
        self.running.remove(&process_id);
        self.calls
            .retain(|_, call| call.process_id != Some(process_id));
    }

    fn observe(
        &mut self,
        event: ToolEvent,
        index: usize,
        readable: bool,
    ) -> Option<InvocationObservation> {
        match event {
            ToolEvent::Call {
                id,
                command,
                context,
                can_poll,
            } => {
                if id.is_empty() || !self.seen_calls.insert(id.clone()) {
                    if let Some(call) = self.calls.remove(&id)
                        && let Some(process_id) = call.process_id
                    {
                        self.invalidate(process_id);
                    }
                    return None;
                }
                if readable
                    && let Some(mut family) = command.as_deref().and_then(CommandFamily::parse)
                {
                    family.bind_context(context.as_deref());
                    self.calls.insert(
                        id,
                        InFlightInvocation {
                            family,
                            call_index: index,
                            request_index: index,
                            can_poll,
                            process_id: None,
                            output: ToolOutput::default(),
                            failure_recorded: false,
                        },
                    );
                }
                None
            }
            ToolEvent::Poll {
                id,
                process_id,
                read_only,
            } => {
                if id.is_empty() || !self.seen_calls.insert(id.clone()) {
                    if let Some(previous) = self.calls.remove(&id)
                        && let Some(previous_id) = previous.process_id
                    {
                        self.invalidate(previous_id);
                    }
                    self.invalidate(process_id);
                    return None;
                }
                if !readable || !read_only || self.invalid_processes.contains(&process_id) {
                    self.invalidate(process_id);
                    return None;
                }
                let Some(mut call) = self.running.remove(&process_id) else {
                    // An overlapping poll cannot inherit another poll's output.
                    // Also prevents a future exec from adopting an orphan id.
                    self.invalidate(process_id);
                    return None;
                };
                call.request_index = index;
                self.calls.insert(id, call);
                None
            }
            ToolEvent::Result { id, output } => {
                let call = id.as_deref().and_then(|id| self.calls.remove(id))?;
                if !readable || call.request_index >= index {
                    if let Some(process_id) = call.process_id {
                        self.invalidate(process_id);
                    }
                    return None;
                }
                self.finish(call, output)
            }
        }
    }

    fn finish(
        &mut self,
        mut call: InFlightInvocation,
        output: ToolOutput,
    ) -> Option<InvocationObservation> {
        if call
            .process_id
            .is_some_and(|id| self.invalid_processes.contains(&id))
        {
            return None;
        }
        let (running, chunk) = if call.can_poll {
            match execution_chunk(&output) {
                Ok(Some(chunk)) => (chunk.process_id, chunk.output),
                Ok(None) if call.process_id.is_none() => (None, output),
                _ => {
                    if let Some(process_id) = call.process_id {
                        self.invalidate(process_id);
                    }
                    return None;
                }
            }
        } else {
            (None, output)
        };
        if let Some(process_id) = running {
            if call
                .process_id
                .is_some_and(|previous| previous != process_id)
                || self.invalid_processes.contains(&process_id)
                || (call.process_id.is_none() && !self.seen_processes.insert(process_id))
            {
                if let Some(previous) = call.process_id {
                    self.invalidate(previous);
                }
                self.invalidate(process_id);
                return None;
            }
            call.process_id = Some(process_id);
        }
        call.append(chunk);
        // Decoded chunks can assemble an instruction that no individual source
        // row contained. Once detected, do not derive from this or later chunks.
        if call.can_poll
            && crate::policy::screen_external_text_for_ingestion(&call.output.text).instruction_like
        {
            if let Some(process_id) = call.process_id {
                self.invalidate(process_id);
            }
            return None;
        }
        let observation = InvocationObservation {
            family: call.family.clone(),
            call_index: call.call_index,
            output: call.output.clone(),
            new_failure: !call.failure_recorded,
            complete: running.is_none(),
        };
        call.failure_recorded |= !failure_diagnostics(&call.output).is_empty();
        if let Some(process_id) = running {
            // Count streams awaiting a poll result too: moving a stream from
            // running to calls must not replenish its memory budget. At most
            // 32 * 64 KiB is retained (plus this observation's transient copy).
            let awaiting = self
                .calls
                .values()
                .filter(|call| call.process_id.is_some())
                .count();
            if self.running.len().saturating_add(awaiting) < MAX_PENDING_FAILURES {
                self.running.insert(process_id, call);
            } else {
                self.invalidate(process_id);
            }
        }
        Some(observation)
    }
}

struct ExecutionChunk {
    process_id: Option<i32>,
    output: ToolOutput,
}

/// Parse the actual unified-exec wrapper, not a status-looking line in stdout.
/// The upstream header has optional Chunk ID, Wall time, exactly one process
/// status, optional Original token count, then Output. A completed poll must
/// carry its process exit; a malformed wrapper never degrades to plain stdout.
fn execution_chunk(raw: &ToolOutput) -> std::result::Result<Option<ExecutionChunk>, ()> {
    let text = raw.text.as_str();
    if !text.starts_with("Chunk ID: ") && !text.starts_with("Wall time: ") {
        return Ok(None);
    }
    let (header, body) = text
        .split_once("\nOutput:")
        .or_else(|| text.split_once("\nFinal output:"))
        .ok_or(())?;
    if header.len() > 1024 {
        return Err(());
    }
    let body = if body.is_empty() {
        body
    } else {
        body.strip_prefix('\n').ok_or(())?
    };
    let mut seen = BTreeSet::new();
    let mut process_id = None;
    let mut exit_code = None;
    for line in header.lines() {
        let field = if let Some(id) = line.strip_prefix("Chunk ID: ") {
            if id.is_empty() {
                return Err(());
            }
            "chunk"
        } else if let Some(duration) = line.strip_prefix("Wall time: ") {
            let duration = duration
                .strip_suffix(" seconds")
                .ok_or(())?
                .parse::<f64>()
                .map_err(|_| ())?;
            if !duration.is_finite() || duration < 0.0 {
                return Err(());
            }
            "time"
        } else if let Some(id) = line.strip_prefix("Process running with session ID ") {
            process_id = Some(id.parse::<i32>().map_err(|_| ())?);
            "status"
        } else if let Some(code) = line.strip_prefix("Process exited with code ") {
            exit_code = Some(i64::from(code.parse::<i32>().map_err(|_| ())?));
            "status"
        } else if let Some(count) = line.strip_prefix("Original token count: ") {
            count.parse::<u64>().map_err(|_| ())?;
            "tokens"
        } else {
            return Err(());
        };
        if !seen.insert(field) {
            return Err(());
        }
    }
    if !seen.contains("time")
        || !seen.contains("status")
        || raw.exit_code.is_some_and(|code| Some(code) != exit_code)
    {
        return Err(());
    }
    Ok(Some(ExecutionChunk {
        process_id,
        output: ToolOutput {
            text: body.to_owned(),
            is_error: raw.is_error,
            exit_code,
        },
    }))
}

/// Walk a session in transcript order and pair failures with the success that
/// verified their fix. Pure over the given rows.
pub(crate) fn session_failure_arcs(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<FailureArc> {
    let mut ledger = InvocationLedger::default();
    let mut pending: Vec<PendingFailure> = Vec::new();
    let mut arcs = Vec::new();

    for (index, span) in spans.iter().enumerate() {
        if span.workspace_id != workspace_id || span.session_id != session.id {
            continue;
        }
        let events = match span.span_kind.as_str() {
            "tool_call" | "tool_result" => tool_events(&span.excerpt),
            _ => Vec::new(),
        };
        for event in events {
            let Some(observed) = ledger.observe(
                event,
                index,
                span.is_class_a_derivation_readable(workspace_id, session),
            ) else {
                continue;
            };
            let diagnostics = if observed.new_failure {
                failure_diagnostics(&observed.output)
            } else {
                Vec::new()
            };
            if diagnostics.is_empty() {
                if observed.complete && observed.output.succeeded() {
                    resolve_pending(
                        &mut pending,
                        &mut arcs,
                        &observed.family,
                        observed.call_index,
                        index,
                        span,
                        spans,
                        workspace_id,
                        session,
                    );
                }
                continue;
            }
            if pending.len() == MAX_PENDING_FAILURES {
                arcs.push(pending.remove(0).into_arc(None));
            }
            pending.push(PendingFailure {
                index,
                failure_id: span.id.clone(),
                attempt_id: spans[observed.call_index].id.clone(),
                symptom: symptom_line(&observed.output),
                family: observed.family,
                diagnostics,
            });
        }
    }
    arcs.extend(pending.into_iter().map(|failure| failure.into_arc(None)));
    arcs.sort_by(|left, right| left.failure_id.cmp(&right.failure_id));
    arcs
}

#[allow(clippy::too_many_arguments)]
fn resolve_pending(
    pending: &mut Vec<PendingFailure>,
    arcs: &mut Vec<FailureArc>,
    family: &CommandFamily,
    success_call_index: usize,
    success_index: usize,
    success: &StoredEvidenceSpan,
    spans: &[StoredEvidenceSpan],
    workspace_id: &str,
    session: &StoredSession,
) {
    let mut still_pending = Vec::with_capacity(pending.len());
    for failure in pending.drain(..) {
        if !family.verifies(&failure.family) {
            still_pending.push(failure);
            continue;
        }
        let Some(range) = repair_span_range(failure.index, success_call_index, success_index)
        else {
            still_pending.push(failure);
            continue;
        };
        let Some(turns) = spans.get(range) else {
            still_pending.push(failure);
            continue;
        };
        // Only explanations observed before the verifying command started can
        // be credited to that run. Narration is not a helpful repair, and a
        // message written while the command was running is not verified by it.
        // A successful retry may still record proof without a repair or card.
        let repair_ids = turns
            .iter()
            .rev()
            .filter(|span| {
                span.span_kind == "message"
                    && span.role.as_deref() == Some("assistant")
                    && span.is_derivation_admitted_for_session(workspace_id, session)
                    && crate::core::incident_card::explains_a_fix(
                        &span.reader_body(),
                        failure.symptom.as_deref(),
                    )
            })
            .take(MAX_REPAIR_TURNS)
            .map(|span| span.id.clone())
            .collect::<Vec<_>>();
        arcs.push(failure.into_arc(Some(Resolution {
            proof_id: success.id.clone(),
            proof_family: family.clone(),
            repair_ids,
        })));
    }
    *pending = still_pending;
}

/// A result delivered later is not necessarily a command run later. Require
/// failure -> verifying call -> result in distinct source spans. Equal-index
/// events inside a bundled result cannot invent an intervening repair, and
/// cannot produce the reversed slice that previously panicked during import.
fn repair_span_range(
    failure_index: usize,
    call_index: usize,
    result_index: usize,
) -> Option<std::ops::Range<usize>> {
    (failure_index < call_index && call_index < result_index).then(|| failure_index + 1..call_index)
}

/// A readable command label plus a private invocation binding. The label is
/// for incident-card presentation only; it never establishes proof coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandFamily {
    program: String,
    subcommand: Option<String>,
    verification_scope: Option<String>,
}

impl std::fmt::Display for CommandFamily {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.subcommand {
            Some(subcommand) => write!(formatter, "{} {subcommand}", self.program),
            None => formatter.write_str(&self.program),
        }
    }
}

impl CommandFamily {
    /// Parse the command a tool call ran. Shell wrappers, `cd ... &&`
    /// prefixes, environment assignments and `rch exec --` are looked through
    /// so `cd repo && RUST_LOG=1 cargo test x` is `cargo test`.
    pub(crate) fn parse(command: &str) -> Option<Self> {
        let segment = command
            .split("&&")
            .flat_map(|part| part.split(';'))
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .last()?;
        let segment = segment.split('|').next()?.trim();
        let mut tokens = segment
            .split_whitespace()
            .map(|token| token.trim_matches(|ch| ch == '"' || ch == '\''))
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        loop {
            let Some(first) = tokens.first().copied() else {
                return None;
            };
            let is_assignment = first.contains('=') && !first.starts_with('-');
            if is_assignment || matches!(first, "sudo" | "time" | "env" | "nice" | "command") {
                tokens.remove(0);
                continue;
            }
            if program_name(first) == "rch" {
                let separator = tokens.iter().position(|token| *token == "--")?;
                tokens.drain(..=separator);
                continue;
            }
            break;
        }
        let program = program_name(tokens.first()?).to_owned();
        let subcommand = tokens
            .iter()
            .skip(1)
            .find(|token| !token.starts_with('-') && !token.starts_with('+'))
            .filter(|_| takes_subcommand(&program))
            .map(|token| (*token).to_owned());
        Some(Self {
            program,
            subcommand,
            verification_scope: proof_command(command)
                .map(|command| blake3::hash(command.as_bytes()).to_hex().to_string()),
        })
    }

    fn bind_context(&mut self, context: Option<&str>) {
        self.verification_scope =
            self.verification_scope
                .as_deref()
                .zip(context)
                .map(|(command, context)| {
                    let mut hasher = blake3::Hasher::new();
                    hasher.update(b"ee.cass.repair_invocation.v1\0");
                    for field in [command, context] {
                        hasher.update(&(field.len() as u64).to_le_bytes());
                        hasher.update(field.as_bytes());
                    }
                    hasher.finalize().to_hex().to_string()
                });
    }

    /// Require an exact invocation and context. Even a rustc error may be in
    /// a bin, integration test, feature, package or target that a different
    /// successful cargo command never compiled. Do not infer set inclusion
    /// from a family name, nor equate two missing bindings.
    fn verifies(&self, failed: &Self) -> bool {
        self.program == failed.program
            && self.subcommand == failed.subcommand
            && self
                .verification_scope
                .as_ref()
                .is_some_and(|scope| failed.verification_scope.as_ref() == Some(scope))
    }
}

/// Deliberately not a shell parser. Only a literal command, optionally behind
/// literal `cd ... &&` prefixes, can be a whole-process success proof. A
/// pipeline's exit may belong to `tail`, and shell lists, substitutions and
/// dynamic paths cannot be certified by a single tool-level status. Such
/// commands still contribute failure fingerprints, but not repair proofs.
/// Keep all argument bytes: package, target, features, filters, toolchain,
/// profile and environment assignments must not collapse to a family label.
fn proof_command(command: &str) -> Option<&str> {
    if command.chars().any(|ch| {
        ch.is_control()
            || matches!(
                ch,
                '|' | ';' | '$' | '`' | '<' | '>' | '(' | ')' | '{' | '}' | '#' | '~' | '\\'
            )
    }) {
        return None;
    }
    let command = command.trim();
    let mut segments = command.split("&&").peekable();
    while let Some(segment) = segments.next() {
        let words = segment.split_whitespace().collect::<Vec<_>>();
        if words.is_empty() || segment.contains('&') {
            return None;
        }
        if segments.peek().is_some() {
            let path = match words.as_slice() {
                ["cd", path] | ["cd", "--", path] => *path,
                _ => return None,
            };
            if path.starts_with('-') || path.contains(['*', '?', '[', ']', '\'', '"']) {
                return None;
            }
        } else if words.iter().any(|word| {
            matches!(
                *word,
                "--help" | "-h" | "--version" | "-V" | "--list" | "--dry-run" | "--no-run"
            )
        }) {
            return None;
        }
    }
    (!command.is_empty()).then_some(command)
}

/// Bind the tool and its argument structure, including cwd/workdir, env,
/// shell, login mode and unknown future options. Only observational controls
/// are excluded. In particular, argv boundaries are preserved even though the
/// readable command label joins argv with spaces. Hashes, not paths or env
/// values, travel with an arc. Map-order differences are conservative misses.
fn call_context(tool: &str, arguments: &Value) -> Option<String> {
    let fields = arguments.as_object()?;
    let selected = fields
        .iter()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "description" | "yield_time_ms" | "max_output_tokens" | "timeout_ms" | "timeout"
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let encoded = serde_json::to_vec(&(tool, selected)).ok()?;
    Some(blake3::hash(&encoded).to_hex().to_string())
}

fn program_name(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

fn takes_subcommand(program: &str) -> bool {
    matches!(
        program,
        "cargo" | "npm" | "pnpm" | "yarn" | "go" | "git" | "make" | "just" | "uv" | "poetry"
    )
}

/// One tool record found in a transcript line.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ToolEvent {
    Call {
        id: String,
        command: Option<String>,
        context: Option<String>,
        can_poll: bool,
    },
    Poll {
        id: String,
        process_id: i32,
        read_only: bool,
    },
    Result {
        id: Option<String>,
        output: ToolOutput,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ToolOutput {
    text: String,
    is_error: Option<bool>,
    exit_code: Option<i64>,
}

impl ToolOutput {
    /// Failure and completion are distinct: unrecognized stdout is neither.
    fn failed(&self) -> bool {
        outcome::failed(self)
    }

    fn succeeded(&self) -> bool {
        outcome::succeeded(self)
    }
}

/// Tool calls and results in one transcript excerpt: Claude Code
/// `tool_use`/`tool_result` content blocks and Codex
/// `function_call`/`function_call_output` payloads.
fn tool_events(excerpt: &str) -> Vec<ToolEvent> {
    // Reject incomplete windows as a whole; a valid prefix must not turn a
    // malformed or truncated trailing result into successful repair evidence.
    if excerpt.len() > 1024 * 1024 {
        return Vec::new();
    }
    let mut events = Vec::new();
    for record in serde_json::Deserializer::from_str(excerpt).into_iter::<Value>() {
        let Ok(record) = record else {
            return Vec::new();
        };
        let content = record
            .get("message")
            .and_then(|message| message.get("content"))
            .or_else(|| record.get("content"));
        if let Some(blocks) = content.and_then(Value::as_array) {
            for block in blocks {
                if let Some(event) = claude_block_event(block) {
                    events.push(event);
                }
            }
        }
        if let Some(payload) = record.get("payload").filter(|payload| payload.is_object()) {
            if let Some(event) = codex_payload_event(payload) {
                events.push(event);
            }
        }
    }
    events
}

fn claude_block_event(block: &Value) -> Option<ToolEvent> {
    match block.get("type").and_then(Value::as_str)? {
        "tool_use" => Some(ToolEvent::Call {
            id: block.get("id").and_then(Value::as_str)?.to_owned(),
            command: block
                .get("input")
                .and_then(|input| input.get("command"))
                .and_then(command_text),
            context: block.get("input").and_then(|input| {
                call_context(
                    block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool_use"),
                    input,
                )
            }),
            can_poll: false,
        }),
        "tool_result" => Some(ToolEvent::Result {
            id: block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            output: ToolOutput {
                text: content_text(block.get("content")?)?,
                is_error: block
                    .get("is_error")
                    .map(|value| value.as_bool().unwrap_or(true)),
                exit_code: None,
            },
        }),
        _ => None,
    }
}

fn codex_payload_event(payload: &Value) -> Option<ToolEvent> {
    let call_id = payload.get("call_id").and_then(Value::as_str);
    match payload.get("type").and_then(Value::as_str)? {
        "function_call" | "local_shell_call" | "custom_tool_call" => {
            let arguments = match payload.get("arguments") {
                Some(Value::String(arguments)) => serde_json::from_str::<Value>(arguments).ok(),
                Some(arguments) => Some(arguments.clone()),
                None => payload.get("action").cloned(),
            };
            let tool_name = payload.get("name").and_then(Value::as_str);
            if matches!(tool_name, Some("write_stdin" | "functions.write_stdin")) {
                let arguments = arguments.as_ref()?;
                let process_id = i32::try_from(arguments.get("session_id")?.as_i64()?).ok()?;
                let read_only = arguments
                    .get("chars")
                    .is_none_or(|value| value.as_str() == Some(""))
                    && arguments.as_object()?.keys().all(|key| {
                        matches!(
                            key.as_str(),
                            "session_id" | "chars" | "yield_time_ms" | "max_output_tokens"
                        )
                    });
                return Some(ToolEvent::Poll {
                    id: call_id?.to_owned(),
                    process_id,
                    read_only,
                });
            }
            let command = arguments.as_ref().and_then(command_argument);
            Some(ToolEvent::Call {
                id: call_id?.to_owned(),
                command,
                context: arguments.as_ref().and_then(|arguments| {
                    call_context(
                        payload
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("local_shell_call"),
                        arguments,
                    )
                }),
                can_poll: matches!(tool_name, Some("exec_command" | "functions.exec_command")),
            })
        }
        "function_call_output" | "local_shell_call_output" | "custom_tool_call_output" => {
            Some(ToolEvent::Result {
                id: call_id.map(str::to_owned),
                // Consume a known call even when its result is malformed. A
                // later duplicate must not replace that refusal with success.
                output: payload
                    .get("output")
                    .and_then(outcome::codex_output)
                    .unwrap_or_else(|| ToolOutput {
                        is_error: Some(true),
                        ..ToolOutput::default()
                    }),
            })
        }
        _ => None,
    }
}

/// Codex exec_command uses `cmd`; older shell tools use `command`. Multiple
/// command fields are ambiguous, not a reason to choose one by map order.
fn command_argument(arguments: &Value) -> Option<String> {
    match (arguments.get("command"), arguments.get("cmd")) {
        (Some(command), None) | (None, Some(command)) => command_text(command),
        _ => None,
    }
}

/// A command given as a string, or as an argv array such as
/// `["bash", "-lc", "cargo test"]`.
fn command_text(value: &Value) -> Option<String> {
    if let Some(command) = value.as_str() {
        return Some(command.to_owned());
    }
    let argv = value
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?;
    match argv.as_slice() {
        [shell, flag, script]
            if matches!(program_name(shell), "bash" | "sh" | "zsh")
                && matches!(*flag, "-c" | "-lc") =>
        {
            Some((*script).to_owned())
        }
        _ => Some(argv.join(" ")),
    }
}

fn content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match block {
                Value::String(text) => Some(text.as_str()),
                Value::Object(_) if block.get("type").and_then(Value::as_str) == Some("text") => {
                    block.get("text").and_then(Value::as_str)
                }
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join("\n")),
        _ => None,
    }
}

/// Structured diagnostics in a failing tool result: native error envelopes,
/// every distinct rustc error code, or the first failing test. Generic
/// non-zero exits (a `grep` that matched nothing) carry no reusable error
/// class and record nothing. Messages are secret-redacted before they are
/// canonicalized, and only masked signatures are ever stored.
pub(crate) fn failure_diagnostics(output: &ToolOutput) -> Vec<CanonicalDiagnostic> {
    if output.text.trim_start().starts_with(['{', '[']) {
        // A diagnostic-looking string inside arbitrary JSON is data, not a
        // compiler failure. Structured output must match its complete schema.
        return structured_error_diagnostics(&output.text);
    }
    let text = bounded(&output.text);
    let mut seen = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for line in text.lines() {
        let Some((code, message)) = rustc_error_line(line) else {
            continue;
        };
        if seen.insert(code.to_owned()) {
            let message = crate::policy::redact_secret_like_content(message).content;
            diagnostics.push(from_rustc(Some(code), &message));
            if diagnostics.len() == MAX_DIAGNOSTICS_PER_FAILURE {
                break;
            }
        }
    }
    if !diagnostics.is_empty() {
        return diagnostics;
    }
    if text.contains("test result: FAILED") {
        if let Some(line) = text.lines().map(str::trim).find(|line| {
            line.starts_with("test ") && line.ends_with("FAILED") || line.contains("panicked at")
        }) {
            let message = crate::policy::redact_secret_like_content(line).content;
            diagnostics.push(from_cargo(None, &format!("test failed: {message}")));
        }
    }
    diagnostics
}

#[derive(serde::Deserialize)]
struct StructuredErrorEnvelope {
    schema: String,
    success: Option<bool>,
    exit_code: Option<i64>,
    verdict: Option<String>,
    status: Option<String>,
    verification_attribution: Option<String>,
    abstention_reason: Option<Value>,
    timed_out: Option<bool>,
    error: Option<StructuredEeError>,
    known_blocker: Option<StructuredRchBlocker>,
    #[serde(default)]
    degraded_codes: Vec<String>,
    stderr_tail: Option<String>,
}

#[derive(serde::Deserialize)]
struct StructuredEeError {
    code: String,
    message: String,
}

#[derive(serde::Deserialize)]
struct StructuredRchBlocker {
    schema: Option<String>,
    blocker_kind: Option<String>,
}

/// Read one complete error document. Typed deserialization rejects duplicate
/// schema, success, code and blocker fields rather than choosing the last one.
/// Neither a nested example nor a valid prefix of truncated JSON is evidence.
fn structured_error_envelope(text: &str) -> Option<StructuredErrorEnvelope> {
    if text.len() > MAX_SCANNED_OUTPUT_BYTES || !text.trim_start().starts_with('{') {
        return None;
    }
    serde_json::from_str(text).ok()
}

fn stable_error_code(code: &str) -> bool {
    (1..=128).contains(&code.len())
        && code.as_bytes()[0].is_ascii_alphabetic()
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Infer the same native fingerprint from a command's complete structured
/// diagnostic that CASS import records. Public readers can reuse this before
/// their free-text fallback, so imported errors and later logs share a key.
pub(crate) fn structured_error_diagnostics(text: &str) -> Vec<CanonicalDiagnostic> {
    let Some(envelope) = structured_error_envelope(text) else {
        return Vec::new();
    };
    match envelope.schema.as_str() {
        "ee.error.v2" if envelope.success != Some(true) => envelope
            .error
            .filter(|error| stable_error_code(&error.code) && !error.message.trim().is_empty())
            .map(|error| {
                let message = crate::policy::redact_secret_like_content(&error.message).content;
                vec![from_ee_error(&error.code, &message)]
            })
            .unwrap_or_default(),
        "ee.rch.verify.v1" if envelope.success == Some(false) => {
            let kind = envelope
                .known_blocker
                .as_ref()
                .filter(|blocker| blocker.schema.as_deref() == Some("ee.rch.known_blocker.v1"))
                .and_then(|blocker| blocker.blocker_kind.as_deref())
                .filter(|kind| stable_error_code(kind))
                .or_else(|| rch_blocker_kind(&envelope.degraded_codes));
            let Some(kind) = kind else {
                return Vec::new();
            };
            let message = envelope
                .stderr_tail
                .as_deref()
                .and_then(|tail| tail.lines().map(str::trim).find(|line| !line.is_empty()))
                .unwrap_or(kind);
            let message = crate::policy::redact_secret_like_content(message).content;
            // These verifier envelopes identify a blocker kind, but do not
            // declare a diagnostic stage. Do not invent one from command_kind.
            vec![from_rch_blocker(kind, "", &message)]
        }
        _ => Vec::new(),
    }
}

/// Older verifier reports predate typed known_blocker records. Recognize only
/// the explicit blocker codes emitted by scripts/rch_verify.sh, in that
/// producer's priority order; generic remote-command failure is not a class.
fn rch_blocker_kind(codes: &[String]) -> Option<&'static str> {
    [
        (
            "rch_verify_cargo_workspace_inheritance_blocked",
            "cargo_workspace_inheritance",
        ),
        (
            "rch_verify_cargo_path_dependency_version_blocked",
            "cargo_path_dependency_version",
        ),
        (
            "rch_verify_client_daemon_version_skew",
            "client_daemon_version_skew",
        ),
        (
            "rch_verify_remote_checkout_incomplete",
            "remote_checkout_incomplete",
        ),
        ("rch_verify_worker_disk_full", "worker_disk_full"),
        (
            "rch_verify_all_workers_preflight_failed",
            "all_workers_preflight_failed",
        ),
        (
            "rch_verify_worker_health_threshold_blocked",
            "worker_health_threshold",
        ),
        (
            "rch_verify_remote_transport_timeout",
            "remote_transport_timeout",
        ),
        ("rch_verify_capacity_or_timeout", "capacity_or_timeout"),
        ("rch_verify_no_worker_capacity", "no_worker_capacity"),
        ("rch_verify_topology_blocked", "topology_blocked"),
        (
            "rch_verify_local_fallback_refused",
            "local_fallback_refused",
        ),
    ]
    .into_iter()
    .find_map(|(code, kind)| {
        codes
            .iter()
            .any(|observed| observed == code)
            .then_some(kind)
    })
}

/// The native envelope itself can contradict an optimistic tool exit. Even a
/// failure with no supported diagnostic code must never certify another fix.
fn structured_error_reports_failure(text: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct Status {
        schema: String,
        success: Option<bool>,
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES || !text.trim_start().starts_with('{') {
        return false;
    }
    serde_json::from_str::<Status>(text).is_ok_and(|status| {
        status.schema == "ee.error.v2"
            || (status.schema == "ee.rch.verify.v1" && status.success == Some(false))
    })
}

/// A successful wrapper can print a refusal or an abstention. Inspect native
/// completion at the end of an invocation without latching an incomplete JSON
/// chunk as failure while more bytes are still arriving.
fn structured_output_allows_completion(text: &str) -> bool {
    let start = text.trim_start();
    if !start.starts_with(['{', '[']) {
        return true;
    }
    if text.len() > MAX_SCANNED_OUTPUT_BYTES {
        return false;
    }
    if start.starts_with('[') {
        // Arrays may contain example diagnostics; they do not declare a
        // native command status. Still require complete JSON for proof.
        return serde_json::from_str::<Value>(text).is_ok();
    }
    #[derive(serde::Deserialize)]
    struct Schema {
        schema: Option<String>,
    }
    let Ok(schema) = serde_json::from_str::<Schema>(text) else {
        // This also rejects duplicate schema fields before any map parser can
        // hide a native failure behind a second, unrelated schema value.
        return false;
    };
    match schema.schema.as_deref() {
        Some("ee.error.v2") => false,
        Some("ee.rch.verify.v1") => structured_error_envelope(text).is_some_and(|envelope| {
            envelope.success == Some(true)
                && envelope.exit_code == Some(0)
                && envelope
                    .verdict
                    .as_deref()
                    .is_none_or(|verdict| verdict == "passed")
                && envelope
                    .status
                    .as_deref()
                    .is_none_or(|status| status == "remote_pass")
                && envelope
                    .verification_attribution
                    .as_deref()
                    .is_none_or(|attribution| !attribution.starts_with("not_run"))
                && envelope.abstention_reason.is_none()
                && envelope.timed_out != Some(true)
                && envelope.known_blocker.is_none()
                && rch_blocker_kind(&envelope.degraded_codes).is_none()
                && !envelope.degraded_codes.iter().any(|code| {
                    matches!(
                        code.as_str(),
                        "rch_verify_remote_command_failed"
                            | "rch_verify_local_fallback_detected"
                            | "rch_verify_remote_marker_missing"
                            | "rch_verify_not_offloaded"
                            | "rch_verify_build_admission_denied"
                            | "rch_verify_known_blocker_active"
                            | "rch_verify_proof_broker_reuse_existing"
                    ) || (code.starts_with("rch_verify_proof_broker_")
                        && !matches!(
                            code.as_str(),
                            "rch_verify_proof_broker_bypassed"
                                | "rch_verify_proof_broker_source_state_mismatch"
                        ))
                })
        }),
        Some("ee.response.v2") => structured_error_envelope(text)
            .is_some_and(|envelope| envelope.success == Some(true) && envelope.error.is_none()),
        _ => true,
    }
}

/// The line an agent would read first in a failing output: the first rustc
/// error (with the `-->` location that follows it), else the first failing
/// test or panic. Secret-redacted; never the whole log.
fn symptom_line(output: &ToolOutput) -> Option<String> {
    if output.text.trim_start().starts_with(['{', '[']) {
        // Native structured errors have a useful masked description even when
        // their raw envelope is not admissible reader content. Cards show that
        // description, never the schema, private verifier paths or raw tails.
        return structured_error_diagnostics(&output.text)
            .first()
            .map(|diagnostic| {
                format!(
                    "{}: {}",
                    diagnostic.layered_key().key,
                    diagnostic.message_template
                )
            });
    }
    let text = bounded(&output.text);
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    let line = if let Some(index) = lines
        .iter()
        .position(|line| rustc_error_line(line).is_some())
    {
        let location = lines[index + 1..lines.len().min(index + 3)]
            .iter()
            .find_map(|line| line.strip_prefix("--> "));
        match location {
            Some(location) => format!("{} ({})", lines[index], location.trim()),
            None => lines[index].to_owned(),
        }
    } else {
        (*lines.iter().find(|line| {
            (line.starts_with("test ") && line.ends_with("FAILED")) || line.contains("panicked at")
        })?)
        .to_owned()
    };
    Some(crate::policy::redact_secret_like_content(&line).content)
}

fn bounded(text: &str) -> &str {
    if text.len() <= MAX_SCANNED_OUTPUT_BYTES {
        return text;
    }
    let mut end = MAX_SCANNED_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `error[E0277]: the trait bound ...` -> (`E0277`, `the trait bound ...`).
fn rustc_error_line(line: &str) -> Option<(&str, &str)> {
    let start = line.find("error[E")? + "error[".len();
    let rest = &line[start..];
    let end = rest.find(']')?;
    let code = &rest[..end];
    if code.len() != 5 || !code[1..].bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let message = rest[end + 1..].trim_start_matches(':').trim();
    Some((code, message))
}

#[cfg(test)]
#[path = "cass_error_recall_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cass_error_recall_proof_tests.rs"]
mod proof_tests;
