# CLOSE_THE_GAP_PLAN — `ee` (Eidetic Engine CLI) — **PART III, TWO-TRACK CONVERGENCE (2026-08-17; reality-check revision 2026-08-24)**

> Track A: mesh / team-confederation acceptance reconciliation after the Unix
> EE-to-EE campaign. Track B: core-product, durability, verification, and
> release convergence uncovered by the 2026-08-23 full-project reality check.
>
> **Status: ACTIVE (Part III).** Parts I and II (2026-05) are archived at
> `docs/archive/close_the_gap_2026-05.md`. This file is the in-place Part III
> revision required by `AGENTS.md` *Reality-Check Cadence*. Do not create a
> second plan file at the repo root.
>
> Companions: `docs/adr/0085-typed-pack-entity-identity.md`,
> `docs/adr/0086-team-memory-confederation.md`,
> `docs/mesh/team_confederation_plan.md`,
> `docs/mesh/verification_matrix.md`, `README.md`.
>
> **2026-08-23 audit addendum (full project reality check).** All remainder
> closures were re-verified against current Beads evidence. One Part III
> acceptance mismatch remains: `.3.8` closed on a live TWO-HOST tailnet soak
> whose own close note says "not two distinct humans", while §5 literally
> requires a two-human artifact. The earlier claim that `.3.9` had an empty
> close reason is stale: its current reason names the property/fuzz surface,
> remote host, duration, and commit. Windows, the publication fence, the
> narrowed fake-IdP v1 decision, remaining fuzz, and the program closeout all
> have recorded dispositions. Part III therefore cannot archive until the
> two-human criterion is either satisfied or explicitly amended.
>
> The same audit found a broader core-product bridge (§§6–13 below). The
> implementation is substantial, but the core CASS → retrieval → pack and
> curate → rule → retrieval loops are open, identical concurrent search/pack
> requests are not deterministic, current verification is red or inconclusive,
> and release/performance evidence is weaker than the README claims. This file
> remains Part III instead of opening a competing Part IV while §5 is unresolved.
>
> **2026-08-24 independent rerun addendum.** The reality check was repeated from
> the complete `AGENTS.md`, `README.md`, controlling plans/ADRs, implementation,
> tracker, installed release, hosted release/CI, and source-attested verification
> surfaces. Current `main` is `5962397d48b1e01aa4cc3886c91423d7830a9c18`
> (`v0.14.2-96-g5962397d`). Recent commits improve rule-body hydration, make
> unavailable upstream PPR degrade truthfully, and advance backup/export paths,
> but they do not implement ADR 0085 typed pack identity, calibrate lexical
> relevance, prove deterministic generations, complete recovery, or close any
> child of `bd-reality-core-convergence-1azkt`.
>
> A fresh installed-`0.14.2` walking-skeleton probe did persist, search, pack,
> and explain one manual procedural memory with provenance, confirming that the
> ordinary component path is real. The same isolated workspace reported
> inconsistent index/model readiness across status/search observations, so it
> does not upgrade the source-attested verdict. A pinned, clean-tree RCH
> `cargo check --locked --all-targets` reached Cargo on worker `vmi1152480` but
> ended `rch_environment_failure` after remote worker disk exhaustion; retry and
> local proof-ledger persistence also failed because the Mac had only 116 MiB
> free and the external build volume was absent. This is neither a green proof
> nor a source failure. Hosted CI for this exact SHA remains pending with zero
> jobs.
>
> The bridge was then re-run through three ambition rounds and five plan-space
> refinement passes: product closure, recovery/operability, adversarial proof,
> dependency order, observability/testing, privacy/failure handling, and agent
> journey/release maturity. No additional unowned controlling goal emerged.
> The existing epic plus reused blockers covers all 24 checklist rows; its 23
> records remain 20 open and 3 in progress, and `.22` still graph-blocks on every
> mandatory child and reused blocker. No duplicate plan or Bead was created.

---

## 0. Premise

The 2026-08-17 mesh-campaign reality check found an inverted tracker, not an
unbuilt product:

- Unix live EE-to-EE works on `main`: create/invite/join, inbound listen,
  `TcpMeshForegroundSyncTransport` EventFetch + grant-gated BodyFetch,
  hydrate, `--memory-scope team` search/pack, `teamProvenance`, P4.4/P4.5,
  US-5 last-sync/reachability.
- README and `docs/mesh/real_tailscale_smoke.md` still said the production
  supervisor used a no-op transport. That claim is false as of this Part III
  honesty edit.
- Beads still showed ~52 open `bd-tc-epic-qzk7o.*` children. Most of those
  slices are shipped. Open-count was being misread as "not built."
- Two-human Tailscale, Windows-host soak, production IdP vendor soak, T2.7
  frame/session fuzz beyond origin properties, and the T5.7 publication fence
  were the **real remainders** at Part III opening. All now have evidence or an
  allowed product decision except the literal two-distinct-human criterion.
  None is an excuse to rebuild transport.

**Non-negotiables for Part III:**

- Do not rebuild shipped Unix team-confed.
- Do not steal `bd-d67os.28` (NavyLotus; T5.7 fence).
- Do not start `bd-1nl13`.
- Do not invent a T6.7 ceremony. `.7.7` waits for the remainder children.
- Do not close the epic until the remainder children close.
- ADR 0086 Context stays historical (2026-07-30). Correct the plan and
  README, not the ADR's original problem statement.
- No file deletion. No worktrees. No local Cargo on this Mac.

---

## 1. What is already true

Unix product on `main` (proof ledger:
`docs/mesh/verification_matrix.md`):

| Surface | State |
| --- | --- |
| `ee team create` / `invite` / `join` | Live signed TCP; join first-sync imports origin genesis; invite `--wait` waits for it |
| Inbound listen | `ee mesh hello-responder run` / `ee daemon --foreground`; Tailscale LocalAPI or loopback `TeamJoinLocalApi` |
| Foreground sync | `TcpMeshForegroundSyncTransport` — not `Noop` |
| Unified recall | Authorized BodyFetch hydrates stubs; search/pack/ask/why carry `teamProvenance` |
| Conflicts / insights / why | P4.4 precedence, T5.6 `peerConflicts`, P4.2 elevation, T5.8 origin-time invariance |
| Status | US-5 `lastSeenAt` + reachability (`self` / `never_synced` / `synced` / `soft_stale` / `hard_stale`) |
| Fake IdP + identity_attest | T7.1–T7.6 proven against the fake harness |
| Windows inbound compile | `x86_64-pc-windows-gnu --lib` compiles; TeamJoin TCP is not Unix-gated |

---

## 2. Original Part III remainder ledger — current disposition

| Gap | Bead | Current disposition |
| --- | --- | --- |
| Two distinct humans on a real Tailscale tailnet exchange memory; US-4 search/pack works; cursors advance; no deferred sync code | `bd-tc-epic-qzk7o.3.8` (T2.6) | **Criterion mismatch.** Closed on a valid two-host tailnet artifact, explicitly not two humans. Resolve through the §10 decision/proof bead before archive. |
| Frame/session/bootstrap fuzz beyond `tests/property_origin_stream.rs` | `bd-tc-epic-qzk7o.3.9` (T2.7) | Closed with frame/session/bootstrap properties and fuzz, MAC-before-counter proof, RCH host/duration, and commit `acc230aa`. |
| Source-snapshot publication fence | `bd-d67os.28` then `.6.7` | Both closed; `.6.7` records coalesced intake plus the source-snapshot publication fence. |
| Windows-host DACL / inbound crash / owner-only key-path | `bd-tc-epic-qzk7o.12` + `.2.4` | Closed with a retained Windows-host DACL and crash/restart artifact. Current cross-platform CI remains a separate release-readiness concern. |
| Production Entra / Okta / Google IdP soak | `bd-tc-epic-qzk7o.8.8` | Closed under §5's allowed explicit decision: fake IdP is the v1 ceiling; a vendor soak is post-v1 unless that decision changes. |
| Program closeout | `bd-tc-epic-qzk7o.7.7` (T6.7) | Closed, as are the milestone parents and root epic, despite the unresolved `.3.8` wording mismatch. |

---

## 3. Tracker policy for this bridge

1. Close a shipped `bd-tc-epic-qzk7o.*` child only with verification-matrix
   evidence (test name + isolated host + duration + commit). No abstention
   close. No "docs-only" close of an implements-surface bead.
2. Split environment remainders into explicit children instead of leaving
   fifty implementation beads open.
3. Historical instruction: keep `.3.8`, `.3.9`, `.2.4`, `.6.7`, `.7.7`,
   `.12`, `.8.8`, affected milestone parents, and the epic open until their
   proof rows were resolved. Those records are now closed; do not pretend that
   tracker state by itself amended `.3.8`'s written two-human acceptance.
4. Unblock `.2.4` from "blocked" once T5.9's body-approval consumer is on
   `main`. Remaining work is Windows key-path, not missing Unix crypto.
5. Comment `.6.7` that protocol tests passed and the fence stays
   `bd-d67os.28`.
6. After README honesty lands, close `.2.5` (T1.7). That bead existed to
   stop README from lying about mesh.

---

## 4. Docs honesty landed in this Part III opening

- `README.md` Mesh / Team / Limitations / FAQ now describe live Unix
  `TcpMeshForegroundSyncTransport` and name the remainders.
- `docs/mesh/real_tailscale_smoke.md` no longer claims a no-op transport.
- `docs/mesh/operator_onboarding.md` points at `ee team` and the ledger.
- `docs/mesh/verification_matrix.md` has an explicit remainder table.
- `docs/mesh/team_confederation_plan.md` status line matches `main`.
- ADR 0086 historical Context is **not** rewritten.

---

## 5. Original mesh close criteria, extended by the full closeout in §15

These were the mesh-only criteria at Part III opening. All rows except the
literal two-human wording now have an evidence-backed disposition. They remain
historical inputs to §15 rather than a second, competing close gate.

Archive this file to `docs/archive/close_the_gap_2026-08.md` and start Part IV
**in this same path** only when these criteria and §15 are both satisfied:

- `.3.8` has a two-human Tailscale proof artifact.
- `.12` has a Windows-host soak artifact (or an explicit fail-closed
  product decision recorded in the matrix).
- `.8.8` has a production IdP soak artifact (or an explicit "fake-IdP is
  the v1 ceiling" decision).
- `.3.9` either grows the remaining fuzz or is narrowed and closed with
  the origin-slice evidence plus a filed follow-up.
- `bd-d67os.28` closes and `.6.7` reuses the fence (or `.6.7` is rewritten
  as honesty-only with a new implements-surface sibling).
- `.7.7` can then write the T6.7 rollup without inventing ceremony.
- README / smoke / matrix still match the code.

Until then, this file stays at the repo root.

---

## 6. 2026-08-23 full-project reality-check verdict

`ee` is **real, broad, and architecturally recognizable**, but it is **not
finished, not currently verified, and not yet delivering every controlling
promise end to end**.

This is not a stub-shell diagnosis. The repository contains real FrankenSQLite/
SQLModel persistence, Frankensearch integration, Asupersync runtime wiring,
FrankenNetworkX projections, stable response envelopes, provenance-rich pack
rendering, explicit degradation, audited curation/maintenance machinery, and a
large CLI/test surface. The forbidden dependency names do not appear in the
current `Cargo.lock`.

The decisive failures are at integration and proof boundaries:

1. CASS transcript spans are persisted and partial projection work exists, but
   job coverage, admissible indexing, typed hydration, and public closed-loop
   proof do not reliably make them searchable and packable.
2. Applied procedural rules are persisted and partially projected, but corpus/
   reembed accounting and native rule-item hydration remain incomplete.
3. Incremental evidence/linkage jobs can stamp a derived index whose actual
   document set is absent or stale.
4. Identical concurrent search and read-only pack calls can observe different
   embedding backends, index availability, selected memories, and pack hashes.
5. The named North Star and vision gates prove file/dispatch presence more
   readily than the exact public behavior they claim.
6. Current RCH and hosted CI evidence is red, cancelled, killed, or pending;
   no immutable current SHA has the required green readiness proof.
7. Release automation is disabled and the latest release lacks the signed/
   provenance asset set produced by the checked-in workflow.
8. README performance numbers are historical and not reproducibly tied to the
   current baseline file, raw samples, host fingerprint, or a release-blocking
   gate.
9. Two hard architecture boundaries have production or public residue: pack
   ranking uses a hand-written PPR implementation instead of FrankenNetworkX,
   and a custom BM25 implementation remains publicly exported even though its
   current callers are tests. The Frankensearch dependency/version contract is
   also internally inconsistent (`0.3.2` dependency versus `0.3.0` report).
10. Lexical BM25 is treated as unit-normalized and clamped, so distinct raw
    scores above one all become public `relevanceScore: 1.0`; downstream floor,
    pack-quality, and ask-confidence semantics can therefore call weak results
    maximally relevant or "good".
11. Backup creation fails on a legitimate targetless audit row. Dry-run can
    create authentication-key state, export omits much of the durable five-job
    model, and restore ignores even exported links/audits. The documented
    recovery surface is materially lossy.
12. CASS pack admission has an accepted design, ADR 0085: a positively admitted
    safe evidence span is a first-class typed pack entity, while unsafe or
    unclassified spans fail closed. Implementation and README wording have not
    converged on that contract.
13. Effective build inputs are not hermetic: sibling trees and semantic
    checkout-time patches are incompletely represented in provenance, release
    tooling has mutable inputs, and release Cargo builds omit `--locked`.

The honest summary is therefore: **the architecture and many component
surfaces work; useful retrieval, recoverable durable memory, the five-job
product loop, and release-readiness do not yet work reliably as one cohesive
product.**

---

## 7. Evidence snapshot and authority boundary

Source base under the latest audit: `main` at
`5962397d48b1e01aa4cc3886c91423d7830a9c18`
(`v0.14.2-96-g5962397d`). The tracked worktree was clean; pre-existing untracked
tracker journals, `.ci/`, and a Cargo manifest backup were left untouched.
Static claims below are source/contract findings, not a green-candidate proof.

Operational probes used `/Users/jemanuel/.local/bin/ee` version `0.14.2`,
SHA-256
`d7e50bc8831c29437fdc23bf6ff6e57e1b2131665a01c8af937dea02323857f5`.
Its `ee version --json` reports `gitCommit: null`, `gitTag: null`, and
`targetTriple: unknown`. Therefore the live runtime results are
**released-binary evidence against the current workspace**, not proof that
commit `5962397d` behaves identically. Source inspection contains a plausible
matching race. Bead `.10` must reproduce or refute it with a source-attested
candidate before `.2` changes the implementation.

### 7.1 Positive evidence

- The installed release binary emits `ee.response.v2` for ordinary status, capability,
  search, pack, ask, and diagnostic probes.
- `ee pack --read-only` emits item-level provenance, trust, relevance/utility,
  selection explanation, lifecycle, redaction, and degradation data.
- Offline/hash fallback is explicit rather than falsely labeled semantic.
- `ee ask` abstains when evidence confidence is inadequate.
- The store/index/status surfaces disclose stale generations and document
  counts rather than silently claiming ready.
- `Cargo.lock` contains none of `tokio`, `tokio-util`, `async-std`, `smol`,
  `rusqlite`, `sqlx`, `diesel`, `sea-orm`, `petgraph`, `hyper`, `axum`,
  `tower`, or `reqwest`.
- Core command dispatch and durable use-case implementations are real rather
  than TODO/unimplemented macros.
- The active Beads dependency graph has no active cycle.
- `closure-lint --audit --json` reports no formal violation, while this plan
  explicitly records what that linter does not prove.

### 7.2 Negative evidence

- In the installed release binary, eight identical concurrent search calls
  split between two `search_index` errors, three hash-fallback/no-result
  responses, two neural one-result responses, and one neural four-result
  response.
- In the installed release binary, six identical concurrent read-only pack
  calls produced four empty packs, one two-item pack, and one three-item pack
  with three distinct hashes.
- Current source still exposes a matching two-rename publication hole and
  process-local model admission; `.10` owns source-attested attribution.
- A live installed-binary lexical query returned distinct BM25 scores roughly
  `1.59..3.12`, all rendered as `relevanceScore: 1.0`. Current source and tests
  explicitly implement that saturation.
- `ee backup create --dry-run --json` failed with high-severity storage error
  `missing required non-empty field 'target_type'`. Current source shows the
  nullable audit/export mismatch, incomplete export inventory, lossy restore,
  and key-opening-before-dry-run behavior.
- The installed release reports source rows that do not become documents: 147
  evidence spans but zero indexed evidence documents and zero rule documents.
- A live Asupersync migration query returned either no result or unrelated RCH,
  stale-binary, and tracker-process memories instead of the required runtime
  rules. A release-preparation pack likewise lacked the promised complete
  project release context.
- `scripts/vision-coverage.sh --json` reports 134/134 implemented and zero
  gaps, but its mapping is based mainly on registered surfaces/file presence;
  `bd-2mpct.1` already records this proof weakness.
- The historical plan sweep labels North Star coverage verified by checking
  that two test files exist, while its own narrative says most scenarios were
  only partial.
- The verifier's basic `e2e_test.sh` header claims the walking skeleton, but
  its actual scenarios omit init/remember/search/pack/why and still assert the
  end-of-life `ee.response.v1` schema.
- Pack candidate relevance is changed by the local `src/graph/ppr.rs`
  algorithm rather than the mandated FrankenNetworkX graph layer; the public
  `src/search/bm25_simd.rs` residue contradicts the no-custom-BM25 boundary.
  Additional source audit found score-changing local fusion, global-store
  token-overlap retrieval, and locally recomputed diagnostic RRF.
- Recent RCH contract runs exited 101 or were stuck-detector cancelled; two
  North Star attempts exited 137. Current full check, clippy, full tests, and
  exact North Star behavior have no green current-SHA proof.
- Hosted CI has no successful substantive current-main verdict. The latest
  release workflow is disabled, and v0.14.2 has archives/checksums/installers
  but no Sigstore bundles or SLSA provenance assets.
- At the latest audit checkpoint, hosted push run `32731128435` for
  `5962397d` was `pending` with zero jobs. The one pinned clean-tree RCH check
  ended in an attributed environment failure (remote worker disk full, retry
  attempted, local proof-ledger write blocked by host disk exhaustion); no
  local Cargo fallback or duplicate verification run was started.

---

## 8. Vision checklist and current status

| # | Controlling promise | Status | Evidence / gap owner |
| ---: | --- | --- | --- |
| 1 | Local-first single CLI; core commands need no daemon | **WORKING** | Real direct CLI paths and source-backed storage exist. |
| 2 | Franken-stack foundations; no forbidden substitute dependencies or core algorithms | **PARTIAL / WRONG-APPROACH** | Static dependency scan is clean, but local PPR/fusion/global token overlap/diagnostic RRF cross hard boundaries; custom BM25 is public; version identity drifts. `.4`, `.15`, `.18`. |
| 3 | Manual memory → DB → search → pack → why | **PARTIAL** | The ordinary path and provenance exist. Released-binary probes are unreliable; source-attested attribution is `.10`, then `.2`/`.3`. |
| 4 | CASS import makes permitted prior incident content searchable and safely packable | **REGRESSED** | `bd-3k1mg`, `bd-16imy`, ADR 0085. Search projection is incomplete and typed live admission/provenance is not proven. |
| 5 | Hybrid BM25 + neural-local retrieval by default | **REGRESSED / ATTRIBUTION PENDING** | Released binary flips backend; current source has plausible causes; fresh-workspace fallback is `bd-fresh-workspace-hash-fallback-kvltg`. |
| 6 | Same declared snapshot gives byte-stable canonical JSON and pack hash | **REGRESSED / NO-BEAD at audit start** | Released-binary semantics diverged; source contract and attribution are `.1`, `.10`, `.2`, `.3`. |
| 7 | Retrieval scores and pack quality mean what their names claim | **REGRESSED / NO-BEAD at audit start** | Current code clamps unbounded lexical BM25 to `1.0` and uses incompatible domains for admission/quality. `.11`, `.12`. |
| 8 | Explainable packs with typed identity, provenance, freshness, trust, and score reasons | **PARTIAL** | Rendering is strong for admitted memories; rule/evidence v3 identity, calibration, and deterministic admission remain open. |
| 9 | Learn loop turns repeated evidence into a rule used by later search/pack | **REGRESSED** | `bd-3h6bz`; rule search work is substantial, but native rule item/content/identity and exact E2E remain open. |
| 10 | Maintain loop links, decays, consolidates, validates, repairs, and converges | **PARTIAL / UNPROVEN** | Decay/machinery exist; public consolidate → apply → index → retrieve is `bd-1oep7`. |
| 11 | Complete durable backup, verify, migration, restore, and rebuild | **REGRESSED / NO-BEAD at audit start** | Create fails on a valid audit; dry-run can mutate; export/restore omit core durable state. `.13`, `.14`. |
| 12 | Graceful offline degradation remains useful and truthful | **PARTIAL** | Honest fallback/abstention exists; released binary may instead return no result/error, and uncalibrated quality is misleading. |
| 13 | Stable machine envelopes and truthful repair exits | **PARTIAL** | `bd-34l8k`, `bd-3ak9b`, `bd-vv2dw`, `bd-aav4p`, `bd-5k6k7`; typed pack v3 is an intentional future break. |
| 14 | Exact eight North Star public-CLI scenarios | **UNPROVEN** | `bd-2mpct`, `bd-2mpct.1`, and bridge `.17`; current runs are red/killed and prior oracles are permissive. |
| 15 | Privacy/trust holds from ingest through index/model/pack/proof/backup/mesh | **PARTIAL / UNPROVEN** | ADR 0085 and source screening are strong; cross-source live admission, retained-generation, proof-sink, and recovery negatives remain. |
| 16 | Graph insight and optional adapters are real or explicitly degraded | **PARTIAL** | Core graph/team surfaces are substantial; placeholder insights remain under `bd-2pos6`; stable claims must close or demote in `.9`. |
| 17 | Multi-agent local writes preserve integrity and truthful freshness | **PARTIAL / UNPROVEN** | Strong tests exist; current full-suite proof is red and evidence/linkage/index generation gaps remain. |
| 18 | Unix team-confederation and documented environment posture | **PARTIAL** | Unix/two-host/Windows/fake-IdP evidence exists; Part III two-human wording and README disagree. `.8`. |
| 19 | Canonical readiness verification and green CI | **REGRESSED / NO-BEAD at audit start** | `.5` builds the runner, `.17` hardens oracles, `.19` owns the factual green-candidate claim. |
| 20 | Reproducible performance and usable first-agent latency | **UNPROVEN / NO-BEAD at audit start** | README baseline/footer drift, no raw provenance, advisory budgets, and remote/local driver mismatch. `.6`. |
| 21 | Hermetic multi-platform release/install chain | **PARTIAL / NO-BEAD at audit start** | Current release has archives/checksums/installers but no candidate checks/provenance set; workflow is disabled. `.7`, `.18`, `.20`, `.21`. |
| 22 | Canonical walking skeleton proves init → remember → search → pack → why | **REGRESSED** | Basic E2E omits the claimed flow and asserts v1. `.5`, `bd-2mpct`. |
| 23 | Recommended agent journey is coherent and fast without a daemon | **PARTIAL** | Five-command core exists, while README promotes broader overlapping flows and latency remains unproven. `.6`, `.9`. |
| 24 | No-silent-mutation lifecycle, helpful/harmful feedback, decay/inversion | **PARTIAL / UNPROVEN** | Machinery exists; exact later-pack behavior, audit semantics, and docs wording need behavioral proof or maturity demotion. `.9`, `.17`, `bd-2mpct`. |

---

## 9. Existing work that this bridge reuses

Do not duplicate these Beads. Their current acceptance text already describes
the implementation or proof slice needed:

| Concern | Existing Beads |
| --- | --- |
| Evidence/index generation truth | `bd-3k1mg`, `bd-index-auto-freshness-m5kwf` |
| CASS transcript search + pack hydration | `bd-16imy` |
| Applied procedural rule search + pack hydration | `bd-3h6bz` |
| Typed Memory/Rule/Evidence identity and admission design | Closed design bead `bd-12ubv`; accepted ADR 0085; implementation stays in `bd-16imy` / `bd-3h6bz` |
| Exact eight North Star flows and behavioral vision gate | `bd-2mpct`, `bd-2mpct.1` |
| Consolidation close-loop proof | `bd-1oep7` |
| Current red contracts/lib suites | `bd-g3yh5`, `bd-2yz9p`, `bd-1eeyw` |
| Stable doctor/Windows machine contracts and repair safety | `bd-34l8k`, `bd-3ak9b`, `bd-vv2dw`, `bd-aav4p`, `bd-5k6k7` |
| Fresh-workspace model cache resolution | `bd-fresh-workspace-hash-fallback-kvltg` |
| Remaining real insight sections | `bd-2pos6` and children |
| Installer matching-version repair/verification | `bd-xww0x` |
| Broad performance-gate hygiene | `bd-je0nb` |
| Warm public-search latency | `bd-search-warm-latency-0bh05` |
| Stable-surface maturity choices | `.9` must close the relevant blocker or demote the advertised surface; examples include `bd-rs4cm`, `bd-d67os.27`, `bd-resume-verb-v0f57`, `bd-orient-fast-content-iubub`, `bd-fyack`, `bd-3ap2m`, `bd-multiplicity-aware-trust-p0u7g`, `bd-d67os.19`, and `bd-degraded-advisory-noise-vfx8u` |

---

## 10. Newly uncovered work

The end-to-end reality check filed one self-contained bridge epic,
`bd-reality-core-convergence-1azkt`, with these dependency-linked children:

| Label | Priority | Gap | Required outcome |
| --- | ---: | --- | --- |
| `bd-reality-core-convergence-1azkt.1` | P0 | Determinism promise lacks a complete snapshot/numeric/serialization domain | Freeze canonical product payload versus telemetry/state-creation; version snapshot identity and pack hashing. |
| `bd-reality-core-convergence-1azkt.2` | P0 | Plausible reader-visible index hole and cross-process model race | Immutable content-addressed generations, atomic DB pointer, reader leases, publisher fences, bounded model admission, zero-mutation read-only. |
| `bd-reality-core-convergence-1azkt.3` | P1 | Equality-only tests miss invalid concurrent histories | Post-fix no-mock linearizability, crash, privacy, cache, platform, and resource matrix. |
| `bd-reality-core-convergence-1azkt.4` | P0 | Local PPR/fusion/global overlap/diagnostic RRF and version drift violate hard stack boundary | Exhaustive call-site classification; only Frankensearch/FrankenNetworkX changes retrieval/metrics. |
| `bd-reality-core-convergence-1azkt.5` | P0 | No single executable manifest/pinned composite RCH/proof-capsule contract | Build the verifier now; final green truth belongs to `.19`, avoiding a dependency deadlock. |
| `bd-reality-core-convergence-1azkt.6` | P1 | Public latency/SLO claims lack reproducible correct-output evidence | RCH-built attested candidate, local M3 black-box driver, raw samples/correctness, explicit reproduce-or-remove decision. |
| `bd-reality-core-convergence-1azkt.7` | P1 | Release staging is non-hermetic and publication currently precedes native installer proof | Private draft/local staging only; locked signed/provenance-complete assets and native smoke before human publish. |
| `bd-reality-core-convergence-1azkt.8` | P1 | Two-human wording disagrees with two-host evidence and closed tracker | Literal proof or explicit approved scope amendment, never retroactive relabeling. |
| `bd-reality-core-convergence-1azkt.9` | P1 | Shipped claims, maturity, primary journey, docs, and release copy disagree | Pre-release stable/beta/experimental/reserved ledger; close blocker or demote claim; generated truthful copy. |
| `bd-reality-core-convergence-1azkt.10` | P0 | Live race evidence lacks source authority | Build exact attested candidate and reproduce or refute before `.2` changes code. |
| `bd-reality-core-convergence-1azkt.11` | P0 | Raw BM25 saturates public relevance and contaminates quality/admission | Frankensearch-backed calibration or explicit unknown; correct per-source domains and every downstream consumer. |
| `bd-reality-core-convergence-1azkt.12` | P1 | No product oracle rejects plausible irrelevant retrieval | Adversarial no-mock quality fixtures, calibrated abstention, MRR/nDCG/precision and false-admission gates. |
| `bd-reality-core-convergence-1azkt.13` | P0 | Backup create fails, dry-run can mutate, and durable state is omitted/lost | Complete versioned five-job snapshot and lossless side-path recovery contract. |
| `bd-reality-core-convergence-1azkt.14` | P1 | Backup contract lacks evolved-store runtime proof | Tamper/failure/migration/restore/rebuild/query E2E with exact durable-state comparison. |
| `bd-reality-core-convergence-1azkt.15` | P0 | FrankenNetworkX lacks personalized seed PageRank required to remove local PPR | Add/pin upstream capability or disable/degrade pack PPR influence. |
| `bd-reality-core-convergence-1azkt.16` | P1 | Generation/model lifecycle can amplify disk/RSS/latency and privacy risk | Bounded admission, lease-aware GC, secure paths, operator inspection/repair, large-corpus proof. |
| `bd-reality-core-convergence-1azkt.17` | P0 | Existing release oracles can pass empty/OR/ignored/file-presence evidence | Adversarial exact assertions, mutation tripwires, linearizability model, exact shard/test inventory. |
| `bd-reality-core-convergence-1azkt.18` | P0 | Effective Franken-stack/release inputs and tools are not hermetic/provenance-complete | No semantic post-checkout rewrite; pin/bind every tree/tool/container/action; locked builds and least privilege. |
| `bd-reality-core-convergence-1azkt.19` | P0 | Verifier implementation is not itself a green product verdict | One immutable clean main candidate receives a complete, tamper-detecting `ee.release_candidate_proof.v1` capsule. |
| `bd-reality-core-convergence-1azkt.20` | P0 | Publication is a separate external authority boundary | Record exact human approval; publish only the verified private draft; no authority is inferred from the bead. |
| `bd-reality-core-convergence-1azkt.21` | P1 | Public assets/channels/links can drift after publish | Read-only post-public audit and generated evidence reconciliation. |
| `bd-reality-core-convergence-1azkt.22` | P0 | Prose/related edges can permit premature epic closure | Final graph-encoded evidence-ledger closeout blocked by every mandatory child and reused blocker. |

**Coverage result.** At audit start, closing all 122 nonclosed records would
still have left determinism attribution, score truth, complete recovery,
verification authority, reproducible performance, supply-chain hermeticity,
release staging/publication separation, and two-human disposition without
complete owners. The bridge epic plus `.1`–`.22` now gives every §8 row an
implementation, proof, decision, or maturity-demotion owner. That means the
*plan* is covered; it does not mean the product gaps are implemented or green.
The final closeout child encodes mandatory edges, and `br dep cycles --json`
reports zero active cycles after refinement.

---

## 11. Dependency-ordered bridge

### T0 — Establish authority, contracts, red oracles, and decisions

1. Implement canonical verifier/proof-capsule machinery `.5` while the product
   is still red; it must not depend on already-green product evidence.
2. Freeze determinism `.1`, create the source-attested pre-fix oracle `.10`,
   harden test-oracle integrity `.17`, and bind hermetic inputs `.18`.
3. Resolve the external personalized-PageRank path `.15`: upstream capability
   or explicit disabled/degraded influence.
4. Resolve two-human scope `.8` now, because external coordination and product
   scope are early critical-path decisions.
5. Record the performance branch in `.6`: reproduce each stable claim or
   remove/narrow it; measurement happens after a candidate exists.

**Gate:** every later result can name exact source/binary/dependency authority,
the released-binary race has a red-or-green current-source oracle, and no
verification or scope decision is circularly blocked on final success.

### T1 — Fix source-of-truth, retrieval, durability, and security boundaries

1. Land immutable-generation/model admission `.2` and bounded lifecycle `.16`.
2. Complete `bd-3k1mg`, `bd-index-auto-freshness-m5kwf`, `bd-16imy`, and
   `bd-3h6bz`: every **eligible, positively admitted** native entity has a
   canonical document, truthful generation, and ADR-0085 typed hydration.
3. Complete stack conformance `.4` and relevance semantics `.11`.
4. Complete full durable recovery `.13`; keep derived indexes/cache rebuildable.
5. Enforce redaction/admission before index persistence, embedding, trace,
   proof, backup, or mesh egress; recheck live authorization/revision/security
   epoch within the pinned snapshot.

**Gate:** core code has no known partial-publication, local core-algorithm,
uncalibrated-confidence, stranded-rule/evidence, or lossy-recovery path.

### T2 — Prove the five jobs, quality, recovery, and eight North Stars

1. Run post-fix linearizability/determinism `.3`, retrieval-quality `.12`, and
   evolved-store recovery `.14` through adversarial oracle `.17`.
2. Complete public manual-memory, CASS, rule, and consolidation loops under
   `bd-16imy`, `bd-3h6bz`, and `bd-1oep7` with exact entity/content/provenance,
   audit, generation, retry, and why assertions.
3. Complete `bd-2mpct` and `bd-2mpct.1` only after Learn and Maintain behavior exists;
   all eight scenarios use public commands and no hand-seeded substitute.
4. Exercise cancellation, contention, offline semantic/CASS, stale graph,
   corrupt derived state, denied content, and harmful/helpful lifecycle paths.

**Gate:** all eight North Stars plus durability and quality pass; empty output,
abstention, OR assertions, ignored tripwires, and file presence cannot pass.

### T3 — Produce the functional green candidate, then measure it

1. Close current red contract/lib/doctor/security/platform prerequisites named
   by `.19` and run the canonical manifest on one clean immutable main SHA.
2. Produce and independently verify the content-addressed green candidate
   capsule `.19`; hosted CI and pinned RCH must agree.
3. Use that exact returned binary for `.6` on the claimed M3 host. Retain
   correctness digests with latency/resource samples, then generate or remove
   public rows according to the T0 decision.

**Gate:** a functionally green, source-attested candidate exists and every
remaining performance claim has reproducible evidence or has been demoted.

### T4 — Make claims truthful and stage a private release

1. Complete pre-release maturity/docs ledger `.9`: every stable claim closes
   its blocker or is demoted; one canonical agent journey remains.
2. Build the hermetic private draft/staging pipeline `.7` from the exact `.19`
   candidate and `.18` effective inputs.
3. Verify packages/installers/signatures/provenance/SBOM posture and native or
   explicitly emulated platform behavior before any public mutation.
4. Keep the release private on every failure; draft asset mismatch fails rather
   than clobbers.

**Gate:** the exact asset set and release copy are privately verified and no
credentialed publication has happened.

### T5 — Human publication, public audit, and closeout

1. `.20` records explicit human authorization for the exact candidate/tag and
   only then publishes the already verified draft and explicitly authorized
   channels.
2. `.21` performs the post-public read-only asset/channel/link/provenance audit
   and regenerates public evidence state.
3. `.22` checks every enumerated checklist row and dependency, runs the final
   cycle/closure/evidence audit, and is the sole Part III rollup.
4. Only after `.22` closes may Part III archive and this path become Part IV.

**Gate:** code, candidate capsule, public release, README/AGENTS/plans/ADRs,
matrix/coverage/CHANGELOG, release page, and Beads describe one coherent state.

---

## 12. Required proof matrix

| Proof | Required assertions |
| --- | --- |
| Source/binary authority | Clean immutable main tree; exact sibling/dependency/toolchain/target/features; candidate binary hash and `ee version` provenance; installed historical evidence never masquerades as current-source proof. |
| Serial determinism | At least 100 identical fresh-process requests pin the same source snapshot, backend/model identity, ordered result IDs/scores, admitted items, provenance, and pack hash. |
| Concurrent linearizability | Multi-process reads/writes/crashes map wholly to one committed DB/index/model/security snapshot; no missing/partial generation, stale fence publication, or mixed-epoch recovery. |
| Retrieval calibration/quality | Raw source score and kind remain truthful; calibration identity or unknown posture; distinct BM25 values do not saturate; distractors fail admission; MRR/nDCG/precision/false-admission/abstention gates pass. |
| CASS closed loop | Import unique positively screened phrase → emitted jobs only → search exact `EvidenceId` → ADR-0085 typed admission or exact denial → pack/why/replay/outcome preserve redacted session/span provenance; no synthetic memory. |
| Rule closed loop | Curate/apply unique rule → emitted jobs only → search exact rule → pack correct procedural section → why resolves source memories/evidence. |
| Maintain closed loop | Duplicates → dry-run no mutation → candidate → validate/apply → lineage/audit → emitted jobs → one retrieval result → idempotent retry. |
| Data durability and upgrade recovery | Evolved historical store → non-mutating dry-run → atomic backup → tamper verification → isolated restore/migrate → derived rebuild → identical durable inventory, audit/trust/provenance, and continued five-job behavior. |
| Privacy/trust boundary | Denied/secret/path-bearing content is absent from index staging/retained generations, model input, result/pack ledgers, stderr/logs, proof/support artifacts, backups/restores, and mesh egress. |
| Harmful/helpful lifecycle | Outcome → audited confidence/decay/demotion/inversion decision → later search/pack behavior; no silent or target-type-confused mutation. |
| North Stars | All eight §4 command sequences, exact good-output fields, and exact success signals; no hand-seeded substitute for CASS/rule flow. |
| Oracle integrity | Mutation/kill-switch proof makes each release scenario fail when its required behavior is broken; empty/OR/ignored/file-presence/degraded alternatives cannot pass. |
| Test inventory | Exact test IDs appear once across shards; zero/omitted/duplicate/filtered/ignored counts and all retries are explicit. |
| Verification truth | Required stage cannot be skipped, tracked-red, infra-failed, timed out, cancelled, OOM-killed, bypassed, or run against another binary and still yield overall pass. |
| Stack conformance | Pack graph influence comes through FrankenNetworkX; retrieval/scoring comes through Frankensearch; no exported custom BM25/PPR core substitute remains; version identity is exact. |
| Hermetic inputs | Effective ee/sibling trees, locks/config/toolchain/linker/SDK/features/tools/actions/containers/advisories are pinned and provenance-bound; clean-cache rebuild matches. |
| Candidate capsule | Tamper-detecting `ee.release_candidate_proof.v1` binds all identities, stage/test inventory, attempts, results, binaries, performance, and CI/RCH evidence. |
| Native packaged behavior | Every advertised archive runs on its native target or is labeled compile/emulated; packaged walking skeleton, model claim, glibc/musl, Windows stdout, macOS targets, and installer modes pass before publish. |
| Performance and first-agent journey | Correct-output raw samples + sufficient count + host/power/thermal/workload/source identity + variance/RSS/disk/backlog; stable core journey meets declared SLO without daemon or claim is demoted. |
| Documentation preflight | Executed snippets/schema selectors, explicit maturity, one primary journey, no contradictions/historical proof theater/ownerless promise, generated release copy. |
| Post-public audit | Exact tag/checks/assets/hashes/signatures/provenance/SBOM/install/channel/link state matches the candidate and generated public docs. |

Every E2E emits structured `ee.test_event.v1` logs, preserves stdout/stderr
separation, uses no mocks where the acceptance is a live integration, and runs
Cargo only through the repository RCH wrapper on this Mac.

Safe deterministic failpoints are permitted for crash/partial-write testing;
"no mocks" means the real FrankenSQLite, Frankensearch, filesystem publication,
and public binary paths remain in use. Proof output goes to an unpredictable,
owner-only, redaction-aware, content-addressed sink.

---

## 13. Architecture and product invariants for implementation

### 13.1 Immutable derived-state publication

- Derived index generations are immutable and content-addressed. Tier files and
  manifests are durable before one atomic SQLModel/FrankenSQLite pointer commit.
- Readers pin DB, generation, model, config, authorization, and security epoch
  before retrieval and hold the generation lease through hydration and hashing.
- Publisher fencing prevents a paused/stolen owner from publishing. GC never
  reclaims a leased or currently eligible generation.
- Recovery never prefers a structurally complete but privacy-stale generation.
- Model admission is cross-process and resource-bounded for the whole semantic
  operation; core CLI correctness never depends on a daemon.

### 13.2 Score semantics and library ownership

- Raw backend scores remain raw and source-typed. A number is called relevance
  or confidence only with a named valid calibration artifact.
- Unknown calibration produces unknown/abstaining quality, not synthetic `1.0`.
- Frankensearch owns lexical/vector/fusion/rerank/calibration math.
  FrankenNetworkX owns core graph metrics. EE owns policy, eligibility, packing,
  provenance, and rendering, but does not recreate dependency algorithms.

### 13.3 Typed CASS/rule admission

ADR 0085 is controlling: `MemoryId`, `RuleId`, and `EvidenceId` remain distinct
pack identities. A safe undistilled span or sourceless advisory rule may enter a
typed pack after live fail-closed admission. Search metadata is never
authorization. Instruction-like, secret-bearing, stale-revision, malformed,
scope-mismatched, unscreened, or wrong-workspace evidence is denied. Curated
memory/rule remains the preferred durable interpretation; no synthetic memory
is invented merely to satisfy a pack schema.

### 13.4 Durable versus rebuildable state

FrankenSQLite/SQLModel is the source of truth. Backup must enumerate and round
trip every durable five-job table and audit/provenance ledger. Search indexes,
embeddings, graph snapshots, and caches are derived and are either omitted or
verified as optional accelerators, then rebuildable. Persistent-data migration
is a durability obligation, not an obsolete public-API compatibility shim.

### 13.5 Verification and release authority

- Building a verifier is different from proving a candidate green.
- Historical/released-binary evidence is different from a current source-
  attested candidate.
- Private staging is different from credentialed publication.
- An open publication bead is not authorization. `.20` requires explicit human
  approval for the exact candidate/tag before any external mutation.
- Post-public audit is independent evidence, not release-job self-attestation.

---

## 14. Complexity, risk, and sequencing rationale

| Workstream | Complexity | Primary risk if rushed | Risk control |
| --- | --- | --- | --- |
| Snapshot/determinism/index/model (`.1`–`.3`, `.10`, `.16`) | Very high | Mixed-generation reads, privacy rollback, memory/disk herd | Immutable generations, SQL pointer, leases/fences, linearizability and resource proofs |
| Franken-stack/relevance (`.4`, `.11`, `.12`, `.15`, `.18`) | High + upstream | A second algorithm or fake confidence survives under a new name | Upstream dependency boundary, call-site/type enforcement, calibrated/unknown score contract |
| Typed CASS/rule and Maintain (existing P0/P1 work) | Very high | Trust laundering, invented identity, false closed loop | ADR 0085 live admission, exact entity/provenance, adversarial public E2Es |
| Backup/recovery (`.13`, `.14`) | Very high | Silent durable-history loss or mutating dry-run | Table inventory, consistent snapshot, atomic create, side-path restore, evolved-store comparison |
| Verification/oracles (`.5`, `.17`, `.19`) | High | Green theater from skips, wrong binary, empty output, incomplete shards | Declarative manifest, mutation tests, exact inventory, immutable proof capsule |
| Performance (`.6`) | Medium-high | Fast incorrect output or irreproducible marketing number | Correctness digests, source-attested local driver, reproduce-or-remove decision |
| Release/docs (`.7`–`.9`, `.18`, `.20`, `.21`) | High + external | Public broken/misrepresented release or unauthorized channel mutation | Hermetic private staging, maturity preflight, explicit human publish, independent audit |
| Mesh criterion (`.8`) | Low code / external | Endless blocker or dishonest retroactive closure | Early literal proof-or-amend decision with approver and residual risk |

The dependency graph intentionally front-loads decisions, contracts, and red
oracles; then implementation; then behavioral proof; then green convergence;
then performance/docs/private staging; and only finally human publication. This
is the shortest honest critical path because it avoids building tests after a
fix, blocking verifier construction on already-green tests, or publishing
before the copy/assets they advertise are verified.

---

## 15. Part III final close criteria

The bridge may archive only when all of the following are simultaneously true:

- The two-human criterion is satisfied or explicitly amended with a recorded
  product decision and an owned post-v1 follow-up if still promised.
- `bd-3k1mg`, `bd-16imy`, and `bd-3h6bz` are behaviorally closed.
- All children `.1` through `.22` are closed with their required artifacts;
  `.22` is the final evidence-ledger rollup. Publication `.20` closes only
  after exact written human authorization if publication remains in scope.
- `bd-2mpct` and `bd-2mpct.1` prove all eight exact North Stars and no ignored core-loop
  tripwire remains.
- `bd-1oep7` proves the public Maintain loop.
- One evolved real store backs up, verifies, restores to an isolated side path,
  migrates, rebuilds, and resumes all five jobs without durable-state loss.
- Lexical, semantic, hybrid, reranked, and degraded relevance/quality fields
  remain truthful; irrelevant distractors do not become maximal relevance.
- CASS evidence follows ADR 0085 live admission and privacy policy end to end.
- The documented primary first-agent journey works within its declared latency
  posture without requiring a daemon.
- One immutable current SHA is green for format, clippy `-D warnings`, complete
  required tests, exact North Stars, representative E2Es, dependency audit,
  and the canonical readiness manifest.
- Hosted CI and the release dry-run agree with that same verdict.
- README performance/release/Windows/Homebrew/crates.io/mesh claims match
  retained evidence.
- Active Beads have no dependency cycles and every remaining vision promise
  in the enumerated §8 checklist has an active owner, maturity demotion, or an
  explicit approved non-goal decision. Unrelated experimental tracker history
  is not silently promoted into the close gate.

Bead-count percentage, file presence, command registration, an abstention
sentinel, a scheduled CI success with substantive jobs skipped, or a remote
run that timed out/OOMed is not sufficient closure evidence.
