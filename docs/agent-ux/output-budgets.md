# Output budgets: fields, ceilings, and cursors

`ee` gives machine consumers three independent controls over response
size (ADR 0063). They compose, and each answers a different question:

| Layer | Control | Question it answers |
|---|---|---|
| Shape | `--fields <preset>` | *Which* fields does the consumer need? |
| Size | `--max-output-tokens <N>` / `EE_MAX_OUTPUT_TOKENS` | *How much* output can the consumer afford? |
| Continuation | `--cursor <token>` | *Where* does the next page start? |

Precedence: the fields projection is applied first, then the governor
sizes what remains. The `--max-output-tokens` flag wins over the
`EE_MAX_OUTPUT_TOKENS` env mirror when both are set; unset or
unparseable values leave output ungoverned (the zero-cost path returns
the envelope byte-identical and unstamped).

Feature detection: `ee capabilities --json` advertises the whole
contract at `data.output.governor` — availability, the flag/env names,
the cursor schema, the four degraded codes, and the full truncation
point table — so a harness never needs to hardcode this document.

## What the governor does

When a ceiling is set on an `ee.response.v2` envelope, the governor
estimates the serialized response (the same cl100k BPE encoder used by
pack budgeting), and if it exceeds the ceiling, drops trailing whole
elements at the response's single declared truncation point until the
page fits. Governed responses always carry `meta.tokensEstimated`.

Three outcomes are possible, and all are honest:

1. **Fits** — the envelope is emitted whole and stamped.
2. **Truncated** — trailing elements are dropped; a degraded entry
   `output_truncated_budget` (severity `info`) reports
   `details.droppedCount` and `details.continuationCursor`.
3. **Unsatisfiable** — even the envelope minimum (shell + one element)
   exceeds the ceiling; the response fails closed to a minimal shell
   with `output_budget_unsatisfiable` (severity `medium`), no
   elements, and no cursor. Raise the ceiling or narrow `--fields`.

Non-envelope output — human text, raw schema dumps, NDJSON stream
frames, and top-level reports such as `ee.audit.timeline.v1` — passes
through the governor unchanged and unstamped. The audit timeline pages
through its own query-level `--cursor` lane on the same `ee.cursor.v1`
codec instead.

## Truncation points

Each governed schema declares exactly ONE array whose trailing whole
elements may be dropped. Everything else in the envelope is never
touched. The registry (`OUTPUT_TRUNCATION_REGISTRY`, mirrored at
`data.output.governor.truncationPoints` in `ee capabilities --json`):

| Surface | Schema | Truncation point | Position key |
|---|---|---|---|
| `search` | `ee.search.v1` | `data.results[]` | `docId` |
| `memory list` | — | `data.memories[]` | `id` |
| `insights` | `ee.insights.v1` | `data.sections[].items[]` (round-robin) | `id` |
| `curate candidates` | `ee.curate.candidates.v1` | `data.candidates[]` | `id` |
| `audit timeline` | `ee.audit.timeline.v1` | `data.entries[]` | `id` |
| `pack` | `ee.pack.v2` | `data.pack.skipped[]` | `id` |
| `recall` | `ee.recall.v1` | `data.recall.items[]` | `memoryId` |
| `journal list` | — | `data.entries[]` | `entryId` |
| `schema list` | — | `data.schemas[]` | `id` |

Hard rule: **pack `data.pack.items[]` is never a truncation point.**
Pack content is governed solely by its own `--max-tokens` retrieval
budget; the governor may only trim the envelope-side `skipped[]`
metadata. The contract suite pins this
(`pack_items_are_never_a_registered_truncation_point`).

The per-section shape (`insights`) drops items round-robin from the
last section backwards, so every section keeps proportional coverage;
whole sections are never deleted.

A list-like schema that declares no truncation point fails closed to
`output_budget_unsatisfiable` rather than guessing — and the drift
contract (`tests/contracts/governor_truncation_registry.rs`) fails CI
when a new list-like schema ships without either a registry entry or a
documented exemption.

## Estimator tolerance

`meta.tokensEstimated` is converged to a fixed point (the stamped
digits themselves contribute tokens). When convergence does not settle
within the iteration bound, the last measurement is stamped and the
true estimate of the emitted bytes may differ by a few tokens — treat
the stamp as accurate to ±1% rather than exact. Two guarantees do not
degrade: a non-fail-closed governed response never exceeds its ceiling
estimate, and a byte backstop additionally caps the serialized page at
`ceiling × 8` bytes against estimate-evading payloads.

## Cursor lifecycle

Cursors are opaque `ee.cursor.v1` tokens:
`base64url(payload).base64url(blake3_mac)`, MAC-keyed per workspace.
The payload binds the target schema, the workspace DB generation, the
normalized invocation parameters (hashed), the last-emitted position
key, and `droppedCount` — the resume basis that stays unambiguous even
for round-robin shapes. Cursors never embed query text or memory
content.

```bash
page1=$(ee search "release gates" --workspace . --max-output-tokens 500 --json)
cursor=$(jq -r '[.degraded[] | select(.code=="output_truncated_budget")
                 | .details.continuationCursor][0]' <<<"$page1")
page2=$(ee search "release gates" --workspace . --max-output-tokens 500 \
          --cursor "$cursor" --json)
```

Rules a consumer can rely on:

- **Pages partition one generation.** Drained to exhaustion (follow
  `continuationCursor` until absent), the pages reproduce the full
  result set exactly once: no duplicates, no gaps, order preserved.
  Page sequences are byte-deterministic, including the cursors.
- **Same invocation only.** A cursor resumes the same command with the
  same parameters in the same workspace. Any mismatch — edited query,
  different flags, another workspace, a tampered byte — rejects as
  `cursor_invalid`.
- **Writes invalidate pages.** If the workspace DB generation advances
  mid-sequence (any memory write), the next resume rejects as
  `cursor_stale`. Re-run without `--cursor` to start a fresh sequence
  against the new generation.
- **Rejection is an empty page, never a restart.** A rejected cursor
  empties the truncation point, appends the `cursor_invalid` /
  `cursor_stale` degraded entry, and offers no continuation. A
  consumer that blindly concatenates pages can therefore never
  double-count: restarting is the consumer's explicit choice.

## Degraded codes

| Code | Severity | Meaning | Repair |
|---|---|---|---|
| `output_truncated_budget` | `info` | Trailing elements dropped to fit the ceiling | Resume with `--cursor`, or raise the ceiling |
| `output_budget_unsatisfiable` | `medium` | Envelope minimum exceeds the ceiling | Raise the ceiling or narrow `--fields` |
| `cursor_stale` | `info` | DB generation advanced since the cursor was issued | Re-run without `--cursor` |
| `cursor_invalid` | `low` | MAC/schema/params mismatch or malformed token | Re-run without `--cursor` |

Full taxonomy entries: [`docs/degraded_codes.md`](../degraded_codes.md).

## Deliberate non-adoption: swarm brief

`ee swarm brief` does not take `--max-output-tokens`. Its output
economy is the `--fields summary|minimal|full` preset family
(bd-kua65): the brief is a coordination posture snapshot whose
sections lose meaning when element-truncated, so shape selection — not
size truncation — is its budget control. Revisit only if the bd-kua65
preset work concludes a ceiling lane is still needed.

## Verification surfaces

The contract is pinned at four layers: engine property tests
(`tests/property_output_governor.rs`), per-surface real-binary
contracts and shape goldens (`tests/contracts/governor_surfaces.rs`),
the registry drift assert
(`tests/contracts/governor_truncation_registry.rs`), the cross-process
byte-identity lane (`tests/determinism_unit.rs`), and the end-to-end
sweep `scripts/e2e_output_governor.sh` (verify.sh gate 6.055).
