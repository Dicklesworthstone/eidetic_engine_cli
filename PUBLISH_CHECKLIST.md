# Publish Checklist

Pre-release checklist for publishing `ee` to crates.io.

## Prerequisites

Before publishing, ensure:

1. **Wave 4 gates cleared**
   - [ ] All process-fix beads closed
   - [ ] First wave of `implements-surface:*` beads closed
   - [ ] Vision coverage gap at 0%

2. **Dependencies publishable**
   - [ ] `frankensearch` published to crates.io (or feature-gated)
   - [ ] `sqlmodel-*` crates published to crates.io (or feature-gated)
   - [ ] `fnx-*` crates published to crates.io (or feature-gated behind `graph`)
   - [ ] `franken-agent-detection` published to crates.io (or feature-gated)
   - [ ] `toon` (`tru`) published to crates.io

3. **Crate name ownership resolved**
   - [ ] `cargo owner --list ee` works with a logged-in crates.io token
   - [ ] `ee` is owned by the project account, or a replacement crate name is chosen
   - [ ] `scripts/audit_install_pipeline.sh` reports `crates_io.repository` as `https://github.com/Dicklesworthstone/eidetic_engine_cli`

   As of the 2026-05-13 audit, crates.io has `ee` at `0.0.0` owned by `ewpratten`
   and pointing at `https://github.com/ewpratten/ee`; the project cannot publish
   `ee` until ownership is transferred or the crate name changes.

4. **Metadata complete**
   - [x] `description` set
   - [x] `license` set (MIT)
   - [x] `repository` set
   - [x] `homepage` set
   - [x] `documentation` set
   - [x] `readme` set
   - [x] `keywords` set
   - [x] `categories` set
   - [x] `exclude` set (no test fixtures, profiling data)

## Manual Steps

1. **Remove publish block**
   ```toml
   # Change from:
   publish = false
   # To:
   publish = true
   ```

2. **Verify dry-run**
   ```bash
   cargo publish --dry-run --allow-dirty
   ```

3. **Run full verification**
   ```bash
   ./scripts/verify.sh
   ```

4. **Publish**
   ```bash
   cargo publish
   ```

## Automated Gates

The following are enforced by CI:

- `./scripts/check-forbidden-deps.sh` - No tokio, rusqlite, petgraph, etc.
- `./scripts/closure-lint.sh` - No abstention-as-implementation closures
- `./scripts/vision-coverage.sh` - Blocks release if gap > 0
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --workspace --all-targets`

## Post-Publish

1. Verify on crates.io: https://crates.io/crates/ee
2. Test install: `cargo install ee`
3. Run `ee --version` and `ee doctor`
