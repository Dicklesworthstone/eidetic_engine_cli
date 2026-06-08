# Trauma-Guard Bypass-Evidence Loop (the high-precision half)

> Epic `bd-1n0np.18`. The trauma guard already **looks up** risk / anti-pattern /
> failure memories for risky commands (`ee preflight check|guard`), but it never
> **learned** from how a human actually resolved a halt. This adds only the
> high-precision, human-confirmed signal — and **explicitly drops** the
> confound-prone "allowed-then-damaging" auto-detector.

## The loop

1. **Halt** — the guard policy-denies a risky command (`preflight.halt` audit
   event, carrying the `command_hash`).
2. **Human bypass** — a human issues a one-shot bypass token for the **exact same
   command** and runs it (`preflight.bypass` audit event, same `command_hash`).
3. **Correlate** (`bd-1n0np.18.1`, `core::trauma_guard::correlate_bypass_evidence`)
   — read the two audit streams and match a halt with a subsequent bypass for the
   **exact** `command_hash` within an explicit window
   (`BYPASS_EVIDENCE_CORRELATION_WINDOW_SECONDS` = 3600 s), greedily one-shot
   (each bypass resolves at most one halt). Output: ranked
   `CommandBypassEvidence { command_hash, correlated_bypass_count,
   last_bypass_at_epoch }`. Pure + deterministic.
4. **Propose** (`bd-1n0np.18.2`, `propose_calibration_candidate`) — turn evidence
   into a **pending** curate candidate (`CreateDerivedMemory`, source-type
   `trauma_guard_bypass_evidence`) recording that this exact command was
   policy-denied then human-bypassed N times. `ee preflight learn --dry-run`
   previews it; `--apply` routes it through curate's existing
   propose → validate → apply (ADR-0014).
5. **Calmer cited prompt** — once accepted, the guard cites the audited bypass
   evidence so the next prompt for that command class is calmer + cited, reducing
   guard fatigue **without weakening safety**.

## Safety invariants

- **Exact-command only.** Correlation keys on `command_hash`; a bypass for a
  different command never calibrates another.
- **Human-confirmed only.** Evidence requires a real `preflight.bypass`. The
  guard never infers calibration from an *allowed* command, and the
  allowed-then-damaging auto-detector is **out of scope** (dropped in the duel as
  confound-prone).
- **Never auto-permanent.** Calibration candidates are `pending` until an explicit
  `ee curate accept`; the bypass override itself stays **one-shot** — there is no
  auto-generated permanent allowlist.
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
