//! Code-less Cargo/build/linker diagnostics from native plain-text reports.
//!
//! Only a native diagnostic, optionally preceded by Cargo progress, claims
//! this format. Quoted examples, JSON and captured build-script output are
//! not independent errors. Validate the whole bounded report before exposing
//! its fingerprints; an exit status still belongs to the invocation ledger.

use std::collections::BTreeSet;

use super::{CanonicalDiagnostic, MAX_DIAGNOSTICS_PER_FAILURE, MAX_SCANNED_OUTPUT_BYTES};
use crate::core::error_recall::{from_cargo, from_rustc};

const MAX_ERRORS: usize = 256;
const MAX_CAUSES: usize = 4;

#[derive(Clone, Copy)]
enum Origin {
    Cargo,
    Rustc,
}

struct Diagnostic<'a> {
    origin: Origin,
    message: &'a str,
    causes: Vec<&'a str>,
}

fn quoted(text: &str) -> Result<(&str, &str), ()> {
    let text = text.strip_prefix('`').ok_or(())?;
    let (value, rest) = text.split_once('`').ok_or(())?;
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(());
    }
    Ok((value, rest))
}

fn diagnostic(line: &str) -> Result<Option<Diagnostic<'_>>, ()> {
    let Some(message) = line.strip_prefix("error: ") else {
        return Ok(None);
    };
    let origin = if let Some(rest) = message.strip_prefix("failed to get ") {
        let (_, rest) = quoted(rest)?;
        let rest = rest.strip_prefix(" as a dependency of package ").ok_or(())?;
        if !quoted(rest)?.1.is_empty() {
            return Err(());
        }
        Origin::Cargo
    } else if let Some(rest) = message.strip_prefix("no matching package named ") {
        if quoted(rest)?.1 != " found" {
            return Err(());
        }
        Origin::Cargo
    } else if let Some(rest) = message.strip_prefix("could not find `Cargo.toml` in ") {
        if quoted(rest)?.1 != " or any parent directory" {
            return Err(());
        }
        Origin::Cargo
    } else if let Some(rest) = message.strip_prefix("linking with ") {
        let (_, rest) = quoted(rest)?;
        let status = rest.strip_prefix(" failed: exit status: ").ok_or(())?;
        if !status.parse::<i32>().is_ok_and(|code| code != 0) {
            return Err(());
        }
        Origin::Rustc
    } else if let Some(rest) = message.strip_prefix("linker ") {
        if quoted(rest)?.1 != " not found" {
            return Err(());
        }
        Origin::Rustc
    } else {
        let prefix = [
            "failed to run custom build command for ",
            "failed to load manifest for dependency ",
            "failed to load manifest for workspace member ",
            "failed to parse manifest at ",
            "failed to read ",
            "failed to download from ",
            "failed to download ",
            "failed to select a version for the requirement ",
            "failed to select a version for ",
        ]
        .into_iter()
        .find(|prefix| message.starts_with(*prefix));
        let Some(prefix) = prefix else {
            return Ok(None);
        };
        let (_, rest) = quoted(&message[prefix.len()..])?;
        if !rest.is_empty()
            && !(prefix.starts_with("failed to select a version") && rest == ".")
        {
            return Err(());
        }
        Origin::Cargo
    };
    if message.chars().any(char::is_control) {
        return Err(());
    }
    Ok(Some(Diagnostic {
        origin,
        message,
        causes: Vec::new(),
    }))
}

fn progress(line: &str) -> bool {
    line.starts_with(char::is_whitespace)
        && [
            "Updating ", "Downloading ", "Downloaded ", "Compiling ", "Checking ",
            "Fresh ", "Running ", "Blocking ", "Locking ", "Adding ", "Removing ",
            "Finished ",
        ]
        .iter()
        .any(|prefix| line.trim_start().starts_with(*prefix))
}

fn resolver_detail(message: &str, line: &str) -> bool {
    (message.starts_with("failed to select a version for ")
        || message.starts_with("no matching package named "))
        && [
            "candidate versions found which didn't match: ",
            "location searched: ",
            "searched package name: ",
            "required by package ",
            "versions that meet the requirements ",
            "package `",
            "previously selected package `",
            "all possible versions conflict with previously selected packages.",
            "failed to select a version for `",
            "perhaps you meant: ",
        ]
        .iter()
        .any(|prefix| line.starts_with(*prefix))
}

fn compiler_summary(line: &str) -> bool {
    line.strip_prefix("error: could not compile ")
        .and_then(|rest| quoted(rest).ok())
        .is_some_and(|(_, rest)| rest.contains(" due to ") && rest.contains("previous error"))
}

fn append(
    current: Diagnostic<'_>,
    keys: &mut BTreeSet<String>,
    output: &mut Vec<CanonicalDiagnostic>,
) {
    let mut description = current.message.to_owned();
    for cause in current.causes {
        description.push_str("; caused by: ");
        description.push_str(cause);
    }
    let description = crate::policy::redact_secret_like_content(&description).content;
    let canonical = match current.origin {
        Origin::Cargo => from_cargo(None, &description),
        Origin::Rustc => from_rustc(None, &description),
    };
    if keys.insert(canonical.layered_key().key) && output.len() < MAX_DIAGNOSTICS_PER_FAILURE {
        output.push(canonical);
    }
}

/// Preserve the existing coded-rustc path when it owns the report. A summary
/// alone is not a reusable error class. Causes qualify their owning error,
/// never create additional fingerprints or replace original source records.
pub(super) fn decode(text: &str) -> Result<Option<Vec<CanonicalDiagnostic>>, ()> {
    let mut current = None::<Diagnostic<'_>>;
    let mut errors = 0;
    let mut cause_pending = false;
    let mut keys = BTreeSet::new();
    let mut output = Vec::new();
    for line in super::bounded(text).lines().filter(|line| !line.trim().is_empty()) {
        if line.starts_with("error[E") {
            return Ok(None);
        }
        if line.chars().any(|ch| ch.is_control() && ch != '\t') {
            return if current.is_some() { Err(()) } else { Ok(None) };
        }
        if cause_pending {
            let cause = line.strip_prefix("  ").filter(|body| !body.trim().is_empty()).ok_or(())?;
            let Some(current) = current.as_mut() else {
                return Err(());
            };
            if current.causes.len() < MAX_CAUSES {
                current.causes.push(cause.trim());
            }
            cause_pending = false;
            continue;
        }
        if let Some(next) = diagnostic(line)? {
            errors += 1;
            if errors > MAX_ERRORS || text.len() > MAX_SCANNED_OUTPUT_BYTES {
                return Err(());
            }
            if let Some(previous) = current.replace(next) {
                append(previous, &mut keys, &mut output);
            }
            continue;
        }
        if progress(line) || line.starts_with("warning: ") {
            continue;
        }
        if current.is_none() {
            return Ok(None);
        }
        if line == "Caused by:" {
            cause_pending = true;
        } else if !(line.starts_with("  ")
            || line.starts_with('\t')
            || line.starts_with("note: ")
            || line.starts_with("help: ")
            || current.as_ref().is_some_and(|current| resolver_detail(current.message, line))
            || compiler_summary(line))
        {
            return Err(());
        }
    }
    if cause_pending || (current.is_some() && text.len() > MAX_SCANNED_OUTPUT_BYTES) {
        return Err(());
    }
    if let Some(current) = current {
        append(current, &mut keys, &mut output);
        Ok(Some(output))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cass_error_recall::{
        ToolOutput, failure_diagnostics, structured_error_diagnostics, symptom_line,
    };

    const BUILD: &str = "   Compiling widget v1.0.0 (/private/widget)\nerror: failed to run custom build command for `widget v1.0.0 (/private/widget)`\n\nCaused by:\n  process didn't exit successfully: `/private/widget/build-script-build` (exit status: 101)\n  --- stdout\n  error[E9999]: this is captured output, not a native diagnostic\n  --- stderr\n  missing native header\n";

    fn optimistic(text: &str) -> ToolOutput {
        ToolOutput { text: text.to_owned(), is_error: Some(false), exit_code: Some(0) }
    }

    #[test]
    fn dependency_manifest_download_and_build_errors_reach_import_and_interactive_recall() {
        for text in [
            "error: failed to get `dep` as a dependency of package `app v1.0.0`",
            "error: no matching package named `dep` found",
            "error: failed to load manifest for dependency `dep`",
            "error: failed to load manifest for workspace member `/private/dep`",
            "error: failed to parse manifest at `/private/Cargo.toml`",
            "error: failed to read `/private/Cargo.toml`",
            "error: failed to download from `https://example.invalid/crate`",
            "error: failed to download `dep v1.0.0`",
            "error: failed to select a version for the requirement `dep = ^1.0`",
            "error: failed to select a version for `dep`.",
            "error: could not find `Cargo.toml` in `/private` or any parent directory",
            BUILD,
        ] {
            let imported = failure_diagnostics(&optimistic(text));
            assert_eq!(imported.len(), 1, "{text}");
            assert_eq!(imported, structured_error_diagnostics(text));
            assert_eq!(imported[0].tool.as_str(), "cargo");
            assert_eq!(imported[0].canonical_code, None);
            assert!(optimistic(text).failed());
            assert!(!optimistic(text).succeeded());
        }
    }

    #[test]
    fn linker_errors_use_rustc_identity_without_fabricated_error_codes() {
        for text in [
            "error: linker `cc` not found",
            "error: linking with `cc` failed: exit status: 1\n  = note: undefined symbol\nerror: could not compile `app` (bin \"app\") due to 1 previous error",
        ] {
            let diagnostics = structured_error_diagnostics(text);
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].tool.as_str(), "rustc");
            assert_eq!(diagnostics[0].canonical_code, None);
            assert!(!optimistic(text).succeeded());
        }
    }

    #[test]
    fn source_paths_versions_and_backticked_arguments_do_not_fragment_templates() {
        let changed = BUILD.replace("widget", "other").replace("v1.0.0", "v2.3.4")
            .replace("/private", "/another");
        let first = structured_error_diagnostics(BUILD);
        assert_eq!(first.len(), 1);
        assert_eq!(first, structured_error_diagnostics(&changed));
        let symptom = symptom_line(&optimistic(BUILD)).expect("native symptom");
        assert!(!symptom.contains("/private"));
        assert!(!symptom.contains("E9999"));
        assert!(!format!("{first:?}").contains("missing native header"));
    }

    #[test]
    fn cause_specific_errors_remain_distinct() {
        let root = "error: failed to get `dep` as a dependency of package `app v1.0.0`";
        let network = format!("{root}\nCaused by:\n  download of registry configuration failed");
        let manifest = format!("{root}\nCaused by:\n  failed to parse manifest at `/private/Cargo.toml`");
        assert_ne!(structured_error_diagnostics(&network), structured_error_diagnostics(&manifest));
        assert_eq!(structured_error_diagnostics(&network).len(), 1);
    }

    #[test]
    fn native_resolver_explanations_do_not_hide_or_multiply_the_root_error() {
        let text = "error: failed to select a version for the requirement `dep = ^1.0`\ncandidate versions found which didn't match: 2.0.0\nlocation searched: crates.io index\nrequired by package `app v1.0.0 (/private/app)`\n";
        assert_eq!(structured_error_diagnostics(text).len(), 1);
        assert!(optimistic(text).failed());
        assert!(!optimistic(text).succeeded());
        let wrong_family = text.replace("failed to select a version for the requirement", "failed to read");
        assert!(decode(&wrong_family).is_err());
        assert!(decode(&format!("{text}Process exited with code 0")).is_err());
    }

    #[test]
    fn malformed_trailers_missing_causes_and_truncated_headers_are_not_salvaged() {
        for tail in ["Caused by:", "Caused by:\nnot indented", "Process exited with code 0",
            "{\"success\":true}", "error: failed to download `unfinished", "unrelated command output"] {
            let text = format!("{BUILD}\n{tail}");
            assert!(decode(&text).is_err(), "{tail}");
            assert!(failure_diagnostics(&optimistic(&text)).is_empty());
            assert!(structured_error_diagnostics(&text).is_empty());
            assert!(!optimistic(&text).succeeded());
        }
    }

    #[test]
    fn examples_json_and_unrelated_stdout_cannot_claim_cargo_errors() {
        for text in [format!("An example:\n{BUILD}"), format!("```text\n{BUILD}\n```"),
            serde_json::json!({"example": BUILD}).to_string(),
            "error: an unrelated shell failure".to_owned(),
            "   error: failed to download `dep`".to_owned(),
            "error: could not compile `app` due to 1 previous error".to_owned()] {
            assert!(decode(&text).expect("other producer").is_none(), "{text}");
        }
    }

    #[test]
    fn diagnostic_caps_do_not_skip_validation_of_remaining_errors() {
        let mut text = String::new();
        for name in ["alpha", "beta", "gamma", "delta", "epsilon"] {
            text.push_str(&format!("error: failed to read `Cargo.toml`\nCaused by:\n  {name} failure\n"));
        }
        assert_eq!(structured_error_diagnostics(&text).len(), MAX_DIAGNOSTICS_PER_FAILURE);
        text.push_str("error: failed to read `unterminated\n");
        assert!(decode(&text).is_err());
        assert!(structured_error_diagnostics(&text).is_empty());
    }

    #[test]
    fn limits_cover_original_utf8_bytes_and_every_root_diagnostic() {
        let short = "error: failed to read `Cargo.toml`\n";
        assert_eq!(structured_error_diagnostics(&short.repeat(MAX_ERRORS)).len(), 1);
        assert!(decode(&short.repeat(MAX_ERRORS + 1)).is_err());
        let oversized = format!("{BUILD}  {}", "界".repeat(MAX_SCANNED_OUTPUT_BYTES));
        assert!(decode(&oversized).is_err());
        assert!(!optimistic(&oversized).succeeded());
    }

    #[test]
    fn retained_causes_are_bounded_but_every_cause_requires_a_body() {
        let root = "error: failed to read `Cargo.toml`";
        let causes = "\nCaused by:\n  a stable cause".repeat(MAX_CAUSES);
        let text = format!("{root}{causes}");
        let longer = format!("{text}\nCaused by:\n  another valid cause");
        assert_eq!(structured_error_diagnostics(&text), structured_error_diagnostics(&longer));
        assert!(decode(&format!("{longer}\nCaused by:")).is_err());
    }

    #[test]
    fn status_progress_and_empty_stdout_still_need_independent_completion() {
        assert!(decode("    Finished dev profile in 1.0s\n").expect("progress").is_none());
        assert!(!ToolOutput::default().succeeded());
        assert!(optimistic("").succeeded());
        assert_eq!(failure_diagnostics(&optimistic("error[E0277]: missing trait"))[0]
            .layered_key().key, "rustc:E0277");
        assert_eq!(structured_error_diagnostics(&BUILD.replace('\n', "\r\n")),
            structured_error_diagnostics(BUILD));
    }

    #[test]
    fn secret_causes_are_screened_before_fingerprinting() {
        let secret = format!("ghp_{}", "Q".repeat(36));
        let text = format!("error: failed to read `Cargo.toml`\nCaused by:\n  auth failed api_key={secret}");
        let diagnostics = structured_error_diagnostics(&text);
        assert_eq!(diagnostics.len(), 1);
        assert!(!format!("{diagnostics:?}").to_ascii_lowercase().contains(&secret.to_ascii_lowercase()));
    }
}
