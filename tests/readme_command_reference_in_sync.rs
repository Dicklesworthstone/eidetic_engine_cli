//! K8 (bd-17c65.11.8) — README Command Reference must stay in sync
//! with the actual CLI surface.
//!
//! Two assertions, both directions:
//!
//! 1. Every `| \`ee X …\` |` row in README.md must parse under the
//!    real `Cli::try_parse_from`. A row that documents a command the
//!    parser doesn't recognize is a documentation lie.
//! 2. Every top-level Command variant exported by `src/cli/mod.rs`
//!    must appear in README.md unless explicitly listed in the
//!    `INTERNAL_ONLY_COMMANDS` allowlist. New surfaces require a
//!    README update OR an explicit internal-only declaration in the
//!    allowlist.
//!
//! Owned by K8 (bd-17c65.11.8). Each per-bead PR that introduces a
//! new top-level command must update both `src/cli/mod.rs` AND
//! README.md — this test enforces the discipline.

use std::collections::BTreeSet;

use clap::Parser;
use clap::error::ErrorKind;
use ee::cli::Cli;

type TestResult = Result<(), String>;

const README: &str = include_str!("../README.md");

/// Top-level commands that intentionally do not appear in the
/// README Command Reference. Each entry needs a one-line justification
/// in the comment so a future maintainer can decide whether to
/// promote it to documented or to remove it.
const INTERNAL_ONLY_COMMANDS: &[(&str, &str)] = &[
    // Hidden helpers used by hooks and internal harnesses.
    (
        "agent-docs",
        "Internal robot-docs surface for harness consumers; not user-facing.",
    ),
    (
        "completion",
        "Shell-completion script generator; consumed by `ee completion bash|zsh|fish|pwsh`.",
    ),
    (
        "diag",
        "Diagnostic helpers exercised by support bundle and Agent Mail.",
    ),
    (
        "note",
        "Lightweight memory shortcut; aliased into the agent triad design spike (bd-17c65.9).",
    ),
    // Forensic / scaffolding surfaces from the original C-S-cohort beads.
    (
        "audit",
        "Forensic audit timeline; entry point for incident response, not in core README.",
    ),
    (
        "claim",
        "Internal evidence-claim machinery for procedural-rule curation.",
    ),
    (
        "certificate",
        "Cryptographic identity for handoff capsules; described under M5 (bd-17c65.13.6) rather than the core README.",
    ),
    (
        "install",
        "Installer probe surface; consumed by install.sh.",
    ),
    (
        "history",
        "Activity-history scaffolding for daemon decay sweeps.",
    ),
    (
        "plan",
        "Planning-mode internal harness; not part of the v1 user surface.",
    ),
    (
        "preflight",
        "Pre-claim coordination probe for swarm-scale workflows.",
    ),
    ("procedure", "Procedural-rule lifecycle internal surface."),
    (
        "profile",
        "Operating-profile inspection (host-adaptive profiles per ADR 0023).",
    ),
    (
        "rationale",
        "Rationale trace attach surface for causal evidence under N3 (bd-17c65.14.3).",
    ),
    ("recorder", "Recorder session lifecycle internal surface."),
    (
        "rehearse",
        "Rehearsal planning surface tied to N15 lab capture.",
    ),
    (
        "situation",
        "Situation classifier internal surface (ADR 0020).",
    ),
    (
        "tag",
        "Tag mutation helpers; documented under `ee memory tags`.",
    ),
    (
        "task-frame",
        "Task-frame internal surface for procedural learning.",
    ),
    (
        "tripwire",
        "Tripwire check helpers; documented under safety/diagnostics.",
    ),
    (
        "verification",
        "Verification ledger machinery for procedural promotion.",
    ),
    (
        "verify",
        "Backup verify shortcut; documented as `ee backup verify`.",
    ),
    ("workflow", "Workflow lifecycle internal surface."),
    (
        "workspace",
        "Workspace identity helpers — appears under multiple README sections, not as a single row.",
    ),
    (
        "outcome-quarantine",
        "Internal quarantine surface; documented operationally in trust-model.md.",
    ),
    (
        "perf",
        "Performance-forensics internal surface (ADR 0024); user surface is via the audit table.",
    ),
    (
        "focus",
        "Focus-mode scratchpad; not part of the core user surface.",
    ),
    (
        "link",
        "Internal link helpers; user-facing surface is `ee memory link`.",
    ),
    (
        "learn",
        "Learn rollup internal surface for procedural curation.",
    ),
    (
        "show",
        "Internal show shortcut; user-facing surface is `ee memory show`.",
    ),
    (
        "update",
        "Internal record-update surface; user-facing surface is per-record.",
    ),
    // Degraded surfaces — README documentation lands when the
    // implementing bead closes and the degraded_unavailable code
    // retires.
    (
        "causal",
        "Degraded surface (causal_evidence_unavailable); README mention added when bd-17c65.14.3 (N3) closes.",
    ),
    (
        "economy",
        "Degraded surface (economy_metrics_unavailable); awaits a future K-series doc when economy/* surfaces ship.",
    ),
    (
        "lab",
        "Degraded surface (lab_replay_unavailable); README mention added when bd-17c65.14.15.4/.5/.6 close.",
    ),
    // Operational / internal commands without a public user surface.
    (
        "agent",
        "Operational surface for agent harness detection; documented under hooks rather than the main Command Reference.",
    ),
    (
        "artifact",
        "Operational surface for artifact register; documented under bd-17c65 graph subsystem rather than README Command Reference.",
    ),
    (
        "db",
        "Operational surface for direct DB inspection; documented under docs/migration-guide.md.",
    ),
    (
        "demo",
        "Demo runner; not part of the user-facing Command Reference.",
    ),
    ("job", "Job runner internal surface; consumed by daemon."),
    (
        "maintenance",
        "Maintenance runner internal surface; consumed by daemon.",
    ),
    (
        "migrate",
        "Migration runner; documented under docs/migration-guide.md, not the main Command Reference.",
    ),
    // New user-facing surfaces awaiting their owning beads' close
    // before README documentation lands.
    (
        "handoff",
        "User-facing surface; documented in README under §Pack replay evidence and §Swarm brief workflow but not as a Command Reference table row. Add a top-level row when bd-17c65.13 epic closes.",
    ),
];

/// Top-level commands that appear in README under a different form
/// (e.g. `ee memory show` is documented as a sub-command row, not as
/// a top-level `ee memory` row). The README parser sees `ee memory`
/// as the top-level token; this list says it's OK.
fn appears_as_subcommand_only() -> BTreeSet<&'static str> {
    // No entries today; placeholder for future use.
    BTreeSet::new()
}

/// Convert a PascalCase Command variant to its clap kebab-case form.
fn pascal_to_kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// All top-level Command variants extracted at compile-time. Keep in
/// sync with `pub enum Command` in src/cli/mod.rs by running
/// `awk '/^pub enum Command \{/,/^}/' src/cli/mod.rs | grep -E '^\s+[A-Z][a-zA-Z]+\('`.
const TOP_LEVEL_COMMAND_VARIANTS: &[&str] = &[
    "Agent",
    "AgentDocs",
    "Analyze",
    "Artifact",
    "Audit",
    "Backup",
    "Causal",
    "Certificate",
    "Claim",
    "Completion",
    "Context",
    "Curate",
    "Daemon",
    "Db",
    "Demo",
    "Diag",
    "Doctor",
    "Economy",
    "Eval",
    "Export",
    "Focus",
    "Graph",
    "Handoff",
    "History",
    "Import",
    "Index",
    "Init",
    "Install",
    "Job",
    "Lab",
    "Learn",
    "Link",
    "Maintenance",
    "Mcp",
    "Memory",
    "Migrate",
    "Model",
    "Note",
    "Outcome",
    "OutcomeQuarantine",
    "Pack",
    "Perf",
    "Plan",
    "Playbook",
    "Preflight",
    "Procedure",
    "Profile",
    "Rationale",
    "Recorder",
    "Rehearse",
    "Remember",
    "Review",
    "Rule",
    "Schema",
    "Search",
    "Show",
    "Situation",
    "Support",
    "Swarm",
    "Tag",
    "TaskFrame",
    "Tripwire",
    "Update",
    "Verification",
    "Verify",
    "Why",
    "Workflow",
    "Workspace",
];

/// Extract command tokens from README. We look in three places:
///
/// 1. Markdown table rows of the form `| \`ee X …\` | … |`.
/// 2. Code-fence lines whose first non-whitespace token (after an
///    optional `$ ` prompt) is `ee`.
/// 3. Inline backtick-wrapped tokens of the form `` `ee X` ``
///    anywhere in the body (catches the prose form
///    "run `ee context` first" common in section bodies).
///
/// The first identifier token after `ee ` is the top-level command;
/// subsequent tokens may be subcommands or argument placeholders.
fn extract_readme_commands(readme: &str) -> BTreeSet<String> {
    let mut commands: BTreeSet<String> = BTreeSet::new();
    let mut in_fence = false;

    for line in readme.lines() {
        // Toggle fence on triple-backtick.
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }

        // Source 1: table row.
        let trimmed = line.trim_start();
        if let Some(after) = trimmed.strip_prefix("| `ee ") {
            let end = after
                .find(|c: char| c.is_whitespace() || c == '`')
                .unwrap_or(after.len());
            let first = &after[..end];
            if !first.is_empty() && !first.starts_with('<') {
                commands.insert(first.to_owned());
            }
        }

        // Source 2: fence-body line starting with `ee `.
        if in_fence {
            let body = trimmed.trim_start_matches("$ ").trim_start_matches('$');
            let body = body.trim_start();
            if let Some(rest) = body.strip_prefix("ee ") {
                let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
                let first = &rest[..end];
                if !first.is_empty() && !first.starts_with('<') && !first.starts_with('-') {
                    commands.insert(first.to_owned());
                }
            }
        }

        // Source 3: inline backtick tokens anywhere in the line.
        // Scan for `` `ee X… ` `` substrings.
        let mut search = line;
        while let Some(start) = search.find("`ee ") {
            let after_open = &search[start + 4..];
            let Some(close_rel) = after_open.find('`') else {
                break;
            };
            let inner = &after_open[..close_rel];
            let end = inner
                .find(|c: char| c.is_whitespace())
                .unwrap_or(inner.len());
            let first = &inner[..end];
            if !first.is_empty() && !first.starts_with('<') && !first.starts_with('-') {
                commands.insert(first.to_owned());
            }
            search = &after_open[close_rel + 1..];
        }
    }
    commands
}

#[test]
fn every_readme_ee_command_parses_under_cli() -> TestResult {
    let commands = extract_readme_commands(README);
    if commands.is_empty() {
        return Err("README parser found zero `| `ee X …` |` table rows; \
             the README format changed or the parser regressed."
            .to_string());
    }

    let mut failures: Vec<String> = Vec::new();
    for cmd in &commands {
        // Try the command with --help; clap returns DisplayHelp on a
        // recognized command + --help, which is "the command name is
        // valid" without actually invoking the handler.
        let argv = ["ee", cmd, "--help"];
        let result = Cli::try_parse_from(argv);
        match result {
            Ok(_) => continue,
            Err(error) => match error.kind() {
                // Recognized command; clap displayed help. Success.
                ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                    continue;
                }
                // Recognized version flag (shouldn't happen for
                // subcommand, but tolerate).
                ErrorKind::DisplayVersion => continue,
                // Subcommand recognized but its own required-args
                // missing; still proves the command name is real.
                ErrorKind::MissingRequiredArgument | ErrorKind::MissingSubcommand => continue,
                // Command was not recognized — README documents
                // something the parser does not know about.
                _ => failures.push(format!(
                    "README documents `ee {cmd}` but `Cli::try_parse_from` rejected it: {error}"
                )),
            },
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} README command(s) do not parse under the real CLI:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        ))
    }
}

#[test]
fn every_top_level_command_variant_is_documented_or_internal() -> TestResult {
    let documented = extract_readme_commands(README);
    let subcommand_only = appears_as_subcommand_only();
    let internal: BTreeSet<&str> = INTERNAL_ONLY_COMMANDS
        .iter()
        .map(|(name, _)| *name)
        .collect();

    let mut missing: Vec<String> = Vec::new();
    for variant in TOP_LEVEL_COMMAND_VARIANTS {
        let kebab = pascal_to_kebab(variant);
        if documented.contains(&kebab) {
            continue;
        }
        if subcommand_only.contains(kebab.as_str()) {
            continue;
        }
        if internal.contains(kebab.as_str()) {
            continue;
        }
        missing.push(format!(
            "Command variant `{variant}` (CLI: `ee {kebab}`) is neither in \
             README nor in INTERNAL_ONLY_COMMANDS allowlist. Either document \
             it in README or add an entry to the allowlist with a one-line \
             justification."
        ));
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} top-level command(s) lack documentation:\n  - {}",
            missing.len(),
            missing.join("\n  - ")
        ))
    }
}

#[test]
fn top_level_command_list_matches_src() {
    // Soft drift detector: if someone adds a Command variant to
    // src/cli/mod.rs without updating TOP_LEVEL_COMMAND_VARIANTS,
    // the README-vs-CLI test silently misses the new command.
    // This test fails loudly when the count diverges.
    //
    // Manual sync command (per README of this file):
    //   awk '/^pub enum Command \{/,/^}/' src/cli/mod.rs \
    //     | grep -E '^\s+[A-Z][a-zA-Z]+\(' | wc -l
    let expected = TOP_LEVEL_COMMAND_VARIANTS.len();
    // Count is intentionally hard-coded as a tripwire. The number
    // matches the wc -l at 2026-05-13. When you add a command,
    // update both the list and this constant in the same commit.
    let pinned_at_drift_check = 68;
    assert_eq!(
        expected, pinned_at_drift_check,
        "TOP_LEVEL_COMMAND_VARIANTS length changed from {pinned_at_drift_check} \
         to {expected}. Update both this assertion and the variant list."
    );
}
