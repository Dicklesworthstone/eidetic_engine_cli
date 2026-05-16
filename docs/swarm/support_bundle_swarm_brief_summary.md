# Support Bundle Swarm Brief Summary

Schema: `ee.support_bundle.swarm_brief_summary.v1`

Support bundles include a compact, redacted summary derived from
`ee.swarm.brief.v1`. The ownership posture appears under
`fileSurfaceRiskSummary`, with counts by severity, reservation holder, and git
status bucket plus the top risks represented by hashes and IDs.
The duplicate-work posture appears under `singleFlight` using the same
redaction-safe `ee.singleflight.posture.v1` shape exposed by `ee status` and
`ee doctor`, so handoff readers can see active leaders, follower waits,
timeouts, failures, reused-result counts, surface names, and key hashes without
raw queries or memory content.

Versioning: field renames or incompatible redaction semantics require a new
schema version and a migration note. Additive counts may remain in
`ee.support_bundle.swarm_brief_summary.v1` when existing consumers can safely
ignore them.

Redaction rules: counts, severity labels, risk codes, reservation subjects,
Bead IDs, path hashes, command hashes, single-flight key hashes, surface names,
and generation counters are allowed. Raw paths in the top-risk summary, raw
commands, mail bodies, raw logs, raw memory content, raw query text, env dumps,
file contents, and secret-like tokens are not allowed.

Example:

```bash
ee support-bundle create --redacted --json
```

Fixture catalog: `tests/fixtures/swarm/ownership_posture_cases.json` covers the
compact summary shape for healthy, degraded-source, and unattributed-blocker
ownership posture cases.

Related schemas: `ee.swarm.brief.v1`, `ee.swarm.recommendation.v1`.

Non-goals: support-bundle summaries do not expose full file listings, recover
mail bodies, preserve raw query text, or replace the full swarm brief when local
inspection is safe.

Tracking Bead: `bd-1zb7k.16.4`
