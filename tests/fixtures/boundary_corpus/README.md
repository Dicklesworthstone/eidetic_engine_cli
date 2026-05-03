# Boundary Fixture Corpus

This directory contains deterministic test-only fixtures for the mechanical
boundary migration. The catalog in `corpus.json` is intentionally small, but it
must cover every scenario class named by `eidetic_engine_cli-uiy3`.

Each fixture record declares:

- stable fixture ID and schema version;
- BLAKE3 hash of the fixture content;
- normalized UTC timestamp;
- provenance URI(s);
- redaction state and trust class;
- degraded codes when the scenario represents unavailable or stale backing data;
- prompt-injection quarantine posture;
- intended command coverage;
- explicit fixture-mode and normal-workspace leakage guards.

Normal `ee` workspace output must never include these records unless the command
is running in an explicit fixture or evaluation mode and says so in JSON.

The smoke contract in `tests/boundary_fixture_corpus.rs` writes a fixture-mode
summary under `target/ee-e2e/boundary_fixture_corpus/`. That summary records the
fixture name and hash, workspace path, DB/index generations, schema versions,
redaction classes, command matrix rows, stdout/stderr artifact paths, and
first-failure diagnosis.
