# RCH Runbook for Agents

> Agent-facing runbook for the RCH (Remote Compilation Helper) verification
> control plane in this repo. The plane decides whether your verification
> evidence is trustworthy and whether direct local Cargo is blocked before
> it can burn the Mac internal SSD.
>
> **The single rule:** Cargo compilation in this repo (`cargo build/check/test/bench/clippy`)
> must go through `rch exec --` on a remote Linux worker. There is no acceptable
> local-Cargo fallback. AGENTS.md, the bd-1h8ji.2 tripwire, and the
> bd-1h8ji.4 portability diagnostic all enforce this contract.

## TL;DR — the one canonical command shape

For any focused Rust verification, use this exact env recipe (proven by every
RCH-verified close in the bd-1h8ji.* and bd-3usjw.* families this week):

```bash
TMPDIR=/tmp \
RCH_REQUIRE_REMOTE=1 \
RCH_QUEUE_WHEN_BUSY=1 \
RCH_TEST_SLOTS=2 \
RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS=900 \
RCH_DAEMON_RESPONSE_TIMEOUT_SECS=900 \
RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel/projects \
RCH_ALIAS_PROJECT_ROOT=/data/projects \
RCH_VISIBILITY=summary \
RCH_COMPRESSION=0 \
/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch exec --json -- \
  env TMPDIR=/tmp CARGO_TARGET_DIR=/Volumes/USBNVME16TB/temp_agent_space/cargo-target \
  cargo <your-cargo-subcommand-here> -- --nocapture
```

Or, for everyday focused verification: prefer the wrapper that builds this
shape for you:

```bash
scripts/rch_verify.sh --bead-id bd-XXXX -- cargo test --lib my_focused_unit_test -- --nocapture
```

The wrapper emits an `ee.rch.verify.v1` JSON proof to stdout, which you paste
into the Beads comment for closure evidence.

## Why every flag matters

| Env var | Why |
|---|---|
| `TMPDIR=/tmp` (outer + inner) | Mac `~/.zshenv` points TMPDIR at `/Volumes/USBNVME16TB/...`. That path does not exist on Linux workers; Rust's `tempfile::tempdir()` inherits it and panics with `os error 2`. The inner `env TMPDIR=/tmp` ensures the remote cargo invocation overrides whatever RCH happens to pass through. |
| `RCH_REQUIRE_REMOTE=1` | Fail-closed when topology preflight fails. Without it, RCH falls back to **local** Cargo, which burns the Mac SSD and produces unsafe evidence. |
| `RCH_QUEUE_WHEN_BUSY=1` | Wait when all workers are busy rather than refusing. |
| `RCH_TEST_SLOTS=2` | Bound concurrent test slots so heavy benches don't starve focused tests. |
| `RCH_DAEMON_{WAIT_,}RESPONSE_TIMEOUT_SECS=900` | The full project metadata is large; the 30s default times out and triggers the manifest-fallback path that produces `RCH-E327`. 900s avoids the timeout. |
| `RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel/projects` + `RCH_ALIAS_PROJECT_ROOT=/data/projects` | DustyOtter's diagnosis (Agent Mail msg 1576, bd-2vyky): the manifest-fallback rejects `/data/projects/asupersync` because it doesn't see the symlink alias. These two env vars install the alias mapping the policy checker actually accepts. |
| `RCH_VISIBILITY=summary` | Less log noise; full transcripts when something goes wrong. |
| `RCH_COMPRESSION=0` | Compression on the sync pipe occasionally corrupts the manifest header during topology preflight. Disabling has zero throughput cost on the local-network workers. |
| Absolute RCH binary path | `~/.local/bin/rch` may be stale (missing the `exec` subcommand). The `target-local/release/rch` is always the live build. |
| Inner `CARGO_TARGET_DIR` | RCH rewrites this to a worker-scoped path automatically; the outer value is only used by RCH to know which artifacts to pull back. |

## Allowed Cargo subcommands and their wrapper variants

The wrapper script `scripts/rch_verify.sh` accepts five subcommands:

```bash
# Focused unit test (single test by name)
scripts/rch_verify.sh -- cargo test --lib has_active_owner_conflict_ -- --nocapture

# Integration test (one --test target, optional name filter)
scripts/rch_verify.sh -- cargo test --test closure_lint_harness -- --nocapture \
  closure_lint_requires_inline_unit_tests_for_part_ii_implementations

# Library-only compile check
scripts/rch_verify.sh -- cargo check --lib

# Clippy with -D warnings
scripts/rch_verify.sh -- cargo clippy --all-targets -- -D warnings

# Format check (proof.would_offload=false because RCH may decline non-compile)
scripts/rch_verify.sh -- cargo fmt --check
```

For criterion benches, use the compare-only mode rather than a full run:

```bash
EE_BENCH_COMPARE_ONLY=1 \
scripts/rch_verify.sh --bead-id bd-3usjw.46 -- \
  cargo bench --bench graph_minhash_rank -- --nocapture
```

## Shell-only / static verification (no cargo)

Many small slices need neither remote Cargo nor RCH — when you're touching
docs, shell scripts, JSON fixtures, or bead descriptions, these are the
right verifiers:

```bash
rustfmt --edition 2024 --check <files>...
sh -n scripts/<your-script>.sh
shellcheck -s sh scripts/<your-script>.sh        # SC3043 about POSIX `local`
                                                  # is pre-existing across this repo
jq empty tests/fixtures/<your>/*.json
git diff --check -- <files>...
scripts/closure-lint.sh --audit --json           # Tracks bead-closure invariants
scripts/check-tracing-fields.sh --json           # bd-3usjw.58 tracing-field convention
scripts/check-rch-portability.sh --json <transcript> # bd-1h8ji.4 portability anomalies
scripts/check-local-cargo-tripwire.sh --cmd '<cmd>' --json # bd-1h8ji.2 RCH-bypass detection
```

None of those need a Cargo round-trip. They run instantly on your shell.

## Beads + Agent Mail workflow

The RCH proof JSON contains enough fields for a Beads comment without needing
hand-curated prose. Typical closeout flow:

```bash
# 1. Generate the proof
scripts/rch_verify.sh --bead-id bd-XXXX --summary -- \
  cargo test --test my_harness -- --nocapture > /tmp/proof.json

# 2. Pretty-print the human summary and paste into Agent Mail
jq -r '.summary_markdown' /tmp/proof.json

# 3. Close the bead with the proof embedded
br close bd-XXXX --reason "$(jq -r '.close_reason_markdown' /tmp/proof.json)"

# 4. Optional: ledger the proof for swarm-wide reuse (bd-1h8ji.3)
scripts/rch_verify.sh --ledger .ee/derived/rch/runs.jsonl -- <cmd>
```

Before claiming a bead, **always**:

1. `br update bd-XXXX --status=in_progress --assignee=<your-agent-name>` so other agents see the claim.
2. Reserve the files you'll edit via `file_reservation_paths` MCP tool — narrow patterns, real TTL (1h–3h depending on slice size).
3. Send a `[bd-XXXX] Start: ...` message to the active swarm announcing scope and explicit out-of-scope deferrals.

When closing:

1. Land the code + tests committed.
2. Verify via the canonical RCH command shape above.
3. `br close bd-XXXX --reason "..."` with the RCH evidence inline.
4. Mail the verifying agent(s) a `[bd-XXXX] closed via ...` thank-you.

## Troubleshooting matrix

Failure-mode → diagnostic command → root cause → fix.

### `RCH-E327` ("Path dependency topology policy failed")

```text
WARN rch::hook: Dependency planner fail-open on <worker> [RCH-E327]:
refusing remote Cargo execution and falling back local (Path dependency
topology policy failed; move dependencies under /data/projects (or /dp) and retry.)
```

**Root cause** (per FrostyMoose's bd-2vyky investigation, msg 1551): the full
`cargo metadata` for `eidetic_engine_cli` runs longer than RCH's 30s metadata
timeout. RCH falls back to manifest parsing. The manifest contains raw
`/data/projects/...` absolute path deps that `validate_absolute_dependency_scope`
rejects before symlink canonicalization can prove they're under `/Users/jemanuel/...`.

**Fix:**

- Set the canonical+alias env vars from the TL;DR recipe.
- Raise the daemon timeouts to 900s (`RCH_DAEMON_*_TIMEOUT_SECS=900`).
- Disable compression (`RCH_COMPRESSION=0`).
- Topology is **flaky** — succeeded slots last 1–2 minutes then re-block. If
  you hit `RCH-E327`, retry every ~30s up to 3 times before reporting blocked.
- Pin a known-good worker: `RCH_WORKER=trj` (or css/csd).

### "All workers busy" (capacity wait)

Symptom: RCH says it's waiting for a slot for >5 minutes.

**Fix:**

- Set `RCH_QUEUE_WHEN_BUSY=1` (the TL;DR has this).
- Reduce slot demand: `RCH_TEST_SLOTS=1` for one slice at a time.
- Check via `rch workers probe --all` to see which workers are saturated.
- Don't fan out 4 parallel verifications when one bead at a time would work.

### Remote compile error in another agent's reserved file

```text
error[E0XXX]: ...
  --> src/some/file.rs:123:45
```

**Fix:**

1. Check Agent Mail reservations for that file before editing.
2. If reserved: send a `[compile-blocker]` mail to the holder with the exact
   compiler line, RCH command shape, and `--no-edit` proof. Do NOT take the
   file out from under them.
3. If unreserved: claim it via `file_reservation_paths` first, then fix.
4. Use `scripts/rch_compile_blocker_router.py` (bd-1h8ji.5) to produce a
   routing JSON; it knows about the reservation database and recommends the
   right action.

### Local Cargo bypass detected

```text
{"allowed":"denied","subcommand":"test","detail":"...RCH_REQUIRE_REMOTE=1 was
set but rch exec wrapper is absent — exact bd-1h8ji.2 failure mode"}
```

**Root cause:** caller set `RCH_REQUIRE_REMOTE=1` thinking that alone was
enough, but the command line lacks `rch exec`. RCH never gets a chance to
route the command — `cargo` runs immediately on the local Mac.

**Fix:** ALWAYS prefix with `rch exec --` (or use `scripts/rch_verify.sh`).
The tripwire is read-only; it doesn't kill the running cargo. You're
responsible for not starting it in the first place.

### Mac TMPDIR leakage on Linux worker

```text
Error: "tempdir: No such file or directory (os error 2) at path
\"/Volumes/USBNVME16TB/temp_agent_space/tmp/.tmpzvq7Bn\""
```

**Root cause:** Mac `~/.zshenv` sets `TMPDIR=/Volumes/USBNVME16TB/...` for
non-interactive shells. That path follows the test binary across to the
Linux worker via env propagation. The Linux worker doesn't have the
USB-NVMe mount, so any `tempfile::tempdir()` call panics.

**Fix:**

- Outer + inner `TMPDIR=/tmp` in the command shape (TL;DR).
- In Rust tests that use `tempfile::tempdir()` and run via RCH, add a
  worker-local helper (see `closure_lint_worker_local_tempdir` in
  `tests/closure_lint_harness.rs`).

### AppleDouble C-source failure on remote compile

```text
warning: vendor/zstd-sys/c/._zstd.c is an AppleDouble file the C compiler
tried to parse
```

**Root cause:** macOS extended-attribute sidecars (`._foo.c`) get rsynced
to the Linux worker; vendored C crates such as `zstd-sys` try to compile
them and choke.

**Fix:** the project `.rchignore` already excludes `._*` and `.DS_Store`
(landed in bd-1h8ji.4). If you see a new one slip through, file a child
bead — most likely you have a stale checkout that needs `git status` +
`git clean -nd` to identify.

### Daemon timeout

```text
ERROR rch::transfer: Wrapping command with external timeout protection
... command timed out after 1800s
```

**Fix:**

- For benches: use `EE_BENCH_COMPARE_ONLY=1` to skip the calibration phase.
- Raise `RCH_DAEMON_*_TIMEOUT_SECS` past the natural completion time of
  your slowest case.
- If the test itself is genuinely too slow, split it into smaller focused
  tests rather than fighting the timeout.

### dash vs bash sh-portability gotchas

If you write a shell script with `#!/bin/sh` and an `awk -F$'\t' ...`,
the bash-only ANSI-C escape `$'\t'` is treated as the literal 4-character
string `$'\t'` by `dash` (which is `/bin/sh` on Linux workers). Awk
receives `-F$'\t'` instead of a tab and splits on the wrong character —
producing `count=N` with an empty results array.

**Fix:** use `awk 'BEGIN{FS="\t"} ...'` instead. The `"\t"` inside the
awk program string is an awk-recognized escape that works under both
bash and dash. Confirmed by repro on this Mac (`dash` is installed at
`/bin/dash`) and by the bd-1h8ji.4 fix.

## Cross-references

- [`docs/rch_verification.md`](rch_verification.md) — wrapper internals and
  `ee.rch.verify.v1` JSON proof schema. Read this if you're modifying
  `scripts/rch_verify.sh` itself.
- [`AGENTS.md`](../AGENTS.md) — RCH section and the "Local Cargo on this Mac"
  hard rules. The runbook supersedes nothing in AGENTS.md; it just spells
  out the operational details.
- [`scripts/rch_verify.sh`](../scripts/rch_verify.sh) — the wrapper itself.
- [`scripts/check-local-cargo-tripwire.sh`](../scripts/check-local-cargo-tripwire.sh)
  — bd-1h8ji.2 detector for direct-cargo bypasses.
- [`scripts/check-rch-portability.sh`](../scripts/check-rch-portability.sh)
  — bd-1h8ji.4 anomaly detector for Mac-only artifacts in remote transcripts.
- [`scripts/rch_compile_blocker_router.py`](../scripts/rch_compile_blocker_router.py)
  — bd-1h8ji.5 router that pairs a remote compile error with the Agent Mail
  reservation holder.
- `bd-2vyky` (closed) — the RCH-E327 topology investigation that produced the
  canonical/alias env-var fix.

## Quick reference card

| Task | Command |
|---|---|
| Verify one unit test | `scripts/rch_verify.sh -- cargo test --lib <name> -- --nocapture` |
| Verify one integration test | `scripts/rch_verify.sh -- cargo test --test <crate> -- --nocapture <name>` |
| Library compile check | `scripts/rch_verify.sh -- cargo check --lib` |
| Clippy gate | `scripts/rch_verify.sh -- cargo clippy --all-targets -- -D warnings` |
| Format check | `scripts/rch_verify.sh -- cargo fmt --check` |
| Compare-only bench | `EE_BENCH_COMPARE_ONLY=1 scripts/rch_verify.sh -- cargo bench --bench <name>` |
| Detect local-cargo bypass | `scripts/check-local-cargo-tripwire.sh --cmd '<cmd>' --json` |
| Detect Mac-leak in transcript | `scripts/check-rch-portability.sh --json /path/to/transcript` |
| Generate closeout proof | `scripts/rch_verify.sh --bead-id <id> --summary --ledger .ee/derived/rch/runs.jsonl -- <cmd>` |

When in doubt: **don't run local cargo**. The wrapper exists precisely so you
never have to think about which env vars to set; just give it a `cargo`
subcommand and let it build the proven shape.
