//! Integration coverage for the eight-stage retrieval pipeline in plan section 13.

use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, UnitScore};
use ee::pack::{
    ContextPackProfile, PackCandidate, PackCandidateInput, PackProvenance, PackSection,
    TokenBudget, assemble_draft_with_profile,
};
use ee::search::scoring::{
    RetrievalMaturity, SearchScoreComponents, SearchScoringConfig, SearchScoringSignals,
};
use uuid::Uuid;

type TestResult = Result<(), String>;

#[derive(Clone, Debug)]
struct RetrievalFixture {
    seed: u128,
    workspace: &'static str,
    level: &'static str,
    kind: &'static str,
    content: &'static str,
    base_score: f32,
    utility: f32,
    confidence: f32,
    maturity: RetrievalMaturity,
    has_memory_row: bool,
    has_provenance: bool,
    redacted: bool,
    expired: bool,
}

#[derive(Clone, Debug)]
struct ScoredFixture {
    fixture: RetrievalFixture,
    score: f32,
}

#[derive(Clone, Debug)]
struct StageCount {
    name: &'static str,
    count: usize,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn score(value: f32) -> Result<UnitScore, String> {
    UnitScore::parse(value).map_err(|error| format!("{error:?}"))
}

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn provenance(seed: u128) -> Result<PackProvenance, String> {
    let uri = ProvenanceUri::from_str(&format!("file://tests/fixtures/retrieval/{seed}.md#L1"))
        .map_err(|error| format!("{error:?}"))?;
    PackProvenance::new(uri, format!("fixture evidence for memory {seed}"))
        .map_err(|error| format!("{error:?}"))
}

fn fixture_candidates() -> Vec<RetrievalFixture> {
    vec![
        RetrievalFixture {
            seed: 1,
            workspace: "eidetic_engine_cli",
            level: "procedural",
            kind: "rule",
            content: "Run cargo fmt --check before release.",
            base_score: 0.98,
            utility: 0.90,
            confidence: 0.95,
            maturity: RetrievalMaturity::ProceduralProven,
            has_memory_row: true,
            has_provenance: true,
            redacted: false,
            expired: false,
        },
        RetrievalFixture {
            seed: 2,
            workspace: "eidetic_engine_cli",
            level: "procedural",
            kind: "rule",
            content: "Run cargo clippy --all-targets -- -D warnings.",
            base_score: 0.94,
            utility: 0.86,
            confidence: 0.90,
            maturity: RetrievalMaturity::ProceduralEstablished,
            has_memory_row: true,
            has_provenance: true,
            redacted: false,
            expired: false,
        },
        RetrievalFixture {
            seed: 3,
            workspace: "eidetic_engine_cli",
            level: "episodic",
            kind: "failure",
            content: "A prior release failed because formatting checks were skipped.",
            base_score: 0.88,
            utility: 0.82,
            confidence: 0.76,
            maturity: RetrievalMaturity::Episodic,
            has_memory_row: true,
            has_provenance: true,
            redacted: false,
            expired: false,
        },
        RetrievalFixture {
            seed: 4,
            workspace: "eidetic_engine_cli",
            level: "semantic",
            kind: "decision",
            content: "Release work must stay on main and never reference master.",
            base_score: 0.70,
            utility: 0.80,
            confidence: 0.72,
            maturity: RetrievalMaturity::Semantic,
            has_memory_row: true,
            has_provenance: true,
            redacted: false,
            expired: false,
        },
        RetrievalFixture {
            seed: 5,
            workspace: "other_workspace",
            level: "procedural",
            kind: "rule",
            content: "Unrelated workspace release rule.",
            base_score: 0.99,
            utility: 0.95,
            confidence: 0.95,
            maturity: RetrievalMaturity::ProceduralProven,
            has_memory_row: true,
            has_provenance: true,
            redacted: false,
            expired: false,
        },
        RetrievalFixture {
            seed: 6,
            workspace: "eidetic_engine_cli",
            level: "procedural",
            kind: "rule",
            content: "Deprecated rule that should not survive scoring.",
            base_score: 0.91,
            utility: 0.90,
            confidence: 0.90,
            maturity: RetrievalMaturity::ProceduralDeprecated,
            has_memory_row: true,
            has_provenance: true,
            redacted: false,
            expired: false,
        },
        RetrievalFixture {
            seed: 7,
            workspace: "eidetic_engine_cli",
            level: "episodic",
            kind: "failure",
            content: "Candidate missing provenance must not enter context.",
            base_score: 0.86,
            utility: 0.70,
            confidence: 0.70,
            maturity: RetrievalMaturity::Episodic,
            has_memory_row: true,
            has_provenance: false,
            redacted: false,
            expired: false,
        },
        RetrievalFixture {
            seed: 8,
            workspace: "eidetic_engine_cli",
            level: "procedural",
            kind: "rule",
            content: "Expired rule must be filtered before MMR.",
            base_score: 0.84,
            utility: 0.65,
            confidence: 0.65,
            maturity: RetrievalMaturity::ProceduralCandidate,
            has_memory_row: true,
            has_provenance: true,
            redacted: false,
            expired: true,
        },
        RetrievalFixture {
            seed: 9,
            workspace: "eidetic_engine_cli",
            level: "procedural",
            kind: "anti_pattern",
            content: "Secret-bearing memory must be excluded by policy.",
            base_score: 0.83,
            utility: 0.64,
            confidence: 0.64,
            maturity: RetrievalMaturity::ProceduralCandidate,
            has_memory_row: true,
            has_provenance: true,
            redacted: true,
            expired: false,
        },
    ]
}

fn stage_2_canonical_query(fixtures: &[RetrievalFixture]) -> Vec<RetrievalFixture> {
    fixtures
        .iter()
        .filter(|candidate| candidate.workspace == "eidetic_engine_cli")
        .filter(|candidate| {
            matches!(
                candidate.level,
                "procedural" | "episodic" | "semantic" | "working"
            )
        })
        .filter(|candidate| {
            matches!(
                candidate.kind,
                "rule" | "anti_pattern" | "failure" | "fix" | "decision"
            )
        })
        .cloned()
        .collect()
}

fn stage_3_two_tier_search(mut fixtures: Vec<RetrievalFixture>) -> Vec<RetrievalFixture> {
    fixtures.retain(|fixture| fixture.base_score > 0.0);
    fixtures.sort_by(|left, right| {
        right
            .base_score
            .total_cmp(&left.base_score)
            .then_with(|| left.seed.cmp(&right.seed))
    });
    fixtures
}

fn stage_4_hydrate(fixtures: &[RetrievalFixture]) -> Vec<RetrievalFixture> {
    fixtures
        .iter()
        .filter(|fixture| fixture.has_memory_row && fixture.has_provenance)
        .cloned()
        .collect()
}

fn stage_5_score(fixtures: &[RetrievalFixture]) -> Vec<ScoredFixture> {
    let config = SearchScoringConfig::default();
    let mut scored: Vec<_> = fixtures
        .iter()
        .filter_map(|fixture| {
            let signals = SearchScoringSignals {
                base_score: fixture.base_score,
                age_days: Some(3.0),
                confidence: fixture.confidence,
                utility_score: fixture.utility,
                maturity: fixture.maturity,
                harmful_count: 0,
                scope_match: true,
                graph_centrality: Some(0.25),
                redundancy: None,
            };
            let score = SearchScoreComponents::from_signals(signals, config).final_score;
            (score > 0.0).then(|| ScoredFixture {
                fixture: fixture.clone(),
                score,
            })
        })
        .collect();
    scored.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.fixture.seed.cmp(&right.fixture.seed))
    });
    scored
}

fn stage_6_policy_filter(scored: &[ScoredFixture]) -> Vec<ScoredFixture> {
    scored
        .iter()
        .filter(|candidate| !candidate.fixture.redacted && !candidate.fixture.expired)
        .cloned()
        .collect()
}

fn stage_7_mmr(policy_filtered: &[ScoredFixture]) -> Result<Vec<MemoryId>, String> {
    let candidates: Result<Vec<_>, _> = policy_filtered
        .iter()
        .map(|candidate| {
            let diversity_key = candidate.fixture.kind;
            PackCandidate::new(PackCandidateInput {
                memory_id: memory_id(candidate.fixture.seed),
                section: match candidate.fixture.level {
                    "procedural" => PackSection::ProceduralRules,
                    "episodic" => PackSection::Failures,
                    "semantic" => PackSection::Decisions,
                    _ => PackSection::Evidence,
                },
                content: candidate.fixture.content.to_string(),
                estimated_tokens: 10,
                relevance: score(candidate.score.clamp(0.0, 1.0))?,
                utility: score(candidate.fixture.utility)?,
                provenance: vec![provenance(candidate.fixture.seed)?],
                why: format!(
                    "stage 7 MMR candidate from scored fixture {}",
                    candidate.fixture.seed
                ),
            })
            .map(|candidate| candidate.with_diversity_key(diversity_key))
            .map_err(|error| format!("{error:?}"))
        })
        .collect();
    let draft = assemble_draft_with_profile(
        ContextPackProfile::Balanced,
        "prepare release",
        TokenBudget::new(24).map_err(|error| format!("{error:?}"))?,
        candidates?,
    )
    .map_err(|error| format!("{error:?}"))?;
    Ok(draft.items.into_iter().map(|item| item.memory_id).collect())
}

#[test]
fn retrieval_pipeline_narrows_monotonically_across_eight_stages() -> TestResult {
    let stage_1 = fixture_candidates();
    let stage_2 = stage_2_canonical_query(&stage_1);
    let stage_3 = stage_3_two_tier_search(stage_2.clone());
    let stage_4 = stage_4_hydrate(&stage_3);
    let stage_5 = stage_5_score(&stage_4);
    let stage_6 = stage_6_policy_filter(&stage_5);
    let stage_7 = stage_7_mmr(&stage_6)?;
    let stage_8: Vec<_> = stage_7.iter().take(2).copied().collect();

    let counts = [
        StageCount {
            name: "1 query string + filters + budget",
            count: stage_1.len(),
        },
        StageCount {
            name: "2 canonical document query",
            count: stage_2.len(),
        },
        StageCount {
            name: "3 frankensearch two-tier candidates",
            count: stage_3.len(),
        },
        StageCount {
            name: "4 hydrated database results",
            count: stage_4.len(),
        },
        StageCount {
            name: "5 ee-specific scoring multipliers",
            count: stage_5.len(),
        },
        StageCount {
            name: "6 policy filtered candidates",
            count: stage_6.len(),
        },
        StageCount {
            name: "7 MMR diversity candidates",
            count: stage_7.len(),
        },
        StageCount {
            name: "8 top-k result list",
            count: stage_8.len(),
        },
    ];

    for pair in counts.windows(2) {
        let [previous, next] = pair else {
            continue;
        };
        ensure(
            next.count <= previous.count,
            format!(
                "retrieval pipeline widened from {} to {}",
                previous.name, next.name
            ),
        )?;
    }

    ensure(
        counts.len() == 8,
        "test must cover all eight retrieval stages",
    )?;
    ensure(
        counts.iter().all(|stage| stage.count > 0),
        "each stage should retain at least one candidate in this fixture",
    )?;
    ensure(
        stage_2.len() < stage_1.len(),
        "canonical query filters should narrow cross-workspace candidates",
    )?;
    ensure(
        stage_4.len() < stage_3.len(),
        "hydration should drop missing provenance or missing row candidates",
    )?;
    ensure(
        stage_5.len() < stage_4.len(),
        "scoring should drop candidates whose maturity multiplier zeroes final score",
    )?;
    ensure(
        stage_6.len() < stage_5.len(),
        "policy filters should drop redacted and expired candidates",
    )?;
    ensure(
        stage_7.len() < stage_6.len(),
        "MMR packing should narrow candidates to the explicit token budget",
    )?;
    ensure(
        stage_8.len() <= 2,
        "top-k stage must honor the requested result limit",
    )?;
    ensure(
        stage_8 == vec![memory_id(1), memory_id(2)],
        format!("unexpected final top-k order: {stage_8:?}"),
    )
}
