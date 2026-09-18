//! bd-e5h7g — the committed-tree path-DEPENDENCY detector must discriminate.
//!
//! `scripts/rch_verify.sh` refuses to materialise a committed tree when the
//! manifest declares a path dependency, because such a dependency resolves from
//! the filesystem and cannot travel with a git archive. That is correct. What
//! was wrong was the question it asked: a substring test for `path =` over the
//! whole manifest, which cannot tell a path DEPENDENCY from a TARGET path.
//!
//! This repository declares 45 target paths and zero path dependencies
//! (`autotests = false` is why every `[[test]]` must spell out its `path =`), so
//! the gate refused every verification lane on a property the repo does not
//! have — in preflight, before RCH was contacted at all.
//!
//! THE REPAIR MAKES A REFUSING GATE MORE PERMISSIVE, and more permissive is
//! indistinguishable from weaker without both arms. So this file asserts both
//! directions, and a fail-closed third case, against the SAME file the script
//! loads: `scripts/cargo_path_deps.py`. A reimplementation of the predicate here
//! would only prove the reimplementation.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

type TestResult = Result<(), String>;
static FIXTURE_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn predicate_script() -> PathBuf {
    repo_root().join("scripts").join("cargo_path_deps.py")
}

fn fixture_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/rch-verify-path-deps"))
}

fn write_manifest(body: &str) -> Result<PathBuf, String> {
    let index = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = fixture_dir().join(format!("pathdeps-{}-{index}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let path = dir.join("Cargo.toml");
    fs::write(&path, body).map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(path)
}

/// Run the predicate exactly as `rch_verify.sh` would reach it, and return its
/// report. Failure to run at all is an error, never a silent `false`.
fn inspect(manifest: &PathBuf) -> Result<Value, String> {
    let script = predicate_script();
    let output = Command::new("python3")
        .arg(&script)
        .arg(manifest)
        .output()
        .map_err(|error| format!("run {}: {error}", script.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} exited {:?}: {}",
            script.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "predicate stdout was not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn verdict(report: &Value) -> &Value {
    &report["path_dependencies"]
}

/// POSITIVE ARM: target paths alone must not be read as dependencies.
///
/// This is the claim the old substring test got wrong, and the reason every
/// verification lane on this repository refused.
#[test]
fn target_paths_alone_are_not_path_dependencies() -> TestResult {
    let manifest = write_manifest(
        r#"
[package]
name = "demo"

[lib]
path = "src/lib.rs"

[[bin]]
name = "demo"
path = "src/main.rs"

[[test]]
name = "integration_a_d"
path = "tests/suites/integration_a_d.rs"

[[bench]]
name = "context_stream"
path = "benches/context_stream.rs"

[dependencies]
serde = { version = "1", features = ["derive"] }
frankensearch = { version = "=0.6.0", default-features = false }
"#,
    )?;
    let report = inspect(&manifest)?;

    // PRECONDITION: the fixture must actually contain the substring that used to
    // trip the gate. Without this the arm is vacuous — it would also pass on a
    // manifest with no `path =` at all, which proves nothing about the fix.
    let body = fs::read_to_string(&manifest).map_err(|error| error.to_string())?;
    let target_paths = body.matches("path = ").count();
    if target_paths < 4 {
        return Err(format!(
            "fixture must carry the substring the old test keyed on; found {target_paths}"
        ));
    }

    if verdict(&report) != &Value::Bool(false) {
        return Err(format!(
            "target-only paths must not count as path dependencies: {report}"
        ));
    }
    Ok(())
}

/// NEGATIVE ARM: a real path dependency must still be refused.
///
/// Without this the change is not a fixed detector, it is a removed one.
#[test]
fn a_real_path_dependency_is_still_detected() -> TestResult {
    for (label, body) in [
        (
            "inline table",
            r#"
[package]
name = "demo"
[lib]
path = "src/lib.rs"
[dependencies]
foo = { path = "../foo" }
"#,
        ),
        (
            "dotted sub-table",
            r#"
[package]
name = "demo"
[dependencies.foo]
path = "../foo"
version = "1"
"#,
        ),
        (
            "dev-dependencies",
            r#"
[package]
name = "demo"
[dev-dependencies]
harness = { path = "../harness" }
"#,
        ),
        (
            "patch redirect",
            r#"
[package]
name = "demo"
[patch.crates-io]
asupersync = { path = "/data/projects/asupersync" }
"#,
        ),
        (
            "platform-gated",
            r#"
[package]
name = "demo"
[target.'cfg(unix)'.dependencies]
nix_helper = { path = "../nix-helper" }
"#,
        ),
    ] {
        let manifest = write_manifest(body)?;
        let report = inspect(&manifest)?;
        if verdict(&report) != &Value::Bool(true) {
            return Err(format!(
                "a path dependency declared as a {label} must still be refused: {report}"
            ));
        }
        // The report must also NAME the offender, so a refusal is actionable
        // rather than a bare boolean.
        let named = report["path_dependency_names"]
            .as_array()
            .map(|names| names.len())
            .unwrap_or(0);
        if named == 0 {
            return Err(format!(
                "a refusal must name the offending dependency: {report}"
            ));
        }
    }
    Ok(())
}

/// FAIL-CLOSED: an unparseable manifest is not evidence of absence.
///
/// `null` here, and the caller in `rch_verify.sh` treats it exactly as `true`.
/// A gate that relaxed on "cannot tell" would be weaker, not fixed.
#[test]
fn an_unparseable_manifest_does_not_report_absence() -> TestResult {
    let manifest = write_manifest("[package\nname = \"broken\n")?;
    let report = inspect(&manifest)?;
    if verdict(&report) != &Value::Null {
        return Err(format!(
            "an unparseable manifest must report null, not a verdict: {report}"
        ));
    }
    if report["parsed"] != Value::Bool(false) {
        return Err(format!(
            "unparseable manifest must report parsed=false: {report}"
        ));
    }
    Ok(())
}

/// NON-VACUITY plus the live case: the two arms must disagree, and this
/// repository's own manifest must land on the passing side.
///
/// If the predicate ever returns the same answer for both fixtures it has
/// stopped discriminating, and every other assertion here would still pass.
#[test]
fn the_detector_discriminates_and_admits_this_repository() -> TestResult {
    let targets_only = write_manifest(
        "[package]\nname = \"demo\"\n[lib]\npath = \"src/lib.rs\"\n[dependencies]\nserde = { version = \"1\" }\n",
    )?;
    let path_dep = write_manifest(
        "[package]\nname = \"demo\"\n[dependencies]\nfoo = { path = \"../foo\" }\n",
    )?;
    let clean = inspect(&targets_only)?;
    let dirty = inspect(&path_dep)?;
    if verdict(&clean) == verdict(&dirty) {
        return Err(format!(
            "detector must discriminate; both fixtures returned {}",
            verdict(&clean)
        ));
    }

    // The live manifest. This is the case bd-e5h7g was filed about: 45 target
    // paths, zero path dependencies. It must be admitted, and it must be
    // admitted for the right reason — some dependency table was actually read.
    let live = repo_root().join("Cargo.toml");
    let report = inspect(&live)?;
    if verdict(&report) != &Value::Bool(false) {
        return Err(format!(
            "this repository declares no path dependencies: {report}"
        ));
    }
    let inspected = report["dependency_tables_inspected"]
        .as_array()
        .map(|tables| tables.len())
        .unwrap_or(0);
    if inspected == 0 {
        return Err(format!(
            "a `false` that inspected no dependency table is vacuous: {report}"
        ));
    }
    Ok(())
}
