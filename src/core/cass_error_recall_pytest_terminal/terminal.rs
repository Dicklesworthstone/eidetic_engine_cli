//! Interpret styling, not terminal execution. Source evidence stays untouched.
//!
//! Only bounded CSI SGR color/emphasis sequences may disappear. Cursor moves,
//! erasure, OSC links, backspace and bare carriage returns are not formatting:
//! they can change the visible result and must not become a successful proof.

use std::borrow::Cow;

const MAX_SGR_PARAMETERS: usize = 64;

fn controls_are_literal(text: &str) -> bool {
    text.char_indices().all(|(offset, ch)| {
        !ch.is_control()
            || matches!(ch, '\n' | '\t')
            || (ch == '\r' && text.as_bytes().get(offset + 1) == Some(&b'\n'))
    })
}

pub(super) fn normalize(text: &str) -> Result<Cow<'_, str>, ()> {
    if text.len() > super::MAX_SCANNED_OUTPUT_BYTES {
        return Err(());
    }
    if !text.contains('\u{1b}') {
        return controls_are_literal(text)
            .then_some(Cow::Borrowed(text))
            .ok_or(());
    }
    let mut visible = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('\u{1b}') {
        visible.push_str(&rest[..start]);
        let parameters = rest[start..].strip_prefix("\u{1b}[").ok_or(())?;
        let end = parameters.find('m').ok_or(())?;
        if end > MAX_SGR_PARAMETERS {
            return Err(());
        }
        let codes = &parameters[..end];
        if !codes.bytes().all(|byte| byte.is_ascii_digit() || byte == b';')
            || codes
                .split(';')
                .any(|code| !code.is_empty() && code.parse::<u8>().is_err())
        {
            return Err(());
        }
        rest = &parameters[end + 1..];
    }
    visible.push_str(rest);
    controls_are_literal(&visible)
        .then_some(Cow::Owned(visible))
        .ok_or(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_crlf_text_stay_borrowed_and_byte_identical() {
        for text in ["plain λ", "first\r\nsecond\tcolumn\n"] {
            assert!(matches!(normalize(text), Ok(Cow::Borrowed(value)) if value == text));
        }
    }

    #[test]
    fn style_sequences_preserve_literal_text_without_concatenating_lines() {
        let styled = "\u{1b}[1mλ\u{1b}[0m\n\u{1b}[38;5;123mPASSED\u{1b}[m";
        assert_eq!(normalize(styled).unwrap(), "λ\nPASSED");
        assert_eq!(
            normalize("\u{1b}[38;2;2;3;4mRGB\u{1b}[0m").unwrap(),
            "RGB"
        );
    }

    #[test]
    fn terminal_execution_and_malformed_styles_never_disappear() {
        for text in [
            "FAILED\rPASSED",
            "FAILED\u{8}PASSED",
            "\u{1b}[2JPASSED",
            "\u{1b}[1APASSED",
            "\u{1b}]8;;url\u{7}PASSED",
            "\u{1b}[31",
            "\u{1b}[9999mPASSED",
            "\u{9b}31mPASSED",
        ] {
            assert!(normalize(text).is_err(), "{text:?}");
        }
        assert!(normalize(&format!("\u{1b}[{}m", "0;".repeat(33))).is_err());
    }
}
