//! Contract checks for the redaction-safe mesh import-ledger inspection API.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const SCHEMA_PATH: &str = "docs/schemas/ee.mesh.import_ledger.v1.json";
const SCHEMA_NAME: &str = "ee.mesh.import_ledger.v1";

fn load_schema() -> Result<Value, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCHEMA_PATH);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

#[test]
fn mesh_import_ledger_schema_identity_and_closed_entry_are_pinned() -> TestResult {
    let schema = load_schema()?;
    if schema["title"] != SCHEMA_NAME
        || schema["properties"]["schema"]["const"] != SCHEMA_NAME
        || schema["properties"]["command"]["const"] != "mesh ledger"
    {
        return Err(format!(
            "mesh import-ledger schema identity drifted: {schema}"
        ));
    }
    let entry = &schema["$defs"]["entry"];
    if entry["additionalProperties"] != false {
        return Err("mesh import-ledger entries must remain closed objects".to_owned());
    }
    let properties = entry["properties"]
        .as_object()
        .ok_or_else(|| "mesh import-ledger entry properties must be an object".to_owned())?;
    if properties.contains_key("eventJson") || properties.contains_key("bodyCacheKey") {
        return Err(
            "mesh import-ledger inspection must not expose eventJson or bodyCacheKey".to_owned(),
        );
    }
    for required in [
        "eventId",
        "originNodeId",
        "originWorkspaceId",
        "seq",
        "eventHash",
        "importDecision",
        "policyFailureSurface",
        "policyDecision",
        "importedAt",
    ] {
        if !entry["required"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field == required))
        {
            return Err(format!(
                "mesh import-ledger entry no longer requires {required}"
            ));
        }
    }
    Ok(())
}

#[test]
fn mesh_import_ledger_schema_is_in_the_public_inventory() -> TestResult {
    let matches = ee::output::public_schemas()
        .iter()
        .filter(|entry| entry.id == SCHEMA_NAME)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "expected one {SCHEMA_NAME} public schema entry, found {}",
            matches.len()
        ));
    }
    let definition: Value = serde_json::from_str(&(matches[0].definition)())
        .map_err(|error| format!("parse registered {SCHEMA_NAME}: {error}"))?;
    if definition["properties"]["schema"]["const"] != SCHEMA_NAME {
        return Err("registered mesh import-ledger definition drifted".to_owned());
    }
    Ok(())
}
