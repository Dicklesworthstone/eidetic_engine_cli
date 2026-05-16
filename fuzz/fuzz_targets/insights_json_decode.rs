#![no_main]

//! Fuzz target for insights JSON and adjacent profile-schema decoding.
//!
//! There is no separate public "insights decoder" type today, so this target
//! pins the JSON surfaces that agents actually consume:
//! - arbitrary bytes either fail JSON parsing cleanly or decode to a Value;
//! - accepted `ee.insights.v1`-shaped objects have stable top-level field
//!   types;
//! - generated successful `ee insights --section topMemories --json` output
//!   remains JSON-decodable and schema-tagged;
//! - context profile names parsed from arbitrary JSON strings use the same
//!   stable model parser as `ee context --profile`.

use std::ffi::OsString;
use std::sync::OnceLock;

use ee::models::{ContextProfileName, ERROR_SCHEMA_V2, RESPONSE_SCHEMA_V1};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 16 * 1024;

fn assert_insights_shape(value: &Value) {
    let Some(object) = value.as_object() else {
        return;
    };
    if object.get("schema").and_then(Value::as_str) != Some("ee.insights.v1") {
        return;
    }

    assert_eq!(
        object.get("command").and_then(Value::as_str),
        Some("insights")
    );
    assert!(
        object.get("availableSections").is_none_or(Value::is_array),
        "availableSections must be an array when present"
    );
    assert!(
        object.get("sections").is_none_or(Value::is_array),
        "sections must be an array when present"
    );
    assert!(
        object.get("degradedSignals").is_none_or(Value::is_array),
        "degradedSignals must be an array when present"
    );
}

fn assert_generated_insights_decodes() {
    static GENERATED: OnceLock<Value> = OnceLock::new();
    let value = GENERATED.get_or_init(|| {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let args = vec![
            OsString::from("ee"),
            OsString::from("--json"),
            OsString::from("insights"),
            OsString::from("--section"),
            OsString::from("topMemories"),
        ];

        let _exit = ee::cli::run(args, &mut stdout, &mut stderr);
        assert!(
            stderr.is_empty(),
            "generated insights JSON should not write stderr"
        );
        let stdout = String::from_utf8_lossy(&stdout);
        serde_json::from_str(&stdout).expect("generated insights output must decode as JSON")
    });

    match value.get("schema").and_then(Value::as_str) {
        Some(RESPONSE_SCHEMA_V1) => {
            let data = value
                .get("data")
                .expect("success envelope must carry data object");
            assert_insights_shape(data);
        }
        Some(ERROR_SCHEMA_V2) => {
            assert!(
                value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .is_some_and(|message| !message.is_empty()),
                "error envelope must remain structured"
            );
        }
        other => panic!("unexpected generated insights envelope schema: {other:?}"),
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let Ok(value) = serde_json::from_slice::<Value>(data) else {
        return;
    };

    assert_insights_shape(&value);

    if let Some(profile_name) = value.as_str() {
        if let Some(parsed) = ContextProfileName::parse(profile_name) {
            let reparsed = ContextProfileName::parse(parsed.as_str())
                .expect("rendered built-in profile names must parse");
            assert_eq!(parsed, reparsed);
        }
    }

    assert_generated_insights_decodes();
});
