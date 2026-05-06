#![no_main]

use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, UnitScore};
use ee::pack::{
    ContextPackProfile, PackCandidate, PackCandidateInput, PackProvenance, PackSection,
    TokenBudget, assemble_draft_with_profile,
};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

const MAX_INPUT_BYTES: usize = 131_072;
const MAX_CANDIDATES: usize = 64;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    if let Ok(value) = serde_json::from_slice::<Value>(data) {
        run_json_case(&value);
    } else {
        run_raw_case(data);
    }
});

fn run_json_case(value: &Value) {
    let budget_tokens = value
        .get("budget")
        .or_else(|| value.get("max_tokens"))
        .and_then(value_as_u32)
        .unwrap_or(4_000);
    let query = value
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("token budget fuzz");
    let candidates_value = value.get("candidates").unwrap_or(value);
    let candidates = candidates_from_json(candidates_value, budget_tokens);

    run_case(query, budget_tokens, &candidates);
}

fn run_raw_case(data: &[u8]) {
    let budget_tokens = read_u32(data, 0);
    let query = match data.get(4).copied().unwrap_or_default() % 16 {
        0 => "",
        1 => "   ",
        2 => "\t\n",
        _ => "token budget fuzz",
    };
    let candidates = candidates_from_raw(data, budget_tokens);

    run_case(query, budget_tokens, &candidates);
}

fn run_case(query: &str, budget_tokens: u32, candidates: &[PackCandidate]) {
    let Ok(budget) = TokenBudget::new(budget_tokens) else {
        return;
    };

    for profile in ContextPackProfile::all() {
        let first = assemble_draft_with_profile(profile, query, budget, candidates.iter().cloned());
        let second =
            assemble_draft_with_profile(profile, query, budget, candidates.iter().cloned());

        assert_eq!(first, second);
        if let Ok(first) = first {
            assert!(first.used_tokens <= budget.max_tokens());
            assert_eq!(first.selection_certificate.budget_used, first.used_tokens);
            assert_eq!(
                first.selection_certificate.budget_limit,
                budget.max_tokens()
            );
            assert_eq!(
                first.selection_certificate.selected_count,
                first.items.len()
            );
            assert_eq!(
                first.selection_certificate.omitted_count,
                first.omitted.len()
            );
            assert!(first.items.iter().all(|item| item.estimated_tokens > 0));
        }
    }
}

fn candidates_from_json(value: &Value, budget_tokens: u32) -> Vec<PackCandidate> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };

    items
        .iter()
        .take(MAX_CANDIDATES)
        .enumerate()
        .filter_map(|(index, item)| {
            let index_byte = index_byte(index);
            let estimated_tokens = item
                .get("estimated_tokens")
                .or_else(|| item.get("tokens"))
                .and_then(value_as_u32)
                .unwrap_or_else(|| token_edge(index_byte, budget_tokens));
            let relevance = item
                .get("relevance")
                .and_then(value_as_unit_f32)
                .unwrap_or(0.75);
            let utility = item
                .get("utility")
                .and_then(value_as_unit_f32)
                .unwrap_or(0.5);
            let section = item
                .get("section")
                .and_then(Value::as_str)
                .and_then(section_from_str)
                .unwrap_or_else(|| section_from_byte(index_byte));
            let content = item
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("budget candidate content");
            let why = item
                .get("why")
                .and_then(Value::as_str)
                .unwrap_or("selected by token budget fuzz target");
            let diversity_key = item
                .get("diversity_key")
                .or_else(|| item.get("diversity"))
                .and_then(Value::as_str);

            build_candidate(
                index,
                section,
                content,
                estimated_tokens,
                relevance,
                utility,
                why,
                diversity_key,
            )
        })
        .collect()
}

fn candidates_from_raw(data: &[u8], budget_tokens: u32) -> Vec<PackCandidate> {
    let mut candidates = Vec::new();
    for (index, chunk) in data.get(5..).unwrap_or_default().chunks(12).enumerate() {
        if index >= MAX_CANDIDATES {
            break;
        }

        let index_byte = index_byte(index);
        let token_seed = chunk.first().copied().unwrap_or(index_byte);
        let estimated_tokens = if chunk.len() >= 5 {
            token_edge(token_seed, budget_tokens)
        } else {
            1
        };
        let relevance = byte_as_unit(chunk.get(5).copied().unwrap_or(191));
        let utility = byte_as_unit(chunk.get(6).copied().unwrap_or(128));
        let section = section_from_byte(chunk.get(7).copied().unwrap_or(index_byte));
        let duplicate = chunk.get(8).copied().unwrap_or_default() % 4 == 0;
        let content = if duplicate {
            "duplicate budget candidate"
        } else {
            "distinct token budget candidate"
        };
        let diversity_key = if duplicate { Some("dup") } else { None };

        if let Some(candidate) = build_candidate(
            index,
            section,
            content,
            estimated_tokens,
            relevance,
            utility,
            "selected by raw token budget fuzz target",
            diversity_key,
        ) {
            candidates.push(candidate);
        }
    }
    candidates
}

fn build_candidate(
    index: usize,
    section: PackSection,
    content: &str,
    estimated_tokens: u32,
    relevance: f32,
    utility: f32,
    why: &str,
    diversity_key: Option<&str>,
) -> Option<PackCandidate> {
    let memory_id = memory_id(index)?;
    let provenance = provenance(index)?;
    let relevance = UnitScore::parse(relevance).ok()?;
    let utility = UnitScore::parse(utility).ok()?;
    let mut candidate = PackCandidate::new(PackCandidateInput {
        memory_id,
        section,
        content: content.to_string(),
        estimated_tokens,
        relevance,
        utility,
        provenance: vec![provenance],
        why: why.to_string(),
    })
    .ok()?;

    if let Some(diversity_key) = diversity_key {
        candidate = candidate.with_diversity_key(diversity_key);
    }

    Some(candidate)
}

fn memory_id(index: usize) -> Option<MemoryId> {
    MemoryId::from_str(&format!("mem_{index:026}")).ok()
}

fn provenance(index: usize) -> Option<PackProvenance> {
    let uri = ProvenanceUri::from_str(&format!(
        "file://fuzz/corpus/pack_token_budget/seed_{index}.md#L1"
    ))
    .ok()?;
    PackProvenance::new(uri, "token budget fuzz seed").ok()
}

fn value_as_u32(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .map(|number| number.min(u64::from(u32::MAX)) as u32)
        .or_else(|| value.as_str()?.parse::<u32>().ok())
}

fn value_as_unit_f32(value: &Value) -> Option<f32> {
    let number = value.as_f64()?;
    if number.is_finite() {
        Some(number.clamp(0.0, 1.0) as f32)
    } else {
        None
    }
}

fn token_edge(seed: u8, budget_tokens: u32) -> u32 {
    match seed % 8 {
        0 => 0,
        1 => 1,
        2 => budget_tokens.saturating_sub(1).max(1),
        3 => budget_tokens,
        4 => budget_tokens.saturating_add(1),
        5 => u32::MAX,
        6 => read_u32(
            &[seed, seed.wrapping_mul(17), seed.wrapping_add(91), 255],
            0,
        ),
        _ => 4_000,
    }
}

fn index_byte(index: usize) -> u8 {
    match u8::try_from(index) {
        Ok(value) => value,
        Err(_) => u8::MAX,
    }
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    let mut bytes = [0_u8; 4];
    if let Some(slice) = data.get(offset..offset.saturating_add(4)) {
        for (target, source) in bytes.iter_mut().zip(slice.iter().copied()) {
            *target = source;
        }
    }
    u32::from_le_bytes(bytes)
}

fn byte_as_unit(byte: u8) -> f32 {
    f32::from(byte) / 255.0
}

fn section_from_str(value: &str) -> Option<PackSection> {
    match value {
        "procedural_rules" | "procedural" => Some(PackSection::ProceduralRules),
        "decisions" | "decision" => Some(PackSection::Decisions),
        "failures" | "failure" => Some(PackSection::Failures),
        "evidence" => Some(PackSection::Evidence),
        "artifacts" | "artifact" => Some(PackSection::Artifacts),
        _ => None,
    }
}

fn section_from_byte(byte: u8) -> PackSection {
    match byte % 5 {
        0 => PackSection::ProceduralRules,
        1 => PackSection::Decisions,
        2 => PackSection::Failures,
        3 => PackSection::Evidence,
        _ => PackSection::Artifacts,
    }
}
