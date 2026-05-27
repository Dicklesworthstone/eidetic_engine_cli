# ADR 0046: v0.1.0 Tag Recovery — Leave Tag in Place, Cut First Signed Release Forward

Status: accepted
Date: 2026-05-27

## Context

`bd-3usjw.72` was filed on 2026-05-19 to block `bd-3usjw.9`
(implements-surface:first_signed_release) until the release recovery path was
explicitly chosen and documented. The original concern:

- The local and origin `v0.1.0` tag both exist.
- The GitHub Release page for `v0.1.0` was absent (no assets, no release notes).
- The tagged tree at `v0.1.0` failed `actions/checkout` because
  `tests/audit_artifacts/latest_install_pipeline.json` was a broken symlink at
  that commit, while `HEAD` has since repaired it.
- Re-running the v0.1.0 release workflow would therefore fail on every
  attempt without operator intervention.

The bead expressly forbade any tag/release/asset mutation (no tag
deletion, no tag movement, no tag recreation, no GitHub Release
creation, no asset upload, no workflow rerun, no branch/reset/stash/
checkout/rebase/worktree, no local Cargo fallback, no file deletion)
until a recovery path was approved.

### Current state (2026-05-27, after the v0.2.0/v0.3.0/v0.3.1 release wave)

`git tag` shows four release tags, all on origin:

| Tag | Date | GitHub Release |
|-----|------|----------------|
| `v0.1.0` | 2026-05-15 | **absent** (orphaned tag; broken-symlink tagged tree) |
| `v0.2.0` | 2026-05-21 | present (2026-05-21T20:12:21Z) |
| `v0.3.0` | 2026-05-25 | present (2026-05-25T17:02:26Z) |
| `v0.3.1` | 2026-05-26 | present (2026-05-27T00:22:15Z) — "Latest" |

The original blocker ("no GitHub Releases exist anywhere") is no longer
true: the v0.2.0, v0.3.0, and v0.3.1 release pages all shipped during
the convergence wave (commit dates and `gh release list` confirm). Only
the v0.1.0 tag remains orphaned — the historical-broken-tree problem is
unchanged, but it is no longer load-bearing for the README install URL
or the first-signed-release contract.

`Cargo.toml` is currently pinned at `version = "0.3.1"`. The next
release will be `v0.4.0` (minor bump under the
`README.md::release-process` table — accumulated features and surfaces
since v0.3.1: focus suggest Phase 2, reflect ingest contract, the
read-cap hardening series, CI signature flexibility) or `v0.3.2` (patch
bump if only hardening lands before tag time). The decision between
those two is owned by whoever cuts the next tag; this ADR does NOT pre-
decide that.

## Decision

**Leave `v0.1.0` as a historical-only git tag. Do not move it, recreate
it, or attach a GitHub Release page. Treat `v0.3.1` as the de facto
first signed release; future signed releases proceed forward from
`HEAD`.**

Concretely:

1. The `v0.1.0` git tag stays on local + origin as-is. It documents that
   a v0.1.0 was tagged on 2026-05-15 and that the tagged tree had a
   broken symlink in `tests/audit_artifacts/latest_install_pipeline.json`
   which prevented the release workflow from completing. The orphaned
   state IS the historical record.
2. No GitHub Release page is created for `v0.1.0`. The audit trail
   already records that no release artifacts were produced at that tag;
   inventing one after the fact would be revisionist.
3. The README install URL stays pointed at `latest`
   (`releases/latest/download/install.sh`), which currently resolves to
   `v0.3.1` and will auto-advance to the next tagged release.
4. Future signed releases proceed forward from `HEAD` under the existing
   `README.md::release-process` flow (version bump in `Cargo.toml` →
   push tag → release workflow ships assets + sigstore bundle). The
   first such release after this ADR is the de facto "first signed
   release" the parent epic asked for; it does not need a special tag
   name.
5. `bd-3usjw.9` (`implements-surface:first_signed_release`) is
   unblocked by this ADR. Its acceptance ("README install URL returns
   200") is already satisfied by the v0.3.1 release page that shipped
   on 2026-05-27.

## Consequences

**Easier:**

- No risky tag-movement or release-recreation operations. The bead's
  forbidden-actions list (tag deletion/movement/recreation, GitHub
  Release creation, asset upload, workflow rerun, branch reset, etc.)
  stays intact without ever being approved or exercised.
- The audit trail stays honest: v0.1.0 is documented in this ADR + the
  CHANGELOG.md `[Unreleased]`/`[0.3.0]` lineage as "tagged 2026-05-15
  but no GitHub Release page produced" — anyone investigating later
  sees the truth, not a backfilled fiction.
- Future release cuts (`v0.3.2`, `v0.4.0`, etc.) need no special
  reasoning about the orphaned tag — the workflow advances from `HEAD`.

**Harder:**

- Anyone walking `gh release list` from oldest first will see v0.2.0 as
  the earliest GitHub Release page, not v0.1.0. The CHANGELOG.md
  reconstruction note (lines 5–17, "This changelog was reconstructed
  from the repository, not from memory") covers this gap; this ADR
  cross-links to it so the divergence is traceable.

**Intentionally impossible:**

- No backfilled v0.1.0 release page. If a future operator decides they
  want one (e.g. for marketing or for a from-scratch install verification
  fixture), that requires a new ADR explicitly superseding this one and
  the original bd-3usjw.72 guardrails — it cannot happen by drift.

## Rejected Alternatives

### Alt A — Move the v0.1.0 tag to current `HEAD`

- Rejected because the bead's hard constraint forbade tag movement
  without explicit operator approval naming the exact commands and
  consequences.
- Tag movement would also rewrite the historical record: the v0.1.0
  commit/tree at the moved tag would be a 2026-05-27 HEAD that bears no
  relationship to the 2026-05-15 release-readiness state the original
  tag documented.
- The original v0.1.0 tagger (`Dicklesworthstone` per `git show
  v0.1.0`) would be replaced by whoever ran the move.

### Alt B — Recreate v0.1.0 with the broken-symlink fix and re-release

- Rejected for the same forbidden-action reason (tag deletion +
  recreation).
- Even with operator approval, this would produce a tag whose tree
  doesn't match what was actually tagged in May 2026, and a GitHub
  Release whose `Latest` flag would have to be manually managed to avoid
  displacing v0.3.1.

### Alt C — Skip v0.1.0 and v0.2.0; tag a fresh v1.0.0 as the first signed release

- Rejected because Cargo.toml is at v0.3.1, the CHANGELOG records v0.2.0
  and v0.3.0/v0.3.1 lines, and there are already GitHub Release pages
  for v0.2.0/v0.3.0/v0.3.1. Jumping to v1.0.0 would lose those audit
  rows and pre-decide a 1.0-readiness claim that this codebase has not
  earned (per the AGENTS.md note "we are still pre-1.0").

### Alt D — Cut a fresh v0.1.1 patch tag at HEAD and treat that as the recovered v0.1.x line

- Rejected because v0.2.0 / v0.3.0 / v0.3.1 already shipped past the
  0.1.x line. Reverting to a 0.1.x lineage now would imply a regression
  in feature coverage that did not happen.

## Verification

- `git tag` continues to show `v0.1.0` through the latest signed
  release; no tag is deleted or moved by this ADR.
- `gh release list` continues to show the v0.2.0+ release pages with
  v0.1.0 absent; no release page is created or backfilled by this ADR.
- `Cargo.toml::version` continues to be the authoritative next-release
  pin; the next release proceeds under the existing
  `README.md::release-process` flow.
- `README.md` install URL continues to point at `latest`; no manual
  v0.1.0 redirection is added.
- `bd-3usjw.9` (implements-surface:first_signed_release) is
  closeable on the strength of the existing `gh release view v0.3.1`
  evidence (binary + sigstore assets present, install.sh / install.ps1
  attached, README install URL returns 200 via the GitHub `latest`
  redirect). This ADR is the recovery-path documentation that
  `bd-3usjw.72` blocked the parent on; closing `bd-3usjw.72` is the
  immediate action.

## References

- `bd-3usjw.72` — release: decide v0.1.0 tag recovery path before first
  signed release (this ADR's source bead).
- `bd-3usjw.9` — implements-surface:first_signed_release (the bead this
  ADR unblocks).
- `README.md::release-process` — canonical version-bump and tag-push
  flow.
- `CHANGELOG.md` lines 5-17 — historical-reconstruction caveat that
  pre-dates this ADR.
- `gh release list` — authoritative source of which GitHub Release pages
  exist (consulted 2026-05-27).
