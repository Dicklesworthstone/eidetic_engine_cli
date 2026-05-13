//! D8 contract: `OutputFormat::Markdown` and `OutputFormat::Mermaid`
//! share the same renderer (`output::Renderer::Markdown`).
//!
//! Owned by `bd-17c65.4.9` (D8). The cross-renderer parity matrix in
//! D8 is sized for the *renderer* set (7 entries), not the input
//! format set (8 entries). This compile-time assertion documents that
//! invariant: if a future change splits Mermaid into its own
//! `output::Renderer` variant, this test will fail to compile or
//! match, forcing the parity-matrix test framework to switch from
//! 7-renderer mode to 8-renderer mode in the same PR.
//!
//! Bead: `bd-17c65.4.9` (D8). Full parity matrix is the follow-up
//! `bd-17c65.4.9.1` (D8.1) once the canonical pack tree from
//! `bd-17c65.4` (D-series) stabilizes.

use ee::cli::OutputFormat;
use ee::output::Renderer;

#[test]
fn markdown_format_maps_to_markdown_renderer() {
    assert_eq!(OutputFormat::Markdown.to_renderer(), Renderer::Markdown);
}

#[test]
fn mermaid_format_maps_to_markdown_renderer() {
    assert_eq!(OutputFormat::Mermaid.to_renderer(), Renderer::Markdown);
}

#[test]
fn markdown_and_mermaid_share_renderer() {
    assert_eq!(
        OutputFormat::Markdown.to_renderer(),
        OutputFormat::Mermaid.to_renderer(),
        "OutputFormat::Markdown and OutputFormat::Mermaid must produce \
         the same output::Renderer. If you intentionally split them, \
         update bd-17c65.4.9 (D8) AND the renderer-parity-matrix test \
         framework to switch from 7-renderer to 8-renderer mode in the \
         same PR. Do not silently break this invariant.",
    );
}

#[test]
fn renderer_set_has_seven_distinct_variants() {
    // The output::Renderer enum is the truth about how many distinct
    // rendering paths exist. The parity matrix sizes its goldens to
    // this count. If a new variant is added, the matrix expands.
    let all: &[Renderer] = &[
        Renderer::Human,
        Renderer::Json,
        Renderer::Toon,
        Renderer::Jsonl,
        Renderer::Compact,
        Renderer::Hook,
        Renderer::Markdown,
    ];
    // Distinct count check: assert each variant compares not-equal to
    // every other variant. Adding a variant to the enum without
    // listing it here will break the test (the new variant won't be
    // in `all`, but compilation succeeds because enum is open) — to
    // catch that, we also assert exhaustive matching below.
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            if i == j {
                assert_eq!(a, b);
            } else {
                assert_ne!(a, b, "Renderer variants {a:?} and {b:?} compare equal");
            }
        }
    }

    // Exhaustive-match catches new variants without registry update.
    // Pattern adds a `#[non_exhaustive]`-style guard at the test level
    // even though the enum itself does not carry that attribute.
    fn assert_exhaustive(r: Renderer) -> &'static str {
        match r {
            Renderer::Human => "human",
            Renderer::Json => "json",
            Renderer::Toon => "toon",
            Renderer::Jsonl => "jsonl",
            Renderer::Compact => "compact",
            Renderer::Hook => "hook",
            Renderer::Markdown => "markdown",
        }
    }
    for r in all {
        let _ = assert_exhaustive(*r);
    }
}
