//! Live procedural rules behind bounded error-guided retrieval.
//!
//! A helpful repair link is a locator, not permission to read a rule. Admit
//! only selected native identities under the caller's source snapshot. Rule
//! lifecycle and review holds are independent of lineage-memory lifecycle;
//! missing/foreign parents still invalidate ownership. Error text is not an
//! explicit task path, so directory and file-pattern advice is withheld.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlmodel_core::Value;

use crate::db::{DbConnection, DbError, DbOperation, Result};
use crate::models::{MemoryId, RuleId, RuleScope, TrustClass};
use crate::search::RuleIndexProjection;

use super::{PACK_RECALL_EXCERPT_BYTES, PACK_RECALL_TARGET_LIMIT, bounded_pack_recall_text};

const MAX_SOURCE_BYTES: usize = 64 * 1024;
const MAX_RELATIONS: usize = 256;

fn unavailable() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not verify native repair rule authority; no partial query returned"
            .to_owned(),
    }
}

fn current(raw: Option<&str>, at: DateTime<Utc>) -> bool {
    raw.and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .is_some_and(|observed| observed.with_timezone(&Utc) <= at)
}

fn placeholders(count: usize, first: usize) -> String {
    (first..first + count)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parameters(ids: &[&str]) -> Vec<Value> {
    ids.iter().map(|id| Value::Text((*id).to_owned())).collect()
}

/// The caller owns a read/write snapshot covering targets and source rows.
/// Work and body hydration are bounded by the already-selected target window,
/// never by the workspace's complete rule, evidence or fingerprint history.
pub(super) fn read(
    connection: &DbConnection,
    workspace: &str,
    targets: &[&str],
    at: DateTime<Utc>,
) -> Result<Vec<(String, String)>> {
    read_observed(connection, workspace, targets, at, |_| {})
}

fn read_observed(
    connection: &DbConnection,
    workspace: &str,
    targets: &[&str],
    at: DateTime<Utc>,
    mut before_hydration: impl FnMut(&[&str]),
) -> Result<Vec<(String, String)>> {
    if targets.len() > PACK_RECALL_TARGET_LIMIT as usize {
        return Err(unavailable());
    }
    let selected: BTreeSet<_> = targets
        .iter()
        .copied()
        .filter(|id| RuleId::from_str(id).is_ok_and(|parsed| parsed.to_string() == *id))
        .collect();
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<_> = selected.iter().copied().collect();
    let mut params = vec![Value::Text(workspace.to_owned())];
    params.extend(parameters(&ids));
    // Do not load text from retired, foreign, future, oversized or path-scoped
    // advice simply to discover that it was not admissible. The canonical
    // projection still checks lifecycle and scope again after hydration.
    let sql = format!(
        "SELECT id, created_at, updated_at, length(CAST(content AS BLOB))
         FROM procedural_rules WHERE workspace_id = ?1
         AND tombstoned_at IS NULL AND superseded_by IS NULL
         AND maturity IN ('candidate', 'validated')
         AND scope IN ('global', 'workspace', 'project')
         AND (scope_pattern IS NULL OR trim(scope_pattern) = '')
         AND id IN ({}) ORDER BY id ASC",
        placeholders(ids.len(), 2)
    );
    let mut admitted = Vec::new();
    let mut seen = BTreeSet::new();
    for row in connection.query(&sql, &params).map_err(|_| unavailable())? {
        let id = row.get(0).and_then(Value::as_str).ok_or_else(unavailable)?;
        if !selected.contains(id) || !seen.insert(id.to_owned()) {
            return Err(unavailable());
        }
        if current(row.get(1).and_then(Value::as_str), at)
            && current(row.get(2).and_then(Value::as_str), at)
            && row
                .get(3)
                .and_then(Value::as_i64)
                .is_some_and(|bytes| (1..=MAX_SOURCE_BYTES as i64).contains(&bytes))
        {
            admitted.push(id.to_owned());
        }
    }
    admitted.sort();
    let ids: Vec<_> = admitted.iter().map(String::as_str).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let workspace_row = connection
        .get_workspace(workspace)
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    if workspace_row.id != workspace {
        return Err(unavailable());
    }
    let mut relations = relations(connection, workspace, &ids)?;
    let ids: Vec<_> = ids
        .into_iter()
        .filter(|id| !relations.withheld.contains(*id))
        .collect();
    before_hydration(&ids);
    let mut repairs = Vec::new();
    for id in ids {
        let rule = connection
            .get_procedural_rule(id)
            .map_err(|_| unavailable())?
            .ok_or_else(unavailable)?;
        if rule.id != id || rule.workspace_id != workspace {
            return Err(unavailable());
        }
        // No location inference or filesystem walk for an error-only query.
        if !matches!(
            RuleScope::from_str(&rule.scope),
            Ok(RuleScope::Global | RuleScope::Workspace | RuleScope::Project)
        ) || TrustClass::from_str(&rule.trust_class).is_err()
            || !current(Some(&rule.created_at), at)
            || !current(Some(&rule.updated_at), at)
        {
            continue;
        }
        let projection = RuleIndexProjection::new(
            rule,
            &workspace_row.path,
            relations.tags.remove(id).unwrap_or_default(),
            relations.sources.remove(id).unwrap_or_default(),
        );
        if projection.is_pack_admissible()
            && let Some(text) = public_body(&projection.rule().content)
        {
            repairs.push((id.to_owned(), text));
        }
    }
    Ok(repairs)
}

#[derive(Default)]
struct Relations {
    withheld: BTreeSet<String>,
    tags: BTreeMap<String, Vec<String>>,
    sources: BTreeMap<String, Vec<String>>,
}

fn relations(connection: &DbConnection, workspace: &str, ids: &[&str]) -> Result<Relations> {
    let mut relations = Relations::default();
    let mut params = vec![Value::Text(workspace.to_owned())];
    params.extend(parameters(ids));
    let sql = format!(
        "SELECT DISTINCT target_id FROM feedback_quarantine WHERE workspace_id = ?1
         AND target_type = 'rule' AND status = 'pending' AND target_id IN ({})
         ORDER BY target_id ASC",
        placeholders(ids.len(), 2)
    );
    for row in connection.query(&sql, &params).map_err(|_| unavailable())? {
        let id = row.get(0).and_then(Value::as_str).ok_or_else(unavailable)?;
        if !ids.contains(&id) {
            return Err(unavailable());
        }
        relations.withheld.insert(id.to_owned());
    }
    // Resolve only rule-owned junctions. A missing source must remain visible
    // as an invalid LEFT JOIN row, not disappear and invent source-less advice.
    // Caps bound returned metadata even if a selected rule has huge lineage.
    let names = placeholders(ids.len(), 1);
    let params = parameters(ids);
    let sql = format!(
        "SELECT rsm.rule_id, rsm.memory_id, m.workspace_id FROM rule_source_memories AS rsm
         LEFT JOIN memories AS m ON m.id = rsm.memory_id WHERE rsm.rule_id IN ({names})
         ORDER BY rsm.rule_id ASC, rsm.memory_id ASC LIMIT {}",
        MAX_RELATIONS + 1
    );
    let rows = connection.query(&sql, &params).map_err(|_| unavailable())?;
    if rows.len() > MAX_RELATIONS {
        return Err(unavailable());
    }
    for row in rows {
        let id = row.get(0).and_then(Value::as_str).ok_or_else(unavailable)?;
        let source = row.get(1).and_then(Value::as_str).ok_or_else(unavailable)?;
        if !ids.contains(&id) {
            return Err(unavailable());
        }
        if row.get(2).and_then(Value::as_str) != Some(workspace)
            || !MemoryId::from_str(source).is_ok_and(|parsed| parsed.to_string() == source)
        {
            relations.withheld.insert(id.to_owned());
        }
        relations
            .sources
            .entry(id.to_owned())
            .or_default()
            .push(source.to_owned());
    }
    let sql = format!(
        "SELECT rule_id, tag FROM rule_tags WHERE rule_id IN ({names})
         ORDER BY rule_id ASC, tag ASC LIMIT {}",
        MAX_RELATIONS + 1
    );
    let rows = connection.query(&sql, &params).map_err(|_| unavailable())?;
    if rows.len() > MAX_RELATIONS {
        return Err(unavailable());
    }
    for row in rows {
        let id = row.get(0).and_then(Value::as_str).ok_or_else(unavailable)?;
        let tag = row.get(1).and_then(Value::as_str).ok_or_else(unavailable)?;
        if !ids.contains(&id) {
            return Err(unavailable());
        }
        relations
            .tags
            .entry(id.to_owned())
            .or_default()
            .push(tag.to_owned());
    }
    Ok(relations)
}

fn public_body(content: &str) -> Option<String> {
    if content.trim().is_empty()
        || content.len() > MAX_SOURCE_BYTES
        || content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
    {
        return None;
    }
    // Use the existing complete-source policy before any UTF-8 excerpt cut.
    // Never expose a screened prefix of a source whose suffix was withheld.
    let screen = crate::policy::screen_external_text_for_ingestion(content);
    if screen.redacted
        || screen.instruction_like
        || !matches!(screen.instruction_risk, "none" | "low")
        || screen.content != content
    {
        return None;
    }
    Some(bounded_pack_recall_text(
        content.trim(),
        PACK_RECALL_EXCERPT_BYTES,
    ))
}

#[cfg(test)]
#[path = "error_recall_rules_tests.rs"]
mod tests;
