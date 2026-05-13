# Migrating from `ee` v0.1 to v0.2 (bd-17c65.11.5 / K5)

> **Audience.** Agent-harness authors and integration code consuming
> `ee`'s `--json` output. Human users of the `ee` CLI surface should
> read [`docs/migration-guide.md`](./migration-guide.md) for the
> per-command behavior changes; this document is the **schema and wire
> contract** delta you need to update against.

This guide documents every wire-format change the v0.2 milestone
makes against v0.1. Per AGENTS.md the project does **not** ship a
permanent backward-compatibility shim; agents pinned to v0.1 must
update before consuming a v0.2 binary. Where we offer a one-release
transition window, it is called out explicitly per surface and
gated by an env var so no consumer trips over it accidentally.

The companion test [`tests/migration_v0_2_unit.rs`](../tests/migration_v0_2_unit.rs)
enforces this guide: removing a guide section without retiring the
corresponding v1 surface fails CI, and adding a new v1 string to
`src/` without a guide entry also fails CI.

---

## Schema versions

| Envelope | v0.1 | v0.2 | Status |
|---|---|---|---|
| Response envelope (success path) | `ee.response.v1` | `ee.response.v1` | **Unchanged at the envelope level.** Only additive field changes within v1 — permissive consumers are forward-compatible. |
| Error envelope | `ee.error.v1` | `ee.error.v2` | **Breaking.** `error.details.recovery[]` added as structured array (F1); `error.nonRecoverable?: bool` added. See [A10](#a10--error-envelope-v1--v2). |
| Pack object inside response | `ee.pack.v1` | `ee.pack.v2` | **Breaking.** See [A1 phase 2](#a1-phase-2--collapse-selectioncertificate--provenancefooter-into-items). |
| Hook contract | `ee.hook.context_pack.v1` | `ee.hook.context_pack.v1` | **Unchanged.** E2's filter applies; no field shape changes. |
| Other envelopes (`ee.memory.list.v1`, `ee.rule.list.v1`, `ee.search.v1`, `ee.handoff.capsule.v1`, …) | v1 | v1 | **Unchanged at envelope level.** Some field-level renames; see per-surface sections. |

The conservative bumping strategy means most agents see additive
changes within v1 envelopes; only a hard switch for the error
envelope and the pack object is required.

---

## Breaking changes by bead

Each section lists the bead ID, the surface affected, a `Before` /
`After` JSON example, the agent-rewrite recipe, and whether
`ee migrate run` is sufficient to handle the data-side of the
migration.

### D1 — canonical `content` field across list/preview surfaces

**Bead.** bd-17c65.4.1

**Surfaces.** `ee memory list`, `ee rule list`, `ee learn uncertainty`, `ee why`.

**Before (v0.1).**
```json
{"data":{"memories":[{"id":"mem_…","content_preview":"Run cargo fmt…","level":"procedural"}]}}
```

**After (v0.2).**
```json
{"data":{"memories":[{"id":"mem_…","content":"Run cargo fmt…","content_truncated":false,"level":"procedural"}]}}
```

**Agent rewrite.**
```python
# v0.1
preview = response["data"]["memories"][0]["content_preview"]

# v0.2
mem = response["data"]["memories"][0]
content = mem["content"]
if mem.get("content_truncated", False):
    full = subprocess.check_output(["ee", "memory", "show", mem["id"], "--json"])
```

**Migration tool.** Not required — D1 is a read-side rename. No on-disk schema changes.

**Verification.** `tests/contracts/canonical_content_field.rs` asserts every list/preview surface uses `content` + `content_truncated` and that `content_preview` is **gone** as a JSON key in the emitted output.

---

### E1 — `posture` enum replaces `healthy: bool` in doctor + status

**Bead.** bd-17c65.5.1

**Surfaces.** `ee doctor --json`, `ee status --json`.

**Before (v0.1).**
```json
{"data":{"healthy":true,"checks":[…]}}
```

**After (v0.2).**
```json
{"data":{"posture":"ok","healthy":true,"checks":[…]}}
```

The `healthy: bool` field is **kept for the v0.2 transition release** so agents pinned to it do not crash mid-migration. Read `posture` for new code; the mapping is:

| `posture` | implied `healthy` |
|---|---|
| `"ok"` | `true` |
| `"degraded_recoverable"` | `false` (no operator action required) |
| `"blocked"` | `false` (operator action required) |

In v0.3 the `healthy` field is removed; `tests/migration_v0_2_unit.rs::healthy_field_pending_v0_3_removal` tracks the residue (currently 5 emission sites in `src/output/mod.rs`) and flips green once they are cleaned up.

**Agent rewrite.**
```python
# v0.1
if response["data"]["healthy"]:
    proceed()

# v0.2 (preferred; v0.3 enforced)
match response["data"]["posture"]:
    case "ok": proceed()
    case "degraded_recoverable": warn_and_proceed()
    case "blocked": abort_with_repair_hint(response["data"].get("repair"))
```

**Migration tool.** None required — pure read-side.

---

### E2 — conditional banner emission for `degraded[]`

**Bead.** bd-17c65.5.2

**Surfaces.** Every command that emits `data.degraded[]` (most prominently `ee context`).

**Before (v0.1).** `data.degraded[]` always populated with every build-time gap and workspace-state condition, plus a meta `degraded_context` summary code. Agents trained to ignore the banner because it always fired.

```json
{"data":{"degraded":[
  {"code":"degraded_context","message":"2 degraded context signals present"},
  {"code":"graph_snapshot_unimplemented","message":"…"},
  {"code":"search_index_stale","message":"…"}
]}}
```

**After (v0.2).** `data.degraded[]` filters out non-affecting signals by default — only entries whose category is `AffectsThisResponse` survive. The meta `degraded_context` code is **deleted**. Agents seeing `degraded: []` can infer the response was unaffected by any degradation.

```json
{"data":{"degraded":[]}}
```

When a signal **does** affect the current response, it surfaces as before:
```json
{"data":{"degraded":[{"code":"no_relevant_results","severity":"medium","message":"…"}]}}
```

**Verbose mode.** Pass `--include-non-affecting-degradations` for the pre-E2 verbose behavior:
```bash
ee context "prepare release" --workspace . --include-non-affecting-degradations=true --json
```

**Agent rewrite.**
```python
# v0.1
# Ignore degraded[] because it's always non-empty.

# v0.2
for d in response["data"].get("degraded", []):
    # Every entry here affects the current response; do not ignore.
    handle_repair(d.get("repair"), d["code"])
```

**Migration tool.** None — pure read-side filter.

**Verification.** `tests/diagnostics_banner_categorization_unit.rs`, `tests/diagnostics_banner_emission_unit.rs`, `tests/diagnostics_banner_aliasing_unit.rs` (16 tests total), plus `scripts/e2e_overhaul/diagnostics_honesty.sh epic_E_banner_emission`.

---

### E3 — `graph_snapshot` → `graphCompute` + `graphSnapshotArtifact`

**Bead.** bd-17c65.5.3

**Surfaces.** `ee status --json`.

**Before (v0.1).**
```json
{"data":{"status":{"graph_snapshot":{"status":"unimplemented","message":"…"}}}}
```

**After (v0.2).** Two separate concerns get their own fields.
```json
{"data":{
  "graphCompute":{"status":"available","liveComputeSupported":true},
  "graphSnapshotArtifact":{"status":"empty","memoryGraph":{"edgeCount":0,"availability":"live_compute_available"}}
}}
```

**Agent rewrite.** Read `status.graphCompute.status` for "can I run an on-demand algorithm" and `status.graphSnapshotArtifact.status` for "is there a persisted snapshot".

**Migration tool.** None — pure read-side rename.

---

### A1 phase 2 — collapse `selectionCertificate.*` + `provenanceFooter` into `items[]`

**Bead.** bd-17c65.1.1 (A1) phase 2 (commit bd-2pe1z).

**Surfaces.** `ee context --json`, `ee pack build --json`.

**Before (v0.1).** Three parallel arrays carried per-item data:
- `data.pack.items[]` — basic fields
- `data.pack.selectionCertificate.selected_items[]` — feasibility, marginal gain
- `data.pack.selectionCertificate.steps[]` — selection trace
- `data.pack.provenanceFooter.entries[]` — provenance schemes per rank

An agent reading `items[i]` had to chase three more arrays by rank.

**After (v0.2).** Each `pack.items[i]` carries the union of fields (rank, memoryId, section, content, estimatedTokens, scores, marginalGain, objectiveValue, feasibility, sourceIndex, provenance). The legacy arrays are **dropped**.

```json
{"data":{"pack":{
  "items":[{
    "rank":1,
    "memoryId":"mem_…",
    "section":"procedural_rules",
    "content":"…",
    "estimatedTokens":42,
    "scores":{"relevance":0.85,"utility":0.7,"marginalGain":0.12},
    "feasibility":"selected",
    "sourceIndex":0,
    "provenance":[{"scheme":"ee-mem","uri":"ee-mem://mem_…"}]
  }],
  "selectionCertificate":{
    "algorithmId":"mmr_with_coverage_fill_v1",
    "algorithmDescription":"Two-pass MMR with coverage fill",
    "guaranteeStatus":"empirical"
  },
  "provenanceFooter":{"memoryCount":1,"sourceCount":1,"schemes":["ee-mem"]}
}}}
```

`selectionCertificate` is retained as a top-level field carrying `algorithmId` + `algorithmDescription` + `guaranteeStatus`. Its sub-arrays are gone. `provenanceFooter` is retained as a summary; its `entries[]` array is gone.

**Agent rewrite.**
```python
# v0.1
selected = response["data"]["pack"]["selectionCertificate"]["selected_items"]
gain_by_rank = {s["rank"]: s["marginalGain"] for s in selected}
prov_by_rank = {p["rank"]: p["provenance"]
                for p in response["data"]["pack"]["provenanceFooter"]["entries"]}

# v0.2
for item in response["data"]["pack"]["items"]:
    rank = item["rank"]
    gain = item.get("scores", {}).get("marginalGain")
    prov = item.get("provenance", [])
    feasibility = item.get("feasibility")
```

**Migration tool.** None — pure read-side restructure.

---

### A4 — `pack.text` rendered Markdown body inline

**Bead.** bd-17c65.1.4

**Surfaces.** `ee context --json`.

**Before (v0.1).** Agents had to parse `items[]` to build a prompt fragment, or run `ee context --format markdown` separately.

**After (v0.2).** `data.pack.text` carries the rendered Markdown body that an agent prepends directly to its LLM prompt.

**Agent rewrite.** Use `pack.text` first, fall back to building from `items[]` if absent for forward-compat. See `scripts/agent_consume_pack.py` for a 42-SLoC reference consumer.

**Opt out.** Pass `--no-rendered-text` to suppress `pack.text` emission.

**Migration tool.** None — pure read-side addition.

---

### A10 — error envelope `ee.error.v1` → `ee.error.v2`

**Bead.** bd-17c65.4.7

**Surfaces.** Every command that emits an error envelope.

**Before (v0.1).**
```json
{"schema":"ee.error.v1","error":{"code":"search_index_stale","message":"…","repair":"ee index rebuild --workspace ."}}
```

**After (v0.2).**
```json
{"schema":"ee.error.v2","error":{
  "code":"search_index_stale",
  "message":"…",
  "severity":"medium",
  "repair":"ee index rebuild --workspace .",
  "details":{
    "recovery":[
      {"priority":1,"kind":"command","command":"ee index rebuild --workspace ."},
      {"priority":2,"kind":"env","envName":"EE_INDEX_DIR","envValue":"<absolute path>"}
    ]
  },
  "nonRecoverable":false
}}
```

**Agent rewrite.**
```python
# v0.1
human_action = parse_freeform(response["error"].get("repair", ""))

# v0.2
recovery_actions = sorted(response["error"]["details"].get("recovery", []),
                          key=lambda r: r["priority"])
for action in recovery_actions:
    if action["kind"] == "command":
        run(action["command"])
        break
    elif action["kind"] == "env":
        os.environ[action["envName"]] = action["envValue"]
```

**Forward-compat.** A v0.1 consumer that ignores unknown fields keeps working against v0.2 because the recovery struct goes into `details`. A v0.1 consumer with `additionalProperties: false` in its own JSON Schema breaks; update the consumer's schema first.

---

### B1 — relevance floor + `no_relevant_results` degraded entry

**Bead.** bd-17c65.2.1

**Surfaces.** `ee search --json`, `ee context --json`.

**Before (v0.1).** Search returned every hit regardless of score. Agents saw long noise tails.

**After (v0.2).** A default relevance floor (0.05 for single-arm sources, 0.005 for hybrid/RRF — see bd-n22a4) drops below-floor hits. When every hit was below floor:
```json
{"data":{"results":[],"degraded":[
  {"code":"no_relevant_results","severity":"medium",
   "message":"All N candidates scored below floor 0.0500. Top candidate scored 0.0123.",
   "repair":"Lower --relevance-floor, rephrase the query, or check `ee status` for stale indexes."}
]}}
```

**Agent rewrite.** Treat empty `results[]` + `no_relevant_results` in `degraded[]` as "no signal" — distinct from "no memories in the workspace". Optionally retry with `--relevance-floor 0`.

**Migration tool.** None — read-side semantic change.

---

### C1 + C3 — secret detector + tag validator overhaul

**Beads.** bd-17c65.3.1 (C1), bd-17c65.3.3 (C3).

**Surfaces.** `ee remember` content rejection, tag validation.

**Before (v0.1).**
- Secret detector: substring keyword match. Memories *mentioning* "secret", "token", "password" in English were rejected.
- Tag validator: `^[a-z0-9-]+$` — no dots, no underscores, no Unicode.

**After (v0.2).**
- Secret detector: **value-shape match.** Recognizes actual API key patterns (sk-…, AKIA…, ghp_…, BEGIN PRIVATE KEY) by their structural shape, not by keyword. English text mentioning "the secret detector" works fine.
- Tag validator: `^[a-zA-Z0-9._:-]+$` after NFC normalization. Dots (`v0.1.0`), underscores (`policy_release`), Unicode (`café`) all accepted.

**Agent rewrite.** Many memories that were rejected by v0.1 now land cleanly. No agent-side change needed; just retry the rejected ones.

**Migration tool.** None — read-side semantic change.

---

### F1 — structured `error.details.recovery[]` actions

**Bead.** bd-17c65.6.1

Already covered as part of [A10](#a10--error-envelope-v1--v2). The recovery struct is the v2 error envelope's signature addition.

---

### H1 — spec-minimal Markdown escape policy

**Bead.** bd-17c65.8.1

**Surfaces.** `ee context --format markdown`, every renderer that escapes string content into Markdown.

**Before (v0.1).** Aggressive escaping. `v0.2.0` rendered as `v0\.2\.0`. `policy.detector.value` rendered as `policy\.detector\.value`. Output looked like noisy code.

**After (v0.2).** Spec-minimal escaping per CommonMark — only characters that would actually produce different rendered output get escaped:
- `## Heading` at line start → `\## Heading` (escaped)
- `# include` mid-line → unescaped (not an ATX heading)
- `v0.2.0` → unescaped (dot after digit mid-line is not a list marker)
- `policy.detector.value` → unescaped (intra-word dot)
- `mem_01HQ3K5Z` → unescaped (intra-word underscore)

**Agent rewrite.** None required if you strip Markdown formatting on the consumer side. Markdown-rendering consumers see cleaner prose now.

**Migration tool.** None — read-side change.

---

## Schema version cross-reference

| Old schema | New schema | Coverage |
|---|---|---|
| `ee.error.v1` | `ee.error.v2` | A10 |
| `ee.pack.v1` (inside data.pack) | `ee.pack.v2` | A1 phase 2, A4 |
| `ee.response.v1` | `ee.response.v1` (unchanged) | — |
| `ee.hook.context_pack.v1` | `ee.hook.context_pack.v1` (unchanged) | — |

---

## Env vars

`EE_CASS_BINARY` was honored in v0.1 but undocumented. Now in [`docs/env_vars.md`](./env_vars.md). The full env var registry ships at v0.2; see K4 (bd-17c65.11.4) for the canonical list.

---

## Things that did **NOT** change

- `data.success` field on the envelope stays a bool.
- Memory `id` format (`mem_<26-char-ULID>`) is unchanged.
- Workspace identity tuple is unchanged.
- Audit log row shape is unchanged.
- `--format` accepted values (`json | markdown | toon | jsonl | compact | hook | mermaid | human`) are unchanged. H1 + H2 change the Markdown content, not the format list.

---

## Test coverage

- [`tests/migration_v0_2_unit.rs`](../tests/migration_v0_2_unit.rs) — CI enforcement: forbidden-list residue check, guide structure check, CHANGELOG cross-reference, forbidden-list duplicate check.
- [`tests/contracts/canonical_content_field.rs`](../tests/contracts/canonical_content_field.rs) — D1.
- [`tests/diagnostics_banner_categorization_unit.rs`](../tests/diagnostics_banner_categorization_unit.rs), [`tests/diagnostics_banner_emission_unit.rs`](../tests/diagnostics_banner_emission_unit.rs), [`tests/diagnostics_banner_aliasing_unit.rs`](../tests/diagnostics_banner_aliasing_unit.rs) — E2.
- [`tests/contracts/schema_canonical_fields.rs`](../tests/contracts/schema_canonical_fields.rs) — D4 schema-drift gate.
- [`tests/contracts/retrieval_field_naming.rs`](../tests/contracts/retrieval_field_naming.rs) — retrieval JSON style.

---

## Reporting issues

If a migration step in this guide doesn't match your observed behavior on a v0.2 binary, the gap is either (a) a bug in the implementation, (b) a stale section in this guide, or (c) a bead that closed without purging its v1 string from `src/`. File a follow-up bead referencing this section by anchor; the migration test points at this document so the guide-versus-impl drift is detectable in CI.
