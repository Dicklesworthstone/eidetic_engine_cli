use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use ee::cass::{CassSearchResponse, REQUIRED_API_VERSION, REQUIRED_CONTRACT_VERSION};
use ee::core::jsonl_import::{JsonlImportOptions, import_jsonl_records};
use serde_json::Value;

type TestResult = Result<(), String>;

const FIXTURE_ROOT: &str = "tests/fixtures/cass";
const PINNED_CASS_CRATE_VERSION: &str = "0.4.1";
const MEMORY_ID: &str = "mem_01234567890123456789012345";
const GOLDEN_FILES: &[&str] = &[
    "PROVENANCE.md",
    "robot_memory_v1.golden",
    "robot_audit_v1.golden",
    "json_contract.golden",
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
