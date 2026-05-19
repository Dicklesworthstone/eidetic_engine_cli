# NUMA-aware graph snapshot pinning

> **Status:** scaffold (bd-ldstd, sub-bead of bd-1prrl.3 / swarmx.4).
> The public surface, configuration, schema, and degraded-code vocabulary
> documented here are stable. The Linux `libc::mbind` / `MAP_POPULATE`
> syscall path and the wiring into `refresh_graph_snapshot` /
> `load_graph_snapshot` are tracked under follow-up slices of bd-1prrl.3.

## What this optimization does

On 2-socket Linux hosts with 256GB+ RAM and 64+ cores, the Linux kernel
scatters a freshly-loaded graph snapshot blob's pages across both NUMA
nodes. When a graph algorithm worker thread on socket 0 touches a page
resident on socket 1's memory controller, latency jumps from roughly
80 ns to 160 ns per random access. Over 10⁸ random accesses — the
typical workload for PPR, HITS, k-truss, and Gomory-Hu hot loops on a
100k-memory workspace — that's the difference between roughly 8 s and
16 s of wall-clock per algorithm pass.

The NUMA pinning path uses `mmap(MAP_POPULATE | MAP_NORESERVE)` plus
`mbind(MPOL_PREFERRED, [requesting_node])` to ensure the snapshot blob
lives on the same NUMA node as the threads that will iterate over it.
No graph algorithm code changes; only the loader path moves.

## Host-class requirements

| Host class | Behavior |
|---|---|
| Linux 2-socket (NUMA available) | Optimization fires by default once the syscall slice ships. Expected wall-clock improvement: ≥ 30% on PPR over the 14k-memory fixture corpus. |
| Linux 1-socket | Optimization fires but `mbind` is a no-op; no regression. |
| Linux (NUMA disabled / `numa_available() < 0`) | Loader falls through to heap deserialization. No degraded code is emitted because the binary cannot prove the host actively rejected pinning. |
| macOS | No NUMA primitives exposed. Loader uses `madvise(MADV_WILLNEED)` plus optional `mlock` (intended; scaffold does not yet invoke either). Emits `numa_pin_unsupported_platform`. |
| Windows | No NUMA primitives in the chosen syscall surface. Loader falls through to heap deserialization. Emits `numa_pin_unsupported_platform`. |
| Linux while scaffold ships without `mbind` | Loader records the snapshot was not pinned. Emits `numa_pin_linux_not_implemented`. |

## Configuration

The optimization is config-driven; no CLI flag is involved.

```toml
[graph.numa_pin]
enabled          = true     # opt out by setting to false
preferred_node   = "auto"   # "auto" detects from CPU affinity; "0"/"1"/... pin to an explicit node
populate_on_load = true     # pre-fault all pages on load
```

Environment variables (registered in `src/config/env_registry.rs` by
the wiring slice — not the scaffold):

| Variable | Equivalent | Notes |
|---|---|---|
| `EE_GRAPH_NUMA_PIN_DISABLE` | `[graph.numa_pin] enabled = false` | Disables the optimization without editing config. |
| `EE_GRAPH_NUMA_PIN_NODE` | `[graph.numa_pin] preferred_node` | Accepts `auto` or a non-negative integer. |

## What `ee status --json` reports

The wiring slice surfaces a `numaPin` block at
`data.graph.numaPin` matching the
[`ee.status.graph.numa_pin.v1`](../schemas/ee.status.graph.numa_pin.v1.json)
schema. The scaffold pins the schema id and the field shape so
consumers can write parsers ahead of the wiring slice.

```jsonc
{
  "schema": "ee.status.graph.numa_pin.v1",
  "platform": "linux",
  "supported": true,
  "enabled": true,
  "attempted": true,
  "succeeded": true,
  "preferredNode": "auto",
  "populateRequested": true,
  "bytesResident": 134217728,
  "populated": true,
  "fallbackPath": "none",
  "snapshotPath": "/var/lib/ee/snapshots/<workspace>/graph-2026-05-18T22-14-01Z.bin",
  "degradedCodes": []
}
```

On any non-success path the loader populates `degradedCodes` with one
of the codes documented in `tests/fixtures/failure_modes/`:

- [`numa_pin_disabled`](../../tests/fixtures/failure_modes/numa_pin_disabled.json) — operator turned the optimization off.
- [`numa_pin_unsupported_platform`](../../tests/fixtures/failure_modes/numa_pin_unsupported_platform.json) — host platform does not expose NUMA primitives.
- [`numa_pin_linux_not_implemented`](../../tests/fixtures/failure_modes/numa_pin_linux_not_implemented.json) — Linux host running the scaffold ahead of the syscall slice.

## Determinism contract

Pack hashes MUST be byte-identical regardless of NUMA pinning state.
The optimization only changes wall-clock; the algorithm output is
unchanged. The determinism gate
(`tests/determinism_unit.rs`, extended by the wiring slice) pins this
invariant across `[graph.numa_pin] enabled = true | false`.

## Snapshot lifecycle interaction

When `ee graph snapshot prune` runs, pinned blobs MUST be unmapped
before the file is deleted. The wiring slice adds a `munmap` plus
reservation release to the prune path; the scaffold's pinning result
includes the snapshot path so the prune handler has the metadata it
needs to release the mapping.

## What the scaffold does NOT do (yet)

- Issue any `mmap`, `mbind`, `mlock`, or `madvise` syscalls.
- Wire into `refresh_graph_snapshot` / `load_graph_snapshot`.
- Surface the `numaPin` block in `ee status --json`.
- Register the `[graph.numa_pin]` config section in `src/config/mod.rs`.
- Register `EE_GRAPH_NUMA_PIN_*` env vars in `src/config/env_registry.rs`.
- Add a `ee doctor` NUMA readiness check.
- Land the criterion bench at `benches/graph_pagerank_numa.rs`.
- Land the e2e script at `scripts/e2e_overhaul/sx4_numa_pin.sh`.

These ship under follow-up sub-beads of bd-1prrl.3. The scaffold
intentionally avoids touching `src/core/status.rs`, `src/cli/mod.rs`,
`src/db/mod.rs`, `src/config/mod.rs`, and `src/config/env_registry.rs`
because those files are currently in flight for unrelated shard-fanout
and workspace-hygiene work.

## Related beads

- **Parent**: bd-1prrl.3 / swarmx.4 — full NUMA pinning surface.
- **Epic**: bd-1prrl — Swarm-X extreme swarm responsiveness on 256GB+ / 64+-core hosts.
- **Soft cooperative**: bd-1zb7k.12 — host calibration and topology-aware resource profiles; supplies the topology probe used by `detect_preferred_node()` once it lands.
- **Cooperative**: bd-oja31 — SRR1 `ee daemon --hot-mode` RAM-pinned ANN; shares the hardware-class dependency and the eventual `[host.topology]` config section.
