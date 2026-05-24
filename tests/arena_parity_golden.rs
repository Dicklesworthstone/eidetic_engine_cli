// bd-1prrl.7.4 — swarmx.12.d: arena parity golden + determinism harness.
//
// Proves `ArenaMode::Disabled` and `ArenaMode::RequestScoped` produce
// byte-identical observable artifacts for representative pack-assembly
// fixtures across every `ContextPackProfile`. The parent (bd-1prrl.7)
// acceptance requires byte-identical public output regardless of
// arena allocation strategy; this file freezes that contract on:
//   - `PackDraft` (items, omitted, used_tokens, budget, selection_audit)
//   - `render_context_markdown` output
//   - omission order, selection-audit step order, algorithm_id
//
// Fixtures cover: empty pool, below-budget, budget-pressured
// (omissions present), coverage-fill triggered, tombstoned items
// mixed with live items, and provenance-heavy candidates. Both
// the MMR-family profiles (Compact/Balanced/Grounding/Orientation/
// Thorough) and the facility-location Submodular profile are exercised.
//
// The parity-gated swap of context orchestration from
// `ArenaMode::Disabled` to `ArenaMode::RequestScoped` lands once this
// file is green: any arena implementation that changes public output
// fails one of these focused tests before broad CI even runs.

use ee::models::{MemoryId, ProvenanceUri, UnitScore};
use ee::pack::{
    ArenaMode, ContextPackProfile, ContextRequest, ContextRequestInput, PackAssemblyOptions,
    PackCandidate, PackCandidateInput, PackDraft, PackProvenance, PackSection, TokenBudget,
    assemble_draft_with_profile_and_options_seeded, render_context_markdown,
};
use ee::runtime::determinism::Deterministic;
use uuid::Uuid;

type TestResult = Result<(), String>;

const ALL_PROFILES: &[ContextPackProfile] = &[
    ContextPackProfile::Compact,
    ContextPackProfile::Balanced,
    ContextPackProfile::Grounding,
    ContextPackProfile::Orientation,
    ContextPackProfile::Thorough,
    ContextPackProfile::Submodular,
];

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed + 1))
}

fn unit_score(value: f32) -> Result<UnitScore, String> {
    UnitScore::parse(value).map_err(|error| format!("UnitScore::parse({value}): {error:?}"))
}

fn provenance(path_seed: u128, label: &str) -> Result<PackProvenance, String> {
    let uri = ProvenanceUri::from_str(&format!("file://arena-parity-{path_seed}.md#L1"))
        .map_err(|error| format!("ProvenanceUri::from_str: {error:?}"))?;
    PackProvenance::new(uri, label.to_string())
        .map_err(|error| format!("PackProvenance::new: {error:?}"))
}

fn candidate_basic(
    seed: u128,
    section: PackSection,
    content: impl Into<String>,
    tokens: u32,
    relevance: f32,
    utility: f32,
) -> Result<PackCandidate, String> {
    PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(seed),
        section,
        content: content.into(),
        estimated_tokens: tokens,
        relevance: unit_score(relevance)?,
        utility: unit_score(utility)?,
        provenance: vec![provenance(seed, "arena-parity evidence")?],
        why: format!("arena-parity candidate {seed} matches the query"),
    })
    .map_err(|error| format!("PackCandidate::new({seed}): {error:?}"))
}

fn candidate_with_n_provenance(
    seed: u128,
    section: PackSection,
    tokens: u32,
    relevance: f32,
    utility: f32,
    provenance_count: usize,
) -> Result<PackCandidate, String> {
    let mut provenance_entries = Vec::with_capacity(provenance_count);
    for index in 0..provenance_count {
        // Stable, non-colliding URIs so PackProvenance::new is happy and
        // the rendered output is deterministic across calls.
        let uri = ProvenanceUri::from_str(&format!(
            "file://arena-parity-{seed}-{index}.md#L{}",
            (index + 1) * 10
        ))
        .map_err(|error| format!("ProvenanceUri::from_str: {error:?}"))?;
        provenance_entries.push(
            PackProvenance::new(uri, format!("provenance entry {index}"))
                .map_err(|error| format!("PackProvenance::new: {error:?}"))?,
        );
    }
    PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(seed),
        section,
        content: format!("Provenance-heavy candidate {seed} with rich evidence."),
        estimated_tokens: tokens,
        relevance: unit_score(relevance)?,
        utility: unit_score(utility)?,
        provenance: provenance_entries,
        why: format!("provenance-heavy candidate {seed} matches the query"),
    })
    .map_err(|error| format!("PackCandidate::new({seed}): {error:?}"))
}

fn candidate_tombstoned(
    seed: u128,
    section: PackSection,
    tokens: u32,
    relevance: f32,
    utility: f32,
    tombstoned_at: &str,
) -> Result<PackCandidate, String> {
    Ok(candidate_basic(
        seed,
        section,
        format!("Tombstoned memory {seed}."),
        tokens,
        relevance,
        utility,
    )?
    .with_tombstoned_at(tombstoned_at))
}

// --- Fixture builders ----------------------------------------------------

fn fixture_empty() -> Vec<PackCandidate> {
    Vec::new()
}

fn fixture_below_budget() -> Result<Vec<PackCandidate>, String> {
    // Few small candidates; every one fits, so the pack has no omissions.
    Ok(vec![
        candidate_basic(
            1,
            PackSection::ProceduralRules,
            "Run cargo fmt before release.",
            40,
            0.9,
            0.8,
        )?,
        candidate_basic(
            2,
            PackSection::Decisions,
            "ADR-0001: arena modes are internal allocation strategy.",
            60,
            0.85,
            0.75,
        )?,
        candidate_basic(
            3,
            PackSection::Evidence,
            "tests/arena_parity_golden.rs proves byte-identical output.",
            80,
            0.8,
            0.7,
        )?,
    ])
}

fn fixture_budget_pressured() -> Result<Vec<PackCandidate>, String> {
    // Many medium candidates each well below budget but the total
    // exceeds the budget, so the selector must omit at least some.
    let sections = [
        PackSection::ProceduralRules,
        PackSection::Decisions,
        PackSection::Failures,
        PackSection::Evidence,
        PackSection::Artifacts,
    ];
    let mut candidates = Vec::with_capacity(24);
    for seed in 0u128..24 {
        let section = sections[(seed as usize) % sections.len()];
        candidates.push(candidate_basic(
            seed + 100,
            section,
            format!("Budget-pressured memory {seed} with varied content."),
            120,
            0.6 + ((seed as f32) * 0.01).min(0.35),
            0.5 + ((seed as f32) * 0.012).min(0.4),
        )?);
    }
    Ok(candidates)
}

fn fixture_coverage_fill() -> Result<Vec<PackCandidate>, String> {
    // Mix of strong-relevance candidates with overlapping content and
    // weaker-relevance candidates from different sections so MMR's
    // coverage-fill phase has something to backfill with after the
    // primary diversity-greedy phase exits.
    let mut candidates = Vec::with_capacity(20);
    for seed in 0u128..10 {
        candidates.push(candidate_basic(
            seed + 200,
            PackSection::ProceduralRules,
            format!("Run cargo fmt before release. variant {seed}"),
            90,
            0.95,
            0.9,
        )?);
    }
    for seed in 0u128..10 {
        candidates.push(candidate_basic(
            seed + 220,
            PackSection::Failures,
            format!("Past incident: arena mode swap broke parity {seed}"),
            85,
            0.55,
            0.6,
        )?);
    }
    Ok(candidates)
}

fn fixture_tombstoned_mix() -> Result<Vec<PackCandidate>, String> {
    let mut candidates = Vec::with_capacity(8);
    for seed in 0u128..4 {
        candidates.push(candidate_basic(
            seed + 300,
            PackSection::ProceduralRules,
            format!("Live memory {seed} for arena parity"),
            60,
            0.85,
            0.8,
        )?);
    }
    for seed in 0u128..4 {
        candidates.push(candidate_tombstoned(
            seed + 320,
            PackSection::Decisions,
            55,
            0.9,
            0.7,
            "2026-05-01T00:00:00Z",
        )?);
    }
    Ok(candidates)
}

fn fixture_provenance_heavy() -> Result<Vec<PackCandidate>, String> {
    let sections = [
        PackSection::Evidence,
        PackSection::Decisions,
        PackSection::Artifacts,
    ];
    let mut candidates = Vec::with_capacity(9);
    for seed in 0u128..9 {
        let section = sections[(seed as usize) % sections.len()];
        candidates.push(candidate_with_n_provenance(
            seed + 400,
            section,
            110,
            0.7 + ((seed as f32) * 0.02).min(0.25),
            0.65 + ((seed as f32) * 0.02).min(0.3),
            // Vary the per-candidate provenance count so the rendered
            // section pulls multiple URIs and the audit output reflects
            // the heavier provenance footprint.
            3 + ((seed as usize) % 4),
        )?);
    }
    Ok(candidates)
}

// --- Parity harness ------------------------------------------------------

fn assemble(
    profile: ContextPackProfile,
    query: &str,
    budget: TokenBudget,
    candidates: Vec<PackCandidate>,
    arena_mode: ArenaMode,
    seed: u64,
) -> Result<PackDraft, String> {
    let determinism = Deterministic::from_seed(seed);
    assemble_draft_with_profile_and_options_seeded(
        profile,
        query,
        budget,
        candidates,
        PackAssemblyOptions {
            arena_mode,
            ..PackAssemblyOptions::default()
        },
        &determinism,
    )
    .map_err(|error| format!("assemble({profile:?}, {arena_mode:?}): {error:?}"))
}

fn build_context_request(
    query: &str,
    profile: ContextPackProfile,
    budget: TokenBudget,
) -> Result<ContextRequest, String> {
    let mut input = ContextRequestInput::for_query(query.to_string());
    input.profile = Some(profile);
    input.max_tokens = Some(budget.max_tokens());
    ContextRequest::new(input).map_err(|error| format!("ContextRequest::new: {error:?}"))
}

fn assert_arena_parity_for_profile(
    label: &str,
    profile: ContextPackProfile,
    query: &str,
    max_tokens: u32,
    candidates: &[PackCandidate],
    seed: u64,
) -> TestResult {
    let budget = TokenBudget::new(max_tokens)
        .map_err(|error| format!("TokenBudget::new({max_tokens}): {error:?}"))?;
    let disabled = assemble(
        profile,
        query,
        budget,
        candidates.to_vec(),
        ArenaMode::Disabled,
        seed,
    )?;
    let request_scoped = assemble(
        profile,
        query,
        budget,
        candidates.to_vec(),
        ArenaMode::RequestScoped,
        seed,
    )?;

    if disabled != request_scoped {
        let mut diffs = Vec::new();
        if disabled.items != request_scoped.items {
            diffs.push(format!(
                "items: disabled={} request_scoped={}",
                disabled.items.len(),
                request_scoped.items.len()
            ));
        }
        if disabled.omitted != request_scoped.omitted {
            diffs.push(format!(
                "omitted: disabled={} request_scoped={}",
                disabled.omitted.len(),
                request_scoped.omitted.len()
            ));
        }
        if disabled.used_tokens != request_scoped.used_tokens {
            diffs.push(format!(
                "used_tokens: disabled={} request_scoped={}",
                disabled.used_tokens, request_scoped.used_tokens
            ));
        }
        if disabled.selection_audit.selected_items != request_scoped.selection_audit.selected_items
        {
            diffs.push("selection_audit.selected_items differ".to_string());
        }
        if disabled.selection_audit.steps != request_scoped.selection_audit.steps {
            diffs.push("selection_audit.steps differ".to_string());
        }
        if disabled.selection_audit.algorithm_id != request_scoped.selection_audit.algorithm_id {
            diffs.push(format!(
                "selection_audit.algorithm_id: disabled={:?} request_scoped={:?}",
                disabled.selection_audit.algorithm_id, request_scoped.selection_audit.algorithm_id
            ));
        }
        return Err(format!(
            "{label}/{profile:?}: PackDraft diverges across arena modes — {}",
            diffs.join("; ")
        ));
    }

    // Markdown rendering parity uses an empty degraded list and a
    // standalone request (no DB) so the test stays mock-free.
    let request = build_context_request(query, profile, budget)?;
    let markdown_disabled = render_context_markdown(&request, &disabled, &[]);
    let markdown_request_scoped = render_context_markdown(&request, &request_scoped, &[]);
    if markdown_disabled != markdown_request_scoped {
        return Err(format!(
            "{label}/{profile:?}: render_context_markdown byte-differs across arena modes ({} vs {} bytes)",
            markdown_disabled.len(),
            markdown_request_scoped.len()
        ));
    }

    Ok(())
}

fn assert_arena_parity_all_profiles(
    label: &str,
    query: &str,
    max_tokens: u32,
    candidates: &[PackCandidate],
    seed: u64,
) -> TestResult {
    for profile in ALL_PROFILES {
        assert_arena_parity_for_profile(label, *profile, query, max_tokens, candidates, seed)?;
    }
    Ok(())
}

// --- Tests ---------------------------------------------------------------

#[test]
fn arena_parity_empty_pool_across_all_profiles() -> TestResult {
    let candidates = fixture_empty();
    assert_arena_parity_all_profiles(
        "empty_pool",
        "find anything about arena parity",
        1_000,
        &candidates,
        0xee_a7_e3_3a,
    )
}

#[test]
fn arena_parity_below_budget_across_all_profiles() -> TestResult {
    let candidates = fixture_below_budget()?;
    assert_arena_parity_all_profiles(
        "below_budget",
        "prepare release with formatting checks",
        4_000,
        &candidates,
        0xfa_c1_77_07,
    )
}

#[test]
fn arena_parity_budget_pressured_forces_omissions_balanced() -> TestResult {
    let candidates = fixture_budget_pressured()?;
    let label = "budget_pressured_balanced";
    assert_arena_parity_for_profile(
        label,
        ContextPackProfile::Balanced,
        "summarize prior arena-mode decisions and failures",
        500,
        &candidates,
        0x42_43_44_45,
    )?;

    // Sanity-check that the fixture actually exercises the omission
    // path; if the assembler stopped omitting we'd be measuring parity
    // on a degenerate case that bypasses the risky surface.
    let budget = TokenBudget::new(500).map_err(|error| format!("TokenBudget: {error:?}"))?;
    let draft = assemble(
        ContextPackProfile::Balanced,
        "summarize prior arena-mode decisions and failures",
        budget,
        candidates,
        ArenaMode::Disabled,
        0x42_43_44_45,
    )?;
    if draft.omitted.is_empty() {
        return Err(format!(
            "{label}: fixture is not actually budget-pressured (no omissions); \
             parity test would not exercise the omission ordering surface"
        ));
    }
    Ok(())
}

#[test]
fn arena_parity_budget_pressured_across_all_profiles() -> TestResult {
    let candidates = fixture_budget_pressured()?;
    assert_arena_parity_all_profiles(
        "budget_pressured_all",
        "summarize prior arena-mode decisions and failures",
        500,
        &candidates,
        0x42_43_44_45,
    )
}

#[test]
fn arena_parity_coverage_fill_balanced() -> TestResult {
    let candidates = fixture_coverage_fill()?;
    assert_arena_parity_for_profile(
        "coverage_fill_balanced",
        ContextPackProfile::Balanced,
        "what should I run before release",
        1_400,
        &candidates,
        0x11_22_33_44,
    )
}

#[test]
fn arena_parity_coverage_fill_submodular() -> TestResult {
    let candidates = fixture_coverage_fill()?;
    assert_arena_parity_for_profile(
        "coverage_fill_submodular",
        ContextPackProfile::Submodular,
        "what should I run before release",
        1_400,
        &candidates,
        0x55_66_77_88,
    )
}

#[test]
fn arena_parity_tombstoned_items_across_all_profiles() -> TestResult {
    let candidates = fixture_tombstoned_mix()?;
    assert_arena_parity_all_profiles(
        "tombstoned_mix",
        "what changed in arena handling",
        2_000,
        &candidates,
        0x99_aa_bb_cc,
    )
}

#[test]
fn arena_parity_provenance_heavy_across_all_profiles() -> TestResult {
    let candidates = fixture_provenance_heavy()?;
    assert_arena_parity_all_profiles(
        "provenance_heavy",
        "show evidence for arena policy decisions",
        4_000,
        &candidates,
        0xab_cd_ef_01,
    )
}

#[test]
fn arena_parity_determinism_repeated_calls_balanced() -> TestResult {
    // Same input + same arena mode + same seed → byte-identical
    // output across repeated invocations. This complements the
    // arena-mode-on-vs-off parity above: it freezes the
    // per-arena-mode determinism contract so a regression that only
    // breaks determinism within one mode (without changing parity)
    // still trips a focused test.
    let candidates = fixture_coverage_fill()?;
    let budget = TokenBudget::new(1_400).map_err(|error| format!("TokenBudget: {error:?}"))?;
    for arena_mode in [ArenaMode::Disabled, ArenaMode::RequestScoped] {
        let first = assemble(
            ContextPackProfile::Balanced,
            "what should I run before release",
            budget,
            candidates.clone(),
            arena_mode,
            0x11_22_33_44,
        )?;
        let second = assemble(
            ContextPackProfile::Balanced,
            "what should I run before release",
            budget,
            candidates.clone(),
            arena_mode,
            0x11_22_33_44,
        )?;
        if first != second {
            return Err(format!(
                "determinism broken within arena_mode={arena_mode:?}: repeated \
                 calls produced different PackDraft output"
            ));
        }
        let request = build_context_request(
            "what should I run before release",
            ContextPackProfile::Balanced,
            budget,
        )?;
        let markdown_first = render_context_markdown(&request, &first, &[]);
        let markdown_second = render_context_markdown(&request, &second, &[]);
        if markdown_first != markdown_second {
            return Err(format!(
                "determinism broken within arena_mode={arena_mode:?}: repeated \
                 calls produced different markdown rendering"
            ));
        }
    }
    Ok(())
}

#[test]
fn arena_parity_markdown_byte_identical_provenance_heavy() -> TestResult {
    // Markdown rendering pulls per-item provenance into the rendered
    // sections; with provenance-heavy candidates the renderer touches
    // every additional URI. Freeze byte parity on that surface for the
    // Thorough profile where the per-item budget is largest.
    let candidates = fixture_provenance_heavy()?;
    let budget = TokenBudget::new(6_000).map_err(|error| format!("TokenBudget: {error:?}"))?;
    let disabled = assemble(
        ContextPackProfile::Thorough,
        "show evidence for arena policy decisions",
        budget,
        candidates.clone(),
        ArenaMode::Disabled,
        0xde_ad_be_ef,
    )?;
    let request_scoped = assemble(
        ContextPackProfile::Thorough,
        "show evidence for arena policy decisions",
        budget,
        candidates,
        ArenaMode::RequestScoped,
        0xde_ad_be_ef,
    )?;
    let request = build_context_request(
        "show evidence for arena policy decisions",
        ContextPackProfile::Thorough,
        budget,
    )?;
    let md_disabled = render_context_markdown(&request, &disabled, &[]);
    let md_request_scoped = render_context_markdown(&request, &request_scoped, &[]);
    if md_disabled != md_request_scoped {
        return Err(format!(
            "markdown rendering byte-differs across arena modes on \
             provenance-heavy/Thorough ({} vs {} bytes)",
            md_disabled.len(),
            md_request_scoped.len()
        ));
    }
    if disabled.items.is_empty() {
        return Err("provenance-heavy/Thorough produced empty items; \
             fixture would not actually exercise the rendering surface"
            .to_string());
    }
    Ok(())
}
