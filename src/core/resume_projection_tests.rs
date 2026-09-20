//! Differential and bounded-work regressions for the real public projection.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::super::{
    RESUME_SESSION_CAP, ResumeSession, SESSION_ITEM_CAP, apply_report_staleness, group_sessions,
    group_sessions_with_projection, item,
};
use super::*;

fn memory(id: &str, kind: &str, created_at: &str) -> StoredMemory {
    StoredMemory {
        id: id.to_owned(),
        workspace_id: "wsp_00000000000000000000000001".to_owned(),
        level: "episodic".to_owned(),
        kind: kind.to_owned(),
        content: format!("Session result {id}"),
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

#[test]
fn indexed_staleness_matches_exhaustive_reference_for_mixed_subjects_and_order() {
    let times = [
        "2026-08-09T08:00:00Z",
        "2026-08-09T08:00:00.500Z",
        "2026-08-09T10:00:00+02:00",
        "2026-08-09T04:00:00-05:00",
        "2026-08-09T09:00:00Z",
        "2026-08-10T00:00:00Z",
        "invalid",
    ];
    let mut corpus = Vec::new();
    let mut tags = BTreeMap::new();
    for index in 0..256 {
        let id = format!("mem_{index:04}");
        let kind = if index % 3 == 0 { "decision" } else { "note" };
        corpus.push(memory(&id, kind, times[index % times.len()]));
        if index % 11 != 0 {
            tags.insert(
                id,
                vec![
                    format!("subject-{}", index % 9),
                    format!("subject-{}", index % 5),
                    "next".to_owned(),
                    format!("session-{}", index % 3),
                ],
            );
        }
    }
    let original: Vec<_> = corpus.iter().map(|row| item(row, &tags, "test")).collect();
    let mut expected = original.clone();
    let expected_ids = reference_staleness(&mut expected, &corpus, &tags);
    let mut actual = original.clone();
    assert_eq!(
        StalenessIndex::new(&corpus, &tags).apply(&mut actual),
        expected_ids
    );
    assert_eq!(
        actual, expected,
        "all public flags, reasons, and shared tags must agree"
    );
    corpus.reverse();
    let mut reversed = original;
    assert_eq!(
        StalenessIndex::new(&corpus, &tags).apply(&mut reversed),
        expected_ids
    );
    assert_eq!(
        reversed, expected,
        "source enumeration order cannot change the winner"
    );
}

#[test]
fn chronology_uses_instants_not_rfc3339_spelling() {
    let rows = [
        memory("mem_offset_old", "note", "2026-08-10T10:00:00+02:00"),
        memory("mem_z", "note", "2026-08-10T09:00:00Z"),
        memory("mem_fraction", "note", "2026-08-10T09:00:00.500Z"),
        memory("mem_equal", "note", "2026-08-10T04:00:00-05:00"),
        memory("mem_invalid", "note", "invalid"),
    ];
    let mut ordered: Vec<_> = rows.iter().collect();
    sort_newest_first(&mut ordered);
    assert_eq!(
        ordered
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        [
            "mem_fraction",
            "mem_z",
            "mem_equal",
            "mem_offset_old",
            "mem_invalid"
        ]
    );
}

#[test]
fn equal_instants_do_not_supersede_and_newer_ties_use_smallest_identity() {
    let rows = vec![
        memory("mem_old", "note", "2026-08-10T08:00:00Z"),
        memory("mem_b", "note", "2026-08-10T10:00:00+01:00"),
        memory("mem_a", "note", "2026-08-10T09:00:00Z"),
    ];
    let tags = rows
        .iter()
        .map(|row| {
            (
                row.id.clone(),
                vec![
                    "subject-b".to_owned(),
                    "subject-a".to_owned(),
                    "subject-a".to_owned(),
                    "next".to_owned(),
                ],
            )
        })
        .collect();
    let mut items: Vec<_> = rows.iter().map(|row| item(row, &tags, "test")).collect();
    let flagged = StalenessIndex::new(&rows, &tags).apply(&mut items);
    assert_eq!(flagged, BTreeSet::from(["mem_old".to_owned()]));
    let flag = items[0].stale.as_ref().unwrap();
    assert_eq!(flag.superseded_by, "mem_a");
    assert_eq!(flag.shared_tags, ["subject-a", "subject-b"]);
    assert!(items[1].stale.is_none() && items[2].stale.is_none());
}

#[test]
fn hidden_cross_level_superseder_is_kept_but_other_kinds_cannot_win() {
    let mut hidden = memory("mem_hidden", "note", "2026-08-10T09:00:00Z");
    hidden.level = "semantic".to_owned();
    let rows = vec![
        memory("mem_visible", "note", "2026-08-10T08:00:00Z"),
        hidden,
        memory("mem_other_kind", "decision", "2026-08-11T10:00:00Z"),
    ];
    let tags = rows
        .iter()
        .map(|row| (row.id.clone(), vec!["subject".to_owned()]))
        .collect();
    let mut items = vec![item(&rows[0], &tags, "test")];
    StalenessIndex::new(&rows, &tags).apply(&mut items);
    assert_eq!(items[0].stale.as_ref().unwrap().superseded_by, "mem_hidden");
}

#[test]
fn control_tags_and_malformed_dates_never_grant_supersession() {
    let rows = vec![
        memory("mem_old", "note", "2026-08-10T08:00:00Z"),
        memory("mem_control", "note", "2026-08-10T09:00:00Z"),
        memory("mem_invalid", "note", "invalid"),
    ];
    let tags = BTreeMap::from([
        (
            rows[0].id.clone(),
            vec![
                "subject".to_owned(),
                "next".to_owned(),
                "session-a".to_owned(),
            ],
        ),
        (
            rows[1].id.clone(),
            vec!["next".to_owned(), "session-a".to_owned()],
        ),
        (rows[2].id.clone(), vec!["subject".to_owned()]),
    ]);
    let index = StalenessIndex::new(&rows, &tags);
    assert_eq!(index.heads["note"].len(), 1);
    let mut items: Vec<_> = rows.iter().map(|row| item(row, &tags, "test")).collect();
    assert!(index.apply(&mut items).is_empty());
}

#[test]
fn only_requested_session_members_reach_public_projection() {
    let base = parse_ts("2026-08-01T00:00:00Z").unwrap();
    let rows: Vec<_> = (0..4096)
        .map(|n| {
            memory(
                &format!("mem_{n:05}"),
                "note",
                &(base + chrono::Duration::seconds(n)).to_rfc3339(),
            )
        })
        .collect();
    let tags = rows
        .iter()
        .map(|row| (row.id.clone(), vec![format!("session-{}", row.id)]))
        .collect();
    let mut ordered: Vec<_> = rows.iter().collect();
    sort_newest_first(&mut ordered);
    let mut visits = 0;
    let sessions = group_sessions_with_projection(&ordered, &tags, 3, |row, row_tags, reason| {
        visits += 1;
        item(row, row_tags, reason)
    });
    assert_eq!(
        visits, 3,
        "off-page sessions must never render private bodies or provenance"
    );
    assert_eq!(sessions.len(), 3);
    assert_eq!(sessions[0].items[0].memory_id, "mem_04095");
    assert_eq!(sessions[2].items[0].memory_id, "mem_04093");
    let empty = group_sessions_with_projection(&ordered, &tags, 0, |_, _, _| {
        panic!("zero output budget must not invoke projection")
    });
    assert!(empty.is_empty());
}

#[test]
fn chronological_session_selection_preserves_counts_bounds_and_backfills() {
    let mut rows = Vec::new();
    let mut tags = BTreeMap::new();
    for n in 0..31 {
        for (label, timestamp) in [
            ("session-new", "2026-08-10T09:00:00Z"),
            ("session-old", "2026-08-10T10:00:00+02:00"),
        ] {
            let id = format!("mem_{label}_{n:02}");
            rows.push(memory(&id, "note", timestamp));
            tags.insert(id, vec![label.to_owned()]);
        }
    }
    // A backfilled member must stay in its stable session and contribute to
    // exact counts/bounds even though it does not fit in the rendered page.
    rows.push(memory("mem_backfill", "note", "2026-08-01T00:00:00Z"));
    tags.insert("mem_backfill".to_owned(), vec!["session-new".to_owned()]);
    let mut ordered: Vec<_> = rows.iter().collect();
    sort_newest_first(&mut ordered);
    let sessions = group_sessions(&ordered, &tags, 1);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].label, "session-new");
    assert_eq!(sessions[0].member_count, 32);
    assert_eq!(sessions[0].items.len(), SESSION_ITEM_CAP);
    assert_eq!(sessions[0].newest_at, "2026-08-10T09:00:00Z");
    assert_eq!(sessions[0].oldest_at, "2026-08-01T00:00:00Z");
    assert!(group_sessions(&ordered, &tags, usize::MAX).len() <= RESUME_SESSION_CAP);
}

#[test]
fn ten_thousand_rows_share_a_bounded_subject_index() {
    let base = parse_ts("2026-08-01T00:00:00Z").unwrap();
    let rows: Vec<_> = (0..10_000)
        .map(|n| {
            memory(
                &format!("mem_{n:05}"),
                "note",
                &(base + chrono::Duration::seconds(n)).to_rfc3339(),
            )
        })
        .collect();
    let tags = rows
        .iter()
        .map(|row| {
            (
                row.id.clone(),
                vec![
                    "subject-a".to_owned(),
                    "subject-b".to_owned(),
                    "next".to_owned(),
                    "session-current".to_owned(),
                ],
            )
        })
        .collect();
    let index = StalenessIndex::new(&rows, &tags);
    assert_eq!(index.heads.len(), 1);
    assert_eq!(
        index.heads["note"].len(),
        2,
        "one head per subject, not per memory or output item"
    );
    let mut items: Vec<_> = rows[..20]
        .iter()
        .map(|row| item(row, &tags, "test"))
        .collect();
    assert_eq!(index.apply(&mut items).len(), 20);
    assert!(
        items
            .iter()
            .all(|row| row.stale.as_ref().unwrap().superseded_by == "mem_09999")
    );
}

#[test]
fn indexed_shared_subjects_stay_redacted_and_duplicate_projections_count_once() {
    let rows = vec![
        memory("mem_old", "note", "2026-08-10T08:00:00Z"),
        memory("mem_new", "note", "2026-08-10T09:00:00Z"),
    ];
    let secret = format!("sk_live_{}", "1234567890abcdef1234567890abcdef");
    let tags = rows
        .iter()
        .map(|row| (row.id.clone(), vec![secret.clone(), "topic".to_owned()]))
        .collect();
    let mut tagged = vec![item(&rows[0], &tags, "open_loop_tag")];
    let mut sessions = vec![ResumeSession {
        label: "session-safe".to_owned(),
        member_count: 1,
        newest_at: rows[0].created_at.clone(),
        oldest_at: rows[0].created_at.clone(),
        items: vec![item(&rows[0], &tags, "recent_session_member")],
    }];
    assert_eq!(
        apply_report_staleness(&mut tagged, &mut sessions, &rows, &tags),
        1
    );
    let serialized = serde_json::to_string(&tagged).unwrap();
    assert!(!serialized.contains(&secret));
    assert!(tagged[0].redaction.applied);
    assert!(
        tagged[0]
            .redaction
            .reasons
            .iter()
            .any(|reason| reason.starts_with("stale.sharedTag:"))
    );
    assert_eq!(tagged[0].stale, sessions[0].items[0].stale);
}

#[test]
fn public_resume_keeps_database_and_sidecar_bytes_unchanged() {
    use super::super::{ResumeOptions, build_resume_report};
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};

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
                path: workspace.display().to_string(),
                name: Some("read-only resume".to_owned()),
            },
        )
        .unwrap();
    let id = crate::models::MemoryId::from_uuid(uuid::Uuid::from_u128(0x52534d49)).to_string();
    writer
        .insert_memory(
            &id,
            &CreateMemoryInput {
                workspace_id,
                level: "episodic".to_owned(),
                kind: "note".to_owned(),
                content: "Resume the current task without changing the source store.".to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: None,
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: None,
                tags: vec!["session-read-only".to_owned(), "next".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .unwrap();
    writer.close().unwrap();
    let fingerprint = || {
        ["", "-wal", "-shm"].map(|suffix| {
            let path = std::path::PathBuf::from(format!("{}{suffix}", database.display()));
            match std::fs::read(&path) {
                Ok(bytes) => Some(blake3::hash(&bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("cannot fingerprint {}: {error}", path.display()),
            }
        })
    };
    let before = fingerprint();
    let options = ResumeOptions {
        workspace_path: &workspace,
        database_path: &database,
        sessions: 3,
    };
    for _ in 0..2 {
        let report = build_resume_report(&options).unwrap();
        assert_eq!(report.episodic_total, 1);
        assert_eq!(report.sessions[0].items[0].memory_id, id);
        assert_eq!(report.open_loops.tagged_items.len(), 1);
        assert_eq!(
            fingerprint(),
            before,
            "resume must not create, checkpoint, or modify store sidecars"
        );
    }
}

// Frozen pre-index algorithm: compare complete public output, not just counts.
fn reference_staleness(
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
