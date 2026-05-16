# Swarm Fixture Corpus

Schema: `ee.swarm_fixture_corpus.v1`

The fixture corpus manifest is the contract for deterministic swarm-load
fixtures used by ranking, coordination, context-pack, and benchmark gates. It
keeps the large 16/64/256-agent scenarios reproducible without committing huge
generated corpora.

The committed manifest lives at
`tests/fixtures/swarm_scale/corpus_manifest.json` and is validated by
`tests/swarm_fixture_corpus_unit.rs`. The public schema lives at
`docs/schemas/swarm/ee.swarm_fixture_corpus.v1.json`; the test-specific schema
copy lives beside the manifest so fixture-only checks can run without loading
the broader schema catalog.

Required invariants:

- `seedFamily` and `fixedClock` pin deterministic generation.
- `hashEmbedder` describes the deterministic hash embedder used for fixture
  vectors.
- `volatileFieldDenylist` lists wall-clock fields that must not appear in
  hash-sensitive fixture sections.
- `scales` covers the 16-agent micro corpus, 64-agent smoke and 10k benchmark
  corpora, and the 256-agent large corpus.
- `scenarioStates` covers dirty worktrees, coordination-source freshness,
  Beads graph shapes, Agent Mail snapshots, and RCH pressure snapshots.
- `expectedPacks` points at golden pack outputs with stable top memory IDs.

Useful checks:

```bash
jq '.scales[] | {name, agentCount, memoryCount}' \
  tests/fixtures/swarm_scale/corpus_manifest.json

jq '.scenarioStates | keys' tests/fixtures/swarm_scale/corpus_manifest.json
```

Related schemas: `ee.resource.profile.v1`, `ee.pack.slo.v1`,
`ee.coordination_snapshot.v1`, `ee.producer.metadata.v1`.

Non-goals:

- This schema is not a live data-ingest format.
- The normal test gate must not materialize 100k memories.
- The corpus must not require live Agent Mail, live RCH workers, paid APIs, or
  downloaded embedding models.
- Runtime-only timestamps and measured durations are excluded from
  hash-sensitive fixture sections.
- Validation rejects any key named by `volatileFieldDenylist`; fixtures must
  use fixed seed and clock fields instead of runtime wall-clock fields.

Tracking Bead: `bd-1zb7k.6`
