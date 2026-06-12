# Same-File Proof Debt (`unproved_same_file_source_debt`)

Tracking beads: `bd-1n3x1.15.1` (this contract) / `bd-1n3x1.15.2` (fixture
corpus and detector harness) / `bd-1n3x1.15.3` (claim-gate and work-packet
wiring). Status: contract-first; nothing in this document is shipped until
`bd-1n3x1.15.3` lands, and implementation agents must treat this page as the
field vocabulary of record rather than inventing ad hoc fields.

## Motivation

Live claim-gate probes already fail closed in crowded checkouts, but they fail
closed *generically*: `dirty_checkout_path_count`, `related_bead_collision`,
`agent_mail_unavailable`, and similar reasons do not distinguish the motivating
condition — a candidate bead that likely edits the same relative path as a
related bead whose work is **source-complete but unproved** because RCH or the
environment blocked Cargo, not because Rust produced a verdict. (Observed
2026-06-09: candidates around `src/cli/insights/mod.rs` while `bd-2pos6.2`
held source-complete same-file work with its Cargo proof blocked by the
environment.) Claiming over such work duplicates or clobbers a peer's
nearly-landed change; the gate needs a *named*, *bounded*, *advisory* signal.

## Emitting situations

Same-file proof-debt evidence MUST be emitted only when **all** of the
following hold:

1. A related bead is `blocked` or `in_progress` (tracker state, stale-safe
   read acceptable when flagged).
2. That bead carries structured source-complete evidence: a source-complete
   comment, an RCH proof-blocker comment with degraded/error codes, or a
   verify-ledger blocker row attributable to it.
3. The candidate bead likely touches the same relative path, established by
   current dirty paths, an active or recently expired reservation, or
   structured bead path evidence — not prose alone.
4. No authoritative reservation/owner resolution already settles the overlap
   (an authoritative live reservation handoff supersedes this signal).

## Evidence vocabulary

All fields are advisory coordination evidence, never a source compile/test
verdict. Sorted field list for the bounded detail object:

| Field | Req | Shape |
| --- | --- | --- |
| `candidateBeadId` | MUST | bead id being gated |
| `relatedBeadId` | MUST | bead id owning the unproved same-file work |
| `pathPreview` | MUST | bounded **relative** path preview; never absolute, never private |
| `proofStatus` | MUST | `unproved` \| `source_complete_proof_blocked` |
| `proofBlockerCodes` | MUST | bounded codes extracted from RCH comments / verify-ledger rows (e.g. `rch_verify_topology_blocked`) |
| `evidenceSources` | MUST | sorted source kinds consulted (e.g. `beads_comment`, `dirty_path`, `reservation`, `verify_ledger`) |
| `confidence` | MUST | `high` \| `medium` \| `low` per the evidence class that established the path relation |
| `recommendedAction` | MUST | `coordinate_before_claim` |
| `nextCommandActions` | SHOULD | bounded structured commands: stale-safe `br show`, Agent Mail coordination, RCH proof inspection |
| redaction posture | MUST | no absolute private paths, raw logs, raw mail bodies, or full diffs (see below) |

## Effect on the claim gate

The signal can only make `safeToClaim` **false or null — never true**. It
composes with, and never replaces or masks, every existing unsafe reason:
dirty/stale checkout, Agent Mail posture, tracker authority, install
freshness, and RCH admission blockers all stay present (sorted, deduped) when
same-file proof-debt evidence is added.

When the related bead's proof was blocked before Cargo ran, any source-verdict
text in the surrounding packet MUST remain `unknown` / `not_reached_cargo`;
proof debt is coordination evidence, not a compile outcome. The RCH-only proof
rule is unchanged: nothing in this contract authorizes local Cargo.

## Abstention cases

The detector MUST abstain (emit nothing) when:

- no candidate-to-path evidence exists at all;
- the only dirty path overlap is unrelated to the related bead's surface;
- bead comments are stale or ambiguous prose with no current dirty path,
  reservation, or structured path evidence (SFPD-SHOULD-003);
- the only path evidence is a private absolute path (redaction wins);
- an authoritative reservation handoff already resolves ownership.

## Surface decision

First landing (`bd-1n3x1.15.3`) is **internal claim-gate / work-packet
evidence only**: the bounded detail object rides the existing claim-gate
unsafe-reason list (named reason `unproved_same_file_source_debt`) and
work-packet internal evidence. No public JSON schema changes in the first
slice — no new `docs/schemas/` file, no `schema_list` golden change, no
`NORMALIZED_CLI_COMMAND_COUNT` change. If a later slice promotes the object
into a public envelope, it must update: the work-packet schema doc
(`docs/swarm/work_packet.md`), the source-authority snapshot source list
(`docs/swarm/source_authority_snapshot.md`), `docs/schemas/` for the affected
envelope, and the `schema_list` golden — in the same commit.

## Requirement matrix

| Id | Clause | Fixture id | Test focus |
| --- | --- | --- | --- |
| SFPD-MUST-001 | Positive same-file proof debt emits the named reason `unproved_same_file_source_debt`, not only generic dirty/collision reasons | `sfpd_positive_same_file_debt` | named reason present |
| SFPD-MUST-002 | Evidence carries `candidateBeadId`, `relatedBeadId`, bounded relative `pathPreview`, `proofStatus`, `proofBlockerCodes`, `evidenceSources`, `confidence`, `recommendedAction=coordinate_before_claim` | `sfpd_positive_same_file_debt` | field completeness |
| SFPD-MUST-003 | Signal can only drive `safeToClaim` to false/null, never true | `sfpd_positive_same_file_debt` | verdict monotonicity |
| SFPD-MUST-004 | Source verdict stays `unknown`/`not_reached_cargo` when RCH failed before Cargo | `sfpd_positive_same_file_debt` | no inferred source failure |
| SFPD-MUST-005 | Redaction: no absolute private paths, raw logs, raw mail bodies, full diffs | `sfpd_redaction_bounds` | leak scan |
| SFPD-MUST-006 | When source authority is non-current (stale installed `ee`, candidate only from stale BV output), SFPD may add bounded context but MUST NOT produce a claimable verdict or mask `install_freshness` / tracker / dirty-checkout reasons | `sfpd_existing_authority_blockers_preserved` | existing reasons preserved, sorted/deduped |
| SFPD-SHOULD-001 | Existing dirty/stale/Agent Mail/RCH unsafe reasons appear alongside the named reason | `sfpd_existing_authority_blockers_preserved` | composition |
| SFPD-SHOULD-002 | `nextCommandActions` include stale-safe `br show`, Agent Mail coordination, RCH proof inspection when available | `sfpd_positive_same_file_debt` | structured next actions |
| SFPD-SHOULD-003 | Abstain when the path relation rests only on ambiguous prose | `sfpd_ambiguous_comments_abstain` | abstention |

Negative and compatibility fixtures completing the corpus for
`bd-1n3x1.15.2`:

- `sfpd_unrelated_dirty_path_negative` — dirty path present but unrelated to
  the related bead's surface: no emission.
- `sfpd_ambiguous_comments_abstain` — prose-only relation: no emission.
- `sfpd_redaction_bounds` — evidence built from inputs containing private
  absolute paths and raw logs: emitted object contains none of them.
- `sfpd_schema_golden_compat` — internal-evidence slice leaves every public
  schema and golden byte-identical.
- `sfpd_existing_authority_blockers_preserved` — stale installed `ee` (e.g.
  0.5.0 vs source 0.8.1) plus stale BV recommendation: existing
  `install_freshness` and tracker reasons remain present and sorted/deduped
  with SFPD evidence added.

An implementation is **not conformant** if any MUST row lacks fixture and test
coverage, or if the emitted object omits the redaction posture.

## Non-goals

No local Cargo fallback, no reservation mutation on behalf of either bead, no
automatic claiming or unclaiming, no prose scraping beyond structured comment
markers, no public schema change in the first slice.
