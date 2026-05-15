# Swarm Incident Drills

Schema: `ee.swarm_incident.v1`

Tracking Bead: `bd-1zb7k.14.1`

Swarm incident fixtures rehearse failure combinations that appear during large
agent runs without depending on live outages. The fixtures are synthetic JSON
records that describe substrate posture, expected degraded codes, safe recovery
actions, redaction expectations, and the invariants a replay harness must
preserve.

The committed fixture catalog lives in `tests/fixtures/swarm_incidents/`. The
public schema lives at `docs/schemas/swarm/ee.swarm_incident.v1.json`, and the
first schema example is mirrored in
`tests/fixtures/swarm_schemas/all_examples.json`.

Required invariants:

- `fixedClock` pins deterministic replay time.
- `substrates` always covers Agent Mail, Beads, RCH, disk, and hot-path
  posture, even when a substrate is `not_applicable`.
- `expectedDegraded` lists every degraded code the replay surface must emit.
- `expectedRecoveryActions` are ordered, evidence-linked, and non-destructive
  by default.
- `assertions.noLiveServices`, `assertions.noLocalCargo`,
  `assertions.noDeletion`, and `assertions.noMutation` must be true for every
  committed fixture.
- Redaction expectations describe whether fixture paths are synthetic or
  home-path-redacted, and fixtures must not contain secrets.

Initial scenarios:

- `agent_mail_unavailable`
- `beads_jsonl_ahead_of_db`
- `rch_topology_blocked`
- `disk_pressure_external_target_ok`
- `hot_path_burst_admission`

Useful checks:

```bash
jq empty tests/fixtures/swarm_incidents/*.json
jq '.examples["ee.swarm_incident.v1"].scenarioId' \
  tests/fixtures/swarm_schemas/all_examples.json
```

Related schemas: `ee.coordination_snapshot.v1`,
`ee.verification.evidence.v1`, `ee.resource.profile.v1`,
`ee.pack.slo.v1`, `ee.swarm.recommendation.v1`.

Non-goals:

- This schema is not a live incident log.
- Replaying a fixture must not send Agent Mail, mutate Beads, run RCH jobs, run
  local Cargo, clean disk, delete files, or require a daemon.
- Recovery actions are recommendations for agents or humans, not autonomous
  repair execution.
- Fixture replay must not expose raw home paths, secrets, mail bodies, or noisy
  command transcripts in context packs.
