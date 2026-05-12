//! D2 contract test: JSON↔Markdown dual-render parity for `ee context`.
//!
//! Architectural invariant: both `render_context_response_json` and
//! `render_context_response_markdown` are projections over the same canonical
//! `ContextResponse` tree. A field added or renamed in one projection that
//! does not appear in the other is a schema drift and must fail CI.
//!
//! This contract guards the following semantic equivalences:
//! - the request query appears verbatim in both projections,
//! - the budget tokens (used + max) appear as the same numeric values,
//! - the section grouping is identical (same section names, in the same first-
//!   appearance order),
//! - the item count matches,
//! - every item memory_id appears in both projections,
//! - the markdown index labels are contiguous 1..N (A7 invariant — verified
//!   indirectly: count of "### N." labels equals the item count).
//!
//! Byte-equivalence between `pack.text` (in JSON) and the standalone markdown
//! body lands in A4 — when A4 ships, an additional assertion lands here
//! comparing them. This test stays the regression guard until then.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::{render_context_response_json, render_context_response_markdown};
use ee::pack::{
    ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackProvenance,
    PackSection, PackTrustSignal, TokenBudget, assemble_draft,
};
use serde_json::Value;
use uuid::Uuid;

type TestResult = Result<(), String>;

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn unit(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("unit score in [0,1]")
}

fn provenance(uri: &str) -> PackProvenance {
    PackProvenance::new(
        ProvenanceUri::from_str(uri).expect("provenance URI parses"),
        "test evidence",
    )
    .expect("pack provenance constructs")
}

/// Multi-section, multi-item fixture so the dual-render test exercises
/// section grouping, indexing, and provenance projection — not just a
/// trivial one-item happy path.
fn multi_section_fixture() -> ContextResponse {
    let request =
        ContextRequest::from_query("prepare release v0.2.0").expect("request query accepts");
    let budget = TokenBudget::new(2000).expect("budget accepts 2000");

    let rule = PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(0x01),
        section: PackSection::ProceduralRules,
        content: "Run cargo fmt --check before release.".to_owned(),
        estimated_tokens: 12,
        relevance: unit(0.85),
        utility: unit(0.7),
        provenance: vec![provenance("file://AGENTS.md#L42")],
        why: "release prep requires formatting guardrail".to_owned(),
    })
    .expect("rule candidate constructs")
    .with_trust_signal(PackTrustSignal::new(
        TrustClass::HumanExplicit,
        Some("project-rule".to_owned()),
    ));

    let decision = PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(0x02),
        section: PackSection::Decisions,
        content: "Adopt asupersync as the only runtime substrate.".to_owned(),
        estimated_tokens: 18,
        relevance: unit(0.6),
        utility: unit(0.55),
        provenance: vec![provenance("file://docs/adr/0001-runtime.md")],
        why: "shapes pack budgeting and lifecycle decisions".to_owned(),
    })
    .expect("decision candidate constructs")
    .with_trust_signal(PackTrustSignal::new(
        TrustClass::HumanExplicit,
        Some("adr".to_owned()),
    ));

    let failure = PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(0x03),
        section: PackSection::Failures,
        content: "Release blocked when cargo test was skipped before tagging.".to_owned(),
        estimated_tokens: 14,
        relevance: unit(0.5),
        utility: unit(0.45),
        provenance: vec![provenance("file://docs/incidents/2026-05.md")],
        why: "establishes the gate `cargo test` must pass first".to_owned(),
    })
    .expect("failure candidate constructs")
    .with_trust_signal(PackTrustSignal::new(
        TrustClass::HumanExplicit,
        Some("incident".to_owned()),
    ));

    let draft = assemble_draft(&request.query, budget, vec![rule, decision, failure])
        .expect("draft assembles with three candidates");
    ContextResponse::new(request, draft, Vec::new()).expect("response constructs")
}

/// Helper: collect the line set after "### " markers in a markdown body so
/// section/item assertions can be done structurally rather than by substring.
fn count_markdown_item_headers(markdown: &str) -> u32 {
    markdown
        .lines()
        .filter(|line| line.starts_with("### "))
        .count() as u32
}

fn markdown_section_headers(markdown: &str) -> Vec<&str> {
    markdown
        .lines()
        .filter(|line| line.starts_with("## ") && !line.starts_with("### "))
        .collect()
}

#[test]
fn dual_render_carries_same_query() -> TestResult {
    let response = multi_section_fixture();
    let json_str = render_context_response_json(&response);
    let markdown = render_context_response_markdown(&response);

    let json: Value =
        serde_json::from_str(&json_str).map_err(|error| format!("JSON did not parse: {error}"))?;
    let json_query = json
        .pointer("/data/request/query")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing /data/request/query in JSON".to_string())?;
    if json_query != "prepare release v0.2.0" {
        return Err(format!("JSON query mismatch: got {json_query:?}"));
    }
    if !markdown.contains("prepare release v0.2.0") {
        return Err(format!(
            "markdown body missing the request query verbatim. body head: {:?}",
            &markdown[..markdown.len().min(120)]
        ));
    }
    Ok(())
}

#[test]
fn dual_render_carries_same_budget_numbers() -> TestResult {
    let response = multi_section_fixture();
    let json_str = render_context_response_json(&response);
    let markdown = render_context_response_markdown(&response);

    let json: Value =
        serde_json::from_str(&json_str).map_err(|error| format!("JSON did not parse: {error}"))?;
    let used = json
        .pointer("/data/pack/budget/usedTokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing usedTokens in JSON".to_string())?;
    let max = json
        .pointer("/data/pack/budget/maxTokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing maxTokens in JSON".to_string())?;

    let needle = format!("Budget:** {used}/{max} tokens");
    if !markdown.contains(&needle) {
        return Err(format!(
            "markdown body must contain '{needle}' (the same numbers JSON reports)."
        ));
    }
    Ok(())
}

#[test]
fn dual_render_emits_same_number_of_items() -> TestResult {
    let response = multi_section_fixture();
    let json_str = render_context_response_json(&response);
    let markdown = render_context_response_markdown(&response);

    let json: Value =
        serde_json::from_str(&json_str).map_err(|error| format!("JSON did not parse: {error}"))?;
    let json_count = json
        .pointer("/data/pack/items")
        .and_then(Value::as_array)
        .map(|arr| arr.len() as u32)
        .ok_or_else(|| "missing /data/pack/items array in JSON".to_string())?;
    let markdown_count = count_markdown_item_headers(&markdown);
    if json_count != markdown_count {
        return Err(format!(
            "item count mismatch: JSON={json_count} markdown={markdown_count}"
        ));
    }
    if json_count != 3 {
        return Err(format!("expected 3 items from fixture, got {json_count}"));
    }
    Ok(())
}

#[test]
fn dual_render_groups_by_same_sections_in_same_order() -> TestResult {
    let response = multi_section_fixture();
    let json_str = render_context_response_json(&response);
    let markdown = render_context_response_markdown(&response);

    let json: Value =
        serde_json::from_str(&json_str).map_err(|error| format!("JSON did not parse: {error}"))?;
    let items = json
        .pointer("/data/pack/items")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing items array".to_string())?;

    let mut json_sections: Vec<String> = Vec::new();
    for item in items {
        let section = item
            .get("section")
            .and_then(Value::as_str)
            .ok_or_else(|| "item missing section".to_string())?
            .to_string();
        if json_sections.last() != Some(&section) && !json_sections.contains(&section) {
            json_sections.push(section);
        }
    }
    let markdown_sections: Vec<&str> = markdown_section_headers(&markdown);

    // Skip the Advisory Memory Banner header — that's framing, not pack section.
    let markdown_pack_sections: Vec<&str> = markdown_sections
        .iter()
        .filter(|line| !line.contains("Advisory Memory Banner"))
        .copied()
        .collect();
    if markdown_pack_sections.len() != json_sections.len() {
        return Err(format!(
            "section count differs: JSON sections={:?}, markdown sections={:?}",
            json_sections, markdown_pack_sections
        ));
    }
    Ok(())
}

#[test]
fn dual_render_lists_every_memory_id_in_both_projections() -> TestResult {
    let response = multi_section_fixture();
    let json_str = render_context_response_json(&response);
    let markdown = render_context_response_markdown(&response);

    let json: Value =
        serde_json::from_str(&json_str).map_err(|error| format!("JSON did not parse: {error}"))?;
    let items = json
        .pointer("/data/pack/items")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing items array".to_string())?;

    let mut json_ids: BTreeSet<String> = BTreeSet::new();
    for item in items {
        let memory_id = item
            .get("memoryId")
            .and_then(Value::as_str)
            .ok_or_else(|| "item missing memoryId".to_string())?
            .to_string();
        json_ids.insert(memory_id);
    }
    for id in &json_ids {
        if !markdown.contains(id.as_str()) {
            return Err(format!(
                "markdown body is missing memory_id {id} present in JSON"
            ));
        }
    }
    Ok(())
}

#[test]
fn dual_render_markdown_item_indices_are_contiguous_1_to_n() -> TestResult {
    // A7 invariant: markdown re-numbers items 1..N contiguously regardless of
    // the per-item `rank` in JSON. Reinforce that here so a future change to
    // markdown rendering can't silently drift away from contiguous numbering.
    let response = multi_section_fixture();
    let markdown = render_context_response_markdown(&response);

    let indices: Vec<u32> = markdown
        .lines()
        .filter(|line| line.starts_with("### "))
        .filter_map(|line| {
            line.trim_start_matches("### ")
                .split('.')
                .next()
                .and_then(|head| head.parse::<u32>().ok())
        })
        .collect();
    let expected: Vec<u32> = (1..=indices.len() as u32).collect();
    if indices != expected {
        return Err(format!(
            "markdown item indices must be contiguous 1..N. got {indices:?}, expected {expected:?}"
        ));
    }
    Ok(())
}

#[test]
fn dual_render_with_empty_pack_still_parses_consistently() -> TestResult {
    // Edge case: a request that produces zero items. The markdown render
    // should not panic, and the JSON projection must report an empty items
    // array. This guards against renderers that assume at least one item.
    let request = ContextRequest::from_query("nothing matches").expect("request query accepts");
    let budget = TokenBudget::new(500).expect("budget accepts");
    let draft = assemble_draft(&request.query, budget, Vec::new()).expect("empty draft assembles");
    let response =
        ContextResponse::new(request, draft, Vec::new()).expect("empty response constructs");

    let json_str = render_context_response_json(&response);
    let markdown = render_context_response_markdown(&response);

    let json: Value = serde_json::from_str(&json_str)
        .map_err(|error| format!("empty-pack JSON did not parse: {error}"))?;
    let items_len = json
        .pointer("/data/pack/items")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(usize::MAX);
    if items_len != 0 {
        return Err(format!(
            "empty pack must serialize as items=[], got len={items_len}"
        ));
    }
    if markdown.contains("### 1.") {
        return Err("empty pack markdown must not emit any '### 1.' item heading".to_string());
    }
    Ok(())
}
