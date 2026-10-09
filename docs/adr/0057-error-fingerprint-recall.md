# ADR 0057: Error Fingerprint Recall

Status: proposed
Date: 2026-06-07
Bead: bd-1n0np.4.1

## Context

The single most repeated event in a coding agent's life is hitting a
compiler/clippy/test/RCH error. "Have we seen this exact blocker, what repair
actually worked, which repair was harmful, what command proved the fix?" is the
highest-frequency, highest-value question durable memory can answer — and on this
repo (Rust, clippy-as-errors, RCH, golden tests) it fires constantly. `ee`
already stores memories, CASS evidence, outcomes, provenance, degraded codes, and
proof expectations; what is missing is a focused retrieval lens over exactly that
data. The 2026-06-07 review scored this 785 and flagged it the highest *daily*
agent utility, with the key insight that consuming structured diagnostics makes
it robust and deterministic.

## Decision

Add a deterministic error-fingerprint subsystem and a recall surface.

- `ErrorFingerprint { tool, canonical_code, message_template_signature,
  location_shape, stderr_simhash, version_hints }`; `ErrorRecallReport { exact,
  near, helpful_repairs, harmful_repairs, proof_links, stale/version warnings }`.
- **Consume structured diagnostics, not stderr regex**: `cargo build
  --message-format=json` / `rustc --error-format=json`, `ee.error.v2` codes, RCH
  blocker code+stage, shell exit+first stable line.
- **Layered key**: primary `(tool, canonical_code)` exact (E0277,
  `clippy::needless_borrow`, exit code, `ee.error.v2` code) so the common case
  needs **no** fuzzy matching; secondary message template; tertiary simhash.
- Storage respects the architecture: `ErrorFingerprint` rows in FrankenSQLite
  (truth) + derived Frankensearch documents; links `fingerprint → repair →
  proof → outcome` via `memory_links` + graph.
- Surfaces: `ee diagnose-error --stdin | --tool <t> --code <c>` (exact →
  normalized → semantic → graph) and `ee pack "fix this" --error-log err.json`
  (seed the pack query with the fingerprint → smallest pack with the prior
  working repair + verifying command + harmful-repair warning).
- **No tool execution** (diagnoses text it is handed); **redaction-by-default**
  (store fingerprints + redacted spans, not raw logs).

This is mutually reinforcing with the Evidence Harvester (ADR 0055): an error-fix
is the least-ambiguous outcome label in the system, since the cited error
literally disappears on the next build.

### Bounded interactive pack recall

`pack --error-log` uses a dedicated bounded query seed. A common error class
can have thousands of historical repairs; enumerating its complete report and
loading every repair before packing would make interactive work grow with that
history. The pack reader considers at most 32 helpful repair targets in stable
target-ID order through the existing fingerprint index, and hydrates at most
that many native evidence rows. It prefers admitted incident cards within that
window, deduplicates text, and includes at most four repair excerpts. Each
excerpt is bounded to 768 UTF-8 bytes and the complete seed to 4096 bytes.
Truncation preserves UTF-8 boundaries and is explicit. These are bounded query
hints; ordinary retrieval and packing still decide which source items fit.

Only live evidence admitted for direct packing contributes text. Valid native
memory IDs can remain bounded query hints without exposing unvalidated memory
bodies. The pack path does not load proof rows whose text cannot be displayed,
nor enumerate the full warning and outcome history. `diagnose-error` retains
the complete report for explicit inspection. The bounded window is
deterministic; it does not claim to identify the latest or globally best repair.

## Consequences

- **Easier**: agents recall the exact repair that held for a recurring failure
  family, with provenance; humans get local "how we fixed this last time."
- **Guarded**: normalization is the dedup knife-edge — contained by exact-code
  matching as the primary path (fuzzy is only the long-tail fallback) and by
  redaction defaults for log privacy.
- **Intentionally impossible**: no command execution; no pack-ranking change
  unless the user passes `--error-log`.

## Rejected Alternatives

- **Stderr regex scraping**: brittle and non-deterministic; rejected for
  structured-diagnostic ingestion.
- **Pure semantic matching**: collapses unrelated errors / misses near-matches;
  rejected for the layered exact→template→simhash key.
- **Storing raw logs**: privacy risk; rejected for fingerprints + redacted spans.

## Verification

- Unit + property/fuzz tests (bd-1n0np.4.7): canonicalization idempotency, no
  collisions across distinct codes, redaction completeness, link-walk resolution,
  fuzz the canonicalizer (no panic).
- e2e `scripts/e2e_error_recall.sh`: a known rustc JSON error → exact recall of
  working repair + harmful warning + verifying command; `--error-log` smallest
  pack; no raw log persisted.
- Ingestion of external log text routes through the injection guard + redaction
  (bd-1n0np.23.3) before storage.
