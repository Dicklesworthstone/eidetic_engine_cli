//! bd-3nc11: golden catalog for every degraded code emitted by the
//! mcp / serve / mesh domain.
//!
//! The existing `tests/contracts/failure_mode_fixtures.rs` walks
//! `tests/fixtures/failure_modes/*.json` forward (fixture → catalog
//! cross-checks). This test walks the OTHER direction —
//! **`src/**` → fixture catalog** — for the codes this pane (cc-mcp)
//! owns. The result is a golden snapshot pinning the current
//! (code → fixture / taxonomy doc / generated-docs) coverage shape
//! so a future drop or addition in any of those four surfaces is
//! caught by a single test failure with a diff that names the
//! exact code and surface that drifted.
//!
//! ## Sources of truth that must stay aligned
//!
//! 1. `src/serve.rs`, `src/mcp.rs`, `src/mesh/*.rs` — `pub const
//!    *_CODE: &str = "snake_case";` declarations.
//! 2. `tests/fixtures/failure_modes/<code>.json` — J6 catalog
//!    failure-mode fixture.
//! 3. `docs/degraded_code_taxonomy.md` — operator-facing table
//!    listing every code with severity + introducing bead.
//! 4. `docs/degraded_codes.md` — generated long-form docs page with
//!    a `## ` section header per code.
//!
//! ## Golden update protocol
//!
//! When this test fails because the catalog legitimately changed
//! (e.g. you added a new mesh degraded code with full coverage), run
//! `UPDATE_GOLDENS=1 cargo test --test contracts
//! mesh_serve_mcp_degraded_code_catalog -- --exact`, then
//! `git diff tests/golden/mesh_serve_mcp_degraded_code_catalog.golden`,
//! review the diff line-by-line, and commit. Per the
//! /testing-golden-artifacts skill, the diff IS the review surface —
//! never blindly accept it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures_dir() -> PathBuf {
    repo_root()
        .join("tests")
        .join("fixtures")
        .join("failure_modes")
}

fn golden_path() -> PathBuf {
    repo_root()
        .join("tests")
        .join("golden")
        .join("mesh_serve_mcp_degraded_code_catalog.golden")
}

/// Files this pane (cc-mcp) owns and whose `pub const *_CODE` declarations
/// belong in the golden catalog. Kept as a closed list so adding a new
/// source file to the domain forces a deliberate update here (and a
/// matching golden refresh), rather than the test silently widening its
/// scope.
fn catalog_source_files() -> Result<Vec<PathBuf>, String> {
    let mut paths = vec![
        repo_root().join("src").join("serve.rs"),
        repo_root().join("src").join("mcp.rs"),
    ];
    let mesh_dir = repo_root().join("src").join("mesh");
    let mesh_entries = fs::read_dir(&mesh_dir)
        .map_err(|error| format!("read mesh source dir {}: {error}", mesh_dir.display()))?;
    let mut mesh_files: Vec<PathBuf> = mesh_entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
        .collect();
    mesh_files.sort();
    paths.extend(mesh_files);
    paths.sort();
    Ok(paths)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DegradedCodeEntry {
    /// Path the constant is declared in, relative to the repo root.
    source: String,
    /// The CONSTANT_NAME (e.g. `MESH_AUDIT_LEDGER_MISSING_CODE`).
    const_name: String,
    /// The wire-form code value (e.g. `mesh_audit_ledger_missing`).
    code_value: String,
    /// `tests/fixtures/failure_modes/<code_value>.json` exists.
    fixture_present: bool,
    /// `docs/degraded_code_taxonomy.md` mentions the code (matches
    /// `taxonomy_has_code` helper shape).
    taxonomy_present: bool,
    /// `docs/degraded_codes.md` has a `## \`<code>\`` section header
    /// for this code (mirrors `generated_docs_has_fixture_link` shape).
    generated_docs_present: bool,
}

/// Parses a single source file for `pub const X_CODE: &str = "y";`
/// declarations. Handles both the single-line shape and the wrapped
/// shape where the value continues on the next line. Lines that look
/// like declarations but never produce a string literal (e.g. an
/// `else` arm) are simply skipped.
fn extract_codes_from_source(path: &Path) -> Result<Vec<(String, String)>, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("read source {}: {error}", path.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim_start();
        let Some(rest) = line.strip_prefix("pub const ") else {
            continue;
        };
        let Some((name_part, after_name)) = rest.split_once(':') else {
            continue;
        };
        let const_name = name_part.trim().to_string();
        if !const_name.ends_with("_CODE") {
            continue;
        }
        let after_eq = after_name.split('=').nth(1).unwrap_or("").trim();
        let collected = if after_eq.is_empty() && index + 1 < lines.len() {
            lines[index + 1].trim().to_string()
        } else {
            after_eq.to_string()
        };
        let Some(value) = collected
            .strip_prefix('"')
            .and_then(|rest| rest.find('"').map(|end| rest[..end].to_string()))
        else {
            continue;
        };
        out.push((const_name, value));
    }
    Ok(out)
}

fn taxonomy_has_code(taxonomy: &str, code: &str) -> bool {
    // Match the row shape used by docs/degraded_code_taxonomy.md
    // (`| \`<code>\` | <severity> | <bead> |`) and tolerate any future
    // surrounding whitespace by looking for the backticked code form.
    taxonomy.contains(&format!("`{code}`"))
}

fn generated_docs_has_code(docs: &str, code: &str) -> bool {
    docs.contains(&format!("## `{code}`"))
}

fn build_catalog() -> Result<Vec<DegradedCodeEntry>, String> {
    let sources = catalog_source_files()?;
    let fixtures = fixtures_dir();
    let taxonomy_text =
        fs::read_to_string(repo_root().join("docs").join("degraded_code_taxonomy.md"))
            .map_err(|error| format!("read taxonomy: {error}"))?;
    let generated_docs_text =
        fs::read_to_string(repo_root().join("docs").join("degraded_codes.md"))
            .map_err(|error| format!("read generated docs: {error}"))?;

    let mut by_code: BTreeMap<String, DegradedCodeEntry> = BTreeMap::new();
    for source in &sources {
        let rel = source
            .strip_prefix(repo_root())
            .unwrap_or(source)
            .display()
            .to_string();
        for (const_name, code_value) in extract_codes_from_source(source)? {
            let fixture_present = fixtures.join(format!("{code_value}.json")).is_file();
            let entry = DegradedCodeEntry {
                source: rel.clone(),
                const_name,
                code_value: code_value.clone(),
                fixture_present,
                taxonomy_present: taxonomy_has_code(&taxonomy_text, &code_value),
                generated_docs_present: generated_docs_has_code(&generated_docs_text, &code_value),
            };
            // Same code declared in two source files collapses to one
            // golden row with a multi-source `source` field. Both
            // const_name and code_value must match byte-for-byte;
            // a mismatch is genuine drift and fails the build (the
            // catalog generator must not paper over divergent
            // declarations of the same code). bd-3e6fq tracks the
            // one known multi-source case today.
            if let Some(existing) = by_code.get_mut(&code_value) {
                if existing.const_name != entry.const_name {
                    return Err(format!(
                        "code {code_value:?} declared with conflicting const_names: \
                         {} in {} vs {} in {}; resolve before regenerating the golden",
                        existing.const_name, existing.source, entry.const_name, entry.source
                    ));
                }
                if !existing.source.contains(&entry.source) {
                    existing.source = format!("{} | {}", existing.source, entry.source);
                }
                existing.fixture_present |= entry.fixture_present;
                existing.taxonomy_present |= entry.taxonomy_present;
                existing.generated_docs_present |= entry.generated_docs_present;
            } else {
                by_code.insert(code_value, entry);
            }
        }
    }
    Ok(by_code.into_values().collect())
}

fn render_catalog(entries: &[DegradedCodeEntry]) -> String {
    let total = entries.len();
    let fixture_hits = entries.iter().filter(|e| e.fixture_present).count();
    let taxonomy_hits = entries.iter().filter(|e| e.taxonomy_present).count();
    let docs_hits = entries.iter().filter(|e| e.generated_docs_present).count();
    let mut out = String::new();
    out.push_str("# mcp/serve/mesh degraded-code catalog (bd-3nc11)\n");
    out.push_str("# Golden snapshot of every `pub const *_CODE: &str = \"...\";` declared in\n");
    out.push_str("# src/serve.rs, src/mcp.rs, and src/mesh/*.rs, with per-surface coverage.\n");
    out.push_str("# Update path: UPDATE_GOLDENS=1 cargo test --test contracts \\\n");
    out.push_str("#   mesh_serve_mcp_degraded_code_catalog -- --exact; review the diff.\n");
    out.push_str("#\n");
    out.push_str(&format!("# Total codes:                {total}\n"));
    out.push_str(&format!("# With failure-mode fixture:  {fixture_hits}\n"));
    out.push_str(&format!(
        "# In docs/degraded_code_taxonomy.md: {taxonomy_hits}\n"
    ));
    out.push_str(&format!(
        "# In docs/degraded_codes.md (## header): {docs_hits}\n"
    ));
    out.push_str("#\n");
    out.push_str("# Columns: code | fixture | taxonomy | gen-docs | const | source\n");
    out.push_str("#\n");
    for entry in entries {
        let flag = |present: bool| if present { "✓" } else { "·" };
        out.push_str(&format!(
            "{code:<48} {fixture:^9} {taxonomy:^10} {docs:^10} {name} :: {src}\n",
            code = entry.code_value,
            fixture = flag(entry.fixture_present),
            taxonomy = flag(entry.taxonomy_present),
            docs = flag(entry.generated_docs_present),
            name = entry.const_name,
            src = entry.source,
        ));
    }
    out
}

fn assert_golden_or_update(test_name: &str, actual: &str, golden: &Path) -> TestResult {
    if std::env::var("UPDATE_GOLDENS").is_ok() {
        if let Some(parent) = golden.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("create golden dir: {error}"))?;
        }
        fs::write(golden, actual).map_err(|error| format!("write golden: {error}"))?;
        eprintln!("[GOLDEN] Updated {}: {}", test_name, golden.display());
        return Ok(());
    }
    let expected = fs::read_to_string(golden).map_err(|error| {
        format!(
            "Golden file missing: {}\n\
             Cause: {error}\n\
             Fix:   UPDATE_GOLDENS=1 cargo test --test contracts {test_name} -- --exact\n\
             Then:  review the new file with `git diff tests/golden/` before commit.",
            golden.display()
        )
    })?;
    if expected != actual {
        let actual_path = golden.with_extension("actual");
        let _ = fs::write(&actual_path, actual);
        return Err(format!(
            "GOLDEN MISMATCH ({test_name}):\n\
             expected (committed):  {expected_path}\n\
             actual (this run):     {actual_path}\n\
             Diff with:             diff -u {expected_path} {actual_path}\n\
             To regenerate:         UPDATE_GOLDENS=1 cargo test --test contracts \\\n\
                                     {test_name} -- --exact\n",
            expected_path = golden.display(),
            actual_path = actual_path.display(),
        ));
    }
    Ok(())
}

/// Primary contract: the rendered catalog matches the golden snapshot.
/// Any change to the set of declared codes OR to their coverage status
/// across the four sources of truth (fixture / taxonomy / generated docs /
/// const declaration) fails this test with a diff-friendly error message.
#[test]
fn mesh_serve_mcp_degraded_code_catalog_matches_golden() -> TestResult {
    let entries = build_catalog()?;
    let rendered = render_catalog(&entries);
    assert_golden_or_update(
        "mesh_serve_mcp_degraded_code_catalog_matches_golden",
        &rendered,
        &golden_path(),
    )
}

/// Sanity-pin the extractor itself. Without this, a regression in
/// `extract_codes_from_source` (e.g. silently producing zero matches
/// because the regex shape drifted) would make the primary golden test
/// pass-by-emptiness against an equally-empty golden update.
#[test]
fn catalog_extractor_finds_a_non_trivial_set_of_codes() -> TestResult {
    let entries = build_catalog()?;
    if entries.len() < 30 {
        return Err(format!(
            "catalog extractor produced only {} entries; the mcp/serve/mesh \
             domain is known to declare ≥30 degraded codes — the extractor or \
             the source-file selector has regressed",
            entries.len()
        ));
    }
    // Spot-check three well-known anchors that should remain stable
    // unless a deliberate rename is in flight.
    for anchor in [
        "mesh_audit_ledger_missing",
        "mesh_audit_ledger_corrupt",
        "mesh_disabled",
    ] {
        if !entries.iter().any(|entry| entry.code_value == anchor) {
            return Err(format!(
                "anchor code {anchor:?} missing from extracted catalog; \
                 either the constant was renamed or the extractor regressed"
            ));
        }
    }
    Ok(())
}

/// Cross-axis assertion that's NOT captured by the golden: every code
/// that DOES have a fixture file also has a taxonomy entry. This pins
/// the J6 catalog's "fixture without doc trail" failure mode the
/// orchestrator's reality-check called out — surface drift surfaces
/// loudly instead of as a quiet golden diff line.
#[test]
fn mesh_serve_mcp_codes_with_fixtures_also_appear_in_taxonomy() -> TestResult {
    let entries = build_catalog()?;
    let mut missing_taxonomy: Vec<String> = entries
        .iter()
        .filter(|entry| entry.fixture_present && !entry.taxonomy_present)
        .map(|entry| format!("  {} (declared in {})", entry.code_value, entry.source))
        .collect();
    missing_taxonomy.sort();
    if !missing_taxonomy.is_empty() {
        return Err(format!(
            "{} mcp/serve/mesh code(s) have a J6 failure-mode fixture but \
             no entry in docs/degraded_code_taxonomy.md — operator-facing \
             docs have drifted from the J6 catalog:\n{}",
            missing_taxonomy.len(),
            missing_taxonomy.join("\n")
        ));
    }
    Ok(())
}
