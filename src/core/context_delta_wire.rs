//! Decode the actual context-delta v2 transport for local client application.
//!
//! Wire strings are owned while parsing and checked before they become the
//! schema/format constants in the public envelope. No input is leaked to obtain
//! a `'static` lifetime. Closed protocol objects and operation maps reject
//! ambiguity; arbitrary JSON field values remain data, not protocol objects.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value as JsonValue;

use super::super::{
    CONTEXT_DELTA_SCHEMA_V2, ContextDeltaDegradation, ContextDeltaEnvelope, ContextDeltaError,
    ContextDeltaFallbackReason, ContextDeltaFieldChange, ContextDeltaFieldChangeRedaction,
    ContextDeltaItemSnapshot, ContextDeltaItems, ContextDeltaModifiedItem, ContextDeltaPayload,
    ContextDeltaRedactionReason, ContextDeltaServerDecision, ContextDeltaTokenSavings,
};

/// Unlike a normal map deserializer, a protocol map must not silently accept
/// the last of two edits to the same field. Nested JsonValue data is deliberately
/// deserialized by serde_json itself, including arbitrary-precision numbers.
struct UniqueMap<T>(BTreeMap<String, T>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for UniqueMap<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueMapVisitor<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for UniqueMapVisitor<T> {
            type Value = UniqueMap<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object with unique field names")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, T>()? {
                    if values.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate context-delta field"));
                    }
                }
                Ok(UniqueMap(values))
            }
        }

        deserializer.deserialize_map(UniqueMapVisitor(PhantomData))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireEnvelope {
    schema: String,
    success: bool,
    data: WirePayload,
    degraded: Vec<WireDegradation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WirePayload {
    prior_pack_hash: String,
    new_pack_hash: String,
    workspace_id: Option<String>,
    base_db_generation: Option<u64>,
    new_db_generation: Option<u64>,
    prior_feature_flag_set_hash: Option<String>,
    new_feature_flag_set_hash: Option<String>,
    items: WireItems,
    token_savings: WireTokenSavings,
    server_decision: WireDecision,
    trace: Option<UniqueMap<JsonValue>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireDecision {
    computed_from_server_verified_pack_record: bool,
    delta_chained: bool,
    format: WireFormat,
    fallback_reason: Option<ContextDeltaFallbackReason>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireFormat {
    Json,
    Markdown,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireDegradation {
    code: String,
    severity: WireSeverity,
    message: String,
    repair: Option<String>,
    details: Option<UniqueMap<JsonValue>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireSeverity {
    Info,
    Low,
    Warning,
    Medium,
    High,
    Critical,
}

impl WireSeverity {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Low => "low",
            Self::Warning => "warning",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireItems {
    added: Vec<WireItem>,
    removed: Vec<String>,
    modified: Vec<WireModifiedItem>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireItem {
    id: String,
    fields: UniqueMap<JsonValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireModifiedItem {
    id: String,
    field_changes: UniqueMap<WireFieldChange>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WireFieldChange {
    Pair([JsonValue; 2]),
    Redacted(WireRedaction),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireRedaction {
    new_value: JsonValue,
    old_value_omitted: bool,
    reason: ContextDeltaRedactionReason,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireTokenSavings {
    full_bytes: u64,
    delta_bytes: u64,
    saved_bytes: i64,
    saved_percent: f64,
    net_pack_tokens: u32,
}

impl WireEnvelope {
    fn into_envelope(self) -> Result<ContextDeltaEnvelope, &'static str> {
        if self.schema != CONTEXT_DELTA_SCHEMA_V2 {
            return Err("unsupported context-delta schema");
        }
        if !self.success || self.data.server_decision.delta_chained {
            return Err("context-delta v2 requires success and forbids delta chaining");
        }
        if !self.data.token_savings.saved_percent.is_finite() {
            return Err("context-delta savings must be finite");
        }
        let items = self.data.items;
        let modified = items
            .modified
            .into_iter()
            .map(|item| {
                let field_changes = item
                    .field_changes
                    .0
                    .into_iter()
                    .map(|(field, change)| {
                        let change = match change {
                            WireFieldChange::Pair(pair) => ContextDeltaFieldChange::Pair(pair),
                            WireFieldChange::Redacted(redaction) => {
                                if !redaction.old_value_omitted {
                                    return Err("redacted change must omit its old value");
                                }
                                ContextDeltaFieldChange::Redacted(
                                    ContextDeltaFieldChangeRedaction {
                                        new_value: redaction.new_value,
                                        old_value_omitted: true,
                                        reason: redaction.reason,
                                    },
                                )
                            }
                        };
                        Ok((field, change))
                    })
                    .collect::<Result<_, _>>()?;
                Ok(ContextDeltaModifiedItem {
                    id: item.id,
                    field_changes,
                })
            })
            .collect::<Result<_, &'static str>>()?;
        let savings = self.data.token_savings;
        let decision = self.data.server_decision;
        Ok(ContextDeltaEnvelope {
            schema: CONTEXT_DELTA_SCHEMA_V2,
            success: true,
            data: ContextDeltaPayload {
                prior_pack_hash: self.data.prior_pack_hash,
                new_pack_hash: self.data.new_pack_hash,
                workspace_id: self.data.workspace_id,
                base_db_generation: self.data.base_db_generation,
                new_db_generation: self.data.new_db_generation,
                prior_feature_flag_set_hash: self.data.prior_feature_flag_set_hash,
                new_feature_flag_set_hash: self.data.new_feature_flag_set_hash,
                items: ContextDeltaItems {
                    added: items
                        .added
                        .into_iter()
                        .map(|item| ContextDeltaItemSnapshot {
                            id: item.id,
                            fields: item.fields.0,
                        })
                        .collect(),
                    removed: items.removed,
                    modified,
                },
                token_savings: ContextDeltaTokenSavings {
                    full_bytes: savings.full_bytes,
                    delta_bytes: savings.delta_bytes,
                    saved_bytes: savings.saved_bytes,
                    saved_percent: savings.saved_percent,
                    net_pack_tokens: savings.net_pack_tokens,
                },
                server_decision: ContextDeltaServerDecision {
                    // This is retained as a sender claim, never as authentication.
                    computed_from_server_verified_pack_record: decision
                        .computed_from_server_verified_pack_record,
                    delta_chained: false,
                    format: match decision.format {
                        WireFormat::Json => "json",
                        WireFormat::Markdown => "markdown",
                    },
                    fallback_reason: decision.fallback_reason,
                },
                trace: self.data.trace.map(|map| map.0),
            },
            degraded: self
                .degraded
                .into_iter()
                .map(|entry| ContextDeltaDegradation {
                    code: entry.code,
                    severity: entry.severity.as_str().to_owned(),
                    message: entry.message,
                    repair: entry.repair,
                    details: entry.details.map(|map| map.0),
                })
                .collect(),
        })
    }
}

impl<'de> Deserialize<'de> for ContextDeltaEnvelope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // serde's ordinary type errors can contain an offending scalar, key,
        // or enum value. Do not copy potentially private evidence into errors.
        let wire = WireEnvelope::deserialize(deserializer)
            .map_err(|_| de::Error::custom("invalid context-delta v2 structure"))?;
        wire.into_envelope().map_err(de::Error::custom)
    }
}

impl ContextDeltaEnvelope {
    /// Default transport limit for [`Self::from_json_slice`]: four MiB.
    pub const DEFAULT_MAX_JSON_BYTES: usize = 4 * 1024 * 1024;

    /// Decode one complete JSON envelope before applying it to a local baseline.
    ///
    /// Checks the v2 wire schema and bounds input to four MiB before parsing.
    /// Fallback/markdown envelopes can be inspected but `apply_to_snapshot`
    /// rejects them. A decoded server-verification marker remains only a claim.
    ///
    /// # Errors
    ///
    /// Rejects oversized, invalid, trailing, unsupported, or structurally
    /// ambiguous input. Errors do not echo raw fields or evidence text.
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, ContextDeltaError> {
        Self::from_json_slice_with_limit(bytes, Self::DEFAULT_MAX_JSON_BYTES)
    }

    /// Decode with a caller-selected inclusive input-byte limit.
    ///
    /// The limit includes any whitespace and line terminator. serde_json's
    /// recursion limit remains enabled. Direct `serde_json::from_slice::<Self>`
    /// also works, but callers using it must supply their own input-size bound.
    /// Byte-accounting fields are preserved, not treated as authenticity proof.
    ///
    /// # Errors
    ///
    /// Returns an error without changing any client state when the size limit
    /// or the context-delta v2 decoding contract is violated.
    pub fn from_json_slice_with_limit(
        bytes: &[u8],
        max_bytes: usize,
    ) -> Result<Self, ContextDeltaError> {
        if bytes.len() > max_bytes {
            return Err(ContextDeltaError {
                message:
                    "context delta exceeds the client input-byte limit; request a fresh full pack"
                        .to_owned(),
            });
        }
        serde_json::from_slice(bytes).map_err(|_| ContextDeltaError {
            message: "invalid context-delta v2 JSON; request a fresh full pack".to_owned(),
        })
    }
}

#[cfg(test)]
#[path = "context_delta_wire_tests.rs"]
mod tests;

#[path = "context_delta_scoped.rs"]
mod scoped;
