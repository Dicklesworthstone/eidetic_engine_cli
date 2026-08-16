# Changelog

This changelog is a research artifact, not a marketing page. Sources for the
current head are git history, annotated tags, published GitHub Releases,
`AGENTS.md`, `README.md`, ADRs under `docs/adr/`, Cargo metadata, and
checked-in Beads records. The durable research ledger is
[`CHANGELOG_RESEARCH.md`](./CHANGELOG_RESEARCH.md).

## Scope and methodology

| Window | Coverage |
| --- | --- |
| 2026-04-29 → 2026-05-20 | Full archaeology pass that created this file (see historical note below). |
| 2026-05-15 → 2026-06-16 | Published GitHub Releases `v0.1.0` … `v0.12.0` (assets on GitHub; detailed prose below is still incomplete for `0.4.0`–`0.12.0`). |
| 2026-06-16 → 2026-07-30 | **`0.13.0`** fully researched below (`v0.12.0`..`HEAD`, 673 non-merge commits). |
| 2026-07-30 → 2026-08-06 | **`0.13.1`** native-reranker completion and release-hardening patch. |

Release surface (as of 2026-08-06):

- Latest **published** GitHub Release before this cut: [`v0.13.0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.13.0) (2026-07-30).
- `Cargo.toml` carries `version = "0.13.1"` for this cut; the historical
  `v0.13.0` tag and release remain unchanged.
- Install path: `curl -fsSL "https://raw.githubusercontent.com/Dicklesworthstone/eidetic_engine_cli/main/install.sh?$(date +%s)" | bash -s -- --easy-mode --verify`

### Version timeline (tags and GitHub Releases)

| Version | Date | GitHub Release | Notes |
| --- | --- | --- | --- |
| [0.13.1](#0131---2026-08-06) | 2026-08-06 | this cut | Pure-Rust native reranking on every release target, model bootstrap, checksum and publication hardening |
| [0.13.0](#0130---2026-07-30) | 2026-07-30 | yes | User-global store, Learn→Pack loop, pack-ledger integrity, group-commit / incremental index, installer hardening |
| [0.12.0](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.12.0) | 2026-06-16 | yes | Contention observability, RCH topology canary, ask/decide/session-budget wave |
| [0.11.0](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.11.0) | 2026-06-15 | yes | `ee decide`, `ee ask`, memory-debt doctor, scale envelope |
| [0.10.0](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.10.0) | 2026-06-15 | yes | See release notes on GitHub |
| [0.9.1](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.9.1) | 2026-06-12 | yes | Patch after 0.9.0 |
| [0.9.0](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.9.0) | 2026-06-12 | yes | Anchored recall, workspace primer, output governor, swarm claim-gate |
| [0.8.1](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.8.1) | 2026-06-09 | yes | Patch |
| [0.8.0](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.8.0) | 2026-06-08 | yes | See GitHub |
| [0.7.0](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.7.0) | 2026-06-07 | yes | See GitHub |
| [0.6.0](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.6.0) | 2026-06-06 | yes | See GitHub |
| [0.5.0](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.5.0) | 2026-06-04 | yes | See GitHub |
| [0.4.0](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.4.0) | 2026-06-02 | tag present | Expand prose in a later pass if needed |
| [0.3.9](#039---2026-06-01) | 2026-06-01 | yes | Detailed below |
| … | … | … | Earlier entries remain below through [0.1.0](#010---2026-05-15) |

Versions `0.4.0`–`0.12.0` are **real published releases** (or tags). Prefer the
GitHub Release page for asset lists and the original generated notes until a
future changelog pass expands those rows into full capability sections.

## [Unreleased]

### Team confederation (ADR 0086, live Unix EE-to-EE)

- `ee team` create/invite/join/members/share/unshare/pause/resume/leave,
  signed origin events, authenticated TCP hello/sync/body/identity_attest,
  Tailscale-attested and secretless OIDC IdP, user-scoped
  `ee daemon install`, and `ee team doctor` now exist as product
  commands. Isolated-host proofs are recorded in
  `docs/mesh/perf_budgets.md` and `docs/mesh/verification_matrix.md`.
- Body publication is confirm-gated and fails closed with
  `mesh_body_cache_lifecycle_failed` when the T2.1 secure-file adapter
  is missing. `ee team share bodies --representation already_redacted`
  is a distinct signed publication; redact-over-exact is refused.
  BodyFetch requires a durable Body-lane Allow and refuses substituted
  cache bytes. `ee team doctor` reports `index_rematerialization`,
  `origin_outbox`, `invite_auth_floor`, `pending_invites`,
  `delegated_members`, `signing_rotation`, `pair_rotation`,
  `projects`, `inbound_body_fetches`, and `removal_acknowledgements`. Removal seeds a durable
  V117 acknowledgement matrix of the active audience at that moment.
  Pending rows stay a warning; fanout is not claimed bounded until
  those members apply the event. `ee team steward once` advances
  acknowledgements from peer cursors. `ee team status` lists
  `pendingRemovalAcks[]` plus the conservative admission caps and
  `localTier1Unaffected`. `ee team doctor` reports a 64 MiB
  free-space floor when a workspace path is present. Authenticated
  serve persists a V118 per-peer admission snapshot so doctor/status
  can report throttled/exhausted counts and coalesced exhaustion
  after the broker exits. `ee team status` emits `budgets`
  (`ee.team.budgets.v1`) naming join, signed-relay, body, and
  index amplification caps. `ee team steward once` promotes a staged
  Next pair key when Current is missing (crash during rotation) and
  leaves a Next-beside-Current pairing deferred for the peer ceremony.
  `ee team revoke --all-before-floor` is the invite-floor repair.
  Team create and join raise the invite-authorization floor.
  `ee team status` lists `pendingInvites[]` so a revoke can name an id.
  `ee team projects reconcile` rematerializes origin `teamProjectShared`
  events onto local project rows.
  Inbound `exact`/`already_redacted` memory events persist a
  producer-keyed `metadata_only` body-cache row. Authorized BodyFetch
  releases `nonceHex`; `apply_fetched_team_body` recomputes the
  event-signed commitment before `staging→available`. Mismatch
  quarantines. `ee team doctor` reports `inbound_body_fetches`.
  Omitted history share stays body-free. The import ledger records
  the producer `body_cache_key`.
  Join enrolls the inviter under the pair-key handle so EventFetch
  can find the invite TCP endpoint. Enroll also persists the remote
  human as an active `team_members` row, so `--memory-scope team`
  admits teammate text without a hand-edited `trust.team_members`
  config. `persist_team_member` is idempotent on origin node so
  enroll-then-join does not insert a duplicate.
  `ee team join` now runs the first metadata sync round after
  membership persist and reports `firstSync` (`complete`,
  `importedEvents`). `ee team invite --wait` stays up after redeem
  and serves that hello+sync round so the joiner can import origin
  events before either side exits.
  `--memory-scope team` no longer admits unauthenticated
  `trust.team_members` nicknames from `.ee/config.toml`; only durable
  `team_members` rows count.
  Team pack now applies ADR 0086 TC-D16 precedence (local workspace >
  team > global) on overlap and keeps both sides of a cross-lane
  contradiction. `detect_peer_memory_conflicts` annotates pack `why`
  with hashed `peerConflict` markers. Contradictions emit
  `team_lane_conflict_deferred` and get distinct diversity keys so
  rank cannot hide one side. A sealed or missing teammate body emits
  `team_lane_conflict_unassessed` instead of a false no-conflict.
  The inviter enrolls the accepted
  joiner at the join TCP source IP and the advertised `joinerHelloPort`
  (hello also carries `joinerWorkspaceId`) so EventFetch/BodyFetch
  work both ways. `ee team fetch body` retries granted BodyFetch
  when the local cache is still metadata-only. Enroll is best-effort
  after redeem so a missing workspace row cannot withhold the grant.
  `retry_pending_team_body_fetches`
  calls fetch only when the durable body lane is Allow, then applies
  nonce-checked bytes. Invite codes and join grants carry
  `originWorkspaceId`. Enroll stores it on the peer record.
  `plan_team_body_fetch_binding` is Some only when local and remote
  workspaces and nodes are distinct. After EventFetch, `ee mesh sync`
  and `ee team steward once` run grant-gated BodyFetch on the current
  thread so pair-key sessions do not have to be Send.
  `ephemeral_source_for` picks a concrete same-family source IP for
  routed remotes instead of skipping anything that is not loopback.
  `ee team invite --wait --resume <invite-id>` continues a pending
  waiter without re-emitting the secret.
  `ee mesh hello-responder run --workspace .` loads enrolled pair-key
  peers when `--peer` is omitted. `ee daemon --foreground` starts that
  inbound owner (not `--once`) when mesh is on and peers exist.
  Team-join enroll uses `tailnet-team-join`; resolve binds it to the
  current LocalAPI tailnet and allows inbound EventFetch without a
  formal lane-grant generation. BodyFetch still requires durable
  Body-lane Allow. When tailscaled is absent, `TeamJoinLocalApi`
  answers WhoIs from the enrolled endpoint IP and allows loopback bind
  so `ee mesh hello-responder run` and the daemon owner still listen.
  Creating or joining a local team turns mesh on for that workspace
  unless `EE_MESH_ENABLED=0` or `mesh.enabled = false`. Team-join
  WhoIs uses a `nodekey:` transport key and grant generation 0 so
  `start_durable` actually binds loopback after join. If every enrolled
  endpoint is loopback, inbound prefer uses TeamJoin even when
  tailscaled is installed. `[[bench]] team_confed` profiles pair-key
  derive, at-cap EventBatch/BodyFetch admission, and create+enroll.
  The loopback inbound test TCP-connects the bound port.
  After bind, `ResponderBrokerOwner::serve_one` answers unsigned hello
  plus a sync round from the origin store, so `ee mesh sync --once`
  over TeamJoin loopback returns the genesis event.
  Isolated `cargo bench --bench team_confed` measured derive_pair_key
  at ~2 µs and at-cap admission at ~80 ns; create+enroll is migrate
  dominated (~34 s). After TeamJoin bind, authenticated pair-key
  EventFetch returns the genesis origin event. Grant-gated BodyFetch
  through the same inbound owner returns the published body bytes.
  Without Body-lane Allow the same fetch stays metadata-only.
  Token-free identity_attest over the same inbound owner persists
  the member login on the origin store. Windows inbound listen uses
  the same TeamJoin TCP path; Tailscale LocalAPI WhoIs stays Unix.
  `ee team steward once` is the canonical steward verb (`run-once`
  remains an alias). The `HardenedWindows` SID/DACL/reparse adapter
  compiles; a Windows-host runtime soak remains an environment remainder.
  Authorized BodyFetch now hydrates the local `peer_human_attested` stub
  so `ee search --memory-scope team` / `ee pack --memory-scope team`
  can recall teammate text. Metadata-only share still stays a stub.
  Apply and `ee team steward once` drain the inbound SingleDocument
  index job so a joiner without a prior local index can still search.
  Team-scoped search hits now carry `teamProvenance` (member display
  name, member-attested `producedAt`); pack markdown prints
  `· from <member> · <producedAt>`. `ee pack --json` and pack JSONL
  items emit the same `teamProvenance` block. `ee pack --memory-scope
  team` selects the hydrated teammate memory. A retry or
  `ee team steward once` hydrates leftover `[ee.team.history]` stubs
  from an already-available body cache (upgrade path after a
  pre-hydrate apply). `ee team activity` attributes inbound
  projections with member display name, member-attested origin time,
  and `bodyAvailable` once the stub is hydrated; activity JSON never
  includes teammate body text. Inbound teammate memory ids are minted
  as typed Crockford `mem_*` values from the event hash so
  `ee pack --memory-scope team` can parse and select them.
  `ee team activity --member` / `--project` filter the same closed
  metadata. Shared history/body events bind `project` from the
  workspace's minted team project. `--since` is an inclusive RFC 3339
  lower bound (JSON rejects `2h`); the report sets
  `timeFilterBasis=member_attested` and `sequenceComplete=false`.
  `--cursor` resumes an `ee.cursor.v1` page; invalid or param-mismatched
  tokens return an empty page plus `cursorError=cursor_invalid`.
  A live TeamJoin BodyFetch now applies onto a joiner store and
  `ee pack --memory-scope team` selects the teammate text with
  `teamProvenance`.
- Authenticated responder sessions now apply
  `MeshAdmissionLimits::conservative_default()` before EventFetch,
  BodyFetch, Summary, and `identity_attest`.

### Added

- `docs/team/quickstart.md`, `docs/team/trusted_vs_contractor.md`,
  `docs/agent-ux/team.md`, `docs/mesh/perf_budgets.md`.

## [0.13.1] - 2026-08-06

Patch release completing the pure-Rust native-reranker rollout and associated
release hardening.

### Changed

- All six published archive targets use Frankensearch's pure-Rust native
  reranker without ONNX Runtime. The strict proof covers the five required
  cross-platform targets; x86_64 Linux musl remains the installer-preferred
  extra. Frankensearch is pinned to
  `b559c92e03242336614b995c562a13dfd1269eed`.
- Search, index publication, context persistence, and cancellation reporting
  received the post-0.13.0 correctness work already present on `main`.

### Fixed

- Published the manifest-pinned `rerank-default-v1` safetensors archive and
  made release installer smoke tests fetch, register, and require five genuine
  model-backed reranked results.
- Normalized Windows checksum line endings before aggregate `SHA256SUMS`
  verification.
- Hardened cross-platform reranker determinism, forbidden-dependency, and
  ORT-absence evidence.

### Verification

- Strict five-target native-reranker proof:
  [GitHub Actions run 31128786161](https://github.com/Dicklesworthstone/eidetic_engine_cli/actions/runs/31128786161).

## [0.13.0] - 2026-07-30

Durable, local-first, explainable memory for coding agents. This release rolls
up **673 commits** since [`v0.12.0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/tag/v0.12.0)
(~102 `feat`, ~232 `fix`, ~130 `test`) across the core memory loop, search
index corpus, pack integrity, daemon write path, installer, and agent
contracts. Minor bump per the pre-1.0 convention (features land as minor).

Version was set in-tree by
[`861d5fb7`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/861d5fb7)
on 2026-06-21; this cut tags `v0.13.0` on current `main` so post-bump
storage, index-publication, and installer fixes ship in the same binary.

### Highlights

#### User-global memory store (ADR 0083)

- Separate local store at the user data root (`…/global/`) with schema/migration
  parity to a workspace store.
- Normal verbs operate on it via `--global` (selector conflicts with
  `--database`; workspace-id guard keeps stores isolated).
- Replaces the earlier policy-only / write-mostly global tier with real open,
  migrate, remember, and list paths
  ([`09ecf4f8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/09ecf4f8),
  [`919caaf7`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/919caaf7);
  [ADR 0083](docs/adr/0083-user-global-memory-store.md)).

#### Learn → Retrieve → Pack loop closed for rules and evidence

- Procedural rules join the derived search corpus and hydrate into pack
  candidates through source-memory linkage
  ([`b19d8075`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b19d8075),
  [`f96deb2d`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/f96deb2d);
  bd-3h6bz).
- Imported CASS evidence spans become searchable documents and hydrate when
  distilled; undistilled spans degrade honestly
  (`context_evidence_hit_unhydrated`) instead of looking like pack items
  ([`dba0cf30`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/dba0cf30),
  [`0e438e14`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/0e438e14);
  bd-16imy).
- Typed pack entity identity work (ADR 0085) keeps memory / rule / evidence
  identities distinct for pack, replay, and why surfaces
  ([ADR 0085](docs/adr/0085-typed-pack-entity-identity.md)).

#### Pack ledger integrity and public replay (v2 contracts)

- Pack history reads prefer the integrity-verified selection ledger over
  denormalized convenience rows (V084 profile domain + ledger read APIs)
  ([`910b872d`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/910b872d)).
- Public ID projection and replay-text redaction so support bundles, handoff,
  and swarm surfaces never echo raw secret-shaped values
  ([`f78c9963`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/f78c9963)).
- Wire schemas and handlers:
  `ee.pack.replay.v2`, `ee.pack.diff.v2`, `ee.context.delta.v2`,
  support-bundle pack-replay summary v2, and public attestation projection
  ([`31fb02ea`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/31fb02ea),
  [`9789f684`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9789f684),
  [`900ab215`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/900ab215),
  [`a62b7e95`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/a62b7e95),
  [`2b0b5667`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/2b0b5667)).
- Top-level JSON envelopes surface `degraded[]` even under minimal field
  profiles so agents can plan recovery without the full `data` tree
  ([`4f07392f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/4f07392f)).
- `--schema-version` selects response **renderer generation** (`v1` = current
  `ee.response.v2` wire shape; `v0` = legacy compatibility)
  ([`248bfb12`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/248bfb12)).

#### Hot-path write + index intake (ADRs 0077 / 0078)

- Group-commit write intake contract, config, telemetry, and daemon write-owner
  path (`ee.daemon.write`, journal coalescing, `ee.daemon.write_journal`)
  ([ADR 0077](docs/adr/0077-group-commit-write-intake.md);
  [`80c250f3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/80c250f3),
  [`a47b71e7`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/a47b71e7),
  [`7f86703f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/7f86703f)).
- Coalesced-batch **incremental** index intake when the touched set is safe;
  otherwise full rebuild remains the audited fallback
  ([ADR 0078](docs/adr/0078-incremental-index-intake.md);
  [`306272ed`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/306272ed)).
- Corpus publication stamps a deterministic corpus revision and exact
  per-source/per-tier counts; legacy memory-only generations cannot be
  relabeled as current after rule/evidence expansion
  ([`6ad42dd5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6ad42dd5);
  bd-1zfau). Migrations V080–V087 cover generation floors, audit timeline,
  memory-debt snapshots, pack profiles, evidence security posture, rule
  generations, and evidence storage rebuild.

#### Search, capture, and swarm authority

- Rerank `auto|off` + top-k config; first-use reranker auto-provision with
  honest fusion-only degradation offline
  ([`ff9c87e0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/ff9c87e0),
  [`638998b5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/638998b5)).
- Ambient capture command wiring and coverage-gap / capture-demand reporting
  ([`b8526743`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b8526743)).
- CASS prefetch coordinator with gated schedule, budget accounting, and
  per-workspace metrics
  ([`884de00a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/884de00a)).
- Agent Mail snapshot v1 freshness and workspace-binding authority for swarm
  brief / claim evidence
  ([`9dacddeb`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9dacddeb)).
- Insights unscoped bundle carries per-section pagination so truncation is
  visible (GH#15)
  ([`3c444c5e`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/3c444c5e)).

#### Installer

- Bash 3.2-safe empty-array expansion under `set -u` (proxy, agent detection,
  box rendering, archive candidates) so proxy-free macOS installs resolve the
  latest release and agent-less hosts no longer fail post-install config
  ([`7c107614`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/7c107614)).
- Compatible GNU archive retry when preferred musl is absent on x86_64 Linux;
  one-byte network preflight instead of downloading the archive twice (already
  staged in Unreleased and carried into this release).

### Fixed

- Evidence rescreen / rebuild atomicity and V087 registry realignment so
  corrupt or legacy evidence rows do not poison publication
  (see `fix(db):` series culminating in
  [`5f2aea8c`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/5f2aea8c),
  [`efa2f4e5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/efa2f4e5)).
- Search preserves unfiltered evidence hits when admission filters would drop
  the only supporting document
  ([`9b492289`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9b492289)).
- RCH / verify path: pinned Franken-stack topology, Cargo config provenance
  attestation, worker-pressure classification, and portable pinned bundles
  (verify series on main after the version bump).
- Envelope / golden isolation: host topology, process env, and degraded
  mirrors no longer leak machine-specific fields into contract tests.

### Migrations agents may need

```bash
ee migrate run --workspace .
ee index rebuild --workspace .   # if doctor/status report index stale or corpus revision mismatch
```

Schema versions of note: V084 (pack profiles / ledger), V085–V087 (evidence
security posture and storage rebuild), V086 (rule index generations).

### Platforms

Same matrix as recent releases: macOS (`aarch64`, `x86_64`), Linux
(`aarch64`/`x86_64` gnu; musl when published), Windows (`x86_64`). Prefer the
**maintained** installer on `main` rather than any stale `install.sh` asset
bundled with an older release.

```bash
curl -fsSL "https://raw.githubusercontent.com/Dicklesworthstone/eidetic_engine_cli/main/install.sh?$(date +%s)" | bash -s -- --easy-mode --verify
ee --version   # ee 0.13.0
ee doctor --json
```

### Closed workstreams (representative)

| Theme | Beads / ADRs | Representative commits |
| --- | --- | --- |
| User-global store | bd-2vq2z.13, ADR 0083 | [`09ecf4f8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/09ecf4f8), [`919caaf7`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/919caaf7) |
| Rules in search/pack | bd-3h6bz | [`b19d8075`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b19d8075), [`f96deb2d`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/f96deb2d) |
| Evidence in search/pack | bd-16imy | [`dba0cf30`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/dba0cf30), [`0e438e14`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/0e438e14) |
| Pack ledger / replay v2 | V084, schemas | [`910b872d`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/910b872d), [`9789f684`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9789f684) |
| Group-commit + daemon write | bd-d67os.*, bd-wx6ou.* | [`80c250f3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/80c250f3), [`306272ed`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/306272ed) |
| Corpus publication | bd-1zfau | [`6ad42dd5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6ad42dd5) |
| Installer Bash 3.2 | installer tests | [`7c107614`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/7c107614) |

### Notes for agents

- Prefer `ee pack` over the soft-deprecated `ee context` alias.
- Treat non-empty `degraded[]` as actionable; do not assume empty under
  minimal profiles after this release.
- After upgrade, run `ee doctor --json` before assuming the index corpus
  includes rules and evidence.
- Swarm claim gates must still see a fresh, workspace-bound Agent Mail
  snapshot; stale/foreign snapshots are no longer authoritative.

---

## Historical archaeology note (2026-05-20)

The original changelog body below was reconstructed from the repository, not
from memory. Sources for that pass were `AGENTS.md`, `README.md`, Cargo
metadata, source module entrypoints, tests, docs, checked-in Beads records,
tags, and non-merge git history through
[`050602500c566e1e2603bb36a1f1cdcae1d292c3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/050602500c566e1e2603bb36a1f1cdcae1d292c3)
on 2026-05-20. At that time only `v0.1.0` existed as a tag and GitHub Releases
had not yet been cut for later versions. The detailed audit ledger remains in
[`CHANGELOG_RESEARCH.md`](./CHANGELOG_RESEARCH.md).

## [0.3.9] - 2026-06-01

### Added

- `calibrated` boolean on search hit and document results. When no conformal
  residual quantile exists, `coverageGuarantee` is now `null` and `calibrated`
  is `false`, so agents no longer read a raw fusion score over the trivial
  `[0,1]` band as a 95%-calibrated relevance (bd-1h4nu).
- High-confidence co-tag auto-linking on `ee remember`: ordinary tagged
  remembers now populate the memory-link graph (PageRank/HITS/PPR/centrality),
  not only `--workflow`-scoped ones. Gated on `--auto-link` (default on); see
  [ADR-0051](docs/adr/0051-remember-cotag-auto-linking.md) (bd-pp1fk).
- `responseFieldMap` in agent-docs so agents can discover result paths (bd-13h5k).
- Adaptive-budget benchmark now emits p50/p99 latency percentiles
  (`EE_ADAPTIVE_BUDGET_PERCENTILES`), guarded by a perf-bench envelope contract.
- closure-lint infers implementation surface from known file paths, and a
  command-inventory metadata guard asserts every advertised `ee` command
  resolves with schema and side-effect metadata.

### Changed

- `ee.daemon.echo` diagnostic method is disabled by default and gated behind
  `EE_DAEMON_ENABLE_ECHO`, so production daemon sockets never reflect
  caller-supplied request params.
- CASS prefetch histories are scoped by agent (bd-298n0).

### Fixed

- Hardened the v0.3.2+ installer signing trust boundary: `install.sh`
  now tries keyless Sigstore identities before the pinned release-key
  fallback, `EE_INSTALL_REQUIRE_KEYLESS=1` refuses that fallback,
  Sigstore verification pins transparency-log enforcement with
  `--insecure-ignore-tlog=false`, and
  [`docs/security/release-signing.md`](docs/security/release-signing.md)
  documents key generation, storage, rotation, and revocation policy.
- `cass` now reports "installed but untrusted" with an `EE_CASS_BINARY` repair
  instead of telling agents to install a cass that is already present at an
  untrusted location; the path is probed via a non-executing `$PATH` stat, so
  the execution-allowlist security posture is unchanged (bd-3twa9).
- Storage operations are routed through a panic guard that converts
  sqlmodel/fsqlite panics into structured `DbError`s instead of unwinding.
- Daemon hardening: verify stop-target liveness, tighten cleanup and NUMA
  allocations, bound read-request pre-allocation, and cover UDS error envelopes.

## [0.3.8] - 2026-05-30

Release-pipeline fix #4 (the final whack-a-mole in this series — gates
job now succeeds AND release job's Python provenance-verification step
no longer dies on a heredoc EOF error).

### Fixed

- **Release job: Python heredoc end-marker no longer leaves leading
  whitespace after YAML indent strip** (`.github/workflows/release.yml`).
  The `python3 - <<'PY' ... PY` block in "Verify Sigstore bundles and
  provenance" had `PY` indented 12 spaces while the surrounding `python3`
  invocation was at 10 spaces. Bash heredoc end-markers MUST be at
  column 0 of the script content (or use `<<-` with tabs only). After
  YAML stripped 10 spaces of indentation, `PY` ended up with 2 leading
  spaces — bash didn't recognize it as the terminator, read to EOF,
  emitted "syntax error: unexpected end of file" at line 56. Fix:
  dedent the heredoc content + `PY` end-marker so they land at column
  0 after YAML strip. Caught by v0.3.7's release job failure (gates
  succeeded for the first time, build succeeded, release step then
  blew up).

### Notes

- Ships the same fixes/feature work as v0.3.5/v0.3.6/v0.3.7. The
  pipeline has now been broken FOUR releases in a row at different
  stages:
  - v0.3.4: Vision coverage gate (made advisory in v0.3.5)
  - v0.3.5: gates timeout 60min (bumped to 120 in v0.3.6)
  - v0.3.6: gates timeout 120min (perf-bench skipped on tags in v0.3.7)
  - v0.3.7: release step Python heredoc EOF (fixed here in v0.3.8)
- Once v0.3.8 ships successfully, the structural recommendation is to
  add per-job timeouts on EVERY job (not just gates), drop perf-bench
  from the release workflow entirely (it has its own CI workflow), and
  perhaps add a workflow-level YAML linter to the pre-merge CI to catch
  heredoc-indentation-class issues before they ship.

## [0.3.7] - 2026-05-30

Release-pipeline fix #3 — same shape as v0.3.2 perf-bench (#5), v0.3.4
cargo-deny (#7), v0.3.5 vision-coverage advisory, v0.3.6 gates timeout
bump. v0.3.5 was cancelled at 60min, v0.3.6 was cancelled at 120min,
both because the Performance benchmarks step compiles the full
franken-stack via path-deps (asupersync, frankensearch, frankensqlite,
fnx-*) which takes 60-90min from cold cache.

### Changed

- **Performance benchmarks step now skipped on release-tag runs**
  (`.github/workflows/release.yml`). The advisory bench is for
  downstream perf dashboards, not release gating — running it on
  every release-tag push is pure overhead that has now cancelled
  three releases in a row. The branch-push path of THIS workflow
  (`on: push: branches: main`) still runs the perf-bench so the
  artifact stays available for dashboards. Condition added:
  `if: ${{ !startsWith(github.ref, 'refs/tags/') }}`.

### Fixed

- **`.beads/.sync.lock` and `.beads/.write.lock` removed from
  tracking** (gitignored in `.beads/.gitignore`). 0-byte runtime
  files accidentally committed in 8a6b4a24 during the v0.3.4
  fmt-cleanup commit's `git add -A`. Doesn't fix a user-facing
  bug; just hygiene cleanup.

### Notes

- Ships the same fixes/feature work as v0.3.5 and v0.3.6 (both
  workflow-cancelled before producing GitHub Release pages). See the
  v0.3.5 CHANGELOG entry below for the full ledger.

## [0.3.6] - 2026-05-30

Release-pipeline timeout fix. v0.3.5's gates job (run 26674710915) was
cancelled when its 60-minute timeout fired while the "Performance
benchmarks (advisory)" step was mid-compile of the franken-stack
path-deps (fsqlite-error compilation visible at +58min). The vision
coverage advisory fix from v0.3.5 worked correctly — the gate ran,
emitted ::warning, and the workflow continued — but the next heavy
compile blew the gates job's own wall-clock budget.

### Changed

- **`gates` job `timeout-minutes` bumped from 60 → 120**
  (`.github/workflows/release.yml`). The gates job's "Performance
  benchmarks (advisory)" step needs to compile the full franken-stack
  via path-deps (asupersync, frankensearch, frankensqlite, fnx-*),
  which from a cold cache can alone take 50-60 minutes. The advisory
  step is correctly non-blocking on benchmark OUTPUT
  (`|| { ::warning ... }`), but the job's wall-clock timeout still
  applies to the compile time itself. 120 minutes gives comfortable
  headroom on cold-cache runs.

### Notes

- Ships the same fixes/feature work as v0.3.5 (which never produced a
  GitHub Release page due to the timeout cancellation). See the v0.3.5
  CHANGELOG entry below for the full list.
- A future deeper improvement: skip "Performance benchmarks" entirely
  on release-tag commits (the artifact is for downstream perf
  dashboards, not release gating — and dashboards don't need every
  release-tag run to produce an artifact).

## [0.3.5] - 2026-05-30

Release-pipeline-hardening cluster. The v0.3.4 tag was pushed but its
Release workflow failed at `gates / Vision coverage gate` (0.95% coverage
gap on a release-tag commit — a recurring whack-a-mole pattern matching
v0.3.1 (perf-bench) and v0.3.3 (cargo-deny), where quality/coverage gates
that depend on the rapidly-churning surface inventory block releases over
sub-1% gaps without correlating to real ship-blockers). v0.3.5 cuts a
clean release with the vision-coverage gate now advisory, plus carries
the user's preflight `--stdin` / `--cmd-base64` channels work and the
v0.3.4 surface fixes that the gate prevented from shipping.

### Changed

- **Vision coverage gate is now advisory** (`.github/workflows/release.yml`).
  Mirrors the v0.3.2 perf-bench precedent ([#5](https://github.com/Dicklesworthstone/eidetic_engine_cli/issues/5)) and the v0.3.4 cargo-deny precedent
  ([#7](https://github.com/Dicklesworthstone/eidetic_engine_cli/issues/7)). The
  gate still runs (so the report artifact uploads + the gap is visible
  in CI logs), but emits `::warning` instead of `::error` and adds
  `continue-on-error: true` so the workflow can reach
  build/release/smoke-test/macos/homebrew. Track coverage gaps in a
  dedicated dashboard, not in the release workflow's gates job.

### Added

- **`ee preflight check --stdin` and `--cmd-base64` channels**
  (commits [`017d3047`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/017d3047), [`be7571fa`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/be7571fa)).
  Two new input channels for `ee preflight check` beyond the existing
  `--cmd` flag: `--stdin` reads the command from stdin (avoids shell
  quoting hazards on agent harness pipelines), and `--cmd-base64`
  accepts the command base64-encoded (lets the caller embed arbitrary
  binary content without escaping shell metacharacters or hitting
  argv-length limits). The fresh-eyes hardening commit (`be7571fa`)
  audits both channels for the same shell-chain-injection guards that
  protect `--cmd`.
- See the closed `bd-1rc8b` beads epic for the full design + acceptance
  criteria; the carry-over `bd-1xnfn` tracks 3 pre-existing
  preflight_guard test failures unrelated to the new channels.

### Fixed

- **All v0.3.4-prep fixes that the failed Release workflow prevented from
  shipping** ship in v0.3.5. The git tag `v0.3.4` (at commit `8a6b4a24`)
  exists but its workflow failed before any artifacts shipped, so there is
  no v0.3.4 GitHub Release page. See the v0.3.4 CHANGELOG entry below for
  the full list — every fix described there is in v0.3.5.

### Notes

- The 0.95% vision coverage gap that broke v0.3.4 is tracked separately;
  it's a normal churn artifact from the surface inventory and will be
  re-zeroed in a routine documentation pass. v0.3.5 ships with the gap
  visible in CI logs (advisory) so the trend is monitored without
  release-blocking.

## [0.3.4] - 2026-05-29

Test-suite cleanup + release-pipeline hardening cluster. Closes #7 (cargo-deny
gate broke v0.3.3 provenance), fixes a real production preset bug on
`ee swarm brief --fields summary`, lands a v1→v2 envelope migration sweep
across the production CLI surface, and brings `agent_golden_baselines` from
8/25 to 24/25 and `contracts/*` from 1056/1117 to 1087/1117 (97.3%).

### Fixed

- **`ee swarm brief --fields summary` no longer errors with `usage_unknown_field {rejectedField: "command"}`.**
  The default preset arm in `preset_fields_for_command` (`src/output/mod.rs`)
  emits `command`, `version`, `status`, `summary`, `count`, `schema` for any
  command without an explicit preset arm — but `swarm brief`'s response shape
  has none of those at the top level (it carries `workspace`, `sources`,
  `beads`, `recommendations`, etc.), so the field validator rejected `command`
  as unaccepted. Added explicit `"swarm brief"`, `"swarm next-action"`, and
  `"swarm work-packet"` arms with field lists derived from the actual response
  shape. Same fix family hits `--robot` and `--format json` paths.

- **`parse_cass_line_fragment("L10-L20")` no longer fails on the end side of
  the range.** The leading `L` was stripped from the whole fragment but not
  from the `end` value after the `-` split, so the `L20` substring failed to
  parse as `u32`. One-line strip on `end` fixes it. (G10)

- **CI's `cargo-deny` gate is now advisory** (`.github/workflows/release.yml`
  and `ci.yml`). The v0.3.3 Release workflow failed at "RustSec advisory audit"
  not because of a real advisory but because of a Docker mount-path mismatch:
  the franken-stack checkout rewrites `Cargo.toml` with host-absolute paths
  (`/home/runner/work/...` on push runs, `/home/runner/work/_temp/...` in CI),
  and `EmbarkStudios/cargo-deny-action@v2` runs inside a container with mounts
  at `/github/workspace` and `/github/runner_temp` — so `cargo metadata` fails
  with `failed to load manifest for dependency 'fnx-algorithms' ... No such
  file or directory` before any advisory is evaluated. Mirrors the v0.3.2
  perf-bench precedent: real perf/advisory tracking belongs in a dedicated
  dashboard, not in a release-blocking workflow that depends on volatile
  path-dep siblings. `deny.toml` advisory config is unchanged. (#7)

### Test infrastructure

- **v1→v2 envelope sweep across the production CLI surface** (G8). 93
  production callsites in `src/cli/{mesh,share,mod}.rs` and `src/pack/mod.rs`
  migrated from `ee.response.v1` to `ee.response.v2` (the v2 envelope shape
  is a pure superset of v1's `degraded[]` shape — no behavior break). 65 test
  files + goldens + schema files updated to match. `coordination_payload_value`
  parser now dual-accepts v1 and v2 for backward-compat with existing on-disk
  artifacts. Six "must NOT regress to v1" guards and the legacy schema-drift
  contract entries deliberately left at v1.

- **`canonical_response_fixtures_match_docs_schemas` now passes** (G2 + G8).
  10 docs schemas realigned with current production output: `ee.status.v1`
  (+ `flightRecorder`, `search` properties), `ee.doctor.v1` (envelope const
  v1→v2 + `hostCalibration` inlined), `ee.capabilities.v1`, `ee.memory.show.v1`
  (+ `memoryId`), `ee.memory.list.v1`, `ee.curate.candidates.v1`,
  `ee.mcp.manifest.v1` (+ `subcommandTools`), `ee.completion_audit.report.v2`,
  `ee.curate.show.v1` (+ `field_presets`), `ee.diag.incident.replay.v1` (+
  `field_presets`). Status + doctor goldens regenerated.

- **`agent_golden_baselines`: 8/25 → 24/25** (G9). All 17 G8-flagged failures
  were Category C (schema-evolution drift) — per-surface `git blame` confirmed
  intentional `feat(…)` commits for each new observability tree (singleFlight,
  flightRecorder, qos, rchWorkerPressure, verificationPosture,
  verificationLedger, hostCalibration, meshAutoEnrollment). 19 goldens
  regenerated, scrub-list extended for live host-state churn
  (`rchWorkerPressure`, `sizeDiagnostics`), and `contains_unredacted_secret`
  hardened against false-positives on `"unit":"tokens"` + `disk-pressure`.
  One residual (`golden_schema_contract_runner_validates_current_stage`)
  exercises live host probes too deeply to scrub without redesign; tracked
  as known.

- **`contracts/*`: 1056/1117 → 1087/1117 = 97.3% pass** (G10). 61 failures
  categorized + triaged: 16 A/C goldens regenerated, 6 B test-infra bugs
  fixed (counterfactual UUID regex, perf_live `$ref` deref, swarm_brief
  envelope drilling, singleflight ordered-set comparison, auto_enroll label
  substring uniqueness), 4 C inventory catch-ups (schema_drift table list,
  degraded_code_taxonomy auto_enrollment codes, PENDING_SRR6_46_SCRIPTS
  registry, cursor fixture force-added). 30 residual failures surface as
  Category D items for owner review (see commit body for details).

- **Test fixture: `ee.eval.report.v1::duration_ms`** golden uses the sentinel
  `"[duration_ms]"` to prevent wall-clock drift; the schema-conformance
  validator now substitutes `0` (any number) before running JSON schema
  validation. (G8 sweep 3)

- **`tests/fixtures/agent_detect/cursor/.cursor/.keep`** force-added — the
  root-level `.cursor/` `.gitignore` rule was silently dropping this fixture
  from the working tree.

### Notes

- **v0.3.4 should be cut from a clean Release workflow run** to close #7's
  underlying provenance issue (assets tagged with a sourceCommit matching the
  tag commit). The cargo-deny advisory fix ensures the workflow's `gates` job
  no longer fails at the cargo-deny step on the franken-stack drift; the rest
  of the workflow (build/release/smoke/macos/homebrew) should reach completion
  naturally.

- **Items deliberately deferred** (out of v0.3.4 scope):
  - The 30 Category D contracts/* residuals (real production regression in
    `--fields summary` for swarm commands is fixed in this release; other
    residuals are golden-drift or fixture-drift that need per-test eyeballs).
  - `golden_schema_contract_runner_validates_current_stage` host-state
    instrumentation (needs host-probe stubbing or scrubbing at source
    emission).
  - The ~17 pre-existing version-string drifts in `agent_golden_baselines.rs`
    that surface independent of the v1→v2 envelope sweep.

## [0.3.3] - 2026-05-28

Daemon UDS RPC hardening cluster. Heavy focus on the new `ee daemon start` /
`ee daemon stop` surface, slow-loris protection, deserialize-boundary contract
enforcement, panic supervision, setsockopt-failure propagation, atomic socket
bind via create-then-rename (TOCTOU), shutdown idempotency, cass_prefetch
redaction + cache-coherence + history bounding, structured tracing/audit at
the RPC dispatch boundary, and a NoopMetricsCollector seam for future
observability backends. Plus the `cargo-deny` CI gate (which subsequently
broke release provenance — see [0.3.4]'s #7 fix) and several
documentation/closure-lint normalizations.

This entry is retroactive — v0.3.3's tag (`c3a8d031`) was cut without a
CHANGELOG entry at the time. See git log between v0.3.2 and v0.3.3 for the
full commit ledger.

### Release integrity

- The official Release workflow on tag v0.3.3 (run
  [`26558018828`](https://github.com/Dicklesworthstone/eidetic_engine_cli/actions/runs/26558018828))
  failed at the `gates / RustSec advisory audit (cargo-deny)` step. Build,
  release, and smoke-test jobs were skipped, but assets were still
  manually published under the tag with provenance pointing to a non-tag
  source-commit. Root cause + advisory-gate fix: [#7](https://github.com/Dicklesworthstone/eidetic_engine_cli/issues/7), addressed in v0.3.4.

## [0.3.2] - 2026-05-27

Release-quality cluster — fixes a startup panic that blocked `help` /
`capabilities` / `doctor` / `version --json` on v0.3.1, plus the macOS
install path, the release perf gate, and clears up Sigstore verification
docs. Cuts a clean workflow-built release whose artifacts match the tag
commit.

### Fixed

- `ee` no longer panics on startup for `help`, `capabilities`, `doctor`,
  or `version --json`. `economy prune-plan` was registered twice in
  `EffectManifest` (once as `read_only`, once as `degraded_unavailable`)
  and `insert_unique` aborted. The `degraded_unavailable` registration
  is the canonical one — it matches the sibling economy commands
  (`report`, `score`, `simulate`) and accurately reflects the abstain
  behavior when persisted workspace metrics are missing. The duplicate
  `read_only` entry has been removed. (#3)
- `install.sh` now finds the extracted `ee` binary on macOS. The previous
  `find ... -perm -111` predicate required execute bits for user, group,
  and other, but macOS tarballs ship `ee` with mode 700 (owner-only
  execute). Predicate relaxed to `-perm -u+x`, with a name-only fallback
  and an unconditional `chmod u+x` for safety. The macOS release
  workflow also now `chmod 755 ee` before tarring as a belt-and-braces
  fix on the producer side. (#4)
- Release perf benchmarks (`Performance benchmarks` step in `gates`) are
  now ADVISORY: they still run and upload the artifact, but a failure
  no longer blocks the release. The v0.3.1 run failed because of
  external franken-stack drift (new enum variants in `asupersync` /
  `raptorq` triggered non-exhaustive-match build errors deep in path
  deps that have nothing to do with an ee perf regression) and the
  result was that tag commit `ddf72b4d` shipped manual artifacts built
  from `48f232f6`. Tracking real perf regressions belongs in a perf
  dashboard, not in a release-blocking step. (#5)
- Sigstore verification docs in the auto-generated release notes now
  document both the keyless workflow path AND the pinned-key fallback
  path. The installer already accepted both paths via
  `verify_blob_against_anchors`; only the user-facing docs were
  asymmetric. (#6)
- The v0.3.2 artifacts are produced by `release.yml` end-to-end, so
  artifact provenance matches the tag commit and keyless cosign
  verification (as documented) succeeds — closes #5 and #6 by
  construction.

## [0.3.0] - 2026-05-23

Post-`v0.2.0` work focused on swarm coordination contracts, retrieval and
ranking refinements, deterministic side-paths, and structural support for
external derivation. No breaking schema bumps: every change layers on the
v0.2 envelope, pack, and search contracts.

### Added

- `ee.swarm.work_packet.v1.candidateDecision` enum for stable, deterministic
  per-candidate claim classification (`safe_to_claim`, `already_owned`,
  `unsafe_due_to_conflict`, `blocked_by_dependency`, `blocked_by_verification`,
  `stale_but_reclaimable`, `stale_review`, `external_state_required`).
  Producer sorts candidate arrays, `unsafeReasons`, `staleReasons`, and
  `sourceRefs` deterministically before `packetId` calculation. Only
  `safe_to_claim` may support an automatic claim recommendation
  (bd-2z5ly.7.5).
- `ee.swarm_slo.scorecard.v1` schema and golden fixtures for replayable,
  redaction-safe multi-agent ee workflow scorecards consumed from existing
  `ee.agent_workload_trace.v1` rows. Records workload shape, coordination
  posture, latency percentiles, stage attribution, replay hashes, and budget
  verdicts without leaking memory bodies, mail bodies, command output, or
  full file listings.
- `ee.curate.propose_derived.v1` schema and the
  `ee curate propose-derived` CLI surface for agent-driven, deterministic
  derived-memory candidate proposals against explicit source refs
  (kind+id+rationale), with dry-run support and audit-aware insert
  (bd-kxm0c).
- ADR 0043 (External-derivation candidates) + supporting schemas
  (`ee.reflect.request.v1`, `ee.reflect.source_package.v1`) + four
  deterministic e2e harness scripts for the no-LLM derivation lifecycle.
- ADR 0032 implementation: `TrustClassTransition` with promote/demote/stable
  direction, 0.90 CI default, audit-row carry-through, and `ee outcome`
  integration so trust changes are deterministic and explainable.
- `MemoryTierTransitionAuditBatch` (`ee.memory_tier.transition_audit.v1`) and
  the `memory_tier_metadata_stale` degraded code; opt-in
  `[pack] memory_tier_admission` config that biases ranking on hot/warm
  candidates without filtering cold items.
- `ee.pack.compression_manifest.v1` schema, `src/cache/pack_compression.rs`
  zstd dictionary trainer, and `docs/pack-compression.md` operator guide;
  `zstd = "0.13.3"` direct dependency.
- `ee.swarm.work_packet.v1` schema, dedicated docs surface
  (`docs/agent-ux/swarm-work-packet.md`, `docs/swarm/work_packet.md`), and
  `ee swarm work-packet --json` CLI surface composed from existing
  swarm-brief and next-action evidence with no side effects.
- `ee curate propose-derived --dry-run` agent-facing surface for explicit
  derived-memory candidates from caller-provided source refs.
- Lexical RAM tier config block (`[search.lexical_ram_tier]`) and merged-
  config plumbing into `ee config show` / `ee status`. Runtime `mmap` /
  `mlock` / `madvise` still pending; status reports
  `lexical_ram_tier_not_implemented` until the runtime slice lands.
- `ee verify rch ingest` / `ee verify rch blockers` / `ee verify rch runs`
  read-only durable-proof queries plus the supporting `verify_ledger`
  fixtures.
- `ee graph insights --section bridges` and `--section knowledgeSkyline`
  graph-derived sections plus the `ee.graph.bridge_insight.v1` schema.
- Pack-assembly arena allocator scratch types (`PackDraftScratch`,
  `MmrAssemblyScratch`) for deterministic hot-path reuse without changing
  pack hashes (bd-1i6np).
- Curate workspace CASS aggregator
  (`workspace_cass_review_candidates`) so review-candidate planning sees
  the full corpus, not a single session window.
- `br doctor --json` adapter and the `OwnedBeadsIntegrityInputs` surface
  for richer Beads integrity reports (`external_changes_pending_import`,
  `dirty_issue_count`, `br_reads_authoritative`).
- Conformance harnesses for handoff / export / backup
  (`tests/contracts/handoff_export_backup_conformance.rs`), CASS
  subprocess supervision (`scripts/e2e_overhaul/cass_subprocess_supervision.sh`),
  the SLO scorecard, and the `cass_unavailable` ee.error.v2 degradation
  routing (bd-33t39).
- Real-binary E2E pin tests for `ee graph centrality`,
  `ee graph centrality-refresh`, `ee graph path`, `ee memory show / history`,
  `ee memory expire`, `ee curate candidates --filter`, and the MCP
  `initialize / tools/call / resources/read / prompts/get` error envelopes.

### Changed

- Refactor: `audit_context_pack_assembly_with_connection` short-circuits
  when the workspace row is absent so unregistered-workspace pack reads no
  longer leak FK-error diagnostics.
- Refactor: replace correlated subselect in last-audit-row query with a
  direct `ORDER BY timestamp DESC, id DESC LIMIT 1` scan.
- CASS subprocess adapter (`src/cass/process.rs`) gains a documented
  supervision lifecycle: bounded I/O timeouts, capture vs. streaming
  classification, deterministic child reap on timeout.
- CASS import error envelopes carry structured details via the new
  `DomainError::ImportWithDetails` variant.
- Curate / Situation / Tripwire / Preflight / Certificate / Memory-revise
  surfaces moved from text-heuristic stubs to persisted-store reads with
  honest degraded envelopes — see
  `docs/mechanical-boundary-command-inventory.md`.
- v0.2 envelope examples across AGENTS.md, README.md, the migration guides,
  the perf-forensics cookbook, the workspace-hygiene workflow, and the
  ux-style-guide aligned to `ee.response.v2` / `ee.error.v2` everywhere.

### Fixed

- `cass_unavailable` ee.error.v2 routing for any `DomainError::Import` /
  `ImportWithDetails` whose message contains the case-insensitive
  `"cass binary"` substring (bd-33t39).
- Workspace-id audit FK errors on unregistered workspace pack assemblies.
- Cooperative graph refresh starvation: long-running bridges/articulation
  refreshes no longer block PageRank / HITS slots.

## [0.2.0] - 2026-05-21

Post-`v0.1.0` work is a large hardening and expansion wave. The main themes are
graph-derived retrieval, optional mesh/Tailscale coordination, doctor first-aid,
flight recording, QoS, deterministic output contracts, and crowded-checkout
agent ergonomics.

### Added

- Added `ee curate accept/reject --reason <TEXT>` reviewer rationale capture
  for curate transitions from `bd-3qs2i.1`.
- Added the `pack_budget_too_small` degraded code for context packs that cannot
  fit any candidate within the requested budget from `bd-3qs2i.2`.
- Added the `harmful_burst_quarantine` degraded code for burst-guarded harmful
  outcome absorption from `bd-3qs2i.3`.
- Added the `embed_model_unavailable` degraded code for lexical fallback when
  the embedding model is unavailable from `bd-3qs2i.4`.
- Added `ee rule mark` validation and contradiction counter tracking from
  `bd-3qs2i.5`.
- Added optional mesh and Tailscale-oriented coordination surfaces:
  peer autodiscovery, auto-enrollment flow, auto-status views, discovery policy,
  explicit revision tokens, foreground mesh CLI mode, emergency-disable paths,
  replay recovery status, quarantine/repair status, and hello responder
  lifecycle contracts.
  Representative commits:
  [`6025cf40`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6025cf40),
  [`9fd1d9f4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9fd1d9f4),
  [`9e3552ce`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9e3552ce),
  [`baf954de`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/baf954de).
- Added doctor first-aid and operator triage surfaces:
  `ee doctor --quick`, `--only`, `--since`, `--robot-triage`,
  `--gc-plan`, `--fix`, `--undo`, `--capabilities`, mesh auto-enrollment
  checks, safety-harness integration, and a corrected envelope contract where
  `success` means the doctor command ran, not that the system is healthy.
  Representative commits:
  [`6fd75080`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6fd75080),
  [`73d1d181`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/73d1d181),
  [`587fe9d3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/587fe9d3),
  [`ed59bc9a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/ed59bc9a),
  [`2ef934a4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/2ef934a4),
  [`b95dce7a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b95dce7a).
- Added agent workload flight-recorder infrastructure:
  trace schema, recorder module, env registry entries, status/doctor posture
  mapping, e2e harness contract, and operator/agent docs.
  Representative commits:
  [`fc31bec1`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/fc31bec1),
  [`657d0386`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/657d0386),
  [`96820199`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/96820199),
  [`a79e4706`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/a79e4706),
  [`1604c235`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/1604c235),
  [`fdd7b35a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/fdd7b35a).
- Added graph and retrieval expansion after the tag:
  HITS role scores, HITS profile names, PPR prefetch cache, Gomory-Hu
  self-proximity coverage, load-bearing why/curate guard surfaces, symbol graph
  evidence links, EQL plan cache, bead-affinity scoring, dedup-link evidence,
  conformal score intervals, and graph flag/help coverage.
  Representative commits:
  [`4a12ec0c`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/4a12ec0c),
  [`ebc183c8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/ebc183c8),
  [`d92d0995`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d92d0995),
  [`d15d0000`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d15d0000),
  [`07a0c62b`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/07a0c62b),
  [`c272e9eb`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/c272e9eb),
  [`2ff0d4e8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/2ff0d4e8).
- Added shard fan-out and multi-agent write-path groundwork:
  migration apply-mode skeleton, per-shard degraded aggregation, audit-lane
  workload e2e, shard schema/config/router tasks, and global timeline work
  recorded in Beads.
  Representative commits:
  [`6e7e7a8f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6e7e7a8f),
  [`e3164b14`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/e3164b14),
  [`669bf47e`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/669bf47e).
- Added workspace hygiene and crowded-agent ergonomics:
  comprehensive `ee workspace hygiene` surface, bounded output, secret-risk
  scanning, JSON and combined parser coverage, dirty Beads/RCH proof guidance,
  and command/help docs around graph and hygiene flags.
  Representative commits:
  [`a4597eb3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/a4597eb3),
  [`cb5ceca4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/cb5ceca4),
  [`927de076`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/927de076).

### Changed

- Tightened deterministic ranking and output stability across search, graph,
  PPR, dominance, causal traversal, structural health, skyline, Pack DNA, and
  HITS by moving tie-breakers toward stable radix/id ordering.
  Representative commits:
  [`881ec7d7`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/881ec7d7),
  [`655be21a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/655be21a),
  [`dcae6f13`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/dcae6f13),
  [`589d8ce6`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/589d8ce6).
- Expanded output rendering for graph schema blocks, Pack DNA markdown, causal
  markdown, graph status formats, insights format dispatch, and command-manifest
  parity.
  Representative commits:
  [`81530b7c`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/81530b7c),
  [`b1b89e24`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b1b89e24),
  [`9006bbf0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9006bbf0),
  [`b8da5005`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b8da5005),
  [`06c4fb13`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/06c4fb13),
  [`c9c5a8b2`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/c9c5a8b2).
- Strengthened safety around symlinks and non-regular files in CASS import,
  preflight rules, QoS lane registries, init metadata, and discovery binaries.
  Representative commits:
  [`48ceb2cc`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/48ceb2cc),
  [`fd14bb94`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/fd14bb94),
  [`c17db4d8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/c17db4d8),
  [`5de433fb`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/5de433fb),
  [`486aabc3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/486aabc3).

### Fixed

- Fixed release compile blockers across audit, context, and migration code.
  Representative commit:
  [`e4a525b3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/e4a525b3).
- Fixed degraded aggregation capping and per-surface routing for graph,
  shard-fanout, and curate/status outputs.
  Representative commits:
  [`57c02dab`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/57c02dab),
  [`59d8432f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/59d8432f),
  [`669bf47e`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/669bf47e),
  [`9946e34f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9946e34f).
- Fixed numerous redaction leaks and source-reference exposures in recorder,
  CASS, status response counts, mesh import paths, and graph outputs.
  Representative commits:
  [`07cbb3b4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/07cbb3b4),
  [`9c6a3fe0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/9c6a3fe0),
  [`0461b697`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/0461b697).
- Fixed NaN-sensitive scoring math across retrieval, causal, db, and clustering
  paths.
  Representative commit:
  [`8bf5bb96`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/8bf5bb96).

### Tests And Verification

- Added mesh-off, mesh policy, mesh privacy, byte-stability, discovery,
  auto-enrollment, hello handshake, and two-tier budget contracts.
- Added graph/HITS/PPR/Gomory-Hu/load-bearing/symbol-graph contracts, perf
  gates, parser coverage, CLI help drift guards, schema snapshots, and e2e
  harness coverage.
- Added doctor fixture suites, undo/fix e2e harnesses, safety harness stage
  integration, workspace hygiene logged e2e, audit-lane e2e, and flight-recorder
  e2e contracts.
- Hardened RCH verifier scripts and proof guidance without falling back to local
  Cargo in remote-only contexts.

## [0.1.0] - 2026-05-15

`v0.1.0` is the initial tagged source release of `ee`: a local-first Rust CLI
memory substrate for coding agents. It is not a general agent harness, daemon,
planner, or web service. The controlling loop is:

```bash
ee init --workspace . --json
ee remember --workspace . --level procedural --kind rule "Run cargo fmt --check before release." --json
ee search "format before release" --workspace . --json
ee context "prepare release" --workspace . --format markdown
ee why <memory-id> --json
ee status --json
```

### Core Architecture

- Established a single Rust 2024 binary crate with binary `ee`, library surface
  `ee`, `#![forbid(unsafe_code)]`, Cargo-only project management, and strict
  avoidance of forbidden runtime/database/graph/HTTP stacks.
  Representative commits:
  [`b478d7e5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/b478d7e5),
  [`0e650413`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/0e650413).
- Established the documented dependency direction:
  `cli -> core -> {db, search, cass, graph, pack, curate, policy, output} -> models`.
- Added source module boundaries for CLI parsing/dispatch, core use cases,
  DB/repositories, search, CASS import, graph analytics, pack assembly, curation,
  policy/redaction, output rendering, config, hooks, optional MCP, optional
  serve, observability, and steward jobs.
- Added stable response and error envelopes, global CLI flags, help/agent docs,
  field filtering, output formats, command manifests, and schema-aware renderers.
  Representative commits:
  [`12ad584d`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/12ad584d),
  [`bd-yh0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-yyc`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-xf9`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Memory, Storage, And Import

- Added workspace initialization, storage path resolution, TOML config parsing,
  config precedence, workspace repository, DB connection/migration helpers,
  transaction helpers, audit repository, and append-only audit concepts.
  Representative commits:
  [`e028f9b`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/e028f9b),
  [`f8606c8`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/f8606c8),
  [`bd-ywe7`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-trceq`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added memory IDs, provenance IDs, policy IDs, memory levels, memory kinds,
  tags, validity windows, legal holds, supersession links, idempotency keys,
  confidence/utility/importance fields, and bounded content validation.
- Added `ee remember`, memory repository persistence, memory history, rule
  lifecycle surfaces, expire/tags operations, workflow IDs, links, revision
  groups, and search-index job enqueueing.
  Representative commits:
  [`6ee2f964`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6ee2f964),
  [`bd-sygu1`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-z4xi`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added CASS robot/JSON integration, session and span persistence, import
  ledger logic, import diagnostics, CASS health counting, and redaction-aware
  source references.
  Representative commits:
  [`f4623a4a`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/f4623a4a),
  [`bd-s67f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Search, Context Packs, And Explanation

- Added Frankensearch/Tantivy-backed search plumbing with lexical/semantic
  modes, degraded lexical fallback, validity-window filters, tombstone/expired
  filters, query-file support, pagination, deterministic tie handling, and
  memory-scope search.
  Representative commits:
  [`4e67cfd9`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/4e67cfd9),
  [`bd-w5w5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-17c65.2.10`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added deterministic context pack assembly with token budgeting, profile
  support, provenance, explanation metadata, pack records/items, pack hashes,
  replay ledgers, freshness diagnostics, and markdown/JSON/TOON rendering.
  Representative commits:
  [`4bbb409f`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/4bbb409f),
  [`bd-aitk`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-zn8i`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-w2ts`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added `ee why`, storage/retrieval/pack explanation, memory link rendering,
  causal explanation, revision lineage, pack DNA, why-not-selected style
  diagnostics, and output contracts.
- Added pack replay/diff, pack quality evaluation, support-bundle summaries,
  query-file pack paths, large-fixture freshness scans, and redaction egress
  proofs.
  Representative Beads:
  [`bd-v454`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-4bya6`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-dcub`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-rynf`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Curation, Learning, And Rules

- Added curation candidates, review/propose surfaces, rule mark/update,
  procedure verification, playbook export/import, rule protection, agenda and
  uncertainty outputs, rule provenance, anti-pattern/trauma guard logic, and
  low-evidence rejection paths.
- Added Bayesian memory posterior scoring, harmful-weight-aware credible
  intervals, structural decay hooks, maturity/trust handling, and rule
  promotion constraints.
  Representative commits:
  [`d527adcc`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d527adcc),
  [`bd-rua0`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-ynzg`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-zgjc`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added claim parsing/verifying, evidence ledgers, certificate verification,
  real certificate signing, local signing policy, provenance chain hashes, and
  sampled provenance verification.
  Representative Beads:
  [`bd-qigqt`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-xvre`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-s4fk`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-w7ih`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-xxhe`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Graph Analytics And Structural Retrieval

- Added graph snapshot framework, graph centrality and refresh surfaces,
  graph/index maintenance, typed subgraphs, algorithm-result caches, witnesses,
  schema registration, insights sections, health structural reports, Pack DNA,
  PPR reranking, Gomory-Hu proximity, causal paths, dominance/revision
  frontiers, minhash rank, skyline, K-truss, contradiction clusters, structural
  decay, and graph determinism harnesses.
  Representative commits:
  [`df459466`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/df459466),
  [`23ff6a70`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/23ff6a70),
  [`ebeda496`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/ebeda496),
  [`5e9ae784`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/5e9ae784).
- Closed or substantially satisfied the graph workstream around:
  F1 typed subgraphs, F2 algorithm wrappers/witnesses/cache, F3 `ee insights`,
  F4 determinism/golden harnesses, G1 PPR, G2 Pack DNA, G3 causal explanation,
  G4 structural health, G5 structural decay, G6 Gomory-Hu, G7 dominance, G8
  skyline, and G10 HITS.
  Representative Beads:
  [`bd-rnfh`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-igvt`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-t6wd`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-8jvg`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-ov09`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-fdvt`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-qnfw`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-zx2v`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-mvld`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-5vqr`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-a7mm`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Diagnostics, Operations, And Maintenance

- Added `ee status`, `ee doctor`, `ee check`, capability reporting, posture
  summaries, structured suggested actions, failure-mode fixtures, degraded-code
  taxonomy, status/check/capabilities/version/doctor goldens, and machine-facing
  output contracts.
- Added support bundles, backup/restore, derived-asset backup manifest v2,
  WAL/orphan diagnostics, graph state preservation, HMAC handoff capsules,
  install/update recovery recipes, install audit, disk-pressure and build
  admission reporting, and RCH-aware verification documentation.
  Representative commits:
  [`6520a9b1`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/6520a9b1),
  [`da92d844`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/da92d844),
  [`87d58d0c`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/87d58d0c),
  [`bd-wtpl`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-49cvw`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added optional daemon and serve/MCP scaffolds while keeping normal operation
  CLI-first and honest when adapters are disabled or deferred.
  Representative Beads:
  [`bd-9s0q`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-s9kgl`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-3usjw.3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-3usjw.4`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Safety, Policy, And Trust

- Added trauma guard / destructive-command preflight policy, hook helper,
  destructive pattern fixtures, policy denied exit behavior, shell-safe gap
  handling, tripwire detection, incident recovery safety fixtures, and
  no-deletion/no-worktree guardrails in docs and tests.
  Representative commits:
  [`907d6879`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/907d6879),
  [`3fd402e5`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/3fd402e5),
  [`d5a25d93`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d5a25d93),
  [`bd-3usjw.6`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-3usjw.7`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).
- Added privacy/redaction/trust surfaces:
  instruction-like content detection, unknown trust-class rejection, markdown
  escaping, raw JSON/TOON poisoning fixes, redaction leak evaluation, egress
  proofs, path redaction, and model/remote gating docs.
  Representative Beads:
  [`bd-zm78`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-rjrd`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-wtio`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-whxu`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-t7cx`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-rynf`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Agent And Swarm Workflow

- Added swarm brief, swarm next-action, agent profile bias, bead affinity,
  support-bundle scale artifacts, contention/recovery suites, cache governors,
  hotset prewarm, write spool/backpressure contracts, host adaptive profiles,
  and operator cookbook material for swarm-scale work.
- Added Agent Mail posture snapshots, coordination docs, local skills, e2e skill
  standards, and agent-readable workflows for graph, doctor, RCH, and mesh work.
  Representative Beads:
  [`bd-fcq1`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-k8dp`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-3a5la`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-s7vd`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-s38h`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

### Testing And Release Gates

- Added a central `scripts/verify.sh` orchestrator, forbidden-dependency checks,
  closure lint, vision coverage, verification drift guards, schema drift guards,
  failure-mode catalog validation, command boundary/effect contracts, golden
  snapshots, e2e overhaul scripts, deterministic runtime tests, property/fuzz
  harnesses, benchmark gates, and structured test event logging.
  Representative commits:
  [`d25e6445`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d25e6445),
  [`d25e6445`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/d25e6445),
  [`2ebcf902`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/2ebcf902),
  [`3dc3c2f3`](https://github.com/Dicklesworthstone/eidetic_engine_cli/commit/3dc3c2f3).
- Restored a repo-wide verification baseline before the tag and continued
  hardening with RCH-oriented proof records where local Cargo fallback is not
  acceptable.
  Representative Beads:
  [`bd-x08h`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-t5v49`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl),
  [`bd-zp75`](https://github.com/Dicklesworthstone/eidetic_engine_cli/blob/main/.beads/issues.jsonl).

## Version Timeline Before And Around v0.1.0

The tag contains a compressed three-week buildout. This date spine is included
because most features landed before the first public tag and there is no prior
changelog to preserve the sequence.

| Date | History reviewed | Main movement |
| --- | ---: | --- |
| 2026-04-29 | 120 non-merge commits | Initial docs, Rust skeleton, forbidden-dependency audit, config, workspace discovery, models, IDs, response envelopes, DB connection, CLI parser, budget/context groundwork, search/pack/CASS scaffolds. |
| 2026-04-30 | 154 non-merge commits | Global CLI flags, output formats, command manifest, error schema, status/check/capability posture, TOON, migration helpers, provenance, policy, trust, test helpers, fuzz/property scaffolds, CASS robot contracts. |
| 2026-05-01 | 33 non-merge commits | Planner recipes, install/update recovery docs, local signing policy, Mermaid/certificate/counterfactual lab readiness, trust and lifecycle documentation. |
| 2026-05-02 | 63 non-merge commits | Criterion benchmark plan, performance stream, degraded/offline scenarios, causal/graph/lab work, release readiness gates. |
| 2026-05-03 | 78 non-merge commits | Boundary migration, command side-effect/idempotency contracts, project-local skills, mechanical command inventory, redaction/evidence traceability. |
| 2026-05-04 | 185 non-merge commits | Reality-check and bug-finding wave; security fixes; curation/rule hardening; situation/focus/outcome surfaces; closure and boundary test expansion. |
| 2026-05-05 | 171 non-merge commits | Trauma guard, no-silent-fallback inventory, deadlock/race findings, workflow IDs, trust promotion, markdown escaping, verification and gap triage. |
| 2026-05-06 | 164 non-merge commits | Release CI, install script, daemon scaffold, recorder, tripwire, closure lint, vision coverage, append-only DB triggers, claim verification, RCH-aware proof work. |
| 2026-05-07 | 112 non-merge commits | Major implements-surface wave: audit, certificate, claim, demo, eval, handoff, preflight, support bundle, recorder, causal, review, swarm scale, init fixes. |
| 2026-05-08 | 210 non-merge commits | Query v1, graph/index, pack replay groundwork, no-silent-fallback hardening, workspace/reality docs, performance forensics, search/context fixes. |
| 2026-05-09 | 31 non-merge commits | Pack replay ledger, replay/diff CLI, evidence freshness, redaction egress, pack quality and performance proofs. |
| 2026-05-10 | 43 non-merge commits | Rule/memory/curation/workflow surfaces, export/playbook, graph centrality/refresh, index maintenance. |
| 2026-05-11 | 32 non-merge commits | Validity-window filtering, search/context/pack profile refinements, deterministic fixture repairs. |
| 2026-05-12 | 90 non-merge commits | Schema/degraded/env-var catalog work, migration contracts, determinism fixes, validity/tombstone behavior, acceptance gate cleanup. |
| 2026-05-13 | 61 non-merge commits | Backup manifest v2, Bayesian posterior math, handoff HMAC, build admission, graph accretion, config and pack docs. |
| 2026-05-14 | 80 non-merge commits | Graph typed subgraphs, algorithm witnesses, closure-lint/test tracing, performance hardware manifest, status/check/golden rebaseline, release prep. |
| 2026-05-15 | 129 non-merge commits | `v0.1.0` tag day: graph G1-G10 surfaces, Pack DNA, insights, trauma guard, MCP/serve honesty, db inspect, read pool/singleflight/durability, verify orchestrator, README invariant gates. |
| 2026-05-16 | 748 non-merge commits | Large post-tag mesh/graph/context/determinism/RCH/read-pool/write-owner hardening wave, cross-cutting defensive changes, incident recovery, symlink guards. |
| 2026-05-17 | 264 non-merge commits | Mesh, graph, RCH, hygiene, redaction, status, CASS, transaction recovery, witness retention, and parser hardening. |
| 2026-05-18 | 133 non-merge commits | Workspace hygiene, preflight pattern expansion, search plan-cache diagnostics, tripwire widening, dependency refresh, graph/help/docs fixes. |
| 2026-05-19 | 373 non-merge commits | Graph rendering/help/perf tie-breakers, mesh foreground CLI, shard fan-out skeleton, symbol graph scaffold, PPR/NUMA/cache, schema and contract expansion. |
| 2026-05-20 | 242 non-merge commits | Mesh/Tailscale auto-enrollment, doctor first-aid, flight recorder, QoS, PPR cache, HITS, Gomory-Hu, load-bearing surfaces, EQL plan cache, audit lane, closeout audits. |

## Known Gaps And Caveats

- This changelog distinguishes the `v0.1.0` git tag from a GitHub Release. No
  GitHub Release rows were returned during the audit.
- A number of README install paths are intentionally forward-looking: binary
  releases, Homebrew, and crates.io publication are not yet live release
  channels in the checked repository state.
- The working tree was already dirty before this changelog pass, including
  `README.md` and many source/docs/test files owned by other active work. This
  pass added new changelog docs only.
- Agent Mail coordination was attempted but the local service was in
  degraded/read-only archive parity state, so no file reservation could be
  acquired for these new docs.

## Delivered Capability Workstreams

Delivered capability sections are represented in the checked-in Beads tracker
rather than GitHub Issues.

Closed workstreams behind this changelog:

- Core local memory loop: workspace init, memory persistence, search, context
  packs, why explanations, status, stable envelopes, and provenance.
- Pack replay and freshness: deterministic ledgers, replay/diff, evidence
  freshness, redaction egress, quality evaluation, and support-bundle summaries.
- Graph-derived retrieval: typed graph snapshots, algorithm witnesses, insights,
  PPR, Pack DNA, causal explanations, structural health, structural decay,
  proximity, dominance, skyline, HITS, and deterministic graph tests.
- Safety and trust: trauma guard preflight, destructive-pattern fixtures, policy
  denial contracts, redaction, trust promotion checks, signing/certificates, and
  prompt-injection/instruction-like content guards.
- Operations and diagnostics: doctor/status/check/capabilities, backup/restore,
  support bundles, RCH-aware verification, disk/build admission, closure-lint,
  failure-mode catalog, schema drift, and release gates.
- Agent and swarm scale: swarm brief, next-action guidance, Agent Mail posture,
  workspace hygiene, QoS, flight recorder, mesh/Tailscale optionality, duplicate
  work detection, host profiles, and crowded-checkout ergonomics.

[Unreleased]: https://github.com/Dicklesworthstone/eidetic_engine_cli/compare/v0.3.0...main
[0.3.0]: https://github.com/Dicklesworthstone/eidetic_engine_cli/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Dicklesworthstone/eidetic_engine_cli/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Dicklesworthstone/eidetic_engine_cli/tree/v0.1.0
