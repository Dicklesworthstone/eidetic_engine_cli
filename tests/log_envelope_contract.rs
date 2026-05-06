use chrono::DateTime;
use ee::obs::{LOG_ENVELOPE_SCHEMA_V1, LogEnvelope};
use serde_json::Value;

type TestResult = Result<(), String>;

const GOLDEN_LOG_ENVELOPE: &str = include_str!("fixtures/golden/obs/log_envelope.json.golden");

#[test]
fn log_envelope_golden_pins_json_line_shape() -> TestResult {
    let line = GOLDEN_LOG_ENVELOPE.trim_end();
    let envelope: LogEnvelope = serde_json::from_str(line)
        .map_err(|error| format!("golden log envelope must parse: {error}"))?;

    assert_eq!(envelope.schema, LOG_ENVELOPE_SCHEMA_V1);
    assert_rfc3339_timestamp(&envelope.ts)?;
    assert_valid_level(&envelope.level)?;
    assert!(!envelope.target.trim().is_empty());
    assert!(envelope.fields.get("command").is_some());

    let reparsed: Value =
        serde_json::from_str(line).map_err(|error| format!("golden must be JSON: {error}"))?;
    assert!(reparsed["fields"].is_object());
    assert_eq!(
        serde_json::to_string(&envelope).map_err(|error| error.to_string())?,
        line
    );
    Ok(())
}

fn assert_rfc3339_timestamp(timestamp: &str) -> TestResult {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|_| ())
        .map_err(|error| format!("timestamp must be RFC3339: {error}"))
}

fn assert_valid_level(level: &str) -> TestResult {
    match level {
        "trace" | "debug" | "info" | "warn" | "error" => Ok(()),
        other => Err(format!("invalid log level {other:?}")),
    }
}
