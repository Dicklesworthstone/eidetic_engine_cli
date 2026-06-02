# Lexical posting-list RAM-tier pinning

> **Status:** scaffold plus config/failure-fixture contract (bd-1hvzh and
> follow-up slices under bd-21xbi). The public surface, configuration, schema,
> and degraded-code vocabulary documented here are stable. The Linux `mmap` +
> `MAP_POPULATE` + `mlock` / `MADV_HUGEPAGE` syscall path and the wiring into
> the Frankensearch lexical index loader are still tracked under follow-up
> slices of bd-21xbi.

## What this optimization does

On 256GB+ Linux hosts the Frankensearch lexical posting-list files under
`indexes/combined/` are not RAM-tier pinned. The first cold search pays
disk page-fault cost on each posting-list access, and subsequent searches
still compete for page-cache slots with everything else on the host. For
a typical 14k-memory workspace the lexical index is 10–100 MB; on a 100k+
memory workspace it can reach tens of GB.

The RAM-tier pinning loader uses `mmap(MAP_POPULATE | MAP_NORESERVE)` plus
`mlock` (and optionally `madvise(MADV_HUGEPAGE)`) to pre-fault the
lexical index into RAM and hold it there. No search-side code changes;
only the loader path moves. Determinism is preserved because the
optimization only changes wall-clock and page-cache residency — the
search results are byte-identical whether the index is RAM-pinned or
read from disk.

## Distinguishability

This bead is intentionally distinct from three other pinning surfaces:

| Bead | Dataset | Why distinct |
|---|---|---|
| **bd-21xbi** (this one) | Frankensearch lexical posting-list files | Pinned + optionally hugepaged; the dominant cold-path cost on text-heavy search |
| **bd-1prrl.3** (swarmx.4) | Graph snapshot blobs | NUMA-aware (`mbind`); graph-algorithm random-access pattern, not search-style sequential scan |
| **bd-ndzfg** | Assembled `ee.context.v2` pack JSON results | Caches RESULTS keyed on (query, workspace, manifest); plan-cache and result-cache misses still pay the lexical first-touch cost this bead eliminates |
| **bd-168gm** | Embedding vectors (LRU keyed on exact text hash) | Caches embedding vectors; lexical posting lists are an unrelated dataset |

## Host-class requirements

| Host class | Behavior |
|---|---|
| Linux 2-socket (256GB+) with THP enabled | Optimization fires by default once the syscall slice ships. Expected p99 search-latency improvement: ≥ 30% on a fixture workspace with ≥ 10 MB lexical index. |
| Linux 1-socket | Optimization fires; hugepages still apply if THP is enabled. |
| Linux without THP | Pinning works at regular page size; `lexical_hugepages_unavailable` emitted iff `request_hugepages=true`. |
| macOS | `madvise(MADV_WILLNEED)` + optional `mlock` only (no THP). Emits `lexical_hugepages_unavailable` iff hugepages requested. |
| Windows | No equivalent syscall; loader falls through to plain page-cache deserialization. |
| Any platform while scaffold ships without syscalls | Loader records the lexical index was not pinned. Emits `lexical_ram_tier_not_implemented`. |

## Configuration

The optimization is config-driven; no CLI flag is involved.

```toml
[search.lexical_ram_tier]
enabled            = true     # opt out by setting to false
request_hugepages  = false    # set to true on Linux hosts with THP
populate_on_open   = true     # pre-fault all pages on load
```

Environment variables (registered in `src/config/env_registry.rs` by
the registry/docs slice):

| Variable | Equivalent | Notes |
|---|---|---|
| `EE_LEXICAL_INDEX_PIN_RAM` | `[search.lexical_ram_tier] enabled` | Accepts `0` or `1`. |
| `EE_LEXICAL_INDEX_HUGEPAGES` | `[search.lexical_ram_tier] request_hugepages` | Accepts `0` or `1`; ignored without `EE_LEXICAL_INDEX_PIN_RAM=1`. |

These variables are already listed in `docs/env_vars.md` and registered in
`src/config/env_registry.rs`. The remaining wiring is the runtime config read
and loader/status integration, not the registry row itself.

## What `ee status --json` reports

The wiring slice surfaces a `lexicalRamTier` block at
`data.search.lexicalRamTier` matching the
[`ee.status.search.lexical_ram_tier.v1`](../schemas/ee.status.search.lexical_ram_tier.v1.json)
schema. The scaffold pins the schema id and the field shape so consumers
can write parsers ahead of the wiring slice.

```jsonc
{
  "schema": "ee.status.search.lexical_ram_tier.v1",
  "platform": "linux",
  "supported": true,
  "enabled": true,
  "attempted": true,
  "succeeded": true,
  "hugepagesRequested": true,
  "hugepagesGranted": true,
  "populateRequested": true,
  "bytesMmapped": 41943040,
  "pageFaultsPre": 12034,
  "pageFaultsPost": 12044,
  "fallbackPath": "none",
  "indexPath": "/var/lib/ee/indexes/combined/lexical",
  "indexRevision": "lexical:8cb00c...",
  "degradedCodes": []
}
```

`indexRevision` is an opaque corpus stamp for the index directory. Consumers
compare it for exact equality only; the hash input is an implementation detail
and may change when the index manifest format grows a first-class revision.

On any non-success path the loader populates `degradedCodes` with one of the
codes documented in `tests/fixtures/failure_modes/`. The fixture files for the
current scaffold vocabulary have landed:

- [`lexical_ram_tier_disabled`](../../tests/fixtures/failure_modes/lexical_ram_tier_disabled.json) — operator turned the optimization off.
- [`lexical_hugepages_unavailable`](../../tests/fixtures/failure_modes/lexical_hugepages_unavailable.json) — hugepages requested but platform/kernel cannot honor them.
- [`lexical_ram_tier_not_implemented`](../../tests/fixtures/failure_modes/lexical_ram_tier_not_implemented.json) — scaffold ships ahead of the syscall slice.

## Determinism contract

Lexical search results MUST be byte-identical regardless of pinning
state. The optimization only changes wall-clock; the algorithm output is
unchanged. The determinism gate (`tests/determinism_unit.rs`, extended
by the wiring slice) pins this invariant across
`[search.lexical_ram_tier] enabled = true | false` and across
`request_hugepages = true | false`.

## Resource accounting

The wiring slice records the pre/post process page-fault counters (from
`/proc/self/stat` on Linux, `mach_task_basic_info` on macOS) so the
bd-21xbi acceptance evidence can prove pinning eliminated first-touch
faults. The `bytesMmapped` field tallies the on-disk size of every
lexical index file that was successfully pinned.

## Current landed artifacts

- `src/search/lexical_ram_tier.rs` defines the status shape, config data
  structures, degraded-code constants, reader-driven env parsing, and the
  shared tracing helper.
- `src/core/search.rs` invokes the loader at search time when the selected
  index exists, then surfaces enabled-tier degraded codes without changing
  search ranking or results.
- `src/core/status.rs` and `src/output/mod.rs` surface
  `data.search.lexicalRamTier` in `ee status --json`.
- `docs/schemas/ee.status.search.lexical_ram_tier.v1.json` pins the status
  block schema for future `ee status --json` output.
- `src/config/env_registry.rs` and `docs/env_vars.md` register
  `EE_LEXICAL_INDEX_PIN_RAM` and `EE_LEXICAL_INDEX_HUGEPAGES`.
- `src/config/file.rs` and `src/config/merge.rs` parse, merge, and
  source-attribute `[search.lexical_ram_tier]` so `ee config show`
  can report the operator-selected values.
- `src/core/status.rs` reads the merged `[search.lexical_ram_tier]` config
  before producing the lexical RAM-tier status block.
- `tests/fixtures/failure_modes/lexical_ram_tier_disabled.json`,
  `tests/fixtures/failure_modes/lexical_hugepages_unavailable.json`, and
  `tests/fixtures/failure_modes/lexical_ram_tier_not_implemented.json`
  document the scaffold degraded vocabulary.

## What the scaffold does NOT do (yet)

- Issue any `mmap`, `mlock`, `madvise`, or `munmap` syscalls.
- Claim `succeeded=true` or non-zero `bytesMmapped`.
- Thread the merged `[search.lexical_ram_tier]` file config into the search
  runtime path; search still reads the registered env vars directly until the
  next runtime-integration slice.
- Add a `ee doctor` lexical-ram-tier readiness check.
- Land the bench at `benches/lexical_ram_tier.rs` proving the ≥30% p99 improvement.
- Land the e2e script at `scripts/e2e_overhaul/lexical_ram_tier.sh` with strace-based first-touch proof.

These ship under follow-up sub-beads of bd-21xbi. The scaffold
intentionally avoids touching `src/core/status.rs`, `src/cli/mod.rs`,
`src/db/mod.rs`, `src/config/mod.rs`, and `src/config/env_registry.rs`
because those files are routinely contested by other agents' work.

## Related beads

- **Parent**: bd-21xbi — full lexical RAM-tier pinning surface.
- **Epic**: bd-1prrl — Swarm-X extreme swarm responsiveness on 256GB+ / 64+-core hosts.
- **Sibling NUMA pinning**: bd-1prrl.3 / bd-ldstd — same scaffold-first pattern, NUMA-pinned graph snapshots instead of RAM-pinned lexical index.
- **Sibling result cache**: bd-ndzfg — L2 pack result cache; complementary, not duplicative.
- **Sibling embedding LRU**: bd-168gm — embedding cache; different dataset.
