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

use std::collections::BTreeSet;
use std::process::Command;

use clap::CommandFactory;
use ee::cli::Cli;

/// Recursively collect every subcommand path in the clap tree, joined by spaces
/// (e.g. "pack", "plan goal", "agent-docs contracts"). The root binary name is
/// not included; clap's auto-generated `help` subcommands are retained (they are
/// valid invocations and never cause false negatives here).
fn collect_command_paths() -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let root = Cli::command();
    let mut stack: Vec<(Vec<String>, clap::Command)> = root
        .get_subcommands()
        .cloned()
        .map(|sub| (Vec::new(), sub))
        .collect();
    while let Some((prefix, cmd)) = stack.pop() {
        let mut path = prefix.clone();
        path.push(cmd.get_name().to_string());
        paths.insert(path.join(" "));
        for sub in cmd.get_subcommands().cloned() {
            stack.push((path.clone(), sub));
        }
    }
    paths
}

/// Extract the leading `ee <subcommand path>` from an advertised command string,
/// stopping at the first flag / quoted argument / `<placeholder>`.
fn subcommand_path(cmd: &str) -> Option<String> {
    let rest = cmd.strip_prefix("ee ")?;
    let mut path = Vec::new();
    for tok in rest.split_whitespace() {
        if tok.starts_with('-')
            || tok.starts_with('"')
            || tok.starts_with('\'')
            || tok.starts_with('<')
        {
            break;
        }
        path.push(tok);
    }
    if path.is_empty() {
        None
    } else {
        Some(path.join(" "))
    }
}

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
    let valid = collect_command_paths();
    let mut dangling = Vec::new();

    for advertised in advertised_commands() {
        let Some(path) = subcommand_path(&advertised) else {
            continue;
        };
        if !valid.contains(&path) {
            dangling.push(format!("`{advertised}` -> unresolved subcommand `{path}`"));
        }
    }

    assert!(
        dangling.is_empty(),
        "agent-docs advertises commands that do not resolve against the clap tree \
         (dangling command references):\n  {}",
        dangling.join("\n  ")
    );
}

/// The guard is only meaningful if it can tell a real command from a removed one.
/// `context` was removed in c8c993c2, so it must NOT be in the valid set.
#[test]
fn removed_context_command_is_absent_from_clap_tree() {
    let valid = collect_command_paths();
    assert!(
        !valid.contains("context"),
        "`ee context` was removed (c8c993c2); its presence would make the \
         dangling-ref guard vacuous"
    );
    // Sanity: the canonical replacement and a known nested command DO resolve.
    assert!(valid.contains("pack"), "`ee pack` must resolve");
    assert!(
        valid.contains("agent-docs"),
        "`ee agent-docs` must resolve"
    );
}
