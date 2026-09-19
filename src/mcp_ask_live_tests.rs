//! Actual JSON-RPC -> CLI -> migrated-store coverage for MCP ask constraints.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use serde_json::{Value, json};

use crate::db::{CreateProceduralRuleInput, CreateWorkspaceInput, DbConnection};
use crate::models::WorkspaceId;

const BODY: &str = "Run cargo fmt before every release tag.";
const QUESTION: &str = "Which command must run before every release tag?";

fn fixture() -> (tempfile::TempDir, DbConnection, String) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".ee")).unwrap();
    let db = DbConnection::open_file(&root.path().join(".ee/ee.db")).unwrap();
    db.migrate().unwrap();
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_u128(99123)).to_string();
    db.insert_workspace(&workspace, &CreateWorkspaceInput {
        path: root.path().to_string_lossy().into_owned(), name: None,
    }).unwrap();
    (root, db, workspace)
}

fn rule(db: &DbConnection, workspace: &str, n: usize, scope: &str, pattern: Option<&str>, trust: &str) -> String {
    let id = format!("rule_{n:026}");
    db.insert_procedural_rule(&id, &CreateProceduralRuleInput {
        workspace_id: workspace.to_owned(), content: BODY.to_owned(),
        confidence: 0.95, utility: 0.5, importance: 0.5,
        trust_class: trust.to_owned(), scope: scope.to_owned(),
        scope_pattern: pattern.map(str::to_owned), maturity: "candidate".to_owned(),
        protected: false, source_memory_ids: Vec::new(), tags: Vec::new(),
    }).unwrap();
    id
}

fn ask(workspace: &Path, extra: Value) -> Value {
    let mut arguments = json!({"workspace": workspace.to_string_lossy(), "question": QUESTION});
    for (key, value) in extra.as_object().unwrap() {
        arguments[key] = value.clone();
    }
    crate::mcp::handle_json_rpc_message(&json!({
        "jsonrpc":"2.0", "id":1, "method":"tools/call",
        "params":{"name":"ee_ask", "arguments":arguments},
    })).expect("a JSON-RPC request with an ID must receive a response")
}

fn payload(response: &Value) -> Value {
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], false, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn denied(response: &Value) {
    assert!(response.get("error").is_some() || response["result"]["isError"] == true, "{response}");
    assert!(!response.to_string().contains("private-canary"), "errors must not echo private constraint values");
}

#[test]
fn registry_advertises_constraints_and_truthful_read_only_behavior() {
    let response = crate::mcp::handle_json_rpc_message(&json!({
        "jsonrpc":"2.0", "id":2, "method":"tools/list",
    })).unwrap();
    let tool = response["result"]["tools"].as_array().unwrap().iter()
        .find(|tool| tool["name"] == "ee_ask").unwrap();
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    assert_eq!(tool["inputSchema"]["properties"]["readOnly"]["const"], true);
    assert!(tool["inputSchema"]["properties"]["memoryScope"]["enum"].as_array().unwrap().contains(&json!("verified")));
    assert!(tool["inputSchema"]["properties"]["paths"].is_object());
}

#[test]
fn native_path_answers_keep_exact_identity_and_never_write_the_store() {
    let (root, db, workspace) = fixture();
    let src = rule(&db, &workspace, 1, "directory", Some("src"), "human_explicit");
    let tests = rule(&db, &workspace, 2, "directory", Some("tests"), "human_explicit");
    let comma = rule(&db, &workspace, 3, "file_pattern", Some("notes/a,b.rs"), "human_explicit");
    let before = std::fs::read(root.path().join(".ee/ee.db")).unwrap();
    let audits = db.list_audit_entries(Some(&workspace), None).unwrap();
    for (path, expected) in [("src/new.rs", &src), ("tests/check.rs", &tests), ("notes/a,b.rs", &comma)] {
        let value = payload(&ask(root.path(), json!({"path":path})));
        assert_eq!(value["data"]["abstained"], false, "{value}");
        assert_eq!(value["data"]["candidatesScanned"], 1);
        let citation = &value["data"]["citations"][0];
        assert_eq!(citation["ruleId"], expected.as_str());
        assert_eq!(citation["entityKind"], "rule");
        assert!(citation.get("memoryId").is_none());
        let start = citation["span"]["byteStart"].as_u64().unwrap() as usize;
        let end = citation["span"]["byteEnd"].as_u64().unwrap() as usize;
        assert_eq!(BODY.get(start..end), citation["text"].as_str());
        assert_eq!(value, payload(&ask(root.path(), json!({"paths":[path], "readOnly":true}))));
    }
    let no_path = payload(&ask(root.path(), json!({})));
    assert_eq!(no_path["data"]["abstained"], true);
    assert_eq!(no_path["data"]["candidatesScanned"], 0);
    let combined = payload(&ask(root.path(), json!({"paths":["tests/check.rs", "src/new.rs"]})));
    assert_eq!(combined["data"]["candidatesScanned"], 2);
    assert_eq!(std::fs::read(root.path().join(".ee/ee.db")).unwrap(), before);
    assert_eq!(db.list_audit_entries(Some(&workspace), None).unwrap(), audits);
    assert!(!root.path().join(".ee/index").exists());
}

#[test]
fn requested_authority_scope_is_not_dropped_when_paths_match() {
    let (root, db, workspace) = fixture();
    let id = rule(&db, &workspace, 1, "directory", Some("src"), "agent_assertion");
    let normal = payload(&ask(root.path(), json!({"path":"src/new.rs"})));
    assert_eq!(normal["data"]["abstained"], false, "{normal}");
    for scope in ["verified", "self", "global", "team"] {
        let scoped = payload(&ask(root.path(), json!({"path":"src/new.rs", "memoryScope":scope})));
        assert_eq!(scoped["data"]["abstained"], true, "{scope}: {scoped}");
        assert!(!scoped.to_string().contains(&id));
    }
}

#[test]
fn invalid_constraints_fail_before_dispatch_without_audit_writes() {
    let (root, db, workspace) = fixture();
    rule(&db, &workspace, 1, "workspace", None, "human_explicit");
    let before = std::fs::read(root.path().join(".ee/ee.db")).unwrap();
    for extra in [
        json!({"memoryScope":"private-canary"}),
        json!({"memoryScope":"verified", "memory_scope":"workspace"}),
        json!({"paths":["src/lib.rs", 12]}),
        json!({"path":"src/lib.rs", "paths":[]}),
        json!({"private-canary-misspelled-scope":"verified"}),
        json!({"readOnly":false}),
        json!({"requireConfidence":2}),
    ] {
        denied(&ask(root.path(), extra));
    }
    assert_eq!(std::fs::read(root.path().join(".ee/ee.db")).unwrap(), before);
}

#[test]
fn scope_revisions_and_retirement_are_visible_without_reindexing() {
    let (root, db, workspace) = fixture();
    let id = rule(&db, &workspace, 1, "directory", Some("src"), "human_explicit");
    let initial = payload(&ask(root.path(), json!({"path":"src/new.rs"})));
    db.execute_raw(&format!("UPDATE procedural_rules SET scope_pattern = 'tests' WHERE id = '{id}'")).unwrap();
    let old = payload(&ask(root.path(), json!({"path":"src/new.rs"})));
    assert_eq!(old["data"]["abstained"], true);
    let moved = payload(&ask(root.path(), json!({"path":"tests/new.rs"})));
    assert_eq!(moved["data"]["citations"][0]["ruleId"], id);
    assert_ne!(moved["data"]["citations"][0]["entityRevision"], initial["data"]["citations"][0]["entityRevision"]);
    db.execute_raw(&format!("UPDATE procedural_rules SET maturity = 'deprecated' WHERE id = '{id}'")).unwrap();
    let retired = payload(&ask(root.path(), json!({"path":"tests/new.rs"})));
    assert_eq!(retired["data"]["abstained"], true);
    assert!(!retired.to_string().contains(&id));
    assert!(!root.path().join(".ee/index").exists());
}

#[test]
fn unsafe_paths_use_the_shared_cli_validation_and_never_write() {
    let (root, db, workspace) = fixture();
    rule(&db, &workspace, 1, "workspace", None, "human_explicit");
    let before = std::fs::read(root.path().join(".ee/ee.db")).unwrap();
    for path in ["../private-canary", "/home/private-canary", "src/*.rs"] {
        denied(&ask(root.path(), json!({"path":path})));
    }
    assert_eq!(std::fs::read(root.path().join(".ee/ee.db")).unwrap(), before);
}
