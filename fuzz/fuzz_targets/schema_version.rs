#![no_main]

use libfuzzer_sys::fuzz_target;
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 4096;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let Ok(schema_id) = std::str::from_utf8(data) else {
        return;
    };

    let _ = ee::models::parse_schema_parts(schema_id);
    let _ = ee::models::validate_schema(schema_id);
    for schema in ee::output::public_schemas() {
        let _ = ee::models::validate_schema(schema.id);
    }

    let exported = ee::output::render_schema_export_json(Some(schema_id));
    let Ok(value) = serde_json::from_str::<Value>(&exported) else {
        panic!("schema export returned invalid JSON");
    };
    assert!(
        value.get("$id").and_then(Value::as_str).is_some()
            || value.get("schema").and_then(Value::as_str) == Some(ee::models::ERROR_SCHEMA_V1)
    );
});
