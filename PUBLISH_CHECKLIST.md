# Publish Checklist

Publish package `eidetic-engine`; its installed binary is `ee`. Current progress
and actual test results are recorded in
[UPGRADE_LOG.md](UPGRADE_LOG.md#cratesio-publication-follow-through).

1. Resolve every required dependency from crates.io, including optional package
   metadata. Remove sibling paths and root Cargo patches. Keep registry versions
   and checksums in `Cargo.lock`; never substitute unpublished source for a
   different published version with the same name.
2. Run formatting, forbidden-dependency inspection, all-target check and Clippy,
   relevant tests, and real memory/search/pack workflows. Use DSR or the repository
   RCH tooling for compilation, with one pinned nightly toolchain. Retain and
   disclose failures, ignored cases and advisory findings.
3. Commit on `main` and inspect `cargo package --list`. Package from that exact
   source and run `cargo publish --dry-run --no-verify`. This validates packaging
   only: separately compile the packaged registry-only graph on a DSR worker.
4. Create and push the release tag without triggering GitHub Actions. Use DSR
   to run the authorized `cargo publish --no-verify` after remote qualification.
   Keep credentials outside source trees, archives, logs and remote workers.
5. Download the public crate. Verify its checksum, source commit, included source
   files and manifest. Run a fresh `cargo +nightly install eidetic-engine --locked`
   through DSR, then exercise init, remember, search, pack and why against a new
   workspace using that installed binary.
6. Build the six supported binaries through DSR, run their platform workflows,
   publish the GitHub release without Actions/dispatch, and verify every public
   asset before updating the four Homebrew archive URLs and hashes.

The current manual releases carry SHA-256 checksums and a build manifest. They
do not carry Sigstore bundles or SLSA attestations; `--require-provenance` remains
unsatisfied. Do not describe unsigned metadata as signed provenance.
