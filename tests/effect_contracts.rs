//! Effect contract integration tests (EE-TST-009).
//!
//! Verifies that read-only commands do not mutate workspace state,
//! and that the effect manifest accurately reflects command behavior.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;

const CLI_SOURCE: &str = include_str!("../src/cli/mod.rs");
const EFFECT_SOURCE: &str = include_str!("../src/core/effect.rs");
/// Live count of the normalized command paths `extract_command_path` emits.
///
/// Measured, not incremented: 454 after adding `ask --read-only`, by mirroring
/// `command_paths_in` over the marker-delimited body of `extract_command_path`
/// (`python3` over `src/cli/mod.rs`, same rule the gate uses — a path is
/// recovered only from `"literal".to_string()`).
///
/// It sat at 416 from `c097d4da9` until now while the real inventory grew to
/// 453. The drift is how this constant is normally wrong: an author adds one
/// command and increments by one without re-measuring, so every count someone
/// else forgot compounds silently. When this fails, re-measure the inventory;
/// do not add the delta of your own change to the old number.
const NORMALIZED_CLI_COMMAND_COUNT: usize = 454;
const MANIFEST_ONLY_OPTION_MODE_COMMANDS: &[&str] = &[
    "daemon background",
    "daemon foreground decay_sweep",
    "daemon foreground non-decay",
    "daemon start",
    "daemon stop",
    "orient decisions",
];

type TestResult = Result<(), String>;

fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
    if actual.eq(&expected) {
        Ok(())
    } else {
        Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
    }
}

fn hash_file(path: &Path) -> Option<u64> {
    let content = fs::read(path).ok()?;
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    Some(hasher.finish())
}

fn hash_directory(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    if let Ok(entries) = fs::read_dir(path) {
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        paths.sort_by_key(|e| e.path());
        for entry in paths {
            let p = entry.path();
            p.to_string_lossy().hash(&mut hasher);
            if p.is_file() {
                if let Some(h) = hash_file(&p) {
                    h.hash(&mut hasher);
                }
            } else if p.is_dir() {
                hash_directory(&p).hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Every path under `root`, mapped to its content hash (`None` for a
/// directory) so a drift report can explain ANY `hash_directory` difference --
/// including an added or removed empty directory, which a files-only map
/// would miss and then report "no changed paths" on a real mismatch.
fn snapshot_directory(root: &Path) -> BTreeMap<String, Option<u64>> {
    let mut snapshot = BTreeMap::new();
    collect_snapshot(root, root, &mut snapshot);
    snapshot
}

fn collect_snapshot(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<u64>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let key = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        if path.is_dir() {
            out.insert(key, None);
            collect_snapshot(root, &path, out);
        } else {
            out.insert(key, hash_file(&path));
        }
    }
}

/// Name what moved between two snapshots. Empty when they agree.
fn describe_directory_drift(
    before: &BTreeMap<String, Option<u64>>,
    after: &BTreeMap<String, Option<u64>>,
) -> Vec<String> {
    let mut drift = Vec::new();
    for (path, before_hash) in before {
        match after.get(path) {
            None => drift.push(format!("removed: {path}")),
            Some(after_hash) if after_hash != before_hash => {
                drift.push(format!("content changed: {path}"));
            }
            Some(_) => {}
        }
    }
    for path in after.keys() {
        if !before.contains_key(path) {
            drift.push(format!("created: {path}"));
        }
    }
    drift
}

/// The path out of a `describe_directory_drift` entry (`"created: a/b"`).
fn drift_path(entry: &str) -> String {
    entry
        .split_once(": ")
        .map_or_else(|| entry.to_owned(), |(_, path)| path.to_owned())
}

/// The two paths whose content moves on a read-write OPEN, with no database
/// write of any kind.
///
/// This is a scrubber, so it is spelled out rather than pattern-matched, and
/// the mechanism is named so nobody has to take it on trust:
///
/// * `.ee/ee.write.lock` -- `advance_flock_gate_epoch` (src/db/mod.rs) does
///   `write_all` of a 21-byte counter into this file on every successful
///   exclusive flock, and that flock is taken for exactly
///   `(DatabaseLocation::File, DatabaseOpenMode::ReadWrite)` at open
///   (src/db/mod.rs:1130-1137). Opening wide is sufficient; committing is not
///   required.
/// * `.ee/ee.db-shm` -- SQLite's WAL shared-memory index, mapped by the
///   connection rather than written by a transaction.
///
/// EXACTLY these two, and nothing else. `.ee/ee.db`, `.ee/ee.db-wal`,
/// `-wal-cert`, and every audit, cache and pack path stay fully hashed, which
/// is what keeps a real write catchable. bd-czj3e's own filing spells out the
/// trap in widening this further: narrowing the instrument BEFORE the product
/// question was settled would have turned the row green and destroyed the
/// only signal pointing at the read-write open. That question is settled now
/// -- `search`/`similar`/`search --all-workspaces` are declared
/// `append_only_write` -- so the instrument may finally be aligned with the
/// contract its own failure message states.
fn is_open_artifact(path: &str) -> bool {
    let database = format!(".ee{}ee.db", std::path::MAIN_SEPARATOR);
    path == format!(".ee{}ee.write.lock", std::path::MAIN_SEPARATOR)
        || path == format!("{database}-shm")
}

/// `true` if a drift entry is evidence of a DURABLE WRITE, not merely of a
/// read-write handle having been opened.
///
/// The distinction is load-bearing and was established by a run, not by
/// reading: bd-czj3e's original filing saw only `.ee/ee.db-shm` and
/// `.ee/ee.write.lock` move and concluded "no audit row landed". Both of
/// those are [`is_open_artifact`] paths. `ee why` opens read-write to run
/// `migrate()` and moves them without writing a thing.
///
/// `.ee/ee.db` and `.ee/ee.db-wal` are different: content only moves there
/// when something commits. `-wal-cert` is a FrankenSQLite certification
/// sidecar that tracks the WAL, so it is engine-level corroboration rather
/// than an independent signal, and is deliberately NOT accepted on its own.
fn is_durable_write(entry: &str) -> bool {
    let path = drift_path(entry);
    let database = format!(".ee{}ee.db", std::path::MAIN_SEPARATOR);
    let wal = format!("{database}-wal");
    path == database || path == wal
}

/// Assert a surface mutated no durable state, NAMING what moved.
///
/// The failure message has always promised "database, WAL, audit, cache, or
/// pack state". The condition used to be full-tree `hash_directory` equality,
/// which is strictly wider than that sentence: it also fails on
/// [`is_open_artifact`] paths, whose content moves when a command merely opens
/// the database read-write. `ee why` does exactly that, to run `migrate()`
/// before reading, so under the old condition a genuine read could never pass.
///
/// bd-czj3e decision (2). The instrument now checks what it says it checks.
/// The narrowing is two named paths with a cited mechanism, NOT a relaxation
/// of the comparison: every other path -- `.ee/ee.db`, `-wal`, `-wal-cert`,
/// audit, cache, pack, and anything outside `.ee/` -- is still hashed and
/// still fails this assertion. A surface that appends one audit row is still
/// caught, which is the property that made this test find bd-czj3e at all.
///
/// The excluded paths are REPORTED on failure rather than dropped silently,
/// so a future reader can see what the scrubber swallowed instead of having
/// to re-derive the exclusion list from this doc comment.
fn ensure_workspace_unchanged(
    workspace: &Path,
    before_hash: u64,
    before_snapshot: &BTreeMap<String, Option<u64>>,
    surface: &str,
) -> TestResult {
    let after_hash = hash_directory(workspace);
    if after_hash == before_hash {
        return Ok(());
    }
    let drift = describe_directory_drift(before_snapshot, &snapshot_directory(workspace));
    // Hashes disagree but the snapshot walk names nothing. That is the two
    // detectors contradicting each other, not a clean run, and it must not
    // fall through the partition below as an empty `durable` list.
    if drift.is_empty() {
        return Err(format!(
            "{surface}: workspace hash moved {before_hash} -> {after_hash} but the snapshot \
             diff named no path; the drift detector and the hash disagree"
        ));
    }
    let (ignored, durable): (Vec<&String>, Vec<&String>) = drift
        .iter()
        .partition(|entry| is_open_artifact(&drift_path(entry.as_str())));
    if durable.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{surface} must not mutate database, WAL, audit, cache, or pack state: \
         hash {before_hash} -> {after_hash}; durable drift: {durable:?}; \
         open-artifact drift ignored by design: {ignored:?}"
    ))
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run ee: {e}"))?;
    Ok(output)
}

/// Slice the body of `extract_command_path` out of the CLI source.
///
/// Split out so the extractor below and the shape gate that guards it read the
/// SAME body, and so both can be driven against a synthetic fixture.
fn cli_extract_command_path_body() -> Result<&'static str, String> {
    let start_marker = "fn extract_command_path(cli: &Cli) -> String {";
    let end_marker = "\n    /// Returns a stable identifier";
    let start = CLI_SOURCE
        .find(start_marker)
        .ok_or_else(|| "extract_command_path function must exist".to_owned())?;
    let rest = &CLI_SOURCE[start..];
    let end = rest
        .find(end_marker)
        .ok_or_else(|| "extract_command_path function end marker must exist".to_owned())?;
    Ok(&rest[..end])
}

/// Recover the command paths a function body emits as quoted literals.
///
/// Unchanged logic, lifted verbatim so it can also run against a fixture.
fn command_paths_in(function: &str) -> Vec<String> {
    let mut commands = Vec::new();

    for line in function.lines() {
        let Some(to_string_at) = line.find(".to_string()") else {
            continue;
        };
        let prefix = &line[..to_string_at];
        let Some(last_quote) = prefix.rfind('"') else {
            continue;
        };
        let Some(first_quote) = prefix[..last_quote].rfind('"') else {
            continue;
        };
        commands.push(prefix[first_quote + 1..last_quote].to_owned());
    }

    commands.sort();
    commands.dedup();
    commands
}

fn command_paths_from_cli_extract_function() -> Result<Vec<String>, String> {
    Ok(command_paths_in(cli_extract_command_path_body()?))
}

/// Lines inside a command-path function that build a `String` by any means
/// OTHER than a quoted literal.
///
/// `command_paths_in` recovers a path only when it appears as
/// `"literal".to_string()`. That recovery is complete exactly while the
/// function builds every path that way. A computed path -- `format!("team
/// {sub}")`, a variable, a concatenation -- would still be emitted by the CLI,
/// would be invisible to the extractor, and so would never be checked for an
/// effect declaration. The consequence is not a red test: `src/core/effect.rs`
/// is the classification an agent consults before running something, so an
/// unseen command is one that can be mis-declared with nothing noticing.
///
/// Deliberately errs toward flagging. For this gate a false positive costs an
/// author one edit or one justification; a false negative costs an agent a
/// destructive command reported as read-only.
fn non_literal_path_constructions(function: &str) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    for (index, line) in function.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let quote_count = trimmed.matches('"').count();
        let is_quoted_literal = trimmed.contains(".to_string()") && quote_count >= 2;
        let builds_a_string = trimmed.contains("format!")
            || trimmed.contains(".to_owned()")
            || trimmed.contains("String::from")
            || trimmed.contains(".into()")
            || trimmed.contains(".join(")
            || trimmed.contains("concat")
            || trimmed.contains("push_str")
            || (trimmed.contains(".to_string()") && quote_count < 2);
        if builds_a_string && !is_quoted_literal {
            hits.push((index + 1, trimmed.to_owned()));
        }
    }
    hits
}

/// A command path the extractor cannot see is a command the effect-coverage
/// test never checks. Pin the SHAPE instead of trusting it.
#[test]
fn extract_command_path_builds_every_path_from_a_quoted_literal() -> TestResult {
    let body = cli_extract_command_path_body()?;
    let hits = non_literal_path_constructions(body);
    if hits.is_empty() {
        return Ok(());
    }
    Err(format!(
        "extract_command_path must build every command path from a quoted literal, \
         or command_paths_in cannot see it and the effect manifest is never checked \
         for it. Non-literal construction(s): {hits:?}"
    ))
}

/// The `--dry-run` guard in `run_in_process` must not do its own manifest
/// lookup.
///
/// bd-qked7: the guard used to be `if let Some(effect) =
/// manifest.get(&command_path) { ... }`, which skipped the entire check when a
/// path had no declaration — 32 of 453 paths at the time. It now delegates to
/// `dry_run_refusal_message`, whose miss arm refuses (unit-tested in
/// `src/cli/mod.rs::tests::dry_run_on_an_undeclared_command_path_is_refused`).
///
/// Pinning the SHAPE here because the refusal itself is unreachable through
/// `run()`: Clap rejects `--dry-run` for any command whose args struct has no
/// such flag, so once every path is declared no CLI invocation can reach the
/// miss arm. A behavioural test cannot catch a regression to the fail-open
/// form; this can.
#[test]
fn dry_run_guard_delegates_instead_of_reopening_the_fail_open_lookup() -> TestResult {
    let start = "if args.iter().any(|arg| arg == \"--dry-run\") {";
    let block_start = CLI_SOURCE
        .find(start)
        .ok_or_else(|| "the --dry-run guard must exist in run_in_process".to_owned())?;
    let rest = &CLI_SOURCE[block_start..];
    let block_end = rest
        .find("\n    match cli.command {")
        .ok_or_else(|| "the --dry-run guard must precede the command dispatch".to_owned())?;
    let block = &rest[..block_end];

    if !block.contains("dry_run_refusal_message(") {
        return Err(format!(
            "the --dry-run guard must route through dry_run_refusal_message, which fails \
             CLOSED on a manifest miss. Guard body: {block:?}"
        ));
    }
    if block.contains("manifest.get(") {
        return Err(format!(
            "the --dry-run guard must not look the command path up itself: an inline \
             `manifest.get()` is how the guard silently skipped every undeclared command \
             (bd-qked7). Guard body: {block:?}"
        ));
    }
    Ok(())
}

/// Prove the gate above can fail, and that the extractor alone could not.
///
/// The fixture is a command-path function whose `team` arm is computed. The
/// OLD behaviour -- extraction alone -- silently returns only the literal arm,
/// so `team ...` would never be checked for an effect declaration. The gate
/// catches it.
#[test]
fn shape_gate_catches_a_computed_path_the_extractor_misses() -> TestResult {
    const COMPUTED_PATH_FIXTURE: &str = concat!(
        "fn extract_command_path(cli: &Cli) -> String {\n",
        "    match cli.command {\n",
        "        Some(Command::Backup(_)) => \"backup create\".to_string(),\n",
        "        Some(Command::Team(team)) => format!(\"team {}\", team.verb()),\n",
        "    }\n",
        "}\n",
    );

    // Before: extraction alone sees only the literal arm and reports success.
    let extracted = command_paths_in(COMPUTED_PATH_FIXTURE);
    ensure(
        extracted,
        vec!["backup create".to_owned()],
        "extractor alone must miss the computed path (this is the gap being closed)",
    )?;

    // After: the shape gate names the line the extractor could not see.
    let hits = non_literal_path_constructions(COMPUTED_PATH_FIXTURE);
    ensure(
        hits.len(),
        1,
        &format!("shape gate must flag exactly the computed arm, got {hits:?}"),
    )?;
    ensure(
        hits[0].1.contains("format!"),
        true,
        &format!("flagged line must be the computed one, got {:?}", hits[0]),
    )?;

    // Control: the same gate must NOT fire on an all-literal body, or it would
    // be flagging everything and proving nothing.
    const LITERAL_ONLY_FIXTURE: &str = concat!(
        "fn extract_command_path(cli: &Cli) -> String {\n",
        "    match cli.command {\n",
        "        Some(Command::Backup(_)) => \"backup create\".to_string(),\n",
        "    }\n",
        "}\n",
    );
    ensure(
        non_literal_path_constructions(LITERAL_ONLY_FIXTURE).len(),
        0,
        "shape gate must not fire on an all-literal body",
    )
}

// ============================================================================
// No-Mutation Contract Tests
// ============================================================================

#[test]
fn status_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be valid UTF-8".to_string())?;

    let before = hash_directory(workspace);
    let output = run_ee(&["status", "--workspace", workspace_arg, "--json"])?;
    let after = hash_directory(workspace);

    ensure(
        output.status.success() || output.status.code() == Some(3),
        true,
        "status exits 0 or 3",
    )?;
    ensure(before, after, "workspace unchanged by status command")
}

#[test]
fn bare_status_inspects_current_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be valid UTF-8".to_string())?;

    // Initialize the workspace first
    let init_output = run_ee(&["init", "--workspace", workspace_arg])?;
    ensure(init_output.status.success(), true, "init should succeed")?;

    // Run bare status from the workspace directory (using --workspace to simulate cwd)
    // The fix ensures that when --workspace is omitted, it defaults to "."
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["status", "--json"])
        .current_dir(workspace)
        .output()
        .map_err(|e| format!("Failed to run ee status: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The status should report the workspace with actual content, not null.
    // JSON output uses "workspace":{"source":..., "root":...} structure.
    // Note: other subsystems may legitimately report "not_inspected" (e.g., agent_inventory),
    // so we only check that the workspace field is populated.
    ensure(
        stdout.contains("\"workspace\":{\"source\":"),
        true,
        "bare status should report workspace with source field populated",
    )?;
    ensure(
        !stdout.contains("\"workspace\":null"),
        true,
        "bare status should not have null workspace",
    )
}

#[test]
fn canonical_database_reads_leave_initialized_workspace_byte_identical() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = temp.path();
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be valid UTF-8".to_owned())?;

    let init = run_ee(&["--workspace", workspace_arg, "--json", "init"])?;
    ensure(init.status.success(), true, "init should succeed")?;
    let remember = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "Run cargo fmt --check before release.",
    ])?;
    ensure(remember.status.success(), true, "remember should succeed")?;
    let remember_json: serde_json::Value = serde_json::from_slice(&remember.stdout)
        .map_err(|error| format!("parse remember JSON: {error}"))?;
    let memory_id = remember_json["data"]["public_id"]
        .as_str()
        .or_else(|| remember_json["data"]["memory_id"].as_str())
        .or_else(|| remember_json["data"]["id"].as_str())
        .ok_or_else(|| format!("remember response missing memory id: {remember_json}"))?
        .to_owned();

    let rebuild = run_ee(&["--workspace", workspace_arg, "--json", "index", "rebuild"])?;
    ensure(
        rebuild.status.success(),
        true,
        "index rebuild should succeed",
    )?;

    // `search` is deliberately ABSENT from these cases. It appends a retrieval
    // audit row on every user-facing query (ADR 0071, bd-l8dn0), so it cannot
    // leave the workspace byte-identical and never could -- this case asserted
    // the behaviour the manifest claimed, not the behaviour the code has.
    // `retrieval_surfaces_append_exactly_one_audit_row_and_nothing_else` below
    // replaces it with the opposite assertion, which is the stronger one:
    // deleting a case only stops a red, and stops it by asking less.
    let cases = [
        (
            "why",
            vec![
                "--workspace",
                workspace_arg,
                "--json",
                "why",
                memory_id.as_str(),
            ],
        ),
        (
            "status",
            vec!["--workspace", workspace_arg, "--json", "status"],
        ),
        (
            "pack --read-only",
            vec![
                "--workspace",
                workspace_arg,
                "--json",
                "pack",
                "prepare release",
                "--read-only",
                "--source-mode",
                "lexical_only",
            ],
        ),
    ];

    for (surface, args) in cases {
        let before = hash_directory(workspace);
        let before_snapshot = snapshot_directory(workspace);
        let output = run_ee(&args)?;
        ensure(
            output.status.success() || output.status.code() == Some(3),
            true,
            &format!(
                "{surface} should complete; stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        ensure_workspace_unchanged(workspace, before, &before_snapshot, surface)?;
    }
    Ok(())
}

/// The behavioural half of bd-czj3e: `ee search` is a writer.
///
/// `search` was declared `read_only_db` and listed above as a canonical
/// database read while the production path opens a WRITABLE handle after
/// releasing the read pin and appends a retrieval audit row
/// (`src/core/search.rs:7652-7669`). ADR 0071 reads the absence of those rows
/// as `never_retrieved`, so the write is deliberate and the declaration was
/// the wrong half.
///
/// `why` runs first as a NEGATIVE CONTROL. Without it this test would pass on
/// any store churn at all -- a WAL or `-shm` touch from merely opening the
/// database read-only would look exactly like an audit append. `why` reads the
/// same store through the same binary and must leave it byte-identical, so a
/// green here means the detector discriminates rather than that everything
/// dirties the workspace.
#[test]
fn retrieval_surfaces_append_exactly_one_audit_row_and_nothing_else() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = temp.path();
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be valid UTF-8".to_owned())?;

    let init = run_ee(&["--workspace", workspace_arg, "--json", "init"])?;
    ensure(init.status.success(), true, "init should succeed")?;
    let remember = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "Run cargo fmt --check before release.",
    ])?;
    ensure(remember.status.success(), true, "remember should succeed")?;
    let remember_json: serde_json::Value = serde_json::from_slice(&remember.stdout)
        .map_err(|error| format!("parse remember JSON: {error}"))?;
    let memory_id = remember_json["data"]["public_id"]
        .as_str()
        .or_else(|| remember_json["data"]["memory_id"].as_str())
        .or_else(|| remember_json["data"]["id"].as_str())
        .ok_or_else(|| format!("remember response missing memory id: {remember_json}"))?
        .to_owned();
    let rebuild = run_ee(&["--workspace", workspace_arg, "--json", "index", "rebuild"])?;
    ensure(
        rebuild.status.success(),
        true,
        "index rebuild should succeed",
    )?;

    // Negative control: `ee why` also takes a WRITABLE handle -- it opens
    // read-write to run `migrate()` before reading (src/cli/mod.rs:54012) --
    // so it exercises the open-artifact paths without appending anything. If
    // the discriminator below called that a write, it would call every
    // command a write and pin nothing.
    let control_snapshot = snapshot_directory(workspace);
    let why = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "why",
        memory_id.as_str(),
    ])?;
    ensure(
        why.status.success() || why.status.code() == Some(3),
        true,
        &format!(
            "why should complete; stdout={} stderr={}",
            String::from_utf8_lossy(&why.stdout),
            String::from_utf8_lossy(&why.stderr)
        ),
    )?;
    let control_drift = describe_directory_drift(&control_snapshot, &snapshot_directory(workspace));
    ensure(
        control_drift
            .iter()
            .any(|entry| is_durable_write(entry.as_str())),
        false,
        &format!("why must not write the database or WAL; drift was {control_drift:?}"),
    )?;

    let before = hash_directory(workspace);
    let before_snapshot = snapshot_directory(workspace);
    let search = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "search",
        "format before release",
        "--source-mode",
        "lexical_only",
    ])?;
    ensure(
        search.status.success(),
        true,
        &format!(
            "search should succeed; stdout={} stderr={}",
            String::from_utf8_lossy(&search.stdout),
            String::from_utf8_lossy(&search.stderr)
        ),
    )?;

    let after = hash_directory(workspace);
    let drift = describe_directory_drift(&before_snapshot, &snapshot_directory(workspace));
    if after == before {
        return Err(
            "search must append a retrieval audit row: the workspace was byte-identical, so \
             either the ADR 0071 recording regressed or the manifest's append_only_write \
             declaration for `search` is now wrong"
                .to_owned(),
        );
    }
    // Bound the change to what the manifest DECLARES, and no tighter. The
    // entry is append_only_write("search", ["audit_log"]) with empty
    // `derived_paths` and empty `workspace_files`, and `append_only`'s
    // contract explicitly permits "queues or refreshes derived index after
    // new records commit". So the pin is: nothing outside the store moved.
    let store_root = format!(".ee{}", std::path::MAIN_SEPARATOR);
    let outside: Vec<String> = drift
        .iter()
        .map(|entry| drift_path(entry.as_str()))
        .filter(|path| path != ".ee" && !path.starts_with(&store_root))
        .collect();
    ensure(
        outside.is_empty(),
        true,
        &format!("search must write no workspace file outside the store; drift was {drift:?}"),
    )?;
    // The declared surface actually moved. This is the half the control
    // above makes meaningful: `.ee/ee.db-shm` and `.ee/ee.write.lock` change
    // on any read-write OPEN, so matching them would prove only that a handle
    // was taken. `is_durable_write` ignores both and requires the database or
    // its WAL, which is what appending a row moves.
    ensure(
        drift.iter().any(|entry| is_durable_write(entry.as_str())),
        true,
        &format!("search must write the database or its WAL; drift was {drift:?}"),
    )
}

#[test]
fn check_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be valid UTF-8".to_string())?;

    let before = hash_directory(workspace);
    let output = run_ee(&["check", "--workspace", workspace_arg, "--json"])?;
    let after = hash_directory(workspace);

    ensure(
        output.status.success() || output.status.code() == Some(3),
        true,
        "check exits 0 or 3",
    )?;
    ensure(before, after, "workspace unchanged by check command")
}

#[test]
fn capabilities_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();

    let before = hash_directory(workspace);
    let output = run_ee(&["capabilities", "--json"])?;
    let after = hash_directory(workspace);

    ensure(output.status.success(), true, "capabilities succeeds")?;
    ensure(before, after, "workspace unchanged by capabilities command")
}

#[test]
fn version_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();

    let before = hash_directory(workspace);
    let output = run_ee(&["version", "--json"])?;
    let after = hash_directory(workspace);

    ensure(output.status.success(), true, "version succeeds")?;
    ensure(before, after, "workspace unchanged by version command")
}

#[test]
fn health_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();

    let before = hash_directory(workspace);
    let output = run_ee(&["health", "--json"])?;
    let after = hash_directory(workspace);

    ensure(
        output.status.success() || output.status.code() == Some(6),
        true,
        "health exits 0 or 6",
    )?;
    ensure(before, after, "workspace unchanged by health command")
}

#[test]
fn introspect_command_does_not_mutate_workspace() -> TestResult {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let workspace = temp.path();

    let before = hash_directory(workspace);
    let output = run_ee(&["introspect", "--json"])?;
    let after = hash_directory(workspace);

    ensure(output.status.success(), true, "introspect succeeds")?;
    ensure(before, after, "workspace unchanged by introspect command")
}

// ============================================================================
// Effect Manifest Contract Tests
// ============================================================================

#[test]
fn effect_manifest_includes_status_as_read_only() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();
    let status = manifest
        .get("status")
        .ok_or_else(|| "status not in manifest".to_string())?;

    ensure(
        status.default_effect,
        EffectClass::ReadOnly,
        "status is read_only",
    )?;
    ensure(status.is_safe_mid_task(), true, "status is safe mid-task")
}

#[test]
fn effect_manifest_includes_swarm_brief_as_read_only() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();
    let effect = manifest
        .get("swarm brief")
        .ok_or_else(|| "swarm brief not in manifest".to_string())?;

    ensure(
        effect.default_effect,
        EffectClass::ReadOnly,
        "swarm brief is read_only",
    )?;
    ensure(
        effect.mutation_contract.side_effect_class,
        SideEffectClass::ReadOnly,
        "swarm brief side effect class",
    )?;
    ensure(
        effect.is_safe_mid_task(),
        true,
        "swarm brief is safe mid-task",
    )
}

#[test]
fn effect_manifest_includes_perf_commands_as_read_only() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();
    for command in ["perf compare", "perf budget check", "perf explain-latency"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{command} is read_only"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::ReadOnly,
            &format!("{command} side effect class"),
        )?;
        ensure(
            effect.is_safe_mid_task(),
            true,
            &format!("{command} is safe mid-task"),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_includes_remember_as_durable_write() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();
    let remember = manifest
        .get("remember")
        .ok_or_else(|| "remember not in manifest".to_string())?;

    ensure(
        remember.default_effect,
        EffectClass::DurableMemoryWrite,
        "remember is durable_memory_write",
    )?;
    ensure(
        remember.is_safe_mid_task(),
        false,
        "remember is not safe mid-task",
    )?;
    ensure(remember.requires_audit, true, "remember requires audit")
}

#[test]
fn effect_manifest_includes_index_rebuild_as_derived_write() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();
    let rebuild = manifest
        .get("index rebuild")
        .ok_or_else(|| "index rebuild not in manifest".to_string())?;

    ensure(
        rebuild.default_effect,
        EffectClass::DerivedArtifactWrite,
        "index rebuild is derived_artifact_write",
    )?;
    ensure(
        rebuild.write_surfaces.derived_paths.contains(&".ee/index/"),
        true,
        "index rebuild writes to .ee/index/",
    )
}

#[test]
fn effect_manifest_classifies_search_recalibration_as_derived_write() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();
    let recalibration = manifest
        .get("search --recalibrate-now")
        .ok_or_else(|| "search --recalibrate-now not in manifest".to_owned())?;
    ensure(
        recalibration.default_effect,
        EffectClass::DerivedArtifactWrite,
        "search recalibration writes a rebuildable derived artifact",
    )?;
    ensure(
        recalibration
            .write_surfaces
            .derived_paths
            .contains(&".ee/search/calibration.jsonl"),
        true,
        "search recalibration declares its calibration JSONL write",
    )
}

#[test]
fn effect_manifest_distinguishes_append_only_writes() -> TestResult {
    use ee::core::effect::{EffectManifest, IdempotencyClass, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in [
        "artifact register",
        "import cass",
        "import jsonl",
        "import eidetic-legacy",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::AppendOnly,
            &format!("{command} is append-only"),
        )?;
        ensure(
            effect.idempotency,
            IdempotencyClass::Idempotent,
            &format!("{command} retries by idempotency key"),
        )?;
        ensure(
            effect.requires_audit,
            true,
            &format!("{command} writes audit"),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_imports_are_duplicate_safe_append_only() -> TestResult {
    use ee::core::effect::{EffectManifest, IdempotencyClass, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in ["import cass", "import jsonl", "import eidetic-legacy"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        let contract = &effect.mutation_contract;

        ensure(
            contract.side_effect_class,
            SideEffectClass::AppendOnly,
            &format!("{command} append-only contract"),
        )?;
        ensure(
            effect.idempotency,
            IdempotencyClass::Idempotent,
            &format!("{command} retries are idempotent"),
        )?;
        ensure(
            contract.idempotency_key,
            Some("source hash"),
            &format!("{command} duplicate key"),
        )?;
        ensure(
            contract
                .db_generation_effect
                .contains("unchanged when idempotency key matches"),
            true,
            &format!("{command} duplicate import leaves DB generation unchanged"),
        )?;
        ensure(
            contract
                .dry_run_behavior
                .is_some_and(|behavior| behavior.contains("no DB rows")),
            true,
            &format!("{command} dry-run is a storage no-op"),
        )?;
        ensure(
            contract.recovery_behavior.contains("partial append"),
            true,
            &format!("{command} failed transaction rolls back partial append"),
        )?;
        ensure(
            effect.requires_audit,
            true,
            &format!("{command} audit required"),
        )?;
        ensure(
            effect.write_surfaces.db_tables.contains(&"audit_log"),
            true,
            &format!("{command} writes audit_log"),
        )?;
    }

    Ok(())
}

#[test]
fn effect_manifest_covers_all_normalized_cli_command_paths() -> TestResult {
    use ee::core::effect::EffectManifest;

    let commands = command_paths_from_cli_extract_function()?;
    let manifest = EffectManifest::build();
    let missing = commands
        .iter()
        .filter(|command| manifest.get(command).is_none())
        .cloned()
        .collect::<Vec<_>>();

    // Both conditions are evaluated before either is reported. The count check
    // used to run first and return early, so whenever the command inventory
    // drifted -- which is exactly when new commands are most likely to lack a
    // declaration -- this test aborted BEFORE the coverage check it is named
    // for. The guard was silent about coverage for as long as the count was
    // wrong. Neither check is removed and neither is relaxed; they simply can
    // no longer mask each other, and the coverage number is now always stated.
    let mut failures: Vec<String> = Vec::new();
    if !missing.is_empty() {
        failures.push(format!(
            "effect manifest is missing {} of {} normalized command paths: {missing:?}",
            missing.len(),
            commands.len()
        ));
    }
    if commands.len() != NORMALIZED_CLI_COMMAND_COUNT {
        failures.push(format!(
            "normalized CLI command count: expected {NORMALIZED_CLI_COMMAND_COUNT}, got {}",
            commands.len()
        ));
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[test]
fn effect_manifest_has_no_undocumented_extra_cli_paths() -> TestResult {
    use std::collections::BTreeSet;

    use ee::core::effect::EffectManifest;

    let commands = command_paths_from_cli_extract_function()?;
    let command_set = commands.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let allowed_manifest_only = MANIFEST_ONLY_OPTION_MODE_COMMANDS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();

    let manifest = EffectManifest::build();
    let manifest_only = manifest
        .command_paths()
        .into_iter()
        .filter(|command| !command_set.contains(command))
        .collect::<BTreeSet<_>>();

    if manifest_only == allowed_manifest_only {
        Ok(())
    } else {
        Err(format!(
            "effect manifest has unexpected command paths not emitted by CLI normalization: {manifest_only:?}"
        ))
    }
}

/// The workspace-drift scrubber is itself tested, in both directions.
///
/// `ensure_workspace_unchanged` now ignores two named paths. An exclusion
/// list that nothing exercises is how a gate quietly becomes a blanket pass:
/// widen `is_open_artifact` by one careless pattern -- `starts_with(".ee/")`,
/// say, or a `-wal` typo -- and every caller keeps passing while the
/// assertion stops meaning anything. This pins both arms against synthetic
/// drift entries, so it needs no workspace and no ee invocation.
#[test]
fn workspace_drift_scrubber_ignores_open_artifacts_and_nothing_else() -> TestResult {
    let sep = std::path::MAIN_SEPARATOR;
    for ignored in [
        format!(".ee{sep}ee.write.lock"),
        format!(".ee{sep}ee.db-shm"),
    ] {
        ensure(
            is_open_artifact(&ignored),
            true,
            &format!("{ignored} is an open artifact"),
        )?;
    }
    // The paths a real write lands on MUST NOT be scrubbed. `-wal` and
    // `-wal-cert` are deliberately here: they are adjacent in name to the
    // two exclusions and are exactly what a careless prefix rule would eat.
    for counted in [
        format!(".ee{sep}ee.db"),
        format!(".ee{sep}ee.db-wal"),
        format!(".ee{sep}ee.db-wal-cert"),
        format!(".ee{sep}index{sep}meta.json"),
        format!(".ee{sep}packs{sep}latest.json"),
        "AGENTS.md".to_owned(),
    ] {
        ensure(
            is_open_artifact(&counted),
            false,
            &format!("{counted} must still be hashed"),
        )?;
    }
    // `is_durable_write` takes a drift ENTRY (`"content changed: <path>"`),
    // not a bare path, and the two predicates must not overlap.
    ensure(
        is_durable_write(&format!("content changed: .ee{sep}ee.db-wal")),
        true,
        "a WAL content change is a durable write",
    )?;
    ensure(
        is_durable_write(&format!("content changed: .ee{sep}ee.write.lock")),
        false,
        "a write-lock epoch bump is not a durable write",
    )?;
    ensure(
        is_durable_write(&format!("content changed: .ee{sep}ee.db-shm")),
        false,
        "an shm remap is not a durable write",
    )
}

#[test]
fn effect_manifest_covers_recent_read_only_surfaces() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();
    // `similar` was in this list and is not any more: it opens a writable
    // audit handle (`src/core/search.rs:8169`, made writable deliberately by
    // 76991fe96) and appends a retrieval audit row. It is re-pinned positively
    // by `retrieval_surfaces_declare_the_audit_row_they_append` in
    // src/core/effect.rs -- dropping a name from a read-only list without
    // asserting what it became would leave the surface unpinned (bd-czj3e).
    for command in ["diag toolchain-provenance", "hook status", "timeline"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{command} is read-only"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::ReadOnly,
            &format!("{command} has read-only side-effect class"),
        )?;
        ensure(
            effect.write_surfaces.is_empty(),
            true,
            &format!("{command} has no write surfaces"),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_distinguishes_health_scorecard_snapshot_write() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, IdempotencyClass, SideEffectClass};

    let manifest = EffectManifest::build();

    let read_only = manifest
        .get("health scorecard")
        .ok_or_else(|| "health scorecard not in manifest".to_owned())?;
    ensure(
        read_only.default_effect,
        EffectClass::ReadOnly,
        "plain health scorecard remains read-only",
    )?;
    ensure(
        read_only.mutation_contract.side_effect_class,
        SideEffectClass::ReadOnly,
        "plain health scorecard mutation contract remains read-only",
    )?;

    let snapshot = manifest
        .get("health scorecard --record-snapshot")
        .ok_or_else(|| "health scorecard --record-snapshot not in manifest".to_owned())?;
    ensure(
        snapshot.default_effect,
        EffectClass::DurableMemoryWrite,
        "scorecard snapshot mode writes durable state",
    )?;
    ensure(
        snapshot.mutation_contract.side_effect_class,
        SideEffectClass::AuditedMutation,
        "scorecard snapshot mode uses audited mutation contract",
    )?;
    ensure(
        snapshot
            .write_surfaces
            .db_tables
            .contains(&"debt_snapshots"),
        true,
        "scorecard snapshot mode records debt_snapshots",
    )?;
    ensure(
        snapshot.idempotency,
        IdempotencyClass::Idempotent,
        "scorecard snapshot mode is idempotent by workspace/day/generation",
    )?;
    let preview = snapshot
        .mutation_contract
        .dry_run_behavior
        .ok_or_else(|| "scorecard snapshot mode missing no-write preview contract".to_owned())?;
    ensure(
        preview.contains("omit --record-snapshot"),
        true,
        "scorecard snapshot mode names its no-write preview path",
    )
}

#[test]
fn effect_manifest_includes_config_write_commands() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in ["init", "workspace alias"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::ConfigWrite,
            &format!("{command} writes configuration"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::AuditedMutation,
            &format!("{command} has audited config contract"),
        )?;
        ensure(
            effect.requires_audit,
            true,
            &format!("{command} requires audit"),
        )?;
    }

    for command in ["config set", "profile config apply"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        let contract = &effect.mutation_contract;
        ensure(
            effect.default_effect,
            EffectClass::ConfigWrite,
            &format!("{command} writes configuration"),
        )?;
        ensure(
            contract.side_effect_class,
            SideEffectClass::SidePathArtifact,
            &format!("{command} writes a config side-path file"),
        )?;
        ensure(
            effect.requires_audit,
            false,
            &format!("{command} does not write audit rows"),
        )?;
        ensure(
            effect.write_surfaces.db_tables.is_empty(),
            true,
            &format!("{command} does not write DB tables"),
        )?;
        ensure(
            effect
                .write_surfaces
                .workspace_files
                .contains(&".ee/config.toml"),
            true,
            &format!("{command} declares config file surface"),
        )?;
        let dry_run_behavior = contract
            .dry_run_behavior
            .ok_or_else(|| format!("{command} missing dry-run behavior"))?;
        ensure(
            dry_run_behavior.contains("--dry-run"),
            true,
            &format!("{command} dry-run contract names the config preview flag"),
        )?;
        ensure(
            dry_run_behavior.contains("key material"),
            false,
            &format!("{command} dry-run contract is not certificate-specific"),
        )?;
        ensure(
            dry_run_behavior.contains("--show"),
            false,
            &format!("{command} dry-run contract is not certificate keygen --show"),
        )?;
        ensure(
            contract.recovery_behavior.contains("config-file"),
            true,
            &format!("{command} recovery contract names config files"),
        )?;
        ensure(
            contract.recovery_behavior.contains("key material"),
            false,
            &format!("{command} recovery contract is not certificate-specific"),
        )?;
    }

    let keygen = manifest
        .get("certificate keygen")
        .ok_or_else(|| "certificate keygen not in manifest".to_string())?;
    let keygen_dry_run = keygen
        .mutation_contract
        .dry_run_behavior
        .ok_or_else(|| "certificate keygen missing dry-run behavior".to_string())?;
    ensure(
        keygen_dry_run.contains("--show"),
        true,
        "certificate keygen names its read-only --show mode",
    )?;
    ensure(
        keygen_dry_run.contains("key material"),
        true,
        "certificate keygen keeps key-material contract wording",
    )?;
    Ok(())
}

#[test]
fn effect_manifest_does_not_classify_daemon_paths_as_unavailable() -> TestResult {
    use ee::core::effect::{EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in [
        "daemon",
        "daemon background",
        "daemon foreground decay_sweep",
        "daemon foreground non-decay",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.mutation_contract.side_effect_class != SideEffectClass::DegradedUnavailable,
            true,
            &format!("{command} must not be modeled as unavailable"),
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            None,
            &format!("{command} has no unavailable sentinel"),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_tracks_daemon_jobs_as_real_supervised_jobs() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, RuntimeClass, SideEffectClass};

    let manifest = EffectManifest::build();

    let daemon = manifest
        .get("daemon")
        .ok_or_else(|| "daemon not in manifest".to_owned())?;
    ensure(
        daemon.default_effect,
        EffectClass::ExternalIo,
        "daemon family conservatively covers socket lifecycle plus foreground jobs",
    )?;
    ensure(
        daemon.mutation_contract.side_effect_class,
        SideEffectClass::Mixed,
        "daemon family is mixed, not unavailable",
    )?;
    ensure(
        daemon.dry_run_effect,
        Some(EffectClass::WorkspaceFileWrite),
        "daemon dry-run still writes daemon job ledger rows",
    )?;
    ensure(
        daemon.mutation_contract.degraded_code,
        None,
        "daemon family has no unavailable sentinel",
    )?;
    ensure(
        daemon
            .write_surfaces
            .workspace_files
            .contains(&".ee/daemon-jobs.jsonl"),
        true,
        "daemon family names persisted daemon job ledger",
    )?;

    for command in [
        "daemon background",
        "daemon foreground decay_sweep",
        "daemon foreground non-decay",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::DurableMemoryWrite,
            &format!("{command} may run real steward mutations"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::SupervisedJobs,
            &format!("{command} uses the supervised job contract"),
        )?;
        ensure(
            effect.runtime_contract.runtime_class,
            RuntimeClass::Supervised,
            &format!("{command} runs under supervised job runtime"),
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            None,
            &format!("{command} has no unavailable sentinel"),
        )?;
        ensure(
            effect.requires_audit,
            true,
            &format!("{command} requires audit"),
        )?;
    }

    Ok(())
}

#[test]
fn effect_manifest_tracks_implemented_surfaces() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in [
        "lab counterfactual",
        "lab replay",
        "support inspect",
        "preflight show",
        "tripwire list",
        "playbook list",
        "causal trace",
        "causal compare",
        "causal estimate",
        "economy report",
        "economy score",
        "economy simulate",
        "economy prune-plan",
        "hook claude-code",
        "hook codex",
        "hook gemini",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{command} is read-only"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::ReadOnly,
            &format!("{command} has read-only contract"),
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            None,
            &format!("{command} has no unavailable sentinel"),
        )?;
    }

    for command in [
        "causal trace",
        "causal compare",
        "causal estimate",
        "economy report",
        "economy score",
        "economy simulate",
        "economy prune-plan",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.requires_read_snapshot,
            true,
            &format!("{command} reads through a DB snapshot"),
        )?;
    }

    let causal_promote = manifest
        .get("causal promote-plan")
        .ok_or_else(|| "causal promote-plan not in manifest".to_string())?;
    ensure(
        causal_promote.default_effect,
        EffectClass::DurableMemoryWrite,
        "causal promote-plan can persist promotion candidates",
    )?;
    ensure(
        causal_promote.dry_run_effect,
        Some(EffectClass::ReadOnly),
        "causal promote-plan dry-run stays read-only",
    )?;
    ensure(
        causal_promote.mutation_contract.side_effect_class,
        SideEffectClass::AuditedMutation,
        "causal promote-plan has audited mutation contract",
    )?;
    ensure(
        causal_promote
            .write_surfaces
            .db_tables
            .contains(&"curation_candidates"),
        true,
        "causal promote-plan names curation_candidates",
    )?;
    ensure(
        causal_promote
            .write_surfaces
            .db_tables
            .contains(&"audit_log"),
        true,
        "causal promote-plan names audit_log",
    )?;
    ensure(
        causal_promote.mutation_contract.degraded_code,
        None,
        "causal promote-plan has no unavailable sentinel",
    )?;

    let support = manifest
        .get("support bundle")
        .ok_or_else(|| "support bundle not in manifest".to_string())?;
    ensure(
        support.default_effect,
        EffectClass::WorkspaceFileWrite,
        "support bundle creates side-path artifact",
    )?;
    ensure(
        support.mutation_contract.side_effect_class,
        SideEffectClass::SidePathArtifact,
        "support bundle side-effect class",
    )?;
    ensure(
        support.mutation_contract.degraded_code,
        None,
        "support bundle has no unavailable sentinel",
    )?;

    let lab_capture = manifest
        .get("lab capture")
        .ok_or_else(|| "lab capture not in manifest".to_string())?;
    ensure(
        lab_capture.default_effect,
        EffectClass::WorkspaceFileWrite,
        "lab capture writes frozen episode side-path artifacts",
    )?;
    ensure(
        lab_capture.mutation_contract.side_effect_class,
        SideEffectClass::SidePathArtifact,
        "lab capture side-effect class",
    )?;
    ensure(
        lab_capture.mutation_contract.degraded_code,
        None,
        "lab capture has no unavailable sentinel",
    )?;

    for command in ["preflight run", "preflight close"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::WorkspaceFileWrite,
            &format!("{command} writes workspace-local preflight state"),
        )?;
        ensure(
            effect
                .write_surfaces
                .workspace_files
                .contains(&".ee/preflight_runs.json"),
            true,
            &format!("{command} names the preflight run store"),
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            None,
            &format!("{command} has no unavailable sentinel"),
        )?;
    }

    for command in [
        "recorder start",
        "recorder event",
        "recorder finish",
        "tripwire check",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::DurableMemoryWrite,
            &format!("{command} writes durable DB state"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::AuditedMutation,
            &format!("{command} has audited mutation contract"),
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            None,
            &format!("{command} has no unavailable sentinel"),
        )?;
    }

    Ok(())
}

#[test]
fn effect_manifest_retires_causal_evidence_unavailable_sentinel() -> TestResult {
    ensure(
        EFFECT_SOURCE.contains("causal_evidence_unavailable"),
        false,
        "effect manifest must not advertise implemented causal commands as unavailable",
    )
}

#[test]
fn effect_manifest_tracks_demo_run_as_real_execution_surface() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();
    let effect = manifest
        .get("demo run")
        .ok_or_else(|| "demo run not in manifest".to_string())?;

    ensure(
        effect.default_effect,
        EffectClass::ExternalIo,
        "demo run executes manifest commands",
    )?;
    ensure(
        effect.dry_run_effect,
        Some(EffectClass::ReadOnly),
        "demo run --dry-run stays read-only",
    )?;
    ensure(
        effect.mutation_contract.side_effect_class,
        SideEffectClass::AuditedMutation,
        "demo run writes an audited ledger",
    )?;
    ensure(
        effect.write_surfaces.db_tables.contains(&"audit_log"),
        true,
        "demo run writes audit_log",
    )?;
    ensure(
        effect.write_surfaces.workspace_files.is_empty(),
        false,
        "demo run names evidence/artifact paths",
    )?;
    ensure(
        effect.mutation_contract.degraded_code,
        None,
        "demo run has no unavailable sentinel",
    )?;
    Ok(())
}

#[test]
fn effect_manifest_tracks_procedure_commands_as_real_surfaces() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in [
        "procedure drift",
        "procedure export",
        "procedure list",
        "procedure show",
        "procedure verify",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{command} reads stored procedure data"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::ReadOnly,
            &format!("{command} is no-mutation"),
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            None,
            &format!("{command} has no unavailable sentinel"),
        )?;
    }

    for command in ["procedure promote", "procedure propose"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::DurableMemoryWrite,
            &format!("{command} mutates the procedure store"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::AuditedMutation,
            &format!("{command} is audited"),
        )?;
        ensure(
            effect.write_surfaces.db_tables.contains(&"procedures"),
            true,
            &format!("{command} writes procedures"),
        )?;
        ensure(
            effect
                .write_surfaces
                .db_tables
                .contains(&"procedure_events"),
            true,
            &format!("{command} writes procedure_events"),
        )?;
        ensure(
            effect.write_surfaces.db_tables.contains(&"audit_log"),
            true,
            &format!("{command} writes audit_log"),
        )?;
    }

    Ok(())
}

#[test]
fn effect_manifest_tracks_handoff_and_eval_as_real_surfaces() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();

    // Read-only claim/handoff/eval surfaces must no longer carry unavailable
    // sentinels once real parsers/verifiers ship.
    for command in [
        "claim list",
        "claim show",
        "claim verify",
        "eval list",
        "eval report",
        "eval run",
        "handoff completion-audit",
        "handoff inspect",
        "handoff preview",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{command} stays read-only"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::ReadOnly,
            &format!("{command} has read-only contract"),
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            None,
            &format!("{command} no longer has an unavailable sentinel"),
        )?;
    }

    // handoff resume is NOT read-only: verifying a tampered capsule appends an
    // audit row, so it is declared append-only rather than having the security
    // record suppressed to preserve a read-only claim (bd-czj3e).
    let resume = manifest
        .get("handoff resume")
        .ok_or_else(|| "handoff resume not in manifest".to_string())?;
    ensure(
        resume.default_effect,
        EffectClass::DurableMemoryWrite,
        "handoff resume appends an audit row on HMAC failure",
    )?;
    ensure(
        resume.mutation_contract.side_effect_class,
        SideEffectClass::AppendOnly,
        "handoff resume is append-only, never an overwrite",
    )?;
    ensure(
        resume.write_surfaces.db_tables.contains(&"audit_log"),
        true,
        "handoff resume declares the audit_log table it writes",
    )?;

    // handoff create writes a capsule file to a user-specified --out path,
    // so it is a workspace-file-write surface (parallel to backup create).
    let create = manifest
        .get("handoff create")
        .ok_or_else(|| "handoff create not in manifest".to_string())?;
    ensure(
        create.default_effect,
        EffectClass::WorkspaceFileWrite,
        "handoff create writes to a user-specified output path",
    )?;
    ensure(
        create.dry_run_effect,
        Some(EffectClass::ReadOnly),
        "handoff create --dry-run is read-only",
    )?;
    ensure(
        create.mutation_contract.side_effect_class,
        SideEffectClass::SidePathArtifact,
        "handoff create class is side-path artifact",
    )?;
    ensure(
        create.mutation_contract.degraded_code,
        None,
        "handoff create no longer has handoff_unavailable sentinel",
    )?;
    ensure(
        create.write_surfaces.workspace_files.is_empty(),
        false,
        "handoff create names the --out workspace file surface",
    )?;
    Ok(())
}

#[test]
fn effect_manifest_tracks_certificate_and_quarantine_as_real_read_only_surfaces() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in [
        "certificate list",
        "certificate show",
        "certificate verify",
        "diag environment-attestation",
        "diag quarantine list",
        "diag quarantine show",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{command} stays read-only"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::ReadOnly,
            &format!("{command} has read-only contract"),
        )?;
        ensure(
            effect.mutation_contract.degraded_code,
            None,
            &format!("{command} no longer has an unavailable sentinel"),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_side_path_exports_have_no_delete_contracts() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();

    for command in [
        "backup create",
        "backup restore",
        "export",
        "playbook export",
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        let contract = &effect.mutation_contract;

        ensure(
            effect.default_effect,
            EffectClass::WorkspaceFileWrite,
            &format!("{command} writes only side-path workspace files"),
        )?;
        ensure(
            effect.dry_run_effect,
            Some(EffectClass::ReadOnly),
            &format!("{command} dry-run is read-only"),
        )?;
        ensure(
            effect.write_surfaces.workspace_files.is_empty(),
            false,
            &format!("{command} names side-path file surfaces"),
        )?;
        ensure(
            effect.requires_audit,
            true,
            &format!("{command} side-path manifest requires audit"),
        )?;
        ensure(
            contract.side_effect_class,
            SideEffectClass::SidePathArtifact,
            &format!("{command} side-path class"),
        )?;
        ensure(
            contract.audit_surface.is_some(),
            true,
            &format!("{command} names manifest or audit surface"),
        )?;
        ensure(
            contract
                .db_generation_effect
                .contains("source DB generation unchanged"),
            true,
            &format!("{command} leaves source DB generation unchanged"),
        )?;
        ensure(
            contract.index_generation_effect,
            "none",
            &format!("{command} does not rebuild indexes"),
        )?;
        ensure(
            contract
                .dry_run_behavior
                .is_some_and(|behavior| behavior.contains("no files are written")),
            true,
            &format!("{command} dry-run writes no files"),
        )?;
        ensure(
            contract.no_overwrite_behavior.is_some_and(|policy| {
                policy.contains("no-overwrite") && policy.contains("no-delete")
            }),
            true,
            &format!("{command} has no-overwrite/no-delete policy"),
        )?;
        ensure(
            contract.recovery_behavior.contains("never deleted by ee"),
            true,
            &format!("{command} recovery never deletes partial output"),
        )?;
    }

    Ok(())
}

#[test]
fn effect_manifest_playbook_import_is_audited_dry_run_write() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();
    let effect = manifest
        .get("playbook import")
        .ok_or_else(|| "playbook import not in manifest".to_string())?;

    ensure(
        effect.default_effect,
        EffectClass::DurableMemoryWrite,
        "playbook import may write procedural rules with --apply",
    )?;
    ensure(
        effect.dry_run_effect,
        Some(EffectClass::ReadOnly),
        "playbook import dry-run is read-only",
    )?;
    ensure(
        effect.mutation_contract.side_effect_class,
        SideEffectClass::AuditedMutation,
        "playbook import uses audited mutation contract",
    )?;
    ensure(
        effect
            .write_surfaces
            .db_tables
            .contains(&"procedural_rules"),
        true,
        "playbook import writes procedural rules",
    )?;
    ensure(
        effect.write_surfaces.db_tables.contains(&"audit_log"),
        true,
        "playbook import writes audit_log",
    )?;
    ensure(
        effect
            .write_surfaces
            .db_tables
            .contains(&"search_index_jobs"),
        true,
        "playbook import queues search indexing",
    )
}

#[test]
fn effect_manifest_rule_mark_and_update_are_audited_writes() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest, SideEffectClass};

    let manifest = EffectManifest::build();
    for command in ["rule mark", "rule update"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.default_effect,
            EffectClass::DurableMemoryWrite,
            &format!("{command} may mutate procedural rule state"),
        )?;
        ensure(
            effect.dry_run_effect,
            Some(EffectClass::ReadOnly),
            &format!("{command} dry-run is read-only"),
        )?;
        ensure(
            effect.mutation_contract.side_effect_class,
            SideEffectClass::AuditedMutation,
            &format!("{command} uses audited mutation contract"),
        )?;
        ensure(
            effect
                .write_surfaces
                .db_tables
                .contains(&"procedural_rules"),
            true,
            &format!("{command} writes procedural_rules"),
        )?;
        ensure(
            effect.write_surfaces.db_tables.contains(&"audit_log"),
            true,
            &format!("{command} writes audit_log"),
        )?;
        ensure(
            effect
                .write_surfaces
                .db_tables
                .contains(&"search_index_jobs"),
            true,
            &format!("{command} queues search indexing when changed"),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_export_paths_do_not_write_side_paths_until_materialized() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();

    for command in ["graph export", "schema export", "procedure export"] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;

        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{command} is read-only today"),
        )?;
        ensure(
            effect.write_surfaces.is_empty(),
            true,
            &format!("{command} has no file write surface"),
        )?;
        ensure(
            effect.requires_audit,
            false,
            &format!("{command} writes no audit while read-only"),
        )?;
    }

    Ok(())
}

#[test]
fn effect_manifest_safe_commands_count_matches_read_only() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();
    let safe = manifest.safe_mid_task_commands();

    for effect in safe {
        ensure(
            effect.default_effect,
            EffectClass::ReadOnly,
            &format!("{} is read_only", effect.command_path),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_mutating_commands_have_non_empty_write_surfaces() -> TestResult {
    use ee::core::effect::{EffectClass, EffectManifest};

    let manifest = EffectManifest::build();

    for effect in manifest.mutating_commands() {
        if effect.default_effect == EffectClass::ReadOnly {
            continue;
        }
        if effect.write_surfaces.is_empty() {
            return Err(format!(
                "Mutating command '{}' has empty write_surfaces",
                effect.command_path
            ));
        }
    }
    Ok(())
}

#[test]
fn effect_manifest_mutating_commands_have_complete_s43e_contracts() -> TestResult {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();

    for effect in manifest.mutating_commands() {
        let contract = &effect.mutation_contract;
        if contract.transaction_scope.is_none() {
            return Err(format!(
                "{} must declare transaction scope",
                effect.command_path
            ));
        }
        if contract.idempotency_key.is_none() {
            return Err(format!(
                "{} must declare idempotency behavior",
                effect.command_path
            ));
        }
        if contract.dry_run_behavior.is_none() {
            return Err(format!(
                "{} must declare dry-run no-op behavior",
                effect.command_path
            ));
        }
        if contract.recovery_behavior.is_empty() {
            return Err(format!(
                "{} must declare rollback/recovery behavior",
                effect.command_path
            ));
        }
        if contract.db_generation_effect.is_empty() || contract.index_generation_effect.is_empty() {
            return Err(format!(
                "{} must declare DB/index generation effects",
                effect.command_path
            ));
        }
    }
    Ok(())
}

#[test]
fn effect_manifest_runtime_classifier_covers_v6h4_command_classes() -> TestResult {
    use ee::core::effect::{EffectManifest, RuntimeClass};

    let manifest = EffectManifest::build();

    for (command, runtime_class, budget_required) in [
        ("status", RuntimeClass::Bounded, false),
        ("index rebuild", RuntimeClass::LongRunning, true),
        ("import cass", RuntimeClass::MultiStage, true),
        ("remember", RuntimeClass::MultiStage, true),
        ("backup create", RuntimeClass::MultiStage, true),
        // daemon reclassified Supervised -> MultiStage when the supervised job
        // registry was retired (e4dec185 fix(daemon): retire unavailable job
        // registry); the expectation lagged that peer change.
        ("daemon", RuntimeClass::MultiStage, true),
    ] {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        ensure(
            effect.runtime_contract.runtime_class,
            runtime_class,
            &format!("{command} runtime class"),
        )?;
        ensure(
            effect.runtime_contract.requires_budget(),
            budget_required,
            &format!("{command} budget requirement"),
        )?;
    }

    Ok(())
}

#[test]
fn effect_manifest_runtime_contracts_have_complete_v6h4_fields() -> TestResult {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();

    for command in manifest.command_paths() {
        let effect = manifest
            .get(command)
            .ok_or_else(|| format!("{command} not in manifest"))?;
        let runtime = &effect.runtime_contract;
        if runtime.runtime_class.as_str().is_empty() {
            return Err(format!("{command} has empty runtime class"));
        }
        if runtime.cancellation_points.is_empty() {
            return Err(format!("{command} has no cancellation checkpoints"));
        }
        if runtime.partial_progress_policy.is_empty() {
            return Err(format!("{command} has no partial-progress policy"));
        }
        if runtime.outcome_mapping.is_empty() {
            return Err(format!("{command} has no deterministic outcome mapping"));
        }
        if runtime.requires_budget() && runtime.default_budget_ms.is_none_or(|budget| budget == 0) {
            return Err(format!(
                "{command} requires a positive default runtime budget"
            ));
        }
    }

    Ok(())
}

#[test]
fn effect_manifest_runtime_budget_deadline_math_is_deterministic() -> TestResult {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();
    let rebuild = manifest
        .get("index rebuild")
        .ok_or_else(|| "index rebuild not in manifest".to_string())?;
    ensure(
        rebuild.runtime_contract.effective_budget_ms(None),
        Ok(Some(300_000)),
        "index rebuild default budget",
    )?;
    ensure(
        rebuild.runtime_contract.effective_budget_ms(Some(12_345)),
        Ok(Some(12_345)),
        "explicit budget overrides index default",
    )?;
    ensure(
        rebuild.runtime_contract.effective_budget_ms(Some(0)),
        Err("runtime budget must be greater than zero"),
        "zero budget rejected",
    )?;

    let support = manifest
        .get("support bundle")
        .ok_or_else(|| "support bundle not in manifest".to_string())?;
    ensure(
        support.runtime_contract.effective_budget_ms(None),
        Ok(Some(120_000)),
        "support bundle side-path artifact has default runtime budget",
    )
}

#[test]
fn effect_manifest_read_only_contracts_forbid_durable_mutation() -> TestResult {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();

    for effect in manifest.safe_mid_task_commands() {
        let contract = &effect.mutation_contract;
        ensure(
            contract.side_effect_class.declares_no_durable_mutation(),
            true,
            &format!("{} declares no durable mutation", effect.command_path),
        )?;
        ensure(
            contract.declares_no_source_mutation(),
            true,
            &format!("{} leaves source DB/index unchanged", effect.command_path),
        )?;
        ensure(
            contract.audit_surface,
            None,
            &format!("{} does not write audit", effect.command_path),
        )?;
    }
    Ok(())
}

#[test]
fn effect_manifest_side_path_artifacts_have_no_overwrite_policy() -> TestResult {
    use ee::core::effect::EffectManifest;

    let manifest = EffectManifest::build();

    for effect in manifest.mutating_commands() {
        let contract = &effect.mutation_contract;
        if contract.side_effect_class.requires_no_overwrite_contract() {
            let Some(policy) = contract.no_overwrite_behavior else {
                return Err(format!(
                    "{} must declare no-overwrite side-path behavior",
                    effect.command_path
                ));
            };
            if !policy.contains("no-overwrite") || !policy.contains("no-delete") {
                return Err(format!(
                    "{} must declare no-overwrite and no-delete side-path behavior",
                    effect.command_path
                ));
            }
            if !contract.recovery_behavior.contains("never deleted by ee") {
                return Err(format!(
                    "{} must not delete partial side-path output during recovery",
                    effect.command_path
                ));
            }
        }
    }
    Ok(())
}
