//! K8 (bd-17c65.11.8) — every `ee X …` command-line example in
//! AGENTS.md must parse under the real CLI.
//!
//! Walks AGENTS.md, extracts lines inside code fences that look like
//! a shell invocation starting with `ee `, and asserts each is
//! parseable under `Cli::try_parse_from`. Catches the failure mode
//! where AGENTS.md tells an agent "Run `ee context …`" but the
//! command shape has changed.
//!
//! Owned by K8 (bd-17c65.11.8). Each per-bead PR that changes a CLI
//! command shape must update AGENTS.md alongside the code change —
//! this test enforces the discipline.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clap::Parser;
use clap::error::ErrorKind;
use ee::cli::Cli;

type TestResult = Result<(), String>;

const AGENTS_MD: &str = include_str!("../AGENTS.md");

/// Each AGENTS.md ee-line that we extract is described by its
/// 1-indexed line number and the literal command text starting from
/// the `ee ` prefix.
#[derive(Debug)]
struct AgentsCommand {
    line_no: usize,
    text: String,
}

/// Extract candidate `ee …` invocations from AGENTS.md code fences.
///
/// A line qualifies if it is inside a triple-backtick fenced block
/// AND begins with `ee ` (after optional leading whitespace).
/// Heredoc bodies and prose paragraphs are excluded.
fn extract_agents_md_commands(doc: &str) -> Vec<AgentsCommand> {
    let mut commands = Vec::new();
    let mut in_code_fence = false;

    for (line_no, line) in doc.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if !in_code_fence {
            continue;
        }

        // Skip pure-prompt rows ("$ ee context …") and dollar prefixes.
        let body = trimmed.trim_start_matches("$ ").trim_start_matches("$");
        let body = body.trim_start();

        if !body.starts_with("ee ") {
            continue;
        }

        // Skip continuation lines (backslash line-continued) — too
        // tricky to reconstruct reliably; flag for manual review by
        // matching only the head of a multi-line invocation.
        // Also skip lines that are comments (start with `#`).
        if body.starts_with('#') {
            continue;
        }

        // Strip trailing line-continuation backslash; downstream
        // parser would reject it anyway.
        let body = body.trim_end_matches('\\').trim();

        commands.push(AgentsCommand {
            line_no: line_no + 1,
            text: body.to_string(),
        });
    }
    commands
}

/// Tokenize an ee command line using shell-aware splitting that
/// honors single and double quotes. Returns a `Vec<String>` of argv
/// tokens excluding the leading `ee`.
fn shell_tokenize(text: &str) -> Result<Vec<String>, String> {
    // shlex is not a dependency; implement a small parser for the
    // subset we need (quotes, no env-vars, no backslash escapes
    // inside double quotes beyond ", \\).
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for ch in text.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '#' if !in_single && !in_double && current.is_empty() => {
                break;
            }
            '\\' if !in_single => {
                escape = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if in_single || in_double || escape {
        return Err(format!("unterminated quote/escape in `{text}`"));
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        return Err(format!("empty command after tokenization: `{text}`"));
    }
    // The first token should be "ee"; strip it. If it's not "ee",
    // the caller's filter is broken.
    if out[0] != "ee" {
        return Err(format!(
            "tokenization expected leading `ee` token, got `{}` in `{text}`",
            out[0]
        ));
    }
    Ok(out)
}

/// Try to parse an ee invocation; returns Ok(()) if clap accepts the
/// command shape (whether or not its required args are present).
fn parse_command(argv: &[String]) -> Result<(), clap::Error> {
    match Cli::try_parse_from(argv) {
        Ok(_) => Ok(()),
        Err(error) => match error.kind() {
            // The command was recognized by clap; missing args or
            // help/version display don't invalidate the command shape.
            ErrorKind::DisplayHelp
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            | ErrorKind::DisplayVersion
            | ErrorKind::MissingRequiredArgument
            | ErrorKind::MissingSubcommand
            | ErrorKind::InvalidValue
            | ErrorKind::ValueValidation
            | ErrorKind::TooManyValues
            | ErrorKind::TooFewValues
            | ErrorKind::WrongNumberOfValues => Ok(()),
            _ => Err(error),
        },
    }
}

#[test]
fn agents_md_ee_invocations_parse_under_cli() -> TestResult {
    let candidates = extract_agents_md_commands(AGENTS_MD);

    if candidates.is_empty() {
        return Err(
            "AGENTS.md parser found zero `ee X …` invocations inside fenced \
             code blocks. The doc format changed or the parser regressed."
                .to_string(),
        );
    }

    let mut failures: Vec<String> = Vec::new();
    for cmd in &candidates {
        let argv = match shell_tokenize(&cmd.text) {
            Ok(toks) => toks,
            Err(error) => {
                failures.push(format!(
                    "AGENTS.md L{}: tokenization failed: {error}",
                    cmd.line_no
                ));
                continue;
            }
        };
        if let Err(error) = parse_command(&argv) {
            failures.push(format!(
                "AGENTS.md L{} ({}): Cli::try_parse_from rejected: {error}",
                cmd.line_no,
                cmd.text.chars().take(80).collect::<String>()
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} AGENTS.md command line(s) do not parse under the real CLI:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        ))
    }
}

#[test]
fn extractor_finds_at_least_a_few_examples() {
    // Defensive: if AGENTS.md is rewritten and no longer carries any
    // `ee …` examples, the test above passes vacuously. Soft floor:
    // we expect at least 5 examples in AGENTS.md to keep agents
    // grounded in concrete invocations.
    let candidates = extract_agents_md_commands(AGENTS_MD);
    assert!(
        candidates.len() >= 5,
        "AGENTS.md should contain at least 5 `ee …` invocation examples; \
         found {}. If this is intentional (e.g. AGENTS.md was moved to a \
         table-only style), update this floor or move the floor into a \
         separate skipped test.",
        candidates.len()
    );
}

#[test]
fn shell_tokenize_strips_unquoted_inline_comments() -> TestResult {
    let argv = shell_tokenize(
        "ee context \"<task>\" --workspace . --json # use pack.text as prompt fragment",
    )?;

    assert_eq!(
        argv,
        vec!["ee", "context", "<task>", "--workspace", ".", "--json"]
    );

    let argv = shell_tokenize("ee remember \"hash#inside\" --tag foo#bar --json")?;

    assert_eq!(
        argv,
        vec![
            "ee",
            "remember",
            "hash#inside",
            "--tag",
            "foo#bar",
            "--json"
        ]
    );

    Ok(())
}
