use std::collections::BTreeSet;

use clap::CommandFactory;

/// Recursively collect every subcommand path in the clap tree, joined by spaces
/// (for example, "pack", "plan goal", or "agent-docs contracts").
///
/// The root binary name is not included. Clap's auto-generated `help`
/// subcommands are retained because they are valid invocations.
pub fn collect_command_paths(root: clap::Command) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
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

pub fn ee_command_paths() -> BTreeSet<String> {
    collect_command_paths(ee::cli::Cli::command())
}

/// Extract the leading `ee <subcommand path>` from an advertised command string,
/// stopping at the first flag, quoted argument, placeholder, or shell operator.
pub fn leading_ee_subcommand_path(command: &str) -> Option<String> {
    let rest = command.strip_prefix("ee ")?;
    let mut path = Vec::new();
    for token in rest.split_whitespace() {
        if token.starts_with('-')
            || token.starts_with('"')
            || token.starts_with('\'')
            || token.starts_with('<')
            || matches!(token, "|" | "||" | "&&" | ";")
        {
            break;
        }
        path.push(token);
    }
    if path.is_empty() {
        None
    } else {
        Some(path.join(" "))
    }
}

pub fn unresolved_ee_invocations<'a>(
    commands: impl IntoIterator<Item = &'a str>,
    valid_paths: &BTreeSet<String>,
) -> Vec<String> {
    let mut unresolved = Vec::new();
    for advertised in commands {
        let Some(path) = leading_ee_subcommand_path(advertised) else {
            continue;
        };
        if !valid_paths.contains(&path) {
            unresolved.push(format!("`{advertised}` -> unresolved subcommand `{path}`"));
        }
    }
    unresolved
}
