use std::collections::{BTreeMap, BTreeSet};

use clap::CommandFactory;
use ee::core::effect::{EffectManifest, SideEffectClass};
use ee::models::RESPONSE_SCHEMA_V2;

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandInventoryEntry {
    pub path: String,
    pub is_leaf: bool,
    pub supports_json: bool,
    pub side_effect_class: &'static str,
    pub declared_response_schema: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ClapCommandPath {
    path: String,
    is_leaf: bool,
    accepts_positional_args: bool,
}

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

fn ee_command_paths_accepting_positionals() -> BTreeSet<String> {
    collect_clap_command_paths(ee::cli::Cli::command())
        .into_iter()
        .filter(|command| command.accepts_positional_args)
        .map(|command| command.path)
        .collect()
}

#[allow(dead_code)]
pub fn collect_command_inventory(root: clap::Command) -> Vec<CommandInventoryEntry> {
    let manifest = EffectManifest::build();
    collect_clap_command_paths(root)
        .into_iter()
        .map(|command| {
            let side_effect_class = match manifest.get(command.path.as_str()) {
                Some(effect) => effect.mutation_contract.side_effect_class.as_str(),
                None if !command.is_leaf => SideEffectClass::Mixed.as_str(),
                None => fallback_side_effect_class(command.path.as_str(), &manifest),
            };
            CommandInventoryEntry {
                path: command.path,
                is_leaf: command.is_leaf,
                supports_json: true,
                side_effect_class,
                declared_response_schema: RESPONSE_SCHEMA_V2,
            }
        })
        .collect()
}

fn fallback_side_effect_class(path: &str, manifest: &EffectManifest) -> &'static str {
    let mut parts = path.split_whitespace().collect::<Vec<_>>();
    while parts.len() > 1 {
        parts.pop();
        let parent = parts.join(" ");
        if let Some(effect) = manifest.get(parent.as_str()) {
            return effect.mutation_contract.side_effect_class.as_str();
        }
    }
    SideEffectClass::Mixed.as_str()
}

#[allow(dead_code)]
pub fn ee_command_inventory() -> Vec<CommandInventoryEntry> {
    collect_command_inventory(ee::cli::Cli::command())
}

#[allow(dead_code)]
pub fn ee_command_inventory_by_path() -> BTreeMap<String, CommandInventoryEntry> {
    ee_command_inventory()
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect()
}

fn collect_clap_command_paths(root: clap::Command) -> Vec<ClapCommandPath> {
    let mut out = Vec::new();
    for subcommand in root.get_subcommands() {
        collect_clap_command_path(Vec::new(), subcommand, &mut out);
    }
    out.sort_by(|left, right| left.path.cmp(&right.path));
    out
}

fn collect_clap_command_path(
    mut prefix: Vec<String>,
    command: &clap::Command,
    out: &mut Vec<ClapCommandPath>,
) {
    prefix.push(command.get_name().to_owned());
    let path = prefix.join(" ");
    let subcommands = command.get_subcommands().collect::<Vec<_>>();
    out.push(ClapCommandPath {
        path,
        is_leaf: subcommands.is_empty(),
        accepts_positional_args: command.get_arguments().any(|arg| arg.is_positional()),
    });
    for subcommand in subcommands {
        collect_clap_command_path(prefix.clone(), subcommand, out);
    }
}

/// Extract the leading `ee <subcommand path>` from an advertised command string,
/// stopping at the first flag, quoted argument, placeholder, or shell operator.
#[allow(dead_code)]
pub fn leading_ee_subcommand_path(command: &str) -> Option<String> {
    let path = leading_ee_path_tokens(command)?;
    Some(path.join(" "))
}

fn leading_ee_path_tokens(command: &str) -> Option<Vec<String>> {
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
        path.push(token.to_string());
    }
    if path.is_empty() { None } else { Some(path) }
}

#[allow(dead_code)]
pub fn unresolved_ee_invocations<'a>(
    commands: impl IntoIterator<Item = &'a str>,
    valid_paths: &BTreeSet<String>,
) -> Vec<String> {
    let mut unresolved = Vec::new();
    let positional_paths = ee_command_paths_accepting_positionals();
    for advertised in commands {
        let Some(tokens) = leading_ee_path_tokens(advertised) else {
            continue;
        };
        let mut resolved = false;
        for end in (1..=tokens.len()).rev() {
            let candidate = tokens[..end].join(" ");
            if valid_paths.contains(&candidate)
                && (end == tokens.len() || positional_paths.contains(&candidate))
            {
                resolved = true;
                break;
            }
        }
        if !resolved {
            let path = tokens.join(" ");
            unresolved.push(format!("`{advertised}` -> unresolved subcommand `{path}`"));
        }
    }
    unresolved
}
