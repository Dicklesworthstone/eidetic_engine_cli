//! Contract guard (bd-2xyv8 harness + bd-2nyem systemic half): every `ee ...`
//! command string the agent-docs surface advertises MUST resolve to a real
//! subcommand path in the live clap command tree.
//!
//! This is the guard that would have caught c8c993c2 leaving `ee context` in
//! agent-docs after the command was removed.
//!
//! Design note (see bd-2xyv8): resolution is done by walking
//! `<Cli as CommandFactory>::command()` IN-PROCESS, not by shelling out to
//! `ee help <path>` / `ee <path> --help`. The subprocess forms are unreliable
//! (`ee help <nested>` exits non-zero for valid nested commands, and
//! `ee <path> --help` is flaky under build-storm contention). The clap-tree walk
//! is deterministic. The binary is only invoked to *read* agent-docs JSON, which
//! is deterministic output.

use std::process::Command;

#[path = "support/command_inventory.rs"]
mod command_inventory;

fn run_json(args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to run ee {args:?}: {e}"));
    assert!(
        output.status.success(),
        "ee {args:?} failed (exit {:?}): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("ee {args:?} stdout was not valid JSON: {e}"))
}

/// Collect every `ee ...` command string advertised by the agent-docs surface:
/// the overview `primaryWorkflow`, the `coreCommands` list, and every recipe's
/// `command`.
fn advertised_commands() -> Vec<String> {
    let mut out = Vec::new();

    let overview = run_json(&["agent-docs", "--json"]);
    let data = &overview["data"];
    if let Some(pw) = data["primaryWorkflow"].as_str() {
        out.push(pw.to_string());
    }
    if let Some(core) = data["coreCommands"].as_array() {
        for c in core {
            if let Some(name) = c.as_str() {
                // coreCommands are bare subcommand names, not full invocations.
                out.push(format!("ee {name}"));
            }
        }
    }

    let recipes = run_json(&["agent-docs", "recipes", "--json"]);
    if let Some(arr) = recipes["data"]["recipes"].as_array() {
        for r in arr {
            if let Some(cmd) = r["command"].as_str() {
                out.push(cmd.to_string());
            }
        }
    }

    assert!(
        !out.is_empty(),
        "expected agent-docs to advertise at least one command"
    );
    out
}

#[test]
fn agent_docs_commands_resolve_against_clap_tree() {
    let valid = command_inventory::ee_command_paths();
    let advertised = advertised_commands();
    let dangling =
        command_inventory::unresolved_ee_invocations(advertised.iter().map(String::as_str), &valid);

    assert!(
        dangling.is_empty(),
        "agent-docs advertises commands that do not resolve against the clap tree \
         (dangling command references):\n  {}",
        dangling.join("\n  ")
    );
}

/// The guard is only meaningful if it can tell a real command from a removed one.
/// `context` is a soft-deprecated alias for `pack`, so it must resolve while
/// genuinely removed commands still fail through the broken-fixture test below.
#[test]
fn context_soft_deprecated_alias_is_present_in_clap_tree() {
    let valid = command_inventory::ee_command_paths();
    assert!(
        valid.contains("context"),
        "`ee context` must resolve as the soft-deprecated alias for `ee pack`"
    );
    // Sanity: the canonical command and a known nested command DO resolve.
    assert!(valid.contains("pack"), "`ee pack` must resolve");
    assert!(valid.contains("agent-docs"), "`ee agent-docs` must resolve");
}

#[test]
fn command_inventory_reports_broken_fixture() {
    let valid = command_inventory::ee_command_paths();
    let dangling = command_inventory::unresolved_ee_invocations(
        [
            "ee definitely-removed --json",
            "ee pack \"real task\" --workspace . --json",
            "cargo test",
        ],
        &valid,
    );

    assert_eq!(
        dangling,
        vec!["`ee definitely-removed --json` -> unresolved subcommand `definitely-removed`"],
        "the reusable command-inventory harness must distinguish broken ee \
         references from valid ee commands and non-ee commands"
    );
}
