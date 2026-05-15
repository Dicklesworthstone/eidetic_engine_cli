//! README ↔ CLI parity invariant test (bd-3usjw.38).
//!
//! Every command-table row in `README.md` that names an `ee <subcommand>`
//! path MUST resolve to a registered clap subcommand whose `--help` succeeds
//! and produces non-empty stdout. This catches:
//!
//! - README rows for renamed or removed commands
//! - README rows whose subcommand path drifted from CLI registration
//!
//! Source: `CLOSE_THE_GAP_PLAN.md` §41 (vision-coverage gap finds plan-doc
//! ↔ CLI drift but does NOT find README ↔ CLI drift).
//!
//! Parsing model:
//! - A command-table row starts with the literal prefix `| \`ee ` (a pipe,
//!   space, backtick, the literal `ee`, then space) and contains one or
//!   more backticked code groups, the first of which is the canonical
//!   full command (e.g. `ee curate apply <id>`).
//! - Subsequent backticked groups on the same row joined by `/` (the
//!   alternation separator used in README) are leaf alternates that share
//!   the parent path of the first group (e.g. `accept <id>`, `reject <id>`).
//! - Within each backticked group, the command path is the leading
//!   whitespace-separated tokens up to (but not including) the first
//!   token that starts with `<`, `[`, `"`, `'`, or `-`.
//!
//! The test invokes the built `ee` binary with the extracted path plus
//! `--help` and asserts a successful exit and non-empty stdout.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ee_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ee")
}

fn trace_readme_cli_parity(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "repo",
        request_id = "readme_cli_parity_contract",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.38"),
        surface = "readme_cli_parity",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "README CLI parity contract checkpoint"
    );
}

/// Return true when `token` starts a placeholder / argument and should
/// terminate the command-path prefix walk.
fn is_arg_or_placeholder(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|character| matches!(character, '<' | '[' | '"' | '\'' | '-'))
}

/// Parse a single backticked code group's command tokens. Returns the
/// leading subcommand-path tokens (excluding the leading literal `ee`).
///
/// Returns `None` when the group does not start with the literal `ee` or
/// when no subcommand tokens are present.
fn parse_full_command_group(group: &str) -> Option<Vec<String>> {
    let tokens: Vec<&str> = group.split_ascii_whitespace().collect();
    if tokens.first().is_none_or(|first| *first != "ee") {
        return None;
    }
    let mut path = Vec::new();
    for token in tokens.iter().skip(1) {
        if is_arg_or_placeholder(token) {
            break;
        }
        path.push((*token).to_owned());
    }
    if path.is_empty() { None } else { Some(path) }
}

/// Parse an alternate code group (no leading `ee`). Returns leading
/// non-placeholder tokens.
fn parse_alternate_group(group: &str) -> Vec<String> {
    let mut path = Vec::new();
    for token in group.split_ascii_whitespace() {
        if is_arg_or_placeholder(token) {
            break;
        }
        path.push(token.to_owned());
    }
    path
}

/// Extract every backtick-delimited code group on `line` along with the
/// raw delimiter text that precedes each group. The first group's
/// preceding-delimiter is the prefix before the first opening backtick;
/// subsequent groups carry the text between the previous closing backtick
/// and the current opening backtick.
fn split_backtick_groups(line: &str) -> Vec<(String, String)> {
    let mut groups: Vec<(String, String)> = Vec::new();
    let mut remaining = line;
    let mut preceding = String::new();
    while let Some(start) = remaining.find('`') {
        preceding.push_str(&remaining[..start]);
        let after_open = &remaining[start + 1..];
        let Some(end) = after_open.find('`') else {
            break;
        };
        let body = after_open[..end].to_owned();
        groups.push((preceding.clone(), body));
        preceding.clear();
        remaining = &after_open[end + 1..];
    }
    groups
}

/// Parse README and return the de-duplicated, sorted set of ee
/// subcommand paths (each represented as a `Vec<String>` excluding the
/// leading `ee` token).
fn parse_readme_commands(readme: &str) -> Vec<Vec<String>> {
    trace_readme_cli_parity("input", 0, &[]);
    let mut commands: BTreeSet<Vec<String>> = BTreeSet::new();
    for line in readme.lines() {
        if !line.starts_with("| `ee ") {
            continue;
        }
        let groups = split_backtick_groups(line);
        if groups.is_empty() {
            continue;
        }
        let Some(first_path) = parse_full_command_group(&groups[0].1) else {
            continue;
        };
        commands.insert(first_path.clone());
        let parent: Vec<String> = if first_path.len() > 1 {
            first_path[..first_path.len() - 1].to_vec()
        } else {
            Vec::new()
        };
        for (delimiter, body) in groups.iter().skip(1) {
            if !delimiter.contains('/') {
                continue;
            }
            let leaf_path = parse_alternate_group(body);
            if leaf_path.is_empty() {
                continue;
            }
            let mut combined = parent.clone();
            combined.extend(leaf_path);
            commands.insert(combined);
        }
    }
    let commands = commands.into_iter().collect();
    trace_readme_cli_parity("dispatch", 0, &[]);
    commands
}

/// Run `ee <args...> --help` and return a structured outcome.
fn invoke_help(path: &[String]) -> Result<(), String> {
    trace_readme_cli_parity("dependency_check", 0, &[]);
    let output = Command::new(ee_binary())
        .args(path)
        .arg("--help")
        .output()
        .map_err(|error| format!("failed to spawn ee binary: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ee {} --help failed with status {:?}; stderr: {}",
            path.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim_end(),
        ));
    }
    if output.stdout.is_empty() {
        return Err(format!(
            "ee {} --help produced empty stdout",
            path.join(" ")
        ));
    }
    trace_readme_cli_parity("response", 0, &[]);
    Ok(())
}

#[test]
fn every_readme_command_row_resolves_to_registered_clap_subcommand() {
    let readme_path = workspace_root().join("README.md");
    let readme = fs::read_to_string(&readme_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", readme_path.display()));
    let commands = parse_readme_commands(&readme);
    assert!(
        !commands.is_empty(),
        "README must contain at least one `| \\`ee ...\\`` command-table row"
    );

    let mut failures: Vec<String> = Vec::new();
    for path in &commands {
        if let Err(reason) = invoke_help(path) {
            failures.push(reason);
        }
    }

    assert!(
        failures.is_empty(),
        "README ↔ CLI parity violations ({} of {} command paths failed):\n{}",
        failures.len(),
        commands.len(),
        failures.join("\n"),
    );
}

#[cfg(test)]
mod parsing_unit_tests {
    use super::*;

    #[test]
    fn parse_full_command_group_strips_placeholder_tokens() {
        assert_eq!(
            parse_full_command_group("ee context \"<task>\" [--profile <p>] [--max-tokens N]"),
            Some(vec!["context".to_owned()])
        );
        assert_eq!(
            parse_full_command_group("ee memory level <id> --to <level> --reason <why>"),
            Some(vec!["memory".to_owned(), "level".to_owned()])
        );
        assert_eq!(
            parse_full_command_group("ee init [--workspace .]"),
            Some(vec!["init".to_owned()])
        );
    }

    #[test]
    fn parse_full_command_group_rejects_non_ee_groups() {
        assert!(parse_full_command_group("br status").is_none());
        assert!(parse_full_command_group("just text").is_none());
        assert!(parse_full_command_group("ee").is_none());
    }

    #[test]
    fn parse_alternate_group_strips_placeholder_tokens() {
        assert_eq!(parse_alternate_group("show <id>"), vec!["show".to_owned()]);
        assert_eq!(parse_alternate_group("list"), vec!["list".to_owned()]);
        assert_eq!(
            parse_alternate_group("export <schema-id>"),
            vec!["export".to_owned()]
        );
    }

    #[test]
    fn split_backtick_groups_captures_preceding_delimiters() {
        let line = "| `ee rule add` / `list` / `show <id>` | description";
        let groups = split_backtick_groups(line);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].1, "ee rule add");
        assert!(!groups[0].0.contains('/'));
        assert_eq!(groups[1].1, "list");
        assert!(groups[1].0.contains('/'));
        assert_eq!(groups[2].1, "show <id>");
        assert!(groups[2].0.contains('/'));
    }

    #[test]
    fn parse_readme_commands_expands_alternates() {
        let readme = "\
| Command | Purpose |
|---|---|
| `ee rule add` / `list` / `show <id>` / `mark <id>` | direct rule management |
| `ee init [--workspace .]` | open workspace |
| `ee curate apply <id>` / `accept <id>` / `reject <id>` | curate lifecycle |
| Random line without command table prefix |
";
        let commands = parse_readme_commands(readme);
        let as_strings: Vec<String> = commands.iter().map(|path| path.join(" ")).collect();
        assert!(
            as_strings.contains(&"rule add".to_owned()),
            "{as_strings:?}"
        );
        assert!(
            as_strings.contains(&"rule list".to_owned()),
            "{as_strings:?}"
        );
        assert!(
            as_strings.contains(&"rule show".to_owned()),
            "{as_strings:?}"
        );
        assert!(
            as_strings.contains(&"rule mark".to_owned()),
            "{as_strings:?}"
        );
        assert!(as_strings.contains(&"init".to_owned()), "{as_strings:?}");
        assert!(
            as_strings.contains(&"curate apply".to_owned()),
            "{as_strings:?}"
        );
        assert!(
            as_strings.contains(&"curate accept".to_owned()),
            "{as_strings:?}"
        );
        assert!(
            as_strings.contains(&"curate reject".to_owned()),
            "{as_strings:?}"
        );
        assert!(!as_strings.iter().any(|entry| entry.is_empty()));
    }

    #[test]
    fn parse_readme_commands_skips_non_command_lines() {
        let readme = "\
Random text.

| Header | Body |
|---|---|

This row has `code` but no ee prefix.
";
        assert!(parse_readme_commands(readme).is_empty());
    }
}
