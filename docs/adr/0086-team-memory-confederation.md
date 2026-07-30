# ADR 0086: Team Memory Confederation

Status: accepted (decisions final; implementation tracked by bd-tc-epic-qzk7o)
Bead: bd-tc-epic-qzk7o (program epic; this ADR is bd-tc-epic-qzk7o.1)
Plan: [`docs/mesh/team_confederation_plan.md`](../mesh/team_confederation_plan.md)
Related: ADR 0037 (optional mesh), 0038 (auto-enrollment), 0041 (anti-entropy),
0009 (trust classes), 0069 (global knowledge lane), 0083 (user-global store)
Date: 2026-07-30
Last amended: 2026-07-30 (independent implementation-readiness review,
landed as b34bacbd + the Git-probe hardening delta; then the post-amendment
audit pass — feature-gate pinning, validity-field carry-forward, cutoff-model
emitter for team_member_removed_stream_rejected, not-a-CRDT and
absolute-timestamp rules reinstated, rematerialization pinned with an owning
bead, RSA policy pinned, and the Ed25519-in-v1 reversal split into plan §13
item 1b — RATIFIED by the operator 2026-07-30, so the signature/relay
dependencies are approved and every TC-D that builds on them is fully
decided; recorded by bd-tc-epic-qzk7o.9 and commit 54ee80b8. The subsequent
audit pinned crash-safe non-downgrading pair rotation, invite/ceremony ID
entropy, symmetric lane revocation, delegated-member review truthfulness, and
the rematerialization publication fence; the final pass superseded the
rotating-key frame-v1 identity, added operation-specific feature gating,
durable revocation fences, rollback-safe rotation grace, and honest
local-source unshare scope)

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

All ee↔ee traffic (hello, anti-entropy, body fetch) runs through one local
responder owner and one TCP port, bound only to the locally verified tailnet
address(es) on `EE_MESH_HELLO_PORT` (default 41888), never a wildcard
address. The daemon normally owns the listener. When no daemon is running,
`ee team invite --wait` may own the same responder under an exclusive local
lease; when the daemon already owns it, `--wait` registers the invite through
the daemon control channel and waits instead of trying to bind a second
socket. A daemon-start/foreground-start race has exactly one lease winner;
an unrelated port occupant is a hard, diagnosed conflict, never a fallback
to another port or wildcard bind.

`teamCreated` commits one validated non-privileged `hello_port`
(`1024..=65535`, default 41888) into the
team root and invites carry it. It is a transport locator, not identity, but
it is immutable for that team in v1 so a peer never guesses which remote port
to dial. Every responder-capable member and every local team route behind one
broker must use that value; joining from a differently configured host is
allowed only in explicit client-only posture until its local responder
matches. All teams multiplexed by one broker must likewise agree on the port.
V1 has no in-band team-port migration; changing the team-wide value requires
an explicit replacement team/root (a versioned migration is deferred), while
a locally mismatched member simply remains client-only until reconfigured.
Because a TCP port on one Tailscale IP is host-wide while the control/data
stores are user-scoped, v1 supports exactly
one responder-capable OS user per Tailscale node; another local user is
client-only or uses a distinct Tailscale node. Status/doctor state this
limitation and diagnose the owner rather than implying that two per-user
daemons can bind the same address.

The owner is a **user-scoped responder broker**, not a listener tied to the
workspace that happened to start it. It extends the existing hardened local
workspace registry with responder routes keyed by exact
`(team_id, target_workspace_id)` and pending invite ID. Here
`target_workspace_id` is the responder's registered local database; it is
distinct from an origin event's producer-owned `origin_workspace_id`.
Registration comes
only from a same-EUID client over a bounded Unix-domain control channel in
the existing private runtime directory; requests bind workspace ID, team
genesis hash, database identity, request nonce, and expiry, contain no invite
secret or memory body, and are idempotently response-correlated. Before
serving, the broker revalidates that the registered canonical database path
is non-symlinked, owner-safe, and still contains the named workspace/team
root. Network input can select only an already-registered exact route; it can
never supply a filesystem path or trigger a scan of local databases.
Missing, stale, ambiguous, or moved routes fail closed with
`mesh_responder_route_unavailable` and redacted repair guidance. A foreground
owner hosts the same broker/control contract for its lifetime, so a second
workspace can register without binding a second port. Each request mutates at
most one workspace database; this does not create a cross-shard transaction.
The broker binds only while at least one validated route is mesh-enabled:
pausing one workspace removes only that route, zero enabled routes means no
listener, and `EE_MESH_HELLO_RESPONDER_DISABLED=1` remains the user-wide kill
switch.
The broker also revalidates its local Tailscale address set at startup and on
network-map/interface changes. Loss of every verified tailnet address closes
the listener and reports `mesh_transport_unreachable`; a changed set drops
stale sockets before binding only the newly verified addresses. Startup while
`tailscaled` is unavailable retries under supervision with the same
coalesced posture, without ever binding a wildcard or stale invite hint.

All **post-enrollment** traffic speaks length-prefixed,
session-authenticated frames (pre-key bootstrap traffic is the TC-D2
exception). The dead `ee.mesh.tailscale_transport_frame.v1` codec is useful
scaffolding, but its `sourceNodeKey`/`targetNodeKey` fields encode rotating
Tailscale public keys as if they were durable endpoint identity, and its MAC
uses the long-term pair key directly. It is therefore superseded **before its
first production caller** by `ee.mesh.tailscale_transport_frame.v2`, not
silently reinterpreted. V2 names random ee `sourceNodeId`/`targetNodeId` and
binds `teamId`, source/target workspace IDs, session ID, direction, monotonic
u64 counter, request/response correlation, capability, bounded requested
budget, and payload hash in a versioned type-tagged/length-prefixed MAC
preimage. The authenticated session transcript binds both Tailscale stable
node IDs; current Tailscale public keys remain verified observations and
never appear as frame identity. A production listener rejects frame v1.

M1 wires v2's `hello`/`summary`/`event_fetch`/`body_fetch` capabilities. M2
adds the version-negotiated, control-only `pair_rotate` capability used by
TC-D5; its payload is capped at 4 KiB and can carry only the pinned rotation
state-machine messages. M6 adds `identity_attest` for the verifier-hosted
device ceremony in TC-D13; older peers reject either extension as
unsupported. Its application payload is capped at 8 KiB and may carry only
ceremony identifiers, the verification URL/user code, and bounded status—not
an ID/access/refresh token. The codec's 64 KiB frame / 32 KiB payload budgets
and constant-time MAC comparison remain the hard outer limits. TC-D5 owns the
v2 endpoint binding, directional session keys, canonical MAC vectors, and
replay protection. Production I/O uses the pinned
`asupersync::net::{TcpListener, TcpStream}` APIs: nonblocking,
readiness-driven accept/connect/read/write with `Cx` cancellation and explicit
deadlines. It does not park blocking `std::net` calls on worker threads or
claim that Rust can kill a blocked thread. The forbidden-dependency rule bans
HTTP *stacks* (hyper/axum/tower/reqwest), not sockets.
Only inbound connection acceptance needs the single listener owner. Once an
outbound session is established, the initiating command may answer bounded
`summary`/`event_fetch`/`body_fetch` requests over that same authenticated
connection from its explicitly selected local workspace; it never binds a
second listener.
**Rejected:** a second port or protocol per message family (doubles the
firewall/ACL story); blocking-thread-per-connection I/O; any HTTP/gRPC
transport (ADR 0037's rejection stands).

### TC-D2 — Bootstrap envelope for pre-key traffic

Frame authentication is pairwise-keyed, so first contact (hello probe of a
stranger; the join ceremony) cannot use an established pair key.
Pre-enrollment traffic uses a distinct **unsigned but strictly bounded**
envelope: its own ≤4096-byte budget, capabilities limited to `hello`/`join`,
and no durable mutation until the invite secret is proved. The accept path
derives the caller identity from the accepted socket address through
Tailscale LocalAPI WhoIs (or a freshly queried status-map fallback); it never
trusts a caller-supplied tailnet, node-key, or owner header as identity.
Pre-authentication rate limits key on source IP plus a listener-global
bucket, then switch to the verified node identity, so rotating an unsigned
claimed node key cannot evade a bucket. Source-IP/node mismatches and WhoIs
failure get a privacy-preserving decline. All post-enrollment traffic
requires authenticated session frames; frames from unkeyed peers are
rejected (`mesh_frame_auth_failed`). Minimal accept-side caps (connection
semaphore + source-IP/global bootstrap budgets) ship with the listener, not
later; full `admission.rs` wiring follows in operations.
The outer join envelope remains unsigned, but the secret is never its first
credential-bearing message. The joiner initially sends only invite ID and a
fresh nonce. The responder returns a bounded Ed25519-signed challenge binding
protocol, invite/team/root, both nonces, inviter ee/stable-node identity, and
the committed port. The invite carries the expected public signing identity;
the joiner verifies that signature and exact root/identity/port before it
sends the raw invite secret over the WireGuard-protected socket. A wrong
process on the same host/port therefore cannot collect the secret merely by
answering first. The server compares the received secret to its stored hash
in constant time and immediately zeroizes it.
**Rejected:** signing bootstrap frames with an invite-derived provisional key
(complicates the codec for no gain — the invite secret already authenticates
the ceremony at a higher layer).

### TC-D3 — A typed, signed origin-event envelope

Each node keeps a durable, append-only sequence of its **own** events in a
new `mesh_origin_events` table. `origin_seq` is contiguous per
(`team_id`, `origin_workspace_id`, `origin_node_id`, signing-key
generation), `prevEventHash` chains the stream, and rows are never updated or
deleted; corrections are new events. Shared-scope memory mutations and
team-manifest mutations append an event **in the same transaction** as the
local source-of-truth mutation. Only origin-owned local rows can emit:
materializing, indexing, curating, or otherwise processing an inbound peer row
never re-emits it under local authority. Editing peer material in place is
rejected; an operator who wants to build on it creates a new locally owned
memory with explicit provenance. This prevents echo storms and signature
laundering.

The current `ee.mesh.event.v1` cannot be reused for manifests: it carries
memory-only required fields and a closed memory `eventKind` enum. (Its
`^mem_` ID pattern is *not* the blocker — the pattern's character class
admits `mem_team:*` spellings — the honest rationale is fields and enum.)
T2.0 therefore introduces a generic `ee.mesh.origin_event.v1` envelope
(mandatory feature `mesh.origin_event.v1`) plus two typed payload contracts:

- `ee.mesh.memory_event.v1`: `create`, `revise`, `tombstone`, and
  `shareWithdraw` for a real logical memory, carrying the optional
  `validFrom`/`validUntil` validity-window fields forward from
  `ee.mesh.event.v1` so validity *filtering* (ADR 0041 scenarios) survives
  the supersession. `trust`, `validity`, and `bodyAvailable` event kinds
  remain deferred; body eligibility is serve-time policy (TC-D12). Every
  memory event requires `mesh.team.memory.v1`; content-bearing events sign
  TC-D12's body representation and redaction-provenance fields alongside
  `content_hash`, so fetch integrity never depends on mutable serving policy.
- `ee.team.manifest_event.v1`: explicit `teamCreated`, `memberAdded`,
  `memberRemoved`, `nodeBound`, `nodeRevoked`, `projectRegistered`,
  `signingKeyRotated`, `laneProfileSet`, `idpPolicySet`, and
  `identityAttested`, plus `manifestConflictResolved` operations.
  `identityAttested` is schema-pinned from v1 even though its emitter and
  semantics land in the tier-2 identity milestone. Every manifest event
  requires `mesh.team.manifest.v1`; `identityAttested` additionally requires
  `mesh.team.identity_attested.v1`. T2.0 registers the memory and manifest
  schema/features but production advertises neither base team feature until
  T4.1 installs the active-member/node/key authorizer and manifest
  materializer. T2.4 therefore depends on T4.1: no incrementally shipped M1
  binary may apply or relay a team event merely because its pair/session and
  signature verify. Once enabled, an event whose cross-origin membership
  predecessor has not arrived is durably quarantined for deterministic
  re-evaluation, not accepted by arrival order. The sole pre-membership
  exception is T4.1's self-authenticating `teamCreated` genesis. A pre-M6
  binary dispositions `identityAttested` as `unsupported` even though it
  understands the base manifest schema—the generic feature bit alone would
  be an ineffective gate. Receivers derive the mandatory feature and
  authorization set from the outer payload schema plus operation; an
  origin-supplied `requiredFeatures[]` can add forward-compatibility
  requirements but can never disable a handler check. The list is bounded,
  sorted, and duplicate-free (at most 32 entries and 64 UTF-8 bytes per
  entry). Omitting a mandatory base/operation feature is
  a quarantined protocol violation (`mesh_event_feature_contract_invalid`)
  and the event is never applied or relayed; an otherwise valid event with an
  unknown additional feature is durably `unsupported` for replay after
  upgrade. The `mesh.` feature namespace rule from
  `docs/mesh/event_schema.md` carries over to the new envelope. Manifest
  payloads are small, inline metadata and never depend
  on body-lane grants.

The outer envelope carries the origin/team/workspace/sequence/hash-chain
fields, an authenticated outer `operation` discriminant, `payloadSchema`,
`payloadHash`, `requiredFeatures`, `producedAt`, signing-key ID, and Ed25519
signature. Event and payload hashes use a versioned, schema-specific,
type-tagged and length-prefixed canonical byte encoding; they never depend on
`serde_json` map insertion order or incidental field declaration order.
Variable-length sets are sorted by their schema-defined stable keys before
encoding, and golden vectors pin every optional/null and generation-boundary
case. The event hash omits only `eventId`, `eventHash`, and `signature`; the
signature covers the team-bound event hash and key generation. `eventId` is
not a free unsigned field: it must equal
`mesh_evt_<64-lowercase-hex-event-digest>`, derived from `eventHash` exactly
as in the existing mesh contract, and receivers reject a mismatch before
idempotence or storage. Omitting it from the preimage avoids self-reference;
it does not let a relay rename an event. The existing
`ee.mesh.event.v1` remains the registered transport-independent **file
replay/import-ledger** memory contract used by `ee mesh export|import`; it has
no local signed-origin producer and cannot represent manifests. T2.0
supersedes it only for live team origin streams—it is not stretched with
synthetic `mem_team:*` IDs, re-signed, or promoted to team authority.
Legacy file events continue through T1.3's local policy/trust cap as
non-origin-authoritative evidence and can never be relayed as a signed team
stream. Any later file-artifact migration to the typed envelope requires an
explicit versioned schema change, not reinterpretation. The typed envelope is
what team tips advertise, ranges address, relays preserve, and fork rejection
chains over.

Every admitted peer receives the immutable safe header sequence contiguously.
Policy may omit a `create` or `revise` memory payload, producing a durable
`withheld` receipt that advances the contiguous verified-header **receipt
frontier** without applying material; a later policy generation can fetch
that payload by event ID/hash. The minimal `tombstone` and `shareWithdraw`
control payloads are never withheld from active team members: they contain
only the opaque origin logical-memory ID and predecessor/content references
needed to close retrieval and purge previously admitted derived material.
This mandatory control metadata cannot disclose a body and is included in
the sharing preview. Manifest payloads are likewise never withheld from
active team members. A second contiguous **disposition-scan frontier**
advances once every event through N has a durable per-event disposition
(`applied`, `withheld`, `quarantined`, or `unsupported`) with policy
generation and reason. Materialized state is derived only from explicit
`applied` dispositions, never from a scalar "application cursor." Hydrating a
withheld event audits and changes that event's disposition without rewinding
either frontier. This sparse disposition ledger is what permits N+1 to apply
while N remains withheld. The safe header leaks event
existence, origin, time, kind, and payload hash to an already-enrolled peer;
that metadata disclosure is explicit in the sharing preview.

Per-request admission limits do not bound an append-only ledger. Inbound
signed events, dispositions, and fork proofs therefore also consume a
transactional **cumulative ingress budget** charged to the signed origin
lineage (not the relayer or current connection, and not reset by signing-key
rotation). V1 defaults are 64 MiB per origin lineage and 256 MiB total per
local team workspace, with 1 MiB of separate control reserve per active
member/origin for safe headers plus mandatory manifest, `tombstone`, and
`shareWithdraw` payloads. Normal `create`/`revise` payloads cannot consume the
reserve. Charged size rounds each durable row's encoded bytes up to a 4 KiB
page and adds one 4 KiB row/index-overhead unit; arithmetic is checked. Intake
also stops before commit when the filesystem would fall below a 1 GiB
free-space floor. These are conservative local defaults under
`[mesh.admission]`; an operator may change them explicitly, but a manifest or
peer cannot.

Budget checking covers the whole batch transaction. A denial writes no event,
disposition, audit-per-attempt, index job, or frontier movement; only one
coalesced bounded posture record/counter per origin is updated. Near the
normal ceiling the range planner requests header/control-only continuation so
an honest remover can still reach a mandatory control through preceding safe
headers. Exhausting the origin's bounded control reserve or crossing the
free-space floor stops that origin entirely and reports
`mesh_inbound_storage_budget_exhausted`; unrelated origins continue while
their own and the team normal budgets permit. Local origin rows and local
source-of-truth mutations are not charged to this remote-ingress budget, so a
peer cannot make `ee remember` fail by filling it. Body objects remain under
TC-D12's separate evictable cache quotas. No automatic ledger deletion is
invented: recovery raises the local budget, revokes/pauses the origin, or
replaces/archives the team through an explicit future maintenance surface.

Team activation starts live projection; it does **not** silently publish the
workspace's pre-team history. A new eligible local memory emits `create`; the
first post-activation mutation of an older, never-projected local memory also
emits `create` for its current state, not an orphan `revise`.
`ee team share history` is the explicit historical path: preview and
confirmation pin the exact origin-owned memory IDs plus entity revisions and
state the real audience: metadata enters durable team history and is
available to current **and future** active members until an origin-wide
`shareWithdraw` (subject to the same cache/hash residuals as TC-D12). It is
not a current-recipient-only grant. A bounded resumable job then emits
missing `create` events in stable order. Each
item is revalidated against the pinned revision immediately before emission;
changed items are reported for re-preview rather than sharing unseen content.
A unique per-team/per-memory projection marker makes a live-mutation/history
race idempotent. Imported peer rows are never candidates. This both closes the
missing-baseline problem and avoids bulk-disclosing years of local metadata
during a join.

### TC-D4 — Per-origin Ed25519 signatures and safe relay are v1

Every origin event is signed by the producing node. Receivers verify the
signature, team ID, bound member/node/key generation, hash chain, and
contiguous origin sequence before admitting it. Any authenticated member may
relay an intact signed event; relayers cannot alter or forge another
origin's stream, and duplicate delivery is idempotent by event ID/hash.
Pairwise session MACs authenticate the live connection but are never treated
as event authorship.

A valid signature does not prevent the origin itself from equivocating.
Same-sequence/different-hash evidence or incompatible signed tip chains marks
the origin `forked_at` the earliest proven sequence and retains both signed
headers as forensic evidence. Production does not preserve ADR 0041's
first-arrival branch as materialized truth: deterministic rematerialization
removes that origin's effects at and above `forked_at`, neither branch applies,
and the origin is suspended from sharing while unrelated origins continue.
Peers exchange fork evidence, so machines that initially accepted different
branches converge on the same fork-blocked prefix without selecting by
arrival, timestamp, or hash. V1 has no operator branch-choice primitive;
recovery requires another active member to revoke the node/member lineage and
bind a fresh one. A root-stream fork blocks the team and requires explicit
team recovery/re-creation. The existing `mesh_anti_entropy_fork_observed`
surface reports this posture.

The implementation dependency is `ed25519-dalek = "=3.0.0"` with
`default-features = false` and exactly `features = ["fast", "zeroize"]`.
Key generation fills a `zeroize::Zeroizing<[u8; 32]>` through the existing
fallible `getrandom::fill`, then constructs `SigningKey::from_bytes`; entropy
failure returns a structured error instead of panicking through an infallible
RNG adapter. `zeroize = "=1.8.2"` is a direct
`default-features = false` dependency (already present transitively), and
verification uses `verify_strict`. `rand_core`, `hazmat`, and
`legacy_compatibility` are forbidden. Its dependency-contract entry and
forbidden-dependency audit land before code. The audit explicitly records the
existing transitive `ed25519-dalek` 2.2 line from Asupersync's `nkeys`
dependency: team code uses only the direct 3.0 API, and the temporary duplicate
major is visible in `cargo tree -d` until upstream converges. The signing key
uses the same hardened storage contract as TC-D5. **Rejected:** silently using
the transitive 2.2 crate as an undeclared API; relaying events under the
relayer's authority; direct-from-origin-only v1, because it makes security
removals unavailable whenever the remover goes offline.

### TC-D5 — Pair keys establish replay-safe directional sessions

Invite and introduction secrets are 32 bytes from the OS CSPRNG, single-use,
and never user-chosen. During pairing, canonical initiator and responder
roles each contribute a fresh 32-byte nonce. A length-prefixed canonical
pre-KDF transcript binds the protocol/KDF version, `team_id`,
invite/introduction ID,
both roles, both random ee node IDs, both Tailscale stable node IDs, both
currently observed Tailscale node public keys, both signing-key fingerprints,
and both nonces. Its hash is exactly
`blake3::derive_key("ee.team.pair.transcript.v1",
canonical_transcript_bytes)`; it excludes the derived key and
key-confirmation messages, so the construction is not circular. With `lp`
denoting u32-LE length-prefixing,
`k_pair = blake3::derive_key("ee.team.pair.v1",
lp(invite_or_introduction_secret) || lp(transcript_hash))`. Key-confirmation
MACs separately bind that transcript hash, pair-key generation, and sender/
receiver roles. Golden vectors pin every field boundary. Both sides prove key
possession before enrollment commits; the invite secret authenticates the
ceremony but does not determine the pair key by itself.

The long-term pair key never MACs application frames directly. Each
connection performs an authenticated fresh-nonce handshake and derives
directional `initiator→responder` and `responder→initiator` session keys.
Every frame binds protocol/team/session, source **and target** ee node IDs,
the initiator's locally selected workspace ID, the responder's one registered
`target_workspace_id`, capability, request ID, and a monotonic per-direction
counter. Pair keys remain team/node-scoped, but each connection handshake
authenticates this exact endpoint-workspace pair; neither peer may select a
different database after the handshake. Event `origin_workspace_id` remains
producer provenance and may differ for an intact relayed event; it never
chooses either receiving database. Target/route mismatch, counter
replay/regression, direction confusion, missing key confirmation, and
response/request mismatch are hard failures. A bounded replay window permits
only explicitly specified retransmission, never arbitrary duplicate mutation.

Tailscale's current node public key is a rotating transport credential, not
the durable ee member identity: Tailscale documents that reauthentication
generates a new node key, while `tailscale status --json` exposes a
`StableID` separately. Each binding therefore pins the tailnet plus non-empty
Tailscale stable node ID and the random ee node/signing identity, while storing
the current node key as a verified observation. LocalAPI WhoIs/fresh status
must resolve an accepted connection to the pinned stable ID. A key change for
that same stable ID is accepted only after the pair/session and ee signing
identity still prove continuity (and the tier-1 owner still matches when
enabled), then updates the observation under audit. A missing or changed
stable ID requires a fresh node-binding ceremony; hostname, IP, or a matching
display login never substitutes. Implementations that cannot obtain a stable
ID fail with prerequisite guidance rather than falling back to the rotating
key. This follows Tailscale's documented
[node-key rotation](https://tailscale.com/docs/concepts/node-keys) and
[`PeerStatus.StableID`](https://pkg.go.dev/tailscale.com/ipn/ipnstate#PeerStatus)
surfaces.

Pair and signing keys live under a 0700 user-data key directory in 0600
files opened without following symlinks, with owner/type checks, atomic
write+rename, and file/directory fsync; existing path-safety helpers are
reused instead of relying on `chmod` alone.
On Windows, "client-only" means no inbound responder, **not** weaker key
storage: team join/sync/rotation requires an equivalent reviewed safe
platform adapter that rejects reparse-point components, verifies a
non-inherited DACL limited to the current user SID and SYSTEM, pins the opened
file identity, and provides write-through atomic replacement. It may not
shell out to `icacls` or add project-owned unsafe code. If those guarantees
are unavailable, credential-bearing team commands fail closed with
`mesh_key_store_unavailable`; ordinary non-team ee commands remain usable.
The adapter is a narrow secure-local-file primitive rather than a key-store
special case so TC-D12 can apply the same path, identity, ACL, durability,
and atomic-publication rules to sensitive body-cache objects.

Pair-key rotation is a
generation-bound, crash-resumable two-phase state machine—not a fictitious
cross-machine atomic write. Both endpoints contribute fresh 32-byte nonces
inside the current authenticated session and construct a canonical rotation
transcript binding team, both ee nodes, roles, `rotation_id`, expected/next
generations, both nonces, and the prior pair transcript hash. Its hash is
`blake3::derive_key("ee.team.pair.rotate.transcript.v1",
canonical_rotation_transcript_bytes)`. The next key is exactly
`blake3::derive_key("ee.team.pair.rotate.v1",
lp(current_pair_key) || lp(rotation_transcript_hash))`; this is routine
key hygiene, not compromise recovery—suspected compromise requires a fresh
pairing ceremony and new independent secret. Both endpoints first durably
stage the same rotation record; each proves the next key in both directions
and persists an
`accepting_next` state before either sends a commit acknowledgement. Each
endpoint then promotes locally with an atomic file replacement and closes
sessions derived from the prior generation. A crash may temporarily leave
one endpoint promoted and the other `accepting_next`; the handshake may use
the prior key for **rotation-resume messages only**, carried over the
`pair_rotate` control capability (the only old-key resume surface, matching
the plan's test contract), bound to that exact
rotation record, for
`PAIR_KEY_ROTATION_GRACE_SECONDS = 86400`. The prior key never authenticates a
new ordinary session or application frame after local promotion, no endpoint
automatically downgrades, and concurrent or generation-mismatched rotations
fail closed. After the grace window, incomplete state emits
`mesh_pair_rotation_repair_required` and requires a fresh pairing ceremony.
The staged record persists its creation/deadline and a nondecreasing local
wall-time high-water. Rotation-resume checks advance that high-water and use
same-process monotonic elapsed time; observing wall time below the persisted
floor blocks old-key resume immediately with the same repair code. A clock
rollback therefore cannot reopen or indefinitely extend the grace window.
This converges after crashes without pretending the two files promote
atomically or leaving an old application key accepted indefinitely.
The authoritative local rotation state is an atomically replaced, fsynced
0600 manifest inside the hardened key directory; database/status rows are a
rebuildable non-secret projection. A manifest/DB disagreement blocks sessions
and is reconciled from that manifest before use, so no transaction is claimed
across SQLite and the filesystem.

`ee mesh rotate-pair <peer-id>` is the explicit v1 trigger and emits
`ee.mesh.rotate_pair.v1`; v1 claims no automatic rotation cadence. It uses the
post-enrollment `pair_rotate` control capability, and a peer ID is resolved to
the exact enrolled ee-node pair/generation before staging. This command is
routine hygiene only. `ee team members rotate-key` rotates the local
Ed25519 signing lineage and does not ambiguously claim to rotate pair keys.

The existing redacted `ee backup` format and support bundles continue to
exclude these credentials. A restore from those artifacts must re-pair; loss
of a signing lineage requires another active member to revoke/rebind it. An
operator-managed protected backup of the entire user-data key directory is
outside the `ee backup` contract and is never inferred from a data artifact.

Signing-key rotation is a separate public lineage transition, not an
implicit local file replacement. A `signingKeyRotated` manifest event is
signed by the current key and contains the next public key/generation, the
old generation's terminal `(seq, event_hash)`, and a domain-separated
proof-of-possession signature by the next key. The first event in the new
generation references that transition hash; peers reject skipped,
regressed, reused, or only-one-key-signed transitions. Routine recovery may
promote a previously staged next key; compromise recovery is an explicit
member-removal or node-revocation action by another active member, followed
by a fresh consent ceremony for any replacement, because a
signature by a stolen current key cannot prove its own recovery.
Introduction exchanges use the pair-key construction above, so an inviter
cannot derive the pair keys it introduced.
**Rejected:** deriving from the invite secret alone; direct use of a
long-term pair key for frames; one-sided or automatically downgraded pair
rotation; claiming cross-machine atomic promotion; silently replacing a
signing key without a dual-signed, hash-linked generation transition.

### TC-D6 — Member identity is a first-class primitive

New tables: `team_members` (`mbr_*` ID containing at least 128 random bits,
display name, state, added_by, timestamps, `contact_hint` display-only) and
`team_member_nodes` (member ↔ random ee `node_*` ID ↔ tailnet/stable-node ID
↔ currently observed rotating Tailscale node key ↔ origin-signing-key
bindings, with provenance and key generations). Member and ee node IDs are
OS-CSPRNG values, never hashes of names, emails, paths, or public node data.
The existing `mesh_peers.peer_id` becomes an opaque local record handle, not a
security principal and never something recomputed from a current Tailscale
key. New handles contain at least 128 OS-CSPRNG bits. T2.2 adds the
stable-node/current-key-observation columns and stops using the legacy
`build_peer_id(workspace, node_key)` derivation for new identity.
An old row that cannot be bound to a non-empty stable node ID through a fresh
ceremony remains `identity_upgrade_required` and cannot sync; ee does not
guess continuity from hostname, login, IP, or the old key. Low-level
`ee mesh preview-grant`/`grant` accept the opaque peer ID, resolve it to the
authenticated ee-node binding, and persist the exact ee node plus grant
generation. A current-key observation update for the same binding therefore
does not mint a peer, change the target principal, or inherit a grant.
Member identity is machine-anchored (verified stable-node bindings + ee
signing keys + pair keys), never env-var-anchored; `EE_AGENT_NAME` remains an
unauthenticated agent label within one member's swarm. Producer metadata gains
an optional `memberId` so synced rows attribute to people. A machine belongs
to at most one team per workspace in v1. Lane grants are ee-node-scoped even
when the product command is
member-shaped: `--with <member>` previews and grants the member's currently
bound active nodes, while a node bound later starts at the metadata-only
default and never inherits an existing body grant without a new preview and
consent. `ee mesh revoke-lane` and the member-shaped
`ee team unshare bodies` advance each exact node's grant generation and stop
future serving; a stale preview cannot re-enable the lane, and a later grant
needs a fresh preview. Status exposes partially granted members.

### TC-D7 — Trust class `peer_human_attested`; `human_explicit` stays local

A sixth trust class sits between `agent_validated` and `human_explicit`
(initial confidence 0.75, ask retrieval weight 0.92, included in `verified`
scope). The name deliberately says *attested*, not *verified*: it means
"arrived in a signed origin event from a node bound to an active member, and
that member's store declared the source row `human_explicit`." It does not
prove that a human typed the row.

Elevation has exactly one audited path, at import, requiring all of: trust
lane `peerHumanViaPeer` with source class `human_explicit`; a valid origin
signature bound to an active member; and local policy
`elevate_member_human_explicit` (default **on** for invite-ceremony members —
the ceremony is the consent; per-member togglable). Amplification controls
ship in the same slice: elevation basis in `ee why`, per-member elevated-row
counts in `ee team status`, and a per-member velocity cap, defaulting to 100
unique content-bearing origin events per rolling 24 hours
(`peer_human_attested_max_per_member_24h`). Every otherwise eligible
`create` **and `revise`** consumes one slot after event-id idempotence; replay
consumes none. A revision never inherits an earlier elevation for free. When
the cap is exhausted, that event's resulting current local revision imports
as `agent_validated` (demoting a previously elevated revision if necessary),
never disappears or silently elevates, and emits
`team_member_elevation_burst`. The cap is an atomic, persistent
**local-admission** rolling window; it never trusts member-supplied
`producedAt` for rate accounting. A persisted nondecreasing accounting
high-water mark prevents wall-clock rollback from reopening a bucket, and a
batch is evaluated in canonical origin/key-generation/sequence order.
Because elevation is local policy, two members may deliberately make
different elevation decisions while preserving the same signed provenance.
The three existing rejection points
that keep `human_explicit` from ever crossing remain intact and
regression-tested. The trust-class admission is a recreate-style rebuild at
every `trust_class` CHECK site via new migration IDs (shipped migrations are
checksummed and never edited), with a migration-safety test.
**Rejected:** letting `human_explicit` cross with member authentication
(destroys the invariant "top class = typed on THIS machine"); a lower cap
only (buries the product's headline value — teammates' rules ranking like
rules).

### TC-D8 — Project identity decouples "same work" from local paths

`project_key` on workspaces: before interpreting roots, git repos query
`git rev-parse --is-shallow-repository`; a shallow boundary commit is never
mistaken for a true root. All identity probes invoke the canonical Git
executable directly without a shell, under the bounded/reaped subprocess
runner and a minimal cleared environment. Ambient `GIT_*` repository
relocation, object-alternate, config, prompt, and trace variables are
forbidden. Probes use `--no-replace-objects`, `--no-lazy-fetch`, and
`--no-optional-locks`, so local replacement refs cannot rewrite identity and
derivation never performs an implicit promisor-network fetch. A nonempty
resolved `$GIT_COMMON_DIR/info/grafts` makes root derivation explicitly
unusable rather than silently honoring a local-only graft. Required-option
support is capability-probed: if the installed Git rejects any safety option
(notably pre-2.45 Git without `--no-lazy-fetch`), ee never retries a weaker
root command. Root derivation is then unavailable, `git_unavailable` gives
upgrade guidance, and only the safe remote fallback or a minted/adopted key
may proceed.

A non-shallow, non-grafted repo derives `prj_git_*` from all validated, sorted
non-empty `git rev-list --max-parents=0 HEAD` lines. The
canonical preimage includes the object format reported by
`git rev-parse --show-object-format` plus length-prefixed full object IDs, so
SHA-1/SHA-256 repositories and multi-root histories cannot alias through an
ambiguous concatenation. The result is stable across clones and intentionally
shared by forks. Shallow or honestly unresolvable roots fall back to
`prj_rem_*` only from the explicitly named `origin` remote after strict
credential stripping and canonical normalization, with the exact reason
surfaced. The fallback reads raw local `remote.origin.url` values with config
includes/system/global config disabled; it requires exactly one distinct
usable canonical value. Multiple differing push/fetch URLs, includes, URL
rewrite rules, or arbitrary remote iteration never choose a winner. Secret
userinfo/query/fragment/control bytes are rejected and redacted before any
diagnostic. ee never chooses the first remote by iteration order. A Git
workspace with no usable `origin`, and a non-git workspace, mints a random
`prj_tm_*` at team-share time unless the operator explicitly adopts an
existing project. The derivation inputs/source and aliases are persisted.
No later candidate drift silently changes a persisted project key—not
unshallowing, an unrelated-history merge that adds a root, a history rewrite,
object-format conversion, or an origin rename. `ee team projects reconcile`
shows old and new derivation evidence, previews and confirms an alias/upgrade
or separation, and an explicit override handles mirrors or forks that should
not share lineage.

Hello uses versioned structured
`workspaceBindings:[{workspaceId,projectKey,source}]`, not parallel arrays
whose positions can drift. Peer-policy origin checks accept project-key
matches, replacing the manual n×n `origin_workspace_ids` mapping for the
common case. Raw local paths and credential-bearing remote URLs never cross
the wire. **Rejected:** remote-URL-only identity (breaks on renames and
mirrors); path-derived identity (the problem being solved); silently
re-keying a workspace whenever Git derivation evidence changes; choosing an
arbitrary remote when `origin` is absent.

### TC-D9 — Team manifest = replicated per-origin events + authorization table

The manifest (members, node/signing-key bindings, project registry, team lane
profile, IdP policy) replicates as typed, signed origin events on the
metadata lane. Events are ordered only inside each origin stream; there is no
invented cross-stream total order. Authorization is checked against a
deterministically materialized manifest: member add/remove, lane-profile,
IdP-policy, and project operations require an active author; node operations
are asymmetric—`nodeBound` requires the member's existing active node plus a
fresh direct ceremony with the new node, while monotonic `nodeRevoked` may be
authored by that member or any active member (which already has authority to
remove the whole member); a signing-key transition requires the bound member
plus the TC-D5 dual-key proof; self-removal is leave. A lost last node cannot
authorize its own replacement: recovery uses another explicit invite/consent
ceremony and, when continuity cannot be proved, a new member ID. An
`identityAttested` operation requires an active verifier member/node distinct
from the subject, binds the exact IdP-policy generation and finite renewal
deadline, and contains canonical evidence only—never the raw token. A member
cannot extend their own tier-2 identity lease. Attestations are an append-only
lease set, not an arrival-winner singleton: all eligible leases are retained
in canonical event-hash order, and the subject is currently attested when at
least one unexpired lease satisfies the locally effective policy floor. The
derived effective deadline is the maximum eligible deadline, capped when each
lease is authored by the policy cadence. Concurrent renewals therefore
commute. "Unexpired" is evaluated against a local, persisted, nondecreasing
identity-authorization time floor, transactionally advanced to at least the
current wall clock before every token-verification, grant/serve, sync-import,
status, and steward decision that depends on identity. Peer `producedAt`,
token timestamps, and attestation claims never advance that floor. A local
clock rollback therefore cannot revive or extend a lease and surfaces
`team_identity_clock_rollback`. A forward jump simply advances the floor and
may expire leases early, which is fail-safe; if the system clock is then
corrected backward, the rollback posture appears. The explicit local repair
lowers the floor only after suppressing every currently eligible tier-2 lease
and requiring fresh interactive attestations, so reset cannot reactivate old
evidence. Two active member IDs cannot claim the same exact
(`issuer`, `subject`) under one policy generation; all such bindings enter a
complete-set `manifest_conflict` and sharing for the affected members pauses
until reconciliation/removal rather than selecting the first arrival. A
verifier whose own current subject matches the target subject is not distinct
and cannot authorize the lease.

V1 has a hard `MAX_ACTIVE_TEAM_MEMBERS = 20` protocol limit (the creator-only
bootstrap state may temporarily contain one). If the complete admitted event
set would exceed it, no hash, timestamp, or arrival order chooses which
contested additions fit: membership-dependent sharing enters a
`team_member_capacity_conflict` posture until explicit reconciliation/removal
brings the complete active set within the limit. This is both the supported
product envelope and a storage/fanout safety boundary, not a UI suggestion.

V1 also hard-caps each member at
`MAX_ACTIVE_NODES_PER_TEAM_MEMBER = 4` (80 active nodes team-wide at the
member cap). `nodeBound` commits the exact predecessor node-set root in
addition to the two-party ceremony. Concurrent successors from one root
commute only when their complete union remains within four; if it would
overflow, every contested successor is held in
`team_node_capacity_conflict`, the predecessor set remains effective, and no
arrival/hash winner inherits a session or grant. Monotonic `nodeRevoked`
operations may bring the set below the cap before reconciliation. This bounds
pairing, route, grant, and fanout amplification while leaving a deliberate
revoke-then-add path for device replacement.

`teamCreated` is the sole pre-membership authorization exception and the
unique manifest genesis. It must be generation 0 / sequence 1 with no
predecessor, be strictly self-signed by the public key embedded for the
initial node, and canonically commit the random `team_id`, protocol version,
display name, immutable v1 `hello_port`, initial random member ID, initial
node/key binding, creation time claim, and default lane/identity policy. Its event hash plus root-key
fingerprint is the permanent team-root reference carried by invites and
introductions. Every non-genesis manifest event must trace to that exact
genesis; a missing, second, or conflicting `teamCreated` is a root fork and
blocks the team rather than selecting by arrival.

Membership and node removals are monotonic, remove-wins operations. A
`memberRemoved` payload contains, for every known origin of the target, the
remover's last accepted `(key_generation, seq, event_hash)` frontier. Once
the signed removal is admitted, effects authored by the target above those
cutoffs are quarantined and any already-materialized effects are reversed by
deterministic rematerialization; arrival order cannot change the result. An
origin attributable to the target but absent from the signed cutoff map has
an implicit cutoff before its first event: no effect from a hidden,
later-discovered node/stream survives the removal. Events through explicit
cutoffs remain valid. Active members added by the removed member through an
accepted cutoff remain active but carry the persistent
`addedByRemovedMember` review flag until each local operator explicitly
acknowledges it — acknowledgement is a local per-node action, never replicated
(a replicated clear would let one member silently vouch for everyone). The
flag is detection, not revocation: a pre-cutoff member (including a
sock-puppet) retains ordinary v1 authority until separately removed.
Removal preview, status, and doctor must therefore identify every such member,
emit `team_delegated_member_review_required`, and recommend pausing the team
until the operator either acknowledges a legitimate member or removes a
suspicious one. V1 does not pretend to solve this without quorum/roles.
Rejoining always mints a new member ID and signing-key lineage. And to keep
the vocabulary honest against the unchanged "no CRDTs" non-goal: the
manifest is still **not** a CRDT — remove-wins cutoffs are a fixed
precedence rule applied to signed evidence, and conflicting writes block for
explicit reconciliation; nothing merges.

**Deterministic rematerialization, pinned** (this ADR's amendments invoke it
for fork rollback and removal cutoffs; without a definition it is the
largest unimplemented mechanism, so): the core is a pure, versioned reducer
over immutable signed events plus their current durable dispositions. An
`applied` disposition records the local policy generation and immutable
admission result needed for replay (including the resulting local trust
class once T3.4 lands); replay never reruns a rolling velocity window or
silently changes an earlier local policy decision.

The total traversal order is exact. Within one team, streams sort by the raw
UTF-8 bytes of `(origin_workspace_id, origin_node_id)`, then numeric
signing-key generation; events within a stream sort by numeric `origin_seq`.
A duplicate `(stream, generation, seq)` with different hashes is
equivocation and is fork-blocked before reduction, never tie-broken by event
hash. Cross-origin authorization, predecessor conflicts, removal cycles, and
capacity conflicts are computed from the complete accepted set/fixed point;
the traversal order is only deterministic execution order and never an
authority winner.

The reducer returns a canonical desired projection and idempotent action
plan; it does not perform I/O. A bounded executor first transactionally makes
newly invalid material non-retrievable, then applies the plan in resumable
generation-fenced batches. Derived-row changes, audit records, index jobs,
and cache-eviction outbox entries use deterministic idempotency keys. Cache
metadata closes the retrieval path in the transaction; physical eviction is
idempotent after commit, so a crash cannot leave invalid bytes readable.
Large reversals checkpoint without exceeding the existing 16-index-jobs-per-
round budget, and only a completed generation becomes visible. While a
rebuild is incomplete, affected reads carry
`mesh_rematerialization_pending` (warning) rather than silently presenting
the thinner fail-closed corpus as complete. An executor/invariant failure is
`mesh_rematerialization_failed` (high) with structured status/doctor repair;
neither condition re-exposes the invalid generation.

The eventual reversal scope is: derived memory rows from affected events;
their `peer_human_attested` projection (reverted to the recorded capped class
with audit); derived-index entries; and body-cache rows for affected content
hashes. Historical elevation-attempt counters are safety accounting, not
projection state, and are not refunded; because the original admission
decision is durable, replay neither consumes another slot nor changes the
old result. The M1 owner ships the reducer/executor, current memory/index
behavior, and versioned integration contract. T3.4 owns the new trust-class
arm; T5.9 owns body-cache publication/eviction integration. It never touches
local source-of-truth memories and never deletes ledger or audit rows
(disposition changes append evidence).

Two nodes with the same materializer version, signed ledger, payloads, and
durable disposition/admission records produce the same canonical projection
hash. This guarantee intentionally excludes local surrogate row IDs, audit
timestamps, filesystem paths, velocity counters, and locally different
policy/admission records; those are not retrieval bytes. Owned by T2.8 under
M1; T2.4 and T4.1 consume it.

A `nodeRevoked` payload similarly names the exact ee node and the revoker's
last accepted frontier for that node. Its append transaction advances a
durable session/grant authorization generation before commit. Every
subsequent frame handler rechecks that generation before import or serve, so
an already-open socket cannot race a post-commit body fetch. In-memory session
closure and connection cancellation are idempotent post-commit effects, not
fictional SQLite side effects; a crash is safe because restart cannot
re-authorize the old generation. Effects above the cutoff quarantine
regardless of arrival order. Ordinary Tailscale node-key rotation for the same
pinned stable node ID is only an audited transport-observation update under
TC-D5 and does not create a new ee node or inherit a new grant.

The materializer evaluates active-author/removal validity as a deterministic
set, not a one-pass arrival-time check. A cycle of removals whose cutoff
claims mutually invalidate the removal events (including the two-member
"A removes B while B removes A" case) does **not** pick a hash, timestamp, or
arrival winner: every removal in the strongly connected component is held in
`manifest_conflict`, the pre-conflict membership remains effective, and
sharing is paused for the affected members until reconciliation references
the complete conflict set. A later competing reconciliation re-blocks the
field. This prevents both split-brain membership and a fully orphaned team
without smuggling a leader-election rule into v1.

Mutable singleton fields such as lane profile and IdP policy use an expected
predecessor event hash. Competing successors do not race to become
authoritative: the field enters `manifest_conflict` and remains blocked until
a signed `manifestConflictResolved` operation references the complete known
conflict set. Reconciliation is itself subject to the same predecessor rule.

Replicated policy is coordination input, not authority to mint local consent.
Effective outbound lane access is the intersection of the manifest profile,
the serving node's local policy, the exact per-requester-node grant
generation, and the current redaction/secret-scan verdict. A
`laneProfileSet` widening therefore grants nothing by itself; narrowing takes
effect immediately as a team-wide ceiling. Each node also persists the
strictest IdP policy it explicitly accepted. A stricter comparable
`idpPolicySet` applies immediately, while a remote relaxation or incomparable
issuer/group change remains in local
`pending_local_policy_acceptance` posture and cannot lower that node's floor
until its operator explicitly accepts the exact manifest generation through
`ee team idp set`. Status/reconcile expose the mixed posture with
`team_policy_relaxation_pending`. This preserves any-active-member manifest
authorship without letting one compromised member silently widen another
machine's export or identity boundary.

Receipt of an unauthorized, removed-window, unsupported, or withheld event
is durable and audited as that event's disposition while the receipt and
disposition-scan frontiers advance; it never enters materialized state merely
because a frontier moved.

`ee team member remove` durably appends the signed removal and advances the
target nodes' session/grant authorization generations in one transaction.
Per-frame reauthorization makes future serving fail closed immediately after
commit; idempotent connection cancellation and bounded fanout happen
afterward and are never described as part of the database transaction. Any
peer that receives the removal can relay it. Fanout uses TC-D11's
bidirectional anti-entropy round: the remover connects to a peer responder,
advertises its new signed tip, and serves the requested removal range over
that same session; no `event_push` capability or reverse connection is
invented. The command reports which members acknowledged and which remain
exposed; status/doctor retain an acknowledgement matrix until every active
member has applied the removal.
Propagation is **not** claimed to be bounded when no active peer receives the
event. A removal never emits
`shareWithdraw`: that event is origin-memory-wide, not recipient-specific,
and would incorrectly withdraw material from members who remain authorized.
Already-synced copies on the removed machine cannot be erased.

Removal is preview-hash pinned. Human mode renders the target member, exact
active nodes/signing generations, per-origin cutoffs, accepted-prefix members
that will remain active, acknowledgement audience, and cached-copy/
no-`shareWithdraw` residual, then asks for confirmation. Automation first runs
`ee team member remove <member> --preview --json` and supplies the returned
`--preview-hash` to the mutating call. The hash binds the team root and
manifest/materializer generation plus every previewed field. Inside the
removal transaction, ee recomputes those inputs immediately before append; a
change returns `team_member_removal_preview_stale` with a new preview action
and commits no removal, authorization-generation advance, invite
invalidation, audit/outbox, connection-cancel, or fanout side effect.
Confirmation never silently approves a different cutoff or delegated-member
set.

**Rejected:** arrival-order authorization; silently choosing one concurrent
manifest writer or capacity winner; treating a rotating Tailscale public key
as the permanent member/node identity; using a memory event as a manifest
operation; automatic `shareWithdraw` on member removal; role hierarchies in
v1 (a ≤20-member trusted team does not need them yet).

### TC-D10 — The join ceremony is the consent event

`ee team join` enrolls both machines with
`trust_established_by = "explicit_human_consent"` — the humans minted and
transferred the code. Human mode reads the invite from a no-echo TTY prompt;
automation uses `ee team join --invite-stdin`. The secret is never accepted
in argv or an environment variable and never appears in process listings,
shell history, logs, audit rows, or error text; only invite ID/fingerprint and
secret hash are persisted. This resolves the auto-enroll trust dead-end without
laundering: zero-touch `tailscale_auto_enrollment` peers remain
sync-ineligible by design. Invites contain a version, team ID, invite ID,
the inviter's exact Tailscale stable node ID and ee node/signing identity,
the currently observed rotating Tailscale node key plus MagicDNS/IP hints,
the team-root `hello_port`, genesis event hash, root/signing-key fingerprint,
and 256-bit secret. Invite IDs and durable ceremony IDs each contain at least
128 independent OS-CSPRNG bits; neither is a counter, hash prefix, display
name, or truncation of the secret. Invites are
single-use, TTL-bound (default 72 h), hashed-at-rest, and revocable. The
inviter persists a nondecreasing invite-authorization wall-time floor and
advances it transactionally on mint, lease, redeem/resume, revoke, expiry,
and introduction-secret authorization; a live process also enforces the
corresponding monotonic elapsed deadline. A wall clock observed below the
persisted floor after restart or correction blocks mint/redemption/resume
with high `team_invite_clock_rollback` instead of extending a bearer
credential. A forward jump may expire credentials early (fail-safe). Doctor
may lower the floor only in the same transaction that revokes every pending
invite, lease, and introduction secret; repair never reactivates one. The
joiner resolves the exact stable node ID through fresh local Tailscale status
and connects only to a current IP and current node key associated with it.
The embedded current key, MagicDNS, and IPs are observations/hints, never
authority or a fallback to another stable node/public endpoint. Normal
Tailscale key rotation for the same stable ID is accepted and shown; an absent
or changed stable ID requires invite reissue. The server still proves the
invite secret, ee signing identity, and team-root binding, and WhoIs verifies
the accepted source stable ID on the reverse path. Per TC-D2, the inviter's
signed challenge proves the expected ee identity/root/port before the joiner
transmits the secret. The invite pins the inviter signing generation and
fingerprint. If that key rotated after minting, the challenge must include a
contiguous TC-D5 dual-signed/hash-linked public transition chain from the
pinned generation to the current signer, bounded by the 4096-byte bootstrap
budget; otherwise the invite must be reissued. Node/member revocation,
fork-block, or compromise recovery invalidates its pending invites and cannot
be bypassed with a self-authored rotation chain.

The simple documented `ee team invite` command always leaves a redeemable
path. If the daemon broker is live, it registers the invite and may return;
otherwise it prints the code and becomes the foreground responder until
redemption, expiry, or interruption. `--wait` also waits for the
nonce-correlated result when the daemon owns the listener. `--no-wait` is
accepted only after an already-running broker confirms the route; it cannot
mint a code that no process can serve. An interrupted waiter leaves the
hashed invite pending and prints
`ee team invite --wait --resume <invite-id>`; resumption carries no secret.
All forms obey TC-D1's single-owner rule and register only invite ID/hash
metadata over the same-EUID, workspace-bound control channel—never the clear
secret.

`ee team join --dry-run` is intentionally **not** a redemption oracle. It
parses and checksums the code locally, checks its embedded expiry, verifies
local Tailnet/tool/config prerequisites, and may send only the ordinary
secret-free bootstrap hello needed to report protocol reachability. It never
sends the invite secret, leases or consumes the invite, or receives manifest,
membership, identity-policy, or other protected team metadata. Its output
states that revocation, prior use, server-side expiry, and authorization were
not validated; those checks happen only in the real, audited ceremony.

Join is a durable, idempotent state machine:
`pending_redemption → key_confirmed → member_committed →
first_sync_complete`. Claiming an invite atomically leases its one redemption
to a stable ceremony ID; a crash resumes that ceremony rather than consuming
the code or admitting a second joiner. Active membership is committed only
after mutual pair-key and signing-key confirmation and, when configured,
successful identity attestation. Exit 0 requires the committed membership
and first metadata sync; nonzero output reports the exact durable phase and
resume command. Deferred pairings to offline members are retryable records
owned by explicit sync and the steward, surfaced as `unpaired`.

Invite and introduction secrets are held in zeroizing byte buffers and are
never persisted in clear. If a crash occurs before `key_confirmed`, resuming
requires the user or agent to supply the same invite again through the
no-echo/stdin path; the safe resume command contains only the ceremony/invite
identifier. The inviter matches the secret hash, stable ceremony, node, and
signing-key fingerprint before continuing the existing lease. After
`key_confirmed`, the confirmed keys and non-secret phase state are sufficient
to resume. An unbound invite remains a bearer credential for any
policy-admitted node on the tailnet: interception can win the one redemption
before the intended recipient. The UI names that residual, keeps `--for` as
a display label rather than an authorization claim, and recommends short TTL,
`--wait`, revocation, or an enabled identity policy for higher-risk teams.

Signed manifest history lets a joiner verify every supplied event against
the team root and origin key bindings, but the inviter can still omit history
or introduce an inviter-controlled sock puppet. Direct sync/reconciliation
with other members exposes omission/divergence; pairing alone does not.

### TC-D11 — Listener-asymmetric, bidirectional rounds

Every exchange requires the counterparty's responder, but an established
authenticated round is **data-bidirectional**: both endpoint workspaces
advertise tips, independently plan missing ranges, and may issue bounded
requests over the same directional session. Each side serves only its
handshake-bound local workspace and may relay intact foreign-origin events.
This is what makes signed removal fanout possible without adding an
unaudited push capability.

A member whose machine never runs the daemon cannot be contacted, but
foreground `ee team sync` still both sends and receives while that member is
the initiator. Two members with no responder cannot exchange at all, and
other peers cannot trigger freshness on a client-only member; `ee team
status` says so plainly. The daemon is `#[cfg(unix)]`; Windows members are
client-only in v1, but a scheduled/manual outbound round can contribute as
well as receive only after TC-D5 key-store parity passes; otherwise
credential-bearing team commands fail with `mesh_key_store_unavailable`.
A Windows responder plus secure same-user local broker control is the named
follow-up. **Rejected:** a strictly requester-read-only
round (cannot carry removal fanout and strands client-only contributions);
adding a separate `event_push` protocol; hiding listener asymmetry behind
"sync just works" prose; making any core command daemon-required.

### TC-D12 — Bodies travel out-of-band via `body_fetch`, never in origin events

Origin memory payloads carry metadata only. Body material moves as
**policy-gated lazy fetches** (`body_fetch` frames) keyed by the signed
`create`/`revise` origin event and its `content_hash`; fetch eligibility is
decided serve-side at fetch time (no `bodyAvailable` event in v1).
The event also signs `bodyRepresentation = "exact" | "already_redacted"`;
the latter includes a bounded redaction-profile/scanner-version identifier
and redaction-evidence hash, never the removed text. V1 never rewrites body
bytes during fetch: the final bytes must hash exactly to the event's
`content_hash`. A policy posture of `redact` may therefore serve only an
event already signed as `already_redacted`; requesting redaction of an
`exact` body is a policy denial/metadata-only result, not an in-flight
transformation with an unverifiable hash. The current secret scan still runs
immediately before serving and may newly deny the exact bytes, but never
mutates them. A future transformed derivative would require a separately
versioned, origin-authenticated derivative descriptor; v1 does not invent
one.
`tombstone` and `shareWithdraw` are mandatory minimal control metadata under
TC-D3, not body events and not policy-withholdable content; otherwise a
later-denied peer could retain material it had already received.

Fetches run only in explicit/background synchronization (`ee team sync`, an
explicit prefetch, or steward rounds), never synchronously inside
pack/search. Retrieval consumes cached bodies or returns an attributed
metadata-only item plus a missing-body posture/revision token; it never
blocks on network I/O. A stable policy denial is terminal for that policy
generation and is retried only after policy/grant change or explicit
operator refresh. Transient unavailability uses capped retry (1 s → 60 s,
at most 5 attempts, one attempt per event per round) and records
`retry_after` in `mesh_body_cache_metadata`.

Every fetch request names the exact signed origin event, requester member and
node, project/workspace binding, and grant/policy generation. The server
re-authorizes that tuple before serving; content hash alone is never an
oracle. Only the event's owning origin workspace/node may serve from its
local source truth in v1; a relayer's cached copy is never a new serving
authority. Tombstoned/withdrawn events and a source that no longer retains
the exact revision return unavailable rather than substituting current or
cached bytes. Chunks carry transfer ID, sequence, declared final length, and
final hash; the receiver requires that hash and the streamed bytes to equal
the signed event `content_hash`. Aggregate bytes are bounded by the streaming
`max_bytes+1` policy and the codec's 32 KiB per-payload limit. Incoming bytes
stream first into a
private temporary cache object under the secure user-data boundary; the
object is never published to retrieval until final length/hash/event
verification and import-policy admission all succeed. On Unix the complete
path is owner-checked and no-symlink under a 0700 directory, and published
body objects are 0600. On Windows the same reviewed safe adapter as TC-D5
rejects reparse-point components, pins opened-file identity, applies a
non-inherited current-user-plus-SYSTEM DACL, and performs write-through
atomic publication. Objects are atomically named by an opaque cache key,
quota/retention governed, and excluded from support bundles (including raw
paths). Failed, quarantined, evicted, expired, and withdrawn objects cannot
remain retrieval-addressable. Publication and invalidation intentionally use
opposite crash-safe orderings:

1. **Publication is object first, visibility last.** A transaction creates a
   `staging` metadata row with an opaque transfer ID. The receiver writes and
   verifies the private temporary object, atomically publishes it, and fsyncs
   the file and containing directory before a second transaction changes the
   row to `available`. Retrieval addresses only `available` rows. A crash
   before that final transaction can therefore leave an inaccessible staged
   object, never a visible unverified body.
2. **Invalidation is visibility first, object purge last.** Withdrawal,
   eviction, expiry, quarantine, or policy reversal transactionally changes
   an `available` row to `invalidated_pending_purge`, removes its retrieval
   and index eligibility, and enqueues an idempotent purge. Only after the
   object removal and directory fsync succeed does metadata advance to
   `purged` or `evicted`. A crash can therefore retain inaccessible bytes,
   never retrieval-addressable invalid bytes.

Startup, steward, and doctor reconciliation resume both state machines,
remove or report orphaned staged objects, retry pending purges, and validate
that no object is addressable outside `available`; they never infer
availability from filesystem presence alone. If a platform cannot prove the
cache contract, body hydration remains metadata-only and records high,
repairable `mesh_body_cache_lifecycle_failed` posture with the filesystem
path redacted; it never publishes under weaker permissions. Absence of the
Windows credential adapter already blocks team traffic under TC-D5.

Rationale: (a) anti-entropy stays bounded — bodies would blow the 512-event
batch and payload budgets; (b) serve-time policy/redaction/secret-scan
eligibility applies to every fetch, so policy changes can deny future fetches
without rewriting streams, while redaction remains hash-honest by requiring
an already-redacted signed representation; (c) **immutable ledger bodies would contradict
`shareWithdraw` purge semantics** — a withdrawal cannot purge what an
append-only origin stream permanently embeds; (d) it is the SRR6.11
eager-metadata/lazy-body architecture the cache modules were built for.
`shareWithdraw` is authored only for an origin-owned logical memory and
purges eligible **derived peer material and body-cache objects** for that
memory on observing nodes; it never deletes the origin's local
source-of-truth memory and is never a per-member revocation primitive. Named
residual: the `content_hash` of a withdrawn body
persists forever in the append-only stream, so a peer who already holds (or
later guesses byte-exactly) the content can confirm it post-purge — purge
removes bodies, not the ability to recognize them.
Likewise, revoking one recipient's body-lane grant prevents future serving
but cannot erase a body that recipient already cached or copied. The
recipient-facing command and audit output state that limitation; a later
origin-wide `shareWithdraw` asks every observing active peer to purge its
derived copy, but is cooperative revocation rather than a remote-erasure
guarantee.
`ee team unshare bodies` is scoped to the **current serving workspace/node**:
`--all-members` means every recipient of this local source, not every source
node owned by the human or team. Its preview/result names that source and
lists other known source nodes as unaffected; the operator runs it on those
nodes separately. A local-first tool must not imply a distributed revoke it
cannot authorize or confirm.
**Rejected:** a body event kind in `mesh_origin_events` (reasons a–c);
widening the frame codec budgets.

### TC-D13 — SSO member identity in two tiers

Tier 1 (zero new dependencies): tailnet-attested identity. The accept and
revalidation paths query local `tailscaled` identity for a connection/node;
`ee team idp require --tailnet-attested [--domain …]` binds member records
to node-owner logins. Mismatch **suspends** grants (reversible, audited).
Tier-1 data is used only to suspend, never to grant — "Tailscale is not
trust" stands.

Tier 2 uses OIDC device authorization only after a provider-capability
preflight confirms a device endpoint, an ID token usable for this client and
scopes, supported signing algorithms, required claims, and a public-client
token endpoint authentication method (`none`) that needs no client secret.
Unsupported providers fail with a structured recovery action. Tier 1 can use
the Google/Microsoft/Okta identity already represented by Tailscale, but tier
2 does not claim that all of those providers or an arbitrary OAuth server
offer a compatible secretless device client. In particular, a flow requiring
`client_secret_basic`/`client_secret_post` is unsupported in v1 rather than
putting a shared OAuth secret in the manifest or join ceremony. RFC 8628
itself defines no nonce parameter. If a provider advertises a nonce extension
and echoes it in the ID token, ee uses and verifies it; otherwise ee explicitly
reports the weaker freshness+single-use binding.

HTTPS egress uses an allowlisted canonical system `curl` executable invoked
without a shell through the project's bounded subprocess runner. The default
must be an absolute regular file under owner/mode-safe, non-symlinked
root-owned ancestors; a non-system override requires explicit local approval
of its canonical path and digest and is never selected from ambient `PATH`.
The runner gains
bounded stdin, a hard total stdout/stderr byte cap, timeout/cancellation,
process-group termination and reap, and inherited-pipe escape tests; it never
waits forever for a descendant that retained a pipe. Secrets and device codes
travel through stdin/request bodies, never argv or logs, and token-bearing
responses pass the redaction boundary before diagnostics.

The invocation starts from `env_clear()` plus an explicit minimal
platform-runtime allowlist, with curl configuration disabled and
no-proxy-for-all set explicitly; ambient `.curlrc`, proxy, netrc, CA-bundle,
TLS-backend, and TLS-keylog environment cannot redirect or disclose identity
traffic. System trust is the default. A private/test issuer requires an
explicitly approved CA-bundle path passed directly to this command and
validated as the expected local regular file; inherited `CURL_CA_BUNDLE`,
`SSL_CERT_FILE`, `SSL_CERT_DIR`, and `SSLKEYLOGFILE` never take effect.
Only strictly parsed HTTPS URLs without userinfo or fragments are accepted.
Redirects are never delegated to `curl --location`: ee resolves and validates
each bounded GET hop, while credential-bearing device/token POSTs reject
redirects rather than replaying a secret body. Every A/AAAA answer is checked
against loopback/link-local/private/reserved ranges (unless the operator
explicitly approved a private-network issuer) and the approved address is
pinned for the connection while TLS still verifies the original hostname,
closing DNS-rebinding/time-of-check gaps. Protocol options constrain both the
initial request and any manually approved hop to HTTPS. Discovery issuer
equality, TLS verification, and response/time limits remain mandatory.

Each explicit attestation refreshes discovery and JWKS (an HTTP cache
validator/304 is acceptable); a merely old offline JWKS cache never verifies
a newly presented token, because it cannot prove that a key remains
published. JWT verification uses an algorithm allowlist (RS256 and ES256
initially),
rejects `none`, validates `kid`/`use`/`key_ops`/`alg`, exact issuer, audience plus `azp`
when required, expiry/not-before with bounded skew, `iat`/`auth_time`
freshness, verified email whenever email participates in identity/policy,
configured group-claim shape, and `jti`-or-token-hash single use in an atomic
replay ledger scoped to issuer/client until token expiry. RSA keys must meet
the pinned minimum policy (modulus ≥ 2048 bits, public exponent exactly
65537); EC keys must be P-256 signing keys
and match the declared algorithm. Discovery, JWKS, JOSE headers, and claims
use a bounded duplicate-member-rejecting JSON decoder; last-key-wins parsing
is forbidden. JWT segments require canonical unpadded base64url. Unknown or
unsupported `crit` headers and header-supplied `jku`, `x5u`, `jwk`, or `x5c`
are rejected; ee fetches keys only from the exact freshly discovered issuer
`jwks_uri`, never from token-controlled locations or embedded key material. A
missing `kid`, or a `kid` that selects zero or multiple eligible keys after
`kty`/`use`/`key_ops`/`alg` filtering, fails instead of trying keys until one
verifies. JWKS certificate URLs/chains are never followed in v1; accepted
RSA/EC keys must contain validated raw public parameters.
Signature verification uses the exact original ASCII
`base64url(header) + "." + base64url(payload)` bytes, never reserialized JSON;
no claim or replay key is trusted/committed until that signature succeeds.
`ee` does not request `offline_access`; any
refresh token returned anyway (including provider-mandated device-flow
responses) is treated as transient secret material and discarded. Raw ID,
access, and refresh tokens are never persisted:
after verification ee retains only the token hash/replay claim, canonical
**minimal** approved claim subset, issuer/client/subject, key ID plus JWK
thumbprint and algorithm, verification/expiry times, and the verifier's
signed attestation evidence. Email is retained/replicated only when the
configured policy or explicitly previewed member display requires it. The
full provider group list and unrelated token claims are never persisted or
put in a manifest: the evidence records only bounded matches against the
configured allowed-group identifiers plus the authorization decision. The
`idp set`/join preview names the claim fields that will become team-visible.
Token buffers are zeroized. A retired JWKS key never verifies a
newly presented token. An existing attestation remains explainable from its
stored verification evidence until its revalidation deadline—there is no raw
cached token to re-read.

The **distinct active verifier member/node**, not the subject's ee process,
hosts the device client over the post-key `identity_attest` session. It starts
the provider flow, returns only the bounded verification URL/user code to the
subject, polls and receives the tokens locally, refreshes discovery/JWKS,
verifies the ID token, and immediately reduces/zeroizes the bearer material.
Polling follows RFC 8628 without granting a provider an unbounded process:
`expires_in` is a required positive integer and `interval` is a positive
integer (default 5 seconds when absent). A same-process monotonic deadline is
the earlier of provider expiry and 1800 seconds from ceremony start, and the
client also stops after 300 token requests. It waits at least the advertised
interval before each request; every `slow_down` adds 5 seconds to the current
interval for all later requests, and connection timeouts use checked
exponential backoff. Wait arithmetic is overflow-checked and limited by the
remaining monotonic deadline—an interval longer than the remaining lifetime
expires the ceremony rather than being shortened in violation of the
provider minimum. Only `authorization_pending` and `slow_down` continue
polling; denial, provider expiry, malformed values, cancellation, and every
other error terminate without an automatic fresh ceremony. Provider expiry,
the local deadline, and the poll budget return
`team_idp_device_flow_expired` with a machine-readable reason and an explicit
restart action. Cancellation terminates/reaps the curl process and zeroizes
the device code and any partial token response.

The surrounding join/renewal workflow is crash-resumable; the OAuth device
sub-ceremony is intentionally not. A process loss leaves the outer workflow
at `identity_pending` with its non-secret checkpoint, destroys the device
code/poll state, and requires the user to explicitly start a fresh provider
ceremony. It never persists bearer-like ephemera merely to make polling
resume automatically, and it never treats the interrupted ceremony as a
successful identity gate.

Verification URLs, user/device codes, and polling state are ceremony-TTL
ephemera excluded from audit logs, support bundles, and the manifest (only a
redacted terminal status may persist). No raw token traverses the ee mesh. The verifier then authors
`identityAttested`, binding subject member, verifier, policy generation,
assurance/evidence hash, and a policy-capped finite renewal deadline. Other
members trust that attributed verifier assertion (named trust link);
self-attestation and self-renewal are rejected. In ordinary renewal the
verifier must itself have a current eligible exact-policy-generation lease;
the explicitly labeled activation bootstrap below is the sole grace-period
exception.
Eligible leases materialize as the deterministic set defined in TC-D9, so
concurrent verifier renewals do not race and duplicate issuer/subject
bindings cannot manufacture distinct human identities.
If no distinct verifier is reachable, renewal stays pending and the ordinary
grace/suspension rules apply. Tier-2 revalidation is interactive and occurs
only in explicit identity commands. The steward performs no IdP HTTP: it
marks leases due/overdue and suspends after the configured grace when no fresh
attestation arrives. Every identity-dependent authorization path uses the
TC-D9 persisted local time floor; checking status only in the steward would
leave a rollback window on a quiet or disabled daemon. A
`members revalidate` local clock-floor repair is audited, requires explicit
confirmation after the system clock is corrected, suppresses all currently
eligible tier-2 leases, and leaves members pending until new ceremonies
complete. Thus directory offboarding is not push-driven or
instantaneous; it prevents the next renewal, bounding continued access by the
revalidation interval plus grace. Tier-1 WhoIs ownership checks remain the
only noninteractive cadence. **Rejected:** rustls in-tree for v1;
authorization-code flow with a localhost redirect server; automatic redirect
following; self-attested OIDC renewal; claiming background directory polling;
treating tailnet attestation as a grant signal; disabling TLS verification.

Tier 2 is off in `teamCreated`. Enabling or tightening it puts members without
an exact-generation lease into an explicit pending/due state and starts the
configured local grace window; it does not instantly orphan a one-member team.
An active member in grace may verify a joiner, after which the distinct
member can verify the creator. The `idp set` preview names this bootstrap
sequence and refuses a zero-grace activation that would suspend every current
member before a distinct verifier can exist.

### TC-D14 — Import trust is authenticated by a store-local MAC

`ee export` artifacts are MAC'd (blake3-keyed, store-local secret in the
hardened key store) over a constant-size versioned canonical header containing
the artifact family/schema and canonical record-encoding version, source
store-key namespace, exact source workspace/scope, key ID, record count, and a
domain-separated ordered `records_root`. `ee export` and playbook artifacts
use distinct MAC domains and record type tags, so a valid header from one
surface cannot authenticate bytes on another or silently cross workspace
scope. The
root is a streaming digest over length-prefixed
`(ordinal, record_id, canonical_record_hash)` entries. Export computes the
root and emits rows from one consistent read snapshot, so mutation between a
prepass and output cannot produce an unverifiable artifact. `ee import jsonl`
honors native trust (including `human_explicit`) only when the header MAC
verifies against the expected local store key; absent/invalid/foreign MACs get
external handling and `human_explicit` is refused. `ee playbook import` caps
at `agent_validated` unless the same MAC passes.

Authenticated native reimport is restore/idempotence, not a rollback API. A
missing record may be restored and a byte-identical ID/hash is a no-op. If the
target already has the same ID with a different entity revision/hash, or a
tombstone/withdrawal that dominates it, the transaction reports a conflict
and never overwrites or resurrects local state; a newer artifact needs an
explicit normal mutation/merge path with its own lineage checks. Workspace or
artifact-family mismatch follows explicit external handling, never native
trust.

The bounded header MAC is verified before any record can receive native trust.
Import recomputes the ordered root/count while applying all rows inside one
rollback-capable transaction and commits only on an exact match. A malformed,
truncated, reordered, duplicated, or late-mismatching artifact therefore
leaves no partially privileged rows, audit entries, index jobs, or other
public side effects. This avoids an unbounded IDs-and-hashes preamble while
still authenticating every row and its position. External fallback is a
separate explicit application decision after verification failure, not a
downgrade performed halfway through a native import.

"Same-store reimport" and "disaster restore" are distinct. A normal export
does not make a lost store key recoverable, and the current redacted
`ee backup` format deliberately contains no private keyring. Restoring those
records on another store therefore follows foreign/external handling and
requires explicit local re-attestation; it never regains native trust merely
because the backup manifest or store UUID matches. If an operator separately
restores the complete user-data key directory through an external protected
system backup, ee may recognize the original key ID only after hardened
owner/type/path checks and a known-answer MAC self-check. That external
recovery is not represented as an `ee backup` capability. Rotation retains a
bounded verification window for same-store artifacts and rejects retired key
IDs outside it. **Rejected:** store-UUID comparison (identifiers leak via
support bundles; a leak reopens the bypass verbatim); adding private keys to
the current redacted backup; claiming full-trust restore from any data
artifact that does not independently recover its authentication key.

### TC-D15 — Schema and status-surface policy

Every executable `ee team` **leaf command** emits its own versioned schema
(`ee.team.*.v1`) with a drift test. Group nodes emit nothing, and sibling
leaves may reuse shared `$defs` but never share a top-level schema ID; flags
on one leaf may select explicitly tagged variants inside that leaf's schema.
This is the meaning of no subsumption, and a command-inventory contract test
fails when a new leaf lacks a schema mapping. The reserved-never-published
`ee.mesh.peer_status.v1`
name is **retired**: mechanism-level posture stays on the existing
`ee.mesh.auto_status.v1` / foreground status surfaces; team-level posture is
`ee.team.status.v1`. The `ee.mesh.import_ledger.v1` inspection surface is
owned by the import-policy bead (bd-tc-epic-qzk7o.2.1) — shipped with it or
explicitly deferred in its closeout, never silent. Mechanism contracts also
register
`ee.mesh.tailscale_transport_frame.v2`, `ee.mesh.pair_rotation.v1`, and
`ee.mesh.rotate_pair.v1`; dead frame v1 is a rejected input, not a
compatibility alias. The existing `ee.mesh.event.v1` registry entry remains
file replay/import-ledger evidence only and is never advertised as a live
team origin-stream capability. Deterministic retrieval
surfaces use immutable origin `producedAt` (or omit a time field), rendered
as absolute RFC 3339 only — relative phrasing ("2h ago") is allowed solely
in non-deterministic human surfaces (`team status`/`activity` human mode);
local `receivedAt`/`syncedAt` never participates in pack/search hashes or
cross-node equality once the canonical materialized corpus and local
maintenance state are held fixed. Receipt times remain available in `why`,
status, and audit diagnostics. A signature authenticates who asserted
`producedAt`, not that their clock was correct: machine provenance labels it
`originTimeAssurance = "member_attested"`. It is provenance/display data and
never drives authorization, trust/elevation caps, retention or decay,
lifecycle mutation, or search/pack relevance ranking. Peer material keeps a
separate local first-receipt/lifecycle clock for those local-only operations;
the origin claim is not copied into an authoritative local `created_at`.
Default search and pack assign every team-synced candidate the same neutral
temporal multiplier; neither the asserted origin time nor a local
receipt/sync time may enter the relevance score, tie-break, or selection.
Canonical team output does render the signed `producedAt`, so changing that
signed claim legitimately changes provenance bytes and any hash over those
bytes even though selected IDs, ordering, and relevance scores stay fixed.
An explicit user-requested time-window filter may compare the
member-attested `producedAt`, but its response must return the resolved
cutoff/as-of and `originTimeAssurance`; this is attributed filtering, not a
freshness or lifecycle authority. Local decay/expiry may independently use
the first-receipt clock, so nodes whose maintenance has materialized
different corpora are not valid byte-equality fixtures; the differing local
maintenance state must be surfaced rather than mislabeled as ranking
nondeterminism.
JSON activity queries require an explicit `--as-of`; when `--since` is
present it must be an absolute cutoff. Relative `--since 2h` is normalized
only in human output and the resolved cutoff/as-of are printed. Activity is
paged with a positive `--limit` (default 100, hard maximum 1000) and the
shared generation- and parameter-bound `ee.cursor.v1` codec; an invalid or
stale cursor returns the existing empty-page `cursor_invalid`/`cursor_stale`
posture rather than silently restarting. Activity orders ordinary rows by
`(producedAt DESC, eventId ASC)`, but a claim later than
`as_of + MAX_ORIGIN_CLOCK_SKEW_SECONDS` (pinned at 600 in v1) is excluded from
the recent-time bucket and reported in a deterministic `clockAnomalies`
collection until wall time catches up. This limits clock-skew ranking abuse
without pretending the member-attested clock is authoritative. A member can
also backdate an event out of an explicit time window; the schema therefore
labels the filter basis `member_attested` and warns that a time-window page is
not sequence-complete. Draining pages without `--since`, or using the
origin-sequence-based team audit surface, remains the completeness path for
all admitted events.

### TC-D16 — Overlap precedence: local workspace > team > global;
contradictions surface

On overlap, more-specific context wins (mirrors bd-1bfwa's
workspace-beats-global). On contradiction, neither silently wins — the pair
routes to the conflict surface labeled by lane; pack assembly never resolves
cross-lane contradictions by rank. The precedence constant lives in one
module cited by both the team and global lanes.

## Threat-model delta (controls required, extending ADR 0037)

| Threat | Control |
|---|---|
| Forged event origin or relay | TC-D4 domain-separated Ed25519 origin signatures with strict verification and TC-D5 dual-signed generation continuity; pairwise session MACs authenticate transport, not authorship |
| Existing unsigned file-replay event is mistaken for team origin authority | TC-D3 keeps `ee.mesh.event.v1` on the separate non-origin-authoritative export/import-ledger surface. It may inform local policy-capped evidence but is never re-signed, relayed, or reinterpreted as a typed team origin event |
| Relay mutates an unsigned event identifier | TC-D3 requires `eventId` to be the exact full-digest derivation of the signed `eventHash`; mismatch is rejected before idempotence/storage |
| A valid origin key equivocates | TC-D4 retains both signed branches, rolls materialization back to the common prefix, suspends the origin, relays fork evidence, and requires another member to revoke/rebind; no first-arrival winner |
| Frame replay, key-identity downgrade, or wrong-target/cross-workspace forwarding | TC-D1/D5 reject dead frame v1 and use frame v2 random ee-node IDs, team/endpoint-workspace/session/direction/counter/request binding under directional keys. Stable Tailscale IDs live in the handshake and current public keys remain observations; producer-owned `origin_workspace_id` is provenance and can never select either receiving database |
| Spoofed bootstrap identity / rate-limit evasion | TC-D2 derives identity from accepted source IP via LocalAPI WhoIs; pre-auth source-IP + global buckets ignore claimed headers |
| Unauthenticated traffic amplifies durable audit/storage | TC-D2 permits no durable mutation before invite proof; unknown-node and malformed bootstrap traffic affects only bounded in-memory source-IP/global counters and aggregate status metrics, never one durable row per attempt |
| Authenticated peer fills the append-only ledger with small valid batches | TC-D3 charges cumulative inbound storage to the signed origin across relays/key rotations, enforces per-origin/team/free-space ceilings transactionally, reserves only bounded control capacity, and never charges local source truth |
| Network request selects a local workspace/path | TC-D1 user-scoped broker routes only exact pre-registered team/workspace or invite IDs, never accepts a path from the wire, and revalidates owner-safe database identity/genesis before serving |
| One workspace daemon monopolizes the shared port | TC-D1 listener owner multiplexes validated routes through a same-EUID bounded local control channel; startup and route ambiguity fail closed rather than spawning another listener |
| Multiple OS users or incompatible team ports contend on one node | TC-D1 roots/invites commit one v1 port, all broker routes must agree, one OS user owns the host-wide listener, and other users/mismatches are explicitly client-only with doctor repair; no scan/fallback |
| Windows client-only mode weakens credential storage | TC-D5 requires DACL/reparse-point/opened-identity/write-through parity through a reviewed safe adapter. If unavailable, `mesh_key_store_unavailable` blocks team key operations; no listener does not mean best-effort secrets |
| Windows body hydration publishes sensitive bytes with Unix-only assumptions | TC-D12 reuses the reviewed secure-local-file adapter for reparse-point rejection, pinned file identity, narrow non-inherited DACLs, and write-through atomic publication. If proof fails, the body lane stays metadata-only with high `mesh_body_cache_lifecycle_failed`; weaker cache storage is never accepted |
| Ambient or repository-local Git state rewrites project identity | TC-D8 runs canonical Git directly with bounded/reaped execution, a minimal cleared environment, replacement/lazy-fetch/optional-lock behavior disabled, and nonempty grafts rejected; an installed Git lacking a required safety option is never retried weakly. Fallback reads exactly one raw local `origin` URL with includes and nonlocal config disabled, so aliases, prompts, rewrites, ambient object alternates, or multiple URLs cannot choose a project key |
| Wrong process answers the inviter port first | TC-D2 requires the invite-pinned Ed25519 challenge over root/identity/nonces/port before the joiner releases the secret |
| Invite interception / local secret exposure | Single-use leased redemption, TTL, hashed at rest, zeroizing buffers, no argv/env/log ingestion, explicit secret re-entry before key confirmation, and mutual key confirmation; an unbound code remains a bearer credential whose one redemption can be stolen, so the UI names that residual and recommends short TTL/`--wait`/identity policy |
| Invite locator redirects, goes stale, or breaks on routine key rotation | TC-D10 binds the exact inviter Tailscale stable node ID, ee identity, and team root; fresh local status resolves its current key/IP, treats embedded key/MagicDNS/IP as observations only, accepts rotation only within that stable binding, and requires reissue when the stable ID disappears/changes |
| Pair rotation crashes, clock rolls back, or a peer forces old-key fallback | TC-D5 uses an explicit control-only capability and two-phase generation record. The prior key can authenticate only the exact pending rotation for 86400 seconds; persisted wall-time high-water plus same-process monotonic elapsed make rollback fail closed, and expired/unverifiable state requires fresh pairing |
| Clock rollback extends an invite or per-pair introduction bearer credential | TC-D10 advances a persisted invite-authorization floor on every credential decision and uses same-process monotonic deadlines. Rollback blocks mint/redeem/resume with `team_invite_clock_rollback`; repair atomically revokes every pending invite/lease/introduction before lowering the floor, so no credential can reactivate |
| Compromised member | Data-lane blast radius is the member's node-scoped grants (per-node lanes, elevation toggle, harmful-feedback demotion, revocation). Manifest authority (lane profile, idp policy, removals) is any-active-member in v1 — mitigated by audit + conflict surfacing + `ee team pause`; roles are a v2 question |
| Compromised member widens lane or relaxes IdP policy | TC-D9 treats manifest policy as coordination input: lane widening cannot mint local grants, and every node retains its explicitly accepted IdP floor until that operator accepts the exact relaxation generation |
| Compromised member narrows lanes or tightens IdP policy | This remains an availability authority in v1: narrowing/tightening may pause sharing but cannot exfiltrate data. Status/audit identify the author and generation; another active member can revoke the attacker and reconcile policy. Roles/quorum are deferred, so the residual is explicit |
| Compromised member floods membership | TC-D9 hard-caps active membership at 20 and turns a complete-set overflow into a sharing-blocking capacity conflict; no attacker-grindable event/hash winner is selected. A valid member can still cause an availability incident, consistent with its other v1 manifest authority |
| Compromised member binds unlimited nodes | TC-D9 caps each member at four active ee nodes; predecessor-rooted concurrent additions commute only within the cap and otherwise all conflict without granting a winner |
| Removal inputs change after the operator previews them | TC-D9 binds root/materializer generation, target, nodes/signing generations, per-origin cutoffs, accepted-prefix additions, acknowledgement audience, and non-erasure residual into the required preview hash; transaction-time mismatch returns `team_member_removal_preview_stale` with zero removal/authorization/invite/audit/outbox/connection/fanout effects |
| An already-open session races node/member revocation | The removal transaction advances durable session/grant authorization generations. Every frame handler rechecks before import/serve; socket cancellation is idempotent after commit, so SQLite is never claimed to close memory state and a crash cannot reauthorize the old generation |
| Removed member's pre-authored authority | TC-D9 signed per-origin cutoffs make rejection arrival-independent. Accepted earlier adds remain active and are therefore an explicit v1 residual, not silently described as revoked: `addedByRemovedMember` plus `team_delegated_member_review_required` persists until each operator acknowledges or separately removes them, with team pause recommended during review |
| Mutually invalidating member removals | TC-D9 detects removal dependency cycles as a complete-set manifest conflict, preserves pre-conflict membership, and pauses affected sharing pending reconciliation |
| Removal propagation latency | TC-D9 bounded foreground fanout + signed relay + acknowledgement matrix; no false bound when nobody receives the removal |
| Local `human_explicit` minting amplified team-wide | TC-D7 controls: basis in `why`, per-member counts, atomic local-admission velocity cap independent of untrusted origin time, clock-rollback high-water mark, canonical batch order, and burst code |
| Inviter omits/fabricates manifest at join | Root/origin signatures prevent alteration and false authorship; direct sync/reconcile exposes omission, but an inviter-controlled sock puppet remains possible and is stated |
| Stolen/reassigned node or routine Tailscale key rotation | Stable node ID + ee pair/signing continuity distinguishes normal current-key rotation from a new device; tier-1 attestation additionally suspends on owner mismatch |
| Self-attested, duplicate, or stale tier-2 identity | TC-D9/D13 make the distinct verifier host the device flow, reject self/same-subject renewal, conflict duplicate subjects, deterministically combine concurrent policy-generation-bound finite leases, perform no steward IdP HTTP, and suspend after cadence plus grace if interactive renewal cannot complete |
| Clock rollback extends an identity lease | TC-D9/D13 use a persisted nondecreasing local authorization-time floor on every identity-dependent decision; forward-jump recovery suppresses every current tier-2 lease before lowering the floor and requires fresh interactive attestations |
| IdP discovery SSRF / token theft | Secretless public-device-client preflight; verifier-hosted flow so raw tokens never cross ee; fresh discovery/JWKS per presentation; HTTPS-only constrained canonical curl from a minimal allowlisted environment with ambient config/proxy/netrc/CA/keylog state disabled, no redirects for credential-bearing POSTs, manually validated GET redirects, validated-and-pinned DNS answers, exact issuer, bounded/reaped subprocess I/O, redacted responses, raw token zeroization/non-persistence, key/algorithm/verified-email/group-claim checks, atomic replay ledger, grace-then-suspend |
| Malicious or malformed IdP keeps a device ceremony polling | RFC 8628 interval/`slow_down` semantics run under the earlier of provider expiry and a 1800-second monotonic deadline plus a 300-request ceiling; checked backoff, cancellation/reap, terminal-error handling, and explicit non-automatic restart bound network/process use |
| Trust laundering via file import | TC-D14 store-local MAC |
| Peer rows echo or acquire local authorship | TC-D3 emits only origin-owned local rows; inbound materialization never re-emits and in-place peer edits are rejected |
| A paired/signed peer reaches team materialization before membership authorization exists | TC-D3 registers but does not advertise `mesh.team.memory.v1` or `mesh.team.manifest.v1` until T4.1 installs the active-member/node/key authorizer; T2.4 depends on that gate. Pair/session proof alone can never apply or relay team authority, and cross-origin predecessor gaps quarantine for deterministic re-evaluation |
| A signed origin omits a required feature bit to bypass a stricter operation handler | TC-D3 derives mandatory features and authorization from payload schema + operation, never from the origin's list alone. Missing mandatory bits quarantine under `mesh_event_feature_contract_invalid`; unknown additional bits remain replayable `unsupported` state |
| Join silently publishes old local history | TC-D3 starts future live projection only; `ee team share history` uses a revision-pinned preview, explicit consent, revalidation, and an idempotent projection ledger |
| New node inherits a member's sensitive grant | TC-D6 grants current nodes only; later bindings return to metadata-only until a fresh per-node preview and consent |
| Inbound abuse on the open port | TC-D2 bootstrap caps at listener birth; full admission control in operations |
| Policy-denied event stalls later sync | TC-D3 contiguous receipt/disposition-scan frontiers + sparse audited per-event dispositions; materialization reads only explicit applied rows |
| Policy denial suppresses a later purge | TC-D3 makes minimal opaque `tombstone`/`shareWithdraw` controls mandatory for active members, so current content policy cannot strand previously admitted derived material |
| Body hash used as an oracle | TC-D12 exact event/requester/grant authorization and bounded sequenced chunks |
| Serve-time redaction changes bytes while claiming the signed source hash | TC-D12 performs no in-flight body transformation in v1. `redact` serves only an `already_redacted` representation whose exact bytes/hash and redaction provenance were signed in the event; an `exact` body that now requires redaction stays metadata-only. Both streamed and declared hashes must equal the signed `content_hash` |
| A relayer turns its cached body into a new serving authority | TC-D12 permits body serving only by the event's owning origin workspace/node from local source truth. Relays carry signed metadata/events but never re-serve cached bodies; missing old revisions return unavailable rather than substitution |
| Body-lane grant is revoked after bytes were fetched or on only one source node | Revocation stops future serving from the named local source and advances the node-scoped grant generation, but cached/copied bytes cannot be remotely erased and other source nodes are unaffected. Command/audit output says both; origin-wide `shareWithdraw` is a cooperative purge control, not proof of deletion on an offline or malicious peer |
| Member lies about origin time to stay fresh or backdates out of a recent-activity window | TC-D15 makes `producedAt` provenance-only for trust/default ranking/lifecycle, requires explicit activity `as_of`, and isolates claims beyond the pinned future-skew window in deterministic clock anomalies. A member-attested time filter is explicitly not an audit-completeness boundary: output names the basis/backdating residual, while an unfiltered cursor drain and origin-sequence audit retain every admitted event |

## Non-goals (v1)

No cloud relay or SaaS control plane; no CRDTs, gossip, Paxos, or
linearizability; no eager full replication or federated search; relay is
peer-assisted only (no always-available relay service); no multi-team
membership per workspace; no web UI; no wizard TUI; no Windows daemon; no
embedding or graphLink team-UX sharing verbs (lanes exist, `ee mesh grant`
remains the power-user path); per-peer selective-sync subscriptions stay
display-only; no IdP round-trip in any core command. V1 teams are trusted
small groups; untrusted contractors belong in a separate tightly scoped team,
not an ordinary member slot with manifest authority.

## Verification hooks

- Mesh-off invariants: `tests/mesh_off_no_network.rs` extended — daemon with
  mesh off binds nothing; `ee team` commands add zero degraded noise.
- Two-node loopback harness (real binaries, real sockets): matching pair keys
  pre-provisioned only through the production hardened key-store API as test
  setup, then real fresh-session handshake, sync, attribution,
  partition/rejoin, fork rejection, withheld-payload cursor progress, removal
  cutoffs, replay/target rejection, flood caps,
  policy-denied lane and planted secret never cross; invite ingestion tests
  assert the secret is absent from argv, environment, logs, audit, and
  structured errors. Nodes bind distinct loopback source addresses so fake
  WhoIs exercises accepted-source identity rather than a port-based shortcut.
  No public raw-key import flag or test bypass ships; unkeyed production peers
  receive pairing-required guidance until the M2/M3 ceremonies complete.
  Unknown-node/bootstrap floods leave durable DB and audit row counts
  unchanged while bounded in-memory counters and backoff still activate.
  Authenticated small-batch flooding crosses the cumulative signed-origin
  ceiling atomically: no partial batch/frontier/index/audit side effects land,
  relaying or key rotation does not reset accounting, other origins continue,
  control-only reserve is bounded, and local `ee remember` still succeeds.
- Responder ownership: daemon already running, foreground-only, simultaneous
  daemon/`invite --wait` start, and unrelated-port-occupant cases prove one
  listener owner, correct control-channel delegation, and no alternate-port
  or wildcard fallback. Two distinct local workspace databases register with
  the same owner and route only by exact target IDs; every session/frame binds
  both the initiator-local and responder-target endpoint workspaces, while
  relayed events keep their independent producer `origin_workspace_id`.
  Origin-as-target confusion, stale/moved/symlinked,
  cross-workspace, wrong-genesis, different-EUID, replayed, and
  network-supplied-path attempts fail without leaking local paths.
  Root/local port mismatch, two registered teams with different committed
  ports, another OS user/process owning the host-wide port, and client-only
  posture are diagnosed; no case scans or falls back. Starting before
  tailscaled is ready binds nothing and retries; tailnet-address loss closes
  the listener, and a verified address-set replacement drops stale sockets
  before binding only the new set.
- Invite locator cases: stale embedded IP/current-key hints resolve through the
  same pinned stable node ID; routine current-key rotation for that stable ID
  succeeds with pair/ee-key continuity, while a missing/changed stable ID or
  hint pointing at another stable node fails before secret transmission.
  Injected-RNG format tests prove invite and durable ceremony IDs each carry
  at least 128 independent random bits and are not derived from the secret or
  public identity; unknown IDs get only the bounded generic decline.
  A fake process on the correct host/port cannot receive the secret because
  its inviter challenge does not verify against the invite-pinned signing
  identity/root/nonces/port; challenge replay and root/port substitution fail.
  Signing rotation after invite mint succeeds only with a contiguous
  dual-signed transition chain that fits the bootstrap budget; a gap,
  oversized chain, node/member revocation, or fork-block invalidates the
  invite and requires reissue.
  With no daemon, plain `ee team invite` waits by default; `--no-wait` without
  a broker is refused, and interruption/resume preserves only the hashed
  invite.
- Invite-time safety: rollback at every mint/lease/redeem/resume/revoke and
  introduction-secret authorization boundary, including restart, cannot
  extend a credential. Same-process monotonic expiry wins over wall rollback;
  a forward jump may expire early; doctor repair revokes all pending
  invites/leases/introductions atomically before lowering the persisted floor,
  and none reactivates.
- Three-node scenarios: a signed event relays unchanged; forged relays fail;
  one origin signs different branches to two peers, direct reconciliation
  leaves both at the same fork-blocked prefix with both proofs retained;
  remover goes offline after one acknowledgement and the removal still
  propagates; unacknowledged exposure remains visible; a target's
  later-revealed origin omitted from the removal cutoff map has zero retained
  authority; deferred pairing completes.
- Directionality: one outbound connection performs requests in both
  directions without opening an initiator listener; a client-only node sends
  and receives in its manual round, two client-only nodes cannot connect, and
  removal fanout succeeds through tip advertisement plus a peer range request
  without any `event_push` frame.
- Pair-key rotation: crash/restart at every stage/accept/commit/promote
  boundary converges through the exact rotation record initiated by
  `ee mesh rotate-pair`; a key-store
  manifest/DB projection mismatch blocks and repairs from the hardened
  manifest. Golden vectors pin initial and rotation transcript bytes, context
  strings, length prefixes, derived keys, and role-bound confirmations. For
  86400 seconds the prior key authenticates only rotation-resume handshake
  messages, never ordinary sessions/frames; wrong transcript,
  generation, concurrent transition, automatic downgrade, persisted-clock
  rollback, and post-grace resume fail, with fresh pairing as the repair.
  `ee team members rotate-key` output is signing-lineage-specific.
- Key-store parity: Unix mode/owner/no-link/fsync and Windows
  SID/DACL/reparse/opened-file-identity/write-through vectors cover
  create/read/rotate/crash and attacker-controlled parents. A Windows build
  without the reviewed safe adapter emits `mesh_key_store_unavailable` and
  blocks join/sync/rotation; it never shells to repair ACLs or adds
  project-owned unsafe code.
- Migration-safety test: row counts + content hashes survive the trust-class
  rebuilds.
- Determinism: J7 harness extended with team-attributed packs using immutable
  origin time as provenance only; byte-identical output on both nodes given
  equal signed origin events, canonical materialized corpus, materializer
  version, local disposition/admission decisions, maintenance state, config,
  and indexes, regardless of diagnostic receipt/sync timestamps. Changing a
  signed origin claim changes rendered provenance bytes but not selected IDs,
  ordering, or default relevance scores. Local first receipt may affect a
  later maintenance decision and therefore the materialized corpus; that
  state difference is explicit and outside a fixed-corpus comparison. Forged
  future origin times do not alter default relevance/lifecycle and land in
  deterministic activity `clockAnomalies` until the explicit as-of window
  reaches them. Activity tests also prove a backdated event may be omitted
  from an explicitly member-attested time window, remains present in an
  unfiltered cursor drain and origin-sequence audit, and is never falsely
  described as absent from the admitted stream.
- Degraded-code discipline: every new code lands with fixture + taxonomy in
  the same commit (J6 validator); the 19 uncovered legacy mesh codes are
  backfilled first.
- Fuzz/property: frame-v2 decode plus v1 downgrade rejection, bootstrap
  envelope, origin/payload codecs, invite codec; property tests cover random
  ee-node/team/endpoint/session/direction/target binding, counter replay,
  manifest genesis uniqueness/root binding, arrival-order independence,
  mutual-removal cycles, and withheld-payload cursor progress. Canonical
  event/payload and dual-signed signing-key transition known-answer vectors
  are stable across encode/decode and field insertion order; changing
  `eventId` without changing the signed digest is always rejected.
  A pre-authorizer feature set advertises neither base team feature and
  cannot apply/relay memory or manifest events; after T4.1, a
  base-memory/manifest feature set dispositions `identityAttested` as
  unsupported because its additional feature bit is absent. Out-of-order
  member authority quarantines and later re-evaluates without an arrival
  winner. Removing any mandatory feature from a signed operation yields
  `mesh_event_feature_contract_invalid` and never downgrades dispatch;
  unknown extra features remain durable `unsupported` state.
- File/live schema boundary: existing unsigned `ee.mesh.event.v1` artifact
  rows remain non-origin-authoritative and locally policy-capped; typed signed
  events reuse the normalized admission decision without reserialization.
  Re-signing, relaying, or reinterpreting a legacy file row as team authority
  is rejected, and both schema purposes remain explicit in inventory tests.
- Import authentication (TC-D14): a teammate's export cannot inject
  `human_explicit`; an artifact with a correct store UUID but no valid MAC
  is refused; context-matched same-store reimport restores missing rows or
  no-ops on byte-identical rows, while divergent revisions and dominating
  tombstones/withdrawals conflict without overwrite/resurrection; current
  `ee backup` artifacts
  contain no keyring and restore only with external trust plus local
  re-attestation. A separately restored user-data key directory is accepted
  only after path/owner/type and known-answer MAC checks. Snapshot mutation,
  truncated, reordered, duplicated, wrong-count, and final ordered-root
  mismatch artifacts leave zero native-trust rows, audit entries, or index
  jobs; the authenticated preamble stays constant-size.
- Project identity (TC-D8): two clones at different paths derive equal keys;
  object-format-tagged multi-root sets agree independent of order; shallow
  repositories are detected before boundary commits can masquerade as roots;
  replacement refs are ignored, local grafts fail closed, lazy promisor fetch
  and ambient Git relocation/config/trace variables cannot affect identity; a
  fake/old Git missing a required safety option is not retried without it and
  reaches only the explicit degraded fallback/mint path. Fallback never
  silently changes after unshallow, root-set addition, history rewrite,
  object-format conversion, or remote rename; missing `origin` never selects
  an arbitrary remote, and multiple distinct raw local `origin` URLs remain
  ambiguous; explicit alias/separation reconcile and Git-without-origin/
  non-git mint/adopt round-trip.
- Rematerialization (TC-D4/D9): exact stream traversal vectors and
  complete-set conflict fixtures produce one canonical projection hash;
  same-sequence divergent hashes fork-block before reduction; invalid rows
  become non-retrievable before a crash at every bounded checkpoint; resume
  never duplicates audit/index/cache-outbox effects or publishes a partial
  generation. Recorded local trust decisions replay without refunding or
  double-consuming velocity, and later T3.4/T5.9 integration arms preserve
  the same reducer contract.
- Precedence and conflicts (TC-D16): planted cross-lane contradictions are
  surfaced, never resolved by rank; the precedence constant is imported by
  both team- and global-lane tests.
- Policy floors (TC-D9): a remote lane widening produces zero new grants; a
  narrowing applies; stricter comparable IdP policy applies; relaxation and
  incomparable issuer/group changes remain pending until exact-generation
  local acceptance, including restart and arrival-order permutations.
- Membership bounds and node continuity (TC-D5/D9): complete-set additions
  over 20 produce the same capacity-conflict posture under every arrival
  permutation; node-set successors over four active nodes for one member
  produce the same node-capacity conflict and preserve the predecessor set
  under every arrival permutation; ordinary Tailscale current-key rotation
  under one stable ID
  preserves the ee node/grants only with pair/signing continuity and does not
  change the opaque peer handle, while a changed stable ID and lost-last-node
  recovery require fresh consent. An old key-derived peer record without a
  ceremony-proven stable binding is blocked with upgrade guidance, never
  auto-bound; low-level grant preview/mutation resolve peer handle to the
  exact ee-node/generation and cannot be retargeted by key rotation.
  Member-removal preview hashes bind every root/generation/node/cutoff/
  delegated-member/acknowledgement input; changing each one before apply emits
  `team_member_removal_preview_stale` and leaves removal, authorization
  generations, invite invalidation, audit/outbox, connection cancellation,
  and fanout untouched. A successful revoke advances the durable generation
  before open sockets are cancelled, and a planted old session cannot import
  or serve after commit.
  Removing a member whose accepted prefix added another member leaves that
  addition honestly active, emits `team_delegated_member_review_required`,
  and keeps `addedByRemovedMember` visible in preview/status/doctor until
  local acknowledgement or separate signed removal; no test treats the flag
  itself as revocation.
- Elevation controls (TC-D7): harness matrix covers elevation on/off, rows
  over the velocity cap landing as `agent_validated` with
  `team_member_elevation_burst`, and the three `human_explicit` rejection
  points asserted unchanged; `create` and every content-bearing `revise`
  consume exactly one slot after idempotence, replay consumes zero, and an
  over-cap revise cannot inherit an earlier elevation; adversarial origin
  times, clock rollback, batch permutation, and concurrent imports cannot
  reopen or overrun the cap.
- Projection boundaries (TC-D3/TC-D6): importing/indexing/curating peer rows
  creates no local origin event; a local derivative has a new ID and explicit
  provenance; future-only activation, revision-pinned history preview,
  history/live-mutation races, changed-after-preview skips, crash-resume, and
  new-node-no-grant-inheritance are exercised end to end. A member joining
  after confirmed history projection receives that durable metadata, while
  history already covered by an origin-wide `shareWithdraw` does not
  rematerialize for the joiner.
- Body-cache lifecycle (TC-D12): interrupted, oversized, hash-mismatched,
  policy-denied, quarantined, evicted, expired, and withdrawn cases prove no
  unverified object becomes retrieval-addressable; Unix owner/mode/no-symlink
  and Windows SID/DACL/reparse/opened-identity/write-through vectors cover
  temporary and published objects, crash boundaries, and hostile parents.
  Crash injection at every transition proves publication remains invisible
  through `staging` until object rename + fsync complete, while invalidation
  closes retrieval/index eligibility in `invalidated_pending_purge` before
  physical removal. Restart/steward/doctor reconciliation handles staged
  orphans and pending purges idempotently, and filesystem presence alone can
  never resurrect availability.
  Failure to prove the platform contract leaves metadata retrieval usable,
  emits high `mesh_body_cache_lifecycle_failed`, and publishes no body.
  Support-bundle leak tests cover bytes and paths; withdrawal removes only
  derived objects and never origin-owned source truth. A peer that previously
  admitted content still receives and applies its minimal withdrawal control
  after later content denial. Exact and `already_redacted` signed
  representations both require streamed/declared hash equality with the event;
  an exact body under a redact-only policy remains metadata-only, no in-flight
  transformed bytes can masquerade under the source hash, a new scanner denial
  sends nothing, relayers cannot serve cache copies, and an unavailable old
  source revision is never replaced by current bytes.
  Per-recipient `revoke-lane`/`team unshare bodies` advances the exact-node
  grant generation and stops future fetches, invalidates stale previews, and
  never claims already-cached/copied bytes were erased or emits an
  origin-wide `shareWithdraw`. `--all-members` names the current local source
  node and proves another known source node remains unaffected.
- Opt-in real-tailnet smokes assert real rounds/joins when enabled, exit-78
  skip-clean otherwise.
- Fake IdP harness covers every tier-2 scenario offline, including the bounded
  token-free `identity_attest` session, rejection of providers requiring a
  client secret, a fresh discovery/JWKS fetch for every new presentation, DNS
  rebinding, cross-origin redirect/body-leak attempts, weak RSA/wrong-curve
  keys, retired-key new-token rejection, no-token-persistence/zeroization,
  concurrent replay-ledger claims, self/same-subject attestation rejection,
  concurrent lease arrival permutations, duplicate-subject conflict,
  one-member activation bootstrap and zero-grace refusal, distinct verifier
  loss, finite-lease expiry, cadence-plus-grace suspension with zero
  background IdP HTTP, clock rollback at every identity-dependent
  authorization path, fail-safe forward jump, clock-floor repair that cannot
  reactivate an old lease, ambient proxy/`.curlrc`/netrc/CA-bundle/TLS-keylog
  traps, unverified-email and malformed-group claims, credential-POST
  redirects, oversized pipes, timeout/descendant-pipe escape, process reap,
  response redaction, missing/zero/overflowing `expires_in`/`interval`,
  omitted-interval default 5, no-early-poll timing, cumulative `slow_down`
  increments, connection-timeout backoff, provider/local/poll-budget expiry,
  cancellation with process reap/zeroization, no automatic ceremony restart,
  process loss returning the outer join/renewal to `identity_pending` without
  persisting or reusing device ephemera,
  duplicate JSON member names, noncanonical JWT segments,
  unsupported critical headers, token-controlled/embedded key references,
  and missing/duplicate/ambiguous `kid` selection.
  Synthetic unrelated claims and oversized group membership prove that only
  the previewed minimal subject/optional-email/configured-group-match
  evidence persists or replicates; ceremony URLs/codes/poll state are gone
  from DB, manifest, audit, logs, and support bundles after their TTL.

## Consequences

Positive: the ~8.4k LOC of dead mesh modules gain production callers (all
except `anti_entropy_model.rs`, which stays an executable spec); the
mesh's consent/audit machinery becomes the team product's foundation instead
of shelf-ware; non-technical teams get a three-command setup with a
cryptographically attributable record of locally observed consent, grants,
events, and sync outcomes. Costs: new migrations including multi-table
trust-class rebuilds; a listener surface that must be defended (WhoIs-bound
bootstrap caps + admission); Ed25519 key lifecycle and a new audited
dependency; separate receipt/disposition-scan frontiers plus sparse
per-event materialization state; two new identity registries (members,
projects); new degraded codes and schemas with their gates; documentation
whose fitness tables and status headers must flip as
surfaces ship. V1 also makes an explicit operational tradeoff: one immutable
team responder port, one responder-capable OS user per Tailscale node, and
client-only posture for mismatches rather than ambiguous port discovery or
cross-user routing. The plan's
milestone gates (M0–M6) are the acceptance contract; the program closeout
bead owes a verification-matrix-style ledger of every child and every
deferral.
