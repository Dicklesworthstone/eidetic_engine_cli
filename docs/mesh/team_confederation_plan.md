# Team Memory Confederation Plan

Status: active plan
Owning ADR: [ADR 0086 — team memory confederation](../adr/0086-team-memory-confederation.md) (decisions TC-D1…TC-D16; where plan and ADR conflict, the ADR wins and the plan gets corrected)
Related ADRs: 0037 (optional mesh), 0038 (auto-enrollment), 0041 (anti-entropy), 0009 (trust classes), 0069 (global knowledge lane), 0083 (user-global store)
Related open beads: bd-30o6g, bd-3mw86, bd-2gvgw, bd-1bfwa (epic + .2/.3/.4/.5)
Date: 2026-07-30

---

## 0. TL;DR

`ee` today is a single-operator, single-machine memory substrate with an optional
"mesh" subsystem that is meticulously specified, extensively unit-tested — and
**not connected to a network**. There is no listener anywhere in the tree, `ee
mesh sync --once` is wired to a no-op transport that always emits
`mesh_sync_once_network_deferred`, discovery reads Tailscale ACL capability
metadata that no code ever publishes, and the mesh policy engine
(`decide_mesh_peer_policy` / `decide_mesh_outbound_policy` / `decide_mesh_import`)
has zero production callers. The working peer data path today is sneakernet:
`ee mesh export` → copy file → `ee mesh import`.

This plan turns that foundation into **team confederation**: N human users, each
running `ee` locally on their own machine, forming a trusted mesh over a shared
tailnet that behaves like one unified team memory — with automated peer
discovery, automated background sync, per-person attribution, and a setup flow
simple enough for non-technical users (`ee team create` → send invite code →
`ee team join <code>`).

The plan deliberately does **not** design a new distributed system. It:

1. **Finishes the bridge whose two ends already exist** — wires the shipped
   frame codec, hello protocol, anti-entropy planner, and policy engine to a
   real socket path (std::net TCP over the tailnet, asupersync-supervised, no
   forbidden deps — the same pattern `src/core/tailscale_probe.rs` already uses
   for its hand-rolled LocalAPI HTTP client).
2. **Adds the three identity primitives the current design lacks** — a human
   member identity (person, not agent nickname), a cross-machine project
   identity (so two teammates' clones of "the same project" can be recognized
   as such), and a new trust class (`peer_human_verified`) so a teammate's
   deliberate `ee remember` can arrive elevated above generic agent assertion
   without violating the `human_explicit`-is-local invariant. Member identity
   optionally binds to corporate SSO (Microsoft Entra / Okta / Google) in two
   tiers: tailnet-attested node ownership (zero new dependencies — tailnets
   already authenticate through these IdPs) and a direct OIDC device-code
   flow for ee-level proof and directory-driven offboarding.
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

The honest current answer is: each analyst has a separate local `ee` memory;
confederation machinery exists in skeletal/specified form but has to be set up
explicitly, requires a Tailscale network for authentication, and — critically —
does not yet actually move data between machines without manually copying
export files.

### 1.2 The product goal

A team of 2–20 humans, each with `ee` installed locally, should be able to:

- **Form a team once** with one command and one shared invite code, given an
  existing tailnet (Tailscale is the transport/auth substrate; a plain-language
  one-page setup doc covers installing Tailscale itself).
- **Keep working locally** exactly as before. Local memory stays the source of
  truth on each machine; nothing blocks on the network.
- **Automatically see teammates' shared memories** in `ee search` / `ee pack` /
  `ee ask` results, clearly attributed ("from Priya · project acme-analysis ·
  synced 2026-07-30T14:02Z" — absolute timestamps in retrieval surfaces per
  the determinism rule in §7.5 P4.2; relative phrasing like "2h ago" appears
  only in `ee team status`/`activity` human output), within the lanes each
  member consented to share.
- **Ask the team-shaped questions**: "has anyone on the team looked at Acme
  Corp?" → `ee search "Acme" --memory-scope team` or `ee team activity
  --project acme-analysis` shows who captured what, when.
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
| **member** | product | A human being, identified by a `member_id` + display name, bound to one or more machine node keys. New primitive (§7.3.1). |
| **mesh peer** | mechanism | A machine (Tailscale node) enrolled for exchange, exactly as today (`mesh_peers` table). |
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
| Anti-entropy protocol math: tips, cursors, bounded range planner, retry/backoff (1s→60s, max 5), digests, redaction-safe sync summary | `src/mesh/anti_entropy_protocol.rs` | Complete + tested. `MeshRangePlanner` has no production caller; cursors only advance via `ee mesh import`. |
| Anti-entropy executable model + 13 pinned scenarios | `src/mesh/anti_entropy_model.rs`, ADR 0041 | The contract to satisfy, not code to run. |
| Signed bounded frame codec: blake3-keyed signatures, capability allowlist (`hello`/`summary`/`event_fetch`/`body_fetch`), 64 KiB frame / 32 KiB payload budgets, constant-time compare | `src/mesh/tailscale_transport.rs` (794 LOC) | **Complete but entirely dead — zero callers.** |
| Hello wire protocol (`ee.mesh.hello.v1` / `.response.v1` / `.error.v1`), ≤4096-byte payloads, version negotiation, privacy-preserving decline | `src/mesh/hello.rs`; `decide_hello_response` at `:405` | Complete; **zero non-test callers**. |
| Discovery policy: `service_tag` (default) / `auto_admit` / `allowlist` on both caller and responder axes; denylist overrides all; TOML files under `<ws>/.ee/` | `src/mesh/discovery_policy.rs`; CLI `src/cli/mesh.rs:1322–1477` | Real and wired for policy *decisions*. |
| Auto-enrollment: 13-step fail-closed flow, forensic audit-before-write, tailnet/node-key identity guard, rollback | `src/mesh/auto_enrollment.rs`, `auto_enrollment_safety.rs`, `identity_change_guard.rs`; CLI `src/cli/mesh.rs:1164–1320` | Real, transactional. But see trust dead-end in §3.3. |
| Mesh policy engine: per-peer per-lane per-origin-workspace inbound/outbound decisions, trust-lane ceilings, side-effect booleans | `src/core/memory_scope.rs:754,999,658`; facade `src/mesh/policy.rs` | Complete + tested; **zero production callers**. |
| Lane-grant preview (counts, redacted samples, cautions) | `src/mesh/lane_grant_preview.rs` | Complete; CLI feeds it an **empty candidate set** (`src/cli/mesh.rs:795–813`). |
| Pre-export secret scan (hard-denies `ee mesh export` with `mesh_secret_export_denied`) | `src/policy/mod.rs:284–350` | Real and enforced. |
| `ee mesh export` / `ee mesh import`: bounded, schema-gated, idempotent, ledger-writing, index-job-enqueuing file exchange | `src/cli/mesh.rs:1793–1872, 1988–2028, 3056–3262` | Real. **This is the only working peer data path today.** |
| `ee share preview` consent ritual: DB-backed counts, redacted examples, stable preview hash, `mesh.share.consent` audit row | `src/cli/share.rs` | Real, but verdicts are simulated (`policy_action: "allow"` hard-coded for metadata; never consults peer policy). |
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

1. **No transport.** `NoopMeshForegroundSyncTransport` is the only production
   implementation of the `MeshForegroundSyncTransport` seam
   (`src/mesh/foreground_cli.rs:623–651`). No `TcpListener`/`UnixListener`/bind
   exists anywhere under `src/mesh/`. `src/daemon/` and `src/serve.rs` contain
   zero mesh references, even though `hello_responder_not_running`'s repair
   text says "Run `ee daemon --foreground`" (`src/mesh/hello_responder.rs:96`).
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
   enrollment or import path. (The dead frame codec has blake3-keyed signing
   ready to use.)
8. **`ee mesh grant` does not exist** despite being the load-bearing widening
   mechanism referenced by ADR 0038 (:104,203,403), the lane-grant preview
   module header, the schema description, and the onboarding doc.
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
11. **Status surfaces lie by hardcoding.** `ee mesh status` reports a fixed
    lane policy regardless of config (`src/mesh/foreground_cli.rs:1296–1303`);
    hello-responder `running:false`, discovery-cache `not_loaded`, peer-state
    zeros, empty drift, `steward_posture: not_inspected` are all constants
    (`:1149–1197`, `:1225–1240`). `probe_mesh_capability()` honestly returns
    `Unimplemented` when `EE_MESH_ENABLED=true` (`src/core/status.rs:1305`) —
    note it string-matches only `"true"`, not `"1"`.
12. **Selective sync is display-only.** `SelectiveSyncConfig::safe_starter_config().summary()`
    is the only production use (`src/mesh/foreground_cli.rs:1028`); no
    persisted subscriptions.
13. **Degraded-code hygiene debt.** Of 52 mesh codes in the catalog golden,
    only 33 have failure-mode fixtures and 34 taxonomy entries; e.g.
    `mesh_sync_once_network_deferred`, `mesh_disabled`, all seven
    `mesh_peer_*` codes lack fixtures (AGENTS.md requires fixture + taxonomy
    per emitted code). Three audit test files are 0 bytes
    (`tests/mesh_{tailnet_change,identity_change_guard,discovery_policy}_audit.rs`).
14. **Effect-registry and docs drift.** `src/core/effect.rs` declares
    nonexistent `.ee/mesh/*.json` paths and a never-created `mesh_audit_events`
    table (`:1934–1955, 2388–2418, 2648–2658`); README documents a
    `[mesh.tailscale]` config block that `MeshConfig::parse` never reads
    (`README.md:1391–1393` vs `src/config/file.rs:512–521`) and a `.ee/mesh/`
    directory that is never created (`README.md:1516`).

### 3.4 Open-work interlocks (respect, don't duplicate)

| Bead | Status | Interaction with this plan |
|---|---|---|
| **bd-30o6g** (P2) — remote-evidence byte policy trusts declared size, not fetched body length (`src/mesh/remote_evidence.rs:435–454`) | open | **Hard prerequisite for any real transport.** Acceptance already demands a streaming `max_bytes+1` cap in the future fetch adapter. Transport beads depend on it. |
| **bd-3mw86** (P1) — `ee mesh disable --peer` lacks durable per-peer containment | in_progress (another agent) | Do not touch. The team UX consumes whatever per-peer suspension state lands; note it as a soft dependency of the incident-containment story. |
| **bd-2gvgw** (P3) — pin lane-grant preview nested `required` fields | blocked (stale — its only blocker is closed) | The M0 grant/preview work builds on this schema; absorb or depend on it. |
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
will and won't be shared by default (metadata yes; bodies no until per-member
consent; embeddings never by default). JSON mode emits `ee.team.create.v1`.

**US-2 — Invite.** Hana: `ee team invite` → short-lived single-use code
(pasteable in Slack): `eeteam1-<base32…>`. Printed text tells her exactly what
the code authorizes and when it expires (default 72h, configurable).

**US-3 — Join.** Priya (on the tailnet, ee installed): `ee team join
eeteam1-…` → the command finds Hana's machine via the invite's endpoint hint +
tailnet discovery, performs a real hello handshake, mutually enrolls both
machines with `explicit_human_consent` (the humans typed the code — that *is*
the consent ceremony), registers Priya as a member, replicates the manifest,
prints a plain-language summary of what she is now sharing and with whom, and
records consent audit rows on both sides. A `--dry-run` preview exists. Exit 0
only if the team is actually joined and the first metadata sync round
completed.

**US-4 — Unified recall.** Priya, six weeks later, is assigned Acme Corp:
`ee search "Acme Corp" --memory-scope team --json` (and `ee pack "prep Acme
analysis" --memory-scope team`) returns her own memories plus teammates'
shared memories, each attributed (member display name, project, origin trust,
synced-at). `ee team activity --project acme-analysis` lists who captured what
recently. The client question "how will I know what another team member ran"
is answered by these two commands.

**US-5 — Background freshness.** Members' machines sync automatically while
online (daemon-hosted supervised job, bounded budgets). `ee team status` shows
per-member reachability and staleness ("Hana: synced 4m ago · Marcus-laptop:
unreachable 3d"). No command ever blocks on a peer; stale is visible, not
silent.

**US-6 — Widen sharing deliberately.** Hana decides rule bodies should flow:
`ee team share bodies --with priya` (or `--all-members`) → runs the lane-grant
preview against real candidate memories, shows counts + redacted samples +
cautions, requires explicit confirmation, records consent, materializes the
lane grant via `ee mesh grant`. Secret scan still hard-denies risky exports.

**US-7 — Someone leaves.** `ee team member remove marcus` → revokes his
machines' peer records, stops future sharing, propagates a share-withdraw for
team-shared material, prints the honest caveat that already-synced copies on
his machine cannot be remotely deleted (mesh withdrawal is best-effort by
design).

**US-8 — Emergency stop.** `ee team pause` (workspace-scoped mesh disable
under the hood) and `ee team resume --confirm`. Marcus-the-compliance-officer
can read `ee team audit --json` for the full consent/sync/grant ledger.

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
that owns their Tailscale node — `ee team members` shows
`priya@acme.com (verified via tailnet)`, and a node owned by any other account
cannot join or sync as Priya. With tier 2 enabled (`ee team idp set --issuer
https://acme.okta.com ...`), joining additionally runs a device-code sign-in
("open https://acme.okta.com/activate and enter QXZ-JKP") and the member
record carries an IdP-verified subject + email.

**US-12 — Offboarding follows the directory.** Marcus removes a departed
analyst from Okta. On the next revalidation cadence (or `ee team members
revalidate`), the member's identity attestation fails, their peer grants are
suspended with an audit row, and `ee team status` shows
`identity_revalidation_failed` — without anyone remembering to run
`ee team member remove` manually.

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
  Networking is std::net + asupersync supervision (precedent:
  `src/core/tailscale_probe.rs`).
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

- **`human_explicit` remains strictly local.** We add `peer_human_verified`
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
                                  bypass, fix bd-30o6g)
P1  Real transport               (listener in daemon + client in CLI,
                                  hello over TCP, anti-entropy rounds that
                                  advance cursors, metadata lane first)
P2  Identity                     (members, projects, peer_human_verified,
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
 │   ├─ hello responder      │  signed frames │   ├─ hello responder      │
 │   └─ sync steward job     │  (blake3-keyed)│   └─ sync steward job     │
 └───────────────────────────┘                └───────────────────────────┘
      ▲ local search/pack reads cached, policy-admitted peer rows
      │ with member attribution; never blocking on the network
```

(Each side's local DB remains its source of truth; the other side's material
lives in the import ledger + derived index, admitted per-lane by policy.)

**Listener asymmetry, stated plainly:** the transport is pull-only. Every
exchange requires the *counterparty's* responder to be listening, and the only
long-lived listener home is the daemon. A member whose machine never runs the
daemon can pull from others but never be pulled from — effectively read-mostly
to the team until their daemon runs. `ee team status` surfaces this per
member; the join ceremony provides a foreground accept path
(`ee team invite --wait`, §7.4 P3.2) so M3 never depends on daemon install
(M5). The existing daemon is `#[cfg(unix)]` (`src/daemon/server.rs:25`);
Windows members are client-only in v1 and documented as such (§7.6 P5.2).

---

## 7. Detailed design

### 7.1 P0 — Wire-reality groundwork

Rationale: building new features on surfaces that hardcode their answers would
compound dishonesty the project has worked hard to eliminate (the
honesty-only/implements-surface taxonomy exists precisely because of this
failure mode). P0 makes the substrate truthful and closes the security holes
that real transport would otherwise amplify.

**P0.1 — Fix bd-30o6g (byte-policy trusts declared size).**
`MeshRemoteEvidenceFetchPolicy` must enforce `fetched_body.len()` with a
streaming `max_bytes+1` read cap, not `reference.size_bytes`. This is currently
latent (test-only call sites) and becomes remotely exploitable the moment a
real fetch adapter exists. Do this first; the transport beads depend on it.

**P0.2 — Wire the policy engine into the existing file paths.**
`ee mesh export` consults `decide_mesh_outbound_policy` per record and lane;
`ee mesh import` consults `decide_mesh_import` per event, recording the policy
decision JSON in the ledger columns that already exist for it (V053/V055).
`[[mesh.peer_policies]]` config becomes load-bearing. `MeshPeerPolicyRegistry`
gets its first production caller. Acceptance: a configured `body: deny` policy
observably strips bodies from an export artifact; an import of a denied lane
records `mesh_peer_policy_denied` in the ledger instead of upserting rows.

**P0.3 — `ee share preview` becomes policy-backed.** Replace the hard-coded
`policy_action: "allow"` verdicts (`src/cli/share.rs:221–247`) with real calls
into the outbound policy for the named peer; unknown peer ⇒ explicit
`share_preview_peer_unknown` degraded entry instead of pretending. The preview
hash and consent-audit flow stay as-is.

**P0.4 — DB-backed `ee mesh preview-grant` + new `ee mesh grant`.**
Feed the existing `compute_lane_grant_preview` real candidate memories from the
DB (the module already samples/redacts/bounds at 500), real `peer_in_group`,
real redaction rules. Add the missing `ee mesh grant <node-key> --lane <lane>`
mutation: preview-hash-pinned (`--preview-hash` required, mirroring
`--record-consent` flow), audited, updating the peer record's lane grants.
Absorb bd-2gvgw's schema `required`-field pinning. Acceptance: grant without a
matching fresh preview hash fails closed; granted lane visibly changes
export/import policy decisions from P0.2.

**P0.5 — De-hardcode status/report fields.** `ee mesh status` reports the
*actual* configured lane policy, actual discovery-cache state, actual peer
state breakdown, actual responder posture (from P1 it becomes genuinely
probeable); `self_advertised_tags` comes from the real probe instead of
hardcoded-empty (`src/cli/mesh.rs:2256`), so `discovery_policy_no_ee_mesh_tag`
only fires when true. `probe_mesh_capability` accepts `"1"` as well as
`"true"` and stops reporting `Unimplemented` once sync actually works (flip in
P1, not before).

**P0.6 — Close the unauthenticated trust bypass.** `ee import jsonl` must not
grant `human_explicit` on the strength of a spoofable `import_source=native`
header. Change: native trust is **authenticated, not merely identified** — the
exporting store MACs the export header + content manifest with a store-local
secret (blake3-keyed; 0600 keychain file under the user data dir, same home as
the pairwise keys in §7.3.1). Import verifies the MAC against the local store
key before honoring native trust; absent, invalid, or foreign MAC ⇒ external
handling, so `human_explicit` rows are refused with the existing
`external_import_human_explicit_trust_class` error and a pointer to the
team-aware path. (A bare store-UUID comparison was considered and rejected:
store identifiers plausibly leak via support bundles and status JSON, and a
leaked identifier would reopen the bypass verbatim.) `ee playbook import` caps
imported `trust_class` at `agent_validated` unless the artifact passes the
same store-local MAC, keeping its maturity attenuation. Per the
no-backwards-compat policy this is a direct behavior fix; CHANGELOG +
migration note required. Acceptance: a teammate's export can no longer inject
`human_explicit` rows; a copied artifact with a correct store UUID but no
valid MAC is refused; one's own backup restore still round-trips at full
trust.

**P0.7 — Honesty-debt backfill.** Failure-mode fixtures + taxonomy entries for
the uncovered mesh degraded codes (52 in the catalog golden: 19
fixture-uncovered, 18 of those also taxonomy-uncovered — close both gaps);
fill the three 0-byte audit test files
with the assertions their names promise; correct `src/core/effect.rs` mesh
declarations (real `.ee/*.toml` paths, real tables); fix README `[mesh.tailscale]`
and `.ee/mesh/` drift; update the stale "future `ee mesh preview-grant`" schema
description. Mechanical but contractually required by AGENTS.md ("new degraded
code ⇒ fixture in the same commit"; the drift radar gate).

### 7.2 P1 — Real transport

Design decision: **one listener, one port, one frame protocol.** The daemon
hosts a supervised TCP listener bound to the machine's tailnet IP on
`EE_MESH_HELLO_PORT` (default 41888, already registered). All ee↔ee traffic —
hello, anti-entropy rounds, future body fetch — uses the existing signed frame
codec (`src/mesh/tailscale_transport.rs`) over that socket, with the codec's
capability allowlist (`hello`/`summary`/`event_fetch`/`body_fetch`)
distinguishing message families. Rationale: a second port or protocol would
double the firewall/ACL story for non-technical operators; the codec already
budgets frames (64 KiB) and payloads (32 KiB) and does constant-time signature
checks; and hello is deliberately small (≤4096 bytes) so it fits unchanged.

**Why std::net + asupersync is compliant and sufficient.** The forbidden-deps
rule bans HTTP *stacks*, not sockets. `src/core/tailscale_probe.rs:887–949`
already ships a hand-rolled HTTP/1.1 client over `UnixStream`. The transport
needs even less than HTTP: length-prefixed signed frames over TCP. Blocking
std::net I/O runs inside asupersync-supervised tasks with explicit budgets and
kill-on-timeout, the same pattern the probe's subprocess fallback uses. Tailnet
TCP between two WireGuard peers is encrypted and mutually authenticated at the
network layer by Tailscale; the frame signature (pairwise blake3 key, §7.3.1)
authenticates the *ee peer* on top, preserving "Tailscale is not trust."

**P1.0 — Origin event stream substrate (the missing table).** Everything in
anti-entropy assumes each node maintains a durable, append-only, per-origin
sequence of **its own** events — that is what a tip advertises, what a
`RangeRequest` addresses, and what fork rejection hash-chains over. No such
table exists: V052–V057 are all inbound-side (`mesh_peers`, cursors, import
ledger, mappings, body-cache metadata). Deriving "events" on demand from
mutable tables cannot yield stable sequence numbers — any later edit would
look like a fork to peers. New migration: `mesh_origin_events`
(`origin_seq` contiguous per (workspace, origin stream), event hash chained
via `prev_event_hash`, event kind, material lane, payload reference,
authored_at), plus the append rules: which local mutations emit events
(memory create/update/tombstone/shareWithdraw within shared scope; manifest
operations in §7.4 P3.1), written in the same transaction as the mutation, and
an immutability contract (rows are never updated or deleted; corrections are
new events). This lands before any wire work and is also what the manifest
(§7.4 P3.1) rides on.

**P1.1 — Hello responder actually binds.** New supervised daemon job:
`mesh-hello-responder`. Accept loop → read one bounded frame → if hello:
`decide_hello_response` (finally wiring `src/mesh/hello.rs:405`) under the
existing rate limiter and tailnet-source validation in `hello_responder.rs`;
respond or privacy-preserving decline. `ee mesh hello-responder status`
switches from hardcoded `running:false` to querying the daemon over the
existing daemon protocol. Mesh disabled or `EE_MESH_HELLO_RESPONDER_DISABLED=1`
⇒ job never starts (mesh-off invariants untouched; `mesh_off_no_network.rs`
extended to assert the daemon starts no listener when mesh is off).
Feasibility note: `src/daemon/server.rs` already implements exactly the needed
pattern — accept-loop-in-thread with connect-to-self wake, bounded workers
(`:220–246, 464–472`) and a background scheduler thread (`:981–995`).

**Bootstrap envelope (pre-key traffic).** Frame signing is pairwise-keyed, but
at hello time — probing a stranger, or the join ceremony itself — no pairwise
key exists yet. Pre-enrollment traffic (hello request/response/decline and the
join ceremony's invite frames) therefore uses a distinct **unsigned but
strictly bounded** envelope: its own ≤4096-byte budget, aggressive rate
limiting, and no capability beyond `hello`/`join`. All post-enrollment traffic
requires signed frames, and signed-capability frames from unkeyed peers are
rejected. Minimal accept-side abuse caps land here too, not in M5: a
connection semaphore and per-peer frame budget on the accept path (full
`admission.rs` wiring remains P5.3, but the port is never open without at
least these caps; the two-node harness includes a flood test).

**P1.2 — Client-side hello probe over TCP.** Replace
`TailscaleStatusCapabilityHelloProbe` with a real probe: for each
policy-admitted candidate peer (same `decide_discovery` filter as today),
connect to `<peer_tailscale_ip>:41888` within the existing 750 ms per-peer /
5 s total budgets and exchange hello frames. The ACL-capability read stays only
as a cheap *pre-filter hint* when present (it can mark peers `ee-capable`
without a connection, e.g. via the `tag:ee-mesh` service tag), never as the
authority. Unreachable/timeout/declined map to the existing skip-reason
vocabulary. This makes `ee mesh auto-enroll` and `ee mesh status --json`
discovery real on a stock tailnet.

**P1.3 — Anti-entropy over the wire.** Implement
`TcpMeshForegroundSyncTransport: MeshForegroundSyncTransport` — the production
implementation the seam was built for. One `contact_peer` call runs one
bounded round: TipAdvertise ⇄, `MeshRangePlanner::plan` (first production
caller), at most one RangeRequest per origin per round, EventBatch replay
through `decide_mesh_import` + ledger insert + cursor advance
(contiguous-replay-only, fork-rejecting — the 13 ADR 0041 scenarios become
integration-tested behavior), RevisionNotice emission. Metadata lane only in
this milestone; bodies/embeddings remain policy-denied by default anyway.
Event **origin authentication (v1): direct-from-origin only** — a connection
authenticated (by pairwise frame key) as peer X may only deliver events whose
`origin_node_id` is X; relayed origins are rejected with a new
`mesh_relay_origin_rejected` code. This closes the forged-origin threat
without asymmetric crypto; relay support via per-origin stream signatures is
an explicit v2 bead. `ee mesh sync --once` stops emitting
`mesh_sync_once_network_deferred` when a round actually ran; the code remains
for daemonless/unreachable cases. `probe_mesh_capability` graduates from
`Unimplemented`. Key storage lands in M1 — in T2.1's session layer, not
here: a `mesh_peer_keys` keychain file (0600, user data dir) with per-peer
key slots, populated by harness fixtures in M1; the invite-ceremony
*derivation* of those keys is §7.3.1/M2.

**P1.3b — Responder-side anti-entropy serving (the other half of the wire).**
The client (P1.3) is useless without a peer that *serves* `summary`/
`event_fetch`: the responder job answers TipAdvertise with its own frontier,
serves bounded `EventBatch` ranges from `mesh_origin_events` (P1.0), and — the
non-negotiable part — applies `decide_mesh_outbound_policy` **per event per
lane** and the pre-export secret scan **on the wire path** before anything
leaves the node, mirroring what P0.2 does for file export. Narrowed responses
(policy-denied or budget-clipped ranges) follow the protocol's re-ask-next-
round rule. Acceptance: in the two-node harness, a `body: deny` policy and a
planted secret observably never cross the wire.

**P1.4 — Sync engine placement.** The round executor lives in core
(`src/mesh/` + `src/core/`), callable from both the foreground CLI (one-shot,
no daemon needed — CLI-first invariant) and the daemon steward job (P5.1).
Budgets: reuse the pinned verification-matrix numbers (sync batch 512 events,
retry 1s→60s max 5, fanout 32) — changing them requires the ADR amendment path
ADR 0041 prescribes.

**P1.5 — Two-node truth harness.** The load-bearing new test asset: a
two-process E2E that starts two real `ee` instances with isolated homes on
127.0.0.1 (fake-tailscale shim supplies identities; loopback substitutes for
the tailnet), forms a pairing, writes memories on A, syncs, and asserts B's
search shows them with correct provenance/trust — and the reverse. Extends the
existing fake-tailscale harness; replaces the jq-only
`mesh_local_two_node_demo.sh` fixture with a real one. The opt-in
real-tailnet smoke (`mesh_sync_once_real_tailscale.sh`) flips from "record
whether deferred" to asserting a real round when `EE_E2E_REAL_TAILSCALE=1`.
Partition/rejoin, fork-rejection, and hole-blocking scenarios from ADR 0041
run against the real wire path.

### 7.3 P2 — Identity

#### 7.3.1 Members and pairwise keys

New DB table `team_members` (workspace-scoped like `mesh_peers`):
`member_id` (`mbr_` + 24-hex blake3-derived), `display_name`, `state`
(active/removed), `added_by_member_id`, `joined_at`, `removed_at`,
`contact_hint` (optional, e.g. email — display only, never authorization), and
`is_self`. New table `team_member_nodes` binding members to node keys
(`member_id`, `node_key`, `peer_id` FK-ish to `mesh_peers`, `bound_at`,
`bound_via` ∈ {invite_ceremony, member_added_node, operator_manual}).

**Authentication model (v1): pairwise symmetric keys established by the invite
ceremony.** The invite code (§7.4 P3.2) carries a one-time secret. During join,
**each side contributes a fresh random 32-byte nonce over the connection**,
and both derive the per-pair long-term key
`k_AB = blake3::derive_key("ee.team.pair.v1", invite_secret ‖ nonce_A ‖ nonce_B ‖ nodekey_A ‖ nodekey_B)`.
The nonces are the load-bearing detail: node keys are visible to every tailnet
member and the invite secret lives forever in Slack/email history, so the
secret must *authenticate the exchange* without *determining the key* — an
attacker with the old invite message must actively MITM the WireGuard path at
join time (already a named threat) rather than derive `k_AB` offline later.
Invite secret and nonces are destroyed after derivation. The key itself lives
in a `mesh_peer_keys` keychain file (0600, user data dir — not in the
workspace, not in git; storage lands in M1 per §7.2 P1.3); `MeshPeerKey`
finally holds real material: the *fingerprint* of `k_AB`. Every
post-enrollment transport frame is signed with the pairwise key (the codec
already implements exactly this). Rotation: `ee team members rotate-key
--with <member>` (the P3.3 command form) re-derives with fresh nonces over
the existing authenticated channel; the existing `ee mesh peer rotate`
generation bookkeeping records it.
Introduction-secret exchanges (§7.4 P3.2) use the same nonce-mixing construction,
so the inviter cannot derive the pair keys it introduced.

Why not Ed25519 now: the dependency set (blake3, sha2) contains no signature
crate; adding one is an operator-level dependency decision, and v1's
direct-from-origin acceptance rule (§7.2 P1.3) makes pairwise MACs sufficient
— B cannot forge A's stream to C because C only accepts A's origin over C↔A
connections. The v2 bead ("per-origin stream signatures enabling relay")
documents the upgrade path and its dependency question explicitly.

`EE_AGENT_NAME` remains what it is — an unauthenticated *agent* label for
attribution within one member's swarm. Member identity is machine-anchored
(node keys + pairwise keys), not env-var-anchored. Producer metadata gains an
optional `memberId` so team-synced rows attribute to people (§7.5.2).

#### 7.3.2 Trust class `peer_human_verified`

New sixth trust class between `agent_validated` and `human_explicit`:

| Class | Initial confidence | Retrieval weight (ask) | In `verified` scope |
|---|---|---|---|
| `human_explicit` | 0.85 | 1.00 | yes |
| **`peer_human_verified`** | **0.75** | **0.92** | **yes** |
| `agent_validated` | 0.65 | 0.85 | yes |
| `agent_assertion` | 0.50 | 0.70 | no |
| `cass_evidence` | 0.45 | 0.55 | no |
| `legacy_import` | 0.30 | 0.40 | no |

Semantics, stated as what the system can actually attest: "this row arrived
over a channel authenticated to a node bound to an active verified member,
and that member's store classed it `human_explicit` at origin." It does NOT
prove a human typed it — `human_explicit` is locally CLI-assignable, so a
misbehaving agent on a member's machine could mint it. Controls for that
amplification risk (also a §8 threat row): `ee why` always shows the
elevation basis; `ee team status` shows per-member elevated-row counts; an
elevation velocity cap per member per day surfaces
`team_member_elevation_burst` for review instead of silently importing.
Elevation happens at import time iff **all** of: (a) event's trust lane is
`peerHumanViaPeer` (an existing `MeshTrustLane` variant,
`src/core/memory_scope.rs:62–86`) with source class `human_explicit`; (b) the delivering
connection is authenticated as a node bound to an active member; (c) the
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

- **Git workspaces:** `prj_git_` + blake3(root-commit-hash)[..24], derived via
  `git rev-list --max-parents=0 HEAD` (first line; multiple roots hash the
  sorted set). Stable across clones/forks; survives remote renames. Fallback
  when history is shallow (`--depth` clones lack the root): normalized origin
  remote URL (`prj_rem_` + hash), with a degraded note recommending
  `git fetch --unshallow` for the stronger key. Never *both* silently — the
  derivation source is recorded (`project_key_source`).
- **Non-git workspaces:** no derivation possible; a project key is minted
  (`prj_tm_` + random) the first time the workspace is shared to a team
  (US-9) and distributed via the manifest.

Storage: new nullable columns on `workspaces` (`project_key`,
`project_key_source`), backfilled lazily on `ee init`/resolution. Wire: hello
`workspaceIds[]` gains parallel `projectKeys[]`; peer-group bindings and
`origin_workspace_ids` policy checks accept project-key matches, which is what
finally kills the manual n×n workspace-ID mapping for the common case. Privacy:
project keys are hashes (or random); they reveal repo identity only to peers
that already passed discovery + hello policy, and the hello decline path
continues to leak nothing.

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
  `/.well-known/openid-configuration`; supported flows: **device authorization
  grant (RFC 8628) only** — the right CLI shape (print
  `verification_uri_complete` + user code, poll the token endpoint), no
  localhost redirect server, works over SSH, trivially agent-narratable. Works
  as-is with Entra ID, Okta, Google, and any conformant OIDC provider.
- HTTPS egress — **decided (§13 item 1): the `curl` subprocess backend** (curl
  is present on macOS, modern Windows, and effectively all Linux), following
  the existing subprocess pattern used for the `tailscale` binary with
  kill-on-timeout — zero new crates; a feature-flagged `rustls` client was
  considered and deferred entirely. The calls happen only inside explicit
  identity commands.
- ID-token verification (issuer, audience, expiry, **signature via JWKS**,
  plus an `iat`/`auth_time` freshness window and single-use recording of the
  token's `jti`-or-hash by the verifier) is non-negotiable — an unverified JWT
  is worthless. Note: RFC 8628 has no nonce parameter and device-flow ID
  tokens generally carry no `nonce` claim, so a verifier challenge cannot be
  bound into the IdP flow; freshness + single-use is the honest substitute.
  RS256/ES256 verification requires pure-Rust RustCrypto crates (`rsa`,
  `p256`; `sha2` is already in-tree) — **approved (§13 item 1)**; the crates
  land in the tree when tier-2 work starts, with dependency-contract-matrix
  entries in the same commit. Only the v2 Ed25519 relay-signature crate
  choice remains open (T3.6 writeup).
- The member record gains `identity_attestation: {kind: oidc, issuer, subject,
  email, verified_at}`. **Verifier model:** the join counterparty verifies the
  presented token and records the attestation in the manifest; other members
  trust that manifest record (this trust link is named in §8) — every-member
  independent verification is rejected as UX-hostile (N device-flow sign-ins
  per join). During join, the joiner completes the device flow locally and
  presents the ID token over the already-authenticated pairwise channel; the
  verifier validates against the manifest-pinned issuer/client and cached
  JWKS (fetched/refreshed during identity commands, cached with rotation
  grace for offline verification).
- Revalidation cadence: manifest-configured (default 30 days, plus on-demand
  `ee team members revalidate`). What the human experiences: a printed
  "sign in again to keep sharing with <team>" prompt with the device code —
  interactive by design, so the cadence is a team policy choice, not a hidden
  background login. Tier-1 checks (node ownership) are non-interactive and
  can run in the steward. Failure ⇒ suspend grants + audit, same as tier 1.
  Grace posture for offline members is explicit
  (`identity_revalidation_overdue`, warning) before suspension.

Both tiers change **authorization posture** only — they never touch memory
content, never run inside retrieval commands, and degrade loudly, not
silently. Teams without any IdP keep the plain invite ceremony; nothing about
tiers 0→2 changes the wire protocol or trust-class semantics (a
`peer_human_verified` elevation simply gains a stronger "verified" basis, and
`ee why` says which basis applied).

### 7.4 P3 — `ee team` UX

A new `src/cli/team.rs` command group. Every subcommand: stable JSON schema
(`ee.team.*.v1`), plain-language human output ending with "what this did"
(named mesh primitives) and "next commands", full audit rows. No TUI, no
wizard (ADR 0038 D1 stands); interactivity is limited to y/N confirmation
prompts that `--yes` bypasses for agents.

**P3.1 — Manifest.** The team manifest is a replicated document:
`team_id`, name, created_by, per-member records (member_id, display name, node
keys, state, added_by, timestamps), shared project registry (§7.3.3), and the
team default lane profile. It is **not** a CRDT: it replicates as ordinary
per-origin manifest events through the origin event stream (§7.2 P1.0,
metadata lane), so every membership change is an attributed, ordered,
fork-rejecting event in its author's stream. Conflicting concurrent
membership edits surface as logical conflicts (existing model) and resolve by
explicit operator action (`ee team members reconcile`, part of T4.4), which
for a ≤20-member team is a corner case, not a steady state. Local cache
table: `team_manifest` (+ raw events in the origin/import ledgers like
everything else).

**Manifest authorization rules** (enforced at event application, pinned in
ADR 0086):

| Operation | Authorized author | Effect on application |
|---|---|---|
| member-add | any *active* member | New member + node bindings recorded |
| member-remove | any active member (self-removal = leave) | Local peer records for the removed member's nodes are revoked **in the same transaction** as applying the event, with an audit row |
| node add/rotate | the member themself | Binding updated |
| lane-profile / idp policy change | any active member (v1; role model is a v2 question) | Policy updated + consent posture re-printed on next `ee team status` |
| project share/adopt | any active member | Registry updated |

Events authored by a member whose local state is `removed` are rejected at
import with `team_member_removed_stream_rejected` (their pre-removal history
remains valid). Remove-vs-remove races are idempotent; remove-vs-add races
surface as manifest conflicts for `reconcile`. **Revocation latency is real
and stated:** under direct-from-origin acceptance, a removal event reaches
member C only when C syncs with the remover (v1 has no relay, so no other
member can forward it), so until then C keeps accepting the removed
member's stream. `ee team status` shows manifest staleness per member, and
the §8 threat table carries this bound honestly.

**P3.2 — Invite/join ceremony.**
`ee team invite [--ttl 72h] [--for "Priya"] [--wait]` mints
`{team_id, inviter node hint (MagicDNS name or tailnet IP), invite_id,
one-time secret, expiry}`, encodes as `eeteam1-<base32>`, stores the pending
invite (hashed secret only) locally, audits. `--wait` runs a foreground
accept loop in-process for the ceremony's duration, so joining works before
anyone has installed the daemon (M3 must not depend on M5; without `--wait`,
the inviter's daemon responder serves the join). `ee team join <code>`:
parse → probe tailnet → connect to inviter (bootstrap-envelope hello + invite
frames, §7.2 P1.1) → inviter verifies secret hash + TTL + single-use,
registers the new member + node binding, returns manifest → pairwise key
derived with ceremony nonces (§7.3.1) → joiner pairs with each *reachable*
member (inviter relays only *introductions*: member list with node keys from
the manifest; each pairwise key still requires a direct nonce-mixed exchange
with that member's node, protected by a TTL-bound per-pair introduction
secret the inviter distributes over the two already-authenticated channels —
inviter compromise at join time, including wholesale manifest fabrication
toward the joiner, is inside the threat model and documented) →
**unreachable members' pairings are deferred: owned by the steward job and
retried by every `ee team sync` run, surfaced as `unpaired` in `ee team
status`, re-issuable via re-introduction when the introduction secret
expires** → both sides enroll mesh peers with `trust_established_by =
"explicit_human_consent"` (the humans typed/sent the code — this resolves the
auto-enroll dead-end **without** laundering: `tailscale_auto_enrollment`
remains sync-ineligible) → default lane profile applied (metadata,
revisionNotice, curationSignal allow; body, embedding, graphLink deny) →
consent summary printed & audited on both sides → first sync round runs.
`--dry-run` previews everything after the hello without mutating.

**P3.3 — Membership ops.** `ee team members
[list|show|trust|rotate-key|reconcile]`, `ee team member add-node` (bind an
additional machine for yourself, via a self-invite variant), `ee team member
remove <member>` (revoke all their nodes' peer records locally + emit the
manifest removal event + shareWithdraw for team-shared material + honest
best-effort caveat), `ee team leave`. Every *other* member enforces the
removal when they apply the manifest event (§7.4 P3.1's same-transaction
revocation rule) — removal is not a remover-machine-only effect, but its
propagation is bounded by sync contact (stated in §8).

**P3.4 — Sharing ops.** `ee team share bodies --with <member>|--all-members`
drives real lane-grant preview → confirm → `ee mesh grant` (P0.4) per peer,
plus `ee share preview` integration for the outbound view. **Honesty gate:**
`ee team share bodies` ships in M4 *together with* body-lane transport
(§7.5 P4.6) — a grant ceremony for a lane nothing transports would be exactly
the surfaces-that-lie failure mode P0 exists to kill. If the command lands
before the transport for any reason, it must emit
`mesh_lane_transport_unavailable` instead of a silent no-op grant. Embedding
**and graphLink** lanes are deliberately **not** given team-UX verbs in v1 —
no task transports graphLink material in v1 either, so a verb for it would
recreate the same lie (power users can still use `ee mesh grant` directly,
which materializes the grant honestly for whenever a transport exists).

**P3.5 — Posture ops.** `ee team status` (members × reachability × last-sync ×
staleness × pending invites × lane matrix summary), `ee team sync [--now]`,
`ee team pause` / `ee team resume --confirm`, `ee team audit [--json]`
(filtered view over the existing audit ledger: consent, grants, membership,
sync summaries).

### 7.5 P4 — Unified retrieval

**P4.1 — Scope plumbing.** Add `--memory-scope` / `--strict-scope` to
`PackArgs` (and `pack build`), closing the README/graph-flags drift; task-lens
overlay keeps working, explicit flag wins. The `team` scope predicate
extends from "agent nickname list" to: **any local row (no `memberId` —
i.e. produced on this machine by me or my agents, today's
`current_agent`/`trust.team_members` matching preserved), plus any synced row
whose `memberId` resolves to an active team member.** Backward-compatible
with existing `trust.team_members` agent lists (document the key at last).
`scope_agent_unavailable` behavior unchanged.

**P4.2 — Attribution rendering.** Search/pack/ask/why surfaces render, for
team-synced items: member display name, project name, origin trust class,
synced-at, and the local trust class after elevation — in markdown packs as a
compact suffix (`· from Priya / acme-analysis · 2026-07-30T14:02Z`), in JSON
as a `teamProvenance` block. **Determinism rule:** pack/search surfaces render
absolute RFC 3339 `synced_at` only — relative phrasing ("2h ago") would make
pack bytes depend on wall clock, violating the byte-determinism invariant.
Relative phrasing is allowed solely in non-deterministic human surfaces
(`ee team status`, `ee team activity` human mode). `ee why` explains
elevation decisions ("arrived as peerHumanViaPeer from member mbr_…, elevated
to peer_human_verified because…").

**P4.3 — Team activity.** `ee team activity [--member X] [--project Y]
[--since N] --json` — a bounded, deterministic listing over synced metadata
(counts + titles/kinds + members + recency), answering US-4's "how will I know
what a teammate ran" without full-text search. Metadata-lane data only.

**P4.4 — Precedence and conflicts.** Pinned chain: **local workspace beats
team beats global** on contradiction, mirroring bd-1bfwa's
workspace-beats-global rule so the three lanes compose associatively. The
SRR6.37 peer duplicate/near-duplicate/contradiction detector (wire shape
pinned in `ee.peer_conflict.v1`, detector never implemented) is implemented
here for team-synced rows; conflicts appear in `ee insights` and pack DNA-style
explanations rather than being silently ranked away. Coordinate the
`memory_in_scope_with_tags` chokepoint edits with bd-1bfwa.3 (same file) —
whichever lands second rebases on the first; the precedence constant lives in
one place both cite.

**P4.5 — Index integration.** Team-synced metadata/bodies (once admitted by
policy) flow into the existing derived-index jobs that `ee mesh import`
already enqueues; verify incremental-intake behavior at team scale (500-row
sync bursts) and cap per-round index amplification at the existing 16-job
budget.

**P4.6 — Body-lane transport.** The lane that makes US-6 real: body events /
`body_fetch` frames over the signed transport, gated per event by outbound
policy + redaction + secret scan on the serving side (P1.3b machinery), with
the byte policy from T1.1 enforcing a streaming `max_bytes+1` cap on the
fetch side. This finally gives `remote_evidence.rs` (fetch planning),
`cache.rs` (retention/quota/eviction), and the `mesh_body_cache_metadata`
table their production callers — the eager-metadata / policy-gated-lazy-body
architecture SRR6.11 specified. `ee team share bodies` (P3.4) is gated on
this landing.

### 7.6 P5 — Operations

**P5.1 — Background sync steward.** A daemon-supervised `mesh-sync-steward`
job runs bounded anti-entropy rounds on an interval (default 300 s, jittered,
budget-capped; config `[mesh] sync_interval_seconds`), using the same core
round executor as the CLI (P1.4), and retries deferred member pairings
(§7.4 P3.2). It finally gives `steward_decision.rs` and `peer_state.rs`
(drift/staleness state machine) their production callers: missed rounds drive
`soft_stale`/`hard_stale` transitions surfaced in `ee team status`.
Explicitly opt-in-by-running-the-daemon. **Honest scope of "no daemon
needed":** foreground `ee team sync` fully replaces the *outbound/pull* side
— but being pulled *from* requires this machine's responder (see §6 listener
asymmetry). Two members who both never run a daemon cannot exchange at all;
`ee team status` says so rather than letting staleness look like a mystery.

**P5.2 — Daemon service install.** Non-technical users will not keep a
terminal open. `ee daemon install|uninstall|status` manages a per-user
service: launchd agent on macOS, systemd user unit on Linux. **Windows: the
daemon itself is `#[cfg(unix)]` today (`src/daemon/server.rs:25`), so v1
declares Windows members client-only (they pull; they are not pulled from)
and documents that posture; a TCP-listener-only Windows daemon variant (the
mesh listener needs none of the UDS machinery) is the named follow-up.** The
installer follows doctor-runtime mutation rules (backups, audit, undo path)
and never requires root. `ee team join` ends by offering the install command
(printed, not auto-run).

**P5.3 — Doctor + admission.** `ee doctor` gains team checks: responder
reachable from loopback, pairwise key file perms, member staleness, pending
invites expiring, manifest divergence, port conflicts. `admission.rs` (dead
931 LOC: rate limits, per-peer resource isolation) gets wired into the
responder accept path — inbound abuse (frame floods, oversized batches) is
where it was always needed.

**P5.4 — Perf and eval gates.** Two-node sync round p50/p99 budgets recorded
via the existing `ee.perf.v1` harness (new `mesh_sync` bench profile, advisory
first); retrieval-quality eval fixture asserting team-scoped pack selection
stays deterministic given a fixed synced corpus.

**P5.5 — Docs.** Rewrite `docs/mesh/operator_onboarding.md`'s fitness table
(the "several humans" row changes from "No by default" to "Yes, via ee team"),
new `docs/team/quickstart.md` written for the Hana/Priya personas (the
client-facing artifact: Tailscale install → ee install → create/invite/join →
what's shared → how to stop), agent-ux notes for team scope, ADR 0086 itself,
and CHANGELOG. Update the SRR6-era docs whose Status headers stay `proposed`
for surfaces this plan ships.

---

## 8. Security and privacy posture

Threat-model deltas on top of ADR 0037's ten rows (each new row keeps the
"control required" discipline):

| Threat | Control |
|---|---|
| Forged event origin over the wire | v1 direct-from-origin acceptance (§7.2 P1.3) + pairwise frame MACs; `mesh_relay_origin_rejected`. v2: per-origin stream signatures. |
| Invite code interception | Codes are single-use, TTL-bound, secret-hashed at rest, bound to the tailnet (join must arrive over a tailnet connection — and, when tier-1 identity is required, from the expected account's node), and revocable (`ee team invite revoke`). Interceptor must also be inside the tailnet — the layered requirement is documented, not assumed away. |
| Malicious/compromised member | Per-member revocation (US-7); lanes remain per-member so blast radius is the member's grants; trust elevation is per-member togglable; harmful-feedback demotion applies to synced rows like any other; emergency `ee team pause`. |
| Removal propagation latency | Under direct-from-origin acceptance, a removal event reaches member C only when C syncs with the remover; until then C keeps accepting the removed member's stream. Bounded by sync cadence; surfaced via manifest staleness in `ee team status`; every member enforces revocation in the same transaction as applying the event. Documented, not hidden. |
| Agent on a member's machine mints `human_explicit` → team-wide elevation | `human_explicit` is locally CLI-assignable, so elevation amplifies an unauthenticated local class. Controls: `ee why` shows elevation basis; per-member elevated-row counts in `ee team status`; per-member daily elevation velocity cap surfacing `team_member_elevation_burst` for review; per-member elevation toggle; harmful-feedback demotion. |
| Inviter fabricates the manifest toward a joiner | The joined-through inviter is trusted for the initial member list (v1, no manifest signatures). Mitigations: pairings still require direct nonce-mixed exchanges with each member's node; fabricated members can never complete pairing; `reconcile` + status expose divergent manifests. Removed by v2 stream signatures. |
| Tier-2 attestation trust link | Members other than the join counterparty trust the manifest-recorded OIDC attestation rather than re-verifying the token themselves (deliberate UX tradeoff, named here). |
| Compromised inviter at join time | Introductions are distributed by the inviter but each pairwise key requires a direct exchange with the introduced member's node; the residual MITM window during a member's *own* join is documented in the ADR as accepted v1 risk, removed by v2 signatures. |
| Membership manifest tampering | Manifest changes are per-origin ledger events (fork-rejecting, hash-chained, attributed); a member can only author manifest events in their own stream; removal/adds surface in `ee team status` and audit. |
| Trust-class laundering via import paths | P0.6 closes the JSONL/playbook bypass; elevation to `peer_human_verified` has exactly one, fully-audited path. |
| Data exfil via wider lanes | Unchanged consent machinery: preview → cautions → explicit confirm → audit, plus the hard secret-scan deny on every export path including transport sends. |
| Tailnet-membership creep (new devices appear) | Discovery policy still gates probes/responses; team sync only talks to *enrolled member nodes* regardless of who else is on the tailnet; unknown-node hellos to the responder get the privacy-preserving decline and an `unknown-attempt` audit trail. |
| Stolen/re-assigned node masquerading as a member | Tier-1 identity attestation: node ownership (SSO login per tailnet) is checked at join and revalidation; mismatch suspends grants. |
| IdP compromise or token theft (tier 2) | Device-flow tokens are used once at attestation time and never stored beyond the attestation record; verification pins issuer + client + audience + expiry + JWKS signature; revalidation cadence bounds the exposure window; suspension is reversible and audited. |
| Offboarded employee retains access | Directory-driven revalidation (US-12): removal from the IdP fails the next revalidation ⇒ grants suspended without relying on a human running `member remove`. Honest caveat unchanged: already-synced local copies on their machine are not remotely erasable. |
| IdP outage locks the team out | Identity checks run only in identity commands and revalidation; sync and retrieval continue on existing grants through the configured grace window (`identity_revalidation_overdue` warning first, suspension only after grace). |

Compliance story for the Marcus persona: `ee team audit --json` +
`ee share preview` consent rows + the hash-chained import ledger give a
complete, exportable record of who consented to what, what was granted, and
what synced when. Withdrawal caveat stays honest (best-effort beyond the local
node).

---

## 9. Schema, contract, and config changes (registry checklist)

Per AGENTS.md contract-drift rules, every item below lands with its gate:

- **New schemas** (`docs/schemas/` + drift tests): `ee.team.create.v1`,
  `ee.team.invite.v1`, `ee.team.join.v1`, `ee.team.members.v1`,
  `ee.team.status.v1`, `ee.team.activity.v1`, `ee.team.audit.v1`,
  `ee.team.manifest.v1` (event payloads), `ee.mesh.grant.v1`, **plus one
  schema per remaining `ee team` subcommand** — `ee.team.projects.v1`
  (T4.8), `ee.team.share.v1` (T4.5), `ee.team.sync.v1` and
  `ee.team.pause.v1`/`resume` (T4.6), `ee.team.idp.v1` and
  `ee.team.revalidate.v1` (T7.2/T7.4) — **per ADR 0086 TC-D15 every `ee
  team` subcommand emits its own schema; no subsumption.** The
  reserved-never-published `ee.mesh.peer_status.v1` name is retired
  (mechanism posture stays on `ee.mesh.auto_status.v1`/foreground status;
  team posture is `ee.team.status.v1`); the `ee.mesh.import_ledger.v1`
  inspection surface is owned by T1.3 (which writes the ledger decision
  columns), shipped with it or explicitly deferred in its closeout.
- **New degraded codes** (each with fixture + taxonomy entry, same commit):
  `mesh_relay_origin_rejected`, `mesh_transport_unreachable`,
  `mesh_frame_auth_failed`, `team_invite_expired`, `team_invite_replayed`,
  `team_member_unknown_node`, `team_manifest_conflict`,
  `share_preview_peer_unknown`, `team_daemon_not_installed` (info),
  `team_member_identity_mismatch`, `identity_revalidation_failed`,
  `identity_revalidation_overdue` (warning), `team_idp_unreachable`,
  `team_idp_token_invalid`, `team_member_removed_stream_rejected`,
  `team_member_elevation_burst`, `mesh_lane_transport_unavailable`, plus
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
  load-bearing here. Emitter map for the new codes:
  `mesh_transport_unreachable` (T2.1 session layer connect/timeout, reused by
  the T2.4 round executor), `mesh_frame_auth_failed` (T2.1 accept path:
  bad MAC / signed frame from an unkeyed peer), `team_member_unknown_node`
  (T4.1/T2.4 import path: stream from a node bound to no member),
  `team_daemon_not_installed` (info; join epilogue + `ee team status`,
  P5.2), `team_manifest_conflict` (T4.1 application; resolved via
  `reconcile`).
- **Config**: `[team]` section (`elevate_member_human_explicit`, defaults),
  `[mesh] sync_interval_seconds`; fix the documented-but-unread
  `[mesh.tailscale]` block one way or the other (read it, or fix README).
- **DB migrations** (append-only; shipped migrations are checksummed):
  **`mesh_origin_events` (the outbound origin stream — §7.2 P1.0, the
  foundational one)**, `team_members`, `team_member_nodes`, `team_manifest`
  cache, `workspaces.project_key(+source)`, pending-invites table, and the
  trust-class admission — which is a recreate-style table rebuild at every
  CHECK site (§7.3.2), not a constraint tweak. Pairwise keys live in a 0600
  keychain file under the user data dir, not in the DB.
- **Exit codes / envelope**: no changes; everything rides `ee.response.v2`.
- **Effect registry**: new team commands classified (`durable_write` for
  join/grant/member ops; `read_only` for status/activity/audit; sync is
  `durable_write` on the ledger).

---

## 10. Testing strategy

| Layer | What |
|---|---|
| Unit | Every new pure decision (invite codec, key derivation, elevation rule, project-key derivation incl. shallow-clone fallback, manifest event application, precedence constant) with happy/edge/error cases per testing policy. |
| Contract | Schema drift tests for every `ee.team.*` and changed mesh schema; degraded-code catalog ↔ fixture ↔ taxonomy sync (extends the J6 failure-mode catalog validator, `tests/contracts/failure_mode_fixtures.rs`). |
| Golden | `ee team status`/`members`/`activity` JSON goldens (server-path regen only, per the golden workflow); refreshed mesh status goldens after P0.5 de-hardcoding. |
| Integration (the centerpiece) | **Two-node loopback harness (P1.5)**: real binaries, real sockets, fake tailscale identities. Scenario matrix: pair → sync → attribute; partition → rejoin → converge (cursor/hole/fork scenarios from ADR 0041 as *behavior*, not model assertions); revoke mid-sync; removal propagation + removed-origin stream rejection; invite replay rejected; elevation on/off + elevation-burst cap; project adoption; policy-denied lane never crosses; secret-scan deny on transport send; accept-path flood test against the bootstrap-envelope caps. Three-node variant: direct-from-origin rejection of relayed events, introduction flow, third-member-joins-while-one-is-offline (deferred pairing completes via steward/sync retry). |
| Migration safety | Trust-class table rebuilds (§7.3.2) get a dedicated migration test asserting row counts and content hashes survive each recreate. |
| Determinism | Team-scoped pack/search determinism given a fixed synced corpus (extends the J7 determinism harness, `scripts/e2e_overhaul/determinism.sh` + `tests/determinism_unit.rs`); sync summaries excluded from determinism surfaces as designed. |
| Mesh-off regression | `mesh_off_no_network.rs` extended: daemon with mesh off binds nothing; `ee team` commands with mesh off fail with honest guidance, add zero degraded noise elsewhere. |
| Opt-in real-tailnet | `mesh_sync_once_real_tailscale.sh` upgraded to assert a real round; a new `team_join_real_tailscale.sh` (exit 78 skip-clean by default, same contract). |
| Fake IdP harness | Tier 1: fake-tailscale fixtures gain `UserProfile` owners (mismatch/reassignment scenarios). Tier 2: a local device-flow simulator (python, same pattern as the fake-tailscale socket responder) serving discovery/JWKS/device/token endpoints with rotatable keys — join, revalidation, expiry, group-denial, and outage-grace scenarios run fully offline. |
| Property | Fuzz the frame decode path (length-prefix truncation, oversize, bad MAC) and invite-code parser — both are new untrusted-input surfaces. |
| Perf | `mesh_sync` bench profile; two-node round latency + index amplification budgets (advisory → blocking per the existing maturation path). |

E2E scripts follow the existing `scripts/e2e_overhaul/` + `ee.test_event.v1`
logging conventions; RCH-remote for cargo-backed stages per repo policy.

---

## 11. Milestones and acceptance gates

| Milestone | Contents | Gate (all must hold) |
|---|---|---|
| **M0 — Truth & safety** | P0.1–P0.7 | bd-30o6g closed with streaming-cap test; export/import observably policy-gated; `ee mesh grant` exists preview-pinned; bypass closed (teammate export cannot inject `human_explicit`); zero uncovered mesh degraded codes; effect/README drift fixed; `verify.sh` green. |
| **M1 — Peers talk** | P1.0–P1.5 (incl. P1.3b) | Origin event stream durably records local mutations; two-node loopback harness green incl. partition/rejoin + fork rejection + flood test; responder-side serving applies outbound policy + secret scan on the wire (planted secret never crosses); `ee mesh sync --once` completes a real round (no deferred code) between two live instances; key storage in place (fixture-provisioned); frame + invite-codec fuzz/property suite green (T2.7); mesh-off binds no sockets; real-tailnet smoke green when opted in; `probe_mesh_capability` no longer `Unimplemented`. |
| **M2 — People & projects** | §7.3.1–§7.3.3 | Trust-class migration applied + all three enums + weights consistent; elevation path audited and togglable; git + non-git project keys derived/adopted; pairwise keys derived, stored 0600, rotatable. |
| **M3 — ee team** | §7.4 all except P3.4 body verbs | US-1/2/3/7/8/9 acceptance sketches pass as E2E (join works via `invite --wait` with no daemon installed); join ceremony ⇒ sync-eligible peers with `explicit_human_consent`; removal enforcement + removed-stream rejection proven; every team command emits schema-valid JSON + audit rows (US-10); invite replay/TTL enforced. |
| **M4 — Unified recall** | §7.5 all | US-4 passes: team-scoped search/pack with attribution on both nodes of the harness; `ee pack --memory-scope` shipped; precedence pinned + tested; conflict detector surfaces planted contradictions; **body-lane transport live and US-6 (`ee team share bodies`) passes end-to-end**; scope docs corrected. |
| **M5 — Operations** | §7.6 all | Background steward syncs on the harness without CLI involvement (US-5); `ee daemon install` works on macOS + Linux; doctor team checks; admission wired; perf profile recorded; quickstart doc validated by a cold run-through. |
| **M6 — SSO identity** | §7.3.4 both tiers | Tier 1 (US-11): probe parses node owners; `idp require --tailnet-attested` enforced at join + revalidation on the harness (mismatch ⇒ suspension + audit). Tier 2 (dependency decision resolved — §13 item 1): device flow against the fake IdP end-to-end via the curl backend; token verification rejects bad issuer/audience/signature/expiry; offboarding scenario (US-12) passes; outage-grace behavior proven. |

M0 → M1 → M2 → M3 → M4 → M5 is the spine; §12 marks the safe parallelism
inside each. M6 tier 1 can start alongside M3 (it needs member records + join,
not retrieval); M6 tier 2 is sequenced last (its crate additions land when it
starts, per §13 item 1).

---

## 12. Work breakdown (bead-conversion source)

Legend: `←` = depends on. IDs are placeholders resolved at bead-creation time.
Every bead below gets: full context paragraph, file anchors, acceptance
criteria, test obligations, and the AGENTS.md contract checklist inline.

**EPIC T0 — Team confederation program** (umbrella; children below)

**T0.0 ADR 0086** — write the ADR (decisions D-team-1…n from §7, rejected
alternatives, verification hooks). ← nothing. Blocks everything else.

**Sub-epic T1 — M0 Truth & safety**
- T1.1 Streaming byte-cap fix (absorbs bd-30o6g; coordinate/close that bead). ← T0.0
- T1.2 Wire outbound policy into `ee mesh export` + share-preview verdicts (P0.2+P0.3). ← T0.0
- T1.3 Wire `decide_mesh_import` into `ee mesh import` + ledger decision columns. ← T0.0
- T1.4 DB-backed preview-grant + new `ee mesh grant` (absorbs bd-2gvgw). ← T1.2
- T1.5 De-hardcode mesh status/report fields. ← T1.2, T1.3
- T1.6 Close JSONL/playbook trust bypass (store-local keyed MAC per P0.6 — the bare store-identity header was considered and rejected). ← T0.0
- T1.7 Degraded-code fixture/taxonomy backfill + empty audit tests + effect/README drift. ← T0.0 (parallel with all T1.x)

**Sub-epic T2 — M1 Transport**
- T2.0 Origin event stream substrate: `mesh_origin_events` migration, append rules wired into shared-scope mutations (same transaction), immutability contract. ← T0.0
- T2.1 Frame-transport session layer: length-prefixed signed frames over std::net TCP, connect/accept, budgets, kill-on-timeout, bootstrap (unsigned, bounded, rate-limited) envelope for pre-key hello/join, minimal accept-side caps (connection semaphore + per-peer frame budget), `mesh_peer_keys` keychain-file storage (fixture-provisioned in M1). First caller of `tailscale_transport.rs`. ← T0.0
- T2.2 Daemon-hosted hello responder job (binds; wires `decide_hello_response`; status becomes real; writes the discovery cache that `discovery_cache.rs` models). ← T2.1
- T2.3 Real client hello probe replacing ACL-capability synthesis. ← T2.1
- T2.4 Anti-entropy round executor + `TcpMeshForegroundSyncTransport` client (cursor advance, fork reject, direct-from-origin rule). ← T2.0, T2.2, T2.3, T1.1, T1.3
- T2.4b Responder-side anti-entropy serving: frontier answers + bounded `EventBatch` ranges from `mesh_origin_events`, per-event outbound policy + secret scan on the wire path, narrowed-response protocol. ← T2.0, T2.2, T1.2
- T2.5 Two-node loopback E2E harness: partition/rejoin/fork scenarios, flood test, policy-denied-lane and planted-secret never cross. ← T2.4, T2.4b
- T2.6 Real-tailnet opt-in smoke upgrade + capability graduation (`probe_mesh_capability`). ← T2.5
- T2.7 Frame/invite fuzz + property tests (incl. bootstrap envelope truncation/oversize/bad-MAC). ← T2.1 (parallel)

**Sub-epic T3 — M2 Identity**
- T3.1 `team_members`/`team_member_nodes`/pending-invites migrations + models. ← T0.0
- T3.2 Pairwise key ceremony derivation/rotation (nonce-mixed per §7.3.1; storage landed in T2.1). ← T3.1, T2.1
- T3.3 Trust class `peer_human_verified`: recreate-style migrations at every CHECK site + migration-safety test + three enums + weights + verified-scope + goldens. ← T0.0
- T3.4 Elevation rule at import (single audited path, policy toggle, per-member velocity cap + `team_member_elevation_burst`, status counts). ← T3.3, T2.4
- T3.5 Project identity: derivation (git/root-commit, shallow fallback, minted), workspace columns, hello `projectKeys[]`, policy matching. ← T3.1
- T3.6 v2 spike bead (non-blocking): per-origin stream signatures for relay + manifest signing; dependency decision writeup. ← T3.2

**Sub-epic T4 — M3 Team UX**
- T4.1 Manifest event model (authorization table per §7.4 P3.1) + local cache + application logic incl. same-transaction removal enforcement + removed-origin stream rejection, **plus the `ee team create` command itself** (team_id mint, create event as the manifest stream root, printed default-lane consent summary, `ee.team.create.v1`). ← T2.0, T3.1
- T4.2 Invite mint/parse/revoke (`eeteam1-` codec, TTL, single-use) + `invite --wait` foreground accept. ← T3.1, T2.1
- T4.3 Join ceremony end-to-end (bootstrap hello + invite frames, mutual enroll `explicit_human_consent`, manifest exchange, nonce-mixed pairwise keys, TTL-bound introductions, deferred-pairing bookkeeping, consent summary, first sync, `--dry-run`). ← T4.1, T4.2, T3.2, T2.4, T2.4b
- T4.4 Membership ops (members list/show/trust/rotate-key/reconcile/add-node/remove/leave + shareWithdraw on removal). ← T4.3
- T4.5 Sharing ops (`ee team share` → preview → grant pipeline; body verb gated on T5.9, emits `mesh_lane_transport_unavailable` if reached early). ← T4.3, T1.4
- T4.6 Posture ops (status/sync/pause/resume/audit; status shows unpaired members, manifest staleness, elevation counts, responder posture). ← T4.3
- T4.7 `ee team` E2E suite over the two-node harness (US-1..3, 7..9 + removal propagation; US-6 moves to T5.10). ← T4.3..T4.6, T4.8, T2.5
- T4.8 `ee team projects share|adopt|list` (US-9: minted project ids for non-git workspaces, adoption mapping, manifest registry). ← T3.5, T4.1

**Sub-epic T5 — M4 Unified retrieval**
- T5.1 `--memory-scope`/`--strict-scope` on `ee pack`/`pack build` + docs-drift fix. ← T0.0 (parallel-safe early)
- T5.2 Team scope over members (+`memberId` producer metadata) with `trust.team_members` compat; document the key. ← T3.1, T5.1
- T5.3 Attribution rendering (search/pack/ask/why `teamProvenance`, markdown suffix, elevation explanation). ← T5.2, T3.4
- T5.4 `ee team activity`. ← T4.1, T5.2
- T5.5 Precedence constant (local>team>global) + bd-1bfwa coordination + tests. ← T5.2
- T5.6 Peer conflict detector (SRR6.37 completion) + insights surfacing. ← T5.2
- T5.7 Index-intake integration + amplification budget verification at team scale. ← T2.4
- T5.8 Team retrieval determinism (absolute-timestamp attribution rule) + eval fixture. ← T5.3
- T5.9 Body-lane transport (P4.6): body events / `body_fetch` frames, serving-side policy+redaction+secret scan, streaming byte cap, `remote_evidence.rs` + `cache.rs` + `mesh_body_cache_metadata` wiring. ← T2.4b, T1.1
- T5.10 US-6 E2E: `ee team share bodies` end-to-end over the harness (grant ⇒ bodies flow; deny ⇒ they don't). ← T5.9, T4.5

**Sub-epic T6 — M5 Operations**
- T6.1 Background sync steward job (wires `steward_decision.rs`, `peer_state.rs` staleness; retries deferred pairings). ← T2.4, T2.4b
- T6.2 `ee daemon install|uninstall|status` (launchd/systemd user; Windows client-only posture documented; doctor-runtime mutation rules). ← T0.0 (parallel-safe)
- T6.3 Full admission control (`admission.rs`) wired into responder accept path, superseding T2.1's minimal caps. ← T2.2
- T6.4 Doctor team checks. ← T4.6, T6.1
- T6.5 Perf bench profile + budgets. ← T2.5
- T6.6 Docs: quickstart, operator-onboarding fitness-table update, agent-ux team notes, CHANGELOG. ← T4.7, T5.3
- T6.7 Program closeout: verification-matrix style ledger of every child, deferred items, and proof rows. ← everything

**Sub-epic T7 — M6 SSO identity**
- T7.1 Probe extension: parse per-node `UserProfile` owners (+ fake-tailscale fixture owners). ← T0.0 (parallel-safe)
- T7.2 Tier-1 tailnet attestation: `ee team idp require --tailnet-attested`, manifest policy, join-time + revalidation checks, suspension/grace posture. ← T7.1, T4.3
- T7.3 `ee team members revalidate` + tier-1 revalidation cadence in the steward job. ← T7.2, T6.1
- T7.4 Tier-2 OIDC device flow: discovery/JWKS/device/token client (curl subprocess backend per §13 item 1, DECIDED), `ee team idp set`. ← T7.2 (crate additions `rsa`/`p256` land at T7.4/T7.5 start per §13 item 1)
- T7.5 ID-token verification (issuer/audience/expiry/JWKS signature + iat/auth_time freshness + jti single-use; RustCrypto `rsa`/`p256`). ← T7.4
- T7.6 OIDC attestation in the join ceremony + member records + group-based authorization (join-counterparty verifier model). ← T7.5
- T7.7 Fake IdP harness + full offline scenario matrix (join, revalidate, expiry, group denial, outage grace, offboarding). ← T7.4 (parallel with T7.5/T7.6)

Cross-cutting rules for every implementation bead: inline `#[cfg(test)]` units;
RCH-remote verification for cargo stages; fixture+taxonomy in the same commit
as any new degraded code; schema + drift test in the same commit as any new
`ee.*.v1`; no new files beyond the module boundaries named here without
justification.

---

## 13. Open decisions for the operator (deliberately few)

1. **Cryptography/TLS dependency bundle** — **DECIDED 2026-07-30 (operator
   adopted the recommendation):** tier-2 HTTPS egress uses the **curl
   subprocess** backend (zero new crates; same pattern as the `tailscale`
   binary fallback); JWT verification uses pure-Rust RustCrypto **`rsa` +
   `p256`** (added to the tree when T7.4/T7.5 start, listed in the
   dependency-contract matrix then); **`rustls` is deferred entirely** (the
   `EE_TEAM_IDP_HTTP_BACKEND` env var stays registered with `curl` as its only
   shipped value, reserving `native` for a future decision). The remaining
   sub-decision — an Ed25519 crate for v2 relay stream signatures — stays open
   until the T3.6 spike writeup recommends one; nothing in v1 blocks on it.
2. **Default for `elevate_member_human_explicit`** — **DECIDED 2026-07-30
   (operator adopted the recommendation): ON** for invite-ceremony members
   (the ceremony is the consent), with the T3.4 amplification controls
   (elevation-basis in `ee why`, per-member counts in status, daily velocity
   cap + `team_member_elevation_burst`) shipping in the same slice as the
   default.
3. **Windows background service** — there is no Windows daemon to schedule
   (the daemon is `#[cfg(unix)]`; P5.2 declares Windows members client-only
   in v1). v1 documents manual Task Scheduler invocation of foreground
   `ee team sync` for freshness; promoting a TCP-listener-only Windows daemon
   variant to a native service is the named follow-up scope call.
4. **`ee context` alias** — team scope lands on `pack`/`search`/`ask`; the
   soft-deprecated `context` alias inherits via shared code, no extra work
   planned.
5. **SSO default posture** — should `ee team create` nudge toward
   `idp require --tailnet-attested` when the probe shows a corporate tailnet
   (printed suggestion only), or stay silent? Plan says: print the suggestion,
   never auto-enable.

---

## 14. Appendix: absorbed hygiene fixes

Captured here so they're not lost if milestones reorder: effect-registry mesh
path/table corrections; README `[mesh.tailscale]` + `.ee/mesh/` + scope-flag
drift; stale schema description on lane-grant preview; `probe_mesh_capability`
`"1"` handling; `self_advertised_tags` hardcoding; three 0-byte audit test
files; 19 uncovered degraded codes; `mesh_anti_entropy_transport_unavailable`
defined-but-never-emitted (either emit or delete); `docs/mesh/local_two_node_demo.md`
replaced by the real harness; SRR6-era `Status: proposed` headers updated as
surfaces ship.
