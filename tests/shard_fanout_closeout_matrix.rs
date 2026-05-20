use std::collections::BTreeSet;

const CLOSEOUT_SCRIPT: &str = include_str!("../scripts/shard_fanout_closeout_matrix.sh");

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[test]
fn closeout_matrix_tracks_every_shard_fanout_child() -> TestResult {
    let expected = [
        "bd-f6jfs.1",
        "bd-f6jfs.2",
        "bd-f6jfs.3",
        "bd-f6jfs.4",
        "bd-f6jfs.5",
        "bd-f6jfs.6",
        "bd-f6jfs.7",
        "bd-f6jfs.8",
        "bd-f6jfs.9",
        "bd-f6jfs.10",
    ];

    let mut missing = Vec::new();
    for bead in expected {
        if !CLOSEOUT_SCRIPT.contains(bead) {
            missing.push(bead);
        }
    }

    ensure(
        missing.is_empty(),
        format!("closeout matrix missing shard-fanout children: {missing:?}"),
    )
}

#[test]
fn closeout_matrix_covers_required_acceptance_criteria() -> TestResult {
    let required_phrases = [
        "ADR/docs/schema contract landed and linked",
        "Env registry, Failure-mode fixtures, and shard resolver docs/tests landed",
        "DbShardRouter and per-shard write ownership implemented",
        "Migration dry-run/apply/idempotence/partial failure evidence",
        "Cross-shard read/search/context parity evidence",
        "Per-shard audit-chain continuity and deterministic global timeline evidence",
        "Backup/restore side-path parity evidence",
        "Concurrency e2e/benchmark throughput evidence",
        "Rollback/off-switch/fail-closed evidence",
        "Failure-mode fixtures",
        "br dep cycles",
        "bv",
    ];

    let mut missing = Vec::new();
    for phrase in required_phrases {
        if !CLOSEOUT_SCRIPT.contains(phrase) {
            missing.push(phrase);
        }
    }

    ensure(
        missing.is_empty(),
        format!("closeout matrix missing proof criteria: {missing:?}"),
    )
}

#[test]
fn closeout_matrix_lists_only_rch_cargo_verification_commands() -> TestResult {
    let mut command_lines = BTreeSet::new();
    for line in CLOSEOUT_SCRIPT.lines() {
        if line.contains("\"cargo\"") || line.contains(" cargo ") {
            command_lines.insert(line.trim().to_owned());
        }
    }

    ensure(
        !command_lines.is_empty(),
        "closeout matrix must declare Cargo verification commands",
    )?;

    let forbidden = ["cargo check", "cargo test", "cargo clippy", "cargo bench"];
    for line in &command_lines {
        for phrase in forbidden {
            ensure(
                line.contains("\"rch\"") || line.contains("rch exec") || !line.contains(phrase),
                format!("non-RCH Cargo verification line found: {line}"),
            )?;
        }
    }

    for required in [
        r#""rch", "exec", "--", "cargo", "check", "--all-targets""#,
        r#""rch", "exec", "--", "cargo", "test", "--workspace", "--all-targets""#,
        r#""rch", "exec", "--", "cargo", "clippy", "--all-targets", "--", "-D", "warnings""#,
        "\"localCargoAllowed\": False",
    ] {
        ensure(
            CLOSEOUT_SCRIPT.contains(required),
            format!("missing RCH-only verification contract: {required}"),
        )?;
    }

    Ok(())
}
