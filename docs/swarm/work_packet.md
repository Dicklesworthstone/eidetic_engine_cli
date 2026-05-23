# Swarm Work Packet

Schema: `ee.swarm.work_packet.v1`

`ee.swarm.work_packet.v1` is the deterministic, redacted, read-only artifact
emitted by `ee swarm work-packet --json` before an agent chooses work in a
crowded checkout. It packages the recommended lane, candidate Bead decisions,
dirty-file collision evidence, active claims, Agent Mail freshness, RCH proof
posture, required verification commands, source provenance, and exact reasons a
task is safe, unsafe, blocked, or stale.

The packet composes existing read-only collectors from `ee swarm brief` and
`ee swarm next-action`. It must not parse Beads, BV, Agent Mail, RCH, or Git
with a second independent vocabulary when a collector already exists.

Versioning: field renames, changed decision semantics, or changed mutation
policy semantics require a new schema version. Additive fields may remain in
`ee.swarm.work_packet.v1` only when consumers can safely ignore them and the
deterministic ordering rules below still hold.

Redaction rules: Bead IDs, titles, statuses, priority values, assignee labels,
path patterns, counts, degraded codes, command templates, and stable source
digests are allowed. Mail bodies, raw command output, raw source snippets, env
dumps, unredacted home paths, file contents, and secret-like tokens are not
allowed.

Determinism rules:

- `packetId` is derived from the redacted payload, not wall-clock time.
- Arrays are sorted by stable keys: source name, Bead ID, path pattern, then
  reason code.
- Source freshness is represented as a class or redacted timestamp supplied by
  the source collector. Packet generation must not add a new volatile timestamp.
- Unknown source state is explicit as `unknown`, `skipped`, `unavailable`, or a
  `degraded[]` record; it is never inferred from missing fields.

Fixture scenarios:

- `healthy_small_repo`: fresh coordination sources, no dirty collisions, and a
  ready candidate that is safe to claim after normal Beads/file-reservation
  steps.
- `crowded_checkout`: active claims and dirty path overlap force
  `coordinate_before_claim`.
- `degraded_mail_rch_topology`: Agent Mail is degraded and remote-only Cargo
  proof is blocked, so only static or docs work can proceed until RCH recovers.

Implementation contract:

- Generate the packet only after reading existing swarm brief and next-action
  snapshots, or equivalent in-memory collector outputs.
- Preserve source provenance for each included decision so an agent can decide
  whether Beads, BV, Agent Mail, or RCH drove the advice.
- Include RCH posture even for docs-first work so closeouts do not accidentally
  imply local Cargo fallback is allowed.
- Keep command fields as templates. They are obligations for the next step, not
  proof that the packet generator ran those commands.

Non-goals: work packets do not claim Beads, reserve files, send Agent Mail,
stage Git changes, run Cargo, delete files, schedule agents, or replace Beads,
Agent Mail, BV, RCH, `ee swarm brief`, or `ee swarm next-action`.

## Agent Mail fallback semantics (bd-2z5ly.8)

`coordination.agentMail` carries enough redacted health metadata for a
downstream candidate-safety classifier to choose a conservative posture without
ever reading mail bodies or raw inbox contents:

- `status` is one of `fresh`/`healthy`, `degraded_read_only`,
  `archive_ahead_of_sqlite`, `inbox_unavailable`, `reservation_unavailable`,
  `outbox_only`, `unreachable`, `unavailable`, or `skipped`. `fresh` and
  `healthy` are aliases; new emitters should prefer `healthy`.
- `recoveryMode` advises the next-step posture: `wait_for_repair`,
  `proceed_via_beads`, `static_work_only`, `manual_coordination`, or `none`.
- `archiveIndexParity` summarises Agent Mail JSONL archive vs SQLite index
  drift: `aligned`, `archive_ahead`, `sqlite_ahead`, or `unknown`.
- `reservationAuthoritative` and `inboxAuthoritative` tell the consumer whether
  reservation evidence or unread/ack counts in this packet can be trusted.
  When either flag is `false` or `null`, candidate safety MUST downgrade
  confidence rather than treating a missing or zero count as evidence that no
  peer conflict exists.
- `fallbackActions` is an ordered, structured workflow keyed by `kind`. The
  array is sorted lexicographically by `kind` so the packet stays deterministic
  across runs. Action kinds: `beads_comment`, `manual_coordination`,
  `record_only`, `retry_later`, `support_bundle`, `switch_to_static_work`.
  This replaces prose-only repair strings so harnesses can branch mechanically
  instead of parsing natural language.

Redaction invariant: `fallbackActions[].summary`, `command`, and `manualStep`
MUST NOT include raw inbox bodies, message IDs, headers (`From:`, `Subject:`,
`Message-ID:`), agent identities, or unredacted reservation paths. The
`agent_mail_degraded_read_only` fixture under
`tests/fixtures/swarm_work_packet/` shows the canonical degraded shape; the
`work_packet_agent_mail_fallback_semantics_are_contractual` lifecycle test
fences these properties.

Tracking Bead: `bd-2z5ly.2`
