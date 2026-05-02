use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value};

type TestResult = Result<(), String>;

const MATRIX_SCHEMA: &str = "ee.dependency_contract_matrix.v1";
const GOLDEN_PATH: &str = "tests/fixtures/golden/dependencies/contract_matrix.json.golden";
const DOC_PATH: &str = "docs/dependency-contract-matrix.md";
const CARGO_TOML_PATH: &str = "Cargo.toml";
const FORBIDDEN_CRATES: &[&str] = &[
    "tokio",
    "tokio-util",
    "async-std",
    "smol",
    "rusqlite",
    "sqlx",
    "diesel",
    "sea-orm",
    "petgraph",
    "hyper",
    "axum",
    "tower",
    "reqwest",
];

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_text(relative: &str) -> Result<String, String> {
    let path = repo_path(relative);
    fs::read_to_string(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))
}

fn load_matrix() -> Result<Value, String> {
    let text = read_text(GOLDEN_PATH)?;
    serde_json::from_str(&text).map_err(|error| format!("{GOLDEN_PATH} is invalid JSON: {error}"))
}

fn find_entry<'a>(matrix: &'a Value, name: &str) -> Result<&'a Map<String, Value>, String> {
    let root = object(matrix, "matrix root")?;
    for (index, entry) in array(root, "entries")?.iter().enumerate() {
        let entry = object(entry, &format!("entries[{index}]"))?;
        if string(entry, "name")? == name {
            return Ok(entry);
        }
    }
    Err(format!("matrix is missing dependency row `{name}`"))
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

fn boolean(object: &Map<String, Value>, key: &str) -> Result<bool, String> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("`{key}` must be a boolean"))
}

fn strings(object: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    array(object, key)?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("`{key}[{index}]` must be a string"))
        })
        .collect()
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn dependency_contract_matrix_golden_has_required_shape() -> TestResult {
    let matrix = load_matrix()?;
    let root = object(&matrix, "matrix root")?;

    ensure(
        string(root, "schema")? == MATRIX_SCHEMA,
        "matrix schema changed without a contract test update",
    )?;
    ensure(
        string(root, "source_bead")? == "eidetic_engine_cli-ilcq",
        "matrix must point at the EE-307 bead",
    )?;
    ensure(
        string(root, "default_feature_profile")? == "default",
        "matrix must freeze the default feature profile",
    )?;

    let canonical_forbidden: Vec<String> =
        FORBIDDEN_CRATES.iter().map(ToString::to_string).collect();
    ensure(
        strings(root, "forbidden_crates")? == canonical_forbidden,
        "matrix forbidden-crate list must match AGENTS.md order",
    )?;

    let entries = array(root, "entries")?;
    ensure(!entries.is_empty(), "matrix must include dependency rows")?;

    let mut names = BTreeSet::new();
    let mut owners: BTreeMap<String, String> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let context = format!("entries[{index}]");
        let entry = object(entry, &context)?;
        let name = string(entry, "name")?;
        let owner = string(entry, "owning_surface")?;
        ensure(
            names.insert(name.to_owned()),
            format!("duplicate matrix row `{name}`"),
        )?;
        ensure(
            owners.insert(name.to_owned(), owner.to_owned()).is_none(),
            format!("dependency `{name}` has more than one owning surface"),
        )?;

        for required in [
            "kind",
            "status",
            "minimum_smoke_test",
            "degradation_code",
            "diagnostic_command",
            "release_pin_decision",
        ] {
            ensure(
                !string(entry, required)?.trim().is_empty(),
                format!("{context}.{required} must be non-empty"),
            )?;
        }

        ensure(
            !strings(entry, "status_fields")?.is_empty(),
            format!("{context}.status_fields must not be empty"),
        )?;
        object(
            entry
                .get("source")
                .ok_or_else(|| format!("{context}.source is required"))?,
            &format!("{context}.source"),
        )?;
        object(
            entry
                .get("default_feature_profile")
                .ok_or_else(|| format!("{context}.default_feature_profile is required"))?,
            &format!("{context}.default_feature_profile"),
        )?;
    }

    for expected in [
        "asupersync",
        "frankensqlite",
        "sqlmodel_rust",
        "frankensearch",
        "franken_networkx",
        "coding_agent_session_search",
        "toon_rust",
        "franken_mermaid",
        "franken_agent_detection",
        "fastmcp-rust",
    ] {
        ensure(
            names.contains(expected),
            format!("matrix is missing dependency row `{expected}`"),
        )?;
    }

    Ok(())
}

#[test]
fn franken_mermaid_adapter_is_repository_and_audit_gated() -> TestResult {
    let matrix = load_matrix()?;
    let entry = find_entry(&matrix, "franken_mermaid")?;

    ensure(
        string(entry, "owning_surface")? == "ee-diagram",
        "FrankenMermaid must be isolated behind the future ee-diagram surface",
    )?;
    ensure(
        string(entry, "status")? == "planned_not_linked",
        "FrankenMermaid must stay planned/not linked until the repository and API are audited",
    )?;
    ensure(
        !boolean(entry, "enabled_by_default")?,
        "FrankenMermaid must never be enabled in the default feature profile",
    )?;

    let source = object(
        entry
            .get("source")
            .ok_or_else(|| "franken_mermaid.source is required".to_string())?,
        "franken_mermaid.source",
    )?;
    ensure(
        string(source, "kind")? == "not_linked",
        "FrankenMermaid source must remain not_linked before repository/API audit",
    )?;
    ensure(
        string(source, "path")? == "/dp/franken_mermaid",
        "FrankenMermaid gate must point at the canonical repository path",
    )?;

    let optional_profiles = array(entry, "optional_feature_profiles")?;
    ensure(
        optional_profiles.len() == 1,
        "FrankenMermaid must have exactly one future adapter profile gate",
    )?;
    let adapter_profile = object(
        optional_profiles
            .first()
            .ok_or_else(|| "franken_mermaid optional profile missing".to_string())?,
        "franken_mermaid.optional_feature_profiles[0]",
    )?;
    ensure(
        string(adapter_profile, "name")? == "franken-mermaid-adapter",
        "FrankenMermaid optional profile name changed",
    )?;
    ensure(
        string(adapter_profile, "status")? == "blocked_until_repository_api_and_dependency_audit",
        "FrankenMermaid adapter must remain blocked until repository/API and dependency audits pass",
    )?;

    ensure(
        string(entry, "degradation_code")? == "diagram_backend_unavailable",
        "FrankenMermaid gate must expose the stable diagram backend degradation code",
    )?;
    ensure(
        string(entry, "diagnostic_command")? == "ee doctor --json",
        "FrankenMermaid gate must name the diagnostic command for adapter readiness",
    )?;

    let cargo_toml = read_text(CARGO_TOML_PATH)?;
    ensure(
        !cargo_toml.contains("franken_mermaid") && !cargo_toml.contains("franken-mermaid"),
        "Cargo.toml must not link FrankenMermaid before the adapter audit passes",
    )
}

#[test]
fn accepted_default_rows_do_not_admit_forbidden_transitives() -> TestResult {
    let matrix = load_matrix()?;
    let root = object(&matrix, "matrix root")?;
    let forbidden: BTreeSet<String> = strings(root, "forbidden_crates")?.into_iter().collect();

    for (index, entry) in array(root, "entries")?.iter().enumerate() {
        let entry = object(entry, &format!("entries[{index}]"))?;
        let name = string(entry, "name")?;
        if boolean(entry, "enabled_by_default")? {
            let hits: Vec<String> = strings(entry, "forbidden_transitive_dependencies")?
                .into_iter()
                .filter(|candidate| forbidden.contains(candidate))
                .collect();
            ensure(
                hits.is_empty(),
                format!("default dependency `{name}` admits forbidden crates: {hits:?}"),
            )?;
        }
    }

    Ok(())
}

#[test]
fn blocked_feature_forbidden_crates_are_canonical() -> TestResult {
    let matrix = load_matrix()?;
    let root = object(&matrix, "matrix root")?;
    let forbidden: BTreeSet<String> = strings(root, "forbidden_crates")?.into_iter().collect();

    for (entry_index, entry) in array(root, "entries")?.iter().enumerate() {
        let entry = object(entry, &format!("entries[{entry_index}]"))?;
        for (feature_index, feature) in array(entry, "blocked_features")?.iter().enumerate() {
            let feature = object(
                feature,
                &format!("entries[{entry_index}].blocked_features[{feature_index}]"),
            )?;
            for crate_name in strings(feature, "forbidden_crates")? {
                ensure(
                    forbidden.contains(&crate_name),
                    format!(
                        "blocked feature references non-canonical forbidden crate `{crate_name}`"
                    ),
                )?;
            }
            ensure(
                !string(feature, "action")?.trim().is_empty(),
                "blocked feature action must explain the gate",
            )?;
        }
    }

    Ok(())
}

#[test]
fn local_path_rows_record_release_pin_decisions() -> TestResult {
    let matrix = load_matrix()?;
    let root = object(&matrix, "matrix root")?;

    for (index, entry) in array(root, "entries")?.iter().enumerate() {
        let entry = object(entry, &format!("entries[{index}]"))?;
        let source = object(
            entry
                .get("source")
                .ok_or_else(|| format!("entries[{index}].source is required"))?,
            &format!("entries[{index}].source"),
        )?;
        let source_kind = string(source, "kind")?;
        if source_kind.contains("path") {
            let decision = string(entry, "release_pin_decision")?;
            ensure(
                decision.contains("release"),
                format!(
                    "{} path source must record a release pin decision",
                    string(entry, "name")?
                ),
            )?;
        }
    }

    Ok(())
}

#[test]
fn markdown_artifact_mentions_every_golden_dependency() -> TestResult {
    let matrix = load_matrix()?;
    let root = object(&matrix, "matrix root")?;
    let docs = read_text(DOC_PATH)?;

    ensure(
        docs.contains(MATRIX_SCHEMA),
        "Markdown artifact must name the golden schema",
    )?;
    ensure(
        docs.contains(GOLDEN_PATH),
        "Markdown artifact must link the golden JSON path",
    )?;
    ensure(
        docs.contains("| Dependency | Owning surface | Default profile |"),
        "Markdown artifact must contain the dependency matrix table",
    )?;

    for (index, entry) in array(root, "entries")?.iter().enumerate() {
        let entry = object(entry, &format!("entries[{index}]"))?;
        let name = string(entry, "name")?;
        ensure(
            docs.contains(name),
            format!("Markdown artifact does not mention dependency `{name}`"),
        )?;
    }

    Ok(())
}
