use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ee::cass::{
    CASS_EXIT_DEGRADED, CASS_EXIT_OK, CassAgent, CassClient, CassContract, CassExitClass,
    CassInvocation, CassOutcome, CassSearchResponse, CassTimestamp, REQUIRED_API_VERSION,
    REQUIRED_CONTRACT_VERSION, STABLE_ENV_OVERRIDES,
};
use serde_json::{Map, Value};

type TestResult = Result<(), String>;

const FIXTURE_ROOT: &str = "tests/fixtures/cass/v1";
const REQUIRED_FEATURES: &[&str] = &[
    "api_version_command",
    "expand_command",
    "field_selection",
    "introspect_command",
    "json_output",
    "request_id",
    "robot_meta",
    "status_command",
    "timeout",
    "view_command",
];
const REQUIRED_CONNECTORS: &[&str] = &["claude_code", "codex"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequirementLevel {
    Must,
    Should,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ContractRequirement {
    id: &'static str,
    section: &'static str,
    level: RequirementLevel,
    description: &'static str,
    tested_by: &'static [&'static str],
}

const CASS_ROBOT_REQUIREMENTS: &[ContractRequirement] = &[
    ContractRequirement {
        id: "CASS-API-001",
        section: "api-version",
        level: RequirementLevel::Must,
        description: "api-version JSON declares the API and contract versions ee requires",
        tested_by: &[
            "cass_v1_capabilities_declare_the_robot_surfaces_ee_consumes",
            "unknown_cass_schema_versions_fail_with_adapter_schema_mismatch",
        ],
    },
    ContractRequirement {
        id: "CASS-CAP-001",
        section: "capabilities",
        level: RequirementLevel::Must,
        description: "capabilities JSON advertises every command and flag surface ee consumes",
        tested_by: &["cass_v1_capabilities_declare_the_robot_surfaces_ee_consumes"],
    },
    ContractRequirement {
        id: "CASS-CAP-002",
        section: "capabilities",
        level: RequirementLevel::Should,
        description: "fixture connectors include the harness agents used by ee tests",
        tested_by: &["cass_v1_capabilities_declare_the_robot_surfaces_ee_consumes"],
    },
    ContractRequirement {
        id: "CASS-SEARCH-001",
        section: "search",
        level: RequirementLevel::Must,
        description: "search root, hit, and robot-meta field sets match the schema snapshot",
        tested_by: &["cass_search_robot_fixture_conforms_to_schema_snapshot_and_parser"],
    },
    ContractRequirement {
        id: "CASS-SEARCH-002",
        section: "search",
        level: RequirementLevel::Must,
        description: "search parser maps provenance, ranking, freshness, timeout, and token fields",
        tested_by: &["cass_search_robot_fixture_conforms_to_schema_snapshot_and_parser"],
    },
    ContractRequirement {
        id: "CASS-SEARCH-003",
        section: "search",
        level: RequirementLevel::Must,
        description: "additive search fields remain forward-compatible while known-field type drift fails loudly",
        tested_by: &["cass_search_parser_handles_contract_extensions"],
    },
    ContractRequirement {
        id: "CASS-SEARCH-004",
        section: "search",
        level: RequirementLevel::Must,
        description: "request IDs and mirrored budget fields stay consistent across root and meta",
        tested_by: &["cass_search_robot_fixture_conforms_to_schema_snapshot_and_parser"],
    },
    ContractRequirement {
        id: "CASS-VIEW-001",
        section: "view-expand",
        level: RequirementLevel::Should,
        description: "view and expand fixtures keep the robot vocabulary ee links as evidence",
        tested_by: &["cass_search_view_and_expand_fixtures_match_robot_vocabulary"],
    },
    ContractRequirement {
        id: "CASS-SESS-001",
        section: "sessions",
        level: RequirementLevel::Should,
        description: "sessions fixture stays tiny, deterministic, and idempotency-ready",
        tested_by: &["tiny_fixture_session_set_is_idempotency_ready"],
    },
    ContractRequirement {
        id: "CASS-INV-001",
        section: "invocations",
        level: RequirementLevel::Must,
        description: "all cass subprocess invocations are noninteractive, budgeted, and reapable",
        tested_by: &["cass_invocations_are_noninteractive_budgeted_and_reapable"],
    },
    ContractRequirement {
        id: "CASS-STREAM-001",
        section: "stdout-stderr",
        level: RequirementLevel::Must,
        description: "stdout data and stderr diagnostics preserve success/degraded/failure classes",
        tested_by: &["cass_outcome_stream_contract_preserves_degraded_data"],
    },
    ContractRequirement {
        id: "CASS-DOCTOR-001",
        section: "doctor",
        level: RequirementLevel::Should,
        description: "doctor JSON fixture pins check names, status codes, and quarantine summary fields",
        tested_by: &["cass_doctor_fixture_pins_health_check_contract"],
    },
];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn fixture(name: &str) -> Result<Value, String> {
    let path = repo_path(&format!("{FIXTURE_ROOT}/{name}"));
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))
}

fn fixture_text(name: &str) -> Result<String, String> {
    let path = repo_path(&format!("{FIXTURE_ROOT}/{name}"));
    fs::read_to_string(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))
}

fn object<'a>(value: &'a Value, context: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{context} must be a JSON object"))
}

fn array<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a Vec<Value>, String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`{key}` must be an array"))
}

fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{key}` must be a string"))
}

fn number(object: &Map<String, Value>, key: &str) -> Result<u64, String> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("`{key}` must be an unsigned integer"))
}

fn boolean(object: &Map<String, Value>, key: &str) -> Result<bool, String> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("`{key}` must be a boolean"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_all_present(
    present: &BTreeSet<&str>,
    required: &[&str],
    description: &str,
) -> TestResult {
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|item| !present.contains(item))
        .collect();
    ensure(
        missing.is_empty(),
        format!("CASS capabilities missing {description}: {missing:?}"),
    )
}

fn field_names(value: &Value, context: &str) -> Result<BTreeSet<String>, String> {
    Ok(object(value, context)?.keys().cloned().collect())
}

fn schema_field_names(object: &Map<String, Value>, key: &str) -> Result<BTreeSet<String>, String> {
    array(object, key)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("schema `{key}` entries must be strings"))
        })
        .collect()
}

fn ensure_args(invocation_args: &[std::ffi::OsString], expected: &[&str]) -> TestResult {
    let actual: Result<Vec<&str>, String> = invocation_args
        .iter()
        .map(|arg| {
            arg.to_str()
                .ok_or_else(|| format!("non-UTF-8 invocation arg: {arg:?}"))
        })
        .collect();
    let actual = actual?;
    ensure(
        actual == expected,
        format!("expected args {expected:?}, got {actual:?}"),
    )
}

fn ensure_env_overrides(invocation: &CassInvocation) -> TestResult {
    let actual: Result<Vec<(&str, &str)>, String> = invocation
        .env_overrides()
        .iter()
        .map(|(key, value)| {
            let key = key
                .to_str()
                .ok_or_else(|| format!("non-UTF-8 env key: {key:?}"))?;
            let value = value
                .to_str()
                .ok_or_else(|| format!("non-UTF-8 env value for {key}: {value:?}"))?;
            Ok((key, value))
        })
        .collect();
    ensure(
        actual? == STABLE_ENV_OVERRIDES,
        "cass invocations must carry the stable noninteractive env overrides",
    )
}

fn ensure_search_accepts(value: &Value, message: &str) -> TestResult {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    CassSearchResponse::from_robot_json(&bytes)
        .map(|_| ())
        .map_err(|error| format!("{message}: {error}"))
}

fn ensure_search_rejects(value: &Value, message: &str) -> TestResult {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    ensure(
        CassSearchResponse::from_robot_json(&bytes).is_err(),
        message,
    )
}

#[test]
fn cass_robot_contract_coverage_matrix_has_no_unknown_gaps() -> TestResult {
    let mut ids = BTreeSet::new();
    let mut missing_tests = Vec::new();
    let mut section_counts: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    let mut covered = 0_u32;

    for requirement in CASS_ROBOT_REQUIREMENTS {
        ensure(
            ids.insert(requirement.id),
            format!(
                "duplicate CASS conformance requirement `{}`",
                requirement.id
            ),
        )?;
        ensure(
            !requirement.description.trim().is_empty(),
            format!("{} must describe the CASS contract clause", requirement.id),
        )?;
        let counts = section_counts.entry(requirement.section).or_default();
        match requirement.level {
            RequirementLevel::Must => counts.0 += 1,
            RequirementLevel::Should => counts.1 += 1,
        }
        if requirement.tested_by.is_empty() {
            missing_tests.push(requirement.id);
        } else {
            covered += 1;
        }
    }

    ensure(
        missing_tests.is_empty(),
        format!("untested CASS conformance requirements: {missing_tests:?}"),
    )?;
    ensure(
        section_counts
            .values()
            .all(|(must_count, should_count)| *must_count + *should_count > 0),
        "each CASS conformance section must contain at least one MUST or SHOULD clause",
    )?;
    ensure(
        covered
            == u32::try_from(CASS_ROBOT_REQUIREMENTS.len())
                .map_err(|error| format!("requirement count overflowed: {error}"))?,
        "all CASS conformance requirements must be covered by named tests",
    )
}

#[test]
fn cass_v1_capabilities_declare_the_robot_surfaces_ee_consumes() -> TestResult {
    let capabilities = fixture("capabilities.json")?;
    let root = object(&capabilities, "capabilities")?;

    ensure(
        number(root, "api_version")? == u64::from(REQUIRED_API_VERSION),
        "capabilities api_version must match ee's required version",
    )?;
    ensure(
        string(root, "contract_version")? == REQUIRED_CONTRACT_VERSION,
        "capabilities contract_version must match ee's required version",
    )?;

    let features: BTreeSet<&str> = array(root, "features")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "capabilities feature must be a string".to_string())
        })
        .collect::<Result<_, _>>()?;
    ensure_all_present(&features, REQUIRED_FEATURES, "required features")?;
    let connectors: BTreeSet<&str> = array(root, "connectors")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "capabilities connector must be a string".to_string())
        })
        .collect::<Result<_, _>>()?;
    ensure_all_present(&connectors, REQUIRED_CONNECTORS, "harness connectors")?;

    let contract = CassContract::new(
        string(root, "crate_version")?,
        u32::try_from(number(root, "api_version")?)
            .map_err(|error| format!("api_version is out of range: {error}"))?,
        string(root, "contract_version")?,
        features.iter().copied(),
    );
    contract
        .ensure_compatible()
        .map_err(|error| error.to_string())?;
    ensure(
        contract.missing_required_capabilities().is_empty(),
        format!(
            "required capability set drifted: {:?}",
            contract.missing_required_capabilities()
        ),
    )
}

#[test]
fn cass_search_robot_fixture_conforms_to_schema_snapshot_and_parser() -> TestResult {
    let schema = fixture("search_schema_snapshot.json")?;
    let schema_root = object(&schema, "schema snapshot")?;
    let search_schema = object(
        schema_root
            .get("search")
            .ok_or_else(|| "schema snapshot missing search object".to_string())?,
        "schema search",
    )?;

    let search = fixture("search_robot.json")?;
    ensure(
        field_names(&search, "search root")? == schema_field_names(search_schema, "root_fields")?,
        "search root fields must match the CASS schema snapshot",
    )?;
    let hit = array(object(&search, "search root")?, "hits")?
        .first()
        .ok_or_else(|| "search fixture must include a hit".to_string())?;
    ensure(
        field_names(hit, "search hit")? == schema_field_names(search_schema, "hit_fields")?,
        "search hit fields must match the CASS schema snapshot",
    )?;
    let meta = object(
        object(&search, "search root")?
            .get("_meta")
            .ok_or_else(|| "search fixture missing _meta".to_string())?,
        "search _meta",
    )?;
    ensure(
        meta.keys().cloned().collect::<BTreeSet<_>>()
            == schema_field_names(search_schema, "meta_fields")?,
        "search _meta fields must match the CASS schema snapshot",
    )?;
    let cache_stats = object(
        meta.get("cache_stats")
            .ok_or_else(|| "search _meta missing cache_stats".to_string())?,
        "cache_stats",
    )?;
    ensure(
        cache_stats.keys().cloned().collect::<BTreeSet<_>>()
            == schema_field_names(search_schema, "cache_stats_fields")?,
        "cache stats fields must match the CASS schema snapshot",
    )?;
    let timing = object(
        meta.get("timing")
            .ok_or_else(|| "search _meta missing timing".to_string())?,
        "timing",
    )?;
    ensure(
        timing.keys().cloned().collect::<BTreeSet<_>>()
            == schema_field_names(search_schema, "timing_fields")?,
        "timing fields must match the CASS schema snapshot",
    )?;
    let index_freshness = object(
        meta.get("index_freshness")
            .ok_or_else(|| "search _meta missing index_freshness".to_string())?,
        "index_freshness",
    )?;
    ensure(
        index_freshness.keys().cloned().collect::<BTreeSet<_>>()
            == schema_field_names(search_schema, "index_freshness_fields")?,
        "index freshness fields must match the CASS schema snapshot",
    )?;

    let search_text = fixture_text("search_robot.json")?;
    let parsed = CassSearchResponse::from_robot_json(search_text.as_bytes())
        .map_err(|error| error.to_string())?;
    ensure(parsed.query == "format before release", "parsed query")?;
    ensure(parsed.limit == 2, "parsed limit")?;
    ensure(parsed.offset == 0, "parsed offset")?;
    ensure(parsed.count == 1, "parsed count")?;
    ensure(parsed.total_matches == 1, "parsed total_matches")?;
    ensure(parsed.max_tokens == Some(200), "parsed root max_tokens")?;
    ensure(
        parsed.request_id.as_deref() == Some("ee-gate6-search-001"),
        "parsed request_id",
    )?;
    ensure(parsed.cursor.is_none(), "parsed cursor")?;
    ensure(!parsed.hits_clamped, "parsed hits_clamped")?;
    ensure(parsed.hits.len() == 1, "parsed hit count")?;

    let parsed_hit = parsed
        .hits
        .first()
        .ok_or_else(|| "parsed response missing hit".to_string())?;
    ensure(
        parsed_hit.source_path == "/workspace/session-a.jsonl",
        "parsed hit source_path",
    )?;
    ensure(parsed_hit.line_number == Some(42), "parsed line_number")?;
    ensure(parsed_hit.agent == CassAgent::Codex, "parsed agent")?;
    ensure(
        parsed_hit.workspace.as_deref() == Some("/workspace"),
        "parsed workspace",
    )?;
    ensure(
        parsed_hit.workspace_original.as_deref() == Some("/remote/workspace"),
        "parsed workspace_original",
    )?;
    ensure(
        parsed_hit.title.as_deref() == Some("release prep"),
        "parsed title",
    )?;
    ensure(
        parsed_hit.content.as_deref() == Some("Run cargo fmt --check before release."),
        "parsed content",
    )?;
    ensure(
        parsed_hit.snippet.as_deref() == Some("cargo fmt --check"),
        "parsed snippet",
    )?;
    ensure(parsed_hit.score == Some(1.0), "parsed score")?;
    match parsed_hit.created_at.as_ref() {
        Some(CassTimestamp::String(timestamp)) => {
            ensure(timestamp == "2026-04-30T00:00:00Z", "parsed created_at")?;
        }
        other => return Err(format!("parsed created_at had unexpected shape: {other:?}")),
    }
    ensure(
        parsed_hit.match_type.as_deref() == Some("lexical"),
        "parsed match_type",
    )?;
    ensure(parsed_hit.source_id == "local", "parsed source_id")?;
    ensure(parsed_hit.origin_kind == "local", "parsed origin_kind")?;
    ensure(parsed_hit.origin_host.is_none(), "parsed origin_host")?;

    let aggregations = parsed
        .aggregations
        .as_ref()
        .ok_or_else(|| "parsed response missing aggregations".to_string())?;
    let agent_buckets = aggregations
        .get("agent")
        .ok_or_else(|| "parsed response missing agent aggregation".to_string())?;
    ensure(agent_buckets.len() == 1, "parsed aggregation count")?;
    let agent_bucket = agent_buckets
        .first()
        .ok_or_else(|| "parsed response missing agent aggregation bucket".to_string())?;
    ensure(agent_bucket.key == "codex", "parsed aggregation key")?;
    ensure(agent_bucket.count == 1, "parsed aggregation count value")?;

    ensure(parsed.warning.is_none(), "parsed warning")?;
    ensure(parsed.meta.elapsed_ms == 12, "parsed elapsed_ms")?;
    ensure(
        parsed.meta.search_mode.as_deref() == Some("lexical"),
        "parsed search_mode",
    )?;
    ensure(
        parsed.meta.requested_search_mode.as_deref() == Some("hybrid"),
        "parsed requested_search_mode",
    )?;
    ensure(
        parsed.meta.mode_defaulted == Some(false),
        "parsed mode_defaulted",
    )?;
    ensure(
        parsed.meta.fallback_tier.as_deref() == Some("lexical"),
        "parsed fallback_tier",
    )?;
    ensure(
        parsed.meta.fallback_reason.as_deref() == Some("semantic context unavailable in fixture"),
        "parsed fallback_reason",
    )?;
    ensure(
        parsed.meta.semantic_refinement == Some(false),
        "parsed semantic_refinement",
    )?;
    ensure(!parsed.meta.wildcard_fallback, "parsed wildcard_fallback")?;
    ensure(parsed.meta.cache_stats.hits == 0, "parsed cache hits")?;
    ensure(parsed.meta.cache_stats.misses == 1, "parsed cache misses")?;
    ensure(
        parsed.meta.cache_stats.shortfall == 0,
        "parsed cache shortfall",
    )?;
    ensure(parsed.meta.timing.search_ms == 9, "parsed search_ms")?;
    ensure(parsed.meta.timing.rerank_ms == 0, "parsed rerank_ms")?;
    ensure(parsed.meta.timing.other_ms == 3, "parsed other_ms")?;
    ensure(
        parsed.meta.tokens_estimated == Some(24),
        "parsed tokens_estimated",
    )?;
    ensure(
        parsed.meta.max_tokens == Some(200),
        "parsed meta max_tokens",
    )?;
    ensure(
        parsed.meta.request_id.as_deref() == Some("ee-gate6-search-001"),
        "parsed meta request_id",
    )?;
    ensure(
        parsed.meta.request_id.as_deref() == parsed.request_id.as_deref(),
        "root and meta request_id must match for audit correlation",
    )?;
    ensure(parsed.meta.next_cursor.is_none(), "parsed next_cursor")?;
    ensure(!parsed.meta.hits_clamped, "parsed meta hits_clamped")?;
    ensure(
        parsed.meta.hits_clamped == parsed.hits_clamped,
        "root and meta hits_clamped must match",
    )?;
    ensure(
        parsed.meta.max_tokens == parsed.max_tokens,
        "root and meta max_tokens must match",
    )?;
    ensure(
        parsed.meta.state.get("status").and_then(Value::as_str) == Some("fixture"),
        "parsed meta state",
    )?;
    ensure(parsed.meta.index_freshness.exists, "parsed index exists")?;
    ensure(
        parsed.meta.index_freshness.status == "fresh",
        "parsed index status",
    )?;
    ensure(
        parsed.meta.index_freshness.reason.is_none(),
        "parsed index reason",
    )?;
    ensure(parsed.meta.index_freshness.fresh, "parsed index fresh")?;
    ensure(
        parsed.meta.index_freshness.last_indexed_at.as_deref() == Some("2026-04-30T00:00:00Z"),
        "parsed last_indexed_at",
    )?;
    ensure(
        parsed.meta.index_freshness.age_seconds == Some(1),
        "parsed age_seconds",
    )?;
    ensure(!parsed.meta.index_freshness.stale, "parsed stale")?;
    ensure(
        parsed.meta.index_freshness.stale_threshold_seconds == 300,
        "parsed stale threshold",
    )?;
    ensure(!parsed.meta.index_freshness.rebuilding, "parsed rebuilding")?;
    ensure(
        parsed.meta.index_freshness.pending_sessions == 0,
        "parsed pending sessions",
    )?;
    ensure(parsed.meta.timeout_ms == Some(30_000), "parsed timeout_ms")?;
    ensure(parsed.meta.timed_out == Some(false), "parsed timed_out")?;
    ensure(
        parsed.meta.partial_results == Some(false),
        "parsed partial_results",
    )?;
    ensure(parsed.meta.ann_stats.is_none(), "parsed ann_stats")?;
    ensure(parsed.suggestions.len() == 1, "parsed suggestions")?;
    ensure(parsed.explanation.is_some(), "parsed explanation")?;
    ensure(parsed.timeout.is_none(), "parsed timeout")
}

#[test]
fn cass_search_parser_handles_contract_extensions() -> TestResult {
    let search = fixture("search_robot.json")?;

    let mut root_extra = search.clone();
    root_extra
        .as_object_mut()
        .ok_or_else(|| "root fixture must be an object".to_string())?
        .insert("surprise".to_string(), Value::Bool(true));
    ensure_search_accepts(&root_extra, "additive root field must be tolerated")?;

    let mut hit_extra = search.clone();
    let hit = hit_extra
        .get_mut("hits")
        .and_then(Value::as_array_mut)
        .and_then(|hits| hits.first_mut())
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "search fixture must contain a hit object".to_string())?;
    hit.insert("surprise".to_string(), Value::Bool(true));
    ensure_search_accepts(&hit_extra, "additive hit field must be tolerated")?;

    let mut aggregation_extra = search.clone();
    let bucket = aggregation_extra
        .get_mut("aggregations")
        .and_then(Value::as_object_mut)
        .and_then(|aggregations| aggregations.get_mut("agent"))
        .and_then(Value::as_array_mut)
        .and_then(|buckets| buckets.first_mut())
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "search fixture must contain an aggregation bucket".to_string())?;
    bucket.insert("surprise".to_string(), Value::Bool(true));
    ensure_search_accepts(
        &aggregation_extra,
        "additive aggregation bucket field must be tolerated",
    )?;

    let mut meta_extra = search.clone();
    let meta = meta_extra
        .get_mut("_meta")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "search fixture must contain _meta object".to_string())?;
    meta.insert("surprise".to_string(), Value::Bool(true));
    ensure_search_accepts(&meta_extra, "additive _meta field must be tolerated")?;

    let mut cache_stats_extra = search.clone();
    let cache_stats = cache_stats_extra
        .pointer_mut("/_meta/cache_stats")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "search fixture must contain _meta.cache_stats object".to_string())?;
    cache_stats.insert("surprise".to_string(), Value::Bool(true));
    ensure_search_accepts(
        &cache_stats_extra,
        "additive cache_stats field must be tolerated",
    )?;

    let mut timing_extra = search.clone();
    let timing = timing_extra
        .pointer_mut("/_meta/timing")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "search fixture must contain _meta.timing object".to_string())?;
    timing.insert("surprise".to_string(), Value::Bool(true));
    ensure_search_accepts(&timing_extra, "additive timing field must be tolerated")?;

    let mut freshness_extra = search;
    let index_freshness = freshness_extra
        .pointer_mut("/_meta/index_freshness")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "search fixture must contain _meta.index_freshness object".to_string())?;
    index_freshness.insert("surprise".to_string(), Value::Bool(true));
    ensure_search_accepts(
        &freshness_extra,
        "additive index_freshness field must be tolerated",
    )?;

    let mut type_drift = fixture("search_robot.json")?;
    type_drift
        .as_object_mut()
        .ok_or_else(|| "root fixture must be an object".to_string())?
        .insert(
            "limit".to_string(),
            Value::String("not-a-number".to_string()),
        );
    ensure_search_rejects(&type_drift, "known field type drift must still be rejected")
}

#[test]
fn unknown_cass_schema_versions_fail_with_adapter_schema_mismatch() -> TestResult {
    let future = fixture("api_version.future.json")?;
    let root = object(&future, "future api-version")?;
    let contract = CassContract::new(
        string(root, "crate_version")?,
        u32::try_from(number(root, "api_version")?)
            .map_err(|error| format!("api_version is out of range: {error}"))?,
        string(root, "contract_version")?,
        ["json_output"],
    );

    let error = match contract.ensure_compatible() {
        Ok(()) => return Err("future CASS schema must not be accepted".to_string()),
        Err(error) => error,
    };
    ensure(
        error.kind_str() == "external_adapter_schema_mismatch",
        format!("unexpected error kind `{}`", error.kind_str()),
    )?;
    ensure(
        error.repair_hint() == Some("upgrade cass to a compatible contract version"),
        "schema mismatch should carry a repair hint",
    )
}

#[test]
fn cass_search_view_and_expand_fixtures_match_robot_vocabulary() -> TestResult {
    let search = fixture("search_robot.json")?;
    let search_root = object(&search, "search robot")?;
    ensure(
        string(search_root, "query")? == "format before release",
        "query",
    )?;
    ensure(number(search_root, "limit")? == 2, "limit")?;
    ensure(!boolean(search_root, "hits_clamped")?, "hits_clamped")?;
    ensure(
        string(search_root, "request_id")? == "ee-gate6-search-001",
        "request_id",
    )?;
    let hit_value = array(search_root, "hits")?
        .first()
        .ok_or_else(|| "search fixture must include at least one hit".to_string())?;
    let hit = object(hit_value, "hits[0]")?;
    ensure(
        string(hit, "source_path")?.ends_with("session-a.jsonl"),
        "hit source_path",
    )?;
    ensure(number(hit, "line_number")? == 42, "hit line_number")?;
    ensure(string(hit, "agent")? == "codex", "hit agent")?;
    let meta = object(
        search_root
            .get("_meta")
            .ok_or_else(|| "search fixture missing _meta".to_string())?,
        "_meta",
    )?;
    ensure(number(meta, "max_tokens")? == 200, "max_tokens")?;
    ensure(number(meta, "timeout_ms")? == 30_000, "timeout_ms")?;

    let view = fixture("view.json")?;
    let view_root = object(&view, "view")?;
    ensure(number(view_root, "target_line")? == 2, "view target_line")?;
    ensure(number(view_root, "context")? == 1, "view context")?;
    let view_lines = array(view_root, "lines")?;
    ensure(view_lines.len() == 3, "view fixture line count")?;
    let highlighted_value = view_lines
        .get(1)
        .ok_or_else(|| "view fixture must include a highlighted target line".to_string())?;
    let highlighted = object(highlighted_value, "view lines[1]")?;
    ensure(
        boolean(highlighted, "highlighted")?,
        "view target highlighted",
    )?;

    let expand = fixture("expand.json")?;
    let messages = expand
        .as_array()
        .ok_or_else(|| "expand fixture must be a JSON array".to_string())?;
    ensure(messages.len() == 3, "expand message count")?;
    let target_value = messages
        .get(1)
        .ok_or_else(|| "expand fixture must include a target message".to_string())?;
    let target = object(target_value, "expand[1]")?;
    ensure(boolean(target, "is_target")?, "expand target flag")?;
    ensure(string(target, "role")? == "assistant", "expand target role")
}

#[test]
fn cass_outcome_stream_contract_preserves_degraded_data() -> TestResult {
    let invocation = CassInvocation::new("cass", ["search", "format before release", "--robot"]);
    let degraded = CassOutcome::synthetic(
        invocation.clone(),
        br#"{"hits":[]}"#.to_vec(),
        br#"{"error":{"kind":"semantic_index_unavailable"}}"#.to_vec(),
        Some(CASS_EXIT_DEGRADED),
    );
    ensure(
        degraded.class() == CassExitClass::Degraded,
        "nonzero cass exit with JSON stdout must be degraded, not failure",
    )?;
    ensure(!degraded.stdout_is_empty(), "degraded stdout is data")?;
    ensure(!degraded.stderr_is_empty(), "degraded stderr is diagnostic")?;

    let success = CassOutcome::synthetic(
        invocation.clone(),
        br#"{"hits":[]}"#.to_vec(),
        Vec::new(),
        Some(CASS_EXIT_OK),
    );
    ensure(
        success.class() == CassExitClass::Success,
        "zero cass exit with JSON stdout must be success",
    )?;
    ensure(
        success.stderr_is_empty(),
        "success stderr stays diagnostic-only",
    )?;

    let failure = CassOutcome::synthetic(invocation, Vec::new(), b"boom\n".to_vec(), Some(2));
    ensure(
        failure.class() == CassExitClass::Failure,
        "nonzero cass exit without stdout has no usable payload",
    )?;
    ensure(failure.stdout_is_empty(), "failure stdout is empty")
}

#[test]
fn cass_invocations_are_noninteractive_budgeted_and_reapable() -> TestResult {
    let client = CassClient::new_default().with_timeout(Duration::from_secs(30));
    let preflight = client.preflight_invocations();
    ensure(preflight.len() == 3, "preflight invocation count")?;
    let api_version = preflight
        .first()
        .ok_or_else(|| "missing api-version preflight invocation".to_string())?;
    let capabilities = preflight
        .get(1)
        .ok_or_else(|| "missing capabilities preflight invocation".to_string())?;
    let introspect = preflight
        .get(2)
        .ok_or_else(|| "missing introspect preflight invocation".to_string())?;
    ensure_args(api_version.args(), &["api-version", "--json"])?;
    ensure_args(capabilities.args(), &["capabilities", "--json"])?;
    ensure_args(introspect.args(), &["introspect", "--json"])?;
    ensure_env_overrides(api_version)?;
    ensure_env_overrides(capabilities)?;
    ensure_env_overrides(introspect)?;

    let search = client.search_invocation("format before release", "ee-gate6-search-001", 2, 200);
    ensure_args(
        search.args(),
        &[
            "search",
            "format before release",
            "--robot",
            "--robot-meta",
            "--fields",
            "minimal",
            "--limit",
            "2",
            "--max-tokens",
            "200",
            "--timeout",
            "30000",
            "--request-id",
            "ee-gate6-search-001",
        ],
    )?;
    ensure(
        search.timeout() == Some(Duration::from_secs(30)),
        "search timeout",
    )?;
    ensure_env_overrides(&search)?;

    let view = client.view_invocation("/workspace/session-a.jsonl", 42, 1);
    ensure_args(
        view.args(),
        &[
            "view",
            "-n",
            "42",
            "-C",
            "1",
            "--json",
            "--",
            "/workspace/session-a.jsonl",
        ],
    )?;
    ensure(
        view.timeout() == Some(Duration::from_secs(30)),
        "view timeout",
    )?;
    ensure_env_overrides(&view)?;

    let expand = client.expand_invocation("/workspace/session-a.jsonl", 42, 1);
    ensure_args(
        expand.args(),
        &[
            "expand",
            "-n",
            "42",
            "-C",
            "1",
            "--json",
            "--",
            "/workspace/session-a.jsonl",
        ],
    )?;
    ensure(
        expand.timeout() == Some(Duration::from_secs(30)),
        "expand timeout",
    )?;
    ensure_env_overrides(&expand)
}

#[test]
fn tiny_fixture_session_set_is_idempotency_ready() -> TestResult {
    let sessions = fixture("sessions.json")?;
    let root = object(&sessions, "sessions")?;
    let list = array(root, "sessions")?;
    ensure(list.len() == 1, "fixture must stay tiny and deterministic")?;
    let session_value = list
        .first()
        .ok_or_else(|| "sessions fixture must include one session".to_string())?;
    let session = object(session_value, "sessions[0]")?;
    ensure(
        string(session, "path")? == "/workspace/session-a.jsonl",
        "session path",
    )?;
    ensure(string(session, "agent")? == "codex", "session agent")?;
    ensure(number(session, "message_count")? == 2, "message count")?;
    ensure(
        !string(session, "content_hash")?.trim().is_empty(),
        "content hash",
    )?;

    let client = CassClient::new_default().with_timeout(Duration::from_secs(30));
    let invocation = client.sessions_invocation(Path::new("/workspace"), 1);
    ensure_args(
        invocation.args(),
        &[
            "sessions",
            "--workspace",
            "/workspace",
            "--json",
            "--limit",
            "1",
        ],
    )?;
    ensure_env_overrides(&invocation)
}

#[test]
fn cass_doctor_fixture_pins_health_check_contract() -> TestResult {
    let doctor = fixture("doctor.json")?;
    let root = object(&doctor, "doctor")?;

    ensure(
        string(root, "status")? == "healthy" || string(root, "status")? == "unhealthy",
        "doctor status must be healthy or unhealthy",
    )?;
    ensure(
        root.contains_key("healthy"),
        "doctor must have boolean healthy field",
    )?;
    ensure(
        root.contains_key("initialized"),
        "doctor must have boolean initialized field",
    )?;
    ensure(
        root.contains_key("issues_found"),
        "doctor must report issues_found count",
    )?;
    ensure(
        root.contains_key("failures"),
        "doctor must report failures count",
    )?;
    ensure(
        root.contains_key("warnings"),
        "doctor must report warnings count",
    )?;
    ensure(
        root.contains_key("needs_rebuild"),
        "doctor must report needs_rebuild flag",
    )?;

    let checks = array(root, "checks")?;
    ensure(!checks.is_empty(), "doctor must include at least one check")?;
    let first_check = object(
        checks
            .first()
            .ok_or_else(|| "checks array unexpectedly empty".to_string())?,
        "checks[0]",
    )?;
    ensure(
        first_check.contains_key("name"),
        "check must have name field",
    )?;
    ensure(
        first_check.contains_key("status"),
        "check must have status field",
    )?;
    ensure(
        first_check.contains_key("message"),
        "check must have message field",
    )?;
    ensure(
        first_check.contains_key("fix_available"),
        "check must have fix_available field",
    )?;

    let check_names: BTreeSet<&str> = checks
        .iter()
        .filter_map(|c| c.get("name").and_then(Value::as_str))
        .collect();
    for required_check in ["data_directory", "database", "index"] {
        ensure(
            check_names.contains(required_check),
            format!("doctor must include '{required_check}' check"),
        )?;
    }

    ensure(
        root.contains_key("quarantine"),
        "doctor must include quarantine summary",
    )?;
    let quarantine = object(
        root.get("quarantine")
            .ok_or_else(|| "quarantine field missing".to_string())?,
        "quarantine",
    )?;
    ensure(
        quarantine.contains_key("summary"),
        "quarantine must have summary",
    )?;

    ensure(root.contains_key("_meta"), "doctor must include _meta")?;
    let meta = object(
        root.get("_meta")
            .ok_or_else(|| "_meta field missing".to_string())?,
        "_meta",
    )?;
    ensure(
        meta.contains_key("elapsed_ms"),
        "_meta must include elapsed_ms",
    )?;
    ensure(meta.contains_key("data_dir"), "_meta must include data_dir")?;
    ensure(meta.contains_key("db_path"), "_meta must include db_path")?;
    ensure(meta.contains_key("fix_mode"), "_meta must include fix_mode")?;

    Ok(())
}
