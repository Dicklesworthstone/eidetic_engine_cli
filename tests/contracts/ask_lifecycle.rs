//! Real CLI coverage for current-memory admission, not only the pure scorer.

use ee::db::{
    CreateMemoryInput, CreateMemoryLinkInput, DbConnection, MemoryLinkRelation, MemoryLinkSource,
};
use serde_json::Value;

fn seed(
    connection: &DbConnection,
    workspace_id: &str,
    number: usize,
    content: &str,
    from: &str,
    to: Option<&str>,
) -> Result<String, String> {
    let id = format!("mem_{number:026}");
    connection
        .insert_memory(
            &id,
            &CreateMemoryInput {
                workspace_id: workspace_id.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: content.to_owned(),
                workflow_id: None,
                confidence: 1.0,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some(format!("manual://ask-lifecycle-cli/{number}")),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: Some(from.to_owned()),
                valid_to: to.map(str::to_owned),
            },
        )
        .map_err(|error| format!("seed memory: {error}"))?;
    Ok(id)
}

fn ask(workspace: &std::path::Path, question: &str) -> Result<Value, String> {
    let output = crate::common_spawn::serialized_real_ee_with(|command| {
        command
            .arg("--json")
            .arg("--workspace")
            .arg(workspace)
            .arg("ask")
            .arg(question);
    })
    .map_err(|error| format!("spawn ask: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ask failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let response: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout must contain one JSON response: {error}"))?;
    assert_eq!(response["schema"], ee::models::RESPONSE_SCHEMA_V2);
    assert_eq!(response["success"], true);
    Ok(response)
}

#[test]
fn public_ask_excludes_expired_and_future_rules_before_conflict_lookup() -> Result<(), String> {
    let (_root, workspace, database) = super::build_empty_workspace()?;
    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let workspace_id = connection
        .get_workspace_by_path(&workspace.to_string_lossy())
        .map_err(|error| error.to_string())?
        .ok_or("missing workspace")?
        .id;
    let active = seed(
        &connection,
        &workspace_id,
        1,
        "Run cargo fmt before release.",
        "2000-01-01T00:00:00Z",
        None,
    )?;
    let expired = seed(
        &connection,
        &workspace_id,
        2,
        "Never run cargo fmt before release.",
        "2000-01-01T00:00:00Z",
        Some("2001-01-01T00:00:00Z"),
    )?;
    let future = seed(
        &connection,
        &workspace_id,
        3,
        "Never run cargo fmt before release.",
        "2999-01-01T00:00:00Z",
        None,
    )?;
    connection
        .insert_memory_link(
            "link_00000000000000000000000001",
            &CreateMemoryLinkInput {
                src_memory_id: active.clone(),
                dst_memory_id: expired.clone(),
                relation: MemoryLinkRelation::Contradicts,
                weight: 1.0,
                confidence: 0.9,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Human,
                created_by: None,
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    drop(connection);

    let response = ask(&workspace, "Run cargo fmt before release")?;
    let data = &response["data"];
    assert_eq!(data["abstained"], false);
    assert!(data["sides"].is_null());
    let citations = data["citations"]
        .as_array()
        .ok_or("citations array missing")?;
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0]["memoryId"], active);
    assert_eq!(citations[0]["text"], "Run cargo fmt before release.");
    assert_eq!(
        citations[0]["provenanceUri"],
        "manual://ask-lifecycle-cli/1"
    );
    let rendered = data.to_string();
    assert!(!rendered.contains(&expired));
    assert!(!rendered.contains(&future));
    assert!(!rendered.contains("Never run"));
    let repeated = ask(&workspace, "Run cargo fmt before release")?;
    assert_eq!(data, &repeated["data"]);
    Ok(())
}

#[test]
fn public_ask_does_not_reveal_retired_advice_in_abstention_hints() -> Result<(), String> {
    let (_root, workspace, database) = super::build_empty_workspace()?;
    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let workspace_id = connection
        .get_workspace_by_path(&workspace.to_string_lossy())
        .map_err(|error| error.to_string())?
        .ok_or("missing workspace")?
        .id;
    let expired = seed(
        &connection,
        &workspace_id,
        1,
        "The historic database requires port 15432.",
        "2000-01-01T00:00:00Z",
        Some("2001-01-01T00:00:00Z"),
    )?;
    let current = seed(
        &connection,
        &workspace_id,
        2,
        "Format Rust source before delivery.",
        "2000-01-01T00:00:00Z",
        None,
    )?;
    drop(connection);

    let response = ask(&workspace, "What port does the database require?")?;
    let data = &response["data"];
    assert_eq!(data["abstained"], true);
    assert!(
        data["citations"]
            .as_array()
            .ok_or("citations array missing")?
            .is_empty()
    );
    let nearest = data["nearestEvidence"]
        .as_array()
        .ok_or("nearest evidence missing")?;
    assert_eq!(nearest.len(), 1);
    assert_eq!(nearest[0]["memoryId"], current);
    let rendered = data.to_string();
    assert!(!rendered.contains(&expired));
    assert!(!rendered.contains("historic database"));
    assert!(!rendered.contains("15432"));
    Ok(())
}

#[path = "ask_scope.rs"]
mod scope;
