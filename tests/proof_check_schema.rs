use serde_json::Value;

#[test]
fn proof_check_schema_file_matches_core_constant() {
    let schema: Value =
        serde_json::from_str(include_str!("../docs/schemas/ee.proof_check.v1.json"))
            .expect("proof check schema must be valid JSON");
    assert_eq!(
        schema
            .get("properties")
            .and_then(|properties| properties.get("schema"))
            .and_then(|schema_property| schema_property.get("const"))
            .and_then(Value::as_str),
        Some(ee::core::proof_verify::PROOF_CHECK_SCHEMA_V1)
    );
}

#[test]
fn supported_schemas_advertises_proof_check() {
    let supported = ee::core::supported_schemas();
    assert!(supported.iter().any(|schema| {
        schema.name == "proof_check"
            && schema.schema == ee::core::proof_verify::PROOF_CHECK_SCHEMA_V1
    }));
}
