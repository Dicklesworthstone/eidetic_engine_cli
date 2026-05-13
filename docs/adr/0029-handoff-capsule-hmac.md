# ADR 0029: Handoff Capsule HMAC Keying

Status: proposed
Date: 2026-05-13

## Context

Handoff capsules are portable JSON artifacts that can be written to disk,
shared between agents, and resumed later. They carry prompt fragments,
selected memories, workspace identity, stale-snapshot metadata, and provenance.
Without integrity verification, a modified capsule can inject false procedural
rules, biased context, or fabricated provenance into the next agent's prompt.

M2 (`bd-17c65.13.3`) already embeds a `workspace_identity` block and implements
resume-side reconciliation. The current identity tuple is:

1. `fingerprint`: a 24-hex BLAKE3 prefix over the canonical workspace root path.
2. `canonical_root`: the canonicalized workspace root string.
3. `scope_kind`: `standalone`, `repository`, or `repository_subdir`.
4. `repository_fingerprint`: an optional `repo:<fingerprint>` for the containing
   repository.

That identity tuple is public because it is stored inside the capsule. It is
good associated data for binding a signature to a workspace, but it is not
secret key material. A design that derives an HMAC key only from public capsule
fields would detect accidental corruption but would not resist adversarial
tampering by anyone who can read the capsule.

## Decision

Handoff capsules will carry a top-level `integrity` block with HMAC-SHA256 over
the capsule body. The HMAC key is derived from a secret plus the public
workspace identity context.

### Signed Body

The signed body is the canonical UTF-8 JSON representation of the capsule value
excluding the top-level `integrity` field. Verification parses the capsule,
removes `integrity`, emits the remaining value with the same canonical JSON
serializer used by signing, and verifies the HMAC over those canonical bytes.

The canonical serializer for the signed body must:

1. Sort object keys lexicographically at every depth.
2. Preserve array order.
3. Emit UTF-8 without insignificant whitespace.
4. Emit numbers and strings through one stable serializer so equivalent parsed
   values produce equivalent signed bytes.

`bodySha256` is the SHA-256 digest of the canonical signed body bytes. Any
mutation to signed capsule data changes both the canonical body bytes and
`bodySha256`, and must fail verification. Whitespace-only changes outside
string values are not signed data because the capsule format is JSON, not a
byte-preservation archive.

### Integrity Block

The `integrity` block has this shape:

```json
{
  "schema": "ee.handoff.integrity.v1",
  "algorithm": "hmac-sha256",
  "keyMode": "workspace_secret",
  "bodySha256": "sha256:<64 lowercase hex>",
  "keyDerivationInputHash": "blake3:<16 lowercase hex>",
  "hmac": "base64url:<unpadded base64url>",
  "hmacPrefix": "<first 8 base64url chars>"
}
```

Only `hmacPrefix` may appear in logs. The full HMAC and the key material are
never logged.

### Key Modes

#### Default: `workspace_secret`

Default mode is portable within the same workspace data directory and across
machines only when the workspace secret is intentionally copied or shared.

The secret is a 32-byte random file at:

`<workspace-data-dir>/keys/handoff_hmac_key`

The key file is created on first use with POSIX mode `0600` where supported. It
is never embedded in a capsule, support bundle, audit log, tracing event, or
error message.

The HMAC key is:

```text
base_key := blake3::derive_key(
  "ee.handoff.capsule.hmac.workspace.v1",
  key_input(workspace_identity, capsule_schema_version, workspace_secret)
)
```

#### Strict: `workspace_secret_machine_bound`

Strict mode is selected by `ee handoff create --bind-to-machine`. It binds a
capsule to both the workspace secret and a machine-local secret.

The machine secret is a 32-byte random file at:

`~/.local/share/ee/keys/handoff_machine_salt`

The strict key is:

```text
strict_key := blake3::derive_key(
  "ee.handoff.capsule.hmac.strict.v1",
  key_input(workspace_identity, capsule_schema_version, workspace_secret)
  || len(machine_salt) || machine_salt
)
```

If strict mode is requested and the machine salt is missing, resume fails with
`strict_mode_no_salt_file` and a repair hint to initialize strict handoff keys
or recreate the capsule without machine binding.

### Key Input Serialization

Every variable-length field uses a big-endian `u32` byte length followed by the
raw bytes. Optional fields use a one-byte presence tag.

```text
key_input :=
  len(capsule_schema_version) || capsule_schema_version
  len(workspace_identity.fingerprint) || lowercase(workspace_identity.fingerprint)
  len(workspace_identity.canonical_root) || workspace_identity.canonical_root
  len(workspace_identity.scope_kind) || workspace_identity.scope_kind
  repo_present:u8
  if repo_present == 1:
    len(workspace_identity.repository_fingerprint)
    || lowercase(workspace_identity.repository_fingerprint)
  len(workspace_secret) || workspace_secret
```

The public identity fields bind the key to the workspace context. The workspace
secret supplies the authentication strength.

### Verification Behavior

`ee handoff resume` verifies HMAC before exposing prompt fragments, selected
memories, next actions, or provenance to callers.

Verification outcomes:

1. Valid HMAC: resume continues.
2. Missing HMAC: resume fails with `handoff_hmac_missing`.
3. Body SHA mismatch or HMAC mismatch: resume fails with
   `handoff_capsule_tampered` and exit code 6.
4. Strict machine salt mismatch or absence: resume fails with
   `handoff_capsule_machine_mismatch` or `strict_mode_no_salt_file`.
5. `--insecure-skip-hmac`: resume continues, emits
   `degraded.handoff_hmac_skipped` with severity `high`, and writes an audited
   `handoff.insecure_load` row containing the capsule path and SHA-256 of the
   loaded bytes.

There is no silent fallback from failed verification to normal resume.

### Key Rotation

`ee handoff rotate-key --capsule <path>` recomputes the integrity block over the
same signed body under the current key mode. The command preserves the signed
body and its `bodySha256`, updates `keyDerivationInputHash`, `hmac`, and
`hmacPrefix`, and writes an audit row.

V1 does not maintain a key ring. A rotated capsule verifies under the current
key and old detached copies verify only if the old secret is restored. Multi-key
grace periods can be added later with a new integrity schema.

### Migration

Capsules without `integrity` are legacy capsules. Inspect may display them with
a warning, but resume treats them as unsigned and fails closed unless the caller
passes `--insecure-skip-hmac`.

This is intentional. A legacy capsule may be benign, but an agent cannot
distinguish a benign legacy artifact from a tampered one without an integrity
record.

## Consequences

The handoff surface becomes fail-closed: tampered or unsigned capsules do not
feed prompt text to an agent by default. Agents still have an explicit escape
hatch for emergency recovery, and that escape hatch is visible in both
`degraded[]` and the audit log.

Default mode is less magically portable than a public identity-only KDF. That
is the security tradeoff: HMAC needs secret material. Teams that want portable
handoffs across machines must share the workspace data directory or copy the
workspace handoff key through their normal secret-management channel.

Strict mode adds a local secret and therefore intentionally breaks portability.
It is useful for capsules that should not be usable after exfiltration from a
specific machine.

Implementations must keep key material out of support bundles, logs, degraded
details, panic messages, and test artifacts. Only short non-secret prefixes and
hashes of derivation inputs may be logged.

## Rejected Alternatives

### Derive the HMAC key only from `workspace_identity`

This was attractive because it made default capsules portable anywhere the
workspace path and repository identity were known. It is rejected because the
identity tuple is embedded in the capsule and is therefore public. Anyone who
can edit the capsule could recompute the HMAC.

### Plain SHA-256 checksum

A checksum detects accidental corruption but not intentional modification. The
handoff threat is prompt injection through edited JSON, so authentication is
required.

### Sign only selected fields

Signing only prompt fragments or memory IDs would leave metadata, provenance,
workspace identity, and stale-snapshot fields mutable. Resume behavior depends
on those fields, so the signed body covers the complete capsule body excluding
only the integrity envelope.

### Accept unsigned legacy capsules with a warning

Warnings are easy for agents to ignore. Resume must fail closed unless the
caller explicitly chooses `--insecure-skip-hmac`, which makes the risk visible
in machine-readable output.

### Store the default HMAC secret in the capsule

Embedding the secret beside the HMAC defeats the authentication property. The
secret lives in workspace data storage and is never serialized into capsule
content.

### Maintain an automatic key ring in V1

Key rings add migration and revocation complexity. V1 rotates by rewriting the
capsule under the current key and auditing the action.

## Verification

M5 is complete only when these checks exist and pass:

1. `tests/adr_0029_docs.rs` asserts this ADR is indexed, documents the secret
   requirement, covers default and strict key modes, describes migration, and
   rejects public identity-only key derivation.
2. Default-mode tests prove a capsule round-trips with the same workspace
   secret and fails if the workspace secret changes.
3. Strict-mode tests prove a capsule created with one machine salt fails with
   `handoff_capsule_machine_mismatch` or `strict_mode_no_salt_file` under a
   different salt state.
4. Tamper tests mutate at least ten signed body byte positions and every
   mutation fails verification with `handoff_capsule_tampered`.
5. Insecure-skip tests prove resume continues only when
   `--insecure-skip-hmac` is present, emits `handoff_hmac_skipped`, and writes
   an audited `handoff.insecure_load` row.
6. Rotation tests prove `ee handoff rotate-key --capsule <path>` preserves the
   signed body hash and changes the HMAC fields.
7. POSIX tests assert workspace and machine key files are mode `0600` where the
   platform supports Unix permissions.
8. Log-redaction tests prove no full HMAC, workspace secret, or machine salt is
   emitted to tracing, test event logs, degraded details, or audit rows.
9. Failure-mode fixtures cover `handoff_capsule_tampered`,
   `handoff_capsule_machine_mismatch`, `handoff_hmac_missing`,
   `handoff_hmac_skipped`, and `strict_mode_no_salt_file`.
10. The E2E handoff HMAC script creates, tampers, resumes, rotates, and
    insecurely resumes capsules while retaining J1 logs.
11. J7 determinism checks sign the same already-created capsule body three
    times and assert the HMAC is byte-identical for the same key mode and
    secrets.
12. RCH-offloaded Cargo verification covers all Rust tests added for this
    decision, and shell checks cover the E2E script.
