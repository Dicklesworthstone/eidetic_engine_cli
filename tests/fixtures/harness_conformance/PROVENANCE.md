# Harness Conformance Fixture Provenance

Schema: `ee.harness_conformance.v1`
ADR: `docs/adr/0075-harness-conformance-contract.md`
Golden summaries: `tests/fixtures/golden/harness_conformance/*.json.golden`

These fixtures are hand-authored synthetic conformance cases for supported
agent harness hook events. They are not copied raw transcripts. Each fixture
uses bounded redacted excerpts and a non-destructive command template so the
simulator can verify behavior without exposing private paths, secrets, or live
session content.

Fixture history:

- `bd-i0iiw.1`: introduced the schema, vocabulary, fixture matrix, and schema
  unit coverage.
- `bd-i0iiw.2`: added the process-based hook-event and transcript simulator.
- `bd-i0iiw.3`: connected doctor/install-audit reporting to the same
  conformance vocabulary.
- `bd-i0iiw.4`: added committed golden summaries and this provenance record.

Update policy:

- Add or change fixtures only when a supported harness event, assertion, or
  transcript policy changes.
- Keep `expected.localCargoFallbackAllowed` set to `false` in every fixture.
- Never place API keys, bearer tokens, private key blocks, private absolute
  paths, or raw session logs in fixtures or goldens.
- Review `docs/agent-ux/harness-conformance.md` when changing proof commands or
  artifact locations.
- Verify through RCH only:

  ```bash
  eval "$(scripts/rch_lane_doctor.sh --emit-env)"
  scripts/rch_verify.sh --summary --no-write --bead-id bd-i0iiw.4 -- cargo test --test harness_conformance_golden -- --nocapture
  ```

If RCH fails before Cargo, preserve the exact blocker text in the tracker and
cite `bd-37ugy`. Never replace that proof with local Cargo.
