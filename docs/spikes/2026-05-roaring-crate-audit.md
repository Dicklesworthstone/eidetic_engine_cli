# Roaring Crate Forbidden-Dependency Audit

Date: 2026-05-15
Owner: AmberCondor
Bead: bd-3usjw.63

## Decision

Use `roaring = "0.11.4"` with default features for `bd-3usjw.51`.

Rationale:

- It is the current non-yanked `roaring` crate release in the crates.io sparse index.
- It is pure Rust and avoids the C build surface introduced by `croaring` / `croaring-sys`.
- `cargo tree -e features` resolved cleanly across the release target matrix.
- The repository forbidden-dependency scanner logic passed against a scratch manifest containing only `roaring = "0.11.4"` and failed as expected against a `tokio` positive control.

Do not use `croaring` for this cache-key slice unless a later benchmark proves the C dependency is required. Do not use `roaring_bitmap`; it is dependency-clean, but it is an old legacy crate and has no reason to displace the maintained `roaring` crate.

## Sources

- `roaring` sparse-index metadata: https://index.crates.io/ro/ar/roaring
- `croaring` sparse-index metadata: https://index.crates.io/cr/oa/croaring
- `croaring-sys` sparse-index metadata: https://index.crates.io/cr/oa/croaring-sys
- `roaring_bitmap` sparse-index metadata: https://index.crates.io/ro/ar/roaring_bitmap
- `roaring-bitmaps` crates.io API lookup: https://crates.io/api/v1/crates/roaring-bitmaps
- Cargo index format reference: https://doc.rust-lang.org/cargo/reference/registry-index.html
- Release target matrix: `.github/workflows/release.yml`

## Release Targets Checked

The audit used the release matrix target triples:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`

Scratch manifests, Cargo locks, `cargo tree` output, and script-check output are under:

`/Volumes/USBNVME16TB/temp_agent_space/roaring_crate_audit_20260515T0148/`

This audit ran metadata and dependency-tree resolution only. It did not run any local Cargo build or test.

## Findings

| Crate | Version | Resolved normal/build deps | Forbidden deps | Verdict | Notes |
| --- | ---: | --- | --- | --- | --- |
| `roaring` | `0.11.4` | `bytemuck 1.25.0`, `byteorder 1.5.0` | None | Choose | Clean across all release targets. Pure Rust. |
| `croaring` | `2.6.0` | `allocator-api2 0.4.0`, `croaring-sys 4.7.0`, build dep `cc 1.2.62`, `find-msvc-tools 0.1.9`, `shlex 1.3.0` | None | Reject for this slice | Dependency-clean, but introduces C FFI/build-tool surface. |
| `roaring_bitmap` | `0.1.3` | None | None | Reject | Dependency-clean but legacy/low-signal compared to maintained `roaring`. |
| `roaring-bitmaps` | N/A | N/A | N/A | Not a crate | Direct crates.io API lookup returned 404; nearest hyphen/underscore lookup resolves to `roaring_bitmap`. |

## Cargo Tree Results

For each candidate and target triple:

```jsonl
{"crate_name":"croaring","version":"2.6.0","forbidden_match":false,"target_triple":"aarch64-apple-darwin","verdict":"clean"}
{"crate_name":"croaring","version":"2.6.0","forbidden_match":false,"target_triple":"x86_64-apple-darwin","verdict":"clean"}
{"crate_name":"croaring","version":"2.6.0","forbidden_match":false,"target_triple":"x86_64-unknown-linux-gnu","verdict":"clean"}
{"crate_name":"croaring","version":"2.6.0","forbidden_match":false,"target_triple":"x86_64-unknown-linux-musl","verdict":"clean"}
{"crate_name":"croaring","version":"2.6.0","forbidden_match":false,"target_triple":"aarch64-unknown-linux-gnu","verdict":"clean"}
{"crate_name":"croaring","version":"2.6.0","forbidden_match":false,"target_triple":"x86_64-pc-windows-msvc","verdict":"clean"}
{"crate_name":"roaring_bitmap","version":"0.1.3","forbidden_match":false,"target_triple":"aarch64-apple-darwin","verdict":"clean"}
{"crate_name":"roaring_bitmap","version":"0.1.3","forbidden_match":false,"target_triple":"x86_64-apple-darwin","verdict":"clean"}
{"crate_name":"roaring_bitmap","version":"0.1.3","forbidden_match":false,"target_triple":"x86_64-unknown-linux-gnu","verdict":"clean"}
{"crate_name":"roaring_bitmap","version":"0.1.3","forbidden_match":false,"target_triple":"x86_64-unknown-linux-musl","verdict":"clean"}
{"crate_name":"roaring_bitmap","version":"0.1.3","forbidden_match":false,"target_triple":"aarch64-unknown-linux-gnu","verdict":"clean"}
{"crate_name":"roaring_bitmap","version":"0.1.3","forbidden_match":false,"target_triple":"x86_64-pc-windows-msvc","verdict":"clean"}
{"crate_name":"roaring","version":"0.11.4","forbidden_match":false,"target_triple":"aarch64-apple-darwin","verdict":"clean"}
{"crate_name":"roaring","version":"0.11.4","forbidden_match":false,"target_triple":"x86_64-apple-darwin","verdict":"clean"}
{"crate_name":"roaring","version":"0.11.4","forbidden_match":false,"target_triple":"x86_64-unknown-linux-gnu","verdict":"clean"}
{"crate_name":"roaring","version":"0.11.4","forbidden_match":false,"target_triple":"x86_64-unknown-linux-musl","verdict":"clean"}
{"crate_name":"roaring","version":"0.11.4","forbidden_match":false,"target_triple":"aarch64-unknown-linux-gnu","verdict":"clean"}
{"crate_name":"roaring","version":"0.11.4","forbidden_match":false,"target_triple":"x86_64-pc-windows-msvc","verdict":"clean"}
```

Forbidden pattern checked:

`tokio|tokio-util|async-std|smol|rusqlite|sqlx|diesel|sea-orm|petgraph|hyper|axum|tower|reqwest`

No generated tree file matched that pattern.

## Forbidden-Dependency Script Check

The existing `scripts/check-forbidden-deps.sh` is hard-coded to derive `REPO_ROOT` from its own path, so the scratch check copied the script unchanged under scratch manifests.

Negative control:

- Manifest: scratch package with `roaring = "0.11.4"`
- Exit status: `0`
- Output: `ok: no forbidden dependencies detected in the resolved tree`

Positive control:

- Manifest: scratch package with `tokio = "1"`
- Exit status: `2`
- Output included `tokio` under forbidden dependencies.

Result: the scanner logic catches a forbidden crate and passes the selected crate.

## Follow-Up Constraint For bd-3usjw.51

`bd-3usjw.51` should pin `roaring = "0.11.4"` and avoid enabling optional features unless the implementation proves they are needed. If serialization is needed, re-run this audit with the exact feature set before changing `Cargo.toml`.
