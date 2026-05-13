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
//! - `pack.text` in JSON is byte-identical to the standalone markdown body
//!   (A4 invariant).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::{
    ContextJsonRenderOptions, render_context_response_compact, render_context_response_hook,
    render_context_response_human, render_context_response_json,
    render_context_response_json_with_options, render_context_response_jsonl,
    render_context_response_markdown, render_context_response_mermaid,
    render_context_response_toon,
};
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

    let mut draft = assemble_draft(&request.query, budget, vec![rule, decision, failure])
        .expect("draft assembles with three candidates");
    draft.hash = Some("blake3:fixture-context-pack".to_owned());
    ContextResponse::new(request, draft, Vec::new()).expect("response constructs")
}

fn pack_hash(response: &ContextResponse) -> &str {
    response
        .data
        .pack
        .hash
        .as_deref()
        .expect("fixture carries pack hash")
}

fn canonical_memory_ids(response: &ContextResponse) -> Vec<String> {
    response
        .data
        .pack
        .items
        .iter()
        .map(|item| item.memory_id.to_string())
        .collect()
}

fn ensure_toon_matches_json(json: &str, toon: &str, context: &str) -> TestResult {
    let expected_json = serde_json::from_str::<Value>(json)
        .map_err(|error| format!("{context}: JSON should parse: {error}"))?;
    let expected = Value::from(toon::JsonValue::from(expected_json));
    let decoded = toon::try_decode(toon, None)
        .map_err(|error| format!("{context}: TOON should decode: {error}"))?;
    let actual = Value::from(decoded);
    if actual != expected {
        return Err(format!(
            "{context}: decoded TOON drifted from canonical JSON.\nexpected: {expected}\nactual: {actual}"
        ));
    }
    Ok(())
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

fn render_all_context_formats(response: &ContextResponse) -> [(&'static str, String); 8] {
    [
        ("human", render_context_response_human(response)),
        ("json", render_context_response_json(response)),
        ("toon", render_context_response_toon(response)),
        ("jsonl", render_context_response_jsonl(response)),
        ("compact", render_context_response_compact(response)),
        ("hook", render_context_response_hook(response)),
        ("markdown", render_context_response_markdown(response)),
        ("mermaid", render_context_response_mermaid(response)),
    ]
}

#[test]
fn all_context_format_renderers_carry_pack_hash_metadata() -> TestResult {
    let response = multi_section_fixture();
    let hash = pack_hash(&response);

    for (format, rendered) in render_all_context_formats(&response) {
        if !rendered.contains(hash) {
            return Err(format!(
                "{format} context renderer must carry pack hash {hash:?}; output head: {:?}",
                &rendered[..rendered.len().min(160)]
            ));
        }
    }
    Ok(())
}

#[test]
fn all_context_format_renderers_are_deterministic_for_fixed_pack() -> TestResult {
    let response = multi_section_fixture();
    let first = render_all_context_formats(&response);
    let second = render_all_context_formats(&response);

    for ((first_format, first_rendered), (second_format, second_rendered)) in
        first.iter().zip(second.iter())
    {
        if first_format != second_format {
            return Err(format!(
                "renderer order drifted between deterministic passes: {first_format} vs {second_format}"
            ));
        }
        if first_rendered != second_rendered {
            return Err(format!(
                "{first_format} renderer is not byte-stable for the fixed canonical pack"
            ));
        }
    }
    Ok(())
}

#[test]
fn jsonl_context_renderer_emits_parseable_header_items_footer() -> TestResult {
    let response = multi_section_fixture();
    let rendered = render_context_response_jsonl(&response);
    let lines = rendered.lines().collect::<Vec<_>>();
    let expected_len = response.data.pack.items.len() + 2;
    if lines.len() != expected_len {
        return Err(format!(
            "jsonl renderer must emit header + one line per item + footer; got {} expected {expected_len}",
            lines.len()
        ));
    }

    let parsed = lines
        .iter()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .map_err(|error| format!("jsonl line did not parse: {line}\n{error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let header_schema = parsed[0]
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| "jsonl header missing schema".to_string())?;
    if header_schema != "ee.context.jsonl.header.v1" {
        return Err(format!("unexpected jsonl header schema: {header_schema}"));
    }
    if parsed[0].get("packHash").and_then(Value::as_str) != Some(pack_hash(&response)) {
        return Err("jsonl header packHash does not match canonical pack hash".to_string());
    }
    if parsed[0].get("query").and_then(Value::as_str) != Some(response.data.request.query.as_str())
    {
        return Err("jsonl header query does not match canonical request query".to_string());
    }
    if parsed[0].get("itemCount").and_then(Value::as_u64)
        != Some(response.data.pack.items.len() as u64)
    {
        return Err("jsonl header itemCount does not match canonical item count".to_string());
    }

    let mut item_ids = Vec::new();
    for (actual, expected) in parsed[1..parsed.len() - 1]
        .iter()
        .zip(&response.data.pack.items)
    {
        let schema = actual
            .get("schema")
            .and_then(Value::as_str)
            .ok_or_else(|| "jsonl item missing schema".to_string())?;
        if schema != "ee.context.jsonl.item.v1" {
            return Err(format!("unexpected jsonl item schema: {schema}"));
        }
        let memory_id = actual
            .get("memoryId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "jsonl item missing memoryId".to_string())?;
        if actual.get("content").and_then(Value::as_str) != Some(expected.content.as_str()) {
            return Err(format!(
                "jsonl content drifted for canonical memory id {}",
                expected.memory_id
            ));
        }
        if actual.get("estimatedTokens").and_then(Value::as_u64)
            != Some(u64::from(expected.estimated_tokens))
        {
            return Err(format!(
                "jsonl token count drifted for canonical memory id {}",
                expected.memory_id
            ));
        }
        if actual.get("why").and_then(Value::as_str) != Some(expected.why.as_str()) {
            return Err(format!(
                "jsonl why drifted for canonical memory id {}",
                expected.memory_id
            ));
        }
        item_ids.push(memory_id);
    }
    if item_ids != canonical_memory_ids(&response) {
        return Err(format!(
            "jsonl item ids drifted from canonical pack order: got {item_ids:?}"
        ));
    }

    let footer = parsed
        .last()
        .ok_or_else(|| "jsonl parser produced no footer".to_string())?;
    if footer.get("schema").and_then(Value::as_str) != Some("ee.context.jsonl.footer.v1") {
        return Err(format!("unexpected jsonl footer: {footer:?}"));
    }
    Ok(())
}

#[test]
fn toon_context_renderer_decodes_to_canonical_json() -> TestResult {
    let response = multi_section_fixture();
    let json = render_context_response_json(&response);
    let toon = render_context_response_toon(&response);
    ensure_toon_matches_json(&json, &toon, "context TOON renderer")
}

#[test]
fn hook_context_renderer_emits_agent_hook_schema() -> TestResult {
    let response = multi_section_fixture();
    let rendered = render_context_response_hook(&response);
    let json: Value =
        serde_json::from_str(&rendered).map_err(|error| format!("hook JSON invalid: {error}"))?;

    if json.get("schema").and_then(Value::as_str) != Some("ee.hook.context_pack.v1") {
        return Err(format!("unexpected hook schema: {json:?}"));
    }
    if json.get("pack_id").and_then(Value::as_str) != Some(pack_hash(&response)) {
        return Err("hook pack_id must equal canonical pack hash".to_string());
    }
    let items = json
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "hook JSON missing items array".to_string())?;
    if items.len() != response.data.pack.items.len() {
        return Err(format!(
            "hook item count mismatch: got {}, expected {}",
            items.len(),
            response.data.pack.items.len()
        ));
    }
    for (actual, expected) in items.iter().zip(&response.data.pack.items) {
        let expected_id = expected.memory_id.to_string();
        if actual.get("id").and_then(Value::as_str) != Some(expected_id.as_str()) {
            return Err(format!(
                "hook item id drifted from canonical memory id {}",
                expected.memory_id
            ));
        }
        if actual.get("content").and_then(Value::as_str) != Some(expected.content.as_str()) {
            return Err(format!(
                "hook item content drifted for memory id {}",
                expected.memory_id
            ));
        }
        if actual.get("tokens").and_then(Value::as_u64)
            != Some(u64::from(expected.estimated_tokens))
        {
            return Err(format!(
                "hook item token count drifted for memory id {}",
                expected.memory_id
            ));
        }
    }
    Ok(())
}

#[test]
fn compact_context_renderer_is_single_line_canonical_summary() -> TestResult {
    let response = multi_section_fixture();
    let rendered = render_context_response_compact(&response);
    if rendered.contains('\n') || rendered.contains('\r') {
        return Err(format!(
            "compact renderer must be single-line, got {rendered:?}"
        ));
    }
    let fields = rendered.split('\t').collect::<Vec<_>>();
    if fields.len() != 4 {
        return Err(format!(
            "compact renderer must emit 4 tab-separated fields, got {fields:?}"
        ));
    }
    if fields[0] != response.data.request.query {
        return Err(format!("compact query drifted: {fields:?}"));
    }
    let expected_budget = format!(
        "{}/{}",
        response.data.pack.items.len(),
        response.data.pack.budget.max_tokens()
    );
    if fields[1] != expected_budget {
        return Err(format!(
            "compact item/budget field drifted: got {:?}, expected {expected_budget:?}",
            fields[1]
        ));
    }
    for id in canonical_memory_ids(&response).into_iter().take(3) {
        if !fields[2].contains(&id) {
            return Err(format!("compact top-id field missing {id}: {fields:?}"));
        }
    }
    if fields[3] != pack_hash(&response) {
        return Err(format!("compact pack hash drifted: {fields:?}"));
    }
    Ok(())
}

#[test]
fn mermaid_context_renderer_projects_pack_items_and_provenance() -> TestResult {
    let response = multi_section_fixture();
    let rendered = render_context_response_mermaid(&response);
    if !rendered.starts_with("%% pack.hash: ") || !rendered.contains("\ngraph TD\n") {
        return Err(format!(
            "mermaid output missing metadata or graph header: {rendered}"
        ));
    }
    for item in &response.data.pack.items {
        let id = item.memory_id.to_string();
        if !rendered.contains(&id) {
            return Err(format!("mermaid graph missing memory id label {id}"));
        }
        for provenance in item.rendered_provenance() {
            if !rendered.contains(&provenance.label) {
                return Err(format!(
                    "mermaid graph missing provenance label {:?}",
                    provenance.label
                ));
            }
        }
    }
    Ok(())
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
fn json_pack_text_matches_standalone_markdown_byte_for_byte() -> TestResult {
    let response = multi_section_fixture();
    let json_str = render_context_response_json(&response);
    let markdown = render_context_response_markdown(&response);

    let json: Value =
        serde_json::from_str(&json_str).map_err(|error| format!("JSON did not parse: {error}"))?;
    let pack_text = json
        .pointer("/data/pack/text")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing /data/pack/text in JSON".to_string())?;
    if pack_text != markdown {
        return Err(format!(
            "pack.text must equal markdown render byte-for-byte.\npack.text:\n{pack_text}\nmarkdown:\n{markdown}"
        ));
    }
    Ok(())
}

#[test]
fn json_pack_text_can_be_suppressed_for_structured_only_consumers() -> TestResult {
    let response = multi_section_fixture();
    let json_str = render_context_response_json_with_options(
        &response,
        ContextJsonRenderOptions {
            include_rendered_text: false,
            ..ContextJsonRenderOptions::default()
        },
    );

    let json: Value =
        serde_json::from_str(&json_str).map_err(|error| format!("JSON did not parse: {error}"))?;
    if json.pointer("/data/pack/text").is_some() {
        return Err("pack.text should be omitted when include_rendered_text=false".to_string());
    }
    json.pointer("/data/pack/items/0/memoryId")
        .ok_or_else(|| "structured pack items should remain present".to_string())?;
    Ok(())
}

#[test]
fn pack_items_carry_merged_certificate_and_step_fields() -> TestResult {
    // A1 phase 1 (bd-17c65.1.1): items[] now carries per-item data that
    // previously required walking selectionAudit.selected_items[],
    // selectionAudit.steps[], and provenanceFooter.entries[] in
    // parallel. This test pins the merge invariant: when a pack has any
    // items, every item must expose tokenCost, feasible, marginalGain,
    // objectiveValue, coveredFeatures, and sourceIndex inline.
    let response = multi_section_fixture();
    let json_str = render_context_response_json(&response);
    let json: Value =
        serde_json::from_str(&json_str).map_err(|error| format!("JSON did not parse: {error}"))?;
    let items = json
        .pointer("/data/pack/items")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing items array".to_string())?;
    if items.is_empty() {
        return Err("fixture must have at least one item to exercise A1 merge".to_string());
    }
    for (i, item) in items.iter().enumerate() {
        for field in ["tokenCost", "feasible", "coveredFeatures", "sourceIndex"] {
            if item.get(field).is_none() {
                return Err(format!(
                    "item[{i}] is missing merged field `{field}` (A1 phase 1 contract)"
                ));
            }
        }
        let scores = item
            .get("scores")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("item[{i}].scores is missing"))?;
        for field in ["marginalGain", "objectiveValue"] {
            if !scores.contains_key(field) {
                return Err(format!(
                    "item[{i}].scores is missing merged field `{field}` (A1 phase 1 contract)"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn markdown_render_appends_pack_metadata_html_comments() -> TestResult {
    // D3 (bd-17c65.4.3) — the markdown body ends with HTML comments
    // carrying pack.hash and pack.schema so an agent piping the body
    // can correlate back to the structured pack record without
    // re-querying. Comments are invisible to rendered markdown and to
    // LLMs but trivially greppable.
    //
    // `pack.generatedAt` is intentionally omitted: see the matching
    // comment in render_context_response_markdown for the rationale
    // (it would break A4 byte-equivalence between standalone markdown
    // and the JSON `pack.text` field).
    let response = multi_section_fixture();
    let markdown = render_context_response_markdown(&response);

    for needle in ["<!-- pack.hash:", "<!-- pack.schema: ee.response.v1 -->"] {
        if !markdown.contains(needle) {
            return Err(format!(
                "rendered markdown is missing `{needle}` trailing comment (D3 contract)"
            ));
        }
    }

    // The comments must appear on consecutive trailing non-empty lines
    // so a grep-based parser can read them with a fixed prefix regex
    // without searching the entire body.
    let trailing: Vec<&str> = markdown
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(2)
        .collect();
    let trailing_joined = trailing.join("\n");
    for prefix in ["<!-- pack.schema:", "<!-- pack.hash:"] {
        if !trailing_joined.contains(prefix) {
            return Err(format!(
                "trailing 2 non-empty lines must include `{prefix}`; got: {trailing_joined}"
            ));
        }
    }
    Ok(())
}

#[test]
fn markdown_pack_metadata_does_not_leak_query_in_comments() -> TestResult {
    // The HTML comments are invisible to rendered markdown but they ARE
    // present in the byte stream. Privacy-wise that means an accidental
    // pipe/log of the body would expose them. We assert that the comment
    // block contains pack.hash, pack.schema, and pack.generatedAt — and
    // nothing else that could leak the raw query string.
    let response = multi_section_fixture();
    let markdown = render_context_response_markdown(&response);
    let comment_block: String = markdown
        .lines()
        .filter(|line| line.starts_with("<!--"))
        .collect::<Vec<_>>()
        .join("\n");
    if comment_block.contains("prepare release v0.2.0") {
        return Err(format!(
            "trailing HTML comments leak the raw query: {comment_block}"
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
