# Team Memory Confederation Plan

Status: active plan
Owning ADR: [ADR 0086 — team memory confederation](../adr/0086-team-memory-confederation.md) (decisions TC-D1…TC-D16; where plan and ADR conflict, the ADR wins and the plan gets corrected)
Related ADRs: 0037 (optional mesh), 0038 (auto-enrollment), 0041 (anti-entropy), 0009 (trust classes), 0069 (global knowledge lane), 0083 (user-global store)
Related beads: bd-30o6g (closed by T1.1), bd-3mw86 (in progress), bd-2gvgw (blocked on/absorbed by T1.4), bd-1bfwa (open epic + .2/.3/.4/.5)
Date: 2026-07-30

---

## 0. TL;DR

Unix live EE-to-EE team confederation is on `main`. `ee team create` /
`invite` / `join` run a signed TCP ceremony, enroll both sides, turn mesh
on unless explicitly disabled, and `ee mesh hello-responder run` /
`ee daemon --foreground` bind inbound (Tailscale LocalAPI when present;
`TeamJoinLocalApi` loopback when every enrolled endpoint is loopback or
tailscaled is absent). `ee mesh sync --once` and `ee team fetch body`
run grant-gated EventFetch/BodyFetch over authenticated frame-v2 TCP.
Authorized BodyFetch hydrates the receiver's `peer_human_attested` stub
so `--memory-scope team` search/pack can recall teammate text.
Sneakernet export/import remains available. The live proof ledger is
`docs/mesh/verification_matrix.md`. Windows inbound uses TeamJoin TCP (Tailscale LocalAPI stays Unix).
A Windows-host DACL soak and production IdP vendor soak remain
environment remainders. Criterion `team_confed` wall-time is
recorded in `docs/mesh/perf_budgets.md`.

This plan turns that foundation into **team confederation**: N human users, each
running `ee` locally on their own machine, forming a trusted mesh over a shared
tailnet that behaves like one unified team memory — with automated peer
discovery, automated background sync, per-person attribution, and a setup flow
simple enough for non-technical users (`ee team create` → send invite code →
`ee team join` → paste into a no-echo prompt).

The plan deliberately does **not** design a new distributed system. It:

1. **Finishes the bridge whose two ends already exist** — reuses the shipped
   frame codec's bounded scaffolding while superseding its rotating-key
   endpoint fields with replay-safe frame v2, then wires hello,
   anti-entropy, and policy to a real socket path (cancel-aware
   `asupersync::net` TCP over the tailnet, no forbidden deps).
2. **Adds the three identity primitives the current design lacks** — a human
   member identity (person, not agent nickname), a cross-machine project
   identity (so two teammates' clones of "the same project" can be recognized
   as such), and a new trust class (`peer_human_attested`) so a teammate's
   deliberate `ee remember` can arrive elevated above generic agent assertion
   without violating the `human_explicit`-is-local invariant. Member identity
   optionally binds to corporate SSO in two tiers: tailnet-attested node
   ownership (zero new dependencies — tailnets already authenticate through
   Microsoft Entra, Okta, Google, and other IdPs) and, for
   capability-compatible secretless public OIDC clients, a verifier-hosted
   device-code flow for ee-level proof with finite, distinct-verifier
   attestation leases. Tier 2 rejects providers that require distributing an
   OAuth client secret; it does not overclaim universal provider support.
   Tier-2 offboarding is bounded by interactive-renewal cadence plus grace;
   it does not claim background directory polling.
3. **Wraps it all in a small product-level command group, `ee team`** — a thin,
   explainable orchestration layer over existing mesh primitives, optimized for
   people who will never read `docs/mesh/`.

Everything stays local-first, default-deny, consent-audited, and CLI-first.
Mesh-off users continue to pay zero cost. No Tokio, no HTTP stacks, no CRDTs,
no cloud.

---

## 1. Why: product motivation

### 1.1 The demand signal

A real prospective customer (a team of investment analysts, non-technical) asked
exactly the question this plan answers:

> "Is there a shared memory layer for the team? Is eidetic engine working right
> now just locally based on one team member? If I want to run an analysis on
> another company how will I know and refer to what another team member ran?"

The honest current answer is: each analyst has a separate local `ee` memory.
Unix team-confed on `main` now creates/joins over signed TCP, turns mesh on
unless explicitly disabled, binds inbound (Tailscale LocalAPI or TeamJoin
loopback), and moves metadata plus grant-gated bodies with `ee mesh sync`
/ `ee team fetch body`. Tailscale is still the production auth substrate;
loopback TeamJoin is the no-tailscaled lab path. Sneakernet export/import
remains available. Windows inbound listen uses TeamJoin TCP; Tailscale LocalAPI WhoIs stays Unix.

### 1.2 The product goal

A team of 2–20 humans, each with `ee` installed locally, should be able to
work as follows. Twenty is a hard v1 active-member protocol bound, not merely
a benchmark size; complete-set overflow conflict-blocks membership-dependent
sharing rather than choosing members by arrival or event hash.

- **Form a team once** with one command and one single-use invite per joining
  ceremony, given an
  existing tailnet (Tailscale is the transport/auth substrate; a plain-language
  one-page setup doc covers installing Tailscale itself).
- **Keep working locally** exactly as before. Local memory stays the source of
  truth on each machine; nothing blocks on the network.
- **Automatically see teammates' shared memories** in `ee search` / `ee pack` /
  `ee ask` results, clearly attributed ("from Priya · project acme-analysis ·
  authored 2026-07-30T14:02Z" — immutable member-attested origin timestamps
  are display/provenance only and never relevance/lifecycle inputs per §7.5
  P4.2; local receipt/sync times appear only in `why`, audit, and status
  diagnostics), within the lanes each
  member consented to share.
- **Ask the team-shaped questions**: "has anyone on the team looked at Acme
  Corp?" → `ee search "Acme" --memory-scope team` or `ee team activity
  --project acme-analysis` shows who captured which project/kind/level and
  when; body text is searchable only where an authorized body is cached.
- **Trust the security story**: default-deny sharing, metadata before bodies,
  redaction + secret scanning on every export, per-member revocation, an audit
  row for every consent and every sync, and an emergency stop.

### 1.3 Why now, and why this shape

- The mesh subsystem (SRR6, 46 closed beads; SRR6.46 zero-touch, 20 closed
  beads) already paid for the hard design work: threat model, lanes, consent
  rituals, anti-entropy math, discovery policy, audit vocabulary. Re-designing
  any of that would be waste; *not* finishing it means the ~25k LOC investment
  keeps returning `mesh_sync_once_network_deferred` forever.
- The client conversation shows the demand is for **team memory**, not
  multi-machine-single-user sync. The current design is explicitly
  single-operator (see §3.3); the missing layer is people, not protocol.
- Non-technical users are the stated audience. That constrains the UX to a
  handful of verbs with safe defaults and printed plain-language explanations —
  not fifteen `ee mesh` subcommands and five TOML files.

---

## 2. Vocabulary

The tracker and code vocabulary is `mesh / peer / peer-group / lane /
workspace-scope / global-lane`. "Confederation" and "team" appear nowhere in the
tree today. This plan introduces **team** as the *product-level* term and keeps
**mesh** as the *mechanism-level* term. Rules:

| Term | Level | Meaning |
|---|---|---|
| **team** | product | A named set of human members whose ee instances confederate. Implemented as: a replicated team manifest + a set of enrolled mesh peers + per-member policy. |
| **member** | product | A human being, identified by a random `member_id` + display name, bound to one or more random ee node IDs whose tailnet/stable-node and signing continuity are authenticated. New primitive (§7.3.1). |
| **mesh peer** | mechanism | A local opaque record for an enrolled remote ee node (`mesh_peers` table). Its `peer_id` is a lookup handle, not the rotating Tailscale key or an authorization principal. |
| **project** | product | A cross-machine identity for "the same body of work" (git repo or shared analysis folder). New primitive (§7.3.3). |
| **workspace** | mechanism | A local path-derived registration, exactly as today (`wsp_*`). |
| **lane** | mechanism | One of the six material lanes (`metadata`, `body`, `embedding`, `graphLink`, `revisionNotice`, `curationSignal`). Unchanged. |
| **confederation** | prose only | The overall capability. Never used in code, schemas, or CLI surface. |

`ee team ...` commands must always print (and emit in JSON) which mesh
primitives they drove, so the mechanism stays inspectable and the product layer
stays thin.

---

## 3. Ground truth: what exists today (verified 2026-07-30)

This section is the evidence base. Every claim was verified against the tree at
commit `8c316c45` by four independent read-only investigations (implementation
map, design docs/ADRs, trust/identity surfaces, beads inventory).

### 3.1 Shipped and reusable (keep, do not redesign)

| Asset | Where | State |
|---|---|---|
| DB schema: `mesh_peers`, `mesh_peer_cursors`, hash-chained `mesh_import_ledger`, `mesh_memory_mappings`, `mesh_body_cache_metadata` | `src/db/mod.rs:4999–5258` (V052–V057) | Real, migrated, CHECK-constrained. The last two tables have no production writers yet. |
| Peer records with fail-closed enrollment gates (consent, handshake, node-key match, capability match), rotate/revoke | `src/mesh/peer.rs:441–631`; CLI `src/cli/mesh.rs:1571–1791` | Real, but the "handshake" is synthesized locally from CLI flags — no live exchange. |
| Anti-entropy protocol math: tips, cursors, bounded range planner, retry/backoff (1s→60s, max 5), digests, redaction-safe sync summary | `src/mesh/anti_entropy_protocol.rs` | Complete + tested. `MeshRangePlanner` has no production caller; file import advances cursors only after destination-local contiguous accepted replay proves the claimed tip. |
| Anti-entropy executable model + 13 pinned scenarios | `src/mesh/anti_entropy_model.rs`, ADR 0041 | The contract to satisfy, not code to run. |
| Signed bounded frame-codec scaffold: blake3-keyed signatures, capability allowlist (`hello`/`summary`/`event_fetch`/`body_fetch`), 64 KiB frame / 32 KiB payload budgets, constant-time compare | `src/mesh/tailscale_transport.rs` (794 LOC) | **Entirely dead — zero callers.** Budgets/enums are reusable; v1's rotating `sourceNodeKey`/`targetNodeKey` identity and long-term-key MAC are not production-safe and are superseded by T2.1 frame v2. |
| Authenticated frame-v2 session and responder-accept substrate | `src/mesh/transport_session.rs`, `src/mesh/key_store.rs`, `src/mesh/responder_broker.rs` | Production path: `ee mesh hello-responder run` / `ee daemon --foreground` construct `ResponderBrokerOwner` from durable enrollments; `ee mesh sync --once` / `ee team steward once` run `TcpMeshForegroundSyncTransport` EventFetch plus grant-gated BodyFetch. TeamJoin LocalAPI binds loopback when tailscaled is absent or every enrolled endpoint is loopback. |
| Hello wire protocol (`ee.mesh.hello.v1` / `.response.v1` / `.error.v1`), ≤4096-byte payloads, version negotiation, privacy-preserving decline | `src/mesh/hello.rs`; `decide_hello_response` at `:405` | Production: unsigned hello+sync is the daemonless first-contact path (`TcpMeshForegroundSyncTransport` / `ResponderBrokerOwner::serve_one`). |
| Discovery policy: `service_tag` (default) / `auto_admit` / `allowlist` on both caller and responder axes; denylist overrides all; TOML files under `<ws>/.ee/` | `src/mesh/discovery_policy.rs`; CLI `src/cli/mesh.rs:1322–1477` | Real and wired for policy *decisions*. |
| Auto-enrollment: 13-step fail-closed flow, forensic audit-before-write, tailnet/node-key identity guard, rollback | `src/mesh/auto_enrollment.rs`, `auto_enrollment_safety.rs`, `identity_change_guard.rs`; CLI `src/cli/mesh.rs:1164–1320` | Real, transactional. But see trust dead-end in §3.3. |
| Mesh policy engine: per-peer per-lane per-origin-workspace inbound/outbound decisions, trust-lane ceilings, side-effect booleans | `src/core/memory_scope.rs`; facade `src/mesh/policy.rs`; production callers in `src/mesh/foreground_cli.rs` and `src/cli/mesh.rs` | Complete and load-bearing for file export/import, consent previews, live EventFetch/BodyFetch, and inbound serve. |
| Authenticated lane-grant consent (DB-backed counts, revision-pinned candidates, redacted samples, cautions, grant/revoke) | `src/mesh/lane_grant_preview.rs`, `src/mesh/lane_grant.rs`; CLI wiring in `src/cli/mesh.rs` | T1.4 is wired: ordinary previews are deterministic and token-free; explicit robot issuance binds the complete eligible memory and mesh-ledger candidate set, and grant/revoke mutate generation plus audit atomically. |
| Pre-export secret scan (hard-denies `ee mesh export` with `mesh_secret_export_denied`) | `src/policy/mod.rs` | Real and enforced. The pure detector remains deterministic and value-free; the command boundary decorates findings with fresh opaque CSPRNG-backed `findingId` values before error/audit projection. |
| `ee mesh export` / `ee mesh import`: bounded, schema-gated, idempotent, ledger-writing, index-job-enqueuing file exchange | `src/cli/mesh.rs` | Real sneakernet path. Import artifacts cannot enroll or re-enable peers, overwrite local peer policy, or advance cursors without durable contiguous accepted replay. Live TCP EventFetch/BodyFetch is the primary team-confed path on Unix. |
| `ee share preview`: DB-backed counts and redacted examples | `src/cli/share.rs`, `src/policy/mod.rs` | Real, read-only, and policy-backed. Public content/aggregate hashes and `--record-consent` are removed; unknown peers fail closed with an explicit degraded signal. |
| Emergency disable/reenable, workspace-scoped, honest `PeerScopeNotDurable` for `--peer` | `src/mesh/emergency_disable.rs`; CLI `src/cli/mesh.rs:923–1012` | Real (peer-scoped containment owed under bd-3mw86, in progress by another agent). |
| Read-side visibility filter for mesh-derived hits | `src/core/search.rs:9816–9830`, `src/core/context.rs:6484` | Real and wired. |
| Local Tailscale probe: unix-socket LocalAPI with a ~60-line hand-rolled HTTP client + subprocess fallback, binary authenticity check | `src/core/tailscale_probe.rs:887–1020` | Real. **The only real network I/O in the subsystem — and the precedent that std::net networking is compatible with the forbidden-deps policy.** |
| Memory scope chokepoint incl. a `team` lane over agent nicknames | `src/core/memory_scope.rs:1147–1161`; config key `trust.team_members` (`src/config/file.rs:1344–1358`) | Real; `team` = hand-listed agent names, undocumented, and **not** exposed on `ee pack` (see §3.3). |
| Fake-Tailscale harness (Rust + bash fixtures + python socket responder) | `tests/support/fake_tailscale.rs`, `scripts/e2e_overhaul/lib/fake_tailscale.sh` + fixtures | Real; the behavioral E2E backbone to extend. |
| Mesh-off guarantees: byte-stability, no sockets, no degraded noise | `tests/mesh_off_no_network.rs` (494 LOC, uses `lsof`) | Real regression gate. Must stay green throughout. |

### 3.2 Dead or unwired modules (~8.4k LOC waiting for callers)

`admission.rs` (931, zero refs anywhere), `steward_decision.rs` (1135),
`peer_state.rs` (471), `tailscale_transport.rs` (794), `anti_entropy_model.rs`
(1632, test-only), `policy.rs` (687, test-only), `remote_evidence.rs` (665,
test-only), `cache.rs` (1718, test-only), `discovery_cache.rs` (349, one const
read). This plan gives production callers to all of them except
`anti_entropy_model.rs` (which stays an executable spec); `remote_evidence.rs`
and `cache.rs` get theirs in the body-lane milestone (P4.6), not before.

### 3.3 Missing primitives and load-bearing blockers

1. **Production transport owner is wired on Unix.** `TcpMeshForegroundSyncTransport`
   is the foreground supervisor transport. `ee mesh hello-responder run` and
   `ee daemon --foreground` construct `ResponderBrokerOwner` from durable
   enrollments. `NoopMeshForegroundSyncTransport` remains a test double only.
   Windows inbound listen uses TeamJoin TCP; Tailscale LocalAPI WhoIs stays Unix.
2. **Discovery reads metadata nobody publishes.** The production hello probe
   (`TailscaleStatusCapabilityHelloProbe`,
   `src/mesh/tailscale_autodiscovery.rs:194–237`) performs no I/O; it parses
   `eeVersion`/`eeProtocol`/`workspaceIds` out of the peer's Tailscale ACL
   `Capabilities` map (`src/core/tailscale_probe.rs:1265–1284`) — which no code
   and no documented operator flow ever sets. On a stock tailnet every peer is
   skipped as `non_ee`.
3. **No human identity.** The only identity axes are agent nickname
   (`EE_AGENT_NAME`, spoofable free text), machine (`node_*`/`peer_*`), and
   workspace (`wsp_*`). `ProducerIdKind::Human`
   (`src/policy/producer_normalization.rs:43`) is the sole human-shaped concept
   and is unwired. `audit_log.actor` is free text.
4. **No cross-machine project identity.** Workspace IDs and fingerprints are
   blake3 hashes of the **absolute local path**
   (`src/config/workspace.rs:1239–1243`, `src/cli/mod.rs:33670–33675`). Two
   teammates cloning the same repo can never agree they share a project.
   Nothing derives identity from git root-commit or remote URL. The mesh
   design's answer is a manually configured n×n `origin_workspace_ids`
   allowlist (`src/config/file.rs:693`).
5. **The `human_explicit` ceiling.** Three independent enforcement points cap
   inbound peer material at `agent_validated`:
   `mesh_peer_import_trust_class_rejection_reason`
   (`src/core/memory_scope.rs:1407–1414`),
   `permits_import_as_human_explicit()` unconditionally `false`
   (`:273–276`), and `MeshTrustLane::permits_peer_import()` rejecting
   `localHuman`. Deliberate and correct for anonymous peers — but it means a
   teammate's deliberate `ee remember` arrives indistinguishable from any
   agent's guess. A team product needs a middle tier (§7.3.2).
6. **The auto-enroll trust dead-end.** Auto-enrolled peers get
   `trust_established_by = "tailscale_auto_enrollment"`
   (`src/cli/mesh.rs:2685`), which permanently fails `is_trusted()`
   (`src/mesh/peer.rs:302–308`), which gates `foreground_sync_peer_allowed`
   (`src/mesh/foreground_cli.rs:831–837`). Zero-touch enrollment and sync
   eligibility are mutually exclusive today.
7. **No cryptographic peer credential.** `public_key_fingerprint` is an opaque
   operator string or fabricated `auto:blake3(node_key)`
   (`src/cli/mesh.rs:2751–2753`). No key exchange, no verification on any
   enrollment or import path. (The dead frame codec's blake3-keyed framing
   is scaffolding only; its long-term-key MAC is superseded by T2.1 frame v2
   before any production caller.)
8. **`ee mesh grant` and lane-level revocation do not exist** despite explicit
   widening/narrowing being the load-bearing consent mechanism. ADR 0038
   (:104,203,403), the lane-grant preview module header, schema description,
   and onboarding doc reference grant; no command can apply that preview or
   later narrow one lane without revoking the entire peer.
9. **`ee pack` lacks `--memory-scope`/`--strict-scope`** even though
   `README.md:627` and `docs/cli-reference/graph-flags.md:54` claim otherwise;
   packs only inherit scope from a task lens with `strict_scope` hard-coded
   `false` (`src/cli/mod.rs:39648, 39853`).
10. **An unauthenticated trust bypass exists.** `ee export` stamps
    `import_source=native` into every artifact header
    (`src/core/backup.rs:2668`); `ee import jsonl`'s only trust guard fires for
    *external* sources (`src/core/jsonl_import.rs:1351–1361`), so a teammate's
    exported `human_explicit` rows import as `human_explicit` with no
    signature, no origin check, no policy consultation. `ee playbook import`
    similarly passes `trust_class` through verbatim (`src/core/rule.rs:2273`),
    attenuating only maturity. **The informal manual team-sync path is strictly
    more permissive than the mesh.** This must be closed, not formalized.
11. **Resolved baseline defect — consent and secret-scan hashes were content
    oracles, and preview “consent” had no matching effect.** At the audited
    baseline, share-preview hashes and `MeshExportSecretFinding.value_hash`
    enabled offline equality checks, while `--record-consent` mutated state
    without applying the reviewed exposure. P0.1/P0.3/T1.4 removed those public
    hashes and the side-effecting preview flag, introduced fresh per-scan
    `findingId` values, and bound actual grant mutations to a short-lived,
    nonce-salted, surface-bound store-local token consumed in-process or
    through bounded stdin—never argv or environment variables.
12. **Status surfaces lie by hardcoding.** `ee mesh status` reports a fixed
    lane policy regardless of config (`src/mesh/foreground_cli.rs:1296–1303`);
    hello-responder `running:false`, discovery-cache `not_loaded`, peer-state
    zeros, empty drift, `steward_posture: not_inspected` are all constants
    (`:1149–1197`, `:1225–1240`). `probe_mesh_capability()` honestly returns
    `Unimplemented` when `EE_MESH_ENABLED=true` (`src/core/status.rs:1305`) —
    note it string-matches only `"true"`, not `"1"`.
13. **Selective sync is display-only.** `SelectiveSyncConfig::safe_starter_config().summary()`
    is the only production use (`src/mesh/foreground_cli.rs:1028`); no
    persisted subscriptions.
14. **Degraded-code hygiene debt.** Of 52 mesh codes in the catalog golden,
    only 33 have failure-mode fixtures and 34 taxonomy entries; e.g.
    `mesh_sync_once_network_deferred`, `mesh_disabled`, all seven
    `mesh_peer_*` codes lack fixtures (AGENTS.md requires fixture + taxonomy
    per emitted code). Three audit test files contain only a newline (1 byte;
    no tests)
    (`tests/mesh_{tailnet_change,identity_change_guard,discovery_policy}_audit.rs`).
15. **Effect-registry and docs drift.** `src/core/effect.rs` declares
    nonexistent `.ee/mesh/*.json` paths and a never-created `mesh_audit_events`
    table (`:1934–1955, 2388–2418, 2648–2658`); README documents a
    `[mesh.tailscale]` config block that `MeshConfig::parse` never reads
    (`README.md:1391–1393` vs `src/config/file.rs:512–521`) and a `.ee/mesh/`
    directory that is never created (`README.md:1516`).

### 3.4 Open-work interlocks (respect, don't duplicate)

| Bead | Status | Interaction with this plan |
|---|---|---|
| **bd-30o6g** (P2) — remote-evidence byte policy trusted declared size, not fetched body length | closed by T1.1 (`63514470`, lint follow-up `40a1c0c8`; audited in `fbcc6252`) | **Completed hard prerequisite.** T5.9 must reuse the bounded `max_bytes+1` reader rather than recreate a post-hoc size check; its first CLI emitter also owns the same-commit failure-mode fixtures/taxonomy for the four module codes. |
| **bd-3mw86** (P1) — `ee mesh disable --peer` lacks durable per-peer containment | in_progress (another agent) | Do not touch. The team UX consumes whatever per-peer suspension state lands; note it as a soft dependency of the incident-containment story. |
| **bd-2gvgw** (P3) — pin lane-grant preview nested `required` fields | blocked on T1.4 (`bd-tc-epic-qzk7o.2.2`); absorbed | T1.4 owns the final authenticated-preview schema and closes both beads together. The older infrastructure blocker is closed, but the new product dependency is intentional. |
| **bd-1bfwa** (P2 epic) — global knowledge lane (within one user, cross-workspace) | open, 4 live children | ADR 0069 explicitly fences mesh out of the global lane. Team lane is a *sibling* lane at the same retrieval chokepoint (`memory_in_scope_with_tags`). Sequence the team retrieval work to coordinate (same files: `src/core/memory_scope.rs`, search/context integration), and pin the precedence chain local-workspace > team > global in one place. No hard dependency either direction, but the two efforts must not silently redefine each other's scope semantics. |
| bd-36bbk.2 / bd-36bbk.3 (closed) | — | Closed on the *seam*, not a working transport; their close notes explicitly defer production wiring. This plan is the successor they anticipated. |

---

## 4. Product requirements and user stories

### 4.1 Personas

- **Hana, team lead (semi-technical).** Can install two tools from a doc and
  paste commands. Sets up the tailnet and the team; decides sharing policy.
- **Priya, analyst (non-technical).** Uses `ee` through an agent harness or a
  handful of memorized commands. Must never need to edit TOML or understand
  lanes.
- **Marcus, compliance (non-user).** Needs to answer: who could see what, who
  consented to what, when, and how do we shut it off.

### 4.2 User stories with acceptance sketches

**US-1 — Create a team.** Hana: `ee team create "Acme Research"`.
→ Team manifest created locally; team_id minted; printed summary explains what
will and won't be shared by default (new metadata from activation onward yes;
pre-team history remains local until `ee team share history`; bodies no until
per-node consent; shared metadata is team history visible to future members
until withdrawn; embeddings never by default). JSON mode emits
`ee.team.create.v1`.

**US-2 — Invite.** Hana: `ee team invite` → short-lived single-use code
(pasteable in Slack): `eeteam1-<base32…>`. Printed text tells her exactly what
the code authorizes and when it expires (default 72h, configurable), and that
`--for` is a label rather than recipient binding: an unbound code is a bearer
credential until redeemed or revoked. The simple command is usable before
daemon installation: it registers and returns when a broker is already live,
otherwise prints the code and waits as the foreground responder. It never
silently creates an invite that no process can serve.

**US-3 — Join.** Priya (on the tailnet, ee installed): `ee team join` and
pastes `eeteam1-…` into the no-echo prompt (agents use `--invite-stdin`) → the
command resolves Hana's exact Tailscale stable node ID from fresh local
status (the embedded current key/address values are hints and may rotate),
performs a real hello handshake, mutually enrolls both
machines with `explicit_human_consent` (the humans typed the code — that *is*
the consent ceremony), registers Priya as a member, replicates the manifest,
prints a plain-language summary of what she is now sharing and with whom, and
records consent audit rows on both sides. A `--dry-run` preview exists. Exit 0
only if the team is actually joined and the first metadata sync round
completed. The summary is explicit that pre-team history did not silently
leave the machine and offers the preview-first `ee team share history`
command.

**US-4 — Unified recall.** Priya, six weeks later, is assigned Acme Corp:
`ee search "Acme Corp" --memory-scope team --json` (and `ee pack "prep Acme
analysis" --memory-scope team`) returns her own memories plus teammates'
shared memories, each attributed (member display name, project, origin trust,
origin-authored-at; local receipt time remains diagnostic). `ee team activity
--project acme-analysis` lists attributed project/kind/level metadata and body
availability. It never invents titles from body text. This answers what a
teammate **captured/shared**, not every shell command or private session they
ran; raw execution telemetry remains in local CASS unless separately shared
through an explicit future surface.

**US-5 — Background freshness.** Members' machines sync automatically while
online (daemon-hosted supervised job, bounded budgets). `ee team status` shows
per-member reachability and staleness ("Hana: synced 4m ago · Marcus-laptop:
unreachable 3d"). Retrieval commands never block on a peer; explicit sync/join
commands use visible bounded deadlines. Stale is visible, not silent.

**US-6 — Widen sharing deliberately.** Hana decides rule bodies should flow:
`ee team share bodies --with priya` (or `--all-members`) → runs the lane-grant
preview against real candidate memories, shows counts + redacted samples +
cautions and the exact current recipient-node set, requires explicit
confirmation, records consent, materializes the lane grant via `ee mesh
grant`. A node added later does not inherit that body grant. Secret scan still
hard-denies risky exports. `ee team unshare bodies --with priya` advances the
same recipient nodes' grant generations and stops future serving from the
named current local source; it states plainly that other source nodes are
unaffected, bytes Priya already fetched or copied cannot be remotely erased,
and no origin-wide `shareWithdraw` is emitted.

**US-7 — Someone leaves.** `ee team member remove marcus` → revokes his
machines' durable session/grant authorization generations and future serving
locally, appends a signed removal with per-origin cutoffs, idempotently
cancels open connections, attempts bounded fanout/relay, and prints the
acknowledgement matrix plus the honest caveat that already-synced copies on his
machine cannot be remotely deleted. It does not misuse origin-wide
`shareWithdraw` as a recipient revocation. If Marcus added members whose
events remain inside his accepted prefix, the command also names them as
still active, emits `team_delegated_member_review_required`, and recommends
pause/review; it never implies removing Marcus removed those members.

**US-8 — Emergency stop.** `ee team pause` commits a durable local
workspace/team pause generation before cancelling sessions and unregistering
the route; every import/serve/frame boundary rechecks it.
`ee team resume --confirm` revalidates the root, keys, identity, and policy
before advancing the generation, and never reuses a stale session. Pause
stops future network exchange; it does not erase local caches or copies peers
already received. Marcus-the-compliance-officer can still read the
side-effect-free `ee team status`, `doctor`, and `audit --json` surfaces while
paused.

**US-9 — Non-git projects.** The analysts' "Acme" workspace is a plain folder,
not a git repo. `ee team projects share ./acme-analysis --name acme-analysis`
mints a team-scoped project id and publishes it in the manifest; Priya's `ee
team projects adopt acme-analysis --path ./clients/acme` maps her local
workspace to it. Git repos get this automatically via root-commit derivation.

**US-10 — Agent harnesses keep working.** Everything above has `--json` with
stable schemas; agents can drive the same flow; `ee pack` gains the scope
flags the README already promises.

**US-11 — Members are their corporate accounts.** Hana's org uses Okta (or
Microsoft Entra, or Google Workspace). She runs `ee team idp require
--tailnet-attested` (tier 1) so every member record is bound to the SSO login
that owns their Tailscale node — `ee team members list` shows
`priya@acme.com (verified via tailnet)`, and a node owned by any other account
cannot join or sync as Priya. With tier 2 enabled for a
capability-compatible secretless public device client (`ee team idp set
--issuer https://acme.okta.com ...`), a distinct verifier hosts the
device-code sign-in ("open https://acme.okta.com/activate and enter
QXZ-JKP") and the member record carries an IdP-verified subject + email.
Providers that require distributing a client secret receive an honest
unsupported-provider result; tier 1 remains available.

**US-12 — Offboarding follows the directory.** Marcus removes a departed
analyst from Okta. Tier 1 observes the tailnet-owner mismatch/disappearance on
its next noninteractive check. Tier 2 does not poll the directory in the
background: the member's finite attestation becomes due, and offboarding
prevents the required interactive renewal with a distinct active verifier.
After the configured grace, peer grants suspend with an audit row and status
shows `identity_revalidation_failed`, without relying on someone to remember
`ee team member remove`. The bound latency is revalidation interval + grace,
not an instantaneous directory push.

### 4.3 Explicit non-requirements (v1)

- No central server, no cloud relay, no SaaS control plane.
- No real-time collaboration semantics; bounded staleness (minutes) is the
  contract, matching ADR 0041's local-cache read semantics.
- No cross-team federation (a machine may belong to at most one team per
  workspace in v1; multi-team is a v2 question).
- No web UI.
- No automatic conflict resolution — conflicts are surfaced as evidence
  (existing `ee.peer_conflict.v1` design), never auto-merged.
- No non-Tailscale transport in v1 (LAN mDNS, WAN relay etc. are out).
  Tailscale is already the assumed substrate and gives mutual machine
  authentication, NAT traversal, and encrypted links for free.
- No IdP round-trip inside any core command. SSO/OIDC touches the network only
  in explicit `ee team` identity commands (join, idp set, revalidate); search,
  pack, remember, and sync never call an IdP. Local-first stands.
- Per-peer selective-sync *subscriptions* (the `sync.rs` profile/subscription
  machinery) stay a display-only preview in v1; lane grants + policy are the
  sharing control surface. Wiring subscriptions is explicitly deferred, not
  silently dropped.

---

## 5. Invariants, non-goals, and previously rejected alternatives

### 5.1 Inherited invariants that this plan must keep true

From ADR 0037 / AGENTS.md / README (all currently enforced by tests that must
stay green):

- Mesh disabled ⇒ zero network activity, zero daemon requirement, byte-stable
  output (`tests/mesh_off_no_network.rs`).
- Local-first reads: Tier-1 answers come from local state; peer probes never
  silently mutate a returned pack/search result; `revisable` mode emits
  revision tokens instead of blocking.
- No Tokio / async-std / smol / hyper / axum / tower / reqwest anywhere.
  Networking uses pinned Asupersync 0.3.9 nonblocking TCP with `Cx`
  cancellation/deadlines; blocking `std::net` reads never occupy runtime
  workers.
- No core command requires the daemon. The daemon accelerates (background sync,
  inbound responder); foreground one-shots must still work.
- Tailscale reachability is transport evidence, never authorization.
- Imported peer material is evidence, not local truth; every remote artifact
  carries origin + provenance + trust lane + policy metadata.
- No silent memory mutation; every promotion/consolidation/tombstone audited.
- Bounded everything: frame sizes, payload budgets, retry counts, round
  budgets, per-peer fanout (existing constants keep their values unless an ADR
  amendment says otherwise).
- Determinism: same DB + indexes + config + query ⇒ byte-identical JSON.
  Anything network-derived lives in clearly non-deterministic surfaces (sync
  reports, status probes) — never inside pack/search payloads except as
  imported-then-indexed local state.
- Secrets never leave: pre-export secret scan stays a hard deny on every
  outbound path, including the new transport.

### 5.2 Previously rejected alternatives (do not re-propose)

ADR 0037: eager full replication; purely federated search; CRDT-first global
graph; HTTP/gRPC transport; daemon-required operation; Tailscale-as-trust.
ADR 0038: wizard UI; always-on auto-reconciliation; separate `ee mesh serve`
process (responder belongs inside `ee daemon`); shared peer-group row across
workspaces; body-lane default-allow; TTL-only staleness (keep the per-peer
state machine); prose repair plans. ADR 0041: gossip; Paxos; CRDT merging;
linearizability; global sequence numbers; automatic conflict resolution;
unbounded retry.

This plan's additions honor all of the above. Where the team UX *appears* to
soften something (e.g. `ee team join` auto-enrolling peers), §7 shows it
actually routes through the existing consent gates with the human ceremony
supplying the consent.

### 5.3 New non-goals introduced by this plan

- **`human_explicit` remains strictly local.** We add `peer_human_attested`
  *below* it rather than ever letting the top class cross a machine boundary.
- **No transitive trust.** Membership and lane grants are per-member facts a
  local operator can always inspect and revoke; a member added by Hana is not
  automatically trusted *more* because Hana is trusted.
- **No silent membership changes.** Every member add/remove/key-rotation is an
  audited event in the manifest stream, attributed to the member who performed
  it.

---

## 6. Architecture overview

Six pillars, each independently valuable, ordered by dependency:

```
P0  Wire-reality groundwork      (make existing surfaces tell the truth,
                                  wire the policy engine, close the trust
                                  bypass, preserve the bounded byte-cap invariant)
P1  Real transport               (listener in daemon + client in CLI,
                                  hello over TCP, anti-entropy rounds that
                                  advance cursors, metadata lane first)
P2  Identity                     (members, projects, peer_human_attested,
                                  pairwise keys from invite ceremony)
P3  ee team UX                   (create/invite/join/members/status/sync/
                                  projects/share/pause, manifest replication)
P4  Unified retrieval            (pack/search team scope + attribution,
                                  team activity, precedence, conflicts)
P5  Operations                   (background sync steward, daemon service
                                  install, doctor, audit views, perf gates,
                                  operator + client docs)
```

Data-plane picture at steady state (two members shown):

```
 Hana's machine                                Priya's machine
 ┌───────────────────────────┐                ┌───────────────────────────┐
 │ ee CLI  ──local truth──►  │                │ ee CLI  ──local truth──►  │
 │ FrankenSQLite (memories)  │                │ FrankenSQLite (memories)  │
 │   ├─ mesh_import_ledger   │   tailnet      │   ├─ mesh_import_ledger   │
 │   ├─ mesh_peer_cursors    │  (WireGuard,   │   ├─ mesh_peer_cursors    │
 │   └─ team manifest cache  │   port 41888)  │   └─ team manifest cache  │
 │ ee daemon (optional)      │◄──────────────►│ ee daemon (optional)      │
 │   ├─ hello responder      │ session MACs + │   ├─ hello responder      │
 │   └─ sync steward job     │ signed events  │   └─ sync steward job     │
 └───────────────────────────┘                └───────────────────────────┘
      ▲ local search/pack reads cached, policy-admitted peer rows
      │ with member attribution; never blocking on the network
```

(Each side's local DB remains its source of truth; the other side's material
lives in the import ledger + derived index, admitted per-lane by policy.)

**Listener asymmetry, stated plainly:** every exchange requires the
*counterparty's* responder to be listening, and the only long-lived listener
home is the daemon, but an established session is data-bidirectional. Both
endpoint workspaces advertise tips and may request missing ranges over the
same connection. A member whose machine never runs the daemon cannot be
contacted, but its foreground/scheduled outbound sync both contributes and
receives; two members with no responder still cannot exchange. `ee team
status` surfaces this per member. The join ceremony provides a foreground
accept path
(`ee team invite --wait`, §7.4 P3.2) so M3 never depends on daemon install
(M5). There is still exactly one local listener owner: `--wait` delegates to
the daemon control channel when the daemon is live, otherwise it acquires an
exclusive responder lease and binds in-process. A startup race has one lease
winner; an unrelated port occupant is diagnosed rather than triggering a
second port or wildcard bind. That owner is a user-scoped broker, not a
workspace-scoped socket: it routes only exact, locally registered
`(team_id, target_workspace_id)` or pending-invite IDs from the hardened
workspace registry. The target is the responder's local DB workspace, not an
event producer's `origin_workspace_id`. The broker revalidates each
owner-safe/non-symlinked database and team genesis before serving, and lets
other same-EUID workspace processes
register over a bounded private-runtime UDS without sending invite secrets
or bodies. Network input never supplies a local path or triggers DB scanning;
stale/ambiguous routes fail closed. Each direction writes at most the
receiver's one handshake-bound workspace database. The existing daemon is `#[cfg(unix)]`
(`src/daemon/server.rs:25`);
Windows members are client-only in v1 and documented as such (§7.6 P5.2);
their outbound rounds can still send as well as receive.

---

## 7. Detailed design

### 7.1 P0 — Wire-reality groundwork

Rationale: building new features on surfaces that hardcode their answers would
compound dishonesty the project has worked hard to eliminate (the
honesty-only/implements-surface taxonomy exists precisely because of this
failure mode). P0 makes the substrate truthful and closes the security holes
that real transport would otherwise amplify.

**P0.1 — Remote-evidence streaming byte cap — COMPLETE 2026-07-30
(`bd-tc-epic-qzk7o.2.6`, absorbing `bd-30o6g`).**
Commit `63514470` added the bounded `max_bytes+1` reader and actual-length
enforcement before hashing or persistence; `40a1c0c8` completed the strict
lint proof, and `fbcc6252` audited the implementation/plan handoff. Declared
size mismatches and hash mismatches quarantine, exact-cap and cap-plus-one
boundaries are covered, and current call sites remain test-only. T5.9 must use
this reader at its fetch boundary. Because the four codes are not yet emitted
by a public CLI response, their failure-mode fixtures and taxonomy entries
land atomically with T5.9's first CLI emitter rather than pretending the
module-only result already has a response-surface catalog entry.

**P0.2 — Wire the policy engine into the existing file paths.**
`ee mesh export` consults `decide_mesh_outbound_policy` per record and lane;
`ee mesh import` consults `decide_mesh_import` per event, recording the policy
decision JSON in the ledger columns that already exist for it (V053/V055).
Artifact control rows remain non-authoritative: an imported peer row may only
refresh cosmetic metadata for an exact enabled local enrollment, while unknown,
disabled, rotated, and duplicate peers are audited rejections. Cursor claims are
evaluated after event replay and advance only across destination-local `allow`
rows whose producer, sequence, previous-hash chain, and final tip all match;
sender status, timestamps, and audit-tip claims are discarded.
`ee mesh export` requires `--peer <peer-id>` naming an enrolled, enabled peer
and always applies that peer's outbound policy; it has no peer-less self-export
bypass. Ordinary local backup remains the responsibility of `ee export` and
`ee backup`.
This path continues to carry the existing transport-independent
`ee.mesh.event.v1` file-replay rows as non-origin-authoritative evidence. Its
policy entry point exposes a versioned normalized request that P1.3 can reuse
for typed signed team events without reserializing, signing, or upgrading a
legacy file row into team origin authority.
`[[mesh.peer_policies]]` config becomes load-bearing. `MeshPeerPolicyRegistry`
gets its first production caller. Acceptance: a configured `body: deny` policy
observably strips bodies from an export artifact; an import of a denied lane
records `mesh_peer_policy_denied` in the ledger instead of upserting rows.

**P0.3 — `ee share preview` becomes policy-backed.** Replace the hard-coded
`policy_action: "allow"` verdicts (`src/cli/share.rs:221–247`) with real calls
into the outbound policy for the named peer; unknown peer ⇒ explicit
`share_preview_peer_unknown` degraded entry instead of pretending. Make the
surface strictly read-only: remove `--record-consent`. A grant/body-share
mutation records consent only when it consumes the exact approval token; an
export audits its actual policy-checked export effect rather than claiming
that preview alone recorded consent. Remove the per-example public content
hash and aggregate public preview hash; memory ID + entity revision identify
a sample locally without creating a dictionary oracle. Also replace the
pre-export secret scan's public
`valueHash` with a fresh, at-least-128-bit OS-CSPRNG `findingId` for each scan
occurrence. The same ID may correlate one report/error with its audit entry,
but repeat scans deliberately differ and the ID is never content-derived.
Keep the pure detector deterministic and identifier-free: after its canonical
sort/dedup, the effectful command layer decorates findings in order using an
injected secure-random source. Tests inject fixed randomness without exposing
a production seed/bypass. Randomness/serialization failure is an
`ee.error.v2`, never a successful hash-shaped identifier.

**P0.4 — DB-backed lane preview, grant, and revocation.**
Feed the existing `compute_lane_grant_preview` real candidate memories from the
DB (the module already samples/redacts/bounds at 500), real `peer_in_group`,
real redaction rules. Change the preview target from its current raw
`<node-key>` argument to the enrolled opaque `<peer-id>`, and add the missing
`ee mesh grant <peer-id> --lane <lane>` mutation: authenticated
preview-token-pinned, audited in the grant transaction, and updating the peer
record's lane grants. Ordinary human/JSON preview is deterministic and
token-free. Human mode previews/confirms/applies in one process without
printing the token. Robot mode must explicitly pass
`--issue-approval-token`; only that response adds a marked-sensitive
15-minute `approvalToken` with recognizable `eeap1_` prefix. Grant accepts it
only through bounded `--preview-token-stdin`, never argv/env.

The versioned opaque token exposes no stable store/workspace/key identifier.
It carries a fresh 32-byte nonce, bounded issue/expiry times, a nonce-salted
keyed snapshot tag, and a separate context-bound envelope MAC derived from
T1.6. Verify the envelope MAC first:
malformed, future-issued, wrong-store/workspace/surface/key, or bad-MAC input
is `mesh_approval_token_invalid`. Then reconstruct the canonical snapshot and
compare its keyed tag: expiry or drift is `mesh_approval_token_stale` plus a
structured re-preview action, never a replacement bearer in the error.
Human mode may re-render the current token-free preview and ask again
in-process; robot mode makes a separate preview call. This is intentionally
not a bare MAC of current state, which could not distinguish forgery from
staleness. Both human and JSON render from the snapshot, which binds
schema/copy version, target + grant generation,
current/proposed policy generation, the complete revision-pinned candidate
set, sample strategy/limit, exact ordered redacted samples, and caution codes.
The nonce makes equal previews unlinkable; raw content/sample hashes are
absent. Envelope verification, snapshot comparison, generation
compare-and-swap, grant, and consent audit occur in one write transaction, so
concurrent/replayed apply cannot grant twice. Audit stores only a
domain-keyed identifier of the high-entropy nonce, not token/tag/samples. Add
the symmetric, idempotent
`ee mesh revoke-lane <peer-id> --lane <lane>` mutation; narrowing needs no
exposure preview, but is audited, advances the node-scoped grant generation,
closes future serving immediately, and prevents a stale grant/preview from
re-enabling the lane. A later re-grant requires a fresh preview of the new
generation. Both commands state that revocation cannot erase bytes the peer
already cached or copied. Neither command treats the peer ID as cryptographic
identity. M0 ends at an opaque-handle, versioned grant-target adapter and
persists enough generation state for later migration; it does **not** wait for
M1/M2 identity types. T2.2/T3.1 consume that adapter when stable ee-node
bindings land, migrate/resolve grants to the exact ee node + generation, and
own the proof that current Tailscale key rotation cannot retarget or silently
drop consent.
Absorb bd-2gvgw's schema `required`-field pinning. Acceptance: grant without a
matching fresh preview token fails closed; cross-store/workspace/surface/key
replay, expiry, nonce/tag tampering, concurrent double-apply, and every
snapshot-field mutation fail with zero side effects; token input is bounded
and absent from argv/env/ee-controlled trace/error/audit/support and CASS-
import materialization; invalid/stale errors contain only structured
recovery, never a replacement token. Default preview JSON is deterministic
and token-free; explicit issuance is nondeterministic/fallible, the envelope
contains no stable identifier, token-prefix/field redaction is tested, and
operator copy names the external-recorder capture residual. Granted lane visibly changes
export/import policy decisions from P0.2; revoke-lane
immediately reverses
future serving, is replay/idempotence safe, never claims remote erasure, and
the versioned handoff contract is pinned without requiring later milestone
types.

**P0.5 — De-hardcode status/report fields.** `ee mesh status` reports the
*actual* configured lane policy, actual discovery-cache state, actual peer
state breakdown, actual responder posture (from P1 it becomes genuinely
probeable); `self_advertised_tags` comes from the real probe instead of
hardcoded-empty (`src/cli/mesh.rs:2256`), so `discovery_policy_no_ee_mesh_tag`
only fires when true. `probe_mesh_capability` accepts `"1"` as well as
`"true"` and stops reporting `Unimplemented` once sync actually works (flip in
P1, not before).

**P0.6 — Store-local authentication root + close the unauthenticated trust
bypass.** Establish one hardened store-local authentication root with
fallible, domain-separated subkey derivation, key IDs/rotation, constant-time
verification, a known-answer self-check, and the TC-D14
no-output/no-audit/no-backup contract. T1.4 and T5.9 derive distinct
snapshot-tag, token-envelope, and audit-token-ID keys; import/playbook MACs
have separate domains. Secret-finding occurrence IDs use fresh OS randomness
and are deliberately independent of this root. No raw root or subkey enters
SQLite, and a current-key-only approval envelope carries no key ID.

`ee import jsonl` must not
grant `human_explicit` on the strength of a spoofable `import_source=native`
header. Change: native trust is **authenticated, not merely identified** — the
exporting store MACs a constant-size versioned canonical header containing the
artifact family/schema, canonical record-encoding version, source store-key
namespace, exact source workspace/scope, key ID, record count, and a
domain-separated ordered `records_root`. `ee export` and playbook artifacts
use distinct MAC domains and record type tags, so a valid header cannot be
replayed across surfaces or scopes. The root is a streaming digest over
length-prefixed `(ordinal, record ID, canonical record hash)` entries, and
export computes it and emits rows from one consistent read snapshot. Import
verifies the MAC against the expected local store key and exact artifact
context before honoring native trust; absent, invalid, foreign, wrong-family,
or wrong-workspace MAC ⇒ external handling, so `human_explicit` rows are
refused with the existing
`external_import_human_explicit_trust_class` error and a pointer to the
team-aware path. (A bare store-UUID comparison was considered and rejected:
store identifiers plausibly leak via support bundles and status JSON, and a
leaked identifier would reopen the bypass verbatim.) `ee playbook import` caps
imported `trust_class` at `agent_validated` unless the artifact passes the
same store-local MAC, keeping its maturity attenuation. Per the
no-backwards-compat policy this is a direct behavior fix; CHANGELOG +
migration note required. Verify the bounded header before native-trust
admission, then recompute its root/count while applying the whole import in one
rollback-capable transaction and commit only on exact agreement. Truncation,
duplication, reordering, snapshot races, or a mismatch near EOF must leave no
partially privileged memory, audit, or index side effect; external fallback is
a separate explicit application path, never an in-flight downgrade. This
keeps the preamble constant-size instead of duplicating an unbounded list of
every ID/hash.

Authenticated native reimport is idempotent restore, never an implicit
rollback or merge API. A missing record may be restored and an existing
byte-identical ID/hash is a no-op. If the target already has that ID with a
different entity revision/hash, or a tombstone/withdrawal that dominates the
artifact row, the transaction reports a conflict and never overwrites or
resurrects local state. Newer artifacts must use the ordinary mutation/merge
path with explicit lineage checks.

Acceptance: a teammate's export can no longer inject
`human_explicit` rows; a copied artifact with a correct store UUID but no
valid MAC is refused; same-store missing/byte-identical reimport round-trips
at full trust, while cross-family/scope replay, divergent revisions, and
dominating tombstones/withdrawals fail without rollback or resurrection. The
current redacted `ee backup` format stays credential-free: restoring it on
another store follows external handling and requires explicit local
re-attestation. A separately restored whole user-data key directory from an
operator-managed protected system backup is recognized only after hardened
path/owner/type checks plus a known-answer MAC self-check; this is not
advertised as an `ee backup` capability. Key rotation and the bounded
same-store import-verification window are covered explicitly; approval tokens
accept only the current key, so rotation forces a fresh preview.

**P0.7 — Honesty-debt backfill.** Failure-mode fixtures + taxonomy entries for
the uncovered mesh degraded codes (52 in the catalog golden: 19
fixture-uncovered, 18 of those also taxonomy-uncovered — close both gaps);
fill the three newline-only audit test files
with the assertions their names promise; correct `src/core/effect.rs` mesh
declarations (real `.ee/*.toml` paths, real tables); fix README `[mesh.tailscale]`
and `.ee/mesh/` drift; update the stale "future `ee mesh preview-grant`" schema
description. Mechanical but contractually required by AGENTS.md ("new degraded
code ⇒ fixture in the same commit"; the drift radar gate).

### 7.2 P1 — Real transport

Design decision: **one listener, one port, one frame protocol.** The daemon
hosts a supervised TCP listener bound only to locally verified Tailscale
address(es), never a wildcard, on `EE_MESH_HELLO_PORT` (default 41888, already
registered). `src/mesh/tailscale_transport.rs` supplies reusable bounded
framing/enums, but its dead `ee.mesh.tailscale_transport_frame.v1` wire shape
uses rotating `sourceNodeKey`/`targetNodeKey` strings as identity and MACs
with the long-term pair key. T2.1 supersedes it before production with
`ee.mesh.tailscale_transport_frame.v2`: random ee source/target node IDs,
team and endpoint-workspace IDs, session ID, direction, monotonic u64
counter, request correlation, capability, bounded requested budget, and
payload hash under the directional session MAC. Stable Tailscale node IDs are
bound in the handshake; current public keys are observations only. V1 frames
are rejected by the production listener. Each receiver accepts exactly the
next per-direction counter; duplicate, skipped, or regressed counters close
the session. Ordered TCP needs no authenticated-frame replay window.
Application retries use a new frame/counter and retain their event ID or
operation idempotency key.

M1 wires v2's `hello`/`summary`/`event_fetch`/`body_fetch` capabilities. M2
adds a ≤4 KiB version-negotiated control-only `pair_rotate` capability; M6
adds `identity_attest`, whose ≤8 KiB messages contain ceremony IDs,
verification URL/user code, and status only, never bearer tokens. The 64 KiB
frame limit, 32 KiB payload limit, and constant-time MAC comparison remain
fixed. The v2 MAC preimage is versioned, type-tagged, and length-prefixed
rather than incidental JSON field order. T2.1 pins endpoint/session/counter
verification and canonical vectors before the first production caller.

Port discovery is explicit rather than assumed. `teamCreated` commits the
validated non-privileged `hello_port` (`1024..=65535`) into the root and every invite carries it; all
responder-capable members and all teams multiplexed by one broker must use the
same value. A local mismatch is honest client-only posture until reconfigured.
V1 has no in-band team-port migration (replacement team/root is the explicit
path); clients never scan or fall back to another port. Since one TCP
address/port is host-wide but ee stores/control channels are per user, v1
supports one responder-capable OS user per Tailscale node. Another local user
is client-only or uses another Tailscale node. Status/doctor expose this
constraint and port-owner conflicts.

**Why Asupersync TCP is compliant and sufficient.** The pinned Asupersync
0.3.9 release already exposes cancel-aware, readiness-driven
`asupersync::net::{TcpListener, TcpStream}` with timed connect and
interrupted-on-cancel accept/read/write. The transport uses those APIs under
`Cx` budgets instead of blocking `std::net` calls or a
thread-per-connection adapter—Rust cannot safely kill a thread stuck in
blocking I/O. The forbidden-deps rule bans HTTP *stacks*, not sockets; this
protocol needs only length-prefixed authenticated frames over TCP. Tailnet
TCP between two WireGuard peers is encrypted and mutually authenticated at
the network layer by Tailscale; the pairwise session MAC authenticates the
live *ee peer* and Ed25519 signs durable event authorship (§7.3.1),
preserving "Tailscale is not trust."

**P1.0 — Origin event stream substrate (the missing table).** Everything in
anti-entropy assumes each node maintains a durable, append-only, per-origin
sequence of **its own** events — that is what a tip advertises, what a
`RangeRequest` addresses, and what fork rejection hash-chains over. No such
table exists: V052–V057 are all inbound-side (`mesh_peers`, cursors, import
ledger, mappings, body-cache metadata). Deriving "events" on demand from
mutable tables cannot yield stable sequence numbers — any later edit would
look like a fork to peers. New migration: `mesh_origin_events`
(`origin_seq` contiguous per team/workspace/origin/signing-key generation,
event hash chained via `prev_event_hash`, authenticated outer operation,
typed payload schema/hash/ref, `produced_at`, key ID, signature), plus
same-transaction append rules and an immutability contract. Hashes and
signatures use a versioned schema-specific, type-tagged, length-prefixed byte
codec with schema-defined sorting for sets—not `serde_json` map order or
struct declaration order—and checked-in golden vectors pin null/optional and
generation-boundary cases.

Emission is origin-safe: only origin-owned local rows append. Importing,
materializing, indexing, or curating a peer row cannot echo it under the local
key; peer rows are immutable as peer evidence, and building on one creates a
new local memory with explicit provenance. A per-team/per-memory projection
marker also defines the first event: new local rows emit `create`, and the
first post-activation edit of an older unprojected row emits `create` for the
current state rather than an orphan `revise`.

The current `ee.mesh.event.v1` is memory-only (`logicalMemoryId: ^mem_` plus a
closed event-kind enum) and cannot represent manifest operations. T2.0
introduces `ee.mesh.origin_event.v1` (every instance requires
`mesh.origin_event.v1`) with typed
`ee.mesh.memory_event.v1` and `ee.team.manifest_event.v1` payloads. Memory v1
emits `create`/`revise`/`tombstone`/`shareWithdraw` and requires
`mesh.team.memory.v1`. Its metadata allowlist is closed: logical, revision,
and predecessor IDs; level; kind; validity; project binding; bounded origin
trust claim; provenance-safe opaque IDs; and, for content-bearing operations,
`bodyRepresentation`, bounded redaction profile/scanner ID,
redaction-evidence hash, and P4.6's salted `bodyCommitment`. It excludes body
text, first-line/title/preview text, tags, provenance URIs, raw paths,
evidence bodies, and the commitment nonce. Metadata-only consumers may filter
those fields and render an attributed missing-body placeholder, but cannot
claim body-text recall. Schema/golden allowlist tests fail if a model-field
addition silently widens disclosure. Manifest v1 has explicit
operation kinds from ADR 0086 TC-D3, including the dual-key
`signingKeyRotated` transition and the tier-2 `identityAttested` evidence
operation. The latter is schema-pinned from v1 and feature-dispositioned as
unsupported until M6 rather than added later to a closed enum. Every manifest
event requires `mesh.team.manifest.v1`; `identityAttested` additionally
requires `mesh.team.identity_attested.v1`. T2.0 registers these schema and
feature names but does not make either base team feature available in
production until T4.1 installs the active-member/node/key authorizer and
manifest materializer. T2.4 depends on T4.1, so pair/session/signature proof
alone never applies or relays team authority in an incrementally shipped
build. After that gate, an event whose cross-origin member predecessor is not
yet present is quarantined for deterministic re-evaluation; it never wins by
arrival. `teamCreated` is the sole pre-membership exception. A pre-M6 binary
that supports the base manifest cannot accidentally treat the generic bit as
sufficient for `identityAttested`. Dispatch derives mandatory features and
authorization from payload schema + operation, not from the origin's
`requiredFeatures[]` alone. That bounded, sorted, unique list may add
forward-compatibility requirements but cannot remove a check (maximum 32
entries and 64 UTF-8 bytes per entry). A missing
mandatory bit quarantines as `mesh_event_feature_contract_invalid` and is
never relayed/applied; an unknown additional bit dispositions the otherwise
valid event `unsupported` for replay after upgrade.
Manifest payloads are inline metadata. The old event schema is superseded
only for live team origin streams, not abused with synthetic memory IDs or
silently removed from its existing file-replay surface.

The outer `eventId` remains mechanically bound to the signed digest:
`mesh_evt_<64-lowercase-hex-event-digest>`. It is excluded from its own hash
preimage only to avoid self-reference; receivers recompute and reject any
mismatch before idempotence/storage so a relay cannot rename one signed event
into many IDs. Existing `ee.mesh.event.v1` is already a registered
transport-independent file replay/import-ledger contract used by `ee mesh
export|import`, not an unpublished schema. It remains on that separate
non-origin-authoritative M0 surface. The typed envelope supersedes it only for
live team origin streams; legacy file rows are never re-signed, relayed as
team authority, or silently reinterpreted. A future file-artifact conversion
would require an explicit versioned migration.

The origin table stores the immutable signed header and locally authored
payload. Inbound state has two contiguous frontiers plus sparse dispositions:
every admitted peer receives safe signed headers through a verified receipt
frontier; a disposition-scan frontier advances once every event through N has
an explicit durable disposition (`applied`, `withheld`, `quarantined`, or
`unsupported`) with policy generation/reason. Materialized state reads only
explicit `applied` rows, never a scalar "application cursor." A withheld
payload can be fetched by event hash after policy change and transitions its
own disposition under audit without rewinding either frontier. This is what
lets N+1 apply while N remains withheld.

The append-only inbound ledger gets cumulative storage admission, not just
the existing per-batch limits. Transactional charged size is attributed to
the signed origin lineage across relays and signing-key rotations: each
durable row rounds encoded bytes to a 4 KiB page and adds a 4 KiB
row/index-overhead unit with checked arithmetic. Local defaults are 64 MiB per
origin lineage and 256 MiB normal inbound total per team workspace. Safe
headers and mandatory manifest/`tombstone`/`shareWithdraw` payloads use a
separately checked reserve of at most 1 MiB per authenticated random ee node
lineage and 80 MiB per team workspace. Control charging is never keyed by an
origin workspace/stream, signing generation, current member binding, or
relayer; workspace churn, rotation, rebinding, removal, and reconnect do not
reset it, and node IDs are never recycled. `create`/`revise` payloads cannot
use either control reserve. A 1 GiB filesystem free-space floor is a hard
pre-commit stop. `[mesh.admission]` may change these local bounds explicitly;
no peer/manifest can.

The whole batch, its dispositions, audit, index jobs, and frontier movement
commit or roll back together. A denied attempt updates only one coalesced
bounded posture/counter per origin, never one durable audit row per retry.
Near the normal ceiling, range planning continues header/control-only through
the bounded reserve. When the authenticated node's reserve, the 80 MiB team
control cap, or the free-space floor is exhausted, it stops with
`mesh_inbound_storage_budget_exhausted`; unrelated
origins continue while their budgets permit. Accounting never charges local
origin events or local source truth, so remote exhaustion cannot make
`ee remember` fail. Body-cache bytes stay under P4.6's separate evictable
quotas; the immutable event ledger is never auto-deleted.

Team activation is future-only by default. Existing origin-owned memory
metadata remains local until the operator runs the revision-pinned,
preview/confirm `ee team share history` flow (§7.4 P3.4). Its bounded
resumable projection emits missing creates in stable order, revalidates each
entity revision immediately before emission, reports changed items for a new
preview, and races idempotently with live mutation through the unique
projection marker. Imported peer rows are never eligible.

**P1.1 — Hello responder actually binds.** The shared responder core is
normally hosted by a new supervised daemon job, `mesh-hello-responder`; the
foreground invite waiter may host it only under the single-owner lease from
§6/P3.2. Accept loop → resolve the accepted source IP through
Tailscale LocalAPI WhoIs (fresh status IP→stable-node fallback only) → require
a non-empty Tailscale stable node ID and record the current rotating node key
as an observation → read one bounded frame → if hello:
`decide_hello_response` under the existing policy; respond or
privacy-preserving decline. Caller-supplied tailnet/stable-ID/node-key/owner
headers are assertions only and never identity. Source-IP/stable-node mismatch
or WhoIs failure declines before mutation. `ee mesh hello-responder status`
switches from hardcoded `running:false` to querying the daemon over the
existing daemon protocol. The broker binds only while at least one validated
workspace route is mesh-enabled; pausing/disabling one route never tears down
other active routes, and zero enabled routes means zero TCP listener.
`EE_MESH_HELLO_RESPONDER_DISABLED=1` is the user-wide kill switch.
At startup and on LocalAPI network-map/interface changes, the owner
revalidates the complete local tailnet-address set. Losing every verified
address closes the listener and reports coalesced
`mesh_transport_unreachable` posture; a changed set drops stale sockets
before binding only the new verified addresses. Starting before `tailscaled`
is ready retries under supervision with the same posture and never falls
back to a wildcard or stale address.
`mesh_off_no_network.rs` proves an isolated mesh-off home has no registered
route and the daemon binds nothing.
Feasibility note: `src/daemon/server.rs` already supplies job lifecycle,
disable/status, and scheduler integration seams. Reuse those control-plane
patterns, but implement mesh socket I/O with the cancel-aware Asupersync TCP
path above rather than copying the daemon's blocking accept-thread pattern.
The responder owner also hosts a bounded user-scoped routing broker. Extend
the existing hardened workspace registry with exact team/genesis/database
routes, and reuse the daemon UDS parent, no-symlink publication, same-EUID
peer-credential check, workspace authorization, bounded framing, and
request/response correlation. A registration is idempotent, carries no
invite secret/body, and is accepted only after the referenced local database
revalidates its workspace ID and team genesis. Invite IDs and authenticated
team/target-workspace fields route requests; no network field is ever
interpreted as a filesystem path. A post-enrollment connection binds the
initiator's locally selected workspace and one registered responder target
workspace, both random ee node IDs, and both pinned Tailscale stable node IDs
during its authenticated handshake; every frame MAC binds both endpoints and
direction. The current Tailscale public key is a WhoIs/status-verified rotating
observation, not the permanent ee identity. Event `origin_workspace_id`
remains independent producer provenance and cannot redirect either local
database.

T2.2 also makes the stable binding real in storage. `mesh_peers.peer_id`
remains an opaque local handle and is never recomputed on current-key change;
new handles contain at least 128 OS-CSPRNG bits;
new security decisions use the random ee node ID, pinned tailnet/stable node
ID, signing/pair continuity, and explicit generation. New enrollments stop
using the legacy `build_peer_id(workspace, node_key)` derivation as identity.
An existing row without a ceremony-proven non-empty stable ID is surfaced as
`mesh_peer_identity_upgrade_required` and cannot participate in sync until
re-enrolled; hostname, login, IP, or the old key cannot auto-upgrade it.
Grant lookup by peer ID resolves to and persists the exact ee-node/generation
principal once that model lands, so routine transport-key rotation neither
retargets nor drops a grant.

**Bootstrap envelope (pre-key traffic).** At hello/join time no pairwise key
exists. Pre-enrollment traffic therefore uses a distinct **unsigned but
strictly bounded** envelope: ≤4096 bytes, no capability beyond `hello`/`join`,
and no durable mutation until invite proof. For a real join, the joiner first
sends only invite ID + nonce; the inviter returns a bounded Ed25519-signed
challenge over protocol/invite/team/root, both nonces, inviter ee/stable-node
identity, and committed port. The invite carries the expected public signing
identity. Only after exact signature/root/identity/port verification does the
joiner send the raw secret over the WireGuard-protected socket; the server
hashes/constant-time compares and zeroizes it. A wrong process on the right
host or port never receives the secret merely by answering first. Before authentication, rate limits
key on source IP plus a listener-global bucket; after WhoIs/pairing they key on
the verified stable node ID. A claimed node key or stable ID cannot rotate
around the bucket. All
post-enrollment traffic requires a fresh authenticated session and frames from
unkeyed peers are rejected. Minimal caps land here, not in M5: connection
semaphore, source-IP/global bootstrap budgets, and per-session frame budget.

**P1.2 — Client-side hello probe over TCP.** Replace
`TailscaleStatusCapabilityHelloProbe` with a real probe: for each
policy-admitted candidate peer (same `decide_discovery` filter as today),
connect to the exact configured local mesh port for generic discovery, or the
root-committed `hello_port` for an enrolled team peer, within the existing
750 ms per-peer / 5 s total budgets and exchange hello frames. The default is
41888; the probe never hard-codes it, scans alternatives, or silently falls
back. A team/local mismatch yields the same explicit client-only posture as
TC-D1. The ACL-capability read stays only as a cheap *pre-filter hint* when
present (it can mark peers `ee-capable` without a connection, e.g. via the
`tag:ee-mesh` service tag), never as the authority.
Unreachable/timeout/declined map to the existing skip-reason vocabulary. This
makes `ee mesh auto-enroll` and `ee mesh status --json` discovery real on a
stock tailnet.

**P1.3 — Anti-entropy over the wire.** Implement
`TcpMeshForegroundSyncTransport: MeshForegroundSyncTransport` — the production
implementation the seam was built for. One `contact_peer` call runs one
bounded, data-bidirectional round: TipAdvertise ⇄, then each endpoint runs
`MeshRangePlanner::plan` against its own receipt state and may issue at most
one RangeRequest per origin per round. Each direction replays EventBatch
responses through that receiver's `decide_mesh_import` + ledger insert +
cursor advance
(contiguous-replay-only and fork-rejecting — the v1-applicable ADR 0041
scenarios become integration-tested behavior; validity/trust payloads remain
deferred), RevisionNotice emission. Metadata lane only in this milestone.
At M1 the transport verifies the enrolled ee origin node/current signing-key
generation, team/workspace binding, payload hash, previous hash, and
contiguous sequence. T4.1 is an explicit T2.4 dependency and enables
`mesh.team.memory.v1`/`mesh.team.manifest.v1` only with the
active-member/node/key authorizer installed. Before that gate, a build may
parse/verify/store safe headers for protocol tests but cannot advertise,
apply, or relay team authority. After it, an authenticated peer may relay an
intact signed event only when the origin is authorized at that event position
(or the event is the unique valid `teamCreated` genesis); missing cross-origin
membership predecessors quarantine for later deterministic re-evaluation.
Duplicates are idempotent, while altered/forged origin events fail regardless
of relayer.
A same-sequence/different-hash or incompatible signed-tip proof is
valid-origin equivocation, not a forgery: retain both proofs, mark the origin
forked at the earliest proven sequence, deterministically de-materialize that
origin at/after the fork, suspend it, and relay the evidence. Neither
first-arrival branch wins; unrelated origins continue. Generic mesh recovery
revokes/re-enrolls the origin; once team manifests land, another active member
must revoke/rebind it. There is no operator branch selection.
Pairwise session MACs authenticate the connection but never confer
event authorship. `ee mesh sync --once` stops emitting
`mesh_sync_once_network_deferred` when a round actually ran; the code remains
for daemonless/unreachable cases. `probe_mesh_capability` graduates from
`Unimplemented`. Hardened pair/signing-key storage lands in M1; pair-key
ceremony derivation lands in M2. That sequencing is explicit: M1's real-binary
wire harness pre-provisions matching long-term pair keys through the hardened
key-store API as fixture setup, then exercises the production fresh-session
handshake and frames. There is no public raw-key import flag or test backdoor.
`ee mesh sync --once` runs a real round for an already keyed peer and returns
structured pairing-required guidance otherwise. M2 tests the production
pair-key KDF/key-confirmation primitive, and M3's invite/join E2E is the first
public pairing ceremony. M1 therefore proves transport, not a user-facing
pairing workflow it has not built.

**P1.3b — Per-session anti-entropy serving (both endpoint roles).**
The round (P1.3) is useless unless both the responder and the initiating CLI
can serve `summary`/`event_fetch` over the established session. One shared
serving core answers TipAdvertise with the bound local workspace frontier,
serves bounded `EventBatch` ranges from `mesh_origin_events` (P1.0), and — the
non-negotiable part — always serves the minimal immutable signed header while
applying `decide_mesh_outbound_policy` and the pre-export secret scan to
`create`/`revise` typed payloads. Denied content payloads produce an audited
`withheld` receipt; budget-clipped batches end at a complete header and
continue next round. Minimal `tombstone`/`shareWithdraw` controls (opaque
logical-memory ID plus predecessor/content references needed to close
retrieval and purge prior material) and manifest payloads are mandatory for
active team members, never body-bearing, and never policy-withheld.
Contiguous receipt/disposition-scan frontiers plus the per-event disposition
ledger let sequence N remain withheld while N+1 applies. Acceptance: a
planted secret never crosses, a denied content payload never applies, later
safe events do apply, mandatory controls purge old material even after policy
denial, and a policy change can hydrate the withheld payload by event hash
without replay ambiguity.

**P1.3c — Deterministic rematerialization.** Disposition changes caused by
fork blocking, removal cutoffs, policy re-evaluation, or feature support do
not mutate derived state ad hoc. A pure versioned reducer consumes immutable
signed events plus current durable dispositions. Each applied disposition
pins its local policy generation and admission result; replay never reruns a
rolling trust-velocity window.

The exact traversal key is raw UTF-8
`(origin_workspace_id, origin_node_id)`, then numeric signing-key generation,
then numeric `origin_seq`. Same-stream/same-sequence conflicting hashes are
fork-blocked before reduction and never hash-tie-broken. Cross-origin
predecessor/removal/capacity rules evaluate the complete accepted set or
fixed point, so traversal order cannot become authority.

The reducer returns a canonical projection plus idempotent action plan. A
bounded executor transactionally hides newly invalid material first, then
uses resumable generation-fenced batches to update derived rows, append
audits, coalesce index jobs, and enqueue cache eviction. Cache metadata closes
the retrieval path by entering `invalidated_pending_purge` before post-commit
physical eviction; only a successful removal plus directory fsync advances it
to `purged`/`evicted`. This invalidation ordering is deliberately the inverse
of new-body publication, where a `staging` row remains non-addressable until
verified atomic object publication and fsync succeed and a final transaction
marks it `available`. Deterministic idempotency keys, crash checkpoints, and
the 16-index-jobs-per-round budget make large rollbacks restart-safe without
publishing a partial generation. The canonical projection hash excludes
local row IDs, audit timestamps, filesystem paths, velocity counters, and
intentionally local policy differences.

Affected reads surface `mesh_rematerialization_pending` while the fail-closed
rebuild is incomplete. Executor/invariant failure becomes high-severity
`mesh_rematerialization_failed` with structured status/doctor recovery;
neither posture re-enables the invalid generation.

T2.8 owns the reducer/executor and current memory/index integration. T3.4
adds the `peer_human_attested` admission/reversal arm; the original durable
admission result is replayed and its historical velocity slot is neither
refunded nor consumed twice. T5.9 adds body-cache publication/eviction
integration. No layer touches origin-owned source truth or deletes ledger/
audit evidence.

**P1.4 — Sync engine placement.** The round executor lives in core
(`src/mesh/` + `src/core/`), callable from both the foreground CLI (one-shot,
no daemon needed — CLI-first invariant) and the daemon steward job (P5.1).
Budgets: reuse the pinned verification-matrix numbers (sync batch 512 events,
retry 1s→60s max 5, fanout 32) — changing them requires the ADR amendment path
ADR 0041 prescribes. The 512-event value is a logical per-round ceiling, not
one wire payload: range responses split only at complete-event boundaries
into as many frames as fit the unchanged 32 KiB payload / 64 KiB frame caps.

**P1.5 — Two-node truth harness.** The load-bearing new test asset: a
two-process E2E that starts two real `ee` instances with isolated homes and
distinct 127/8 addresses (fake-tailscale LocalAPI maps each accepted source
address to exactly one node; clients use `asupersync::net::TcpSocket::bind`
so identity never falls back to source ports), provisions matching fixture
pair keys through the production hardened key-store API, establishes a valid
T4.1-signed genesis/member/node authorization state (never a test-only
authorizer bypass), writes memories on A, syncs, and asserts B's
search shows them with correct provenance/trust — and the reverse. Extends the
existing fake-tailscale harness; replaces the jq-only
`mesh_local_two_node_demo.sh` fixture with a real one. The opt-in
real-tailnet smoke (`mesh_sync_once_real_tailscale.sh`) flips from "record
whether deferred" to asserting a real round when `EE_E2E_REAL_TAILSCALE=1`.
Partition/rejoin, fork-rejection, and hole-blocking scenarios from ADR 0041
run against the real wire path, plus a three-node equivocation case where two
peers first accept different validly signed branches and later converge on the
same rolled-back, fork-blocked prefix with both proofs retained.

### 7.3 P2 — Identity

#### 7.3.1 Members and pairwise keys

New DB table `team_members` (workspace-scoped like `mesh_peers`):
`member_id` (`mbr_` + at least 128 OS-CSPRNG bits), `display_name`, `state`
(active/removed), `added_by_member_id`, `joined_at`, `removed_at`,
`contact_hint` (optional, e.g. email — display only, never authorization), and
`is_self`. New table `team_member_nodes` binds members to a random ee node ID,
the tailnet + non-empty Tailscale stable node ID, the currently observed
rotating Tailscale node key, and the ee origin signing public-key lineage
(`member_id`, `ee_node_id`, `tailscale_stable_node_id`,
`observed_tailscale_node_key`, signing public key + generation, `peer_id`
FK-ish to `mesh_peers`, `bound_at`, `bound_via` ∈
{team_genesis, invite_ceremony, member_added_node}). Every post-genesis
binding records the existing-node/new-node direct ceremony; there is no
operator flag that silently bypasses proof of the fresh node.

Tailscale node public keys are intentionally not durable identifiers:
reauthentication rotates them, while status/WhoIs exposes a stable node ID.
Pair/session plus ee
signing continuity may update the observed key under audit for the same
pinned stable ID; a missing/changed stable ID requires a new binding
ceremony. Hostnames, IPs, display logins, and a rotating key alone never
substitute. Older Tailscale builds that do not expose the stable ID fail with
an upgrade prerequisite. A lost last node cannot sign its own replacement:
another active member must drive fresh consent, and recovery mints a new
member ID when the old lineage cannot be proved. See Tailscale's
[node-key documentation](https://tailscale.com/docs/concepts/node-keys) and
[`PeerStatus`](https://pkg.go.dev/tailscale.com/ipn/ipnstate#PeerStatus).

Lane grants remain node-scoped. A member-shaped product command expands to
the member's active nodes at preview time and pins that exact node set in the
consent hash. A node bound later starts with metadata-only defaults; it does
not inherit an earlier body grant. `ee team status` shows members whose active
nodes have different grant posture.

**Authentication model (v1): origin signatures plus pairwise sessions.**
Every node generates an Ed25519 signing key; origin events are signed and may
be safely relayed. The pinned dependency is
`ed25519-dalek = "=3.0.0"` with `default-features = false` and exactly
`features = ["fast", "zeroize"]`. Generation fills a
`zeroize::Zeroizing<[u8; 32]>` with the existing fallible
`getrandom::fill`, then uses `SigningKey::from_bytes`; entropy failure is
returned, not converted into a panic. `zeroize = "=1.8.2"` becomes a direct
`default-features = false` dependency (already in the transitive tree).
Verification uses `verify_strict`; `rand_core`, `hazmat`, and
`legacy_compatibility` stay forbidden. Dependency-contract entries land with
T2.0 before the origin-event implementation closes; T3.6 hardens the
resulting lifecycle. The contract also records the already-present transitive
`ed25519-dalek` 2.2 from Asupersync/`nkeys`: team code imports only the direct
3.0 API, and `cargo tree -d` keeps the temporary duplicate major explicit
until upstream converges.

Invite and introduction codes contain a 32-byte OS-CSPRNG secret. Canonical
initiator/responder roles contribute fresh 32-byte nonces and derive the
long-term pair key from a length-prefixed transcript binding protocol/KDF
version, team and invite/introduction IDs, both random ee node IDs, both
Tailscale stable node IDs and currently observed node keys, both signing-key
fingerprints, and both nonces. Hash the exact pre-KDF transcript as
`blake3::derive_key("ee.team.pair.transcript.v1", bytes)` and derive
`k_pair = blake3::derive_key("ee.team.pair.v1",
lp(secret) || lp(transcript_hash))`, where every `lp` is u32-LE. The
transcript hash excludes the key and confirmation messages; confirmations
bind hash, generation, and roles. Both sides confirm key possession before
membership commits. A copied invite **by itself**, without
observing or participating in the fresh ceremony, therefore cannot determine
the pair key; a bearer who also participates in or observes the protected
transcript can, which is why invite interception remains a named residual.

Each TCP connection performs an authenticated fresh-nonce handshake and
derives directional session keys. The authenticated session transcript binds
the pinned Tailscale stable IDs; each frame's canonical MAC preimage binds
team/session, random source and target ee nodes, the initiator's locally
selected workspace, exactly one registered responder `target_workspace_id`,
direction, capability, request ID, monotonic counter, budget, and payload
hash. Payload endpoint fields must agree. WhoIs supplies the stable ID and
current key observation. Either endpoint may request missing ranges, but
neither can change this workspace pair. Event `origin_workspace_id` remains
independent relay provenance. Duplicate/skipped/regressed counters,
wrong-target/route forwarding, origin-as-target confusion, direction
confusion, and unmatched responses fail closed. Application idempotence
belongs to signed event/operation IDs, never replayed frame bytes. Long-term
pair keys never MAC application frames directly.

Pair and signing keys live under a 0700 user-data key directory in 0600 files
opened without following symlinks, with owner/type checks, atomic
write+rename, and file/directory fsync. Windows client-only operation still
holds credentials and therefore requires equivalent hardening: a reviewed
safe adapter rejects reparse-point components, verifies a non-inherited DACL
limited to the current user SID and SYSTEM, pins opened-file identity, and
provides write-through atomic replacement. It may neither shell out to
`icacls` nor introduce project-owned unsafe code. When that parity is
unavailable, credential-bearing team commands fail closed with
`mesh_key_store_unavailable`; "client-only" never means plaintext or
best-effort keys. This is a narrow reusable secure-local-file primitive, not
a key-store-only exception; T5.9 consumes it for sensitive body-cache
publication.

Pair-key rotation uses a
generation-bound two-phase state machine. Both endpoints durably stage the
same rotation ID, expected/next generations, and transcript hash. Each
contributes a fresh 32-byte nonce over the current authenticated session;
`k_next = blake3::derive_key("ee.team.pair.rotate.v1",
lp(k_current) || lp(rotation_transcript_hash))`, whose canonical transcript
binds team, nodes, roles, generations, nonces, and prior pair transcript.
Its hash is
`blake3::derive_key("ee.team.pair.rotate.transcript.v1", transcript_bytes)`.
This is routine hygiene, not compromise recovery; suspected compromise
requires a fresh ceremony/secret. Both prove the next key in both directions
and persist `accepting_next` before commit
acknowledgements. Promotion is atomic only **locally**; a crash may leave one
side promoted and the other staged. For exactly
`PAIR_KEY_ROTATION_GRACE_SECONDS = 86400`, the old key can authenticate only
rotation-resume handshake messages bound to that record—never a new ordinary
session or application frame. Promotion closes old sessions; automatic
downgrade and concurrent/generation-mismatched rotations fail closed. An
incomplete transition after the grace emits
`mesh_pair_rotation_repair_required` and requires fresh pairing. The source of
truth is an atomically replaced/fsynced 0600 rotation manifest inside the
hardened key directory; DB/status state is a rebuildable non-secret
projection, and disagreement blocks sessions until reconciled from the
manifest. The staged record persists its deadline and nondecreasing local
wall-time high-water; resume checks also use same-process monotonic elapsed,
and observed wall-clock rollback blocks old-key resume with the repair code
instead of extending the 86400-second window.

The explicit v1 trigger is `ee mesh rotate-pair <peer-id> --json`, resolved to
the exact enrolled ee-node pair and expected generation. It uses the
version-negotiated ≤4 KiB `pair_rotate` control capability and emits
`ee.mesh.rotate_pair.v1`. V1 claims no automatic rotation cadence.
`ee team members rotate-key` is explicitly the local Ed25519 signing-lineage
operation and never implies that pair keys rotated. Signing keys use a
distinct public lineage transition:
`signingKeyRotated` is signed
by the current key, carries the next generation/public key and old terminal
event hash, plus a next-key proof-of-possession signature; the first event of
the new generation references the transition hash. Peers reject gaps,
regressions, generation reuse, or one-key-only transitions. A stolen current
signing key cannot attest its own recovery; another active member must revoke
the compromised node/member lineage.
Introduction exchanges use the same construction, so an inviter cannot derive
the pair keys it introduced.

`EE_AGENT_NAME` remains what it is — an unauthenticated *agent* label for
attribution within one member's swarm. Member identity is machine-anchored
(random ee node ID + pinned tailnet/stable-node binding + ee signing lineage
+ pairwise keys), not env-var- or rotating-node-key-anchored. The producer
does not author an authoritative `memberId`. On admission, the receiver
derives `producerMemberId` from the verified origin node/signing generation
and manifest authorization position, and derives project attribution from
the authorized origin workspace/project registry. Missing, ambiguous, or
payload-mismatched attribution quarantines. Explicit source provenance—not a
null member field—proves that a row is locally owned. Body-request member
identity is likewise derived from the authenticated session node rather than
trusted from request data (§7.5.2).

#### 7.3.2 Trust class `peer_human_attested`

New sixth trust class between `agent_validated` and `human_explicit`:

| Class | Initial confidence | Retrieval weight (ask) | In `verified` scope |
|---|---|---|---|
| `human_explicit` | 0.85 | 1.00 | yes |
| **`peer_human_attested`** | **0.75** | **0.92** | **yes** |
| `agent_validated` | 0.65 | 0.85 | yes |
| `agent_assertion` | 0.50 | 0.70 | no |
| `cass_evidence` | 0.45 | 0.55 | no |
| `legacy_import` | 0.30 | 0.40 | no |

Semantics, stated as what the system can actually attest: "this row arrived
in a valid signed origin event from a node bound to an active member, and that
member's store classed it `human_explicit` at origin." *Attested* does not
prove a human typed it — `human_explicit` is locally CLI-assignable, so a
misbehaving agent on a member's machine could mint it. Controls for that
amplification risk (also a §8 threat row): `ee why` always shows the
elevation basis; `ee team status` shows per-member elevated-row counts; an
elevation velocity cap defaults to 100 unique content-bearing events per
member per rolling 24 hours
(`peer_human_attested_max_per_member_24h`). Every eligible `create` and
`revise` consumes one slot only after event-ID idempotence; replay consumes
none, and a revision cannot inherit a prior elevation for free. An over-cap
event's resulting current local revision is `agent_validated` (including
demotion of a previously elevated revision) and surfaces
`team_member_elevation_burst`; rows are never silently dropped or elevated.
The cap is an atomic persistent local-admission rolling window keyed by the
receiver-derived producer member, not a payload claim or the origin's
untrusted `producedAt`.
A persisted nondecreasing accounting high-water mark prevents local clock
rollback from reopening capacity, and each incoming batch is evaluated in
canonical origin/key-generation/sequence order. Elevation is local policy, so
different members may make different elevation decisions while retaining the
same signed source provenance.
Elevation happens at import time iff **all** of: (a) event's trust lane is
`peerHumanViaPeer` (an existing `MeshTrustLane` variant,
`src/core/memory_scope.rs:62–86`) with source class `human_explicit`; (b) the
origin signature is bound to an active member; (c) the
local team policy `elevate_member_human_explicit` is true (set during the
join ceremony's consent summary, default **on** for invite-ceremony members —
the ceremony is the explicit consent; changeable anytime via `ee team members
trust`). Otherwise the existing `agent_validated` cap applies unchanged. The
three existing rejection points (§3.3 item 5) stay intact for
`human_explicit` itself — it still never crosses.

Touch set — the three parallel trust-class-shaped enums with overlapping
names and different semantics (`src/models/trust.rs:59`,
`src/mesh/lane_grant_preview.rs:170`, `src/mesh/sync.rs:69`) must move in
one slice:
`src/models/trust.rs` enum + DB CHECK migrations + `ask.rs` weights +
`is_verified_memory` + the mesh-side `lane_grant_preview.rs` and `sync.rs`
trust enums + schemas + golden refresh. **Schedule-risk flag:** `trust_class`
CHECK constraints exist at multiple shipped, checksummed migration sites
(`src/db/mod.rs:2923, 2991, 3939, 6972` — separate tables/baselines), and
SQLite cannot ALTER a CHECK constraint. Admitting the new class means
recreate-style 12-step table rebuilds for **each** affected table — on the
largest tables in the store — via new migration IDs (never editing shipped
SQL), with a migration test asserting row counts and content hashes survive.
This is a rebuild, not a one-line constraint tweak; budget it accordingly.

#### 7.3.3 Project identity

New identity `project_key` decoupling "the same body of work" from local paths:

- **Git workspaces:** `prj_git_` + blake3(root-commit-set)[..24], derived from
  **all** non-empty output lines of `git rev-list --max-parents=0 HEAD`, after
  first requiring `git rev-parse --is-shallow-repository = false`, reading
  `git rev-parse --show-object-format`, validating each full object ID for
  that format, and sorting the complete set. Every probe invokes canonical
  Git directly without a shell through the bounded/reaped runner, starts from
  a minimal cleared environment, rejects ambient `GIT_*`
  repository/object/config/prompt/trace controls, and uses
  `--no-replace-objects --no-lazy-fetch --no-optional-locks`. A nonempty
  resolved common-dir `info/grafts` makes root derivation unusable; a local
  replace ref is ignored and recorded in evidence, never allowed to rewrite
  the root set. Required-option support is capability-probed: if the installed
  Git rejects any safety option (notably pre-2.45 Git without
  `--no-lazy-fetch`), ee never retries a weaker root command. It emits the
  existing `git_unavailable` warning with upgrade guidance and proceeds only
  through the safe remote fallback or minted/adopted key path. The canonical preimage
  length-prefixes the object-format tag and every ID. A shallow boundary can
  therefore never masquerade as a root, and SHA-1/SHA-256 or multi-root
  concatenations cannot alias. Stable across clones and intentionally shared
  by forks; an
  explicit override separates a fork or mirror that should be distinct.
  Fallback when history is shallow (`--depth` clones lack the root): a
  credential-stripped, canonically normalized URL from the explicitly named
  `origin` remote
  (`prj_rem_` + hash), with a degraded note. It reads only raw local
  `remote.origin.url` with includes/system/global config disabled and requires
  exactly one distinct usable canonical value; multiple URLs, rewrite rules,
  or remote iteration never pick a winner. Secret userinfo/query/fragment/
  control bytes are rejected and redacted before diagnostics. The persisted fallback never
  silently changes after unshallowing. More generally, no persisted key
  silently changes after a root-set addition, history rewrite, object-format
  conversion, or origin rename: `ee team projects reconcile` shows the old
  and new derivation evidence and previews/confirms an alias, upgrade, or
  separation. Remote iteration order is never a selector.
- **Git without a usable `origin`, and non-git workspaces:** no safe
  derivation is possible; a project key is minted
  (`prj_tm_` + random) the first time the workspace is shared to a team
  (US-9), unless the operator explicitly adopts an existing project, and
  distributed via the manifest.

Storage: new nullable columns on `workspaces` (`project_key`,
`project_key_source`, alias/override metadata), backfilled lazily on
`ee init`/resolution. Wire: a versioned hello carries structured
`workspaceBindings:[{workspaceId,projectKey,source}]`, never parallel arrays.
Peer-group bindings and `origin_workspace_ids` policy checks accept
project-key matches, which kills the manual n×n workspace-ID mapping for the
common case. Privacy: project keys are hashes (or random); raw paths and
credential-bearing remotes never cross, and the decline path leaks nothing.

#### 7.3.4 SSO member identity (Microsoft Entra / Okta / Google), two tiers

The invite ceremony (§7.4 P3.2) proves possession of a code; it does not prove
*who* a member is in the organization's terms. For teams inside companies, the
authoritative human identity already lives in an SSO IdP — and offboarding is
done there, not in ee. Two tiers, both opt-in per team, both recorded in the
manifest so every member enforces the same policy:

**Tier 1 — tailnet-attested identity (v1, zero new dependencies).**
Tailscale tailnets themselves authenticate through exactly these IdPs
(Google / Microsoft / Okta / OIDC): every node in `tailscale status --json`
carries a `UserProfile` (the SSO login that owns the node). The local probe
(`src/core/tailscale_probe.rs`) already parses Self/Peer records; extend it to
parse per-node user profiles. Then:

- `ee team idp require --tailnet-attested [--domain acme.com]` records in the
  manifest that every member's node bindings must be owned by a tailnet
  account (optionally restricted to a domain).
- At join and at every revalidation, each side checks that the counterparty's
  node is owned by the SSO login recorded on the member record
  (`identity_attestation: {kind: tailnet, login, checked_at}`). Mismatch or
  disappearance ⇒ `team_member_identity_mismatch` /
  `identity_revalidation_failed`, grants suspended (not deleted), audit row.
- Trust chain and its honest limits: this trusts the local `tailscaled` and
  Tailscale's control plane (which performed the actual OIDC). It is exactly
  as strong as the tailnet's own membership — which the team already relies on
  for transport. It requires no TLS stack, no JWT verification, no new crates,
  and it works offline against the daemon's cached state (staleness is
  surfaced, not hidden).

**Tier 2 — direct OIDC device-code flow (crates land at start per §13 item 1;
sequenced after core team UX).** For teams that want ee-level proof independent of the tailnet
IdP binding (or group-based authorization, or a different IdP than the
tailnet's):

- `ee team idp set --issuer <url> --client-id <id> [--allowed-group <g>]…`
  pins the issuer + client in the manifest. Discovery uses the issuer's
  `/.well-known/openid-configuration`, followed by a provider-capability
  preflight: a device endpoint, an ID token usable for the configured client
  and scopes, required claims, an allowed signing algorithm, and a
  public-client token endpoint authentication method (`none`) must all be
  present. A provider requiring `client_secret_basic` or
  `client_secret_post` is unsupported in v1; ee never distributes an OAuth
  client secret through the manifest or join flow. This means tier 1 can
  represent Google Workspace identity through Tailscale while a Google
  device-client registration that requires its documented client secret does
  not qualify for tier 2. Unsupported providers return a structured
  `team_idp_provider_unsupported` recovery action. Device authorization
  ([RFC 8628](https://www.rfc-editor.org/rfc/rfc8628)) is the only flow:
  print `verification_uri_complete` + user code and poll the token endpoint.
  The verifier parses required positive `expires_in` and optional positive
  `interval` (default 5 seconds) with checked arithmetic. Its same-process
  monotonic deadline is `min(provider expiry, start + 1800 seconds)` and it
  stops after at most 300 token requests. It never polls before the current
  provider minimum; each `slow_down` adds 5 seconds for all later requests
  and connection timeouts use checked exponential backoff, both limited by
  the remaining deadline. An interval beyond the remaining lifetime expires
  rather than being shortened. Only `authorization_pending` and `slow_down`
  continue; denial, provider expiry, malformed/overflowing values,
  cancellation, and any other error stop without silently starting a new
  ceremony. Provider/local/poll-budget expiry emits structured
  `team_idp_device_flow_expired` with a reason and explicit restart action.
  A process crash resumes the outer join/renewal only: its non-secret
  checkpoint remains `identity_pending`, while device code and poll state are
  gone and the user must explicitly start a fresh provider ceremony. No
  device-flow secret is persisted or silently reused to make OAuth polling
  itself crash-resumable.
  The plan does **not** claim that Entra, Okta, Google, and every conformant
  OAuth provider expose identical OIDC token behavior; each provider is
  capability-tested.
- HTTPS egress uses an allowlisted canonical system `curl` executable,
  invoked directly without a shell through an extension of the hardened
  bounded subprocess runner. The default must resolve to an absolute regular
  file under non-symlinked, owner/mode-safe root-owned ancestors; a non-system
  override requires explicit local approval of its canonical path and digest
  and is never selected from ambient `PATH`. That runner supplies bounded
  stdin, a hard total
  stdout/stderr cap, timeout/cancellation, process-group termination/reap, and
  inherited-pipe escape handling. Secrets/device codes are sent in request
  bodies or stdin, never argv or logs; token-bearing responses cross the
  redaction boundary before diagnostics.
- Curl starts from `env_clear()` plus an explicit minimal platform-runtime
  allowlist. Ambient configuration, proxy, netrc, CA-bundle, TLS-backend, and
  TLS-keylog environment is absent; no-proxy-for-all is explicit. System
  trust is the default, while a private/test issuer requires an explicitly
  approved and validated local regular-file CA bundle passed directly by the
  identity command. Strict URL parsing accepts HTTPS only and rejects
  userinfo/fragments. TLS verification cannot be disabled. Discovery requires
  exact issuer matching and bounded response bytes/time. ee never delegates
  redirects to `curl --location`: it manually validates a small number of GET
  hops, while credential-bearing device/token POSTs reject redirects instead
  of replaying a secret body. Before every connection it validates all
  A/AAAA answers against loopback/link-local/private/reserved ranges (unless
  that private issuer was explicitly approved), pins an approved address for
  the connection while TLS verifies the original hostname, constrains the
  protocol to HTTPS, and repeats the check for every approved GET hop. This
  closes DNS-rebinding, ambient-proxy, and redirect body-leak gaps. Calls
  happen only inside explicit identity commands.
- Each explicit presentation refreshes discovery and JWKS (an HTTP cache
  validator/304 is acceptable); a stale offline JWKS cache cannot prove that
  a key remains published and never verifies a new token. ID-token
  verification is non-negotiable: algorithm allowlist (RS256/ES256
  initially), reject `none`, enforce `kid`/`use`/`key_ops`/`alg`, exact issuer,
  audience plus `azp` when applicable, expiry/not-before with bounded skew,
  `iat`/`auth_time` freshness, `email_verified` whenever email participates
  in identity/policy, the configured group-claim type, and single-use
  `jti`-or-token-hash in an atomic issuer/client-scoped replay ledger retained
  through token expiry. RSA keys must meet the pinned modulus/exponent
  minimum; EC keys must be P-256 signing keys matching the declared
  algorithm. Discovery/JWKS/JOSE-header/claims parsing is byte-bounded and
  rejects duplicate member names instead of accepting last-key-wins. JWT
  segments require canonical unpadded base64url; unknown/unsupported `crit`
  and token-controlled `jku`/`x5u`/`jwk`/`x5c` headers are rejected. Keys
  come only from the exact freshly discovered issuer `jwks_uri`; v1 never
  follows a JWK certificate URL/chain or trusts embedded key material. `kid`
  is mandatory and must select exactly one eligible raw-parameter key after
  `kty`/`use`/`key_ops`/`alg` filtering—zero or multiple matches fail, and ee
  never tries candidates until a signature happens to verify. The signature
  covers the exact original compact-JWT header/payload segments, never
  reserialized JSON, and no claim/replay entry is trusted or committed before
  signature success.
  RFC 8628 itself defines no nonce
  parameter; if a provider advertises and echoes a nonce extension, ee uses
  it, otherwise the response explicitly records the weaker
  freshness+single-use assurance. ee never requests `offline_access`; any
  refresh token returned anyway, including a provider-mandated device-flow
  response, is zeroized and discarded. Raw ID/access/refresh
  tokens are held only in zeroizing buffers and never persisted. The durable record contains the
  token hash/replay claim, a minimal canonical approved-claim subset,
  issuer/client/subject,
  key ID/JWK thumbprint/algorithm, verification/expiry times, and verifier-
  signed attestation evidence. Email is retained/replicated only if policy or
  an explicitly previewed display needs it; the full provider group list and
  unrelated claims never persist or enter the manifest. Evidence contains
  only bounded matches against configured allowed-group identifiers plus the
  authorization decision, and `idp set`/join preview names every claim field
  that will become team-visible. A retired JWKS key never verifies a newly
  presented token; an existing attestation remains explainable from the
  stored evidence until revalidation, with no cached raw token to re-read.
  RS256/ES256 verification requires pure-Rust RustCrypto crates (`rsa`,
  `p256`; `sha2` is already in-tree) — **approved (§13 item 1)**; the crates
  land in the tree when tier-2 work starts, with dependency-contract-matrix
  entries in the same commit.
- The member projection gains an ordered `identity_attestations[]` lease set
  whose entries contain `{kind: oidc, issuer, subject, email?,
  matchedAllowedGroups, authorizationDecision, verified_at, valid_until,
  verified_evidence_expires_at,
  verifier_member_id, verifier_node_id, policy_generation, evidence_hash,
  assurance}`—never an unfiltered claim or full group list. The effective deadline is the maximum eligible
  policy-capped lease deadline; no arrival winner overwrites another
  verifier. At first admission the receiver requires `valid_until >
  verified_at`, rejects `verified_at` more than 600 seconds ahead of its
  effective local authorization time, and caps `valid_until` by both
  `verified_at + policy_cadence` and verified token/evidence expiry. A
  late-delivered already-expired lease stays ineligible; receipt time never
  revives it. Lease eligibility uses a local persisted nondecreasing
  identity-authorization time floor. Token verification, grant/serve,
  sync-import, steward suspension/renewal, and explicit revalidation
  transactionally advance it to at least wall time before deciding. Read-only
  status, doctor, activity, and audit compute
  `max(persisted_floor, current_wall_clock)` without persisting it. Remote
  origin/token/attestation/receipt timestamps never advance it. Clock rollback
  on an identity-dependent authorizing or mutating path cannot
  extend or revive a lease and emits `team_identity_clock_rollback`. A
  forward jump advances the floor and may expire leases early; if the system
  clock is corrected backward, that rollback posture appears. The explicitly
  confirmed local repair suppresses every currently eligible tier-2 lease
  before lowering the floor and requires fresh interactive attestations.
  **Verifier model:** over the post-key, ≤8 KiB `identity_attest` capability,
  the **distinct active verifier member/node** starts and hosts the device
  flow for the subject. It returns only the verification URL/user code and
  bounded status, polls and receives tokens locally, refreshes
  discovery/JWKS, reduces the token immediately to canonical evidence, and
  authors the typed `identityAttested` manifest operation. Raw tokens never
  cross the ee mesh or enter the manifest. Verification URLs, user/device
  codes, and polling state are TTL-bound ceremony ephemera omitted from audit,
  support bundles, and the manifest; only redacted terminal status may
  persist. Other members trust that attributed
  verifier assertion (the trust link is named in §8);
  every-member independent verification is rejected as UX-hostile, but
  self-attestation/self-renewal is also rejected. A compromised verifier can
  lie within this named trust boundary. The materializer rejects a verifier
  whose current IdP subject equals the target subject. If two active member
  IDs claim the same exact `(issuer, subject)` under one policy generation,
  the complete set is a manifest conflict and sharing for those members
  pauses; neither arrival nor event hash picks a human-identity winner.
- Revalidation cadence: manifest-configured (default 30 days, plus on-demand
  `ee team members revalidate`). What the human experiences: a printed
  "sign in again to keep sharing with <team>" prompt with the device code —
  interactive by design, so the cadence is a team policy choice, not a hidden
  background login. Ordinary renewal needs a reachable distinct active
  verifier that has its own current exact-policy-generation lease; if none is
  available, it stays pending rather than accepting self-proof.
  Tier-1 checks (node ownership) are non-interactive and can run in the
  steward. For tier 2 the steward performs no IdP HTTP: it marks finite
  attestation leases due/overdue and suspends after grace when no fresh
  `identityAttested` event arrives. Offboarding therefore prevents the next
  renewal and is enforced within cadence + grace, not by background directory
  polling. Grace posture is explicit (`identity_revalidation_overdue`,
  warning) before suspension.

  Tier 2 is disabled in `teamCreated`. Enabling or tightening it puts current
  members without an exact-generation lease into pending/due posture and
  starts the configured local grace window rather than suspending them
  synchronously. An in-grace active member can verify a joiner, who can then
  verify the creator. `ee team idp set` previews this bootstrap and rejects a
  zero-grace activation that would suspend every member before any distinct
  verifier can exist.

Both tiers change **authorization posture** only — they never touch memory
content, never run inside retrieval commands, and degrade loudly, not
silently. Teams without any IdP keep the plain invite ceremony; nothing about
tiers 0→2 changes the wire protocol or trust-class semantics (a
`peer_human_attested` elevation simply gains a stronger identity basis, and
`ee why` says which basis applied).

### 7.4 P3 — `ee team` UX

A new `src/cli/team.rs` command group. Every subcommand: stable JSON schema
(`ee.team.*.v1`), plain-language human output ending with "what this did"
(named mesh primitives) and "next commands". Mutating or
network-state-changing leaves append bounded audit rows in the same
transaction as their durable state; status/doctor-diagnose/list/activity/audit
and preview leaves remain side-effect-free and append no audit or time-floor
row (explicit doctor *repair* actions are separate mutating leaves under the
doctor-runtime mutation rules and audit their mutations like any other
mutating leaf).
Failed commands have no durable effect except a specifically contracted
coalesced security posture counter. No TUI, no
wizard (ADR 0038 D1 stands); interactivity is limited to y/N confirmation
prompts that `--yes` bypasses for agents.

**P3.1 — Manifest.** The team manifest is a replicated document:
`team_id`, name, created_by, immutable v1 `hello_port`, per-member records (member ID, display name, node
and signing-key generations, state, added_by, identity-attestation evidence,
timestamps), shared project registry (§7.3.3), and team lane/IdP policy. It
replicates as typed, signed
`ee.team.manifest_event.v1` payloads inside the generic origin envelope
(§7.2 P1.0). Events are attributed and fork-rejecting and are ordered **only
inside their author's origin stream**; there is no cross-stream order hidden
behind arrival time. The materialized `team_manifest` cache is always
rebuildable from admitted events.

**Manifest authorization rules** (enforced at event application, pinned in
ADR 0086):

| Operation | Authorized author | Effect on application |
|---|---|---|
| `teamCreated` | unique pre-membership genesis: generation 0 / sequence 1 / no predecessor, self-signed by its embedded initial ee signing public key | Canonically commits random team/member/ee-node IDs, protocol version, immutable v1 `hello_port`, initial tailnet/stable-node binding + current transport observation + signing key, display metadata, creation-time claim, and default policies; its event hash + root fingerprint become the permanent team-root reference |
| `memberAdded` | any *active* member | New random member ID + initial node/signing-key binding, subject to the complete-set 20-active-member protocol bound below |
| `memberRemoved` | any active member (self-removal = leave) | Monotonic removal with accepted per-origin cutoffs; the append transaction advances durable session/grant authorization generations, every frame rechecks them, and socket cancellation is idempotent after commit |
| `nodeBound` | the member's existing active node plus the fresh new node in a direct ceremony | Binds a random ee node ID to the tailnet + non-empty Tailscale stable node ID, current rotating node-key observation, ee signing identity, and exact predecessor node-set root; starts at metadata-only grants and is subject to the four-active-node cap |
| `nodeRevoked` | the member themself or any active member | Monotonic exact-ee-node revocation with a per-origin cutoff; advances the durable authorization generation before post-commit session cancellation, without treating ordinary current-key rotation for the same stable node ID as a new node |
| `signingKeyRotated` | the bound member + current-key signature + next-key proof | Hash-linked signing-key generation transition; no implicit file-only replacement |
| `laneProfileSet` / `idpPolicySet` | any active member (roles deferred) | Expected-predecessor coordination update; conflict blocks the field. Lane widening never mints a local grant; IdP relaxation never lowers another node's locally accepted floor without explicit acceptance |
| `projectRegistered` | any active member | Registry update under expected predecessor |
| `identityAttested` | a distinct active verifier member/node whose current IdP subject, when known, differs from the target | Adds a commuting lease bound to subject, verifier, exact IdP-policy generation, canonical evidence hash/assurance, signed verification time, verified evidence expiry, and a finite deadline independently bounded on admission by 600-second future skew, policy cadence, and evidence expiry; raw tokens are forbidden and pre-M6 peers disposition the feature as unsupported |
| `manifestConflictResolved` | any active member | References the complete known conflict set and is itself predecessor-checked |

A `memberRemoved` event includes the remover's last accepted
`(key_generation, seq, event_hash)` for each known origin of the target.
Effects above those cutoffs are quarantined and deterministically
de-materialized even if they arrived first; effects through the cutoffs stay
valid. Any target-attributable origin missing from the signed cutoff map has
an implicit cutoff before its first event, so a hidden or later-revealed
node/stream cannot escape removal. Active members added by the removed member
through an accepted cutoff remain active but carry `addedByRemovedMember`
until each local operator acknowledges them. That flag is detection, not
revocation: the member retains ordinary authority until separately removed.
Removal preview, status, and doctor enumerate the affected members, emit
`team_delegated_member_review_required`, and recommend pausing the team until
each is acknowledged as legitimate or removed. Rejoin always mints a new
member ID/key lineage.

Active-author validity is computed from the complete admitted event set, not
from the order rows happened to arrive. If removal cutoff claims form a cycle
that mutually invalidates the removal events (for example, two members remove
each other concurrently), the whole strongly connected component enters
`manifest_conflict`: no removal in that component applies, pre-conflict
membership remains effective, and sharing for the affected members pauses
until reconciliation names the complete conflict set. Competing
reconciliations re-block instead of electing an arrival/hash winner.

Competing predecessor-based updates put the affected manifest field in a
blocked conflict posture; no rank or arrival time selects a winner.
Identity leases are not a predecessor-based singleton: all authorized,
unexpired, exact-policy-generation leases are retained in stable event-hash
order, and the effective deadline is their maximum. Concurrent renewals
therefore commute. Two active member IDs claiming the same exact
`(issuer, subject)` form a complete-set manifest conflict; neither becomes
the first-arrival winner, and sharing for the affected members pauses until
reconciliation/removal.
V1 hard-caps the complete active member set at
`MAX_ACTIVE_TEAM_MEMBERS = 20`. If concurrently admitted additions would
exceed that set, no timestamp, arrival order, or attacker-grindable event hash
chooses who fits: membership-dependent sharing enters
`team_member_capacity_conflict` until explicit reconciliation/removal brings
the complete set within the bound.
Each member is independently capped at
`MAX_ACTIVE_NODES_PER_TEAM_MEMBER = 4`, bounding the team to 80 active nodes.
Concurrent `nodeBound` successors from the same exact predecessor node-set
root commute only if their union fits; otherwise every contested successor
enters `team_node_capacity_conflict` and the predecessor set remains
effective. No winner gets a session or grant. A monotonic node revocation plus
explicit reconciliation is the replacement path.
Replicated policy never substitutes for local consent. Effective outbound
lane access intersects the manifest ceiling, local policy, exact
requester-node grant generation, and current redaction/secret-scan verdict;
lane widening alone grants nothing, while narrowing applies immediately.
Each node persists its explicitly accepted IdP floor. A stricter comparable
manifest policy applies immediately; a relaxation or incomparable
issuer/group change stays `pending_local_policy_acceptance` until the local
operator accepts that exact generation via `ee team idp set`, with
`team_policy_relaxation_pending` in status/reconcile meanwhile.
Conversely, a malicious member can narrow a lane or tighten a comparable IdP
policy into an availability failure in v1. That action cannot exfiltrate data,
but it is deliberately visible by author/generation and recoverable through
revocation plus reconciliation; roles/quorum remain deferred.
Unauthorized/removed-window/unsupported events are durably quarantined while
the receipt and disposition-scan frontiers advance; materialization still
reads only explicit applied dispositions.
`ee team status` and `reconcile` expose every conflict and local confirmation.
A missing, second, or conflicting `teamCreated`, or any manifest event not
rooted in its exact hash, is a root fork that blocks the team; it is never
accepted as an ordinary scalar conflict or resolved by arrival order.

Removal appends a signed event, revokes locally, and attempts bounded
foreground fanout to all reachable members. Any acknowledging member can
relay the event. A durable acknowledgement matrix lists members that have not
applied it; propagation is not falsely described as bounded when nobody
received the removal. Removal never emits `shareWithdraw`, which is
origin-memory-wide rather than recipient-specific.

`nodeRevoked` applies the same arrival-independent cutoff discipline to one
exact ee node and closes that node's sessions and future serving immediately.
An ordinary current Tailscale key change for the same pinned stable node ID is
an audited transport-observation update, not a new ee node, a revocation, or a
way to inherit grants. Binding a replacement/additional node requires an
existing active node plus a fresh direct ceremony; if the last node is lost
and continuity cannot be proved, recovery is fresh consent and a new member
identity rather than self-authorized replacement.

**P3.2 — Invite/join ceremony.**
`ee team invite [--ttl 72h] [--for "Priya"] [--wait|--no-wait]
[--resume <invite-id>]` mints
`{version, team_id, inviter_stable_node_id, inviter_ee_node_id,
inviter_signing_identity, last-observed rotating node-key/MagicDNS/IP hints,
hello_port, invite_id, genesis event hash, root/signing-key fingerprint,
256-bit one-time secret, expiry}`, encodes as
`eeteam1-<base32>`, stores only the hashed secret locally, audits. `--wait`
runs a foreground accept path for the ceremony's duration, so joining works
before anyone has installed the daemon (M3 must not depend on M5). If the
daemon already owns the one responder listener, the CLI registers the invite
over its same-EUID, workspace-bound control channel using invite ID/hash
metadata and a request nonce—never the clear secret—and waits for the
correlated result; otherwise it acquires the exclusive responder lease and
binds the same user-scoped broker in-process. Daemon/foreground startup races
have one winner, and an unrelated port occupant fails diagnostically rather
than causing a fallback bind.

`invite_id` and the durable ceremony ID each contain at least 128 independent
OS-CSPRNG bits; neither is sequential, derived from names/public identifiers,
or a truncation of the bearer secret. Unknown IDs receive only the same
bounded privacy-preserving decline used by other invalid bootstrap attempts.
The inviter maintains a persisted nondecreasing invite-authorization
wall-time floor, advanced transactionally on mint, lease, redeem/resume,
revoke, expiry, and per-pair introduction-secret authorization. A live
process also enforces monotonic elapsed deadlines. Observing wall time below
the floor after restart/correction emits high
`team_invite_clock_rollback` and blocks mint/redemption/resume instead of
extending a bearer credential; a forward jump may expire one early. Doctor
can lower the floor only while atomically revoking every pending
invite/lease/introduction, and repair never reactivates one.

The simple command is useful by default: when a live daemon broker confirms
the exact route, plain `ee team invite` registers the invite, prints the code,
and may return; with no daemon it prints the code and waits in the foreground
until redemption, expiry, or interruption. Explicit `--wait` waits for the
nonce-correlated result even when the daemon owns the listener. Explicit
`--no-wait` is accepted only after a live broker confirms the route, so it
cannot mint an unreachable invite. An interrupted waiter leaves only the
hashed pending invite and prints
`ee team invite --wait --resume <invite-id>`; resumption never needs or
reveals the clear secret.

`ee team join` reads the secret from a no-echo TTY prompt; noninteractive
callers must use
`--invite-stdin`. Positional and environment-secret forms are rejected so the
invite cannot enter process listings, shell history, or environment captures;
logs/audit/errors retain only invite ID/fingerprint/hash. Then:
parse → probe tailnet → resolve the invite's exact tailnet + non-empty
Tailscale stable node ID from fresh local status and connect only to a current
IP/current rotating key bound to that stable ID (embedded key/MagicDNS/IP
values are non-authoritative observations; ordinary key rotation for the same
stable ID is accepted and surfaced, while a missing/changed stable ID requires
invite reissue—never fallback to another identity or a public endpoint) →
connect only to the root-committed `hello_port` → send invite ID + nonce only
→ verify the inviter's signed challenge binds the expected protocol,
invite/team/root, nonces, ee/stable-node identity, port, and any required
bounded signing-transition chain → only then send
the secret in the bounded bootstrap flow (§7.2 P1.1) → inviter hashes,
constant-time compares, zeroizes, and atomically leases single redemption to a stable
ceremony ID → both sides confirm pair and signing keys → optional IdP check →
membership commit → signed manifest sync → joiner pairs with each *reachable*
member (inviter relays only *introductions*: ee node IDs, stable-node
bindings, signing identities, and current transport observations from the
manifest; each pairwise key still requires a direct transcript-bound exchange
with that member's node, protected by a TTL-bound per-pair introduction
secret) →
**unreachable members' pairings are deferred: owned by the steward job and
retried by every `ee team sync` run, surfaced as `unpaired` in `ee team
status`, re-issuable via re-introduction when the introduction secret
expires** → both sides enroll mesh peers with `trust_established_by =
"explicit_human_consent"` (the humans typed/sent the code — this resolves the
auto-enroll dead-end **without** laundering: `tailscale_auto_enrollment`
remains sync-ineligible) → default lane profile applied (metadata,
revisionNotice, curationSignal allow; body, embedding, graphLink deny) →
that join consent records the local default grants, while later manifest
widening alone cannot create them → consent summary printed & audited on both
sides → first sync round runs.
`--dry-run` is deliberately local/secret-free: it parses and checksums the
code, checks its embedded expiry and local prerequisites, and may issue only
the ordinary unauthenticated hello needed to report protocol reachability. It
does **not** transmit the invite secret, validate revocation/prior use,
lease/consume the invite, or reveal manifest/member/IdP-policy metadata; output
labels those server-side checks as unvalidated. Durable join phases are
`pending_redemption → key_confirmed → member_committed →
first_sync_complete`; crashes resume the same ceremony, never consume an
invite into an orphan or admit two joiners. Secrets live only in zeroizing
buffers. A pre-`key_confirmed` resume asks for the same invite again through
the prompt/stdin path and matches its hash plus ceremony/node/signing-key
binding; the printed resume command never contains it. Later phases resume
from confirmed keys and non-secret state. Exit 0 requires the last phase.
`--for` is display-only: an unbound code is still a bearer credential for any
policy-admitted tailnet node, so an interceptor can win its one redemption.
The consent text names that residual and recommends a short TTL, `--wait`,
revocation, or an enabled identity policy where that risk matters.

The invite pins the inviter signing generation/fingerprint. A routine
signing-key rotation before redemption is accepted only when the challenge
carries a contiguous TC-D5 dual-signed/hash-linked public transition chain
from that pinned generation to the current signer within the 4096-byte
bootstrap budget. A missing/gapped/oversized chain requires invite reissue.
Node/member revocation, fork-block, or compromise recovery invalidates all
pending invites from that lineage; a self-authored rotation chain cannot
revive them.

**P3.3 — Membership ops.** `ee team members
[list|show|trust|rotate-key|reconcile]` (`rotate-key` means the local
Ed25519 signing lineage; pair-key hygiene is the separate
`ee mesh rotate-pair` surface), `ee team member add-node` (bind an
additional machine for yourself, via a self-invite variant), `ee team member
remove <member>` (atomically append the signed manifest removal and advance
all target nodes' durable session/grant authorization generations, then
idempotently cancel open connections and attempt bounded foreground fanout,
with honest acknowledgement/cached-copy caveats), `ee team leave`. Frame
handlers recheck the durable authorization generation before every import or
serve, so SQLite is never claimed to close an in-memory socket and an old
session cannot race post-commit serving. Every other member enforces the
removal when it applies or relays the event. No automatic `shareWithdraw` is
emitted: removal revokes future serving/grants, while already-synced copies on
the removed machine cannot be remotely erased.

Member removal is preview-hash pinned. Human mode previews and confirms; robot
mode first calls
`ee team member remove <member> --preview --json`, then passes
`--preview-hash`. The canonical hash binds team root plus current
manifest/materializer generation, target, exact nodes/signing generations,
per-origin cutoffs, accepted-prefix members that remain active,
acknowledgement audience, and the cached-copy/no-`shareWithdraw` residual.
The mutating transaction recomputes all inputs before append; drift emits
`team_member_removal_preview_stale`, returns a new structured preview action,
and commits zero removal, authorization-generation advance, invite
invalidation, audit/outbox, connection-cancel, or fanout effects.

**P3.4 — History-sharing op.** Team activation shares future eligible metadata, not
pre-team history. `ee team share history [--project <project>]` drives a
revision-pinned preview of origin-owned, never-projected memories → explicit
confirm → bounded resumable projection. The consent hash pins IDs and entity
revisions; the preview names current members but explicitly authorizes durable
team metadata history, including future active members until an origin-wide
`shareWithdraw`—it never misrepresents this as a current-recipient-only grant.
Each row is revalidated before emission, changed rows require a new preview,
imported peer rows are ineligible, and a unique projection marker makes
history/live-mutation races idempotent.

M3 intentionally ships no body-sharing verb. P4.6/T5.9 owns
`ee team share bodies` and `ee team unshare bodies`, their schemas,
preview/consent/grant/revoke behavior, and the actual transport as one M4
slice. This prevents an unavailable sentinel or persisted no-op grant from
being mistaken for a shipped feature. Before that slice, low-level
`ee mesh grant` validates lane capability and returns a structured
`ee.error.v2` unsupported-capability error without persisting anything.
Embedding and graphLink deliberately receive no team-UX verbs in v1.

**P3.5 — Posture ops.** `ee team status` (members × reachability × last-sync ×
staleness × pending invites × lane matrix summary × responder owner/route
validation), `ee team sync [--now]`, `ee team pause` /
`ee team resume --confirm`, `ee team audit [--json]` (filtered view over the
existing audit ledger: consent, grants, membership, sync summaries).
Pause transactionally commits a new durable workspace/team pause generation
and audit row before route unregistration, session cancellation, and steward
shutdown. Every round start, frame handler, import, and serve boundary
rechecks the generation. Resume explicitly revalidates the team root, key
store, identity, and policy, advances the generation, and never reuses a
stale session. Pause prevents future network exchange but neither deletes
local cached bodies nor claims to erase peer copies. Status and audit remain
read-only and usable while paused.

### 7.5 P4 — Unified retrieval

**P4.1 — Scope plumbing.** Add `--memory-scope` / `--strict-scope` to
`PackArgs` (and `pack build`), closing the README/graph-flags drift; task-lens
overlay keeps working, explicit flag wins. The `team` scope predicate is:
**an explicitly origin-owned local row eligible for this team's projection,
or an inbound projection whose receiver-derived producer member is active at
the event's authorization position.** Null or payload-authored member fields
never prove local ownership or attribution. The undocumented legacy
`trust.team_members` agent-nickname list is removed rather than retained as
an unauthenticated human-team compatibility shim; existing `self` and `swarm`
scopes continue to cover current/local agents. This intentional early-product
configuration break is migrated and documented directly — and
**operator-confirmed 2026-07-30** ("we have no users so don't care about
backwards compatibility"), consistent with the repo-wide
no-backwards-compat policy in AGENTS.md.
`scope_agent_unavailable` behavior for agent-shaped scopes is unchanged.

**P4.2 — Attribution rendering.** Search/pack/ask/why surfaces render, for
team-synced items: member display name, project name, origin trust class,
immutable origin `producedAt`, and the local trust class after elevation — in markdown packs as a
compact suffix (`· from Priya / acme-analysis · 2026-07-30T14:02Z`), in JSON
as a `teamProvenance` block with
`originTimeAssurance: "member_attested"`. The signature authenticates who
asserted the timestamp, not clock correctness; authorization and elevation
caps never use it. It is also excluded from retention/decay, lifecycle
mutation, and search/pack relevance ranking; peer material uses a separate
local first-receipt/lifecycle clock for local-only operations instead of
copying the origin claim into an authoritative local `created_at`.
Under `--memory-scope team`, default search and pack give every candidate the
same neutral temporal multiplier, including the producer's explicitly shared
local row and another node's projection of the same event: neither local
created time, origin time, nor receipt/sync time affects relevance, tie-break,
or selection. Given the same admitted event/body corpus and maintenance
state, producer and receiver select the same event IDs/order. Additional
local-private rows make the corpus different and are labeled; workspace-scope
temporal behavior is unchanged. An explicit user time-window filter
may compare member-attested `producedAt` only when the response returns its
resolved cutoff/as-of and assurance label; this is attributed filtering, not
freshness authority. **Determinism rule:** given equal signed origin events,
canonical materialized corpus, materializer version, local
disposition/admission decisions, maintenance state, config, and indexes,
local `receivedAt`/`syncedAt` never enters pack/search bytes or hashes.
Changing signed `producedAt` changes its rendered provenance bytes and any
hash over them, but not selected IDs, ordering, or relevance scores. Local
first receipt may affect later local decay/expiry and thereby produce a
different corpus; that explicit maintenance-state difference is outside a
fixed-corpus equality claim. Receipt/sync time remains diagnostic in `why`,
status, and audit. Relative phrasing is allowed solely in non-deterministic
human surfaces. `ee why` explains elevation decisions
("valid signed origin event from member mbr_…, elevated to
peer_human_attested because…").

**P4.3 — Team activity.** `ee team activity [--member X] [--project Y]
[--since <absolute-rfc3339>] --as-of <absolute-rfc3339> [--limit N]
[--cursor TOKEN] --json` — a bounded, deterministic listing over synced
metadata (counts + projects/kinds/levels + members + origin times + body
availability), answering US-4's
"what did a teammate capture and share" question without full-text search.
It is not a command-execution or complete CASS-session feed. `--limit`
defaults to 100 and has a hard maximum of 1000; continuation uses the shared
generation- and normalized-parameter-bound `ee.cursor.v1` codec, including
the existing empty-page `cursor_invalid`/`cursor_stale` behavior. Human mode
may accept `--since 2h`, but prints the resolved absolute cutoff and as-of;
JSON rejects unresolved relative time and always requires explicit as-of.
Ordinary rows order by `(producedAt DESC, eventId ASC)`. A member claim later
than `as_of + MAX_ORIGIN_CLOCK_SKEW_SECONDS` (600 in v1) is excluded from the
recent-time bucket and reported in a deterministic `clockAnomalies[]`
collection until the explicit as-of window reaches it. A member can backdate
an event out of an explicit time window, so JSON labels
`timeFilterBasis: "member_attested"` and
`sequenceComplete: false` whenever `--since` is used; this view is a
convenience chronology, never an authorization or audit-completeness
boundary. The surface never derives a title/preview from body text. Draining generation-stable pages without `--since`, or using the
origin-sequence team audit surface, remains complete for admitted events.
Metadata-lane data only.
This is memory-event activity, not a command-execution or complete CASS
session log; the schema and human copy say so explicitly.

**P4.4 — Precedence and conflicts.** Pinned chain (ADR 0086 TC-D16): **local
workspace beats team beats global on *overlap*** (same/near-duplicate
content — more-specific context wins, decision recorded); **on
contradiction, neither silently wins** — the pair routes to the conflict
surface labeled by lane, and pack assembly never resolves cross-lane
contradictions by rank. This mirrors bd-1bfwa's rule exactly, so the three
lanes compose associatively. The
SRR6.37 peer duplicate/near-duplicate/contradiction detector (wire shape
pinned in `ee.peer_conflict.v1`, detector never implemented) is implemented
here for team-synced rows; conflicts appear in `ee insights` and pack DNA-style
explanations rather than being silently ranked away. Coordinate the
`memory_in_scope_with_tags` chokepoint edits with bd-1bfwa.3 (same file) —
the later implementer works from the then-current `main` and preserves the
earlier change; the precedence constant lives in one place both cite.
Detector claims stay evidence-bounded: exact duplicate
and near-duplicate use the existing deterministic search/similarity surfaces;
`contradiction` is emitted only for a canonical subject/predicate or typed
field with demonstrably incompatible values. Free-text-only suspicion is a
lower-confidence review candidate, never an authoritative contradiction, and
missing bodies produce `unassessed_missing_body` posture rather than a false
"no conflict." Every finding records detector version, evidence fields, and
confidence; no paid/hidden LLM call is introduced.

**P4.5 — Index integration.** Team-synced metadata/bodies (once admitted by
policy) flow into the existing derived-index jobs that `ee mesh import`
already enqueues; verify incremental-intake behavior at team scale (500-row
sync bursts) and cap per-round index amplification at the existing 16-job
budget. The current one-job-per-event file-import shape cannot meet that
budget unchanged: a wire round transactionally coalesces affected documents
into jobs keyed by workspace/source/round generation range (reusing the
source-snapshot publication fence), and the worker resolves IDs from the
durable import/disposition ledger. Retries reuse the idempotency key; a
partially processed round cannot stamp an uncovered source generation current.

**P4.6 — Body-lane transport.** The lane that makes US-6 real: `body_fetch`
frames over an authenticated session, keyed by the exact signed
`create`/`revise` event and salted `bodyCommitment` (there is no body event
kind). For each content-bearing revision, the origin generates a fresh
32-byte CSPRNG nonce and signs
`blake3("ee.team.body.commitment.v1" || lp(nonce) || lp(exact_body_bytes))`.
The nonce is stored atomically with the local origin event and omitted from
safe headers, metadata, audit, status, diagnostics, and support bundles. It is
released only inside an authenticated, authorized body response. Replays of
one event reuse its nonce; byte-identical distinct revisions use fresh nonces
and are unlinkable to metadata-only peers.
The event signs `bodyRepresentation = exact | already_redacted`; the latter
also signs a bounded redaction-profile/scanner-version ID and an evidence
hash that contains no removed text. V1 performs no in-flight body
transformation. An outbound `redact` posture serves only a body already signed
as `already_redacted`; an `exact` body under that posture stays metadata-only.
The current secret scan may newly deny bytes immediately before serving but
never mutates them under the signed commitment. Any future transformed derivative
needs a new origin-authenticated versioned descriptor.
Each request binds requester node, project/workspace, event ID, and
grant/policy generation. The authenticated session node resolves the
requester member; a request field cannot impersonate one. The server
re-authorizes the tuple before releasing nonce or bytes, so the public
commitment cannot test a body guess. Only the event's owning origin workspace/node may serve
from local source truth; a relayer never promotes its cache into serving
authority. A tombstoned/withdrawn event or unavailable old source revision
returns unavailable rather than substituting current/cached bytes. The
authorized response carries the nonce; chunks carry transfer ID, sequence,
final length, and a transfer hash. The receiver checks transfer integrity and
recomputes the signed `bodyCommitment` from nonce plus exact bytes before
publication. An ordinary content hash may then be derived for the receiver's
private local index but never enters team metadata. Aggregate bytes obey T1.1's
streaming `max_bytes+1` cap and the 32 KiB per-frame payload limit.

Fetches execute only in foreground sync, explicit prefetch, or steward rounds.
Retrieval consumes cache or returns metadata-only + missing-body posture; it
never fetches or waits. A policy denial is terminal for that policy generation
and retries only after policy/grant change or operator refresh. Transient
unavailability uses 1 s → 60 s capped retry, max 5 attempts and at most one
attempt per event per round. Outcomes live in
`mesh_body_cache_metadata`, extended to bind source event ID, origin
workspace/node, signed source commitment, representation, redaction profile/scanner
ID, redaction-evidence hash, and the grant/policy generations used for
admission. Removed text is never stored in provenance. Bytes stream into a
private link-safe temporary
object under the secure user-data boundary and become an opaque-keyed,
atomically published cache object only after final length/hash/event
verification and import-policy admission. Unix requires owner/type/
no-symlink 0700-directory and 0600-file semantics plus file/directory fsync.
Windows reuses T2.1's reviewed safe adapter for reparse-point rejection,
opened-file identity, a non-inherited current-user-plus-SYSTEM DACL, and
write-through atomic replacement. Quota/retention govern every published
object; support bundles include neither body bytes nor raw cache paths.
Failed/quarantined/evicted/expired/withdrawn objects must not remain
retrieval-addressable. The durable cache lifecycle is explicit and
asymmetric: publication records `staging`, completes verified atomic
publication plus file/directory fsync, then marks `available`; invalidation
first marks `invalidated_pending_purge` and removes retrieval/index
eligibility, then removes/fsyncs and marks `purged` or `evicted`. Retrieval
uses only `available`; filesystem presence never implies availability.
Startup/steward/doctor reconciliation resumes staging and purge intents,
handles inaccessible staged orphans, and is idempotent across every crash
boundary. If lifecycle or platform-security proof fails, body hydration stays
metadata-only and metadata records high, repairable
`mesh_body_cache_lifecycle_failed` with paths redacted; no platform may
publish under weaker permissions. `shareWithdraw` purges
derived peer material/cache objects without ever deleting the origin's local
source-of-truth memory. Per-recipient grant revocation merely prevents future
fetches; a previously published cache object on that recipient is not claimed
remotely erased. This finally gives `remote_evidence.rs` (fetch
planning), `cache.rs` (retention/quota/eviction), and the
`mesh_body_cache_metadata` table their production callers — the
eager-metadata / policy-gated-lazy-body architecture SRR6.11 specified.

T5.9 lands the product commands atomically with that transport.
`ee team share bodies --with <member>|--all-members` performs a real
preview/confirm and grants only the member's currently active, receiver-
resolved node bindings. The preview enumerates the closed metadata disclosure
fields, representation/redaction posture, recipient nodes, non-inheritance
for later nodes, and the non-erasure residual. It never exposes commitment
nonces. The local operator preview may show bounded, locally redacted samples
through the existing redaction/secret-scan path; sample text never enters the
manifest, wire exchange, durable audit, or recipient-visible metadata, and
the versioned authenticated preview token uses T1.6's body-share-specific
subkeys and TC-D6's two-layer envelope around one canonical approval
snapshot. It binds team root/materializer generation, serving workspace/node,
body lane and future-serving semantics, exact recipient nodes plus grant
generations, outbound policy/scanner generation, the complete candidate
ID/revision/representation/commitment set, sample strategy/limit, exact
ordered redacted samples, caution codes, and schema/copy version. Human and
JSON render from that same snapshot. Human confirmation keeps the token
in-process. Default JSON is deterministic and token-free; robot issuance
requires `--issue-approval-token`, marks the no-stable-ID `eeap1_` bearer
sensitive, and apply accepts it only from bounded `--preview-token-stdin`.
Envelope verification distinguishes invalid context, key, or MAC from an
authentic stale/expired snapshot; the snapshot check, generation
compare-and-swap, grant, and audit share one write transaction.
Failure returns only a structured re-preview action, never a replacement
bearer, and has zero grant/audit/outbox/fetch/cache effects. The token is
opaque and nonce-unlinkable; neither it nor ee-controlled
trace/audit/support or CASS-import materialization reveals a body/sample hash,
and durable audit stores only a domain-keyed nonce identifier. Operator copy
names the residual that an external recorder may retain an explicitly issued
token until expiry.
`ee team unshare bodies` advances the same
exact-node grant generations and stops future serving from the named current
local source. `--all-members` means recipients of that source, lists other
known source nodes as unaffected, never claims cached/copied bytes were
erased, and never emits origin-wide `shareWithdraw`. Both command schemas,
preview tokens, audits, grant/revoke mutations, and body transport are one M4
acceptance slice; no unavailable/degraded success variant exists.

### 7.6 P5 — Operations

**P5.1 — Background sync steward.** A daemon-supervised `mesh-sync-steward`
job runs bounded anti-entropy rounds on an interval (default 300 s, jittered,
budget-capped; config `[mesh] sync_interval_seconds`), using the same core
round executor as the CLI (P1.4), and retries deferred member pairings
(§7.4 P3.2). It finally gives `steward_decision.rs` and `peer_state.rs`
(drift/staleness state machine) their production callers: missed rounds drive
`soft_stale`/`hard_stale` transitions surfaced in `ee team status`.
Explicitly opt-in-by-running-the-daemon. **Honest scope of "no daemon
needed":** foreground `ee team sync` performs a full bidirectional exchange
as the initiator, but other members cannot trigger a round against this
machine without its responder (see §6 listener asymmetry). Two members who
both never run a daemon cannot exchange at all; `ee team status` says so
rather than letting staleness look like a mystery.

**P5.2 — Daemon service install.** Non-technical users will not keep a
terminal open. `ee daemon install|uninstall|status` manages a per-user
service: launchd agent on macOS, systemd user unit on Linux. **Windows: the
daemon itself is `#[cfg(unix)]` today (`src/daemon/server.rs:25`), so v1
declares Windows members client-only: they send and receive during their own
outbound round, but cannot be contacted, **only when T2.1 has proved the
Windows credential-store parity contract**. Otherwise team join/sync/rotation
fail closed with `mesh_key_store_unavailable` while ordinary local ee remains
available. A Windows responder plus same-user
local control transport is the named follow-up.** The installed service is user-scoped and
multiplexes every valid registered workspace/team route; it is not silently
bound to whichever workspace installed it first. The installer follows
doctor-runtime mutation rules (backups, audit, undo path) and never requires
root. `ee team join` ends by offering the install command (printed, not
auto-run).

**P5.3 — Doctor + admission.** `ee doctor` gains team checks: responder
reachable from loopback, WhoIs/source binding, pair/signing-key owner/type/
symlink/parent-directory posture on Unix and SID/DACL/reparse/write-through
posture on Windows, body-cache path/publication parity on both platforms,
user-scoped broker registry integrity and
per-route DB/genesis validation, root-committed/local port agreement, all
broker routes agreeing on one port, host-wide port owner and the v1
one-responder-capable-OS-user limitation, member staleness, removal acknowledgements,
pending invites expiring, invite-authorization clock rollback, and the
revoke-all-before-floor-lowering repair invariant across pending
invites/leases/introductions, plus manifest divergence and port conflicts.
`admission.rs` (dead
931 LOC: rate limits, per-peer resource isolation) gets wired into the
responder accept path — inbound abuse (frame floods, oversized batches) is
where it was always needed. Its current decision API assumes an authenticated
peer, so the integration adds a separate bounded pre-auth path keyed only by
accepted source IP plus a listener-global ceiling. Unknown-node and malformed
bootstrap attempts update bounded in-memory counters/status only: they do not
reuse the `ee mesh peer unknown-attempt` diagnostic command as an audit
facility and never create one durable row per unauthenticated request.
Doctor/status also report P1.0's cumulative inbound normal usage by signed
origin and control usage by authenticated ee-node lineage plus team total,
the effective local ceilings and free-space floor, the
coalesced exhausted origins, and whether local writes remain unaffected.
Admission charges normal intake to the origin and control intake to the
non-recycled authenticated node rather than the relayer/connection; relay,
workspace churn, key rotation, or member rebinding cannot reset either
counter.

**P5.4 — Perf and eval gates.** Two-node sync round p50/p99 budgets recorded
via the existing `ee.perf.v1` harness (new `mesh_sync` bench profile, advisory
first); retrieval-quality eval fixture asserting team-scoped pack selection
stays deterministic given a fixed synced corpus.

**P5.5 — Docs.** Rewrite `docs/mesh/operator_onboarding.md`'s fitness table
(the **trusted small team** row changes to "Yes, via ee team"; contractor or
untrusted-peer rows remain "No by default" and recommend a separate
tightly-scoped team),
new `docs/team/quickstart.md` written for the Hana/Priya personas (the
client-facing artifact: Tailscale install → ee install → create/invite/join →
what is shared → how to stop future body serving, including the cached-copy
non-erasure caveat), agent-ux notes for team scope, ADR 0086 itself, and
CHANGELOG. Update the SRR6-era docs whose Status headers stay `proposed` for
surfaces this plan ships.

---

## 8. Security and privacy posture

Threat-model deltas on top of ADR 0037's ten rows (each new row keeps the
"control required" discipline):

| Threat | Control |
|---|---|
| Forged event origin or relay | Domain-separated Ed25519 origin signatures use strict verification and bind team, origin, sequence, key generation, and payload hash; dual-signed generation transitions prevent silent key substitution; relays preserve the signed bytes and cannot claim authorship. |
| Existing unsigned file-replay event is mistaken for team origin authority | `ee.mesh.event.v1` remains a separate non-origin-authoritative export/import-ledger contract. T1.3 normalizes it only into local policy input; T2.0/T2.4 never re-sign, relay, or reinterpret it as `ee.mesh.origin_event.v1`. Any future artifact conversion requires a new versioned schema. |
| Pair/session/signature proof is mistaken for team membership before the manifest authorizer exists | T2.0 only registers the base memory/manifest feature names; T4.1 installs the authorizer and is a hard T2.4 dependency before either feature is advertised. No pre-gate team event applies or relays; post-gate predecessor gaps quarantine and replay deterministically, with `teamCreated` the sole pre-membership exception. |
| Signed origin omits a stricter operation's required feature | Dispatch derives the mandatory feature/auth set from payload schema + operation and never trusts the origin list to turn checks off. Missing mandatory bits quarantine as `mesh_event_feature_contract_invalid`; unknown extra bits are `unsupported` and replayable after upgrade. |
| Clock rollback extends a pending invite, redemption lease, or per-pair introduction | A persisted invite-authorization floor advances on every credential decision and same-process monotonic deadlines still expire. Rollback blocks mint/redeem/resume with `team_invite_clock_rollback`; repair revokes all pending credentials atomically before lowering the floor, so none reactivates. |
| Valid origin-key equivocation | Incompatible signed tips/sequence hashes preserve both proofs, roll the origin back to the common materialized prefix, suspend further sharing, and converge peers on a fork-blocked posture; another member must revoke/rebind the lineage. |
| Frame replay, key-identity downgrade, or wrong-target forwarding | Production rejects dead key-shaped frame v1. V2 directional session keys bind random ee source/target nodes, team, initiator/responder endpoint workspaces, session, counter, direction, and request/response correlation. The receiver requires the exact next counter; application retry uses a fresh frame and stable event/operation idempotency key, never a TCP replay window. Stable Tailscale IDs stay in the handshake, current public keys are observations, and a relayed event origin never chooses a receiving DB. |
| Pair-key rotation crashes, local clock rolls back, or an attacker forces old-key fallback | Rotation is an explicit control-only durable two-phase generation transition, not a cross-node atomicity claim. A prior key can authenticate only the exact pending rotation for 86400 seconds, never ordinary traffic after local promotion; persisted time high-water makes rollback fail closed, no automatic downgrade exists, and expired/split state requires fresh pairing. |
| Spoofed bootstrap identity / rate-limit evasion | Accepted source IP is resolved with LocalAPI WhoIs; pre-auth buckets key on source IP plus a global cap, never a claimed header. |
| Authenticated member fills the append-only ledger or multiplies control reserve through origin churn | Ordinary cumulative intake charges the signed origin across relays/key rotations. Mandatory control intake charges the non-recycled authenticated ee node lineage with independent 1 MiB/node and 80 MiB/team caps; new workspaces/streams, rotations, reconnects, relays, and member rebinding cannot reset it. All limits are transactional, denial posture is coalesced, and local source truth is never charged. |
| Invite interception, locator redirection, wrong local process, or local secret capture | Codes are 256-bit, single-use leased, TTL-bound, secret-hashed at rest, exact-inviter-stable-node/ee-identity/team-root/committed-port-bound, revocable, and require mutual transcript/key confirmation. Fresh local Tailscale status resolves the current key/IP for that exact stable ID; embedded key/MagicDNS/IP values are observations only, routine key rotation inside the stable binding is accepted, and a missing/changed stable ID requires reissue. Before the secret is sent, the inviter must answer an invite-ID/nonce request with an Ed25519 challenge over root, identities, nonces, and port; a process that merely won the port race cannot harvest it. Join accepts the secret only into zeroizing buffers from a no-echo TTY or stdin—never argv/env/log/audit/error text—and a pre-key resume requires safe re-entry. An unbound code is still a bearer credential whose redemption can be stolen; `--for` never pretends otherwise. |
| Multiple OS users or mismatched custom ports on one Tailscale node | The root commits one immutable v1 port; all responder routes behind a broker agree, clients never scan/fallback, and status/doctor label a mismatched member client-only. One responder-capable OS user owns the host-wide address/port; another user must remain client-only or use another Tailscale node. |
| Windows client-only mode falls back to weak key files | Client-only removes the inbound listener only. Team credentials still require a reviewed safe DACL/reparse-point/opened-identity/write-through adapter; if parity is unavailable, `mesh_key_store_unavailable` blocks credential-bearing team commands instead of weakening storage. |
| Windows body hydration falls back to Unix-only cache assumptions | T5.9 reuses the reviewed secure-local-file adapter for reparse-point rejection, pinned opened identity, narrow non-inherited DACLs, and write-through atomic publication. Failure leaves retrieval metadata-only with high `mesh_body_cache_lifecycle_failed`; no sensitive body is published weakly. |
| Ambient or repository-local Git state rewrites project identity | Canonical Git runs directly with bounded/reaped execution, a minimal cleared environment, replacement/lazy-fetch/optional-lock behavior disabled, and nonempty grafts rejected; an installed Git lacking a required safety option is never retried weakly. Fallback reads exactly one raw local `origin` URL with includes and nonlocal config disabled, so aliases, prompts, rewrites, ambient object alternates, or multiple URLs cannot choose a project key. |
| Malicious/compromised member | Per-member revocation (US-7); sensitive lane grants remain per-node so blast radius is the explicitly granted nodes; trust elevation is per-member togglable; harmful-feedback demotion applies to synced rows like any other; emergency `ee team pause`. |
| An already-open session races emergency pause | The pause transaction commits a new durable generation before unregister/cancel/steward shutdown. Every round/frame/import/serve boundary rechecks it; resume advances the generation only after root/key/identity/policy validation and never reuses a stale session. Pause makes no cache/remote-erasure claim. |
| Compromised member widens lane profile or relaxes IdP policy | Manifest policy is coordination input: effective serving still intersects local policy + exact node grant + redaction/secret scan, and each node retains its explicitly accepted IdP floor until its operator accepts the exact relaxation generation. |
| Compromised member narrows lanes or tightens IdP policy | This is an explicit availability authority in v1: it can pause sharing but cannot widen disclosure. Status/audit expose author + generation; another active member can revoke the attacker and reconcile policy. Quorum/roles remain deferred. |
| Compromised member floods membership | The protocol hard-caps the complete active set at 20. Overflow is a complete-set `team_member_capacity_conflict` that pauses membership-dependent sharing; no arrival time or attacker-grindable hash chooses winners. A valid member can still cause an explicit availability incident, consistent with its other v1 manifest authority. |
| Compromised member binds unlimited devices | Each member has a four-active-ee-node protocol cap. `nodeBound` references the predecessor node-set root; concurrent additions commute only within the cap and otherwise all conflict while the predecessor set remains effective, so no arrival/hash winner gains sessions or grants. |
| Removal propagation latency or hidden target origin | Signed removals carry deterministic per-origin cutoffs, use bounded foreground fanout and peer relay, and retain an acknowledgement matrix. A target-attributable origin omitted from the map has a pre-first-event implicit cutoff, so a hidden node/stream cannot escape. If nobody receives the removal, propagation is unbounded and status says so. |
| Member/node/cutoff state changes after removal approval | Human confirmation and robot mutation use a canonical preview hash over the root/materializer generation and every security-relevant removal input. The transaction recomputes it before append; drift emits `team_member_removal_preview_stale` and commits no removal, authorization-generation advance, invite invalidation, audit/outbox, connection-cancel, or fanout side effect. |
| An already-open session races member/node revocation | Removal atomically advances durable session/grant authorization generations with the signed event. Every frame handler rechecks before import/serve; in-memory cancellation is idempotent post-commit, so a socket cannot outrun the durable fence. |
| Removed member already added a sock-puppet before the cutoff | That accepted-prefix member remains active in v1; the system does not mislabel it revoked. Every node persists `addedByRemovedMember`, emits `team_delegated_member_review_required`, enumerates it in removal preview/status/doctor, and recommends pause until the operator acknowledges or separately removes it. Preventing one active member from adding another requires quorum/roles and remains outside v1. |
| Concurrent removals mutually invalidate one another | Removal-dependency cycles are conflict-blocked as a complete set; pre-conflict membership remains effective and affected sharing pauses until explicit reconciliation. |
| Agent on a member's machine mints `human_explicit` → team-wide elevation | `human_explicit` is locally CLI-assignable. `ee why` shows the attestation basis; status counts rows; an atomic local-admission rolling cap ignores origin time, survives clock rollback, and demotes excess to `agent_validated` with `team_member_elevation_burst`; policy toggle and harmful-feedback demotion remain. |
| Inviter omits/fabricates the manifest toward a joiner | Root/origin signatures prevent alteration and false authorship; pairings and direct reconciliation expose omissions. An inviter-controlled sock puppet can still pair and is an explicit residual. |
| Tier-2 self-attestation, duplicate identity, future/overlong lease, verifier compromise, or bearer-token exposure | The distinct verifier hosts the secretless-public-client device flow and receives/zeroizes bearer material locally; the token-free `identity_attest` capability carries only ceremony metadata/status. The subject cannot author or renew its own lease; duplicate issuer/subject bindings conflict; receiver admission independently enforces 600-second future skew plus policy-cadence/evidence-expiry deadline caps, and late receipt cannot renew. Other members still trust a bounded attributed verifier assertion, so verifier compromise/collusion remains a named residual. |
| Local clock rollback extends a tier-2 lease | A persisted nondecreasing local identity-authorization time floor advances transactionally on identity-dependent token/grant/serve/import/steward/revalidation paths and never from peer claims. Read-only status/doctor/activity/audit use a non-persisting effective time. Forward-jump repair suppresses every current tier-2 lease before lowering the floor, then requires fresh interactive attestations. |
| IdP claims or device ceremony leak unnecessary identity data | Claim reduction is allowlist/minimum: subject, optional explicitly previewed email, configured-group matches, and decision only; full group lists/unrelated claims never persist or replicate. Verification URLs/codes/poll state are TTL ephemera excluded from manifest/audit/support bundles. |
| Compromised inviter at join time | Introductions are distributed by the inviter, but pair/signing-key confirmation is direct. The inviter can omit history or introduce its own sock puppet; reconcile surfaces divergence. |
| Membership manifest tampering / arrival-order split | Signed typed operations, cutoff-based removals, cycle-conflicted mutual removals, expected predecessors, conflict-blocked scalar fields, and deterministic rematerialization; no arrival winner. |
| Trust-class laundering via import paths | P0.6 closes the JSONL/playbook bypass with a store-local MAC over a constant-size ordered-record root computed and emitted from one read snapshot; import recomputes it inside one rollback-capable transaction before any native-trust side effects commit. Elevation to `peer_human_attested` has exactly one, fully-audited path. |
| Peer material echoes, impersonates another member, or gains local authorship through a null member field | Origin emission admits only explicitly source-owned local rows. Receivers derive producer member/project from the verified origin node/key/manifest authorization position, quarantine absent/ambiguous/mismatched bindings, and never infer local ownership from null attribution. Inbound materialization/index/curation never emits; derivatives mint a local ID with explicit provenance. |
| Join silently publishes old workspace history | Activation projects future metadata only; historical sharing requires a revision-pinned preview/confirm flow with per-item revalidation and an idempotent projection marker. |
| History preview understates future audience | Historical metadata consent says it enters team history for current and future active members until origin-wide withdrawal; only body grants are pinned to current nodes. |
| Member-supplied time manipulates trust, ranking, lifecycle, or recent-activity visibility | Signed `producedAt` is member-attested provenance/display only. Authorization, elevation caps, retention/decay, lifecycle mutation, and default retrieval relevance use no origin clock; explicit-as-of activity isolates claims over the 600-second future-skew bound in deterministic `clockAnomalies`. Backdating can omit an event from an explicit member-attested time window, so the schema states that residual and directs completeness checks to an unfiltered generation-stable cursor drain or origin-sequence audit. |
| Policy-denied event stalls its stream | Signed safe headers advance a contiguous receipt frontier; a separate scan frontier advances over explicit per-event applied/withheld/quarantined/unsupported dispositions, so later events apply while denied payloads remain fetchable after policy change. |
| Policy denial suppresses a later purge | `tombstone`/`shareWithdraw` carry only opaque revocation references and are mandatory for active members; current content policy cannot strand previously admitted derived material. |
| Data exfil via wider lanes or state changes after approval | Deterministic token-free preview (or explicit marked-sensitive robot issuance) → canonical approval snapshot → short-lived nonce-salted snapshot tag + context-bound envelope MAC → bounded stdin/in-process verification → generation CAS + mutation + audit in one transaction, plus the hard secret-scan deny on every export/serve path. Invalid, expired, replayed, or target/grant/policy/scanner/candidate/sample/caution/copy-drifted tokens require a separate fresh preview with zero mutation side effects; errors expose no replacement bearer. |
| Preview/secret-scan hashes become content or equality oracles | Public per-content, per-sample, aggregate-preview, and secret-value hashes are removed. Secret findings use unrelated random per-occurrence IDs. Approval tags are keyed and salted by a fresh token nonce, so equal previews do not link; only opaque non-replayable identifiers persist. Chosen-input, cross-store, and support-bundle observers cannot test guesses. |
| A robot approval response is captured by an external session recorder | Ordinary preview JSON is token-free; issuance requires `--issue-approval-token`. The optional `approvalToken` is marked sensitive, prefixed `eeap1_`, and omitted/redacted from ee-controlled traces, errors, audit, support, and CASS-import materialization. Raw third-party stdout/session capture remains an explicit residual until the 15-minute, context-bound, generation-single-use bearer expires. |
| Team metadata or body commitment is used as a content oracle | The closed metadata schema excludes body/title/preview/tags/URIs/raw paths. Every revision signs a fresh-nonce salted commitment and withholds its nonce until authenticated-session-derived member/grant authorization succeeds; metadata-only peers cannot test guesses or link byte-identical revisions. |
| Redaction changes bytes while claiming the signed event commitment | V1 never transforms a body during fetch. A redact-only policy serves only an event-signed `already_redacted` representation; exact bytes otherwise stay metadata-only. Returned nonce plus streamed bytes must reproduce the signed `bodyCommitment`, and a newly triggered secret scan denies rather than mutates. |
| Relayer serves a cached body or an old event receives current bytes | Only the event's owning origin workspace/node serves from local source truth. Relays never become body authorities; tombstoned/withdrawn/missing old revisions return unavailable rather than substitution. |
| Body-lane grant is revoked after the recipient fetched bytes or on only one source node | `revoke-lane`/`team unshare bodies` advance the node-scoped generation and stop future serving from the named local source, but cannot erase cached/copied bytes or affect another source node. Output and audit say both, and a per-recipient revoke never misuses origin-wide `shareWithdraw`. |
| Partial/failed body fetch leaks or publishes bytes | Private link-safe temporary objects stay outside retrieval until full verification/admission; Unix 0600/no-symlink/fsync or Windows narrow-DACL/reparse-safe/write-through publication is atomic, opaque-keyed, quota-governed, and excluded from support bundles. Security/lifecycle failure leaves metadata-only posture and publishes nothing. |
| Withdrawal deletes local source truth or leaves a readable cache object | Purge targets only derived peer material/cache objects; origin-owned truth is protected, and metadata cannot report an object unavailable until its retrieval path is actually closed. |
| New device inherits a sensitive member grant | Grants pin the current active node set; later node bindings start metadata-only and require a new body preview/consent. |
| Tailnet-membership creep (new devices appear) | Discovery policy still gates probes/responses; team sync only talks to *enrolled member nodes* regardless of who else is on the tailnet; unknown-node hellos get a privacy-preserving decline and bounded in-memory rate/status accounting, never per-attempt durable audit amplification. |
| Stolen/re-assigned or routinely rotated node masquerading as a member | The ee node binds tailnet + Tailscale stable node ID + ee signing identity; current public keys are audited observations. Same-stable-ID rotation needs pair/signing continuity (and tier-1 owner continuity when enabled), while missing/changed stable identity needs a fresh ceremony. Tier-1 owner mismatch suspends grants. |
| IdP discovery SSRF, JOSE key redirection/parser ambiguity, incompatible provider, compromise, or token theft | Tier 2 preflights a secretless public-client device method and rejects client-secret-required providers. HTTPS-only constrained curl runs from a minimal allowlisted environment with ambient config/proxy/netrc/CA/keylog state disabled; credential-bearing POST redirects are rejected; GET redirects are manually validated; DNS answers are validated and pinned; exact issuer/endpoints and fresh discovery/JWKS per presentation are required; bounded/reaped/redacted subprocess I/O applies. Duplicate JSON names, noncanonical JWT encoding, unsupported critical headers, token-controlled/embedded keys, and ambiguous `kid` all fail; only exact issuer JWKS raw parameters are eligible. Raw ID/access/refresh tokens stay at the distinct verifier, are zeroized, and never persist or traverse the mesh; key-strength + algorithm/issuer/audience/`azp`/time/verified-email/group checks, an atomic replay ledger, and nonce extension when available (otherwise explicit freshness+single-use assurance) complete the boundary. |
| Offboarded employee retains access | Tier 1 detects tailnet-owner mismatch/disappearance noninteractively. Tier 2 is lease-based, not background directory polling: offboarding prevents the next interactive renewal with a distinct verifier, and grants suspend after revalidation interval + grace. Already-synced local copies remain non-erasable. |
| IdP outage locks the team out | Identity checks run only in identity commands and revalidation; sync and retrieval continue on existing grants through the configured grace window (`identity_revalidation_overdue` warning first, suspension only after grace). |

Compliance story for the Marcus persona: `ee team audit --json` +
actual grant/body-share/export mutation audit rows + the hash-chained import
ledger give an
exportable, cryptographically attributable record of what **this store
observed**: which operator approved and applied an exposure, which node grants
were issued, which signed
events were received/applied/withheld, and which sync attempts completed.
It is not falsely called a globally complete record: an offline or compromised
peer can omit events, and clocks remain attributed claims. Revocation caveat
stays honest: future serving can be stopped, but already-synced copies on
another machine are not remotely erased.

---

## 9. Schema, contract, and config changes (registry checklist)

Per AGENTS.md contract-drift rules, every item below lands with its gate:

- **New schemas** (`docs/schemas/` + drift tests): mechanism contracts
  `ee.mesh.tailscale_transport_frame.v2` (supersedes and rejects dead v1
  before production), `ee.mesh.pair_rotation.v1`,
  `ee.mesh.rotate_pair.v1`, `ee.mesh.origin_event.v1`,
  `ee.mesh.memory_event.v1`,
  `ee.team.manifest_event.v1`, `ee.mesh.share_preview.v2`,
  `ee.mesh.export_secret_scan.v2`,
  `ee.mesh.lane_grant_preview.v2`, the nested opaque-envelope contract
  `ee.mesh.approval_token.v1`, `ee.mesh.grant.v1`, and
  `ee.mesh.revoke_lane.v1`; plus one top-level schema per executable
  `ee team` leaf:
  `ee.team.create.v1`, `ee.team.invite.v1`,
  `ee.team.invite.revoke.v1`, `ee.team.join.v1`,
  `ee.team.members.list.v1`, `ee.team.members.show.v1`,
  `ee.team.members.trust.v1`, `ee.team.members.rotate_key.v1`,
  `ee.team.members.reconcile.v1`, and `ee.team.members.revalidate.v1`,
  `ee.team.member.add_node.v1`, `ee.team.member.remove.v1`,
  `ee.team.leave.v1`,
  `ee.team.projects.share.v1`, `ee.team.projects.adopt.v1`,
  `ee.team.projects.list.v1`, and `ee.team.projects.reconcile.v1`,
  `ee.team.share.history.v1`, `ee.team.share.bodies.v1`,
  `ee.team.unshare.bodies.v1`,
  `ee.team.status.v1`, `ee.team.sync.v1`, `ee.team.pause.v1`,
  `ee.team.resume.v1`, `ee.team.activity.v1`, `ee.team.audit.v1`,
  `ee.team.idp.require.v1`, and `ee.team.idp.set.v1`.
  The already-registered `ee.mesh.event.v1` remains explicitly labeled as
  file replay/import-ledger evidence and is never advertised as a live team
  origin-stream schema.
  The three preview/scan v2 contracts directly supersede their v1 shapes:
  public content/secret/sample hashes and side-effecting preview consent are
  removed, lane preview targets opaque peer identity and returns an
  authenticated `approvalToken` only under explicit
  `--issue-approval-token`, and no compatibility alias is retained. The
  ordinary response is deterministic/token-free; the optional field is
  omitted unless issuance was requested. When present it is one closed object:
  `approvalToken = { schema: "ee.mesh.approval_token.v1", value:
  "eeap1_<base64url>", expiresAt: <RFC3339>, handling: "secret" }`. The
  envelope serializes no stable store/workspace/key identifier; schema and
  contract tests pin the prefix, omission rule, `handling` constant, bounds,
  and redaction behavior.
  Group nodes emit no response. Flags such as
  invite default/`--wait`/`--no-wait`/`--resume` and join `--dry-run` use
  explicitly tagged variants within their one leaf schema; member removal
  likewise tags read-only `preview` versus hash-confirmed `applied`. A
  command-inventory contract test fails if a new leaf
  lacks an exact schema mapping; sibling leaves may share `$defs` but never a
  top-level schema ID (ADR 0086 TC-D15). The
  reserved-never-published `ee.mesh.peer_status.v1` name is retired
  (mechanism posture stays on `ee.mesh.auto_status.v2`/foreground status;
  team posture is `ee.team.status.v1`); the `ee.mesh.import_ledger.v1`
  inspection surface is owned by T1.3 (which writes the ledger decision
  columns), shipped with it or explicitly deferred in its closeout.
- **New degraded codes** (each with fixture + taxonomy entry, same commit):
  `mesh_transport_unreachable`, `mesh_frame_auth_failed`,
  `mesh_frame_target_mismatch`, `mesh_frame_replay_rejected`,
  `mesh_bootstrap_identity_unverified`, `mesh_responder_route_unavailable`,
  `mesh_peer_identity_upgrade_required`,
  `mesh_key_store_unavailable` (high),
  `mesh_store_authentication_unavailable` (high),
  `mesh_approval_token_invalid` (high),
  `mesh_approval_token_stale` (warning),
  `mesh_responder_port_conflict`, `team_responder_port_mismatch`,
  `mesh_pair_rotation_repair_required` (high),
  `mesh_event_payload_withheld`,
  `mesh_event_feature_contract_invalid` (high),
  `mesh_inbound_storage_budget_exhausted`,
  `team_invite_expired`, `team_invite_replayed`,
  `team_invite_clock_rollback` (high),
  `team_member_unknown_node`, `team_manifest_conflict`,
  `team_member_removal_preview_stale` (warning),
  `team_removal_acknowledgement_pending`,
  `team_delegated_member_review_required` (high),
  `share_preview_peer_unknown`, `team_daemon_not_installed` (info),
  `team_member_identity_mismatch`, `identity_revalidation_failed`,
  `identity_revalidation_overdue` (warning), `team_idp_unreachable`,
  `team_identity_clock_rollback` (warning),
  `team_idp_provider_unsupported`, `team_idp_token_invalid`,
  `mesh_remote_evidence_body_size_exceeds_policy`,
  `mesh_remote_evidence_declared_size_mismatch`,
  `mesh_fetched_body_hash_mismatch`, and
  `mesh_remote_evidence_stream_io_failed` (module-level plan/stream codes
  shipped by T1.1 in commit 63514470 — `src/mesh/remote_evidence.rs`
  `degraded_codes` — with test-only call sites today; the response-surface
  fixture + taxonomy obligation lands with the first CLI emitter, T5.9's
  fetch adapter, in that same commit per the registry rule),
  `team_attribution_unresolved` (T2.4/T4.1 receiver-derived attribution:
  missing/ambiguous/payload-mismatched member resolution quarantines the
  event — ADR TC-D6),
  `team_idp_device_flow_expired` (T7.4/T7.6 device-ceremony
  provider/local-deadline/poll-budget expiry; machine reason + explicit
  restart action),
  `team_member_removed_stream_rejected` (emitter under the cutoff model:
  events quarantined above a signed removal cutoff or from a removed
  lineage's stream),
  `mesh_rematerialization_pending` (warning),
  `mesh_rematerialization_failed` (high),
  `team_member_capacity_conflict`,
  `team_node_capacity_conflict`,
  `team_member_elevation_burst`,
  `mesh_body_cache_lifecycle_failed`, `team_policy_relaxation_pending`, plus
  backfill of the 19 existing uncovered codes (P0.7).
- **New env vars** (register in `src/config/env_registry.rs` + `docs/env_vars.md`):
  `EE_TEAM_INVITE_TTL_SECONDS`, `EE_MESH_SYNC_INTERVAL_SECONDS`,
  `EE_MESH_TRANSPORT_DISABLED` (belt-and-braces kill switch),
  `EE_TEAM_IDP_HTTP_BACKEND` (only shipped value `curl`; `native` reserved and
  rejected at parse time — §13 item 1),
  `EE_TEAM_IDENTITY_REVALIDATE_DAYS` (local override that may only *tighten*
  the manifest-configured cadence, never loosen it — the manifest stays the
  team-wide policy authority). `EE_MESH_HELLO_PORT` and
  `EE_MESH_HELLO_RESPONDER_DISABLED` pre-exist in the registry and become
  load-bearing here: create commits the current port to the team root, join
  consumes that locator, and a local override that disagrees yields
  client-only mismatch posture rather than scanning/fallback. Emitter map for the new codes:
  `mesh_transport_unreachable` (T2.1 session connect/timeout, T2.2 no verified
  local tailnet address/listener closure, reused by the T2.4 round executor;
  repeated supervision retries coalesce rather than append one row each),
  `mesh_frame_auth_failed` (T2.1 accept path:
  bad MAC / session frame from an unkeyed peer), frame target/replay codes
  (T2.1), bootstrap identity failure (T2.2), payload-withheld posture
  (T2.4b), `mesh_inbound_storage_budget_exhausted` (T2.4 cumulative
  signed-origin normal plus authenticated-node/team control and free-space
  admission; T6.3 reports usage/repair),
  `mesh_peer_identity_upgrade_required` (T2.2 blocks a legacy
  node-key-derived peer record that lacks a ceremony-proven stable binding;
  recovery re-enrolls it), `mesh_responder_route_unavailable` (T2.2 local route validation;
  stale/missing/ambiguous workspace/team registration, paths redacted),
  `mesh_key_store_unavailable` (T2.1: platform key storage cannot prove the
  required owner/no-link/atomic-durability contract; credential-bearing team
  commands are blocked rather than using weaker storage),
  `mesh_store_authentication_unavailable` (T1.6: the hardened store-local
  authentication root cannot be created/read/verified; native-trust import
  and exposure approval fail closed while ordinary local memory remains
  usable), `mesh_approval_token_invalid` (T1.4/T5.9: malformed, wrong-domain,
  wrong-store/workspace/surface, future-issued, retired-key, or envelope-MAC-
  invalid token;
  no mutation), `mesh_approval_token_stale` (T1.4/T5.9: the canonical
  approval snapshot changed, the token expired, or a successful/concurrent
  generation advance made it replayed; return a structured re-preview action
  with no replacement bearer and commit nothing),
  `mesh_responder_port_conflict` (T2.2 cannot acquire the root-committed
  address/port, including another local user/process; never falls back),
  `team_responder_port_mismatch` (T4.1/join/status: local configured port or
  another registered team differs from the immutable team-root port, so this
  member is client-only until repaired),
  `mesh_pair_rotation_repair_required` (T3.2: a two-phase pair-key rotation
  remains incomplete after the 86400-second control-only resume grace, its
  key-store/DB projection disagrees, or local clock rollback makes the grace
  unverifiable; fresh pairing repairs it, never automatic downgrade),
  `team_member_unknown_node`
  (T4.1/T2.4 import path: signature key bound to no member),
  `team_member_removal_preview_stale` (T4.4: team/root/materializer, target,
  node/signing, cutoff, delegated-member, or acknowledgement inputs changed
  after preview; no mutation commits and recovery regenerates the preview),
  `team_daemon_not_installed` (info; join epilogue + `ee team status`,
  P5.2), `team_manifest_conflict` (T4.1 application; resolved via
  `reconcile`), `team_member_capacity_conflict` (T4.1 complete-set
  materialization exceeds the 20-active-member protocol bound; sharing
  remains paused until explicit reconciliation/removal),
  `team_node_capacity_conflict` (T4.1 a predecessor-rooted complete node-set
  successor would exceed four active nodes for one member; all contested
  additions remain inactive),
  `team_delegated_member_review_required` (T4.1/T4.4: a member introduced
  through the removed member's accepted prefix remains active and requires
  explicit local acknowledgement or separate removal; pause is recommended),
  `team_policy_relaxation_pending` (T4.1/T7.4 local IdP floor
  refuses an unaccepted remote relaxation or incomparable policy generation),
  `team_identity_clock_rollback` (T7.3/T7.5/T7.6 shared local identity-time
  guard; decisions continue against the persisted high-water floor, and an
  explicit repair suppresses current leases before lowering it),
  `mesh_body_cache_lifecycle_failed` (T5.9 platform-security proof,
  publication, eviction, or purge consistency failure; high severity, paths
  redacted; affected bodies remain metadata-only and no weak fallback is
  published).
- **Config**: `[team]` section (`elevate_member_human_explicit`, defaults,
  `peer_human_attested_max_per_member_24h = 100` with checked positive local
  override). `PAIR_KEY_ROTATION_GRACE_SECONDS = 86400` is a v1 protocol
  constant, not a peer- or environment-controlled setting.
  `APPROVAL_TOKEN_TTL_SECONDS = 900` is likewise a fixed v1 local-consent
  bound, not peer- or environment-controlled;
  `[mesh] sync_interval_seconds`; `[mesh.admission]`
  `max_inbound_origin_bytes = 67108864`,
  `max_inbound_team_bytes = 268435456` (the per-team-workspace total —
  renamed from the review's `max_inbound_event_bytes`, which misleadingly
  suggested a per-event limit),
  `control_reserve_bytes_per_node = 1048576`,
  `max_control_reserve_bytes_per_team = 83886080`, and
  `min_free_bytes = 1073741824` (local-only, checked/positive, peer and
  manifest cannot change them). The old undocumented
  `trust.team_members` nickname list is removed with direct migration and
  CHANGELOG guidance; no unauthenticated human-team shim survives. Fix the documented-but-unread
  `[mesh.tailscale]` block one way or the other (read it, or fix README).
- **DB migrations** (append-only; shipped migrations are checksummed):
  **`mesh_origin_events` (the outbound origin stream — §7.2 P1.0, the
  foundational one)**, opaque-handle `mesh_peers` stable-node/ee-node/current-
  key-observation identity columns and upgrade posture, `team_members`,
  `team_member_nodes`, `team_manifest`
  cache, verified-receipt/disposition-scan frontiers and per-event disposition
  ledger, transactionally maintained cumulative inbound charged-byte counters
  and coalesced exhausted-origin posture, signed origin-fork evidence/state,
  removal acknowledgements,
  crash-resumable join state, tailnet/stable-node bindings with separately
  audited current rotating-key observations,
  `workspaces.project_key(+source+aliases)`,
  pending-invites table plus per-team invite-authorization time floor,
  per-node accepted-IdP-policy floor/generation,
  per-team local identity-authorization time floor plus suppressed-lease
  state for explicitly confirmed forward-jump repair,
  per-team/per-memory origin-projection state plus
  revision-pinned resumable history-share jobs, receiver-derived
  producer-member/project attribution on peer projections, per-origin-event
  private body-commitment nonce storage, and the
  trust-class admission — which is a recreate-style table rebuild at every
  CHECK site (§7.3.2), not a constraint tweak. Pair/signing secrets live in
  hardened 0700-directory/0600-file storage on Unix or equivalent
  SID/DACL/reparse/write-through storage on Windows under the user data dir,
  not in the DB. The store-local authentication root follows the same
  hardened boundary but has a distinct key namespace/lifecycle; raw root and
  derived subkeys never enter SQLite. Only nonsecret key IDs and bounded
  import-verification-window metadata may be persisted.
- **User-scoped responder registry**: extend the existing hardened workspace
  registry/catalog with exact workspace ID, canonical database identity,
  team ID/genesis hash, route generation, and enabled/posture fields. The
  registry never becomes a cross-workspace source-of-truth transaction and
  exposes no raw path over network or support-bundle surfaces.
- **Exit codes / envelope**: no changes; everything rides `ee.response.v2`.
- **Effect registry**: new team commands classified (`durable_write` for
  join/grant/revoke/pair-rotate/member/history-share/body-share/body-unshare
  ops, sync, pause/resume, identity mutation/revalidation, and daemon/service
  changes; `read_only` for status/doctor/members/project list/show,
  activity/audit, every consent preview, and the tagged
  `team member remove --preview` variant). Read-only commands append neither
  audit rows nor persisted identity-time changes; `ee share preview
  --record-consent` is removed, and the token-consuming exposure mutation
  owns the consent audit. The hash-confirmed removal is `durable_write`.

---

## 10. Testing strategy

| Layer | What |
|---|---|
| Unit | Every new pure decision (canonical origin/payload codecs and deterministic event IDs, closed memory-metadata allowlist and salted body commitment, unique manifest-genesis/root validation, stable-node-bound invite codec, persisted invite-authorization floor/monotonic expiry/revoke-all repair, pair/session KDF and exact-next counters, dual-signed signing-key transition, receiver-derived member/project attribution, local-admission elevation cap/clock high-water, project-key derivation/aliasing including shallow and Git object-format cases, origin-projection eligibility/revision fencing, manifest cutoff/cycle/member-capacity/node-set-capacity application, node-bind/revoke continuity, commuting identity leases/future-skew/deadline bounds/duplicate-subject conflicts, persisted identity-authorization time floor and safe repair, lane/intersection and IdP-floor partial-order decisions, origin-clock anomaly classification, body-cache publication/lifecycle state, precedence constant) with happy/edge/error cases per testing policy. |
| Contract | Schema drift tests for every `ee.team.*` and changed mesh schema; degraded-code catalog ↔ fixture ↔ taxonomy sync (extends the J6 failure-mode catalog validator, `tests/contracts/failure_mode_fixtures.rs`). |
| Golden | `ee team status`/`members`/`activity` JSON goldens (server-path regen only, per the golden workflow); refreshed mesh status goldens after P0.5 de-hardcoding. |
| File/live event boundary | Existing `ee.mesh.event.v1` export/import rows remain locally policy-capped, non-origin-authoritative evidence. Typed signed `ee.mesh.origin_event.v1` inputs normalize into the same policy decision without reserialization; attempts to re-sign, relay, or reinterpret a legacy row as team authority fail. Schema inventory keeps both purposes explicit. |
| Team-authority staging boundary | A binary with the T2.0 codecs but without T4.1 advertises neither `mesh.team.memory.v1` nor `mesh.team.manifest.v1` and cannot apply or relay those operations merely on pair/session/signature proof. After T4.1, valid genesis enables the base features; missing member predecessors quarantine, replay after arrival, and never gain arrival-order authority. Base-feature-only peers still disposition `identityAttested` unsupported. |
| Required-feature anti-omission | For every typed operation, remove each mandatory derived feature in turn and assert high `mesh_event_feature_contract_invalid`, no materialization/relay, and a durable quarantine. Duplicate/unsorted/oversized feature lists fail canonical validation; unknown extra features produce replayable `unsupported` state and apply only after feature support is installed. |
| Frame-v2 boundary | Production rejects dead frame v1. V2 golden/MAC vectors bind random ee node IDs, team, both endpoint workspaces, session, direction, counter, request, capability, budget, and payload hash; rotating Tailscale keys never occupy endpoint-ID fields. Wrong target/workspace/team/session/direction plus duplicate/skipped/regressed counters and v1 downgrade fail before capability dispatch. A retried application uses the next frame counter and the same idempotency key. Base-manifest-only peers disposition an `identityAttested` event requiring `mesh.team.identity_attested.v1` as unsupported rather than applying it. |
| Listener lifecycle | Starting before tailscaled is ready binds nothing and retries under coalesced `mesh_transport_unreachable`; tailnet-address loss closes the listener with the same code; a verified address-set replacement drops stale sockets and binds only the new set; wildcard/stale-hint fallback never occurs and unrelated workspace routes retain honest posture. |
| Cross-platform key store | Unix owner/mode/no-symlink/atomic-fsync vectors and Windows SID/DACL/reparse-point/opened-file-identity/write-through vectors exercise create/read/rotate/crash/attacker-controlled-parent cases. A Windows build lacking the safe adapter emits high `mesh_key_store_unavailable` and blocks join/sync/rotate without affecting ordinary local commands; no test accepts inherited broad ACLs, shell-based permission repair, or project-owned unsafe. |
| Cross-platform body cache | Unix owner/mode/no-symlink/fsync and Windows SID/DACL/reparse-point/opened-file-identity/write-through vectors cover temporary creation, verified publication, crash boundaries, eviction, and withdrawal under hostile parents. Crash injection proves `staging` stays invisible until verified rename/fsync and `invalidated_pending_purge` closes retrieval/index access before removal; restart/steward/doctor reconcile staged orphans and pending purges idempotently, and filesystem presence never resurrects availability. Failure emits high `mesh_body_cache_lifecycle_failed`, leaves the item metadata-only, and proves no weakly protected object is retrieval-addressable; the T2.1 secure-file primitive is reused rather than forked. |
| Metadata disclosure and body commitment | Schema/golden tests enumerate the exact safe-header and memory-metadata allowlists and prove body/title/preview/tags/URI/raw-path/evidence/nonce fields cannot leak through metadata, activity, audit, status, or support bundles. Fresh 32-byte nonces make equal bodies in distinct revisions unlinkable; metadata-only peers cannot verify guesses. `exact` and `already_redacted` bodies publish only when transfer integrity passes and returned nonce plus exact bytes recompute the signed `bodyCommitment`. A redact-only policy over `exact` remains metadata-only; no transformed bytes publish under the source commitment. |
| Preview/secret-finding privacy and consent fencing | `ee.mesh.share_preview.v2` is read-only, has no `--record-consent`, public content/sample hashes, or token-shaped fallback. `ee.mesh.export_secret_scan.v2` replaces raw-value hashes with unrelated per-scan random occurrence IDs, including chosen-input/repeat-scan tests; its pure detector remains byte-deterministic and ID-free, while an injected secure-random command boundary decorates canonical findings. Grant/body tokens have fresh nonces, a 15-minute bound, separate nonce-salted snapshot tag and context-bound envelope MAC, current-key-only verification, and no serialized stable identifier. Tests distinguish malformed/wrong-context/MAC-invalid from authentic stale/expired, prove equal previews unlinkable, and serialize concurrent apply so one generation-CAS transaction wins. Default preview JSON is deterministic/token-free; human mode never prints the token, while robot issuance is explicit and apply accepts bounded stdin only. The marked-sensitive `eeap1_` value is absent from argv/env/ee-controlled trace/error/audit/support and redacted by CASS import; tests and operator copy name raw external-recorder capture as a bounded residual. Mutating every target/grant/policy/scanner/candidate/sample/caution/copy input requires a separate fresh preview and yields zero grant/audit/outbox/fetch/cache effects; failure errors contain no replacement bearer. |
| Pair-key rotation | `ee mesh rotate-pair` is the explicit trigger and the ≤4 KiB `pair_rotate` capability is the only old-key resume surface. Crash/restart at every stage/accept/commit/promote boundary converges without accepting old-key application traffic. A promoted/staged split resumes only the exact rotation for 86400 seconds; replayed, concurrent, wrong-generation, wrong-transcript, ordinary-session downgrade, post-grace, and persisted-time-floor rollback attempts fail. Promotion closes old sessions, expired/unverifiable state emits `mesh_pair_rotation_repair_required`, and fresh pairing repairs it. `ee team members rotate-key` goldens say signing lineage, never pair key. |
| Invite/introduction time safety | Mint, lease, redeem/resume, revoke, expiry, and introduction authorization advance one persisted nondecreasing floor; same-process monotonic deadlines also fire. Rollback at every boundary and restart emits `team_invite_clock_rollback` and cannot extend/reuse a credential; a forward jump may expire early. Doctor repair proves all pending invites, leases, and introductions are atomically revoked before lowering the floor, with no resurrection. |
| Grant narrowing | `revoke-lane` immediately blocks future serving, advances the exact-node generation, is idempotent under replay, invalidates stale previews, survives current Tailscale key rotation without retargeting, and requires a fresh preview to re-grant. Team body unshare fans out to the current recipient set **from the named local source node** and every result/golden carries both the other-source-nodes-unaffected and cached-copy non-erasure caveats; no per-recipient revoke emits `shareWithdraw`. |
| Command effects and emergency pause | Read-only status/doctor/list/activity/audit/preview commands leave application tables, audit count, persisted identity floor, and pause generation byte-identical. Mutating leaves audit in their state transaction. A planted live session cannot import/serve/fetch after pause-generation commit even when cancellation is delayed or a crash intervenes; resume validates root/key/identity/policy, advances the generation, rejects stale sessions, and never claims cache/remote erasure. |
| Attribution boundary | Receiver-derived producer member/project follows the verified node/key/manifest authorization position. Spoofed payload member IDs, authenticated nodes claiming another member, ambiguous/missing bindings, and legacy peer rows with null member attribution quarantine and never become local. Elevation accounting and body-request authorization use the derived member. |
| Integration (the centerpiece) | **Two-node loopback harness (P1.5)**: real binaries, real sockets, distinct bound 127/8 source addresses, fake Tailscale WhoIs keyed only by accepted source. Scenario matrix: one outbound connection exchanges missing ranges in both directions without an initiator listener; pair → sync → attribute; two client-only nodes cannot connect; partition → rejoin; ordinary fork/signature rejection; signing-key rotation and generation-gap rejection; same-stable-ID Tailscale key rotation preserves the ee binding/session and exact-node grant while a changed/missing stable ID fails closed; peer IDs remain opaque handles and an unverifiable legacy key-derived peer is blocked with upgrade guidance rather than auto-bound; relay-mutated event IDs fail deterministic recomputation; wrong-endpoint-workspace + frame replay rejection; removal fanout uses tip/range exchange and no event-push frame; spoofed bootstrap headers and bucket rotation fail without growing durable DB/audit rows; withheld payload at N does not block N+1; a later policy denial cannot withhold tombstone/shareWithdraw control or strand previously admitted material; daemon/foreground listener ownership races, unrelated/other-EUID port conflicts, root/local port mismatch, and multiple teams with different committed ports all fail without scan/fallback; two local workspace DBs multiplex through one responder while stale/moved/symlinked/cross-workspace/wrong-genesis/different-EUID/replayed/network-path routes fail closed; invite stale-IP and same-stable-ID current-key rotation succeed, while hint-to-wrong-stable-ID and missing/changed stable identity fail before secret transmission; a wrong process on the right host/port cannot receive the secret because its signed challenge fails first; plain invite without a daemon waits usefully, daemon-backed/default/explicit-wait correlate correctly, no-wait requires a confirmed broker route, and interrupted wait resumes by invite ID; invite genesis-hash/port mismatch plus replay/crash-resume with required pre-key secret re-entry and no secret in argv/env/log/audit/error captures; missing/duplicate/conflicting genesis blocks; remote lane widening mints no grant and IdP relaxation/incomparability stays locally pending until exact-generation acceptance; unbound-invite bearer residual is printed; peer-import/index/curation never re-emits; future-only activation plus history preview/revision-change/live-race/crash-resume; a later joiner receives confirmed durable history but not origin-withdrawn history; new nodes do not inherit body grants; node-revocation cutoff and lost-last-node recovery fail closed; mutual removals conflict without orphaning the team; complete-set membership overflow produces the same capacity conflict under arrival permutations; elevation on/off/overflow/origin-time/clock-rollback/concurrency; Git SHA-1/SHA-256/multi-root/shallow project identity and aliasing; planted memory secret never crosses. Three-node variant: intact signed relay succeeds, forged relay fails, a valid origin equivocates to two peers and both later retain both proofs/roll back to the common fork-blocked prefix, remover goes offline after one acknowledgement and removal propagates, unacknowledged exposure remains visible, a target's later-revealed origin omitted from the removal cutoff map retains no authority, and introductions/deferred pairing complete. |
| Project identity probe hardening | Replacement refs are ignored; nonempty grafts fail closed; absent promisor objects cannot trigger lazy network fetch; a fake/old Git missing a required global safety option is never retried without it and reaches only the explicit degraded fallback/mint path. Ambient `GIT_*` repository/object/config/prompt/trace state, ambient object alternates, global/system config, includes, rewrite rules, and optional locks cannot influence a key or leak. Raw local `origin` is accepted only when exactly one distinct usable canonical URL remains; multiple values are ambiguous. |
| Manifest capacity | Starting from three active nodes, every arrival permutation of two valid concurrent `nodeBound` successors from the same predecessor root yields the same `team_node_capacity_conflict`, preserves the three-node predecessor set, and grants neither successor; revoke-then-add succeeds. Member overflow retains the analogous complete-set test at 20. |
| Member-removal preview and delegated residual | Robot removal requires a canonical preview hash over root/generation, target, nodes/signing generations, cutoffs, accepted-prefix active additions, acknowledgement audience, and non-erasure residual. Mutating each input after preview returns `team_member_removal_preview_stale` with zero removal/session/grant/audit/fanout effects. A successful removal leaves accepted-prefix additions active but produces `addedByRemovedMember` and high `team_delegated_member_review_required` on every node; preview/status/doctor enumerate them and recommend pause. Local acknowledgement clears only local review state, while separate signed removal revokes the member. Tests prove the flag alone never gets described as revocation. |
| Ingress storage | Repeated individually valid under-limit batches cross per-origin normal, 256 MiB team normal, 1 MiB authenticated-node control, 80 MiB team control, and simulated free-space ceilings at exact charged-byte boundaries. Whole-batch rollback leaves event/disposition/audit/index/frontier counts unchanged; relay, signing rotation, workspace/origin churn, reconnect, removal, and member rebinding cannot reset accounting; retries coalesce; another eligible origin and local `ee remember` continue; only safe headers/mandatory controls consume reserve. |
| Trust elevation | At the default 100-event rolling boundary, unique eligible `create` and `revise` events each consume exactly one slot after idempotence; replay consumes zero; the 101st revision makes the resulting current local revision `agent_validated` rather than retaining a prior elevation. Permuted/concurrent batches, restart, forged origin time, and local clock rollback cannot overrun or reopen the window. |
| Rematerialization | Fork/cutoff/policy disposition changes hide invalid material before any rebuild, then converge through restartable generation-fenced batches. Exact stream ordering, complete-set conflict evaluation, canonical projection hashes, crash at every checkpoint, idempotent audit/index/cache-outbox keys, large rollback under the 16-job budget, and post-commit cache-eviction failure are covered. Replaying a prior applied disposition preserves its recorded local trust result without refunding or double-consuming a velocity slot. Reads during rebuild report `mesh_rematerialization_pending`; injected executor/invariant failure reports `mesh_rematerialization_failed`, preserves the fail-closed fence, and follows structured repair. |
| Migration safety | Trust-class table rebuilds (§7.3.2) get a dedicated migration test asserting row counts and content hashes survive each recreate. |
| Determinism | Team-scoped pack/search determinism given equal signed origin events/body corpus, canonical materialized corpus, materializer version, local disposition/admission decisions, maintenance state, config, and indexes (extends the J7 harness). The producer's local shared row and receiver projection use the same neutral temporal multiplier and select the same IDs/order. Vary only diagnostic receipt/sync/local-created timestamps and assert byte-identical selection; vary signed `producedAt` and assert selected IDs, ordering, and neutral default temporal scores stay fixed while rendered provenance changes. Additional local-private rows are an explicitly different corpus. Explicit time filters return cutoff/as-of and assurance. Different maintenance/elevation state is explained policy state, not ranking nondeterminism. |
| Activity chronology and pagination | `--limit` defaults to 100 and rejects zero or values over 1000; the shared `ee.cursor.v1` codec binds normalized filters, explicit as-of, DB generation, and the stable activity position key. Pages partition one generation without gaps/duplicates; invalid, mismatched, and stale cursors return the existing empty-page postures. A future claim enters `clockAnomalies`; a backdated claim may be absent from an explicit time window whose schema says `member_attested`/not sequence-complete, but appears in an unfiltered full cursor drain and origin-sequence audit. |
| Mesh-off regression | `mesh_off_no_network.rs` extended: daemon with mesh off binds nothing; `ee team` commands with mesh off fail with honest guidance, add zero degraded noise elsewhere. |
| Opt-in real-tailnet | `mesh_sync_once_real_tailscale.sh` upgraded to assert a real round; a new `team_join_real_tailscale.sh` (exit 78 skip-clean by default, same contract). |
| Fake IdP harness | Tier 1: fake-tailscale fixtures gain stable node IDs plus `UserProfile` owners (routine key rotation, mismatch, and reassignment scenarios). Tier 2: a local TLS device-flow simulator serving discovery/JWKS/device/token endpoints with rotatable keys — secretless-public-client qualification and client-secret-required rejection, fresh discovery/JWKS per presentation (304 allowed; stale offline key rejected), DNS rebinding, ambient proxy/`.curlrc`/netrc/CA-bundle/TLS-keylog traps, GET redirect validation, credential-POST redirect rejection, inherited-pipe/timeout/reap and output-cap behavior, response redaction, required positive expiry/interval parsing, omitted-interval default 5, no-early-poll timing, cumulative `slow_down`, timeout backoff, provider/local/300-request expiry, cancellation/reap/zeroization and no automatic restart, process loss preserving only outer `identity_pending` and requiring a fresh explicit ceremony, raw-token non-persistence/zeroization, proof that bearer tokens never cross the token-free `identity_attest` session, private-network policy, join, distinct-verifier `identityAttested`, future-dated/overlong/late-delivered/evidence-expired lease rejection, concurrent lease arrival permutations, duplicate issuer/subject conflict, self-renewal rejection, one-member policy bootstrap/zero-grace refusal, no-verifier pending posture, finite-lease expiry with cadence-plus-grace suspension and zero background IdP HTTP, rollback-safe identity time on every authorizing/mutating/import/serve path, read-only identity surfaces leaving the persisted floor unchanged, fail-safe forward jump and non-reactivating floor repair, algorithm confusion, weak RSA/wrong curve, concurrent replay claims, unverified email/malformed group denial, key retirement, and outage grace all run offline. |
| Adversarial OIDC parsing | Duplicate-member discovery/JWKS/header/claims JSON; noncanonical/padded base64url; malformed compact JWT segments; unknown/unsupported `crit`; `jku`/`x5u`/embedded `jwk`/`x5c`; missing/duplicate/ambiguous `kid`; `kty`/`use`/`key_ops`/`alg` mismatch; oversized/deep JSON all fail before signature/claim acceptance or network fetch. Only the exact freshly discovered issuer JWKS raw key parameters are used. |
| OIDC privacy | Tokens containing synthetic unrelated PII and a large group list prove that only subject, explicitly previewed optional email, configured-group matches/decision, and verification evidence persist/replicate. Raw groups/claims, device/user codes, verification URLs, polling state, and bearer tokens are absent from DB, manifest, audit, support bundles, logs, and post-TTL crash state. |
| Property | Fuzz frame/session decode (truncation, oversize, bad MAC, target, counter), origin/payload codecs, the invite parser, and the bounded duplicate-key-rejecting OIDC JSON/JWT decoder. Property tests cover canonical byte/hash stability; checked page-rounded ingress charging and transactional monotonic counters across arbitrary batch partitions/relays/key generations; constant-size export-header ordered-record-root stability; cross-family/workspace/scope MAC separation; reorder/truncation/duplicate/late-mismatch all-or-nothing import; and native reimport idempotence without divergent-revision overwrite or tombstone/withdrawal resurrection. They also cover unique genesis/root binding, no peer-row re-emission, history/live projection idempotence and revision fencing, elevation-cap concurrency/clock rollback, withheld-cursor progress, manifest arrival-order independence including the 20-member and four-node-per-member capacity bounds, and removal-cycle detection under arbitrary event permutations. |
| Perf | `mesh_sync` bench profile; two-node round latency + index amplification budgets (advisory → blocking per the existing maturation path). |

In the integration row, the compact `pair → sync → attribute` label spans two
gates: M1's transport harness fixture-provisions matching keys only through
the production hardened store, while M2/M3 owns and exercises the real
KDF/invite ceremony. It never implies an M1 public raw-key or pairing bypass.

E2E scripts follow the existing `scripts/e2e_overhaul/` + `ee.test_event.v1`
logging conventions; RCH-remote for cargo-backed stages per repo policy.

---

## 11. Milestones and acceptance gates

| Milestone | Contents | Gate (all must hold) |
|---|---|---|
| **M0 — Truth & safety** | P0.1–P0.7 | bd-30o6g closed with streaming-cap test; export/import observably policy-gated; share preview is read-only with no public content/secret hashes or consent-without-effect, and secret findings use unrelated random occurrence IDs; the hardened store authentication root supplies domain-separated import and approval subkeys; `ee mesh grant` requires a short-lived nonce-salted two-layer preview token, consumed in-process or through bounded stdin, and `ee mesh revoke-lane` narrows idempotently without stale-token regrant. Default preview JSON is deterministic/token-free; robot issuance is explicit, marked sensitive, and carries no stable store/key identifier. Invalid versus authentic-stale is implementably distinct; wrong context/key, expiry, concurrent replay, and approval-snapshot drift fail with zero effects and no argv/env/ee-trace/audit/support/CASS-materialization token leak; external-recorder capture is documented as a bounded residual. The import bypass is closed with a constant-size context-bound store-local-MAC header whose ordered-record root is emitted from one read snapshot and recomputed inside one rollback transaction (teammate/cross-family/cross-workspace artifacts cannot inject `human_explicit`, and native reimport cannot overwrite divergent state or resurrect a tombstone/withdrawal); zero uncovered mesh degraded codes; effect/README drift fixed; `verify.sh` green. |
| **M1 — Peers talk** | P1.0–P1.5 (incl. P1.3b) | Canonically encoded, strictly verified signed origin events durably record only origin-owned local mutations; peer material never re-emits and first-event projection is valid; T2.4 waits for T4.1's active-member authorizer before advertising/applying/relaying either base team feature, so pair/session/signature proof alone is never team authority; cumulative normal intake is charged to signed origins while control intake is independently capped by non-recycled authenticated node (1 MiB) and team (80 MiB), so origin/workspace/key/member churn cannot amplify reserve or block local source truth; dead key-identity frame v1 is rejected and frame v2 binds random ee nodes/team/endpoint workspaces/session/direction/exact-next counter under directional keys; hardened key storage passes Unix mode/no-link/fsync and Windows DACL/reparse/write-through parity or blocks team credentials with `mesh_key_store_unavailable`; local-target/replay-safe sessions, accepted-source WhoIs stable-ID binding, opaque peer handles, and legacy upgrade guidance hold; one user-scoped responder multiplexes exact validated routes for multiple local workspaces only when their root/local ports agree, rebinds only verified tailnet-address changes, diagnoses host-wide/other-EUID ownership, and never scans/wildcard-falls back; distinct-source loopback with fixture-preprovisioned hardened pair keys proves real production session traffic, partition/fork/flood/withheld-cursor/stable-key-rotation scenarios, three-node authorized signed relay, and valid-origin equivocation rollback/convergence; no public raw-key import/test backdoor exists, and an unkeyed peer receives structured pairing-required guidance until M2/M3 ceremonies land; responder policy + secret scan hold; `ee mesh sync --once` runs a real round for a keyed peer; frame/bootstrap/origin/storage-accounting fuzz/properties pass; mesh-off binds no sockets; real-tailnet smoke and capability probe graduate. |
| **M2 — People & projects** | §7.3.1–§7.3.3 | Random ee node IDs + pinned tailnet/stable-node identity survive ordinary current-key rotation and reject stable-ID substitution; `peer_human_attested` migration/enums/weights are consistent; the atomic default-100-per-rolling-24h local-admission cap survives forged origin time, clock rollback, and concurrent batches, counts each unique content `create`/`revise` exactly once after idempotence, and makes an over-cap revision `agent_validated` instead of inheriting stale elevation; explicit `ee mesh rotate-pair` converges through the pinned two-phase/control-only-resume protocol without old-key application fallback or clock-extended grace, while `ee team members rotate-key` remains the distinct dual-signed/hash-linked signing-lineage operation; Git SHA-1/SHA-256/multi-root/non-Git keys derive/adopt; shallow roots never masquerade as complete, replacement refs/grafts/lazy fetch/ambient Git state cannot rewrite identity, multiple raw local `origin` URLs remain ambiguous rather than selecting a winner, and no root-set/history/object-format/remote drift silently rekeys a persisted project—explicit reconcile passes. |
| **M3 — ee team** | §7.4 all (history sharing only; no body verbs) | US-1/2/3/7/8/9 pass E2E; plain invite always leaves a live redemption path, daemon/default/wait/no-wait/resume behavior is explicit, the root/invite commit the one v1 port, invite/ceremony IDs meet their entropy floor, the inviter's signed nonce challenge proves exact ee identity/root/port before the secret is transmitted, and persisted-floor + monotonic invite/introduction expiry cannot be extended by clock rollback/restart; join is stable-node-bound, crash-resumable/idempotent with safe pre-key secret re-entry, exiting 0 only after first sync; post-genesis node binding requires an existing-node/new-node ceremony, revocation is cutoff-bound, and lost-last-node recovery cannot self-authorize; the complete active set never silently exceeds 20, no member silently exceeds four active nodes, and both overflow classes converge on capacity conflicts without granting arrival/hash winners; pre-team history stays local unless the revision-pinned history-share flow is confirmed, whose consent explicitly covers current and future active members until origin-wide withdrawal; a later joiner receives confirmed non-withdrawn history; member removal is preview-hash pinned and any target/node/cutoff/delegation drift fails with zero side effects; signed cutoffs are arrival-independent, omitted target origins default to no retained authority, mutually invalidating removals conflict without orphaning the team, accepted-prefix delegated members remain honestly active with mandatory review posture, relay/ack posture is proven, and no recipient removal emits `shareWithdraw`; every leaf emits schema-valid JSON, mutating/network leaves audit, and read-only/list/preview leaves produce zero durable side effects; pause-generation fencing blocks planted live sessions and resume rejects them; invite replay/TTL/concurrent redemption fail closed. |
| **M4 — Unified recall** | §7.5 all | US-4 passes: team-scoped search/pack with receiver-derived attribution on both nodes; null/spoofed member fields cannot become local or impersonate; all team candidates use the same neutral temporal default; activity exposes only the closed metadata allowlist and never body-derived titles. `trust.team_members` is removed rather than retained as unauthenticated human-team compatibility. Member-attested origin time remains provenance-only; explicit-as-of activity labels skew/backdating limits and generation-stable pagination recovers admitted events. `ee pack --memory-scope` ships; overlap/contradiction behavior is tested. **T5.9 atomically ships body transport plus `ee team share bodies`/`unshare bodies` schemas, authenticated preview-token/grant/revoke/audit behavior**; no degraded-success stub exists. Default preview JSON is deterministic/token-free; explicit robot issuance adds a marked-sensitive, no-stable-ID `eeap1_` bearer. The two-layer, fresh-nonce, 15-minute token distinguishes invalid from stale, is generation-CAS single-use, and travels only in-process or over bounded stdin; every approval-snapshot drift requires a separate fresh preview with zero effects, while errors/persistence expose neither replacement token nor sample/body hashes. Ee sinks and CASS materialization redact it; operator copy names external-recorder capture until expiry. Fresh-nonce signed commitments prevent metadata guessing/linkability, authorized transfer recomputes the commitment before publish, revoke makes no remote-erasure/source-wide claim, cross-platform cache parity or metadata-only failure is proven, and support-bundle/withdrawal boundaries pass. |
| **M5 — Operations** | §7.6 all | Background steward syncs on the harness without CLI involvement (US-5); `ee daemon install` works on macOS + Linux; doctor validates root/local port agreement, one host-wide responder owner, client-only posture, routes/keys/identity, and cross-platform body-cache security; admission wired; perf profile recorded; quickstart doc validates custom-port and multi-user limitations in a cold run-through. |
| **M6 — SSO identity** | §7.3.4 both tiers | Tier 1 owner mismatch suspends + audits. Tier 2 accepts only capability-compatible secretless public device clients and rejects providers requiring a distributed client secret; the distinct verifier hosts the flow, receives and zeroizes bearer tokens locally, and sends only bounded ceremony metadata/status over version-negotiated `identity_attest`. Fresh discovery/JWKS per presentation, minimal-environment constrained curl, duplicate-key-rejecting bounded JSON/JWT parsing, exact-original-input signature verification, issuer-JWKS-only key selection, and strict token verification reject stale retired keys, token-controlled/embedded keys, ambiguous `kid`, unsupported critical headers/noncanonical encoding, unsupported providers, DNS-rebinding/private-address/ambient-proxy/config/CA/keylog/redirect leaks, credential-POST redirects, unbounded or unreaped subprocess I/O, weak or mismatched keys, bad issuer/audience/`azp`/algorithm/time/verified-email/group claims, and replay races. Durable/team-visible evidence is limited to the previewed subject/optional-email/configured-group-match decision and verification provenance; full groups, unrelated claims, tokens, and ceremony ephemera never persist or replicate. Receiver-side future-skew, cadence, evidence-expiry, and late-delivery checks bound every lease. Nondecreasing local authorization time makes authorizing/mutating/import/serve paths rollback-safe while read-only surfaces remain non-persisting; forward-jump repair cannot reactivate old leases. Nonce extension is used when present and weaker fallback is explicit; finite attestation leases reject self-renewal, commute under concurrent arrival, conflict duplicate issuer/subject bindings, bootstrap without instant one-member lockout, expire through cadence + grace without background IdP HTTP, and make the offboarding latency honest. |

M0 → M1 → M2 → M3 → M4 → M5 is the spine; §12 marks the safe parallelism
inside each. Exception (2026-07-30 hardening): the M1 gate's
authorized-relay and P1.5 E2E items are gated on T4.1 (manifest
authorization), which is pulled forward together with its T3.1/T3.2/T3.6
prerequisites — pre-gate M1 transport work ships with the base team features
registered but never advertised, and M1 cannot fully close before that
pulled-forward slice lands. M6 tier 1 can start alongside M3 (it needs
member records + join, not retrieval); M6 tier 2 is sequenced last (its
crate additions land when it starts, per §13 item 1).

---

## 12. Work breakdown (bead-conversion source)

Legend: `←` = depends on. IDs are placeholders resolved at bead-creation time.
Every bead below gets: full context paragraph, file anchors, acceptance
criteria, test obligations, and the AGENTS.md contract checklist inline.

**EPIC T0 — Team confederation program** (umbrella; children below)

**T0.0 ADR 0086** — write the ADR (decisions D-team-1…n from §7, rejected
alternatives, verification hooks). ← nothing. Blocks everything else.

**T0.1 Ed25519-in-v1 operator ratification gate — COMPLETE 2026-07-30
(`bd-tc-epic-qzk7o.9`).** The operator explicitly accepted plan §13 item 1b,
authorizing the pinned `ed25519-dalek`/`zeroize` dependency profiles and the
signature/relay architecture across TC-D2–D5/D9/D10. The closed gate remains
an explicit dependency of T2.0 and T4.2 so the authorization trail is
machine-visible. ← T0.0. Blocks T2.0 and T4.2.

**Sub-epic T1 — M0 Truth & safety**
- T1.1 Streaming byte-cap fix — **COMPLETE 2026-07-30**
  (`bd-tc-epic-qzk7o.2.6` and absorbed `bd-30o6g`; implementation
  `63514470`, lint follow-up `40a1c0c8`, handoff audit `fbcc6252`). ← T0.0
- T1.2 Wire outbound policy into `ee mesh export` + share-preview verdicts
  (P0.2+P0.3); remove `--record-consent`, public per-content/aggregate
  preview hashes, and the secret scan's public `valueHash`; emit only
  revision-pinned samples and a fresh random per-scan `findingId` unrelated to
  secret bytes. Preserve a deterministic ID-free pure detector, then decorate
  canonical findings at the effectful boundary with injected secure
  randomness; failures are errors and v2 schemas replace v1 directly. ← T0.0
- T1.3 Wire `decide_mesh_import` into `ee mesh import` + ledger decision
  columns, and expose a versioned normalized admission request that live typed
  events can reuse without turning an unsigned `ee.mesh.event.v1` file row
  into team origin authority. ← T0.0
- T1.4 DB-backed preview-grant retargeted from raw node key to opaque enrolled
  peer ID + T1.6-keyed canonical approval snapshot and short-lived,
  nonce-salted snapshot-tag/envelope-MAC token; human apply keeps it
  in-process. Default JSON is deterministic/token-free; robot issuance is
  explicit via `--issue-approval-token`, marks the no-stable-ID `eeap1_`
  bearer sensitive, and `ee mesh grant <peer-id>` consumes it from bounded
  `--preview-token-stdin`, never argv/env. Ee sinks/CASS materialization redact
  it and operator copy names the external-recorder residual. Add idempotent
  generation-advancing `ee mesh revoke-lane <peer-id>` (absorbs bd-2gvgw);
  peer ID is lookup only. M0 pins a versioned target adapter; T2.2/T3.1—not
  this bead—later migrate/resolve it to the exact ee-node grant
  principal/generation. Invalid versus authentic-stale/expired is distinct;
  verification + generation CAS + grant/audit share one transaction, so
  wrong-context/key, drift, and concurrent replay commit nothing. Stale
  previews cannot undo a revoke and neither command claims remote erasure.
  ← T1.2, T1.6
- T1.5 De-hardcode mesh status/report fields. ← T1.2, T1.3
- T1.6 Hardened store-local authentication root + close JSONL/playbook trust
  bypass: fallible fixed-domain subkeys and known-answer/key-rotation
  lifecycle; constant-size versioned store-local keyed-MAC header binding
  artifact family/schema, record encoding, source
  key namespace, exact workspace/scope, key ID/count, and a
  domain-/record-type-separated ordered-record root, computed and emitted from
  one consistent read snapshot; importer recomputes root/count while applying
  inside one rollback-capable transaction and commits native trust only on an
  exact contextual match; native reimport restores missing rows or no-ops on
  byte-identical rows but never overwrites divergent revisions or resurrects
  tombstones/withdrawals; credential-free `ee backup` restore is external and
  requires local re-attestation. Approval surfaces derive separate
  snapshot-tag, envelope-MAC, and audit-ID subkeys; tokens accept the current
  key only and serialize no key ID, while import verification alone retains
  the bounded prior-key window. ← T0.0
- T1.7 Degraded-code fixture/taxonomy backfill + empty audit tests + effect/README drift. ← T0.0 (parallel with all T1.x)

**Sub-epic T2 — M1 Transport**
- T2.0 Typed signed origin stream: `ee.mesh.origin_event.v1` + memory/manifest payload schemas, explicit outer operation, canonical byte/hash vectors, deterministic full-digest `eventId` recomputation, exact `ed25519-dalek`/`zeroize` contract entries, fallible zeroizing key generation, strict signing/verification logic, `mesh_origin_events`, origin-owned-only same-transaction append (no inbound echo/in-place peer edits), per-team/per-memory projection marker with valid first-event behavior, and a closed memory-metadata allowlist excluding body/title/preview/tags/URI/path/evidence/nonce fields. Content revisions atomically store a private fresh commitment nonce and sign the salted `bodyCommitment` plus exact/already-redacted representation and bounded redaction provenance. Add contiguous verified-receipt/disposition-scan frontiers plus sparse per-event applied/withheld/quarantined/unsupported dispositions whose applied form pins local policy generation/admission result for replay; every envelope requires `mesh.origin_event.v1`, memory requires `mesh.team.memory.v1`, manifest requires `mesh.team.manifest.v1`, and `identityAttested` additionally requires `mesh.team.identity_attested.v1`; receiver-derived mandatory feature/auth checks reject omission independently of the bounded canonical origin list, while unknown extras stay replayable unsupported; register but do not advertise the base team features until T4.1's authorizer gate. ← T2.1, T0.1
- T2.1 Frame-transport session layer: supersede/reject dead `ee.mesh.tailscale_transport_frame.v1` before production with v2 fields for random ee source/target node IDs, team, endpoint workspaces, session, direction, monotonic counter, request correlation, capability, budget, and payload hash under a canonical directional-session MAC; rotating Tailscale keys are handshake observations, never frame identity. Run length-prefixed frames over cancel-aware `asupersync::net` TCP; authenticated fresh-nonce handshake binds the initiator-selected local workspace to exactly one registered responder target workspace and both pinned tailnet/Tailscale-stable-node IDs; require the exact next per-direction counter, reject duplicate/skipped/regressed counters and v1/wrong endpoint/workspace/team/session/direction/request/origin-as-target confusion, and put retries on fresh frames with stable application idempotency keys; `Cx` budgets/deadlines/cancellation; bounded unsigned bootstrap; source-IP/global pre-auth caps; hardened pair/signing-key storage with Unix mode/no-link/fsync and Windows SID/DACL/reparse/opened-identity/write-through parity through a reviewed safe adapter—otherwise high `mesh_key_store_unavailable` blocks team credentials, with no shell permission repair or project-owned unsafe; expose that narrow secure-local-file primitive for T5.9 cache publication rather than duplicating platform logic; version-negotiated capability extension points used by M2's ≤4 KiB `pair_rotate` and M6's ≤8 KiB token-free `identity_attest`. ← T0.0
- T2.2 Single-owner user-scoped responder broker: daemon normally owns verified-tailnet-only listener; foreground waiter lease/control-channel delegation and startup-race arbitration; exact `(team_id, target_workspace_id)` multi-workspace route registry with same-EUID bounded UDS registration, owner-safe DB/path checks, and an opaque team/genesis/committed-port revalidation interface (T4.1 plugs in real manifest roots; T4.2 plugs in invites); all routes behind one owner must agree on one port, another OS user/process is a diagnosed host-wide conflict, clients never scan/fallback, and mismatched local team posture is client-only; startup/network-map changes revalidate the complete local tailnet-address set, close on loss, drop stale sockets before verified rebind, and never use wildcard/stale-hint fallback; `origin_workspace_id` remains producer provenance and never selects a route; LocalAPI WhoIs accepted-source stable identity with current-key observation; migrate `mesh_peers` so `peer_id` is lookup-only, store random ee node + pinned stable ID + current observation/generation, stop new key-derived identity, block unverifiable legacy rows with `mesh_peer_identity_upgrade_required`, and consume T1.4's versioned grant-target adapter so exact-node grants/revokes survive same-stable-ID current-key rotation but never transfer to a new node; reject spoofed headers/source/stable-ID mismatch, missing stable identity, network paths, and stale or ambiguous routes; per-route pause/zero-route binding semantics; real status/discovery cache. ← T2.1, T1.4
- T2.3 Real client hello probe replacing ACL-capability synthesis, exercised
  against the actual responder rather than a duplicate test listener; uses
  configured generic or root-committed team port and never hard-codes
  41888/scans/falls back. ← T2.1, T2.2
- T2.4 Bidirectional anti-entropy round executor: authenticate the exact
  initiator/responder endpoint-workspace pair independently of event origins;
  both endpoints advertise tips, plan missing ranges, and import replies into
  only their bound local DB; verify signed origin/hash chains and idempotent
  relay; detect same-sequence/tip equivocation, retain/relay both proofs,
  de-materialize to the common prefix and fork-block the origin with no
  arrival winner; advance contiguous receipt/disposition-scan frontiers and
  materialize only explicit applied dispositions; transactionally charge
  page-rounded cumulative inbound event/disposition/proof bytes: normal
  intake to the signed origin across relays/key rotations, mandatory controls
  to the non-recycled authenticated ee node lineage with 1 MiB/node and
  80 MiB/team caps unaffected by workspace/origin/signing/member churn;
  enforce the pinned normal/team/free-space bounds, coalesce denial posture, roll back
  the whole batch on breach, and never charge/block local source truth. ←
  T2.0, T2.2, T2.3, T2.8, T4.1, T1.1, T1.3
- T2.4b Shared per-session serving core for responder and initiator roles: dormant for team capabilities until T4.1's active-member authorizer; thereafter safe signed headers always contiguous for authorized origins/recipients, policy/secret scan the closed metadata-only `create`/`revise` payload allowlist, audit withheld content, serve mandatory minimal tombstone/shareWithdraw and manifest controls, and clip budgets at complete headers. Body/nonce bytes never enter this lane. ← T2.0, T2.2, T4.1, T1.2
- T2.5 Loopback E2E: distinct bound 127/8 source identities with fake-WhoIs
  stable IDs and rotating current-key observations; test setup provisions
  matching pair keys only through the production hardened key-store API, with
  no public raw-key import/bypass, establishes valid T4.1-signed
  genesis/member/node state rather than a test-only authorizer bypass, while
  an unkeyed peer gets structured pairing-required guidance; the
  pre-authorizer feature matrix proves neither base team feature can
  apply/relay, and missing cross-origin predecessors quarantine/replay after
  authorization; one outbound connection exchanges both
  directions without an initiator listener; same-stable-ID key rotation
  preserves authenticated continuity while stable-ID substitution fails; two
  client-only nodes cannot connect; removal fanout uses tip/range exchange
  with no event-push capability; two-node
  partition/fork/exact-next-counter/application-idempotency/endpoint/
  withheld-cursor/flood/control-reserve-amplification/secret/no-re-emission
  scenarios plus responder-ownership races; two registered local targets
  reject cross-route and origin-as-target confusion while accepting relayed
  foreign-origin events into the selected target; three-node valid
  relay/forged-relay and valid-origin equivocation convergence/rollback. ←
  T2.4, T2.4b
- T2.6 Real-tailnet opt-in smoke upgrade + capability graduation (`probe_mesh_capability`). ← T2.5
- T2.7 Frame/session/bootstrap/origin-payload fuzz + properties (truncation, oversize, bad MAC/signature, wrong target, duplicate/skipped/regressed counters, canonical hash/commitment stability, closed metadata allowlist, withheld-cursor progress). ← T2.0 (invite parser fuzz belongs to T4.2)
- T2.8 Deterministic rematerialization core: pure versioned reducer over signed
  ledger + payloads + durable dispositions/admission decisions; exact
  workspace/node-byte, numeric-key-generation, numeric-sequence traversal;
  complete-set/fixed-point cross-origin conflict evaluation; canonical
  projection hash; transactionally hide invalid material before bounded,
  generation-fenced, crash-resumable execution; deterministic audit/index/
  cache-outbox idempotency, `invalidated_pending_purge` retrieval/index fence
  before post-commit removal, and 16-job amplification budget; pending/failed
  degraded emitters, fixtures, taxonomy, status, and doctor recovery. Own
  current memory/index integration and versioned extension contract; T3.4
  adds the attested-trust arm and T5.9 adds body-cache integration. ← T2.0

**Sub-epic T3 — M2 Identity**
- T3.1 `team_members`/`team_member_nodes`/pending-invites migrations + models: random ee node IDs; pinned tailnet/non-empty Tailscale stable node IDs; separately audited current rotating-key observations; ee signing lineages; no operator-manual proof bypass; map opaque mesh peer handles to exact ee-node/grant generations so grant consent survives same-stable-ID key rotation without transferring to another node. ← T2.2
- T3.2 Pair/session ceremony derivation + two-generation rotation (canonical
  role/transcript/team/invite or introduction, both ee node IDs, both stable
  node IDs, current observed keys, signing identities, and fresh nonces
  bound; mutual key confirmation; storage in T2.1). Rotation is a durable
  two-phase expected-generation state machine: local-only atomic promotion,
  old-key `pair_rotate` control messages for exactly 86400 seconds, no
  old-key application session or automatic downgrade, persisted time-floor
  rollback rejection, crash convergence, and
  `mesh_pair_rotation_repair_required` + fresh pairing after expiry. Ship
  explicit `ee mesh rotate-pair <peer-id>` plus
  `ee.mesh.pair_rotation.v1`/`ee.mesh.rotate_pair.v1`; claim no automatic v1
  cadence. ← T3.1, T2.1
- T3.3 Trust class `peer_human_attested`: recreate-style migrations at every CHECK site + migration-safety test + three enums + weights + verified-scope + goldens. ← T0.0
- T3.4 Elevation rule at import (single signed-origin path, policy toggle,
  default 100 unique content events/member/rolling 24h atomic persistent
  local-admission cap independent of origin time, clock-rollback high-water,
  canonical/concurrent batch handling keyed by receiver-derived producer
  member rather than payload attribution; every eligible `create`/`revise`
  consumes once after event-id idempotence, replay consumes zero, and an
  over-cap revision becomes `agent_validated` rather than inheriting old
  elevation; burst code/status counts). ← T3.3, T3.1, T2.4
- T3.5 Project identity: invoke canonical Git directly through the bounded/reaped
  runner with a minimal cleared environment; reject ambient `GIT_*`
  repository/object/config/prompt/trace controls and run with
  `--no-replace-objects --no-lazy-fetch --no-optional-locks`; reject nonempty
  common-dir grafts. Capability-probe those safety options and never retry a
  weaker root command; unsupported Git emits `git_unavailable` with upgrade
  guidance and can use only the safe fallback/mint path. Require non-shallow
  history before root derivation;
  validate/sort the complete root set for
  `git rev-parse --show-object-format`; length-prefix object format + full
  IDs. For fallback, read exactly one distinct usable raw local `origin` URL
  with includes/system/global config disabled, reject secret-bearing or
  control-byte URLs, and never choose through rewrite rules, multiple values,
  or remote iteration. Mint/adopt when Git has no safe origin or is non-Git;
  persist full derivation evidence/aliases/override; no silent rekey after
  unshallow, root-set addition, history rewrite, object-format conversion, or
  origin rename; structured hello bindings and policy matching. ← T3.1
- T3.6 Origin-signature/key-lifecycle hardening: exact feature/dependency audit, `verify_strict`, dual-signed/hash-linked signing-key rotation, staged-next routine recovery, explicit other-member revocation/fresh-consent compromise recovery, plus a versioned pure bounded-transition-chain/invalidation decision contract for later invite consumers; relay and key-generation security review. M2 does not wait for pending-invite tables: T4.2 consumes the chain verifier and T4.2/T4.4 own live invite invalidation. ← T2.0, T3.2

**Sub-epic T4 — M3 Team UX**
- T4.1 Typed manifest materializer + authorization: unique self-signed
  `teamCreated` genesis/root, including immutable v1 `hello_port`, wired into
  T2.2 broker route/port validation; hard complete-set
  `MAX_ACTIVE_TEAM_MEMBERS = 20` and predecessor-rooted
  `MAX_ACTIVE_NODES_PER_TEAM_MEMBER = 4` with arrival/hash-independent
  capacity conflicts; asymmetric node authorization (`nodeBound` = existing
  active node + fresh new-node ceremony + exact predecessor node-set root,
  `nodeRevoked` = self or any active member with cutoff) and same-stable-ID
  rotating-key observation updates that mint no node/grant; signing-key
  transition operation; schema-pinned future-gated `identityAttested`
  operation with distinct-verifier authorization, receiver-enforced
  future-skew/policy-cadence/evidence-expiry bounds, commuting lease-set
  materialization and duplicate issuer/subject conflicts; monotonic
  member/node removals with per-origin cutoffs whose omitted target origins
  default before their first event, deterministic fixed-point
  de-materialization, mutual-removal cycle conflicts, predecessor conflicts,
  lane/local-consent intersection, persisted accepted IdP-policy
  floor/pending-relaxation posture, acknowledgement state, local cache;
  install the active-member/node/key authorizer before advertising
  `mesh.team.memory.v1` + `mesh.team.manifest.v1`; accept only the unique
  self-authenticating `teamCreated` before a valid genesis, require that
  genesis for every other operation, quarantine/replay missing cross-origin
  predecessors, and retain the additional
  `mesh.team.identity_attested.v1` gate; plus signed `ee team create`. ←
  T2.0, T2.2, T2.8, T3.1, T3.6
- T4.2 Invite mint/parse/revoke (`eeteam1-`, exact inviter tailnet + non-empty Tailscale stable node ID + ee node/signing generation/fingerprint, root-committed `hello_port`, current key/address observations only, ≥128-bit random invite/ceremony IDs, 256-bit zeroizing secret, genesis hash + root fingerprint, TTL, leased single redemption) + persisted nondecreasing invite-authorization floor and same-process monotonic deadlines across mint/lease/redeem/resume/revoke/expiry, with high rollback block and revoke-all-before-floor-repair; fresh status-map resolution accepts current-key rotation only inside the same stable binding; before secret release, invite-ID/nonce request and bounded inviter Ed25519 challenge bind protocol/team/root/nonces/ee+stable identity/port and must verify exactly; if the inviter signing key rotated, require a contiguous TC-D5 dual-signed transition chain from the invite-pinned generation within the bootstrap budget, while revocation/fork/compromise invalidates pending invites; secret-free nonce-correlated broker registration/lease; plain invite waits in-process when no daemon, registers and may return when a live daemon confirms the route, `--wait` always correlates, `--no-wait` requires that confirmation, interruption prints secret-free `--wait --resume`; no-echo TTY/`--invite-stdin` ingestion only; `--for` display-only bearer residual; invite parser fuzz/properties, locator/port/identity/rotation/wrong-process/time-rollback tests, and argv/env/log/audit/error leak tests. ← T4.1, T3.1, T2.1, T2.2, T0.1
- T4.3 Crash-resumable stable-node-bound join state machine (no-echo/stdin secret ingestion and required pre-key re-entry without persistence, bootstrap, mutual pair/signing-key confirmation, optional identity gate, signed manifest, rollback-safe TTL introductions carrying stable-node/ee/signing identities plus current transport observations, deferred direct pairings, consent, first sync; secret-free/local `--dry-run`, never a redemption or metadata oracle). ← T4.1, T4.2, T3.2, T2.4, T2.4b
- T4.4 Membership ops (members list/show/trust/rotate-key/reconcile/add-node/remove/leave; `members rotate-key` is explicitly signing-lineage-only; existing-node/new-node direct binding, arrival-independent exact-node revocation, lost-last-node recovery requiring fresh consent/new member when continuity is unavailable; member/node revoke and fork-block consume T3.6's invalidation contract for affected pending invites; removal atomically appends the event + advances durable session/grant authorization generations, while per-frame checks fence post-commit serving and in-memory connection cancellation is idempotent afterward; member removal uses a root/generation/node/cutoff/delegation/ack-bound preview hash, zero-side-effect stale rejection, fanout/relay/acks; accepted-prefix additions remain honestly active with `addedByRemovedMember` + `team_delegated_member_review_required` until locally acknowledged or separately removed; **no automatic shareWithdraw**). ← T4.3, T3.4
- T4.5 History sharing only: revision-pinned/resumable `ee team share history` for origin-owned pre-team metadata with per-item revalidation and projection-race idempotence; consent enumerates the closed metadata disclosure fields and explicitly covers current and future active members until origin-wide withdrawal. M3 ships no body verb or unavailable success variant. ← T4.3, T2.0
- T4.6 Posture ops (status/sync/pause/resume/audit; unpaired, conflicts,
  removal acknowledgements, delegated-member review, pair-rotation repair,
  history projection, rematerialization generation/fence, staleness,
  elevation counts, responder posture); durable pause-generation barrier
  checked at round/frame/import/serve boundaries and validated-generation
  resume; side-effect registry and tests keep status/audit/list/preview
  read-only while sync/pause/resume audit their mutations. M3 reports only
  the existing mechanism-cache posture and reserves a versioned extension
  point for T5.9; it does not claim body-cache lifecycle or body-lane success
  before M4. ← T4.3, T4.4, T4.5
- T4.7 `ee team` E2E over the harness: listener-owner races and two-workspace one-broker routing isolation; other-EUID/unrelated owner, cross-team committed-port mismatch, and local client-only posture; genesis/root/port mismatch and missing/duplicate/conflicting genesis; default/wait/no-wait/resume invite behavior, ≥128-bit invite/ceremony ID vectors, generic unknown-ID decline, pre-secret signed challenge/wrong-process rejection, same-stable-ID key rotation, stable-ID substitution, join crash/retry/pre-key re-entry/concurrent redemption/bearer warning, invite/introduction clock rollback + restart + revoke-all floor repair; future-only activation and history preview/change/race/resume; later-member receipt of confirmed non-withdrawn history; node-bind/revocation/lost-last-node recovery; membership-cap arrival permutations; removal cutoff arrival permutations + mutual-removal cycle + offline-remover relay/ack + accepted-prefix delegated-member review that never masquerades as revocation; durable pause-generation fencing and validated resume; read-only leaves make zero durable changes while mutating/network leaves audit; ordinary independent memory streams converge, manifest and capacity conflicts remain explicitly blocked. ← T4.3..T4.6, T4.8, T2.5
- T4.8 `ee team projects share|adopt|list` (US-9: minted project ids for non-git workspaces, adoption mapping, manifest registry). ← T3.5, T4.1

**Sub-epic T5 — M4 Unified retrieval**
- T5.1 `--memory-scope`/`--strict-scope` on `ee pack`/`pack build` + docs-drift fix. ← T0.0 (parallel-safe early)
- T5.2 Team scope over explicit local-origin ownership plus inbound projections whose `producerMemberId`/project are receiver-derived from the verified node/key/manifest authorization position; spoofed/missing/ambiguous/null attribution quarantines. Remove the undocumented `trust.team_members` nickname list directly with migration/CHANGELOG; `self`/`swarm` remain the agent scopes. ← T3.1, T5.1, T2.4
- T5.3 Attribution rendering (search/pack/ask/why `teamProvenance`, markdown suffix, elevation explanation); origin `producedAt` labeled member-attested provenance only and excluded from authorization, elevation, default relevance, retention/decay, and lifecycle mutation; every team-scope candidate, including a producer's local shared row, uses one neutral default temporal multiplier, explicit time filters return cutoff/as-of + assurance, and local created/first-receipt clocks stay separate. ← T5.2, T3.4
- T5.4 `ee team activity` over only the closed project/kind/level/member/origin-time/body-availability metadata allowlist (never body-derived title/preview); the schema and human copy describe captured/shared memory events, not command execution or a complete CASS session log. Optional absolute JSON cutoff + required `--as-of`; relative human cutoff resolves visibly; default-100/max-1000 shared `ee.cursor.v1` pagination; stable `(producedAt DESC,eventId ASC)` ordinary ordering; claims later than as-of + 600 seconds move to deterministic `clockAnomalies`; time-window output labels member-attested/backdating incompleteness, while unfiltered cursor drain + origin-sequence audit remain complete. ← T4.1, T5.2
- T5.5 Overlap precedence constant (local>team>global), contradiction
  surfacing, bd-1bfwa coordination, and tests. ← T5.2
- T5.6 Evidence-bounded peer conflict detector (SRR6.37 completion): deterministic duplicate/near-duplicate, typed/canonical-field contradiction only, free-text suspicion as review candidate, missing-body unassessed posture, detector provenance + insights surfacing through T5.5's shared precedence/conflict surface. ← T5.5
- T5.7 Index-intake integration: transactionally coalesced workspace/source/round-range jobs with idempotency and source-snapshot publication fence; amplification budget verification at team scale. Reuse the one source-snapshot protocol owned by `bd-d67os.28`; that bead must close with source-based verification before this task can claim the fence is safe. ← T2.4, bd-d67os.28
- T5.8 Team retrieval determinism: with canonical event/body corpus and maintenance state fixed, producer-local shared and receiver-projected rows select the same IDs/order under neutral team temporal scores; varying local created/diagnostic receipt/sync times leaves selection unchanged; varying signed origin time changes rendered provenance bytes but not selection/scores; explicit time filters and activity anomaly output stay assurance-labeled and explicit-as-of deterministic; local first receipt may independently alter later maintenance state. ← T5.3, T5.4
- T5.9 Body product slice: atomically ship `ee team share
  bodies`/`unshare bodies` schemas, authenticated preview tokens,
  receiver-resolved current-node grant/revoke mutations, audits, and
  transport—no unavailable success variant. The T1.6-domain-keyed canonical
  approval snapshot binds team root/materializer generation, source
  workspace/node, lane/future-serving semantics, exact recipient nodes and
  grant generations, outbound policy/scanner generation, complete candidate
  ID/revision/representation/commitment set, sample strategy/limit, exact
  ordered redacted samples, cautions, and schema/copy version. Human/JSON
  render from it. Default JSON is deterministic/token-free; explicit robot
  issuance adds a marked-sensitive, no-stable-ID `eeap1_` bearer. Use TC-D6's
  15-minute fresh-nonce snapshot-tag/envelope-MAC token: keep it in-process
  for human confirmation, consume bounded stdin for robot apply, distinguish
  invalid context/key/MAC from authentic stale/expired, and verify snapshot +
  generation CAS + grant/audit in one transaction. Drift/replay requires a
  separate fresh preview and has zero grant/audit/outbox/fetch/cache effects;
  errors contain no replacement bearer. Token/tag/sample/body hashes never
  persist or leak through argv/env/ee-controlled trace/support/CASS
  materialization; external capture until expiry remains documented. Fetch
  authorization derives requester member from the authenticated session and
  binds exact event/project/grant to the owning origin workspace/node.
  Event-signed fresh-nonce `bodyCommitment`, `exact`/`already_redacted`
  representation, and redaction provenance permit no in-flight transform,
  metadata guess/link oracle, relayer-cache authority, or missing-revision
  substitution; release nonce only after authorization and recompute
  commitment before publication. Use sequenced transfer integrity, aggregate
  cap, sync/prefetch-only execution; crash-safe cache state machine where
  verified private publication/fsync precedes `staging→available`, while
  retrieval/index invalidation precedes
  `available→invalidated_pending_purge→purged|evicted` physical removal, with
  startup/steward/doctor reconciliation and filesystem presence never
  authoritative; Unix 0700/0600/no-link/fsync plus Windows
  SID/DACL/reparse/opened-identity/write-through parity through T2.1's
  secure-file primitive, with high `mesh_body_cache_lifecycle_failed`
  metadata-only behavior on any proof failure; lifecycle/withdrawal
  consistency, support-bundle exclusion, missing-body posture, bounded retry,
  later-node non-inheritance, and honest
  future-serving-only/source-specific/non-erasure semantics. ← T2.4, T2.4b,
  T1.1, T1.4, T4.3
- T5.10 US-6 E2E over the product harness: body preview/token/grant ⇒ sync
  fetch/cache ⇒ indexed retrieval; default preview is
  deterministic/token-free, explicit issuance is marked sensitive, and the
  envelope has no stable identifier. Malformed/wrong
  store/workspace/surface/key/MAC tokens, future/expired tokens, concurrent
  replay, and independent drift of every canonical approval field require a
  separate fresh preview and fail with zero effects, while errors contain no
  replacement bearer, equal previews are unlinkable, and
  token/tag/sample/body hashes stay out of argv/env/ee-controlled
  trace/audit/support/CASS materialization. External-recorder capture is
  documented and TTL-bounded. Human in-process and robot bounded-stdin
  workflows both pass. Exact and already-redacted signed representations pass
  while redact-over-exact, transformed-commitment masquerade, later scanner
  denial, relayer-cache serve, and old-revision substitution stay
  metadata-only; metadata-only peers cannot guess or link equal bodies;
  authenticated-node/member mismatch fails before nonce release; ordinary
  deny performs no fetch/read blocking; revoke/unshare ⇒ no future fetch from
  the named local source while prior copied bytes are explicitly non-erasable,
  later nodes do not inherit, and other local source nodes are explicitly
  unaffected; partial-publication attempts fail; crash injection at every
  staging/publication/invalidation/purge transition proves no
  pre-verification visibility, no post-invalidation access, and idempotent
  reconciliation; Unix/Windows cache-security failure publishes nothing and
  remains metadata-only; withdrawal purges only derived state and support
  bundles leak no body/path/nonce. ← T5.9, T5.7, T4.7, T2.5

**Sub-epic T6 — M5 Operations**
- T6.1 Background sync steward job (wires `steward_decision.rs`, `peer_state.rs`; retries deferred pairings/removal fanout/body fetch). ← T2.4, T2.4b, T4.3, T5.9
- T6.2 `ee daemon install|uninstall|status` (one user-scoped launchd/systemd service multiplexing registered workspaces; Windows client-only posture is conditional on T2.1 credential-store parity and otherwise blocks team key operations; doctor-runtime mutation rules). ← T2.2
- T6.3 Full admission control (`admission.rs`) wired into responder accept
  path, superseding T2.1's minimal caps; doctor/status report T2.4 cumulative
  signed-origin normal usage plus authenticated-node and team-wide control
  usage, effective local limits, free-space floor, coalesced exhaustion, and
  unaffected local-write posture. ← T2.2,
  T2.4
- T6.4 Doctor team checks (service/broker route posture, WhoIs binding,
  hardened keys and stuck/expired pair rotations, immutable root/local port
  agreement, all broker routes sharing one port, host-wide
  owner/one-responder-user limitation, client-only repair, removal
  acknowledgements, invite-authorization time rollback and atomic
  revoke-all-before-floor repair, delegated-member review, and rematerialization
  generation/fence/outbox recovery, plus T5.9 body-cache lifecycle
  reconciliation). ← T4.6, T5.9, T6.1, T6.2
- T6.5 Perf bench profile + budgets (join, signed relay, body/index amplification). ← T2.5, T4.3, T5.7, T5.9, T5.10
- T6.6 Docs: quickstart, trusted-team vs untrusted-contractor fitness table, agent-ux team notes, CHANGELOG, with codes and repair commands sourced from the completed doctor surface. ← T4.7, T5.3, T5.10, T6.2, T6.4
- T6.7 Program closeout: verification-matrix style ledger of every child, deferred items, and proof rows. ← everything

**Sub-epic T7 — M6 SSO identity**
- T7.1 Tier-1 probe extension: reuse the stable-node/current-key observation
  model and parser that T2.2 must already land for M1, then add per-node
  `UserProfile` owners plus fake-tailscale owner mismatch/reassignment
  fixtures. This bead must not become a hidden prerequisite for transport.
  ← T2.2
- T7.2 Tier-1 tailnet attestation: `ee team idp require --tailnet-attested`, manifest policy, join-time + revalidation checks, suspension/grace posture. ← T7.1, T4.3
- T7.3 `ee team members revalidate` + **tier-1-only** noninteractive IdP check
  cadence in the steward; tier-2 handling is timer-only due/overdue/grace
  suspension with zero background IdP HTTP. Land the shared persisted
  nondecreasing identity-authorization time floor advanced by
  authorizing/mutating/import/serve paths; status/doctor/activity/audit use a
  non-persisting effective view. The explicitly confirmed repair suppresses
  current tier-2 leases before lowering a forward-jumped floor. ← T7.2, T6.1
- T7.7 Base fake-IdP harness/protocol fixtures (discovery/JWKS/device/token, secretless-public-client and client-secret-required capability variants, positive/zero/missing/overflow expiry+interval, omitted-interval default, no-early-poll, cumulative slow-down, timeout backoff, provider/local/300-request expiry, cancellation/reap/no-auto-restart, process-loss outer-identity-pending/fresh-ceremony behavior, rotatable/retired/weak/wrong-curve keys, duplicate JSON member names at every layer, noncanonical compact-JWT/base64url forms, unsupported `crit`, token-controlled/embedded key headers, missing/duplicate/ambiguous `kid` and key metadata mismatch, DNS rebinding, inherited proxy/`.curlrc`/netrc/CA/keylog traps, GET redirect and credential-POST traps, inherited-pipe/timeout/output-cap/reap cases, replay races, and assertions that bearer tokens never enter mesh frames); no CLI acceptance dependency. ← T0.0 (parallel-safe)
- T7.4 Tier-2 provider preflight + constrained curl device client: require a capability-compatible secretless public client (`token_endpoint_auth_methods_supported: none`) and reject client-secret-required providers without distributing a secret; invoke an allowlisted canonical system curl directly from a minimal environment with ambient config/proxy/netrc/CA/keylog disabled, explicit approved CA-bundle handling, strict URLs, bounded stdin/output/process lifetime, redacted token responses, no credential-POST redirects, manual validated GET redirects, and pinned DNS; implement RFC 8628 polling with positive checked expiry/interval, default-5 interval, no-early-poll, cumulative +5 slow-down, timeout backoff, earlier-of-provider/1800-second monotonic deadline, 300-request ceiling, terminal cancellation/error handling, and structured `team_idp_device_flow_expired` without automatic restart; add `ee team idp set`, including exact-generation explicit local acceptance for policy relaxation/incomparability and grace-safe activation bootstrap. Because `idp set` is an identity-policy mutation, it advances T7.3's shared identity-authorization time floor transactionally before accepting or activating policy. ← T7.2, T7.3, T7.7
- T7.5 ID-token verification (fresh discovery/JWKS on every new presentation,
  with 304 allowed and stale offline keys forbidden; bounded
  duplicate-member-rejecting JSON and canonical unpadded
  base64url/compact-JWT decoder; reject unknown/unsupported `crit`,
  `jku`/`x5u`/embedded `jwk`/`x5c`; use only exact issuer `jwks_uri` raw
  parameters and require one unambiguous eligible `kid` after
  `kty`/`use`/`key_ops`/`alg`; verify exact original compact signing input
  before claims/replay; key strength/curve,
  issuer/audience/`azp`/time/verified-email/configured-group checks against
  the T7.3 local time floor; atomic single-use ledger; nonce extension when
  available; never request offline access; persist only token hash + minimal
  canonical evidence—subject, optional previewed email, bounded configured-
  group matches/decision, never full groups/unrelated claims—zeroize and never
  store raw ID/access/refresh tokens; retired keys never verify new
  presentations). ← T7.4, T7.3
- T7.6 Verifier-hosted OIDC attestation + member lease records + group
  authorization, integrated with the crash-resumable outer join/renewal:
  distinct active verifier owns
  provider HTTP/token receipt and immediately zeroizes bearer material;
  version-negotiated ≤8 KiB `identity_attest` sends only TTL-bound ceremony
  ID/URL/user code/status to the subject and none of those ephemera enter
  DB/manifest/audit/log/support bundles; `idp set`/join previews team-visible
  minimal claim fields; process loss resumes only the non-secret outer
  `identity_pending` checkpoint and requires an explicit fresh device
  ceremony—device codes/poll state are never persisted or reused;
  every grant/serve/import decision checks lease expiry through the T7.3
  floor; read-only identity surfaces never advance it. Emit finite
  exact-policy-generation-bound `identityAttested` records carrying signed
  verification time and evidence expiry; receivers reject future-skewed,
  nonpositive, over-cadence, evidence-overrun, and already-expired deliveries.
  Reject self/same-subject renewal, handle concurrent
  leases and duplicate-subject conflict deterministically, proves one-member
  activation bootstrap, and owns the full fake-IdP CLI/privacy/clock scenario
  matrix. ← T7.5, T7.3, T2.1

Cross-cutting rules for every implementation bead: inline `#[cfg(test)]` units;
RCH-remote verification for cargo stages; fixture+taxonomy in the same commit
as any new degraded code; schema + drift test in the same commit as any new
`ee.*.v1`; no new files beyond the module boundaries named here without
justification.

---

## 13. Pinned operator decisions and named deferrals

1. **Cryptography/TLS dependency bundle** — **DECIDED 2026-07-30 (operator
   adopted the recommendation):** tier-2 HTTPS egress uses the **curl
   subprocess** backend (zero new crates; same pattern as the `tailscale`
   binary fallback); JWT verification uses pure-Rust RustCrypto **`rsa` +
   `p256`** (added to the tree when T7.4/T7.5 start, listed in the
   dependency-contract matrix then); **`rustls` is deferred entirely** (the
   `EE_TEAM_IDP_HTTP_BACKEND` env var stays registered with `curl` as its only
   shipped value, reserving `native` for a future decision).
1b. **Ed25519 origin signatures + relay in v1** — **DECIDED 2026-07-30
   (operator ratified explicitly, same day).** The implementation-readiness
   review reversed the earlier direct-from-origin-only v1 decision, making
   per-origin Ed25519 stream signatures and peer-assisted relay v1
   requirements (rationale: security removals must propagate even when the
   remover is offline; equivocation handling and removal cutoff fan-out are
   built on signatures). Approved direct dependencies:
   **`ed25519-dalek = "=3.0.0"`**, `default-features = false`, audited
   `features = ["fast", "zeroize"]` only; direct
   **`zeroize = "=1.8.2"`**, `default-features = false`; generation from a
   `Zeroizing<[u8; 32]>` via fallible `getrandom::fill`; `verify_strict`
   required; `rand_core`/`hazmat`/`legacy_compatibility` forbidden. T2.0
   adds the dependency-contract entries and T3.6 hardens the lifecycle;
   the forbidden-deps audit records the transitive `ed25519-dalek` 2.2
   line (via asupersync's `nkeys`) until upstream converges.
2. **Default for `elevate_member_human_explicit`** — **DECIDED 2026-07-30
   (operator adopted the recommendation): ON** for invite-ceremony members
   (the ceremony is the consent), with the T3.4 amplification controls
   (elevation-basis in `ee why`, per-member counts in status, daily velocity
   cap; excess rows import as `agent_validated` +
   `team_member_elevation_burst`) shipping in the same slice as the default.
3. **Windows background service** — there is no Windows daemon to schedule
   (the daemon is `#[cfg(unix)]`; P5.2 declares Windows members client-only
   in v1 only when T2.1 key-store parity passes). v1 documents manual Task
   Scheduler invocation of a full bidirectional foreground `ee team sync`;
   without the safe Windows credential adapter, team key operations fail
   closed, and body hydration additionally requires T5.9 cache-security
   parity or remains metadata-only. A native Windows responder/service plus a
   secure same-user local broker-control transport is the named follow-up.
4. **`ee context` alias** — team scope lands on `pack`/`search`/`ask`; the
   soft-deprecated `context` alias inherits via shared code, no extra work
   planned.
5. **SSO default posture** — `ee team create` prints a suggestion for
   `idp require --tailnet-attested` when the probe shows a corporate tailnet;
   it never auto-enables identity policy.
6. **Private-key backup posture** — **proposed by the 2026-07-30 review;
   posture carried by accepted ADR 0086 TC-D5/TC-D14 (no separate operator
   marker; adopt-by-default unless objected):**
   <!-- marker-rule: items in this section lead with DECIDED only when an
   operator explicitly ratified that numbered item; "architectural
   consequence" does not confer the marker. This wording has been reverted
   twice by review — do not escalate it again. -->

   the current redacted `ee backup` format
   remains credential-free: it never gains MAC, pair, signing, or OIDC key
   material in this program. Data restored from it cannot silently recover
   native trust or mesh identity. Whole-user-data key recovery is an external,
   operator-protected system-backup concern and must pass hardened local
   validation before reuse; a first-class encrypted credential-backup format
   would require a separate ADR and threat model.

---

## 14. Appendix: absorbed hygiene fixes

Captured here so they're not lost if milestones reorder: effect-registry mesh
path/table corrections; README `[mesh.tailscale]` + `.ee/mesh/` + scope-flag
drift; stale schema description on lane-grant preview; `probe_mesh_capability`
`"1"` handling; `self_advertised_tags` hardcoding; three newline-only audit test
files; 19 uncovered degraded codes; `mesh_anti_entropy_transport_unavailable`
defined-but-never-emitted (either emit or delete); `docs/mesh/local_two_node_demo.md`
revised in place to document the real harness (do not delete the file);
SRR6-era `Status: proposed` headers updated as surfaces ship.
