//! Contract tests for the shared command inventory (bd-2xyv8).

use clap::CommandFactory;
use ee::cli::Cli;
use ee::models::RESPONSE_SCHEMA_V2;

#[path = "../support/command_inventory.rs"]
mod command_inventory_support;

type TestResult = Result<(), String>;

fn direct_clap_leaf_count() -> usize {
    let root = Cli::command();
    root.get_subcommands().map(count_leaf_commands).sum()
}

fn count_leaf_commands(command: &clap::Command) -> usize {
    let subcommands = command.get_subcommands().collect::<Vec<_>>();
    if subcommands.is_empty() {
        1
    } else {
        subcommands.into_iter().map(count_leaf_commands).sum()
    }
}

#[test]
fn inventory_covers_every_clap_command_path() -> TestResult {
    let expected = command_inventory_support::ee_command_paths();
    let actual = command_inventory_support::ee_command_inventory()
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    if expected != actual {
        let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
        let extra = actual.difference(&expected).cloned().collect::<Vec<_>>();
        return Err(format!(
            "command inventory drifted from live clap tree; missing={missing:?}; extra={extra:?}"
        ));
    }
    Ok(())
}

#[test]
fn inventory_leaf_count_matches_live_clap_leaf_count() -> TestResult {
    let inventory_leaf_count = command_inventory_support::ee_command_inventory()
        .into_iter()
        .filter(|entry| entry.is_leaf)
        .count();
    let clap_leaf_count = direct_clap_leaf_count();
    if inventory_leaf_count != clap_leaf_count {
        return Err(format!(
            "inventory leaf-count drifted from clap leaf-count: inventory={inventory_leaf_count}, clap={clap_leaf_count}"
        ));
    }
    Ok(())
}

#[test]
fn inventory_entries_have_schema_and_side_effect_metadata() -> TestResult {
    let entries = command_inventory_support::ee_command_inventory_by_path();
    let mut invalid = Vec::new();

    for entry in entries.values() {
        if entry.path.trim().is_empty() {
            invalid.push("<empty path>".to_owned());
        }
        if !entry.supports_json {
            invalid.push(format!("{}: supports_json=false", entry.path));
        }
        if entry.declared_response_schema != RESPONSE_SCHEMA_V2 {
            invalid.push(format!(
                "{}: declared_response_schema={}",
                entry.path, entry.declared_response_schema
            ));
        }
        if entry.side_effect_class == "class=unclassified"
            || entry.side_effect_class.trim().is_empty()
        {
            invalid.push(format!("{}: side_effect_class unresolved", entry.path));
        }
    }

    if invalid.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "command inventory entries need path, JSON, response schema, and side-effect metadata:\n  {}",
            invalid.join("\n  ")
        ))
    }
}

#[test]
fn inventory_includes_nested_command_groups() -> TestResult {
    let paths = command_inventory_support::ee_command_paths();
    for expected in [
        "agent-docs",
        "curate candidates",
        "graph centrality-refresh",
        "memory show",
        "pack",
        "pack replay",
        "reflect request-ledger diagnostics",
    ] {
        if !paths.contains(expected) {
            return Err(format!(
                "shared command inventory is missing expected clap path `{expected}`"
            ));
        }
    }
    Ok(())
}
