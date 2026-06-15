# Harness Conformance

Use the harness conformance fixtures before enabling or changing agent hooks in
a real harness. The contract is `ee.harness_conformance.v1`, with fixtures in
`tests/fixtures/harness_conformance/` and committed golden summaries in
`tests/fixtures/golden/harness_conformance/`.

## What To Check

1. Inspect the hook install audit before installing:

   ```bash
   ee hook claude-code --print --workspace . --json
   ee hook codex --print --workspace . --json
   ```

   Check `data.harnessInstall.installAudit` for the generated events, matcher
   coverage, degraded entries, and suggested repairs.

2. Run the conformance golden test through RCH only:

   ```bash
   eval "$(scripts/rch_lane_doctor.sh --emit-env)"
   scripts/rch_verify.sh --summary --no-write --bead-id bd-i0iiw.4 -- cargo test --test harness_conformance_golden -- --nocapture
   ```

   Do not run local Cargo for this proof. If the RCH lane fails before Cargo,
   preserve the exact blocker string in the bead closeout and cite `bd-37ugy`.

3. Review deliberate fixture changes:

   - Update `docs/schemas/ee.harness_conformance.v1.json` only when the
     contract vocabulary changes.
   - Update `tests/fixtures/harness_conformance/*.json` when a supported hook
     event, transcript policy, or assertion expectation changes.
   - Update `tests/fixtures/golden/harness_conformance/*.json.golden` in the
     same change when real simulator output changes.
   - Update `tests/fixtures/harness_conformance/PROVENANCE.md` when a fixture
     source, generation method, or proof command changes.

## Required Properties

- Fixture transcripts are bounded, redacted excerpts, never raw harness logs.
- `expected.localCargoFallbackAllowed` stays `false` in every fixture.
- The simulator may execute only non-destructive hook commands.
- API keys, bearer tokens, private key blocks, and private absolute paths must
  be redacted before they appear in fixture or golden content.
- Non-zero tool exits are conformance successes when the fixture expects a
  failure path and the hook preserves the bounded error envelope.

The claim gate can degrade independently of the fixture contract. If
`ee swarm work-packet --claim-gate` refuses solely because the leaf is hidden by
the parent epic or reports stale `agent_mail_unavailable` while Agent Mail is
healthy, use the documented degraded-gate fallback: reserve files, announce the
claim, update the bead to `in_progress`, and keep proof RCH-only.
