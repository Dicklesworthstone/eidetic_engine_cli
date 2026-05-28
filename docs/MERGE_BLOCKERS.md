# Merge Blockers — Daemon Multi-Agent Exposure Gate

**Author:** reviewer-triage-p7
**Date:** 2026-05-27
**Source:** docs/reports/FINAL_REVIEW_REPORT_ROUND5 (Rounds 1-3, 113 findings on b21bc8df..f518de3b)
**Gate:** The daemon code (bd-oja31, SRR1 UDS RPC skeleton in `src/daemon/`) MUST NOT be exposed to multi-agent traffic until every bead listed here is closed with shipped code.

## Method

Each P0 + P1 finding from the Round-5 review was triaged for must-fix-before-merge.
"Multi-agent traffic" is interpreted as: more than one local UID, or more than
one concurrent agent process, connecting to the daemon UDS. Findings outside
that exposure surface (e.g. install.sh release supply chain) are tracked but
excluded from this gate.

## Triage Result

8 of 9 P0/P1 findings are blocking. One (bd-3anfz, install.sh pinned-key trust
root) is excluded — it's a real P1 supply-chain finding but gates the release
pipeline, not the daemon-traffic exposure.

---

## The Blockers

### 1. bd-3j0td (P0) — UDS socket world-connectable, no SO_PEERCRED

**Why blocking:** Definitional. The daemon's dispatch table currently has zero
authentication AND zero redaction. Once warm-load lands (the next slice
already enumerated in the bd-oja31 epic), every accepted connection becomes
a cross-tenant exfiltration channel with a one-line attack script. The
canonical CLI pipeline routes data-bearing responses through redaction
(`src/core/outcome.rs`, `src/core/support_bundle.rs`, `src/mcp.rs`); the
daemon bypasses all of it.

**Proposed fix:**
- `src/daemon/server.rs:116-185` (`start_server`) — after `UnixListener::bind`,
  call `fs::set_permissions(&socket_path, Permissions::from_mode(0o600))`.
- `src/daemon/server.rs` `handle_connection` — `getpeereid(2)` check before
  `read_request`; refuse with new `daemon_peer_unauthorized` degraded code.
- `src/daemon/mod.rs:90-104` (`default_daemon_socket_path`) — TMPDIR fallback
  becomes `${TMPDIR}/ee-${uid}/daemon.sock` with parent dir mode `0o700`.
- Regression test asserting `metadata.mode() & 0o777 == 0o600`.

---

### 2. bd-jnyui (P1) — Unbounded thread spawn per connection (local DoS)

**Why blocking:** `run_accept_loop` spawns a new `std::thread::Builder::spawn`
per accept with no semaphore, no per-peer rate-limit, no queue depth. Each
worker holds ~2 MiB stack + up to 4 MiB request buffer + 30s read timeout.
The whole point of "multi-agent traffic" is many concurrent connections; the
daemon must survive its intended workload before exposure.

**Proposed fix:**
- `src/daemon/server.rs:187-212` (`run_accept_loop`) — replace per-connection
  spawn with a bounded thread pool (e.g. `rayon::ThreadPoolBuilder` capped at
  `DAEMON_MAX_INFLIGHT`, default 32).
- Add accept-loop backpressure: when pool is saturated, reject with framed
  `daemon_overloaded` degraded code rather than queueing.

---

### 3. bd-3ik2d (P1) — TOCTOU + predictable-path on `/tmp/ee-daemon.sock`

**Why blocking:** Two races at bind time:
(a) `symlink_metadata` → `remove_file` window allows a symlink swap;
(b) shared `/tmp` path collides across UIDs.
Same threat model as bd-3j0td; fixing socket permissions without fixing
predictable-path leaves a pre-bind attack vector.

**Proposed fix:**
- `src/daemon/server.rs:130-159` — replace stat-then-remove with
  `O_EXCL`-style atomic create (bind to a temp path, `rename` into place
  after `set_permissions`).
- `src/daemon/mod.rs:95-104` — per-UID parent directory (covered by bd-3j0td
  fix #3; coordinate the diffs).

---

### 4. bd-37o8k (P1) — `ee daemon start` lifecycle lies

**Why blocking:** Surface contract violation. The detached path emits
`success: true` to stdout while `std::mem::forget(handle)` + parent-process
exit kills the listener immediately. Every multi-agent harness that relies
on the start envelope to decide "the daemon is up, connect now" will
mis-schedule. Orphan sockets accumulate on disk. This is correctness, not
just security.

**Proposed fix:**
- `src/cli/mod.rs:41922-42040` (`handle_daemon_hot_mode_start`) — replace
  `std::mem::forget(handle)` with a true daemonize step (`fork(2)` + detach,
  or spawn the listener as a separate process via `Command::new`) and poll
  for the socket to become connectable before emitting success.
- On poll-timeout, emit `success: false` with `daemon_start_failed`.

---

### 5. bd-1feff (P1) — 6 new degraded codes ship without fixtures or taxonomy

**Why blocking:** Per AGENTS.md K2 contract gate: "Adding a feature without
updating its gate is a regression." Multi-agent harnesses parse degraded
codes; codes absent from `tests/fixtures/failure_modes/` and the taxonomy
docs are unparseable contract surfaces. Two of the six (`daemon_ram_pinning_unavailable_on_macos`,
`daemon_socket_unavailable`) are also DEAD — declared but never emitted —
indicating the registration sweep was never run.

**Proposed fix:** Add for each of `daemon_ann_warmload_not_yet_implemented`,
`daemon_ram_pinning_unavailable_on_macos`, `daemon_socket_unavailable`,
`daemon_unknown_method`, `daemon_request_decode_failed`,
`daemon_request_schema_mismatch`:
- `tests/fixtures/failure_modes/<code>.json`
- Entry in `docs/degraded_code_taxonomy.md`
- Entry in `docs/degraded_codes.md`
- Either wire emission for the two DEAD codes, or remove them.

---

### 6. bd-3q19d (P1) — `ee.daemon.start.v1` / `ee.daemon.stop.v1` envelopes lack schema files

**Why blocking:** The CLI emits envelopes whose `data.schema` is
`"ee.daemon.start.v1"` (`src/cli/mod.rs:41954`) and `"ee.daemon.stop.v1"`
(`src/cli/mod.rs:42043`), but no `docs/schemas/ee.daemon.start.v1.json` or
`.../stop.v1.json` exists. Multi-agent consumers cannot validate the
envelopes they parse; future drift is silently un-caught.

**Proposed fix:**
- Create `docs/schemas/ee.daemon.start.v1.json` and
  `docs/schemas/ee.daemon.stop.v1.json` describing the emitted shape.
- Tie in via bd-2oxdj registration step below.

---

### 7. bd-2oxdj (P1) — `ee.daemon.*` schemas not registered in `schema_drift::all_schemas()`

**Why blocking:** K2 drift gate (`tests/contracts/schema_drift.rs:523`,
`all_schemas()`) is documented in AGENTS.md as the load-bearing CI surface
that "fails CI when an emitted response doesn't validate against its declared
schema." The four new daemon schemas
(`ee.daemon.request.v1`, `ee.daemon.response.v1`, `ee.daemon.start.v1`,
`ee.daemon.stop.v1`) are absent from every `SchemaEntry` array. The gate is
silently passing on a surface it was supposed to gate. Once daemon is exposed,
drift becomes undetectable.

**Proposed fix:**
- `tests/contracts/schema_drift.rs` — add `DAEMON_SCHEMAS` array, include in
  `all_schemas()` union.
- Add `ee.daemon.*` schemas to `public_contract_inventory` (search for the
  inventory call in the same file).

---

### 8. bd-30i43 (P1) — `daemon start`/`daemon stop` missing from `EffectManifest`

**Why blocking:** `src/core/effect.rs::EffectManifest` is the agent-readable
catalog of blast radius. Trauma-guard, agent harnesses, and policy layers
consult it before invoking a command. Both new subcommands mutate the
filesystem (bind / unlink UDS file) but neither has a manifest entry. In
multi-agent context, harnesses cannot make safe scheduling decisions about
commands whose blast radius they cannot see.

**Proposed fix:**
- `src/core/effect.rs:1391-1408` — add entries for `"daemon start"` and
  `"daemon stop"`, both classified `durable_write` (filesystem mutation
  outside workspace).
- Verify with `tests/contracts/effect_manifest_completeness.rs` (or create
  the test if absent).

---

## Excluded From This Gate

**bd-3anfz (P1)** — install.sh pinned-key parallel trust root. This is a
real P1 supply-chain finding gating the v0.3.2+ release pipeline, but it
does not gate daemon-traffic exposure. Track separately under the release
hardening gate.

## Recommended Landing Order

1. **bd-2oxdj + bd-3q19d + bd-30i43** (schema/manifest wiring — fast, mechanical, unblocks the K2 gate to catch downstream drift on later fixes).
2. **bd-3j0td + bd-3ik2d** (socket security — same diff zone, coordinate).
3. **bd-jnyui** (bounded thread pool — independent diff zone).
4. **bd-37o8k** (lifecycle correctness — independent).
5. **bd-1feff** (fixtures + taxonomy — independent, can land in parallel).

Once all 8 are closed with shipped code (verified via `git log -S` for the
sentinel strings in each bead's proposed fix), the daemon is cleared for
multi-agent exposure.
