//! The pairwise cache is optional; candidate admission and selection are not.

use super::*;

fn candidate(number: u128, content: &str, section: PackSection) -> PackCandidate {
    let memory_id = MemoryId::from_uuid(uuid::Uuid::from_u128(number));
    PackCandidate::new(PackCandidateInput {
        memory_id,
        section,
        content: content.to_owned(),
        estimated_tokens: 8,
        relevance: UnitScore::parse(0.9).unwrap(),
        utility: UnitScore::parse(0.5).unwrap(),
        provenance: vec![
            PackProvenance::new(ProvenanceUri::EeMemory(memory_id), "cache fixture").unwrap(),
        ],
        why: "facility cache parity".to_owned(),
    })
    .unwrap()
}

fn profiles() -> Vec<FacilityCandidateProfile> {
    let mut result: Vec<_> = [
        (
            1,
            "bounded worker retries with backoff",
            PackSection::ProceduralRules,
        ),
        (
            2,
            "worker retries after transient failures",
            PackSection::Failures,
        ),
        (
            3,
            "bounded worker retries with backoff",
            PackSection::Evidence,
        ),
        (
            4,
            "preserve a durable transaction journal",
            PackSection::Decisions,
        ),
        (5, "record the incident identifier", PackSection::Artifacts),
        (
            6,
            "the transaction journal is durable",
            PackSection::Evidence,
        ),
        (
            1,
            "same identity from another retrieval path",
            PackSection::ProceduralRules,
        ),
    ]
    .into_iter()
    .map(|(number, content, section)| {
        FacilityCandidateProfile::from(candidate(number, content, section))
    })
    .collect();
    // Cover both the diversity floor and overlap overriding that floor.
    result[0].signature.diversity_key = Some("workers".to_owned());
    result[1].signature.diversity_key = Some("workers".to_owned());
    result[4].signature.diversity_key = Some("workers".to_owned());
    result
}

fn assert_pairwise_equal(left: &FacilitySimilarityCache, right: &FacilitySimilarityCache) {
    assert_eq!(left.width, right.width);
    for row in 0..left.width {
        for column in 0..left.width {
            assert_eq!(
                left.similarity(row, column).to_bits(),
                right.similarity(row, column).to_bits(),
                "different similarity bits at ({row}, {column})"
            );
        }
    }
}

#[test]
fn small_pool_keeps_dense_cache_without_copying_signatures() {
    let universe = profiles();
    let cache = FacilitySimilarityCache::new(&universe);
    assert_eq!(cache.values.len(), universe.len() * universe.len());
    assert!(cache.fallback_signatures.is_empty());
}

#[test]
fn zero_budget_preserves_all_exact_pairwise_scores() {
    let universe = profiles();
    let dense = FacilitySimilarityCache::new(&universe);
    let fallback = FacilitySimilarityCache::with_byte_limit(&universe, 0);
    assert!(fallback.values.is_empty());
    assert_eq!(fallback.fallback_signatures.len(), universe.len());
    assert_pairwise_equal(&dense, &fallback);
    for row in 0..universe.len() {
        for column in 0..universe.len() {
            assert_eq!(
                fallback.similarity(row, column).to_bits(),
                facility_signature_similarity(
                    &universe[row].signature,
                    &universe[column].signature,
                )
                .to_bits()
            );
        }
    }
}

#[test]
fn checked_matrix_sizing_includes_bytes_and_accepts_exact_boundary() {
    let limit = FACILITY_SIMILARITY_CACHE_MAX_BYTES;
    assert_eq!(FacilitySimilarityCache::dense_cell_count(0, 0), Some(0));
    assert_eq!(FacilitySimilarityCache::dense_cell_count(1, 3), None);
    assert_eq!(FacilitySimilarityCache::dense_cell_count(1, 4), Some(1));
    assert_eq!(
        FacilitySimilarityCache::dense_cell_count(2_048, limit),
        Some(4_194_304)
    );
    assert_eq!(
        FacilitySimilarityCache::dense_cell_count(2_049, limit),
        None
    );
    assert_eq!(
        FacilitySimilarityCache::dense_cell_count(usize::MAX, usize::MAX),
        None
    );
    // The square fits in usize, but converting its cells to bytes does not.
    let byte_overflow_width = 1_usize << ((usize::BITS - 2) / 2);
    assert!(
        byte_overflow_width
            .checked_mul(byte_overflow_width)
            .is_some()
    );
    assert_eq!(
        FacilitySimilarityCache::dense_cell_count(byte_overflow_width, usize::MAX),
        None
    );
}

#[test]
fn one_byte_below_dense_size_selects_exact_fallback() {
    let universe = profiles();
    let bytes = universe.len() * universe.len() * std::mem::size_of::<f32>();
    let dense = FacilitySimilarityCache::with_byte_limit(&universe, bytes);
    let fallback = FacilitySimilarityCache::with_byte_limit(&universe, bytes - 1);
    assert!(!dense.values.is_empty());
    assert!(fallback.values.is_empty());
    assert_pairwise_equal(&dense, &fallback);
}

#[test]
fn large_pool_does_not_allocate_a_quadratic_matrix() {
    let template = profiles().remove(0);
    // The former implementation would allocate 64 MiB just for this matrix.
    let universe = vec![template; 4_096];
    let cache = FacilitySimilarityCache::new(&universe);
    assert!(cache.values.is_empty());
    assert_eq!(cache.fallback_signatures.len(), universe.len());
    assert_eq!(cache.similarity(0, 4_095), 1.0);
    assert_eq!(cache.similarity(4_095, 0), 1.0);
}

#[test]
fn invalid_coordinates_cannot_alias_the_next_dense_row() {
    let universe = profiles();
    for bytes in [0, FACILITY_SIMILARITY_CACHE_MAX_BYTES] {
        let cache = FacilitySimilarityCache::with_byte_limit(&universe, bytes);
        for (row, column) in [
            (0, universe.len()),
            (universe.len(), 0),
            (1, usize::MAX),
            (usize::MAX, 1),
        ] {
            assert_eq!(cache.similarity(row, column), 0.0);
        }
    }
}

#[test]
fn empty_universe_is_safe_in_both_modes() {
    for bytes in [0, FACILITY_SIMILARITY_CACHE_MAX_BYTES] {
        let cache = FacilitySimilarityCache::with_byte_limit(&[], bytes);
        assert_eq!(cache.width, 0);
        assert!(cache.values.is_empty());
        assert!(cache.fallback_signatures.is_empty());
        assert_eq!(cache.similarity(0, 0), 0.0);
    }
}

#[test]
fn signature_snapshot_survives_candidate_removal_and_cache_cloning() {
    let mut universe = profiles();
    let dense = FacilitySimilarityCache::new(&universe);
    let fallback = FacilitySimilarityCache::with_byte_limit(&universe, 0);
    for profile in &mut universe {
        let _ = profile.candidate.take();
    }
    assert_pairwise_equal(&dense, &fallback.clone());
    assert_pairwise_equal(&dense.clone(), &fallback);
}

#[test]
fn marginal_gains_and_coverage_updates_preserve_f32_bits() {
    let universe = profiles();
    let dense = FacilitySimilarityCache::new(&universe);
    let fallback = FacilitySimilarityCache::with_byte_limit(&universe, 0);
    let mut dense_coverages = vec![0.0; universe.len()];
    let mut fallback_coverages = dense_coverages.clone();
    for chosen in [4, 1, 0, 3, 5, 2, 6] {
        for index in 0..universe.len() {
            assert_eq!(
                facility_marginal_gain_cached(index, &universe, &dense_coverages, &dense).to_bits(),
                facility_marginal_gain_cached(index, &universe, &fallback_coverages, &fallback)
                    .to_bits()
            );
        }
        let dense_value =
            update_facility_coverages_cached(&universe, &mut dense_coverages, &dense, chosen);
        let fallback_value =
            update_facility_coverages_cached(&universe, &mut fallback_coverages, &fallback, chosen);
        assert_eq!(dense_value.to_bits(), fallback_value.to_bits());
        assert_eq!(dense_coverages, fallback_coverages);
    }
}

fn assert_selection_queue_parity(lod_budget_shares: Option<PackLodBudgetShares>) {
    let mut universe = profiles();
    let dense = FacilitySimilarityCache::new(&universe);
    let fallback = FacilitySimilarityCache::with_byte_limit(&universe, 0);
    let mut dense_coverages = vec![0.0; universe.len()];
    let mut fallback_coverages = dense_coverages.clone();
    let mut dense_queue = FacilitySelectionQueue::new(&universe, &dense_coverages, &dense);
    let mut fallback_queue = FacilitySelectionQueue::new(&universe, &fallback_coverages, &fallback);
    let budget = TokenBudget::new(128).unwrap();
    let quotas = SectionQuotas::for_profile(ContextPackProfile::Submodular, budget.max_tokens());
    let mut section_usage = SectionTokenUsage::default();
    let mut lod_usage = PackLodBudgetState::from_options(
        PackAssemblyOptions {
            lod_budget_shares,
            ..PackAssemblyOptions::default()
        },
        budget,
    );
    let mut used_tokens = 0_u32;
    let mut selected_count = 0;
    loop {
        let dense_pick = dense_queue.select(
            &universe,
            &dense_coverages,
            &dense,
            used_tokens,
            budget,
            &quotas,
            &section_usage,
            &lod_usage,
        );
        let fallback_pick = fallback_queue.select(
            &universe,
            &fallback_coverages,
            &fallback,
            used_tokens,
            budget,
            &quotas,
            &section_usage,
            &lod_usage,
        );
        assert_eq!(
            dense_pick.map(|(index, gain)| (index, gain.to_bits())),
            fallback_pick.map(|(index, gain)| (index, gain.to_bits())),
            "cache mode changed a selection or marginal gain"
        );
        let Some((index, gain)) = dense_pick else {
            break;
        };
        if gain <= FACILITY_LOCATION_EPSILON {
            break;
        }
        let source = universe[index].candidate.take().unwrap();
        let plan = pack_lod_candidate_plan(
            &source,
            used_tokens,
            budget,
            &quotas,
            &section_usage,
            &lod_usage,
        )
        .unwrap();
        used_tokens += plan.candidate.estimated_tokens;
        assert!(used_tokens <= budget.max_tokens());
        section_usage.add_candidate(&plan.candidate);
        lod_usage.add(plan.tier, plan.candidate.estimated_tokens);
        let dense_value =
            update_facility_coverages_cached(&universe, &mut dense_coverages, &dense, index);
        let fallback_value =
            update_facility_coverages_cached(&universe, &mut fallback_coverages, &fallback, index);
        assert_eq!(dense_value.to_bits(), fallback_value.to_bits());
        assert_eq!(dense_coverages, fallback_coverages);
        dense_queue.advance_round();
        fallback_queue.advance_round();
        selected_count += 1;
        assert!(selected_count <= universe.len());
    }
    assert!(
        selected_count > 1,
        "fixture must exercise multiple selection rounds"
    );
}

#[test]
fn lazy_selection_queue_is_independent_of_cache_mode() {
    assert_selection_queue_parity(None);
}

#[test]
fn lod_selection_queue_is_independent_of_cache_mode() {
    assert_selection_queue_parity(Some(PackLodBudgetShares::default()));
}
