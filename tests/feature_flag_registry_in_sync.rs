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
const REGISTRY: &str = include_str!("../docs/feature_flag_registry.md");

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
