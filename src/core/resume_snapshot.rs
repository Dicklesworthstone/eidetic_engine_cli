//! A resume bundle is one source snapshot, not a sequence of current reads.
//!
//! Session tags, open-loop labels and canonical decision fields must agree with
//! the admitted memory bodies. A concurrent writer may commit while we read,
//! but its changes belong to the next resume, never half of this one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Utc};

use super::{
    RESUME_STORAGE_PAGE_SIZE, ResumeAdmissionBoundary, ResumeOptions, load_decision_typed_fields,
};
use crate::db::{DbConnection, StoredMemory};
use crate::models::DomainError;

pub(super) struct ResumeState {
    pub(super) workspace_id: String,
    pub(super) all_live: Vec<StoredMemory>,
    pub(super) tags: BTreeMap<String, Vec<String>>,
    pub(super) typed_decision_fields: BTreeMap<String, String>,
    pub(super) transcript_history: super::ResumeTranscriptHistory,
}

pub(super) fn load(
    connection: &DbConnection,
    options: &ResumeOptions<'_>,
    canonical_workspace: &Path,
    now: DateTime<Utc>,
) -> Result<ResumeState, DomainError> {
    load_with_boundary(connection, options, canonical_workspace, now, || Ok(()))
}

// The boundary permits deterministic real-writer interleavings in tests,
// following ask's corpus loader. Production supplies a no-op, not a mock store.
fn load_with_boundary(
    connection: &DbConnection,
    options: &ResumeOptions<'_>,
    canonical_workspace: &Path,
    now: DateTime<Utc>,
    after_memories: impl FnOnce() -> Result<(), DomainError>,
) -> Result<ResumeState, DomainError> {
    let snapshot = ResumeReadSnapshot::begin(connection)?;
    if connection
        .needs_migration()
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to inspect addressed workspace schema: {error}"),
            repair: Some("ee doctor --workspace . --json".to_owned()),
        })?
    {
        return Err(DomainError::MigrationRequired {
            message: "The addressed workspace database requires migration before resume."
                .to_owned(),
            repair: Some("ee migrate run --workspace . --json".to_owned()),
        });
    }
    let workspace_id = crate::core::workspace::addressed_workspace_row(
        connection,
        options.workspace_path,
        options.database_path,
    )?
    .map_or_else(
        || crate::core::workspace::stable_workspace_id(canonical_workspace),
        |row| row.id,
    );
    let mut current_memories = connection
        .list_recent_current_memories_for_retrieval(
            &workspace_id,
            // The storage query derives its own canonical validity bound.
            // Keep fractional precision for created_at/updated_at: a fresh
            // row must not disappear for the rest of the current second.
            &crate::core::memory::normalize_row_timestamp(now),
            u32::MAX,
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list current resume memories: {error}"),
            repair: Some("ee doctor --workspace . --json".to_owned()),
        })?;
    after_memories()?;
    // A populated body is not proof of reveal. Resolve the actual seal in the
    // same snapshot before loading session tags or typed decision payloads;
    // otherwise a sealed decision can reappear as queued work or a revisit.
    // One workspace read replaces per-memory seal lookups, and the ordinary
    // empty-store path does not need an authority read at all.
    if !current_memories.is_empty() {
        let closed: BTreeSet<_> = crate::core::memory_lifecycle::load_memory_seals_for_admission(
            connection,
            &workspace_id,
        )
        .map_err(|_| DomainError::Storage {
            message: "Could not verify resume memory seals; bundle withheld".to_owned(),
            repair: Some("ee doctor --workspace . --json".to_owned()),
        })?
        .into_iter()
        .filter(|seal| seal.is_sealed())
        .map(|seal| seal.memory_id)
        .collect();
        current_memories.retain(|memory| !closed.contains(&memory.id));
    }
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
        ResumeAdmissionBoundary::for_bound_workspace(canonical_workspace, workspace_id.clone());
    let all_live: Vec<StoredMemory> = current_memories
        .into_iter()
        .filter_map(|memory| {
            let memory_tags = tags.get(&memory.id).map(Vec::as_slice).unwrap_or_default();
            admission.admit(memory, memory_tags)
        })
        .collect();

    let typed_decision_fields = load_decision_typed_fields(connection, &all_live)?;
    let transcript_history = super::transcripts::load(
        connection,
        &workspace_id,
        &all_live,
        options.sessions,
    )?;
    snapshot.finish()?;
    Ok(ResumeState {
        workspace_id,
        all_live,
        tags,
        typed_decision_fields,
        transcript_history,
    })
}

/// Own only a transaction whose BEGIN succeeded. A nested begin must not
/// release someone else's transaction; errors and unwinding release ours.
struct ResumeReadSnapshot<'a> {
    connection: &'a DbConnection,
    active: bool,
}

impl<'a> ResumeReadSnapshot<'a> {
    fn begin(connection: &'a DbConnection) -> Result<Self, DomainError> {
        connection
            .begin_read_snapshot()
            .map_err(|_| snapshot_error("begin"))?;
        Ok(Self {
            connection,
            active: true,
        })
    }

    fn finish(mut self) -> Result<(), DomainError> {
        self.connection
            .commit_read_snapshot()
            .map_err(|_| snapshot_error("finish"))?;
        self.active = false;
        Ok(())
    }
}

impl Drop for ResumeReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.connection.rollback_read_snapshot().is_err() {
            tracing::error!(
                target: "ee::core::resume::snapshot",
                "failed to release resume read snapshot"
            );
        }
    }
}

fn snapshot_error(stage: &str) -> DomainError {
    DomainError::Storage {
        message: format!("Could not {stage} a coherent resume snapshot; bundle withheld"),
        repair: Some(
            "Retry ee resume; use ee doctor --workspace . --json if the failure persists."
                .to_owned(),
        ),
    }
}

#[cfg(test)]
#[path = "resume_snapshot_tests.rs"]
mod tests;

#[cfg(test)]
mod seal_authority_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
    use std::path::PathBuf;

    const SEALED: &str = "mem_00000000000000000000000061";
    const PUBLIC: &str = "mem_00000000000000000000000062";
    const TIME: &str = "2026-08-09T10:00:00Z";
    const PRIVATE_BODY: &str = "Topic: Private launch\nChosen: reserved choice";

    struct Fixture {
        _root: tempfile::TempDir,
        workspace: PathBuf,
        database: PathBuf,
        workspace_id: String,
        writer: DbConnection,
        reader: DbConnection,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace");
            std::fs::create_dir_all(workspace.join(".ee")).unwrap();
            let workspace = workspace.canonicalize().unwrap();
            let database = workspace.join(".ee/ee.db");
            let writer = DbConnection::open_file(&database).unwrap();
            writer.migrate().unwrap();
            let workspace_id = crate::core::workspace::stable_workspace_id(&workspace);
            writer
                .insert_workspace(
                    &workspace_id,
                    &CreateWorkspaceInput {
                        path: workspace.to_string_lossy().into_owned(),
                        name: None,
                    },
                )
                .unwrap();
            let reader = DbConnection::open_file_read_only(&database).unwrap();
            let fixture = Self {
                _root: root,
                workspace,
                database,
                workspace_id,
                writer,
                reader,
            };
            fixture.seed(SEALED, PRIVATE_BODY, true);
            fixture
        }

        fn seed(&self, id: &str, body: &str, decision: bool) {
            self.writer
                .insert_memory(
                    id,
                    &CreateMemoryInput {
                        workspace_id: self.workspace_id.clone(),
                        level: "episodic".to_owned(),
                        kind: if decision { "decision" } else { "note" }.to_owned(),
                        content: body.to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: Some(format!("ee://memory/{id}")),
                        trust_class: "agent_assertion".to_owned(),
                        trust_subclass: None,
                        tags: if decision {
                            vec!["next".to_owned(), "session-reserved".to_owned()]
                        } else {
                            vec!["session-public".to_owned()]
                        },
                        valid_from: Some(TIME.to_owned()),
                        valid_to: None,
                    },
                )
                .unwrap();
            if decision {
                self.writer
                    .set_memory_typed_fields_json(
                        id,
                        Some(
                            &serde_json::json!({
                                "options": ["reserved choice", "alternative"],
                                "chosen": "reserved choice",
                                "rationale": "Private launch plan",
                                "revisit_by": "2026-08-11T00:00:00Z"
                            })
                            .to_string(),
                        ),
                    )
                    .unwrap();
            }
            self.writer
                .execute_raw(&format!(
                    "UPDATE memories SET created_at = '{TIME}', updated_at = '{TIME}' WHERE id = '{id}'"
                ))
                .unwrap();
        }

        fn options(&self) -> ResumeOptions<'_> {
            ResumeOptions {
                workspace_path: &self.workspace,
                database_path: &self.database,
                sessions: 3,
            }
        }

        fn seal(&self) {
            self.writer
                .insert_memory_seal(SEALED, &format!("blake3:{}", "a".repeat(64)), TIME)
                .unwrap();
        }

        fn reveal(&self) {
            assert!(self.writer.mark_memory_seal_revealed(SEALED, TIME).unwrap());
        }

        fn load(&self) -> ResumeState {
            load(
                &self.reader,
                &self.options(),
                &self.workspace,
                reference_time(),
            )
            .unwrap()
        }
    }

    fn reference_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn assert_hidden(state: &ResumeState) {
        assert!(state.all_live.iter().all(|memory| memory.id != SEALED));
        assert!(!state.tags.contains_key(SEALED));
        assert!(!state.typed_decision_fields.contains_key(SEALED));
    }

    #[test]
    fn closed_seals_withhold_session_queue_and_typed_decision_projections() {
        let fixture = Fixture::new();
        fixture.seed(PUBLIC, "Completed release validation.", false);
        fixture.seal();
        let before = fixture.writer.get_memory(SEALED).unwrap().unwrap();
        assert_eq!(
            before.content, PRIVATE_BODY,
            "exercise a non-placeholder body"
        );
        let audits = fixture.writer.count_table_rows("audit_log").unwrap();
        let state = fixture.load();
        assert_hidden(&state);
        assert_eq!(state.all_live.len(), 1);
        assert_eq!(state.all_live[0].id, PUBLIC);
        let report = super::super::build_resume_report(&fixture.options()).unwrap();
        assert_eq!(report.episodic_total, 1);
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].label, "session-public");
        assert_eq!(report.open_loops.revisit_decisions_total, 0);
        assert_eq!(report.open_loops.tagged_items_total, 0);
        let output = serde_json::to_string(&report).unwrap();
        for hidden in [
            SEALED,
            "reserved choice",
            "session-reserved",
            "Private launch",
        ] {
            assert!(
                !output.contains(hidden),
                "hidden source must not reach any projection"
            );
        }
        assert_eq!(fixture.writer.get_memory(SEALED).unwrap().unwrap(), before);
        assert_eq!(
            fixture.writer.count_table_rows("audit_log").unwrap(),
            audits
        );
        assert!(!fixture.workspace.join(".ee/index").exists());
    }

    #[test]
    fn an_explicit_reveal_readmits_the_original_session_and_decision() {
        let fixture = Fixture::new();
        fixture.seal();
        assert_hidden(&fixture.load());
        fixture.reveal();
        let state = fixture.load();
        assert_eq!(state.all_live.len(), 1);
        assert_eq!(state.all_live[0].content, PRIVATE_BODY);
        assert!(state.tags[SEALED].contains(&"session-reserved".to_owned()));
        assert!(state.typed_decision_fields.contains_key(SEALED));
        let (decisions, total, truncated) = super::super::collect_revisit_decisions(
            &state.all_live,
            &state.typed_decision_fields,
            reference_time(),
        )
        .unwrap();
        assert_eq!(total, 1);
        assert!(!truncated);
        assert_eq!(decisions[0].memory_id, SEALED);
        assert_eq!(decisions[0].chosen, "reserved choice");
    }

    #[test]
    fn concurrent_sealing_affects_the_next_snapshot_not_half_the_current_bundle() {
        let fixture = Fixture::new();
        let state = load_with_boundary(
            &fixture.reader,
            &fixture.options(),
            &fixture.workspace,
            reference_time(),
            || {
                fixture.seal();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(state.all_live.len(), 1);
        assert!(state.tags.contains_key(SEALED));
        assert!(state.typed_decision_fields.contains_key(SEALED));
        assert_hidden(&fixture.load());
    }

    #[test]
    fn concurrent_reveal_cannot_publish_a_body_from_a_still_sealed_snapshot() {
        let fixture = Fixture::new();
        fixture.seal();
        let state = load_with_boundary(
            &fixture.reader,
            &fixture.options(),
            &fixture.workspace,
            reference_time(),
            || {
                fixture.reveal();
                Ok(())
            },
        )
        .unwrap();
        assert_hidden(&state);
        let revealed = fixture.load();
        assert_eq!(revealed.all_live.len(), 1);
        assert!(revealed.tags.contains_key(SEALED));
        assert!(revealed.typed_decision_fields.contains_key(SEALED));
        fixture
            .reader
            .begin_read_snapshot()
            .expect("snapshot released");
        fixture.reader.rollback_read_snapshot().unwrap();
    }
}

#[cfg(test)]
#[path = "resume_transcript_tests.rs"]
mod transcript_tests;
