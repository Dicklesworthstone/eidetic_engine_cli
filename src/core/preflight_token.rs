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

use super::preflight_guard::{GuardMatch, MatchResolution, PreflightMemoryMatch};

pub const PREFLIGHT_BYPASS_TOKEN_SCHEMA_V1: &str = "ee.preflight.bypass_token.v1";
pub const PREFLIGHT_BYPASS_AUDIT_SCHEMA_V1: &str = "ee.preflight.bypass.v1";
pub const PREFLIGHT_HALT_AUDIT_SCHEMA_V1: &str = "ee.preflight.halt.v1";
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
    pub command: String,
    pub rule_ids: Vec<String>,
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
    pub command: String,
    pub rule_ids: Vec<String>,
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

#[derive(Clone, Debug, PartialEq)]
pub struct RecordPreflightHaltAuditOptions {
    pub workspace_id: String,
    pub actor: Option<String>,
    pub command: String,
    pub matches: Vec<GuardMatch>,
    pub matched_memories: Vec<PreflightMemoryMatch>,
    pub exit_code: u32,
    pub checked_at: String,
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
    pub command: String,
    pub command_hash: String,
    pub rule_ids: Vec<String>,
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
    pub command_hash: String,
    pub audit_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightHaltAuditReport {
    pub schema: String,
    pub command_hash: String,
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
    pub command: String,
    pub command_hash: String,
    pub rule_ids: Vec<String>,
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

#[must_use]
pub fn normalize_bypass_command_scope(command: &str) -> String {
    command.trim().to_owned()
}

#[must_use]
pub fn canonical_bypass_rule_ids(rule_ids: &[String]) -> Vec<String> {
    let mut ids = rule_ids
        .iter()
        .map(|rule_id| rule_id.trim())
        .filter(|rule_id| !rule_id.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

#[must_use]
pub fn bypass_command_scope_hash(command: &str, rule_ids: &[String]) -> String {
    let command = normalize_bypass_command_scope(command);
    let rule_ids = canonical_bypass_rule_ids(rule_ids);
    let mut payload = String::with_capacity(
        command.len() + rule_ids.iter().map(String::len).sum::<usize>() + rule_ids.len() + 1,
    );
    payload.push_str(&command);
    payload.push('\0');
    for rule_id in &rule_ids {
        payload.push_str(rule_id);
        payload.push('\0');
    }
    format!("blake3:{}", blake3::hash(payload.as_bytes()).to_hex())
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
    let command = normalize_bypass_command_scope(&options.command);
    let rule_ids = canonical_bypass_rule_ids(&options.rule_ids);
    let command_hash = bypass_command_scope_hash(&command, &rule_ids);
    let rule_ids_json = serde_json::to_string(&rule_ids).map_err(|error| {
        PreflightBypassTokenError::new(
            BYPASS_TOKEN_STORAGE_ERROR,
            "critical",
            format!("failed to encode preflight bypass token rule ids: {error}"),
            "Retry after inspecting the preflight rule identifiers.",
            None,
        )
    })?;
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
                command: command.clone(),
                command_hash: command_hash.clone(),
                rule_ids_json,
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
            "command": &command,
            "command_hash": &command_hash,
            "rule_ids": &rule_ids,
            "reason": options.reason,
        }),
    )?;

    tracing::info!(
        action = audit_actions::PREFLIGHT_BYPASS_TOKEN_ISSUE,
        workspace_id = %options.workspace_id,
        token_hash_prefix = %prefix,
        command_hash = %command_hash,
        rule_count = rule_ids.len(),
        max_uses,
        expires_at = %expires_at.to_rfc3339(),
        "issued preflight bypass token"
    );

    Ok(BypassTokenIssueReport {
        schema: PREFLIGHT_BYPASS_TOKEN_SCHEMA_V1.to_owned(),
        token,
        token_hash_prefix: prefix,
        command,
        command_hash,
        rule_ids,
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

    let command = normalize_bypass_command_scope(&options.command);
    let rule_ids = canonical_bypass_rule_ids(&options.rule_ids);
    let command_hash = bypass_command_scope_hash(&command, &rule_ids);
    let stored_rule_ids = stored_token_rule_ids(&token)?;
    if token.command != command || token.command_hash != command_hash || stored_rule_ids != rule_ids
    {
        audit_reject(
            connection,
            options,
            &prefix,
            BYPASS_TOKEN_INVALID,
            "command scope mismatch",
        );
        return Err(scope_mismatch_error(prefix));
    }

    if token.revoked_at.is_some() {
        audit_reject(
            connection,
            options,
            &prefix,
            BYPASS_TOKEN_REVOKED,
            "token revoked",
        );
        return Err(revoked_token_error(prefix));
    }

    if parse_rfc3339_utc(&token.expires_at)? <= now {
        audit_reject(
            connection,
            options,
            &prefix,
            BYPASS_TOKEN_EXPIRED,
            "token expired",
        );
        return Err(expired_token_error(prefix));
    }

    if token.used_count >= token.max_uses {
        audit_reject(
            connection,
            options,
            &prefix,
            BYPASS_TOKEN_EXHAUSTED,
            "token exhausted",
        );
        return Err(exhausted_token_error(prefix));
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
    let used_at = now.to_rfc3339();
    let consumed = connection
        .increment_preflight_bypass_token_use(&hash, &used_at)
        .map_err(storage_error)?;
    if !consumed {
        return Err(classify_failed_token_consume(
            connection, options, &hash, &prefix, now,
        )?);
    }

    let audit_id = insert_token_audit(
        connection,
        &options.workspace_id,
        options.actor.as_deref(),
        audit_actions::PREFLIGHT_BYPASS_TOKEN_USE,
        &prefix,
        json!({
            "token_hash_prefix": prefix,
            "command_hash": &command_hash,
            "rule_ids": &rule_ids,
            "used_count": used_count,
            "remaining_uses": token.max_uses.saturating_sub(used_count),
        }),
    )?;

    tracing::info!(
        action = audit_actions::PREFLIGHT_BYPASS_TOKEN_USE,
        workspace_id = %options.workspace_id,
        token_hash_prefix = %prefix,
        command_hash = %command_hash,
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
    let command = normalize_bypass_command_scope(&options.command);
    let command_hash = bypass_command_scope_hash(&command, &rule_ids);
    let audit_id = insert_token_audit(
        connection,
        &options.workspace_id,
        options.actor.as_deref(),
        audit_actions::PREFLIGHT_BYPASS,
        &prefix,
        json!({
            "schema": PREFLIGHT_BYPASS_AUDIT_SCHEMA_V1,
            "token_hash_prefix": &prefix,
            "command": &command,
            "commandHash": &command_hash,
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
        command_hash = %command_hash,
        match_count = options.matches.len(),
        matched_memory_count = options.matched_memories.len(),
        "recorded preflight bypass provenance"
    );

    Ok(PreflightBypassAuditReport {
        schema: PREFLIGHT_BYPASS_AUDIT_SCHEMA_V1.to_owned(),
        token_hash_prefix: prefix,
        command_hash,
        audit_id,
    })
}

pub fn record_preflight_halt_audit(
    connection: &DbConnection,
    options: &RecordPreflightHaltAuditOptions,
) -> Result<PreflightHaltAuditReport> {
    let command = normalize_bypass_command_scope(&options.command);
    let rule_ids = canonical_guard_rule_ids(&options.matches);
    let command_hash = bypass_command_scope_hash(&command, &rule_ids);
    let enforced_halt_rule_ids = options
        .matches
        .iter()
        .filter(|matched| {
            matched.action.stops_execution() && matched.resolution == MatchResolution::Enforced
        })
        .map(|matched| matched.rule_id.clone())
        .collect::<Vec<_>>();
    let matched_memory_ids = options
        .matched_memories
        .iter()
        .map(|memory| memory.memory_id.clone())
        .collect::<Vec<_>>();
    let audit_id = generate_audit_id();
    let actor = options.actor.as_deref().map(preflight_audit_text);
    let details = preflight_audit_details_json(json!({
        "schema": PREFLIGHT_HALT_AUDIT_SCHEMA_V1,
        "command": command,
        "commandHash": command_hash,
        "exitCode": options.exit_code,
        "checkedAt": options.checked_at,
        "ruleIds": rule_ids,
        "enforcedHaltRuleIds": enforced_halt_rule_ids,
        "matchedMemoryIds": matched_memory_ids,
        "matches": options.matches,
        "matchedMemories": options.matched_memories,
    }));
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(options.workspace_id.clone()),
                actor,
                action: audit_actions::PREFLIGHT_HALT.to_owned(),
                target_type: Some("preflight_guard".to_owned()),
                target_id: Some(command_hash.clone()),
                details: Some(details),
            },
        )
        .map_err(storage_error)?;

    tracing::info!(
        action = audit_actions::PREFLIGHT_HALT,
        workspace_id = %options.workspace_id,
        command_hash = %command_hash,
        match_count = options.matches.len(),
        matched_memory_count = options.matched_memories.len(),
        "recorded preflight halt provenance"
    );

    Ok(PreflightHaltAuditReport {
        schema: PREFLIGHT_HALT_AUDIT_SCHEMA_V1.to_owned(),
        command_hash,
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
        .map(bypass_token_list_entry)
        .collect::<Result<Vec<_>>>()?;
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
    if normalize_bypass_command_scope(&options.command).is_empty() {
        errors.insert("command", "must not be empty");
    }
    if canonical_bypass_rule_ids(&options.rule_ids).is_empty() {
        errors.insert("rule_ids", "must include at least one matching rule id");
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
        "Provide workspace_id, issuer_workspace, command, rule_ids, reason, ttl 1..60, and max_uses >= 1.",
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

fn scope_mismatch_error(prefix: String) -> PreflightBypassTokenError {
    PreflightBypassTokenError::new(
        BYPASS_TOKEN_INVALID,
        "high",
        "preflight bypass token is not valid for this command and rule set",
        "Issue a fresh bypass token for this exact command after human confirmation.",
        Some(prefix),
    )
}

fn revoked_token_error(prefix: String) -> PreflightBypassTokenError {
    PreflightBypassTokenError::new(
        BYPASS_TOKEN_REVOKED,
        "high",
        "preflight bypass token has been revoked",
        "Issue a fresh bypass token after renewed human confirmation.",
        Some(prefix),
    )
}

fn expired_token_error(prefix: String) -> PreflightBypassTokenError {
    PreflightBypassTokenError::new(
        BYPASS_TOKEN_EXPIRED,
        "medium",
        "preflight bypass token has expired",
        "Issue a fresh bypass token with an explicit reason.",
        Some(prefix),
    )
}

fn exhausted_token_error(prefix: String) -> PreflightBypassTokenError {
    PreflightBypassTokenError::new(
        BYPASS_TOKEN_EXHAUSTED,
        "high",
        "preflight bypass token has no remaining uses",
        "Issue a fresh one-shot bypass token if the command is still approved.",
        Some(prefix),
    )
}

fn failed_consume_storage_error(prefix: String) -> PreflightBypassTokenError {
    PreflightBypassTokenError::new(
        BYPASS_TOKEN_STORAGE_ERROR,
        "critical",
        "preflight bypass token use update affected no rows while the token still appeared usable",
        "Run `ee doctor --json` and inspect preflight_bypass_tokens rows for inconsistent state.",
        Some(prefix),
    )
}

fn classify_failed_token_consume(
    connection: &DbConnection,
    options: &VerifyBypassTokenOptions,
    hash: &str,
    prefix: &str,
    now: DateTime<Utc>,
) -> Result<PreflightBypassTokenError> {
    let token = connection
        .get_preflight_bypass_token(hash)
        .map_err(storage_error)?
        .ok_or_else(|| {
            audit_reject(
                connection,
                options,
                prefix,
                BYPASS_TOKEN_INVALID,
                "token disappeared before use update",
            );
            invalid_token_error(prefix.to_owned())
        })?;

    if token.workspace_id != options.workspace_id {
        audit_reject(
            connection,
            options,
            prefix,
            BYPASS_TOKEN_INVALID,
            "workspace mismatch before use update",
        );
        return Ok(invalid_token_error(prefix.to_owned()));
    }

    if token.revoked_at.is_some() {
        audit_reject(
            connection,
            options,
            prefix,
            BYPASS_TOKEN_REVOKED,
            "token revoked before use update",
        );
        return Ok(revoked_token_error(prefix.to_owned()));
    }

    if parse_rfc3339_utc(&token.expires_at)? <= now {
        audit_reject(
            connection,
            options,
            prefix,
            BYPASS_TOKEN_EXPIRED,
            "token expired before use update",
        );
        return Ok(expired_token_error(prefix.to_owned()));
    }

    if token.used_count >= token.max_uses {
        audit_reject(
            connection,
            options,
            prefix,
            BYPASS_TOKEN_EXHAUSTED,
            "token exhausted before use update",
        );
        return Ok(exhausted_token_error(prefix.to_owned()));
    }

    Ok(failed_consume_storage_error(prefix.to_owned()))
}

fn stored_token_rule_ids(token: &StoredPreflightBypassToken) -> Result<Vec<String>> {
    serde_json::from_str::<Vec<String>>(&token.rule_ids_json)
        .map(|rule_ids| canonical_bypass_rule_ids(&rule_ids))
        .map_err(|error| {
            PreflightBypassTokenError::new(
                BYPASS_TOKEN_STORAGE_ERROR,
                "critical",
                format!("stored preflight bypass token rule scope is invalid: {error}"),
                "Run `ee doctor --json` and inspect preflight_bypass_tokens rows.",
                Some(token.token_hash_prefix.clone()),
            )
        })
}

fn audit_reject(
    connection: &DbConnection,
    options: &VerifyBypassTokenOptions,
    token_hash_prefix: &str,
    code: &'static str,
    reason: &'static str,
) {
    let command = normalize_bypass_command_scope(&options.command);
    let rule_ids = canonical_bypass_rule_ids(&options.rule_ids);
    let command_hash = bypass_command_scope_hash(&command, &rule_ids);
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
            "command": &command,
            "command_hash": &command_hash,
            "rule_ids": &rule_ids,
        }),
    ) {
        tracing::error!(%error, "failed to insert token audit");
    }
}

fn canonical_guard_rule_ids(matches: &[GuardMatch]) -> Vec<String> {
    let mut rule_ids = matches
        .iter()
        .map(|matched| matched.rule_id.trim())
        .filter(|rule_id| !rule_id.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    rule_ids.sort();
    rule_ids.dedup();
    rule_ids
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
    let actor = actor.map(preflight_audit_text);
    let details = preflight_audit_details_json(details);
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor,
                action: action.to_owned(),
                target_type: Some("preflight_bypass_token".to_owned()),
                target_id: Some(token_hash_prefix.to_owned()),
                details: Some(details),
            },
        )
        .map_err(storage_error)?;
    Ok(audit_id)
}

fn preflight_audit_details_json(details: serde_json::Value) -> String {
    preflight_audit_text(&details.to_string())
}

fn preflight_audit_text(text: &str) -> String {
    crate::output::redact_mesh_approval_bearers(text)
}

fn bypass_token_list_entry(token: StoredPreflightBypassToken) -> Result<BypassTokenListEntry> {
    // Reuse the verification-path parser so a token with corrupt stored
    // rule scope fails the listing with BYPASS_TOKEN_STORAGE_ERROR
    // instead of displaying an empty (and therefore wrong) rule set.
    let rule_ids = stored_token_rule_ids(&token)?;
    Ok(BypassTokenListEntry {
        token_hash_prefix: token.token_hash_prefix,
        issued_at: token.issued_at,
        expires_at: token.expires_at,
        max_uses: token.max_uses,
        used_count: token.used_count,
        revoked: token.revoked_at.is_some(),
        issuer_workspace: token.issuer_workspace,
        reason: token.reason,
        command: token.command,
        command_hash: token.command_hash,
        rule_ids,
    })
}

#[cfg(test)]
mod tests {
    use crate::core::preflight_guard::{
        GuardAction, GuardMatch, MatchResolution, PreflightMemoryMatch, RuleSource,
    };
    use crate::db::{CreateWorkspaceInput, DbConnection};

    use super::{
        PREFLIGHT_BYPASS_AUDIT_SCHEMA_V1, PREFLIGHT_HALT_AUDIT_SCHEMA_V1,
        RecordPreflightBypassAuditOptions, RecordPreflightHaltAuditOptions,
        bypass_command_scope_hash, record_preflight_bypass_audit, record_preflight_halt_audit,
    };

    const WORKSPACE_ID: &str = "wsp_preflightbeareraudit00000";
    const RULE_ID: &str = "builtin:approval_bearer_audit_test";

    fn mesh_approval_bearer_canary() -> String {
        format!(
            "{}{}",
            ["e", "e", "a", "p", "1", "_"].concat(),
            "A".repeat(151)
        )
    }

    fn test_connection() -> std::result::Result<DbConnection, String> {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/preflight-bearer-audit".to_owned(),
                    name: Some("preflight-bearer-audit".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }

    fn guard_match(bearer: &str, resolution: MatchResolution) -> GuardMatch {
        GuardMatch {
            rule_id: RULE_ID.to_owned(),
            pattern: format!("safe pattern before {bearer} and after"),
            action: GuardAction::Halt,
            message: format!("safe guard message before {bearer} and after"),
            source: RuleSource::Builtin {
                name: "approval_bearer_audit_test".to_owned(),
            },
            resolution,
        }
    }

    fn memory_match(bearer: &str) -> PreflightMemoryMatch {
        PreflightMemoryMatch {
            memory_id: "mem_preflight_bearer_audit".to_owned(),
            kind: "risk".to_owned(),
            content: format!("safe memory context before {bearer} and after"),
            provenance_uri: Some("memory://mem_preflight_bearer_audit".to_owned()),
            severity: "high",
            severity_source: "risk_memory",
            score: 1.0,
            matched_terms: vec!["safe-term".to_owned()],
        }
    }

    fn audit_projection(
        connection: &DbConnection,
        audit_id: &str,
    ) -> std::result::Result<(Option<String>, String), String> {
        let entry = connection
            .get_audit(audit_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("audit row {audit_id} must exist"))?;
        let details = entry
            .details
            .ok_or_else(|| format!("audit row {audit_id} must have details"))?;
        Ok((entry.actor, details))
    }

    fn assert_safe_audit_actor(
        actor: Option<&str>,
        bearer: &str,
    ) -> std::result::Result<(), String> {
        let actor = actor.ok_or_else(|| "preflight audit actor must be present".to_owned())?;
        if actor.contains(bearer) {
            return Err(format!(
                "preflight audit actor exposed an approval bearer: {actor}"
            ));
        }
        if !actor.contains("safe actor before [REDACTED:mesh_approval_token] and after") {
            return Err(format!(
                "preflight audit actor lost its safe redacted projection: {actor}"
            ));
        }
        Ok(())
    }

    fn assert_safe_audit_details(
        details: &str,
        bearer: &str,
        expected_schema: &str,
    ) -> std::result::Result<(), String> {
        if details.contains(bearer) {
            return Err(format!(
                "preflight audit details exposed an approval bearer: {details}"
            ));
        }
        if !details.contains("[REDACTED:mesh_approval_token]") {
            return Err(format!(
                "preflight audit details omitted the approval-bearer marker: {details}"
            ));
        }
        if !details.contains("safe") || !details.contains("and after") {
            return Err(format!(
                "preflight audit redaction discarded useful safe context: {details}"
            ));
        }
        let value: serde_json::Value =
            serde_json::from_str(details).map_err(|error| error.to_string())?;
        if value.get("schema").and_then(serde_json::Value::as_str) != Some(expected_schema) {
            return Err(format!(
                "preflight audit retained the wrong schema after redaction: {details}"
            ));
        }
        Ok(())
    }

    #[test]
    fn preflight_halt_and_bypass_audits_scrub_mesh_approval_bearers() -> Result<(), String> {
        let connection = test_connection()?;
        let bearer = mesh_approval_bearer_canary();
        if bearer.len() != crate::mesh::lane_grant::APPROVAL_TOKEN_BEARER_LEN {
            return Err("approval bearer canary has the wrong length".to_owned());
        }
        let command = format!("safe command before {bearer} and after");
        let expected_command_hash = bypass_command_scope_hash(&command, &[RULE_ID.to_owned()]);
        let actor = format!("safe actor before {bearer} and after");

        let halt = record_preflight_halt_audit(
            &connection,
            &RecordPreflightHaltAuditOptions {
                workspace_id: WORKSPACE_ID.to_owned(),
                actor: Some(actor.clone()),
                command: command.clone(),
                matches: vec![guard_match(&bearer, MatchResolution::Enforced)],
                matched_memories: vec![memory_match(&bearer)],
                exit_code: 7,
                checked_at: "2026-08-04T00:00:00Z".to_owned(),
            },
        )
        .map_err(|error| error.to_string())?;
        if halt.command_hash != expected_command_hash {
            return Err("halt audit command hash was not bound to the original command".to_owned());
        }
        let (halt_actor, halt_details) = audit_projection(&connection, &halt.audit_id)?;
        assert_safe_audit_actor(halt_actor.as_deref(), &bearer)?;
        assert_safe_audit_details(&halt_details, &bearer, PREFLIGHT_HALT_AUDIT_SCHEMA_V1)?;

        let bypass = record_preflight_bypass_audit(
            &connection,
            &RecordPreflightBypassAuditOptions {
                workspace_id: WORKSPACE_ID.to_owned(),
                token: "safe-preflight-bypass-token".to_owned(),
                actor: Some(actor),
                command,
                matches: vec![guard_match(&bearer, MatchResolution::BypassedWithToken)],
                matched_memories: vec![memory_match(&bearer)],
            },
        )
        .map_err(|error| error.to_string())?;
        if bypass.command_hash != expected_command_hash {
            return Err(
                "bypass audit command hash was not bound to the original command".to_owned(),
            );
        }
        let (bypass_actor, bypass_details) = audit_projection(&connection, &bypass.audit_id)?;
        assert_safe_audit_actor(bypass_actor.as_deref(), &bearer)?;
        assert_safe_audit_details(&bypass_details, &bearer, PREFLIGHT_BYPASS_AUDIT_SCHEMA_V1)
    }
}
