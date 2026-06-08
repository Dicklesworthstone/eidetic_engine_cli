# Telescoping Level-of-Detail (LOD) Packing

LOD packing raises information density at the token-budget wall. Instead of
hard-dropping the Nth memory when the budget is exhausted, `ee pack` renders
selected candidates at three levels of detail and fills each tier by a
configurable share of the budget:

| Tier | Rendering | Default budget share |
|---|---|---|
| **Full** | complete memory content | ~70% |
| **Truncated preview** | deterministic *extractive* prefix of the content + ` ...` | ~20% |
| **Link-only (peripheral index)** | a `Memory <id>` stub the agent can expand with `ee memory show <id>` | ~10% |

Bead lineage: `bd-1n0np.5` (feature), `5.1` (tiered partition + budget-share
fill), `5.2` (deterministic previews + exact token accounting + config knob),
`5.3` (markdown/json/toon rendering of the peripheral index), `5.4` (tests),
`5.5` (`scripts/e2e_lod_packing.sh`).

## How it works inside `ee`

The MMR / facility-location selector ranks the full candidate pool unchanged.
LOD then partitions the selected/eligible candidates and fills by tier
(`src/pack/mod.rs`):

- `PackLodBudgetShares` holds the per-tier basis-point shares (default
  `default_70_20_10()`); `PackLodBudgetState` tracks per-tier token usage.
- `pack_lod_candidate_plan` walks Full → Truncated_Preview → Link_Only and emits
  the first tier whose rendered candidate fits the remaining tier budget.
- `candidate_for_lod_tier` routes to `preview_lod_candidate`
  (`truncated_preview_content` — a deterministic word-prefix, never generated) or
  `link_only_lod_candidate` (the `Memory <id>` stub).

When all selected candidates fit the Full tier (the common case), every item is
Full and output is byte-identical to pre-LOD packs — so existing pack goldens are
unchanged.

## Two hard invariants

1. **Previews are deterministic and EXTRACTIVE, never generated.**
   `truncated_preview_content` returns a strict word-prefix of the source
   (+ ` ...`); the same content + limit always yields the same preview. No
   abstractive summarization — that would break reproducibility and pack hashes.
2. **Pack hash stays byte-stable.** Token accounting across the three tiers is
   off-by-one-free; the pack hash reproduces across identical runs. The
   determinism contract (`docs/agent-ux/float-determinism.md`) holds with LOD on.

## Rendering the peripheral index

- **Markdown** (`ee pack --format markdown`): link-only items render in a
  dedicated `## Peripheral Index` section (id + section + drill-in hint), not
  inline with full/preview content. Full/preview items render normally.
- **JSON / TOON**: peripheral rendering is additive and gated on a deliberate
  `ee.pack.v2` schema field (the pack-item object is `additionalProperties:
  false`), so it lands with a schema bump + the schema-drift gate rather than a
  free-form field.

JSON/TOON consumers can already distinguish a link-only item by its content
signature: `content == "Memory <id>" || content == "<id>"`.

## Configuration

| Surface | Effect |
|---|---|
| `[pack] lod_full_basis_points` / preview / link basis points | Override the 70/20/10 tier shares (must sum to ≤ 10000; all-zero degrades to full-only). |
| `--no-lod` (or a flat profile) | Disable LOD and reproduce the legacy flat single-tier pack byte-for-byte. |

Keep the default at 70/20/10 so existing pack goldens stay byte-identical when
the knob is absent.

## Verifying

- Unit: `lod_budget_shares_*`, `lod_candidate_plan_fills_full_preview_and_link_tiers`,
  `lod_preview_is_deterministic_extractive_and_off_by_one_free`,
  `lod_link_only_candidate_is_deterministic_and_within_budget`,
  `link_only_pack_items_classify_for_peripheral_index`,
  `markdown_render_routes_link_only_items_to_peripheral_index` (`src/pack/mod.rs`).
- Integration: `tests/lod_packing_e2e.rs` (full+preview tiers + pack-hash
  stability) and `scripts/e2e_lod_packing.sh` (real-binary, wired into
  `scripts/verify.sh` gate 6.10).

## See also

- [`adaptive-pack-budget.md`](adaptive-pack-budget.md) — how the token budget LOD partitions is computed.
- [`why-not.md`](why-not.md) — link-only items have a clear, explainable exclusion-from-full reason.
