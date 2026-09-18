//! Stamp build provenance into the binary (bd-reality-core-convergence-1azkt.10).
//!
//! WHY THIS FILE DID NOT EXIST AND WHAT THAT COST. `src/core/mod.rs:248-251`
//! reads its provenance from `option_env!("VERGEN_GIT_SHA")` and three
//! siblings. Those are COMPILE-TIME lookups, this package had no build script,
//! `vergen` is not a dependency, and nothing in the repository ever set the
//! variables. So every build of `ee` on every host reported
//! `targetTriple: "unknown"` and `state: "unavailable"`, and
//! `tests/retrieval_index_regression_oracle.rs::attest()` refuses exactly those
//! values. The oracle could not attest to a commit because no mechanism to
//! stamp one existed, not because any probe had failed.
//!
//! WHAT THIS DELIBERATELY DOES NOT DO: RUN GIT. An earlier draft of this file
//! shelled out to `git rev-parse HEAD`. That is wrong twice over.
//!
//! It is wrong on CORRECTNESS, because the build script's working directory is
//! not authoritative about what it is building. Workers build from a source
//! tree exported WITHOUT `.git` (`rch exec --clean-overlay`), so git would
//! answer from whatever repository happened to enclose the build, or not at
//! all. The harness that exported the tree is the only thing that knows which
//! commit it came from, and it already knows: it was passed `--base <sha>`.
//! A binary that infers its own commit from ambient state is asserting
//! something it cannot know; a binary handed one by the harness is repeating
//! something the harness does know.
//!
//! It is wrong on HERMETICITY, because `VersionReport::gather`
//! (`src/core/mod.rs:408-424`) turns missing provenance into entries in the
//! response's `degraded[]` array. Running git would make `degraded[]`,
//! `source.state` and three source fields all depend on whether the build host
//! happened to have a `.git` directory — the same host-dependence that is
//! already the subject of bd-rm8wj's environmental causes. `degraded[]` is a
//! contract-bearing array, and the fix for a golden that pins it is never to
//! widen a scrubber over it.
//!
//! SO THE SCOPE IS TWO THINGS, BOTH DETERMINISTIC:
//!
//! 1. `EE_BUILD_TARGET`, from cargo's `TARGET`. This is the part that CANNOT be
//!    done any other way: cargo sets `TARGET` for build scripts only, never for
//!    the crate's own rustc invocation, so `option_env!("EE_BUILD_TARGET")` can
//!    only ever see a value a build script put there. It is set on every build
//!    on every host, so it moves `targetTriple` off `"unknown"` uniformly and
//!    retires `target_triple_unavailable` uniformly.
//!
//! 2. Pass-through plus `rerun-if-env-changed` for the three `VERGEN_GIT_*`
//!    variables. Re-emitting them is what makes an externally supplied stamp
//!    take effect rather than being served from a cached build, and it does not
//!    depend on ambient environment reaching rustc on its own.
//!
//! When nobody supplies those variables — an ordinary `cargo build`, a worker,
//! a release tarball — this emits nothing for them and the binary reports
//! `state: "unavailable"` with all three git fields null, which is precisely
//! what `tests/fixtures/golden/version/version.golden` has always asserted.
//! That path is unchanged on purpose.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Cargo sets TARGET for build scripts on every build, cross or native.
    if let Ok(target) = std::env::var("TARGET") {
        if let Some(target) = sanitized(&target) {
            println!("cargo:rustc-env=EE_BUILD_TARGET={target}");
        }
    }

    // bd-reality-core-convergence-1azkt.10, bullet 2. A binary that can name its
    // commit still cannot say WHICH ENGINE it was linked against, and this
    // repository has already produced phantom reds from exactly that gap: two
    // runs at identical source, different sibling pins, different verdicts.
    // Cargo.lock is committed, so the answer travels with the source tree and is
    // the same on every host that builds that commit.
    println!("cargo:rerun-if-changed=Cargo.lock");
    if let Some(pins) = franken_stack_pins() {
        println!("cargo:rustc-env=EE_FRANKEN_STACK={pins}");
    }

    for key in ["VERGEN_GIT_SHA", "VERGEN_GIT_DESCRIBE", "VERGEN_GIT_DIRTY"] {
        // Declared unconditionally, including when the variable is absent:
        // going from unset to set has to invalidate the build, or the first
        // attested build after an unattested one would be served from cache
        // with no stamp in it.
        println!("cargo:rerun-if-env-changed={key}");
        if let Ok(value) = std::env::var(key) {
            if let Some(value) = sanitized(&value) {
                println!("cargo:rustc-env={key}={value}");
            }
        }
    }
}

/// The franken-stack crate versions this build resolved, as
/// `asupersync@0.5.0,frankensearch@0.6.0,fsqlite@0.4.1`, or `None` when
/// `Cargo.lock` is absent or does not name them.
///
/// Read from the LOCKFILE, not from `Cargo.toml`, because the lock is what was
/// actually resolved: a manifest requirement of `0.6` is satisfied by several
/// versions and only one of them is in this binary.
///
/// `@` separates name from version rather than `=`, because
/// `clean_build_metadata` in `src/core/mod.rs` rejects any value containing
/// `=`, `/` or `\`, and a rejected stamp would read downstream as "no
/// franken-stack information" — silently, and indistinguishably from a build
/// that genuinely had none.
fn franken_stack_pins() -> Option<String> {
    const TRACKED: [&str; 3] = ["asupersync", "frankensearch", "fsqlite"];

    let lock = std::fs::read_to_string("Cargo.lock").ok()?;
    let mut pins: Vec<String> = Vec::new();
    let mut current: Option<&str> = None;

    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            current = None;
        } else if let Some(rest) = line.strip_prefix("name = ") {
            let name = rest.trim_matches('"');
            current = TRACKED.iter().find(|tracked| **tracked == name).copied();
        } else if let Some(rest) = line.strip_prefix("version = ") {
            // `version` always follows `name` inside a `[[package]]` block, so
            // `current` still names the package this version belongs to.
            if let Some(name) = current.take() {
                let version = rest.trim_matches('"');
                if !version.is_empty() {
                    pins.push(format!("{name}@{version}"));
                }
            }
        }
    }

    if pins.is_empty() {
        // Emit nothing rather than an empty or partial string: a caller cannot
        // tell "no siblings" from "could not read the lock", and this field is
        // an attestation input, so the honest answer to "cannot tell" is
        // silence.
        return None;
    }
    pins.sort();
    sanitized(&pins.join(",")).map(str::to_owned)
}

/// Accept a value only if it is safe to put after `=` on a cargo directive
/// line, and non-empty once trimmed.
///
/// A newline in a value would end the directive and let the remainder be read
/// as a further instruction to cargo, so a hostile or merely malformed
/// environment could inject one. Refusing the value degrades to the honest
/// unavailable state; passing it through would not.
fn sanitized(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    Some(trimmed)
}
