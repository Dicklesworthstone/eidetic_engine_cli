# ADR 0047: `Cargo.toml` `publish = true` Gate

Status: accepted
Date: 2026-05-27

## Context

`Cargo.toml` line 15 currently carries `publish = false`. This is deliberate;
without it, a casual `cargo publish` from any agent or CI worker would
attempt to push `ee` to crates.io before the prerequisites described below
are in place. The flag is the final guard on the path from "local-only
crate" to "`cargo install eidetic-engine` works on a clean host."

bd-3usjw.12 (`implements-surface:publish_flip`) asks for the flip itself —
`publish = false` → `publish = true` — and for the verification that
follows: `cargo publish --dry-run` must succeed, the real `cargo publish`
must accept the upload, https://crates.io/crates/eidetic-engine must show
`0.1.0` (or later) within the timeout the publish endpoint advertises, and
`cargo install eidetic-engine --version 0.1.0` must work on a clean host.

This ADR exists because the flip itself is a one-line change but
authorising it requires evidence from several other tracking lanes that
the agent loop has no way to verify on its own. Recording the gate as an
ADR pins the agent-visible decision so any future pane that picks up
bd-3usjw.12 can answer "what must be true before I flip this bit?"
without re-deriving the rule from scratch.

Related history:

- ADR [0046](0046-v0.1.0-tag-recovery-path.md) /
  [0046-v0.1.0-tag-recovery-strategy.md](0046-v0.1.0-tag-recovery-strategy.md)
  record the parallel decision about how to recover the `v0.1.0` git tag
  before the first signed release ships. Publish-flip and tag-recovery
  are independent — `cargo install eidetic-engine` does not require a
  GitHub release — but they ship through the same release wave in
  practice.
- bd-3usjw.10 (`crate_name_resolution`) decided the crate would publish
  under the name `eidetic-engine`; the binary remains `ee`. Reasoning
  recorded in that bead's history: `crates.io/crates/ee` resolves to
  `https://github.com/ewpratten/ee`, not this project, so the short name
  is unavailable.
- bd-3usjw.11 (`franken_dep_publishing`) and its bd-3usjw.11.1.*
  per-dep children own the upstream path-dep → crates.io publishing
  work for the franken-stack crates (asupersync, frankensearch,
  fsqlite, sqlmodel, fnx-*, etc.). Until every transitive path-dep
  resolves to a crates.io version, `cargo publish --dry-run` for `ee`
  will fail on dependency resolution.
- `CHANGELOG.md` already carries `[Unreleased]`, `[0.3.0]`, and `[0.3.1]`
  sections; the publish wave will cut whichever version is current at
  flip time, not necessarily `0.1.0`.

## Decision

`publish = false` stays in place until ALL of the following are
demonstrably true, in this order:

1. **Name resolution closed.** bd-3usjw.10 closed with a recorded
   `cargo search eidetic-engine` showing the name is available, or the
   chosen name is updated here. Package metadata MUST declare the
   intended `name`, `version`, `description`, `license`, `repository`,
   `readme`, and `keywords` so the rendered crates.io page is not blank.
2. **Path-dep publishing closed.** bd-3usjw.11 closed AND every
   bd-3usjw.11.1.* per-dep child closed. The proof is one of:
   - `cargo tree -e features --no-default-features` shows no `path = ...`
     entries for franken-stack crates, OR
   - the path-deps that remain are explicitly behind an opt-in feature
     gate that is `default-features = false` in published metadata, AND
     the corresponding feature is documented as "developer-only — not
     enabled in published crate."
3. **`cargo publish --dry-run --allow-dirty` succeeds.** Run via RCH
   (`RCH_REQUIRE_REMOTE=1`) so the dry-run runs on a worker with the
   same toolchain matrix the real publish would use. Local Cargo
   fallback is not acceptable.
4. **Readiness gates green.** `./scripts/verify.sh` returns exit 0 with
   every gate green (forbidden-deps, closure-lint, vision-coverage,
   tests, e2e, boundary-migration, doctor safety). Vision-coverage
   gap percentage MUST be 0 before flip; a non-zero gap means the
   public-facing claim set on crates.io would not match the shipped
   binary.
5. **Operator authorization.** Per
   [AGENTS.md "Irreversible Git & Filesystem Actions — DO NOT EVER
   BREAK GLASS"](../../AGENTS.md), `cargo publish` is irreversible.
   crates.io does not delete versions; it only yanks them (which still
   leaves the slot occupied). The actual publish MUST be initiated by a
   human after the four preconditions above are recorded as
   green, with the exact `cargo publish` command repeated back to the
   operator before execution.

When all five conditions are recorded as green and a human has
authorised the flip in a session message that names the exact command
and the version being shipped, the agent MAY:

- Change `Cargo.toml` line 15 from `publish = false` to `publish = true`.
- Run `cargo publish --dry-run --allow-dirty` one more time as the
  immediate-pre-flip evidence.
- Wait for explicit human approval to run the non-dry `cargo publish`.

If the human authorises the non-dry publish, the agent MAY run it once.
A second `cargo publish` for the same version will be rejected by
crates.io regardless, so the agent MUST NOT retry on failure without
human re-authorisation.

## Consequences

- The path from a green `./scripts/verify.sh` to "`cargo install
  eidetic-engine` works on a clean host" is documented end-to-end in
  one place.
- Future agents picking up bd-3usjw.12 can immediately tell whether
  the gate is open. If any of the five conditions is unrecorded, the
  agent SHOULD comment the gap on bd-3usjw.12 (and the matching parent
  bead) rather than flipping the bit.
- The README's "Cargo" install section becomes a working install path
  the moment the flip lands AND a valid version is on crates.io.
- The `scripts/audit_install_pipeline.sh` crates.io row flips from
  "not_published" to "available" automatically once the version is
  visible; no separate audit edit is required.
- `cargo install eidetic-engine` becomes a supported install path
  alongside the existing `curl | bash` GitHub-release installer
  (planned under bd-3usjw.9) and the planned Homebrew tap
  (bd-3usjw.13). The three paths are independent — each ships when
  its lane closes.

## Rejected Alternatives

- **Flip `publish = true` now and let `cargo publish` fail on the
  path-deps.** Rejected. A failed publish leaves no public artifact but
  burns the version number locally and pollutes the verification ledger
  with a known-bad attempt that future agents would have to interpret.
  Better to keep the gate honest.
- **Ship `ee` to crates.io with vendored franken-stack source in the
  publishable artifact.** Rejected. The franken-stack crates are
  separately useful, and vendoring would force every `ee` patch to
  redeliver the entire vendored tree. The deliberate path is upstream
  publishing of each franken crate, then `ee` consumes crates.io
  versions like any other downstream.
- **Hide `publish = true` behind a `--manifest-path` override or a
  separate `Cargo.publish.toml`.** Rejected. Two manifests drift; one
  manifest gated by an ADR doesn't. The agent loop already knows how
  to honour single-source-of-truth gates.
- **Flip `publish = true` before the audited installer
  (`install.sh` + Sigstore bundle) ships.** Rejected on safety
  grounds. Publishing to crates.io exposes the binary to a much
  broader audience than the README install URL does today. The
  release wave should sequence: dependency-publishing → readiness gates
  → installer hardening → crates.io publish, so a regression in any
  earlier stage blocks the broadest-reach distribution channel.
