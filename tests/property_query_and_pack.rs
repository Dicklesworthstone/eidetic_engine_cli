use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use chrono::{DateTime, Duration, Utc};
use ee::models::{MemoryId, ProvenanceUri, RedactionLevel, UnitScore};
use ee::pack::{
    ContextPackProfile, PackAssemblyOptions, PackCandidate, PackCandidateInput, PackDraft,
    PackProvenance, PackSection, TokenBudget, assemble_draft,
    assemble_draft_with_profile_and_options_seeded,
};
use ee::runtime::determinism::Deterministic;
use ee::search::parse_search_query;
use proptest::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const REGRESSION_FIXTURE_SCHEMA: &str = "ee.proptest.regression.v1";
const REGRESSION_FIXTURE_CAPTURED_AT: &str = "1970-01-01T00:00:00Z";
const PROPTEST_RUN_EVENT_SCHEMA: &str = "ee.test_event.v1";
const PROPTEST_RUN_EVENT_KIND: &str = "proptest_run";
const PROPTEST_AXIS_COUNT: usize = 6;

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
    content_shape: u8,
) -> Result<PackCandidate, String> {
    let content = match content_shape % 4 {
        0 => "Shared property-test memory content.".to_string(),
        1 => format!("Property-test memory content {index}."),
        2 => format!("Property-test memory references /tmp/ee-pack-{index}/target/debug."),
        _ => format!("Property-test credential placeholder password=redact-me-{index}."),
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

fn candidates_from_specs(specs: Vec<(u32, u16, u16, u8)>) -> Result<Vec<PackCandidate>, String> {
    let mut candidates = Vec::with_capacity(specs.len());
    for (index, (tokens, relevance, utility, content_shape)) in specs.into_iter().enumerate() {
        candidates.push(candidate_from_spec(
            index,
            tokens,
            relevance,
            utility,
            content_shape,
        )?);
    }
    Ok(candidates)
}

fn candidate_specs() -> impl Strategy<Value = Vec<(u32, u16, u16, u8)>> {
    prop::collection::vec((1_u32..=250, 0_u16..=1000, 0_u16..=1000, 0_u8..=7), 0..32)
}

fn profile_for(raw: u8) -> ContextPackProfile {
    match raw % 4 {
        0 => ContextPackProfile::Compact,
        1 => ContextPackProfile::Balanced,
        2 => ContextPackProfile::Thorough,
        _ => ContextPackProfile::Submodular,
    }
}

fn redaction_level_for(raw: usize) -> RedactionLevel {
    RedactionLevel::all()[raw % RedactionLevel::all().len()]
}

fn pack_options() -> impl Strategy<Value = PackAssemblyOptions> {
    (
        any::<bool>(),
        any::<bool>(),
        0_usize..RedactionLevel::all().len(),
    )
        .prop_map(
            |(include_coverage_fill, output_redaction_enabled, redaction_level_raw)| {
                PackAssemblyOptions {
                    include_coverage_fill,
                    output_redaction_enabled,
                    redaction_level: redaction_level_for(redaction_level_raw),
                }
            },
        )
}

fn determinism_seed() -> impl Strategy<Value = u64> {
    prop_oneof![
        Just(0),
        Just(1),
        Just(42),
        Just(0xdead_beef),
        Just(u64::MAX),
        any::<u64>(),
    ]
}

fn deterministic_reordered<T>(mut values: Vec<T>, seed: u64) -> Vec<T> {
    let mut indexed = values.drain(..).enumerate().collect::<Vec<_>>();
    indexed.sort_by_key(|(index, _)| stable_test_word(seed ^ *index as u64));
    indexed.into_iter().map(|(_, value)| value).collect()
}

fn stable_test_word(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn draft_bytes(draft: &PackDraft) -> Vec<u8> {
    format!("{draft:#?}").into_bytes()
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
struct DeterminismRegressionFixture {
    schema: String,
    captured_at: String,
    last_verified_at: String,
    seed: u64,
    input: serde_json::Value,
    input_hash: String,
    expected_hash: String,
    observed_hash_run1: String,
    observed_hash_run2: String,
    first_diff_byte_offset: usize,
    first_diff_window: DeterminismDiffWindow,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
struct DeterminismDiffWindow {
    expected: String,
    observed: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
struct DeterminismProptestRunEvent {
    schema: String,
    kind: String,
    axes_count: usize,
    cases_sampled: usize,
    cases_passed: usize,
    cases_failed: usize,
    new_regressions: Vec<String>,
    stale_regressions_flagged: Vec<String>,
    elapsed_seconds: f64,
    budget_seconds: f64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
struct DeterminismRegressionInput {
    workspace_state: String,
    index_state: String,
    config: DeterminismRegressionConfig,
    query: String,
    seed: u64,
    candidate_specs: Vec<DeterminismRegressionCandidateSpec>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
struct DeterminismRegressionConfig {
    profile: String,
    max_tokens: u32,
    include_coverage_fill: bool,
    output_redaction_enabled: bool,
    redaction_level: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
struct DeterminismRegressionCandidateSpec {
    estimated_tokens: u32,
    relevance_raw: u16,
    utility_raw: u16,
    content_shape: u8,
}

fn regression_fixture_for_mismatch(
    seed: u64,
    input: &[u8],
    expected: &[u8],
    observed: &[u8],
) -> Option<DeterminismRegressionFixture> {
    let first_diff_byte_offset = first_diff_byte_offset(expected, observed)?;
    let input_hash = hash_bytes(input);
    Some(DeterminismRegressionFixture {
        schema: REGRESSION_FIXTURE_SCHEMA.to_string(),
        captured_at: REGRESSION_FIXTURE_CAPTURED_AT.to_string(),
        last_verified_at: REGRESSION_FIXTURE_CAPTURED_AT.to_string(),
        seed,
        input: regression_input_payload(input),
        input_hash: input_hash.clone(),
        expected_hash: hash_bytes(expected),
        observed_hash_run1: hash_bytes(expected),
        observed_hash_run2: hash_bytes(observed),
        first_diff_byte_offset,
        first_diff_window: DeterminismDiffWindow {
            expected: diff_window(expected, first_diff_byte_offset),
            observed: diff_window(observed, first_diff_byte_offset),
        },
    })
}

fn regression_input_payload(input: &[u8]) -> serde_json::Value {
    serde_json::from_slice(input)
        .unwrap_or_else(|_| serde_json::json!({ "raw": String::from_utf8_lossy(input) }))
}

fn regression_input_for_pack_case(
    query: String,
    budget: TokenBudget,
    profile: ContextPackProfile,
    options: PackAssemblyOptions,
    seed: u64,
    specs: &[(u32, u16, u16, u8)],
) -> Result<serde_json::Value, String> {
    let input = DeterminismRegressionInput {
        workspace_state: "synthetic_pack_candidates.v1".to_string(),
        index_state: "derived_from_candidate_specs.v1".to_string(),
        config: DeterminismRegressionConfig {
            profile: format!("{profile:?}"),
            max_tokens: budget.max_tokens(),
            include_coverage_fill: options.include_coverage_fill,
            output_redaction_enabled: options.output_redaction_enabled,
            redaction_level: format!("{:?}", options.redaction_level),
        },
        query,
        seed,
        candidate_specs: specs
            .iter()
            .map(
                |(estimated_tokens, relevance_raw, utility_raw, content_shape)| {
                    DeterminismRegressionCandidateSpec {
                        estimated_tokens: *estimated_tokens,
                        relevance_raw: *relevance_raw,
                        utility_raw: *utility_raw,
                        content_shape: *content_shape,
                    }
                },
            )
            .collect(),
    };

    serde_json::to_value(input).map_err(|error| error.to_string())
}

fn regression_fixture_for_pack_case_mismatch(
    query: String,
    budget: TokenBudget,
    profile: ContextPackProfile,
    options: PackAssemblyOptions,
    seed: u64,
    specs: &[(u32, u16, u16, u8)],
    expected: &[u8],
    observed: &[u8],
) -> Result<Option<DeterminismRegressionFixture>, String> {
    let input = regression_input_for_pack_case(query, budget, profile, options, seed, specs)?;
    let input_bytes = serde_json::to_vec(&input).map_err(|error| error.to_string())?;
    Ok(regression_fixture_for_mismatch(
        seed,
        &input_bytes,
        expected,
        observed,
    ))
}

fn regression_fixture_file_name(input_hash: &str) -> String {
    let input_hash_hex = input_hash.trim_start_matches("blake3:");
    format!("{}.json", &input_hash_hex[..16])
}

fn serialize_regression_fixture(fixture: &DeterminismRegressionFixture) -> Result<String, String> {
    Ok(serde_json::to_string_pretty(fixture).map_err(|error| error.to_string())? + "\n")
}

fn persist_regression_fixture(
    dir: &Path,
    fixture: &DeterminismRegressionFixture,
) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let path = dir.join(regression_fixture_file_name(&fixture.input_hash));
    fs::write(&path, serialize_regression_fixture(fixture)?)
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(path)
}

fn load_regression_fixtures(dir: &Path) -> Result<Vec<DeterminismRegressionFixture>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(|error| format!("read {}: {error}", dir.display()))? {
        let entry = entry.map_err(|error| format!("read {} entry: {error}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("non-utf8 fixture path: {}", path.display()))?
            .to_string();
        let content = fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        entries.push((file_name, content));
    }

    parse_regression_fixture_entries(entries)
}

fn parse_regression_fixture_entries(
    mut entries: Vec<(String, String)>,
) -> Result<Vec<DeterminismRegressionFixture>, String> {
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut fixtures = Vec::with_capacity(entries.len());
    for (file_name, content) in entries {
        if !file_name.ends_with(".json") {
            continue;
        }
        let fixture: DeterminismRegressionFixture = serde_json::from_str(&content)
            .map_err(|error| format!("parse {file_name}: {error}"))?;
        if fixture.schema != REGRESSION_FIXTURE_SCHEMA {
            return Err(format!(
                "parse {file_name}: unsupported schema {}",
                fixture.schema
            ));
        }
        fixtures.push(fixture);
    }
    Ok(fixtures)
}

fn stale_regression_fixture_hashes(
    fixtures: &[DeterminismRegressionFixture],
    now: &str,
    stale_after_days: i64,
) -> Result<Vec<String>, String> {
    let now = parse_fixture_timestamp("now", now)?;
    let stale_after = Duration::days(stale_after_days);
    let mut stale = Vec::new();
    for fixture in fixtures {
        let last_verified_at =
            parse_fixture_timestamp(&fixture.input_hash, &fixture.last_verified_at)?;
        if now.signed_duration_since(last_verified_at) > stale_after {
            stale.push(fixture.input_hash.clone());
        }
    }
    stale.sort();
    Ok(stale)
}

fn parse_fixture_timestamp(label: &str, value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|error| format!("parse {label} timestamp {value:?}: {error}"))
}

fn proptest_run_event(
    cases_sampled: usize,
    cases_failed: usize,
    new_regressions: &[DeterminismRegressionFixture],
    mut stale_regressions_flagged: Vec<String>,
    elapsed_seconds: f64,
    budget_seconds: f64,
) -> Result<DeterminismProptestRunEvent, String> {
    if cases_failed > cases_sampled {
        return Err(format!(
            "cases_failed {cases_failed} exceeds cases_sampled {cases_sampled}"
        ));
    }
    if !elapsed_seconds.is_finite() || elapsed_seconds < 0.0 {
        return Err(format!("invalid elapsed_seconds {elapsed_seconds}"));
    }
    if !budget_seconds.is_finite() || budget_seconds < 0.0 {
        return Err(format!("invalid budget_seconds {budget_seconds}"));
    }

    let mut new_regressions = new_regressions
        .iter()
        .map(|fixture| fixture.input_hash.clone())
        .collect::<Vec<_>>();
    new_regressions.sort();
    stale_regressions_flagged.sort();

    Ok(DeterminismProptestRunEvent {
        schema: PROPTEST_RUN_EVENT_SCHEMA.to_string(),
        kind: PROPTEST_RUN_EVENT_KIND.to_string(),
        axes_count: PROPTEST_AXIS_COUNT,
        cases_sampled,
        cases_passed: cases_sampled - cases_failed,
        cases_failed,
        new_regressions,
        stale_regressions_flagged,
        elapsed_seconds,
        budget_seconds,
    })
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn first_diff_byte_offset(left: &[u8], right: &[u8]) -> Option<usize> {
    let shared_len = left.len().min(right.len());
    for index in 0..shared_len {
        if left[index] != right[index] {
            return Some(index);
        }
    }
    (left.len() != right.len()).then_some(shared_len)
}

fn diff_window(bytes: &[u8], offset: usize) -> String {
    let start = offset.saturating_sub(16);
    let end = bytes.len().min(offset.saturating_add(17));
    String::from_utf8_lossy(&bytes[start..end]).into_owned()
}

#[test]
fn seeded_pack_assembly_replays_edge_seeds_and_option_axes() -> Result<(), String> {
    let specs = vec![
        (12, 980, 700, 1),
        (18, 920, 600, 0),
        (24, 850, 900, 2),
        (15, 810, 500, 3),
        (21, 760, 650, 5),
    ];
    let candidates = candidates_from_specs(specs)?;
    let budget = TokenBudget::new(64).map_err(|error| format!("{error:?}"))?;
    let profiles = [
        ContextPackProfile::Compact,
        ContextPackProfile::Balanced,
        ContextPackProfile::Thorough,
        ContextPackProfile::Submodular,
    ];
    let option_axes = [
        PackAssemblyOptions::default(),
        PackAssemblyOptions {
            include_coverage_fill: false,
            ..PackAssemblyOptions::default()
        },
        PackAssemblyOptions {
            output_redaction_enabled: false,
            redaction_level: RedactionLevel::None,
            ..PackAssemblyOptions::default()
        },
        PackAssemblyOptions {
            redaction_level: RedactionLevel::Strict,
            ..PackAssemblyOptions::default()
        },
    ];

    for seed in [0, 1, 42, 0xdead_beef, u64::MAX] {
        for profile in profiles {
            for options in option_axes {
                let first = assemble_draft_with_profile_and_options_seeded(
                    profile,
                    "property pack deterministic edge seed",
                    budget,
                    candidates.clone(),
                    options,
                    &Deterministic::from_seed(seed),
                )
                .map_err(|error| format!("{error:?}"))?;
                let replay = assemble_draft_with_profile_and_options_seeded(
                    profile,
                    "property pack deterministic edge seed",
                    budget,
                    candidates.clone(),
                    options,
                    &Deterministic::from_seed(seed),
                )
                .map_err(|error| format!("{error:?}"))?;
                let reordered = assemble_draft_with_profile_and_options_seeded(
                    profile,
                    "property pack deterministic edge seed",
                    budget,
                    deterministic_reordered(candidates.clone(), seed),
                    options,
                    &Deterministic::from_seed(seed),
                )
                .map_err(|error| format!("{error:?}"))?;

                assert_eq!(draft_bytes(&first), draft_bytes(&replay));
                assert_eq!(draft_bytes(&first), draft_bytes(&reordered));
            }
        }
    }

    Ok(())
}

#[test]
fn determinism_regression_fixture_metadata_is_stable() -> Result<(), String> {
    let input = br#"{"query":"release","profile":"compact","seed":42}"#;
    let expected = br#"{"pack":{"hash":"blake3:expected","items":[1,2]}}"#;
    let observed = br#"{"pack":{"hash":"blake3:observed","items":[1,2]}}"#;

    let first = regression_fixture_for_mismatch(42, input, expected, observed)
        .ok_or_else(|| "fixture should detect mismatch".to_owned())?;
    let replay = regression_fixture_for_mismatch(42, input, expected, observed)
        .ok_or_else(|| "fixture should detect mismatch".to_owned())?;

    assert_eq!(first, replay);
    assert_eq!(first.schema, "ee.proptest.regression.v1");
    assert_eq!(first.captured_at, REGRESSION_FIXTURE_CAPTURED_AT);
    assert_eq!(first.last_verified_at, REGRESSION_FIXTURE_CAPTURED_AT);
    assert_eq!(first.seed, 42);
    assert_eq!(first.input["query"], "release");
    assert_eq!(first.input["profile"], "compact");
    let file_name = regression_fixture_file_name(&first.input_hash);
    assert!(file_name.ends_with(".json"));
    assert_eq!(file_name.len(), 21);
    assert_ne!(first.expected_hash, first.observed_hash_run2);
    assert_eq!(first.expected_hash, first.observed_hash_run1);
    assert!(first.first_diff_byte_offset > 0);
    assert!(first.first_diff_window.expected.contains("expected"));
    assert!(first.first_diff_window.observed.contains("observed"));
    Ok(())
}

#[test]
fn determinism_regression_fixture_handles_prefix_drift() -> Result<(), String> {
    let fixture = regression_fixture_for_mismatch(
        7,
        b"input",
        b"same-prefix-left",
        b"same-prefix-left-and-longer",
    )
    .ok_or_else(|| "fixture should detect length drift".to_owned())?;

    assert_eq!(fixture.first_diff_byte_offset, b"same-prefix-left".len());
    assert_eq!(fixture.input["raw"], "input");
    assert_eq!(fixture.first_diff_window.expected, "same-prefix-left");
    assert!(fixture.first_diff_window.observed.ends_with("-and-longer"));
    Ok(())
}

#[test]
fn determinism_regression_fixture_json_roundtrips() -> Result<(), String> {
    let fixture = regression_fixture_for_mismatch(99, b"input", b"expected", b"observed")
        .ok_or_else(|| "fixture should detect mismatch".to_owned())?;
    let rendered = serialize_regression_fixture(&fixture)?;
    let parsed: DeterminismRegressionFixture =
        serde_json::from_str(&rendered).map_err(|error| error.to_string())?;

    assert_eq!(parsed, fixture);
    assert!(rendered.ends_with('\n'));
    Ok(())
}

#[test]
fn determinism_regression_fixture_captures_structured_pack_input() -> Result<(), String> {
    let specs = vec![(8, 900, 700, 1), (13, 500, 950, 3)];
    let budget = TokenBudget::new(64).map_err(|error| format!("{error:?}"))?;
    let options = PackAssemblyOptions {
        include_coverage_fill: false,
        output_redaction_enabled: true,
        redaction_level: RedactionLevel::Strict,
    };

    let fixture = regression_fixture_for_pack_case_mismatch(
        "structured replay".to_string(),
        budget,
        ContextPackProfile::Balanced,
        options,
        42,
        &specs,
        br#"{"pack":{"hash":"blake3:expected"}}"#,
        br#"{"pack":{"hash":"blake3:observed"}}"#,
    )?
    .ok_or_else(|| "fixture should detect mismatch".to_owned())?;
    let replay = regression_fixture_for_pack_case_mismatch(
        "structured replay".to_string(),
        budget,
        ContextPackProfile::Balanced,
        options,
        42,
        &specs,
        br#"{"pack":{"hash":"blake3:expected"}}"#,
        br#"{"pack":{"hash":"blake3:observed"}}"#,
    )?
    .ok_or_else(|| "fixture should detect mismatch".to_owned())?;

    assert_eq!(fixture, replay);
    assert_eq!(
        fixture.input["workspace_state"],
        "synthetic_pack_candidates.v1"
    );
    assert_eq!(
        fixture.input["index_state"],
        "derived_from_candidate_specs.v1"
    );
    assert_eq!(fixture.input["query"], "structured replay");
    assert_eq!(fixture.input["seed"], 42);
    assert_eq!(fixture.input["config"]["profile"], "Balanced");
    assert_eq!(fixture.input["config"]["max_tokens"], 64);
    assert_eq!(fixture.input["config"]["include_coverage_fill"], false);
    assert_eq!(fixture.input["config"]["output_redaction_enabled"], true);
    assert_eq!(fixture.input["config"]["redaction_level"], "Strict");
    assert_eq!(fixture.input["candidate_specs"][0]["estimated_tokens"], 8);
    assert_eq!(fixture.input["candidate_specs"][1]["content_shape"], 3);
    Ok(())
}

#[test]
fn determinism_regression_fixture_loader_replays_sorted_json_entries() -> Result<(), String> {
    let first = regression_fixture_for_mismatch(1, b"first-input", b"expected-a", b"observed-a")
        .ok_or_else(|| "first fixture should detect mismatch".to_owned())?;
    let second = regression_fixture_for_mismatch(2, b"second-input", b"expected-b", b"observed-b")
        .ok_or_else(|| "second fixture should detect mismatch".to_owned())?;
    let first_name = regression_fixture_file_name(&first.input_hash);
    let second_name = regression_fixture_file_name(&second.input_hash);
    let mut expected = vec![
        (first_name.clone(), first.clone()),
        (second_name.clone(), second.clone()),
    ];
    expected.sort_by(|(left, _), (right, _)| left.cmp(right));
    let expected = expected
        .into_iter()
        .map(|(_, fixture)| fixture)
        .collect::<Vec<_>>();

    let loaded = parse_regression_fixture_entries(vec![
        ("ignored.txt".to_string(), "{}".to_string()),
        (second_name, serialize_regression_fixture(&second)?),
        (first_name, serialize_regression_fixture(&first)?),
    ])?;

    assert_eq!(loaded, expected);
    Ok(())
}

#[test]
fn determinism_regression_fixture_loader_tolerates_missing_dir() -> Result<(), String> {
    let missing = Path::new("tests/fixtures/proptest_regressions");
    if missing.exists() {
        return Ok(());
    }

    let loaded = load_regression_fixtures(missing)?;
    assert!(loaded.is_empty());
    Ok(())
}

#[test]
fn determinism_regression_fixture_persists_to_stable_file_name() -> Result<(), String> {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fixture = regression_fixture_for_mismatch(
        123,
        br#"{"query":"stable writeback","seed":123}"#,
        br#"{"pack":{"items":["a"]}}"#,
        br#"{"pack":{"items":["b"]}}"#,
    )
    .ok_or_else(|| "fixture should detect mismatch".to_owned())?;

    let first_path = persist_regression_fixture(tempdir.path(), &fixture)?;
    let second_path = persist_regression_fixture(tempdir.path(), &fixture)?;
    let expected_name = regression_fixture_file_name(&fixture.input_hash);

    assert_eq!(first_path, second_path);
    assert_eq!(
        first_path.file_name().and_then(|name| name.to_str()),
        Some(expected_name.as_str())
    );

    let loaded = load_regression_fixtures(tempdir.path())?;
    assert_eq!(loaded, vec![fixture]);
    Ok(())
}

#[test]
fn determinism_regression_fixture_staleness_detection_is_stable() -> Result<(), String> {
    let mut fresh =
        regression_fixture_for_mismatch(1, b"fresh", b"expected-fresh", b"observed-fresh")
            .ok_or_else(|| "fresh fixture should detect mismatch".to_owned())?;
    fresh.last_verified_at = "2026-05-01T00:00:00Z".to_string();

    let mut stale =
        regression_fixture_for_mismatch(2, b"stale", b"expected-stale", b"observed-stale")
            .ok_or_else(|| "stale fixture should detect mismatch".to_owned())?;
    stale.last_verified_at = "2026-01-01T00:00:00Z".to_string();

    let stale_hashes =
        stale_regression_fixture_hashes(&[fresh, stale.clone()], "2026-05-16T00:00:00Z", 90)?;

    assert_eq!(stale_hashes, vec![stale.input_hash]);
    Ok(())
}

#[test]
fn determinism_regression_fixture_staleness_rejects_invalid_timestamp() -> Result<(), String> {
    let mut fixture = regression_fixture_for_mismatch(3, b"invalid-date", b"expected", b"observed")
        .ok_or_else(|| "fixture should detect mismatch".to_owned())?;
    fixture.last_verified_at = "not-a-date".to_string();

    let error = stale_regression_fixture_hashes(&[fixture], "2026-05-16T00:00:00Z", 90)
        .expect_err("invalid fixture timestamp should be rejected");

    assert!(error.contains("not-a-date"));
    Ok(())
}

#[test]
fn determinism_proptest_run_event_is_stable_and_sorted() -> Result<(), String> {
    let second =
        regression_fixture_for_mismatch(2, b"second", b"expected-second", b"observed-second")
            .ok_or_else(|| "second fixture should detect mismatch".to_owned())?;
    let first = regression_fixture_for_mismatch(1, b"first", b"expected-first", b"observed-first")
        .ok_or_else(|| "first fixture should detect mismatch".to_owned())?;

    let event = proptest_run_event(
        1024,
        2,
        &[second.clone(), first.clone()],
        vec![second.input_hash.clone(), first.input_hash.clone()],
        12.5,
        60.0,
    )?;
    let rendered = serde_json::to_string(&event).map_err(|error| error.to_string())?;
    let replay: DeterminismProptestRunEvent =
        serde_json::from_str(&rendered).map_err(|error| error.to_string())?;

    let mut expected_hashes = vec![first.input_hash, second.input_hash];
    expected_hashes.sort();
    assert_eq!(event, replay);
    assert_eq!(event.schema, "ee.test_event.v1");
    assert_eq!(event.kind, "proptest_run");
    assert_eq!(event.axes_count, 6);
    assert_eq!(event.cases_sampled, 1024);
    assert_eq!(event.cases_passed, 1022);
    assert_eq!(event.cases_failed, 2);
    assert_eq!(event.new_regressions, expected_hashes);
    assert_eq!(event.stale_regressions_flagged, event.new_regressions);
    Ok(())
}

#[test]
fn determinism_proptest_run_event_rejects_impossible_counts_and_times() -> Result<(), String> {
    let count_error = proptest_run_event(1, 2, &[], Vec::new(), 0.0, 60.0)
        .expect_err("failed cases above sampled cases should be rejected");
    assert!(count_error.contains("cases_failed 2 exceeds cases_sampled 1"));

    let elapsed_error = proptest_run_event(1, 0, &[], Vec::new(), f64::NAN, 60.0)
        .expect_err("non-finite elapsed time should be rejected");
    assert!(elapsed_error.contains("invalid elapsed_seconds"));

    let budget_error = proptest_run_event(1, 0, &[], Vec::new(), 1.0, -1.0)
        .expect_err("negative budget should be rejected");
    assert!(budget_error.contains("invalid budget_seconds"));
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

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
        let candidates = candidates_from_specs(specs).map_err(TestCaseError::fail)?;

        let draft = assemble_draft("property pack budget", budget, candidates)
            .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        let selected_tokens: u32 = draft.items.iter().map(|item| item.estimated_tokens).sum();

        prop_assert_eq!(draft.used_tokens, selected_tokens);
        prop_assert!(draft.used_tokens <= budget.max_tokens());
        prop_assert_eq!(draft.selection_audit.budget_used, draft.used_tokens);
        prop_assert_eq!(draft.selection_audit.budget_limit, budget.max_tokens());
        prop_assert_eq!(
            draft.selection_audit.selected_count,
            draft.items.len(),
        );
        prop_assert_eq!(
            draft.selection_audit.omitted_count,
            draft.omitted.len(),
        );
        for item in &draft.items {
            prop_assert!(item.estimated_tokens > 0);
        }
        for omission in &draft.omitted {
            prop_assert!(omission.estimated_tokens > 0);
        }
        for step in &draft.selection_audit.steps {
            prop_assert!(step.objective_value.is_finite());
            prop_assert!(step.token_cost > 0);
        }
    }

    #[test]
    fn seeded_pack_assembly_replays_byte_identical_output(
        budget_raw in 1_u32..=400,
        seed in determinism_seed(),
        profile_raw in any::<u8>(),
        options in pack_options(),
        query_suffix in "[a-z0-9 _-]{0,48}",
        specs in candidate_specs(),
    ) {
        let budget = TokenBudget::new(budget_raw)
            .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        let candidates = candidates_from_specs(specs).map_err(TestCaseError::fail)?;
        let query = format!("property pack determinism {query_suffix}");
        let profile = profile_for(profile_raw);

        let first = assemble_draft_with_profile_and_options_seeded(
            profile,
            query.clone(),
            budget,
            candidates.clone(),
            options,
            &Deterministic::from_seed(seed),
        )
        .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        let replay = assemble_draft_with_profile_and_options_seeded(
            profile,
            query.clone(),
            budget,
            candidates.clone(),
            options,
            &Deterministic::from_seed(seed),
        )
        .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        let reordered = assemble_draft_with_profile_and_options_seeded(
            profile,
            query,
            budget,
            deterministic_reordered(candidates, seed),
            options,
            &Deterministic::from_seed(seed),
        )
        .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;

        prop_assert_eq!(draft_bytes(&first), draft_bytes(&replay));
        prop_assert_eq!(draft_bytes(&first), draft_bytes(&reordered));
    }
}
