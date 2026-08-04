//! SRR6.39 - Mesh audit and forensics ledger.
//!
//! The mesh layer needs a durable, redaction-safe explanation of what crossed
//! peer boundaries and why. This module owns the stable event vocabulary,
//! canonical event hashing, support-bundle projection, and repository adapter
//! that appends mesh forensics events to the existing `audit_log` chain.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::db::{CreateAuditInput, DbConnection, generate_audit_id};
use crate::models::DomainError;

pub const MESH_AUDIT_EVENT_SCHEMA_V1: &str = "ee.mesh.audit_event.v1";
pub const MESH_AUDIT_SUPPORT_BUNDLE_ENTRY_SCHEMA_V1: &str = "ee.mesh.audit_support_bundle_entry.v1";
pub const MESH_AUDIT_LEDGER_MISSING_CODE: &str = "mesh_audit_ledger_missing";
pub const MESH_AUDIT_LEDGER_CORRUPT_CODE: &str = "mesh_audit_ledger_corrupt";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum MeshAuditEventKind {
    Export,
    Import,
    PeerEnrollment,
    PolicyDecision,
    SharePreviewConsent,
    LaneGrant,
    LaneRevoke,
    Withdrawal,
    BodyFetch,
    Quarantine,
    Revision,
}

impl MeshAuditEventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Export => "export",
            Self::Import => "import",
            Self::PeerEnrollment => "peer_enrollment",
            Self::PolicyDecision => "policy_decision",
            Self::SharePreviewConsent => "share_preview_consent",
            Self::LaneGrant => "lane_grant",
            Self::LaneRevoke => "lane_revoke",
            Self::Withdrawal => "withdrawal",
            Self::BodyFetch => "body_fetch",
            Self::Quarantine => "quarantine",
            Self::Revision => "revision",
        }
    }

    #[must_use]
    pub const fn audit_action(self) -> &'static str {
        match self {
            Self::Export => "mesh.audit.export",
            Self::Import => "mesh.audit.import",
            Self::PeerEnrollment => "mesh.audit.peer_enrollment",
            Self::PolicyDecision => "mesh.audit.policy_decision",
            Self::SharePreviewConsent => "mesh.audit.share_preview_consent",
            Self::LaneGrant => "mesh.audit.lane_grant",
            Self::LaneRevoke => "mesh.audit.lane_revoke",
            Self::Withdrawal => "mesh.audit.withdrawal",
            Self::BodyFetch => "mesh.audit.body_fetch",
            Self::Quarantine => "mesh.audit.quarantine",
            Self::Revision => "mesh.audit.revision",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAuditDetails {
    entries: BTreeMap<String, MeshAuditDetailValue>,
}

impl MeshAuditDetails {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn insert_count(&mut self, key: &str, value: u64) -> Result<(), MeshAuditLedgerError> {
        self.insert_value(key, MeshAuditDetailValue::Count { value })
    }

    pub fn insert_bool(&mut self, key: &str, value: bool) -> Result<(), MeshAuditLedgerError> {
        self.insert_value(key, MeshAuditDetailValue::Bool { value })
    }

    pub fn insert_reference(&mut self, key: &str, value: &str) -> Result<(), MeshAuditLedgerError> {
        let value = validate_safe_reference(key, value)?;
        self.insert_value(key, MeshAuditDetailValue::Reference { value })
    }

    pub fn insert_reference_list(
        &mut self,
        key: &str,
        values: Vec<String>,
    ) -> Result<(), MeshAuditLedgerError> {
        let mut safe_values = values
            .iter()
            .map(|value| validate_safe_reference(key, value))
            .collect::<Result<Vec<_>, _>>()?;
        safe_values.sort();
        safe_values.dedup();
        self.insert_value(
            key,
            MeshAuditDetailValue::ReferenceList {
                values: safe_values,
            },
        )
    }

    pub fn insert_digest(&mut self, key: &str, value: &str) -> Result<(), MeshAuditLedgerError> {
        let value = validate_digest(key, value)?;
        self.insert_value(key, MeshAuditDetailValue::Digest { value })
    }

    pub fn insert_redacted_text(
        &mut self,
        key: &str,
        label: &str,
        raw_text: &str,
    ) -> Result<(), MeshAuditLedgerError> {
        let label = validate_safe_reference(key, label)?;
        let digest = blake3_digest(raw_text.as_bytes());
        self.insert_value(key, MeshAuditDetailValue::RedactedText { label, digest })
    }

    fn insert_value(
        &mut self,
        key: &str,
        value: MeshAuditDetailValue,
    ) -> Result<(), MeshAuditLedgerError> {
        let key = validate_detail_key(key)?;
        self.entries.insert(key, value);
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MeshAuditDetailValue {
    Count { value: u64 },
    Bool { value: bool },
    Reference { value: String },
    ReferenceList { values: Vec<String> },
    Digest { value: String },
    RedactedText { label: String, digest: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshAuditEventInput {
    pub workspace_id: String,
    pub event_kind: MeshAuditEventKind,
    pub peer_id: Option<String>,
    pub origin_workspace_id: Option<String>,
    pub target_workspace_id: Option<String>,
    pub workspace_scope: Option<String>,
    pub policy_decision_id: Option<String>,
    pub local_row_refs: Vec<String>,
    pub cached_body_refs: Vec<String>,
    pub details: MeshAuditDetails,
    pub previous_event_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAuditEvent {
    pub schema: &'static str,
    pub event_id: String,
    pub event_kind: String,
    pub action: String,
    pub workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_decision_id: Option<String>,
    pub local_row_refs: Vec<String>,
    pub cached_body_refs: Vec<String>,
    pub details: MeshAuditDetails,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_event_hash: Option<String>,
    pub event_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAuditSupportBundleEntry {
    pub schema: &'static str,
    pub event_id: String,
    pub event_kind: String,
    pub action: String,
    pub peer_id: Option<String>,
    pub workspace_scope: Option<String>,
    pub local_row_count: usize,
    pub cached_body_ref_count: usize,
    pub previous_event_hash: Option<String>,
    pub event_hash: String,
}

#[derive(Debug)]
pub enum MeshAuditLedgerError {
    EmptyField { field: &'static str },
    UnsafeDetail { key: String, reason: &'static str },
    Serialize(serde_json::Error),
    AuditWrite(DomainError),
}

impl std::fmt::Display for MeshAuditLedgerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyField { field } => write!(formatter, "mesh audit {field} is required"),
            Self::UnsafeDetail { key, reason } => {
                write!(formatter, "mesh audit detail {key:?} is unsafe: {reason}")
            }
            Self::Serialize(error) => write!(formatter, "failed to serialize mesh audit: {error}"),
            Self::AuditWrite(error) => write!(formatter, "failed to write mesh audit: {error}"),
        }
    }
}

impl std::error::Error for MeshAuditLedgerError {}

pub fn compute_mesh_audit_event(
    input: &MeshAuditEventInput,
) -> Result<MeshAuditEvent, MeshAuditLedgerError> {
    let workspace_id = validate_required_reference("workspace_id", &input.workspace_id)?;
    let mut local_row_refs = validate_reference_list("local_row_refs", &input.local_row_refs)?;
    let mut cached_body_refs =
        validate_reference_list("cached_body_refs", &input.cached_body_refs)?;
    local_row_refs.sort();
    local_row_refs.dedup();
    cached_body_refs.sort();
    cached_body_refs.dedup();

    let mut event = MeshAuditEvent {
        schema: MESH_AUDIT_EVENT_SCHEMA_V1,
        event_id: String::new(),
        event_kind: input.event_kind.as_str().to_owned(),
        action: input.event_kind.audit_action().to_owned(),
        workspace_id,
        peer_id: validate_optional_reference("peer_id", input.peer_id.as_deref())?,
        origin_workspace_id: validate_optional_reference(
            "origin_workspace_id",
            input.origin_workspace_id.as_deref(),
        )?,
        target_workspace_id: validate_optional_reference(
            "target_workspace_id",
            input.target_workspace_id.as_deref(),
        )?,
        workspace_scope: validate_optional_reference(
            "workspace_scope",
            input.workspace_scope.as_deref(),
        )?,
        policy_decision_id: validate_optional_reference(
            "policy_decision_id",
            input.policy_decision_id.as_deref(),
        )?,
        local_row_refs,
        cached_body_refs,
        details: input.details.clone(),
        previous_event_hash: validate_optional_digest(
            "previous_event_hash",
            input.previous_event_hash.as_deref(),
        )?,
        event_hash: String::new(),
    };
    event.event_id = compute_event_id(&event);
    event.event_hash = compute_event_hash(&event);
    Ok(event)
}

pub fn append_mesh_audit_event(
    connection: &DbConnection,
    event: &MeshAuditEvent,
    actor: Option<&str>,
) -> Result<String, MeshAuditLedgerError> {
    let details_json = serde_json::to_string(event).map_err(MeshAuditLedgerError::Serialize)?;
    let audit_id = generate_audit_id();
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(event.workspace_id.clone()),
                actor: actor.map(str::to_owned),
                action: event.action.clone(),
                target_type: Some("mesh".to_owned()),
                target_id: Some(event.event_id.clone()),
                details: Some(details_json),
            },
        )
        .map_err(|error| MeshAuditLedgerError::AuditWrite(domain_error_from(error)))?;
    Ok(audit_id)
}

#[must_use]
pub fn support_bundle_entry(event: &MeshAuditEvent) -> MeshAuditSupportBundleEntry {
    MeshAuditSupportBundleEntry {
        schema: MESH_AUDIT_SUPPORT_BUNDLE_ENTRY_SCHEMA_V1,
        event_id: event.event_id.clone(),
        event_kind: event.event_kind.clone(),
        action: event.action.clone(),
        peer_id: event.peer_id.clone(),
        workspace_scope: event.workspace_scope.clone(),
        local_row_count: event.local_row_refs.len(),
        cached_body_ref_count: event.cached_body_refs.len(),
        previous_event_hash: event.previous_event_hash.clone(),
        event_hash: event.event_hash.clone(),
    }
}

fn compute_event_id(event: &MeshAuditEvent) -> String {
    let mut clone = event.clone();
    clone.event_id.clear();
    clone.event_hash.clear();
    let canonical = canonical_json_for(&clone);
    let hex = blake3::hash(canonical.as_bytes()).to_hex().to_string();
    let prefix: String = hex.chars().take(24).collect();
    format!("mesh_audit_{prefix}")
}

fn compute_event_hash(event: &MeshAuditEvent) -> String {
    let mut clone = event.clone();
    clone.event_hash.clear();
    let canonical = canonical_json_for(&clone);
    blake3_digest(canonical.as_bytes())
}

fn canonical_json_for<T: Serialize>(value: &T) -> String {
    let value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    canonical_json_string(&value)
}

fn canonical_json_string(value: &serde_json::Value) -> String {
    let canonical = canonicalize_json_value(value);
    serde_json::to_string(&canonical).unwrap_or_else(|_| "null".to_owned())
}

fn canonicalize_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<String, serde_json::Value> = map
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize_json_value(value)))
                .collect();
            let mut out = serde_json::Map::new();
            for (key, value) in sorted {
                out.insert(key, value);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize_json_value).collect())
        }
        other => other.clone(),
    }
}

fn blake3_digest(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn validate_detail_key(key: &str) -> Result<String, MeshAuditLedgerError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(MeshAuditLedgerError::EmptyField {
            field: "detail key",
        });
    }
    if trimmed.len() > 64
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err(MeshAuditLedgerError::UnsafeDetail {
            key: trimmed.to_owned(),
            reason: "keys must be lowercase ascii snake_case and <= 64 bytes",
        });
    }
    Ok(trimmed.to_owned())
}

fn validate_safe_reference(key: &str, value: &str) -> Result<String, MeshAuditLedgerError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(MeshAuditLedgerError::UnsafeDetail {
            key: key.to_owned(),
            reason: "empty reference",
        });
    }
    if trimmed.len() > 256 {
        return Err(MeshAuditLedgerError::UnsafeDetail {
            key: key.to_owned(),
            reason: "reference too long for a support-safe audit row",
        });
    }
    if looks_secret_like(trimmed) {
        return Err(MeshAuditLedgerError::UnsafeDetail {
            key: key.to_owned(),
            reason: "reference resembles raw secret or body text",
        });
    }
    Ok(trimmed.to_owned())
}

fn validate_digest(key: &str, value: &str) -> Result<String, MeshAuditLedgerError> {
    let trimmed = value.trim();
    if trimmed.starts_with("blake3:") || trimmed.starts_with("sha256:") {
        return validate_safe_reference(key, trimmed);
    }
    Err(MeshAuditLedgerError::UnsafeDetail {
        key: key.to_owned(),
        reason: "digest must start with blake3: or sha256:",
    })
}

fn validate_required_reference(
    field: &'static str,
    value: &str,
) -> Result<String, MeshAuditLedgerError> {
    validate_safe_reference(field, value).map_err(|error| match error {
        MeshAuditLedgerError::UnsafeDetail { reason, .. } if reason == "empty reference" => {
            MeshAuditLedgerError::EmptyField { field }
        }
        other => other,
    })
}

fn validate_optional_reference(
    field: &str,
    value: Option<&str>,
) -> Result<Option<String>, MeshAuditLedgerError> {
    value
        .map(|value| validate_safe_reference(field, value))
        .transpose()
}

fn validate_optional_digest(
    field: &str,
    value: Option<&str>,
) -> Result<Option<String>, MeshAuditLedgerError> {
    value.map(|value| validate_digest(field, value)).transpose()
}

fn validate_reference_list(
    field: &str,
    values: &[String],
) -> Result<Vec<String>, MeshAuditLedgerError> {
    values
        .iter()
        .map(|value| validate_safe_reference(field, value))
        .collect()
}

fn looks_secret_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "api_key",
        "apikey",
        "access_token",
        "bearer ",
        "-----begin",
        "sk-",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn domain_error_from<E: std::fmt::Display>(error: E) -> DomainError {
    DomainError::Storage {
        message: format!("mesh audit insert failed: {error}"),
        repair: Some(
            "Inspect `ee audit verify --json` and retry the mesh operation after the audit ledger is healthy."
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateWorkspaceInput, DbConnection};

    type TestResult = Result<(), String>;

    const WORKSPACE_ID: &str = "wsp_meshaust000000000000000aaa";
    const WORKSPACE_PATH: &str = "/tmp/ee-mesh-audit-test";

    fn fixture_details() -> Result<MeshAuditDetails, MeshAuditLedgerError> {
        let mut details = MeshAuditDetails::default();
        details.insert_count("memory_count", 3)?;
        details.insert_bool("body_fetch_allowed", false)?;
        details.insert_reference("policy_outcome", "deny")?;
        let sensitive_body = ["raw body text with pass", "word=do-not-store"].concat();
        details.insert_redacted_text("representative_body", "body_preview", &sensitive_body)?;
        Ok(details)
    }

    fn fixture_input() -> Result<MeshAuditEventInput, MeshAuditLedgerError> {
        Ok(MeshAuditEventInput {
            workspace_id: WORKSPACE_ID.to_owned(),
            event_kind: MeshAuditEventKind::Export,
            peer_id: Some("peer_alpha".to_owned()),
            origin_workspace_id: Some("origin_workspace_a".to_owned()),
            target_workspace_id: Some("target_workspace_b".to_owned()),
            workspace_scope: Some("workspace:repo-only".to_owned()),
            policy_decision_id: Some("policy_decision_001".to_owned()),
            local_row_refs: vec!["mem_b".to_owned(), "mem_a".to_owned(), "mem_a".to_owned()],
            cached_body_refs: vec!["body_cache_ref_001".to_owned()],
            details: fixture_details()?,
            previous_event_hash: None,
        })
    }

    fn fresh_db() -> Result<DbConnection, String> {
        let connection = DbConnection::open_memory().map_err(|error| format!("open: {error}"))?;
        connection
            .migrate()
            .map_err(|error| format!("migrate: {error}"))?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: WORKSPACE_PATH.to_owned(),
                    name: Some("mesh-audit".to_owned()),
                },
            )
            .map_err(|error| format!("workspace insert: {error}"))?;
        Ok(connection)
    }

    #[test]
    fn redacted_details_do_not_store_raw_body_or_secret_text() -> TestResult {
        let event = compute_mesh_audit_event(&fixture_input().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let rendered = serde_json::to_string(&event).map_err(|error| error.to_string())?;

        assert!(!rendered.contains("do-not-store"));
        assert!(!rendered.contains("raw body text"));
        assert!(rendered.contains("redacted_text"));
        assert!(rendered.contains("blake3:"));
        Ok(())
    }

    #[test]
    fn event_hash_is_stable_and_links_previous_event_hash() -> TestResult {
        let first = compute_mesh_audit_event(&fixture_input().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let repeated =
            compute_mesh_audit_event(&fixture_input().map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        assert_eq!(first.event_id, repeated.event_id);
        assert_eq!(first.event_hash, repeated.event_hash);
        assert_eq!(
            first.local_row_refs,
            vec!["mem_a".to_owned(), "mem_b".to_owned()]
        );

        let mut second_input = fixture_input().map_err(|error| error.to_string())?;
        second_input.event_kind = MeshAuditEventKind::Import;
        second_input.previous_event_hash = Some(first.event_hash.clone());
        let second = compute_mesh_audit_event(&second_input).map_err(|error| error.to_string())?;
        assert_eq!(
            second.previous_event_hash.as_deref(),
            Some(first.event_hash.as_str())
        );
        assert_ne!(first.event_hash, second.event_hash);
        Ok(())
    }

    #[test]
    fn unsafe_reference_detail_is_rejected() -> TestResult {
        let mut details = MeshAuditDetails::default();
        let sensitive_reference = ["pass", "word=super-", "sec", "ret"].concat();
        let error = match details.insert_reference("raw_body", &sensitive_reference) {
            Ok(()) => return Err("secret-like detail should reject".to_owned()),
            Err(error) => error,
        };
        assert!(error.to_string().contains("raw secret"));
        Ok(())
    }

    #[test]
    fn repository_append_writes_existing_audit_log_row() -> TestResult {
        let connection = fresh_db()?;
        let event = compute_mesh_audit_event(&fixture_input().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;

        let audit_id = append_mesh_audit_event(&connection, &event, Some("LavenderHollow"))
            .map_err(|error| error.to_string())?;
        let row = connection
            .get_audit(&audit_id)
            .map_err(|error| format!("get audit: {error}"))?
            .ok_or_else(|| "audit row missing".to_owned())?;

        assert_eq!(row.action, "mesh.audit.export");
        assert_eq!(row.surface, "mesh");
        assert_eq!(row.target_id.as_deref(), Some(event.event_id.as_str()));
        assert_eq!(row.actor.as_deref(), Some("LavenderHollow"));
        let parsed: serde_json::Value = serde_json::from_str(
            row.details
                .as_deref()
                .ok_or_else(|| "details missing".to_owned())?,
        )
        .map_err(|error| format!("parse details: {error}"))?;
        assert_eq!(parsed["schema"], MESH_AUDIT_EVENT_SCHEMA_V1);
        assert_eq!(parsed["eventHash"], event.event_hash);
        assert_eq!(parsed["cachedBodyRefs"][0], "body_cache_ref_001");
        Ok(())
    }

    #[test]
    fn support_bundle_projection_excludes_details_and_row_refs() -> TestResult {
        let event = compute_mesh_audit_event(&fixture_input().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        let bundle_entry = support_bundle_entry(&event);
        let rendered = serde_json::to_string(&bundle_entry).map_err(|error| error.to_string())?;

        assert_eq!(bundle_entry.local_row_count, 2);
        assert_eq!(bundle_entry.cached_body_ref_count, 1);
        assert!(!rendered.contains("representative_body"));
        assert!(!rendered.contains("body_cache_ref_001"));
        assert_eq!(bundle_entry.event_hash, event.event_hash);
        Ok(())
    }
}
