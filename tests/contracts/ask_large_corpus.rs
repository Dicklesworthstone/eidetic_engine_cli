//! Public-CLI regression: neither endpoint of the decisive contradiction is
//! among the old database-order prefix. These tests use the real store/binary.

use ee::core::ask::ASK_CANDIDATE_SCAN_CAP;
use ee::db::{
    CreateMemoryInput, CreateMemoryLinkInput, DbConnection, MemoryLinkRelation, MemoryLinkSource,
};
use serde_json::Value;

#[test]
fn ask_discloses_late_counterevidence_and_keeps_the_confidence_gate() -> Result<(), String> {
    let (_root, workspace, database) = super::build_empty_workspace()?;
    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let workspace_id = connection
        .get_workspace_by_path(&workspace.to_string_lossy())
        .map_err(|error| error.to_string())?
        .ok_or("initialized workspace row missing")?
        .id;
    let count = ASK_CANDIDATE_SCAN_CAP + 8;
    for index in 0..count {
        connection
            .insert_memory(
                &format!("mem_{index:026}"),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    // Disjoint content terms make only the selected anchor a
                    // direct hit. The other endpoint needs its explicit edge.
                    content: format!("Deployment_token_{index}."),
                    workflow_id: None,
                    confidence: 1.0,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some(format!("manual://ask-corpus/{index}")),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("seed memory {index}: {error}"))?;
    }
    // Choose endpoints from the actual storage order, rather than assuming
    // whether a backend sorts timestamps/IDs ascending or descending.
    let stored = connection
        .list_memories(&workspace_id, None, false)
        .map_err(|error| error.to_string())?;
    assert_eq!(stored.len(), count);
    let anchor = &stored[ASK_CANDIDATE_SCAN_CAP];
    let opposite = &stored[ASK_CANDIDATE_SCAN_CAP + 1];
    assert!(
        stored[..ASK_CANDIDATE_SCAN_CAP]
            .iter()
            .all(|memory| { memory.id != anchor.id && memory.id != opposite.id })
    );
    let link_id = "link_00000000000000000000000001";
    connection
        .insert_memory_link(
            link_id,
            &CreateMemoryLinkInput {
                src_memory_id: anchor.id.clone(),
                dst_memory_id: opposite.id.clone(),
                relation: MemoryLinkRelation::Contradicts,
                weight: 1.0,
                confidence: 0.9,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Agent,
                created_by: Some("ask-large-corpus".to_owned()),
                metadata_json: None,
            },
        )
        .map_err(|error| format!("seed contradiction: {error}"))?;
    let anchor = anchor.clone();
    let opposite = opposite.clone();
    drop(connection);

    let run = |question: &str, require: bool| {
        crate::common_spawn::serialized_real_ee_with(|command| {
            command
                .arg("--workspace")
                .arg(&workspace)
                .arg("ask")
                .arg(question)
                .arg("--json");
            if require {
                command.arg("--require-confidence").arg("0.9");
            }
        })
        .map_err(|error| format!("spawn ee ask: {error}"))
    };
    let output = run(&anchor.content, false)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("ask stdout must be exactly one JSON response: {error}"))?;
    assert_eq!(response["schema"], ee::models::RESPONSE_SCHEMA_V2);
    assert_eq!(response["success"], true);
    let data = &response["data"];
    assert_eq!(data["abstained"], false);
    assert_eq!(data["_conflictDetected"], true);
    assert_eq!(data["conflictLink"]["id"], link_id);
    assert_eq!(data["candidatesScanned"].as_u64(), Some(count as u64));
    assert!(data["answerText"].is_null());
    assert!(
        data["citations"]
            .as_array()
            .expect("citations array")
            .is_empty()
    );
    let sides = data["sides"].as_array().expect("both conflict sides");
    assert_eq!(sides.len(), 2);
    for (side, source) in sides.iter().zip([&anchor, &opposite]) {
        let citations = side["citations"].as_array().expect("side citations");
        assert_eq!(citations.len(), 1);
        let citation = &citations[0];
        assert_eq!(citation["memoryId"], source.id);
        assert_eq!(
            citation["provenanceUri"].as_str(),
            source.provenance_uri.as_deref()
        );
        let start = citation["span"]["byteStart"].as_u64().expect("byte start") as usize;
        let end = citation["span"]["byteEnd"].as_u64().expect("byte end") as usize;
        assert_eq!(source.content.get(start..end), citation["text"].as_str());
    }
    let repeated = run(&anchor.content, false)?;
    assert!(repeated.status.success());
    let repeated: Value =
        serde_json::from_slice(&repeated.stdout).map_err(|error| error.to_string())?;
    assert_eq!(
        repeated["data"], *data,
        "audit writes must not change answer bytes"
    );

    let unrelated = run("orbital veterinary anesthesia", false)?;
    assert!(unrelated.status.success());
    let unrelated: Value =
        serde_json::from_slice(&unrelated.stdout).map_err(|error| error.to_string())?;
    assert_eq!(unrelated["data"]["abstained"], true);
    assert!(unrelated["data"]["sides"].is_null());
    assert!(
        unrelated["data"]["citations"]
            .as_array()
            .expect("citations")
            .is_empty()
    );

    let strict = run(&anchor.content, true)?;
    assert_eq!(
        strict.status.code(),
        Some(6),
        "supported conflict must still fail a 0.9 confidence requirement"
    );
    let strict: Value =
        serde_json::from_slice(&strict.stdout).map_err(|error| error.to_string())?;
    assert_eq!(strict["success"], false);
    Ok(())
}
