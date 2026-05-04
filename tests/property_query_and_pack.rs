use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, UnitScore};
use ee::pack::{
    PackCandidate, PackCandidateInput, PackProvenance, PackSection, TokenBudget, assemble_draft,
};
use ee::search::parse_search_query;
use proptest::prelude::*;
use uuid::Uuid;

fn section_for(index: usize) -> PackSection {
    match index % 5 {
        0 => PackSection::ProceduralRules,
        1 => PackSection::Decisions,
        2 => PackSection::Failures,
        3 => PackSection::Evidence,
        _ => PackSection::Artifacts,
    }
}

fn memory_id(index: usize) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(index as u128 + 1))
}

fn unit_score(raw: u16) -> Result<UnitScore, String> {
    UnitScore::parse(f32::from(raw) / 1000.0).map_err(|error| format!("{error:?}"))
}

fn provenance(index: usize) -> Result<PackProvenance, String> {
    let uri = ProvenanceUri::from_str(&format!("file://property-pack-{index}.md#L1"))
        .map_err(|error| format!("{error:?}"))?;
    PackProvenance::new(uri, "property evidence").map_err(|error| format!("{error:?}"))
}

fn candidate_from_spec(
    index: usize,
    tokens: u32,
    relevance: u16,
    utility: u16,
    duplicate_content: bool,
) -> Result<PackCandidate, String> {
    let content = if duplicate_content {
        "Shared property-test memory content.".to_string()
    } else {
        format!("Property-test memory content {index}.")
    };
    PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(index),
        section: section_for(index),
        content,
        estimated_tokens: tokens,
        relevance: unit_score(relevance)?,
        utility: unit_score(utility)?,
        provenance: vec![provenance(index)?],
        why: format!("Property-test candidate {index} matches the query."),
    })
    .map_err(|error| format!("{error:?}"))
}

fn candidate_specs() -> impl Strategy<Value = Vec<(u32, u16, u16, bool)>> {
    prop::collection::vec(
        (1_u32..=250, 0_u16..=1000, 0_u16..=1000, any::<bool>()),
        0..32,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn query_parser_never_panics_on_arbitrary_bytes(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let input = String::from_utf8_lossy(&data);
        let parsed = parse_search_query(&input);
        let printed = parsed.to_string();
        let reparsed = parse_search_query(&printed);

        prop_assert_eq!(reparsed, parsed);
    }

    #[test]
    fn query_parser_parse_print_roundtrips(chars in prop::collection::vec(any::<char>(), 0..512)) {
        let input: String = chars.into_iter().collect();
        let parsed = parse_search_query(&input);
        let printed = parsed.to_string();
        let reparsed = parse_search_query(&printed);

        prop_assert_eq!(reparsed, parsed);
    }

    #[test]
    fn pack_selection_respects_token_budget(
        budget_raw in 1_u32..=400,
        specs in candidate_specs(),
    ) {
        let budget = TokenBudget::new(budget_raw)
            .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        let mut candidates = Vec::with_capacity(specs.len());
        for (index, (tokens, relevance, utility, duplicate_content)) in specs.into_iter().enumerate() {
            candidates.push(
                candidate_from_spec(index, tokens, relevance, utility, duplicate_content)
                    .map_err(TestCaseError::fail)?,
            );
        }

        let draft = assemble_draft("property pack budget", budget, candidates)
            .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        let selected_tokens: u32 = draft.items.iter().map(|item| item.estimated_tokens).sum();

        prop_assert_eq!(draft.used_tokens, selected_tokens);
        prop_assert!(draft.used_tokens <= budget.max_tokens());
        prop_assert_eq!(draft.selection_certificate.budget_used, draft.used_tokens);
        prop_assert_eq!(draft.selection_certificate.budget_limit, budget.max_tokens());
        prop_assert_eq!(
            draft.selection_certificate.selected_count,
            draft.items.len(),
        );
        prop_assert_eq!(
            draft.selection_certificate.omitted_count,
            draft.omitted.len(),
        );
        for item in &draft.items {
            prop_assert!(item.estimated_tokens > 0);
        }
        for omission in &draft.omitted {
            prop_assert!(omission.estimated_tokens > 0);
        }
        for step in &draft.selection_certificate.steps {
            prop_assert!(step.objective_value.is_finite());
            prop_assert!(step.token_cost > 0);
        }
    }
}
