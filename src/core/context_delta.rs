use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

pub const CONTEXT_DELTA_SCHEMA_V1: &str = "ee.context.delta.v1";
pub const CONTEXT_DELTA_OVERSIZED_CODE: &str = "context_delta_larger_than_full";

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

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaEnvelope {
    pub schema: &'static str,
    pub prior_pack_hash: String,
    pub new_pack_hash: String,
    pub base_db_generation: u64,
    pub new_db_generation: u64,
    pub items: ContextDeltaItems,
    pub token_savings: ContextDeltaTokenSavings,
    pub fallback: Option<ContextDeltaFallback>,
}

impl ContextDeltaEnvelope {
    #[must_use]
    pub fn emits_delta(&self) -> bool {
        self.fallback.is_none()
    }
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaFieldChange {
    pub old: Option<JsonValue>,
    pub new: Option<JsonValue>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDeltaFallback {
    pub code: &'static str,
    pub reason: String,
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
        prior_pack_hash: prior.pack_hash.clone(),
        new_pack_hash: new.pack_hash.clone(),
        base_db_generation: prior.db_generation,
        new_db_generation: new.db_generation,
        items,
        token_savings: token_savings(new.full_bytes, 0, new.net_pack_tokens),
        fallback: None,
    };

    let candidate_delta_bytes = stable_serialized_len(&mut envelope)?;
    envelope.token_savings =
        token_savings(new.full_bytes, candidate_delta_bytes, new.net_pack_tokens);

    if let Some(max_delta_bytes) = options.max_delta_bytes
        && candidate_delta_bytes > max_delta_bytes
    {
        envelope.fallback = Some(ContextDeltaFallback {
            code: CONTEXT_DELTA_OVERSIZED_CODE,
            reason: format!(
                "Delta envelope is {candidate_delta_bytes} bytes, above the configured \
                 {max_delta_bytes} byte limit; emit the full pack instead."
            ),
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
                ContextDeltaFieldChange {
                    old: old.cloned(),
                    new: new_value.cloned(),
                },
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
        envelope.token_savings = token_savings(
            envelope.token_savings.full_bytes,
            delta_bytes,
            envelope.token_savings.net_pack_tokens,
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
        CONTEXT_DELTA_OVERSIZED_CODE, ContextDeltaItemSnapshot, ContextDeltaOptions,
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
        assert!(delta.items.added.is_empty());
        assert!(delta.items.removed.is_empty());
        assert!(delta.items.modified.is_empty());
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
                .items
                .added
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["mem_b"]
        );
        assert_eq!(delta.items.removed, vec!["mem_a".to_string()]);
        Ok(())
    }

    #[test]
    fn modified_item_emits_old_new_field_pair() -> TestResult {
        let prior = snapshot("h1", 1, 1000, vec![item("mem_a", "old", 10)]);
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "new", 12)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;

        assert_eq!(delta.items.modified.len(), 1);
        let modified = &delta.items.modified[0];
        assert_eq!(modified.id, "mem_a");
        assert_eq!(
            modified.field_changes["contentHash"].old,
            Some(json!("old"))
        );
        assert_eq!(
            modified.field_changes["contentHash"].new,
            Some(json!("new"))
        );
        assert_eq!(
            modified.field_changes["estimatedTokens"].old,
            Some(json!(10))
        );
        assert_eq!(
            modified.field_changes["estimatedTokens"].new,
            Some(json!(12))
        );
        Ok(())
    }

    #[test]
    fn empty_prior_returns_full_added_set() -> TestResult {
        let prior = snapshot("h1", 1, 1000, Vec::new());
        let new = snapshot("h2", 2, 1000, vec![item("mem_a", "a", 10)]);
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;

        assert_eq!(delta.items.added.len(), 1);
        assert!(delta.items.removed.is_empty());
        Ok(())
    }

    #[test]
    fn empty_new_returns_full_removed_set() -> TestResult {
        let prior = snapshot("h1", 1, 1000, vec![item("mem_a", "a", 10)]);
        let new = snapshot("h2", 2, 1000, Vec::new());
        let delta = compute_context_delta(&prior, &new, ContextDeltaOptions::new(None))
            .map_err(|error| error.to_string())?;

        assert!(delta.items.added.is_empty());
        assert_eq!(delta.items.removed, vec!["mem_a".to_string()]);
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
            delta.fallback.as_ref().map(|fallback| fallback.code),
            Some(CONTEXT_DELTA_OVERSIZED_CODE)
        );
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
