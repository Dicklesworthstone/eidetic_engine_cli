# ADR 0086: Team Memory Confederation

Status: accepted (decisions final; implementation tracked by bd-tc-epic-qzk7o)
Bead: bd-tc-epic-qzk7o (program epic; this ADR is bd-tc-epic-qzk7o.1)
Plan: [`docs/mesh/team_confederation_plan.md`](../mesh/team_confederation_plan.md)
Related: ADR 0037 (optional mesh), 0038 (auto-enrollment), 0041 (anti-entropy),
0009 (trust classes), 0069 (global knowledge lane), 0083 (user-global store)
Date: 2026-07-30

## Context

`ee` is single-operator today. The mesh subsystem (SRR6 / SRR6.46) shipped a
large, well-tested set of pure decision modules — frame codec, hello protocol,
anti-entropy planner, per-lane policy engine, discovery policy, consent
rituals — but no peer-to-peer data path: no listener exists, `ee mesh sync
--once` runs a no-op transport, discovery parses Tailscale ACL capability
metadata nothing publishes, and the policy engine has zero production callers.
Product demand (teams of non-technical analysts) is for **team memory**: N
humans, each running `ee` locally, sharing one unified, attributed team memory
over a trusted tailnet, with setup a non-technical user can complete.

Three primitives are missing and cannot be retrofitted implicitly: a human
member identity (all current axes are agent-nickname, machine, or
path-derived workspace), a cross-machine project identity (workspace IDs are
absolute-path hashes), and a trust tier for a teammate's deliberate
`ee remember` (inbound peer material is hard-capped at `agent_assertion` /
`agent_validated`; `human_explicit` never crosses).

This ADR records the decisions. The plan document holds the full design,
milestones, and task DAG; the two must not drift — where they conflict, this
ADR wins and the plan gets corrected.

## Decisions

### TC-D1 — One listener, one port, one frame protocol

All ee↔ee traffic (hello, anti-entropy, body fetch) runs over a single
daemon-hosted TCP listener bound to the tailnet IP on `EE_MESH_HELLO_PORT`
(default 41888). All **post-enrollment** traffic speaks length-prefixed
signed frames via the existing codec (pre-key bootstrap traffic is the
TC-D2 exception) (`src/mesh/tailscale_transport.rs`: blake3-keyed signatures, capability
allowlist `hello`/`summary`/`event_fetch`/`body_fetch`, 64 KiB frame /
32 KiB payload budgets, constant-time compare). Implementation is std::net +
asupersync supervision — the forbidden-dependency rule bans HTTP *stacks*
(hyper/axum/tower/reqwest), not sockets; precedent is the hand-rolled
LocalAPI HTTP client in `src/core/tailscale_probe.rs:887–949`.
**Rejected:** a second port or protocol per message family (doubles the
firewall/ACL story); any HTTP/gRPC transport (ADR 0037's rejection stands).

### TC-D2 — Bootstrap envelope for pre-key traffic

Frame signing is pairwise-keyed, so first contact (hello probe of a stranger;
the join ceremony) cannot be signed. Pre-enrollment traffic uses a distinct
**unsigned but strictly bounded** envelope: its own ≤4096-byte budget,
aggressive rate limiting, capabilities limited to `hello`/`join`. All
post-enrollment traffic requires signed frames; signed-capability frames from
unkeyed peers are rejected (`mesh_frame_auth_failed`). Minimal accept-side
caps (connection semaphore + per-peer frame budget) ship with the listener,
not later; full `admission.rs` wiring follows in operations.
**Rejected:** signing bootstrap frames with an invite-derived provisional key
(complicates the codec for no gain — the invite secret already authenticates
the ceremony at a higher layer).

### TC-D3 — Origin event stream substrate

Each node keeps a durable, append-only, per-origin sequence of its **own**
events in a new `mesh_origin_events` table: `origin_seq` contiguous per
(workspace, origin stream), hash-chained via `prevEventHash` per
`docs/mesh/event_schema.md`, rows never updated or deleted (corrections are
new events). Shared-scope memory mutations and team-manifest operations
append events **in the same transaction** as the mutation. Event-kind
vocabulary follows `docs/mesh/event_schema.md`: v1 emits `create`, `revise`
(the schema's revision kind — there is no `update` kind), `tombstone`, and
`shareWithdraw`; the schema's `trust`,
`validity`, and `bodyAvailable` kinds are **explicitly deferred** — trust
and validity propagation are post-v1 tracks, and body-fetch eligibility is
computed serve-side at fetch time (TC-D12) rather than signaled by a
`bodyAvailable` event. **Manifest operations do not extend the envelope:**
`ee.mesh.event.v1` has a closed `eventKind` enum and it stays closed.
Manifest ops are encoded as ordinary `create`/`revise` events over
manifest-scoped logical IDs spelled `mem_team:<kind>:<id>` (satisfying the
schema's `^mem_` pattern — one logical document per member record, node
binding, project record, and the lane-profile document), with `contentHash`
over an `ee.team.manifest.v1` payload that travels **inline in the event**
(manifest records are small; they ride the metadata lane within the event
payload budget and never depend on `body_fetch` or body-lane grants —
otherwise the default-deny body lane would block manifest replication);
member removal is a `revise` of the member record to `state=removed` (a
payload-level operation — `tombstone` keeps its memory semantics). Every
manifest event carries `requiredFeatures: ["mesh.team.manifest.v1"]`
(inside the schema's mandatory `mesh.` feature namespace), so pre-team
peers structurally quarantine them via the existing forward-compat gate
instead of misapplying them; the wire schema and its drift tests are
untouched. This is what tips advertise, ranges address,
and fork rejection chains over; deriving events on demand from mutable tables
is rejected because any later edit would present as a fork to peers.

### TC-D4 — Direct-from-origin acceptance (v1); signatures are v2

A connection authenticated (pairwise key) as peer X may deliver only events
whose origin is X; relayed origins are rejected (`mesh_relay_origin_rejected`).
This closes the forged-origin threat without asymmetric cryptography, at the
cost of relay-assisted partition healing. Per-origin Ed25519 stream
signatures (enabling relay and independently-verifiable manifests) are an
explicit v2 track; the crate choice is owed by the bd-tc-epic-qzk7o.4.6
spike. **Rejected for v1:** trusting relayed events on the relayer's
authority (peer impersonation by construction).

### TC-D5 — Pairwise symmetric keys, nonce-mixed at the ceremony

Per-pair long-term keys are derived during join with fresh 32-byte nonces
from **both** sides:
`k_AB = blake3::derive_key("ee.team.pair.v1", invite_secret ‖ nonce_A ‖ nonce_B ‖ nodekey_A ‖ nodekey_B)`,
where `‖` denotes a canonical concatenation with each variable-length field
length-prefixed (u32-LE), so field boundaries are unambiguous in the
derivation input. The invite secret authenticates the exchange but does not
determine the key:
an attacker holding the old invite message (chat history is forever) must
actively MITM the WireGuard path at join time. Secrets and nonces are
destroyed after derivation. Keys live in a 0600 keychain file under the user
data dir (never the workspace, never git); `MeshPeerKey` records the
fingerprint; rotation re-derives with fresh nonces over the authenticated
channel. Introduction-secret exchanges (meeting non-inviter members) use the
same construction, so the inviter cannot derive pair keys it introduced.
**Rejected:** deriving from the invite secret alone (offline-derivable
forever after); asymmetric identity keys in v1 (no signature crate in the
approved dependency set; direct-from-origin makes MACs sufficient).

### TC-D6 — Member identity is a first-class primitive

New tables: `team_members` (`mbr_*` id, display name, state, added_by,
timestamps, `contact_hint` display-only) and `team_member_nodes` (member ↔
node-key bindings with provenance). Member identity is machine-anchored
(node bindings + pairwise keys), never env-var-anchored; `EE_AGENT_NAME`
remains an unauthenticated agent label within one member's swarm. Producer
metadata gains an optional `memberId` so synced rows attribute to people.
A machine belongs to at most one team per workspace in v1.

### TC-D7 — Trust class `peer_human_verified`; `human_explicit` stays local

A sixth trust class sits between `agent_validated` and `human_explicit`
(initial confidence 0.75, ask retrieval weight 0.92, included in `verified`
scope). Semantics are exactly what is attestable: "arrived over a channel
authenticated to a node bound to an active member, and that member's store
classed it `human_explicit` at origin." Elevation has exactly one audited
path, at import, requiring all of: trust lane `peerHumanViaPeer` with source
class `human_explicit`; delivering connection bound to an active member; and
local policy `elevate_member_human_explicit` (default **on** for
invite-ceremony members — the ceremony is the consent; per-member togglable).
Amplification controls ship in the same slice: elevation basis in `ee why`,
per-member elevated-row counts in `ee team status`, and a per-member daily
velocity cap (`team_member_elevation_burst`). The three existing rejection
points that keep `human_explicit` from ever crossing remain intact and
regression-tested. The trust-class admission is a recreate-style rebuild at
every `trust_class` CHECK site via new migration IDs (shipped migrations are
checksummed and never edited), with a migration-safety test.
**Rejected:** letting `human_explicit` cross with member authentication
(destroys the invariant "top class = typed on THIS machine"); a lower cap
only (buries the product's headline value — teammates' rules ranking like
rules).

### TC-D8 — Project identity decouples "same work" from local paths

`project_key` on workspaces: git repos derive `prj_git_*` from the
root-commit set (stable across clones); shallow clones fall back to
`prj_rem_*` from the normalized origin URL with a degraded note; non-git
workspaces mint `prj_tm_*` at team-share time, distributed via the manifest.
Derivation source is recorded and never silently mixed. Hello carries
`projectKeys[]` alongside workspace IDs; peer-policy origin checks accept
project-key matches, replacing the manual n×n `origin_workspace_ids` mapping
for the common case. Raw local paths never cross the wire (existing
invariant). **Rejected:** remote-URL-only identity (breaks on renames and
mirrors); path-derived identity (the problem being solved).

### TC-D9 — Team manifest = replicated per-origin events + authorization table

The manifest (members, node bindings, project registry, team lane profile,
idp policy) replicates as ordinary origin-stream events on the metadata lane
— attributed, ordered, fork-rejecting. It is **not** a CRDT. Authorization
is enforced at event application: member-add / member-remove /
lane-profile / idp-policy / project ops by any *active* member; node ops by
the member themself; self-removal = leave. Events from a locally-removed
member's stream are rejected (`team_member_removed_stream_rejected`);
applying a removal event revokes the removed member's peer records **in the
same transaction**; remove-vs-add races surface as manifest conflicts for
explicit `ee team members reconcile`. **Removal also disarms the removed member's
granted authority, operationally** (there is deliberately no cross-stream
causality in this design — per-origin sequences only — so the rule avoids
"concurrent" entirely): member-add events from the removed member's stream
that arrive *after* the removal has been applied locally are already
rejected by the removed-stream rule above; for member-adds that were applied
*before* the removal arrived, applying the removal flags every active member
whose add was authored by the removed member (`added_by_removed_member`) in
`ee team status` and `reconcile`, with an audit row, requiring explicit
operator confirmation to clear. Grants are **not** auto-suspended — a
departed founder's legitimate invitees must not break the team on an
ordinary departure — so the residual sock-puppet window is "operator ignores
a loud, persistent flag," which is the honest v1 posture without signatures;
v2 signatures close it. Three mechanics pinned so this is implementable as
written: (a) **rejected-event cursor disposition** — removed-stream events
are terminally skipped per the shipped SRR6.32 quarantine semantics
(`mesh_event_quarantined` + `SkipWithAudit` in the executable model; the
implementation persists the disposition, and the cursor advances past
skipped events with an audit trail), so remaining members never re-request
dead ranges (ADR 0041's
bounded-retry invariant holds) while a mistaken removal remains recoverable
by quarantine replay after reconcile; (b) **re-join** — a removed member's
node re-joining via a fresh invite mints a **new** `member_id`, and event
acceptance resumes only from the post-rejoin frontier; removed-window events
stay terminally skipped in quarantine; (c) **flag clearing** — confirming an
`added_by_removed_member` flag is a local per-node operator action via
`ee team members reconcile`, recorded in the local audit log and **not
replicated** (each operator vouches for their own node's view; a replicated
clear would let one member silently vouch for everyone). Removal propagation is bounded by sync contact with the remover (v1
has no relay) — stated, surfaced as manifest staleness, and accepted. **Rejected:** CRDT membership (ADR 0041's rejection
of merge semantics stands); remover-machine-only revocation (unbounded
exposure); role hierarchies in v1 (a ≤20-member trusted team does not need
them yet; revisit with v2 signatures).

### TC-D10 — The join ceremony is the consent event

`ee team join <invite-code>` enrolls both machines with
`trust_established_by = "explicit_human_consent"` — the humans minted and
typed the code. This resolves the auto-enroll trust dead-end without
laundering: zero-touch `tailscale_auto_enrollment` peers remain
sync-ineligible by design. Invites are single-use, TTL-bound (default 72 h),
hashed-at-rest, revocable; `ee team invite --wait` runs a foreground accept
loop so joining never requires a daemon install. Deferred pairings (offline
members) are owned by the steward and retried by every `ee team sync`,
surfaced as `unpaired`. Inviter compromise at join time (including manifest
fabrication toward the joiner) is inside the threat model, stated precisely:
fabricated member bindings to nodes the inviter does **not** control can
never complete the direct nonce-mixed pairing; a sock-puppet member bound to
an inviter-controlled node **can** pair normally, and is caught by manifest
divergence against other members (`reconcile`) rather than by the pairing
itself — v2 signatures close the window.

### TC-D11 — Pull-only transport; listener asymmetry stated, not hidden

Every exchange requires the counterparty's responder. A member whose machine
never runs the daemon is pull-only (read-mostly to the team); two such
members cannot exchange at all. `ee team status` says so plainly. The daemon
is `#[cfg(unix)]`; Windows members are client-only in v1 (documented;
TCP-listener-only Windows daemon is the named follow-up). Foreground
`ee team sync` fully covers the pull side without a daemon (CLI-first
invariant intact). **Rejected:** hiding the asymmetry behind "sync just
works" prose; making any core command daemon-required.

### TC-D12 — Bodies travel out-of-band via `body_fetch`, never in origin events

Origin events carry metadata shapes only (create/revise/tombstone/
shareWithdraw + manifest ops — TC-D3). Body material moves as **policy-gated
lazy fetches** (`body_fetch` frames) keyed off the `create`/`revise` events'
`content_hash`; fetch *eligibility* is decided serve-side at fetch time by
the outbound policy (no `bodyAvailable` signal event in v1). **Fetch
execution and retry are bounded and local-first:** fetches run in the sync
path (foreground `ee team sync` or the steward's rounds), never
synchronously inside pack/search — retrieval only consumes already-cached
bodies and never blocks a read on an in-flight fetch. Denied or unavailable
outcomes are recorded in `mesh_body_cache_metadata` with a retry-after
posture in the same capped shape as anti-entropy ranges (1 s → 60 s, max
5 attempts, then blocked-with-`retry_after` until the next operator refresh
or policy change), and at most one attempt per `content_hash` per sync
round — a permanently-denied body costs one bounded probe per round, not
one per retrieval. Fetched
bodies are verified against the origin event's `content_hash`, chunked
within the 32 KiB payload budget,
capped by the streaming `max_bytes+1` policy, landed in the body cache
(`mesh_body_cache_metadata` + `cache.rs` retention/eviction), and admitted
through the same import policy chokepoint. Rationale: (a) anti-entropy
stays bounded — bodies would blow the 512-event batch and payload budgets;
(b) serve-time policy/redaction/secret-scan applies to every fetch, so policy
changes affect future fetches without rewriting streams; (c) **immutable
ledger bodies would contradict `shareWithdraw` purge semantics** — a
withdrawal cannot purge what an append-only origin stream permanently
embeds; (d) it is the SRR6.11 eager-metadata/lazy-body architecture the
cache modules were built for. Named residual: the `content_hash` of a
withdrawn body persists forever in the append-only stream, so a peer who
already holds (or later guesses byte-exactly) the content can confirm it
post-purge — purge removes bodies, not the ability to recognize them.
**Rejected:** a body event kind in `mesh_origin_events` (reasons a–c);
widening the frame codec budgets.

### TC-D13 — SSO member identity in two tiers

Tier 1 (zero new dependencies): tailnet-attested identity — tailnets already
authenticate through Google/Microsoft/Okta/OIDC, and the local `tailscaled`
knows each node's owning account. `ee team idp require --tailnet-attested
[--domain …]` binds member records to node-owner logins, checked at join and
revalidation; mismatch **suspends** grants (reversible, audited). Tier-1
data is used only to suspend, never to grant — "Tailscale is not trust"
stands. Tier 2 (decided 2026-07-30): direct OIDC via **device authorization
grant only** (RFC 8628), HTTPS egress via the **curl subprocess** backend
(`EE_TEAM_IDP_HTTP_BACKEND` ships `curl` as its only value; `native`
reserved and rejected at parse); JWT verification via pure-Rust RustCrypto
`rsa` + `p256` (approved; land when tier-2 work starts) checking issuer,
audience, expiry, JWKS signature, `iat`/`auth_time` freshness, and
`jti`-single-use — device flow has no nonce to bind, and that substitution
is documented rather than papered over. The join counterparty verifies and
records the attestation; other members trust the manifest record (named
trust link; every-member verification is UX-hostile). IdP calls happen only
in explicit identity commands; outage degrades with grace
(`identity_revalidation_overdue` before suspension). **Rejected:** rustls
in-tree (deferred entirely); authorization-code flow with a localhost
redirect server; treating tailnet attestation as a grant signal.

### TC-D14 — Import trust is authenticated by a store-local MAC

`ee export` artifacts are MAC'd (blake3-keyed, store-local secret in a 0600
keychain file) over header + content manifest. `ee import jsonl` honors
native trust (including `human_explicit`) only when the MAC verifies against
the local store key; absent/invalid/foreign MACs get external handling and
`human_explicit` is refused. `ee playbook import` caps at `agent_validated`
unless the same MAC passes. This closes the pre-existing bypass where any
teammate's export imported `human_explicit` unauthenticated. **Rejected:**
store-UUID comparison (identifiers leak via support bundles; a leak reopens
the bypass verbatim).

### TC-D15 — Schema and status-surface policy

Every `ee team` subcommand emits its own versioned schema (`ee.team.*.v1`)
with a drift test — no subsumption; the repo's per-surface schema discipline
wins over saving files. The reserved-never-published `ee.mesh.peer_status.v1`
name is **retired**: mechanism-level posture stays on the existing
`ee.mesh.auto_status.v1` / foreground status surfaces; team-level posture is
`ee.team.status.v1`. The `ee.mesh.import_ledger.v1` inspection surface is
owned by the import-policy bead (bd-tc-epic-qzk7o.2.1) — shipped with it or
explicitly deferred in its closeout, never silent. Retrieval surfaces render
absolute RFC 3339 timestamps only (byte-determinism); relative phrasing is
allowed solely in non-deterministic human surfaces (`team status`/`activity`).

### TC-D16 — Precedence: local workspace > team > global

On overlap, more-specific context wins (mirrors bd-1bfwa's
workspace-beats-global). On contradiction, neither silently wins — the pair
routes to the conflict surface labeled by lane; pack assembly never resolves
cross-lane contradictions by rank. The precedence constant lives in one
module cited by both the team and global lanes.

## Threat-model delta (controls required, extending ADR 0037)

| Threat | Control |
|---|---|
| Forged event origin | TC-D4 direct-from-origin + pairwise MACs; v2 signatures |
| Invite interception / chat-history replay | Single-use, TTL, hashed at rest, revocable; TC-D5 nonce mixing keeps old codes from determining keys |
| Compromised member | Data-lane blast radius is the member's grants (per-member lanes, elevation toggle, harmful-feedback demotion, revocation). Manifest authority (lane profile, idp policy, removals) is any-active-member in v1 — mitigated by audit + conflict surfacing + `ee team pause`; roles are a v2 question |
| Removed member's pre-authored sock-puppet add | TC-D9: post-removal arrivals rejected by the removed-stream rule; pre-removal-applied adds flagged `added_by_removed_member` in status/reconcile pending explicit confirmation (no auto-suspend — ordinary departures must not break their invitees); v2 signatures close the window |
| Removal propagation latency | TC-D9 same-transaction enforcement on application; staleness surfaced; bound documented |
| Local `human_explicit` minting amplified team-wide | TC-D7 controls: basis in `why`, per-member counts, velocity cap + burst code |
| Inviter fabricates manifest at join | Direct nonce-mixed pairing defeats fabricated members; conflicts surface; v2 signatures close the window |
| Stolen/reassigned node | Tier-1 attestation suspends on owner mismatch |
| IdP outage or token theft | Identity commands only; freshness + jti single-use; grace-then-suspend |
| Trust laundering via file import | TC-D14 store-local MAC |
| Inbound abuse on the open port | TC-D2 bootstrap caps at listener birth; full admission control in operations |

## Non-goals (v1)

No cloud relay or SaaS control plane; no CRDTs, gossip, Paxos, or
linearizability; no eager full replication or federated search; no relay
(v2); no multi-team membership per workspace; no web UI; no wizard TUI; no
Windows daemon; no embedding or graphLink team-UX sharing verbs (lanes exist,
`ee mesh grant` remains the power-user path); per-peer selective-sync
subscriptions stay display-only; no IdP round-trip in any core command.

## Verification hooks

- Mesh-off invariants: `tests/mesh_off_no_network.rs` extended — daemon with
  mesh off binds nothing; `ee team` commands add zero degraded noise.
- Two-node loopback harness (real binaries, real sockets): pairing, sync,
  attribution, partition/rejoin, fork rejection, hole blocking, removal
  propagation, flood caps, policy-denied lane and planted secret never cross.
- Three-node scenarios: relayed-origin rejection; deferred pairing completes.
- Migration-safety test: row counts + content hashes survive the trust-class
  rebuilds.
- Determinism: J7 harness extended with team-attributed packs (absolute
  timestamps); byte-identical output on both nodes given equal admitted state.
- Degraded-code discipline: every new code lands with fixture + taxonomy in
  the same commit (J6 validator); the 19 uncovered legacy mesh codes are
  backfilled first.
- Fuzz/property: frame decode, bootstrap envelope, invite codec.
- Import authentication (TC-D14): a teammate's export cannot inject
  `human_explicit`; an artifact with a correct store UUID but no valid MAC
  is refused; own backup restore round-trips at full trust (M0 gate).
- Project identity (TC-D8): two clones at different paths derive equal keys;
  shallow-clone fallback annotated; non-git mint + adopt round-trips.
- Precedence and conflicts (TC-D16): planted cross-lane contradictions are
  surfaced, never resolved by rank; the precedence constant is imported by
  both team- and global-lane tests.
- Elevation controls (TC-D7): harness matrix covers elevation on/off, the
  per-member velocity cap tripping `team_member_elevation_burst`, and the
  three `human_explicit` rejection points asserted unchanged.
- Opt-in real-tailnet smokes assert real rounds/joins when enabled, exit-78
  skip-clean otherwise.
- Fake IdP harness covers every tier-2 scenario offline.

## Consequences

Positive: the ~8.4k LOC of dead mesh modules gain production callers (all
except `anti_entropy_model.rs`, which stays an executable spec); the
mesh's consent/audit machinery becomes the team product's foundation instead
of shelf-ware; non-technical teams get a three-command setup with a
compliance-grade audit trail. Costs: new migrations including multi-table
trust-class rebuilds; a listener surface that must be defended (bootstrap
caps + admission); two new identity registries (members, projects); ~15 new
degraded codes and ~12 new schemas with their gates; documentation whose
fitness tables and status headers must flip as surfaces ship. The plan's
milestone gates (M0–M6) are the acceptance contract; the program closeout
bead owes a verification-matrix-style ledger of every child and every
deferral.
