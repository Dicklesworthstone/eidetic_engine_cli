# Volatile Field Registry

This registry is the single source for fields that determinism checks may strip
before comparing machine-facing JSON produced from the same workspace state.

The Rust registry lives in `src/obs/volatile_fields.rs` as
`VOLATILE_FIELD_NAMES`. The J7 determinism harness mirrors the same list in
`scripts/e2e_overhaul/determinism.sh`. Additions must update this document,
the Rust constant, and the shell list together.

| Field path | Reason for volatility | Introduced in version | Notes |
|---|---|---|---|
| `generatedAt` / `generated_at` | Wall-clock timestamp | v0.1 | RFC 3339 report timestamp. |
| `computed_at` | Wall-clock timestamp | v0.1 | Resume and diagnostic comparisons that recompute live state. |
| `last_accessed` / `last_accessed_at` | Per-read update | v0.1 | Access signals for memory freshness and decay. |
| `last_seen_at` | Per-read or per-observation update | v0.1 | Agent, workspace, and discovery observations may refresh this field. |
| `last_used_at` | Per-read update | v0.1 | Usage freshness signal. |
| `audit_ts` | Per-write timestamp | v0.1 | Audit chain event time. |
| `elapsedMs` / `elapsed_ms` | Wall-clock elapsed time | v0.1 | Performance-only measurement. |
| `elapsedMsBucket` / `durationMs` / `wallClockMs` | Wall-clock elapsed time | v0.3 | Bucketed or alternate spellings used by perf, verification, and workload surfaces. |
| `startedAt` / `started_at` | Wall-clock start time | v0.1 | Maintenance jobs and long-running operations. |
| `endedAt` / `ended_at` | Wall-clock end time | v0.1 | Maintenance jobs and long-running operations. |
| `ts` / `timestamp` | Generic wall-clock timestamp | v0.1 | Log envelopes and event records. |
| `runIndex` / `run_index` | Measurement run ordinal | v0.1 | Perf gates compare stable payloads across repeated invocations. |
| `ee_binary_hash` | Per-build artifact hash | v0.1 | Included in run summaries and status-like diagnostics. |
| `databasePath` / `workspacePath` | Machine-dependent absolute path | v0.1 | Canonicalized but environment-dependent. |
| `indexDir` | Machine-dependent absolute path | v0.1 | Rebuildable derived asset location. |
| `snapshotRefreshedAt` | Wall-clock graph snapshot refresh time | v0.2 | Graph determinism strips this before hash comparison. |
| `runDurationMs` | Wall-clock graph or algorithm run duration | v0.2 | Measurement-only timing; not semantic graph content. |
| `witnessElapsedMs` | Wall-clock algorithm witness duration | v0.2 | CGSE witness timing varies by host and load. |
| `witnessRecordedAt` | Wall-clock witness persistence time | v0.2 | Audit timing for the witness record. |
| `algorithmStartedAt` | Wall-clock graph algorithm start time | v0.2 | Used to explain operations, not rank or selection. |
| `projectionMs` / `pagerankMs` / `betweennessMs` / `totalMs` | Wall-clock graph algorithm duration | v0.3 | Algorithm timing varies by host and load; graph content is compared separately. |
| `createdAt` / `created_at` / `updatedAt` / `completedAt` / `finishedAt` / `expiresAt` | Lifecycle wall-clock timestamp | v0.3 | Creation/update/completion/expiry times on records, locks, runs, snapshots, and evidence rows. |
| `capturedAt` / `captured_at` / `computedAt` / `computed_at` / `observedAt` / `recordedAt` / `refreshedAt` | Observation wall-clock timestamp | v0.3 | Capture/compute/observe/record/refresh times on diagnostics, graph, support, and evidence surfaces. |
| `selectedAt` / `decidedAt` / `estimatedAt` / `exposedAt` / `lastValidatedAt` | Derived decision wall-clock timestamp | v0.3 | Selection, decision, estimate, exposure, and validation timestamps that depend on when a run happened. |
| `run_duration_ms` | Wall-clock run duration (snake_case spelling) | v0.3 | Same measurement class as `runDurationMs`. |
| `capsule_id` | Per-create random identifier | v0.3 | Handoff capsules mint a fresh id even for identical workspace state. |
| `integrity` | Per-create HMAC signature block | v0.3 | Signed over volatile content; never part of canonical capsule identity. |
| `swarm_brief_summary` / `swarm_incident_summary` / `swarm_replay_summary` | Runtime diagnostic subtree | v0.3 | Coordination posture snapshots embedded in capsules and support bundles. |
| `environment_attestation_summary` / `pack_replay_summary` / `proof_broker_summary` / `regression_causality_summary` / `shadow_policy_summary` | Runtime diagnostic subtree | v0.3 | Embed run-scoped attestation ids, artifact hashes over volatile inputs, and raw command output with timestamps. |
| `selfNodeKey` / `selfTailscaleIp` / `selfMagicDnsName` / `tailnetId` / `tailnetDisplayName` / `selfAdvertisedTags` | Machine/network identity | v0.3 | Tailscale local-probe identity; machine-specific and sensitive in shared bundles. |
| `peerNodeKey` / `peerTailscaleIps` / `peerMagicDnsName` / `peerHostname` / `peerAdvertisedTags` | Machine/network identity | v0.3 | Tailscale peer identity fields; same posture as the self fields. |
| `binaryVersionRaw` / `binaryAbsolutePath` | Per-host binary metadata | v0.3 | Host-installed binary probe details. |

The registry is intentionally field-name based, not JSON-pointer based. These
fields may appear at multiple nesting depths across command responses, golden
fixtures, and E2E support logs.

See `docs/agent-ux/float-determinism.md` for the graph-specific contract around
same-machine byte determinism, cross-architecture float drift, and stable rank
ordering for float-bearing surfaces.
