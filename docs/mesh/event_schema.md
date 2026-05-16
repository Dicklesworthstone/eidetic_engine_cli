# Mesh Event Schema

Status: proposed
Bead: bd-2gtjn
ADR: docs/adr/0037-optional-mesh-memory.md
Schema: docs/schemas/ee.mesh.event.v1.json

## Purpose

`ee.mesh.event.v1` is the transport-independent envelope for optional mesh
memory. It is an append-only replay record, not a database row dump and not a
network protocol. Later export, import-ledger, anti-entropy, and two-node
fixture work should emit this shape before any peer material can affect local
search, context, graph, or curation behavior.

## Identity

Each event carries three separate identities:

- `originNodeId`: the stable producing node.
- `originWorkspaceId`: the producing workspace namespace asserted by the event.
- `logicalMemoryId`: the memory-level identity being created, revised,
  tombstoned, trusted, or announced.

The collision-resistant global identity is:

```text
global-memory-id = originWorkspaceId + "/" + logicalMemoryId
```

Receivers must still pass the event through local peer-group and lane policy
before importing it. Matching identifiers alone are not authorization.

## Hash Rules

`eventHash` is computed over canonical JSON for the event with `eventHash` and
`eventId` temporarily removed. `eventId` is removed because it is mechanically
derived from the same digest; including it in the preimage would make the event
self-referential and unrecomputable. Canonical JSON means:

- UTF-8 JSON with no insignificant whitespace.
- Object keys sorted lexicographically at every depth.
- Arrays preserved in their original order.
- Integers encoded as JSON integers and strings preserved byte-for-byte.
- `null` fields are retained when present in the schema-required event shape.

The hash string is:

```text
blake3:<64 lowercase hex characters>
```

`eventId` is derived from the same digest:

```text
mesh_evt_<64 lowercase hex characters>
```

`prevEventHash` links the previous accepted event from the same
`originNodeId`/`originWorkspaceId` stream. It is `null` for the first event in a
stream. Importers use `(originNodeId, originWorkspaceId, seq, eventHash)` as the
idempotence key and reject replays where the same `(originNodeId,
originWorkspaceId, seq)` maps to a different `eventHash`.

## Required Features

`requiredFeatures[]` is a forward-compatibility gate. A receiver may replay an
event only when every entry is known. Unknown required features must produce a
structured reject/quarantine decision instead of being silently ignored.

The v1 baseline feature understood by this contract is:

```text
mesh.event.v1
```

Future features must use the `mesh.<name>` namespace and must be documented in
the same commit that first emits them.

## Event Kinds

| Kind | Meaning |
| --- | --- |
| `create` | Introduces a logical memory identity and metadata. |
| `revise` | Points a logical memory identity at revised content or metadata. |
| `tombstone` | Withdraws a logical memory identity from future import/search use. |
| `trust` | Updates trust posture without changing body content. |
| `validity` | Updates validity windows for time-bounded evidence. |
| `bodyAvailable` | Announces policy-gated body availability for a previously metadata-only item. |

Event kind semantics are intentionally narrow. Storage, cursor, and replay
beads may add tables and commands later, but they must preserve this canonical
hash and identity contract.
