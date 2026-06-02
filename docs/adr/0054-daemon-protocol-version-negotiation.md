# ADR 0054: Daemon Protocol Version Negotiation

Status: Accepted
Date: 2026-06-02
Bead: bd-3fzzs

## Context

The daemon wire protocol is a local Unix-domain socket RPC surface with
length-prefixed JSON envelopes. Version 1 intentionally keeps the request
envelope small:

- `schema`
- `request_id`
- `agent_id`
- optional `workspace_id`
- `method`
- optional object `params`

The published JSON Schema uses `additionalProperties: false`, and the Rust
deserializer rejects unknown top-level fields. That strictness prevents silent
drift between the schema and the runtime, but it also means a future request
field or method cannot be added to v1 without a documented negotiation path.

Without discovery, a v2 client connecting to a v1 daemon would only learn that
its request schema is unknown. It would not know which schemas or methods are
available, so it could not choose a deterministic downgrade path.

## Decision

Keep `ee.daemon.request.v1` strict and add `ee.daemon.capabilities` as a v1
method. A client that might use a future schema or method must first send a
v1 capabilities request and inspect the response before attempting the newer
contract.

The capabilities response advertises:

- the daemon protocol name;
- supported request schema versions;
- supported response schema versions;
- supported method names;
- the strict v1 forward-compat policy for unknown fields and methods.

The v1 policy is:

- unknown request envelope fields are rejected;
- unknown methods return `daemon_unknown_method`;
- non-v1 request schemas return `daemon_request_schema_mismatch`;
- clients must downgrade to an advertised schema and method, or fall back to
  the in-process CLI path, when the needed daemon contract is absent.

Future daemon protocol versions must preserve the ability for v1 clients to
ask for capabilities. If a v2 daemon changes the main request envelope, it must
still accept `ee.daemon.capabilities` over `ee.daemon.request.v1`, or provide a
documented out-of-band replacement before the v1 capability path is removed.

## Rejected Alternatives

1. **Relax v1 to `additionalProperties: true`.** Rejected because the current
   runtime and schema would stop being a crisp contract. Unknown fields would
   look accepted while having no defined semantics.

2. **Replace the method enum with an open string pattern.** Rejected because it
   would make the schema accept methods the server cannot dispatch. The
   structured `daemon_unknown_method` response is a better runtime signal than
   a schema that pretends every `ee.daemon.*` method is valid.

3. **Put schema versions on every error response only.** Rejected because
   clients need discovery on the success path before sending a speculative v2
   request, not only after a failed request.

4. **Wait until v2 exists.** Rejected because v1 is still fresh. Adding the
   discovery method now prevents the first client population from baking in an
   unnegotiated protocol assumption.

## Verification Hooks

- Dispatch unit: `dispatch_capabilities_advertises_strict_v1_migration_contract`
  pins the capabilities result fields and deterministic method list.
- UDS integration:
  `daemon_capabilities_advertises_schema_and_method_contract_over_wire` checks
  the same payload through the framed socket path.
- Schema contract:
  `daemon_schema_cross_validation_accepts_capabilities_request` proves the
  published request schema accepts the discovery method.
- Schema contract:
  `daemon_schema_cross_validation_accepts_capabilities_response` proves the
  published response schema accepts the discovery payload.
- Schema contract:
  `daemon_request_schema_documents_strict_v1_capabilities_migration` keeps the
  v1 downgrade policy visible in the published request schema.

## Consequences

The daemon v1 envelope remains strict and schema-aligned. Clients have a
stable discovery call they can use before attempting future schema versions or
method names. Future daemon protocol changes now have an explicit migration
rule instead of relying on accidental serde behavior or open-ended JSON Schema
allowances.
