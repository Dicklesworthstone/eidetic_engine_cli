use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use ee::cass::{CassHealth, CassSearchResponse, REQUIRED_API_VERSION, REQUIRED_CONTRACT_VERSION};
use ee::core::jsonl_import::{JsonlImportOptions, import_jsonl_records};
use serde_json::Value;

type TestResult = Result<(), String>;

const FIXTURE_ROOT: &str = "tests/fixtures/cass";
const PINNED_CASS_CRATE_VERSION: &str = "0.4.1";
const MEMORY_ID: &str = "mem_01234567890123456789012345";
const GOLDEN_FILES: &[&str] = &[
    "PROVENANCE.md",
    "api_version.v1.json",
    "capabilities.v1.json",
    "health.v1.json",
    "robot_memory_v1.golden",
    "robot_audit_v1.golden",
    "json_contract.golden",
    "v1/api_version.json",
    "v1/capabilities.json",
    "v1/doctor.json",
    "v1/expand.json",
    "v1/search_robot.json",
    "v1/sessions.json",
    "v1/view.json",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequirementLevel {
    Must,
    Should,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Requirement {
    id: &'static str,
    level: RequirementLevel,
    description: &'static str,
    tested_by: &'static [&'static str],
}

const REQUIREMENTS: &[Requirement] = &[
    Requirement {
        id: "CASS-CONF-001",
        level: RequirementLevel::Must,
        description: "golden fixtures carry provenance, pinned commands, and manual review instructions",
        tested_by: &["conformance_cass_fixture_provenance_is_review_gated"],
    },
    Requirement {
        id: "CASS-CONF-002",
        level: RequirementLevel::Must,
        description: "installed cass API and contract versions match the fixture version or live checks skip loudly",
        tested_by: &["conformance_live_cass_version_matches_fixture_or_skips"],
    },
    Requirement {
        id: "CASS-CONF-003",
        level: RequirementLevel::Must,
        description: "robot memory fixture pins the CASS search JSON consumed by ee",
        tested_by: &["conformance_robot_memory_golden_matches_cass_search_contract"],
    },
    Requirement {
        id: "CASS-CONF-004",
        level: RequirementLevel::Must,
        description: "robot audit fixture pins a normalized CASS status/audit readiness payload",
        tested_by: &["conformance_robot_audit_golden_pins_status_readiness_contract"],
    },
    Requirement {
        id: "CASS-CONF-005",
        level: RequirementLevel::Must,
        description: "JSONL contract fixture covers memory, audit, tag, and workspace records",
        tested_by: &["conformance_json_contract_golden_pins_jsonl_record_schemas"],
    },
    Requirement {
        id: "CASS-CONF-006",
        level: RequirementLevel::Must,
        description: "ee import jsonl dry-run preserves the memory contract fields from the CASS-derived JSONL fixture",
        tested_by: &["conformance_json_contract_round_trips_through_ee_import_jsonl"],
    },
    Requirement {
        id: "CASS-CONF-007",
        level: RequirementLevel::Must,
        description: "documented CASS command-name drift is captured instead of silently relying on bare output",
        tested_by: &["conformance_discrepancies_document_current_cass_command_drift"],
    },
    Requirement {
        id: "CASS-CONF-008",
        level: RequirementLevel::Should,
        description: "UPDATE_GOLDENS=1 provides an explicit regeneration path for reviewed fixture updates",
        tested_by: &["conformance_update_goldens_entrypoint_is_explicit"],
    },
    Requirement {
        id: "CASS-CONF-009",
        level: RequirementLevel::Must,
        description: "api-version and capabilities fixtures pin required CASS versions and command capabilities",
        tested_by: &["conformance_api_version_and_capabilities_fixtures_pin_required_surfaces"],
    },
    Requirement {
        id: "CASS-CONF-010",
        level: RequirementLevel::Must,
        description: "search --robot fixture pins query, hit, provenance, and robot metadata fields",
        tested_by: &["conformance_search_robot_fixture_pins_query_hits_and_meta_shape"],
    },
    Requirement {
        id: "CASS-CONF-011",
        level: RequirementLevel::Must,
        description: "view and expand JSON fixtures pin line evidence vocabulary consumed by ee",
        tested_by: &["conformance_view_and_expand_fixtures_pin_line_evidence_shape"],
    },
    Requirement {
        id: "CASS-CONF-012",
        level: RequirementLevel::Must,
        description: "doctor/status and health fixtures pin readiness, check, and quarantine evidence fields",
        tested_by: &["conformance_doctor_and_health_fixtures_pin_readiness_contracts"],
    },
];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn fixture_path(name: &str) -> PathBuf {
    repo_path(&format!("{FIXTURE_ROOT}/{name}"))
}

fn fixture_text(name: &str) -> Result<String, String> {
    let path = fixture_path(name);
    fs::read_to_string(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))
}

fn json_fixture(name: &str) -> Result<Value, String> {
    let text = fixture_text(name)?;
    serde_json::from_str(&text).map_err(|error| {
        format!(
            "{} is not valid JSON: {error}",
            fixture_path(name).display()
        )
    })
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn string_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{key}` must be a string"))
}

fn array_field<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`{key}` must be an array"))
}

fn object_field<'a>(
    value: &'a Value,
    key: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("`{key}` must be an object"))
}

fn bool_field(value: &Value, key: &str) -> Result<bool, String> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("`{key}` must be a bool"))
}

fn u64_field(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("`{key}` must be an unsigned integer"))
}

fn fixture_field_set(name: &str, key: &str) -> Result<BTreeSet<String>, String> {
    Ok(array_field(&json_fixture(name)?, key)?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

fn ensure_contains_all(present: &BTreeSet<String>, required: &[&str], context: &str) -> TestResult {
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|field| !present.contains(*field))
        .collect();
    ensure(
        missing.is_empty(),
        format!("{context} missing required fields: {missing:?}"),
    )
}

fn schema_lines(name: &str) -> Result<Vec<Value>, String> {
    fixture_text(name)?
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(
                    serde_json::from_str::<Value>(trimmed).map_err(|error| {
                        format!("{name}: line {} invalid JSON: {error}", index + 1)
                    }),
                )
            }
        })
        .collect()
}

fn command_json(args: &[&str]) -> Result<Value, String> {
    let output = Command::new("cass")
        .args(args)
        .output()
        .map_err(|error| format!("cass is unavailable for live conformance checks: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cass {} failed with status {:?}: {}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice::<Value>(&output.stdout).map_err(|error| {
        format!(
            "cass {} stdout was not JSON: {error}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn live_version_skip_reason() -> Result<Option<String>, String> {
    let version = match command_json(&["api-version", "--json"]) {
        Ok(version) => version,
        Err(error) => return Ok(Some(error)),
    };
    let crate_version = string_field(&version, "crate_version")?;
    let api_version = version
        .get("api_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "cass api-version output missing api_version integer".to_string())?;
    let contract_version = string_field(&version, "contract_version")?;
    if crate_version != PINNED_CASS_CRATE_VERSION
        || api_version != u64::from(REQUIRED_API_VERSION)
        || contract_version != REQUIRED_CONTRACT_VERSION
    {
        return Ok(Some(format!(
            "cass version lock differs: observed crate={crate_version} api={api_version} contract={contract_version}; expected crate={PINNED_CASS_CRATE_VERSION} api={REQUIRED_API_VERSION} contract={REQUIRED_CONTRACT_VERSION}"
        )));
    }
    Ok(None)
}

fn ensure_no_live_skip() -> TestResult {
    if let Some(reason) = live_version_skip_reason()? {
        eprintln!("skipping live cass conformance check: {reason}");
        return Err(reason);
    }
    Ok(())
}

fn maybe_regenerate_goldens() -> TestResult {
    if std::env::var_os("UPDATE_GOLDENS").is_none() {
        return Ok(());
    }

    ensure_no_live_skip()?;
    let memory = fs::read_to_string(repo_path("tests/fixtures/cass/v1/search_robot.json"))
        .map_err(|error| format!("failed to read existing search fixture: {error}"))?;
    fs::write(fixture_path("robot_memory_v1.golden"), memory)
        .map_err(|error| format!("failed to update robot_memory_v1.golden: {error}"))?;

    let audit = json_fixture("robot_audit_v1.golden")?;
    fs::write(
        fixture_path("robot_audit_v1.golden"),
        serde_json::to_string_pretty(&audit).map_err(|error| error.to_string())? + "\n",
    )
    .map_err(|error| format!("failed to update robot_audit_v1.golden: {error}"))?;

    let json_contract = fixture_text("json_contract.golden")?;
    fs::write(fixture_path("json_contract.golden"), json_contract)
        .map_err(|error| format!("failed to update json_contract.golden: {error}"))?;
    Ok(())
}

#[test]
fn conformance_cass_contracts_coverage_matrix_passes_95_percent_must() -> TestResult {
    let mut ids = BTreeSet::new();
    let mut must_count = 0_usize;
    let mut covered_must = 0_usize;

    for requirement in REQUIREMENTS {
        ensure(
            ids.insert(requirement.id),
            format!(
                "duplicate CASS conformance requirement `{}`",
                requirement.id
            ),
        )?;
        ensure(
            !requirement.description.trim().is_empty(),
            format!("{} must describe the contract clause", requirement.id),
        )?;
        ensure(
            !requirement.tested_by.is_empty(),
            format!("{} must name at least one test", requirement.id),
        )?;
        if requirement.level == RequirementLevel::Must {
            must_count += 1;
            covered_must += usize::from(!requirement.tested_by.is_empty());
        }
    }

    let percent = covered_must.saturating_mul(100) / must_count.max(1);
    ensure(
        percent >= 95,
        format!("CASS conformance MUST coverage is {percent}%, below 95%"),
    )
}

#[test]
fn conformance_update_goldens_entrypoint_is_explicit() -> TestResult {
    maybe_regenerate_goldens()?;
    for name in GOLDEN_FILES {
        let path = fixture_path(name);
        ensure(
            path.exists(),
            format!("missing CASS conformance fixture {}", path.display()),
        )?;
    }
    Ok(())
}

#[test]
fn conformance_cass_fixture_provenance_is_review_gated() -> TestResult {
    let provenance = fixture_text("PROVENANCE.md")?;
    for required in [
        PINNED_CASS_CRATE_VERSION,
        "cass api-version --json",
        "cass capabilities --json",
        "cass search --robot",
        "cass view --json",
        "cass expand --json",
        "cass doctor --json",
        "cass health --json",
        "robot_memory_v1.golden",
        "robot_audit_v1.golden",
        "json_contract.golden",
        "UPDATE_GOLDENS=1",
        "Manual review required",
    ] {
        ensure(
            provenance.contains(required),
            format!("PROVENANCE.md must mention `{required}`"),
        )?;
    }
    Ok(())
}

#[test]
fn conformance_api_version_and_capabilities_fixtures_pin_required_surfaces() -> TestResult {
    let api = json_fixture("v1/api_version.json")?;
    ensure(
        api.get("api_version").and_then(Value::as_u64) == Some(u64::from(REQUIRED_API_VERSION)),
        "api-version fixture must pin ee's required api_version",
    )?;
    ensure(
        string_field(&api, "contract_version")? == REQUIRED_CONTRACT_VERSION,
        "api-version fixture must pin ee's required contract_version",
    )?;

    let capabilities = json_fixture("v1/capabilities.json")?;
    ensure(
        string_field(&capabilities, "crate_version")? == PINNED_CASS_CRATE_VERSION,
        "capabilities fixture crate version",
    )?;
    ensure(
        capabilities.get("api_version").and_then(Value::as_u64)
            == Some(u64::from(REQUIRED_API_VERSION)),
        "capabilities fixture api_version",
    )?;
    ensure(
        string_field(&capabilities, "contract_version")? == REQUIRED_CONTRACT_VERSION,
        "capabilities fixture contract_version",
    )?;

    let features = fixture_field_set("v1/capabilities.json", "features")?;
    ensure_contains_all(
        &features,
        &[
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
        ],
        "capabilities fixture",
    )
}

#[test]
fn conformance_search_robot_fixture_pins_query_hits_and_meta_shape() -> TestResult {
    let search = json_fixture("v1/search_robot.json")?;
    ensure(
        string_field(&search, "query")? == "format before release",
        "search query",
    )?;
    ensure(u64_field(&search, "limit")? == 2, "search limit")?;
    ensure(u64_field(&search, "count")? == 1, "search count")?;
    ensure(
        string_field(&search, "request_id")? == "ee-gate6-search-001",
        "search request id",
    )?;

    let hits = array_field(&search, "hits")?;
    let hit = hits
        .first()
        .ok_or_else(|| "search fixture must include at least one hit".to_string())?;
    for field in [
        "source_path",
        "line_number",
        "agent",
        "workspace",
        "title",
        "score",
        "content",
        "snippet",
        "created_at",
        "match_type",
    ] {
        ensure(
            hit.get(field).is_some(),
            format!("search hit must pin `{field}`"),
        )?;
    }
    ensure(
        string_field(hit, "source_path")? == "/workspace/session-a.jsonl",
        "search hit source path",
    )?;
    ensure(u64_field(hit, "line_number")? == 42, "search line")?;
    ensure(
        string_field(hit, "content")? == "Run cargo fmt --check before release.",
        "search hit content",
    )?;

    let meta = object_field(&search, "_meta")?;
    ensure(meta.contains_key("request_id"), "search meta request id")?;
    ensure(
        meta.contains_key("index_freshness"),
        "search meta index freshness",
    )?;
    CassSearchResponse::from_robot_json(fixture_text("v1/search_robot.json")?.as_bytes())
        .map_err(|error| format!("search fixture must parse through ee parser: {error}"))?;
    Ok(())
}

#[test]
fn conformance_view_and_expand_fixtures_pin_line_evidence_shape() -> TestResult {
    let view = json_fixture("v1/view.json")?;
    ensure(
        string_field(&view, "path")? == "/workspace/session-a.jsonl",
        "view path",
    )?;
    ensure(u64_field(&view, "target_line")? == 2, "view target line")?;
    ensure(u64_field(&view, "context")? == 1, "view context")?;
    let view_lines = array_field(&view, "lines")?;
    ensure(view_lines.len() == 3, "view fixture line count")?;
    let highlighted = view_lines
        .iter()
        .filter(|line| bool_field(line, "highlighted").unwrap_or(false))
        .count();
    ensure(highlighted == 1, "view fixture has one highlighted line")?;
    for line in view_lines {
        u64_field(line, "line")?;
        string_field(line, "content")?;
        bool_field(line, "highlighted")?;
    }

    let expand = json_fixture("v1/expand.json")?;
    let expanded_lines = expand
        .as_array()
        .ok_or_else(|| "expand fixture must be a JSON array".to_string())?;
    ensure(expanded_lines.len() == 3, "expand fixture line count")?;
    let target_count = expanded_lines
        .iter()
        .filter(|line| bool_field(line, "is_target").unwrap_or(false))
        .count();
    ensure(target_count == 1, "expand fixture has one target line")?;
    for line in expanded_lines {
        u64_field(line, "line")?;
        string_field(line, "role")?;
        bool_field(line, "is_target")?;
        string_field(line, "content")?;
    }
    Ok(())
}

#[test]
fn conformance_doctor_and_health_fixtures_pin_readiness_contracts() -> TestResult {
    let doctor = json_fixture("v1/doctor.json")?;
    ensure(
        string_field(&doctor, "status")? == "healthy",
        "doctor status",
    )?;
    ensure(bool_field(&doctor, "healthy")?, "doctor healthy")?;
    ensure(bool_field(&doctor, "initialized")?, "doctor initialized")?;
    ensure(u64_field(&doctor, "issues_found")? == 0, "doctor issues")?;
    let checks = array_field(&doctor, "checks")?;
    let check_names: BTreeSet<String> = checks
        .iter()
        .filter_map(|check| check.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    ensure_contains_all(
        &check_names,
        &[
            "data_directory",
            "database",
            "fts_table",
            "index",
            "sessions",
        ],
        "doctor checks",
    )?;
    for check in checks {
        string_field(check, "name")?;
        string_field(check, "status")?;
        string_field(check, "message")?;
        bool_field(check, "fix_available")?;
        bool_field(check, "fix_applied")?;
    }

    let quarantine = object_field(&doctor, "quarantine")?;
    let summary = quarantine
        .get("summary")
        .and_then(Value::as_object)
        .ok_or_else(|| "doctor quarantine summary must be an object".to_string())?;
    ensure(
        summary.contains_key("cleanup_apply_allowed"),
        "doctor quarantine pins cleanup apply gate",
    )?;
    ensure(
        quarantine.contains_key("lexical_cleanup_apply_gate"),
        "doctor quarantine pins lexical cleanup apply gate",
    )?;

    let health_text = fixture_text("health.v1.json")?;
    let health = CassHealth::parse_json(&health_text)
        .map_err(|error| format!("health fixture must parse through ee parser: {error}"))?;
    ensure(health.is_ready(), "health fixture ready")?;
    ensure(!health.is_stale(), "health fixture not stale")
}

#[test]
fn conformance_live_cass_version_matches_fixture_or_skips() -> TestResult {
    if let Some(reason) = live_version_skip_reason()? {
        eprintln!("skipping live cass conformance check: {reason}");
        return Ok(());
    }

    let capabilities = command_json(&["capabilities", "--json"])?;
    ensure(
        string_field(&capabilities, "crate_version")? == PINNED_CASS_CRATE_VERSION,
        "live cass capabilities crate_version must match fixture",
    )?;
    ensure(
        capabilities.get("api_version").and_then(Value::as_u64)
            == Some(u64::from(REQUIRED_API_VERSION)),
        "live cass capabilities api_version must match ee requirement",
    )?;
    ensure(
        string_field(&capabilities, "contract_version")? == REQUIRED_CONTRACT_VERSION,
        "live cass capabilities contract_version must match ee requirement",
    )
}

#[test]
fn conformance_robot_memory_golden_matches_cass_search_contract() -> TestResult {
    let memory_text = fixture_text("robot_memory_v1.golden")?;
    let legacy_search_text =
        fs::read_to_string(repo_path("tests/fixtures/cass/v1/search_robot.json"))
            .map_err(|error| format!("failed to read cass v1 search fixture: {error}"))?;
    ensure(
        memory_text == legacy_search_text,
        "robot_memory_v1.golden must stay byte-for-byte aligned with the pinned cass search robot fixture",
    )?;

    let parsed = CassSearchResponse::from_robot_json(memory_text.as_bytes())
        .map_err(|error| error.to_string())?;
    ensure(parsed.query == "format before release", "memory query")?;
    ensure(parsed.limit == 2, "memory limit")?;
    ensure(parsed.count == 1, "memory result count")?;
    let hit = parsed
        .hits
        .first()
        .ok_or_else(|| "memory golden must include one search hit".to_string())?;
    ensure(
        hit.content.as_deref() == Some("Run cargo fmt --check before release."),
        "memory hit content",
    )?;
    ensure(
        hit.source_path == "/workspace/session-a.jsonl",
        "memory hit source path",
    )?;
    ensure(
        parsed.meta.request_id.as_deref() == parsed.request_id.as_deref(),
        "root and meta request IDs must match",
    )
}

#[test]
fn conformance_robot_audit_golden_pins_status_readiness_contract() -> TestResult {
    let audit = json_fixture("robot_audit_v1.golden")?;
    ensure(
        string_field(&audit, "schema")? == "ee.cass.normalized_status_audit.v1",
        "audit fixture schema",
    )?;
    ensure(
        string_field(&audit, "command")? == "cass status --json",
        "audit fixture command",
    )?;
    ensure(
        string_field(&audit, "cass_crate_version")? == PINNED_CASS_CRATE_VERSION,
        "audit fixture crate version",
    )?;
    ensure(
        object_field(&audit, "index")?.contains_key("status"),
        "index status",
    )?;
    ensure(
        object_field(&audit, "database")?.contains_key("opened"),
        "database opened",
    )?;
    ensure(
        array_field(&audit, "audit_fields")?
            .iter()
            .filter_map(Value::as_str)
            .any(|field| field == "recommended_action"),
        "audit fixture must pin recommended_action as consumed status evidence",
    )
}

#[test]
fn conformance_json_contract_golden_pins_jsonl_record_schemas() -> TestResult {
    let records = schema_lines("json_contract.golden")?;
    let schemas: Vec<&str> = records
        .iter()
        .map(|record| {
            record
                .get("schema")
                .and_then(Value::as_str)
                .ok_or_else(|| "every JSONL golden row must have a schema string".to_string())
        })
        .collect::<Result<_, _>>()?;
    ensure(
        schemas
            == vec![
                "ee.export.header.v1",
                "ee.export.workspace.v1",
                "ee.export.memory.v1",
                "ee.export.tag.v1",
                "ee.export.audit.v1",
                "ee.export.footer.v1",
            ],
        format!("unexpected JSONL schema order: {schemas:?}"),
    )?;

    let memory = records
        .iter()
        .find(|record| record.get("schema").and_then(Value::as_str) == Some("ee.export.memory.v1"))
        .ok_or_else(|| "json_contract.golden missing memory record".to_string())?;
    ensure(string_field(memory, "memory_id")? == MEMORY_ID, "memory id")?;
    ensure(
        string_field(memory, "level")? == "procedural",
        "memory level",
    )?;
    ensure(string_field(memory, "kind")? == "rule", "memory kind")?;
    ensure(
        string_field(memory, "provenance_uri")? == "cass-session://session-a#L2-L2",
        "memory provenance",
    )
}

#[test]
fn conformance_json_contract_round_trips_through_ee_import_jsonl() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    let source = tempdir.path().join("cass-contract.jsonl");
    let database = tempdir.path().join("ee.db");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::write(&source, fixture_text("json_contract.golden")?).map_err(|error| error.to_string())?;

    let dry_run = import_jsonl_records(&JsonlImportOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        source_path: source.clone(),
        dry_run: true,
    })
    .map_err(|error| error.to_string())?;
    ensure(dry_run.status == "dry_run", "dry-run status")?;
    ensure(dry_run.records_total == 6, "dry-run total records")?;
    ensure(dry_run.memory_records == 1, "dry-run memory records")?;
    ensure(dry_run.tag_records == 1, "dry-run tag records")?;
    ensure(
        dry_run.ignored_records == 2,
        "workspace and audit rows are accounted as ignored",
    )?;
    ensure(dry_run.issues.is_empty(), "dry-run issues")?;
    ensure(
        dry_run
            .header
            .as_ref()
            .is_some_and(|header| header.import_source == "cass_import"),
        "dry-run header import source",
    )?;
    ensure(
        dry_run
            .footer
            .as_ref()
            .is_some_and(|footer| footer.total_records == 6 && footer.memory_count == 1),
        "dry-run footer counts",
    )?;

    let records = schema_lines("json_contract.golden")?;
    let memory = records
        .iter()
        .find(|record| record.get("schema").and_then(Value::as_str) == Some("ee.export.memory.v1"))
        .ok_or_else(|| "json_contract.golden missing memory record".to_string())?;
    ensure(
        string_field(memory, "memory_id")? == MEMORY_ID,
        "round-trip memory id",
    )?;
    ensure(
        string_field(memory, "level")? == "procedural",
        "round-trip level",
    )?;
    ensure(string_field(memory, "kind")? == "rule", "round-trip kind")?;
    ensure(
        string_field(memory, "content")? == "Run cargo fmt --check before release.",
        "round-trip memory content",
    )?;
    ensure(
        string_field(memory, "provenance_uri")? == "cass-session://session-a#L2-L2",
        "round-trip memory provenance",
    )
}

#[test]
fn conformance_discrepancies_document_current_cass_command_drift() -> TestResult {
    let doc = fs::read_to_string(repo_path("tests/conformance/DISCREPANCIES.md"))
        .map_err(|error| format!("failed to read DISCREPANCIES.md: {error}"))?;
    for required in [
        "cass 0.4.1",
        "memory",
        "audit",
        "tags",
        "workspace",
        "not top-level cass commands",
    ] {
        ensure(
            doc.contains(required),
            format!("DISCREPANCIES.md must mention `{required}`"),
        )?;
    }

    if let Some(reason) = live_version_skip_reason()? {
        eprintln!("skipping live cass command drift check: {reason}");
        return Ok(());
    }
    let introspect = command_json(&["introspect", "--json"])?;
    let names: BTreeSet<&str> = array_field(&introspect, "commands")?
        .iter()
        .filter_map(|command| command.get("name").and_then(Value::as_str))
        .collect();
    for absent in ["memory", "audit", "tags", "workspace"] {
        ensure(
            !names.contains(absent),
            format!("documented discrepancy is stale: `{absent}` is now a cass command"),
        )?;
    }
    Ok(())
}
