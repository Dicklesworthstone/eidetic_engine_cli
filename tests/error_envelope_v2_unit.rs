use ee::models::DomainError;
use ee::output::{error_response_json, render_schema_export_json};
use serde_json::Value;

type TestResult = Result<(), String>;

fn parse_error(error: DomainError) -> Result<Value, String> {
    serde_json::from_str(&error_response_json(&error)).map_err(|err| err.to_string())
}

fn pointer<'a>(value: &'a Value, path: &str) -> Result<&'a Value, String> {
    value
        .pointer(path)
        .ok_or_else(|| format!("missing JSON pointer {path} in {value}"))
}

#[test]
fn minimum_error_envelope_declares_v2_without_extra_root_fields() -> TestResult {
    let value = parse_error(DomainError::Usage {
        message: "bad flag".to_owned(),
        repair: None,
    })?;
    let root = value
        .as_object()
        .ok_or_else(|| "root must be an object".to_owned())?;
    if root.keys().any(|key| key == "success") {
        return Err("error envelope must not include success".to_owned());
    }
    assert_eq!(
        pointer(&value, "/schema")?,
        &serde_json::json!("ee.error.v2")
    );
    assert_eq!(pointer(&value, "/error/code")?, &serde_json::json!("usage"));
    assert_eq!(
        pointer(&value, "/error/severity")?,
        &serde_json::json!("low")
    );
    assert_eq!(pointer(&value, "/error/details")?, &serde_json::json!({}));
    Ok(())
}

#[test]
fn recovery_actions_are_structured_in_v2_details() -> TestResult {
    let value = parse_error(DomainError::SearchIndex {
        message: "Index is stale.".to_owned(),
        repair: Some("ee index rebuild".to_owned()),
    })?;
    let recovery = pointer(&value, "/error/details/recovery")?
        .as_array()
        .ok_or_else(|| "recovery must be an array".to_owned())?;
    let first = recovery
        .first()
        .ok_or_else(|| "recovery must contain at least one action".to_owned())?;
    assert_eq!(pointer(first, "/priority")?, &serde_json::json!(1));
    assert_eq!(pointer(first, "/kind")?, &serde_json::json!("migration"));
    assert_eq!(
        pointer(first, "/command")?,
        &serde_json::json!("ee index rebuild --workspace .")
    );
    Ok(())
}

#[test]
fn cass_import_error_lists_attempted_paths_and_recovery() -> TestResult {
    let value = parse_error(DomainError::Import {
        message: "cass binary not found at 'cass'".to_owned(),
        repair: Some("install cass or set EE_CASS_BINARY".to_owned()),
    })?;
    assert_eq!(
        pointer(&value, "/schema")?,
        &serde_json::json!("ee.error.v2")
    );
    assert_eq!(
        pointer(&value, "/error/details/attemptedPaths/0")?,
        &serde_json::json!("/usr/local/bin/cass")
    );
    assert_eq!(
        pointer(&value, "/error/details/recovery/0/envName")?,
        &serde_json::json!("EE_CASS_BINARY")
    );
    Ok(())
}

#[test]
fn schema_export_serves_error_v2_document() -> TestResult {
    let exported: Value = serde_json::from_str(&render_schema_export_json(Some("ee.error.v2")))
        .map_err(|err| err.to_string())?;
    assert_eq!(
        pointer(&exported, "/title")?,
        &serde_json::json!("ee.error.v2")
    );
    assert_eq!(
        pointer(&exported, "/properties/schema/const")?,
        &serde_json::json!("ee.error.v2")
    );
    assert_eq!(
        pointer(
            &exported,
            "/properties/error/properties/nonRecoverable/type"
        )?,
        &serde_json::json!("boolean")
    );
    Ok(())
}
