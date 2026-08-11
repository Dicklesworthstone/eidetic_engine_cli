-- DRAFT — pre-implementation artifact for bd-tc-epic-qzk7o.4.1 (T3.1),
-- banked 2026-08-11 by ScarletMill so the claimer starts from mechanical DDL
-- instead of prose. NOT a compiled migration: T3.1 allocates the real
-- version from the dueling-wizards registry (next planned slot V105+ is
-- RESERVED for doc-only allocations 105-107 — coordinate; the compiled tail
-- at draft time is V104). The bead description and ADR 0086 TC-D5/D6/D9/D10
-- override this file wherever they disagree.
--
-- Design rules encoded below:
-- * identity is RANDOM (mbr_/node_ ids); never derived from names, paths,
--   emails, hostnames, IPs, or current keys;
-- * one team per workspace (T4.x enforces at the command layer);
-- * post-genesis binding happens only via invite or an existing-node
--   ceremony — no operator-proof bypass column exists on purpose;
-- * a same-StableID current-key update PRESERVES the grant principal, while
--   a different/new node starts metadata-only and inherits nothing — the
--   grant maps to an exact (ee node, grant generation) pair;
-- * pending invite/join state stores hashes and non-secret phases only,
--   plus a per-team persisted NONDECREASING invite-authorization time floor
--   (rollback-safe: restoring an old backup cannot resurrect revoked
--   invites, because the floor row travels with the team and only moves
--   forward);
-- * lost last node cannot self-replace (no self-recovery path in schema).

CREATE TABLE team_members (
    member_id TEXT PRIMARY KEY CHECK (
        member_id GLOB 'mbr_*'
        AND length(member_id) = 30
        AND member_id NOT GLOB '*[^A-Za-z0-9_]*'
    ),
    team_id TEXT NOT NULL CHECK (
        team_id GLOB 'team_*' AND length(trim(team_id)) > 5
    ),
    -- Display label is presentation-only and NEVER identity.
    display_label TEXT CHECK (display_label IS NULL OR length(trim(display_label)) > 0),
    state TEXT NOT NULL CHECK (state IN ('active', 'removed')),
    -- Provenance of the membership (genesis | invite | ceremony).
    admitted_via TEXT NOT NULL CHECK (admitted_via IN ('genesis', 'invite', 'ceremony')),
    admitted_at TEXT NOT NULL CHECK (length(trim(admitted_at)) > 0),
    removed_at TEXT CHECK (removed_at IS NULL OR length(trim(removed_at)) > 0),
    CHECK ((state = 'removed') = (removed_at IS NOT NULL))
);

CREATE INDEX idx_team_members_team ON team_members(team_id, state);

CREATE TABLE team_member_nodes (
    origin_node_id TEXT PRIMARY KEY CHECK (
        origin_node_id GLOB 'node_*'
        AND length(trim(origin_node_id)) > 5
        AND origin_node_id NOT GLOB '*[^A-Za-z0-9_-]*'
    ),
    member_id TEXT NOT NULL REFERENCES team_members(member_id) ON DELETE CASCADE,
    team_id TEXT NOT NULL CHECK (
        team_id GLOB 'team_*' AND length(trim(team_id)) > 5
    ),
    -- Transport binding: non-empty StableID from the tailnet identity layer.
    -- The CURRENT rotating key is an audited OBSERVATION, not identity.
    transport_stable_id TEXT NOT NULL CHECK (length(trim(transport_stable_id)) > 0),
    observed_current_key TEXT NOT NULL CHECK (length(trim(observed_current_key)) > 0),
    observed_current_key_at TEXT NOT NULL CHECK (length(trim(observed_current_key_at)) > 0),
    -- Opaque mesh peer handle (T2.2 responder-broker semantics).
    mesh_peer_handle TEXT CHECK (mesh_peer_handle IS NULL OR length(trim(mesh_peer_handle)) > 0),
    -- Ed25519 signing lineage: generation is nondecreasing per node.
    signing_key_generation INTEGER NOT NULL CHECK (signing_key_generation >= 0),
    signing_public_key TEXT NOT NULL CHECK (length(trim(signing_public_key)) > 0),
    -- Exact grant principal: same-StableID key rotation preserves this pair;
    -- a substituted node gets a NEW row and inherits nothing.
    grant_generation INTEGER NOT NULL DEFAULT 0 CHECK (grant_generation >= 0),
    lane_posture TEXT NOT NULL DEFAULT 'metadata_only' CHECK (
        lane_posture IN ('metadata_only', 'body_eligible')
    ),
    provenance TEXT NOT NULL CHECK (
        provenance IN ('genesis', 'invite', 'ceremony')
    ),
    revoked_at TEXT CHECK (revoked_at IS NULL OR length(trim(revoked_at)) > 0),
    bound_at TEXT NOT NULL CHECK (length(trim(bound_at)) > 0)
);

CREATE INDEX idx_team_member_nodes_member ON team_member_nodes(member_id);
CREATE INDEX idx_team_member_nodes_stable
    ON team_member_nodes(team_id, transport_stable_id)
    WHERE revoked_at IS NULL;

CREATE TABLE team_pending_invites (
    invite_id TEXT PRIMARY KEY CHECK (
        invite_id GLOB 'tinv_*'
        AND length(invite_id) = 31
        AND invite_id NOT GLOB '*[^A-Za-z0-9_]*'
    ),
    team_id TEXT NOT NULL CHECK (
        team_id GLOB 'team_*' AND length(trim(team_id)) > 5
    ),
    -- HASH of the bearer secret; the clear secret never persists anywhere.
    invite_secret_hash TEXT NOT NULL CHECK (
        invite_secret_hash GLOB 'blake3:*' AND length(invite_secret_hash) = 71
    ),
    -- The stable node the invite is BOUND to (T4.2 stable-node-bound
    -- lifecycle); NULL only while the phase is 'issued'.
    bound_transport_stable_id TEXT CHECK (
        bound_transport_stable_id IS NULL OR length(trim(bound_transport_stable_id)) > 0
    ),
    phase TEXT NOT NULL CHECK (
        phase IN ('issued', 'presented', 'confirmed', 'expired', 'revoked')
    ),
    authorized_at TEXT NOT NULL CHECK (length(trim(authorized_at)) > 0),
    expires_at TEXT NOT NULL CHECK (length(trim(expires_at)) > 0),
    resolved_at TEXT CHECK (resolved_at IS NULL OR length(trim(resolved_at)) > 0)
);

CREATE INDEX idx_team_pending_invites_team ON team_pending_invites(team_id, phase);

-- Per-team nondecreasing invite-authorization time floor: invites with
-- authorized_at strictly BELOW the floor are dead regardless of phase.
-- The write API advances it monotonically in the same transaction as
-- revocations/joins (crash invariant: floor moves before an invite is
-- honored, never after).
CREATE TABLE team_invite_time_floors (
    team_id TEXT PRIMARY KEY CHECK (
        team_id GLOB 'team_*' AND length(trim(team_id)) > 5
    ),
    authorization_floor TEXT NOT NULL CHECK (length(trim(authorization_floor)) > 0),
    advanced_at TEXT NOT NULL CHECK (length(trim(advanced_at)) > 0)
);
