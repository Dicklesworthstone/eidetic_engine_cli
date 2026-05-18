//! DB-backed preflight bypass token lifecycle.
//!
//! Bypass tokens are one-shot by default, short-lived, stored only as BLAKE3
//! hashes, and audited on issue/use/reject/revoke. The raw token is returned
//! only from issuance so CLI callers can hand it to a human confirmation flow.

use std::collections::BTreeMap;
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::{
    CreateAuditInput, CreatePreflightBypassTokenInput, DbConnection, StoredPreflightBypassToken,
    audit_actions, generate_audit_id,
};

use super::preflight_guard::{GuardMatch, PreflightMemoryMatch};

pub const PREFLIGHT_BYPASS_TOKEN_SCHEMA_V1: &str = "ee.preflight.bypass_token.v1";
pub const PREFLIGHT_BYPASS_AUDIT_SCHEMA_V1: &str = "ee.preflight.bypass.v1";
pub const DEFAULT_TTL_MINUTES: i64 = 10;
pub const MAX_TTL_MINUTES: i64 = 60;
pub const DEFAULT_MAX_USES: u32 = 1;
pub const BYPASS_RATE_LIMIT_PER_HOUR: u32 = 5;
pub const TOKEN_BYTES: usize = 32;

pub const BYPASS_RATE_LIMIT_EXCEEDED: &str = "bypass_rate_limit_exceeded";
pub const BYPASS_TOKEN_EXPIRED: &str = "bypass_token_expired";
pub const BYPASS_TOKEN_REVOKED: &str = "bypass_token_revoked";
pub const BYPASS_TOKEN_INVALID: &str = "bypass_token_invalid";
pub const BYPASS_TOKEN_EXHAUSTED: &str = "bypass_token_exhausted";
pub const BYPASS_TOKEN_STORAGE_ERROR: &str = "bypass_token_storage_error";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueBypassTokenOptions {
    pub workspace_id: String,
    pub issuer_workspace: String,
    pub reason: String,
    pub ttl_minutes: Option<i64>,
    pub max_uses: Option<u32>,
    pub actor: Option<String>,
    pub now: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyBypassTokenOptions {
    pub workspace_id: String,
    pub token: String,
    pub actor: Option<String>,
    pub now: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordPreflightBypassAuditOptions {
    pub workspace_id: String,
    pub token: String,
    pub actor: Option<String>,
    pub command: String,
    pub matches: Vec<GuardMatch>,
    pub matched_memories: Vec<PreflightMemoryMatch>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevokeBypassTokenOptions {
    pub workspace_id: String,
    pub token: String,
    pub actor: Option<String>,
    pub now: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassTokenIssueReport {
    pub schema: String,
    pub token: String,
    pub token_hash_prefix: String,
    pub expires_at: String,
    pub max_uses: u32,
    pub audit_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassTokenUseReport {
    pub schema: String,
    pub token_hash_prefix: String,
    pub used_count: u32,
    pub remaining_uses: u32,
    pub audit_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightBypassAuditReport {
    pub schema: String,
    pub token_hash_prefix: String,
    pub audit_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassTokenRevokeReport {
    pub schema: String,
    pub token_hash_prefix: String,
    pub revoked_at: String,
    pub audit_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassTokenListEntry {
    pub token_hash_prefix: String,
    pub issued_at: String,
    pub expires_at: String,
    pub max_uses: u32,
    pub used_count: u32,
    pub revoked: bool,
    pub issuer_workspace: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassTokenListReport {
    pub schema: String,
    pub workspace_id: String,
    pub tokens: Vec<BypassTokenListEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightBypassTokenError {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
    pub token_hash_prefix: Option<String>,
}

impl PreflightBypassTokenError {
    fn new(
        code: &'static str,
        severity: &'static str,
        message: impl Into<String>,
        repair: impl Into<String>,
        token_hash_prefix: Option<String>,
    ) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            repair: repair.into(),
            token_hash_prefix,
        }
    }
}

impl fmt::Display for PreflightBypassTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PreflightBypassTokenError {}

pub type Result<T> = std::result::Result<T, PreflightBypassTokenError>;

#[must_use]
pub fn token_hash(raw_token: &str) -> String {
    format!("blake3:{}", blake3::hash(raw_token.as_bytes()).to_hex())
}

#[must_use]
pub fn token_hash_prefix(hash: &str) -> String {
    hash.chars().take(20).collect()
}

pub fn generate_bypass_token() -> Result<String> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| {
        PreflightBypassTokenError::new(
            BYPASS_TOKEN_STORAGE_ERROR,
            "critical",
            format!("failed to read operating-system randomness: {error}"),
            "Retry on a host with a healthy OS CSPRNG.",
            None,
        )
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn issue_bypass_token(
    connection: &DbConnection,
    options: &IssueBypassTokenOptions,
) -> Result<BypassTokenIssueReport> {
    validate_issue_options(options)?;
    let now = options.now.unwrap_or_else(Utc::now);
    let ttl_minutes = options.ttl_minutes.unwrap_or(DEFAULT_TTL_MINUTES);
    let max_uses = options.max_uses.unwrap_or(DEFAULT_MAX_USES);
    let expires_at = now + Duration::minutes(ttl_minutes);
    let token = generate_bypass_token()?;
    let hash = token_hash(&token);
    let prefix = token_hash_prefix(&hash);

    connection
        .insert_preflight_bypass_token(
            &hash,
            &CreatePreflightBypassTokenInput {
                workspace_id: options.workspace_id.clone(),
                issued_at: now.to_rfc3339(),
                expires_at: expires_at.to_rfc3339(),
                max_uses,
                issuer_workspace: options.issuer_workspace.clone(),
                reason: options.reason.clone(),
            },
        )
        .map_err(storage_error)?;

    let audit_id = insert_token_audit(
        connection,
        &options.workspace_id,
        options.actor.as_deref(),
        audit_actions::PREFLIGHT_BYPASS_TOKEN_ISSUE,
        &prefix,
        json!({
            "token_hash_prefix": prefix,
            "expires_at": expires_at.to_rfc3339(),
            "max_uses": max_uses,
            "issuer_workspace": options.issuer_workspace,
            "reason": options.reason,
        }),
    )?;

    tracing::info!(
        action = audit_actions::PREFLIGHT_BYPASS_TOKEN_ISSUE,
        workspace_id = %options.workspace_id,
        token_hash_prefix = %prefix,
        max_uses,
        expires_at = %expires_at.to_rfc3339(),
        "issued preflight bypass token"
    );

    Ok(BypassTokenIssueReport {
        schema: PREFLIGHT_BYPASS_TOKEN_SCHEMA_V1.to_owned(),
        token,
        token_hash_prefix: prefix,
        expires_at: expires_at.to_rfc3339(),
        max_uses,
        audit_id,
    })
}

pub fn verify_bypass_token(
    connection: &DbConnection,
    options: &VerifyBypassTokenOptions,
) -> Result<BypassTokenUseReport> {
    let now = options.now.unwrap_or_else(Utc::now);
    let hash = token_hash(&options.token);
    let prefix = token_hash_prefix(&hash);
    let token = connection
        .get_preflight_bypass_token(&hash)
        .map_err(storage_error)?
        .ok_or_else(|| {
            audit_reject(
                connection,
                options,
                &prefix,
                BYPASS_TOKEN_INVALID,
                "token not found",
            );
            invalid_token_error(prefix.clone())
        })?;

    if token.workspace_id != options.workspace_id {
        audit_reject(
            connection,
            options,
            &prefix,
            BYPASS_TOKEN_INVALID,
            "workspace mismatch",
        );
        return Err(invalid_token_error(prefix));
    }

    if token.revoked_at.is_some() {
        audit_reject(
            connection,
            options,
            &prefix,
            BYPASS_TOKEN_REVOKED,
            "token revoked",
        );
        return Err(PreflightBypassTokenError::new(
            BYPASS_TOKEN_REVOKED,
            "high",
            "preflight bypass token has been revoked",
            "Issue a fresh bypass token after renewed human confirmation.",
            Some(prefix),
        ));
    }

    if parse_rfc3339_utc(&token.expires_at)? <= now {
        audit_reject(
            connection,
            options,
            &prefix,
            BYPASS_TOKEN_EXPIRED,
            "token expired",
        );
        return Err(PreflightBypassTokenError::new(
            BYPASS_TOKEN_EXPIRED,
            "medium",
            "preflight bypass token has expired",
            "Issue a fresh bypass token with an explicit reason.",
            Some(prefix),
        ));
    }

    if token.used_count >= token.max_uses {
        audit_reject(
            connection,
            options,
            &prefix,
            BYPASS_TOKEN_EXHAUSTED,
            "token exhausted",
        );
        return Err(PreflightBypassTokenError::new(
            BYPASS_TOKEN_EXHAUSTED,
            "high",
            "preflight bypass token has no remaining uses",
            "Issue a fresh one-shot bypass token if the command is still approved.",
            Some(prefix),
        ));
    }

    let since = now - Duration::hours(1);
    let recent_uses = connection
        .count_preflight_bypass_token_uses_since(&options.workspace_id, &since.to_rfc3339())
        .map_err(storage_error)?;
    if recent_uses >= BYPASS_RATE_LIMIT_PER_HOUR {
        audit_reject(
            connection,
            options,
            &prefix,
            BYPASS_RATE_LIMIT_EXCEEDED,
            "workspace bypass rate limit exceeded",
        );
        return Err(PreflightBypassTokenError::new(
            BYPASS_RATE_LIMIT_EXCEEDED,
            "high",
            "workspace exceeded the preflight bypass token rate limit",
            "Wait for the hourly window to clear or inspect recent bypass audit rows.",
            Some(prefix),
        ));
    }

    let used_count = token.used_count.saturating_add(1);
    connection
        .increment_preflight_bypass_token_use(&hash, &now.to_rfc3339())
        .map_err(storage_error)?;

    let audit_id = insert_token_audit(
        connection,
        &options.workspace_id,
        options.actor.as_deref(),
        audit_actions::PREFLIGHT_BYPASS_TOKEN_USE,
        &prefix,
        json!({
            "token_hash_prefix": prefix,
            "used_count": used_count,
            "remaining_uses": token.max_uses.saturating_sub(used_count),
        }),
    )?;

    tracing::info!(
        action = audit_actions::PREFLIGHT_BYPASS_TOKEN_USE,
        workspace_id = %options.workspace_id,
        token_hash_prefix = %prefix,
        used_count,
        remaining_uses = token.max_uses.saturating_sub(used_count),
        "used preflight bypass token"
    );

    Ok(BypassTokenUseReport {
        schema: PREFLIGHT_BYPASS_TOKEN_SCHEMA_V1.to_owned(),
        token_hash_prefix: prefix,
        used_count,
        remaining_uses: token.max_uses.saturating_sub(used_count),
        audit_id,
    })
}

pub fn record_preflight_bypass_audit(
    connection: &DbConnection,
    options: &RecordPreflightBypassAuditOptions,
) -> Result<PreflightBypassAuditReport> {
    let hash = token_hash(&options.token);
    let prefix = token_hash_prefix(&hash);
    let matched_memory_ids = options
        .matched_memories
        .iter()
        .map(|memory| memory.memory_id.clone())
        .collect::<Vec<_>>();
    let rule_ids = options
        .matches
        .iter()
        .map(|matched| matched.rule_id.clone())
        .collect::<Vec<_>>();
    let audit_id = insert_token_audit(
        connection,
        &options.workspace_id,
        options.actor.as_deref(),
        audit_actions::PREFLIGHT_BYPASS,
        &prefix,
        json!({
            "schema": PREFLIGHT_BYPASS_AUDIT_SCHEMA_V1,
            "token_hash_prefix": &prefix,
            "command": options.command,
            "rule_ids": rule_ids,
            "matched_memory_ids": matched_memory_ids,
            "matches": options.matches,
            "matched_memories": options.matched_memories,
        }),
    )?;

    tracing::info!(
        action = audit_actions::PREFLIGHT_BYPASS,
        workspace_id = %options.workspace_id,
        token_hash_prefix = %prefix,
        match_count = options.matches.len(),
        matched_memory_count = options.matched_memories.len(),
        "recorded preflight bypass provenance"
    );

    Ok(PreflightBypassAuditReport {
        schema: PREFLIGHT_BYPASS_AUDIT_SCHEMA_V1.to_owned(),
        token_hash_prefix: prefix,
        audit_id,
    })
}

pub fn revoke_bypass_token(
    connection: &DbConnection,
    options: &RevokeBypassTokenOptions,
) -> Result<BypassTokenRevokeReport> {
    let now = options.now.unwrap_or_else(Utc::now);
    let hash = token_hash(&options.token);
    let prefix = token_hash_prefix(&hash);
    let token = connection
        .get_preflight_bypass_token(&hash)
        .map_err(storage_error)?
        .ok_or_else(|| invalid_token_error(prefix.clone()))?;
    if token.workspace_id != options.workspace_id {
        return Err(invalid_token_error(prefix));
    }

    connection
        .revoke_preflight_bypass_token(&hash, &now.to_rfc3339())
        .map_err(storage_error)?;

    let audit_id = insert_token_audit(
        connection,
        &options.workspace_id,
        options.actor.as_deref(),
        audit_actions::PREFLIGHT_BYPASS_TOKEN_REVOKE,
        &prefix,
        json!({
            "token_hash_prefix": prefix,
            "revoked_at": now.to_rfc3339(),
        }),
    )?;

    tracing::info!(
        action = audit_actions::PREFLIGHT_BYPASS_TOKEN_REVOKE,
        workspace_id = %options.workspace_id,
        token_hash_prefix = %prefix,
        revoked_at = %now.to_rfc3339(),
        "revoked preflight bypass token"
    );

    Ok(BypassTokenRevokeReport {
        schema: PREFLIGHT_BYPASS_TOKEN_SCHEMA_V1.to_owned(),
        token_hash_prefix: prefix,
        revoked_at: now.to_rfc3339(),
        audit_id,
    })
}

pub fn list_bypass_tokens(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<BypassTokenListReport> {
    let tokens = connection
        .list_preflight_bypass_tokens(workspace_id)
        .map_err(storage_error)?
        .into_iter()
        .map(BypassTokenListEntry::from)
        .collect();
    Ok(BypassTokenListReport {
        schema: PREFLIGHT_BYPASS_TOKEN_SCHEMA_V1.to_owned(),
        workspace_id: workspace_id.to_owned(),
        tokens,
    })
}

fn validate_issue_options(options: &IssueBypassTokenOptions) -> Result<()> {
    let mut errors = BTreeMap::new();
    if options.workspace_id.trim().is_empty() {
        errors.insert("workspace_id", "must not be empty");
    }
    if options.issuer_workspace.trim().is_empty() {
        errors.insert("issuer_workspace", "must not be empty");
    }
    if options.reason.trim().is_empty() {
        errors.insert("reason", "must not be empty");
    }
    let ttl = options.ttl_minutes.unwrap_or(DEFAULT_TTL_MINUTES);
    if !(1..=MAX_TTL_MINUTES).contains(&ttl) {
        errors.insert("ttl_minutes", "must be between 1 and 60");
    }
    if options.max_uses.unwrap_or(DEFAULT_MAX_USES) == 0 {
        errors.insert("max_uses", "must be at least 1");
    }
    if errors.is_empty() {
        return Ok(());
    }
    Err(PreflightBypassTokenError::new(
        BYPASS_TOKEN_INVALID,
        "medium",
        format!("invalid preflight bypass token options: {errors:?}"),
        "Provide workspace_id, issuer_workspace, reason, ttl 1..60, and max_uses >= 1.",
        None,
    ))
}

fn parse_rfc3339_utc(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            PreflightBypassTokenError::new(
                BYPASS_TOKEN_STORAGE_ERROR,
                "critical",
                format!("stored preflight bypass token timestamp is invalid: {error}"),
                "Run `ee doctor --json` and inspect preflight_bypass_tokens rows.",
                None,
            )
        })
}

fn storage_error(error: crate::db::DbError) -> PreflightBypassTokenError {
    PreflightBypassTokenError::new(
        BYPASS_TOKEN_STORAGE_ERROR,
        "critical",
        format!("preflight bypass token storage operation failed: {error}"),
        "Run `ee doctor --json` and retry after storage is healthy.",
        None,
    )
}

fn invalid_token_error(prefix: String) -> PreflightBypassTokenError {
    PreflightBypassTokenError::new(
        BYPASS_TOKEN_INVALID,
        "high",
        "preflight bypass token is invalid for this workspace",
        "Issue a fresh bypass token after human confirmation.",
        Some(prefix),
    )
}

fn audit_reject(
    connection: &DbConnection,
    options: &VerifyBypassTokenOptions,
    token_hash_prefix: &str,
    code: &'static str,
    reason: &'static str,
) {
    tracing::info!(
        action = audit_actions::PREFLIGHT_BYPASS_TOKEN_REJECT,
        workspace_id = %options.workspace_id,
        token_hash_prefix,
        code,
        reason,
        "rejected preflight bypass token"
    );
    if let Err(error) = insert_token_audit(
        connection,
        &options.workspace_id,
        options.actor.as_deref(),
        audit_actions::PREFLIGHT_BYPASS_TOKEN_REJECT,
        token_hash_prefix,
        json!({
            "token_hash_prefix": token_hash_prefix,
            "code": code,
            "reason": reason,
        }),
    ) {
        tracing::error!(%error, "failed to insert token audit");
    }
}

fn insert_token_audit(
    connection: &DbConnection,
    workspace_id: &str,
    actor: Option<&str>,
    action: &str,
    token_hash_prefix: &str,
    details: serde_json::Value,
) -> Result<String> {
    let audit_id = generate_audit_id();
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: actor.map(str::to_owned),
                action: action.to_owned(),
                target_type: Some("preflight_bypass_token".to_owned()),
                target_id: Some(token_hash_prefix.to_owned()),
                details: Some(details.to_string()),
            },
        )
        .map_err(storage_error)?;
    Ok(audit_id)
}

impl From<StoredPreflightBypassToken> for BypassTokenListEntry {
    fn from(token: StoredPreflightBypassToken) -> Self {
        Self {
            token_hash_prefix: token.token_hash_prefix,
            issued_at: token.issued_at,
            expires_at: token.expires_at,
            max_uses: token.max_uses,
            used_count: token.used_count,
            revoked: token.revoked_at.is_some(),
            issuer_workspace: token.issuer_workspace,
            reason: token.reason,
        }
    }
}
