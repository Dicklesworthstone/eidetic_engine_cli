# Release Signing Policy

This document is the public ceremony record for Eidetic Engine release
signing. The preferred path is Sigstore keyless signing from
`.github/workflows/release.yml` with the GitHub Actions OIDC issuer:

```bash
cosign verify-blob \
  --bundle ee-<target>.tar.xz.sigstore.json \
  --insecure-ignore-tlog=false \
  --certificate-identity-regexp "^https://github\.com/Dicklesworthstone/eidetic_engine_cli/\.github/workflows/release\.yml@refs/tags/v[0-9].*$" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  ee-<target>.tar.xz
```

The Unix installer tries keyless identities before the pinned release key.
Operators who only accept workflow-built releases can set:

```bash
EE_INSTALL_REQUIRE_KEYLESS=1
```

That mode refuses missing Sigstore bundles, missing `cosign`, and pinned-key
fallback signatures.

## Manual-Cut Fallback

The pinned release key exists only for emergency manual releases when the
GitHub Actions release workflow is unavailable. It is a fallback trust root,
not the normal release channel.

Manual signatures must upload to the transparency log:

```bash
COSIGN_PASSWORD="" cosign sign-blob --yes \
  --tlog-upload=true \
  --key ~/.config/ee-signing/cosign-ee.key \
  --bundle ee-<target>.tar.xz.sigstore.json \
  ee-<target>.tar.xz
```

The installer verifies fallback signatures with `--insecure-ignore-tlog=false`
so offline bundles are rejected by policy.

## Key Generation

Generate a replacement pinned key only during an announced rotation:

```bash
COSIGN_PASSWORD="" cosign generate-key-pair --output-key-prefix=cosign-ee
install -m 0600 cosign-ee.key ~/.config/ee-signing/cosign-ee.key
install -m 0644 cosign-ee.pub signing/cosign.pub
```

Storage requirements:

- Private key material must live on a maintainer-controlled machine or hardware
  token. Hardware-backed storage is preferred.
- The private key file must be mode `0600`.
- The public key must be committed to `signing/cosign.pub` and embedded in
  `install.sh` in the same pull request.
- The rotation pull request must state who generated the key, where the public
  key is committed, and why rotation is needed.

## Rotation

Pinned-key rotation requires a normal reviewed pull request. A rotation is not
complete until all of these are true:

- `install.sh` embeds the new public key.
- `signing/cosign.pub` contains the same public key.
- `CHANGELOG.md` links to this policy and describes the release-channel impact.
- At least two maintainers approve the rotation record.
- A test release or dry-run artifact verifies with both keyless policy and the
  new pinned fallback policy.

Do not rotate the pinned key inside a release-only commit without review. A
release that needs emergency signing should use the current pinned key and then
rotate separately.

## Revocation

If the pinned private key is suspected compromised:

1. Stop publishing manual-cut artifacts.
2. Prefer keyless workflow releases only.
3. Ship an `install.sh` update that disables the compromised key or replaces it
   with a newly reviewed key.
4. Add a tombstone note beside `signing/cosign.pub` in the same pull request.
5. Publish a security note that tells operators to use
   `EE_INSTALL_REQUIRE_KEYLESS=1` until the rotation is complete.

Revocation never affects verification of keyless GitHub Actions releases whose
identity and issuer match the release workflow policy.
