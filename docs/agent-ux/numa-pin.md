# NUMA-aware graph snapshot pinning

> **Status:** scaffold (bd-ldstd, sub-bead of bd-1prrl.3 / swarmx.4).
> Nothing pins a graph snapshot on any platform yet. What runs today is
> the advisory `graph_numa_pin` check in `ee doctor`, configured by three
> environment variables; the `[graph.numa_pin]` config table is not
> accepted. The Linux `libc::mbind` / `MAP_POPULATE` syscall path and the
> wiring into `refresh_graph_snapshot` / `load_graph_snapshot` remain
> unbuilt. bd-1prrl.3 is closed and no open bead tracks them.

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

No CLI flag is involved. The planned `.ee/config.toml` table was:

```text
[graph.numa_pin]
enabled          = true     # opt out by setting to false
preferred_node   = "auto"   # "auto" detects from CPU affinity; "0"/"1"/... pin to an explicit node
populate_on_load = true     # pre-fault all pages on load
```

Do not add it. `[graph]` is a closed table in `src/config/file.rs`, and
`numa_pin` is not one of its keys, so the config loader rejects the whole
file with an unknown-key error.

The settings are environment variables, registered in
`src/config/env_registry.rs`. The two switches accept `true|1|yes|on` and
`false|0|no|off`. An unparseable value in any of the three is ignored.
Today only `ee doctor` reads them.

| Variable | Default | Notes |
|---|---|---|
| `EE_GRAPH_NUMA_PIN_DISABLE` | off | Turns the optimization off; doctor then reports the check as disabled. |
| `EE_GRAPH_NUMA_PIN_NODE` | `auto` | Accepts `auto` or a non-negative integer. |
| `EE_GRAPH_NUMA_PIN_POPULATE` | on | Requests pre-faulting all pages on load. |

## What `ee doctor --json` reports

The `graph_numa_pin` check is advisory, so it never changes doctor's
overall health. On Linux it reports `ok` with a not-implemented note. On
macOS and Windows, with the default settings, it reports a `warning`
carrying `numa_pin_unsupported_platform`. With
`EE_GRAPH_NUMA_PIN_DISABLE=1` it reports `ok` as disabled.

## What `ee status --json` reports

Nothing yet: `ee status --json` emits no `numaPin` block. The
[`ee.status.graph.numa_pin.v1`](../schemas/ee.status.graph.numa_pin.v1.json)
schema is registered and fixes the planned shape of a block at
`data.graph.numaPin`, shown below, but no command fills it.

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
unchanged. Nothing tests this yet, because nothing pins:
`tests/determinism_unit.rs` does not mention NUMA. When the syscall path
lands, that gate needs a case run with pinning on and off.

## Snapshot lifecycle interaction

Any command that deletes a pinned snapshot file must unmap it first.
There is no such command today: `ee graph snapshot` has only `refresh`.
The pinning result already carries the snapshot path, which is what a
future prune path would need to release the mapping.

## What the scaffold does NOT do (yet)

- Issue any `mmap`, `mbind`, `mlock`, or `madvise` syscalls.
- Wire into `refresh_graph_snapshot` / `load_graph_snapshot`.
- Surface the `numaPin` block in `ee status --json`.
- Accept a `[graph.numa_pin]` config table (the loader rejects one).
- Land the criterion bench at `benches/graph_pagerank_numa.rs`.
- Land the e2e script at `scripts/e2e_overhaul/sx4_numa_pin.sh`.

The `EE_GRAPH_NUMA_PIN_*` environment variables and the `ee doctor`
check, which earlier versions of this list also named, have since landed.
bd-1prrl.3 closed after the `NumaPinningAdapter` trait landed, leaving the
items above with no open bead.

## Related beads

- **Parent**: bd-1prrl.3 / swarmx.4 — full NUMA pinning surface.
- **Epic**: bd-1prrl — Swarm-X extreme swarm responsiveness on 256GB+ / 64+-core hosts.
- **Soft cooperative**: bd-1zb7k.12 — host calibration and topology-aware resource profiles; supplies the topology probe used by `detect_preferred_node()` once it lands.
- **Cooperative**: bd-oja31 — SRR1 `ee daemon --hot-mode` RAM-pinned ANN; shares the hardware-class dependency and the eventual `[host.topology]` config section.
