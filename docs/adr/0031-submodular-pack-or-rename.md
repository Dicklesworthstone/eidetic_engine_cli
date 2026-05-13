# ADR 0031: Submodular Pack Assembly — Audit, Don't Certify

Status: accepted
Date: 2026-05-13

## Context

`ee context` and `ee pack build` ship a `selectionCertificate` field
inside every context-pack response. Today, that certificate carries
`monotone`, `submodular`, `guarantee_status`, `algorithm`, and
`guarantee` strings that purport to attest *mathematical properties of
the selection method*. Two concrete shapes live in
`src/pack/mod.rs`:

- The `submodular` profile path (`src/pack/mod.rs:2640-2654`) emits
  `objective: FacilityLocation`,
  `algorithm: "deterministic_greedy_facility_location_gain_per_token"`,
  `guarantee: "monotone submodular facility-location objective;
   deterministic budgeted greedy certificate, exact optimum not claimed"`,
  `guarantee_status: Conditional`, `monotone: true`, `submodular: true`.
- The MMR-based profiles (`src/pack/mod.rs:2510-2517` and equivalent in
  `src/core/context.rs:4422-4423`) emit `monotone: false`,
  `submodular: false`, `guarantee_status` set per call site.

The 2026-05-10 walking-through audit
(`bd-17c65` reality-check epic body) called this out as an honesty
fail:

> The selection certificate self-declares `monotone: false,
> submodular: false, guaranteeStatus: "conditional"` — i.e. the
> algorithm reports it does NOT satisfy its claimed guarantee. We're
> shipping ~6KB of certificate machinery to attest that we're not
> certifying anything.

`bd-17c65.14.5` (N5) was filed against this. Its sub-bead
`bd-17c65.14.5.1` (this ADR) was tasked with making the choice between
two resolutions before any implementation begins:

- **Resolution A (PROVE)** — keep the name `selectionCertificate`,
  switch every selection path to a provably monotone-and-submodular
  facility-location objective, prove the property formally
  (e.g. through a small Lean artifact under `docs/proofs/`), and gate
  the proof in CI. The `guarantee_status: Conditional` field becomes
  `Valid` after the proof.
- **Resolution B (RENAME)** — drop the certificate framing. Rename
  `selectionCertificate` to `selectionAudit` across the canonical pack
  tree, JSON contracts, goldens, and docs. Drop the `guarantee` and
  `guarantee_status` strings; replace with `algorithmId` and
  `algorithmDescription` fields that describe the actual selection
  method without promising mathematical properties the user did not
  ask for.

The decision affects every downstream consumer of the pack envelope
and is the principal blocker on `bd-17c65.14.5.2` (N5.1) implementation
work.

## Decision

We adopt **Resolution B (RENAME)**, with two refinements that preserve
the structural information the existing certificate carries.

### What changes

1. The field is renamed: `pack.selectionCertificate` →
   `pack.selectionAudit` in every canonical-pack envelope, every
   goldens file under `tests/snapshots/`, every JSON-schema artifact
   under `docs/schemas/`, every README and ADR cross-reference, and
   every `--format` renderer.
2. The `guarantee` and `guarantee_status` strings are dropped from the
   audit struct.
3. The structural booleans `monotone` and `submodular` are kept inside
   `selectionAudit` as *descriptive* properties of the algorithm
   actually used on this call, not as guarantee attestations. The
   facility-location path keeps reporting `monotone: true,
   submodular: true`; the MMR paths keep reporting `false, false`.
   These booleans are useful introspection for an agent that wants
   to know which selection method ran.
4. New required fields `algorithmId` (snake_case identifier,
   e.g. `deterministic_greedy_facility_location_gain_per_token`,
   `mmr_with_coverage_fill`) and `algorithmDescription` (one short
   sentence) replace the prose `algorithm` and `guarantee` strings.
5. The envelope schema version increments. Pack envelopes carry
   `schema: "ee.context.v2"` (or the equivalent successor of whatever
   `bd-17c65.4.7` D7/A10 settles on) so older consumers parse the new
   shape correctly.

### What does not change

- The selection methods themselves. The facility-location greedy stays
  for the `submodular` profile; the MMR coverage-fill objective stays
  for `balanced`, `compact`, and `thorough`. The math is whatever it
  was; the framing is now honest.
- The persisted pack-replay ledger format (ADR 0025) is not affected
  except that its `selectionCertificate` references are renamed
  consistently.
- The `objective: FacilityLocation | Mmr | …` enum stays. It is the
  ground truth for which method ran; `algorithmId` is its
  user-facing string projection.

## Consequences

What becomes easier:

- **The contract becomes honest.** A pack response no longer carries a
  certificate that simultaneously claims and disclaims its own
  guarantee. An agent reading `selectionAudit` sees an audit log of
  *what method was used and what its descriptive properties are*, not
  a (broken) mathematical attestation.
- **Refinement is unblocked.** N5.1 ships a mechanical rename + schema
  bump rather than a Lean proof effort. The cost is bounded and
  predictable.
- **MMR profiles stay first-class.** Resolution A would have pressured
  us to fold MMR into facility-location or to fork the certificate
  shape per profile. Resolution B treats every profile uniformly.
- **D-series schema coordination is one rename.** `bd-17c65.4`
  canonical pack tree and `bd-17c65.4.7` envelope schema bump absorb
  the rename in their landing window without inventing a new
  guarantee taxonomy.

What becomes harder:

- **Resolution B is a breaking change for envelope consumers** that
  parse `selectionCertificate`. Per AGENTS.md ("we do not care about
  backwards compatibility"), this is acceptable for pre-1.0. The
  envelope schema-version bump signals the change.
- **Formal-guarantee fans lose the option of a proven certificate.** A
  future ADR can revisit and add `selectionProof` as a separate field
  alongside `selectionAudit`, gated on a Lean artifact, *if* there is
  a concrete consumer who needs the proof. There is no such consumer
  today.
- **Three documentation sites** (README, `docs/pack-replay.md`,
  `docs/adr/0025-replayable-context-pack-selection-ledgers.md`)
  reference the certificate by name. Each must be edited in the N5.1
  PR.

What becomes intentionally impossible:

- Shipping a `guaranteeStatus: Conditional` field that means "the
  algorithm reports it does not certify anything." The status field
  is deleted; there is nothing to be conditional about.

## Rejected Alternatives

### Resolution A — Prove and keep the certificate

Switch every selection path to a single, provably monotone and
submodular weighted facility-location objective, prove the property
in Lean under `docs/proofs/submodular_pack.lean`, and gate the proof
in CI. The certificate's `guarantee_status` becomes `Valid` after the
proof.

Rejected because:

- **MMR is not submodular in general.** Forcing every profile into
  facility-location either changes selection behavior in user-visible
  ways or requires us to ship two parallel algorithms and pick by
  profile — which is what we have today. Resolution A only honestly
  works if we *delete* the MMR profiles, and the MMR profiles produce
  better packs for the `balanced`/`compact` use cases empirically
  (this is why they exist).
- **The cost of a Lean proof is real.** The
  `lean-formal-feedback-loop` skill exists, but threading Lean into CI
  for one math property is large infrastructure for one bead's
  payoff. The cost would be measured in person-days for the proof
  alone, plus ongoing Lean-version maintenance.
- **No downstream consumer asked for a verified guarantee.** No agent
  harness today reads `guarantee_status` and changes behavior on
  `Valid` vs `Conditional`. We would be paying for a proof that no
  user observes.
- **Resolution A does not retire the honesty fail.** Even with a
  proof, calling the field a "certificate" overpromises. A certificate
  in security or cryptographic usage is a signed assertion of
  identity, not a math property. The name is wrong for what it does
  regardless of whether the math is sound.

### Resolution C — Keep the field name, drop only the guarantee strings

Keep `selectionCertificate` as the field name; drop only the
`guarantee` and `guarantee_status` strings. Leave the booleans.

Rejected because the field name `selectionCertificate` is itself the
honesty problem the audit identified. Half-renaming is the worst of
both worlds: existing consumers still see the misleading name, and
new consumers do not get a clean rename.

### Resolution D — Per-profile field shape

Emit a `selectionProof` field for the facility-location profile (with
the proof artifact reference) and a `selectionAudit` field for the
MMR profiles, picking which one per call.

Rejected because:

- A polymorphic top-level field complicates every renderer and every
  schema validator.
- It still leaves us with the Lean proof obligation for the
  facility-location case, with the same cost objection as Resolution
  A.
- An agent reading a pack response should not have to branch on
  which field is present to find the selection record.

## Verification

The verification contract is split between the ADR-presence checks
that this sub-bead (`bd-17c65.14.5.1`) owns and the implementation
checks that `bd-17c65.14.5.2` (N5.1) owns.

This ADR ships with three static tests under the closure contract:

1. `tests/adr_0031_present.rs` asserts that
   `docs/adr/0031-submodular-pack-or-rename.md` exists.
2. `tests/adr_0031_required_sections.rs` asserts the file contains
   `## Context`, `## Decision`, `## Consequences`, `## Rejected
   Alternatives`, and `## Verification` headers.
3. `tests/adr_0031_alternatives_listed.rs` asserts the rejected
   alternatives section enumerates at least three concrete
   alternatives (A, C, D above) with explicit rejection rationale
   per AGENTS.md ADR requirements.

When N5.1 lands the implementation, additional verification is owned
by that bead:

- `tests/selection_audit_renamed_unit.rs` — no occurrence of the
  literal string `selectionCertificate` in canonical-pack codegen
  paths, JSON output, or top-level renderer matches under
  `tests/snapshots/`.
- `tests/snapshots/pack_selection_audit.snap` — golden showing the
  new field name and shape.
- `tests/pack_schema_version_bump_unit.rs` — pack envelope schema
  version bumped in the same PR as the rename.

CI gate via `verify.sh` contract stage. The closure-lint script
(`scripts/closure-lint.sh --audit --json`) treats `bd-17c65.14.5.1` as
closed only when the three ADR-presence tests pass; treats
`bd-17c65.14.5.2` as closed only when the rename tests pass and the
literal string `selectionCertificate` is absent from non-deprecated
paths.

## Related

- `bd-17c65.14.5` — N5 umbrella feature bead.
- `bd-17c65.14.5.2` — N5.1 implementation sub-bead (this ADR gates it).
- ADR 0007 — Context packs as primary UX.
- ADR 0025 — Replayable context pack selection ledgers (cross-reference
  rename in `bd-17c65.14.5.2`).
- `bd-17c65.4` — canonical pack tree (D-series); coordinates the
  rename.
- `bd-17c65.4.7` — D7/A10 envelope schema bump; absorbs the rename.
- `bd-17c65.1.2` — A2 algorithm self-description (overlaps with this
  decision; A2's existing `algorithm: &'static str` field is supplanted
  by the new `algorithmId` + `algorithmDescription`).
