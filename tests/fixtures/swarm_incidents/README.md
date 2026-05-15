# Swarm Incident Fixtures

These fixtures are synthetic `ee.swarm_incident.v1` scenarios for
cross-substrate outage and recovery rehearsal. They are not captured live logs.

Fixtures:

- `agent_mail_unavailable.json` - mail transport unavailable; Beads remains the
  fallback coordination trail.
- `beads_jsonl_ahead_of_db.json` - JSONL source of truth is ahead of the Beads
  SQLite cache.
- `rch_topology_blocked.json` - RCH fleet is healthy, but remote-required Cargo
  cannot start because path topology rejects the local alias layout.
- `disk_pressure_external_target_ok.json` - internal workspace space is low
  while Cargo target/temp scratch are correctly externalized.
- `hot_path_burst_admission.json` - pack concurrency exceeds the configured
  profile limit and must return structured backoff.

Fixture invariants:

- No live Agent Mail, Beads mutation, RCH job, local Cargo, file cleanup, or
  deletion is required to replay these scenarios.
- `assertions.noLiveServices`, `assertions.noLocalCargo`,
  `assertions.noDeletion`, and `assertions.noMutation` must stay true.
- Recovery actions are ordered, evidence-linked recommendations. They are not
  autonomous repairs.
- Real home paths and secrets are not allowed in committed fixtures.
