#[path = "support/test_tracing.rs"]
mod test_tracing;

use serde_json::Value;

type TestResult = Result<(), String>;

const GOLDEN_TRACE: &str = include_str!("golden/logs/test_tracing_support.jsonl.golden");

#[test]
fn test_tracing_helper_emits_normalized_jsonl_contract() -> TestResult {
    let trace = test_tracing::init_test_tracing(
        "bd-3usjw.55",
        "test_tracing_helper_emits_normalized_jsonl_contract",
    );

    trace.setup("minimal_fixture", "created fixture workspace");
    trace.exercise(
        "minimal_fixture",
        "status --json",
        "ran command under trace",
    );
    trace.verify(
        "minimal_fixture",
        "ee.response.v1",
        "ee.response.v1",
        "schema matched",
    );
    trace.teardown("minimal_fixture", "removed temp workspace");

    let normalized = test_tracing::normalize_trace_jsonl(trace.path())?;
    assert_eq!(normalized, GOLDEN_TRACE);

    let mut phases = Vec::new();
    for line in normalized.lines() {
        let value: Value =
            serde_json::from_str(line).map_err(|error| format!("golden line JSON: {error}"))?;
        let fields = value
            .get("fields")
            .and_then(Value::as_object)
            .ok_or_else(|| "normalized event missing fields object".to_owned())?;
        for required in [
            "actual",
            "bead_id",
            "degraded_codes",
            "elapsed_ms",
            "expected",
            "fixture_name",
            "message",
            "phase",
            "request_id",
            "surface",
            "test_name",
            "workspace_id",
        ] {
            if !fields.contains_key(required) {
                return Err(format!(
                    "normalized event missing required field {required}"
                ));
            }
        }
        phases.push(
            fields
                .get("phase")
                .and_then(Value::as_str)
                .ok_or_else(|| "phase must be a string".to_owned())?
                .to_owned(),
        );
    }

    assert_eq!(phases, ["setup", "exercise", "verify", "teardown"]);
    Ok(())
}
