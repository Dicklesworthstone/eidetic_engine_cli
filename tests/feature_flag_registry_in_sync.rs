//! Verification hook for `bd-17c65.11.7` (K7). Enforces 1:1 correspondence
//! between the Cargo.toml `[features]` section and the registry table in
//! `docs/feature_flag_registry.md`.
//!
//! Closure-lint treats K7 as closed only when these tests pass. Drift in
//! either direction (a flag declared in Cargo.toml without a registry entry,
//! or a registry row without a Cargo flag) fails the CI gate.

use std::collections::BTreeSet;

type TestResult = Result<(), String>;

const CARGO_TOML: &str = include_str!("../Cargo.toml");
const FAILURE_MODE_README: &str = include_str!("../tests/fixtures/failure_modes/README.md");
const GRAPH_CONFIG_DOC: &str = include_str!("../docs/configuration/graph.md");
const GRAPH_FEATURE_DISABLED_FIXTURE: &str =
    include_str!("../tests/fixtures/failure_modes/graph_feature_disabled.json");
const GRAPH_ROLLOUT_DOC: &str = include_str!("../docs/rollout/graph-accretion.md");
const REGISTRY: &str = include_str!("../docs/feature_flag_registry.md");
const RUNTIME_GRAPH_FEATURE_FLAGS: &[&str] = &[
    "graph.feature.ppr.enabled",
    "graph.feature.pack_dna.enabled",
    "graph.feature.causal_explain.enabled",
    "graph.feature.structural_health.enabled",
    "graph.feature.structural_decay.enabled",
    "graph.feature.proximity.enabled",
    "graph.feature.revision_dominance.enabled",
    "graph.feature.skyline.enabled",
    "graph.feature.load_bearing.enabled",
    "graph.feature.hits_profiles.enabled",
];

/// Extract the set of feature flag names declared in the `[features]`
/// section of `Cargo.toml`. Returns names in the order they appear.
fn cargo_features() -> Result<BTreeSet<String>, String> {
    let section_start = CARGO_TOML
        .find("\n[features]\n")
        .ok_or_else(|| "Cargo.toml missing `[features]` section".to_string())?;
    let after = &CARGO_TOML[section_start + "\n[features]\n".len()..];
    let section_end = after.find("\n[").unwrap_or(after.len());
    let block = &after[..section_end];

    let mut names = BTreeSet::new();
    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(eq_pos) = trimmed.find('=') else {
            continue;
        };
        let name = trimmed[..eq_pos].trim();
        if name.is_empty() {
            continue;
        }
        // `default` is composite over other features but counts as a flag.
        names.insert(name.to_string());
    }
    Ok(names)
}

/// Extract the set of flag names listed in the registry markdown table.
fn registry_features() -> Result<BTreeSet<String>, String> {
    let table_start = REGISTRY
        .find("\n## Registry\n")
        .ok_or_else(|| "registry missing `## Registry` section".to_string())?;
    let after = &REGISTRY[table_start + "\n## Registry\n".len()..];
    let section_end = after.find("\n## ").unwrap_or(after.len());
    let block = &after[..section_end];

    let mut names = BTreeSet::new();
    for line in block.lines() {
        let trimmed = line.trim();
        // Table rows look like: `| `name` | status | ... |`. Header and
        // separator rows do not contain a backtick-wrapped flag name in
        // the first cell.
        if !trimmed.starts_with("| `") {
            continue;
        }
        // Find the closing backtick of the first cell.
        let after_open = &trimmed[3..];
        let Some(close) = after_open.find('`') else {
            continue;
        };
        let name = &after_open[..close];
        if name.is_empty() {
            continue;
        }
        names.insert(name.to_string());
    }
    Ok(names)
}

fn graph_runtime_feature_flags() -> BTreeSet<String> {
    RUNTIME_GRAPH_FEATURE_FLAGS
        .iter()
        .map(|flag| (*flag).to_owned())
        .collect()
}

fn markdown_table_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') || trimmed.contains("---") {
        return None;
    }
    let cells = trimmed
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().trim_matches('`').to_owned())
        .collect::<Vec<_>>();
    (cells.len() >= 2).then_some(cells)
}

fn graph_runtime_flag_rows(doc: &str) -> Vec<Vec<String>> {
    doc.lines()
        .filter_map(markdown_table_cells)
        .filter(|cells| {
            cells
                .iter()
                .any(|cell| cell.starts_with("graph.feature.") && cell.ends_with(".enabled"))
        })
        .collect()
}

fn graph_runtime_flag_keys_in_doc(doc: &str) -> BTreeSet<String> {
    graph_runtime_flag_rows(doc)
        .into_iter()
        .flat_map(|cells| {
            cells
                .into_iter()
                .filter(|cell| cell.starts_with("graph.feature.") && cell.ends_with(".enabled"))
        })
        .collect()
}

fn require_graph_flag_row_with_default_false(
    rows: &[Vec<String>],
    key: &str,
    context: &str,
) -> TestResult {
    let matching_rows = rows
        .iter()
        .filter(|cells| cells.iter().any(|cell| cell == key))
        .collect::<Vec<_>>();
    if matching_rows.is_empty() {
        return Err(format!(
            "{context} missing runtime graph feature flag `{key}`"
        ));
    }
    if matching_rows
        .iter()
        .any(|cells| cells.iter().any(|cell| cell == "false"))
    {
        return Ok(());
    }
    Err(format!(
        "{context} documents runtime graph feature flag `{key}` but does not pin its rollout default to `false`"
    ))
}

fn require_graph_flag_row_with_boolean_default(
    rows: &[Vec<String>],
    key: &str,
    context: &str,
) -> TestResult {
    let matching_rows = rows
        .iter()
        .filter(|cells| cells.iter().any(|cell| cell == key))
        .collect::<Vec<_>>();
    if matching_rows.is_empty() {
        return Err(format!(
            "{context} missing runtime graph feature flag `{key}`"
        ));
    }
    if matching_rows
        .iter()
        .any(|cells| cells.iter().any(|cell| cell == "false" || cell == "true"))
    {
        return Ok(());
    }
    Err(format!(
        "{context} documents runtime graph feature flag `{key}` but does not include a boolean default"
    ))
}

#[test]
fn registry_has_an_entry_for_every_cargo_feature() -> TestResult {
    let cargo = cargo_features()?;
    let registry = registry_features()?;
    let missing: Vec<_> = cargo.difference(&registry).cloned().collect();
    if !missing.is_empty() {
        return Err(format!(
            "Cargo.toml declares feature(s) `{}` that have no entry in \
             docs/feature_flag_registry.md. Add an entry to the registry \
             table OR remove the flag from Cargo.toml.",
            missing.join("`, `")
        ));
    }
    Ok(())
}

#[test]
fn cargo_declares_every_registry_entry() -> TestResult {
    let cargo = cargo_features()?;
    let registry = registry_features()?;
    let extra: Vec<_> = registry.difference(&cargo).cloned().collect();
    if !extra.is_empty() {
        return Err(format!(
            "docs/feature_flag_registry.md lists feature(s) `{}` that are \
             not declared in Cargo.toml `[features]`. Either declare the \
             flag in Cargo.toml or remove the row from the registry.",
            extra.join("`, `")
        ));
    }
    Ok(())
}

#[test]
fn registry_documents_status_legend() -> TestResult {
    // The `status` column values are part of the contract. Every row's
    // status must be one of the documented enum values.
    let allowed = ["active", "reserved", "deprecated"];
    let table_start = REGISTRY
        .find("\n## Registry\n")
        .ok_or_else(|| "registry missing `## Registry` section".to_string())?;
    let after = &REGISTRY[table_start + "\n## Registry\n".len()..];
    let section_end = after.find("\n## ").unwrap_or(after.len());
    let block = &after[..section_end];

    for line in block.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("| `") {
            continue;
        }
        // The status is in the second column; split on `|` and skip the
        // leading empty cell + the name cell.
        let cells: Vec<&str> = trimmed.split('|').collect();
        // cells[0] = "" (before leading `|`); cells[1] = ` `name` `;
        // cells[2] = ` status `.
        if cells.len() < 3 {
            return Err(format!(
                "Malformed registry row (fewer than 3 cells): `{trimmed}`"
            ));
        }
        let status = cells[2].trim();
        if !allowed.contains(&status) {
            return Err(format!(
                "Row `{trimmed}` has status `{status}`; expected one of \
                 {:?}. Update docs/feature_flag_registry.md if the status \
                 vocabulary changes.",
                allowed
            ));
        }
    }
    Ok(())
}

#[test]
fn registry_index_pointer_present_in_silent_fallback_inventory() -> TestResult {
    // The science-analytics flag is the one current `reserved` flag tied
    // to a degraded-mode surface; docs/silent-fallback-inventory.md must
    // cross-reference the registry so an operator chasing a
    // degraded_unavailable code lands on this registry.
    const INVENTORY: &str = include_str!("../docs/silent-fallback-inventory.md");
    if !INVENTORY.contains("feature_flag_registry.md") {
        return Err("docs/silent-fallback-inventory.md should cross-reference \
             docs/feature_flag_registry.md (the science-analytics flag \
             links the two). Add a pointer or update K7 acceptance."
            .to_string());
    }
    Ok(())
}

#[test]
fn graph_runtime_feature_flags_are_documented_in_graph_config() -> TestResult {
    let expected = graph_runtime_feature_flags();
    let documented = graph_runtime_flag_keys_in_doc(GRAPH_CONFIG_DOC);
    if documented != expected {
        return Err(format!(
            "docs/configuration/graph.md runtime graph feature flags drifted. missing={:?} extra={:?}",
            expected.difference(&documented).collect::<Vec<_>>(),
            documented.difference(&expected).collect::<Vec<_>>()
        ));
    }

    let rows = graph_runtime_flag_rows(GRAPH_CONFIG_DOC);
    for key in RUNTIME_GRAPH_FEATURE_FLAGS {
        require_graph_flag_row_with_default_false(&rows, key, "docs/configuration/graph.md")?;
    }
    Ok(())
}

#[test]
fn graph_runtime_feature_flags_are_documented_in_rollout_plan() -> TestResult {
    let expected = graph_runtime_feature_flags();
    let documented = graph_runtime_flag_keys_in_doc(GRAPH_ROLLOUT_DOC);
    if documented != expected {
        return Err(format!(
            "docs/rollout/graph-accretion.md runtime graph feature flags drifted. missing={:?} extra={:?}",
            expected.difference(&documented).collect::<Vec<_>>(),
            documented.difference(&expected).collect::<Vec<_>>()
        ));
    }

    let rows = graph_runtime_flag_rows(GRAPH_ROLLOUT_DOC);
    for key in RUNTIME_GRAPH_FEATURE_FLAGS {
        require_graph_flag_row_with_boolean_default(&rows, key, "docs/rollout/graph-accretion.md")?;
    }
    if !GRAPH_ROLLOUT_DOC.contains("graph_feature_disabled") {
        return Err(
            "docs/rollout/graph-accretion.md must name the graph_feature_disabled sentinel"
                .to_string(),
        );
    }
    Ok(())
}

#[test]
fn graph_rollout_disabled_sentinel_matches_failure_mode_fixture() -> TestResult {
    let fixture: serde_json::Value = serde_json::from_str(GRAPH_FEATURE_DISABLED_FIXTURE)
        .map_err(|error| format!("graph_feature_disabled fixture is invalid JSON: {error}"))?;

    if fixture.get("schema").and_then(serde_json::Value::as_str)
        != Some("ee.failure_mode_fixture.v1")
    {
        return Err(
            "graph_feature_disabled fixture must use ee.failure_mode_fixture.v1".to_string(),
        );
    }
    if fixture.get("code").and_then(serde_json::Value::as_str) != Some("graph_feature_disabled") {
        return Err("graph_feature_disabled fixture code drifted".to_string());
    }
    if fixture.get("severity").and_then(serde_json::Value::as_str) != Some("medium") {
        return Err("graph_feature_disabled fixture severity must stay medium".to_string());
    }
    if fixture
        .get("repair_present")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err("graph_feature_disabled fixture must require repair guidance".to_string());
    }

    let surfaces = fixture
        .get("surfaces")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "graph_feature_disabled fixture missing surfaces array".to_string())?;
    for required_surface in ["graph", "graph feature-enrichment"] {
        if !surfaces
            .iter()
            .any(|surface| surface.as_str() == Some(required_surface))
        {
            return Err(format!(
                "graph_feature_disabled fixture missing surface `{required_surface}`"
            ));
        }
    }

    let expected = fixture
        .get("expected_emission")
        .ok_or_else(|| "graph_feature_disabled fixture missing expected_emission".to_string())?;
    if expected.get("code").and_then(serde_json::Value::as_str) != Some("graph_feature_disabled") {
        return Err("graph_feature_disabled expected emission code drifted".to_string());
    }
    if expected.get("severity").and_then(serde_json::Value::as_str) != Some("medium") {
        return Err("graph_feature_disabled expected emission severity drifted".to_string());
    }

    Ok(())
}

#[test]
fn graph_rollout_disabled_sentinel_matches_failure_mode_catalog() -> TestResult {
    let Some(row) = FAILURE_MODE_README
        .lines()
        .find(|line| line.starts_with("| `graph_feature_disabled` |"))
    else {
        return Err(
            "tests/fixtures/failure_modes/README.md missing graph_feature_disabled row".to_string(),
        );
    };

    let cells = markdown_table_cells(row)
        .ok_or_else(|| "graph_feature_disabled catalog row is malformed".to_string())?;
    let expected_cells = [
        "graph_feature_disabled",
        "graph, graph feature-enrichment",
        "medium",
        "bd-17c65.10.6 (J6)",
    ];
    for expected in expected_cells {
        if !cells.iter().any(|cell| cell == expected) {
            return Err(format!(
                "graph_feature_disabled catalog row missing `{expected}`"
            ));
        }
    }

    Ok(())
}
