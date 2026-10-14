#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ee::config::EnvVar;

type TestResult = Result<(), String>;

const ALLOWED_RAW_ENV_FILE: &str = "src/config/env_registry.rs";
const RAW_RUNTIME_EE_ENV_PATTERNS: [&str; 4] = [
    r#"std::env::var("EE_"#,
    r#"env::var("EE_"#,
    r#"std::env::var_os("EE_"#,
    r#"env::var_os("EE_"#,
];
const RAW_BUILD_TIME_EE_ENV_PATTERNS: [&str; 2] = [r#"option_env!("EE_"#, r#"env!("EE_"#];
const ALLOWED_BUILD_TIME_EE_ENV_VARS: [&str; 4] = [
    "EE_BUILD_TARGET",
    // Stamped by build.rs:73 via `cargo:rustc-env` from Cargo.lock, so the
    // binary records WHICH sibling engine versions it was built against and not
    // only which commit. Read with `option_env!` at src/core/mod.rs:264 for
    // build provenance and again at :1234 by the test that asserts the stamp
    // exists and names every linked sibling.
    "EE_FRANKEN_STACK",
    "EE_RELEASE_CHANNEL",
    "EE_TRACE_BEAD_ID",
];
const README_WORKSPACE_HYGIENE_ENV_VARS: [&str; 4] = [
    "EE_WORKSPACE_HYGIENE_ALWAYS_REVIEW_PATTERNS",
    "EE_WORKSPACE_HYGIENE_GENERATED_PATTERNS",
    "EE_WORKSPACE_HYGIENE_LOCAL_MACHINE_PATTERNS",
    "EE_WORKSPACE_HYGIENE_SCRATCH_PATTERNS",
];
const README_OBSOLETE_WORKSPACE_HYGIENE_ENV_VARS: [&str; 2] = [
    "EE_WORKSPACE_HYGIENE_SECRET_PATTERNS",
    "EE_WORKSPACE_HYGIENE_IGNORE_PATTERNS",
];

/// Registered variables that no production code reads, and why each is absent.
///
/// The other guards here enforce one direction only: a raw `EE_*` read must go
/// through the registry (`production_code_uses_env_registry_for_ee_vars`), and
/// `docs/env_vars.md` must list every registered name
/// (`tests/env_vars_doc_coverage.rs`). Nothing enforced the converse -- that a
/// registered, documented name is read by anything -- so a variable could be
/// registered, documented with a present-tense description of what it
/// "controls", and honored by nobody. That gap becomes a false statement to a
/// harness in `ee capabilities`, whose `envOverrides` block publishes every
/// registered variable under the claim that ee "honors" it, with
/// `controls: var.description()` (`write_capabilities_env_overrides_block` in
/// src/output/mod.rs).
///
/// Measured 2026-10-14: 11 of 131 registered variables had no production read.
/// Now 10. `EE_CURATION_AUTO_PROMOTE_MAX_PER_RUN` was wired to
/// `ee curate auto-promote --max-per-run` and its row removed in the same
/// change, which is this list working as intended: the test fails when an entry
/// becomes read, so honouring a variable forces its deletion here.
/// The reasons differ in kind and the distinction is the point -- most name a
/// subsystem that has no production caller at all, which is a different defect
/// from a knob someone forgot to plumb -- so each entry carries its own.
///
/// An entry is not permission to leave a variable unread. The test fails both
/// ways: an unread variable missing from this list fails, and an entry that
/// becomes read fails until it is removed. Wiring one up forces its deletion.
const REGISTERED_WITHOUT_PRODUCTION_READ: [(&str, &str); 10] = [
    (
        "EE_AUDIT_LANE_BATCH_MAX",
        "audit lane has no production caller: the only AuditLane::new sits in the \
         #[cfg(test)] module of src/core/memory.rs, and production emits through \
         emit_with_direct_fallback instead",
    ),
    (
        "EE_AUDIT_LANE_CAPACITY",
        "same unspawned audit lane; DEFAULT_AUDIT_LANE_CAPACITY in src/core/audit_lane.rs \
         is the only capacity any queue is built with",
    ),
    (
        "EE_AUDIT_LANE_FLUSH_MS",
        "same unspawned audit lane; no production flush loop exists to bound",
    ),
    (
        "EE_CURATION_AUTO_PROMOTE_CONFIDENCE_FLOOR",
        "docs/env_vars.md calls it a reserved contract; `ee curate auto-promote` exposes \
         --min-confidence-episodic and --min-confidence-semantic, and one floor does not \
         map onto both without deciding which it overrides",
    ),
    (
        "EE_CURATION_DERIVED_PREVIEW_LIMIT",
        "documented as reserved for derived-candidate preview and reject ergonomics",
    ),
    (
        "EE_EXPERIMENTAL_TRIAD",
        "documented compatibility no-op, retained so spike-era scripts keep running; this \
         one is deliberate and expected to stay listed",
    ),
    (
        "EE_MESH_DISCOVERY_CACHE_TTL_SECONDS",
        "DEFAULT_DISCOVERY_CACHE_TTL_SECONDS in src/mesh/discovery_cache.rs is the only TTL \
         any decision uses; nothing reads the override",
    ),
    (
        "EE_REFLECTION_REQUEST_SHOW_SOURCE_LIMIT",
        "documented as a downstream contract that paths 'should' report, not one they do",
    ),
    (
        "EE_REFLECTION_SOURCE_BUDGET_BYTES",
        "same reflection contract; no propose or ingest path reads the budget",
    ),
    (
        "EE_WORKSPACE_CLOSE_DRAIN_TIMEOUT_S",
        "drain_snapshot_pins in src/db/read_pool.rs has no caller outside that file's \
         #[cfg(test)] module, so no production workspace-close wait exists to bound",
    ),
];

fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) -> TestResult {
    for entry in fs::read_dir(root).map_err(|error| format!("read {}: {error}", root.display()))? {
        let entry =
            entry.map_err(|error| format!("read entry under {}: {error}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

fn compact_code_line(line: &str) -> String {
    line.split("//")
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn line_has_raw_runtime_ee_env_read(line: &str) -> bool {
    let compact = compact_code_line(line);
    RAW_RUNTIME_EE_ENV_PATTERNS
        .iter()
        .any(|pattern| compact.contains(pattern))
}

fn build_time_ee_var_name(line: &str) -> Option<String> {
    let compact = compact_code_line(line);
    for pattern in RAW_BUILD_TIME_EE_ENV_PATTERNS {
        let Some(pattern_start) = compact.find(pattern) else {
            continue;
        };
        let name_start = pattern_start + pattern.len() - "EE_".len();
        let rest = &compact[name_start..];
        let name_end = rest.find('"')?;
        return Some(rest[..name_end].to_owned());
    }
    None
}

#[test]
fn production_code_uses_env_registry_for_ee_vars() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files)?;

    let mut violations = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .map_err(|error| format!("strip {}: {error}", file.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == ALLOWED_RAW_ENV_FILE {
            continue;
        }

        let content = fs::read_to_string(&file)
            .map_err(|error| format!("read {}: {error}", file.display()))?;
        for (line_index, line) in content.lines().enumerate() {
            if line_has_raw_runtime_ee_env_read(line) {
                violations.push(format!("{}:{}", relative, line_index + 1));
            }
            if let Some(var_name) = build_time_ee_var_name(line)
                && !ALLOWED_BUILD_TIME_EE_ENV_VARS.contains(&var_name.as_str())
            {
                violations.push(format!(
                    "{}:{} unregistered build-time env {var_name}",
                    relative,
                    line_index + 1
                ));
            }
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "raw EE_* env reads must use config::env_registry: {}",
            violations.join(", ")
        ))
    }
}

#[test]
fn allowed_build_time_ee_vars_are_documented() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let docs_path = root.join("docs/env_vars.md");
    let docs = fs::read_to_string(&docs_path)
        .map_err(|error| format!("read {}: {error}", docs_path.display()))?;
    for var_name in ALLOWED_BUILD_TIME_EE_ENV_VARS {
        let needle = format!("`{var_name}`");
        if !docs.contains(&needle) {
            return Err(format!(
                "allowed build-time env var {var_name} must be documented in docs/env_vars.md"
            ));
        }
    }
    Ok(())
}

#[test]
fn registered_env_vars_are_read_by_production_code() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files)?;
    if files.is_empty() {
        return Err("no .rs files under src/: the scan cannot observe any read".to_owned());
    }

    // Comment-stripped, whitespace-free code for every src file EXCEPT the
    // registry, which spells all 131 names and every variant and so would
    // match each one as its own reader.
    let mut code = Vec::new();
    for file in &files {
        let relative = file
            .strip_prefix(&root)
            .map_err(|error| format!("strip {}: {error}", file.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if relative == ALLOWED_RAW_ENV_FILE {
            continue;
        }
        let content = fs::read_to_string(file)
            .map_err(|error| format!("read {}: {error}", file.display()))?;
        code.push(
            content
                .lines()
                .map(compact_code_line)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    let is_read = |token: &str| code.iter().any(|text| text.contains(token));

    // POSITIVE CONTROL, run through the same containment scan that produces the
    // verdict. A variable known to be read must come out read. Without it an
    // empty or wrongly-stripped corpus reports every variable as unread, and
    // because the verdict is a set comparison that failure would land as "every
    // allowlist entry is correctly unread" -- a pass. The scan must be shown
    // capable of seeing a read before its claim that it saw none means anything.
    let control = format!("{:?}", EnvVar::DaemonMaxInflight);
    if !is_read(&control) {
        return Err(format!(
            "positive control failed: {control} is read via env_registry::read in \
             src/daemon/server.rs, but this scan did not see it, so its unread set is void"
        ));
    }

    // A variant is the read path (`env_registry::read(EnvVar::Variant)`); the
    // literal is checked too, for a name reached some other way.
    let unread: BTreeSet<String> = EnvVar::all()
        .iter()
        .filter(|var| !is_read(&format!("{var:?}")) && !is_read(var.name()))
        .map(|var| var.name().to_owned())
        .collect();
    let registered: BTreeSet<String> = EnvVar::all()
        .iter()
        .map(|var| var.name().to_owned())
        .collect();
    let allowlisted: BTreeSet<String> = REGISTERED_WITHOUT_PRODUCTION_READ
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();

    let mut failures = Vec::new();
    for name in allowlisted.difference(&registered) {
        failures.push(format!(
            "{name} is in REGISTERED_WITHOUT_PRODUCTION_READ but is not a registered EnvVar \
             name -- fix the spelling or drop the entry"
        ));
    }
    for name in unread.difference(&allowlisted) {
        failures.push(format!(
            "{name} is registered and documented, and `ee capabilities` advertises what it \
             controls, but nothing in src/ reads it: wire it up, or add it to \
             REGISTERED_WITHOUT_PRODUCTION_READ with the reason"
        ));
    }
    for name in allowlisted.difference(&unread) {
        if registered.contains(name) {
            failures.push(format!(
                "{name} is read by production code now: remove it from \
                 REGISTERED_WITHOUT_PRODUCTION_READ so the list keeps meaning something"
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[test]
fn raw_runtime_detector_handles_whitespace_and_ignores_line_comments() {
    assert!(line_has_raw_runtime_ee_env_read(
        r#"std::env::var ( "EE_UNREGISTERED" )"#
    ));
    assert!(line_has_raw_runtime_ee_env_read(
        r#"env :: var_os ( "EE_UNREGISTERED" )"#
    ));
    assert!(!line_has_raw_runtime_ee_env_read(
        r#"// std::env::var("EE_DOCUMENTED_ONLY")"#
    ));
}

#[test]
fn build_time_detector_handles_whitespace_and_ignores_line_comments() {
    assert_eq!(
        build_time_ee_var_name(r#"option_env ! ( "EE_TRACE_BEAD_ID" )"#).as_deref(),
        Some("EE_TRACE_BEAD_ID")
    );
    assert_eq!(
        build_time_ee_var_name(r#"env ! ( "EE_NOT_ALLOWED" )"#).as_deref(),
        Some("EE_NOT_ALLOWED")
    );
    assert_eq!(
        build_time_ee_var_name(r#"// option_env!("EE_COMMENT_ONLY")"#),
        None
    );
}

#[test]
fn readme_workspace_hygiene_env_overlays_match_registry() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let readme_path = root.join("README.md");
    let readme = fs::read_to_string(&readme_path)
        .map_err(|error| format!("read {}: {error}", readme_path.display()))?;
    let row = readme
        .lines()
        .find(|line| line.contains("workspace hygiene classifier overlays"))
        .ok_or_else(|| "README is missing workspace hygiene classifier overlay row".to_string())?;

    for var_name in README_WORKSPACE_HYGIENE_ENV_VARS {
        if !row.contains(var_name) {
            return Err(format!(
                "README workspace hygiene overlay row must document registered env var {var_name}"
            ));
        }
    }
    for var_name in README_OBSOLETE_WORKSPACE_HYGIENE_ENV_VARS {
        if row.contains(var_name) {
            return Err(format!(
                "README workspace hygiene overlay row still documents obsolete env var {var_name}"
            ));
        }
    }
    Ok(())
}
