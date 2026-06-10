//! Clap CLI argument-definition hygiene gate.
//!
//! WHY THIS EXISTS: ee 0.8.0 shipped with `ee search <query>` panicking on
//! *every* invocation —
//!
//! ```text
//! thread 'main' panicked: Mismatch between definition and access of `fields`.
//! Could not downcast to TypeId(..)
//! ```
//!
//! Root cause: `SearchArgs` declared a Rust field literally named `fields`
//! (flag `--field`, type `Vec<String>`). clap derives an argument's *id* from
//! the field identifier — NOT from `long` — so its id was `fields`, colliding
//! with the `global = true` `--fields` (`output::FieldSelector`) on `Cli`. clap
//! registered the id with one value type and the subcommand accessed it with
//! another, panicking at parse/access time. `--help` short-circuits before the
//! access, which is why help "worked" and nothing else caught it.
//!
//! The whole class is structurally detectable. `clap::Command::debug_assert()`
//! builds the entire command tree (propagating globals into every subcommand)
//! and asserts, among many invariants, that no two arguments on the same
//! (sub)command share an id. Running it over the real `Cli` would have failed
//! the build the instant `fields` collided. This file makes that a permanent,
//! fast, exhaustive gate — plus a direct parse-and-access regression for the
//! exact crash, and a recursive sweep that forces every subcommand to build.

use clap::{CommandFactory, Parser};
use ee::cli::Cli;

/// Structural catch-all: validates the ENTIRE command tree (every subcommand,
/// every global after propagation) for duplicate argument ids, conflicting
/// settings, and definition/access type drift. This single assertion would have
/// caught the 0.8.0 `ee search` panic at test time, and catches any future
/// recurrence anywhere in the CLI surface.
#[test]
fn cli_command_tree_passes_clap_debug_assert() {
    Cli::command().debug_assert();
}

/// Direct regression for the exact 0.8.0 crash: parsing `ee search <query>`
/// must reach `from_arg_matches` (the access path that panicked) and succeed,
/// with and without `--json`. A panic here = the `fields`-class collision is
/// back. We assert `Ok` (not merely "no panic") so a silent reintroduction of a
/// required-arg / type regression on the primary retrieval command is caught.
#[test]
fn ee_search_parses_and_accesses_without_panicking() {
    for argv in [
        vec!["ee", "search", "some query"],
        vec!["ee", "search", "some query", "--json"],
        vec![
            "ee",
            "search",
            "some query",
            "--field",
            "family=x",
            "--json",
        ],
        vec![
            "ee",
            "search",
            "some query",
            "--fields",
            "minimal",
            "--json",
        ],
    ] {
        let parsed = Cli::try_parse_from(argv.clone());
        assert!(
            parsed.is_ok(),
            "`{}` must parse without error or panic; got {:?}",
            argv.join(" "),
            parsed.err().map(|error| error.to_string()),
        );
    }
}

/// Exhaustive build sweep: force clap to fully build every leaf subcommand.
/// `debug_assert()` already does this internally, but keeping an explicit walk
/// makes the coverage obvious and fails with the offending subcommand path if a
/// future change makes one un-buildable.
#[test]
fn every_subcommand_builds_and_has_unique_arg_ids() {
    fn walk(command: &clap::Command, path: &str) {
        // Building help forces argument propagation/resolution for this command.
        let mut owned = command.clone();
        let _ = owned.render_long_help();

        let mut ids = std::collections::HashMap::<String, usize>::new();
        for arg in command.get_arguments() {
            *ids.entry(arg.get_id().to_string()).or_default() += 1;
        }
        for (id, count) in &ids {
            assert_eq!(
                *count, 1,
                "subcommand `{path}` defines argument id `{id}` {count} times \
                 (clap id collision — the `ee search`/`fields` class of bug)",
            );
        }

        for sub in command.get_subcommands() {
            walk(sub, &format!("{path} {}", sub.get_name()));
        }
    }

    // Fully build the tree (propagates globals into subcommands) before walking.
    let mut root = Cli::command();
    root.build();
    walk(&root, "ee");
}

/// bd-39tzu.3 — `ee primer` and `ee orient --include-primer` must parse
/// without panicking across their documented argument combinations (the
/// `ee search`/`fields` arg-id collision class).
#[test]
fn ee_primer_parses_and_accesses_without_panicking() {
    for argv in [
        vec!["ee", "primer"],
        vec!["ee", "primer", "--json"],
        vec!["ee", "primer", "--tokens", "800"],
        vec!["ee", "primer", "--format", "markdown"],
        vec!["ee", "primer", "--format", "json", "--json"],
        vec!["ee", "primer", "--refresh"],
        vec!["ee", "primer", "--no-persist"],
        vec![
            "ee",
            "primer",
            "--tokens",
            "600",
            "--format",
            "markdown",
            "--refresh",
            "--no-persist",
            "--workspace",
            ".",
        ],
        vec!["ee", "orient", "task", "--include-primer"],
        vec![
            "ee",
            "orient",
            "task",
            "--fast",
            "--include-primer",
            "--json",
        ],
    ] {
        let parsed = Cli::try_parse_from(argv.clone());
        assert!(
            parsed.is_ok(),
            "`{}` must parse without panic; got {:?}",
            argv.join(" "),
            parsed.err()
        );
    }
}
