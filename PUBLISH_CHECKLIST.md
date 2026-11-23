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
   disclose failures, ignored cases and advisory findings. Keep each pinned
   RCH verification JSON receipt and both of its terminal streams. Supply its
   `receiptPath` in the capsule manifest stage entry along with the stage log
   and full candidate commit. Verify the Rust inventory from the candidate tree.

   ```bash
   python3 scripts/lib/emit_proof_capsule.py --verify-capsule "$capsule" \
     --expected-sha "$release_commit" --verify-logs --verify-rch-policy
   ```
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
6. Build the six supported binaries through DSR and run their platform workflows.
   Generate a `release-probe-{target}.json` receipt for each native run using
   `scripts/release_binary_probe.sh`. In the capsule manifest, include the
   DSR `dsrManifestPath` and each binary's executable `path` and `archivePath`.
   Add `probePath` for native runs. Verify staging against the committed target
   inventory and the same candidate used for Rust qualification.

   ```bash
   python3 scripts/lib/emit_proof_capsule.py --verify-capsule "$capsule" \
     --expected-sha "$release_commit" --expected-version "$release_version" \
     --dsr-manifest "$dsr_manifest" --staging-dir "$stage_dir"
   ```

   `scripts/release_binary_probe.sh --verify-release-dir` on its own inspects
   captures. The staging check also requires archive, executable and receipt
   digests and the candidate's committed target dispositions. The capsule's
   self-hash detects changes relative to the retained evidence. Release signing
   separately authenticates the published assets. Publish the GitHub release
   without Actions and verify every public asset before updating the four
   Homebrew archive URLs and hashes.

The current manual releases carry SHA-256 checksums and minisign `.minisig`
signatures (without a release `manifest.json` build manifest; see `AGENTS.md`). They
do not carry Sigstore bundles or SLSA attestations; `--require-provenance` remains
unsatisfied. Do not describe unsigned metadata as signed provenance.
