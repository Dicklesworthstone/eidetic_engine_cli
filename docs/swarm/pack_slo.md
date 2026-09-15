# Pack SLO

Schema: `ee.pack.slo.v1`

Pack SLO records compare actual pack work against the selected resource profile.
Agents use them to tell whether a pack was within budget, merely warning, or
failed a resource constraint.

`resourceStatus` compares deterministic candidate counts, graph work, and
admission to the profile limits. `elapsedStatus` compares the measured assembly
duration to `elapsedMsWarning` and `elapsedMsFailure`: equality reaches the
respective threshold. The target is advisory; below warning remains
`within_budget`. `status` is the worse of the two statuses, so a standard pack
with `actuals.elapsedMs: 24457` and `elapsedMsFailure: 2000` reports `failure`.

The measured interval is pack assembly, including selected-memory drift and
coordination work, rather than the entire CLI invocation. A timing failure is
diagnostic: selected content remains usable and command success is unchanged.
Only resource/admission `degradations` enter the pack's signed content and hash;
their messages contain no wall-clock measurements. A cache hit retains the
producer's elapsed measurement and statuses. Current lookup latency is exposed
by `--read-only --explain-performance` and tracing.

Example:

```bash
ee pack "release" --json | jq '.data.pack.slo'
```

Related schemas: `ee.resource.profile.v1`, `ee.coordination_snapshot.v1`.

Non-goals: SLO output is a report, not a retry loop or remote execution policy.
Arena-allocation metrics are planned as tracing/perf artifact fields first, not
as `ee.pack.slo.v1` fields; see `docs/pack-arena-assembly.md`.

Tracking Bead: `bd-1zb7k.5`
