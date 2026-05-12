//! H2 contract test (eidetic_engine_cli bd-17c65.8.2).
//!
//! Uses pulldown-cmark as an independent CommonMark oracle for context-pack
//! Markdown. The renderer is still ee's own code; the parser checks that
//! generated Markdown remains parse-stable and that escape markers do not leak
//! into normal text nodes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::render_context_response_markdown;
use ee::pack::{
    ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackProvenance,
    PackSection, PackTrustSignal, TokenBudget, assemble_draft,
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd, html};
use uuid::Uuid;

type TestResult = Result<(), String>;

const FIXTURE_COUNT: usize = 120;

const QUERY_FRAGMENTS: &[&str] = &[
    "prepare release v0.2.0",
    "# heading-shaped query",
    "1. ordered-list-shaped query",
    "- bullet-shaped query",
    "policy.detector.value and mem_01KR9VVVWSE8",
    "link [label](https://example.invalid) and image ![alt](x)",
    "inline `code` and <script>alert(1)</script>",
    "a*b*c plus this _is_ emphasis-shaped",
    "OAuth refresh token rotation policy",
    "CommonMark table pipe a|b remains text",
];

const CONTENT_FRAGMENTS: &[&str] = &[
    "Run cargo fmt --check before release.",
    "Keep JSON stdout machine-only and diagnostics on stderr.",
    "Fence stress:\n```\n# injected heading\n```\nDone.",
    "Path-like text C:\\Users\\agent\\workspace stays inside code fences.",
    "Raw HTML sample <script>alert('x')</script> stays inert in code fences.",
    "Markdown image sample ![alt](javascript:alert(1)) stays inert in code fences.",
    "Ordered list sample\n1. first\n2. second",
    "Tilde ~~strike~~ and stars *emphasis* are fixture content.",
];

const WHY_FRAGMENTS: &[&str] = &[
    "matched release guardrail via lexical evidence",
    "# not a heading inside why text",
    "1. not an ordered list inside why text",
    "- not a bullet inside why text",
    "preserves mem_01KR9VVVWSE8 intra-word underscore behavior",
    "neutralizes [link](javascript:alert(1)) syntax",
    "neutralizes ![image](javascript:alert(1)) syntax",
    "keeps a*b*c but escapes standalone _emphasis_ shape",
];

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn unit(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("fixture score is inside unit interval")
}

fn provenance(seed: usize) -> PackProvenance {
    let uri = format!("file://tests/fixtures/markdown/{seed}.md#L{}", seed + 1);
    PackProvenance::new(
        ProvenanceUri::from_str(&uri).expect("fixture URI parses"),
        "markdown oracle fixture",
    )
    .expect("fixture provenance constructs")
}

fn generated_response(seed: usize) -> ContextResponse {
    let query = format!(
        "{} fixture {}",
        QUERY_FRAGMENTS[seed % QUERY_FRAGMENTS.len()],
        seed
    );
    let request = ContextRequest::from_query(query).expect("fixture query is valid");
    let budget = TokenBudget::new(4_000).expect("fixture budget is valid");
    let sections = PackSection::all();
    let mut candidates = Vec::new();

    for item_index in 0..3 {
        let content_index = (seed + item_index) % CONTENT_FRAGMENTS.len();
        let why_index = (seed * 3 + item_index) % WHY_FRAGMENTS.len();
        let candidate = PackCandidate::new(PackCandidateInput {
            memory_id: memory_id(((seed as u128) << 8) + item_index as u128 + 1),
            section: sections[(seed + item_index) % sections.len()],
            content: format!(
                "{}\ncase={seed}; item={item_index}",
                CONTENT_FRAGMENTS[content_index]
            ),
            estimated_tokens: 24 + item_index as u32,
            relevance: unit(0.95 - (item_index as f32 * 0.1)),
            utility: unit(0.70 - (item_index as f32 * 0.05)),
            provenance: vec![provenance(seed + item_index)],
            why: format!("{}; case={seed}", WHY_FRAGMENTS[why_index]),
        })
        .expect("fixture candidate constructs")
        .with_trust_signal(PackTrustSignal::new(
            TrustClass::HumanExplicit,
            Some("markdown-oracle".to_owned()),
        ));
        candidates.push(candidate);
    }

    let draft =
        assemble_draft(&request.query, budget, candidates).expect("fixture draft assembles");
    ContextResponse::new(request, draft, Vec::new()).expect("fixture response constructs")
}

fn event_signature(markdown: &str) -> Vec<String> {
    Parser::new(markdown)
        .map(|event| format!("{event:?}"))
        .collect()
}

fn html_render(markdown: &str) -> String {
    let mut output = String::new();
    html::push_html(&mut output, Parser::new(markdown));
    output
}

fn assert_text_events_have_no_escape_backslashes(markdown: &str, label: &str) -> TestResult {
    let mut code_block_depth = 0_u32;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                code_block_depth = code_block_depth.saturating_add(1);
            }
            Event::End(TagEnd::CodeBlock) => {
                code_block_depth = code_block_depth.saturating_sub(1);
            }
            Event::Text(text) if code_block_depth == 0 && text.contains('\\') => {
                return Err(format!(
                    "{label}: CommonMark text event leaked a markdown escape backslash: {text:?}"
                ));
            }
            Event::Html(html) if !html.trim_start().starts_with("<!-- pack.") => {
                return Err(format!(
                    "{label}: renderer emitted non-pack raw HTML event: {html:?}"
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

#[test]
fn context_markdown_commonmark_roundtrip_is_stable_for_generated_fixtures() -> TestResult {
    for seed in 0..FIXTURE_COUNT {
        let response = generated_response(seed);
        let first_markdown = render_context_response_markdown(&response);
        let second_markdown = render_context_response_markdown(&response);

        if first_markdown != second_markdown {
            return Err(format!(
                "fixture {seed}: context markdown render is not byte-stable"
            ));
        }

        let first_signature = event_signature(&first_markdown);
        let second_signature = event_signature(&second_markdown);
        if first_signature != second_signature {
            return Err(format!(
                "fixture {seed}: pulldown-cmark event stream changed across identical renders"
            ));
        }

        let first_html = html_render(&first_markdown);
        let second_html = html_render(&second_markdown);
        if first_html != second_html {
            return Err(format!(
                "fixture {seed}: pulldown-cmark HTML projection changed across identical renders"
            ));
        }

        assert_text_events_have_no_escape_backslashes(&first_markdown, &format!("fixture {seed}"))?;
    }
    Ok(())
}

#[test]
fn markdown_oracle_fixture_count_covers_hundred_plus_cases() -> TestResult {
    if FIXTURE_COUNT < 100 {
        return Err(format!(
            "H2 requires 100+ generated fixtures; configured {FIXTURE_COUNT}"
        ));
    }
    Ok(())
}
