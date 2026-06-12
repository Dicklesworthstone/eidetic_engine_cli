# Lexical posting-list RAM-tier warmload

> **Status:** heap-warmload contract landed (bd-21xbi.2), with the historical
> `lexical_ram_tier_not_implemented` fixture retained as a retired tombstone.
> The public surface, configuration, schema, and degraded-code vocabulary are
> stable. Runtime config, status, doctor, and search-path plumbing are landed.
> The V1 implementation does not promise OS-level `mmap` / `mlock` /
> `MADV_HUGEPAGE` pinning under the crate-level `forbid(unsafe_code)` policy.

## What this optimization does

On large Linux hosts the Frankensearch lexical posting-list files under
`indexes/combined/` can pay disk page-fault cost on first touch. For a typical
14k-memory workspace the lexical index is 10–100 MB; on a 100k+ memory
workspace it can reach tens of GB.

The RAM-tier loader keeps an opt-in process-local heap mirror of the lexical
index files. No search-side code changes; only the loader path moves.
Determinism is preserved because the optimization only changes wall-clock and
page-cache residency — the search results are byte-identical whether the index
is heap-warmloaded or read from disk. The status surface intentionally reports
`succeeded=false`, `bytesMmapped=0`, and
`degradedCodes=["lexical_ram_tier_heap_warmload"]` so operators do not mistake
the warmload for OS-level pinning.

## Distinguishability

This bead is intentionally distinct from three other memory-residency surfaces:

| Bead | Dataset | Why distinct |
|---|---|---|
| **bd-21xbi** (this one) | Frankensearch lexical posting-list files | Heap-warmloaded under the safe V1 contract; dominant cold-path cost on text-heavy search |
| **bd-1prrl.3** (swarmx.4) | Graph snapshot blobs | NUMA-aware (`mbind`); graph-algorithm random-access pattern, not search-style sequential scan |
| **bd-ndzfg** | Assembled `ee.context.v2` pack JSON results | Caches RESULTS keyed on (query, workspace, manifest); plan-cache and result-cache misses still pay the lexical first-touch cost this bead eliminates |
| **bd-168gm** | Embedding vectors (LRU keyed on exact text hash) | Caches embedding vectors; lexical posting lists are an unrelated dataset |

## Host-class requirements

| Host class | Behavior |
|---|---|
| Linux 2-socket (256GB+) with THP enabled | Optimization can be enabled as heap warmload. It does not claim hugepage or mlock residency. |
| Linux 1-socket | Optimization can be enabled as heap warmload. |
| Linux without THP | Heap warmload still works. If `request_hugepages=true`, the status surface does not claim hugepages were granted. |
| macOS | No Linux-equivalent THP path. Emits `lexical_ram_unavailable_on_macos`, plus `lexical_hugepages_unavailable` iff hugepages are requested. |
| Windows | No equivalent syscall; loader falls through to plain page-cache deserialization. |
| Linux under the crate-level unsafe-code ban | This is the supported V1 path: loader retains lexical index file bytes in process heap memory and emits `lexical_ram_tier_heap_warmload`; it does not claim OS-level pinning. |

## Configuration

The optimization is config-driven; no CLI flag is involved.

```toml
[search.lexical_ram_tier]
enabled            = true     # opt in; the runtime default is false
request_hugepages  = false    # set to true on Linux hosts with THP
populate_on_open   = true     # pre-fault all pages on load
```

Environment variables (registered in `src/config/env_registry.rs` by
the registry/docs slice):

| Variable | Equivalent | Notes |
|---|---|---|
| `EE_LEXICAL_INDEX_PIN_RAM` | `[search.lexical_ram_tier] enabled` | Boolean; accepts `true`/`false`, `1`/`0`, `yes`/`no`, or `on`/`off`. |
| `EE_LEXICAL_INDEX_HUGEPAGES` | `[search.lexical_ram_tier] request_hugepages` | Boolean; accepts the same vocabulary and is ignored unless pinning is enabled. |

These variables are listed in `docs/env_vars.md`, registered in
`src/config/env_registry.rs`, and consumed through the shared config readers
used by status, doctor, and search.

## What `ee status --json` reports

The wiring slice surfaces a `lexicalRamTier` block at
`data.search.lexicalRamTier` matching the
[`ee.status.search.lexical_ram_tier.v1`](../schemas/ee.status.search.lexical_ram_tier.v1.json)
schema. The status schema pins the field shape so consumers
can distinguish disabled, heap-warmloaded, macOS-limited, unsupported, and
historical retired states.

For Linux, an enabled tier reports the heap warmload fallback instead of
claiming OS-level pinning:

```jsonc
{
  "schema": "ee.status.search.lexical_ram_tier.v1",
  "platform": "linux",
  "supported": true,
  "enabled": true,
  "attempted": true,
  "succeeded": false,
  "hugepagesRequested": true,
  "hugepagesGranted": false,
  "populateRequested": true,
  "bytesMmapped": 0,
  "bytesWarmloaded": 41943040,
  "pageFaultsPre": 0,
  "pageFaultsPost": 0,
  "fallbackPath": "heap_warmload",
  "indexPath": "/var/lib/ee/indexes/combined/lexical",
  "indexRevision": "lexical:8cb00c...",
  "degradedCodes": ["lexical_ram_tier_heap_warmload"]
}
```

`indexRevision` is an opaque corpus stamp for the index directory. Consumers
compare it for exact equality only; the hash input is an implementation detail
and may change when the index manifest format grows a first-class revision.

On any non-success path the loader populates `degradedCodes` with one of the
codes documented in `tests/fixtures/failure_modes/`. The fixture files for the
live vocabulary are:

- [`lexical_ram_tier_disabled`](../../tests/fixtures/failure_modes/lexical_ram_tier_disabled.json) — operator turned the optimization off.
- [`lexical_hugepages_unavailable`](../../tests/fixtures/failure_modes/lexical_hugepages_unavailable.json) — hugepages requested but platform/kernel cannot honor them.
- [`lexical_ram_tier_heap_warmload`](../../tests/fixtures/failure_modes/lexical_ram_tier_heap_warmload.json) — safe process-local heap warmload is active, but OS-level pinning is not claimed.
- [`lexical_ram_unavailable_on_macos`](../../tests/fixtures/failure_modes/lexical_ram_unavailable_on_macos.json) — macOS host class cannot reproduce the Linux RAM-tier posture.

## Determinism contract

Lexical search results MUST be byte-identical regardless of RAM-tier
state. The optimization only changes wall-clock; the algorithm output is
unchanged. The determinism gate (`tests/determinism_unit.rs`, extended
by the wiring slice) pins this invariant across
`[search.lexical_ram_tier] enabled = true | false` and across
`request_hugepages = true | false`.

## Resource accounting

The V1 heap-warmload path reports retained bytes through `bytesWarmloaded`.
`bytesMmapped`, `pageFaultsPre`, and `pageFaultsPost` remain `0` because this
crate does not issue OS-level pinning or page-fault inspection syscalls.

## Current landed artifacts

- `src/search/lexical_ram_tier.rs` defines the status shape, config data
  structures, degraded-code constants, reader-driven env parsing, and the
  shared tracing helper.
- `src/core/search.rs` invokes the loader at search time when the selected
  index exists, reads the merged `[search.lexical_ram_tier]` config, then
  surfaces enabled-tier degraded codes without changing search ranking or
  results.
- `src/core/status.rs` and `src/output/mod.rs` read the merged runtime config
  and surface `data.search.lexicalRamTier` in `ee status --json`.
- `src/core/doctor.rs` includes the lexical RAM-tier readiness check.
- `docs/schemas/ee.status.search.lexical_ram_tier.v1.json` pins the status
  block schema for `ee status --json` output.
- `src/config/env_registry.rs` and `docs/env_vars.md` register
  `EE_LEXICAL_INDEX_PIN_RAM` and `EE_LEXICAL_INDEX_HUGEPAGES`.
- `src/config/file.rs` and `src/config/merge.rs` parse, merge, and
  source-attribute `[search.lexical_ram_tier]` so `ee config show`
  can report the operator-selected values.
- `tests/fixtures/failure_modes/lexical_ram_tier_disabled.json`,
  `tests/fixtures/failure_modes/lexical_hugepages_unavailable.json`, and
  `tests/fixtures/failure_modes/lexical_ram_tier_heap_warmload.json`
  document the live degraded vocabulary.

## What the V1 contract does not do

- Issue any `mmap`, `mlock`, `madvise`, or `munmap` syscalls.
- Claim `succeeded=true` or non-zero `bytesMmapped`.
- Retain bytes outside the process-local heap warmload cache.
- Land the bench at `benches/lexical_ram_tier.rs` proving the ≥30% p99 improvement.
- Promote OS-pinning or first-touch/page-fault evidence as a V1 acceptance
  promise.

## Related beads

- **Parent**: bd-21xbi — lexical RAM-tier surface; V1 scope is heap warmload.
- **Epic**: bd-1prrl — Swarm-X extreme swarm responsiveness on 256GB+ / 64+-core hosts.
- **Sibling NUMA pinning**: bd-1prrl.3 / bd-ldstd — related memory-residency pattern, NUMA-pinned graph snapshots instead of heap-warmloaded lexical index.
- **Sibling result cache**: bd-ndzfg — L2 pack result cache; complementary, not duplicative.
- **Sibling embedding LRU**: bd-168gm — embedding cache; different dataset.
