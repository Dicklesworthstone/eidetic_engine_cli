# ADR 0021: Claim Evidence Kind Taxonomy

## Status

Accepted.

## Context

`ee claim list`, `ee claim show`, and `ee claim verify` need to prove product
claims from local, inspectable evidence instead of returning empty placeholder
reports. Claims live in `.ee/claims.yaml` so the workspace owns the statements
and the verification contract is explicit.

## Decision

`claims.yaml` records use `claim_id`, `statement`, `owner`, `ttl`, and one or
more `evidence` entries. The first executable evidence kinds are:

- `file-hash`: read a workspace-relative file and compare its BLAKE3 hash.
- `command-exit`: run a command directly without a shell and compare its exit
  code.
- `memory-presence`: require a target memory marker in the local EE memory
  store file.
- `rule-status`: require a target rule marker with the expected status in the
  local EE rule store file.

Verification is read-only. It may execute a declared command for
`command-exit`, but it does not rewrite `claims.yaml`, evidence files, memory
records, rule records, or artifact manifests.

## Consequences

- Claim output can report concrete pass/fail/expired evidence counts.
- Invalid YAML or invalid claim IDs return normal usage errors, not degraded
  success or unavailable placeholders.
- File evidence refuses absolute paths, parent traversal, and symlink
  components.
- New evidence kinds require tests before being accepted.

## Verification

- `tests/contracts/claims.rs` covers all four evidence kinds, tampered file
  evidence, expired TTL, malformed YAML, and CLI JSON execution.
- The effect manifest classifies `claim list`, `claim show`, and `claim verify`
  as read-only surfaces.
