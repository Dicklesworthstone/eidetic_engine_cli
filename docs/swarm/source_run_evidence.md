# Source Run Evidence

`ee.source_run_evidence.v1` is the source-run watchdog contract for
`bd-12v87.1` and `bd-12v87.2`. It records one bounded external source execution: Beads/BV
reads, Agent Mail health, CASS probes, RCH verifier calls, and future swarm
collectors.

The shared Rust runner lives in `src/core/source_run.rs`. It enforces an
explicit timeout budget, captures bounded stdout/stderr tails, records spawn
failures as evidence, and terminates only the child process group it created.
Higher-level integration beads consume the stable evidence shape rather than
re-implementing subprocess watchdog logic.

## Contract

Records distinguish clean exits from `timed_out`, `spawn_failed`,
`parse_failed`, `stale_source`, `malformed_store`, and `blocked` runs. The
`policy` block decides whether a higher-level surface can continue degraded or
must fail closed. Best-effort coordination probes normally use
`continue_degraded`; remote-required verification and mutation guards use
`fail_closed` when the source is mandatory.

Command identity is stored through `display`, `argv`, `commandHash`, and
`normalizedArgvHash`. `argvRedaction` must be `literal_safe`, `redacted`, or
`hash_only`; agents must not store shell strings that are unsafe to paste.

## Redaction

The v1 contract forbids raw mail bodies and raw environment dumps:
`redaction.rawBodiesIncluded` and `redaction.rawEnvIncluded` are both constant
`false`. Output tails are bounded by `output.tailBytesMax`, may be null, and
must be redacted before storage. Local paths should use `labels_only`,
`redact_home`, or `hash_paths` unless a path is already public and harmless.

`exit.killedPeerProcesses` is constant `false`. A watchdog may terminate only
the child process it spawned, never unrelated peer agent processes.

## Degraded And Recovery

`degraded[].severity` uses the canonical six-tier vocabulary:
`info < low < warning < medium < high < critical`.

`recovery[]` mirrors the structured recovery style used by error envelopes:
each action has `priority`, `kind`, `command`, and `message`. Mutating repair
commands, such as Agent Mail database repair, must be represented as
`repair_substrate_after_approval` until explicit human approval exists.

## Fixture

The canonical example in
`tests/fixtures/swarm_schemas/all_examples.json` covers a malformed Agent Mail
backing store. It records the coordination fallback without raw inbox bodies,
raw database pages, secrets, or full environment data.

## Non-goals

- Do not replace Agent Mail, Beads, BV, CASS, RCH, or the agent harness.
- Do not run Cargo locally when the project requires remote verification.
- Do not repair coordination substrates or mutate trackers while recording
  evidence.
- Do not store raw mail bodies, memory content, secrets, raw database pages, or
  full environment dumps.
- Do not treat best-effort coordination evidence as proof that a mandatory
  verification gate passed.
