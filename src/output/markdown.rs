//! Markdown escaping helpers for renderer surfaces that include user memory.

/// Escape text that should be rendered as Markdown text, not interpreted as
/// Markdown syntax or raw HTML.
#[must_use]
pub(crate) fn escape_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\\' => output.push_str("\\\\"),
            '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.' | '!'
            | '|' | '~' => {
                output.push('\\');
                output.push(ch);
            }
            '\r' => {}
            _ => output.push(ch),
        }
    }
    output
}

/// Escape text for a Markdown heading, collapsing newlines so injected block
/// syntax cannot start a new section.
#[must_use]
pub(crate) fn escape_heading(input: &str) -> String {
    escape_text(&input.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Render trusted presentation of arbitrary text as a Markdown inline code
/// span, choosing a delimiter longer than any backtick run in the input.
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

/// Render arbitrary block content inside a Markdown code fence that cannot be
/// closed by the content's own backticks.
#[must_use]
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

    #[test]
    fn escape_text_neutralizes_markdown_links_backticks_and_html() {
        let escaped = escape_text("[x](javascript:alert(1)) `code` <script>");

        assert_eq!(
            escaped,
            "\\[x\\]\\(javascript:alert\\(1\\)\\) \\`code\\` &lt;script&gt;"
        );
    }

    #[test]
    fn escape_text_neutralizes_strikethrough_markers() {
        let escaped = escape_text("keep ~~do not strike~~ visible");

        assert_eq!(escaped, "keep \\~\\~do not strike\\~\\~ visible");
    }

    #[test]
    fn escape_heading_collapses_block_injection() {
        let escaped = escape_heading("Title\n## injected");

        assert_eq!(escaped, "Title \\#\\# injected");
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
}
