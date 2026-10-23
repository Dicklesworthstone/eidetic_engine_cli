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

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code may unwrap/expect
use clap::{CommandFactory, Parser};
use ee::cli::{Cli, Command, PlanCommand};

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

#[test]
fn remember_and_note_typed_field_flags_do_not_collide_with_global_fields() {
    let remember = Cli::try_parse_from([
        "ee",
        "remember",
        "Remote verification won.",
        "--kind",
        "decision",
        "--field",
        "chosen=RCH remote",
        "--field",
        "options=local Cargo",
        "--fields",
        "standard",
    ])
    .expect("remember typed fields parse");
    let Some(Command::Remember(args)) = remember.command else {
        panic!("expected remember command");
    };
    assert_eq!(
        args.typed_field_assignments,
        ["chosen=RCH remote", "options=local Cargo"]
    );

    let note = Cli::try_parse_from([
        "ee",
        "note",
        "A failed prefetch attempt.",
        "--kind",
        "failure",
        "--field",
        "family=aggressive-prefetch",
    ])
    .expect("note typed field parses");
    let Some(Command::Note(args)) = note.command else {
        panic!("expected note command");
    };
    assert_eq!(args.typed_field_assignments, ["family=aggressive-prefetch"]);
}

/// `ee search` carries the read fence ADR 0059 requires on a retrieval surface
/// (bd-1n0np.8.13), and asking for nothing still means nothing.
///
/// The default assertions are the load-bearing half. The fence is implemented as
/// an opt-in pre-flight precisely so that every existing caller, golden and
/// snapshot is unaffected, and that claim is only worth anything if something
/// fails when the defaults drift. `Eventual` plus `strict == false` is what makes
/// `read_fence_preflight` return before it opens a store.
#[test]
fn ee_search_read_fence_defaults_to_eventual_and_parses_every_mode() {
    use ee::core::read_fence::ReadFence;

    let default = Cli::try_parse_from(["ee", "search", "some query"]).expect("plain search parses");
    let Some(Command::Search(args)) = default.command else {
        panic!("expected search command");
    };
    assert_eq!(
        args.read_fence,
        ReadFence::Eventual,
        "the default fence must stay eventual, or every existing search caller \
         starts paying for a store read it did not ask for"
    );
    assert!(
        !args.strict_read_fence,
        "search must not fail closed by default"
    );

    let strict = Cli::try_parse_from([
        "ee",
        "search",
        "some query",
        "--read-fence",
        "latest",
        "--strict-read-fence",
    ])
    .expect("search --read-fence latest --strict-read-fence parses");
    let Some(Command::Search(args)) = strict.command else {
        panic!("expected search command");
    };
    assert_eq!(args.read_fence, ReadFence::Latest);
    assert!(args.strict_read_fence);

    // The snapshot form carries a generation, so a malformed one must be a parse
    // error rather than a silent fallback to eventual.
    let snapshot =
        Cli::try_parse_from(["ee", "search", "some query", "--read-fence", "snapshot:12"])
            .expect("search --read-fence snapshot:12 parses");
    let Some(Command::Search(args)) = snapshot.command else {
        panic!("expected search command");
    };
    assert_eq!(args.read_fence, ReadFence::Snapshot(12));

    assert!(
        Cli::try_parse_from(["ee", "search", "q", "--read-fence", "banana"]).is_err(),
        "an unknown fence mode must be rejected, never silently treated as eventual"
    );
}

/// `ee context` and `ee why` carry the same fence as `ee search`, because ADR
/// 0059 names all three (bd-1n0np.8.13).
///
/// A DIFFERENT DEFAULT ON ONE SURFACE WOULD BE THE WORST OUTCOME: an agent that
/// fences its search and then packs from a stale derived asset has paid for the
/// guarantee and not received it, and nothing in the output would say so. So the
/// defaults are asserted per surface rather than assumed to be shared, even
/// though all three read the same parser.
///
/// `ee pack` is deliberately absent: it is a subcommand GROUP, so the flag would
/// have to be placed per leaf, which is a separate decision from threading the
/// fence through the three surfaces the ADR names.
#[test]
fn ee_context_and_why_carry_the_same_read_fence_defaults() {
    use ee::core::read_fence::ReadFence;

    let context =
        Cli::try_parse_from(["ee", "context", "some task"]).expect("plain context parses");
    let Some(Command::Context(args)) = context.command else {
        panic!("expected context command");
    };
    assert_eq!(
        args.read_fence,
        ReadFence::Eventual,
        "context must default to eventual or every pack caller pays for a store read"
    );
    assert!(!args.strict_read_fence);

    let context_strict = Cli::try_parse_from([
        "ee",
        "context",
        "some task",
        "--read-fence",
        "snapshot:9",
        "--strict-read-fence",
    ])
    .expect("context --read-fence snapshot:9 parses");
    let Some(Command::Context(args)) = context_strict.command else {
        panic!("expected context command");
    };
    assert_eq!(args.read_fence, ReadFence::Snapshot(9));
    assert!(args.strict_read_fence);

    let why = Cli::try_parse_from(["ee", "why", "mem-1"]).expect("plain why parses");
    let Some(Command::Why(args)) = why.command else {
        panic!("expected why command");
    };
    assert_eq!(
        args.read_fence,
        ReadFence::Eventual,
        "why must default to eventual"
    );
    assert!(!args.strict_read_fence);

    let why_latest = Cli::try_parse_from(["ee", "why", "mem-1", "--read-fence", "latest"])
        .expect("why --read-fence latest parses");
    let Some(Command::Why(args)) = why_latest.command else {
        panic!("expected why command");
    };
    assert_eq!(args.read_fence, ReadFence::Latest);

    for surface in [
        vec!["ee", "context", "t", "--read-fence", "banana"],
        vec!["ee", "why", "mem-1", "--read-fence", "banana"],
    ] {
        assert!(
            Cli::try_parse_from(surface.clone()).is_err(),
            "`{}` must reject an unknown fence mode rather than falling back to eventual",
            surface.join(" ")
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

/// The global workspace flag must work on either side of the subcommand.
/// A second local flag with a different Rust id still collides by long name.
#[test]
fn ee_plan_goal_workspace_flag_uses_global_path() {
    for argv in [
        vec![
            "ee",
            "plan",
            "goal",
            "--goal",
            "prepare release",
            "--workspace",
            ".",
            "--json",
        ],
        vec![
            "ee",
            "--workspace",
            ".",
            "plan",
            "goal",
            "--goal",
            "prepare release",
            "--json",
        ],
    ] {
        let parsed = Cli::try_parse_from(argv).expect("plan goal --workspace parses");
        assert_eq!(parsed.workspace.as_deref(), Some(std::path::Path::new(".")));
        match parsed.command {
            Some(Command::Plan(PlanCommand::Goal(args))) => {
                assert_eq!(args.goal, "prepare release");
            }
            other => panic!("expected plan goal command, got {other:?}"),
        }
    }
}

#[test]
fn recipe_explanation_and_promotion_arguments_are_unambiguous() {
    let cli = Cli::try_parse_from([
        "ee",
        "why",
        "plan",
        "plrec_example",
        "--task",
        "prepare release",
        "--json",
    ])
    .expect("recipe explanation parses");
    match cli.command {
        Some(Command::Why(args)) => {
            assert_eq!(args.memory_id, "plan");
            assert_eq!(args.recipe_id.as_deref(), Some("plrec_example"));
            assert_eq!(args.task.as_deref(), Some("prepare release"));
        }
        other => panic!("expected why, got {other:?}"),
    }
    assert!(
        Cli::try_parse_from([
            "ee",
            "curate",
            "apply",
            "curate_example",
            "--as-recipe",
            "Release"
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "ee",
            "curate",
            "apply",
            "curate_example",
            "--when",
            "release"
        ])
        .is_err()
    );
    let cli = Cli::try_parse_from([
        "ee",
        "curate",
        "apply",
        "curate_example",
        "--as-recipe",
        "Release",
        "--when",
        "prepare release",
        "--dry-run",
    ])
    .expect("recipe promotion parses");
    match cli.command {
        Some(Command::Curate(ee::cli::CurateCommand::Apply(args))) => {
            assert_eq!(args.as_recipe.as_deref(), Some("Release"));
            assert_eq!(args.when.as_deref(), Some("prepare release"));
            assert!(args.dry_run);
        }
        other => panic!("expected curate apply, got {other:?}"),
    }
}
