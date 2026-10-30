# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is ee?

`ee` is a Rust CLI that provides durable, local-first memory for coding agents. It captures facts, decisions, rules, and session evidence; indexes them with hybrid search; and emits compact context packs that agent harnesses (Codex, Claude Code, etc.) consume.

## Build Commands

**Cargo does not run locally in this repo.**
`scripts/check-local-cargo-tripwire.sh` forbids the subcommands `build`,
`check`, `test`, `bench`, `clippy`, `doc`, `run`, `install`, `rustc` and `fix`.
`fmt` is the one exception, so it is also the only local syntax check you have.

**AND IT IS UNAVAILABLE WHENEVER THE USB VOLUME IS NOT MOUNTED, which is most
of the time. Do not read its failure as a formatting verdict.** `~/.rustup/toolchains`
is a symlink to `/Volumes/USBNVME16TB/rustup_toolchains`, so with the volume
absent rustup tries to install the pinned nightly and dies:

```text
error: could not create component directory:
  '.../toolchains/nightly-2026-08-31-aarch64-apple-darwin/bin': File exists (os error 17)
```

**That message names the wrong cause.** `mkdir` through a DANGLING symlink
returns EEXIST, so "File exists" is reporting the broken link, not a leftover
directory, and hunting for a stale directory to clear finds nothing. Confirmed
exhaustively 2026-10-27: no `rustfmt` binary exists anywhere on this machine --
every `cargo`, `rustc` and `rustfmt` on PATH is a rustup shim symlinked to
`rustup`. So while the volume is absent there is NO local syntax gate at all,
and `cargo fmt --check` is UNRUN rather than clean or red. Mount the volume, or
treat Rust edits as unverified and say so.

**WHEN THE VOLUME COMES BACK, FOUR GATES STOP LYING AND ONE OF THEM IS PROBABLY
RED. RUN THEM FIRST.** The window is rare and nobody had a procedure for it, so
on 2026-10-30 the volume was mounted and `cargo fmt --check` — a gated verify.sh
stage — had been RED since 2026-10-27 with nobody able to see it. Check the
state and then run the family:

```bash
test -d "$(readlink ~/.rustup/toolchains)" && echo MOUNTED || echo dangling
cargo fmt --check                              # the gated stage; fix with `cargo fmt`
./scripts/check-format.sh --self-test          # 7 arms; INCONCLUSIVE (3) when absent
./scripts/check-include-fmt.sh                 # and --self-test, 6 arms
./scripts/check-forbidden-deps.sh              # needs `cargo metadata`; rc 3 when absent
./scripts/package-artifact-leak-check.sh       # needs `cargo package`; rc 1 when absent
./scripts/verification-drift-guard.sh          # needs `cargo tree`; input-error when absent
```

All six were run on 2026-10-30: `cargo fmt --check` red (9 hunks in 3 files,
fixed), then 7/7, 6/6 + rc 0, rc 0, rc 0 (1026 files), rc 0 ("all red gates have
tracking beads"). The last five FAIL CLOSED while the volume is absent, which is
the correct direction and is why they are easy to mistake for permanent
environment noise — they are not, they are gates waiting for a rail.

`metadata`, `package` and `tree` are NOT tripwired subcommands, so those gates
are legitimate locally in either state; they simply cannot ANSWER without the
toolchain, which is a different thing from being forbidden. Conflating the two
is how they got filed as permanently environment-blocked.
`verification-drift-guard.sh:205` is worth reading as the model for that
distinction — it checks `cargo tree`'s own exit status BEFORE grep runs,
because the old form "conflates two very different zeros: grep finding no
forbidden dependency (a real verdict) and cargo tree failing so grep read an
empty stream (no verdict at all)".

**VERIFY.SH IS STILL BLOCKED, AND NOW FOR A DIFFERENT REASON THAN BEFORE.** With
the toolchain present the remaining obstacle is `ee_require_current_binary`: the
installed `ee` is 0.14.2 against a 0.18.0 tree, and the printed remedy
(`cargo build --locked --bin ee`) is a tripwired subcommand. The fleet cannot
supply one either — it is Linux, this is arm64 Darwin, and rch refuses the
cross-platform artifact return with RCH-E327. So a mounted volume buys the five
checks above and NOT a verify.sh run; that needs either a macOS builder or an
explicit operator exception to the local tripwire
(bd-no-runnable-current-ee-binary-on-the-mac-wu6ay).

**But it is not the only local GATE, and treating it as such is how three
features shipped past a red assertion.** `scripts/check-*.py` are pure-Python
checks over tracked files: they need no toolchain, no USB volume and no lane,
and the whole family runs in seconds.

```bash
for s in scripts/check-*.py; do python3 "$s" || echo "FAILED $s"; done
```

Several of them mirror a Rust assertion that is otherwise unreachable here.
`check-boundary-inventory-coverage.py` is one: the assertion it precedes was red
for `config explain`, `curate candidates --propose`, `evidence withdraw`,
`index backfill-anchors`, `memory anchors` and `workspace rebind` at once,
because `verify.sh` exits 1 before any stage on this host, hosted Actions is off
repo-wide, and nobody pays for a pinned dispatch to check a markdown table
(bd-no-compile-gate-before-a-commit-lands-qdxc0). Run the family before a commit
that touches docs or the CLI surface; it is the cheapest gate in the repository.

**AND ONE OF THEM NOW CATCHES A COMPILE ERROR, which is worth knowing because
the rest of this file says no local gate can.**
`check-deleted-symbol-orphans.py` inspects `HEAD~1..HEAD` by default — so the
loop above grades the last commit — and reports a `fn` the commit deleted whose
callers survive, i.e. `error[E0425]`. It is a narrow class and not a substitute
for a build: no type, borrow or trait error is visible to it. But it is the
class `#[path]` + `use super::*` hides from review, and it is what left main
uncompilable for 24 hours on 2026-10-29 (bd-1ks7e). Validated in both
directions against real history — it fires on `f9ecec213^..f9ecec213` naming
`src/core/context_why_not.rs:772`, and one fire in 120 commits of src/ history.
0.066 s when the range deletes nothing; it skips the 1563-file tree read and
says so rather than reporting a count it never looked at. `--self-test` is 16
arms, three of them controls on its comment stripper's failure DIRECTION, since
over-stripping would be a silent false pass. Exit 3 is cannot-answer, not red.

**THE FAMILY IS NOT THE WHOLE LOCALLY-RUNNABLE SET, and the gap is where the
red was hiding.** Of verify.sh's 135 `run_stage` lines (134 distinct names; one
stage is invoked twice and shares a single budget row), **31** match the grep
below and run here with no toolchain, no binary and no lane: 16 `python3` stages
and 15 shell `--self-test` stages. Re-measured 2026-10-28; it read 30 / 15 / 15
before the Golden Consumption Floor stage landed, and these counts move whenever
a stage is added, so re-run the grep rather than quoting the number.
**Two of those Python stages are not named `check-*.py`**, so the glob above
cannot see them — and one of the two was the red. Enumerate from verify.sh
itself rather than from the glob:

```bash
grep -oE 'run_stage "[^"]*" "(python3 [^"]*|\./scripts/[^"]*--self-test[^"]*)"' scripts/verify.sh
```

Do not widen that to a bare `\./scripts` alternative: it matches 85 of the 134
stages, most of which need the `ee` binary or a dispatch, so the list stops
being the locally-runnable set and you cannot tell a real red from an absent
rail. (`fuzz_target_audit_self_test` is a shell FUNCTION stage, not a script
path, so it is outside this enumeration and was not run.)

**AND THAT GREP IS STILL NARROWER THAN THE HOST, WHICH IS WHERE A RED HID FOR
EIGHT HOURS ON 2026-10-28.** The warning above is right — do not widen the
pattern — but the set it produces is a claim about a GREP, not about this
machine. `./scripts/e2e_invocation_audit.sh` carries no flag, so it matches
neither alternative, and it completes here in under a second. It was RED (a
stale-baseline failure caused by prose in a Python docstring naming an e2e
script) and no sweep saw it, because every sweep used the grep.

Classified all 135 `run_stage` lines: 49 shell-function dispatch, 5 naming
cargo/binary/rch on the command line, 81 local candidates, of which the grep
sees 26 and MISSES 55. Seventeen of the 55 carry an `ee_require_current_binary`
guard directly or through a sourced library, so they genuinely cannot run.
**NINE MORE DO RUN, measured green twice on 2026-10-28, and they must be named
because no pattern distinguishes them:**

```bash
./scripts/e2e_repo_hygiene.sh
./scripts/contract-drift-radar.sh --quiet
./scripts/e2e_event_contract_radar_golden.sh
./scripts/panic_helper_radar_golden.sh
./scripts/ci_proof_lane_snapshot_fixture_test.sh
./scripts/ci_proof_lane_hygiene.sh --json
./scripts/e2e_release_provenance.sh --static
./scripts/beads_export_repair.sh --fixture-suite tests/fixtures/beads_export
./scripts/e2e_overhaul/lib/test_fake_tailscale.sh
```

So the locally-runnable set is **40**, not 31. Run these nine alongside the grep's
output; all nine were green and none had ever been run on this host.

**CLASSIFY BY THE SCRIPT'S CONTENTS, NOT ITS COMMAND LINE, and this is a tripwire
matter rather than a tidiness one.** `./scripts/mcp_lib_tests.sh` names no cargo
in `verify.sh`, so a command-line filter calls it local — but `:100` runs
`cargo test --workspace --features mcp --lib`, which
`check-local-cargo-tripwire.sh` forbids. A sweep built on the command line
INVOKES A FORBIDDEN SUBCOMMAND without ever printing the word `cargo`; mine did,
and it hung past 150s rather than failing, because a cargo BUILD command goes
through the RCH shim and attempts a dispatch. (A trivial `cargo --version` fails
locally in 1.4s with the dangling-symlink EEXIST, so "cargo fails fast here" is
true of trivial commands and false of build commands.) Grep the script for
`cargo (build|check|test|bench|clippy|doc|run|install|rustc|fix)` before adding
it to any local sweep.

Two more of the 55 are environment-dependent rather than logic reds, and the
distinction is in their own output: `./scripts/lexical_relevance_pins.sh` and
its `--guard` arm report "no lexical_relevance_contract binary under
`<target>/debug/deps`. Gate 5 builds it; this stage must run after it." One of
the two is labelled `(tracked red)` in its own stage name.

Measured 2026-10-27 by running all 30: **29 green, one hard red.**
`scripts/lib/emit_proof_capsule.py --self-test` had been exiting 1 for two days
with `ValueError: too many values to unpack (expected 2, got 3)` — a sibling
caller of `grade_test_log.parse` was updated when it gained a third return
value and this one was not, so every call to `build_capsule` raised and no
`rch_run.sh` run emitted a capsule. Fixed in 3e40c02c9. Nothing saw it because
its stage only ever runs inside verify.sh, which exits before stage one here.

All 30 `--self-test` invocations are green at that commit. The LINT halves of
the shell gates are a separate matter: three exit non-zero for ENVIRONMENT
reasons and are not defects. `check-forbidden-deps.sh` (rc 3) and
`package-artifact-leak-check.sh` (rc 1) both shell out to `cargo
metadata`/`cargo package`, so they fail CLOSED while the toolchain is absent,
which is the correct direction and is a second place the dangling-symlink error
above surfaces. `check-local-cargo-tripwire.sh` exits 2 with a usage block
unless you pass `--cmd`, the way verify.sh calls it. Read those three before
reporting a red.

```bash
cargo fmt --check                   # The ONLY cargo subcommand allowed locally
./scripts/verify.sh                 # Full verification; dispatches its own cargo stages
```

Everything else is dispatched to the pinned RCH lane:

```bash
# Type check
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo check --locked --all-targets
# Lint (warnings = errors)
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo clippy --locked --all-targets -- -D warnings
# Run all tests (the NON-BENCH gate, which is what verify.sh runs; benches go
# through ./scripts/verify.sh --include-bench)
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo test --locked --workspace --lib --bins --tests --examples
# A single test
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo test --locked test_name -- --exact
```

**THE LINT GATE IS GREEN, SO TREAT A CLIPPY RED AS YOURS.** Measured 2026-10-30
at `2f5e6fa21` on the lane: `cargo clippy --locked --all-targets -j 2 --
-D warnings` reached `Finished \`dev\` profile ... in 17m 49s` with exit 0 and
**zero** errors, warnings and `clippy::` lints. This is worth dating because the
standing assumption here has been the opposite — project memory described
`--all-targets` clippy as carrying "latent reds", which was true when written
and is what makes an agent discount a clippy failure as pre-existing. It is not
pre-existing any more. A clippy red after this date is a regression introduced
since it, and the gate is a trustworthy signal rather than noise to be stepped
around.

Two things that green does NOT mean. It is a lint gate, not a test run — zero
`test result:` lines, so it says nothing about behaviour. And `-D warnings`
promotes the crate-wide `expect_used`/`unwrap_used` settings, so every
`tests/*.rs` still needs its
`#![allow(clippy::unwrap_used, clippy::expect_used)]` header after the `//!`
block; the green means all 196 existing files carry it, not that a new file can
omit it.

**`--pinned-franken-stack` refuses any cargo command without `--locked`.** It
exits 1 before materializing anything, with `"status":
"source_state_refused"`, `blocking_codes:
["rch_verify_franken_stack_locked_required"]` and no `test result:` line —
`elapsed_ms` is 0, so the round trip is cheap but it buys you nothing. The
examples above carried no `--locked` until 2026-10-13 and every one of them
was refused as written; `scripts/check-rch-doc-examples.py` could not catch it
because it only checks that the wrapper precedes `cargo`. It now also requires
`--locked` on pinned examples.

**A pinned refusal almost always means one stale row in `franken-stack.lock`,
not a problem with your checkouts.** Each locked revision's own manifest must
declare the version `Cargo.toml` pins. The report carries BOTH readings per
repository — a live-checkout reading and a `materialized` block — and only the
materialized one has `matches`; the live numbers are informational and do NOT
block. On 2026-10-13 one stale row (`franken_agent_detection` locked at a
commit declaring 0.3.3 against a `=0.3.8` pin) refused every pinned dispatch,
and the live readings correlated so neatly with the symptom that they looked
causal. They were not: the live checkouts are still drifted today and the lane
is green. The repair text now names the offending `repo/package locked=
expected=` rows, so read that rather than inferring.

**But "almost always" is not always, and the LOCK IS THE WRONG SUSPECT when
`status` is `build_admission_refused`.** Measured 2026-10-25: a pinned dispatch
refused with `observed_codes` carrying `rch_verify_franken_stack_revision_mismatch`
and `_version_mismatch`, and those codes repeated in the `codes` array of **7 of
7** sibling repositories — which reads exactly like the stale-row failure above.
It was not. In the same document `"blocking_codes": []` and every repository's
`materialized.packages[].matches` was `true`. The cause was one line:

```text
stdout_tail: "build-admission preflight denied RCH execution"
```

**Read `status`, `blocking_codes` and `stdout_tail`/`stderr_tail` BEFORE the
mismatch codes.** The live per-repo version readings are drift in your working
checkouts and are present on a green lane too.

**The pinned lane runs a LOCAL `ee diag build-admission` preflight first, and it
refuses two different ways on this Mac.** Both are environment, neither is a
test result, and both name themselves precisely:

| `build_admission.status` | message / code | fix |
|---|---|---|
| `unavailable` | `no executable ee binary found for build-admission preflight`, with `degraded_codes: ["rch_verify_pinned_build_admission_verdict_required"]` | pass `--build-admission-ee-bin <path>` |
| `denied` | a `checks[]` entry with `admitted: false`, plus `degraded_codes: ["build_admission_denied", "tmpdir_not_external"]` | set `TMPDIR` for the invocation |

The first is near-circular: the lane that verifies the code wants a built binary
to start. Use the installed `ee` on PATH. The preflight checks the ENVIRONMENT,
not the code under test, so a stale binary is fine for it.

**AND IT IS STALE — THERE IS NO RUNNABLE CURRENT `ee` ON THIS MAC.** Measured
2026-10-26, and this is fine for admission and ruinous for anything else:

| | |
|---|---|
| `~/.local/bin/ee --version` | **ee 0.14.2** |
| `Cargo.toml` | **0.18.0** — four minor versions ahead |
| `$CARGO_TARGET_DIR/debug/ee`, `release/ee` | **`ELF 64-bit LSB pie executable, x86-64`** — Linux binaries from remote builds; `exec format error` on this arm64 host |

So the advice above to "look under `CARGO_TARGET_DIR`" was wrong and is removed:
those files exist, are 2.1 GB and 159 MB, and cannot run here.

**NEVER USE THAT BINARY AS EVIDENCE ABOUT PRODUCT BEHAVIOUR.** It cost two
wrong changes in one session, both shipped before being caught:

- a probe showed `ee situation compare` without `--dry-run` returning exit 7
  `policy_denied`, so a deleted test asserting that was "restored". At HEAD
  `handle_situation_compare` returns `ProcessExitCode::Success` unconditionally
  and never reads `args.dry_run`, whose doc comment says why: "comparisons never
  persist changes". The test had been deleted *with its premise*;
- a probe showed seven daemon capability paths present with `available: true`,
  so an e2e loop was added asserting exactly that. `src/cli/mod.rs:87082` lists
  six of them as `forbidden_path` and fails with "non-canonical capability path
  leaked" if they appear. The new assertion contradicted a test already in the
  tree.

Both behaviours changed inside the 0.14.2 → 0.18.0 gap. **Read the source at
HEAD, or dispatch to the lane.** A binary probe is only evidence about the
binary, and the one on PATH is not this tree.

The second is **not a free-space failure, and reading it as one wastes the
fix.** The denying check reported 30 GB available against a 1 GB minimum; what
failed was `external_required: true` against `external: false`, because `TMPDIR`
pointed at the macOS default `/var/folders/...`.

(That 1 GB is the figure measured then. Re-measured 2026-10-25, `ee diag
build-admission --json` reports `minFreeBytes: 5368709120` — **5 GiB** — on each
of its three roots, so do not quote 1 GB as current. And note WHICH roots: it
checks `workspace_root`, `cargo_build_target` and `temporary_directory`. It does
NOT check the committed-tree export base, and for a cargo dispatch it checks the
*configured* `CARGO_TARGET_DIR` rather than the `<project>/.rch-target-…` pool
dir rch rewrites to — which is the volume that actually filled in bd-to1fr. A
green admission is therefore not a statement about the disk the build will
write to.) AGENTS.md explains why it is
not exported globally — the USB drive is ExFAT, which has no Unix domain
sockets, so an exported ExFAT `TMPDIR` silently breaks gitstatusd in interactive
shells. So set it per invocation, as the `cargo()` wrapper in `~/.zshrc` does:

```bash
TMPDIR=/Volumes/USBNVME16TB/temp_agent_space/tmp scripts/rch_verify.sh \
    --pinned-franken-stack --treeish HEAD \
    --build-admission-ee-bin /Users/jemanuel/.local/bin/ee -- \
    cargo test --locked -j 2 --test <target> <filter>
```

`--skip-build-admission` also exists and costs a documented proof degradation,
so prefer satisfying the check over skipping it. Also note `--treeish HEAD`
resolves at RUN time, not launch time, so a commit made while the lane is
building changes the tree it verifies — pass an explicit sha when that matters.

Ignore `selector_admission_probe` while admission is refused: it reported
`selected_worker: null`, `no_worker_selected` and
`workers_vs_selection_contradiction: true` across 12 reported workers in a run
that never reached selection, which reads as a capacity problem and is not one.
With both fixes applied the same run reported `build_admission: status=passed,
admitted=true, degraded_codes: []` and `selector: status=selected`, which is
what proves the contradiction was downstream of admission rather than a fleet
condition.

**Past that point the lane is a separate problem, and the report labels it.** The
admitted run then failed in transport — a `status Some(73)` namespace refusal on
the first worker, then `[RCH-E104] SSH command timed out after 20s` on rch's own
retry — with `degraded_codes: ["rch_verify_remote_command_failed",
"rch_verify_remote_transport_timeout"]` and `elapsed_ms: 31315`. That is
bd-o7q3t / bd-s5v42 territory, not your change: `error_codes: ["RCH-E104"]` with
a 31-second elapsed and no `test result:` line is an UNRUN acceptance. Say so
rather than counting the admission pass as a verdict.

**`RCH_VERIFY_FRANKEN_STACK_PREFLIGHT=0` disables the whole franken-stack
preflight** (it defaults to `1`). It is a blunt escape hatch, not a fix: it
skips provenance verification entirely rather than scoping it, so a dispatch
run under it carries no attestation that the dependency sources match the lock.
Use it only to establish that the preflight is what is blocking you, then
repair the lock row and drop the flag. Never use it to obtain a verdict you
intend to cite as proof.

Write the wrapper out in full on every line. A `RCH="..."` shorthand reads
nicely and breaks both copy-paste of a single line and
`scripts/check-rch-doc-examples.py`, which looks for the literal wrapper
before `cargo` and cannot resolve a shell variable.

Budget the wait pessimistically: **the same build has been measured at 32
minutes and at 217 minutes, and you cannot choose which you get.** All
2026-10-11, same project:

| lane | target dir | crates | build |
|---|---|---|---|
| `cargo` dispatch | `<project>/.rch-target-<worker>-pool-<hash>` | 278 | 127m33s |
| `rch exec --job` | `/data/projects/<project>/.rch-target` | 475 | 217m07s |
| `rch exec --job` | same path, different worker | 475 | **31m57s** |

Two independent effects, and it is worth not confusing them:

**Which directory** — a cargo dispatch logs `Rewriting CARGO_TARGET_DIR for
remote execution` and redirects into a worker-scoped pool dir; a `--job` logs
no such line and inherits the worker's own default dir. So a `--job` never
reuses a cargo dispatch's warmth, only a previous `--job`'s. That is a real
difference in WORK — 278 crates against 475.

**That rewrite DISCARDS the configured `target-dir` and can exhaust the
internal disk.** `.cargo/config.toml` sends builds to
`/Volumes/USBNVME16TB/temp_agent_space/cargo-target`, but the pool dir it
rewrites to is `<project>/.rch-target-…`, i.e. on whichever volume holds the
project. Measured 2026-10-13: a `cargo test --locked --lib` dispatch compiled
496 crates over 30.7 minutes and then died with

    error: failed to write file <project>/.rch-target-<worker>-pool-<hash>/debug/
      incremental/…/dep-graph.part.bin: No space left on device (os error 28)

with 2.3 TiB free on the configured volume and 10 GiB free on `/`. It was not
too big for the machine; it was pointed at the wrong volume. Confirmed by the
log line itself: present once in the cargo dispatch, absent in the `--job`
rerun, which formed no local pool dir. **For a long build prefer
`RCH_REQUIRE_REMOTE=1 rch exec --job -- cargo <subcommand> --locked …`, which
never writes artifacts to this machine.** Keep `RCH_REQUIRE_REMOTE=1`: a bare
`rch exec` may fall back to a LOCAL cargo build, which lands straight back on
the volume that just filled up.
The failure surfaces as exit 101 with NO `test result:` line — 101 is cargo's
tests-failed code, so reading the exit code alone reports a compile-space
failure as a failing test. See bd-to1fr.

**"Never writes artifacts to this machine" is not "survives this machine
running out of disk," and the difference cost 4h17m on 2026-10-23.** A `--job`
writes no build artifacts locally, but it still writes its own lease state
under `~/.local/state/rch/`, and `--result-dir` must land in the repo — both on
the volume that fills. A 14-target `--job` ran to completion on the worker and
then died on the client:

```text
WARN Failed to persist durable lease finalize heartbeat:
     No space left on device (os error 28)
WARN Remote completion unconfirmed; retaining build <id> ownership
WARN refusing local fallback: automatic replay is unsafe
exit 1
```

Three things to read off that, because every one of them is easy to get wrong:

- **Exit 1 is the infra-refusal class, not a verdict.** Same signature as the
  "completion unconfirmed" failures above. Nothing about the tests is known.
- **The phase matters.** `finalize` is artifact retrieval, not execution — the
  body had already written its completion marker. So a client-side ENOSPC at
  finalize destroys the BOOKKEEPING, not the work.
- **The outputs may still arrive.** That result dir landed 99 minutes later
  once space was freed, complete and gradeable. So before paying for a
  re-dispatch, look in the `--result-dir`: check `rch jobs` for a lease that
  has reached `state=finished`, which also means no retained ownership is left
  to poison the next dispatch to that worker.

Free space on the client BEFORE a long dispatch, not after it fails.

**Which worker** — and this dominates the clock. Rows 2 and 3 are the same
lane, the same directory path and the same 475 crates, 6.8x apart, on workers
whose rated speeds differ by 9%. Do not attribute a slow build to a cold
directory without comparing crate COUNTS; if two runs compiled the same number
of crates, the directory was not the variable.

Treat the slow case as the budget: `RCH_BUILD_TIMEOUT_SEC`/`RCH_TEST_TIMEOUT_SEC`
at 18000, and **one cargo stage per `--job`** — a second `cargo test` stage needs
the TEST profile, another dependency tier on top of whatever the first stage cost.

**ON THE PINNED LANE THOSE TWO VARIABLES ARE NOT THE BINDING BUDGET, AND SETTING
ONLY THEM GUARANTEES A KILL.** `scripts/rch_verify.sh` applies its OWN per-attempt
wall clock, `RCH_VERIFY_ATTEMPT_TIMEOUT_MS`, **defaulting to 1800000 ms = 30
minutes** (`scripts/rch_verify.sh:123`), and it caps the whole `rch exec`
regardless of what the build and test budgets say. Thirty minutes is *below the
measured floor of a cold pinned build* — the fastest cold build in the table
above is 31m57s, and the slowest is 217 minutes — so a cold pinned dispatch run
at the default cannot reach a verdict no matter how large you set the other two.

Measured 2026-10-25, a dispatch carrying `RCH_BUILD_TIMEOUT_SEC=18000` and
`RCH_TEST_TIMEOUT_SEC=18000` exactly as advised above:

| field | value |
|---|---|
| `status` | `capacity_or_timeout` |
| `exit_code` | **124** |
| `timed_out` | `true` |
| `attempt_timeout_ms` | 1800000 |
| `elapsed_ms` | 1800144 — **144 ms over the ceiling** |
| `stdout_bytes` | **0**, no `test result:` line, still compiling deps |
| `build_admission` | `passed` |

Read that overshoot before reaching for any other explanation: 144 ms past a
configured ceiling is the ceiling, the same arithmetic that distinguishes a
timeout from exhaustion in the exit-137 case below. Exit **124** is this lane's
timeout code and is an UNRUN acceptance, not a red. So raise all three:

```bash
RCH_VERIFY_ATTEMPT_TIMEOUT_MS=18000000 RCH_BUILD_TIMEOUT_SEC=18000 \
RCH_TEST_TIMEOUT_SEC=18000 TMPDIR=/Volumes/USBNVME16TB/temp_agent_space/tmp \
scripts/rch_verify.sh --pinned-franken-stack --treeish <sha> \
    --build-admission-ee-bin /Users/jemanuel/.local/bin/ee -- \
    cargo test --locked -j 2 --test <target> <filter>
```

A killed attempt is not wasted: the export is cached and reused
(`pinned_bundle_is_valid`), and the worker keeps its target dir, so the retry
starts warm. Note `RCH_VERIFY_DEFAULT_TEST_TIMEOUT_SEC` also defaults to 1800
but only supplies `RCH_TEST_TIMEOUT_SEC` when that is unset, so it is already
handled if you set the variable above.

**When that budget fires, rch blames the wrong thing (bd-f4pmz).** Its own
external timeout kill is reported as resource exhaustion:

```text
WARN rch::hook: Remote build killed by SIGKILL (exit 137) on <worker>
  — likely resource exhaustion; will retry on a bigger worker if available
```

Check the arithmetic before believing it. Measured: the kill landed at
`9000728ms` against the `9000000ms` budget rch itself configured and echoed back
(`readback: "kind: job, timeout_secs: 9000"`) — within a second of the ceiling,
which is the timeout, not the OOM killer. Acting on the warning sends you to
look for memory pressure and move workers, which is the wrong repair and costs
another full build. **Exit 137 plus an elapsed time at your configured ceiling
is a timeout; exit 137 well short of it is worth treating as exhaustion.**
Either way a SIGKILL is UNGRADEABLE, not RED — there is no verdict in a killed
run, and `scripts/rch_run.sh` pins that distinction in its self-test.

**That warning's text has since changed, so do not grep for the old one.** As of
2026-10-24 rch says `— cause unconfirmed; inspect worker OOM and SSH/session
logs; will retry on another worker if available`. The blame is gone; the reading
rules above still apply.

**DIVIDE BEFORE YOU DISPATCH: a whole-target single-threaded run of a big e2e
target cannot finish inside any budget, and discovering that costs 4h15m.**
Measured 2026-10-24 on `--test integration_g_m -- --test-threads=1`, uncapped by
any filter:

| | |
|---|---|
| announced | `running 508 tests` |
| executed before the kill | 159 — 150 ok, 8 FAILED, 1 ignored |
| elapsed | 15340 s (exit 137) |
| observed rate | **96.5 s per test** |
| implied full suite | **13.6 h** |
| ceiling that was configured | 18000 s = 5 h |

The job was unsatisfiable the moment it was sent: at that rate the ceiling buys
about 186 of 508 tests, so it would have timed out with no `test result:` line
even had nothing killed it. The precheck is one division — tests announced by
the target, times the per-test cost you have measured, against your ceiling —
and it is cheaper than every other instrument in this file. **Filter the target
to the module you actually need.**

**The 96.5 s is a PROFILE artefact, not the tests being slow (bd-3vuhv).** The
e2e harness resolves `EE_BINARY` to the debug build and says so itself:

```text
ee_resolve_binary: PROFILE MISMATCH: caller requested release, EE_BINARY is a debug build
ee_resolve_binary: ... A debug build ran `ee init` ~10x slower than
ee_resolve_binary: release on an RCH worker (62.3s vs 6.18s), so timeouts calibrated for
ee_resolve_binary: release will fire against it and report a timeout, not a profile
```

So on a debug dispatch, read any e2e timeout as a profile symptom first. In that
same run `g5_curate_decay_e2e` went red solely because its `ee init` exited 124.

**A killed run still carries per-test verdicts, and they are worth harvesting.**
There was no summary line and the announced denominator never reconciled, so the
run yielded no SUITE verdict — but each `test … ok` / `… FAILED` line it printed
is a real observation of that test. Those 8 reds were the session's most
valuable output, and two of them were genuine defects nothing else had surfaced.
Read the log of a SIGKILLed job before paying to re-dispatch it.

**AND THE SAME IS TRUE OF A BUILD THAT FINISHED AND THEN FAILED TO POST THE
MAIL. Exit 102 with `rch_verify_topology_blocked` can be a COMPLETELY CLEAN
BUILD.** Measured 2026-10-26 on `cargo test --locked -j 2 --no-run --lib
--tests`, dispatched as a detector for a test target that had silently stopped
compiling:

| field | value |
|---|---|
| `status` | `rch_environment_failure` |
| `exit_code` | **102** |
| `degraded_codes` | `rch_verify_remote_command_failed`, `rch_verify_topology_blocked` |
| `elapsed_ms` | 11097933 — 185 minutes |
| in the log | `Finished \`test\` profile ... in 184m 20s`, **48** `Executable` lines, **0** lines matching `^error` |

The build succeeded. What failed came after it:

```text
[RCH] RCH-E327: artifact debug/ee is incompatible with aarch64-apple-darwin;
  rejected before downloading build artifacts; local outputs were not replaced
```

`--lib` builds `unittests src/main.rs`, which produces a `debug/ee`, and rch
correctly refuses to sync a Linux binary down onto an arm64 Mac. So the verdict
field describes the ARTIFACT RETURN and says nothing about the compile — while
the compile is the only thing that dispatch was for. Read `Finished` and count
the `Executable` lines; they are what `--no-run` produces instead of a
`test result:` line, and there must be one per built target (48 here = 46
declared `[[test]]` targets plus `unittests src/lib.rs` and `src/main.rs`,
which reconciles with the census above).

Two consequences worth keeping:

- **`--no-run --lib --tests` is the only detector for a test target that
  cannot COMPILE**, and that failure is invisible to everything else: exit 101
  is cargo's tests-failed code, a target that does not build prints no
  `Running` line so announcement pairing is silent, and there is no summary for
  the `filtered out` total to live in. One such break went unnoticed for three
  days (see bd-mbupe). `--require-targets` is the one rule that catches it,
  because it reads `Running` lines.
- **Dispatch it as `--tests` without `--lib`, or set
  `RCH_ALLOW_FOREIGN_ARTIFACTS=1`**, if you want the run to end in a status that
  describes the build. A single `--test <target>` run produces no `debug/ee` and
  does not hit this at all.

**After a SIGKILL, do NOT re-pin to that worker for its warm target dir.** The
next dispatch there was refused in 19 s:

```text
WARN rch::hook: Remote execution failed on <worker>: worker source namespace
     initialization refused on <worker> (status Some(73)): Killed
```

while `rch status` still reported that box `circuit=closed status=healthy` with
3 free slots, and `rch workers list` showed **92 GB free** — so the kill was not
build-disk exhaustion, and the post-kill unusability is invisible to every field
in both commands. Same family as the healthy-but-broken worker above. Pin
elsewhere and pay the cold build.

**`rch exec` DOES have worker pinning: `--worker <ID>`**, "Execute on this exact
worker; overrides environment/project preferences. Refuses local execution or a
different worker when unavailable." This line said it did not until 2026-10-14,
and that cost three dispatches: the scheduler re-picked one box that was
refusing every job with `worker source namespace initialization refused (status
73)` while reporting healthy with free slots, and a blind retry loop picked the
same box again. Pin past a bad worker rather than retrying into it. Pinning also
buys the warm target dir, since a `--job` reuses that worker's own directory.

**`scripts/rch_verify.sh` CAN PIN TOO — it forwards `RCH_WORKER`/`RCH_WORKERS`
to `rch exec` (`:5057-5058`) — and a WARM TARGET DIR BELONGS TO A WORKER, NOT
TO A BUNDLE SHA.** That distinction cost 98 minutes on 2026-10-26. A 184-minute
`--no-run` build had just finished for one pinned bundle; the very next
dispatch, at the same sha and the same export base, was sent without a pin,
landed on a different worker, and paid a fresh build of the same tree. The
export was reused (`pinned_bundle_is_valid`); the compiled artifacts were not,
because they live in the worker's own target directory.

**MEASURED FROM THE OTHER SIDE ON 2026-10-30, AND THE RATIO IS 23x.** Three
`cargo check --locked --all-targets -j 2` dispatches, same tree, same day:

| dispatch | worker | elapsed | why |
|---|---|---|---|
| 1 | unpinned | 1870 s | cold |
| 2 | unpinned, landed elsewhere | 1666 s | cold again |
| 3 | **`--worker <id of dispatch 2>`** | **70 s** | warm |

Dispatch 3 compiled the same 0.18.0 tree with a two-line change and reported
`Finished \`dev\` profile ... in 1m 05s`. So a pin is not a micro-optimisation
on this lane: it is the difference between a 28-minute round trip and a
coffee-free one, and it converts fix-verify-fix from a once-an-hour loop into
an interactive one. `rch exec` takes `--worker <ID>` directly; read the id from
the previous run's log line `Remote command finished` context or from
`rch status`.

**The matching hazard: pin only to a worker whose last run ENDED NORMALLY.** A
box that has just been SIGKILLed refuses the next dispatch in ~19 s while still
reporting `circuit=closed status=healthy`, and an unpinned dispatch that dies in
SSH transport retains build ownership on that worker. Both are above. Dispatch 3
was safe to pin because dispatch 2 ended with its own `Remote command finished`
line rather than a kill.

So when you deliberately want the previous run's warmth, read `worker_id` out
of that run's proof and pass it:

```bash
RCH_WORKER=<the worker_id from the previous proof> \
RCH_VERIFY_ATTEMPT_TIMEOUT_MS=18000000 \
RCH_VERIFY_COMMITTED_TREE_BASE=/Volumes/EERCHExport/rch-committed-tree \
TMPDIR=/Volumes/USBNVME16TB/temp_agent_space/tmp scripts/rch_verify.sh \
    --pinned-franken-stack --treeish <sha> \
    --build-admission-ee-bin /Users/jemanuel/.local/bin/ee -- \
    cargo test --locked -j 2 --test <target> <filter>
```

Weigh it against the refusal classes above: a pin that cannot be admitted is
refused whole rather than rescheduled, and a worker that has just been SIGKILLed
is unusable while still reporting healthy. Pin when the warmth is worth more
than the fallback, which — at 184 minutes a build — it usually is.

Pinning is not a capacity escape. A pinned request that cannot be admitted is
refused whole — `[RCH-I001] requested worker set [...] refused (selection error:
queue_timeout)`, exit 103 — because `--worker` declines to fall back. So pin to a
worker you have just seen idle in `rch status`.

**SIX DIFFERENT 103s, and only one is worth retrying.** Read the failure class
before the retry loop runs, because they are indistinguishable by exit code —
and five of the six are labelled `— retryable` by rch itself:

| refusal | meaning | action |
|---|---|---|
| `(selection error: queue_timeout)` | transient: nothing free right now | retry is appropriate |
| `(no_free_slots)` naming a core estimate | ARITHMETIC: the request cannot fit that worker | retrying is useless; change `-j` or drop the pin |
| `(unavailable); '<w>' is draining/unreachable` | that worker is GONE; its circuit is open | retrying the pin is useless; pin elsewhere |
| `RCH_SYNC_LIVE_CHECKOUT_REFUSED ... (status Some(73))` | that worker's `/data/projects/<project>` is a live git checkout, so syncing your tree over it is refused | permanent for that box; pin elsewhere |
| `[RCH-I002] ... refused (unavailable); failed admission under critical pressure (admission rejected: critical pressure (confidence=high, rule=memory_pressure>=critical_memory_pressure))` | that worker's MEMORY is exhausted; admission is rejected before any work | retrying the pin is useless, but this one is PREDICTABLE — see `pressure_state` below |
| `[RCH-E410] ... missing source_entrypoint .../benches/._<name>.rs` | the export is on a filesystem that cannot store xattrs, so macOS wrote AppleDouble `._*` sidecars; rch's preflight REQUIRES them remotely and `.rchignore` deliberately EXCLUDES them | retrying is useless on any worker — put the export on APFS; see below |

**That sixth one is the one that will eat a whole session, because it is
deterministic and rch calls it retryable.** Measured 2026-10-25, exit 103,
`status: rch_environment_failure`, `error_codes: ["RCH-E410"]`, ~29 s per
attempt plus ~4 min of export:

```text
WARN rch::hook::dependency_closure: Remote dependency preflight blocked remote
  execution [RCH-E410] evidence='missing source_entrypoint
  <export>/eidetic_engine_cli/benches/._adaptive_budget.rs: required package
  source entrypoint is missing on remote worker'
```

`._adaptive_budget.rs` is not a source file. The pinned export is materialized
by `git archive | tar -x` into `/Volumes/USBNVME16TB/rch-committed-tree/...`,
which is ExFAT; ExFAT cannot store extended attributes, so the AppleDouble shim
writes a `._name` companion per entry — **6071** of them in one export root.
Then two correct rules collide: `.rchignore:203-206` excludes `._*` from the
transfer, deliberately, because vendored C crates such as `zstd-sys` will
compile any `._foo.c` sibling and poison the build (bd-1h8ji.4); and rch's
closure preflight enumerates required entrypoints from the LOCAL export and
demands them REMOTELY. The requirement and the exclusion are both derived
locally, so **no worker can satisfy it** — pinning elsewhere cannot help.

Two things not to chase, both measured and both wrong:

- **`autobenches = false` fixes nothing.** It looks right beside the existing
  `autotests = false`, and it is safe (all 40 tracked `benches/*.rs` are
  explicitly declared, both difference sets empty). But `cargo metadata
  --no-deps --offline` on the export reports 40 bench targets and **zero**
  whose `src_path` contains `/._`. Cargo never sees these files; rch's own
  scanner does. (`metadata` is not a tripwired subcommand, so check this
  locally before theorising.)
- **`COPYFILE_DISABLE=1` does not suppress them.** Same archive, same ExFAT
  directory: 50 sidecars with it set, 50 without, 40 real `.rs` both arms.

**The filesystem is the only variable, and a sparse APFS image on the USB fixes
it** without deleting anything and without touching the internal disk. Same
archive: ExFAT 50 sidecars, APFS 0, 40 real `.rs` in both. `hdiutil info` shows
this machine already uses sparse APFS images for release work.

```bash
hdiutil create -size 240g -type SPARSE -fs APFS -volname EERCHExport \
    /Volumes/USBNVME16TB/temp_agent_space/tmp/ee-rch-export
hdiutil attach /Volumes/USBNVME16TB/temp_agent_space/tmp/ee-rch-export.sparseimage
RCH_VERIFY_COMMITTED_TREE_BASE=/Volumes/EERCHExport/rch-committed-tree \
TMPDIR=/Volumes/USBNVME16TB/temp_agent_space/tmp scripts/rch_verify.sh \
    --pinned-franken-stack --treeish <sha> \
    --build-admission-ee-bin /Users/jemanuel/.local/bin/ee -- \
    cargo test --locked -j 2 --test <target> <filter>
```

`RCH_VERIFY_COMMITTED_TREE_BASE` overrides the export base at
`resolve_committed_tree_export_base` (`scripts/rch_verify.sh:3103`), so this
needs no code change. Do NOT point it at `/` instead: one export root is 27 GB
against 29 GB free there, and a `.recovery-1` slot can double it — filling `/`
already killed a 50-minute run (bd-to1fr). The image must be re-attached after
a reboot. See bd-r4iwj.

**THIS IS NOT A REMEDY YOU REACH FOR AFTER AN E410 — IT IS MANDATORY ON EVERY
PINNED DISPATCH, because the DEFAULT on this host is the broken filesystem.**
`~/.zshenv:62` exports

```text
RCH_VERIFY_COMMITTED_TREE_BASE=/Volumes/USBNVME16TB/rch-committed-tree
```

which is the ExFAT volume, and it is in the live environment of every shell —
so a dispatch that passes no base at all inherits the hazardous one without
anything on the command line to show it. That path already holds 106 cached
bundles. Measured 2026-10-25: a pinned dry run with no override resolves to
exactly that base and is refused.

**The wrapper now refuses this before exporting rather than letting rch find it
four minutes later.** `assert_committed_tree_base_stores_xattrs`
(`scripts/rch_verify.sh:3156`) writes a real extended attribute into the
resolved base and looks for the `._*` sidecar it produces:

```text
status          source_state_refused     (NOT rch_environment_failure: nothing
exit_code       1                         to retry — no worker can satisfy it)
degraded_codes  ["rch_verify_committed_tree_base_appledouble_shim"]
stdout_tail     names the base, says it came from the environment, and gives
                the fix
```

Do not reach for the xattr write's exit status if you ever reimplement this.
Measured on both filesystems, `xattr -w` returns **0 on ExFAT as well as on
APFS** — it succeeds on ExFAT precisely because the shim writes the sidecar, so
a guard keyed on the write passes the hazardous case and detects nothing. The
sidecar's existence is the only reading that separates them.

**CONFIRMED on a real export, not just the probe.** The materialized bundle in
the image held **0** AppleDouble files against 8745 real `.rs` — the second
number is the control, because a zero-sidecar count on an export that failed to
materialize reads identically — and the dispatch went past the preflight into
compilation, 35+ minutes against refusals at 28.8 s and 29.2 s on ExFAT. One
variable, both directions. Duration alone separates the two states: you cannot
be 35 minutes into a run the closure refuses in 29 seconds.

**Do not choose a worker from `rch workers list`, because it cannot answer
admissibility — and its errors are biased toward the unusable.** Its records
carry `total_slots`, `priority`, `tags` and five disk fields, and *no*
reachability, circuit or drain field at all. `rch status --json` carries the
missing ones, at `.data.daemon.workers[]`: `circuit_state`, `status`,
`used_slots`. Measured 2026-10-24, the whole fleet:

```text
rch workers list    best candidate: 4 slots, disk_pressure_state=healthy
rch status          that same box:  circuit=open  status=unreachable
```

It advertised the most slots and the only `healthy` disk **because** it was
unreachable: nothing was running on it, so nothing was consuming its slots or
its disk. Ranking on "most free slots, healthiest disk" therefore selects FOR
dead workers. That cost one dispatch, refused in under a second.

The same reading corrected a worse error in the other direction. Every other
worker reported `warning` disk pressure, which read as a fleet with no
headroom and stopped this lane for a day; the reason code is
`disk_ratio_below_warning`, a PERCENTAGE threshold, and those boxes held
41–67 GB free with `used_slots=0` — all of them idle and admissible. Read
`disk_pressure_reason` before believing `disk_pressure_state`, and read
`used_slots`/`total_slots` rather than inferring free capacity from disk.

**`circuit_state` + `status` + slots is NOT enough: add `pressure_state`.** The
worker records carry a whole pressure block that the predicate above ignored —
`pressure_state`, `pressure_confidence`, `pressure_reason_code`,
`pressure_policy_rule`, `pressure_memory_pressure`, `pressure_disk_free_gb`,
`pressure_telemetry_fresh`. Measured 2026-10-25, a pinned `-j 2` job was
refused 103 on a box that `rch status` called `circuit=closed status=healthy`
with **3 free slots**, and the one field that saw it coming was in that block:

```text
pressure_state=critical  pressure_memory_pressure=93.8
pressure_policy_rule=memory_pressure>=critical_memory_pressure
```

The admissibility predicate that works:

```bash
rch status --json | jq -r '.data.daemon.workers[]
  | select(.circuit_state=="closed" and .status=="healthy"
           and ((.total_slots//0) - (.used_slots//0)) >= 2
           and .pressure_state != "critical") | .id'
```

**Exclude only `critical`, never `warning`.** Same trap as the disk reading
above, and it is easy to over-correct into: on that same fleet census all 12
workers reported `warning` or `critical` and **not one** reported healthy
pressure, so a predicate demanding healthy pressure selects nothing at all.
Read `pressure_policy_rule` to see which resource tripped — a `disk_free_ratio`
warning on a box holding tens of GB free is noise, a `memory_pressure` critical
is a refusal.

That still does not prove the lane is open. Two refusals the pressure block
does NOT predict, both measured the same day on boxes it rated `warning`:

```text
worker source namespace initialization refused (status Some(73)): Killed
worker source namespace initialization refused (status Some(73)):
  RCH: namespace control ended without settled group teardown
```

Same exit code and the same `status Some(73)` as the live-checkout refusal, but
different causes, and nothing in `rch status` or `rch workers list` exposes
either. Hitting it on two different workers in a row is the signal that it is
not per-box: see bd-o7q3t (source-authority lock failures across multiple
workers) and bd-s5v42 before spending more dispatches, because that bead
already records this family blocking every verdict on this project.

**Do NOT try to probe it with a no-build `--job`, and this corrects a recipe
that was in this file for part of 2026-10-24.** A dispatched command with no
`-j` is estimated at **4 cores whatever the command is** — measured on a bare
`echo`:

```text
[RCH-I003] requested worker set [worker-01] refused (no_free_slots);
'worker-01' has 3 total slot(s) but the request is estimated at 4 core(s);
pass -j 3 (or fewer), or request a larger worker
```

No worker in this fleet has 4 slots, so the probe is unadmissible on every one
of them, and `echo` has no `-j` to lower. **`-j` on `rch exec` itself is
`--json`** — the refusal's advice means the `-j` of the *dispatched cargo
command*, so it only exists when you are dispatching cargo.

Two consequences, both of which cost real time before being understood:

- A sweep of "probe each idle worker" returns `no_free_slots` on ALL of them
  and looks exactly like a saturated fleet. It is not; it is one arithmetic
  default. Do not conclude "a same-project build is holding the slot" from it —
  that inference was drawn here and was wrong, with `rch jobs` showing this
  project's only lease already `state=finished phase=finished`.
- The real dispatch is its own probe. A live-checkout refusal arrives in about
  two seconds, long before any compilation, so dispatch the actual target with
  `-j 2` and read the first lines of the log rather than paying for a separate
  round trip.

**The general consequence, measured 2026-10-24: NO NON-CARGO `--job` IS
ADMISSIBLE ON THIS FLEET.** The 4-core estimate is not overridable — `rch exec
--help` has no flag to declare one, and the `-j` in the refusal text belongs to
a dispatched *cargo* command, so a `python3 …` or `bash script.sh` job has
nothing to lower. Against that, the fleet census:

| | |
|---|---|
| workers | 12 |
| max `total_slots` among `closed` + `healthy` | **3** |
| admissible workers with `total_slots >= 4` | **0** |
| the only 4-slot box | `circuit=open`, `status=unreachable`, and tagged `os:windows` |

So the estimate exceeds the entire capacity of every worker that would take the
job. This is the arithmetic behind "shell e2e scripts are unrunnable on the
lane" (bd-unreachable-e2e-scripts-u14sr): they are not blocked by their
contents, they are blocked before selection. Do not spend dispatches
rediscovering it — if a job's command is not `cargo`, it will be refused until
rch gains an estimate flag or a worker gains a 4th slot.

Two further readings from that census, both of which bite the obvious
selection heuristic:

- **`closed` + `healthy` is not capacity.** Six of the twelve report
  `total_slots = 0` while `closed` and `healthy`, so a predicate that stops at
  those two fields calls them admissible. The `(total_slots - used_slots) >= 2`
  arm above is what excludes them; keep it.
- **Ranking on free slots picks the WINDOWS box first**, because it advertises
  4 free slots for the same reason it advertises a healthy disk — nothing runs
  on it. It could not build this project even if it were reachable. Platform is
  not a field the slot numbers expose; check `tags` before pinning.

Measured 2026-10-14: a `-j 2` job pinned to a worker that had since shrunk from
3 slots to 1 was refused five times by a retry loop, roughly six minutes spent
on a request that could never be admitted. The refusal named its own fix every
time:

    [RCH-I003] requested worker set [worker-01] refused (no_free_slots);
    'worker-01' has 1 total slot(s) but the request is estimated at 2 core(s);
    pass -j 1 (or fewer), or request a larger worker

Slots hot-reload, so a worker that hosted `-j 2` an hour ago may not now. A
retry loop that re-sends an unchanged pinned request is the blind-retry
anti-pattern this file already warns about, one layer down: it picks the same
box because you told it to.

**Before retrying a 103 at all, cap the job with `-j N`.** A dispatch's core
estimate must fit ONE worker's `total_slots`, and `rch status`'s fleet-wide
"slots_available" cannot tell you whether it does. An unhostable request is
refused as a bare `queue_timeout` — it took four dispatches on 2026-10-14 before
one attempt finally reported the real condition:

    [RCH-I003] requested worker set [...] refused (no_free_slots); '<worker>' has
    2 total slot(s) but the request is estimated at 4 core(s); pass -j 2 (or
    fewer), or request a larger worker

Adding `-j 2` to the job's own cargo line — inside the dispatched command, never
run locally, where `build` is a tripwire subcommand — was then admitted on the
FIRST attempt, against the same fleet that had just refused the uncapped form
three times. No amount of waiting fixes arithmetic; 17 free slots spread across
workers do not host a 4-core request. See bd-qi3qx for the reporting defect.

`rch diagnose -- <command>` prints the selection it WOULD make, including
"Requested worker", "Effective worker" and the reason — it is the fastest way to
see that affinity is routing you to a worker too small for the job, and it is
what the I001 text tells you to run.

## Architecture

```
src/
├── main.rs              # Entry point
├── lib.rs               # Library surface
├── cli/mod.rs           # CLI handlers (clap commands → core calls)
├── core/                # Business logic
│   ├── effect.rs        # Command effect declarations (read_only, durable_write)
│   ├── search.rs        # Hybrid search via frankensearch
│   ├── context.rs       # Context pack assembly with provenance
│   ├── memory.rs        # Memory storage (remember_memory, revisions, validity)
│   ├── learn.rs         # Procedural rule distillation
│   └── ...
├── db/                  # SQLModel/FrankenSQLite schema and migrations
├── pack/                # Pack binary format, budget classifier, validation
├── search/              # Search index management
├── eval/                # Evaluation runner and fixtures
└── output/              # JSON/Markdown/TOON formatters

tests/
├── golden/              # Surface contract snapshots (*.snap)
├── snapshots/           # JSON contract snapshots (insta)
├── fixtures/            # Test fixtures (eval scenarios, etc.)
│   └── golden/          # Command/schema golden artifacts (316 files)
└── *.rs                 # Integration and E2E tests
```

## Key Conventions

**Forbidden dependencies** — CI fails if these appear anywhere in the tree:
- `tokio`, `async-std`, `smol` — use `asupersync` for runtime
- `rusqlite` — use `fsqlite` via `sqlmodel`
- `petgraph` — use `fnx-*` crates
- `sqlx`, `diesel`, `sea-orm` — use `sqlmodel`
- `hyper`, `axum`, `tower`, `reqwest` — no HTTP stacks in core

**Franken-stack crates** live in `/data/projects/` siblings:
- `asupersync` — runtime (`Cx`, `Scope`, `Outcome`, budgets)
- `fsqlite` + `sqlmodel` — database layer
- `frankensearch` — hybrid BM25 + vector search
- `fnx-*` — graph analytics

**Golden snaps** document surface contracts across three locations:
- `tests/golden/*.snap` — command output and schema contracts
- `tests/snapshots/*.snap` — JSON contract snapshots (context, search, why, status, doctor)
- `tests/fixtures/golden/**/*.golden` — command/schema golden artifacts (316 files)

The closure-lint script requires these for implements-surface beads.

**Effect declarations** in `src/core/effect.rs` classify commands as `read_only`, `durable_write`, or `degraded_unavailable`.

**No worktrees or branches** — all work happens directly on `main`.

## Testing

- Unit tests: inline `#[cfg(test)]` modules
- Golden tests: `tests/golden.rs`, `tests/golden/*.snap`, `tests/snapshots/*.snap`, `tests/fixtures/golden/**`
- E2E: `tests/*_e2e.rs` and `scripts/e2e_*.sh`
- Property tests: `proptest` in `tests/property_*.rs`
- Forbidden deps: `tests/forbidden_deps.rs`

Run `./scripts/verify.sh` before pushing. It is the broadest check available
here, but nothing enforces it and it is **not** congruent with the hosted CI
gates — several of those are disabled, so a green run is necessary, not
sufficient. It does gate rustfmt (Rustfmt Conformance, added in `453291c25`).

**ON THIS MAC IT CANNOT RUN A SINGLE STAGE, IN ANY MODE.** Measured 2026-10-26
by running it:

```text
./scripts/verify.sh                  rc=1, 3 lines of stdout (the banner)
./scripts/verify.sh --plan-doc-smoke rc=1, 3 lines of stdout
./scripts/verify.sh --ci-smoke       rc=1, 3 lines of stdout
    verify: refusing to run stages against an unverified binary.
    verify: build it first (cargo build --locked --bin ee) or export
    verify: EE_BINARY to one built from this source tree.
```

The gate is `scripts/verify.sh:448` and it sits BEFORE mode dispatch, so the
narrow modes do not escape it — including `--ci-smoke`, which `--help` describes
as "agent pre-push readiness". `ee_require_current_binary` refuses all three
candidates on this host, correctly: `~/.local/bin/ee` is **0.14.2** against a
**0.18.0** tree, and both `CARGO_TARGET_DIR` binaries are Linux ELF x86-64.
Both printed remedies are unavailable — `cargo build` is tripwired
(`check-local-cargo-tripwire.sh:112`), and `EE_BINARY` needs a native current
binary that does not exist here.

So **do not cite a local verify.sh run as evidence, and do not read its exit 1
as a red**: nothing has been verified, and nothing has failed either. The stage
results on this host are *unobserved*, not green. The same applies to every
`scripts/e2e_*.sh` — `ee_require_current_binary` has 34 callers and they are all
in this position. Use the pinned RCH lane, which builds from source on a worker
and is the only rail that exercises this tree.

Tracked as `bd-no-runnable-current-ee-binary-on-the-mac-wu6ay`. The guard is
not the defect; the missing native binary is.

### Four ways a test run reports success having verified NOTHING

All four exit 0, all four print `ok.`, and all four have cost this project
hours. None is detectable from the exit code, so **read the denominator, never
the status.** The third is the worst of the first three, because its green is
real — it just belongs to a different process. The fourth is worse still,
because it is the only one that survives the announcement-pairing defence
below.

**1. A multi-target run stops at the first failing target (bd-dwe8f).** Cargo
runs targets in order and stops at the first failure. The lib target carries
chronic failures, so in

```bash
scripts/rch_verify.sh --pinned-franken-stack --treeish HEAD -- \
    cargo test --locked --lib --test contracts     # contracts NEVER RUNS
```

the second target is silently never reached, while the command reads as a
*broader* check than a single-target run. Measured across three jobs at three
bases: zero `Running` lines for a contracts target hash in every log. That
voided five certifications in one night. Dispatch the gate target on its own, or
pass `--no-fail-fast` when you genuinely want both.

**AND THE SAME ORDERING MEANS A RED LIB HIDES AN UNKNOWN NUMBER OF TEST-TARGET
BREAKS, so the first green lib is not the end of a repair.** Measured
2026-10-30 across three `cargo check --locked --all-targets` dispatches. While
the lib was red (bd-1ks7e), no test target was ever reached. The dispatch that
confirmed the lib fix reported, 36 lines apart:

```text
warning: `eidetic-engine` (lib) generated 1 warning          <- the lib COMPILES
error[E0433]: cannot find `MemoryLinkRelation` in `models`
error: could not compile `eidetic-engine` (test "integration_e_f")
```

That second break was older than the lib break and completely invisible behind
it; `cargo check` and `cargo check --lib` do not build test targets at all.
**Read the lib's own verdict separately from the run's**, because the run exits
101 with zero `Finished` lines and reads as a total failure while the lib
succeeded. Then expect to repeat: each repair can expose the next target in
line. Here it took two rounds to reach a green `--all-targets`, and the second
round cost 70 s rather than 28 minutes only because it was pinned to the warm
worker.

**2. `--exact` matches the FULL module path, and a zero-match run says `ok.`**
A test pulled into a suite by `#[path]` lives under the *including* module, so
`tests/contracts/resume_schema.rs` inside `tests/contracts.rs` gives ids like
`resume_schema::<name>`. A bare name under `--exact` matches nothing and prints

```text
test result: ok. 0 passed; 0 failed; 0 ignored; 1507 filtered out
```

with exit 0. Measured 2026-10-17: that cost a 56-minute release-profile dispatch
and produced no verdict. Prefer a long unique substring and omit `--exact`, so a
module rename degrades to a correct match instead of to silence.

**And `--test <file_stem>` is usually NOT a target, because `Cargo.toml` sets
`autotests = false`.** Census 2026-10-25, with a validated instrument:

| | |
|---|---|
| top-level `tests/*.rs` files | 531 |
| declared `[[test]]` targets | 46 |
| reachable AS their own target | **36** |
| reachable only via `#[path]` inclusion | **495** |
| reachable by neither (dead files) | **0** |

So for 495 of 531 files there is no `--test <file_stem>` to dispatch; you must
filter the SUITE that includes the file. `tests/volatile_field_registry_consistency_test.rs`,
for instance, is reached only through `tests/suites/integration_s_z.rs`, so the
target is `--test integration_s_z` and the test id is prefixed
`volatile_field_registry_consistency_test::`. Find the owner before dispatching:

```bash
grep -rn 'path = "../<file>.rs"' tests/suites/ tests/*.rs
```

The zero in that last row is a real negative, not an unmeasured one: the same
predicate reports a synthetic unreferenced file as dead, and an earlier version
of it over-matched `#[path]` inside comments — which is how it initially credited
four includes pointing at files that do not exist (`tests/contracts/X.rs`,
`tests/suites/...`). Strip comment lines before trusting any `#[path]` sweep.

**3. A CHILD PROCESS PRINTS ITS OWN SUMMARY ONTO THE SAME STREAM (bd-6g6s2).**
Several `ee` tests spawn a child that itself runs tests — the `ee` binary, a
nested cargo invocation, an e2e harness. The child emits a complete, well-formed
libtest summary that is textually identical in form to the target's:

```text
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10041 filtered out
```

Nothing in that line says which process wrote it, and it can land **before the
parent target has finished**, so a log that is still being written already
contains a green summary. Measured 2026-09-18: tail-reading one of these nearly
closed a P0 fail-open security fix (bd-zjcx6) on a green belonging to another
process, and the orchestrator had independently reported the job complete, so
every pressure pointed at believing it.

**4. A REPEAT `--job` DISPATCH RUNS THE PREVIOUS DISPATCH'S SOURCE (bd-9z8ee).**
Re-dispatching `rch exec --job` to the SAME pinned worker can execute the tree
as it was at an earlier dispatch's sync. Measured 2026-10-25 across three
consecutive dispatches: after adding one `#[test]` to a target, the worker
produced the **same test binary hash** and the same total test count, and
`-- --list` showed 8 of the file's 9 tests. Cargo never rebuilt, because as far
as the worker was concerned nothing had changed.

**This is the only one of the four that beats the announcement-pairing rule
below.** Re-run an EXISTING test after editing the code it covers and you get
exit 0, a `Running` line proving the target executed, and an announcement and
summary that reconcile *perfectly* — because both are honest. They are simply
about source that is no longer on disk. It was caught here only by luck: the
filtered test did not exist in the stale binary, so it printed `running 0
tests`, which is trap 2.

**The tell is the TOTAL, not the summary.** `passed + failed + ignored +
filtered out` is the target's whole test count, and it cannot stay the same
across a commit that added a test. Record that number; when it does not move,
the source did not arrive. And note that `--job` cannot be audited: it refuses
both `--source-content-receipt` and `--clean-overlay`, so declaring or checking
the transferred set is not available on that rail. Until bd-9z8ee is fixed,
treat a repeat `--job` to the same pinned worker as UNVERIFIED — dispatch to a
different worker, or verify a committed tree through `scripts/rch_verify.sh`,
which materialises a source export rather than syncing the working tree.

**The discriminator for the first three is the announcement, and it is cheap.**
libtest prints
`running N tests` *before* it runs them. A summary belongs to that announcement
only when `passed + failed + ignored == N`. Pair each announcement with its
summary; a run whose announced `N` is tiny relative to the target is somebody
else's process. `scripts/rch_run.sh` implements exactly this pairing and
self-tests it (`--self-test`, 31 arms, including a `--job` whose body runs cargo
test and the same job when its announcements do not reconcile).

**And on that target the exit code is not a verdict either.** An unfiltered
`--lib --test contracts` run exits 101 regardless of the change under test,
because the lib target carries known pre-existing failures (baseline 9795
passed / 11 failed / 4 ignored of 9810). Both the exit status and the last
summary line are unreliable there; the announced denominator is what is left.

**The defence is an assertion inside the job, not a habit.** All three traps are
invisible for as long as the dispatch takes, so have the job grade itself and
exit nonzero:

```bash
# STRIP ANSI FIRST. cargo emits escapes *between* "Running" and the following
# space, so a literal 'Running .*<target>' never matches the raw bytes.
sed $'s/\033\\[[0-9;]*m//g' out.txt > clean.txt
RESULT=$(grep -a '^test result:' clean.txt | tail -1)
P=$(echo "$RESULT" | sed -n 's/.* \([0-9]*\) passed.*/\1/p')
F=$(echo "$RESULT" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')
grep -qa 'Running .*<target>' clean.txt || { echo 'VERDICT=NONE no Running line'; exit 91; }
test $(( ${P:-0} + ${F:-0} )) -eq <N filters> || { echo 'VERDICT=NONE'; exit 90; }
```

`Running` proves the target executed; `passed + failed` proves which tests did.
`filtered out` is not part of that sum.

**That `tail -1` is safe only when nothing in the job spawns a test-running
child** — otherwise it is trap 3 above, written into the defence against it. Use
it when you dispatch one target and know its tests spawn no nested runner. When
you are not sure, or the body is a `--job` shell script, SUM across every
`test result:` line (`awk '{s+=$1}'`) and require the total to equal the
expected count, or pair `running N tests` with its summary as
`scripts/rch_run.sh` does. Summing is also what a multi-target `--no-fail-fast`
run needs, since `tail -1` there grades only the last target.

**The `sed` is not optional, and this is measured, not theoretical.** An earlier
version of this block omitted it. A dispatch on 2026-10-17 then ran the target,
executed exactly one test and failed it legitimately — and the grader reported
`RUNNING_LINES=0` and exited 91 as though nothing had run, because the log held
`Running` + `ESC[0m` + ` tests/contracts.rs`. The real reading was `TEST_RC=101`
with `RAN=1 FAILED=1`. A grader that cannot see a `Running` line turns a genuine
product failure into "no verdict", which is the worse error of the two: it looks
like infrastructure noise and gets retried instead of reported.

**`CARGO_TERM_COLOR=never` does NOT remove the need for it.** Measured
2026-10-25: a `--job` dispatched with that variable set still returned a log
holding **199** escape sequences, and they sit *before* `Running` as well as
inside it, because the indent the lane applies is itself colored. Set the
variable if you like; still strip.

**And write the strip as ONE pattern.** This grader is BSD `sed` on the Mac
client, where `\|` is a literal, not alternation — so "improving" the line to
`s/\033\[[0-9;]*h\|\033\[[0-9;]*m//g` matches nothing and silently strips
NOTHING. Measured on the same log: the two-alternative form left all 199
escapes in place and `grep '^ *Running '` found zero lines; the single-pattern
form above left 0 escapes and matched immediately. The failure mode is the
worst available — a grader that reports "no verdict" for a run that passed,
with no sign that its own sanitiser did nothing. Chain two `sed` calls, or use
one bracket class, rather than an alternation.
