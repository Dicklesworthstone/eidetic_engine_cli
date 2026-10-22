//! Bounded CPython exception-group trees for the ordinary command-failure lane.
//!
//! Structural margins, child ordinals/counts and the final closing border must
//! all agree before any leaf summary escapes. Group labels, frames, source and
//! locals are not fingerprints. The parent composes complete top-level causal
//! chains; notes, branch-local chains and formatter elisions remain unsupported
//! and keep the failure veto rather than exposing a partial key.

use super::{
    HEADER, MAX_FRAMES, MAX_LINES, MAX_SCANNED_OUTPUT_BYTES, exception_line, frame, repeated_frames,
};

pub(super) const ROOT_HEADER: &str = "  + Exception Group Traceback (most recent call last):";
const GROUP_HEADER: &str = "Exception Group Traceback (most recent call last):";
const MAX_GROUP_DEPTH: usize = 10;
const MAX_GROUP_NODES: usize = 256;

#[cfg(test)]
fn summaries(text: &str) -> Option<Vec<&str>> {
    let mut frames = 0;
    let mut summaries = Vec::new();
    append_summaries(text, &mut frames, &mut summaries)?;
    Some(summaries)
}

/// Charge frames against the enclosing chain, not a fresh per-tree allowance.
/// Commit leaves only after the entire tree and its trailing bytes validate.
pub(super) fn append_summaries<'a>(
    text: &'a str,
    frames: &mut usize,
    summaries: &mut Vec<&'a str>,
) -> Option<()> {
    if text.len() > MAX_SCANNED_OUTPUT_BYTES || text.lines().count() > MAX_LINES {
        return None;
    }
    let lines: Vec<_> = text.lines().collect();
    // Do not salvage a later well-formed tree after an earlier damaged or
    // differently indented group header. Ordinary stdout may precede the tree.
    let index = lines
        .iter()
        .position(|line| line.trim_start() == ROOT_HEADER.trim_start())?;
    if lines[index] != ROOT_HEADER || lines[..index].contains(&HEADER) {
        return None;
    }
    let mut parser = Parser {
        lines,
        index,
        frames: *frames,
        nodes: 0,
        leaves: Vec::new(),
    };
    parser.exception(1, true)?;
    if parser.leaves.is_empty()
        || parser.lines[parser.index..]
            .iter()
            .any(|line| !line.trim().is_empty())
    {
        return None;
    }
    *frames = parser.frames;
    summaries.extend(parser.leaves);
    Some(())
}

struct Parser<'a> {
    lines: Vec<&'a str>,
    index: usize,
    frames: usize,
    nodes: usize,
    leaves: Vec<&'a str>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.lines.get(self.index).copied()
    }

    fn expect(&mut self, expected: &str) -> Option<()> {
        if self.peek()? != expected {
            return None;
        }
        self.index += 1;
        Some(())
    }

    fn content(&self, depth: usize) -> Option<&'a str> {
        let margin = format!("{}| ", "  ".repeat(depth));
        self.peek()?.strip_prefix(margin.as_str())
    }

    fn stack(&mut self, depth: usize) -> Option<()> {
        let mut observed = 0;
        while let Some(line) = self.content(depth) {
            if frame(line) {
                observed += 1;
                self.frames += 1;
                if self.frames > MAX_FRAMES {
                    return None;
                }
            } else if observed > 0 && (line.starts_with("    ") || repeated_frames(line)) {
                // Source, carets and captured locals stay opaque. In particular
                // exception-looking code here is not a second leaf summary.
            } else {
                break;
            }
            self.index += 1;
        }
        (observed > 0).then_some(())
    }

    /// True means this child is a group whose descendant consumed the closing
    /// border. CPython omits redundant ancestor borders for a last-child group.
    fn exception(&mut self, depth: usize, root: bool) -> Option<bool> {
        if self.nodes == MAX_GROUP_NODES || depth > MAX_GROUP_DEPTH + 1 {
            return None;
        }
        self.nodes += 1;
        let explicit_group = if root {
            self.expect(ROOT_HEADER)?;
            self.stack(depth)?;
            true
        } else {
            match self.content(depth)? {
                GROUP_HEADER => {
                    self.index += 1;
                    self.stack(depth)?;
                    true
                }
                HEADER => {
                    self.index += 1;
                    self.stack(depth)?;
                    false
                }
                _ => false,
            }
        };
        let summary = self.content(depth)?;
        if !exception_line(summary) {
            return None;
        }
        self.index += 1;
        let first = format!(
            "{}+-+---------------- 1 ----------------",
            "  ".repeat(depth)
        );
        let group = explicit_group
            || summary.starts_with("ExceptionGroup:")
            || summary.starts_with("BaseExceptionGroup:")
            || self.peek() == Some(first.as_str());
        if !group {
            self.leaves.push(summary);
            return Some(false);
        }
        if depth > MAX_GROUP_DEPTH {
            return None;
        }
        let count = child_count(summary)?;
        if count > MAX_GROUP_NODES - self.nodes {
            return None;
        }
        self.expect(&first)?;
        for child in 1..=count {
            let closed = self.exception(depth + 1, false)?;
            if child < count {
                self.expect(&format!(
                    "{}+---------------- {} ----------------",
                    "  ".repeat(depth + 1),
                    child + 1,
                ))?;
            } else if !closed {
                self.expect(&format!(
                    "{}+------------------------------------",
                    "  ".repeat(depth + 1),
                ))?;
            }
        }
        Some(true)
    }
}

fn child_count(summary: &str) -> Option<usize> {
    let (_, suffix) = summary.rsplit_once(" (")?;
    let (number, rest) = suffix.split_once(' ')?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let count: usize = number.parse().ok()?;
    if count == 0 || count > MAX_GROUP_NODES || number != count.to_string() {
        return None;
    }
    let expected = if count == 1 {
        "sub-exception)"
    } else {
        "sub-exceptions)"
    };
    (rest == expected).then_some(count)
}

#[cfg(test)]
#[path = "exception_group_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "exception_group_store_tests.rs"]
mod store_tests;
