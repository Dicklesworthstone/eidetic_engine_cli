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
//!    surface) plus memories tagged next/queue/blocking/pending/todo.
//! 3. STALENESS — surfaced items superseded by newer writes on the same
//!    subject (same kind, ≥ [`STALE_SHARED_TAG_MIN`] shared tags, newer
//!    timestamp) are flagged rather than silently ranked down: a stale
//!    next-step note actively misleads a resuming agent.
//! 4. Resume-flavored next commands, and nearby populated stores (reusing
//!    the bd-orient-store-discovery-ft1z5 scan) when the addressed store
//!    has nothing episodic to resume from.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::core::orient::{NearbyStoreScan, discover_nearby_stores};
use crate::db::{DbConnection, StoredMemory};
use crate::models::DomainError;

/// Wire schema id for the resume report.
pub const RESUME_SCHEMA_V1: &str = "ee.resume.v1";
/// Write-time gap that starts a new inferred session.
pub const SESSION_GAP_SECONDS: i64 = 4 * 3600;
/// Items listed per session (summary counts stay exact).
pub const SESSION_ITEM_CAP: usize = 20;
/// Open-loop tag vocabulary.
pub const OPEN_LOOP_TAGS: [&str; 5] = ["next", "queue", "blocking", "pending", "todo"];
/// Minimum shared tags for the staleness heuristic.
pub const STALE_SHARED_TAG_MIN: usize = 2;
/// Wall-clock budget for the nearby-store scan.
pub const RESUME_NEARBY_SCAN_BUDGET_MS: u64 = 250;
/// Cap on open-loop tagged items and staleness flags.
const OPEN_LOOP_CAP: usize = 32;

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
}

/// Open loops: revisit-conditioned decisions + tagged queue items.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenLoops {
    pub revisit_decisions: Vec<ResumeDecision>,
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
    pub stale_count: usize,
    /// Populated when the store has nothing episodic to resume from.
    pub nearby_stores: Option<NearbyStoreScan>,
    pub next_commands: Vec<String>,
}

fn parse_ts(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|ts| ts.with_timezone(&chrono::Utc))
}

fn item(memory: &StoredMemory, tags: &BTreeMap<String, Vec<String>>) -> ResumeItem {
    ResumeItem {
        memory_id: memory.id.clone(),
        level: memory.level.clone(),
        kind: memory.kind.clone(),
        content: memory.content.clone(),
        tags: tags.get(&memory.id).cloned().unwrap_or_default(),
        created_at: memory.created_at.clone(),
        stale: None,
    }
}

/// Group episodic memories (pre-sorted newest first) into sessions.
///
/// A `session-*` tag names the session; consecutive same-tag runs group
/// together. Untagged runs cluster by write-time gap.
fn group_sessions(
    memories: &[&StoredMemory],
    tags: &BTreeMap<String, Vec<String>>,
    limit: usize,
) -> Vec<ResumeSession> {
    let session_tag = |memory: &StoredMemory| -> Option<String> {
        tags.get(&memory.id).and_then(|memory_tags| {
            memory_tags
                .iter()
                .find(|tag| tag.starts_with("session-"))
                .cloned()
        })
    };

    let mut sessions: Vec<(Option<String>, Vec<&StoredMemory>)> = Vec::new();
    for memory in memories {
        let tag = session_tag(memory);
        let start_new = match sessions.last() {
            None => true,
            Some((last_tag, members)) => {
                if tag.is_some() || last_tag.is_some() {
                    tag != *last_tag
                } else {
                    // Both untagged: cluster by write-time gap.
                    let previous = members
                        .last()
                        .and_then(|previous| parse_ts(&previous.created_at));
                    let current = parse_ts(&memory.created_at);
                    match (previous, current) {
                        (Some(previous), Some(current)) => {
                            (previous - current).num_seconds().abs() > SESSION_GAP_SECONDS
                        }
                        _ => false,
                    }
                }
            }
        };
        if start_new {
            if sessions.len() >= limit {
                break;
            }
            sessions.push((tag, vec![memory]));
        } else if let Some(last) = sessions.last_mut() {
            last.1.push(memory);
        }
    }

    sessions
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
            let label = tag.unwrap_or_else(|| {
                let date = newest_at.get(..10).unwrap_or("unknown");
                format!("inferred-{date}")
            });
            let mut items: Vec<ResumeItem> = members
                .iter()
                .map(|memory| item(memory, &BTreeMap::new()))
                .collect();
            items.truncate(SESSION_ITEM_CAP);
            ResumeSession {
                label,
                member_count: members.len(),
                newest_at,
                oldest_at,
                items,
            }
        })
        .collect()
}

/// Flag surfaced items superseded by a newer live memory on the same
/// subject (same kind, ≥ [`STALE_SHARED_TAG_MIN`] shared tags, strictly
/// newer `created_at`). Returns the number of flags applied.
fn apply_staleness(
    items: &mut [ResumeItem],
    all_live: &[StoredMemory],
    tags: &BTreeMap<String, Vec<String>>,
) -> usize {
    let mut flagged = 0usize;
    for surfaced in items.iter_mut() {
        if surfaced.tags.is_empty() {
            continue;
        }
        let mut best: Option<StaleFlag> = None;
        for candidate in all_live {
            if candidate.id == surfaced.memory_id
                || candidate.kind != surfaced.kind
                || candidate.created_at <= surfaced.created_at
            {
                continue;
            }
            let candidate_tags = tags.get(&candidate.id);
            let Some(candidate_tags) = candidate_tags else {
                continue;
            };
            let shared: Vec<String> = surfaced
                .tags
                .iter()
                .filter(|tag| !tag.starts_with("session-") && candidate_tags.contains(tag))
                .cloned()
                .collect();
            if shared.len() < STALE_SHARED_TAG_MIN {
                continue;
            }
            let replace = match &best {
                None => true,
                Some(existing) => candidate.created_at > existing.superseded_by_created_at,
            };
            if replace {
                best = Some(StaleFlag {
                    superseded_by: candidate.id.clone(),
                    superseded_by_created_at: candidate.created_at.clone(),
                    shared_tags: shared,
                });
            }
        }
        if let Some(flag) = best {
            surfaced.stale = Some(flag);
            flagged += 1;
        }
    }
    flagged
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
    let workspace_id = crate::core::workspace::stable_workspace_id(&canonical_workspace);
    let all_live = connection
        .list_memories(&workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list memories: {error}"),
            repair: Some("ee doctor --workspace . --json".to_owned()),
        })?;

    let ids: Vec<&str> = all_live.iter().map(|memory| memory.id.as_str()).collect();
    let tags = connection.get_memory_tags_batch(&ids).unwrap_or_default();

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

    let mut sessions = group_sessions(&episodic, &tags, options.sessions.max(1));
    // group_sessions built items without the tag map (borrow simplicity);
    // hydrate tags now.
    for session in &mut sessions {
        for session_item in &mut session.items {
            session_item.tags = tags
                .get(&session_item.memory_id)
                .cloned()
                .unwrap_or_default();
        }
    }

    // Open loops: revisit-conditioned decisions via the decide surface.
    let decide = crate::core::decide::decide_list(&crate::core::decide::DecideListOptions {
        workspace_path: options.workspace_path,
        database_path: Some(options.database_path),
        about: None,
        include_superseded: false,
        limit: 200,
        now: None,
    });
    let revisit_decisions = match decide {
        Ok(report) => report
            .decisions
            .into_iter()
            .filter(|decision| decision.revisit_by.is_some() && !decision.superseded)
            .map(|decision| ResumeDecision {
                memory_id: decision.memory_id,
                topic: decision.topic,
                chosen: decision.chosen,
                revisit_by: decision.revisit_by,
                revisit_status: decision.revisit_status,
                created_at: decision.created_at,
            })
            .collect(),
        // A store without the decide surface still resumes; loops are empty.
        Err(_) => Vec::new(),
    };

    let mut tagged_items: Vec<ResumeItem> = all_live
        .iter()
        .filter(|memory| {
            tags.get(&memory.id).is_some_and(|memory_tags| {
                memory_tags
                    .iter()
                    .any(|tag| OPEN_LOOP_TAGS.contains(&tag.as_str()))
            })
        })
        .map(|memory| item(memory, &tags))
        .collect();
    tagged_items.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.memory_id.cmp(&a.memory_id))
    });
    tagged_items.truncate(OPEN_LOOP_CAP);

    // Staleness pass over everything surfaced.
    let mut stale_count = apply_staleness(&mut tagged_items, &all_live, &tags);
    for session in &mut sessions {
        stale_count += apply_staleness(&mut session.items, &all_live, &tags);
    }

    let nearby_stores = if episodic_total == 0 {
        Some(discover_nearby_stores(
            options.workspace_path,
            std::time::Duration::from_millis(RESUME_NEARBY_SCAN_BUDGET_MS),
        ))
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
            revisit_decisions,
            tagged_items,
        },
        stale_count,
        nearby_stores,
        next_commands,
    })
}

fn resume_next_commands(nearby_stores: Option<&NearbyStoreScan>) -> Vec<String> {
    let mut commands = vec![
        "ee decide list --json  # open decisions incl. revisit conditions".to_owned(),
        "ee orient \"<current task>\" --json  # task-conditioned pack once you know the task"
            .to_owned(),
        "ee conflict list --json  # anything contradictory left behind".to_owned(),
    ];
    if let Some(best) = nearby_stores.and_then(|scan| scan.stores.first()) {
        let workspace = shell_quote_cli_arg(&best.workspace_root);
        commands.insert(0, format!("ee resume --workspace {workspace} --json"));
    }
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
        OPEN_LOOP_TAGS, ResumeItem, SESSION_GAP_SECONDS, STALE_SHARED_TAG_MIN, StaleFlag,
        apply_staleness, group_sessions, resume_next_commands,
    };
    use crate::core::orient::{NearbyStore, NearbyStoreScan};
    use crate::db::StoredMemory;
    use std::collections::BTreeMap;

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
            trust_class: "agent_inferred".to_owned(),
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
        assert!(9 * 3600 > SESSION_GAP_SECONDS);
        assert_eq!(sessions[2].member_count, 1);
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
    fn staleness_flags_newer_same_subject_write() {
        let mut items = vec![ResumeItem {
            memory_id: "mem_old".to_owned(),
            level: "episodic".to_owned(),
            kind: "note".to_owned(),
            content: "Next: run arc 4".to_owned(),
            tags: vec!["next".to_owned(), "arc4".to_owned()],
            created_at: "2026-08-01T00:00:00Z".to_owned(),
            stale: None,
        }];
        let newer = memory("mem_new", "episodic", "note", "2026-08-09T00:00:00Z");
        let unrelated = memory("mem_x", "episodic", "note", "2026-08-09T00:00:00Z");
        let mut tags = BTreeMap::new();
        tags.insert(
            "mem_new".to_owned(),
            vec!["next".to_owned(), "arc4".to_owned()],
        );
        tags.insert("mem_x".to_owned(), vec!["other".to_owned()]);
        let all = vec![newer, unrelated];
        let flagged = apply_staleness(&mut items, &all, &tags);
        assert_eq!(flagged, 1);
        let flag: &StaleFlag = items[0].stale.as_ref().unwrap();
        assert_eq!(flag.superseded_by, "mem_new");
        assert_eq!(flag.shared_tags.len(), 2);
        assert!(flag.shared_tags.len() >= STALE_SHARED_TAG_MIN);
    }

    #[test]
    fn staleness_ignores_single_shared_tag_and_older_writes() {
        let mut items = vec![ResumeItem {
            memory_id: "mem_old".to_owned(),
            level: "episodic".to_owned(),
            kind: "note".to_owned(),
            content: "note".to_owned(),
            tags: vec!["next".to_owned(), "arc4".to_owned()],
            created_at: "2026-08-05T00:00:00Z".to_owned(),
            stale: None,
        }];
        let single_overlap = memory("mem_s", "episodic", "note", "2026-08-09T00:00:00Z");
        let older = memory("mem_older", "episodic", "note", "2026-08-01T00:00:00Z");
        let mut tags = BTreeMap::new();
        tags.insert("mem_s".to_owned(), vec!["next".to_owned()]);
        tags.insert(
            "mem_older".to_owned(),
            vec!["next".to_owned(), "arc4".to_owned()],
        );
        let all = vec![single_overlap, older];
        assert_eq!(apply_staleness(&mut items, &all, &tags), 0);
        assert!(items[0].stale.is_none());
        assert!(OPEN_LOOP_TAGS.contains(&"next"));
    }

    #[test]
    fn nearby_resume_command_is_prepended_and_shell_quoted() {
        let scan = NearbyStoreScan {
            stores: vec![NearbyStore {
                workspace_root: "/tmp/campaign's best root".to_owned(),
                store_dir: "/tmp/campaign's best root/.ee-campaign".to_owned(),
                documents: 42,
                last_write: Some("2026-08-10T14:15:16Z".to_owned()),
            }],
            truncated: false,
        };

        let commands = resume_next_commands(Some(&scan));
        assert_eq!(
            commands.first().map(String::as_str),
            Some("ee resume --workspace '/tmp/campaign'\\''s best root' --json")
        );
    }
}
