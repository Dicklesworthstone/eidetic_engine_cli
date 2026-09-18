#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::core::ask::{AskRequest, ask_data_json, evaluate_ask, render_ask_markdown};
use crate::db::{
    CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, MemoryLinkRelation,
    MemoryLinkSource, StoredMemory,
};
use crate::models::WorkspaceId;

fn fixture() -> (tempfile::TempDir, DbConnection, String) {
    let root = tempfile::tempdir().unwrap();
    let connection = DbConnection::open_file(&root.path().join("ask.db")).unwrap();
    connection.migrate().unwrap();
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([43; 16])).to_string();
    connection
        .insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: root.path().to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
    (root, connection, workspace)
}

fn seed(connection: &DbConnection, workspace: &str, number: usize, body: &str) -> StoredMemory {
    let id = format!("mem_{number:026}");
    connection
        .insert_memory(
            &id,
            &CreateMemoryInput {
                workspace_id: workspace.to_owned(),
                content: body.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.5,
                importance: 0.5,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                provenance_uri: Some("manual://ask/privacy".to_owned()),
                tags: Vec::new(),
                valid_from: Some("2020-01-01T00:00:00Z".to_owned()),
                valid_to: None,
            },
        )
        .unwrap();
    connection.get_memory(&id).unwrap().unwrap()
}

fn answer(corpus: &AskCorpus, floor: f32) -> super::super::AskReport {
    evaluate_ask(
        &AskRequest {
            question: "Run cargo fmt before release".to_owned(),
            min_confidence: floor,
            contradictions: corpus.contradictions.clone(),
            ..AskRequest::default()
        },
        &corpus.candidates,
    )
}

#[test]
fn private_bodies_are_absent_from_answers_conflicts_and_abstention_hints() {
    let (_root, db, workspace) = fixture();
    let safe = seed(&db, &workspace, 1, "Run cargo fmt before release.");
    let private = seed(
        &db,
        &workspace,
        2,
        "Run cargo fmt before release using api_key=ask-private-canary.",
    );
    seed(
        &db,
        &workspace,
        3,
        crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT,
    );
    seed(
        &db,
        &workspace,
        4,
        "Release notes are in /home/private/operator/release.txt.",
    );
    db.insert_memory_link(
        "link_00000000000000000000000001",
        &CreateMemoryLinkInput {
            src_memory_id: safe.id.clone(),
            dst_memory_id: private.id.clone(),
            relation: MemoryLinkRelation::Contradicts,
            weight: 1.0,
            confidence: 1.0,
            directed: true,
            evidence_count: 1,
            last_reinforced_at: None,
            source: MemoryLinkSource::Human,
            created_by: None,
            metadata_json: None,
        },
    )
    .unwrap();
    let before = db.list_memories(&workspace, None, true).unwrap();
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert_eq!(corpus.candidates.len(), 1);
    assert_eq!(corpus.candidates[0].memory_id, safe.id);
    assert!(corpus.contradictions.is_empty());
    let accepted = answer(&corpus, 0.55);
    assert!(!accepted.abstained);
    assert_eq!(accepted.citations.len(), 1);
    assert_eq!(accepted.citations[0].text, safe.content);
    let abstained = answer(&corpus, 1.0);
    assert!(abstained.abstained);
    assert!(
        abstained
            .nearest_evidence
            .as_ref()
            .is_some_and(|items| !items.is_empty())
    );
    for report in [accepted, abstained] {
        for output in [
            ask_data_json(&report).to_string(),
            render_ask_markdown(&report),
        ] {
            for forbidden in [
                private.id.as_str(),
                "ask-private-canary",
                "/home/private",
                crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT,
            ] {
                assert!(
                    !output.contains(forbidden),
                    "private evidence escaped admission"
                );
            }
        }
    }
    let after = db.list_memories(&workspace, None, true).unwrap();
    assert_eq!(
        before, after,
        "admission must not redact durable source rows"
    );
}

#[test]
fn private_only_corpus_abstains_without_nearest_evidence() {
    let (_root, db, workspace) = fixture();
    seed(
        &db,
        &workspace,
        1,
        "Run cargo fmt with password=ask-only-secret.",
    );
    let corpus = load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap();
    assert!(corpus.candidates.is_empty());
    let report = answer(&corpus, 0.0);
    assert!(report.abstained);
    assert!(report.citations.is_empty());
    assert!(report.nearest_evidence.unwrap_or_default().is_empty());
}

#[test]
fn citation_metadata_is_sanitized_without_rewriting_the_body() {
    let (_root, db, workspace) = fixture();
    let mut memory = seed(&db, &workspace, 1, "Run cargo fmt before release.");
    memory.trust_class = "peer_human_attested".to_owned();
    memory.trust_subclass = Some(
        "agent:/home/private/member; project=/home/private/project; produced_at=/home/private/time"
            .to_owned(),
    );
    memory.provenance_uri = Some("file:///home/private/citation".to_owned());
    let admitted = admission::into_candidate(memory.clone()).unwrap();
    assert_eq!(admitted.content, memory.content);
    assert_eq!(
        admitted.provenance_uri,
        Some(format!("ee-mem://{}", memory.id))
    );
    let team = admitted.team_provenance.as_ref().unwrap();
    assert!(!team.member_display_name.contains("/home/private"));
    assert!(
        !team
            .project_name
            .as_deref()
            .unwrap()
            .contains("/home/private")
    );
    assert!(!team.produced_at.contains("/home/private"));
    let report = answer(
        &AskCorpus {
            candidates: vec![admitted],
            contradictions: vec![],
            native_sources: BTreeMap::new(),
        },
        0.0,
    );
    assert!(!ask_data_json(&report).to_string().contains("/home/private"));
    assert!(!render_ask_markdown(&report).contains("/home/private"));
}

#[test]
fn public_provenance_and_unicode_byte_offsets_are_preserved() {
    let (_root, db, workspace) = fixture();
    let memory = seed(
        &db,
        &workspace,
        1,
        "Café release notes. Run cargo fmt before release.",
    );
    let candidate = admission::into_candidate(memory.clone()).unwrap();
    assert_eq!(candidate.provenance_uri, memory.provenance_uri);
    let report = answer(
        &AskCorpus {
            candidates: vec![candidate],
            contradictions: vec![],
            native_sources: BTreeMap::new(),
        },
        0.55,
    );
    assert!(!report.abstained);
    for citation in report.citations {
        assert_eq!(
            memory.content.get(citation.byte_start..citation.byte_end),
            Some(citation.text.as_str())
        );
    }
}

#[test]
fn malformed_identity_or_vocabulary_never_becomes_public_evidence() {
    let (_root, db, workspace) = fixture();
    let memory = seed(&db, &workspace, 1, "Run cargo fmt before release.");
    for field in ["id", "kind", "level", "trust"] {
        let mut malformed = memory.clone();
        match field {
            "id" => malformed.id = "private-untyped-identity".to_owned(),
            "kind" => malformed.kind = "invalid/kind".to_owned(),
            "level" => malformed.level = "private-level".to_owned(),
            _ => malformed.trust_class = "private-trust".to_owned(),
        }
        assert!(admission::into_candidate(malformed).is_none(), "{field}");
    }
}

#[test]
fn invalid_or_absent_provenance_uses_the_real_memory_identity() {
    use crate::models::{MemoryId, ProvenanceUri};
    use std::str::FromStr;

    let (_root, db, workspace) = fixture();
    let memory = seed(&db, &workspace, 1, "Run cargo fmt before release.");
    for uri in [
        None,
        Some("not a provenance URI".to_owned()),
        Some("manual://source?password=canary".to_owned()),
    ] {
        let mut copy = memory.clone();
        copy.provenance_uri = uri;
        let actual = admission::into_candidate(copy)
            .unwrap()
            .provenance_uri
            .unwrap();
        assert_eq!(actual, format!("ee-mem://{}", memory.id));
        assert_eq!(
            ProvenanceUri::from_str(&actual).unwrap(),
            ProvenanceUri::EeMemory(MemoryId::from_str(&memory.id).unwrap())
        );
    }
}

#[test]
fn custom_kinds_remain_supported_but_cannot_smuggle_credentials() {
    let (_root, db, workspace) = fixture();
    let mut memory = seed(&db, &workspace, 1, "Run cargo fmt before release.");
    memory.kind = "project-release-check".to_owned();
    assert_eq!(
        admission::into_candidate(memory.clone()).unwrap().kind,
        "project-release-check"
    );
    memory.kind = "project-AKIAABCDEFGHIJKLMNOP".to_owned();
    assert!(admission::into_candidate(memory).is_none());
}

#[test]
fn uri_wrappers_cannot_hide_private_paths_in_bodies_or_citation_fields() {
    let (_root, db, workspace) = fixture();
    let memory = seed(&db, &workspace, 1, "Run cargo fmt before release.");
    for uri in [
        "file:///home/private/uri-canary.txt",
        "file:///ROOT/private/uri-canary.txt",
        "file:///Users/private/uri-canary.txt",
    ] {
        let mut body = memory.clone();
        body.content = format!("Run cargo fmt before release. See {uri}.");
        assert!(admission::into_candidate(body).is_none());
        let mut metadata = memory.clone();
        metadata.provenance_uri = Some(uri.to_owned());
        let admitted = admission::into_candidate(metadata).unwrap();
        assert_eq!(admitted.content, memory.content);
        assert_eq!(
            admitted.provenance_uri,
            Some(format!("ee-mem://{}", memory.id))
        );
    }
}

#[test]
fn file_targets_are_checked_portably_and_relative_citations_survive() {
    let (_root, db, workspace) = fixture();
    let memory = seed(&db, &workspace, 1, "Run cargo fmt before release.");
    for uri in [
        "file:///opt/company/internal-notes",
        "file://Q:/company/internal-notes",
        "file://Q:\\company\\internal-notes",
        "file://../internal-notes",
        "file://..\\internal-notes",
        "file://~/internal-notes",
    ] {
        let mut copy = memory.clone();
        copy.provenance_uri = Some(uri.to_owned());
        assert_eq!(
            admission::into_candidate(copy).unwrap().provenance_uri,
            Some(format!("ee-mem://{}", memory.id)),
            "{uri}"
        );
    }
    for uri in [
        "file://AGENTS.md#L42",
        "file://docs/release.md#L2-5",
        "manual://run/release-note",
        "https://example.test/release",
    ] {
        let mut copy = memory.clone();
        copy.provenance_uri = Some(uri.to_owned());
        assert_eq!(
            admission::into_candidate(copy)
                .unwrap()
                .provenance_uri
                .as_deref(),
            Some(uri)
        );
    }
}
