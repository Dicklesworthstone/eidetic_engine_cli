//! Markdown escaping helpers for renderer surfaces that include user memory.
//!
//! Bead bd-17c65.8.1 (H1) — spec-minimal escape policy.
//!
//! The 2026-05-10 walkthrough showed the pre-overhaul renderer escaped
//! every potentially-syntax character defensively: `v0.2.0` became
//! `v0\.2\.0`, `mem_01KR9V…` became `mem\_01KR9V…`. CommonMark does NOT
//! require those escapes in body context — `.` only matters at line
//! start preceded by digits (ordered-list marker) and `_` is NOT
//! emphasis inside a word (intra-word underscore rule).
//!
//! Aggressive escaping costs tokens (an LLM tokenizer treats `v0\.2\.0`
//! as a different sequence than `v0.2.0`) and degrades the legibility of
//! the rendered context pack.
//!
//! ## Policy (per character)
//!
//! - **`\`**: always escape (it's the escape character itself).
//! - **`<` `>` `&`**: always escape as HTML entities so raw HTML cannot
//!   bypass the renderer's safety boundary.
//! - **`` ` ``**: always escape — could open a code span anywhere.
//! - **`#`**: escape only at start of a line (ATX heading marker).
//! - **`+` `-`**: escape only at start of a line followed by space
//!   (unordered list marker). A run of three or more `-` markers that
//!   occupies the whole line is escaped as a thematic break marker.
//! - **`=`**: escape only when a run occupies the whole line, where it
//!   could turn the preceding paragraph into a setext heading.
//! - **`.` `)`** (after a digit): escape only at line start preceded by
//!   one-or-more digits (ordered list marker).
//! - **`!`**: escape only when followed by `[` (image link).
//! - **`[` `]`**: escape always (link / footnote / reference syntax can
//!   start anywhere; preserve defensive shape).
//! - **`*`**: escape only when adjacent to whitespace or punctuation on
//!   one side and a non-whitespace char on the other (emphasis-eligible).
//!   Inside `a*b*c` no escape.
//! - **`_`**: same emphasis-eligibility rule. Inside `mem_01ABC` no
//!   escape (CommonMark intra-word underscore rule).
//! - **`~`**: escape only when adjacent to another `~` (strikethrough
//!   pair `~~text~~`).
//! - **`|`**: never escape outside table cells; this helper is for body
//!   text. Table renderers should escape pipes inside cells themselves.
//! - **`{` `}`**: never escape — not CommonMark syntax.
//! - **`(` `)`** (other contexts): never escape — only relevant inside
//!   link destinations, which are not produced from arbitrary text by
//!   this helper.
//!
//! `escape_heading` collapses whitespace then defers to `escape_text`
//! since heading bodies have the same rules as paragraph bodies once
//! the heading marker is fixed at line start.

/// Escape text that should be rendered as Markdown text, not interpreted
/// as Markdown syntax or raw HTML.
///
/// See module doc for the per-character policy. The implementation walks
/// the input once, tracking whether we're at a line start and inspecting
/// the previous / next character for emphasis-eligibility decisions.
#[must_use]
pub(crate) fn escape_text(input: &str) -> String {
    // Allocate worst-case (every char doubles); typical inputs use far
    // less so this is a benign upper bound.
    let mut output = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut line_start = true;
    let mut digits_at_line_start: usize = 0;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let prev_ch = if i > 0 { Some(chars[i - 1]) } else { None };
        let next_ch = chars.get(i + 1).copied();
        match ch {
            '\\' => output.push_str("\\\\"),
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '`' => {
                output.push('\\');
                output.push('`');
            }
            // Newline resets line-start tracking.
            '\n' => {
                output.push('\n');
                line_start = true;
                digits_at_line_start = 0;
                i += 1;
                continue;
            }
            '\r' => {
                // Drop carriage returns; downstream renderers normalize.
                i += 1;
                continue;
            }
            '#' if line_start => {
                output.push('\\');
                output.push('#');
            }
            '=' if line_start && line_is_marker_run(&chars, i, '=', 1) => {
                output.push('\\');
                output.push('=');
            }
            '+' if line_start && next_is_space_or_eol(next_ch) => {
                output.push('\\');
                output.push('+');
            }
            '-' if line_start
                && (next_is_space_or_eol(next_ch) || line_is_marker_run(&chars, i, '-', 3)) =>
            {
                output.push('\\');
                output.push('-');
            }
            '.' if line_start && digits_at_line_start > 0 && next_is_space_or_eol(next_ch) => {
                output.push('\\');
                output.push('.');
            }
            ')' if line_start && digits_at_line_start > 0 && next_is_space_or_eol(next_ch) => {
                output.push('\\');
                output.push(')');
            }
            '!' if next_ch == Some('[') => {
                output.push('\\');
                output.push('!');
            }
            '[' | ']' => {
                output.push('\\');
                output.push(ch);
            }
            '*' | '_' => {
                if emphasis_eligible(prev_ch, next_ch) {
                    output.push('\\');
                    output.push(ch);
                } else {
                    output.push(ch);
                }
            }
            '~' if prev_ch == Some('~') || next_ch == Some('~') => {
                output.push('\\');
                output.push('~');
            }
            other => output.push(other),
        }
        // Track digit run at line start (for ordered-list detection on
        // the next `.` or `)`).
        if line_start {
            if ch.is_ascii_digit() {
                digits_at_line_start += 1;
            } else if !ch.is_ascii_whitespace() {
                // First non-digit, non-newline → line content has begun;
                // ordered-list-marker can no longer fire.
                line_start = false;
                digits_at_line_start = 0;
            }
        }
        i += 1;
    }
    output
}

/// Is this character adjacent to a word boundary on both sides, making
/// emphasis-marker interpretation valid per CommonMark?
///
/// Returns `true` when escape IS needed. Examples:
/// - `prev=' '`, `next='word'` → could open emphasis → escape.
/// - `prev='word'`, `next=' '` → could close emphasis → escape.
/// - `prev='m'`, `next='0'` (intra-word, like `mem_01ABC`) → not emphasis
///   per CommonMark → do not escape.
fn emphasis_eligible(prev: Option<char>, next: Option<char>) -> bool {
    let prev_is_word = prev.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let next_is_word = next.is_some_and(|c| c.is_alphanumeric() || c == '_');
    // Intra-word case: both neighbors are word characters → NOT emphasis.
    if prev_is_word && next_is_word {
        return false;
    }
    // Otherwise (at least one side is a boundary): conservative escape.
    true
}

fn next_is_space_or_eol(next: Option<char>) -> bool {
    match next {
        None => true,
        Some(ch) => ch == ' ' || ch == '\t' || ch == '\n',
    }
}

fn line_is_marker_run(chars: &[char], start: usize, marker: char, min_count: usize) -> bool {
    let mut count = 0;
    let mut index = start;
    while index < chars.len() {
        match chars[index] {
            ch if ch == marker => count += 1,
            ' ' | '\t' => {}
            '\n' | '\r' => break,
            _ => return false,
        }
        index += 1;
    }
    count >= min_count
}

/// Escape text for a Markdown heading, collapsing newlines so injected
/// block syntax cannot start a new section.
#[must_use]
pub(crate) fn escape_heading(input: &str) -> String {
    escape_text(&input.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Render trusted presentation of arbitrary text as a Markdown inline
/// code span, choosing a delimiter longer than any backtick run in the
/// input.
#[must_use]
pub(crate) fn inline_code(input: &str) -> String {
    let normalized = input.replace(['\r', '\n'], " ");
    let delimiter = "`".repeat(longest_backtick_run(&normalized).saturating_add(1).max(1));
    let needs_padding = normalized.starts_with('`')
        || normalized.ends_with('`')
        || normalized.starts_with(' ')
        || normalized.ends_with(' ');
    let padding = if needs_padding { " " } else { "" };
    format!("{delimiter}{padding}{normalized}{padding}{delimiter}")
}

/// Render arbitrary block content inside a Markdown code fence that
/// cannot be closed by the content's own backticks.
#[must_use]
#[cfg(test)]
pub(crate) fn fenced_code_block(content: &str) -> String {
    let delimiter = "`".repeat(longest_backtick_run(content).saturating_add(1).max(3));
    let mut output = String::with_capacity(content.len() + delimiter.len() * 2 + 4);
    output.push_str(&delimiter);
    output.push('\n');
    output.push_str(content);
    if !content.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&delimiter);
    output.push('\n');
    output
}

fn longest_backtick_run(input: &str) -> usize {
    let mut current = 0;
    let mut longest = 0;
    for ch in input.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::{escape_heading, escape_text, fenced_code_block, inline_code};

    #[derive(Debug, serde::Deserialize)]
    struct MarkdownCornerFixture {
        name: String,
        renderer: String,
        input: String,
        expected: String,
        rationale: String,
    }

    fn commonmark_event_signature(markdown: &str) -> Vec<String> {
        pulldown_cmark::Parser::new(markdown)
            .map(|event| format!("{event:?}"))
            .collect()
    }

    // -- Adversarial cases the old policy escaped — these still get
    // escaped because they ARE markdown syntax at the position they
    // appear in (link / inline-code / HTML).

    #[test]
    fn escape_text_neutralizes_markdown_links_backticks_and_html() {
        let escaped = escape_text("[x](javascript:alert(1)) `code` <script>");
        // [ and ] still escape; ( ) inside body text don't; ` always; < >
        // become HTML entities.
        assert_eq!(
            escaped,
            "\\[x\\](javascript:alert(1)) \\`code\\` &lt;script&gt;"
        );
    }

    #[test]
    fn escape_text_neutralizes_strikethrough_markers() {
        let escaped = escape_text("keep ~~do not strike~~ visible");
        assert_eq!(escaped, "keep \\~\\~do not strike\\~\\~ visible");
    }

    // -- Bead bd-17c65.8.1 (H1) regression: these used to over-escape.

    #[test]
    fn version_string_does_not_escape_dots() {
        // `v0.2.0` mid-text: dots are not list markers.
        assert_eq!(
            escape_text("prepare release v0.2.0"),
            "prepare release v0.2.0"
        );
    }

    #[test]
    fn identifier_with_underscores_passes_through_unescaped() {
        // CommonMark intra-word underscore rule.
        assert_eq!(
            escape_text("See mem_01KR9VVVWSE8 for details."),
            "See mem_01KR9VVVWSE8 for details."
        );
        assert_eq!(
            escape_text("policy.detector.value"),
            "policy.detector.value"
        );
        assert_eq!(escape_text("under_score_word"), "under_score_word");
    }

    #[test]
    fn hash_mid_line_not_escaped() {
        // ATX heading marker only fires at line start.
        assert_eq!(
            escape_text("Use #include in C code."),
            "Use #include in C code."
        );
    }

    #[test]
    fn hash_at_line_start_is_escaped() {
        // Real heading-like prefix must be neutralized.
        assert_eq!(
            escape_text("## Inline heading attempt"),
            "\\## Inline heading attempt"
        );
    }

    #[test]
    fn dot_after_digit_mid_line_not_escaped() {
        assert_eq!(
            escape_text("Items 1. apple 2. banana"),
            "Items 1. apple 2. banana"
        );
    }

    #[test]
    fn ordered_list_marker_at_line_start_is_escaped() {
        // The leading "1." would create a list; escape it.
        assert_eq!(escape_text("1. first item"), "1\\. first item");
        assert_eq!(escape_text("42. item"), "42\\. item");
    }

    #[test]
    fn list_dash_at_line_start_is_escaped() {
        assert_eq!(escape_text("- bullet"), "\\- bullet");
        assert_eq!(escape_text("+ bullet"), "\\+ bullet");
        // Mid-line dash/plus: no escape.
        assert_eq!(escape_text("a - b"), "a - b");
        assert_eq!(escape_text("1+2=3"), "1+2=3");
    }

    #[test]
    fn url_with_parens_in_body_passes_through() {
        assert_eq!(
            escape_text("See https://example.com/path(test) for details."),
            "See https://example.com/path(test) for details."
        );
    }

    #[test]
    fn brackets_still_escape_defensively() {
        // Square brackets are link/footnote syntax; escape for safety.
        assert_eq!(escape_text("note [1] section"), "note \\[1\\] section");
    }

    #[test]
    fn image_marker_escaped_when_followed_by_bracket() {
        assert_eq!(escape_text("![alt](url)"), "\\!\\[alt\\](url)");
        // Bare `!` mid-text doesn't escape.
        assert_eq!(escape_text("Wow! Great."), "Wow! Great.");
    }

    #[test]
    fn emphasis_markers_escape_at_word_boundaries_only() {
        // Boundary on both sides → escape.
        assert_eq!(escape_text("this *is* bold"), "this \\*is\\* bold");
        // Intra-word → no escape (CommonMark rule).
        assert_eq!(escape_text("a*b*c"), "a*b*c");
        // Underscore intra-word stays unescaped.
        assert_eq!(escape_text("foo_bar_baz"), "foo_bar_baz");
        // Underscore at boundary escapes.
        assert_eq!(escape_text("this _is_ italic"), "this \\_is\\_ italic");
    }

    #[test]
    fn backslash_is_always_escaped() {
        assert_eq!(escape_text("C:\\Users\\jane"), "C:\\\\Users\\\\jane");
    }

    #[test]
    fn html_entities_neutralize_raw_html() {
        assert_eq!(
            escape_text("<script>alert('x')</script>"),
            "&lt;script&gt;alert('x')&lt;/script&gt;"
        );
    }

    // -- Headings + structural helpers (regression: unchanged behavior).

    #[test]
    fn escape_heading_collapses_block_injection() {
        let escaped = escape_heading("Title\n## injected");
        // The newline is collapsed to a space; the surviving "##" is
        // mid-text (no longer at line start), so under the new policy
        // it does NOT escape.
        assert_eq!(escaped, "Title ## injected");
    }

    #[test]
    fn escape_heading_with_dots_in_body() {
        assert_eq!(
            escape_heading("Context Pack: prepare release v0.2.0"),
            "Context Pack: prepare release v0.2.0"
        );
    }

    #[test]
    fn inline_code_uses_delimiter_longer_than_embedded_backticks() {
        let rendered = inline_code("run `cmd` then ```stop```");
        assert_eq!(rendered, "```` run `cmd` then ```stop``` ````");
    }

    #[test]
    fn fenced_code_block_uses_delimiter_longer_than_embedded_fence() {
        let rendered = fenced_code_block("before\n```\n# injected\n```\nafter");
        assert_eq!(
            rendered,
            "````\nbefore\n```\n# injected\n```\nafter\n````\n"
        );
    }

    // -- Multi-line content keeps line-start tracking correct.

    #[test]
    fn line_start_tracking_resets_after_newline() {
        let escaped = escape_text("Run me first.\n1. Then bullet.");
        // First line: "Run me first." — `.` mid-text, no escape.
        // Second line: starts "1." → ordered marker → escape.
        assert_eq!(escaped, "Run me first.\n1\\. Then bullet.");
    }

    #[test]
    fn thematic_break_and_setext_lines_are_escaped() {
        assert_eq!(escape_text("Before\n---\nAfter"), "Before\n\\---\nAfter");
        assert_eq!(escape_text("Before\n===\nAfter"), "Before\n\\===\nAfter");
        assert_eq!(escape_text("--- not a rule"), "--- not a rule");
    }

    #[test]
    fn commonmark_corner_fixture_catalog_matches_renderer() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("markdown_corner_cases");
        let mut entries = std::fs::read_dir(&root)
            .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
            .map(|entry| {
                entry
                    .unwrap_or_else(|error| panic!("read fixture entry: {error}"))
                    .path()
            })
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        entries.sort();

        assert!(
            entries.len() >= 20,
            "expected at least 20 markdown corner fixtures, got {}",
            entries.len()
        );

        let mut names = std::collections::BTreeSet::new();
        for path in entries {
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let fixture: MarkdownCornerFixture = serde_json::from_str(&content)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            assert!(
                names.insert(fixture.name.clone()),
                "duplicate markdown corner fixture name: {}",
                fixture.name
            );
            assert!(
                !fixture.rationale.trim().is_empty(),
                "{} rationale must be non-empty",
                fixture.name
            );

            let rendered = match fixture.renderer.as_str() {
                "text" => escape_text(&fixture.input),
                "heading" => escape_heading(&fixture.input),
                "inline_code" => inline_code(&fixture.input),
                "fenced_code_block" => fenced_code_block(&fixture.input),
                other => panic!("{} unknown renderer {other}", fixture.name),
            };
            assert_eq!(
                rendered, fixture.expected,
                "{} rendered output drifted",
                fixture.name
            );
            assert_eq!(
                commonmark_event_signature(&rendered),
                commonmark_event_signature(&fixture.expected),
                "{} pulldown-cmark event stream drifted",
                fixture.name
            );
        }
    }
}
