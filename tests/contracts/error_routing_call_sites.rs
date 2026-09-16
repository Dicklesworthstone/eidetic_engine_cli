//! Call-site guard for the `--format toon` error-routing contract (bd-oqrjn,
//! bd-sibling-error-writers-bool-mode-y46lf).
//!
//! WHAT THIS PINS, AND WHY NOTHING ELSE DID.
//!
//! Error routing has three layers and, before this file, only two were pinned:
//!
//!   1. THE RENDERER. `error_response_toon` / `render_toon_from_json` produce a
//!      valid toon envelope. Pinned by `tests/output_negative.rs`.
//!   2. THE ROUTING FUNCTION. `write_domain_error` maps a mode to a stream --
//!      machine renderers to stdout, human to stderr. Pinned by
//!      `error_render_routing_tests` in `src/cli/mod.rs`, but by calling the
//!      function DIRECTLY with an injected mode.
//!   3. THE CALL SITES. Whether a handler actually PASSES a renderer rather
//!      than a bool. Pinned by NOTHING.
//!
//! Layer 3 is where the defect lived: `From<bool>` maps `false` to `Human`, so
//! a surface passing `cli.wants_json()` sent failures to stderr as prose while
//! the matching success wrote toon to stdout. Over a thousand call sites were
//! migrated across four files, and until this guard existed, reverting any one
//! of them to `cli.wants_json()` broke no test.
//!
//! This reads source text rather than running commands, the same technique
//! `tests/contracts/context_delta_prior_unknown_repair_pinned.rs` uses to pin
//! emission sites in the same file. A runtime test would need a forced failure
//! per surface; this covers every call site in one pass and cannot go stale
//! against an unrun command.

#![allow(clippy::expect_used)]

type TestResult = Result<(), String>;

const SOURCES: &[(&str, &str)] = &[
    ("src/cli/mod.rs", include_str!("../../src/cli/mod.rs")),
    ("src/cli/mesh.rs", include_str!("../../src/cli/mesh.rs")),
    ("src/cli/team.rs", include_str!("../../src/cli/team.rs")),
    ("src/cli/share.rs", include_str!("../../src/cli/share.rs")),
];

/// Error writers whose mode argument must carry a renderer.
///
/// The index is the position of the mode in the argument list; the two
/// recorder writers take it first, everything else takes it second.
const WRITERS: &[(&str, usize)] = &[
    ("write_domain_error", 1),
    ("write_cancelled_error", 1),
    ("write_context_pack_error", 1),
    ("write_index_rebuild_error", 1),
    ("write_query_file_error", 1),
    ("write_record_persisted_event_error", 1),
    ("write_search_error", 1),
    ("write_recorder_event_usage_error", 0),
    ("write_recorder_event_validation_error", 0),
];

/// Mode expressions that are allowed to remain non-renderer, each with the
/// reason recorded at the call site in source.
///
/// This list may only SHRINK. Every entry was ruled on individually:
///   - `true` inside `if args.explain_performance`: that block's success path
///     writes raw performance output regardless of `--format`, so the literal
///     encodes "explain-performance implies machine-readable". Swapping it
///     would send prose to stderr for `--explain-performance` without `--json`.
///   - the `|| args.explain_performance` and `|| matches!(..)` compounds: a
///     mechanical swap drops the second disjunct, which is a behaviour change.
///   - `true` in `reject_unsupported_mermaid_format`: guarded by
///     `format == Mermaid`, which toon cannot reach, and `Renderer` has no
///     Mermaid variant, so migrating would replace JSON with prose.
const ALLOWED_NON_RENDERER_MODES: &[&str] = &[
    "true",
    "cli.wants_json() || args.explain_performance",
    "cli.wants_json() || matches!(renderer, output::Renderer::Json)",
];

/// A mode argument carries a renderer when it mentions one.
fn is_renderer_mode(mode: &str) -> bool {
    mode.contains("renderer") || mode.contains("ErrorRenderMode") || mode == "mode"
}

/// A production bool mode, spelled the way handlers spell it.
///
/// Deliberately matches `cli.wants_json()` and `args.json` rather than a bare
/// `wants_json` identifier. Test modules in these files bind a local
/// `wants_json` and pass it on purpose, and excluding them properly would need
/// `#[cfg(test)]` span detection -- which means brace matching, which on a
/// 97k-line file must skip string and comment literals to avoid running away.
/// A naive matcher did exactly that during this migration and silently marked
/// forty thousand lines as test code. Matching the call spelling instead needs
/// no span detection and still catches every realistic regression, because a
/// handler reintroducing a bool writes `cli.wants_json()`.
fn is_bool_mode(mode: &str) -> bool {
    mode.contains("cli.wants_json()") || mode.contains("args.json")
}

/// Split a call's argument list at depth-zero commas.
fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in args.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        if ch == ',' && depth == 0 {
            out.push(current.trim().to_owned());
            current.clear();
        } else {
            current.push(ch);
        }
    }
    out.push(current.trim().to_owned());
    out
}

/// Blank out `//` line comments before the argument list is split.
///
/// This must happen BEFORE splitting, not after. The ruled exclusion sites
/// carry a multi-line `//` note inside the argument list, and that note
/// contains commas -- "...by bd-oqrjn, deliberately: the...". A comma in a
/// comment sits at depth zero, so splitting first makes it an argument
/// separator and shifts the index of every argument after it. The guard then
/// reads a comment fragment as the mode and the real expression moves to an
/// index nobody checks, which is a silent miss rather than a loud one.
fn strip_line_comments(text: &str) -> String {
    text.lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Collapse whitespace so a multi-line argument compares as one string.
fn flatten(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every mode argument passed to a writer in one file, with its 1-based line.
fn mode_arguments(source: &str, writer: &str, index: usize) -> Vec<(usize, String)> {
    let needle = format!("{writer}(");
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(offset) = source[from..].find(&needle) {
        let call = from + offset;
        from = call + needle.len();
        // Skip a longer identifier ending in this name, e.g. `foo_write_domain_error(`.
        // The character immediately before the match must be checked WITHOUT
        // trimming: `return write_domain_error(` is separated by a space, and
        // trimming first makes `return` look like an identifier prefix, which
        // silently skipped roughly seven hundred real call sites.
        if source[..call]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
        {
            continue;
        }
        // Skip the definition itself.
        if source[..call].trim_end().ends_with("fn") {
            continue;
        }
        let open = call + needle.len();
        let mut depth = 1i32;
        let mut cursor = open;
        for ch in source[open..].chars() {
            match ch {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
            cursor += ch.len_utf8();
            if depth == 0 {
                break;
            }
        }
        if depth != 0 {
            continue;
        }
        let args = split_args(&strip_line_comments(
            &source[open..cursor.saturating_sub(1)],
        ));
        let Some(mode) = args.get(index) else {
            continue;
        };
        let line = source[..call].lines().count();
        found.push((line, flatten(mode)));
    }
    found
}

/// No error writer may receive a bare `wants_json` bool as its render mode.
///
/// A bool cannot express toon: `From<bool>` maps `false` to `Human`, so any
/// site passing one routes `--format toon` failures to stderr as prose.
#[test]
fn no_error_writer_call_site_passes_a_bool_mode() -> TestResult {
    let mut offenders = Vec::new();
    for (path, source) in SOURCES {
        for (writer, index) in WRITERS {
            for (line, mode) in mode_arguments(source, writer, *index) {
                if !is_bool_mode(&mode) {
                    continue;
                }
                if ALLOWED_NON_RENDERER_MODES.contains(&mode.as_str()) {
                    continue;
                }
                offenders.push(format!("  {path}:{line}  {writer}(.., {mode}, ..)"));
            }
        }
    }
    if offenders.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} error-writer call site(s) pass a bool render mode, so `--format toon` \
         failures render prose to stderr while the matching success renders toon \
         to stdout:\n{}\n\nPass the renderer instead (`cli.renderer()`, or \
         `cli.context_renderer()` on context surfaces). If a site must keep a \
         non-renderer mode, rule on it, comment the reason at the site, and add \
         the exact expression to ALLOWED_NON_RENDERER_MODES.",
        offenders.len(),
        offenders.join("\n"),
    ))
}

/// The guard must be looking at something.
///
/// Every assertion above passes trivially if the parser finds no call sites at
/// all -- a renamed writer, a moved file, or a broken `include_str!` would turn
/// this whole contract green while establishing nothing. That is the exact
/// failure shape this file exists to prevent, so it is checked here too.
#[test]
fn the_guard_actually_finds_call_sites() -> TestResult {
    let mut total = 0usize;
    let mut renderer_modes = 0usize;
    for (_, source) in SOURCES {
        for (writer, index) in WRITERS {
            for (_, mode) in mode_arguments(source, writer, *index) {
                total += 1;
                if is_renderer_mode(&mode) {
                    renderer_modes += 1;
                }
            }
        }
    }
    if total < 900 {
        return Err(format!(
            "error-writer call-site parser found only {total} sites across {} files; \
             over a thousand were migrated, so a number this low means the parser \
             is broken, not that the call sites are gone",
            SOURCES.len(),
        ));
    }
    if renderer_modes < 900 {
        return Err(format!(
            "only {renderer_modes} of {total} call sites carry a renderer mode; \
             the migration recorded over a thousand, so this is a parser fault \
             or a mass regression"
        ));
    }
    Ok(())
}
