# EE-282: Asupersync CLI Runtime Boundary Spike

Status: completed spike  
Recommendation: go, with a thin CLI runtime adapter and an explicit request-Cx API gap  
Owner: SwiftBasin  
Date: 2026-04-29

## Question

How should `ee` use Asupersync at the CLI boundary so command execution keeps
the project's no-Tokio, cancellation-aware, budget-aware, and deterministic-test
requirements without turning Phase 0 into a runtime project?

The answer is to keep Asupersync as the process-level async shell now, preserve
`Outcome` and budget concepts in command contracts immediately, and avoid
claiming full production `&Cx` propagation until the public Asupersync API can
mint runtime-backed request contexts outside the Asupersync crate.

## Recommendation

Use the current-thread Asupersync runtime as the only Phase 0 CLI executor:

```text
clap parse
  -> workspace/config/output policy
  -> command budget description
  -> core::build_cli_runtime()
  -> core command future
  -> ee output envelope + exit code
```

Keep the adapter small. It should live in `core` or a similarly low-level
runtime module, not in individual commands. Command services should be shaped so
they can accept `&Cx` when a real runtime-backed context is available, but the
current implementation must not create fake production contexts through
test-only APIs.

Concrete guidance:

- Keep `asupersync = { version = "0.3.1", default-features = false, features = ["tracing-integration"] }`.
- Do not enable `asupersync/default`, `test-internals`, `sqlite`, `tower`,
  `cli`, `kafka`, `tls`, `http3`, `quic`, or the Tokio compatibility crate in
  `ee` default builds.
- Use `RuntimeBuilder::current_thread()` for Phase 0 CLI commands.
- Preserve an `Outcome<T, EeError>` or equivalent internal command result shape
  before mapping to stdout, stderr, and exit codes.
- Use `LabRuntime` for deterministic cancellation, budget, and scheduling tests.
- Do not use `Cx::for_testing`, `Cx::for_request`, or
  `Cx::for_testing_with_budget` outside tests.

## Source Snapshot

Local dependency source inspected:

- `/data/projects/asupersync`, git `3057119a0`.
- `git -C /data/projects/asupersync status --short` returned
  `fatal: bad object HEAD`, so source cleanliness could not be established.
- `/data/projects/asupersync/Cargo.toml`
- `/data/projects/asupersync/src/lib.rs`
- `/data/projects/asupersync/src/cx/cx.rs`
- `/data/projects/asupersync/src/cx/scope.rs`
- `/data/projects/asupersync/src/runtime/builder.rs`
- `/data/projects/asupersync/src/types/budget.rs`
- `/data/projects/asupersync/src/types/outcome.rs`
- `/data/projects/asupersync/src/lab/runtime.rs`
- `/data/projects/asupersync/asupersync-macros/src/{lib,race}.rs`
- `Cargo.toml`
- `src/core/mod.rs`
- `README.md`
- `docs/adr/0003-native-asupersync-runtime.md`
- `docs/dependency-research-notes.md`

No Cargo build, clippy, or test command was run for this docs-only spike. Any
future compile or test verification must use `rch exec -- cargo ...`.

## Current EE State

`Cargo.toml` already declares:

```toml
asupersync = { version = "0.3.1", default-features = false, features = ["tracing-integration"] }
```

That is the right starting point. It avoids Asupersync's default
`test-internals` and `proc-macros` features, and it does not enable optional
surfaces that would violate EE constraints.

`src/core/mod.rs` already contains the walking runtime bootstrap:

- `CLI_RUNTIME_WORKERS: usize = 1`
- `RuntimeProfile::CurrentThread`
- `runtime_status()` reporting `engine = "asupersync"` and
  `async_boundary = "core"`
- `build_cli_runtime()` using
  `asupersync::runtime::RuntimeBuilder::current_thread()`
- `run_cli_future()` building the runtime and calling `Runtime::block_on`
- Unit coverage for build info, runtime status, future completion, and
  deterministic `LabRuntime` seed construction

That implementation aligns with this spike's recommendation as a first
boundary. The remaining work is not to replace it, but to add command result,
budget, cancellation, and test contracts around it.

## Asupersync Facts That Matter

### Runtime Builder

`RuntimeBuilder::current_thread()` is documented as the single-threaded Phase 0
and testing configuration. `Runtime::block_on(future)` is public and installs a
thread-local `RuntimeHandle` while the future is polled.

This is enough for `ee` to have one Asupersync executor for command futures.
It is not enough, by itself, to prove that every command has a production
`Cx::current()` or request-scoped `&Cx`.

Important public/private split:

- `Runtime::block_on` is public.
- `Runtime::current_handle` is public.
- `Runtime::spawn_blocking` is public, but returns `None` when no blocking pool
  is configured.
- `Runtime::block_on_with_cx` is `pub(crate)`.
- `Runtime::block_on_current_with_cx` is `pub(crate)`.
- `Runtime::request_cx_with_budget` is `pub(crate)`.

That split is the main integration gap for EE's desired "every async path takes
`&Cx`" rule.

### Cx

`Cx` is the capability token for task identity, budget, cancellation, tracing,
time, I/O, entropy, and related runtime capabilities. It is technically
`Send + Sync`, but the source explicitly says its semantic contract is
task-associated and it should not be shared across task boundaries.

Public production methods that matter:

- `budget() -> Budget`
- `is_cancel_requested() -> bool`
- `checkpoint() -> Result<(), asupersync::Error>`
- `checkpoint_with(message) -> Result<(), asupersync::Error>`
- `cancel_with(kind, message)`
- `cancel_fast(kind)`
- `cancel_reason()`
- `restrict<NewCaps>()`

`checkpoint()` checks both cancellation and budget exhaustion, and returns an
Asupersync error when cancellation is observed. EE command services should call
checkpoints around long loops, index rebuild phases, import phases, and pack
assembly phases once a real `&Cx` is available.

### Test-Only Context Constructors

The convenient context constructors are intentionally gated:

- `Cx::for_testing()`
- `Cx::for_testing_with_budget(...)`
- `Cx::for_request()`
- `Cx::for_request_with_budget(...)`

They are behind `#[cfg(any(test, feature = "test-internals"))]`. The source
comment explains why: allowing external crates to mint full-capability contexts
would bypass runtime capability masks.

EE must not enable `test-internals` in production just to get these
constructors. Tests may use them through normal `cfg(test)` visibility when the
Asupersync crate exposes them to test builds, but production code should receive
contexts from a real runtime boundary.

### Budget

`Budget` carries:

- Optional absolute deadline
- Poll quota
- Optional cost quota
- Priority

Its meet semantics tighten deadlines and quotas while preserving higher
priority. Asupersync treats exhausted budget as cancellation. For EE, command
timeouts and pack token budgets should stay separate concepts:

- Runtime budget: cancellation, poll/deadline/cost behavior.
- Pack budget: deterministic context token accounting.

They can be reported together, but they should not be collapsed into one type.

### Outcome

`Outcome<T, E>` is a four-variant result lattice:

- `Ok(T)`
- `Err(E)`
- `Cancelled(CancelReason)`
- `Panicked(PanicPayload)`

Severity order is `Ok < Err < Cancelled < Panicked`. EE should preserve that
distinction internally and only map it at the final CLI boundary.

Recommended CLI mapping:

| Outcome | CLI behavior |
| ------- | ------------ |
| `Ok(T)` | write machine data to stdout, diagnostics to stderr, exit `0` |
| `Err(EeError)` | emit human or JSON error shape, use the error's exit code |
| `Cancelled(reason)` | emit stable cancellation/degraded error, exit `6` unless a later ADR defines a dedicated code |
| `Panicked(payload)` | emit stable internal error, exit `6` or a future dedicated internal-error code |

Do not flatten `Cancelled` or `Panicked` into a generic application error inside
core command code.

### Scope And Structured Concurrency

`Scope` owns spawned work and closes regions to quiescence. Child scopes meet
their budget with the parent budget. Spawned task outputs must be
`Send + 'static`, which is a useful pressure against accidentally capturing
borrowed command state in long-lived work.

Phase 0 commands probably do not need much spawning. When they do, use scoped
work for clearly parallel derived tasks, such as independent read-only
projection or verification steps. Do not introduce background jobs that outlive
the command.

### LabRuntime

`LabRuntime` provides virtual time, deterministic scheduling, deterministic
entropy, trace capture, and reports with trace fingerprints and oracle results.
Useful public surfaces include:

- `LabRuntime::new(LabConfig::new(seed))`
- `now()`
- `steps()`
- `run_until_quiescent()`
- `run_with_auto_advance()`
- `run_until_quiescent_with_report()`
- `report()`

EE runtime-contract tests should use fixed seeds and assert stable fingerprints,
step counts, cancellation outcomes, and stdout/stderr separation where the CLI
surface is involved.

## Minimal Boundary Shape

Recommended module responsibilities:

```text
cli
  parse args only
  choose human/json output mode
  call core command entry

core runtime adapter
  build current-thread Asupersync runtime
  install tracing subscriber if configured
  run one command future
  map command outcome to process output contract

core command service
  accept workspace/config/request/budget data
  accept real &Cx when available
  return Outcome<T, EeError> or equivalent

output
  render ee.*.v1 JSON to stdout
  render human diagnostics to stderr
  never inspect runtime internals
```

The current adapter can remain:

```rust
pub fn run_cli_future<F, T>(future: F) -> RuntimeResult<T>
where
    F: Future<Output = T>,
{
    let runtime = build_cli_runtime()?;
    Ok(runtime.block_on(future))
}
```

The next layer should wrap it with an EE command outcome, for example:

```rust
pub enum CommandOutcome<T> {
    Ok(T),
    Err(EeError),
    Cancelled(CommandCancel),
    Panicked(CommandPanic),
}
```

or directly use `asupersync::Outcome<T, EeError>` if that composes cleanly with
the rest of the domain model. The important part is preserving the four states
until the final output/exit-code mapping.

## Request-Cx API Gap

There is a real gap between EE's architecture rule and the public Asupersync
surface available today:

```text
EE wants:
  command_service(cx: &Cx, request) -> Outcome<T, EeError>

Asupersync currently exposes:
  Runtime::block_on(future) -> T

Asupersync currently keeps crate-private:
  Runtime::request_cx_with_budget(...)
  Runtime::block_on_with_cx(...)
```

Do not work around this by enabling `test-internals` or calling test-only
constructors in production. The honest Phase 0 path is:

1. Keep `run_cli_future` as the executor boundary.
2. Add an EE-owned command budget/request context type for public command
   contracts.
3. Shape services so they can accept a real `&Cx` without major rewrites.
4. Add tests that use `LabRuntime` and test-only `Cx` only under `cfg(test)`.
5. Track the upstream/public-API need as a follow-up before claiming complete
   `&Cx` propagation for production commands.

Potential follow-up with Asupersync:

```text
Expose a public, non-escalating request context runner:
  Runtime::run_with_budget(budget, future: impl FnOnce(&Cx) -> Future)

Requirements:
  - request Cx inherits runtime drivers and cap mask
  - no ambient full-capability constructor
  - cancellation and budget exhaustion become Outcome::Cancelled
  - panic capture can become Outcome::Panicked
```

Until such a public API exists, EE documentation should say "Asupersync is the
runtime shell" rather than "all production command paths already receive `&Cx`".

## Blocking Work

The current-thread runtime does not configure a blocking pool. Asupersync
`spawn_blocking` returns `None` when no blocking pool is configured.

Phase 0 should therefore keep blocking operations simple and explicit:

- SQLModel/FrankenSQLite open and query calls happen in bounded command phases.
- CASS process invocation happens at a clear import adapter boundary.
- Index rebuild file I/O happens in a single foreground command path.
- Long loops checkpoint once a real `&Cx` is available.

Do not add a blocking pool until there is a concrete command that needs it and a
test showing cancellation, stdout/stderr behavior, and shutdown semantics.

## Feature Policy

Recommended production dependency:

```toml
asupersync = { version = "0.3.1", default-features = false, features = ["tracing-integration"] }
```

Feature decisions:

| Feature | Decision | Reason |
| ------- | -------- | ------ |
| `tracing-integration` | allow | Needed for structured diagnostics without Tokio |
| `proc-macros` | defer | Useful later, but direct API keeps Phase 0 easier to audit |
| `test-internals` | tests only | Exposes context constructors that must not exist in production |
| `sqlite` | forbid | Pulls optional `rusqlite`, forbidden by EE |
| `tower` | forbid by default | Adapter surface not needed for CLI walking skeleton |
| `cli` | forbid | Adds Asupersync's own CLI dependencies, not EE command logic |
| `tls`, `kafka`, `quic`, `http3` | forbid | Non-goals for local CLI Phase 0 |
| `asupersync-tokio-compat` | forbid | Tokio compatibility layer is outside EE core constraints |

CI must continue to audit `cargo tree -e features` for forbidden crates. When
feature changes are tested, use:

```bash
rch exec -- cargo tree -e features
```

## Test Requirements

The next implementation beads should add tests in this order.

### Runtime Bootstrap

Already started by EE-004:

- `runtime_status()` reports Asupersync, current-thread profile, one worker, and
  `core` async boundary.
- `run_cli_future(async { value })` completes and returns the value.
- `LabRuntime::new(LabConfig::new(seed))` is deterministic for basic state.

### Outcome Mapping

Add focused tests for the final CLI mapper:

- `Ok` writes only data to stdout and exits `0`.
- `Err(EeError)` uses the configured error exit code and stable JSON/human error
  shape.
- `Cancelled` maps to stable degraded/cancelled output and does not masquerade
  as an application error.
- `Panicked` maps to stable internal-failure output without panic text leaking
  into machine fields.

### Budget And Cancellation

Add unit tests under `cfg(test)` using test-only contexts:

- A command loop observes `cx.checkpoint()` cancellation.
- A zero or exhausted budget becomes cancellation.
- Nested command budgets meet with parent constraints once public request-Cx
  support exists.
- Cancellation diagnostics go to stderr or explicit JSON error fields, never
  mixed into successful JSON stdout.

### Lab Runtime Contracts

Add deterministic runtime tests:

- Same seed and same command scenario produce the same report fingerprint.
- Different cancellation point produces a distinct failure artifact or reason.
- `run_until_quiescent_with_report()` is used for scenarios with spawned work.
- Tests have `max_steps` so regressions fail diagnostically instead of hanging.

### Feature And Dependency Audit

Add or keep a forbidden dependency test that proves the selected Asupersync
feature set does not pull:

- `tokio`
- `tokio-util`
- `async-std`
- `smol`
- `rusqlite`
- `hyper`
- `axum`
- `tower`

This audit should run through RCH in local agent workflows:

```bash
rch exec -- cargo tree -e features
```

## Risks

| Risk | Impact | Mitigation |
| ---- | ------ | ---------- |
| Public request-Cx API is unavailable | EE cannot honestly claim full production `&Cx` propagation | Keep adapter thin, avoid fake contexts, track follow-up |
| Enabling Asupersync defaults accidentally | `test-internals` and proc macros enter production build | Keep `default-features = false`, audit feature tree |
| Enabling `sqlite` | Pulls forbidden `rusqlite` | Explicitly forbid and audit |
| Flattening `Outcome` into `Result` too early | Cancellation and panic semantics disappear | Preserve four-state command outcome until output boundary |
| Blocking operations on current-thread runtime | Cancellation can be delayed | Keep blocking phases bounded, add blocking pool only with tests |
| Tests rely on wall time | Runtime behavior becomes flaky | Use `LabRuntime`, fixed seeds, virtual time, and max steps |

## Follow-Up Beads

This spike should inform or unblock:

- Gate 3: Asupersync Contract Fixtures
- Gate 0: Integration Foundation Smoke Test
- EE-313: Add integration foundation smoke test gate
- The first command-output mapper bead
- The first command cancellation/budget fixture bead
- Any upstream task to expose a public, non-escalating
  `Runtime::run_with_budget` or equivalent request-Cx runner

## Closure Notes

This is a docs-only source-research spike. It does not modify runtime code
because EE-004 already added the current-thread bootstrap and because the next
implementation step requires a public contract decision, not another local
runtime wrapper.

Acceptance coverage:

- Go/no-go recommendation: go, with a thin adapter and explicit request-Cx gap.
- No-Tokio/no-rusqlite behavior: preserved by recommended feature set.
- Public output: no new command output introduced.
- Tests: listed above for the implementation beads that will exercise this
  boundary.
- Verification: Markdown-only checks were run locally; no Cargo command was
  needed for this spike.
