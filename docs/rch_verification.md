# RCH Verification Wrapper

Use `scripts/rch_verify.sh` for focused Rust verification in this repository.
It always builds an explicit `rch exec -- env TMPDIR=/tmp ...` invocation and
emits a JSON proof that can be copied into a Beads comment.

Examples:

```bash
scripts/rch_verify.sh --dry-run -- cargo test --lib output::streaming -- --nocapture
scripts/rch_verify.sh -- cargo clippy --all-targets -- -D warnings
scripts/rch_verify.sh -- cargo fmt --check
```

Accepted verifier shapes are `cargo check`, `cargo test`, `cargo bench`,
`cargo clippy`, and `cargo fmt --check`. `cargo fmt --check` is accepted for
contract consistency, but current RCH may classify it as non-compilation; its
proof therefore reports `would_offload=false`. Other commands are refused unless
`--allow-raw` is passed. Raw commands still run through explicit `rch exec`, but
the JSON marks `would_offload=false` because RCH may decline non-compilation
commands.

The wrapper sets these remote-safe defaults:

- `RCH_REQUIRE_REMOTE=1`
- `RCH_QUEUE_WHEN_BUSY=1`
- `RCH_COMPRESSION=0`
- RCH binary `/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch` when present, falling back to `rch`
- `RCH_CANONICAL_PROJECT_ROOT=/Users/jemanuel/projects`
- `RCH_ALIAS_PROJECT_ROOT=/data/projects`
- remote command `TMPDIR=/tmp`
- remote command `CARGO_TARGET_DIR=/tmp/ee-rch-verify-target`

The JSON proof schema is `ee.rch.verify.v1` and includes the command kind,
remote-required flag, planned or actual RCH invocation, worker id when observed,
remote project root, remote target dir, exit code, elapsed time, output tail,
and degradation codes.
