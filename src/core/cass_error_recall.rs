//! Feed imported CASS failure->fix arcs into the error-fingerprint store
//! (bd-reality-core-convergence-1azkt.60).
//!
//! `ee diagnose-error` and `ee pack --error-log` recall an error class by its
//! layered fingerprint key (ADR 0057). Until now only a manual
//! `ee diagnose-error --record` populated that store, while every imported
//! session carried real tool failures and the fixes that followed them. This
//! derivation reads one imported session in transcript order, pairs each
//! failing tool result with the first later success of the same command
//! family, and records:
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
use crate::core::error_recall::{CanonicalDiagnostic, from_cargo, from_rustc};
use crate::db::{DbConnection, Result, StoredEvidenceSpan, StoredSession};

/// Actor recorded on CASS-derived repair links.
pub const CASS_ERROR_RECALL_ACTOR: &str = "ee import cass";

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
    /// Admitted assistant turns between the failure and its proof, newest
    /// first.
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
    compile_error: bool,
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

/// Walk a session in transcript order and pair failures with the success that
/// verified their fix. Pure over the given rows.
pub(crate) fn session_failure_arcs(
    workspace_id: &str,
    session: &StoredSession,
    spans: &[StoredEvidenceSpan],
) -> Vec<FailureArc> {
    // Tool-call id -> (command family, index of the call span).
    let mut calls: std::collections::HashMap<String, (CommandFamily, usize)> =
        std::collections::HashMap::new();
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
            match event {
                ToolEvent::Call { id, command } => {
                    if let Some(family) = command.as_deref().and_then(CommandFamily::parse) {
                        calls.insert(id, (family, index));
                    }
                }
                ToolEvent::Result { id, output } => {
                    let Some((family, call_index)) =
                        id.as_deref().and_then(|id| calls.get(id)).cloned()
                    else {
                        continue;
                    };
                    // Both halves feed the derivation: the command decides the
                    // family, the output decides the outcome. Neither may be a
                    // class-B (instruction-risk) record.
                    if !span.is_class_a_derivation_readable(workspace_id, session)
                        || !spans[call_index].is_class_a_derivation_readable(workspace_id, session)
                    {
                        continue;
                    }
                    let diagnostics = failure_diagnostics(&output);
                    if diagnostics.is_empty() {
                        if output.failed() {
                            continue;
                        }
                        resolve_pending(
                            &mut pending,
                            &mut arcs,
                            &family,
                            index,
                            span,
                            spans,
                            workspace_id,
                            session,
                        );
                        continue;
                    }
                    if pending.len() == MAX_PENDING_FAILURES {
                        arcs.push(pending.remove(0).into_arc(None));
                    }
                    pending.push(PendingFailure {
                        index,
                        failure_id: span.id.clone(),
                        attempt_id: spans[call_index].id.clone(),
                        symptom: symptom_line(&output),
                        compile_error: diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic.canonical_code.is_some()),
                        family,
                        diagnostics,
                    });
                }
            }
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
    success_index: usize,
    success: &StoredEvidenceSpan,
    spans: &[StoredEvidenceSpan],
    workspace_id: &str,
    session: &StoredSession,
) {
    let mut still_pending = Vec::with_capacity(pending.len());
    for failure in pending.drain(..) {
        if !family.verifies(&failure.family, failure.compile_error) {
            still_pending.push(failure);
            continue;
        }
        let repair_ids = spans[failure.index + 1..success_index]
            .iter()
            .rev()
            .filter(|span| {
                span.span_kind == "message"
                    && span.role.as_deref() == Some("assistant")
                    && span.is_derivation_admitted_for_session(workspace_id, session)
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

/// A command reduced to the part that decides whether a later run verifies an
/// earlier failure: the program and its subcommand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandFamily {
    program: String,
    subcommand: Option<String>,
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
        })
    }

    /// Whether a success of `self` verifies a failure of `failed`. A compile
    /// error is fixed by any later successful cargo build of the code; any
    /// other failure needs the same program and subcommand to pass.
    fn verifies(&self, failed: &Self, compile_error: bool) -> bool {
        if self.program != failed.program {
            return false;
        }
        if compile_error && self.program == "cargo" {
            return self.subcommand.as_deref().is_some_and(|subcommand| {
                matches!(
                    subcommand,
                    "build" | "check" | "test" | "clippy" | "run" | "nextest" | "b" | "c" | "t"
                )
            });
        }
        self.subcommand == failed.subcommand
    }
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
    /// Any sign the run failed, structured or not. A result that shows none of
    /// them is the only kind that may verify a fix.
    fn failed(&self) -> bool {
        self.is_error == Some(true)
            || self.exit_code.is_some_and(|code| code != 0)
            || exit_code_line(&self.text).is_some_and(|code| code != 0)
            || bounded(&self.text).lines().any(|line| {
                let line = line.trim_start();
                line.starts_with("error:")
                    || line.starts_with("error[")
                    || line.starts_with("test result: FAILED")
                    || line.contains("panicked at")
            })
    }
}

/// Tool calls and results in one transcript excerpt: Claude Code
/// `tool_use`/`tool_result` content blocks and Codex
/// `function_call`/`function_call_output` payloads.
fn tool_events(excerpt: &str) -> Vec<ToolEvent> {
    let mut events = Vec::new();
    for record in serde_json::Deserializer::from_str(excerpt).into_iter::<Value>() {
        let Ok(record) = record else {
            break;
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
        }),
        "tool_result" => Some(ToolEvent::Result {
            id: block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            output: ToolOutput {
                text: block.get("content").map(content_text).unwrap_or_default(),
                is_error: block.get("is_error").and_then(Value::as_bool),
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
            let command = payload
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                .and_then(|arguments| arguments.get("command").and_then(command_text))
                .or_else(|| {
                    payload
                        .get("action")
                        .and_then(|action| action.get("command"))
                        .and_then(command_text)
                });
            Some(ToolEvent::Call {
                id: call_id?.to_owned(),
                command,
            })
        }
        "function_call_output" | "local_shell_call_output" | "custom_tool_call_output" => {
            let raw = payload.get("output")?;
            let (text, exit_code) = match raw.as_str() {
                Some(text) => match serde_json::from_str::<Value>(text) {
                    Ok(structured) if structured.is_object() => (
                        structured
                            .get("output")
                            .map(content_text)
                            .unwrap_or_default(),
                        structured
                            .get("metadata")
                            .and_then(|metadata| metadata.get("exit_code"))
                            .and_then(Value::as_i64),
                    ),
                    _ => (text.to_owned(), None),
                },
                None => (content_text(raw), None),
            };
            Some(ToolEvent::Result {
                id: call_id.map(str::to_owned),
                output: ToolOutput {
                    text,
                    is_error: None,
                    exit_code,
                },
            })
        }
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

fn content_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| block.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn exit_code_line(text: &str) -> Option<i64> {
    let first = text.lines().find(|line| !line.trim().is_empty())?.trim();
    first
        .strip_prefix("Exit code ")
        .and_then(|code| code.trim().parse().ok())
}

/// Structured diagnostics in a failing tool result: every distinct rustc
/// error code with its message line, else the first failing test. Generic
/// non-zero exits (a `grep` that matched nothing) carry no reusable error
/// class and record nothing. Message lines are secret-redacted before they
/// are canonicalized, and only masked signatures are ever stored.
pub(crate) fn failure_diagnostics(output: &ToolOutput) -> Vec<CanonicalDiagnostic> {
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

/// The line an agent would read first in a failing output: the first rustc
/// error (with the `-->` location that follows it), else the first failing
/// test or panic. Secret-redacted; never the whole log.
fn symptom_line(output: &ToolOutput) -> Option<String> {
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
