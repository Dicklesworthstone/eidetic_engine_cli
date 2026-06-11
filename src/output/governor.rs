//! Output-token governor middleware (ADR 0063, bd-7lvbg.2).
//!
//! Three cooperating pieces, all render-path generic so individual command
//! handlers never reimplement governing:
//!
//! 1. **Token estimator** — wraps the same `tiktoken-rs` cl100k_base encoder
//!    pack budgeting uses ([`crate::pack::estimate_tokens_default`]) so pack
//!    content math and output math agree by construction. Estimation runs
//!    over the serialized JSON string and is memoized per response.
//! 2. **Truncation engine** — given a serialized `ee.response.v2` envelope
//!    and the per-schema truncation-point registry
//!    ([`super::OUTPUT_TRUNCATION_REGISTRY`]), drops trailing whole elements
//!    from the declared truncation point until the estimate fits the
//!    ceiling, then appends an `output_truncated_budget` degraded entry
//!    carrying `droppedCount` + `continuationCursor`. No mid-object
//!    truncation, ever; debug builds assert a serde round-trip of the
//!    governed payload.
//! 3. **Cursor codec** — deterministic encode/decode of `ee.cursor.v1`
//!    (wire form `base64url(payload).base64url(blake3_mac)`), MAC-protected
//!    with a per-workspace key derived via [`derive_workspace_mac_key`].
//!
//! **Zero cost when unused**: when no ceiling is set the estimator is never
//! invoked and the render path is byte-identical — [`govern_if_ceiling`]
//! returns the borrowed input without parsing it. The per-thread
//! [`estimator_invocation_count`] instrumentation counter proves the
//! invariant in tests.
//!
//! **Resume engine** (bd-7lvbg.3): surfaces that accept `--cursor` route the
//! token through [`govern_response_json_with_resume`]. A validated cursor
//! removes the already-emitted elements (the page-1 prefix; round-robin
//! reconstruction for per-section shapes) before the ceiling pass, so a page
//! sequence partitions one DB generation's result set exactly — no
//! duplicates, no gaps. A rejected cursor yields an **empty page** plus a
//! `cursor_invalid` / `cursor_stale` degraded entry (never a restarted page,
//! mirroring the recall house contract), so a sequence can never re-emit
//! items it already delivered.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::models::{DomainError, RESPONSE_SCHEMA_V2};

/// Schema id for the shared continuation-cursor contract (ADR 0063 §3).
pub const CURSOR_SCHEMA_V1: &str = "ee.cursor.v1";

/// Degraded code: trailing elements dropped at the declared truncation
/// point; carries `details.droppedCount` + `details.continuationCursor`.
/// Severity `info`, class `response_time` (ADR 0063 §5).
pub const OUTPUT_TRUNCATED_BUDGET_CODE: &str = "output_truncated_budget";

/// Degraded code: the envelope minimum (or a schema with no declared
/// truncation point) exceeds the ceiling; the response fails closed.
/// Severity `medium`, class `response_time` (ADR 0063 §5).
pub const OUTPUT_BUDGET_UNSATISFIABLE_CODE: &str = "output_budget_unsatisfiable";

/// Degraded code: cursor generation < current DB generation. Severity
/// `low`, class `response_time` (ADR 0063 §5).
pub const CURSOR_STALE_CODE: &str = "cursor_stale";

/// Degraded code: cursor MAC failure, params mismatch, or legacy format.
/// Severity `low`, class `response_time` (ADR 0063 §5).
pub const CURSOR_INVALID_CODE: &str = "cursor_invalid";

/// Documented constant salt (BLAKE3 `derive_key` context string) for the
/// per-workspace cursor MAC key.
///
/// Decision (bd-7lvbg.2): no workspace-scoped secret exists in the DB or
/// config today — the only registered secrets (`EE_PREFLIGHT_BYPASS_SECRET`,
/// `EE_REFLECTION_HMAC_KEY_PATH`, `EE_SERVE_TOKEN`) are env-scoped and owned
/// by other surfaces — so the key is derived deterministically from the
/// canonical workspace scope string plus this constant context, mirroring
/// the `ee preflight bypass v1` derive-key pattern. Cursors are
/// workspace-scoped and short-lived by design (ADR 0063 appendix); the MAC
/// guards against accidental tampering and cross-workspace replay, not
/// against an attacker who can already read the workspace.
pub const CURSOR_MAC_KEY_CONTEXT: &str = "ee.cursor.v1 workspace mac key v1";

/// Anti-pathological byte backstop: in addition to `estimate <= ceiling`,
/// the emitted payload must satisfy `bytes <= ceiling * 8` (ADR 0063 §1).
pub const OUTPUT_BYTE_BACKSTOP_MULTIPLIER: u64 = 8;

/// Iteration cap for the `meta.tokensEstimated` fixed-point stamping loop.
/// The stamped digit count stabilizes in two or three passes in practice;
/// the documented tolerance ("estimate <= ceiling, not a byte guarantee")
/// covers the non-converged tail.
const META_FIXPOINT_MAX_ITERATIONS: usize = 4;

thread_local! {
    /// Per-thread count of real (non-memoized) estimator invocations.
    /// Thread-local so parallel test threads cannot interfere with the
    /// zero-cost-when-unused assertion; governed rendering is always
    /// single-threaded per command.
    static ESTIMATOR_INVOCATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Number of real estimator invocations observed on this thread.
#[must_use]
pub fn estimator_invocation_count() -> u64 {
    ESTIMATOR_INVOCATIONS.with(Cell::get)
}

/// Token estimator memoized per response (ADR 0063 §1).
///
/// Wraps [`crate::pack::estimate_tokens_default`] (the cl100k_base BPE
/// encoder shared with pack budgeting) and memoizes by content hash so the
/// truncation engine's repeated probes over identical candidates are free.
#[derive(Debug, Default)]
pub struct TokenEstimator {
    memo: HashMap<blake3::Hash, u64>,
}

impl TokenEstimator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            memo: HashMap::new(),
        }
    }

    /// Estimate the token count of a serialized payload. Memoized: the
    /// underlying encoder runs at most once per distinct string.
    pub fn estimate(&mut self, serialized: &str) -> u64 {
        let key = blake3::hash(serialized.as_bytes());
        if let Some(cached) = self.memo.get(&key) {
            return *cached;
        }
        ESTIMATOR_INVOCATIONS.with(|count| count.set(count.get().saturating_add(1)));
        let estimate = u64::from(crate::pack::estimate_tokens_default(serialized));
        self.memo.insert(key, estimate);
        estimate
    }
}

/// One declared truncation point: the single array whose trailing whole
/// elements the governor may drop for a given response schema (ADR 0063
/// §2). Pack `data.pack.items[]` is NEVER registered here by hard rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TruncationPoint {
    /// `data.schema` value identifying the surface; empty when the surface
    /// emits only `data.command`.
    pub schema_id: &'static str,
    /// `data.command` value identifying the surface; empty when the
    /// surface is schema-first only.
    pub command: &'static str,
    /// Path under `data` to the droppable array (for per-section points,
    /// the path to the sections array).
    pub array_path: &'static [&'static str],
    /// `true` for `sections[].items[]` shapes (insights): elements are the
    /// per-section `items[]` entries, dropped round-robin from the last
    /// section backwards (ADR 0063 §2).
    pub per_section_items: bool,
    /// Element field used as the cursor `positionKey`; falls back to the
    /// element's original array index when the field is absent.
    pub position_key_field: &'static str,
}

/// Decoded `ee.cursor.v1` payload (ADR 0063 appendix, amended by
/// bd-7lvbg.3). Field order is the canonical wire order; cursors never embed
/// secrets or raw query text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CursorPayload {
    /// Always [`CURSOR_SCHEMA_V1`].
    pub schema: String,
    /// Schema id of the governed response the cursor continues.
    pub target_schema: String,
    /// Workspace DB generation the page sequence was issued at.
    pub db_generation: u64,
    /// Stable ordered-position key of the last emitted element.
    pub position_key: String,
    /// Count of elements still unemitted when the cursor was issued. This —
    /// not `position_key` — is what reconstructs the emitted set on resume:
    /// per-section round-robin shapes (insights) map many drop counts onto
    /// the same last-kept element, so the position key alone is ambiguous.
    /// `position_key` stays as the honesty cross-check (bd-7lvbg.3).
    pub dropped_count: u64,
    /// BLAKE3 of the normalized query/filter parameters.
    pub params_hash: String,
}

/// Why a cursor was rejected by [`decode_cursor`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CursorRejection {
    /// MAC failure, malformed wire form, schema mismatch, params mismatch,
    /// or a generation from the future — emit [`CURSOR_INVALID_CODE`].
    Invalid,
    /// The DB generation advanced after the cursor was issued — emit
    /// [`CURSOR_STALE_CODE`].
    Stale {
        cursor_generation: u64,
        current_generation: u64,
    },
}

/// Derive the deterministic per-workspace cursor MAC key. See
/// [`CURSOR_MAC_KEY_CONTEXT`] for the key-sourcing decision.
#[must_use]
pub fn derive_workspace_mac_key(workspace_scope: &str) -> [u8; 32] {
    blake3::derive_key(CURSOR_MAC_KEY_CONTEXT, workspace_scope.as_bytes())
}

/// BLAKE3 hash of normalized invocation parameters, length-prefixed so
/// `["ab","c"]` and `["a","bc"]` cannot collide. Deterministic across
/// processes for identical parameter vectors.
#[must_use]
pub fn hash_invocation_params<I, S>(params: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut hasher = blake3::Hasher::new();
    for param in params {
        let bytes = param.as_ref().as_bytes();
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Encode a cursor payload into its opaque wire form
/// `base64url(payload).base64url(blake3_mac)`.
pub fn encode_cursor(payload: &CursorPayload, mac_key: &[u8; 32]) -> Result<String, DomainError> {
    let payload_bytes = serde_json::to_vec(payload).map_err(|error| DomainError::Usage {
        message: format!("Failed to serialize continuation cursor: {error}."),
        repair: Some("Re-run without --max-output-tokens and report the failure.".to_string()),
    })?;
    let mac = blake3::keyed_hash(mac_key, &payload_bytes);
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&payload_bytes),
        URL_SAFE_NO_PAD.encode(mac.as_bytes())
    ))
}

/// Decode and validate an `ee.cursor.v1` wire-form cursor.
///
/// Rejections (ADR 0063 §3): MAC failure, malformed encoding, schema
/// mismatch, `paramsHash` mismatch, or a generation from the future are
/// [`CursorRejection::Invalid`]; a cursor issued before the current DB
/// generation is [`CursorRejection::Stale`].
pub fn decode_cursor(
    token: &str,
    mac_key: &[u8; 32],
    expected_params_hash: &str,
    current_generation: u64,
) -> Result<CursorPayload, CursorRejection> {
    let (payload_part, mac_part) = token.split_once('.').ok_or(CursorRejection::Invalid)?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_part)
        .map_err(|_| CursorRejection::Invalid)?;
    let mac_bytes = URL_SAFE_NO_PAD
        .decode(mac_part)
        .map_err(|_| CursorRejection::Invalid)?;
    let mac_array: [u8; 32] = mac_bytes
        .as_slice()
        .try_into()
        .map_err(|_| CursorRejection::Invalid)?;
    // blake3::Hash equality is constant-time, so MAC verification does not
    // leak a comparison-prefix timing channel.
    let expected_mac = blake3::keyed_hash(mac_key, &payload_bytes);
    if expected_mac != blake3::Hash::from(mac_array) {
        return Err(CursorRejection::Invalid);
    }
    let payload: CursorPayload =
        serde_json::from_slice(&payload_bytes).map_err(|_| CursorRejection::Invalid)?;
    if payload.schema != CURSOR_SCHEMA_V1 {
        return Err(CursorRejection::Invalid);
    }
    if payload.params_hash != expected_params_hash {
        return Err(CursorRejection::Invalid);
    }
    if payload.db_generation > current_generation {
        return Err(CursorRejection::Invalid);
    }
    if payload.db_generation < current_generation {
        return Err(CursorRejection::Stale {
            cursor_generation: payload.db_generation,
            current_generation,
        });
    }
    Ok(payload)
}

/// Canonical `output_truncated_budget` degraded entry (severity `info`).
#[must_use]
pub fn truncated_degraded_entry(
    dropped_count: u64,
    continuation_cursor: &str,
    ceiling_tokens: u64,
) -> JsonValue {
    serde_json::json!({
        "code": OUTPUT_TRUNCATED_BUDGET_CODE,
        "severity": "info",
        "message": format!(
            "Dropped {dropped_count} trailing element(s) at the declared truncation point to \
             satisfy the output ceiling of {ceiling_tokens} tokens."
        ),
        "repair": "Resume with --cursor <details.continuationCursor>, or re-run with a larger \
                   --max-output-tokens ceiling or a narrower --fields preset.",
        "details": {
            "droppedCount": dropped_count,
            "continuationCursor": continuation_cursor,
        },
    })
}

/// Canonical `output_budget_unsatisfiable` degraded entry (severity
/// `medium`).
#[must_use]
pub fn unsatisfiable_degraded_entry(
    estimate_tokens: u64,
    ceiling_tokens: u64,
    reason: &str,
) -> JsonValue {
    serde_json::json!({
        "code": OUTPUT_BUDGET_UNSATISFIABLE_CODE,
        "severity": "medium",
        "message": format!(
            "Output budget unsatisfiable: {reason} (estimated {estimate_tokens} tokens against \
             a ceiling of {ceiling_tokens}); payload withheld."
        ),
        "repair": format!(
            "Re-run with --max-output-tokens {estimate_tokens} or higher, or use --fields \
             minimal."
        ),
    })
}

/// Canonical `cursor_stale` degraded entry (severity `low`). Emission
/// anchor for the per-surface cursor wiring (bd-7lvbg.3).
#[must_use]
pub fn cursor_stale_degraded_entry(cursor_generation: u64, current_generation: u64) -> JsonValue {
    serde_json::json!({
        "code": CURSOR_STALE_CODE,
        "severity": "low",
        "message": format!(
            "Continuation cursor was issued at DB generation {cursor_generation} but the \
             workspace is now at generation {current_generation}; pages cannot partition the \
             result set honestly across writes."
        ),
        "repair": "Re-run the command without --cursor to start a fresh page sequence.",
    })
}

/// Canonical `cursor_invalid` degraded entry (severity `low`). Emission
/// anchor for the per-surface cursor wiring (bd-7lvbg.3).
#[must_use]
pub fn cursor_invalid_degraded_entry() -> JsonValue {
    serde_json::json!({
        "code": CURSOR_INVALID_CODE,
        "severity": "low",
        "message": "Continuation cursor failed validation (MAC mismatch, parameter mismatch, \
                    or legacy format).",
        "repair": "Re-run the command without --cursor to start a fresh page sequence.",
    })
}

/// Everything the truncation engine needs to govern one response.
pub struct GovernorContext<'a> {
    /// Declared output ceiling in estimated tokens.
    pub ceiling_tokens: u64,
    /// BLAKE3 of the normalized invocation parameters (cursor field).
    pub params_hash: String,
    /// Per-workspace cursor MAC key (see [`derive_workspace_mac_key`]).
    pub mac_key: [u8; 32],
    /// Lazy workspace DB generation reader; only consulted when truncation
    /// actually engages, so fitting responses never pay a DB open.
    pub db_generation: &'a dyn Fn() -> u64,
}

/// Look up the declared truncation point for a response, preferring the
/// `data.schema` id over the `data.command` fallback.
#[must_use]
pub fn truncation_point_for<'r>(
    registry: &'r [TruncationPoint],
    schema_id: Option<&str>,
    command: Option<&str>,
) -> Option<&'r TruncationPoint> {
    if let Some(schema_id) = schema_id
        && let Some(point) = registry
            .iter()
            .find(|point| !point.schema_id.is_empty() && point.schema_id == schema_id)
    {
        return Some(point);
    }
    if let Some(command) = command {
        return registry
            .iter()
            .find(|point| !point.command.is_empty() && point.command == command);
    }
    None
}

/// Governor entry point for the render chokepoint.
///
/// `ctx == None` (no `--max-output-tokens`, no `EE_MAX_OUTPUT_TOKENS`) is
/// the zero-cost path: the input is returned borrowed, byte-identical,
/// without parsing and without invoking the estimator.
pub fn govern_if_ceiling<'a>(
    json: &'a str,
    ctx: Option<&GovernorContext<'_>>,
    registry: &[TruncationPoint],
) -> Result<Cow<'a, str>, DomainError> {
    match ctx {
        None => Ok(Cow::Borrowed(json)),
        Some(ctx) => govern_response_json(json, ctx, registry).map(Cow::Owned),
    }
}

/// Apply the output-token governor to one serialized response.
///
/// Non-envelope output (human text, raw schema dumps, NDJSON frames) is
/// passed through unchanged: the governor governs `ee.response.v2` machine
/// envelopes (ADR 0063 §4). For envelopes, the fields projection has
/// already been applied by the caller (precedence rule, ADR 0063 §2) and
/// `meta.tokensEstimated` is stamped on every governed response.
pub fn govern_response_json(
    json: &str,
    ctx: &GovernorContext<'_>,
    registry: &[TruncationPoint],
) -> Result<String, DomainError> {
    govern_response_json_with_resume(json, ctx, registry, None)
}

/// [`govern_response_json`] with an optional `--cursor` resume token
/// (bd-7lvbg.3 per-surface wiring).
///
/// A validated cursor removes the elements already emitted by the earlier
/// pages (prefix removal for flat arrays; round-robin reconstruction via the
/// cursor's `droppedCount` for per-section shapes) before the ceiling pass
/// runs, so chained pages partition one generation's result set exactly. A
/// rejected cursor empties the declared truncation point and appends the
/// `cursor_invalid` / `cursor_stale` degraded entry — an empty page, never a
/// restarted one.
pub fn govern_response_json_with_resume(
    json: &str,
    ctx: &GovernorContext<'_>,
    registry: &[TruncationPoint],
    resume_cursor: Option<&str>,
) -> Result<String, DomainError> {
    let Ok(mut original) = serde_json::from_str::<JsonValue>(json) else {
        return Ok(json.to_owned());
    };
    if original
        .as_object()
        .and_then(|object| object.get("schema"))
        .and_then(JsonValue::as_str)
        != Some(RESPONSE_SCHEMA_V2)
    {
        return Ok(json.to_owned());
    }

    if let Some(token) = resume_cursor {
        apply_resume_to_envelope(&mut original, ctx, registry, token);
    }
    govern_envelope(&original, ctx, registry)
}

fn govern_envelope(
    original: &JsonValue,
    ctx: &GovernorContext<'_>,
    registry: &[TruncationPoint],
) -> Result<String, DomainError> {
    let mut estimator = TokenEstimator::new();
    // Sizing pass on a clone so the over-ceiling probe baseline does not
    // carry the meta stamp.
    let mut sizing = original.clone();
    let (serialized, estimate) = finalize_with_meta(&mut sizing, &mut estimator)?;
    if fits(estimate, serialized.len(), ctx.ceiling_tokens) {
        debug_assert_round_trip(&serialized);
        return Ok(serialized);
    }

    let data = original.get("data").and_then(JsonValue::as_object);
    let schema_id = data
        .and_then(|map| map.get("schema"))
        .and_then(JsonValue::as_str);
    let command = data
        .and_then(|map| map.get("command"))
        .and_then(JsonValue::as_str);
    let Some(point) = truncation_point_for(registry, schema_id, command) else {
        let out = fail_closed_unsatisfiable(
            original,
            ctx,
            &mut estimator,
            estimate,
            "this response schema declares no truncation point",
        )?;
        debug_assert_round_trip(&out);
        return Ok(out);
    };

    let total = droppable_element_count(original, point);
    if total <= 1 {
        // Nothing droppable (or dropping everything but the envelope shell
        // would still be required): the envelope minimum is the floor.
        let out = fail_closed_unsatisfiable(
            original,
            ctx,
            &mut estimator,
            estimate,
            "the envelope minimum exceeds the ceiling",
        )?;
        debug_assert_round_trip(&out);
        return Ok(out);
    }

    // The DB generation is read exactly once per truncated response (the
    // fitting path never pays it; ADR 0063 §1 cost posture).
    let db_generation = (ctx.db_generation)();

    // Binary search the smallest drop count whose candidate fits. fits() is
    // monotone in the drop count up to tokenizer boundary noise; the linear
    // guard below absorbs that noise deterministically.
    let max_drops = total - 1; // always keep at least one element
    let mut low = 1u64;
    let mut high = max_drops;
    let mut best: Option<u64> = None;
    while low <= high {
        let mid = low + (high - low) / 2;
        let (_, candidate_serialized, candidate_estimate) =
            candidate_with_drops(original, point, mid, ctx, db_generation, &mut estimator)?;
        if fits(
            candidate_estimate,
            candidate_serialized.len(),
            ctx.ceiling_tokens,
        ) {
            best = Some(mid);
            if mid == 0 {
                break;
            }
            high = mid - 1;
        } else {
            low = mid + 1;
        }
    }

    let Some(mut drops) = best else {
        let out = fail_closed_unsatisfiable(
            original,
            ctx,
            &mut estimator,
            estimate,
            "the envelope minimum exceeds the ceiling",
        )?;
        debug_assert_round_trip(&out);
        return Ok(out);
    };

    // Deterministic guard for tokenizer boundary noise: walk forward until
    // the candidate truly fits (bounded by max_drops, already known to be
    // reachable or we fail closed above).
    loop {
        let (_, candidate_serialized, candidate_estimate) =
            candidate_with_drops(original, point, drops, ctx, db_generation, &mut estimator)?;
        if fits(
            candidate_estimate,
            candidate_serialized.len(),
            ctx.ceiling_tokens,
        ) {
            debug_assert_round_trip(&candidate_serialized);
            return Ok(candidate_serialized);
        }
        if drops >= max_drops {
            let out = fail_closed_unsatisfiable(
                original,
                ctx,
                &mut estimator,
                candidate_estimate,
                "the envelope minimum exceeds the ceiling",
            )?;
            debug_assert_round_trip(&out);
            return Ok(out);
        }
        drops += 1;
    }
}

/// `estimate <= ceiling` plus the anti-pathological byte backstop.
const fn fits(estimate_tokens: u64, byte_len: usize, ceiling_tokens: u64) -> bool {
    let byte_cap = ceiling_tokens.saturating_mul(OUTPUT_BYTE_BACKSTOP_MULTIPLIER);
    estimate_tokens <= ceiling_tokens && (byte_len as u64) <= byte_cap
}

fn to_json_string(value: &JsonValue) -> Result<String, DomainError> {
    serde_json::to_string(value).map_err(|error| DomainError::Usage {
        message: format!("Failed to render governed output: {error}."),
        repair: Some("Re-run without --max-output-tokens and report the failure.".to_string()),
    })
}

/// Stamp `meta.tokensEstimated` and converge the estimate to a fixed point
/// (the stamped digits themselves contribute tokens). Returns the final
/// serialized payload and its estimate.
fn finalize_with_meta(
    value: &mut JsonValue,
    estimator: &mut TokenEstimator,
) -> Result<(String, u64), DomainError> {
    let mut estimate = 0u64;
    for _ in 0..META_FIXPOINT_MAX_ITERATIONS {
        set_meta_tokens_estimated(value, estimate);
        let serialized = to_json_string(value)?;
        let next = estimator.estimate(&serialized);
        if next == estimate {
            return Ok((serialized, estimate));
        }
        estimate = next;
    }
    // Non-converged tail: stamp the last measurement and report the true
    // estimate of the emitted bytes (documented tolerance, ADR 0063 §1).
    set_meta_tokens_estimated(value, estimate);
    let serialized = to_json_string(value)?;
    let final_estimate = estimator.estimate(&serialized);
    Ok((serialized, final_estimate))
}

fn set_meta_tokens_estimated(value: &mut JsonValue, estimate: u64) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    match object.get_mut("meta") {
        Some(JsonValue::Object(meta)) => {
            meta.insert("tokensEstimated".to_string(), JsonValue::from(estimate));
        }
        Some(_) => {}
        None => {
            let mut meta = JsonMap::new();
            meta.insert("tokensEstimated".to_string(), JsonValue::from(estimate));
            object.insert("meta".to_string(), JsonValue::Object(meta));
        }
    }
}

/// Count droppable elements at the declared point. Missing or non-array
/// paths count zero (the schema then fails closed as point-less).
fn droppable_element_count(original: &JsonValue, point: &TruncationPoint) -> u64 {
    let Some(target) = resolve_data_path(original, point.array_path) else {
        return 0;
    };
    if point.per_section_items {
        let Some(sections) = target.as_array() else {
            return 0;
        };
        sections
            .iter()
            .map(|section| {
                section
                    .get("items")
                    .and_then(JsonValue::as_array)
                    .map_or(0, Vec::len) as u64
            })
            .sum()
    } else {
        target.as_array().map_or(0, |items| items.len() as u64)
    }
}

fn resolve_data_path<'v>(envelope: &'v JsonValue, path: &[&str]) -> Option<&'v JsonValue> {
    let mut current = envelope.get("data")?;
    for segment in path {
        current = current.get(segment)?;
    }
    Some(current)
}

fn resolve_data_path_mut<'v>(
    envelope: &'v mut JsonValue,
    path: &[&str],
) -> Option<&'v mut JsonValue> {
    let mut current = envelope.get_mut("data")?;
    for segment in path {
        current = current.get_mut(segment)?;
    }
    Some(current)
}

/// Build the governed candidate for a given drop count: clone the pristine
/// envelope, apply the deterministic drop sequence, append the
/// `output_truncated_budget` entry (with cursor), and stamp meta.
fn candidate_with_drops(
    original: &JsonValue,
    point: &TruncationPoint,
    drops: u64,
    ctx: &GovernorContext<'_>,
    db_generation: u64,
    estimator: &mut TokenEstimator,
) -> Result<(JsonValue, String, u64), DomainError> {
    let mut candidate = original.clone();
    let position_key = apply_drops(&mut candidate, point, drops)?;

    let payload = CursorPayload {
        schema: CURSOR_SCHEMA_V1.to_string(),
        target_schema: cursor_target_schema(original, point),
        db_generation,
        position_key,
        dropped_count: drops,
        params_hash: ctx.params_hash.clone(),
    };
    let cursor = encode_cursor(&payload, &ctx.mac_key)?;
    append_degraded_entry(
        &mut candidate,
        truncated_degraded_entry(drops, &cursor, ctx.ceiling_tokens),
    );
    let (serialized, estimate) = finalize_with_meta(&mut candidate, estimator)?;
    Ok((candidate, serialized, estimate))
}

fn cursor_target_schema(original: &JsonValue, point: &TruncationPoint) -> String {
    original
        .pointer("/data/schema")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if point.schema_id.is_empty() {
                format!("ee.command:{}", point.command)
            } else {
                point.schema_id.to_owned()
            }
        })
}

/// Apply `drops` whole-element drops at the declared point and return the
/// `positionKey` of the last kept element (in document order). Empty-string
/// position keys are impossible here because the engine always keeps at
/// least one element.
fn apply_drops(
    candidate: &mut JsonValue,
    point: &TruncationPoint,
    drops: u64,
) -> Result<String, DomainError> {
    let missing_point = || DomainError::Usage {
        message: "Declared truncation point is missing from the response payload.".to_string(),
        repair: Some("Re-run without --max-output-tokens and report the failure.".to_string()),
    };
    let target = resolve_data_path_mut(candidate, point.array_path).ok_or_else(missing_point)?;
    if point.per_section_items {
        let sections = target.as_array_mut().ok_or_else(missing_point)?;
        // Round-robin from the last section backwards (ADR 0063 §2). The
        // per-section positionKey names the last DROPPED element in the
        // engine's deterministic drop order — unlike a "last kept" key it
        // is identical in page-local and full-set coordinates (the drop
        // sequence is preserved across pages), so resume can recompute and
        // verify it from `droppedCount` alone (bd-7lvbg.3).
        let (lens, last_dropped) = round_robin_reduction(&section_item_lens(sections), drops);
        let position_key = last_dropped
            .and_then(|(section_index, item_index)| {
                let items = sections
                    .get(section_index)?
                    .get("items")
                    .and_then(JsonValue::as_array)?;
                Some(element_position_key(
                    items.get(item_index)?,
                    point,
                    item_index,
                ))
            })
            .unwrap_or_default();
        for (index, section) in sections.iter_mut().enumerate() {
            if let Some(items) = section.get_mut("items").and_then(JsonValue::as_array_mut) {
                items.truncate(lens[index]);
            }
        }
        Ok(position_key)
    } else {
        let items = target.as_array_mut().ok_or_else(missing_point)?;
        let total = items.len() as u64;
        let kept = total.saturating_sub(drops) as usize;
        items.truncate(kept);
        let position_key = items
            .last()
            .map(|element| element_position_key(element, point, kept.saturating_sub(1)))
            .unwrap_or_default();
        Ok(position_key)
    }
}

fn element_position_key(element: &JsonValue, point: &TruncationPoint, index: usize) -> String {
    match element.get(point.position_key_field) {
        Some(JsonValue::String(key)) => key.clone(),
        Some(JsonValue::Null) | None => index.to_string(),
        Some(other) => other.to_string(),
    }
}

/// Per-section `items[]` lengths for a per-section truncation point.
fn section_item_lens(sections: &[JsonValue]) -> Vec<usize> {
    sections
        .iter()
        .map(|section| {
            section
                .get("items")
                .and_then(JsonValue::as_array)
                .map_or(0, Vec::len)
        })
        .collect()
}

/// Reduce per-section item lengths by `drops` whole elements, one element
/// per non-empty section per pass, starting from the last section (ADR 0063
/// §2). Returns the kept lengths plus the `(section index, item index)` of
/// the LAST element dropped.
///
/// Drops form a fixed total order over the elements, so the kept set for a
/// smaller drop count strictly contains the kept set for a larger one — the
/// nesting that lets cursor resume reconstruct page boundaries from
/// `droppedCount` alone — and the drop sequence over a page remainder is a
/// prefix of the drop sequence over the full set, which is what makes the
/// last-dropped element a coordinate-stable cursor `positionKey`
/// (bd-7lvbg.3).
fn round_robin_reduction(lens: &[usize], drops: u64) -> (Vec<usize>, Option<(usize, usize)>) {
    let mut kept = lens.to_vec();
    let mut remaining = drops;
    let mut last_dropped = None;
    while remaining > 0 {
        let mut progressed = false;
        for index in (0..kept.len()).rev() {
            if remaining == 0 {
                break;
            }
            if kept[index] > 0 {
                kept[index] -= 1;
                last_dropped = Some((index, kept[index]));
                remaining -= 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    (kept, last_dropped)
}

/// Resolve a `--cursor` resume token against the parsed envelope, mutating
/// it in place (bd-7lvbg.3).
///
/// Valid cursor: the already-emitted elements are removed so the remaining
/// page sequence partitions the result set exactly. Rejected cursor: the
/// declared truncation point is emptied (an empty page, never a restarted
/// one — recall house contract) and the matching degraded entry appended.
/// Reading the DB generation is unconditional here: resume is explicitly
/// requested, so the generation read is part of the contract, unlike the
/// lazy ceiling path.
fn apply_resume_to_envelope(
    envelope: &mut JsonValue,
    ctx: &GovernorContext<'_>,
    registry: &[TruncationPoint],
    token: &str,
) {
    let data = envelope.get("data").and_then(JsonValue::as_object);
    let schema_id = data
        .and_then(|map| map.get("schema"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let command = data
        .and_then(|map| map.get("command"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned);
    let Some(point) = truncation_point_for(registry, schema_id.as_deref(), command.as_deref())
    else {
        // No declared truncation point means this surface never issued a
        // cursor; reject without emptying (there is no page array to empty)
        // so the defensive path stays observable instead of destructive.
        append_degraded_entry(envelope, cursor_invalid_degraded_entry());
        return;
    };
    let current_generation = (ctx.db_generation)();
    match decode_cursor(token, &ctx.mac_key, &ctx.params_hash, current_generation) {
        Err(CursorRejection::Invalid) => {
            empty_truncation_point(envelope, point);
            append_degraded_entry(envelope, cursor_invalid_degraded_entry());
        }
        Err(CursorRejection::Stale {
            cursor_generation,
            current_generation,
        }) => {
            empty_truncation_point(envelope, point);
            append_degraded_entry(
                envelope,
                cursor_stale_degraded_entry(cursor_generation, current_generation),
            );
        }
        Ok(payload) => {
            let target_matches = payload.target_schema == cursor_target_schema(envelope, point);
            if !target_matches || !apply_resume_drop(envelope, point, &payload) {
                empty_truncation_point(envelope, point);
                append_degraded_entry(envelope, cursor_invalid_degraded_entry());
            }
        }
    }
}

/// Remove the elements pages 1..N already emitted, leaving exactly the
/// unemitted remainder. Returns `false` when the cursor's `droppedCount` /
/// `positionKey` pair does not honestly describe this result set (the
/// caller then rejects as `cursor_invalid`).
fn apply_resume_drop(
    envelope: &mut JsonValue,
    point: &TruncationPoint,
    payload: &CursorPayload,
) -> bool {
    let Some(target) = resolve_data_path_mut(envelope, point.array_path) else {
        return false;
    };
    if point.per_section_items {
        let Some(sections) = target.as_array_mut() else {
            return false;
        };
        let lens = section_item_lens(sections);
        let total: usize = lens.iter().sum();
        let Ok(remaining) = usize::try_from(payload.dropped_count) else {
            return false;
        };
        // The engine always keeps at least one element when it issues a
        // cursor, so a droppedCount of zero or >= total cannot be honest.
        if remaining == 0 || remaining >= total {
            return false;
        }
        let (kept, last_dropped) = round_robin_reduction(&lens, payload.dropped_count);
        // Honesty cross-check: the cursor's positionKey must name the last
        // element the reduction withholds (drop-order coordinates, stable
        // across pages — see round_robin_reduction).
        let boundary_element = last_dropped.and_then(|(section_index, item_index)| {
            sections
                .get(section_index)?
                .get("items")
                .and_then(JsonValue::as_array)?
                .get(item_index)
                .map(|element| (element, item_index))
        });
        let Some((element, item_index)) = boundary_element else {
            return false;
        };
        if !position_key_honest(element, point, item_index, &payload.position_key) {
            return false;
        }
        for (index, section) in sections.iter_mut().enumerate() {
            if let Some(items) = section.get_mut("items").and_then(JsonValue::as_array_mut) {
                items.drain(..kept[index]);
            }
        }
        true
    } else {
        let Some(items) = target.as_array_mut() else {
            return false;
        };
        let total = items.len();
        let Ok(remaining) = usize::try_from(payload.dropped_count) else {
            return false;
        };
        if remaining == 0 || remaining >= total {
            return false;
        }
        let kept = total - remaining;
        if !position_key_honest(&items[kept - 1], point, kept - 1, &payload.position_key) {
            return false;
        }
        items.drain(..kept);
        true
    }
}

/// Whether the cursor's `positionKey` honestly names the recomputed
/// boundary element.
///
/// Elements that lack the declared key field fall back to their array index
/// at ISSUE time — and a page-2 cursor is issued in page-local coordinates,
/// which a later resume (recomputing in full-set coordinates) cannot
/// reproduce. The cursor is already MAC-protected, params-bound, and
/// generation-bound, so for fallback-key elements `droppedCount` is the
/// sole (and sufficient) authority and the positional cross-check is
/// skipped; elements that carry the declared field must match exactly.
fn position_key_honest(
    element: &JsonValue,
    point: &TruncationPoint,
    index: usize,
    claimed_key: &str,
) -> bool {
    match element.get(point.position_key_field) {
        Some(JsonValue::Null) | None => true,
        _ => element_position_key(element, point, index) == claimed_key,
    }
}

/// Empty the declared truncation point for a rejected-cursor page: flat
/// arrays are cleared; per-section shapes keep their section scaffolding
/// with every `items[]` cleared.
fn empty_truncation_point(envelope: &mut JsonValue, point: &TruncationPoint) {
    let Some(target) = resolve_data_path_mut(envelope, point.array_path) else {
        return;
    };
    if point.per_section_items {
        if let Some(sections) = target.as_array_mut() {
            for section in sections {
                if let Some(items) = section.get_mut("items").and_then(JsonValue::as_array_mut) {
                    items.clear();
                }
            }
        }
    } else if let Some(items) = target.as_array_mut() {
        items.clear();
    }
}

/// Append a degraded entry where the surface already reports degradations:
/// `data.degraded[]` when present, else the top-level envelope `degraded[]`
/// (created when absent).
fn append_degraded_entry(envelope: &mut JsonValue, entry: JsonValue) {
    if let Some(data_degraded) = envelope
        .get_mut("data")
        .and_then(|data| data.get_mut("degraded"))
        .and_then(JsonValue::as_array_mut)
    {
        data_degraded.push(entry);
        return;
    }
    let Some(object) = envelope.as_object_mut() else {
        return;
    };
    match object.get_mut("degraded") {
        Some(JsonValue::Array(degraded)) => degraded.push(entry),
        _ => {
            object.insert("degraded".to_string(), JsonValue::Array(vec![entry]));
        }
    }
}

/// Fail closed (ADR 0063 §2): keep the envelope shell, retain only the
/// identifying `data.command` / `data.schema` fields, and report
/// `output_budget_unsatisfiable`. The minimal shell is emitted even when it
/// still exceeds the ceiling (documented floor). Cursor-rejection entries
/// (`cursor_invalid` / `cursor_stale`) survive the shell rebuild: a
/// rejected-resume page that also misses the ceiling must still tell the
/// agent WHY its page sequence ended (bd-7lvbg.3).
fn fail_closed_unsatisfiable(
    original: &JsonValue,
    ctx: &GovernorContext<'_>,
    estimator: &mut TokenEstimator,
    estimate_tokens: u64,
    reason: &str,
) -> Result<String, DomainError> {
    let mut shell = JsonMap::new();
    let mut degraded_entries: Vec<JsonValue> = Vec::new();
    if let Some(object) = original.as_object() {
        for key in ["schema", "success", "fields"] {
            if let Some(value) = object.get(key) {
                shell.insert(key.to_string(), value.clone());
            }
        }
        let mut minimal_data = JsonMap::new();
        if let Some(data) = object.get("data").and_then(JsonValue::as_object) {
            for (key, value) in data {
                if key == "command" || key == "schema" {
                    minimal_data.insert(key.clone(), value.clone());
                }
            }
        }
        shell.insert("data".to_string(), JsonValue::Object(minimal_data));
        for degraded in [
            object.get("degraded"),
            object.get("data").and_then(|data| data.get("degraded")),
        ] {
            if let Some(entries) = degraded.and_then(JsonValue::as_array) {
                degraded_entries.extend(
                    entries
                        .iter()
                        .filter(|entry| {
                            matches!(
                                entry.get("code").and_then(JsonValue::as_str),
                                Some(CURSOR_INVALID_CODE) | Some(CURSOR_STALE_CODE)
                            )
                        })
                        .cloned(),
                );
            }
        }
    }
    degraded_entries.push(unsatisfiable_degraded_entry(
        estimate_tokens,
        ctx.ceiling_tokens,
        reason,
    ));
    shell.insert("degraded".to_string(), JsonValue::Array(degraded_entries));
    let mut value = JsonValue::Object(shell);
    let (serialized, _) = finalize_with_meta(&mut value, estimator)?;
    Ok(serialized)
}

/// Debug-build guarantee from ADR 0063 §2: governed output always
/// round-trips through serde as a JSON object.
fn debug_assert_round_trip(serialized: &str) {
    if cfg!(debug_assertions) {
        let parsed = serde_json::from_str::<JsonValue>(serialized);
        debug_assert!(
            parsed.as_ref().is_ok_and(JsonValue::is_object),
            "governed output must round-trip as a JSON object: {parsed:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::{Config as ProptestConfig, TestCaseError};
    use serde_json::{Value as JsonValue, json};

    use super::{
        CURSOR_INVALID_CODE, CURSOR_SCHEMA_V1, CURSOR_STALE_CODE, CursorPayload, CursorRejection,
        GovernorContext, OUTPUT_BUDGET_UNSATISFIABLE_CODE, OUTPUT_TRUNCATED_BUDGET_CODE,
        TokenEstimator, TruncationPoint, cursor_invalid_degraded_entry,
        cursor_stale_degraded_entry, decode_cursor, derive_workspace_mac_key, encode_cursor,
        estimator_invocation_count, fits, govern_if_ceiling, govern_response_json,
        govern_response_json_with_resume, hash_invocation_params, truncation_point_for,
    };

    type TestResult = Result<(), String>;

    const TEST_REGISTRY: &[TruncationPoint] = &[
        TruncationPoint {
            schema_id: "ee.test.list.v1",
            command: "test list",
            array_path: &["items"],
            per_section_items: false,
            position_key_field: "id",
        },
        TruncationPoint {
            schema_id: "ee.test.sections.v1",
            command: "test sections",
            array_path: &["sections"],
            per_section_items: true,
            position_key_field: "id",
        },
    ];

    fn test_context<'a>(ceiling: u64, generation: &'a dyn Fn() -> u64) -> GovernorContext<'a> {
        GovernorContext {
            ceiling_tokens: ceiling,
            params_hash: hash_invocation_params(["test", "list", "--json"]),
            mac_key: derive_workspace_mac_key("/tmp/test-workspace"),
            db_generation: generation,
        }
    }

    fn list_envelope(item_count: usize) -> String {
        let items: Vec<JsonValue> = (0..item_count)
            .map(|index| {
                json!({
                    "id": format!("item_{index:04}"),
                    "content": format!("deterministic body text for element {index:04}; ")
                        .repeat(4),
                })
            })
            .collect();
        json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {
                "command": "test list",
                "schema": "ee.test.list.v1",
                "items": items,
            },
            "degraded": [],
        })
        .to_string()
    }

    fn parse(json: &str) -> Result<JsonValue, String> {
        serde_json::from_str(json).map_err(|error| format!("parse governed output: {error}"))
    }

    fn kept_item_ids(value: &JsonValue) -> Vec<String> {
        value
            .pointer("/data/items")
            .and_then(JsonValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("id"))
                    .filter_map(JsonValue::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn degraded_entries(value: &JsonValue) -> Vec<JsonValue> {
        let mut entries = Vec::new();
        for pointer in ["/degraded", "/data/degraded"] {
            if let Some(array) = value.pointer(pointer).and_then(JsonValue::as_array) {
                entries.extend(array.iter().cloned());
            }
        }
        entries
    }

    fn degraded_entry_with_code(value: &JsonValue, code: &str) -> Result<JsonValue, String> {
        degraded_entries(value)
            .into_iter()
            .find(|entry| entry.get("code").and_then(JsonValue::as_str) == Some(code))
            .ok_or_else(|| format!("expected a degraded entry with code {code}"))
    }

    #[test]
    fn estimator_is_deterministic_and_agrees_with_pack_budgeting() {
        let text = r#"{"schema":"ee.response.v2","success":true,"data":{"command":"search"}}"#;
        let mut estimator = TokenEstimator::new();
        let first = estimator.estimate(text);
        let second = estimator.estimate(text);
        assert_eq!(first, second, "estimator must be deterministic");
        assert_eq!(
            first,
            u64::from(crate::pack::estimate_tokens_default(text)),
            "governor and pack budgeting must agree on identical text"
        );
    }

    #[test]
    fn estimator_memoizes_per_response() {
        let mut estimator = TokenEstimator::new();
        let before = estimator_invocation_count();
        let _ = estimator.estimate("memoized payload");
        let _ = estimator.estimate("memoized payload");
        let _ = estimator.estimate("memoized payload");
        let after = estimator_invocation_count();
        assert_eq!(
            after - before,
            1,
            "identical strings must hit the encoder exactly once"
        );
    }

    #[test]
    fn zero_cost_when_no_ceiling_is_set() -> TestResult {
        let json = list_envelope(20);
        let before = estimator_invocation_count();
        let governed = govern_if_ceiling(&json, None, TEST_REGISTRY)
            .map_err(|error| format!("ungoverned path must not fail: {error:?}"))?;
        let after = estimator_invocation_count();
        if after != before {
            return Err(format!(
                "estimator invoked {} time(s) without a ceiling",
                after - before
            ));
        }
        if !matches!(governed, std::borrow::Cow::Borrowed(_)) {
            return Err("ungoverned output must be the borrowed input".to_string());
        }
        if governed.as_ref() != json {
            return Err("ungoverned output must be byte-identical".to_string());
        }
        Ok(())
    }

    #[test]
    fn fitting_response_is_stamped_with_tokens_estimated_only() -> TestResult {
        let json = list_envelope(2);
        let generation = || 3u64;
        let ctx = test_context(100_000, &generation);
        let governed = govern_response_json(&json, &ctx, TEST_REGISTRY)
            .map_err(|error| format!("govern: {error:?}"))?;
        let value = parse(&governed)?;
        let stamped = value
            .pointer("/meta/tokensEstimated")
            .and_then(JsonValue::as_u64)
            .ok_or("meta.tokensEstimated must be present under a ceiling")?;
        let mut estimator = TokenEstimator::new();
        if stamped != estimator.estimate(&governed) {
            return Err("meta.tokensEstimated must reflect the final emitted payload".to_string());
        }
        if kept_item_ids(&value).len() != 2 {
            return Err("fitting responses must not be truncated".to_string());
        }
        if !degraded_entries(&value)
            .iter()
            .all(|entry| entry.get("code").and_then(JsonValue::as_str).is_none())
        {
            return Err("fitting responses must not gain degraded entries".to_string());
        }
        Ok(())
    }

    #[test]
    fn truncation_emits_dropped_count_and_decodable_cursor() -> TestResult {
        let json = list_envelope(40);
        let generation = || 7u64;
        let ctx = test_context(600, &generation);
        let governed = govern_response_json(&json, &ctx, TEST_REGISTRY)
            .map_err(|error| format!("govern: {error:?}"))?;
        let value = parse(&governed)?;
        let kept = kept_item_ids(&value);
        if kept.is_empty() || kept.len() >= 40 {
            return Err(format!(
                "expected a strict non-empty prefix, kept {}",
                kept.len()
            ));
        }
        let entry = degraded_entry_with_code(&value, OUTPUT_TRUNCATED_BUDGET_CODE)?;
        if entry.get("severity").and_then(JsonValue::as_str) != Some("info") {
            return Err("output_truncated_budget must be severity info".to_string());
        }
        let dropped = entry
            .pointer("/details/droppedCount")
            .and_then(JsonValue::as_u64)
            .ok_or("details.droppedCount missing")?;
        if dropped != 40 - kept.len() as u64 {
            return Err(format!(
                "droppedCount {dropped} disagrees with kept {}",
                kept.len()
            ));
        }
        let cursor = entry
            .pointer("/details/continuationCursor")
            .and_then(JsonValue::as_str)
            .ok_or("details.continuationCursor missing")?;
        let payload = decode_cursor(cursor, &ctx.mac_key, &ctx.params_hash, 7)
            .map_err(|rejection| format!("cursor must decode: {rejection:?}"))?;
        if payload.target_schema != "ee.test.list.v1" {
            return Err(format!("unexpected targetSchema {}", payload.target_schema));
        }
        if payload.db_generation != 7 {
            return Err(format!("unexpected dbGeneration {}", payload.db_generation));
        }
        let last_kept = kept.last().cloned().unwrap_or_default();
        if payload.position_key != last_kept {
            return Err(format!(
                "positionKey {} must name the last kept element {last_kept}",
                payload.position_key
            ));
        }
        let stamped = value
            .pointer("/meta/tokensEstimated")
            .and_then(JsonValue::as_u64)
            .ok_or("meta.tokensEstimated missing on truncated output")?;
        if stamped > ctx.ceiling_tokens {
            return Err(format!(
                "stamped estimate {stamped} exceeds ceiling {}",
                ctx.ceiling_tokens
            ));
        }
        Ok(())
    }

    #[test]
    fn smaller_budget_output_is_a_prefix_of_larger_budget_output() -> TestResult {
        let json = list_envelope(50);
        let generation = || 1u64;
        let small_ctx = test_context(600, &generation);
        let large_ctx = test_context(1_200, &generation);
        let small = parse(
            &govern_response_json(&json, &small_ctx, TEST_REGISTRY)
                .map_err(|error| format!("govern small: {error:?}"))?,
        )?;
        let large = parse(
            &govern_response_json(&json, &large_ctx, TEST_REGISTRY)
                .map_err(|error| format!("govern large: {error:?}"))?,
        )?;
        let small_ids = kept_item_ids(&small);
        let large_ids = kept_item_ids(&large);
        if small_ids.len() > large_ids.len() {
            return Err("smaller ceiling must not keep more elements".to_string());
        }
        if small_ids.as_slice() != &large_ids[..small_ids.len()] {
            return Err("smaller-budget output must be an item-level prefix".to_string());
        }
        Ok(())
    }

    #[test]
    fn same_input_same_ceiling_is_byte_identical_including_cursor() -> TestResult {
        let json = list_envelope(40);
        let generation = || 4u64;
        let ctx = test_context(600, &generation);
        let first = govern_response_json(&json, &ctx, TEST_REGISTRY)
            .map_err(|error| format!("govern: {error:?}"))?;
        let second = govern_response_json(&json, &ctx, TEST_REGISTRY)
            .map_err(|error| format!("govern: {error:?}"))?;
        if first != second {
            return Err("governed output must be deterministic".to_string());
        }
        Ok(())
    }

    #[test]
    fn point_less_schema_fails_closed_with_unsatisfiable() -> TestResult {
        let json = json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {
                "command": "status",
                "blob": "x".repeat(4096),
            },
        })
        .to_string();
        let generation = || 1u64;
        let ctx = test_context(50, &generation);
        let governed = govern_response_json(&json, &ctx, TEST_REGISTRY)
            .map_err(|error| format!("govern: {error:?}"))?;
        let value = parse(&governed)?;
        let entry = degraded_entry_with_code(&value, OUTPUT_BUDGET_UNSATISFIABLE_CODE)?;
        if entry.get("severity").and_then(JsonValue::as_str) != Some("medium") {
            return Err("output_budget_unsatisfiable must be severity medium".to_string());
        }
        if value.pointer("/data/blob").is_some() {
            return Err("fail-closed output must withhold the oversized payload".to_string());
        }
        if value.pointer("/data/command").and_then(JsonValue::as_str) != Some("status") {
            return Err("fail-closed output must keep data.command".to_string());
        }
        if value.pointer("/meta/tokensEstimated").is_none() {
            return Err("fail-closed output must still stamp meta.tokensEstimated".to_string());
        }
        Ok(())
    }

    #[test]
    fn per_section_items_drop_round_robin_from_the_last_section_backwards() -> TestResult {
        let mut sections = Vec::new();
        for section_index in 0..3 {
            let items: Vec<JsonValue> = (0..4)
                .map(|item_index| {
                    json!({
                        "id": format!("s{section_index}_i{item_index}"),
                        "content": format!(
                            "section {section_index} item {item_index} deterministic body; "
                        )
                        .repeat(4),
                    })
                })
                .collect();
            sections.push(json!({ "name": format!("section_{section_index}"), "items": items }));
        }
        let json = json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {
                "command": "test sections",
                "schema": "ee.test.sections.v1",
                "sections": sections,
            },
            "degraded": [],
        })
        .to_string();
        let generation = || 2u64;
        let ctx = test_context(500, &generation);
        let governed = govern_response_json(&json, &ctx, TEST_REGISTRY)
            .map_err(|error| format!("govern: {error:?}"))?;
        let value = parse(&governed)?;
        let entry = degraded_entry_with_code(&value, OUTPUT_TRUNCATED_BUDGET_CODE)?;
        let dropped = entry
            .pointer("/details/droppedCount")
            .and_then(JsonValue::as_u64)
            .ok_or("details.droppedCount missing")?;
        let lens: Vec<usize> = value
            .pointer("/data/sections")
            .and_then(JsonValue::as_array)
            .map(|sections| {
                sections
                    .iter()
                    .map(|section| {
                        section
                            .get("items")
                            .and_then(JsonValue::as_array)
                            .map_or(0, Vec::len)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if lens.len() != 3 {
            return Err("all sections must survive truncation".to_string());
        }
        let kept_total: usize = lens.iter().sum();
        if kept_total as u64 + dropped != 12 {
            return Err(format!(
                "kept {kept_total} + dropped {dropped} must equal 12"
            ));
        }
        // Round-robin from the last section backwards: earlier sections can
        // never be shorter than later ones by more than one element.
        for window in lens.windows(2) {
            if window[0] + 1 < window[1] {
                return Err(format!(
                    "drop order must favor later sections first: {lens:?}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn cursor_round_trip_preserves_payload() -> TestResult {
        let key = derive_workspace_mac_key("/tmp/test-workspace");
        let payload = CursorPayload {
            schema: CURSOR_SCHEMA_V1.to_string(),
            target_schema: "ee.search.v1".to_string(),
            db_generation: 12,
            position_key: "mem_0042".to_string(),
            dropped_count: 17,
            params_hash: hash_invocation_params(["search", "query text", "--json"]),
        };
        let token = encode_cursor(&payload, &key).map_err(|error| format!("encode: {error:?}"))?;
        let decoded = decode_cursor(&token, &key, &payload.params_hash, 12)
            .map_err(|rejection| format!("decode: {rejection:?}"))?;
        if decoded != payload {
            return Err("cursor round trip must preserve the payload".to_string());
        }
        Ok(())
    }

    #[test]
    fn cursor_encoding_is_deterministic() -> TestResult {
        let key = derive_workspace_mac_key("/tmp/test-workspace");
        let payload = CursorPayload {
            schema: CURSOR_SCHEMA_V1.to_string(),
            target_schema: "ee.search.v1".to_string(),
            db_generation: 3,
            position_key: "mem_0001".to_string(),
            dropped_count: 2,
            params_hash: hash_invocation_params(["search", "q"]),
        };
        let first = encode_cursor(&payload, &key).map_err(|error| format!("encode: {error:?}"))?;
        let second = encode_cursor(&payload, &key).map_err(|error| format!("encode: {error:?}"))?;
        if first != second {
            return Err("cursor encoding must be deterministic".to_string());
        }
        Ok(())
    }

    #[test]
    fn tampered_cursor_is_rejected_as_invalid() -> TestResult {
        let key = derive_workspace_mac_key("/tmp/test-workspace");
        let params_hash = hash_invocation_params(["search", "q"]);
        let payload = CursorPayload {
            schema: CURSOR_SCHEMA_V1.to_string(),
            target_schema: "ee.search.v1".to_string(),
            db_generation: 5,
            position_key: "mem_0009".to_string(),
            dropped_count: 4,
            params_hash: params_hash.clone(),
        };
        let token = encode_cursor(&payload, &key).map_err(|error| format!("encode: {error:?}"))?;

        // Flip one character in the payload half.
        let mut tampered_payload = token.clone();
        let replacement = if tampered_payload.starts_with('A') {
            "B"
        } else {
            "A"
        };
        tampered_payload.replace_range(0..1, replacement);
        if decode_cursor(&tampered_payload, &key, &params_hash, 5) != Err(CursorRejection::Invalid)
        {
            return Err("payload tampering must be rejected as cursor_invalid".to_string());
        }

        // Flip one character in the MAC half.
        let dot = token.find('.').ok_or("wire form must contain a dot")?;
        let mut tampered_mac = token.clone();
        let mac_first = dot + 1;
        let current = &token[mac_first..=mac_first];
        let replacement = if current == "A" { "B" } else { "A" };
        tampered_mac.replace_range(mac_first..=mac_first, replacement);
        if decode_cursor(&tampered_mac, &key, &params_hash, 5) != Err(CursorRejection::Invalid) {
            return Err("MAC tampering must be rejected as cursor_invalid".to_string());
        }

        // Wrong workspace key.
        let other_key = derive_workspace_mac_key("/tmp/other-workspace");
        if decode_cursor(&token, &other_key, &params_hash, 5) != Err(CursorRejection::Invalid) {
            return Err("cross-workspace cursors must be rejected as cursor_invalid".to_string());
        }

        // Params mismatch.
        let other_params = hash_invocation_params(["search", "different query"]);
        if decode_cursor(&token, &key, &other_params, 5) != Err(CursorRejection::Invalid) {
            return Err("params mismatch must be rejected as cursor_invalid".to_string());
        }

        // Legacy / garbage formats.
        for garbage in ["", "not-a-cursor", "a.b.c", "audit:legacy:42"] {
            if decode_cursor(garbage, &key, &params_hash, 5) != Err(CursorRejection::Invalid) {
                return Err(format!("garbage cursor {garbage:?} must be invalid"));
            }
        }
        Ok(())
    }

    #[test]
    fn generation_advance_is_rejected_as_stale() -> TestResult {
        let key = derive_workspace_mac_key("/tmp/test-workspace");
        let params_hash = hash_invocation_params(["search", "q"]);
        let payload = CursorPayload {
            schema: CURSOR_SCHEMA_V1.to_string(),
            target_schema: "ee.search.v1".to_string(),
            db_generation: 5,
            position_key: "mem_0009".to_string(),
            dropped_count: 4,
            params_hash: params_hash.clone(),
        };
        let token = encode_cursor(&payload, &key).map_err(|error| format!("encode: {error:?}"))?;
        match decode_cursor(&token, &key, &params_hash, 9) {
            Err(CursorRejection::Stale {
                cursor_generation: 5,
                current_generation: 9,
            }) => {}
            other => return Err(format!("expected stale rejection, got {other:?}")),
        }
        // A cursor from the future is invalid, not stale.
        if decode_cursor(&token, &key, &params_hash, 3) != Err(CursorRejection::Invalid) {
            return Err("future-generation cursors must be cursor_invalid".to_string());
        }
        Ok(())
    }

    #[test]
    fn cursor_degraded_entry_helpers_carry_codes_and_severities() -> TestResult {
        let stale = cursor_stale_degraded_entry(4, 9);
        if stale.get("code").and_then(JsonValue::as_str) != Some(CURSOR_STALE_CODE) {
            return Err("stale helper must carry cursor_stale".to_string());
        }
        if stale.get("severity").and_then(JsonValue::as_str) != Some("low") {
            return Err("cursor_stale must be severity low".to_string());
        }
        let invalid = cursor_invalid_degraded_entry();
        if invalid.get("code").and_then(JsonValue::as_str) != Some(CURSOR_INVALID_CODE) {
            return Err("invalid helper must carry cursor_invalid".to_string());
        }
        if invalid.get("severity").and_then(JsonValue::as_str) != Some("low") {
            return Err("cursor_invalid must be severity low".to_string());
        }
        Ok(())
    }

    #[test]
    fn registry_lookup_prefers_schema_id_over_command() -> TestResult {
        let by_schema = truncation_point_for(TEST_REGISTRY, Some("ee.test.sections.v1"), None)
            .ok_or("schema-id lookup must hit")?;
        if !by_schema.per_section_items {
            return Err("schema-id lookup resolved the wrong entry".to_string());
        }
        let by_command = truncation_point_for(TEST_REGISTRY, None, Some("test list"))
            .ok_or("command lookup must hit")?;
        if by_command.array_path != ["items"] {
            return Err("command lookup resolved the wrong entry".to_string());
        }
        if truncation_point_for(TEST_REGISTRY, Some("ee.unknown.v1"), Some("unknown")).is_some() {
            return Err("unknown surfaces must have no truncation point".to_string());
        }
        Ok(())
    }

    #[test]
    fn non_envelope_output_passes_through_unchanged() -> TestResult {
        let generation = || 1u64;
        let ctx = test_context(10, &generation);
        for raw in [
            "plain human text\n",
            r#"{"$schema":"https://json-schema.org/draft/2020-12/schema"}"#,
            r#"{"schema":"ee.pack.v2","data":{}}"#,
            "not json at all",
        ] {
            let governed = govern_response_json(raw, &ctx, TEST_REGISTRY)
                .map_err(|error| format!("passthrough must not fail: {error:?}"))?;
            if governed != raw {
                return Err(format!("non-envelope output must pass through: {raw:?}"));
            }
        }
        Ok(())
    }

    fn continuation_cursor(value: &JsonValue) -> Option<String> {
        degraded_entries(value).iter().find_map(|entry| {
            entry
                .pointer("/details/continuationCursor")
                .and_then(JsonValue::as_str)
                .map(str::to_owned)
        })
    }

    fn sections_envelope(section_count: usize, items_per_section: usize) -> String {
        let sections: Vec<JsonValue> = (0..section_count)
            .map(|section_index| {
                let items: Vec<JsonValue> = (0..items_per_section)
                    .map(|item_index| {
                        json!({
                            "id": format!("s{section_index}_i{item_index}"),
                            "content": format!(
                                "section {section_index} item {item_index} deterministic body; "
                            )
                            .repeat(4),
                        })
                    })
                    .collect();
                json!({ "name": format!("section_{section_index}"), "items": items })
            })
            .collect();
        json!({
            "schema": "ee.response.v2",
            "success": true,
            "data": {
                "command": "test sections",
                "schema": "ee.test.sections.v1",
                "sections": sections,
            },
            "degraded": [],
        })
        .to_string()
    }

    fn kept_section_item_ids(value: &JsonValue) -> Vec<String> {
        value
            .pointer("/data/sections")
            .and_then(JsonValue::as_array)
            .map(|sections| {
                sections
                    .iter()
                    .flat_map(|section| {
                        section
                            .get("items")
                            .and_then(JsonValue::as_array)
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(|item| item.get("id"))
                                    .filter_map(JsonValue::as_str)
                                    .map(str::to_owned)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Drain a page sequence to exhaustion: govern, collect ids, resume with
    /// each emitted cursor against the same pristine input, and return the
    /// per-page id lists.
    fn drain_pages(
        json: &str,
        ctx: &GovernorContext<'_>,
        collect: fn(&JsonValue) -> Vec<String>,
    ) -> Result<Vec<Vec<String>>, String> {
        let mut pages = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..64 {
            let governed =
                govern_response_json_with_resume(json, ctx, TEST_REGISTRY, cursor.as_deref())
                    .map_err(|error| format!("govern page {}: {error:?}", pages.len() + 1))?;
            let value = parse(&governed)?;
            pages.push(collect(&value));
            match continuation_cursor(&value) {
                Some(next) => cursor = Some(next),
                None => return Ok(pages),
            }
        }
        Err("page sequence failed to terminate within 64 pages".to_string())
    }

    #[test]
    fn flat_resume_partitions_the_result_set_exactly() -> TestResult {
        let json = list_envelope(40);
        let generation = || 7u64;
        let ctx = test_context(600, &generation);
        let pages = drain_pages(&json, &ctx, kept_item_ids)?;
        if pages.len() < 2 {
            return Err(format!(
                "ceiling 600 over 40 items must paginate, got {} page(s)",
                pages.len()
            ));
        }
        let drained: Vec<String> = pages.iter().flatten().cloned().collect();
        let expected: Vec<String> = (0..40).map(|index| format!("item_{index:04}")).collect();
        if drained != expected {
            return Err(format!(
                "drained sequence must reconstruct the full set exactly once in order; \
                 got {} ids across {} pages",
                drained.len(),
                pages.len()
            ));
        }
        Ok(())
    }

    #[test]
    fn per_section_resume_partitions_the_result_set_exactly() -> TestResult {
        let json = sections_envelope(3, 8);
        let generation = || 2u64;
        let ctx = test_context(500, &generation);
        let pages = drain_pages(&json, &ctx, kept_section_item_ids)?;
        if pages.len() < 2 {
            return Err(format!(
                "ceiling 500 over 24 section items must paginate, got {} page(s)",
                pages.len()
            ));
        }
        let mut drained: Vec<String> = pages.iter().flatten().cloned().collect();
        let total: usize = pages.iter().map(Vec::len).sum();
        if total != 24 {
            return Err(format!("expected 24 items exactly once, drained {total}"));
        }
        drained.sort();
        drained.dedup();
        if drained.len() != 24 {
            return Err("page sequence must never duplicate a section item".to_string());
        }
        Ok(())
    }

    #[test]
    fn resume_works_without_a_truncating_ceiling_on_the_second_page() -> TestResult {
        let json = list_envelope(40);
        let generation = || 7u64;
        let tight = test_context(600, &generation);
        let first = parse(
            &govern_response_json(&json, &tight, TEST_REGISTRY)
                .map_err(|error| format!("govern page 1: {error:?}"))?,
        )?;
        let page_one = kept_item_ids(&first);
        let cursor = continuation_cursor(&first).ok_or("page 1 must carry a cursor")?;
        // Page 2 resumes under an effectively unbounded ceiling (the CLI
        // models `--cursor` without `--max-output-tokens` as u64::MAX) and
        // must emit the exact remainder in one page.
        let unbounded = test_context(u64::MAX, &generation);
        let second = parse(
            &govern_response_json_with_resume(&json, &unbounded, TEST_REGISTRY, Some(&cursor))
                .map_err(|error| format!("govern page 2: {error:?}"))?,
        )?;
        let page_two = kept_item_ids(&second);
        if continuation_cursor(&second).is_some() {
            return Err("an unbounded resume page must not issue another cursor".to_string());
        }
        let mut drained = page_one;
        drained.extend(page_two);
        let expected: Vec<String> = (0..40).map(|index| format!("item_{index:04}")).collect();
        if drained != expected {
            return Err("tight page + unbounded resume must partition exactly".to_string());
        }
        Ok(())
    }

    #[test]
    fn rejected_cursor_yields_an_empty_page_not_a_restarted_one() -> TestResult {
        let json = list_envelope(12);
        let generation = || 7u64;
        let ctx = test_context(100_000, &generation);
        let governed = govern_response_json_with_resume(
            &json,
            &ctx,
            TEST_REGISTRY,
            Some("not-a-valid-cursor"),
        )
        .map_err(|error| format!("govern: {error:?}"))?;
        let value = parse(&governed)?;
        if !kept_item_ids(&value).is_empty() {
            return Err("an invalid cursor must yield an empty page".to_string());
        }
        let entry = degraded_entry_with_code(&value, CURSOR_INVALID_CODE)?;
        if entry.get("severity").and_then(JsonValue::as_str) != Some("low") {
            return Err("cursor_invalid must be severity low".to_string());
        }
        Ok(())
    }

    #[test]
    fn generation_advance_mid_sequence_is_an_empty_stale_page() -> TestResult {
        let json = list_envelope(40);
        let issue_generation = || 7u64;
        let issue_ctx = test_context(600, &issue_generation);
        let first = parse(
            &govern_response_json(&json, &issue_ctx, TEST_REGISTRY)
                .map_err(|error| format!("govern page 1: {error:?}"))?,
        )?;
        let cursor = continuation_cursor(&first).ok_or("page 1 must carry a cursor")?;
        let advanced_generation = || 9u64;
        let resume_ctx = test_context(600, &advanced_generation);
        let second = parse(
            &govern_response_json_with_resume(&json, &resume_ctx, TEST_REGISTRY, Some(&cursor))
                .map_err(|error| format!("govern page 2: {error:?}"))?,
        )?;
        if !kept_item_ids(&second).is_empty() {
            return Err("a stale cursor must yield an empty page".to_string());
        }
        let entry = degraded_entry_with_code(&second, CURSOR_STALE_CODE)?;
        let message = entry
            .get("message")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if !message.contains("generation 7") || !message.contains("generation 9") {
            return Err(format!(
                "stale entry must name both generations, got: {message}"
            ));
        }
        Ok(())
    }

    #[test]
    fn dishonest_position_key_is_rejected_as_invalid() -> TestResult {
        let json = list_envelope(40);
        let generation = || 7u64;
        let ctx = test_context(600, &generation);
        let first = parse(
            &govern_response_json(&json, &ctx, TEST_REGISTRY)
                .map_err(|error| format!("govern page 1: {error:?}"))?,
        )?;
        let cursor = continuation_cursor(&first).ok_or("page 1 must carry a cursor")?;
        let payload = decode_cursor(&cursor, &ctx.mac_key, &ctx.params_hash, 7)
            .map_err(|rejection| format!("decode: {rejection:?}"))?;
        let forged = CursorPayload {
            position_key: "item_9999".to_string(),
            ..payload
        };
        let forged_token =
            encode_cursor(&forged, &ctx.mac_key).map_err(|error| format!("encode: {error:?}"))?;
        let second = parse(
            &govern_response_json_with_resume(&json, &ctx, TEST_REGISTRY, Some(&forged_token))
                .map_err(|error| format!("govern page 2: {error:?}"))?,
        )?;
        if !kept_item_ids(&second).is_empty() {
            return Err("a dishonest positionKey must yield an empty page".to_string());
        }
        degraded_entry_with_code(&second, CURSOR_INVALID_CODE)?;
        Ok(())
    }

    #[test]
    fn legacy_payload_without_dropped_count_is_rejected_as_invalid() -> TestResult {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;

        let key = derive_workspace_mac_key("/tmp/test-workspace");
        let params_hash = hash_invocation_params(["test", "list", "--json"]);
        // Hand-encode the pre-bd-7lvbg.3 payload shape (no droppedCount)
        // with a VALID MAC: the missing field itself must reject the cursor.
        let legacy_payload = serde_json::to_vec(&json!({
            "schema": CURSOR_SCHEMA_V1,
            "targetSchema": "ee.test.list.v1",
            "dbGeneration": 7,
            "positionKey": "item_0009",
            "paramsHash": params_hash,
        }))
        .map_err(|error| format!("serialize legacy payload: {error}"))?;
        let mac = blake3::keyed_hash(&key, &legacy_payload);
        let token = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&legacy_payload),
            URL_SAFE_NO_PAD.encode(mac.as_bytes())
        );
        if decode_cursor(&token, &key, &params_hash, 7) != Err(CursorRejection::Invalid) {
            return Err("legacy payloads without droppedCount must be cursor_invalid".to_string());
        }
        Ok(())
    }

    #[test]
    fn byte_backstop_bounds_estimate_evading_payloads() {
        assert!(fits(10, 80, 10), "estimate and bytes at the cap must fit");
        assert!(!fits(11, 80, 10), "estimate over ceiling must not fit");
        assert!(
            !fits(10, 81, 10),
            "bytes over ceiling*8 must not fit even when the estimate fits"
        );
        assert!(fits(0, 0, 0), "zero everything fits a zero ceiling");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn truncation_never_produces_invalid_json(
            item_count in 0usize..60,
            body_len in 1usize..120,
            ceiling in 16u64..2_000,
        ) {
            let items: Vec<JsonValue> = (0..item_count)
                .map(|index| {
                    json!({
                        "id": format!("item_{index:04}"),
                        "content": "x".repeat(body_len),
                    })
                })
                .collect();
            let json = json!({
                "schema": "ee.response.v2",
                "success": true,
                "data": {
                    "command": "test list",
                    "schema": "ee.test.list.v1",
                    "items": items,
                },
                "degraded": [],
            })
            .to_string();
            let generation = || 1u64;
            let ctx = GovernorContext {
                ceiling_tokens: ceiling,
                params_hash: hash_invocation_params(["test", "list"]),
                mac_key: derive_workspace_mac_key("/tmp/test-workspace"),
                db_generation: &generation,
            };
            let governed = govern_response_json(&json, &ctx, TEST_REGISTRY)
                .map_err(|error| TestCaseError::fail(format!("govern failed: {error:?}")))?;
            let value: JsonValue = serde_json::from_str(&governed)
                .map_err(|error| TestCaseError::fail(format!("invalid JSON emitted: {error}")))?;
            prop_assert!(value.is_object(), "governed output must be a JSON object");
            prop_assert_eq!(
                value.get("schema").and_then(JsonValue::as_str),
                Some("ee.response.v2"),
                "envelope schema field must survive governing"
            );
            prop_assert!(
                value.pointer("/meta/tokensEstimated").and_then(JsonValue::as_u64).is_some(),
                "meta.tokensEstimated must be stamped under a ceiling"
            );
        }
    }
}
