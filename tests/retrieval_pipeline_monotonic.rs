//! Integration coverage for the eight-stage retrieval pipeline in plan section 13.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};
use std::str::FromStr;

use ee::models::{MemoryId, ProvenanceUri, UnitScore};
use ee::pack::{
    ContextPackProfile, PackCandidate, PackCandidateInput, PackDraft, PackProvenance, PackSection,
    PackSelectionPhase, TokenBudget, assemble_draft_with_profile,
};
use ee::search::scoring::{
    RetrievalMaturity, SearchScoreComponents, SearchScoringConfig, SearchScoringSignals,
};
use serde_json::Value;
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

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn run_ee_json(args: &[&str]) -> Result<Value, String> {
    let output = run_ee(args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("ee {} stdout was not UTF-8: {error}", args.join(" ")))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("ee {} stderr was not UTF-8: {error}", args.join(" ")))?;
    ensure(
        output.status.success(),
        format!(
            "ee {} should succeed; status={:?}; stdout={stdout}; stderr={stderr}",
            args.join(" "),
            output.status.code()
        ),
    )?;
    ensure(
        stderr.is_empty(),
        format!(
            "ee {} JSON stderr must be empty, got {stderr:?}",
            args.join(" ")
        ),
    )?;
    serde_json::from_str(&stdout).map_err(|error| {
        format!(
            "ee {} stdout must be JSON: {error}; stdout={stdout:?}",
            args.join(" ")
        )
    })
}

fn json_u64(value: &Value, pointer: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("JSON pointer {pointer} must be an unsigned integer"))
}

fn json_array<'a>(value: &'a Value, pointer: &str) -> Result<&'a [Value], String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("JSON pointer {pointer} must be an array"))
}

fn source_uri(workspace: &Path, filename: &str) -> String {
    format!("file://{}#L1", workspace.join(filename).display())
}

fn assert_search_results_have_why_and_provenance(results: &[Value]) -> TestResult {
    ensure(
        !results.is_empty(),
        "search should return at least one result",
    )?;
    for result in results {
        let doc_id = result
            .get("docId")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("search result missing docId: {result:?}"))?;
        ensure(
            result
                .get("why")
                .and_then(Value::as_str)
                .is_some_and(|why| !why.trim().is_empty()),
            format!("search result {doc_id} missing non-empty why"),
        )?;
        ensure(
            result
                .get("provenance")
                .and_then(Value::as_array)
                .is_some_and(|provenance| !provenance.is_empty()),
            format!("search result {doc_id} missing provenance"),
        )?;
    }
    Ok(())
}

fn assert_context_items_have_why_and_provenance(context: &Value, items: &[Value]) -> TestResult {
    ensure(!items.is_empty(), "context should return at least one item")?;
    let mut memory_ids = BTreeSet::new();
    let mut source_schemes = BTreeSet::new();
    let mut source_count = 0_u64;
    for item in items {
        let memory_id = item
            .get("memoryId")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("context item missing memoryId: {item:?}"))?;
        ensure(
            memory_ids.insert(memory_id),
            format!("context repeats memory {memory_id}"),
        )?;
        ensure(
            item.get("why")
                .and_then(Value::as_str)
                .is_some_and(|why| !why.trim().is_empty()),
            format!("context item {memory_id} missing non-empty why"),
        )?;
        let sources = json_array(item, "/provenance")?;
        ensure(
            !sources.is_empty(),
            format!("context item {memory_id} missing provenance"),
        )?;
        let source_index = json_u64(item, "/sourceIndex")?;
        ensure(
            source_index > 0 && source_index <= sources.len() as u64,
            format!("context item {memory_id} sourceIndex does not identify an inline source"),
        )?;
        for source in sources {
            let uri = source
                .get("uri")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("context item {memory_id} source has no URI"))?;
            let parsed = ProvenanceUri::from_str(uri)
                .map_err(|error| format!("context item {memory_id} has invalid URI: {error:?}"))?;
            ensure(
                source.get("scheme").and_then(Value::as_str) == Some(parsed.scheme()),
                format!("context item {memory_id} source scheme disagrees with its URI"),
            )?;
            source_schemes.insert(parsed.scheme().to_owned());
            source_count += 1;
        }
    }
    // The A1 pack contract keeps provenance inline and aggregate statistics
    // in the footer; the redundant provenanceFooter.entries array was removed.
    ensure(
        json_u64(context, "/data/pack/provenanceFooter/memoryCount")? == memory_ids.len() as u64,
        "context provenance footer memoryCount must match unique packed memories",
    )?;
    ensure(
        json_u64(context, "/data/pack/provenanceFooter/sourceCount")? == source_count,
        "context provenance footer sourceCount must match all inline sources",
    )?;
    let expected_schemes: Vec<_> = source_schemes.into_iter().map(Value::String).collect();
    ensure(
        json_array(context, "/data/pack/provenanceFooter/schemes")? == expected_schemes.as_slice(),
        "context provenance footer schemes must match the inline sources",
    )
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
            content: "Release work must stay on main and never create branches.",
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
                anchor_match: None,
                bead_affinity: None,
                freshness_drift: None,
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

fn stage_7_mmr(policy_filtered: &[ScoredFixture]) -> Result<PackDraft, String> {
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
    assemble_draft_with_profile(
        ContextPackProfile::Compact,
        "prepare release",
        TokenBudget::new(40).map_err(|error| format!("{error:?}"))?,
        candidates?,
    )
    .map_err(|error| format!("{error:?}"))
}

#[test]
fn retrieval_pipeline_narrows_monotonically_across_eight_stages() -> TestResult {
    let stage_1 = fixture_candidates();
    let stage_2 = stage_2_canonical_query(&stage_1);
    let stage_3 = stage_3_two_tier_search(stage_2.clone());
    let stage_4 = stage_4_hydrate(&stage_3);
    let stage_5 = stage_5_score(&stage_4);
    let stage_6 = stage_6_policy_filter(&stage_5);
    let draft = stage_7_mmr(&stage_6)?;
    let stage_7: Vec<_> = draft.items.iter().map(|item| item.memory_id).collect();
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
        stage_8 == vec![memory_id(3), memory_id(1)],
        format!("the reserved failure must precede the highest-scoring rule: {stage_8:?}"),
    )?;
    ensure(
        draft.items.first().is_some_and(|item| {
            item.selected_in == PackSelectionPhase::AntiPatternFirst
                && item.section == PackSection::Failures
                && item.why.starts_with("What NOT to do:")
        }),
        "the leading failure must explain its reserved selection phase",
    )?;
    ensure(
        draft.used_tokens <= draft.budget.max_tokens()
            && draft.used_tokens
                == draft
                    .items
                    .iter()
                    .map(|item| item.estimated_tokens)
                    .sum::<u32>(),
        "MMR token accounting must match selected items and stay within budget",
    )?;
    let eligible_ids: BTreeSet<_> = stage_6
        .iter()
        .map(|candidate| memory_id(candidate.fixture.seed))
        .collect();
    let selected_ids: BTreeSet<_> = stage_7.iter().copied().collect();
    ensure(
        selected_ids.len() == stage_7.len() && selected_ids.is_subset(&eligible_ids),
        "MMR must select each policy-eligible candidate at most once",
    )?;
    for item in &draft.items {
        let candidate = stage_6
            .iter()
            .find(|candidate| memory_id(candidate.fixture.seed) == item.memory_id)
            .ok_or_else(|| format!("selected memory {} was not policy-eligible", item.memory_id))?;
        ensure(
            item.provenance == vec![provenance(candidate.fixture.seed)?],
            "MMR must preserve each selected candidate's provenance",
        )?;
    }
    let mut reversed = stage_6.clone();
    reversed.reverse();
    ensure(
        stage_7_mmr(&reversed)?.items == draft.items,
        "MMR selection and explanations must be stable under candidate input reordering",
    )
}

#[test]
fn real_binary_search_context_pipeline_narrows_and_preserves_explanations() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace_path = tempdir.path();
    let workspace = workspace_path.to_string_lossy().into_owned();
    run_ee_json(&["--workspace", &workspace, "--json", "init"])?;

    let memories = [
        (
            "Run cargo fmt --check before release to keep generated artifacts stable.",
            "procedural",
            "rule",
            source_uri(workspace_path, "release-fmt.md"),
        ),
        (
            "Run cargo clippy --all-targets -- -D warnings before release.",
            "procedural",
            "rule",
            source_uri(workspace_path, "release-clippy.md"),
        ),
        (
            "A prior release failed when context provenance was missing from search results.",
            "episodic",
            "failure",
            source_uri(workspace_path, "release-provenance.md"),
        ),
        (
            "Unrelated note about database backup windows.",
            "semantic",
            "decision",
            source_uri(workspace_path, "backup-window.md"),
        ),
    ];

    for (content, level, kind, source) in &memories {
        run_ee_json(&[
            "--workspace",
            &workspace,
            "remember",
            content,
            "--level",
            level,
            "--kind",
            kind,
            "--source",
            source,
            "--json",
        ])?;
    }

    let rebuild = run_ee_json(&["--workspace", &workspace, "index", "rebuild", "--json"])?;
    ensure(
        rebuild
            .pointer("/data/memories_indexed")
            .and_then(Value::as_u64)
            .is_some_and(|count| count >= 4),
        format!("index rebuild should index remembered memories: {rebuild:?}"),
    )?;

    let broad_search = run_ee_json(&[
        "--workspace",
        &workspace,
        "search",
        "release",
        "--limit",
        "10",
        "--explain",
        "--json",
    ])?;
    let broad_results = json_array(&broad_search, "/data/results")?;
    assert_search_results_have_why_and_provenance(broad_results)?;

    let narrowed_search = run_ee_json(&[
        "--workspace",
        &workspace,
        "search",
        "cargo fmt release",
        "--limit",
        "2",
        "--explain",
        "--json",
    ])?;
    let narrowed_results = json_array(&narrowed_search, "/data/results")?;
    assert_search_results_have_why_and_provenance(narrowed_results)?;

    let context = run_ee_json(&[
        "--workspace",
        &workspace,
        "pack",
        "cargo fmt release",
        "--candidate-pool",
        "2",
        "--max-tokens",
        "160",
        "--profile",
        "compact",
        "--fields",
        "full",
        "--json",
    ])?;
    let context_items = json_array(&context, "/data/pack/items")?;
    assert_context_items_have_why_and_provenance(&context, context_items)?;

    let stored_count = memories.len();
    let broad_count = broad_results.len();
    let narrowed_count = narrowed_results.len();
    let context_count = context_items.len();

    ensure(
        broad_count <= stored_count,
        format!("search widened beyond stored memories: {broad_count} > {stored_count}"),
    )?;
    ensure(
        narrowed_count <= broad_count,
        format!("narrowed search widened result set: {narrowed_count} > {broad_count}"),
    )?;
    ensure(
        context_count <= narrowed_count,
        format!("context pack widened candidates: {context_count} > {narrowed_count}"),
    )?;
    Ok(())
}
