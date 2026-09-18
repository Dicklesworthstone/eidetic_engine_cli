//! Native-rule acceptance through the actual CLI, with no shadow memory rows.
use super::*;
use ee::db::CreateCurationCandidateInput;
use ee::search::RuleIndexProjection;
use std::path::Path;

const RULE: &str = "Run cargo fmt before every release tag.";
const QUESTION: &str = "Which command must run before every release tag?";

fn command(workspace: &Path, args: &[&str]) -> Result<Value, String> {
    let output = crate::common_spawn::serialized_real_ee_with(|cmd| {
        cmd.env_remove("EE_AGENT_NAME")
            .arg("--workspace")
            .arg(workspace)
            .arg("--json")
            .args(args);
    })
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "command {args:?} failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let response: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    assert_eq!(response["success"], true);
    Ok(response)
}

fn added_rule(workspace: &Path, body: &str, flags: &[&str]) -> Result<String, String> {
    let mut args = vec!["rule", "add", body, "--confidence", "0.95"];
    args.extend_from_slice(flags);
    command(workspace, &args)?["data"]["ruleId"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "missing native rule ID".to_owned())
}

fn check_citation(value: &Value, id: &str, body: &str) -> Result<(), String> {
    assert_eq!(value["entityKind"], "rule");
    assert_eq!(value["entityId"], id);
    assert_eq!(value["ruleId"], id);
    assert_eq!(value["provenanceUri"], format!("ee://rule/{id}"));
    assert!(value.get("memoryId").is_none(), "a rule is not a memory");
    let start = value["span"]["byteStart"]
        .as_u64()
        .ok_or("missing byte start")? as usize;
    let end = value["span"]["byteEnd"]
        .as_u64()
        .ok_or("missing byte end")? as usize;
    assert_eq!(body.get(start..end), value["text"].as_str());
    assert!(
        value["entityRevision"]
            .as_str()
            .ok_or("missing revision")?
            .starts_with("blake3:")
    );
    Ok(())
}

#[test]
fn native_rule_add_update_retire_is_immediately_visible_to_read_only_ask() -> Result<(), String> {
    let (_root, workspace, database) = super::super::build_empty_workspace()?;
    let id = added_rule(&workspace, RULE, &[])?;
    let db = DbConnection::open_file_read_only(&database).map_err(|error| error.to_string())?;
    let row = db
        .get_procedural_rule(&id)
        .map_err(|error| error.to_string())?
        .ok_or("missing rule")?;
    let workspace_id = row.workspace_id.clone();
    let projection = RuleIndexProjection::new(row, &workspace, vec![], vec![]);
    let audits_before = db
        .list_audit_entries(Some(&workspace_id), None)
        .map_err(|error| error.to_string())?;
    drop(db);
    let first = command(&workspace, &["ask", QUESTION, "--read-only"])?;
    assert_eq!(first["data"]["abstained"], false);
    assert_eq!(first["data"]["candidatesScanned"], 1);
    check_citation(&first["data"]["citations"][0], &id, RULE)?;
    assert_eq!(
        first["data"]["citations"][0]["entityRevision"],
        projection.entity_revision()
    );
    assert_eq!(
        first,
        command(&workspace, &["ask", QUESTION, "--read-only"])?
    );
    let db = DbConnection::open_file_read_only(&database).map_err(|error| error.to_string())?;
    assert!(
        db.list_memories(&workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    assert_eq!(
        audits_before,
        db.list_audit_entries(Some(&workspace_id), None)
            .map_err(|error| error.to_string())?
    );
    drop(db);
    let replacement = "Run cargo clippy before every release tag.";
    command(
        &workspace,
        &["rule", "update", &id, "--content", replacement],
    )?;
    let updated = command(&workspace, &["ask", QUESTION, "--read-only"])?;
    assert_eq!(updated["data"]["abstained"], false);
    check_citation(&updated["data"]["citations"][0], &id, replacement)?;
    assert_ne!(
        updated["data"]["citations"][0]["entityRevision"],
        first["data"]["citations"][0]["entityRevision"]
    );
    command(&workspace, &["rule", "mark", &id, "--trigger", "deprecate"])?;
    let retired = command(&workspace, &["ask", QUESTION, "--read-only"])?;
    assert_eq!(retired["data"]["abstained"], true);
    assert_eq!(retired["data"]["candidatesScanned"], 0);
    assert!(!retired.to_string().contains(&id));
    Ok(())
}

#[test]
fn applied_curation_rule_answers_from_its_own_body_and_records_its_own_target() -> Result<(), String>
{
    let (_root, workspace, database) = super::super::build_empty_workspace()?;
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let workspace_id = db
        .get_workspace_by_path(&workspace.to_string_lossy())
        .map_err(|error| error.to_string())?
        .ok_or("missing workspace")?
        .id;
    let memory_id = seed(
        &db,
        &workspace_id,
        1,
        "A historical observation recorded during an earlier run.",
        "2000-01-01T00:00:00Z",
        None,
    )?;
    let candidate_id = "curate_00000000000000000000000017";
    db.insert_curation_candidate(
        candidate_id,
        &CreateCurationCandidateInput {
            workspace_id: workspace_id.clone(),
            candidate_type: "rule".to_owned(),
            target_memory_id: Some(memory_id.clone()),
            proposed_content: Some(RULE.to_owned()),
            proposed_confidence: Some(0.95),
            proposed_trust_class: Some("human_explicit".to_owned()),
            source_type: "feedback_event".to_owned(),
            source_id: None,
            reason: "Approved release guidance from reviewed historical evidence.".to_owned(),
            confidence: 0.95,
            status: Some("approved".to_owned()),
            created_at: Some("2020-01-01T00:00:00Z".to_owned()),
            ttl_expires_at: None,
            derivation_source_refs_json: None,
            derivation_metadata_json: None,
        },
    )
    .map_err(|error| error.to_string())?;
    drop(db);
    let applied = command(
        &workspace,
        &[
            "curate",
            "apply",
            candidate_id,
            "--actor",
            "native-ask-acceptance",
        ],
    )?;
    assert_eq!(applied["data"]["application"]["decision"], "create_rule");
    let id = applied["data"]["application"]["changes"]
        .as_array()
        .ok_or("missing apply changes")?
        .iter()
        .find(|change| change["field"] == "ruleId")
        .and_then(|change| change["after"].as_str())
        .ok_or("missing applied rule ID")?
        .to_owned();
    let answer = command(&workspace, &["ask", QUESTION])?;
    assert_eq!(answer["data"]["abstained"], false);
    check_citation(&answer["data"]["citations"][0], &id, RULE)?;
    assert!(
        !answer.to_string().contains(&memory_id),
        "source memory must not replace rule identity"
    );
    let db = DbConnection::open_file_read_only(&database).map_err(|error| error.to_string())?;
    let audits = db
        .list_audit_by_target("rule", &id, None)
        .map_err(|error| error.to_string())?;
    let reads: Vec<_> = audits
        .iter()
        .filter(|entry| entry.action == ee::db::audit_actions::SEARCH_RETURNED_MEM)
        .collect();
    assert_eq!(reads.len(), 1);
    let details: Value =
        serde_json::from_str(reads[0].details.as_deref().ok_or("missing audit details")?)
            .map_err(|error| error.to_string())?;
    assert_eq!(details["ruleId"], id);
    assert_eq!(
        details["entityRevision"],
        answer["data"]["citations"][0]["entityRevision"]
    );
    assert!(
        db.list_audit_by_target("memory", &id, None)
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    assert_eq!(
        db.list_memories(&workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn native_draft_and_directory_rules_cannot_become_universal_answers() -> Result<(), String> {
    let (_root, workspace, _database) = super::super::build_empty_workspace()?;
    let draft = added_rule(&workspace, RULE, &["--maturity", "draft"])?;
    let narrow = added_rule(
        &workspace,
        RULE,
        &["--scope", "directory", "--scope-pattern", "src"],
    )?;
    let unscoped = command(&workspace, &["ask", QUESTION, "--read-only"])?;
    assert_eq!(unscoped["data"]["abstained"], true);
    assert_eq!(unscoped["data"]["candidatesScanned"], 0);
    assert!(!unscoped.to_string().contains(&draft));
    assert!(!unscoped.to_string().contains(&narrow));
    let broad = added_rule(&workspace, RULE, &["--scope", "global"])?;
    let global = command(
        &workspace,
        &["ask", QUESTION, "--memory-scope", "global", "--read-only"],
    )?;
    assert_eq!(global["data"]["abstained"], false);
    check_citation(&global["data"]["citations"][0], &broad, RULE)?;
    let no_actor = command(
        &workspace,
        &["ask", QUESTION, "--memory-scope", "self", "--read-only"],
    )?;
    assert_eq!(no_actor["data"]["abstained"], true);
    assert!(!no_actor.to_string().contains(&broad));
    Ok(())
}

#[test]
fn native_conflict_and_weak_evidence_keep_kind_and_revision_at_every_boundary() -> Result<(), String>
{
    let (_root, workspace, _database) = super::super::build_empty_workspace()?;
    let yes = added_rule(&workspace, RULE, &[])?;
    let weak = command(
        &workspace,
        &["ask", "release", "--min-confidence", "1", "--read-only"],
    )?;
    assert_eq!(weak["data"]["abstained"], true);
    assert_eq!(weak["data"]["nearestEvidence"][0]["ruleId"], yes);
    assert_eq!(
        weak["data"]["queryAssist"]["didYouMean"][0]["entityKind"],
        "rule"
    );
    assert_eq!(
        weak["data"]["queryAssist"]["reformulations"][0]["matchedRuleId"],
        yes
    );
    assert!(!weak.to_string().contains("memoryId"));
    let opposite = "Do not run cargo fmt before every release tag.";
    let no = added_rule(&workspace, opposite, &[])?;
    let conflict = command(&workspace, &["ask", QUESTION, "--read-only"])?;
    assert_eq!(conflict["data"]["_conflictDetected"], true);
    assert!(conflict["data"]["answerText"].is_null());
    let sides = conflict["data"]["sides"]
        .as_array()
        .ok_or("missing conflict sides")?;
    assert_eq!(sides.len(), 2);
    for side in sides {
        let citation = &side["citations"][0];
        let id = citation["ruleId"]
            .as_str()
            .ok_or("missing rule-side identity")?;
        assert!(id == yes || id == no);
        check_citation(citation, id, if id == yes { RULE } else { opposite })?;
    }
    Ok(())
}
