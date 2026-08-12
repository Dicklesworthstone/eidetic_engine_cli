//! Contract checks for the daemon request/response JSON Schemas.
//!
//! bd-333u0 pins the response `result` / `error` xor invariant structurally.
//! bd-3cwdd pins real Rust-serialized daemon envelopes against the published
//! schema files so Rust type drift cannot silently diverge from the docs.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use ee::core::search::PERFORMANCE_FALLBACK_REDACTED_MESSAGE;
use ee::daemon::DAEMON_RESPONSE_SCHEMA_V1;
use ee::daemon::protocol::{DaemonRequest, DaemonResponse};
use ee::daemon::server::{
    DAEMON_SEARCH_EXECUTION_FAILED_CODE, DAEMON_SEARCH_PARAMS_INVALID_CODE,
    DAEMON_SEARCH_REQUEST_SCHEMA_V2, DAEMON_SEARCH_RESPONSE_SCHEMA_V3, METHOD_CAPABILITIES,
    METHOD_CONTEXT, METHOD_ECHO, METHOD_SEARCH, METHOD_SHUTDOWN, METHOD_TELEMETRY, METHOD_WRITE,
    METHOD_WRITE_JOURNAL, dispatch,
};
use ee::db::{CreateWorkspaceInput, DbConnection};

type TestResult = Result<(), String>;

const REQUEST_SCHEMA_PATH: &str = "docs/schemas/ee.daemon.request.v1.json";
const RESPONSE_SCHEMA_PATH: &str = "docs/schemas/ee.daemon.response.v1.json";
const SEARCH_REQUEST_SCHEMA_PATH: &str = "docs/schemas/ee.daemon.search.request.v2.json";
const SEARCH_RESPONSE_SCHEMA_PATH: &str = "docs/schemas/ee.daemon.search.response.v3.json";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_schema(schema_path: &str) -> Result<Value, String> {
    let path = repo_root().join(schema_path);
    let text =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn read_request_schema() -> Result<Value, String> {
    read_schema(REQUEST_SCHEMA_PATH)
}

fn read_response_schema() -> Result<Value, String> {
    read_schema(RESPONSE_SCHEMA_PATH)
}

fn collect_string_set(value: &Value, context: &str) -> Result<BTreeSet<String>, String> {
    let array = value
        .as_array()
        .ok_or_else(|| format!("{context} must be an array, got {value}"))?;
    array
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{context} contains non-string value {entry}"))
        })
        .collect()
}

fn string_set(fields: &[&str]) -> BTreeSet<String> {
    fields.iter().map(|field| (*field).to_owned()).collect()
}

fn serialized_request() -> Result<Value, String> {
    let mut request = DaemonRequest::new(
        "req-schema-cross-validation",
        "agent-schema-cross-validation",
        METHOD_ECHO,
        serde_json::json!({"hello": "world", "n": 42}),
    );
    request.workspace_id = Some("workspace-schema-cross-validation".to_owned());
    serde_json::to_value(request).map_err(|error| format!("serialize DaemonRequest: {error}"))
}

fn capabilities_request() -> Result<Value, String> {
    let request = DaemonRequest::new(
        "req-capabilities-schema-cross-validation",
        "agent-schema-cross-validation",
        METHOD_CAPABILITIES,
        serde_json::json!({}),
    );
    serde_json::to_value(request).map_err(|error| format!("serialize DaemonRequest: {error}"))
}

fn success_response() -> Value {
    serde_json::to_value(DaemonResponse::ok(
        "req-success",
        "agent-a",
        Some("workspace-a".to_owned()),
        serde_json::json!({"echo": true}),
    ))
    .expect("DaemonResponse::ok must serialize")
}

fn capabilities_response() -> Value {
    serde_json::to_value(DaemonResponse::ok(
        "req-capabilities",
        "agent-a",
        None,
        serde_json::json!({
            "protocol": "ee.daemon",
            "request_schemas": ["ee.daemon.request.v1"],
            "response_schemas": [DAEMON_RESPONSE_SCHEMA_V1],
            "methods": [
                METHOD_CAPABILITIES,
                METHOD_CONTEXT,
                METHOD_ECHO,
                METHOD_SEARCH,
                METHOD_SHUTDOWN,
                METHOD_TELEMETRY,
                METHOD_WRITE,
                METHOD_WRITE_JOURNAL
            ],
            "authorization": {
                "ee.daemon.capabilities": "same_uid",
                "ee.daemon.context": "same_uid_workspace",
                "ee.daemon.echo": "same_uid",
                "ee.daemon.search": "same_uid_workspace",
                "ee.daemon.shutdown": "same_uid",
                "ee.daemon.telemetry": "same_uid",
                "ee.daemon.write": "same_uid_workspace",
                "ee.daemon.write_journal": "same_uid_workspace"
            },
            "method_schemas": {
                "ee.daemon.search": {
                    "request": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
                    "response": DAEMON_SEARCH_RESPONSE_SCHEMA_V3
                }
            },
            "forward_compat": {
                "v1_unknown_fields": "rejected",
                "v1_unknown_methods": "daemon_unknown_method",
                "v2_migration": "Call ee.daemon.capabilities with ee.daemon.request.v1 before sending any non-v1 schema or method; downgrade to an advertised schema/method when absent."
            }
        }),
    ))
    .expect("DaemonResponse::ok must serialize")
}

fn error_response() -> Value {
    serde_json::to_value(
        DaemonResponse::err(
            "req-error",
            "agent-a",
            None,
            "daemon_unknown_method",
            "unknown method",
        )
        .with_degraded("daemon_unknown_method"),
    )
    .expect("DaemonResponse::err must serialize")
}

fn actual_daemon_search_result(query: &str) -> Result<Value, String> {
    let temp = tempfile::tempdir().map_err(|error| format!("search tempdir: {error}"))?;
    let workspace = temp.path().join("workspace");
    let state_dir = workspace.join(".ee");
    fs::create_dir_all(&state_dir).map_err(|error| format!("create search workspace: {error}"))?;
    let database = state_dir.join("ee.db");
    let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_workspace(
            "wsp_daemon_schema_contract_00001",
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: Some("daemon schema contract".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    let workspace_id = workspace.display().to_string();
    let mut request = DaemonRequest::new(
        "req-search-response-schema",
        "agent-search-response-schema",
        METHOD_SEARCH,
        serde_json::json!({
            "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
            "query": query,
            "workspacePath": workspace_id,
            "databasePath": database,
            "indexDir": state_dir.join("missing-index"),
            "speed": "instant",
            "sourceMode": "lexical_only",
            "strictSourceMode": true,
            "dedupe": "doc_id",
            "memoryScope": "swarm",
            "explainPerformance": true
        }),
    );
    request.workspace_id = Some(workspace.display().to_string());
    let response = dispatch(&request);
    if let Some(error) = response.error {
        return Err(format!(
            "real daemon search did not serialize a result: {}: {}",
            error.code, error.message
        ));
    }
    response
        .result
        .ok_or_else(|| "real daemon search response omitted result".to_owned())
}

#[test]
fn daemon_schema_cross_validation_accepts_serialized_rust_request() -> TestResult {
    let schema = read_request_schema()?;
    validate_json_schema(&serialized_request()?, &schema, "$")
}

#[test]
fn daemon_schema_cross_validation_accepts_capabilities_request() -> TestResult {
    let schema = read_request_schema()?;
    validate_json_schema(&capabilities_request()?, &schema, "$")
}

#[test]
fn daemon_schema_cross_validation_accepts_serialized_rust_result_response() -> TestResult {
    let schema = read_response_schema()?;
    validate_json_schema(&success_response(), &schema, "$")
}

#[test]
fn daemon_schema_cross_validation_accepts_capabilities_response() -> TestResult {
    let schema = read_response_schema()?;
    validate_json_schema(&capabilities_response(), &schema, "$")
}

#[test]
fn daemon_schema_cross_validation_accepts_serialized_rust_error_response() -> TestResult {
    let schema = read_response_schema()?;
    validate_json_schema(&error_response(), &schema, "$")
}

#[test]
fn daemon_response_schema_structurally_requires_result_xor_error() -> TestResult {
    let schema = read_response_schema()?;
    let root_required = collect_string_set(&schema["required"], "schema root required")?;
    let expected_root_required = string_set(&["schema", "request_id", "agent_id"]);
    if root_required != expected_root_required {
        return Err(format!(
            "schema root required drifted; expected {expected_root_required:?}, got {root_required:?}"
        ));
    }

    let one_of = schema
        .pointer("/oneOf")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema root oneOf missing".to_string())?;
    if one_of.len() != 2 {
        return Err(format!(
            "schema root oneOf must have 2 branches, got {}",
            one_of.len()
        ));
    }

    let actual_branch_required = one_of
        .iter()
        .enumerate()
        .map(|(index, branch)| {
            collect_string_set(
                &branch["required"],
                &format!("schema root oneOf[{index}].required"),
            )
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let expected_branch_required = [string_set(&["result"]), string_set(&["error"])]
        .into_iter()
        .collect::<BTreeSet<_>>();
    if actual_branch_required != expected_branch_required {
        return Err(format!(
            "schema root oneOf required branches drifted; expected {expected_branch_required:?}, got {actual_branch_required:?}"
        ));
    }

    Ok(())
}

#[test]
fn daemon_response_schema_documents_seed_error_codes() -> TestResult {
    let schema = read_response_schema()?;
    let description = schema
        .pointer("/properties/error/description")
        .and_then(Value::as_str)
        .ok_or_else(|| "schema error.description missing".to_string())?;

    for code in [
        "daemon_unknown_method",
        "daemon_method_unauthorized",
        "daemon_request_decode_failed",
        "daemon_request_schema_mismatch",
        "daemon_context_params_invalid",
        "daemon_context_deadline_exceeded",
        "daemon_context_execution_failed",
        DAEMON_SEARCH_PARAMS_INVALID_CODE,
        DAEMON_SEARCH_EXECUTION_FAILED_CODE,
    ] {
        if !description.contains(code) {
            return Err(format!("schema error.description missing {code}"));
        }
    }

    Ok(())
}

#[test]
fn daemon_search_method_schemas_pin_paths_timings_and_nested_strictness() -> TestResult {
    let request = read_schema(SEARCH_REQUEST_SCHEMA_PATH)?;
    let response = read_schema(SEARCH_RESPONSE_SCHEMA_PATH)?;
    if request.pointer("/$id").and_then(Value::as_str)
        != Some("https://eidetic-engine/schemas/ee.daemon.search.request.v2.json")
        || response.pointer("/$id").and_then(Value::as_str)
            != Some("https://eidetic-engine/schemas/ee.daemon.search.response.v3.json")
    {
        return Err("daemon search method schema ids drifted".to_owned());
    }
    for (schema, pointer, context) in [
        (&request, "/additionalProperties", "search request"),
        (&response, "/additionalProperties", "search response"),
        (
            &response,
            "/properties/response/additionalProperties",
            "canonical response",
        ),
        (
            &response,
            "/properties/response/properties/data/additionalProperties",
            "canonical search data",
        ),
        (
            &response,
            "/$defs/degradation/additionalProperties",
            "canonical response degradation",
        ),
        (
            &response,
            "/properties/timing/additionalProperties",
            "search timing",
        ),
        (
            &response,
            "/$defs/performance/additionalProperties",
            "canonical performance envelope",
        ),
        (
            &response,
            "/$defs/performance/properties/data/additionalProperties",
            "canonical performance data",
        ),
        (
            &response,
            "/$defs/performanceRuntimeProfile/additionalProperties",
            "performance runtime profile",
        ),
        (
            &response,
            "/$defs/performanceRuntimeProfile/properties/budgets/additionalProperties",
            "performance runtime budgets",
        ),
        (
            &response,
            "/$defs/performanceRuntimeProfile/properties/budgets/properties/search/additionalProperties",
            "performance runtime search budget",
        ),
        (
            &response,
            "/$defs/performanceRuntimeProfile/properties/budgets/properties/pack/additionalProperties",
            "performance runtime pack budget",
        ),
        (
            &response,
            "/$defs/performanceRuntimeProfile/properties/budgets/properties/cache/additionalProperties",
            "performance runtime cache budget",
        ),
        (
            &response,
            "/$defs/performanceRuntimeProfile/properties/budgets/properties/writeSpool/additionalProperties",
            "performance runtime write-spool budget",
        ),
        (
            &response,
            "/$defs/performanceRuntimeProfile/properties/budgets/properties/steward/additionalProperties",
            "performance runtime steward budget",
        ),
        (
            &response,
            "/$defs/performanceRuntimeProfile/properties/budgets/properties/verification/additionalProperties",
            "performance runtime verification budget",
        ),
        (
            &response,
            "/$defs/performanceRuntimeProfile/properties/budgets/properties/diagnostics/additionalProperties",
            "performance runtime diagnostics budget",
        ),
        (
            &response,
            "/$defs/performanceSearch/additionalProperties",
            "performance search report",
        ),
        (
            &response,
            "/$defs/performanceSearch/properties/sourceCounts/additionalProperties",
            "performance search source counts",
        ),
        (
            &response,
            "/$defs/performanceSearch/properties/scoreDistribution/additionalProperties",
            "performance search score distribution",
        ),
        (
            &response,
            "/$defs/performanceSearch/properties/fieldCoverage/additionalProperties",
            "performance search field coverage",
        ),
        (
            &response,
            "/$defs/performanceNamedTiming/additionalProperties",
            "named performance timing",
        ),
        (
            &response,
            "/$defs/performance/properties/data/properties/redaction/additionalProperties",
            "performance redaction",
        ),
        (
            &response,
            "/$defs/performanceFallback/additionalProperties",
            "redacted performance fallback",
        ),
        (
            &response,
            "/$defs/searchDocument/additionalProperties",
            "search document",
        ),
    ] {
        if schema.pointer(pointer).and_then(Value::as_bool) != Some(false) {
            return Err(format!("{context} must reject unknown fields"));
        }
    }
    let timing_required = collect_string_set(
        &response["properties"]["timing"]["required"],
        "search timing required",
    )?;
    let expected_timing = string_set(&["daemonTotal", "embedderPreparation", "indexOpen", "query"]);
    if timing_required != expected_timing {
        return Err(format!(
            "search timing required fields drifted; expected {expected_timing:?}, got {timing_required:?}"
        ));
    }
    if request
        .pointer("/required")
        .and_then(Value::as_array)
        .is_some_and(|required| {
            required
                .iter()
                .any(|field| field.as_str() == Some("explainPerformance"))
        })
    {
        return Err("search request explainPerformance must remain optional".to_owned());
    }
    if request
        .pointer("/properties/explainPerformance/type")
        .and_then(Value::as_str)
        != Some("boolean")
    {
        return Err("search request explainPerformance must be a boolean".to_owned());
    }
    if response
        .pointer("/required")
        .and_then(Value::as_array)
        .is_some_and(|required| {
            required
                .iter()
                .any(|field| field.as_str() == Some("performance"))
        })
    {
        return Err("search response performance must remain optional".to_owned());
    }
    if response
        .pointer("/$defs/performance/properties/data/properties/redaction/properties/queryTextIncluded/const")
        .and_then(Value::as_bool)
        != Some(false)
        || response
            .pointer("/$defs/performanceFallback/properties/message/const")
            .and_then(Value::as_str)
            != Some(PERFORMANCE_FALLBACK_REDACTED_MESSAGE)
    {
        return Err(
            "search performance schema must pin its query and fallback redaction claims"
                .to_owned(),
        );
    }
    if response
        .pointer("/properties/performance/$ref")
        .and_then(Value::as_str)
        != Some("#/$defs/performance")
        || response
            .pointer("/$defs/performance/properties/schema/const")
            .and_then(Value::as_str)
            != Some("ee.explain.performance.v1")
        || response
            .pointer("/$defs/performance/properties/success/const")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err(
            "search response performance must reference the canonical performance envelope"
                .to_owned(),
        );
    }
    let request_description = request
        .pointer("/description")
        .and_then(Value::as_str)
        .ok_or_else(|| "search request description missing".to_owned())?;
    for phrase in ["absolute", "canonical", "symlink escape"] {
        if !request_description.contains(phrase) {
            return Err(format!(
                "search request containment contract missing {phrase:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn daemon_search_request_v2_schema_accepts_only_canonical_enum_values() -> TestResult {
    let schema = read_schema(SEARCH_REQUEST_SCHEMA_PATH)?;
    let base = serde_json::json!({
        "schema": DAEMON_SEARCH_REQUEST_SCHEMA_V2,
        "query": "release",
        "workspacePath": "/tmp/ee-daemon-search-contract",
        "speed": "default",
        "dedupe": "doc_id",
        "sourceMode": "hybrid",
        "memoryScope": "swarm"
    });
    validate_json_schema(&base, &schema, "$")?;

    for (field, canonical) in [
        ("speed", "quality"),
        ("dedupe", "mi"),
        ("sourceMode", "semantic_only"),
        ("memoryScope", "workspace"),
    ] {
        let mut value = base.clone();
        value[field] = serde_json::json!(canonical);
        validate_json_schema(&value, &schema, "$")
            .map_err(|error| format!("canonical {field}={canonical:?} rejected: {error}"))?;
    }

    for (field, noncanonical) in [
        ("speed", " Quality "),
        ("dedupe", "doc-id"),
        ("sourceMode", "lexical"),
        ("sourceMode", "Semantic_Only"),
        ("memoryScope", " SWARM "),
    ] {
        let mut value = base.clone();
        value[field] = serde_json::json!(noncanonical);
        if validate_json_schema(&value, &schema, "$").is_ok() {
            return Err(format!(
                "request schema accepted noncanonical {field}={noncanonical:?}"
            ));
        }
    }

    let mut blank_query = base;
    blank_query["query"] = serde_json::json!("   \t");
    if validate_json_schema(&blank_query, &schema, "$").is_ok() {
        return Err("request schema accepted a whitespace-only query".to_owned());
    }
    Ok(())
}

#[test]
fn real_daemon_search_response_v3_validates_and_redacts_performance_fallbacks() -> TestResult {
    const SECRET_QUERY: &str = "release sk_live_daemon_query_must_not_escape_performance";
    let schema = read_schema(SEARCH_RESPONSE_SCHEMA_PATH)?;
    let result = actual_daemon_search_result(SECRET_QUERY)?;
    validate_json_schema(&result, &schema, "$")?;

    if result
        .pointer("/performance/data/redaction/queryTextIncluded")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err("real performance response must pin queryTextIncluded=false".to_owned());
    }
    let performance = result
        .get("performance")
        .ok_or_else(|| "real response omitted requested performance envelope".to_owned())?;
    let rendered = serde_json::to_string(performance)
        .map_err(|error| format!("serialize real performance envelope: {error}"))?;
    if rendered.contains(SECRET_QUERY) || rendered.contains("sk_live_daemon_query") {
        return Err(format!(
            "real performance envelope leaked planted query: {rendered}"
        ));
    }
    let fallbacks = performance
        .pointer("/data/fallbacks")
        .and_then(Value::as_array)
        .ok_or_else(|| "real performance envelope omitted fallbacks[]".to_owned())?;
    if fallbacks.is_empty()
        || fallbacks.iter().any(|fallback| {
            fallback.get("message").and_then(Value::as_str)
                != Some(PERFORMANCE_FALLBACK_REDACTED_MESSAGE)
        })
    {
        return Err(format!(
            "real performance fallbacks are not uniformly redacted: {fallbacks:?}"
        ));
    }

    let mut false_claim = result.clone();
    false_claim["performance"]["data"]["redaction"]["queryTextIncluded"] = serde_json::json!(true);
    if validate_json_schema(&false_claim, &schema, "$").is_ok() {
        return Err("response schema accepted queryTextIncluded=true".to_owned());
    }
    let mut leaked_fallback = result;
    leaked_fallback["performance"]["data"]["fallbacks"][0]["message"] =
        serde_json::json!(SECRET_QUERY);
    if validate_json_schema(&leaked_fallback, &schema, "$").is_ok() {
        return Err("response schema accepted a query-bearing performance fallback".to_owned());
    }
    Ok(())
}

#[test]
fn daemon_search_response_v3_schema_rejects_rust_validator_drift_matrix() -> TestResult {
    let schema = read_schema(SEARCH_RESPONSE_SCHEMA_PATH)?;
    let valid = actual_daemon_search_result("schema drift matrix")?;
    validate_json_schema(&valid, &schema, "$")?;

    let mut invalid_cases = Vec::new();

    let mut profile_unknown = valid.clone();
    profile_unknown["performance"]["data"]["profileRuntime"]["budgets"]["search"]["unexpected"] =
        serde_json::json!(1);
    invalid_cases.push(("profileRuntime nested unknown field", profile_unknown));

    let mut profile_enum = valid.clone();
    profile_enum["performance"]["data"]["profileRuntime"]["budgets"]["verification"]["heavyStrategy"] =
        serde_json::json!("remote_magic");
    invalid_cases.push(("profileRuntime nested enum drift", profile_enum));

    let mut profile_source = valid.clone();
    profile_source["performance"]["data"]["profileRuntime"]["source"] = serde_json::json!("");
    invalid_cases.push(("profileRuntime empty source", profile_source));

    let mut profile_missing = valid.clone();
    profile_missing["performance"]["data"]["profileRuntime"]["budgets"]
        .as_object_mut()
        .ok_or_else(|| "runtime budgets fixture must be an object".to_owned())?
        .remove("diagnostics");
    invalid_cases.push(("profileRuntime missing required budget", profile_missing));

    for (case, budget, field, invalid_value) in [
        (
            "profileRuntime pack enum drift",
            "pack",
            "explanationVerbosity",
            serde_json::json!("brief"),
        ),
        (
            "profileRuntime cache negative value",
            "cache",
            "entryCap",
            serde_json::json!(-1),
        ),
        (
            "profileRuntime writeSpool type drift",
            "writeSpool",
            "retryBudget",
            serde_json::json!("3"),
        ),
        (
            "profileRuntime steward type drift",
            "steward",
            "daemonPrewarm",
            serde_json::json!(1),
        ),
        (
            "profileRuntime diagnostics enum drift",
            "diagnostics",
            "redaction",
            serde_json::json!("none"),
        ),
    ] {
        let mut invalid = valid.clone();
        invalid["performance"]["data"]["profileRuntime"]["budgets"][budget][field] = invalid_value;
        invalid_cases.push((case, invalid));
    }

    let mut search_status = valid.clone();
    search_status["performance"]["data"]["search"]["status"] = serde_json::json!("partial");
    invalid_cases.push(("search status vocabulary drift", search_status));

    let mut search_type = valid.clone();
    search_type["performance"]["data"]["search"]["returnedHits"] = serde_json::json!("0");
    invalid_cases.push(("search unsigned type drift", search_type));

    let mut source_counts_unknown = valid.clone();
    source_counts_unknown["performance"]["data"]["search"]["sourceCounts"]["other"] =
        serde_json::json!(0);
    invalid_cases.push(("search sourceCounts unknown field", source_counts_unknown));

    let mut score_distribution_type = valid.clone();
    score_distribution_type["performance"]["data"]["search"]["scoreDistribution"]["top"] =
        serde_json::json!("none");
    invalid_cases.push((
        "search scoreDistribution type drift",
        score_distribution_type,
    ));

    let mut field_coverage_negative = valid.clone();
    field_coverage_negative["performance"]["data"]["search"]["fieldCoverage"]["metadataCount"] =
        serde_json::json!(-1);
    invalid_cases.push((
        "search fieldCoverage negative value",
        field_coverage_negative,
    ));

    let mut errors_type = valid.clone();
    errors_type["performance"]["data"]["search"]["errors"] = serde_json::json!([7]);
    invalid_cases.push(("search errors item type drift", errors_type));

    let mut elapsed_value = valid.clone();
    elapsed_value["performance"]["data"]["search"]["elapsed"]["nondeterministic"] =
        serde_json::json!(false);
    invalid_cases.push(("search elapsed value drift", elapsed_value));

    let invalid_named_timing = serde_json::json!({
        "elapsedMs": 1.0,
        "elapsedMsBucket": "1_9ms",
        "nondeterministic": true
    });
    let mut search_timing = valid.clone();
    search_timing["performance"]["data"]["search"]["timings"] =
        serde_json::json!([invalid_named_timing.clone()]);
    invalid_cases.push(("search timing missing name", search_timing));

    let mut aggregate_timing = valid.clone();
    aggregate_timing["performance"]["data"]["timings"] = serde_json::json!([invalid_named_timing]);
    invalid_cases.push(("aggregate timing missing name", aggregate_timing));

    let mut aggregate_timing_extra = valid.clone();
    aggregate_timing_extra["performance"]["data"]["timings"] = serde_json::json!([{
        "elapsedMs": 1.0,
        "elapsedMsBucket": "1_9ms",
        "nondeterministic": true,
        "name": "query",
        "unexpected": true
    }]);
    invalid_cases.push(("aggregate timing unknown field", aggregate_timing_extra));

    let mut safe_fields_order = valid.clone();
    safe_fields_order["performance"]["data"]["redaction"]["safeFields"] = serde_json::json!([
        "elapsedMs",
        "counts",
        "elapsedMsBucket",
        "status",
        "fingerprints",
        "degradationCodes"
    ]);
    invalid_cases.push(("redaction safeFields order drift", safe_fields_order));

    let mut fallback_sources = valid;
    fallback_sources["performance"]["data"]["fallbacks"][0]["sources"] =
        serde_json::json!(["search", "reranker"]);
    invalid_cases.push(("fallback source drift", fallback_sources));

    for (case, invalid) in invalid_cases {
        if validate_json_schema(&invalid, &schema, "$").is_ok() {
            return Err(format!(
                "daemon search response v3 schema accepted invalid {case}"
            ));
        }
    }
    Ok(())
}

#[test]
fn daemon_search_current_method_schemas_are_registered_and_exportable() -> TestResult {
    for (schema_id, schema_path, expected_id) in [
        (
            DAEMON_SEARCH_REQUEST_SCHEMA_V2,
            SEARCH_REQUEST_SCHEMA_PATH,
            "https://eidetic-engine/schemas/ee.daemon.search.request.v2.json",
        ),
        (
            DAEMON_SEARCH_RESPONSE_SCHEMA_V3,
            SEARCH_RESPONSE_SCHEMA_PATH,
            "https://eidetic-engine/schemas/ee.daemon.search.response.v3.json",
        ),
    ] {
        let registration_count = ee::output::public_schemas()
            .iter()
            .filter(|entry| entry.id == schema_id)
            .count();
        if registration_count != 1 {
            return Err(format!(
                "{schema_id} must be registered exactly once, got {registration_count}"
            ));
        }
        let on_disk = read_schema(schema_path)?;
        let exported: Value =
            serde_json::from_str(&ee::output::render_schema_export_json(Some(schema_id)))
                .map_err(|error| format!("parse exported {schema_id}: {error}"))?;
        if on_disk != exported {
            return Err(format!(
                "{schema_id} export must byte-semantically match {schema_path}"
            ));
        }
        if exported.pointer("/$id").and_then(Value::as_str) != Some(expected_id) {
            return Err(format!("{schema_id} exported document id drifted"));
        }
    }
    Ok(())
}

#[test]
fn daemon_request_schema_documents_strict_v1_capabilities_migration() -> TestResult {
    let schema = read_request_schema()?;
    let description = schema
        .pointer("/description")
        .and_then(Value::as_str)
        .ok_or_else(|| "schema description missing".to_string())?;

    for needle in [
        METHOD_CAPABILITIES,
        "unknown top-level fields and unknown methods are rejected",
        "downgrade or fall back to the in-process CLI path",
    ] {
        if !description.contains(needle) {
            return Err(format!(
                "request schema description missing migration phrase {needle:?}"
            ));
        }
    }

    Ok(())
}

#[test]
fn daemon_request_schema_advertises_seed_methods() -> TestResult {
    let schema = read_request_schema()?;
    let methods = schema
        .pointer("/properties/method/enum")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema method enum missing".to_string())?;

    for method in [
        METHOD_CAPABILITIES,
        METHOD_ECHO,
        METHOD_CONTEXT,
        METHOD_SEARCH,
        METHOD_SHUTDOWN,
        METHOD_TELEMETRY,
        METHOD_WRITE,
        METHOD_WRITE_JOURNAL,
    ] {
        if !methods.iter().any(|value| value.as_str() == Some(method)) {
            return Err(format!("request schema method enum missing {method}"));
        }
    }

    Ok(())
}

#[test]
fn daemon_response_schema_accepts_result_response() -> TestResult {
    let schema = read_response_schema()?;
    validate_json_schema(&success_response(), &schema, "$")
}

#[test]
fn daemon_response_schema_accepts_error_response() -> TestResult {
    let schema = read_response_schema()?;
    validate_json_schema(&error_response(), &schema, "$")
}

#[test]
fn daemon_response_schema_rejects_both_result_and_error() -> TestResult {
    let schema = read_response_schema()?;
    let mut response = success_response();
    response
        .as_object_mut()
        .ok_or_else(|| "success response is not an object".to_string())?
        .insert(
            "error".to_string(),
            serde_json::json!({
                "code": "daemon_unknown_method",
                "message": "unknown method"
            }),
        );

    match validate_json_schema(&response, &schema, "$") {
        Ok(()) => Err("validator accepted response with both result and error".to_string()),
        Err(_) => Ok(()),
    }
}

#[test]
fn daemon_response_schema_rejects_neither_result_nor_error() -> TestResult {
    let schema = read_response_schema()?;
    let response = serde_json::json!({
        "schema": DAEMON_RESPONSE_SCHEMA_V1,
        "request_id": "req-empty",
        "agent_id": "agent-a",
        "degraded_codes": []
    });

    match validate_json_schema(&response, &schema, "$") {
        Ok(()) => Err("validator accepted response missing both result and error".to_string()),
        Err(_) => Ok(()),
    }
}

/// bd-2yg7d / bd-1dzhz: the daemon schema descriptions are the machine-facing
/// contract for where clients should expect the socket. The production
/// resolver (`default_daemon_socket_path`, ADR 0055) never defaults to a bare
/// shared socket directly under /tmp — that shape was the bd-3j0td
/// cross-tenant attack surface — so no daemon schema may describe it as a
/// default. The forbidden-substring arm is the planted regression trap: it
/// fails against the pre-fix descriptions that claimed the bare path on
/// macOS, and fails again if anyone reintroduces the claim.
#[test]
fn daemon_schema_descriptions_document_per_uid_socket_default() -> TestResult {
    const DAEMON_LIFECYCLE_SCHEMAS: &[&str] = &[
        REQUEST_SCHEMA_PATH,
        "docs/schemas/ee.daemon.start.v1.json",
        "docs/schemas/ee.daemon.stop.v1.json",
    ];
    // The pre-fix descriptions claimed this bare shared default on macOS.
    let forbidden_bare_default = "/tmp/ee-daemon.sock";
    // The true fallback documented by the resolver: a per-UID parent under
    // the temp root, alongside the XDG-first Linux default.
    let required_per_uid_fallback = "${TMPDIR:-/tmp}/ee-${uid}/daemon.sock";
    let required_xdg_default = "${XDG_RUNTIME_DIR}/ee/daemon.sock";

    for schema_path in DAEMON_LIFECYCLE_SCHEMAS {
        let path = repo_root().join(schema_path);
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if text.contains(forbidden_bare_default) {
            return Err(format!(
                "{schema_path} still documents the forbidden bare shared socket default \
                 {forbidden_bare_default}; the resolver publishes per-UID paths only \
                 (bd-3j0td, bd-10ex7, ADR 0055)"
            ));
        }
        if !text.contains(required_per_uid_fallback) {
            return Err(format!(
                "{schema_path} does not document the per-UID fallback \
                 {required_per_uid_fallback} that production `default_daemon_socket_path` uses"
            ));
        }
        if !text.contains(required_xdg_default) {
            return Err(format!(
                "{schema_path} does not document the XDG-first Linux default \
                 {required_xdg_default}"
            ));
        }
    }
    Ok(())
}

fn validate_json_schema(value: &Value, schema: &Value, path: &str) -> TestResult {
    ee::testing::validate_json_schema_instance(value, schema)
        .map_err(|error| format!("{path}: {error}"))
}
