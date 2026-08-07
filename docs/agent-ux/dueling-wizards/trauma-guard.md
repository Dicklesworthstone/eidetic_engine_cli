# Historical Trauma-Guard Evidence Loop

> Epic `bd-1n0np.18`. `ee preflight check|guard` looks up risk, anti-pattern,
> and failure memories for an inspected command. It is advisory-only: `ee`
> never authorizes, denies, suppresses, interrupts, or executes that command.
> Optional token, memory, or audit storage failures are degraded evidence and
> do not change a valid check's exit-zero contract.

Older releases recorded policy-denied halt and bypass events. The correlation
pipeline below remains useful for learning from those historical audit rows,
but current preflight checks do not create new halt events and never require a
bypass token.

## The loop

1. **Historical halt evidence** — an older release recorded a
   `preflight.halt` audit event carrying a `command_hash`.
2. **Historical authorization evidence** — a human recorded a one-shot token
   for the **exact same command** (`preflight.bypass`, same `command_hash`).
3. **Correlate** (`bd-1n0np.18.1`, `core::trauma_guard::correlate_bypass_evidence`)
   — read the two audit streams and match a halt with a subsequent bypass for the
   **exact** `command_hash` within an explicit window
   (`BYPASS_EVIDENCE_CORRELATION_WINDOW_SECONDS` = 3600 s), greedily one-shot
   (each bypass resolves at most one halt). Output: ranked
   `CommandBypassEvidence { command_hash, correlated_bypass_count,
   last_bypass_at_epoch }`. Pure + deterministic.
4. **Propose** (`bd-1n0np.18.2`, `propose_calibration_candidate`) — turn evidence
   into a **pending** curate candidate (`CreateDerivedMemory`, source-type
   `trauma_guard_bypass_evidence`) recording that this exact command
   historically had a halt followed by human authorization N times.
   `ee preflight learn --dry-run`
   previews it; `--apply` routes it through curate's existing
   propose → validate → apply (ADR-0014).
5. **Better cited context** — once accepted, advisory risk lookup can cite the
   audited evidence when the same command class is inspected again.

## Safety invariants

- **Exact-command only.** Correlation keys on `command_hash`; a bypass for a
  different command never calibrates another.
- **No command authority.** Neither historical halt/bypass rows nor a current
  lookup changes whether a shell command runs. That decision belongs to the
  human or harness invoking the shell.
- **Human-confirmed only.** Historical evidence requires a real
  `preflight.bypass`. The correlator never infers calibration from an *allowed* command, and the
  allowed-then-damaging auto-detector is **out of scope** (dropped in the duel as
  confound-prone).
- **Never an allowlist.** Calibration candidates are `pending` until an
  explicit `ee curate accept`; accepted memory changes retrieval context, not
  shell permissions.
- **Deterministic over explicit windows.** No wall-clock heuristics in the
  correlator; same inputs → same evidence.

## Surfaces

| Stage | Surface |
|-------|---------|
| Correlate | `core::trauma_guard::correlate_bypass_evidence` (18.1) |
| Propose | `core::trauma_guard::propose_calibration_candidate` (18.2 core) |
| Apply | `ee curate accept` (existing ADR-0014 pipeline + `CURATION_CANDIDATE_*` audit rows) |
| Learn CLI | `ee preflight learn --dry-run/--apply` (18.2 surface) |

Schema: `ee.trauma_guard.bypass_evidence.v1`.
