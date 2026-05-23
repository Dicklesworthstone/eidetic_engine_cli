//! Contract drift radar for public schema/source-string parity (bd-31nul.7).
//!
//! This test keeps the contract inventory, `src/output::public_schemas`,
//! current schema files, and source-facing schema strings from drifting apart.
//! Negative fixtures exercise MCP-style prompt text and schema-file constants
//! without depending on the currently contested MCP implementation file.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const BEAD_ID: &str = "bd-31nul.7";
const CONTRACT_INVENTORY_JSON: &str =
    include_str!("fixtures/contracts/public_contract_inventory.json");
const TEST_EVENT_SCHEMA: &str = "ee.test_event.v1";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContractInventory {
    contracts: Vec<ContractInventoryEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContractInventoryEntry {
    schema_id: String,
    status: String,
    surface: String,
    owner: String,
    schema_file: Option<String>,
    allowed_historical_contexts: Vec<HistoricalContext>,
    forbidden_current_claims: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoricalContext {
    path_pattern: String,
}

#[derive(Debug, Eq, PartialEq)]
struct SourceSchemaViolation {
    path: String,
    line: usize,
    schema_id: String,
    source_kind: SourceKind,
    policy_decision: &'static str,
    message: String,
    source_excerpt: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceKind {
    Comment,
    StringLiteral,
    SchemaConst,
    SourceText,
}

impl SourceKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::StringLiteral => "string_literal",
            Self::SchemaConst => "schema_const",
            Self::SourceText => "source_text",
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_path(path: &str) -> PathBuf {
    repo_root().join(path)
}

fn contract_inventory() -> Result<ContractInventory, String> {
    serde_json::from_str(CONTRACT_INVENTORY_JSON)
        .map_err(|error| format!("parse public contract inventory: {error}"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn path_matches_pattern(path: &str, pattern: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        path == prefix || path.starts_with(&format!("{prefix}/"))
    } else {
        path == pattern
    }
}

fn path_is_allowed_historical(path: &str, entry: &ContractInventoryEntry) -> bool {
    entry
        .allowed_historical_contexts
        .iter()
        .any(|context| path_matches_pattern(path, &context.path_pattern))
}

fn current_public_schema_ids(inventory: &ContractInventory) -> BTreeSet<String> {
    inventory
        .contracts
        .iter()
        .filter(|entry| entry.status == "current")
        .filter(|entry| entry.owner == "src/output/mod.rs::public_schemas")
        .map(|entry| entry.schema_id.clone())
        .collect()
}

fn exported_public_schema_ids() -> BTreeSet<String> {
    ee::output::public_schemas()
        .iter()
        .map(|entry| entry.id.to_owned())
        .collect()
}

fn classify_source_line(line: &str) -> SourceKind {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        SourceKind::Comment
    } else if trimmed.contains("\"schema\"") && trimmed.contains("\"const\"") {
        SourceKind::SchemaConst
    } else if trimmed.contains('"') {
        SourceKind::StringLiteral
    } else {
        SourceKind::SourceText
    }
}

fn source_excerpt(line: &str) -> String {
    line.split_whitespace()
        .take(32)
        .collect::<Vec<_>>()
        .join(" ")
}

fn legacy_claim_message(
    source_kind: SourceKind,
    line_lower: &str,
    entry: &ContractInventoryEntry,
) -> Option<String> {
    for phrase in &entry.forbidden_current_claims {
        if line_lower.contains(&phrase.to_ascii_lowercase()) {
            return Some(format!(
                "{} appears with forbidden current-facing phrase {phrase:?}",
                entry.schema_id
            ));
        }
    }

    let mcp_prompt_like = source_kind == SourceKind::StringLiteral
        && line_lower.contains("envelope")
        && (line_lower.contains("read")
            || line_lower.contains("parse")
            || line_lower.contains("returned")
            || line_lower.contains("agents"));
    if mcp_prompt_like {
        return Some(format!(
            "{} appears in current-facing prompt text that instructs agents about an envelope",
            entry.schema_id
        ));
    }

    None
}

fn source_schema_violations_for_text(
    path: &str,
    text: &str,
    inventory: &ContractInventory,
) -> Vec<SourceSchemaViolation> {
    let mut violations = Vec::new();

    for entry in inventory
        .contracts
        .iter()
        .filter(|entry| entry.status == "legacy")
    {
        if path_is_allowed_historical(path, entry) {
            continue;
        }
        if entry.surface != "response_success_envelope" {
            continue;
        }

        let schema_id_lower = entry.schema_id.to_ascii_lowercase();
        for (index, line) in text.lines().enumerate() {
            let line_lower = line.to_ascii_lowercase();
            if !line_lower.contains(&schema_id_lower) {
                continue;
            }
            let source_kind = classify_source_line(line);
            if let Some(message) = legacy_claim_message(source_kind, &line_lower, entry) {
                violations.push(SourceSchemaViolation {
                    path: path.to_owned(),
                    line: index + 1,
                    schema_id: entry.schema_id.clone(),
                    source_kind,
                    policy_decision: "violation",
                    message,
                    source_excerpt: source_excerpt(line),
                });
            }
        }
    }

    violations
}

fn source_schema_violation_event(violation: &SourceSchemaViolation) -> Value {
    json!({
        "schema": TEST_EVENT_SCHEMA,
        "phase": "contract_drift_schema_source",
        "beadId": BEAD_ID,
        "path": &violation.path,
        "line": violation.line,
        "schemaId": &violation.schema_id,
        "sourceKind": violation.source_kind.as_str(),
        "policyDecision": violation.policy_decision,
        "message": &violation.message,
        "sourceExcerpt": &violation.source_excerpt,
    })
}

fn source_schema_violation_events(violations: &[SourceSchemaViolation]) -> String {
    violations
        .iter()
        .map(source_schema_violation_event)
        .map(|event| event.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn schema_const_violations_for_value(
    path: &str,
    value: &Value,
    inventory: &ContractInventory,
) -> Vec<SourceSchemaViolation> {
    let mut violations = Vec::new();
    collect_schema_const_violations(path, value, inventory, &mut violations);
    violations
}

fn collect_schema_const_violations(
    path: &str,
    value: &Value,
    inventory: &ContractInventory,
    violations: &mut Vec<SourceSchemaViolation>,
) {
    match value {
        Value::Object(object) => {
            if let Some(schema_id) = object.get("const").and_then(Value::as_str)
                && (schema_id.starts_with("ee.response.") || schema_id.starts_with("ee.error."))
                && let Some(entry) = inventory
                    .contracts
                    .iter()
                    .find(|entry| entry.schema_id == schema_id)
                && entry.status == "legacy"
                && !path_is_allowed_historical(path, entry)
            {
                violations.push(SourceSchemaViolation {
                    path: path.to_owned(),
                    line: 1,
                    schema_id: schema_id.to_owned(),
                    source_kind: SourceKind::SchemaConst,
                    policy_decision: "violation",
                    message: format!(
                        "{schema_id} is a legacy envelope const in a current schema file"
                    ),
                    source_excerpt: format!("\"const\":\"{schema_id}\""),
                });
            }

            for child in object.values() {
                collect_schema_const_violations(path, child, inventory, violations);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_schema_const_violations(path, item, inventory, violations);
            }
        }
        _ => {}
    }
}

fn current_schema_file_const_violations(
    inventory: &ContractInventory,
) -> Result<Vec<SourceSchemaViolation>, String> {
    let mut violations = Vec::new();
    for entry in inventory
        .contracts
        .iter()
        .filter(|entry| entry.status == "current")
    {
        let Some(schema_file) = entry.schema_file.as_deref() else {
            continue;
        };
        let text = fs::read_to_string(repo_path(schema_file))
            .map_err(|error| format!("read schema file {schema_file}: {error}"))?;
        let value = serde_json::from_str::<Value>(&text)
            .map_err(|error| format!("parse schema file {schema_file}: {error}"))?;
        violations.extend(schema_const_violations_for_value(
            schema_file,
            &value,
            inventory,
        ));
    }
    Ok(violations)
}

fn read_source_file(path: &str) -> Result<String, String> {
    fs::read_to_string(repo_path(path)).map_err(|error| format!("read source file {path}: {error}"))
}

#[test]
fn public_schema_registry_matches_current_contract_inventory() -> TestResult {
    let inventory = contract_inventory()?;
    let expected = current_public_schema_ids(&inventory);
    let exported = exported_public_schema_ids();

    let missing = expected.difference(&exported).cloned().collect::<Vec<_>>();
    ensure(
        missing.is_empty(),
        format!("current inventory schemas missing from public_schemas(): {missing:?}"),
    )?;

    let exported_legacy = inventory
        .contracts
        .iter()
        .filter(|entry| entry.status == "legacy")
        .filter(|entry| exported.contains(&entry.schema_id))
        .map(|entry| entry.schema_id.as_str())
        .collect::<Vec<_>>();
    ensure(
        exported_legacy.is_empty(),
        format!("legacy inventory schemas still exported by public_schemas(): {exported_legacy:?}"),
    )
}

#[test]
fn schema_file_consts_do_not_claim_legacy_envelopes() -> TestResult {
    let inventory = contract_inventory()?;
    let violations = current_schema_file_const_violations(&inventory)?;
    ensure(
        violations.is_empty(),
        format!(
            "current schema files contain legacy envelope consts:\n{}",
            source_schema_violation_events(&violations)
        ),
    )
}

#[test]
fn source_string_policy_flags_current_mcp_prompt_legacy_envelope_instruction() -> TestResult {
    let inventory = contract_inventory()?;
    let prompt_source = r#"
fn render_pre_task_context_prompt() -> String {
    "Read the returned `ee.response.v1` response envelope before editing.".to_string()
}
"#;
    let violations = source_schema_violations_for_text("src/mcp.rs", prompt_source, &inventory);
    ensure(
        violations.len() == 1,
        format!(
            "expected one MCP prompt violation, got:\n{}",
            source_schema_violation_events(&violations)
        ),
    )?;
    let event = source_schema_violation_event(&violations[0]);
    ensure(
        event.get("schema").and_then(Value::as_str) == Some(TEST_EVENT_SCHEMA),
        format!("event must use {TEST_EVENT_SCHEMA}: {event}"),
    )
}

#[test]
fn source_string_policy_allows_legacy_constants_without_current_claims() -> TestResult {
    let inventory = contract_inventory()?;
    let neutral_source = r#"
pub const RESPONSE_SCHEMA_V1: &str = "ee.response.v1";
"#;
    let violations =
        source_schema_violations_for_text("src/models/mod.rs", neutral_source, &inventory);
    ensure(
        violations.is_empty(),
        format!(
            "neutral legacy constant should not be a current-claim violation:\n{}",
            source_schema_violation_events(&violations)
        ),
    )
}

#[test]
fn current_public_registry_sources_do_not_claim_legacy_success_envelopes() -> TestResult {
    let inventory = contract_inventory()?;
    let mut violations = Vec::new();

    for path in ["src/output/mod.rs"] {
        let text = read_source_file(path)?;
        violations.extend(source_schema_violations_for_text(path, &text, &inventory));
    }

    ensure(
        violations.is_empty(),
        format!(
            "current public registry sources contain legacy success-envelope claims:\n{}",
            source_schema_violation_events(&violations)
        ),
    )
}

#[test]
fn schema_const_policy_flags_legacy_envelope_const_fixture() -> TestResult {
    let inventory = contract_inventory()?;
    let schema = json!({
        "type": "object",
        "properties": {
            "schema": {
                "type": "string",
                "const": "ee.response.v1"
            }
        }
    });
    let violations =
        schema_const_violations_for_value("docs/schemas/current-example.json", &schema, &inventory);
    ensure(
        violations.len() == 1,
        format!(
            "expected one schema-const violation, got:\n{}",
            source_schema_violation_events(&violations)
        ),
    )?;
    ensure(
        violations[0].source_kind == SourceKind::SchemaConst,
        format!("expected schema_const source kind, got {:?}", violations[0]),
    )
}
