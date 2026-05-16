# Franken Publish Status

`scripts/franken_publish_status.py` is a read-only release-readiness probe for
franken-stack dependency publishing. It replaces manual crates.io refreshes for
beads such as `bd-3usjw.11.1.33` with deterministic JSON and a Beads-ready
Markdown summary.

Default fnx check:

```bash
scripts/franken_publish_status.py --group fnx
```

Markdown for a tracker comment:

```bash
scripts/franken_publish_status.py --group fnx --markdown
```

Fixture mode for CI-safe parser checks:

```bash
scripts/franken_publish_status.py \
  --group fnx \
  --fixtures-dir tests/fixtures/franken_publish_status/api_missing \
  --root-override tests/fixtures/franken_publish_status/fnx_repo \
  --generated-at 2026-05-16T00:00:00Z \
  --no-git-status
```

The script never runs Cargo, never attempts `cargo publish`, never reads publish
credentials, and never mutates sibling repositories. Live mode uses the official
crates.io API endpoint `https://crates.io/api/v1/crates/<crate>`. Static sibling
checks are limited to manifest parsing, release workflow parsing, dependency
publish order, tag gating, token-check presence, and a redaction-safe dirty
worktree count.

Crates.io statuses:

- `available`: required version exists and is not yanked.
- `missing`: official API returned HTTP 404.
- `wrong_version`: crate exists, but the required version is absent or yanked.
- `network_unavailable`: API request failed, timed out, or returned malformed
  data.

The initial target is `fnx`, but the schema and CLI also cover `sqlmodel`,
`frankensearch`, and `fsqlite` groups for later release-readiness beads.

## fnx-generators decision

`eidetic_engine_cli` uses `fnx-generators = "0.1.0"` as a dev-dependency for
all-targets and release-readiness checks. The release decision for
`bd-3usjw.11.1.35` is to publish `fnx-generators` with the rest of
`franken_networkx`, not to hide or remove the ee dev-dependency.

The publish-status checker therefore treats `fnx-generators` as a required fnx
crate. It must appear in the fnx crate list, have local publish metadata, resolve
from crates.io at the required version, and appear in the release workflow after
the core/runtime crates. If the workflow omits it, the report emits
`workflow_missing_publish_crate`; if crates.io returns 404, the report emits
`crates_io_missing`. This keeps `bd-3usjw.11.1.33` from closing while the
generator crate needed by ee dev verification is still unpublished.
