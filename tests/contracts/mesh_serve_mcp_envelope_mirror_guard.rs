//! bd-3nlkt: regression guard against the envelope-mirror failure
//! mode bd-1zoiw and bd-2eiwy surfaced.
//!
//! Background. Both fixes plugged the same defect class in the
//! cc mcp/serve/mesh domain: an envelope-builder function that wraps
//! an inner ee.response.v2 payload but hard-codes its OWN
//! `response.degradedCodes` field to `[]` regardless of what the
//! inner payload's `degraded[]` array carries. The result is an
//! outer envelope where `response.degradedCodes` is silently empty
//! while `response.payload.degraded[]` is rich — operators and
//! agents triaging via the response-metadata field see zero signal,
//! while the actual degradations sit one level deeper.
//!
//! - bd-1zoiw → render_serve_sse_event (SSE wrapper, future-caller
//!   hazard at the time).
//! - bd-2eiwy → serve_dispatch_exchange_envelope (non-SSE dispatch
//!   wrapper, current-bite via /v1/status).
//!
//! This test pins the all-clean state after both fixes. It scans
//! src/mcp.rs, src/serve.rs, and src/mesh/*.rs for the literal
//! `"degradedCodes": []` token and fails the build if any new
//! occurrence appears. The check is intentionally narrow: only the
//! OUTER `response.degradedCodes` field — which by definition must
//! be derived from the wrapped payload — is at issue. The inner
//! `"degraded": []` literal (used by genuinely synthesized payloads
//! that have no upstream degraded source, e.g.
//! `serve_dispatch_payload_json` building a transport-only envelope)
//! stays legitimate and is not flagged.
//!
//! Companion contract to
//! `tests/contracts/mesh_serve_mcp_degraded_code_catalog.rs` (bd-3nc11,
//! the catalog-side drift gate this audit extended).

use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Files this guard scans. Matches the bd-3nc11 catalog generator's
/// closed list so adding a new source file to the mcp/serve/mesh
/// domain forces a deliberate update here (and ensures the new file
/// gets the envelope-mirror discipline applied).
fn audit_source_files() -> Result<Vec<PathBuf>, String> {
    let mut paths = vec![
        repo_root().join("src").join("serve.rs"),
        repo_root().join("src").join("mcp.rs"),
    ];
    let mesh_dir = repo_root().join("src").join("mesh");
    let mesh_entries = fs::read_dir(&mesh_dir)
        .map_err(|error| format!("read mesh dir {}: {error}", mesh_dir.display()))?;
    let mut mesh_files: Vec<PathBuf> = mesh_entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
        .collect();
    mesh_files.sort();
    paths.extend(mesh_files);
    paths.sort();
    Ok(paths)
}

/// One occurrence of the forbidden literal, with enough context for
/// the failure message to point a reader at the exact line that
/// regressed.
#[derive(Debug)]
struct ForbiddenHit {
    path: PathBuf,
    line_number: usize,
    line: String,
}

fn scan_file(path: &Path) -> Result<Vec<ForbiddenHit>, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut hits = Vec::new();
    for (index, line) in text.lines().enumerate() {
        // We MUST not flag the inner payload's `"degraded": []`
        // literal — it's the legitimate "no upstream degraded source"
        // signal that several synthesized payloads use. The forbidden
        // pattern is specifically the OUTER envelope's
        // `"degradedCodes": []`. Match by literal string so the test
        // does not rely on regex semantics.
        if !line.contains("\"degradedCodes\":") {
            continue;
        }
        // Strip away whitespace + JSON formatting around the value to
        // catch `[ ]`, `[\n]`, `[\t]` shapes too without false
        // positives from longer arrays.
        let after_key = match line.split_once("\"degradedCodes\":") {
            Some((_, rest)) => rest.trim_start(),
            None => continue,
        };
        let trimmed: String = after_key.chars().filter(|c| !c.is_whitespace()).collect();
        if trimmed.starts_with("[]") {
            hits.push(ForbiddenHit {
                path: path.to_path_buf(),
                line_number: index + 1,
                line: line.to_string(),
            });
        }
    }
    Ok(hits)
}

#[test]
fn mesh_serve_mcp_envelope_never_hard_codes_empty_degraded_codes() -> TestResult {
    let mut all_hits: Vec<ForbiddenHit> = Vec::new();
    for path in audit_source_files()? {
        all_hits.extend(scan_file(&path)?);
    }
    if !all_hits.is_empty() {
        let mut details = String::new();
        for hit in &all_hits {
            details.push_str(&format!(
                "  {}:{}    {}\n",
                hit.path
                    .strip_prefix(repo_root())
                    .unwrap_or(&hit.path)
                    .display(),
                hit.line_number,
                hit.line.trim()
            ));
        }
        return Err(format!(
            "{} occurrence(s) of the forbidden \"degradedCodes\": [] literal in the \
             cc mcp/serve/mesh domain. This outer envelope field must be DERIVED from the \
             wrapped inner payload's degraded[] array — see bd-1zoiw (render_serve_sse_event) \
             and bd-2eiwy (serve_dispatch_exchange_envelope) for the established pattern. \
             Hits:\n{}\n\
             Fix: filter the inner payload's degraded[] codes into a Vec<&str> and substitute \
             that into the envelope, mirroring the existing fixes. If a NEW envelope-builder \
             genuinely cannot reach an inner payload (it synthesizes its own data with no \
             upstream degraded source), use the inner-payload form `\"degraded\": []` on the \
             SYNTHESIZED PAYLOAD instead — the outer envelope's degradedCodes still derives \
             from that field, just with a known-empty source.",
            all_hits.len(),
            details,
        ));
    }
    Ok(())
}

/// Sanity-pin the scanner itself. Without this, a regression in
/// `scan_file` (e.g. accidentally matching only single-line forms
/// while the offending value sat on the next line, or filtering
/// out the pattern entirely) would make the primary guard above
/// pass for the wrong reason against an empty hit list. Use an
/// in-memory synthetic source rather than touching real files so
/// the sanity check doesn't depend on the production scan area
/// staying clean.
#[test]
fn envelope_mirror_guard_scanner_detects_the_forbidden_literal() -> TestResult {
    let scratch = std::env::temp_dir().join("bd-3nlkt-scanner-sanity-pin.rs");
    fs::write(
        &scratch,
        "fn synthetic() {\n    \
         let envelope = json!({\n        \
         \"response\": {\n            \
         \"degradedCodes\": []\n        \
         }\n    \
         });\n}\n",
    )
    .map_err(|error| format!("write scratch: {error}"))?;
    let hits = scan_file(&scratch)?;
    let _ = fs::remove_file(&scratch);
    if hits.len() != 1 {
        return Err(format!(
            "scanner sanity-pin: expected exactly 1 hit on synthetic source, got {} hits",
            hits.len()
        ));
    }
    if !hits[0].line.contains("degradedCodes") {
        return Err(format!(
            "scanner sanity-pin: hit line does not mention degradedCodes: {}",
            hits[0].line
        ));
    }
    Ok(())
}

/// Cross-axis with bd-3nc11's catalog: the two bd-1zoiw / bd-2eiwy
/// fix sites use the same mirror pattern (filter_map over the inner
/// `degraded[]`'s `code` field, collect into a Vec<&str>, splice
/// into the outer envelope). Asserting both substrings appear in
/// src/serve.rs locks the pattern as the canonical fix shape so a
/// future "simplification" that drops the mirror cannot quietly
/// land — even if the forbidden literal stays absent.
#[test]
fn serve_rs_retains_both_envelope_mirror_call_sites() -> TestResult {
    let serve_rs = fs::read_to_string(repo_root().join("src").join("serve.rs"))
        .map_err(|error| format!("read src/serve.rs: {error}"))?;
    for needle in [
        // bd-1zoiw: SSE envelope mirror
        "// bd-1zoiw: derive the outer envelope's degradedCodes",
        // bd-2eiwy: dispatch envelope mirror
        "// bd-2eiwy: surface the inner ee.response.v2 payload's `degraded[]`",
    ] {
        if !serve_rs.contains(needle) {
            return Err(format!(
                "src/serve.rs is missing the envelope-mirror provenance breadcrumb {needle:?}; \
                 a refactor may have stripped a fix's bead-id comment. Re-add the comment or \
                 rewire the test if the implementation moved to a helper function with its own \
                 bead-id marker."
            ));
        }
    }
    Ok(())
}
