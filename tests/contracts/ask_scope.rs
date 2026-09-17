//! Public CLI coverage for scoped, privacy-safe extractive answers.
use super::*;
use ee::db::InsertTeamMemberInput;
use std::path::Path;

fn scoped(
    workspace: &Path,
    scope: &str,
    actor: Option<&str>,
    question: &str,
) -> Result<Value, String> {
    let output = crate::common_spawn::serialized_real_ee_with(|command| {
        command.env_remove("EE_AGENT_NAME");
        if let Some(actor) = actor {
            command.env("EE_AGENT_NAME", actor);
        }
        command
            .arg("--json")
            .arg("--workspace")
            .arg(workspace)
            .arg("ask")
            .arg(question)
            .arg("--memory-scope")
            .arg(scope);
    })
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "scoped ask failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let response: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    assert_eq!(response["success"], true);
    Ok(response["data"].clone())
}

fn seed_scope(
    db: &DbConnection,
    workspace: &str,
    number: usize,
    actor: &str,
    trust: &str,
    tags: &[&str],
) -> Result<String, String> {
    let id = format!("mem_{number:026}");
    db.insert_memory(
        &id,
        &CreateMemoryInput {
            workspace_id: workspace.to_owned(),
            content: "Run cargo fmt before release.".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: Some(format!("manual://ask-scope-cli/{number}")),
            trust_class: trust.to_owned(),
            trust_subclass: Some(format!("agent:{actor}")),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            valid_from: Some("2000-01-01T00:00:00Z".to_owned()),
            valid_to: None,
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(id)
}

#[test]
fn public_ask_routes_all_memory_scopes_before_evidence_selection() -> Result<(), String> {
    let (_root, workspace, database) = super::super::build_empty_workspace()?;
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let workspace_id = db
        .get_workspace_by_path(&workspace.to_string_lossy())
        .map_err(|error| error.to_string())?
        .ok_or("missing workspace")?
        .id;
    let alice = seed_scope(&db, &workspace_id, 1, "Alice", "human_explicit", &[])?;
    let bob = seed_scope(&db, &workspace_id, 2, "Bob", "human_explicit", &["global"])?;
    let assertion = seed_scope(&db, &workspace_id, 3, "Alice", "agent_assertion", &[])?;
    let outsider = seed_scope(&db, &workspace_id, 4, "Outsider", "agent_assertion", &[])?;
    db.insert_team_member(&InsertTeamMemberInput {
        member_id: format!("mbr_{:032x}", 1),
        team_id: "team_ask".to_owned(),
        workspace_id: workspace_id.clone(),
        display_name: "Bob".to_owned(),
        state: "active".to_owned(),
        is_self: false,
        origin_node_id: "node_Bob".to_owned(),
        bound_via: "invite_ceremony".to_owned(),
        joined_at: "2020-01-01T00:00:00Z".to_owned(),
    })
    .map_err(|error| error.to_string())?;
    drop(db);
    let question = "Run cargo fmt before release";
    for (scope, expected, forbidden) in [
        ("self", 2, vec![bob.as_str(), outsider.as_str()]),
        ("team", 3, vec![outsider.as_str()]),
        ("verified", 2, vec![assertion.as_str(), outsider.as_str()]),
        (
            "global",
            1,
            vec![alice.as_str(), assertion.as_str(), outsider.as_str()],
        ),
        ("workspace", 4, vec![]),
        ("swarm", 4, vec![]),
    ] {
        let data = scoped(&workspace, scope, Some("Alice"), question)?;
        assert_eq!(data["abstained"], false, "{scope}");
        assert_eq!(data["candidatesScanned"], expected, "{scope}");
        assert!(!data["citations"].as_array().ok_or("citations")?.is_empty());
        for id in forbidden {
            assert!(
                !data.to_string().contains(id),
                "out-of-scope evidence leaked for {scope}"
            );
        }
        assert_eq!(data, scoped(&workspace, scope, Some("Alice"), question)?);
    }
    let no_actor = scoped(&workspace, "self", None, question)?;
    assert_eq!(no_actor["candidatesScanned"], 0);
    assert_eq!(no_actor["abstained"], true);
    assert!(
        no_actor["citations"]
            .as_array()
            .ok_or("citations")?
            .is_empty()
    );
    Ok(())
}

#[test]
fn public_ask_withholds_private_bodies_and_citation_metadata() -> Result<(), String> {
    let (_root, workspace, database) = super::super::build_empty_workspace()?;
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let workspace_id = db
        .get_workspace_by_path(&workspace.to_string_lossy())
        .map_err(|error| error.to_string())?
        .ok_or("missing workspace")?
        .id;
    let private = seed(
        &db,
        &workspace_id,
        1,
        "Run cargo fmt before release using password=ask-cli-canary.",
        "2000-01-01T00:00:00Z",
        None,
    )?;
    let safe = seed(
        &db,
        &workspace_id,
        2,
        "Run cargo fmt before release.",
        "2000-01-01T00:00:00Z",
        None,
    )?;
    db.execute_raw(&format!("UPDATE memories SET provenance_uri = 'file:///home/private/release-source' WHERE id = '{safe}'"))
        .map_err(|error| error.to_string())?;
    drop(db);
    for question in ["Run cargo fmt before release", "What is the database port?"] {
        let response = ask(&workspace, question)?;
        let rendered = response.to_string();
        assert!(!rendered.contains("ask-cli-canary"));
        assert!(!rendered.contains("/home/private"));
        assert!(!rendered.contains(&private));
        assert_eq!(response["data"]["candidatesScanned"], 1);
    }
    let response = ask(&workspace, "Run cargo fmt before release")?;
    assert_eq!(response["data"]["citations"][0]["memoryId"], safe);
    assert_eq!(
        response["data"]["citations"][0]["provenanceUri"],
        format!("ee://memory/{safe}")
    );
    Ok(())
}

#[test]
fn public_ask_rejects_invalid_scope_at_the_argument_boundary() -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let output = crate::common_spawn::serialized_real_ee_with(|command| {
        command
            .arg("--workspace")
            .arg(root.path())
            .arg("ask")
            .arg("release")
            .arg("--memory-scope")
            .arg("unknown-scope");
    })
    .map_err(|error| error.to_string())?;
    assert!(!output.status.success());
    assert!(!root.path().join(".ee").exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("scope"));
    Ok(())
}
