# ADR 0047: publish_flip — Cargo.toml `publish = false` → `true` gate criteria

Status: Accepted — gate procedure documented. Actual flip awaits child-bead closure
(see § Preconditions). Tracker: bd-3usjw.12.

Context: bd-3usjw (Bridge Plan Part II) opened bd-3usjw.12 as the
`implements-surface:publish_flip` micro-bead. The flip itself is a one-line
edit (`Cargo.toml:15` `publish = false` → `publish = true`) plus `cargo
publish`, but the timing of that flip is the load-bearing decision: a
premature flip publishes a crate whose dependency tree still resolves
to path-deps, which crates.io rejects; a too-late flip leaves the README
install URL returning 404 indefinitely. The decision criteria, evidence
shape, and rollback path need an ADR so successive review rounds do not
re-debate them.

The ADR is paired with ADR 0046 (v0.1.0 tag recovery decision,
c0585cf6) and the per-dep publish micro-beads under bd-3usjw.11.1.\*
(workflow patches in `/Users/jemanuel/projects/sqlmodel_rust`,
`/Users/jemanuel/projects/franken_networkx`,
`/Users/jemanuel/projects/frankensearch`,
`/Users/jemanuel/projects/frankensqlite` already shipped; actual
`cargo publish` is the remaining external gate).

## Decision

The flip is a discrete, reviewable, reversible-by-yank operation gated
by a documented precondition checklist. The procedure is:

1. Verify every child precondition is closed (see § Preconditions).
2. Run `cargo publish --dry-run --allow-dirty` on a clean checkout
   pinned to the tag being flipped. The dry-run must complete without
   error AND without any `path = "..."` entries surviving in the
   packaged `Cargo.toml`.
3. Edit `Cargo.toml:15` `publish = false` → `publish = true`. Commit
   under message `chore(release): flip publish gate for v<version>
   (bd-3usjw.12)`.
4. Run `cargo publish` (no `--allow-dirty`). On success, the response
   includes the `Uploaded` line and the crate appears at
   `https://crates.io/crates/<name>` within ~60 seconds.
5. Verify by running `cargo install ee --version <version>` on a host
   that has not previously installed `ee` from source. The installed
   binary's `ee --version` output must match the published tag.
6. Update `scripts/audit_install_pipeline.sh` to expect the crates.io
   row at `available` instead of `not_published`.
7. Mark bd-3usjw.12 closed with the published version + crates.io URL
   in the close reason.

## Preconditions (all must be true before step 3)

| # | Precondition | Tracker | Evidence shape |
|---|---|---|---|
| 1 | Crate name available on crates.io | bd-3usjw.10 | `curl https://crates.io/api/v1/crates/<name>` returns 404 OR returns a crate this project owns |
| 2 | All path-deps published to crates.io with matching version | bd-3usjw.11 + .11.1.\* | `cargo tree -e features` produces no `(registry+...)` entries with `(path+...)` overrides; `franken_publish_status.py` reports all required-crates `version_available` |
| 3 | Vision-coverage gap percentage = 0 | scripts/vision-coverage.sh | `.vision-coverage-report.json` has `gap_percentage: 0` |
| 4 | Closure-linter green | scripts/closure-lint.sh | `.closure-lint-report.json` has `status: pass` and zero open `implements-surface` beads |
| 5 | `scripts/verify.sh` exit 0 | All nine gates | full output preserved as artifact |
| 6 | PUBLISH_CHECKLIST §3 boxes all checked | docs/publish-checklist.md | every checkbox `[x]` |

The flip MUST be refused if ANY precondition fails. A failed precondition
is structural drift; the right response is to reopen the parent micro-bead
and ship the missing evidence, not to bypass the check.

## Rollback

`cargo publish` produces an immutable artifact. If a published version
is found to be broken AFTER the flip:

1. Run `cargo yank --vers <version> <name>` to mark the version
   yanked. New installs default to the next-newest non-yanked version.
2. Edit `Cargo.toml:15` `publish = true` → `publish = false` to prevent
   accidental re-publish from CI while the fix lands.
3. Open a new bd-3usjw.12.<n> micro-bead documenting the yank reason
   and the follow-up version. The original bd-3usjw.12 close reason
   remains accurate (the flip happened) but the follow-up bead tracks
   the remediation.

Yank does NOT delete the artifact (crates.io retains it for
dependency-pin reproducibility); it only marks it as "do not use for
new dependency resolution." A truly broken crate must ship a fixed
version under the same major; the yank is the agent-facing signal.

## Risk envelope (what the flip authorizes)

- A successful flip makes `cargo install ee` work for every downstream
  consumer. The blast radius is "every install of `ee` from this point
  forward."
- A failed flip (precondition violation, dry-run error, network
  flake) is locally-reversible: `git revert` the commit, no external
  state changed.
- A successful flip followed by a broken release is handled by yank
  (above) and a fast follow-up patch release. Yank does NOT recall
  installed binaries; downstreams must `cargo update`.

## Non-goals

- The flip does NOT decide release cadence. That belongs to a separate
  release ADR.
- The flip does NOT decide which crate names ship (umbrella vs
  per-component); that is bd-3usjw.10 / crate_name_resolution.
- The flip does NOT decide signing posture. ADR 0046 records the
  v0.1.0 tag recovery decision; signing belongs to bd-3usjw.9
  (first_signed_release).
- The flip does NOT modify the path-dep publishing chain. The
  per-dep micro-beads under bd-3usjw.11.1.\* own that work.

## Why not flip now

As of this ADR's commit, at least one precondition (specifically
precondition 2: per-dep crates.io availability) is unmet. The upstream
workflow patches that prepare those publishes already shipped (see
bd-3usjw.11.1.32 close reason for the sqlmodel-frankensqlite sequence,
and the parallel patches in /data/projects/franken_networkx and
/data/projects/frankensearch). The remaining work is the actual
`cargo publish` invocation on a host with crates.io API token
access — that is external to this repository's commit history and
cannot be staged from within the ee_cli source tree.

When precondition 2 flips green (all required crates report
`version_available` in `franken_publish_status.py`), this ADR's
procedure becomes the runbook. Until then, the procedure is
documented but unexecuted, which is honest.

## Verification

This ADR is documentation-only; no Cargo, no schema change, no source
mutation. Static checks:

- `git diff --check -- docs/adr/0047-publish-flip-gate.md`: passes.
- Cross-link sanity: ADR 0046 (v0.1.0 tag recovery) and bd-3usjw.11.\*
  (per-dep publishes) are the linked tracker dependencies; both are
  reachable from this file.
- No code path references this ADR programmatically; readers find it
  through `docs/adr/README.md` index.

Refs: bd-3usjw (parent), bd-3usjw.10 (crate name resolution),
bd-3usjw.11 + .11.1.\* (per-dep publishes), bd-3usjw.9
(first_signed_release), bd-3usjw.72 + ADR 0046 (v0.1.0 tag recovery),
bd-17c65.10.17.1.2 (RCH topology remediation).
