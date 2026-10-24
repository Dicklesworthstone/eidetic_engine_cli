# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is ee?

`ee` is a Rust CLI that provides durable, local-first memory for coding agents. It captures facts, decisions, rules, and session evidence; indexes them with hybrid search; and emits compact context packs that agent harnesses (Codex, Claude Code, etc.) consume.

## Build Commands

**Cargo does not run locally in this repo.**
`scripts/check-local-cargo-tripwire.sh` forbids the subcommands `build`,
`check`, `test`, `bench`, `clippy`, `doc`, `run`, `install`, `rustc` and `fix`.
`fmt` is the one exception, so it is also the only local syntax check you have.

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

**`rch exec` DOES have worker pinning: `--worker <ID>`**, "Execute on this exact
worker; overrides environment/project preferences. Refuses local execution or a
different worker when unavailable." This line said it did not until 2026-10-14,
and that cost three dispatches: the scheduler re-picked one box that was
refusing every job with `worker source namespace initialization refused (status
73)` while reporting healthy with free slots, and a blind retry loop picked the
same box again. Pin past a bad worker rather than retrying into it. Pinning also
buys the warm target dir, since a `--job` reuses that worker's own directory.

Pinning is not a capacity escape. A pinned request that cannot be admitted is
refused whole — `[RCH-I001] requested worker set [...] refused (selection error:
queue_timeout)`, exit 103 — because `--worker` declines to fall back. So pin to a
worker you have just seen idle in `rch status`.

**FOUR DIFFERENT 103s, and only one is worth retrying.** Read the failure class
before the retry loop runs, because they are indistinguishable by exit code —
and three of the four are labelled `— retryable` by rch itself:

| refusal | meaning | action |
|---|---|---|
| `(selection error: queue_timeout)` | transient: nothing free right now | retry is appropriate |
| `(no_free_slots)` naming a core estimate | ARITHMETIC: the request cannot fit that worker | retrying is useless; change `-j` or drop the pin |
| `(unavailable); '<w>' is draining/unreachable` | that worker is GONE; its circuit is open | retrying the pin is useless; pin elsewhere |
| `RCH_SYNC_LIVE_CHECKOUT_REFUSED ... (status Some(73))` | that worker's `/data/projects/<project>` is a live git checkout, so syncing your tree over it is refused | permanent for that box; pin elsewhere |

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

The admissibility predicate that works:

```bash
rch status --json | jq -r '.data.daemon.workers[]
  | select(.circuit_state=="closed" and .status=="healthy"
           and ((.total_slots//0) - (.used_slots//0)) >= 2) | .id'
```

That still does not prove the lane is open, because the live-checkout refusal
above is invisible to every field in both commands.

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

### Three ways a test run reports success having verified NOTHING

All three exit 0, all three print `ok.`, and all three have cost this project
hours. None is detectable from the exit code, so **read the denominator, never
the status.** The third is the worst, because its green is real — it just
belongs to a different process.

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

**The discriminator is the announcement, and it is cheap.** libtest prints
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
