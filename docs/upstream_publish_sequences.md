# Upstream Publish Sequences

This file is the agent-facing decision record for how franken-stack
dependency crates are published from their upstream repositories before
eidetic_engine_cli can `cargo publish`. It documents the **decisions**
(crate ordering, idempotent-rerun shape, version pinning) so future
agents do not re-derive them, and lists the **verification commands**
that close the corresponding release-readiness beads.

The doc is paired with `scripts/franken_publish_status.py` (see
`docs/franken_publish_status.md`), which is the read-only crates.io
status probe agents use to check whether a publish sequence has
actually run.

## Why this exists

Multiple agents have repeatedly refreshed crates.io status for
`sqlmodel-frankensqlite`, `fnx-runtime`, and the rest of the
franken_dep_publishing chain (see bd-3usjw.11.1.32 comment trail). Each
refresh ends with the same finding: the upstream workflow patch already
landed, but crates.io still shows the older version because no operator
has triggered the upstream release. Without a recorded decision, every
fresh agent re-reads the same evidence and refiles the same bead state.

This doc replaces those repeated audits with one place future agents
can read to see (a) which upstream patches landed, (b) what an
operator triggering the upstream release would need to do, and (c) how
to verify the publish actually happened.

## Decisions

### Publish order is dependency-topological, with idempotent rerun

For each upstream workspace (sqlmodel, frankensqlite, frankensearch,
franken_networkx), the release workflow publish array MUST list the
crates in dependency-topological order — a crate may only appear after
every crate it depends on. The eidetic_engine_cli umbrella consumer is
NOT part of any upstream workflow; it remains `publish = false` in
this repo's `Cargo.toml` until bd-3usjw.11 closes for every
franken-stack dep.

Idempotent rerun: every upstream release workflow MUST treat
`already uploaded` / `already exists` / `is already uploaded` output
from `cargo publish` as success and continue to the next crate in
the array. Without this, rerunning a release after a partial-publish
failure (rate limit, network blip, missing intermediate crate) aborts
before the new crates get published. The canonical pattern lives in
`sqlmodel_rust/.github/workflows/release.yml` line ~91:

```yaml
- name: Publish ${{ matrix.crate }}
  run: |
    if ! out=$(cargo publish -p "${{ matrix.crate }}" --token "$CRATES_TOKEN" 2>&1); then
      if echo "${out}" | grep -qiE 'already (uploaded|exists)|is already uploaded'; then
        echo "already published, continuing"
        exit 0
      fi
      echo "${out}"
      exit 1
    fi
```

### Version pinning is workspace-uniform per upstream

A franken-stack workspace MUST publish every constituent crate at the
same version. The eidetic_engine_cli dependency-resolution audit
(`scripts/audit_install_pipeline.sh`) reports a `wrong_version` status
when a workspace's published crates drift in version. Mixing
`sqlmodel-core 0.2.2` with `sqlmodel-frankensqlite 0.2.1` is not
allowed; either both are 0.2.2 or both are 0.2.1.

### `sqlmodel-frankensqlite` is part of the sqlmodel workspace publish

Decided in bd-3usjw.11.1.32 (RubyWolf patch landed 2026-05-15). The
sqlmodel-frankensqlite crate lives in the sqlmodel_rust workspace
under `crates/sqlmodel-frankensqlite/Cargo.toml`. Its release order
is fixed: it MUST publish after `sqlmodel-sqlite` (which it depends
on for the SQLite backend trait) and BEFORE the umbrella `sqlmodel`
crate (which optionally re-exports it).

The canonical sqlmodel release-workflow publish array therefore
reads (line 80 of `sqlmodel_rust/.github/workflows/release.yml`):

```bash
crates=(
  sqlmodel-core
  sqlmodel-macros
  sqlmodel-query
  sqlmodel-schema
  sqlmodel-session
  sqlmodel-pool
  sqlmodel-console
  sqlmodel-postgres
  sqlmodel-mysql
  sqlmodel-sqlite
  sqlmodel-frankensqlite
  sqlmodel
)
```

Future agents adding a new sqlmodel-* crate MUST insert it into this
array in dependency-topological order; an alphabetical or
arbitrary-order insertion will cause `cargo publish` to fail with a
"crate depends on unpublished version" error.

## Per-dependency status

The table below tracks the publish-readiness of each franken-stack
crate that eidetic_engine_cli depends on. "Required" is the version
this repo's `Cargo.lock` pins; "published" is the latest non-yanked
version on crates.io. A crate is "publish-ready" when those columns
match.

| Crate | Required | Published | Status |
| --- | --- | --- | --- |
| sqlmodel-core | 0.2.2 | 0.2.2 | ready |
| sqlmodel-macros | 0.2.2 | 0.2.2 | ready |
| sqlmodel-query | 0.2.2 | 0.2.2 | ready |
| sqlmodel-schema | 0.2.2 | 0.2.2 | ready |
| sqlmodel-session | 0.2.2 | 0.2.2 | ready |
| sqlmodel-pool | 0.2.2 | 0.2.2 | ready |
| sqlmodel-sqlite | 0.2.2 | 0.2.2 | ready |
| sqlmodel-frankensqlite | 0.2.2 | 0.2.1 | **wrong_version** (bd-3usjw.11.1.32) |
| sqlmodel | 0.2.2 | 0.2.2 | ready |
| frankensearch | 0.2.x | varies | tracked by `--group frankensearch` |
| fnx-runtime | 0.1.0 | varies | tracked by `--group fnx` |
| fnx-classes | 0.1.0 | varies | tracked by `--group fnx` |
| fnx-cgse | 0.1.0 | varies | tracked by `--group fnx` |
| fnx-convert | 0.1.0 | varies | tracked by `--group fnx` |
| fnx-algorithms | 0.1.0 | varies | tracked by `--group fnx` |

The table is informational and may drift; agents MUST run
`scripts/franken_publish_status.py --all-groups --markdown` to get the
live state before opening or closing a publish-sequence bead. The
`docs/franken_publish_status.md` companion doc covers the live tool.

## Verification commands

The acceptance contract for every `bd-3usjw.11.1.*` bead is the
same: "crates.io API shows <crate> <version> non-yanked AND
eidetic_engine_cli dependency_resolution reports status=version_available".

Read-only verification (no cargo, no publish, no upstream mutation):

```bash
scripts/franken_publish_status.py --all-groups --markdown
scripts/audit_install_pipeline.sh
```

The audit script emits a `dependency_resolution_ready` boolean and
a per-crate `crate_exists_version_missing` vs `version_available`
status. A bead closes when both:

- `franken_publish_status.py` shows the crate's row as `available`
  (required == published, not yanked), AND
- `audit_install_pipeline.sh` reports `version_available` for that
  crate.

## What this doc explicitly DOES NOT do

- Trigger upstream releases. Publishing requires a maintainer with
  crates.io credentials to run the upstream release workflow; agents
  cannot do this even with explicit operator approval, because the
  CRATES_TOKEN secret is not available to agent harnesses.
- Yank, supersede, or rewrite already-published versions. Yanking is
  irreversible at the registry level; the existing
  `sqlmodel-frankensqlite 0.2.1` stays published forever even after
  0.2.2 ships.
- Move or recreate git tags in the upstream repository. Each upstream
  workspace owns its own tag policy; this repo's
  `docs/adr/0046-v0.1.0-tag-recovery-strategy.md` ADR records the
  parallel decision for `eidetic_engine_cli`'s own v0.1.0 tag, not
  any upstream tag.
