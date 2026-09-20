//! Real CLI/store numeric-dispute contracts, including a fail-closed consumer.
use ee::db::DbConnection;
use serde_json::Value;

const FIRST: &str = "The production database service port is 5432.";
const SECOND: &str = "The production database service port is 6432.";
const QUESTION: &str = "What is the production database service port?";

fn run(workspace: &std::path::Path, require: Option<&str>) -> Result<(Option<i32>, Value), String> {
    let output = crate::common_spawn::serialized_real_ee_with(|command| {
        command.env("EE_EMBED_DOWNLOAD", "off")
            .env("EE_EMBED_MODEL_DIR", workspace.join("numeric-model-absent"))
            .arg("--json").arg("--workspace").arg(workspace)
            .arg("ask").arg(QUESTION).arg("--read-only");
        if let Some(minimum) = require {
            command.arg("--require-confidence").arg(minimum);
        }
    }).map_err(|error| error.to_string())?;
    let response: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!("numeric ask JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
    })?;
    eprintln!("numeric ask exit={:?}, response={response}", output.status.code());
    Ok((output.status.code(), response))
}

fn populate(workspace: &std::path::Path, database: &std::path::Path, second: &str, expires: Option<&str>) -> Result<[String; 2], String> {
    let db = DbConnection::open_file(database).map_err(|error| error.to_string())?;
    let workspace_id = db.get_workspace_by_path(&workspace.to_string_lossy())
        .map_err(|error| error.to_string())?.ok_or("missing workspace")?.id;
    Ok([
        super::seed(&db, &workspace_id, 1, FIRST, "2000-01-01T00:00:00Z", None)?,
        super::seed(&db, &workspace_id, 2, second, "2000-01-01T00:00:00Z", expires)?,
    ])
}

#[test]
fn public_numeric_dispute_preserves_both_citations_and_is_repeatable() -> Result<(), String> {
    let (_root, workspace, database) = super::super::build_empty_workspace()?;
    let ids = populate(&workspace, &database, SECOND, None)?;
    let (exit, response) = run(&workspace, None)?;
    assert_eq!(exit, Some(0));
    assert_eq!(response["success"], true);
    let data = &response["data"];
    assert_eq!(data["abstained"], false);
    let sides = data["sides"].as_array().ok_or("missing conflict sides")?;
    assert_eq!(sides.len(), 2);
    assert_eq!(sides[0]["label"], "query_match");
    assert_eq!(sides[1]["label"], "numeric_alternative");
    for (index, text) in [FIRST, SECOND].iter().enumerate() {
        let citations = sides[index]["citations"].as_array().ok_or("missing side citations")?;
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0]["memoryId"], ids[index]);
        assert_eq!(citations[0]["text"], *text);
        assert_eq!(citations[0]["provenanceUri"], format!("manual://ask-lifecycle-cli/{}", index + 1));
    }
    assert!(response["degraded"].as_array().ok_or("missing degradations")?
        .iter().any(|row| row["code"] == "ask_conflicting_evidence"));
    let (again_exit, again) = run(&workspace, None)?;
    assert_eq!(again_exit, Some(0));
    assert_eq!(again["data"], *data);
    Ok(())
}

#[test]
fn public_numeric_dispute_enforces_required_confidence() -> Result<(), String> {
    let (_root, workspace, database) = super::super::build_empty_workspace()?;
    populate(&workspace, &database, SECOND, None)?;
    // Prove that this is a supported answer first, not a generic empty-corpus
    // failure that happens to use the same nonzero exit code.
    let (normal_exit, normal) = run(&workspace, None)?;
    assert_eq!(normal_exit, Some(0));
    assert_eq!(normal["data"]["sides"].as_array().ok_or("missing sides")?.len(), 2);
    let (required_exit, required) = run(&workspace, Some("0.99"))?;
    assert_eq!(required_exit, Some(6), "conflict penalty must reach the fail-closed CLI gate: {required}");
    assert!(required.is_object(), "the gate must retain structured JSON");
    Ok(())
}

#[test]
fn public_numeric_opposition_cannot_resurrect_expired_evidence() -> Result<(), String> {
    let (_root, workspace, database) = super::super::build_empty_workspace()?;
    let ids = populate(&workspace, &database, SECOND, Some("2001-01-01T00:00:00Z"))?;
    let (exit, response) = run(&workspace, None)?;
    assert_eq!(exit, Some(0));
    let data = &response["data"];
    assert_eq!(data["abstained"], false);
    assert!(data["sides"].is_null());
    let citations = data["citations"].as_array().ok_or("missing citations")?;
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0]["memoryId"], ids[0]);
    assert_eq!(citations[0]["text"], FIRST);
    assert!(!data.to_string().contains(&ids[1]));
    assert!(!data.to_string().contains("6432"));
    Ok(())
}

#[test]
fn public_equal_settings_still_answer_without_a_conflict() -> Result<(), String> {
    let (_root, workspace, database) = super::super::build_empty_workspace()?;
    populate(&workspace, &database, FIRST, None)?;
    let (exit, response) = run(&workspace, None)?;
    assert_eq!(exit, Some(0));
    let data = &response["data"];
    assert_eq!(data["abstained"], false);
    assert!(data["sides"].is_null());
    let citations = data["citations"].as_array().ok_or("citations missing")?;
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0]["text"], FIRST);
    assert!(!response["degraded"].as_array().ok_or("missing degradations")?
        .iter().any(|row| row["code"] == "ask_conflicting_evidence"));
    Ok(())
}
