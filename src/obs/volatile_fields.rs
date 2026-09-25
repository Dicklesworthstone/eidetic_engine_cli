//! Canonical volatile-field registry for determinism comparisons.
//!
//! These fields legitimately vary between invocations against the same
//! workspace state, so J7-style determinism checks strip them before hashing
//! machine-facing JSON outputs.

use std::collections::BTreeSet;

use serde_json::Value;

use super::test_log::{EventKind, TestEvent, log_event, test_id_or};

/// Canonical list of fields that legitimately vary between invocations against
/// the same workspace state.
pub const VOLATILE_FIELD_NAMES: &[&str] = &[
    "generatedAt",
    "generated_at",
    "createdAt",
    "created_at",
    "updatedAt",
    "completedAt",
    "finishedAt",
    "expiresAt",
    "capturedAt",
    "captured_at",
    "computedAt",
    "computed_at",
    "observedAt",
    "recordedAt",
    "refreshedAt",
    "selectedAt",
    "decidedAt",
    "estimatedAt",
    "exposedAt",
    "lastValidatedAt",
    "last_accessed",
    "last_accessed_at",
    "last_seen_at",
    "last_used_at",
    "audit_ts",
    "elapsedMs",
    "elapsed_ms",
    "elapsedMsBucket",
    "durationMs",
    "wallClockMs",
    "startedAt",
    "started_at",
    "endedAt",
    "ended_at",
    "ts",
    "timestamp",
    "runIndex",
    "run_index",
    "runDurationMs",
    "run_duration_ms",
    "ee_binary_hash",
    // Handoff capsule determinism (bd-1um33): capsule_id and integrity are
    // freshly generated per create even for identical workspace state.
    // swarm_brief_summary/swarm_incident_summary/swarm_replay_summary and
    // environment_attestation_summary are runtime diagnostic subtrees;
    // section-level diagnostic redaction is handled specially in handoff.rs
    // (value-dependent on "id" field), but volatile top-level children are
    // covered here.
    "capsule_id",
    "integrity",
    "swarm_brief_summary",
    "swarm_incident_summary",
    "swarm_replay_summary",
    "environment_attestation_summary",
    // Newer capsule diagnostic subtrees (same class as the four above): they
    // embed run-scoped attestation ids, artifact hashes over volatile inputs,
    // and raw command output carrying timestamps, so two creates of the same
    // workspace state would otherwise produce different canonical hashes.
    "pack_replay_summary",
    "proof_broker_summary",
    "regression_causality_summary",
    "shadow_policy_summary",
    "contention_summary",
    "databasePath",
    "workspacePath",
    "indexDir",
    // Graph determinism surfaces (previously only in the determinism.sh bash
    // mirror; registered here so the two lists stay identical).
    "snapshotRefreshedAt",
    "witnessElapsedMs",
    "witnessRecordedAt",
    "algorithmStartedAt",
    "projectionMs",
    "pagerankMs",
    "betweennessMs",
    "totalMs",
    // Tailscale local-probe identity fields are machine/network specific and
    // sensitive in shared support bundles.
    "selfNodeKey",
    "selfTailscaleIp",
    "selfMagicDnsName",
    "tailnetId",
    "tailnetDisplayName",
    "selfAdvertisedTags",
    "peerNodeKey",
    "peerTailscaleIps",
    "peerMagicDnsName",
    "peerHostname",
    "peerAdvertisedTags",
    "binaryVersionRaw",
    "binaryAbsolutePath",
];

/// Report emitted by a volatile-field strip operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolatileStripReport {
    /// Number of distinct volatile field names removed.
    pub fields_stripped_count: usize,
    /// Distinct volatile names in registry order, followed by validated SLO paths.
    pub fields_stripped: Vec<&'static str>,
    /// JSON byte size before stripping, or 0 if serialization failed.
    pub input_bytes: usize,
    /// JSON byte size after stripping, or 0 if serialization failed.
    pub output_bytes: usize,
}

/// Return true when `field_name` is registered as volatile.
#[must_use]
pub fn is_volatile_field_name(field_name: &str) -> bool {
    canonical_field_name(field_name).is_some()
}

/// Validate and remove only the unsigned producer measurements in a pack SLO.
/// All deterministic resource evidence and every unrelated status remain intact.
/// Returns false for a response without a pack SLO; invalid measurements are
/// rejected before any field is changed.
pub fn normalize_pack_slo_measurements(value: &mut Value) -> Result<bool, String> {
    let Some(slo) = value.pointer("/data/pack/slo") else {
        return Ok(false);
    };
    if slo.get("schema").and_then(Value::as_str) != Some("ee.pack.slo.v1") {
        return Err("pack SLO schema is missing or unsupported".to_owned());
    }
    let numeric = |pointer: &str| {
        slo.pointer(pointer)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("pack SLO {pointer} must be an unsigned integer"))
    };
    let target = numeric("/budgetClass/elapsedMsTarget")?;
    let warning = numeric("/budgetClass/elapsedMsWarning")?;
    let failure = numeric("/budgetClass/elapsedMsFailure")?;
    let elapsed = numeric("/actuals/elapsedMs")?;
    if target == 0 || target > warning || warning >= failure {
        return Err("pack SLO elapsed thresholds must be positive and ordered".to_owned());
    }
    let statuses = ["within_budget", "warning", "failure"];
    let resource = slo.get("resourceStatus").and_then(Value::as_str);
    let resource_rank = statuses
        .iter()
        .position(|status| Some(*status) == resource)
        .ok_or_else(|| "pack SLO resourceStatus is missing or invalid".to_owned())?;
    let elapsed_rank = if elapsed >= failure {
        2
    } else if elapsed >= warning {
        1
    } else {
        0
    };
    if slo.get("elapsedStatus").and_then(Value::as_str) != Some(statuses[elapsed_rank]) {
        return Err(format!(
            "pack SLO elapsedStatus disagrees with elapsedMs={elapsed}, warning={warning}, failure={failure}"
        ));
    }
    if slo.get("status").and_then(Value::as_str) != Some(statuses[resource_rank.max(elapsed_rank)])
    {
        return Err(
            "pack SLO status must be the worst of resourceStatus and elapsedStatus".to_owned(),
        );
    }
    // Validation above guarantees the object shapes; mutate only after every check.
    if let Some(object) = value
        .pointer_mut("/data/pack/slo")
        .and_then(Value::as_object_mut)
    {
        object.remove("status");
        object.remove("elapsedStatus");
        if let Some(actuals) = object.get_mut("actuals").and_then(Value::as_object_mut) {
            actuals.remove("elapsedMs");
        }
    }
    Ok(true)
}

/// The degraded codes whose presence is decided by WALL-CLOCK TIME.
///
/// One entry, and the product already calls it non-reproducible in its own
/// words (src/pack/mod.rs, `pack_assembly_elapsed_degradation`): it "reports
/// wall-clock time, which is not reproducible, and it is therefore kept out of
/// pack identity". Kept out of IDENTITY, but still emitted into the response
/// payload — which is how a golden came to assert it.
///
/// ADR 0087 declares these codes non-canonical telemetry. The product's own
/// list, which the v2 pack hash drops by construction, is the single source.
const TIMING_DEGRADED_CODES: &[&str] = crate::pack::NON_CANONICAL_TELEMETRY_DEGRADATION_CODES;

/// Opening words of the timing degradation as rendered into markdown.
///
/// The markdown body carries severity + message + repair and NOT the `code`, so
/// the bullet can only be matched on message shape. If that wording changes this
/// stops matching and a comparison goes RED on a slow host, which is the safe
/// direction: it can never turn into a silent pass.
const TIMING_DEGRADED_MESSAGE_PREFIX: &str = "Pack assembly took ";

/// Erase the wall-clock degradation and every value derived from it, so a
/// response reads identically on a fast and a slow host. Returns how many
/// entries were dropped, so a caller can tell "bit" from "no-op".
///
/// THE OTHER HALF OF [`normalize_pack_slo_measurements`], AND IT LIVES HERE FOR
/// THAT REASON. That function strips the SLO's own timing fields; this one
/// strips the degradation DERIVED from the same clock. Splitting the two across
/// src and tests is exactly what let one be scrubbed while the other was
/// asserted: tests/fixtures/golden/agent/context_pack.json.golden hard-codes
/// `degradationCount: 2`, so it reads 2 on an idle worker and 3 on a loaded one
/// (bd-context-pack-golden-stale-and-load-sensitive-8ig10).
///
/// Deterministic degradations are untouched, so a comparison keeps asserting
/// them. A scrub that simply emptied `degraded[]` would be indistinguishable
/// from this one on a slow host and would delete real evidence on every host.
pub fn normalize_pack_timing_degradations(value: &mut Value) -> usize {
    let dropped = strip_timing_degraded_entries(value);
    if dropped > 0 {
        adjust_timing_derived_counts(value, dropped);
    }
    dropped
}

/// Filter timing entries out of every `degraded` array, returning the LARGEST
/// number taken from any single array.
///
/// Largest, not total: one logical list is serialized at BOTH `.degraded` and
/// `.data.degraded`, so summing double-counts and over-corrects every derived
/// number. Measured in the context pack golden, which carries both.
fn strip_timing_degraded_entries(value: &mut Value) -> usize {
    let mut dropped = 0usize;
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if key == "degraded" {
                    if let Value::Array(items) = child {
                        let before = items.len();
                        items.retain(|item| !is_timing_degradation(item));
                        dropped = dropped.max(before.saturating_sub(items.len()));
                    }
                }
                dropped = dropped.max(strip_timing_degraded_entries(child));
            }
        }
        Value::Array(items) => {
            for item in items {
                dropped = dropped.max(strip_timing_degraded_entries(item));
            }
        }
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
    dropped
}

fn is_timing_degradation(item: &Value) -> bool {
    item.get("code")
        .and_then(Value::as_str)
        .is_some_and(|code| TIMING_DEGRADED_CODES.contains(&code))
}

/// Bring every value DERIVED from the degraded list back to its fast-host
/// reading: the numeric count and the count inside prose.
///
/// `pack.text` is skipped. It is canonical (ADR 0087 §1): the product renders
/// it without the timing entry, so its count never included one, and a timing
/// bullet found there is a regression that
/// [`normalize_pack_envelope_timing`] must see, not a volatile value to erase.
fn adjust_timing_derived_counts(value: &mut Value, dropped: usize) {
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if key == "text" {
                    continue;
                }
                if key == "degradationCount" {
                    if let Some(count) = child.as_u64() {
                        *child = Value::from(count.saturating_sub(dropped as u64));
                        continue;
                    }
                }
                adjust_timing_derived_counts(child, dropped);
            }
        }
        Value::Array(items) => {
            for item in items {
                adjust_timing_derived_counts(item, dropped);
            }
        }
        Value::String(text) => {
            let without_bullet = strip_timing_degradation_markdown(text);
            *text = renumber_degraded_signal_prose(&without_bullet, dropped);
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

/// Rewrite "Context includes N degraded signal(s)" down by `dropped`.
///
/// The sentence is built from `degraded.len()`, so it counted the timing entry.
/// The noun is re-pluralized because the renderer pluralizes from the same
/// count, and 2 -> 1 must read "signal", not "signals".
fn renumber_degraded_signal_prose(text: &str, dropped: usize) -> String {
    const PREFIX: &str = "Context includes ";
    const SUFFIX: &str = " degraded signal";
    if !text.contains(SUFFIX) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(PREFIX) {
        let split = at + PREFIX.len();
        out.push_str(&rest[..split]);
        rest = &rest[split..];

        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        let Ok(count) = digits.parse::<usize>() else {
            continue;
        };
        let after_digits = &rest[digits.len()..];
        if !after_digits.starts_with(SUFFIX) {
            continue;
        }
        let after_noun = &after_digits[SUFFIX.len()..];
        let tail = after_noun.strip_prefix('s').unwrap_or(after_noun);

        let adjusted = count.saturating_sub(dropped);
        out.push_str(&adjusted.to_string());
        out.push_str(SUFFIX);
        if adjusted != 1 {
            out.push('s');
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// The markdown half of [`normalize_pack_timing_degradations`], for a pack
/// rendered with `--format markdown`, where there is no `degraded[]` to count.
///
/// The JSON normalizer learns how many entries it dropped from the array and
/// rewrites the prose by that number. A standalone markdown document has only
/// the rendered bullets, so the count comes from them instead: each timing
/// bullet is one entry the "Context includes N degraded signals" sentence
/// counted. Without this, tests/fixtures/golden/agent/context_pack.md.golden
/// stayed load-sensitive after the JSON golden was fixed -- it read 3 signals
/// plus a millisecond bullet on a loaded worker and 2 without it on an idle one
/// (bd-context-pack-golden-stale-and-load-sensitive-8ig10).
///
/// Returns the normalized text and the number of bullets dropped, so a caller
/// can tell "bit" from "no-op".
pub fn normalize_pack_timing_markdown(text: &str) -> (String, usize) {
    let dropped = text
        .split('\n')
        .filter(|line| is_timing_degradation_bullet(line))
        .count();
    if dropped == 0 {
        return (text.to_owned(), 0);
    }
    let without_bullet = strip_timing_degradation_markdown(text);
    (
        renumber_degraded_signal_prose(&without_bullet, dropped),
        dropped,
    )
}

/// What [`normalize_pack_envelope_timing`] found and removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PackEnvelopeTimingReport {
    /// A wall-clock timing entry was in some `degraded` list before the scrub.
    pub timing_entries_present: bool,
    /// Timing entries dropped, the largest count from any one `degraded` list
    /// (the list is serialized at both `.degraded` and `.data.degraded`).
    pub timing_entries_dropped: usize,
}

/// The timing channel for a pack or context JSON ENVELOPE, with its guards.
///
/// bd-4w1up: every caller that compares pack envelopes needs the wall-clock
/// degradation removed, and each used to hand-roll the same scrub plus its own
/// subset of guards. This is the one place for it. It holds ONLY the timing
/// step: the SLO measurements stay with each caller, because that step is not
/// idempotent and [`strip_volatile_fields`] already applies it, and the
/// name-based strip stays separate because golden comparisons keep volatile
/// names. Unlike the strip, it never fails silently:
///
/// - a timing entry that is present but not dropped is an error, not a no-op;
/// - a timing bullet left in ANY string afterwards is an error (a reworded
///   message, or a bullet without its `degraded` entry).
pub fn normalize_pack_envelope_timing(
    value: &mut Value,
) -> Result<PackEnvelopeTimingReport, String> {
    let timing_entries_present = contains_timing_degradation(value);
    let timing_entries_dropped = normalize_pack_timing_degradations(value);
    if timing_entries_present && timing_entries_dropped == 0 {
        return Err(format!(
            "the envelope carries {} but the timing normalizer dropped nothing",
            crate::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE
        ));
    }
    let leftover = count_timing_bullets(value);
    if leftover > 0 {
        return Err(format!(
            "{leftover} timing bullet(s) remain in the envelope's text after the timing normalizer"
        ));
    }
    Ok(PackEnvelopeTimingReport {
        timing_entries_present,
        timing_entries_dropped,
    })
}

fn contains_timing_degradation(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, child)| {
            (key == "degraded"
                && child
                    .as_array()
                    .is_some_and(|items| items.iter().any(is_timing_degradation)))
                || contains_timing_degradation(child)
        }),
        Value::Array(items) => items.iter().any(contains_timing_degradation),
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => false,
    }
}

fn count_timing_bullets(value: &Value) -> usize {
    match value {
        Value::Object(object) => object.values().map(count_timing_bullets).sum(),
        Value::Array(items) => items.iter().map(count_timing_bullets).sum(),
        Value::String(text) => text
            .split('\n')
            .filter(|line| is_timing_degradation_bullet(line))
            .count(),
        Value::Number(_) | Value::Bool(_) | Value::Null => 0,
    }
}

/// Stand-in for a per-workspace daemon socket path in comparable output.
pub const WORKSPACE_DAEMON_SOCKET_PLACEHOLDER: &str = "<workspace-daemon-socket>";

/// Replace every per-workspace daemon socket path with
/// [`WORKSPACE_DAEMON_SOCKET_PLACEHOLDER`], returning the text and how many
/// paths were replaced.
///
/// `crate::daemon::workspace_daemon_socket_path` names the socket
/// `d-<first 24 hex of blake3(canonical workspace)>.sock` beside the default
/// socket, and falls back to `/tmp/ee-<euid>/` when that would exceed 65
/// bytes. All three inputs -- the euid, the absolute workspace path inside
/// the hash, and the length fallback -- belong to the HOST, not the
/// workspace state, so `ee doctor` reads `/tmp/ee-1000/d-dd8d….sock` on one
/// worker and something else on the next (bd-j4njd). The default
/// `…/daemon.sock` form is NOT touched: only the hashed per-workspace name.
pub fn normalize_workspace_daemon_socket_paths(text: &str) -> (String, usize) {
    const NAME_PREFIX: &str = "/d-";
    const HEX_LEN: usize = 24;
    const SUFFIX: &str = ".sock";
    let mut out = String::with_capacity(text.len());
    let mut replaced = 0usize;
    let mut cursor = 0usize;
    let mut search_from = 0usize;
    while let Some(offset) = text[search_from..].find(NAME_PREFIX) {
        let slash = search_from + offset;
        let hex_start = slash + NAME_PREFIX.len();
        let hex_end = hex_start + HEX_LEN;
        let is_socket_name = text
            .get(hex_start..hex_end)
            .is_some_and(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
            && text
                .get(hex_end..)
                .is_some_and(|rest| rest.starts_with(SUFFIX));
        if !is_socket_name {
            search_from = hex_start;
            continue;
        }
        // The path token starts after the nearest preceding ASCII whitespace
        // or quote, never before the end of the previous replacement. Every
        // delimiter is one byte, which is what makes `at + 1` a char boundary.
        let token_start = text[cursor..slash]
            .rfind(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '"' | '\'' | '`' | '('))
            .map_or(cursor, |at| cursor + at + 1);
        let token_end = hex_end + SUFFIX.len();
        out.push_str(&text[cursor..token_start]);
        out.push_str(WORKSPACE_DAEMON_SOCKET_PLACEHOLDER);
        replaced += 1;
        cursor = token_end;
        search_from = token_end;
    }
    out.push_str(&text[cursor..]);
    (out, replaced)
}

/// [`normalize_workspace_daemon_socket_paths`] applied to every string in a
/// JSON value, returning how many paths were replaced. The one walker for every
/// harness that compares doctor JSON (bd-47x3l): a second harness without it
/// byte-compared the host's socket path and red on every other worker.
pub fn normalize_workspace_daemon_socket_paths_in_json(value: &mut Value) -> usize {
    match value {
        Value::String(text) => {
            let (normalized, replaced) = normalize_workspace_daemon_socket_paths(text);
            if replaced > 0 {
                *text = normalized;
            }
            replaced
        }
        Value::Array(items) => items
            .iter_mut()
            .map(normalize_workspace_daemon_socket_paths_in_json)
            .sum(),
        Value::Object(object) => object
            .values_mut()
            .map(normalize_workspace_daemon_socket_paths_in_json)
            .sum(),
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

/// One predicate for "this line is the timing bullet", shared by the counter
/// above and the stripper below so the number subtracted from the prose is
/// always the number of bullets actually removed.
fn is_timing_degradation_bullet(line: &str) -> bool {
    line.trim_start().starts_with("- **[") && line.contains(TIMING_DEGRADED_MESSAGE_PREFIX)
}

/// Drop the rendered markdown bullet for the timing degradation, and the
/// `- *Repair:*` line that belongs to it.
fn strip_timing_degradation_markdown(text: &str) -> String {
    if !text.contains(TIMING_DEGRADED_MESSAGE_PREFIX) {
        return text.to_owned();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in text.split('\n') {
        let trimmed = line.trim_start();
        if is_timing_degradation_bullet(line) {
            skipping = true;
            continue;
        }
        if skipping {
            if trimmed.starts_with("- *Repair:*") {
                continue;
            }
            skipping = false;
        }
        kept.push(line);
    }
    kept.join("\n")
}

/// Recursively remove volatile fields from a JSON value and emit a structured
/// test-log event when the J1 log harness is configured.
pub fn strip_volatile_fields(value: &mut Value) -> VolatileStripReport {
    let input_bytes = serialized_len(value);
    let mut stripped = BTreeSet::new();
    let pack_measurements = normalize_pack_slo_measurements(value);
    // A malformed SLO is evidence of a defect, not volatility. Preserve it
    // verbatim, including elapsedMs, so normalization cannot conceal the defect.
    if pack_measurements.is_ok() {
        strip_volatile_fields_inner(value, &mut stripped);
    }
    let output_bytes = serialized_len(value);
    let mut fields_stripped = VOLATILE_FIELD_NAMES
        .iter()
        .copied()
        .filter(|field| stripped.contains(field))
        .collect::<Vec<_>>();
    if pack_measurements == Ok(true) {
        fields_stripped.extend([
            "/data/pack/slo/actuals/elapsedMs",
            "/data/pack/slo/elapsedStatus",
            "/data/pack/slo/status",
        ]);
    }
    let report = VolatileStripReport {
        fields_stripped_count: fields_stripped.len(),
        fields_stripped,
        input_bytes,
        output_bytes,
    };
    log_volatile_strip(&report);
    report
}

fn strip_volatile_fields_inner(value: &mut Value, stripped: &mut BTreeSet<&'static str>) {
    match value {
        Value::Object(object) => {
            let keys = object
                .keys()
                .filter_map(|key| canonical_field_name(key))
                .collect::<Vec<_>>();
            for key in keys {
                object.remove(key);
                stripped.insert(key);
            }
            for child in object.values_mut() {
                strip_volatile_fields_inner(child, stripped);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_volatile_fields_inner(item, stripped);
            }
        }
        _ => {}
    }
}

fn canonical_field_name(field_name: &str) -> Option<&'static str> {
    VOLATILE_FIELD_NAMES
        .iter()
        .copied()
        .find(|registered| *registered == field_name)
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

fn log_volatile_strip(report: &VolatileStripReport) {
    let fields = report
        .fields_stripped
        .iter()
        .map(|field| Value::String((*field).to_owned()))
        .collect::<Vec<_>>();
    let event = TestEvent::new(test_id_or("volatile_field_strip"), EventKind::VolatileStrip)
        .with_field(
            "fields_stripped_count",
            u64::try_from(report.fields_stripped_count).unwrap_or(u64::MAX),
        )
        .with_field("fields_stripped", Value::Array(fields))
        .with_field(
            "input_bytes",
            u64::try_from(report.input_bytes).unwrap_or(u64::MAX),
        )
        .with_field(
            "output_bytes",
            u64::try_from(report.output_bytes).unwrap_or(u64::MAX),
        );
    log_event(event);
}

#[cfg(test)]
mod tests {
    use super::{
        PackEnvelopeTimingReport, VOLATILE_FIELD_NAMES, WORKSPACE_DAEMON_SOCKET_PLACEHOLDER,
        is_volatile_field_name, normalize_pack_envelope_timing, normalize_pack_timing_degradations,
        normalize_pack_timing_markdown, normalize_workspace_daemon_socket_paths,
        normalize_workspace_daemon_socket_paths_in_json, strip_volatile_fields,
    };

    type TestResult = Result<(), String>;

    /// Build the two readings the SAME request produces on a fast and a slow
    /// host, differing only in the wall-clock degradation.
    ///
    /// SYNTHETIC ON PURPOSE. This defect is load-sensitive, so a real run on an
    /// idle worker produces the fast document and proves nothing about the slow
    /// one. Constructing both is the only way to assert convergence without
    /// owning the load.
    fn timing_pair() -> (serde_json::Value, serde_json::Value) {
        let embed = serde_json::json!({
            "code": "embed_model_unavailable",
            "severity": "warning",
            "message": "Embedding model unavailable; semantic similarity is disabled.",
        });
        let freshness = serde_json::json!({
            "code": "context_evidence_freshness_missing_source",
            "severity": "low",
            "message": "Memory evidence freshness is missing_source.",
        });
        let timing = serde_json::json!({
            "code": crate::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE,
            "severity": "low",
            "message": "Pack assembly took 812ms, at or over the standard \
                        resource-profile elapsed warning threshold of 500ms. \
                        The pack contents are unaffected.",
            "repair": "Re-run to see whether the overrun is repeatable.",
        });

        // `degraded` is serialized at BOTH `.degraded` and `.data.degraded`,
        // which is what the "largest, not total" rule in the stripper exists for.
        // `pack.text` is canonical (ADR 0087 §1): the product renders it from
        // the deterministic degradations only, so it reads the same on both
        // hosts. The banner still counts the timing entry.
        let document = |entries: serde_json::Value, count: u64| {
            let sentence =
                format!("Context includes {count} degraded signals; semantic embedding is off.");
            serde_json::json!({
                "degraded": entries,
                "data": {
                    "degraded": entries,
                    "pack": {
                        "advisoryBanner": { "degradationCount": count, "summary": sentence },
                        "text": canonical_pack_text(),
                    }
                }
            })
        };

        (
            document(serde_json::json!([embed, freshness]), 2),
            document(serde_json::json!([embed, freshness, timing]), 3),
        )
    }

    /// The shipped `pack.text` of the `timing_pair` request: two deterministic
    /// degradations, no timing bullet.
    fn canonical_pack_text() -> String {
        format!(
            "Context includes 2 degraded signals; semantic embedding is off.\n\n{}",
            deterministic_markdown()
        )
    }

    fn deterministic_markdown() -> &'static str {
        "## Degradations\n\n\
         - **[warning]** Embedding model unavailable; semantic similarity is disabled.\n  \
         - *Repair:* `ee index reembed`\n"
    }

    /// A pre-ADR-0087-T2 markdown body, which rendered the timing bullet and
    /// counted it. The product no longer emits this; it is the input
    /// `normalize_pack_timing_markdown` exists for and the regression the
    /// envelope guard must reject.
    fn markdown_with_timing_bullet() -> String {
        format!(
            "Context includes 3 degraded signals; semantic embedding is off.\n\n{}\
             - **[low]** Pack assembly took 812ms, at or over the standard \
             resource-profile elapsed warning threshold of 500ms. The pack \
             contents are unaffected.\n  \
             - *Repair:* `Re-run to see whether the overrun is repeatable.`\n",
            deterministic_markdown()
        )
    }

    /// The load-sensitive half of
    /// bd-context-pack-golden-stale-and-load-sensitive-8ig10: the same request
    /// must read the same whether the host was busy or idle.
    #[test]
    fn timing_degradations_read_the_same_on_a_fast_and_a_slow_host() -> TestResult {
        let (fast, slow) = timing_pair();

        // NEGATIVE CONTROL: if the fixtures were already equal, every assertion
        // below would pass against a normalizer that does nothing at all.
        if fast == slow {
            return Err(
                "fixtures are identical before normalization; the test proves nothing".into(),
            );
        }

        let mut fast_out = fast.clone();
        let mut slow_out = slow.clone();
        let fast_dropped = normalize_pack_timing_degradations(&mut fast_out);
        let slow_dropped = normalize_pack_timing_degradations(&mut slow_out);

        // It must BITE on the slow document and NO-OP on the fast one. The
        // returned count is what makes those two distinguishable rather than
        // inferred from the output.
        if slow_dropped != 1 {
            return Err(format!(
                "slow document must drop exactly 1 entry, dropped {slow_dropped}"
            ));
        }
        if fast_dropped != 0 {
            return Err(format!(
                "fast document must drop nothing, dropped {fast_dropped}"
            ));
        }
        if fast_out != fast {
            return Err(format!(
                "fast document must be untouched, but changed:\n{fast_out:#}"
            ));
        }
        if fast_out != slow_out {
            return Err(format!(
                "host-dependent output: fast and slow normalized differently\n\
                 fast:\n{fast_out:#}\n\nslow:\n{slow_out:#}"
            ));
        }

        // The deterministic degradations must SURVIVE. Emptying `degraded[]`
        // would satisfy every assertion above and delete real evidence.
        let codes: Vec<&str> = slow_out
            .pointer("/data/degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or("normalized document lost /data/degraded")?
            .iter()
            .filter_map(|e| e.get("code").and_then(serde_json::Value::as_str))
            .collect();
        if codes
            != [
                "embed_model_unavailable",
                "context_evidence_freshness_missing_source",
            ]
        {
            return Err(format!(
                "deterministic degradations must survive, got {codes:?}"
            ));
        }

        // And the raw millisecond reading must be gone from the rendered body,
        // where it rides inside a message rather than in a key.
        let text = slow_out
            .pointer("/data/pack/text")
            .and_then(serde_json::Value::as_str)
            .ok_or("normalized document lost /data/pack/text")?;
        if text.contains("Pack assembly took ") {
            return Err(format!(
                "timing bullet survived in the markdown body:\n{text}"
            ));
        }

        println!("surviving codes: {codes:?}");
        println!("normalized body:\n{text}");
        Ok(())
    }

    /// 2 -> 1 must read "signal", not "signals": the renderer pluralizes from
    /// the same count it prints, so a normalizer that only rewrites the digit
    /// produces prose the product never emits.
    #[test]
    fn dropping_to_one_repluralizes_the_prose() -> TestResult {
        let mut value = serde_json::json!({
            "degraded": [
                {"code": "embed_model_unavailable"},
                {"code": crate::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE},
            ],
            "summary": "Context includes 2 degraded signals; check the index.",
        });
        let dropped = normalize_pack_timing_degradations(&mut value);
        if dropped != 1 {
            return Err(format!("expected to drop 1, dropped {dropped}"));
        }
        let summary = value["summary"].as_str().unwrap_or_default();
        if summary != "Context includes 1 degraded signal; check the index." {
            return Err(format!("prose not re-pluralized: {summary}"));
        }
        println!("re-pluralized to: {summary}");
        Ok(())
    }

    /// A document with no timing entry must come back byte-identical, so the
    /// normalizer cannot quietly rewrite deterministic content.
    #[test]
    fn a_document_without_a_timing_entry_is_untouched() -> TestResult {
        let original = serde_json::json!({
            "degraded": [{"code": "embed_model_unavailable"}],
            "data": {"pack": {"advisoryBanner": {"degradationCount": 1}}},
            "summary": "Context includes 1 degraded signal; unrelated.",
        });
        let mut value = original.clone();
        let dropped = normalize_pack_timing_degradations(&mut value);
        if dropped != 0 {
            return Err(format!("expected no drop, dropped {dropped}"));
        }
        if value != original {
            return Err(format!(
                "document was modified with nothing to drop:\n{value:#}"
            ));
        }
        Ok(())
    }

    /// bd-4w1up: the envelope helper bites on the slow reading, no-ops on the
    /// fast one, reports which it did, and leaves both identical.
    #[test]
    fn envelope_timing_reports_what_it_dropped_and_converges() -> TestResult {
        let (fast, slow) = timing_pair();
        let mut fast_out = fast.clone();
        let mut slow_out = slow;
        let fast_report = normalize_pack_envelope_timing(&mut fast_out)?;
        let slow_report = normalize_pack_envelope_timing(&mut slow_out)?;
        let expected_fast = PackEnvelopeTimingReport::default();
        let expected_slow = PackEnvelopeTimingReport {
            timing_entries_present: true,
            timing_entries_dropped: 1,
        };
        if fast_report != expected_fast || slow_report != expected_slow {
            return Err(format!(
                "reports: fast {fast_report:?} (want {expected_fast:?}), \
                 slow {slow_report:?} (want {expected_slow:?})"
            ));
        }
        if fast_out != fast {
            return Err(format!("fast envelope must be untouched:\n{fast_out:#}"));
        }
        if fast_out != slow_out {
            return Err(format!(
                "fast and slow envelopes normalized differently\n\
                 fast:\n{fast_out:#}\n\nslow:\n{slow_out:#}"
            ));
        }
        Ok(())
    }

    /// bd-4w1up: a timing bullet whose `degraded` entry is missing (a reworded
    /// code, or text rendered from a different list) must be an error, never a
    /// silent pass with the milliseconds still in the text.
    #[test]
    fn envelope_timing_rejects_a_bullet_left_without_its_entry() -> TestResult {
        let (_, slow) = timing_pair();
        let mut orphaned = slow;
        if let Some(text) = orphaned.pointer_mut("/data/pack/text") {
            *text = serde_json::Value::String(markdown_with_timing_bullet());
        }
        for pointer in ["/degraded", "/data/degraded"] {
            if let Some(entries) = orphaned
                .pointer_mut(pointer)
                .and_then(serde_json::Value::as_array_mut)
            {
                entries.retain(|entry| {
                    entry.get("code").and_then(serde_json::Value::as_str)
                        != Some(crate::pack::PACK_ASSEMBLY_ELAPSED_OVER_BUDGET_CODE)
                });
            }
        }
        match normalize_pack_envelope_timing(&mut orphaned) {
            Err(message) if message.contains("timing bullet") => Ok(()),
            other => Err(format!(
                "an orphaned timing bullet must be rejected, got {other:?}"
            )),
        }
    }

    /// ADR 0087 T2: `pack.text` is canonical, so a timing bullet there is a
    /// product regression even when its `degraded[]` entry is present. The
    /// envelope helper must reject it rather than scrub it into a pass.
    #[test]
    fn envelope_timing_rejects_a_timing_bullet_in_canonical_pack_text() -> TestResult {
        let (_, slow) = timing_pair();
        let mut regressed = slow;
        if let Some(text) = regressed.pointer_mut("/data/pack/text") {
            *text = serde_json::Value::String(markdown_with_timing_bullet());
        }
        match normalize_pack_envelope_timing(&mut regressed) {
            Err(message) if message.contains("timing bullet") => Ok(()),
            other => Err(format!(
                "a timing bullet in pack.text must be rejected, got {other:?}"
            )),
        }
    }

    /// The same convergence for a pack rendered as markdown, which has no
    /// `degraded[]` to count and so must take the count from the bullets.
    ///
    /// Since ADR 0087 T2 the product renders no timing bullet, so the slow body
    /// here is the pre-T2 shape, and the normalized result must equal the
    /// canonical body `timing_pair` embeds in `/data/pack/text`.
    #[test]
    fn timing_markdown_reads_the_same_on_a_fast_and_a_slow_host() -> TestResult {
        let fast_md = canonical_pack_text();
        let slow_md = markdown_with_timing_bullet();
        if fast_md == slow_md {
            return Err("markdown fixtures are identical; the test proves nothing".into());
        }

        let (fast_out, fast_dropped) = normalize_pack_timing_markdown(&fast_md);
        let (slow_out, slow_dropped) = normalize_pack_timing_markdown(&slow_md);
        if slow_dropped != 1 || fast_dropped != 0 {
            return Err(format!(
                "must drop 1 bullet on slow and 0 on fast, dropped {slow_dropped} and {fast_dropped}"
            ));
        }
        if fast_out != fast_md {
            return Err(format!("fast markdown must be untouched, got:\n{fast_out}"));
        }
        if slow_out != fast_out {
            return Err(format!(
                "host-dependent markdown:\nfast:\n{fast_out}\n\nslow:\n{slow_out}"
            ));
        }
        if !slow_out.contains("Context includes 2 degraded signals")
            || !slow_out.contains("Embedding model unavailable")
        {
            return Err(format!(
                "deterministic prose and bullets must survive:\n{slow_out}"
            ));
        }
        println!("normalized markdown:\n{slow_out}");
        Ok(())
    }

    /// Two hosts, one workspace state: the doctor message must read the same.
    ///
    /// The socket path depends on the euid, on the absolute workspace path
    /// (inside the hash) and on a 65-byte length fallback, so the two readings
    /// below are what the same check prints on two different workers.
    #[test]
    fn workspace_daemon_socket_reads_the_same_on_two_hosts() -> TestResult {
        let message = |path: &str| {
            format!(
                "Optional daemon socket is not present at {path}; in-process CLI execution remains authoritative."
            )
        };
        let worker = message("/tmp/ee-1000/d-dd8d8dfe03d558040e031a6f.sock");
        let laptop = message("/run/user/501/ee/d-0123456789abcdef01234567.sock");
        if worker == laptop {
            return Err("fixtures are identical; the test proves nothing".into());
        }
        let (worker_out, worker_hits) = normalize_workspace_daemon_socket_paths(&worker);
        let (laptop_out, laptop_hits) = normalize_workspace_daemon_socket_paths(&laptop);
        if worker_hits != 1 || laptop_hits != 1 {
            return Err(format!(
                "each reading must replace exactly one path, got {worker_hits} and {laptop_hits}"
            ));
        }
        if worker_out != laptop_out {
            return Err(format!(
                "host-dependent output:\n{worker_out}\n{laptop_out}"
            ));
        }
        if worker_out != message(WORKSPACE_DAEMON_SOCKET_PLACEHOLDER) {
            return Err(format!("unexpected normalized message: {worker_out}"));
        }
        Ok(())
    }

    /// bd-47x3l. The JSON walker every doctor-comparing harness shares: the
    /// same doctor-shaped document from two hosts must normalize to one value,
    /// counting one path each, while a non-host socket string stays put.
    #[test]
    fn workspace_daemon_socket_json_reads_the_same_on_two_hosts() -> TestResult {
        let doctor = |path: &str| {
            serde_json::json!({"data": {"checks": [
                {"name": "daemon_socket_reachable",
                 "message": format!("Optional daemon socket is not present at {path}; ok.")},
                {"name": "default_socket",
                 "message": "at <workspace>/.runtime/ee/daemon.sock"}
            ]}})
        };
        let mut worker = doctor("/tmp/ee-1000/d-3f005d529dc893aa9eb9a073.sock");
        let mut other = doctor("/tmp/ee-1001/d-dd8d8dfe03d558040e031a6f.sock");
        if worker == other {
            return Err("fixtures are identical; the test proves nothing".into());
        }
        let worker_hits = normalize_workspace_daemon_socket_paths_in_json(&mut worker);
        let other_hits = normalize_workspace_daemon_socket_paths_in_json(&mut other);
        if worker_hits != 1 || other_hits != 1 {
            return Err(format!(
                "each document must replace exactly one path, got {worker_hits} and {other_hits}"
            ));
        }
        if worker != other || worker != doctor(WORKSPACE_DAEMON_SOCKET_PLACEHOLDER) {
            return Err(format!("host-dependent or wrong output: {worker}"));
        }
        Ok(())
    }

    /// Only the hashed per-workspace name is scrubbed. The default socket, a
    /// non-hex `d-` name and a short hash all pass through byte-identical, so
    /// the scrubber cannot hide a socket that is not host-derived.
    #[test]
    fn non_workspace_socket_paths_are_untouched() -> TestResult {
        for text in [
            "Optional daemon socket is not present at <workspace>/.runtime/ee/daemon.sock; ok.",
            "path /tmp/ee-1000/d-zzzzzzzzzzzzzzzzzzzzzzzz.sock is not hex",
            "path /tmp/ee-1000/d-dd8d8dfe.sock is too short",
            "no socket here at all",
        ] {
            let (out, hits) = normalize_workspace_daemon_socket_paths(text);
            if hits != 0 || out != text {
                return Err(format!("must be untouched, got {hits} hits: {out}"));
            }
        }
        Ok(())
    }

    /// 2 -> 1 in markdown must re-pluralize exactly as the JSON path does.
    #[test]
    fn timing_markdown_dropping_to_one_repluralizes() -> TestResult {
        let text = "Context includes 2 degraded signals; check the index.\n\n\
                    - **[low]** Pack assembly took 900ms, over budget.\n  \
                    - *Repair:* `Re-run.`\n";
        let (out, dropped) = normalize_pack_timing_markdown(text);
        if dropped != 1 {
            return Err(format!("expected to drop 1, dropped {dropped}"));
        }
        if out != "Context includes 1 degraded signal; check the index.\n\n" {
            return Err(format!("unexpected normalized markdown: {out:?}"));
        }
        Ok(())
    }

    #[test]
    fn registry_names_are_unique() -> TestResult {
        let mut names = std::collections::BTreeSet::new();
        for name in VOLATILE_FIELD_NAMES {
            if name.trim().is_empty() {
                return Err("empty volatile field name".to_owned());
            }
            if !names.insert(name) {
                return Err(format!("duplicate volatile field name: {name}"));
            }
        }
        Ok(())
    }

    #[test]
    fn strip_volatile_fields_recurses_and_reports() -> TestResult {
        let mut value = serde_json::json!({
            "schema": "ee.response.v2",
            "generatedAt": "2026-05-13T00:00:00Z",
            "data": {
                "createdAt": "2026-05-13T00:00:00Z",
                "updatedAt": "2026-05-13T00:00:01Z",
                "computed_at": "2026-05-13T00:00:01Z",
                "observedAt": "2026-05-13T00:00:02Z",
                "items": [
                    {"id": "mem_a", "elapsedMs": 12, "durationMs": 11, "content": "keep"},
                    {"id": "mem_b", "last_seen_at": "2026-05-13T00:00:01Z"}
                ],
                "workspacePath": "/tmp/ws"
            }
        });
        let report = strip_volatile_fields(&mut value);
        if value.pointer("/generatedAt").is_some()
            || value.pointer("/data/createdAt").is_some()
            || value.pointer("/data/updatedAt").is_some()
            || value.pointer("/data/computed_at").is_some()
            || value.pointer("/data/observedAt").is_some()
            || value.pointer("/data/items/0/elapsedMs").is_some()
            || value.pointer("/data/items/0/durationMs").is_some()
            || value.pointer("/data/items/1/last_seen_at").is_some()
            || value.pointer("/data/workspacePath").is_some()
        {
            return Err(format!("volatile fields were not stripped: {value}"));
        }
        if value
            .pointer("/data/items/0/content")
            .and_then(|v| v.as_str())
            != Some("keep")
        {
            return Err("non-volatile content was stripped".to_owned());
        }
        for expected in [
            "generatedAt",
            "createdAt",
            "updatedAt",
            "computed_at",
            "observedAt",
            "elapsedMs",
            "durationMs",
            "last_seen_at",
            "workspacePath",
        ] {
            if !report.fields_stripped.contains(&expected) {
                return Err(format!("report missing stripped field {expected}"));
            }
        }
        Ok(())
    }

    #[test]
    fn registry_predicate_matches_list() {
        assert!(is_volatile_field_name("generatedAt"));
        assert!(is_volatile_field_name("createdAt"));
        assert!(is_volatile_field_name("created_at"));
        assert!(is_volatile_field_name("updatedAt"));
        assert!(is_volatile_field_name("completedAt"));
        assert!(is_volatile_field_name("expiresAt"));
        assert!(is_volatile_field_name("observedAt"));
        assert!(is_volatile_field_name("recordedAt"));
        assert!(is_volatile_field_name("selectedAt"));
        assert!(is_volatile_field_name("lastValidatedAt"));
        assert!(is_volatile_field_name("durationMs"));
        assert!(is_volatile_field_name("captured_at"));
        assert!(is_volatile_field_name("last_accessed_at"));
        assert!(is_volatile_field_name("capsule_id"));
        assert!(is_volatile_field_name("integrity"));
        assert!(is_volatile_field_name("swarm_brief_summary"));
        assert!(is_volatile_field_name("swarm_incident_summary"));
        assert!(is_volatile_field_name("swarm_replay_summary"));
        assert!(!is_volatile_field_name("content"));
    }

    #[test]
    fn strip_volatile_fields_covers_handoff_capsule_names() -> TestResult {
        let mut value = serde_json::json!({
            "schema": "ee.handoff.capsule.v1",
            "capsule_id": "cap_a",
            "created_at": "2026-05-16T00:00:00Z",
            "integrity": {"hmac": "secret"},
            "swarm_brief_summary": {"hostname": "agent-host"},
            "swarm_incident_summary": {"summaryHash": "blake3:volatile"},
            "swarm_replay_summary": {"summaryHash": "blake3:volatile-replay"},
            "memory_snapshot": {"captured_at": "2026-05-16T00:00:00Z"},
            "sections": [
                {
                    "id": "objective",
                    "content": "keep this"
                }
            ]
        });
        let report = strip_volatile_fields(&mut value);

        for pointer in [
            "/capsule_id",
            "/created_at",
            "/integrity",
            "/swarm_brief_summary",
            "/swarm_incident_summary",
            "/swarm_replay_summary",
            "/memory_snapshot/captured_at",
        ] {
            if value.pointer(pointer).is_some() {
                return Err(format!("{pointer} was not stripped: {value}"));
            }
        }
        if value
            .pointer("/sections/0/content")
            .and_then(|v| v.as_str())
            != Some("keep this")
        {
            return Err("non-volatile handoff section content was stripped".to_owned());
        }
        for expected in [
            "capsule_id",
            "created_at",
            "captured_at",
            "integrity",
            "swarm_brief_summary",
            "swarm_incident_summary",
            "swarm_replay_summary",
        ] {
            if !report.fields_stripped.contains(&expected) {
                return Err(format!("report missing stripped capsule field {expected}"));
            }
        }
        Ok(())
    }

    #[test]
    fn strip_volatile_fields_covers_tailscale_local_probe_identity() -> TestResult {
        let mut value = serde_json::json!({
            "schema": "ee.response.v2",
            "data": {
                "mesh": {
                    "tailscale": {
                        "schema": "ee.tailscale.local.v1",
                        "tailnetId": "tailnet-alpha",
                        "tailnetDisplayName": "alpha.example",
                        "selfNodeKey": "nodekey:selfalpha",
                        "selfTailscaleIp": "100.64.0.10",
                        "selfMagicDnsName": "ee-local.tailnet.test.",
                        "selfAdvertisedTags": ["tag:ee-mesh"],
                        "peers": [{
                            "peerNodeKey": "nodekey:peeralpha",
                            "peerTailscaleIps": ["100.64.0.20"],
                            "peerMagicDnsName": "peer-alpha.tailnet.test.",
                            "peerHostname": "peer-alpha",
                            "peerAdvertisedTags": ["tag:ee-mesh"],
                            "online": true
                        }],
                        "binaryVersionRaw": "1.66.0\n  tailscale commit: abc",
                        "binaryAbsolutePath": "/opt/homebrew/bin/tailscale",
                        "probeMethod": "cli"
                    }
                }
            }
        });
        let report = strip_volatile_fields(&mut value);

        for pointer in [
            "/data/mesh/tailscale/tailnetId",
            "/data/mesh/tailscale/tailnetDisplayName",
            "/data/mesh/tailscale/selfNodeKey",
            "/data/mesh/tailscale/selfTailscaleIp",
            "/data/mesh/tailscale/selfMagicDnsName",
            "/data/mesh/tailscale/selfAdvertisedTags",
            "/data/mesh/tailscale/peers/0/peerNodeKey",
            "/data/mesh/tailscale/peers/0/peerTailscaleIps",
            "/data/mesh/tailscale/peers/0/peerMagicDnsName",
            "/data/mesh/tailscale/peers/0/peerHostname",
            "/data/mesh/tailscale/peers/0/peerAdvertisedTags",
            "/data/mesh/tailscale/binaryVersionRaw",
            "/data/mesh/tailscale/binaryAbsolutePath",
        ] {
            if value.pointer(pointer).is_some() {
                return Err(format!("{pointer} was not stripped: {value}"));
            }
        }
        if value
            .pointer("/data/mesh/tailscale/probeMethod")
            .and_then(|v| v.as_str())
            != Some("cli")
        {
            return Err("non-volatile tailscale field was stripped".to_owned());
        }
        for expected in [
            "tailnetId",
            "tailnetDisplayName",
            "selfNodeKey",
            "selfTailscaleIp",
            "selfMagicDnsName",
            "selfAdvertisedTags",
            "peerNodeKey",
            "peerTailscaleIps",
            "peerMagicDnsName",
            "peerHostname",
            "peerAdvertisedTags",
            "binaryVersionRaw",
            "binaryAbsolutePath",
        ] {
            if !report.fields_stripped.contains(&expected) {
                return Err(format!(
                    "report missing stripped tailscale field {expected}"
                ));
            }
        }
        Ok(())
    }
    #[test]
    fn pack_slo_normalization_validates_measurements_and_preserves_semantics() -> TestResult {
        let baseline = serde_json::json!({
            "status": "outside",
            "data": {"pack": {
                "hash": "blake3:unchanged", "items": [{"status": "selected"}],
                "slo": {
                    "schema": "ee.pack.slo.v1",
                    "budgetClass": {"elapsedMsTarget": 200, "elapsedMsWarning": 500, "elapsedMsFailure": 2000},
                    "actuals": {"elapsedMs": 18, "scannedCount": 12},
                    "resourceStatus": "within_budget", "elapsedStatus": "within_budget",
                    "status": "within_budget", "degradations": []
                }
            }},
            "unrelated": {"elapsedStatus": "keep", "status": "failure"}
        });
        let mut canonical = baseline.clone();
        assert!(super::normalize_pack_slo_measurements(&mut canonical)?);
        assert_eq!(canonical["status"], "outside");
        assert_eq!(canonical["unrelated"], baseline["unrelated"]);
        assert_eq!(canonical["data"]["pack"]["hash"], "blake3:unchanged");
        assert_eq!(
            canonical["data"]["pack"]["slo"]["resourceStatus"],
            "within_budget"
        );
        for (elapsed, status) in [
            (499, "within_budget"),
            (500, "warning"),
            (1999, "warning"),
            (2000, "failure"),
            (24457, "failure"),
        ] {
            let mut value = baseline.clone();
            value["data"]["pack"]["slo"]["actuals"]["elapsedMs"] = elapsed.into();
            value["data"]["pack"]["slo"]["elapsedStatus"] = status.into();
            value["data"]["pack"]["slo"]["status"] = status.into();
            assert!(super::normalize_pack_slo_measurements(&mut value)?);
            assert_eq!(value, canonical);
        }
        for (pointer, replacement) in [
            ("/data/pack/slo/elapsedStatus", serde_json::json!("failure")),
            ("/data/pack/slo/status", serde_json::json!("warning")),
            ("/data/pack/slo/actuals/elapsedMs", serde_json::json!(-1)),
            ("/data/pack/slo/actuals/elapsedMs", serde_json::json!("18")),
            (
                "/data/pack/slo/budgetClass/elapsedMsWarning",
                serde_json::json!(2000),
            ),
            (
                "/data/pack/slo/resourceStatus",
                serde_json::json!("unknown"),
            ),
        ] {
            let mut invalid = baseline.clone();
            *invalid
                .pointer_mut(pointer)
                .ok_or_else(|| format!("missing fixture path {pointer}"))? = replacement;
            let original = invalid.clone();
            assert!(
                super::normalize_pack_slo_measurements(&mut invalid).is_err(),
                "{pointer}"
            );
            assert_eq!(invalid, original, "rejection cannot mutate evidence");
            assert_eq!(strip_volatile_fields(&mut invalid).fields_stripped_count, 0);
            assert_eq!(
                invalid, original,
                "generic stripping must preserve invalid SLO evidence"
            );
        }
        let mut resource_failure = baseline;
        resource_failure["data"]["pack"]["slo"]["resourceStatus"] = "failure".into();
        resource_failure["data"]["pack"]["slo"]["status"] = "failure".into();
        resource_failure["data"]["pack"]["slo"]["degradations"] =
            serde_json::json!([{"code": "pack_assembly_budget_exceeded"}]);
        strip_volatile_fields(&mut resource_failure);
        assert_ne!(
            resource_failure, canonical,
            "resource failures cannot normalize away"
        );
        assert!(!is_volatile_field_name("status"));
        assert!(!is_volatile_field_name("elapsedStatus"));
        Ok(())
    }
}
