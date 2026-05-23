# Swarm Work Packet

Schema: `ee.swarm.work_packet.v1`

`ee.swarm.work_packet.v1` is the deterministic, redacted, read-only artifact
emitted by `ee swarm work-packet --json` before an agent chooses work in a
crowded checkout. It packages the recommended lane, candidate Bead decisions,
dirty-file collision evidence, active claims, Agent Mail freshness, Beads
tracker integrity, RCH proof posture, required verification commands, source
provenance, and exact reasons a task is safe, unsafe, blocked, or stale.

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

## Beads tracker integrity (bd-2z5ly.9)

`trackerIntegrity` is the packet's bounded view of Beads JSONL/DB health. It is
derived from `br doctor --json` or equivalent collector evidence, not by
re-parsing raw tracker rows inside the work-packet layer.

- `health` is one of `ok`, `merge_artifacts_warn`,
  `external_changes_pending_import`, `db_jsonl_count_mismatch`, or
  `jsonl_parse_error`.
- `brReadsAuthoritative` is true only for `ok`; when false, consumers must not
  treat `br ready` or a zero-conflict candidate list as proof that claiming is
  safe.
- `requiresCandidateDowngrade` is true for malformed JSONL and DB/JSONL count
  mismatches. Candidate safety MUST refuse auto-claim-style advice in those
  states.
- Counts and paths are bounded summaries: JSONL row count, DB row count,
  pending import count, dirty issue count, merge artifact count, and at most a
  small sorted list of merge artifact paths.
- `jsonlParseError` carries only the first invalid line/column plus a redacted,
  length-capped excerpt. It must never include raw issue bodies beyond that
  bounded diagnostic.

The work-packet generator never repairs Beads state. Recovery remains explicit:
inspect the malformed row, run `br doctor --json`, use
`br --no-auto-import --allow-stale` for read-only fallback when needed, and only
then claim or update tracker state.

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

## Shell-safe agent command actions (bd-13dmm.3)

Every agent-actionable command in the work packet now has a structured
`commandAction` representation alongside the legacy human-readable
`commandTemplate` string. The structured shape lets a harness execute
the recommended next step without invoking a shell.

- `commandAction` is defined under `definitions/commandAction` in
  `docs/schemas/swarm/ee.swarm.work_packet.v1.json`. Required fields:
  - `commandId` — stable, dot-delimited identifier.
  - `displayCommand` — single-line human-readable form. Must use the
    `safeCommandString` shape (no shell metacharacters, mail headers,
    raw home paths, or secret-looking tokens).
  - `argv` — exact argv vector to execute. Each entry uses the
    `safeCommandString` redaction guard; this is the only field a
    consumer should pass to `Command::new`/`spawn`.
  - `shellRequired` — `false` for safe argv execution; `true` for
    commands that genuinely need shell evaluation.
  - `copySafety` — one of `safe_structured_argv`, `display_only`,
    `shell_required_review`, `forbidden_until_human_approval`. The
    schema's `allOf` cross-check forbids `shellRequired=true` paired
    with `safe_structured_argv`, and forces `shell_required_review`
    or `forbidden_until_human_approval` whenever `shellRequired` is
    `true`.
  - `mutatesState` — `true` for any command that writes Beads, sends
    Agent Mail, mutates git, runs Cargo, or otherwise changes durable
    state. Consumers must require explicit confirmation before
    invoking a mutating action without prior human review.
  - `requiredSubstrate` — `agent_mail`, `beads`, `bv`, `ee`, `git`,
    `human`, `jq`, `rch`, `static_local`, or `none`.
  - `when` — short trigger predicate (also `safeCommandString`).
  - `rationale` — one-line reason (max 240 chars). Must not embed PEM
    blocks, GitHub PATs, `DATABASE_URL=` literals, or mail headers.

- `recommendedAction.suggestedCommandActions[]` is the canonical
  argv-bearing surface for agent-recommended next steps.
  `recommendedAction.suggestedCommands[]` remains for human display
  during migration but MUST NOT be passed to a shell — consumers
  prefer `suggestedCommandActions` when both are present.

- `verification.requiredCommands[].commandAction` and
  `verification.staticChecks[].commandAction` carry the same shape so
  a harness can replay verification commands without parsing the
  legacy `commandTemplate` string. The existing `commandTemplate`
  field is now explicitly marked legacy display-only in the schema
  description.

Redaction invariant: every `safeCommandString` slot (`displayCommand`,
each `argv[]` entry, `when`, `rationale`) blocks raw home paths,
PEM blocks, GitHub PATs, `DATABASE_URL=` strings, and mail headers
(`From:`, `Subject:`, `Message-ID:`). The
`work_packet_command_actions_require_shell_safe_argv_contract`
lifecycle test fences the definition + every reference site + the
legacy `commandTemplate` marker text.

Tracking Bead: `bd-2z5ly.2`
