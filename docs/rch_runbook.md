# RCH Runbook for Agents

> Agent-facing runbook for the RCH (Remote Compilation Helper) verification
> control plane in this repo. The plane decides whether your verification
> evidence is trustworthy and whether direct local Cargo is blocked before
> it can burn the Mac internal SSD.
>
> **The single rule:** Cargo/Rust verification in this repo (`cargo
> build/check/test/bench/clippy/doc/run/install/rustc/fix`, plus direct
> `rustc`/`rustdoc`) must go through the approved RCH wrapper on a remote Linux
> worker. There is no acceptable local-Cargo fallback. AGENTS.md, the local
> Cargo tripwire, and the bd-1h8ji.4 portability diagnostic all enforce this
> contract.

## TL;DR - the canonical agent command

For focused Rust verification, use the repo wrapper. It builds the remote-only
RCH invocation, re-execs from an in-memory copy so long proofs keep running
even if the checkout script changes, and emits an `ee.rch.verify.v1` JSON proof:

```bash
scripts/rch_verify.sh --bead-id bd-XXXX --summary -- \
  cargo test --lib my_focused_unit_test -- --nocapture
```

Do not bypass this with a bare or hand-written `rch exec` command from the
shared checkout. Raw RCH can fall back to local Cargo when topology admission is
wrong unless every fail-closed guard is present, and local Cargo output is
contaminated evidence in this repo.

If you are debugging RCH itself and must inspect the low-level shape, start with
`--dry-run`:

```bash
scripts/rch_verify.sh --dry-run --skip-build-admission --summary -- \
  cargo test --lib my_focused_unit_test -- --nocapture
```

Only then compare against an explicit remote-required low-level command. This
shape is incident evidence, not the normal agent workflow:

```bash
TMPDIR=/tmp \
RCH_REQUIRE_REMOTE=1 \
RCH_QUEUE_WHEN_BUSY=1 \
RCH_TEST_SLOTS=2 \
RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS=900 \
RCH_DAEMON_RESPONSE_TIMEOUT_SECS=900 \
RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel \
RCH_ALIAS_PROJECT_ROOT=/data \
RCH_VISIBILITY=summary \
RCH_COMPRESSION=0 \
RCH_BUILD_TIMEOUT_SEC=1200 \
RCH_ENV_ALLOWLIST=CARGO_TARGET_DIR,TMPDIR \
/Users/jemanuel/.local/bin/rch-manifestfix-20260605-5 exec -- \
  cargo <your-cargo-subcommand-here> -- --nocapture
```

If RCH prints that it is `running locally`, stop immediately, report the exact
path-normalization or topology line, and do not count any Cargo output as proof.
If a failed proof leaves source state, worker topology, tracker history, or
fixture ownership unclear, build a read-only regression causality capsule from
the existing proof artifacts instead of rerunning local Cargo or guessing from
raw logs:

```bash
ee regress explain \
  --from verification_evidence=/tmp/proof.json \
  --from rch_selector_admission=/tmp/selector-admission.json \
  --surface verification_gate \
  --json
```

The capsule schema is `ee.regression_causality.v1`; its hypotheses are
non-authoritative leads. Close only the owner later evidence confirms, and use
Agent Mail or a handoff note when Beads is not authoritative.

Before a real remote run, the wrapper also attempts a read-only
`ee diag build-admission --json` preflight when it can find a usable `ee`
binary. This is local diagnostics only, not local Cargo. It checks the
workspace, `CARGO_TARGET_DIR`, `TMPDIR`, and explicit artifact destinations so
agents do not launch RCH when the Mac checkout volume is already below the
admission threshold. If this preflight denies admission, the wrapper refuses
before RCH with `status=build_admission_refused`. If the `ee` binary is missing
or stale, the proof records `build_admission.status=unavailable`; automatic
discovery skips target-directory `ee` files that do not produce non-empty
`--version` output on this host. Provide `--build-admission-ee-bin <path>` for
stronger evidence, or
`--skip-build-admission` only when the weaker proof is intentional.

## Choose the source proof mode first

Before launching RCH, decide what source tree the proof should mean:

| Situation | Mode | Command |
|---|---|---|
| You are intentionally verifying the current shared checkout, including dirty files. | Live checkout | `scripts/rch_verify.sh --bead-id bd-XXXX -- cargo test --lib my_test -- --nocapture` |
| You need closeout evidence that no dirty source, Beads churn, or scratch artifacts influenced the run. | Strict clean checkout | `scripts/rch_verify.sh --bead-id bd-XXXX --summary --require-clean-tree -- cargo test --lib my_test -- --nocapture` |
| You need to verify committed source while other agents have dirty files. | Committed-tree export | `scripts/rch_verify.sh --bead-id bd-XXXX --summary --committed-tree --treeish HEAD -- cargo test --lib my_test -- --nocapture` |

Committed-tree mode is deliberately conservative: it resolves `--treeish`,
records the commit/tree and manifest hash, then materializes that committed tree
into a generated source export when it can be represented safely. If it reports
`rch_verify_committed_tree_unsupported`, the ref was unresolved or the committed
tree could not be exported safely. If it reports
`rch_verify_committed_tree_path_deps_unsupported`, the committed tree has path
dependencies that cannot be represented safely by the export alone.

Never use source-proof modes as permission to run `git worktree`, `git stash`,
`git reset`, `git checkout`, `git clean`, deletion cleanup, or local Cargo. The correct
response to an ambiguous proof is coordination, not mutation.

## Why every flag matters

| Env var | Why |
|---|---|
| `TMPDIR=/tmp` / `RCH_ENV_ALLOWLIST=CARGO_TARGET_DIR,TMPDIR` | Mac `~/.zshenv` points TMPDIR at `/Volumes/USBNVME16TB/...`. That path does not exist on Linux workers; Rust's `tempfile::tempdir()` inherits it and panics with `os error 2`. The wrapper lets RCH rewrite target/tmp values for the worker instead of hiding Cargo behind a leading `env` argv. |
| `RCH_REQUIRE_REMOTE=1` | Fail-closed when topology preflight fails. Bare `rch exec -- cargo ...` can fall back to **local** Cargo, which burns the Mac SSD and produces unsafe evidence. The repo tripwire denies bare `rch exec` Cargo commands unless this env var is present. |
| `RCH_QUEUE_WHEN_BUSY=1` | Wait when all workers are busy rather than refusing. |
| `RCH_TEST_SLOTS=2` | Bound concurrent test slots so heavy benches don't starve focused tests. |
| `RCH_DAEMON_{WAIT_,}RESPONSE_TIMEOUT_SECS=900` | The full project metadata is large; the 30s default times out and triggers the manifest-fallback path that produces `RCH-E327`. 900s avoids the timeout. |
| `RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel` + `RCH_ALIAS_PROJECT_ROOT=/data` | Current Mac wrapper default for the bd-3opmx E327 unblock. Requires `/Users/jemanuel/.local/bin/rch-manifestfix-20260605-5` or a newer RCH with the fixed worker preflight and manifest path rewrite; older release binaries apply the local `/data` alias on workers and fail before Cargo. |
| `RCH_BUILD_TIMEOUT_SEC=1200` | Large `cargo check` proofs can exceed the default 300s build timeout after remote Cargo starts. The bd-3tmeg proof passed with 1200s; the 300s run failed closed with `RCH-E104` and no local fallback. |
| `RCH_VISIBILITY=summary` | Less log noise; full transcripts when something goes wrong. |
| `RCH_COMPRESSION=0` | Compression on the sync pipe occasionally corrupts the manifest header during topology preflight. Disabling has zero throughput cost on the local-network workers. |
| Absolute RCH binary path | `~/.local/bin/rch` may be stale. The wrapper prefers `/Users/jemanuel/.local/bin/rch-manifestfix-20260605-5`, then newer sidecars or source-built clients known to handle this checkout's path topology. |
| Worker-scoped `CARGO_TARGET_DIR` | RCH rewrites this to a worker-scoped path automatically; agents should not bake `/Volumes/USBNVME16TB/...` into remote command argv. |
| Build-admission preflight | Stops before RCH when local workspace/target/tmp/artifact paths are below threshold. This is why an external `CARGO_TARGET_DIR` is necessary but not sufficient when `/System/Volumes/Data` is critically full. |

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
scripts/check-local-cargo-tripwire.sh --cmd '<cmd>' --json # RCH-bypass preflight
```

The central `scripts/verify.sh` runner also executes the deterministic
`--self-test` contracts for both guardrails before Cargo-backed gates. Those
stages validate classifier and portability logic only; they do not scan live
peer processes, kill processes, delete artifacts, launch RCH, or run Cargo.

None of those need a Cargo round-trip. They run instantly on your shell.

## Local-Cargo tripwire contract

Use the tripwire before shell execution when an agent or hook is about to run
an arbitrary command string:

```bash
scripts/check-local-cargo-tripwire.sh --cmd 'cargo test --lib foo' --json
scripts/check-local-cargo-tripwire.sh --probe-processes --json
```

The `--cmd` mode is the admission hook shape. Exit code `1` means block the
command and show the JSON `repairActions[]`; it does not rewrite the command.
The `--probe-processes` mode is read-only incident evidence for support bundles,
completion audits, and Beads comments. It reports local `cargo`/`rustc`/`rustdoc`
processes targeting this checkout and forbidden non-canonical git worktrees, but
never kills or cleans anything.

The live scan knows about the stable wrapper re-exec shape. A
`bash -s -- ... cargo ...` wrapper shell is treated as compliant data plumbing,
not as local Cargo; any spawned local `cargo`, `rustc`, or `rustdoc` child still
appears as its own process row. The planned-command classifier remains stricter:
a standalone `bash -s -- ... cargo ...` command string is still denied because
it has no visible remote-required RCH launcher.

Command-bearing Beads or Agent Mail updates need the same care as verifier
commands. Do not place verifier commands inside shell command substitution when
adding comments or messages: backtick and dollar-paren forms execute before the
tracker or mail tool receives the text. The tripwire denies command strings that
contain `cargo`, `rustc`, or `rustdoc` inside shell command substitution, even
when the inner command uses the RCH wrapper. Use plain quoted prose, direct MCP
tool calls, or an existing artifact path instead of shell expansion for evidence
transport.

The same rule is enforced by the command-facing preflight guard:

```bash
ee preflight check --cmd 'br comment bd-XXXX --message "$(cargo test --lib foo)"' --json
```

That command exits with policy-denied status and cites
`builtin:rust_verifier_command_substitution`. Direct wrapper invocations like
`scripts/rch_verify.sh --bead-id bd-XXXX -- cargo test --lib foo` remain allowed;
only shell substitution used as evidence transport is blocked.

If `--probe-processes` returns `status:"bypass_detected"`, do not launch a
remote proof even when `rch status` says the worker fleet is remote-ready. The
tripwire is reporting host evidence that can invalidate the proof lane before
RCH dispatch: active local Cargo/Rust, a forbidden extra worktree, or critical
workspace disk pressure. Record the exact `detectedLocalBuilds[]`,
`forbiddenWorktrees[]`, and `disk_pressure_context.workspace_free_bytes` in the
bead. Then stop at read-only coordination unless the human explicitly approves
the cleanup or process termination command.

The Mac incident pattern to preserve is: a forbidden worktree or local Cargo
bypass may leave a large scratch directory such as `$HOME/ee-clean.noindex` even
after the worktree itself is no longer registered. The tripwire and runbook may
name that path as evidence, but that is not permission to delete it. Under
AGENTS.md, commands such as `rm -rf "$HOME/ee-clean.noindex"` or
`git worktree remove --force <path>` require explicit written human approval for
that exact command.

Stable fields for automation:

- `localBuildPolicy`: the policy name and status for the planned command or
  live-process scan.
- `requiredRemoteWrapper`: the canonical repair shape,
  `scripts/rch_verify.sh -- <cargo command>`.
- `repairActions[]`: machine-readable remediation steps for denied commands or
  low disk headroom.
- `evidence[]`: compact policy and disk-pressure facts for support bundles.
- `detectedLocalBuilds[]`: bounded pid/ppid/cwd/elapsed/command-kind rows from
  the read-only scanner. When tmux is available, each row includes `tmuxPane`
  with pane id, pane pid, locator, current path, and title; null fields mean no
  pane ancestor was found or tmux was unavailable.
- `worktreePolicy`: single-canonical-worktree posture for the checkout.
- `forbiddenWorktreeCount` / `forbiddenWorktrees[]`: read-only rows from
  `git worktree list --porcelain`, including path, head, branch/detached state,
  git common dir when cheaply available, severity, and operator action.
- `disk_pressure_context`: workspace, target-dir, tmp-dir, and external-drive
  mount facts. This can recommend a repair action, but it must not recommend
  deletion.

## CI artifact proof lanes

When the proof requires a fresh native `ee` binary, start with the CI proof-lane
snapshot runbook instead of trying to build locally:
[`docs/ci-proof-lane-snapshot.md`](ci-proof-lane-snapshot.md). It tells agents
whether to reuse an active run, wait, verify an artifact, dispatch exactly one
new run after Agent Mail coordination, abstain, or file a follow-up Bead.

Artifact source authority and RCH source/test proof are separate. A verified CI
artifact can prove workflow/run/head-SHA provenance and a surface probe. It does
not prove Rust tests passed unless a separate RCH or CI source-test artifact says
so. If the snapshot reports `wait_for_active_run`,
`duplicate_dispatch_detected`, `artifact_stale`, `surface_probe_failed`, or
`abstain_manual_review`, preserve that verdict in Agent Mail and do not use a
local Cargo build as replacement proof.

## Beads + Agent Mail workflow

The RCH proof JSON contains enough fields for a Beads comment without needing
hand-curated prose. Typical closeout flow:

```bash
# 1. Generate the proof
scripts/rch_verify.sh --bead-id bd-XXXX --summary -- \
  cargo test --test my_harness -- --nocapture > /tmp/proof.json

# 2. Pretty-print the human summary and paste into Agent Mail
jq -r '.summary_markdown' /tmp/proof.json

# 3. Close the bead with a pasted/static reason, never command substitution
br close bd-XXXX --reason "RCH proof: command_hash=<hash>; status=<status>; verification_attribution=<mode>; see /tmp/proof.json"

# 4. Optional: ledger the proof for swarm-wide reuse (bd-1h8ji.3)
scripts/rch_verify.sh --ledger .ee/derived/rch/runs.jsonl -- <cmd>

# 5. Durable verifier ledger: store/query proof rows without running Cargo/RCH
ee verify rch ingest --workspace . --from-json /tmp/proof.json --json
ee verify rch blockers --workspace . --bead-id bd-XXXX --json
ee verify rch runs --workspace . --bead-id bd-XXXX --json
```

Before spending another RCH slot on the same bead, query active blockers first.
If the ledger reports a matching blocker, respect `retry_after`, cite
`command_hash`, `blocker_fingerprint`, `degraded_codes`, `remediation_bead`, and
`retry_after`, and do not run local Cargo as a substitute proof. The concrete
success, topology, and no-worker comment templates live in
[`docs/rch_verification.md`](rch_verification.md#durable-rch-verify-ledger-cli).
Status, doctor, and swarm work-packet surfaces also project active blockers:
inspect `data.verificationLedger` in `ee status --json` / `ee doctor --json`
or `rchProofPosture.knownBlockers` in `ee swarm work-packet --json` before
launching another remote proof.

Paste or summarize these proof fields in the Beads comment. Keep fields with
`none` values when the verifier did not reach that phase; the absence itself is
part of the source-attribution evidence.

```text
RCH proof:
- command_hash: <command_hash>
- status: <status>
- verification_attribution: <verification_attribution>
- git_head: <git_head>
- git_tree: <git_tree>
- dirty_status_hash: <dirty_status_hash>
- source_materialization: <source_materialization>
- remote_source_materialized: <true|false>
- source_manifest_hash: <source_manifest_hash or none>
- worker_id: <worker_id or none>
- exit_code: <exit_code>
- degraded_codes: <degraded_codes or none>
- source_state_degraded_codes: <source_state_degraded_codes or none>
- worker_state_degraded_codes: <worker_state_degraded_codes or none>
- known_blocker: <known_blocker.blocker_fingerprint or none>
- remediation_bead: <known_blocker.remediation_bead or none>
- retry_after: <known_blocker.retry_after or none>
- build_admission: <build_admission.status>/<build_admission.admitted>
- first_error: <first_error_file>:<first_error_line or none>
```

Use precise Agent Mail wording:

- `strict_clean_tree` + `remote_pass`: closeout-quality proof.
- `local_checkout_observed_remote_source_unknown` + `remote_pass`: useful
  remote signal, but the remote source was not materialized by the wrapper.
- `committed_tree` + `remote_pass`: remote run passed from the committed-tree
  export named by `resolved_commit`, `git_tree`, and `source_manifest_hash`.
- `build_admission_refused`: remote run did not start because local disk or
  artifact-path admission failed; fix the environment or ask the human before
  cleanup.
- `source_state_refused`: implementation may be done, but clean proof is
  blocked by dirty checkout state.
- `committed_tree_unsupported`: committed source identity is known, but remote
  Cargo did not run from that source set.

Use these exact handoff openings when the distinction matters:

- `Code implemented but clean proof blocked`: use when source-state or RCH
  worker evidence refused before remote Cargo, and include the blocker fields
  from the template above.
- `Committed tree verified`: use only when `verification_attribution` is
  `committed_tree`, `status` is `remote_pass`, and the message names
  `resolved_commit`, `git_tree`, and `source_manifest_hash`.

Dirty `.beads/issues.jsonl` is safe metadata churn only when you own the tracker
update and can commit it. It still blocks `--require-clean-tree`. If Beads is
dirty, run `br doctor --json` and `br sync --flush-only`; if the file is reserved
or contains another agent's work, do not claim strict proof. Coordinate and use
the live-checkout or committed-tree wording instead.

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

**Current root cause** (bd-3opmx, 2026-06-05): the dependency planner resolved
`/data/projects/asupersync` through symlinks to
`/Users/jemanuel/dp/asupersync`, then rejected it because the wrapper supplied
`RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel/projects`. That root contains the
`eidetic_engine_cli` checkout but not the Franken dependency tree under
`/Users/jemanuel/dp`.

**Current fix** (bd-3opmx / bd-3tmeg, 2026-06-05): use the current-source
sidecar `/Users/jemanuel/.local/bin/rch-manifestfix-20260605-5` and the
widened local topology:

```toml
# .rch/config.toml is gitignored local machine config.
[path_topology]
canonical_root = "/Users/jemanuel"
alias_root = "/data"

[transfer]
max_transfer_time_ms = 300000
exclude_patterns = [
  "target/",
  ".git/objects/",
  "node_modules/",
  "*.rlib",
  "*.rmeta",
  ".beads/",
  ".beads/**",
  ".beads_recovery/",
  ".beads_recovery/**",
  ".repo_janitor_workspace/",
  ".repo_janitor_workspace/**",
  ".ee/",
  ".ee/**",
  ".ruff_cache/",
  ".ruff_cache/**",
]
```

Proof signal from the unblock run:

```text
[RCH] topology preflight ok on vmi1264463 (/dp -> /data/projects enforced)
cargo test -p rch manifest_rewrite_rules --quiet -- --nocapture
[RCH] remote vmi1264463 (513.9s)

RCH_BUILD_TIMEOUT_SEC=1200 ... rch-manifestfix-20260605-5 exec -- \
  env TMPDIR=/tmp CARGO_TARGET_DIR=/tmp/ee-rch-verify-target cargo check --lib --quiet
[RCH] remote vmi1264463 (839.6s)
```

The former next blocker is also resolved: the sidecar rewrites manifest-declared
path dependencies such as `frankensearch` to their synced `.../projects/dp/...`
roots and syncs dependency roots discovered from manifests even when Cargo
metadata did not include them as used packages.

**Fix / disposition:**

- Use `scripts/rch_verify.sh` for fail-closed evidence and preserve the exact
  first blocker text.
- If `RCH-E327` recurs, first check that the wrapper selected
  `/Users/jemanuel/.local/bin/rch-manifestfix-20260605-5` or newer and that
  `.rch/config.toml` still contains the local topology above.
- For large `cargo check` proofs, set `RCH_BUILD_TIMEOUT_SEC=1200`; the default
  300s build timeout fails closed with `RCH-E104` once remote Cargo runs longer
  than five minutes.
- Raise the daemon timeouts to 900s (`RCH_DAEMON_*_TIMEOUT_SECS=900`).
- Disable compression (`RCH_COMPRESSION=0`).
- Pin a known-good worker: `RCH_WORKERS=trj` (or css/csd).

### Selector health after a timed-out proof

Symptom: a remote-required proof starts remote Cargo, the local wrapper times
out first, and the next dry-run reports `no_workers_passed_health` even though
`rch status --json` still shows a healthy worker.

Observed 2026-06-08 from this repo:

```text
scripts/rch_verify.sh -- cargo test --lib blind_spots -- --nocapture
=> timed out locally while remote rustc was still compiling eidetic-engine
```

The daemon cancelled the orphaned remote job cleanly, but the short-term
selection history still counted the cancellation as a worker failure. Refresh
capabilities, then pin the known worker for the retry so the proof does not wait
for health-score decay:

```bash
rch workers capabilities --refresh --json
RCH_WORKERS=vmi1149989 \
RCH_VERIFY_ATTEMPT_TIMEOUT_MS=1200000 \
scripts/rch_verify.sh --summary -- \
  cargo test --lib blind_spots -- --nocapture
```

The retry produced `status=remote_pass`, `worker_id=vmi1149989`, `exit_code=0`,
local Cargo tripwire `count=0`, and `[RCH] remote vmi1149989 (874.9s)`. Treat
the first timeout as a capacity/timeout proof, not a source failure; only the
successful retry is source evidence.

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
{
  "schema": "ee.rch_local_cargo_tripwire.v1",
  "allowed": "denied",
  "policyStatus": "local_cargo_disallowed",
  "subcommand": "test",
  "requiredRemoteWrapper": "scripts/rch_verify.sh -- <cargo command>",
  "repairActions": [
    {
      "kind": "use_remote_wrapper",
      "command": "scripts/rch_verify.sh -- <cargo command>"
    }
  ]
}
```

**Root cause:** caller set `RCH_REQUIRE_REMOTE=1` thinking that alone was
enough, but the command line lacks `rch exec`. RCH never gets a chance to
route the command — `cargo` runs immediately on the local Mac.

**Fix:** use `scripts/rch_verify.sh -- <cargo command>` for verification. A
carefully shaped `RCH_REQUIRE_REMOTE=1 rch exec -- ... cargo ...` is also
accepted, but bare `rch exec -- cargo ...` is denied because it can fall back to
local Cargo. The wrapper is the agent-facing default because it records proof
fields and fail-closed degraded evidence. The tripwire is read-only; it does not
kill a running Cargo process, rewrite the command, or clean disk.

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
bead after read-only inspection with `git status --short --ignored`.

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
  — local-Cargo preflight and read-only process scanner for direct-cargo
  bypasses.
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
| Scan active local Cargo processes | `scripts/check-local-cargo-tripwire.sh --probe-processes --json` |
| Detect Mac-leak in transcript | `scripts/check-rch-portability.sh --json /path/to/transcript` |
| Generate closeout proof | `scripts/rch_verify.sh --bead-id <id> --summary --ledger .ee/derived/rch/runs.jsonl -- <cmd>` |
| Ingest verifier proof | `ee verify rch ingest --from-json proof.json --json` |
| Query active RCH blockers | `ee verify rch blockers --bead-id <id> --json` |
| Query RCH verifier run history | `ee verify rch runs --bead-id <id> --json` |

When in doubt: **don't run local cargo**. The wrapper exists precisely so you
never have to think about which env vars to set; just give it a `cargo`
subcommand and let it build the proven shape.
