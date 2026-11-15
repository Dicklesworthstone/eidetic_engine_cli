# Handoff Memory Set Fingerprint

Schema: `ee.handoff.memory_set_fingerprint.v1`

The memory-set fingerprint is the planned handoff proof for exactly which
memories were included in a capsule or resume context. Agents will use it to
detect stale handoffs and pack mismatches.

Example:

```bash
jq '.memorySetHash' handoff-fingerprint.json
```

Related schemas: `ee.producer.metadata.v1`, `ee.pack.slo.v1`.

Non-goals: this schema describes the fingerprint contract only; it is not a
complete handoff capsule schema and should not be used as the resume envelope.

Hash format contract (`bd-xmhzk`):
- Capsule-level `content_hash` (`ee.handoff.create.v1` and stored `HandoffCapsule`)
  is a bare 16-character lowercase hexadecimal BLAKE3 prefix (`^[0-9a-f]{16}$`,
  without a `blake3:` prefix) so stored capsule verification and round-trip
  determinism contracts remain byte-compatible.
- Snapshot, diff, and resume-fragment digests (`memorySetHash`, `tree_root`,
  `workspace_state_hash`, and `prompt_fragment.hash`) carry the algorithm tag
  (`blake3:<16-hex>`).

Tracking Bead: `bd-17c65.13.5`
