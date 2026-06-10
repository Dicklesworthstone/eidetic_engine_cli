//! Real-binary "no command panics" smoke gate.
//!
//! Companion to `cli_arg_hygiene.rs`. That file proves the clap *definition*
//! is internally consistent (no duplicate ids) at unit speed. This file proves
//! the *actual compiled binary* does not panic when an agent runs the commands
//! it is documented to run.
//!
//! WHY: ee 0.8.0 shipped with `ee search <query>` panicking at clap
//! parse/access time. A real-binary test that ran `ee search <query>` and
//! asserted "this did not PANIC" (as opposed to merely "this exited non-zero",
//! which a panic also satisfies) would have caught it. The pre-existing
//! `golden.rs` search test asserted `!status.success()` — which a panic happily
//! satisfies — so the crash slipped through.
//!
//! Contract enforced here, for every command exercised:
//!   * the process did not die from a Rust panic / abort
//!     (exit code 101 or signal-derived 134), and
//!   * stderr contains neither `panicked` nor clap's
//!     `Mismatch between definition and access` downcast message, and
//!   * a clean error envelope (`success:false`) is perfectly acceptable —
//!     we are guarding against CRASHES, not against commands that decline.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::CommandFactory;
use ee::cli::Cli;

const EE: &str = env!("CARGO_BIN_EXE_ee");

fn unique_workspace(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("ee-nopanic-{label}-{nonce}"));
    std::fs::create_dir_all(&dir).expect("create temp workspace");
    // Canonicalize so the path does not traverse a symlink (macOS `/tmp` ->
    // `/private/tmp`), which ee's DB-open guard would otherwise refuse. The
    // crash contract holds either way, but this lets the with-data paths run.
    dir.canonicalize().unwrap_or(dir)
}

fn run(workspace: &Path, args: &[&str]) -> Output {
    Command::new(EE)
        .args(args)
        .env("EE_WORKSPACE", workspace) // harmless if unused
        .current_dir(workspace)
        .output()
        .unwrap_or_else(|error| panic!("failed to spawn `ee {}`: {error}", args.join(" ")))
}

/// The crash contract: a clean non-zero exit is fine; a panic/abort is not.
fn assert_did_not_crash(label: &str, output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_no_panic_stderr(label, &stderr);
    assert_status_did_not_crash(label, output, &stderr);
}

fn assert_no_panic_stderr(label: &str, stderr: &str) {
    assert!(
        !stderr.contains("panicked"),
        "`{label}` panicked:\n{stderr}"
    );
    assert!(
        !stderr.contains("Mismatch between definition and access"),
        "`{label}` hit the clap arg id/type collision (the 0.8.0 `ee search` class):\n{stderr}"
    );
}

fn assert_status_did_not_crash(label: &str, output: &Output, stderr: &str) {
    // Rust panic => 101; SIGABRT/SIGSEGV-style death => 134/139 via shells.
    if let Some(code) = output.status.code() {
        assert!(
            code != 101 && code != 134 && code != 139,
            "`{label}` exited {code} (crash-like); stderr:\n{stderr}"
        );
    } else {
        // Killed by a signal with no code at all — definitely a crash.
        panic!("`{label}` was killed by a signal; stderr:\n{stderr}");
    }
}

/// Recursively collect every leaf subcommand's argv path (e.g. `["diag","search"]`).
fn leaf_paths(command: &clap::Command, prefix: &[String], out: &mut Vec<Vec<String>>) {
    let subs: Vec<&clap::Command> = command.get_subcommands().collect();
    if subs.is_empty() {
        if !prefix.is_empty() {
            out.push(prefix.to_vec());
        }
        return;
    }
    for sub in subs {
        if sub.get_name() == "help" {
            continue;
        }
        let mut next = prefix.to_vec();
        next.push(sub.get_name().to_string());
        leaf_paths(sub, &next, out);
    }
}

fn is_long_running_leaf(path: &[String]) -> bool {
    if path
        .iter()
        .any(|segment| matches!(segment.as_str(), "daemon" | "serve"))
    {
        return true;
    }

    const STREAMING_LEAVES: &[&[&str]] = &[
        &["perf", "live"],
        &["recorder", "follow"],
        &["subscribe", "stream"],
    ];

    STREAMING_LEAVES.iter().any(|expected| {
        path.len() == expected.len()
            && path
                .iter()
                .map(String::as_str)
                .zip(expected.iter().copied())
                .all(|(actual, expected)| actual == expected)
    })
}

/// Every documented subcommand must at least render `--help` from the real
/// binary without crashing. This auto-covers NEW subcommands with no manual
/// upkeep, and catches any command whose clap definition panics at build time.
#[test]
fn every_subcommand_help_does_not_crash() {
    let workspace = unique_workspace("help");
    let mut paths = Vec::new();
    leaf_paths(&Cli::command(), &[], &mut paths);
    assert!(
        paths.len() > 30,
        "expected a broad command surface, got {}",
        paths.len()
    );

    for path in paths {
        let mut argv: Vec<&str> = path.iter().map(String::as_str).collect();
        argv.push("--help");
        let output = run(&workspace, &argv);
        let label = format!("ee {} --help", path.join(" "));
        assert_did_not_crash(&label, &output);
        // --help must succeed (exit 0) — a non-zero here means a broken definition.
        assert!(
            output.status.success(),
            "`{label}` did not exit 0; stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// The agent operating loop, exercised against a REAL initialized workspace with
/// REAL arguments that reach `from_arg_matches` (the access path that panicked in
/// 0.8.0). These run the genuine retrieval/parse code, not `--help`.
#[test]
fn agent_loop_commands_do_not_crash_on_real_invocations() {
    let workspace = unique_workspace("loop");

    // Bring a workspace + one memory into being so downstream commands have data.
    assert_did_not_crash(
        "init",
        &run(&workspace, &["init", "--workspace", ".", "--json"]),
    );
    let remember = run(
        &workspace,
        &[
            "remember",
            "Run cargo fmt --check before every release.",
            "--workspace",
            ".",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--json",
        ],
    );
    assert_did_not_crash("remember", &remember);

    // The exact 0.8.0 crash surface and its neighbours, with real queries/flags.
    let invocations: &[&[&str]] = &[
        &["search", "format before release", "--workspace", "."],
        &[
            "search",
            "format before release",
            "--workspace",
            ".",
            "--json",
        ],
        &[
            "search",
            "q",
            "--workspace",
            ".",
            "--field",
            "kind=rule",
            "--json",
        ],
        &[
            "search",
            "q",
            "--workspace",
            ".",
            "--fields",
            "minimal",
            "--json",
        ],
        &[
            "search",
            "q",
            "--workspace",
            ".",
            "--limit",
            "5",
            "--explain",
            "--json",
        ],
        &["pack", "prepare a release", "--workspace", ".", "--json"],
        &[
            "pack",
            "prepare a release",
            "--workspace",
            ".",
            "--format",
            "markdown",
        ],
        &["context", "prepare a release", "--workspace", ".", "--json"],
        &["status", "--workspace", ".", "--json"],
        &["doctor", "--json"],
        &["orient", "ship a release", "--workspace", ".", "--json"],
        &["swarm", "brief", "--workspace", ".", "--json"],
        &["insights", "--workspace", ".", "--json"],
        &["health", "--workspace", ".", "--json"],
        &[
            "preflight",
            "check",
            "--cmd",
            "git status",
            "--workspace",
            ".",
            "--json",
        ],
        &[
            "diagnose-error",
            "--error-log",
            "error[E0277]: trait bound",
            "--workspace",
            ".",
            "--json",
        ],
        &["curate", "candidates", "--workspace", ".", "--json"],
    ];
    for argv in invocations {
        let output = run(&workspace, argv);
        assert_did_not_crash(&format!("ee {}", argv.join(" ")), &output);
    }
}

struct CappedOutput {
    output: Output,
    timed_out: bool,
}

fn assert_capped_did_not_crash(label: &str, result: &CappedOutput) {
    let stderr = String::from_utf8_lossy(&result.output.stderr);
    assert_no_panic_stderr(label, &stderr);
    // A timed-out command was killed by this test harness, not by its own
    // crash path. We still inspect stderr above for panic/downcast evidence.
    if !result.timed_out {
        assert_status_did_not_crash(label, &result.output, &stderr);
    }
}

/// Run the binary but kill it if it outlives `secs` (defends the exhaustive
/// sweep against any command that would block — we cannot enumerate every
/// command's required args, so some will wait on input we never supply).
fn run_capped(workspace: &Path, args: &[&str], secs: u64) -> CappedOutput {
    let mut child = Command::new(EE)
        .args(args)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("failed to spawn `ee {}`: {error}", args.join(" ")));

    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if child.try_wait().expect("poll ee").is_some() {
            return CappedOutput {
                output: child.wait_with_output().expect("collect ee output"),
                timed_out: false,
            };
        }

        if Instant::now() >= deadline {
            if child
                .try_wait()
                .expect("poll ee before timeout kill")
                .is_some()
            {
                return CappedOutput {
                    output: child.wait_with_output().expect("collect ee output"),
                    timed_out: false,
                };
            }
            let _ = child.kill();
            return CappedOutput {
                output: child.wait_with_output().expect("collect killed ee output"),
                timed_out: true,
            };
        }

        thread::sleep(Duration::from_millis(25));
    }
}

/// EXHAUSTIVE runtime sweep: every leaf subcommand (enumerated from the clap
/// tree, so nested commands like `swarm brief` / `diag search` / `memory show`
/// are all covered) is run through the REAL binary with real arguments that
/// reach `from_arg_matches`, in two forms (bare, and with a generic positional)
/// to maximize the chance of exercising the access path. The contract is the
/// same crash contract: a clean error envelope is fine, a panic/abort is not.
///
/// This is the broad analogue of the `ee search` regression — it would catch a
/// `fields`-style downcast panic (or any other runtime panic) in ANY command an
/// agent might run, not just the curated agent-loop set above.
#[test]
fn every_leaf_subcommand_real_invocation_does_not_crash() {
    let workspace = unique_workspace("sweep");
    // Seed a workspace with data so retrieval/graph paths execute for real.
    let _ = run(&workspace, &["init", "--workspace", ".", "--json"]);
    let _ = run(
        &workspace,
        &[
            "remember",
            "Async governor must be Send + Sync; fix E0277 by importing the trait",
            "--workspace",
            ".",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "rust,async",
            "--json",
        ],
    );
    let _ = run(
        &workspace,
        &["index", "rebuild", "--workspace", ".", "--json"],
    );

    let mut paths = Vec::new();
    leaf_paths(&Cli::command(), &[], &mut paths);

    let mut ran = 0usize;
    for path in &paths {
        if is_long_running_leaf(path) {
            continue;
        }
        let base: Vec<&str> = path.iter().map(String::as_str).collect();

        let mut bare = base.clone();
        bare.extend_from_slice(&["--workspace", ".", "--json"]);
        assert_capped_did_not_crash(
            &format!("ee {} [bare]", path.join(" ")),
            &run_capped(&workspace, &bare, 20),
        );

        let mut with_pos = base.clone();
        with_pos.push("generic test input");
        with_pos.extend_from_slice(&["--workspace", ".", "--json"]);
        assert_capped_did_not_crash(
            &format!("ee {} [pos]", path.join(" ")),
            &run_capped(&workspace, &with_pos, 20),
        );

        ran += 2;
    }
    assert!(
        ran > 100,
        "expected an exhaustive sweep over the command tree, ran only {ran}"
    );
}
