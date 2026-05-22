use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

pub const CONTEXT_DELTA_SCHEMA_V1: &str = "ee.context.delta.v1";
pub const CONTEXT_DELTA_PRIOR_UNKNOWN_CODE: &str = "context_delta_prior_unknown";
pub const CONTEXT_DELTA_OVERSIZED_CODE: &str = "context_delta_larger_than_full";
pub const CONTEXT_DELTA_FORMAT_UNSUPPORTED_CODE: &str = "context_delta_format_unsupported";

/// bd-n0vkg: pinned repair string for `context_delta_prior_unknown`.
/// MUST stay byte-identical to
/// `tests/fixtures/failure_modes/context_delta_prior_unknown.json`'s
/// `expected_emission.repair_string`. Every CLI emission site for this
/// code routes through this const, and
/// `tests/contracts/context_delta_prior_unknown_repair_pinned.rs`
/// enforces the equality at every `cargo test --test contracts`
/// invocation so the J6 catalog's pinned-repair contract stays a real
/// guarantee instead of documentation drift.
pub const CONTEXT_DELTA_PRIOR_UNKNOWN_REPAIR: &str = "Run ee context without --since, or pass a pack hash from a prior ee context --json response in the same workspace while the pack record is still retained.";

const CONTEXT_DELTA_FORMAT_JSON: &str = "json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaPackSnapshot {
    pub pack_hash: String,
    pub db_generation: u64,
    pub full_bytes: u64,
    pub net_pack_tokens: u32,
    pub items: Vec<ContextDeltaItemSnapshot>,
}

impl ContextDeltaPackSnapshot {
    #[must_use]
    pub fn new(
        pack_hash: impl Into<String>,
        db_generation: u64,
        full_bytes: u64,
        net_pack_tokens: u32,
        items: Vec<ContextDeltaItemSnapshot>,
    ) -> Self {
        Self {
            pack_hash: pack_hash.into(),
            db_generation,
            full_bytes,
            net_pack_tokens,
            items,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaItemSnapshot {
    pub id: String,
    pub fields: BTreeMap<String, JsonValue>,
}

impl ContextDeltaItemSnapshot {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            fields: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_field(mut self, name: impl Into<String>, value: JsonValue) -> Self {
        self.fields.insert(name.into(), value);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextDeltaOptions {
    pub max_delta_bytes: Option<u64>,
}

impl ContextDeltaOptions {
    #[must_use]
    pub const fn new(max_delta_bytes: Option<u64>) -> Self {
        Self { max_delta_bytes }
    }
}

/// Top-level envelope matching `ee.context.delta.v1` (see
/// `docs/schemas/ee.context.delta.v1.json`). Always serializes as
/// `{schema, success, data, degraded}`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaEnvelope {
    pub schema: &'static str,
    pub success: bool,
    pub data: ContextDeltaPayload,
    pub degraded: Vec<ContextDeltaDegradation>,
}

impl ContextDeltaEnvelope {
    /// Returns true when this envelope carried a real item-scoped delta.
    /// False when the server fell back to emitting a full pack (the
    /// fallback reason is then carried on `data.server_decision`).
    #[must_use]
    pub fn emits_delta(&self) -> bool {
        self.data.server_decision.fallback_reason.is_none()
    }

    /// bd-270ep: project a pack-side `ContextResponseDegradation` onto the
    /// agent-visible delta envelope. The kernel constructs `degraded[]`
    /// from its own oversized/fallback bookkeeping only and has no view of
    /// the surrounding `ContextResponse`, so without this projection the
    /// `--since` happy-path silently drops the `deprecated_alias` every
    /// `ee context` invocation carries plus any pack-assembly
    /// degradations (BM25-only fallback, scope-strict miss, slow pack
    /// assembly, RPC fallback, …) that `run_context_pack` attached
    /// upstream. This is the symmetric counterpart of
    /// `push_context_delta_kernel_degradation` (which runs in the other
    /// direction on the fallback path).
    pub fn append_response_degradation(
        &mut self,
        code: impl Into<String>,
        severity: impl Into<String>,
        message: impl Into<String>,
        repair: Option<String>,
    ) {
        self.degraded.push(ContextDeltaDegradation {
            code: code.into(),
            severity: severity.into(),
            message: message.into(),
            repair,
            details: None,
        });
    }
}

/// The `data` payload of `ee.context.delta.v1`. Field set is closed
/// (`additionalProperties: false`); add new fields only after bumping
/// the schema to v2.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaPayload {
    pub prior_pack_hash: String,
    pub new_pack_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_db_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_db_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_feature_flag_set_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_feature_flag_set_hash: Option<String>,
    pub items: ContextDeltaItems,
    pub token_savings: ContextDeltaTokenSavings,
    pub server_decision: ContextDeltaServerDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<BTreeMap<String, JsonValue>>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaServerDecision {
    pub computed_from_server_verified_pack_record: bool,
    pub delta_chained: bool,
    pub format: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<ContextDeltaFallbackReason>,
}

/// Closed enum mirroring `serverDecision.fallbackReason` in the v1
/// schema. Wire form is snake_case strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDeltaFallbackReason {
    PriorUnknown,
    DeltaLargerThanFull,
    RedactionDrift,
    ComputeBudgetExceeded,
    EnvelopeOversized,
    PriorCorrupted,
    FormatUnsupported,
    FeatureFlagDrift,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, JsonValue>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaItems {
    pub added: Vec<ContextDeltaItemSnapshot>,
    pub removed: Vec<String>,
    pub modified: Vec<ContextDeltaModifiedItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaModifiedItem {
    pub id: String,
    pub field_changes: BTreeMap<String, ContextDeltaFieldChange>,
}

/// Field-level change shape. Matches the `fieldChange` `oneOf` in
/// the v1 schema: ordinary changes serialize as a two-element
/// `[old, new]` array; redaction-safe changes serialize as a struct
/// with `newValue`, `oldValueOmitted`, and `reason`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContextDeltaFieldChange {
    Pair([JsonValue; 2]),
    Redacted(ContextDeltaFieldChangeRedaction),
}

impl ContextDeltaFieldChange {
    #[must_use]
    pub fn pair(old: Option<JsonValue>, new: Option<JsonValue>) -> Self {
        Self::Pair([
            old.unwrap_or(JsonValue::Null),
            new.unwrap_or(JsonValue::Null),
        ])
    }

    #[must_use]
    pub fn redacted(new_value: JsonValue, reason: ContextDeltaRedactionReason) -> Self {
        Self::Redacted(ContextDeltaFieldChangeRedaction {
            new_value,
            old_value_omitted: true,
            reason,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaFieldChangeRedaction {
    pub new_value: JsonValue,
    pub old_value_omitted: bool,
    pub reason: ContextDeltaRedactionReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDeltaRedactionReason {
    RedactionDrift,
    PolicyRestricted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaTokenSavings {
    pub full_bytes: u64,
    pub delta_bytes: u64,
    pub saved_bytes: i64,
    pub saved_percent: f64,
    pub net_pack_tokens: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextDeltaError {
    message: String,
}

impl ContextDeltaError {
    fn serialize(context: &str, error: serde_json::Error) -> Self {
        Self {
            message: format!("{context} could not be serialized: {error}"),
        }
    }
}

impl fmt::Display for ContextDeltaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ContextDeltaError {}

pub fn compute_context_delta(
    prior: &ContextDeltaPackSnapshot,
    new: &ContextDeltaPackSnapshot,
    options: ContextDeltaOptions,
) -> Result<ContextDeltaEnvelope, ContextDeltaError> {
    let items = diff_items(&prior.items, &new.items);
    let mut envelope = ContextDeltaEnvelope {
        schema: CONTEXT_DELTA_SCHEMA_V1,
        success: true,
        data: ContextDeltaPayload {
            prior_pack_hash: prior.pack_hash.clone(),
            new_pack_hash: new.pack_hash.clone(),
            workspace_id: None,
            base_db_generation: Some(prior.db_generation),
            new_db_generation: Some(new.db_generation),
            prior_feature_flag_set_hash: None,
            new_feature_flag_set_hash: None,
            items,
            token_savings: token_savings(new.full_bytes, 0, new.net_pack_tokens),
            server_decision: ContextDeltaServerDecision {
                computed_from_server_verified_pack_record: true,
                delta_chained: false,
                format: CONTEXT_DELTA_FORMAT_JSON,
                fallback_reason: None,
            },
            trace: None,
        },
        degraded: Vec::new(),
    };

    let candidate_delta_bytes = stable_serialized_len(&mut envelope)?;
    envelope.data.token_savings =
        token_savings(new.full_bytes, candidate_delta_bytes, new.net_pack_tokens);

    if let Some(max_delta_bytes) = options.max_delta_bytes
        && candidate_delta_bytes > max_delta_bytes
    {
        envelope.data.server_decision.fallback_reason =
            Some(ContextDeltaFallbackReason::DeltaLargerThanFull);
        envelope.degraded.push(ContextDeltaDegradation {
            code: CONTEXT_DELTA_OVERSIZED_CODE.to_string(),
            severity: "info".to_string(),
            message: format!(
                "Delta envelope is {candidate_delta_bytes} bytes, above the configured \
                 {max_delta_bytes} byte limit; emit the full pack instead."
            ),
            repair: None,
            details: None,
        });
    }

    Ok(envelope)
}

fn diff_items(
    prior_items: &[ContextDeltaItemSnapshot],
    new_items: &[ContextDeltaItemSnapshot],
) -> ContextDeltaItems {
    let prior_by_id = prior_items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let new_by_id = new_items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<BTreeMap<_, _>>();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();

    for (id, new_item) in &new_by_id {
        match prior_by_id.get(id) {
            Some(prior_item) => {
                if let Some(change) = diff_item_fields(prior_item, new_item) {
                    modified.push(change);
                }
            }
            None => added.push((*new_item).clone()),
        }
    }

    for id in prior_by_id.keys() {
        if !new_by_id.contains_key(id) {
            removed.push((*id).to_string());
        }
    }

    ContextDeltaItems {
        added,
        removed,
        modified,
    }
}

fn diff_item_fields(
    prior: &ContextDeltaItemSnapshot,
    new: &ContextDeltaItemSnapshot,
) -> Option<ContextDeltaModifiedItem> {
    let field_names = prior
        .fields
        .keys()
        .chain(new.fields.keys())
        .collect::<BTreeSet<_>>();
    let mut field_changes = BTreeMap::new();

    for field_name in field_names {
        let old = prior.fields.get(field_name);
        let new_value = new.fields.get(field_name);
        if old != new_value {
            field_changes.insert(
                field_name.clone(),
                ContextDeltaFieldChange::pair(old.cloned(), new_value.cloned()),
            );
        }
    }

    (!field_changes.is_empty()).then(|| ContextDeltaModifiedItem {
        id: new.id.clone(),
        field_changes,
    })
}

fn stable_serialized_len(envelope: &mut ContextDeltaEnvelope) -> Result<u64, ContextDeltaError> {
    let mut delta_bytes = 0;
    for _ in 0..8 {
        envelope.data.token_savings = token_savings(
            envelope.data.token_savings.full_bytes,
            delta_bytes,
            envelope.data.token_savings.net_pack_tokens,
        );
        let serialized = serde_json::to_vec(envelope)
            .map_err(|error| ContextDeltaError::serialize("context delta envelope", error))?;
        let next_delta_bytes = serialized.len() as u64;
        if next_delta_bytes == delta_bytes {
            return Ok(delta_bytes);
        }
        delta_bytes = next_delta_bytes;
    }
    Ok(delta_bytes)
}

fn token_savings(
    full_bytes: u64,
    delta_bytes: u64,
    net_pack_tokens: u32,
) -> ContextDeltaTokenSavings {
    let saved_bytes = full_bytes as i64 - delta_bytes as i64;
    ContextDeltaTokenSavings {
        full_bytes,
        delta_bytes,
        saved_bytes,
        saved_percent: saved_percent(full_bytes, saved_bytes),
        net_pack_tokens,
    }
}

fn saved_percent(full_bytes: u64, saved_bytes: i64) -> f64 {
    if full_bytes == 0 {
        return 0.0;
    }
    ((saved_bytes as f64 / full_bytes as f64) * 10_000.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        CONTEXT_DELTA_OVERSIZED_CODE, CONTEXT_DELTA_SCHEMA_V1, ContextDeltaFallbackReason,
        ContextDeltaFieldChange, ContextDeltaItemSnapshot, ContextDeltaOptions,
        ContextDeltaPackSnapshot, compute_context_delta,
    };

    type TestResult = Result<(), String>;

    fn item(id: &str, content: &str, tokens: u32) -> ContextDeltaItemSnapshot {
        ContextDeltaItemSnapshot::new(id)
            .with_field("contentHash", json!(content))
            .with_field("estimatedTokens", json!(tokens))
            .with_field("section", json!("facts"))
    }

    fn snapshot(
        hash: &str,
        generation: u64,
        full_bytes: u64,
        items: Vec<ContextDeltaItemSnapshot>,
    ) -> ContextDeltaPackSnapshot {
        ContextDeltaPackSnapshot::new(hash, generation, full_bytes, 123, items)
    }

    #[test]
    fn identical_packs_emit_empty_delta() -> TestResult {
        let prior = snapshot("h1", 1, 1000, vec![item("mem_a", "a", 10)]);
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "a", 10)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;

        assert!(delta.emits_delta());
        assert_eq!(delta.schema, CONTEXT_DELTA_SCHEMA_V1);
        assert!(delta.success);
        assert!(delta.degraded.is_empty());
        assert!(delta.data.items.added.is_empty());
        assert!(delta.data.items.removed.is_empty());
        assert!(delta.data.items.modified.is_empty());
        Ok(())
    }

    #[test]
    fn no_op_delta_serializes_empty_arrays_not_special_case_shape() -> TestResult {
        let prior = snapshot("h1", 1, 1000, vec![item("mem_a", "a", 10)]);
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "a", 10)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;
        let serialized =
            serde_json::to_string(&delta).map_err(|error| format!("serialize delta: {error}"))?;

        assert!(serialized.contains("\"added\":[]"));
        assert!(serialized.contains("\"removed\":[]"));
        assert!(serialized.contains("\"modified\":[]"));
        assert!(
            !serialized.contains("noChange"),
            "no-op deltas must keep the normal item-diff shape"
        );
        Ok(())
    }

    #[test]
    fn added_and_removed_items_are_reported_in_stable_order() -> TestResult {
        let prior = snapshot(
            "h1",
            1,
            1000,
            vec![item("mem_c", "c", 30), item("mem_a", "a", 10)],
        );
        let new = snapshot(
            "h2",
            2,
            1000,
            vec![item("mem_b", "b", 20), item("mem_c", "c", 30)],
        );
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;

        assert_eq!(
            delta
                .data
                .items
                .added
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["mem_b"]
        );
        assert_eq!(delta.data.items.removed, vec!["mem_a".to_string()]);
        Ok(())
    }

    #[test]
    fn modified_item_emits_old_new_field_pair() -> TestResult {
        let prior = snapshot("h1", 1, 1000, vec![item("mem_a", "old", 10)]);
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "new", 12)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;

        assert_eq!(delta.data.items.modified.len(), 1);
        let modified = &delta.data.items.modified[0];
        assert_eq!(modified.id, "mem_a");
        match &modified.field_changes["contentHash"] {
            ContextDeltaFieldChange::Pair([old, new]) => {
                assert_eq!(*old, json!("old"));
                assert_eq!(*new, json!("new"));
            }
            ContextDeltaFieldChange::Redacted(_) => {
                return Err("contentHash change should be an ordinary pair, not redacted".into());
            }
        }
        match &modified.field_changes["estimatedTokens"] {
            ContextDeltaFieldChange::Pair([old, new]) => {
                assert_eq!(*old, json!(10));
                assert_eq!(*new, json!(12));
            }
            ContextDeltaFieldChange::Redacted(_) => {
                return Err(
                    "estimatedTokens change should be an ordinary pair, not redacted".into(),
                );
            }
        }
        Ok(())
    }

    #[test]
    fn modified_field_change_serializes_as_two_item_array() -> TestResult {
        let prior = snapshot("h1", 1, 1000, vec![item("mem_a", "old", 10)]);
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "new", 12)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;
        let serialized =
            serde_json::to_value(&delta).map_err(|error| format!("serialize delta: {error}"))?;

        let field_changes = serialized
            .pointer("/data/items/modified/0/fieldChanges/contentHash")
            .ok_or_else(|| "missing fieldChanges entry".to_string())?;
        let pair = field_changes
            .as_array()
            .ok_or_else(|| "fieldChange must serialize as a JSON array".to_string())?;
        assert_eq!(
            pair.len(),
            2,
            "fieldChange pair must have exactly two elements"
        );
        assert_eq!(pair[0], json!("old"));
        assert_eq!(pair[1], json!("new"));
        Ok(())
    }

    #[test]
    fn empty_prior_returns_full_added_set() -> TestResult {
        let prior = snapshot("h1", 1, 1000, Vec::new());
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "a", 10)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;

        assert_eq!(delta.data.items.added.len(), 1);
        assert!(delta.data.items.removed.is_empty());
        Ok(())
    }

    #[test]
    fn empty_new_returns_full_removed_set() -> TestResult {
        let prior = snapshot("h1", 1, 1000, vec![item("mem_a", "a", 10)]);
        let new = snapshot("h2", 2, 1000, Vec::new());
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;

        assert!(delta.data.items.added.is_empty());
        assert_eq!(delta.data.items.removed, vec!["mem_a".to_string()]);
        Ok(())
    }

    #[test]
    fn oversized_delta_falls_back_to_full_pack() -> TestResult {
        let prior = snapshot("h1", 1, 1000, Vec::new());
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "a", 10)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(Some(1)))
            .map_err(|error| error.to_string())?;

        assert!(!delta.emits_delta());
        assert_eq!(
            delta.data.server_decision.fallback_reason,
            Some(ContextDeltaFallbackReason::DeltaLargerThanFull)
        );
        assert_eq!(delta.degraded.len(), 1);
        assert_eq!(delta.degraded[0].code, CONTEXT_DELTA_OVERSIZED_CODE);
        Ok(())
    }

    #[test]
    fn max_delta_bytes_equal_to_candidate_size_still_emits_delta() -> TestResult {
        let prior = snapshot("h1", 1, 1000, Vec::new());
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "a", 10)]);
        let baseline = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;
        let bounded = compute_context_delta(
            &prior,
            &new,
            ContextDeltaOptions::new(Some(baseline.data.token_savings.delta_bytes)),
        )
        .map_err(|error| error.to_string())?;

        assert!(bounded.emits_delta());
        assert_eq!(
            bounded.data.token_savings.delta_bytes,
            baseline.data.token_savings.delta_bytes
        );
        Ok(())
    }

    #[test]
    fn token_savings_reports_new_pack_token_budget() -> TestResult {
        let prior =
            ContextDeltaPackSnapshot::new("h1", 1, 1000, 321, vec![item("mem_a", "old", 10)]);
        let new = ContextDeltaPackSnapshot::new(
            "h2",
            2,
            1200,
            654,
            vec![item("mem_a", "new", 12), item("mem_b", "b", 20)],
        );
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;

        assert_eq!(delta.data.token_savings.full_bytes, 1200);
        assert_eq!(delta.data.token_savings.net_pack_tokens, 654);
        assert!(
            delta.data.token_savings.delta_bytes > 0,
            "delta byte accounting should be finalized after serialization"
        );
        Ok(())
    }

    #[test]
    fn envelope_has_required_top_level_keys() -> TestResult {
        let prior = snapshot("h1", 1, 1000, vec![item("mem_a", "a", 10)]);
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "b", 11)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;
        let serialized =
            serde_json::to_value(&delta).map_err(|error| format!("serialize delta: {error}"))?;
        let object = serialized
            .as_object()
            .ok_or_else(|| "envelope must serialize as a JSON object".to_string())?;

        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["data", "degraded", "schema", "success"]);
        assert_eq!(object["schema"], json!(CONTEXT_DELTA_SCHEMA_V1));
        assert_eq!(object["success"], json!(true));
        Ok(())
    }

    #[test]
    fn server_decision_present_on_every_envelope() -> TestResult {
        let prior = snapshot("h1", 1, 1000, vec![item("mem_a", "a", 10)]);
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "b", 11)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;
        let serialized =
            serde_json::to_value(&delta).map_err(|error| format!("serialize delta: {error}"))?;

        let server = serialized
            .pointer("/data/serverDecision")
            .ok_or_else(|| "serverDecision missing".to_string())?
            .as_object()
            .ok_or_else(|| "serverDecision must be an object".to_string())?;
        assert_eq!(server["computedFromServerVerifiedPackRecord"], json!(true));
        assert_eq!(server["deltaChained"], json!(false));
        assert_eq!(server["format"], json!("json"));
        Ok(())
    }

    #[test]
    fn same_inputs_serialize_byte_stable_across_runs() -> TestResult {
        let prior = snapshot("h1", 1, 1000, vec![item("mem_a", "a", 10)]);
        let new = snapshot(
            "h2",
            2,
            1000,
            vec![item("mem_b", "b", 20), item("mem_a", "new", 11)],
        );

        let first = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .and_then(|delta| {
                serde_json::to_string(&delta).map_err(|error| {
                    super::ContextDeltaError::serialize("test context delta", error)
                })
            })
            .map_err(|error| error.to_string())?;
        for _ in 0..3 {
            let next = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
                .and_then(|delta| {
                    serde_json::to_string(&delta).map_err(|error| {
                        super::ContextDeltaError::serialize("test context delta", error)
                    })
                })
                .map_err(|error| error.to_string())?;
            assert_eq!(first, next);
        }
        Ok(())
    }
}
