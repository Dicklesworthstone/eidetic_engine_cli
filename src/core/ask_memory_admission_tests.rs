#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::super::{AskReadSnapshot, load_current_ask_corpus};
use super::*;
use crate::core::ask::{AskRequest, evaluate_ask};
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

const WORKSPACE: &str = "wsp_00000000000000000000000071";
const CUTOFF: &str = "2026-09-17T12:00:00Z";
const FIXTURE_VALID_FROM: &str = "2019-01-01T00:00:00Z";
const BODY: &str = "Run cargo fmt before release.";

fn at(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .expect("fixture timestamp")
        .with_timezone(&Utc)
}

fn id(index: usize) -> String {
    format!("mem_{index:026}")
}

fn fixture() -> (tempfile::TempDir, DbConnection) {
    let root = tempfile::tempdir().expect("real store");
    let path = root.path().canonicalize().expect("physical root");
    let db = DbConnection::open_file(&path.join("ask.db")).expect("open store");
    db.migrate().expect("migrate real schema");
    db.insert_workspace(
        WORKSPACE,
        &CreateWorkspaceInput {
            path: path.to_string_lossy().into_owned(),
            name: None,
        },
    )
    .expect("workspace");
    (root, db)
}

fn seed(db: &DbConnection, index: usize, from: Option<&str>, to: Option<&str>) {
    db.insert_memory(
        &id(index),
        &CreateMemoryInput {
            workspace_id: WORKSPACE.to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: BODY.to_owned(),
            workflow_id: None,
            confidence: 1.0,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: Some("manual://metadata-first-ask".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            // These cases query a fixed historical snapshot. An omitted
            // production bound inherits the real creation time, which would
            // put every fixture after CUTOFF and invert the expired cases.
            valid_from: Some(from.unwrap_or(FIXTURE_VALID_FROM).to_owned()),
            valid_to: to.map(str::to_owned),
        },
    )
    .expect("memory");
}

fn observed(db: &DbConnection, reference: &str) -> (Vec<StoredMemory>, Vec<Vec<String>>) {
    let snapshot = AskReadSnapshot::begin(db).expect("owned read snapshot");
    let mut batches = Vec::new();
    let memories = load_with_hydration_observer(db, WORKSPACE, at(reference), |page| {
        batches.push(page.iter().map(|id| (*id).to_owned()).collect());
    })
    .expect("admission");
    snapshot.finish().expect("release snapshot");
    (memories, batches)
}

#[test]
fn inactive_and_withheld_bodies_never_reach_the_body_decoder() {
    let (_root, db) = fixture();
    seed(&db, 1, None, Some("2026-01-01T00:00:00Z"));
    seed(&db, 2, Some("2027-01-01T00:00:00Z"), None);
    for index in 3..=6 {
        seed(&db, index, None, None);
    }
    assert!(
        db.restore_imported_memory_supersession(&id(3), CUTOFF)
            .unwrap()
    );
    db.insert_memory_seal(&id(4), &format!("blake3:{}", "a".repeat(64)), CUTOFF)
        .unwrap();
    db.execute_raw(&format!(
        "UPDATE memories SET tombstoned_at = '{CUTOFF}' WHERE id = '{}'",
        id(5)
    ))
    .unwrap();

    let audits = db.count_table_rows("audit_log").unwrap();
    let (memories, batches) = observed(&db, CUTOFF);
    assert_eq!(batches, vec![vec![id(6)]]);
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].id, id(6));
    assert_eq!(db.count_table_rows("audit_log").unwrap(), audits);

    // Exercise the actual corpus and answer entry points, not just the helper.
    let corpus = load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF)).unwrap();
    let report = evaluate_ask(
        &AskRequest {
            question: "Run cargo fmt before release".to_owned(),
            contradictions: corpus.contradictions,
            native_sources: corpus.native_sources,
            ..AskRequest::default()
        },
        &corpus.candidates,
    );
    assert!(!report.abstained);
    assert_eq!(report.citations.len(), 1);
    assert_eq!(report.citations[0].memory_id, id(6));
    assert_eq!(report.citations[0].text, BODY);
}

#[test]
fn a_fully_inactive_corpus_makes_no_body_queries() {
    let (_root, db) = fixture();
    db.with_transaction(|| {
        for index in 1..=ASK_MEMORY_REVISION_PAGE_SIZE + 1 {
            seed(&db, index, None, Some("2020-01-01T00:00:00Z"));
        }
        Ok(())
    })
    .unwrap();
    let (memories, batches) = observed(&db, CUTOFF);
    assert!(memories.is_empty());
    assert!(batches.is_empty());
}

#[test]
fn all_eligible_sources_survive_beyond_two_hydration_pages() {
    let (_root, db) = fixture();
    let count = ASK_MEMORY_REVISION_PAGE_SIZE * 2 + 1;
    db.with_transaction(|| {
        for index in 1..=count {
            seed(&db, index, None, None);
        }
        Ok(())
    })
    .unwrap();
    let (memories, batches) = observed(&db, CUTOFF);
    let expected: Vec<_> = (1..=count).map(id).collect();
    let actual: Vec<_> = memories.iter().map(|memory| memory.id.clone()).collect();
    assert_eq!(actual, expected);
    assert_eq!(batches.len(), 3);
    assert!(
        batches
            .iter()
            .all(|batch| batch.len() <= ASK_MEMORY_REVISION_PAGE_SIZE)
    );
    assert_eq!(batches.into_iter().flatten().collect::<Vec<_>>(), expected);
}

#[test]
fn a_late_eligible_source_is_not_lost_among_inactive_rows() {
    let (_root, db) = fixture();
    let last = ASK_MEMORY_REVISION_PAGE_SIZE * 2 + 1;
    db.with_transaction(|| {
        for index in 1..last {
            seed(&db, index, None, Some("2020-01-01T00:00:00Z"));
        }
        seed(&db, last, None, None);
        Ok(())
    })
    .unwrap();
    let (memories, batches) = observed(&db, CUTOFF);
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].id, id(last));
    assert_eq!(batches, vec![vec![id(last)]]);
}

#[test]
fn validity_is_inclusive_and_compares_instants_not_timestamp_spelling() {
    let (_root, db) = fixture();
    seed(
        &db,
        1,
        Some("2026-09-17T08:00:00-04:00"),
        Some("2026-09-17T14:00:00+02:00"),
    );
    for (reference, expected) in [
        ("2026-09-17T11:59:59.999999999Z", 0),
        (CUTOFF, 1),
        ("2026-09-17T12:00:00.000000001Z", 0),
    ] {
        let (memories, batches) = observed(&db, reference);
        assert_eq!(memories.len(), expected, "{reference}");
        assert_eq!(batches.len(), expected, "{reference}");
    }
}

#[test]
fn historical_reads_admit_pre_cutoff_revisions_but_never_closed_seals() {
    let (_root, db) = fixture();
    seed(&db, 1, None, Some("2099-01-01T00:00:00Z"));
    seed(&db, 2, None, None);
    assert!(
        db.restore_imported_memory_supersession(&id(1), CUTOFF)
            .unwrap()
    );
    db.insert_memory_seal(&id(2), &format!("blake3:{}", "a".repeat(64)), CUTOFF)
        .unwrap();
    let (prior, batches) = observed(&db, "2026-09-17T11:59:59Z");
    assert_eq!(prior.len(), 1);
    assert_eq!(batches, vec![vec![id(1)]]);
    let (current, batches) = observed(&db, CUTOFF);
    assert!(current.is_empty() && batches.is_empty());
}

#[test]
fn invalid_validity_on_hidden_sources_fails_closed_before_any_body_query() {
    for corrupt_bound in [
        "valid_from = 'PRIVATE-VALIDITY-CANARY'",
        "valid_from = '2030-01-01T00:00:00Z', valid_to = '2020-01-01T00:00:00Z'",
        "valid_from = '2020-01-01T00:00:00Z', valid_to = 'PRIVATE-VALIDITY-CANARY'",
    ] {
        let (_root, db) = fixture();
        seed(&db, 1, None, None);
        db.insert_memory_seal(&id(1), &format!("blake3:{}", "a".repeat(64)), CUTOFF)
            .unwrap();
        db.execute_raw(&format!("UPDATE memories SET {corrupt_bound}"))
            .unwrap();
        let mut body_queries = 0;
        let error = load_with_hydration_observer(&db, WORKSPACE, at(CUTOFF), |_| {
            body_queries += 1;
        })
        .unwrap_err();
        assert_eq!(body_queries, 0);
        assert!(matches!(error, DomainError::Storage { .. }));
        let diagnostic = format!("{error:?}");
        assert!(!diagnostic.contains("PRIVATE-VALIDITY-CANARY"));
        assert!(!diagnostic.contains(&id(1)));
        assert!(!diagnostic.contains(BODY));

        // The public caller must still release its owned snapshot on failure.
        assert!(load_current_ask_corpus(&db, WORKSPACE, at(CUTOFF)).is_err());
        db.begin_read_snapshot().expect("no leaked failed snapshot");
        db.commit_read_snapshot().unwrap();
    }
}

#[test]
fn invalid_supersession_is_not_hidden_by_expiry() {
    let (_root, db) = fixture();
    seed(&db, 1, None, Some("2020-01-01T00:00:00Z"));
    db.execute_raw("UPDATE memories SET superseded_at = 'PRIVATE-REVISION-CANARY'")
        .unwrap();
    let mut body_queries = 0;
    let error = load_with_hydration_observer(&db, WORKSPACE, at(CUTOFF), |_| {
        body_queries += 1;
    })
    .unwrap_err();
    assert_eq!(body_queries, 0);
    assert!(!format!("{error:?}").contains("PRIVATE-REVISION-CANARY"));
}

#[test]
fn concurrent_revision_change_cannot_mix_admission_metadata_with_a_new_body() {
    let (root, reader) = fixture();
    seed(&reader, 1, None, None);
    let writer = DbConnection::open_file(&root.path().join("ask.db")).unwrap();
    let snapshot = AskReadSnapshot::begin(&reader).unwrap();
    let memories = load_with_hydration_observer(&reader, WORKSPACE, at(CUTOFF), |page| {
        assert_eq!(page, &[id(1).as_str()]);
        writer
            .execute_raw(&format!(
                "UPDATE memories SET superseded_at = '{CUTOFF}', content = 'Never run cargo fmt before release.' WHERE id = '{}'",
                id(1)
            ))
            .expect("writer can commit while the read snapshot remains pinned");
    })
    .unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].content, BODY);
    snapshot.finish().unwrap();
    let (next, batches) = observed(&reader, CUTOFF);
    assert!(next.is_empty() && batches.is_empty());
}

#[test]
fn missing_timestamp_cells_fail_but_null_is_unbounded() {
    assert!(optional_timestamp(None).is_err());
    assert_eq!(optional_timestamp(Some(&Value::Null)).unwrap(), None);
    let text = Value::Text(CUTOFF.to_owned());
    assert_eq!(optional_timestamp(Some(&text)).unwrap(), Some(CUTOFF));
}
