//! bd-resume-verb-v0f57 — `ee resume`: the session-resume bundle.
//!
//! The question every agent session starts with is not task-conditioned
//! retrieval but "WHERE WAS I — what did the last session finish, decide,
//! and queue". `ee resume` assembles that in one read-only call:
//!
//! 1. RECENT END-STATE — the last N sessions' episodic memories, newest
//!    first. Session boundaries come from `session-*` tags when present,
//!    else write-time clustering (a gap over [`SESSION_GAP_SECONDS`] starts
//!    a new session).
//! 2. OPEN LOOPS — decisions carrying revisit conditions (the `ee decide`
//!    surface) plus memories tagged next/queue/blocking/pending/todo/revisit.
//! 3. STALENESS — surfaced items superseded by newer writes on the same
//!    subject (same kind, strictly newer timestamp, and at least one shared
//!    subject tag) are flagged rather than silently ranked down. Session tags
//!    and open-loop tags are control tags, not subject identity.
//! 4. Resume-flavored next commands, and nearby populated stores (reusing
//!    the bd-orient-store-discovery-ft1z5 scan) when the addressed store
//!    has nothing episodic to resume from.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlmodel_core::Value as SqlValue;

use crate::core::memory_scope::MemoryScopeContext;
use crate::core::orient::{
    AddressedStoreState, NEARBY_STORE_REPORT_LIMIT, NearbyStoreScanAssessment,
    NearbyStoreScanOutcome, addressed_store_state, discover_nearby_stores_for_database,
};
use crate::db::{DbConnection, StoredMemory};
use crate::models::memory::typed_memory_fields_from_json;
use crate::models::{
    DomainError, MemoryId, MemoryKind, MemoryLevel, MemoryScope, ProvenanceUri, TrustClass,
};
use crate::pack::PackProvenance;

/// Wire schema id for the resume report.
pub const RESUME_SCHEMA_V1: &str = "ee.resume.v1";
/// Write-time gap that starts a new inferred session.
pub const SESSION_GAP_SECONDS: i64 = 4 * 3600;
/// Items listed per session (summary counts stay exact).
pub const SESSION_ITEM_CAP: usize = 20;
/// Maximum number of session groups one response may materialize.
pub const RESUME_SESSION_CAP: usize = 64;
/// Open-loop tag vocabulary.
pub const OPEN_LOOP_TAGS: [&str; 6] = ["next", "queue", "blocking", "pending", "todo", "revisit"];
/// Wall-clock budget for the nearby-store scan. Discovery runs only on the
/// cold path (no surfaced episodic memories), so populated resumes never pay
/// it. The budget must absorb scheduler starvation on loaded machines before
/// child probes run; truncation with zero candidates is the one outcome this
/// recovery surface exists to prevent.
pub const RESUME_NEARBY_SCAN_BUDGET_MS: u64 = 750;
/// Cap on open-loop tagged items and staleness flags.
const OPEN_LOOP_CAP: usize = 32;
/// Base commands plus one discovery diagnostic and one ranked retarget.
const RESUME_NEXT_COMMAND_CAP: usize = 5;
/// Page size for resume storage reads and the single-pass decision projection.
/// This stays below ordinary SQLite bind limits and prevents per-row queries.
const RESUME_STORAGE_PAGE_SIZE: usize = 256;

/// Redaction-safe provenance carried by every surfaced memory.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeProvenance {
    pub uri: String,
    pub trust_class: String,
    pub verification_status: String,
}

/// Public-egress redaction posture for a surfaced memory.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeRedactionPosture {
    pub applied: bool,
    pub reasons: Vec<String>,
}

/// One surfaced memory.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeItem {
    pub memory_id: String,
    pub level: String,
    pub kind: String,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub selection_reason: &'static str,
    pub provenance: ResumeProvenance,
    pub redaction: ResumeRedactionPosture,
    /// Set when the staleness heuristic flagged this item.
    pub stale: Option<StaleFlag>,
}

/// Why an item is considered stale.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaleFlag {
    /// The newer memory on the same subject.
    pub superseded_by: String,
    pub superseded_by_created_at: String,
    /// Shared non-control subject tags that establish identity.
    pub shared_tags: Vec<String>,
}

/// One inferred session, newest first.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSession {
    /// `session-*` tag when the session was tag-delimited, else
    /// `inferred-<newest date>`.
    pub label: String,
    pub member_count: usize,
    pub newest_at: String,
    pub oldest_at: String,
    /// Newest first, capped at [`SESSION_ITEM_CAP`].
    pub items: Vec<ResumeItem>,
}

/// A decision carrying a revisit condition.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeDecision {
    pub memory_id: String,
    pub topic: String,
    pub chosen: String,
    pub revisit_by: Option<String>,
    pub revisit_status: String,
    pub created_at: String,
    pub provenance: ResumeProvenance,
    pub redaction: ResumeRedactionPosture,
}

/// Open loops: revisit-conditioned decisions + tagged queue items.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenLoops {
    pub revisit_decisions_total: usize,
    pub revisit_decisions_truncated: bool,
    pub revisit_decisions: Vec<ResumeDecision>,
    pub tagged_items_total: usize,
    pub tagged_items_truncated: bool,
    pub tagged_items: Vec<ResumeItem>,
}

/// The full `ee.resume.v1` report payload.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeReport {
    pub schema: &'static str,
    pub workspace_id: String,
    pub episodic_total: usize,
    pub sessions: Vec<ResumeSession>,
    pub open_loops: OpenLoops,
    /// Unique stale memory IDs across every rendered projection.
    pub stale_count: usize,
    /// Populated when the store has nothing episodic to resume from.
    pub nearby_stores: Option<NearbyStoreScanAssessment>,
    pub next_commands: Vec<String>,
}

fn parse_ts(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|ts| ts.with_timezone(&chrono::Utc))
}

fn public_resume_text(value: &str, field: &str, reasons: &mut Vec<String>) -> String {
    let report = crate::policy::redact_public_replay_text(value);
    if report.redacted {
        reasons.extend(
            report
                .redacted_reasons
                .into_iter()
                .map(|reason| format!("{field}:{reason}")),
        );
    }
    report.content
}

/// Batched, read-only admission boundary for resume projections.
///
/// Storage supplies the candidate rows and tags in bounded queries. This
/// boundary then applies the same workspace scope, typed identity,
/// provenance parsing, secret screening, and public-egress posture used by
/// normal context admission without introducing per-memory SQL.
struct ResumeAdmissionBoundary {
    scope: MemoryScopeContext,
    workspace_id: String,
}

impl ResumeAdmissionBoundary {
    fn for_workspace(workspace_path: &Path) -> Self {
        Self::for_bound_workspace(
            workspace_path,
            crate::core::workspace::stable_workspace_id(workspace_path),
        )
    }

    fn for_bound_workspace(workspace_path: &Path, workspace_id: String) -> Self {
        Self {
            scope: MemoryScopeContext::for_workspace(workspace_path, MemoryScope::Workspace, false),
            workspace_id,
        }
    }

    fn admit(&self, mut memory: StoredMemory, tags: &[String]) -> Option<StoredMemory> {
        let level = MemoryLevel::from_str(&memory.level).ok()?;
        let kind = MemoryKind::from_str(&memory.kind).ok()?;
        if MemoryId::from_str(&memory.id).is_err()
            || memory.workspace_id != self.workspace_id
            || memory.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
            || !self.scope.memory_in_scope_with_tags(&memory, tags)
            || !crate::policy::redact_secret_like_content(&memory.content)
                .redacted_reasons
                .is_empty()
        {
            return None;
        }
        memory.level = level.as_str().to_owned();
        memory.kind = kind.as_str().to_owned();
        Some(memory)
    }
}

fn resume_provenance_uri(memory: &StoredMemory, reasons: &mut Vec<String>) -> String {
    let memory_id = match MemoryId::from_str(&memory.id) {
        Ok(memory_id) => memory_id,
        Err(_) => {
            reasons.push("provenanceUri:invalid_memory_id".to_owned());
            return "[REDACTED:invalid_memory_id]".to_owned();
        }
    };
    let uri = match memory.provenance_uri.as_deref() {
        Some(raw) => {
            // Record the public-egress posture even though the canonical URI
            // parser and PackProvenance renderer own the value we emit.
            let public_raw = public_resume_text(raw, "provenanceUri", reasons);
            if public_raw == raw {
                match ProvenanceUri::from_str(raw) {
                    Ok(uri) => uri,
                    Err(_) => {
                        reasons.push("provenanceUri:invalid_fallback".to_owned());
                        ProvenanceUri::EeMemory(memory_id)
                    }
                }
            } else {
                ProvenanceUri::EeMemory(memory_id)
            }
        }
        None => ProvenanceUri::EeMemory(memory_id),
    };
    let rendered = match PackProvenance::new(uri, "Admitted by the bounded resume projection.") {
        Ok(provenance) => provenance.rendered().uri,
        Err(_) => {
            reasons.push("provenanceUri:render_failed".to_owned());
            return "[REDACTED:invalid_provenance]".to_owned();
        }
    };
    if memory
        .provenance_uri
        .as_deref()
        .is_some_and(|raw| raw != rendered)
    {
        reasons.push("provenanceUri:pack_output_redaction".to_owned());
    }
    rendered
}

fn resume_provenance(memory: &StoredMemory, reasons: &mut Vec<String>) -> ResumeProvenance {
    let trust_class = match TrustClass::from_str(&memory.trust_class) {
        Ok(trust_class) => trust_class,
        Err(_) => {
            reasons.push("provenance.trustClass:invalid_fallback".to_owned());
            TrustClass::AgentAssertion
        }
    }
    .as_str()
    .to_owned();
    let verification_status = public_resume_text(
        &memory.provenance_verification_status,
        "provenance.verificationStatus",
        reasons,
    );
    ResumeProvenance {
        uri: resume_provenance_uri(memory, reasons),
        trust_class,
        verification_status,
    }
}

fn item(
    memory: &StoredMemory,
    tags: &BTreeMap<String, Vec<String>>,
    selection_reason: &'static str,
) -> ResumeItem {
    let mut redaction_reasons = Vec::new();
    let content = public_resume_text(&memory.content, "content", &mut redaction_reasons);
    let safe_tags = tags
        .get(&memory.id)
        .into_iter()
        .flatten()
        .map(|tag| public_resume_text(tag, "tag", &mut redaction_reasons))
        .collect();
    let provenance = resume_provenance(memory, &mut redaction_reasons);
    redaction_reasons.sort();
    redaction_reasons.dedup();
    ResumeItem {
        memory_id: memory.id.clone(),
        level: memory.level.clone(),
        kind: memory.kind.clone(),
        content,
        tags: safe_tags,
        created_at: memory.created_at.clone(),
        selection_reason,
        provenance,
        redaction: ResumeRedactionPosture {
            applied: !redaction_reasons.is_empty(),
            reasons: redaction_reasons,
        },
        stale: None,
    }
}

/// Group episodic memories (pre-sorted newest first) into sessions.
///
/// A `session-*` tag is a stable session identity even when later imports
/// interleave or backfill its rows. Untagged rows cluster by write-time gap.
fn group_sessions(
    memories: &[&StoredMemory],
    tags: &BTreeMap<String, Vec<String>>,
    limit: usize,
) -> Vec<ResumeSession> {
    let session_tag = |memory: &StoredMemory| -> Option<String> {
        tags.get(&memory.id).and_then(|memory_tags| {
            memory_tags
                .iter()
                .filter(|tag| tag.starts_with("session-"))
                .min()
                .cloned()
        })
    };

    let mut sessions: Vec<(Option<String>, Vec<&StoredMemory>)> = Vec::new();
    let mut tagged_session_indexes = BTreeMap::<String, usize>::new();
    let mut last_untagged_index: Option<usize> = None;
    for memory in memories {
        if let Some(tag) = session_tag(memory) {
            if let Some(index) = tagged_session_indexes.get(&tag).copied() {
                sessions[index].1.push(memory);
            } else {
                let index = sessions.len();
                sessions.push((Some(tag.clone()), vec![memory]));
                tagged_session_indexes.insert(tag, index);
            }
            continue;
        }

        let append_to = last_untagged_index.filter(|index| {
            let previous = sessions[*index]
                .1
                .last()
                .and_then(|previous| parse_ts(&previous.created_at));
            let current = parse_ts(&memory.created_at);
            match (previous, current) {
                (Some(previous), Some(current)) => {
                    (previous - current).num_seconds().abs() <= SESSION_GAP_SECONDS
                }
                _ => true,
            }
        });
        if let Some(index) = append_to {
            sessions[index].1.push(memory);
        } else {
            let index = sessions.len();
            sessions.push((None, vec![memory]));
            last_untagged_index = Some(index);
        }
    }

    let mut grouped: Vec<ResumeSession> = sessions
        .into_iter()
        .map(|(tag, members)| {
            let newest_at = members
                .first()
                .map(|memory| memory.created_at.clone())
                .unwrap_or_default();
            let oldest_at = members
                .last()
                .map(|memory| memory.created_at.clone())
                .unwrap_or_default();
            let label = tag.map_or_else(
                || {
                    let date = newest_at.get(..10).unwrap_or("unknown");
                    format!("inferred-{date}")
                },
                |tag| {
                    let mut reasons = Vec::new();
                    public_resume_text(&tag, "session.label", &mut reasons)
                },
            );
            let items: Vec<ResumeItem> = members
                .iter()
                .take(SESSION_ITEM_CAP)
                .map(|memory| item(memory, tags, "recent_session_member"))
                .collect();
            ResumeSession {
                label,
                member_count: members.len(),
                newest_at,
                oldest_at,
                items,
            }
        })
        .collect();
    grouped.sort_by(|left, right| {
        right
            .newest_at
            .cmp(&left.newest_at)
            .then_with(|| left.label.cmp(&right.label))
    });
    grouped.truncate(limit.min(RESUME_SESSION_CAP));
    grouped
}

fn is_control_tag(tag: &str) -> bool {
    tag.starts_with("session-") || OPEN_LOOP_TAGS.contains(&tag)
}

/// Flag surfaced items superseded by a newer live memory on the same
/// subject (same kind, at least one shared non-control subject tag, strictly
/// newer `created_at`). Returns the unique IDs flagged in this projection.
fn apply_staleness(
    items: &mut [ResumeItem],
    all_live: &[StoredMemory],
    tags: &BTreeMap<String, Vec<String>>,
) -> BTreeSet<String> {
    let mut flagged_ids = BTreeSet::new();
    for surfaced in items.iter_mut() {
        let surfaced_tags = tags
            .get(&surfaced.memory_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if surfaced_tags.is_empty() {
            continue;
        }
        let Some(surfaced_created_at) = parse_ts(&surfaced.created_at) else {
            continue;
        };
        let mut best: Option<(DateTime<Utc>, StaleFlag)> = None;
        for candidate in all_live {
            if candidate.id == surfaced.memory_id || candidate.kind != surfaced.kind {
                continue;
            }
            let Some(candidate_created_at) = parse_ts(&candidate.created_at) else {
                continue;
            };
            if candidate_created_at <= surfaced_created_at {
                continue;
            }
            let candidate_tags = tags.get(&candidate.id);
            let Some(candidate_tags) = candidate_tags else {
                continue;
            };
            let shared: Vec<String> = surfaced_tags
                .iter()
                .filter(|tag| !is_control_tag(tag) && candidate_tags.contains(tag))
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if shared.is_empty() {
                continue;
            }
            let replace = match &best {
                None => true,
                Some((existing_created_at, existing)) => {
                    candidate_created_at > *existing_created_at
                        || (candidate_created_at == *existing_created_at
                            && candidate.id < existing.superseded_by)
                }
            };
            if replace {
                best = Some((
                    candidate_created_at,
                    StaleFlag {
                        superseded_by: candidate.id.clone(),
                        superseded_by_created_at: candidate.created_at.clone(),
                        shared_tags: shared,
                    },
                ));
            }
        }
        if let Some((_, mut flag)) = best {
            flag.shared_tags = flag
                .shared_tags
                .iter()
                .map(|tag| {
                    public_resume_text(tag, "stale.sharedTag", &mut surfaced.redaction.reasons)
                })
                .collect();
            surfaced.redaction.reasons.sort();
            surfaced.redaction.reasons.dedup();
            surfaced.redaction.applied = !surfaced.redaction.reasons.is_empty();
            surfaced.stale = Some(flag);
            flagged_ids.insert(surfaced.memory_id.clone());
        }
    }
    flagged_ids
}

/// Apply staleness to every report projection while counting each memory ID
/// once even when it appears in both open loops and a recent session.
fn apply_report_staleness(
    tagged_items: &mut [ResumeItem],
    sessions: &mut [ResumeSession],
    all_live: &[StoredMemory],
    tags: &BTreeMap<String, Vec<String>>,
) -> usize {
    let mut stale_memory_ids = apply_staleness(tagged_items, all_live, tags);
    for session in sessions {
        stale_memory_ids.extend(apply_staleness(&mut session.items, all_live, tags));
    }
    stale_memory_ids.len()
}

fn decision_line(content: &str, prefix: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix(prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn revisit_status(revisit_by: &str, now: &DateTime<Utc>) -> String {
    let Some(revisit_by) = parse_ts(revisit_by) else {
        return "unknown".to_owned();
    };
    if &revisit_by < now {
        "overdue".to_owned()
    } else if &revisit_by == now {
        "due".to_owned()
    } else {
        "future".to_owned()
    }
}

fn resume_storage_error(message: impl Into<String>) -> DomainError {
    DomainError::Storage {
        message: message.into(),
        repair: Some(
            "Run `ee doctor --workspace . --json`, repair the reported storage failure, then retry `ee resume`."
                .to_owned(),
        ),
    }
}

fn load_decision_typed_fields(
    connection: &DbConnection,
    memories: &[StoredMemory],
) -> Result<BTreeMap<String, String>, DomainError> {
    let decision_ids: Vec<&str> = memories
        .iter()
        .filter(|memory| memory.kind == "decision")
        .map(|memory| memory.id.as_str())
        .collect();
    let mut typed_fields = BTreeMap::new();
    for page in decision_ids.chunks(RESUME_STORAGE_PAGE_SIZE) {
        let placeholders: Vec<String> = (1..=page.len()).map(|index| format!("?{index}")).collect();
        let sql = format!(
            "SELECT id, typed_fields_json FROM memories WHERE id IN ({}) AND typed_fields_json IS NOT NULL ORDER BY id ASC",
            placeholders.join(", ")
        );
        let params: Vec<SqlValue> = page
            .iter()
            .map(|memory_id| SqlValue::Text((*memory_id).to_owned()))
            .collect();
        let rows = connection.query(&sql, &params).map_err(|error| {
            resume_storage_error(format!(
                "Failed to batch-load canonical decision typed fields for resume: {error}"
            ))
        })?;
        for row in rows {
            let memory_id = match row.get(0) {
                Some(SqlValue::Text(value)) => value.clone(),
                value => {
                    return Err(resume_storage_error(format!(
                        "Canonical decision typed-field row has invalid memory id value {value:?}"
                    )));
                }
            };
            let raw = match row.get(1) {
                Some(SqlValue::Text(value)) => value.clone(),
                value => {
                    return Err(resume_storage_error(format!(
                        "Canonical decision typed-field row for {memory_id} has invalid sidecar value {value:?}"
                    )));
                }
            };
            typed_fields.insert(memory_id, raw);
        }
    }
    Ok(typed_fields)
}

fn typed_decision_string_field(
    fields: &BTreeMap<String, serde_json::Value>,
    name: &str,
) -> Option<String> {
    fields
        .get(name)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Project every current decision carrying a revisit condition in one bounded,
/// chunked pass over the already-loaded newest-first memory rows. This avoids
/// the decide list's presentation limit, performs no per-decision storage
/// queries, and only redacts/materializes the public output page.
fn collect_revisit_decisions(
    memories: &[StoredMemory],
    typed_fields_by_memory: &BTreeMap<String, String>,
    now: DateTime<Utc>,
) -> Result<(Vec<ResumeDecision>, usize, bool), DomainError> {
    let mut decisions = Vec::new();
    let mut total = 0usize;
    for page in memories.chunks(RESUME_STORAGE_PAGE_SIZE) {
        for memory in page {
            if memory.kind != "decision" {
                continue;
            }
            let typed_fields = typed_fields_by_memory
                .get(&memory.id)
                .map(|raw| typed_memory_fields_from_json(&MemoryKind::Decision, raw))
                .transpose()
                .map_err(|error| {
                    resume_storage_error(format!(
                        "Invalid canonical decision typed fields for {}: {error}",
                        memory.id
                    ))
                })?;
            let topic = typed_fields.as_ref().map_or_else(
                || decision_line(&memory.content, "Topic:").unwrap_or_default(),
                |_| {
                    decision_line(&memory.content, "Topic:")
                        .unwrap_or_else(|| memory.content.clone())
                },
            );
            let chosen = typed_fields.as_ref().map_or_else(
                || decision_line(&memory.content, "Chosen:").unwrap_or_default(),
                |fields| typed_decision_string_field(fields, "chosen").unwrap_or_default(),
            );
            let revisit_by = typed_fields.as_ref().map_or_else(
                || decision_line(&memory.content, "Revisit by:"),
                |fields| typed_decision_string_field(fields, "revisit_by"),
            );
            let Some(revisit_by) = revisit_by else {
                continue;
            };
            total = total.saturating_add(1);
            if decisions.len() >= OPEN_LOOP_CAP {
                continue;
            }
            let mut redaction_reasons = Vec::new();
            let topic = public_resume_text(&topic, "decision.topic", &mut redaction_reasons);
            let chosen = public_resume_text(&chosen, "decision.chosen", &mut redaction_reasons);
            let safe_revisit_by =
                public_resume_text(&revisit_by, "decision.revisitBy", &mut redaction_reasons);
            let provenance = resume_provenance(memory, &mut redaction_reasons);
            redaction_reasons.sort();
            redaction_reasons.dedup();
            decisions.push(ResumeDecision {
                memory_id: memory.id.clone(),
                topic,
                chosen,
                revisit_status: revisit_status(&revisit_by, &now),
                revisit_by: Some(safe_revisit_by),
                created_at: memory.created_at.clone(),
                provenance,
                redaction: ResumeRedactionPosture {
                    applied: !redaction_reasons.is_empty(),
                    reasons: redaction_reasons,
                },
            });
        }
    }
    Ok((decisions, total, total > OPEN_LOOP_CAP))
}

/// Options for [`build_resume_report`].
#[derive(Clone, Debug)]
pub struct ResumeOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: &'a Path,
    /// How many recent sessions to include.
    pub sessions: usize,
}

/// Assemble the resume bundle. Read-only.
pub fn build_resume_report(options: &ResumeOptions<'_>) -> Result<ResumeReport, DomainError> {
    if options.sessions == 0 {
        return Err(DomainError::Usage {
            message: "`--sessions` must be at least 1; received 0.".to_owned(),
            repair: Some("ee resume --sessions 1 --workspace . --json".to_owned()),
        });
    }
    if options.sessions > RESUME_SESSION_CAP {
        return Err(DomainError::Usage {
            message: format!(
                "`--sessions` cannot exceed {RESUME_SESSION_CAP}; received {}.",
                options.sessions
            ),
            repair: Some(format!(
                "ee resume --sessions {RESUME_SESSION_CAP} --workspace . --json"
            )),
        });
    }
    match std::fs::symlink_metadata(options.database_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(empty_resume_report(options));
        }
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!(
                    "Failed to inspect addressed workspace database {}: {error}",
                    options.database_path.display()
                ),
                repair: Some("ee doctor --workspace . --json".to_owned()),
            });
        }
        Ok(_) => {}
    }
    if addressed_store_state(options.workspace_path, options.database_path)
        == AddressedStoreState::Unavailable
    {
        return Err(DomainError::Storage {
            message: format!(
                "Addressed workspace database {} exists but is unsafe, unreadable, or incompatible.",
                options.database_path.display()
            ),
            repair: Some(
                "Run `ee doctor --workspace . --json`, repair the addressed store, then retry `ee resume`."
                    .to_owned(),
            ),
        });
    }

    let connection = DbConnection::open_file_read_only(options.database_path).map_err(|error| {
        DomainError::Storage {
            message: format!("Failed to open workspace database: {error}"),
            repair: Some(crate::core::storeless_workspace_repair(
                options.database_path,
            )),
        }
    })?;
    let canonical_workspace = options
        .workspace_path
        .canonicalize()
        .unwrap_or_else(|_| options.workspace_path.to_path_buf());
    let workspace_id = crate::core::workspace::bound_workspace_id_or_hash(
        &connection,
        &crate::core::workspace::stable_workspace_id(&canonical_workspace),
        &[options.workspace_path, canonical_workspace.as_path()],
    )?;
    let now = Utc::now();
    let current_memories = connection
        .list_recent_current_memories_for_retrieval(&workspace_id, &now.to_rfc3339(), u32::MAX)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list current resume memories: {error}"),
            repair: Some("ee doctor --workspace . --json".to_owned()),
        })?;
    let ids: Vec<&str> = current_memories
        .iter()
        .map(|memory| memory.id.as_str())
        .collect();
    let mut tags = BTreeMap::new();
    for page in ids.chunks(RESUME_STORAGE_PAGE_SIZE) {
        let page_tags = connection
            .get_memory_tags_batch(page)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to load memory tags required for resume session grouping, open-loop detection, and staleness: {error}"
                ),
                repair: Some(
                    "Run `ee doctor --workspace . --json`, repair the reported storage failure, then retry `ee resume`."
                        .to_owned(),
                ),
            })?;
        tags.extend(page_tags);
    }

    // Apply the ordinary workspace-scope and public-content admission rules
    // only after the one batched tag read. The exact sealed placeholder and
    // secret-bearing bodies fail closed; tags and provenance remain eligible
    // for field-level public redaction during projection. This deliberately
    // performs no per-memory storage lookup.
    let admission =
        ResumeAdmissionBoundary::for_bound_workspace(&canonical_workspace, workspace_id.clone());
    let all_live: Vec<StoredMemory> = current_memories
        .into_iter()
        .filter_map(|memory| {
            let memory_tags = tags.get(&memory.id).map(Vec::as_slice).unwrap_or_default();
            admission.admit(memory, memory_tags)
        })
        .collect();

    // Recent end-state: episodic memories, newest first (created_at desc, id
    // desc as the deterministic tie-break).
    let mut episodic: Vec<&StoredMemory> = all_live
        .iter()
        .filter(|memory| memory.level == "episodic")
        .collect();
    episodic.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    let episodic_total = episodic.len();

    let mut sessions = group_sessions(&episodic, &tags, options.sessions);

    // Open loops: all current revisit-conditioned decisions, then a bounded
    // public page with exact total/truncation posture.
    let typed_decision_fields = load_decision_typed_fields(&connection, &all_live)?;
    let (revisit_decisions, revisit_decisions_total, revisit_decisions_truncated) =
        collect_revisit_decisions(&all_live, &typed_decision_fields, now)?;

    let mut tagged_memories: Vec<&StoredMemory> = all_live
        .iter()
        .filter(|memory| {
            tags.get(&memory.id).is_some_and(|memory_tags| {
                memory_tags
                    .iter()
                    .any(|tag| OPEN_LOOP_TAGS.contains(&tag.as_str()))
            })
        })
        .collect();
    tagged_memories.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    let tagged_items_total = tagged_memories.len();
    let tagged_items_truncated = tagged_items_total > OPEN_LOOP_CAP;
    let mut tagged_items: Vec<ResumeItem> = tagged_memories
        .into_iter()
        .take(OPEN_LOOP_CAP)
        .map(|memory| item(memory, &tags, "open_loop_tag"))
        .collect();

    // Staleness pass over everything surfaced. Count unique memories rather
    // than rendered projections because an episodic open loop appears twice.
    let stale_count = apply_report_staleness(&mut tagged_items, &mut sessions, &all_live, &tags);

    let nearby_stores = if episodic_total == 0 {
        let mut scan = discover_nearby_stores_for_database(
            options.workspace_path,
            options.database_path,
            std::time::Duration::from_millis(RESUME_NEARBY_SCAN_BUDGET_MS),
        );
        scan.stores.truncate(NEARBY_STORE_REPORT_LIMIT);
        Some(scan)
    } else {
        None
    };

    let next_commands = resume_next_commands(nearby_stores.as_ref());

    Ok(ResumeReport {
        schema: RESUME_SCHEMA_V1,
        workspace_id,
        episodic_total,
        sessions,
        open_loops: OpenLoops {
            revisit_decisions_total,
            revisit_decisions_truncated,
            revisit_decisions,
            tagged_items_total,
            tagged_items_truncated,
            tagged_items,
        },
        stale_count,
        nearby_stores,
        next_commands,
    })
}

fn empty_resume_report(options: &ResumeOptions<'_>) -> ResumeReport {
    let canonical_workspace = options
        .workspace_path
        .canonicalize()
        .unwrap_or_else(|_| options.workspace_path.to_path_buf());
    let mut scan = discover_nearby_stores_for_database(
        options.workspace_path,
        options.database_path,
        std::time::Duration::from_millis(RESUME_NEARBY_SCAN_BUDGET_MS),
    );
    scan.stores.truncate(NEARBY_STORE_REPORT_LIMIT);
    let nearby_stores = Some(scan);
    let next_commands = resume_next_commands(nearby_stores.as_ref());

    ResumeReport {
        schema: RESUME_SCHEMA_V1,
        workspace_id: crate::core::workspace::stable_workspace_id(&canonical_workspace),
        episodic_total: 0,
        sessions: Vec::new(),
        open_loops: OpenLoops::default(),
        stale_count: 0,
        nearby_stores,
        next_commands,
    }
}

fn resume_next_commands(nearby_stores: Option<&NearbyStoreScanAssessment>) -> Vec<String> {
    let mut commands = vec![
        "ee decide list --json  # open decisions incl. revisit conditions".to_owned(),
        "ee orient \"<current task>\" --json  # task-conditioned pack once you know the task"
            .to_owned(),
        "ee conflict list --json  # anything contradictory left behind".to_owned(),
    ];
    if let Some(scan) = nearby_stores {
        match scan.outcome {
            NearbyStoreScanOutcome::Complete => {}
            NearbyStoreScanOutcome::Truncated => commands.insert(
                0,
                "ee workspace list --json  # nearby-store discovery truncated; inspect registered stores"
                    .to_owned(),
            ),
            NearbyStoreScanOutcome::TruncatedRegistryUnavailable => commands.insert(
                0,
                "ee doctor --workspace . --json  # optional workspace registry unavailable; local nearby stores remain actionable"
                    .to_owned(),
            ),
            NearbyStoreScanOutcome::Unavailable => commands.insert(
                0,
                "ee doctor --workspace . --json  # diagnose unavailable nearby-store discovery"
                    .to_owned(),
            ),
        }
    }
    if let Some(best) = nearby_stores
        .filter(|scan| scan.outcome != NearbyStoreScanOutcome::Unavailable)
        .and_then(|scan| scan.stores.first())
    {
        let workspace = shell_quote_cli_arg(&best.workspace_root);
        let database =
            shell_quote_cli_arg(&Path::new(&best.store_dir).join("ee.db").to_string_lossy());
        commands.insert(
            0,
            format!("ee resume --workspace {workspace} --database {database} --json"),
        );
    }
    commands.truncate(RESUME_NEXT_COMMAND_CAP);
    commands
}

fn shell_quote_cli_arg(value: &str) -> String {
    if value.is_empty() {
        "''".to_owned()
    } else if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{
        OPEN_LOOP_CAP, OPEN_LOOP_TAGS, RESUME_SESSION_CAP, ResumeAdmissionBoundary, ResumeItem,
        ResumeOptions, ResumeProvenance, ResumeRedactionPosture, ResumeSession, StaleFlag,
        apply_report_staleness, apply_staleness, build_resume_report, collect_revisit_decisions,
        group_sessions, item, parse_ts, resume_next_commands,
    };
    use crate::core::orient::{NearbyStore, NearbyStoreScanAssessment, NearbyStoreScanOutcome};
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection, StoredMemory};
    use crate::models::{DomainError, MemoryId};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    fn memory(id: &str, level: &str, kind: &str, created_at: &str) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
            workspace_id: "wsp_00000000000000000000000001".to_owned(),
            level: level.to_owned(),
            kind: kind.to_owned(),
            content: format!("content {id}"),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_assertion".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "1".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: created_at.to_owned(),
            updated_at: created_at.to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    fn resume_storage_fixture(
        kind: &str,
        tags: &[&str],
    ) -> Result<(tempfile::TempDir, PathBuf, PathBuf), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("workspace");
        let store = workspace.join(".ee");
        std::fs::create_dir_all(&store).map_err(|error| error.to_string())?;
        let database = store.join("ee.db");
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let canonical = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let workspace_id = crate::core::workspace::stable_workspace_id(&canonical);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: canonical.display().to_string(),
                    name: Some("resume-storage-fixture".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d45)).to_string();
        let content = if kind == "decision" {
            "Topic: Resume failure propagation\nChosen: preserve truth\nOptions: preserve truth | hide failure\nRationale: False success is unsafe."
        } else {
            "Resume storage failure fixture."
        };
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id,
                    level: "episodic".to_owned(),
                    kind: kind.to_owned(),
                    content: content.to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.9,
                    importance: 0.9,
                    provenance_uri: Some("test://resume-storage-failure".to_owned()),
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        drop(connection);
        Ok((temp, workspace, database))
    }

    #[test]
    fn sessions_group_by_tag_then_by_time_gap() {
        let tagged_a = memory("mem_a1", "episodic", "note", "2026-08-09T20:00:00Z");
        let tagged_a2 = memory("mem_a2", "episodic", "note", "2026-08-09T19:00:00Z");
        let untagged_recent = memory("mem_u1", "episodic", "note", "2026-08-08T10:00:00Z");
        let untagged_old = memory("mem_u2", "episodic", "note", "2026-08-08T01:00:00Z");
        let mut tags = BTreeMap::new();
        tags.insert("mem_a1".to_owned(), vec!["session-20260809".to_owned()]);
        tags.insert("mem_a2".to_owned(), vec!["session-20260809".to_owned()]);

        let ordered = vec![&tagged_a, &tagged_a2, &untagged_recent, &untagged_old];
        let sessions = group_sessions(&ordered, &tags, 5);
        assert_eq!(
            sessions.len(),
            3,
            "tag run + two gap-separated: {sessions:?}"
        );
        assert_eq!(sessions[0].label, "session-20260809");
        assert_eq!(sessions[0].member_count, 2);
        assert!(sessions[1].label.starts_with("inferred-"));
        assert_eq!(sessions[1].member_count, 1);
        // The 9h gap exceeds SESSION_GAP_SECONDS so u2 starts session 3.
        assert_eq!(sessions[2].member_count, 1);
    }

    #[test]
    fn interleaved_tagged_rows_group_by_stable_session_identity() {
        let tagged_a_new = memory("mem_a_new", "episodic", "note", "2026-08-09T20:00:00Z");
        let tagged_b = memory("mem_b", "episodic", "note", "2026-08-09T19:30:00Z");
        let tagged_a_backfill =
            memory("mem_a_backfill", "episodic", "note", "2026-08-09T19:00:00Z");
        let mut tags = BTreeMap::new();
        tags.insert("mem_a_new".to_owned(), vec!["session-stable-a".to_owned()]);
        tags.insert("mem_b".to_owned(), vec!["session-stable-b".to_owned()]);
        tags.insert(
            "mem_a_backfill".to_owned(),
            vec!["session-stable-a".to_owned()],
        );

        let ordered = vec![&tagged_a_new, &tagged_b, &tagged_a_backfill];
        let sessions = group_sessions(&ordered, &tags, 3);
        assert_eq!(sessions.len(), 2, "stable identities: {sessions:?}");
        assert_eq!(sessions[0].label, "session-stable-a");
        assert_eq!(sessions[0].member_count, 2);
        assert_eq!(sessions[1].label, "session-stable-b");
        assert_eq!(sessions[1].member_count, 1);
    }

    #[test]
    fn session_limit_bounds_output() {
        let one = memory("mem_1", "episodic", "note", "2026-08-09T20:00:00Z");
        let two = memory("mem_2", "episodic", "note", "2026-08-08T02:00:00Z");
        let three = memory("mem_3", "episodic", "note", "2026-08-06T02:00:00Z");
        let ordered = vec![&one, &two, &three];
        let sessions = group_sessions(&ordered, &BTreeMap::new(), 2);
        assert_eq!(sessions.len(), 2, "limit honored: {sessions:?}");
    }

    #[test]
    fn admission_boundary_rejects_cross_workspace_rows() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let boundary = ResumeAdmissionBoundary::for_workspace(&workspace);
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d47)).to_string();
        let mut stored = memory(&memory_id, "episodic", "note", "2026-08-09T20:00:00Z");
        stored.level = "EPISODIC".to_owned();
        stored.kind = "playbook_step".to_owned();

        assert!(boundary.admit(stored.clone(), &[]).is_none());
        stored.workspace_id = crate::core::workspace::stable_workspace_id(&workspace);
        let admitted = boundary
            .admit(stored, &[])
            .ok_or("same-workspace memory should be admitted")?;
        assert_eq!(admitted.level, "episodic");
        assert_eq!(admitted.kind, "playbook-step");
        Ok(())
    }

    #[test]
    fn resume_item_applies_public_redaction_and_safe_provenance() {
        let secret = format!("sk_live_{}", "1234567890abcdef1234567890abcdef");
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d46)).to_string();
        let mut stored = memory(&memory_id, "episodic", "note", "2026-08-09T20:00:00Z");
        stored.content = format!("Use token={secret} only for the fixture.");
        stored.provenance_uri = Some("/Users/alice/private/session.jsonl".to_owned());
        stored.trust_class = "agent_assertion".to_owned();
        stored.provenance_verification_status = "unverified".to_owned();
        let mut tags = BTreeMap::new();
        tags.insert(
            stored.id.clone(),
            vec![format!("credential-{secret}"), "session-safe".to_owned()],
        );

        let projected = item(&stored, &tags, "recent_session_member");
        assert!(projected.redaction.applied);
        assert!(!projected.content.contains(&secret));
        assert!(projected.tags.iter().all(|tag| !tag.contains(&secret)));
        assert_eq!(projected.provenance.trust_class, "agent_assertion");
        assert_eq!(projected.provenance.verification_status, "unverified");
        assert!(!projected.provenance.uri.contains("/Users/alice"));
        assert!(
            projected
                .redaction
                .reasons
                .iter()
                .any(|reason| reason.starts_with("content:"))
        );
        assert!(
            projected
                .redaction
                .reasons
                .iter()
                .any(|reason| reason.starts_with("provenanceUri:"))
        );
    }

    #[test]
    fn invalid_trust_class_falls_back_with_explicit_redaction_posture() {
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d48)).to_string();
        let mut stored = memory(&memory_id, "episodic", "note", "2026-08-09T20:00:00Z");
        stored.trust_class = "untrusted-free-form".to_owned();

        let projected = item(&stored, &BTreeMap::new(), "recent_session_member");

        assert_eq!(projected.provenance.trust_class, "agent_assertion");
        assert!(projected.redaction.applied);
        assert_eq!(
            projected.redaction.reasons,
            vec!["provenance.trustClass:invalid_fallback".to_owned()]
        );
    }

    #[test]
    fn revisit_decision_scan_has_no_200_row_prefilter_and_reports_exact_bound() {
        let mut memories = Vec::new();
        for index in 0..225 {
            let mut decision = memory(
                &format!("mem_decision_{index:03}"),
                "semantic",
                "decision",
                "2026-08-09T20:00:00Z",
            );
            decision.content = format!(
                "Topic: decision {index}\nChosen: keep {index}\nRevisit by: 2026-12-31T00:00:00Z"
            );
            memories.push(decision);
        }
        let now = parse_ts("2026-08-10T00:00:00Z").unwrap();
        let (decisions, total, truncated) =
            collect_revisit_decisions(&memories, &BTreeMap::new(), now).unwrap();
        assert_eq!(total, 225);
        assert!(truncated);
        assert_eq!(decisions.len(), OPEN_LOOP_CAP);
    }

    #[test]
    fn revisit_decision_scan_prefers_canonical_typed_sidecar() {
        let mut decision = memory(
            "mem_typed_decision",
            "semantic",
            "decision",
            "2026-08-09T20:00:00Z",
        );
        decision.content = "Typed sidecar decision without literal field prose.".to_owned();
        let typed = BTreeMap::from([(
            decision.id.clone(),
            serde_json::json!({
                "schema": "ee.memory.typed_fields.v2",
                "kind": "decision",
                "fields": {
                    "chosen": "ship the canonical sidecar",
                    "revisit_by": "2026-12-31T00:00:00Z"
                }
            })
            .to_string(),
        )]);
        let now = parse_ts("2026-08-10T00:00:00Z").unwrap();

        let (decisions, total, truncated) =
            collect_revisit_decisions(&[decision], &typed, now).unwrap();

        assert_eq!(total, 1);
        assert!(!truncated);
        assert_eq!(
            decisions[0].topic,
            "Typed sidecar decision without literal field prose."
        );
        assert_eq!(decisions[0].chosen, "ship the canonical sidecar");
        assert_eq!(
            decisions[0].revisit_by.as_deref(),
            Some("2026-12-31T00:00:00Z")
        );
        assert_eq!(decisions[0].provenance.trust_class, "agent_assertion",);
        assert_eq!(decisions[0].provenance.verification_status, "unverified",);
        assert!(!decisions[0].redaction.applied);
        assert!(decisions[0].redaction.reasons.is_empty());
    }

    #[test]
    fn staleness_flags_next_plus_arc4_and_excludes_all_control_tags() {
        let mut items = vec![ResumeItem {
            memory_id: "mem_old".to_owned(),
            level: "episodic".to_owned(),
            kind: "note".to_owned(),
            content: "Next: run arc 4".to_owned(),
            tags: vec!["next".to_owned(), "arc4".to_owned()],
            created_at: "2026-08-01T00:00:00Z".to_owned(),
            selection_reason: "open_loop_tag",
            provenance: ResumeProvenance {
                uri: "ee-mem://fixture".to_owned(),
                trust_class: "agent_assertion".to_owned(),
                verification_status: "unverified".to_owned(),
            },
            redaction: ResumeRedactionPosture {
                applied: false,
                reasons: Vec::new(),
            },
            stale: None,
        }];
        let newer = memory("mem_new", "episodic", "note", "2026-08-09T00:00:00Z");
        let unrelated = memory("mem_x", "episodic", "note", "2026-08-09T00:00:00Z");
        let mut tags = BTreeMap::new();
        tags.insert(
            "mem_old".to_owned(),
            vec![
                "session-arc4".to_owned(),
                "next".to_owned(),
                "queue".to_owned(),
                "blocking".to_owned(),
                "pending".to_owned(),
                "todo".to_owned(),
                "revisit".to_owned(),
                "arc4".to_owned(),
            ],
        );
        tags.insert(
            "mem_new".to_owned(),
            vec![
                "session-arc4".to_owned(),
                "next".to_owned(),
                "queue".to_owned(),
                "blocking".to_owned(),
                "pending".to_owned(),
                "todo".to_owned(),
                "revisit".to_owned(),
                "arc4".to_owned(),
            ],
        );
        tags.insert("mem_x".to_owned(), vec!["other".to_owned()]);
        let all = vec![newer, unrelated];
        let flagged = apply_staleness(&mut items, &all, &tags);
        assert_eq!(flagged, BTreeSet::from(["mem_old".to_owned()]));
        let flag: &StaleFlag = items[0].stale.as_ref().unwrap();
        assert_eq!(flag.superseded_by, "mem_new");
        assert_eq!(flag.shared_tags, vec!["arc4".to_owned()]);
    }

    #[test]
    fn staleness_requires_same_kind_strictly_newer_and_a_shared_subject_tag() {
        let mut items = vec![ResumeItem {
            memory_id: "mem_old".to_owned(),
            level: "episodic".to_owned(),
            kind: "note".to_owned(),
            content: "note".to_owned(),
            tags: vec!["next".to_owned(), "arc4".to_owned()],
            created_at: "2026-08-05T00:00:00Z".to_owned(),
            selection_reason: "open_loop_tag",
            provenance: ResumeProvenance {
                uri: "ee-mem://fixture".to_owned(),
                trust_class: "agent_assertion".to_owned(),
                verification_status: "unverified".to_owned(),
            },
            redaction: ResumeRedactionPosture {
                applied: false,
                reasons: Vec::new(),
            },
            stale: None,
        }];
        let next_only = memory("mem_next_only", "episodic", "note", "2026-08-09T00:00:00Z");
        let different_kind = memory(
            "mem_different_kind",
            "episodic",
            "fact",
            "2026-08-09T00:00:00Z",
        );
        let equal_time = memory("mem_equal", "episodic", "note", "2026-08-05T00:00:00Z");
        let older = memory("mem_older", "episodic", "note", "2026-08-01T00:00:00Z");
        let mut tags = BTreeMap::new();
        tags.insert(
            "mem_old".to_owned(),
            vec!["next".to_owned(), "arc4".to_owned()],
        );
        tags.insert("mem_next_only".to_owned(), vec!["next".to_owned()]);
        tags.insert("mem_different_kind".to_owned(), vec!["arc4".to_owned()]);
        tags.insert("mem_equal".to_owned(), vec!["arc4".to_owned()]);
        tags.insert(
            "mem_older".to_owned(),
            vec!["next".to_owned(), "arc4".to_owned()],
        );
        let all = vec![next_only, different_kind, equal_time, older];
        assert!(apply_staleness(&mut items, &all, &tags).is_empty());
        assert!(items[0].stale.is_none());
        assert_eq!(
            OPEN_LOOP_TAGS,
            ["next", "queue", "blocking", "pending", "todo", "revisit"]
        );
    }

    #[test]
    fn report_stale_count_deduplicates_open_loop_and_session_projection() {
        let old = memory("mem_old", "episodic", "note", "2026-08-01T00:00:00Z");
        let newer = memory("mem_new", "episodic", "note", "2026-08-09T00:00:00Z");
        let mut tags = BTreeMap::new();
        tags.insert(
            old.id.clone(),
            vec![
                "session-arc4".to_owned(),
                "next".to_owned(),
                "arc4".to_owned(),
            ],
        );
        tags.insert(
            newer.id.clone(),
            vec![
                "session-arc4".to_owned(),
                "next".to_owned(),
                "arc4".to_owned(),
            ],
        );

        let projected = item(&old, &tags, "open_loop_tag");
        let mut tagged_items = vec![projected.clone()];
        let mut sessions = vec![ResumeSession {
            label: "session-arc4".to_owned(),
            member_count: 1,
            newest_at: old.created_at.clone(),
            oldest_at: old.created_at.clone(),
            items: vec![ResumeItem {
                selection_reason: "recent_session_member",
                ..projected
            }],
        }];

        assert_eq!(
            apply_report_staleness(&mut tagged_items, &mut sessions, &[newer], &tags),
            1
        );
        assert_eq!(
            tagged_items[0].stale.as_ref().unwrap().shared_tags,
            vec!["arc4".to_owned()]
        );
        assert_eq!(
            sessions[0].items[0].stale.as_ref().unwrap().shared_tags,
            vec!["arc4".to_owned()]
        );
    }

    #[test]
    fn tag_storage_failure_is_not_reported_as_an_empty_resume() -> Result<(), String> {
        let (_temp, workspace, database) = resume_storage_fixture("note", &["next", "queue"])?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection
            .execute_raw("DROP TABLE memory_tags")
            .map_err(|error| error.to_string())?;
        drop(connection);

        match build_resume_report(&ResumeOptions {
            workspace_path: &workspace,
            database_path: &database,
            sessions: 3,
        }) {
            Err(DomainError::Storage { message, repair }) => {
                assert!(message.contains("Failed to load memory tags required for resume"));
                assert!(
                    repair
                        .as_deref()
                        .is_some_and(|value| value.contains("ee doctor --workspace . --json"))
                );
                Ok(())
            }
            other => Err(format!("expected resume tag storage error, got {other:?}")),
        }
    }

    #[test]
    fn zero_session_limit_is_a_usage_error_before_store_access() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("zero-session-workspace");
        let database = workspace.join(".ee").join("ee.db");

        match build_resume_report(&ResumeOptions {
            workspace_path: &workspace,
            database_path: &database,
            sessions: 0,
        }) {
            Err(DomainError::Usage { message, repair }) => {
                assert!(message.contains("must be at least 1"));
                assert!(
                    repair
                        .as_deref()
                        .is_some_and(|value| value.contains("--sessions 1"))
                );
                assert!(!workspace.exists());
                Ok(())
            }
            other => Err(format!("expected zero sessions usage error, got {other:?}")),
        }
    }

    #[test]
    fn session_limit_above_public_cap_is_a_usage_error_before_store_access() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("oversized-session-workspace");
        let database = workspace.join(".ee").join("ee.db");

        match build_resume_report(&ResumeOptions {
            workspace_path: &workspace,
            database_path: &database,
            sessions: RESUME_SESSION_CAP + 1,
        }) {
            Err(DomainError::Usage { message, repair }) => {
                assert!(message.contains("cannot exceed 64"));
                assert!(
                    repair
                        .as_deref()
                        .is_some_and(|value| value.contains("--sessions 64"))
                );
                assert!(!workspace.exists());
                Ok(())
            }
            other => Err(format!(
                "expected oversized sessions usage error, got {other:?}"
            )),
        }
    }

    #[test]
    fn revisit_decisions_do_not_depend_on_per_row_link_queries() -> Result<(), String> {
        let (_temp, workspace, database) = resume_storage_fixture("decision", &[])?;
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection
            .execute_raw("DROP TABLE memory_links")
            .map_err(|error| error.to_string())?;
        drop(connection);

        let report = build_resume_report(&ResumeOptions {
            workspace_path: &workspace,
            database_path: &database,
            sessions: 3,
        })
        .map_err(|error| error.to_string())?;
        assert_eq!(report.open_loops.revisit_decisions_total, 0);
        assert!(!report.open_loops.revisit_decisions_truncated);
        Ok(())
    }

    #[test]
    fn external_custom_database_uses_explicit_resume_workspace_identity() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("recorded resume workspace");
        let external_store = temp.path().join("external stores");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&external_store).map_err(|error| error.to_string())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let database = external_store.join("custom-resume.db");
        let workspace_id = crate::core::workspace::stable_workspace_id(&workspace);
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("external resume store".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d99)).to_string();
        connection
            .insert_memory(
                &memory_id,
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "episodic".to_owned(),
                    kind: "note".to_owned(),
                    content: "External custom resume state.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.9,
                    importance: 0.9,
                    provenance_uri: Some("test://external-resume".to_owned()),
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: vec!["session-external-resume".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = build_resume_report(&ResumeOptions {
            workspace_path: &workspace,
            database_path: &database,
            sessions: 3,
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(report.workspace_id, workspace_id);
        assert_eq!(report.episodic_total, 1);
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].items[0].memory_id, memory_id);
        Ok(())
    }

    #[test]
    fn sealed_placeholder_is_not_admitted_to_public_resume() -> Result<(), String> {
        let (_temp, workspace, database) = resume_storage_fixture("note", &["session-sealed"])?;
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d45)).to_string();
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection
            .execute_raw(&format!(
                "UPDATE memories SET content = '{}' WHERE id = '{}'",
                crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT,
                memory_id
            ))
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory_seal(
                &memory_id,
                &crate::models::memory_seal_commitment(b"withheld resume fixture"),
                "2026-08-09T20:00:00Z",
            )
            .map_err(|error| error.to_string())?;
        drop(connection);

        let report = build_resume_report(&ResumeOptions {
            workspace_path: &workspace,
            database_path: &database,
            sessions: 3,
        })
        .map_err(|error| error.to_string())?;
        assert_eq!(report.episodic_total, 0);
        assert!(report.sessions.is_empty());
        Ok(())
    }

    #[test]
    fn secret_bearing_body_is_not_admitted_to_public_resume() -> Result<(), String> {
        let (_temp, workspace, database) =
            resume_storage_fixture("note", &["session-secret-body", "next"])?;
        let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d45)).to_string();
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let secret = format!("sk_live_{}", "1234567890abcdef1234567890abcdef");
        connection
            .execute_raw(&format!(
                "UPDATE memories SET content = 'token={secret}' WHERE id = '{memory_id}'"
            ))
            .map_err(|error| error.to_string())?;
        drop(connection);

        let report = build_resume_report(&ResumeOptions {
            workspace_path: &workspace,
            database_path: &database,
            sessions: 3,
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(report.episodic_total, 0);
        assert!(report.sessions.is_empty());
        assert_eq!(report.open_loops.tagged_items_total, 0);
        assert!(report.open_loops.tagged_items.is_empty());
        let serialized = serde_json::to_string(&report).map_err(|error| error.to_string())?;
        assert!(!serialized.contains(&secret));
        Ok(())
    }

    #[test]
    fn genuinely_missing_database_returns_empty_resume_without_initializing_store()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("cold workspace");
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        let database = workspace.join(".ee").join("ee.db");

        let report = build_resume_report(&ResumeOptions {
            workspace_path: &workspace,
            database_path: &database,
            sessions: 3,
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(report.schema, super::RESUME_SCHEMA_V1);
        assert_eq!(report.episodic_total, 0);
        assert!(report.sessions.is_empty());
        assert!(report.open_loops.revisit_decisions.is_empty());
        assert!(report.open_loops.tagged_items.is_empty());
        assert!(report.nearby_stores.is_some());
        assert!(
            !workspace.join(".ee").exists(),
            "read-only resume must leave a genuinely cold workspace uninitialized"
        );
        Ok(())
    }

    #[test]
    fn unsafe_existing_database_is_an_error_not_an_empty_resume() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("unsafe workspace");
        let database = workspace.join(".ee").join("ee.db");
        std::fs::create_dir_all(&database).map_err(|error| error.to_string())?;

        match build_resume_report(&ResumeOptions {
            workspace_path: &workspace,
            database_path: &database,
            sessions: 3,
        }) {
            Err(DomainError::Storage { message, .. }) => {
                assert!(message.contains("unsafe, unreadable, or incompatible"));
                Ok(())
            }
            other => Err(format!(
                "expected unsafe addressed store to remain an error, got {other:?}"
            )),
        }
    }

    #[test]
    fn nearby_resume_command_is_prepended_and_shell_quoted() {
        let scan = NearbyStoreScanAssessment {
            stores: vec![NearbyStore {
                workspace_root: "/tmp/campaign's best root".to_owned(),
                store_dir: "/tmp/campaign's best root/.ee-campaign".to_owned(),
                documents: 42,
                last_write: Some("2026-08-10T14:15:16Z".to_owned()),
                provenance: crate::core::orient::NearbyStoreProvenance::ChildScan,
            }],
            outcome: NearbyStoreScanOutcome::Complete,
        };

        let commands = resume_next_commands(Some(&scan));
        assert_eq!(
            commands.first().map(String::as_str),
            Some(
                "ee resume --workspace '/tmp/campaign'\\''s best root' --database '/tmp/campaign'\\''s best root/.ee-campaign/ee.db' --json"
            )
        );
    }

    #[test]
    fn unavailable_nearby_scan_emits_diagnostic_without_claiming_no_store() {
        let scan = NearbyStoreScanAssessment {
            stores: vec![NearbyStore {
                workspace_root: "/tmp/unverified foreign workspace".to_owned(),
                store_dir: "/tmp/unverified foreign workspace/.ee".to_owned(),
                documents: 99,
                last_write: Some("2026-08-10T14:15:16Z".to_owned()),
                provenance: crate::core::orient::NearbyStoreProvenance::WorkspaceRegistry,
            }],
            outcome: NearbyStoreScanOutcome::Unavailable,
        };

        let commands = resume_next_commands(Some(&scan));

        assert_eq!(
            commands.first().map(String::as_str),
            Some("ee doctor --workspace . --json  # diagnose unavailable nearby-store discovery")
        );
        assert!(commands.iter().all(|command| !command.contains("no nearby")
            && !command.contains("unverified foreign workspace")));
    }
}
