use ee::core::profile::{OperatingProfile, RuntimeProfileReport};
use ee::core::search::{
    RERANK_MODEL_UNAVAILABLE_ADVISORY, RERANK_MODEL_UNAVAILABLE_REPAIR,
    SEARCH_ADVISORY_SCOPE_PROCESS, ScoreSource, SearchAdvisorySession, SearchDegradation,
    SearchHit, SearchReport, SearchSourceMode, SearchStatus,
};
use ee::models::{EmbedBackend, MemoryScope, MemoryScopeStats};

const DEGRADED_CODES_DOC: &str = include_str!("../docs/degraded_codes.md");
const DEGRADED_CODES_DOC_BUILDER: &str = include_str!("../scripts/build_degraded_codes_doc.sh");

fn runtime_profile() -> RuntimeProfileReport {
    RuntimeProfileReport::for_profile(OperatingProfile::Swarm, "rerank-posture-contract")
}

fn scope_stats() -> MemoryScopeStats {
    MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0)
}

#[test]
fn generated_catalog_preserves_the_exact_offline_reranker_repair() {
    assert!(
        DEGRADED_CODES_DOC.contains(&format!(
            "**Repair hint.** {RERANK_MODEL_UNAVAILABLE_REPAIR}"
        )),
        "the agent-facing catalog must publish the exact actionable offline repair"
    );
    assert!(
        DEGRADED_CODES_DOC_BUILDER.contains(
            "repair_string=\"$(jq -r '.expected_emission.repair_string // \"\"' \"$fixture\")\"",
        ),
        "regeneration must prefer the pinned repair_string over repair_contains"
    );
}

fn empty_search_report(
    degraded: Vec<SearchDegradation>,
    rerank_runtime_available: bool,
) -> SearchReport {
    SearchReport {
        status: SearchStatus::NoResults,
        embed_backend: EmbedBackend::HashFallback,
        query: "release formatting policy".to_string(),
        requested_limit: 10,
        results: Vec::<SearchHit>::new(),
        elapsed_ms: 0.0,
        errors: Vec::new(),
        degraded,
        runtime_profile: runtime_profile(),
        rerank_configured_mode: ee::config::SearchRerankMode::Auto,
        rerank_configured_top_k: 50,
        rerank_runtime_available,
        relevance_floor_applied: Some(0.0),
        candidates_below_floor: 0,
        query_assist: None,
        source_mode_requested: SearchSourceMode::Hybrid,
        source_mode_applied: SearchSourceMode::Hybrid,
        source_mode_fallback: false,
        strict_source_mode: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        scope_stats: scope_stats(),
    }
}

#[test]
fn ordinary_search_json_is_pure_and_structural() {
    let report = empty_search_report(
        vec![
            SearchDegradation {
                code: "rerank_model_unavailable".to_string(),
                severity: "low".to_string(),
                message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_string(),
                repair: Some(RERANK_MODEL_UNAVAILABLE_REPAIR.to_owned()),
            },
            SearchDegradation {
                code: "rerank_model_unavailable".to_string(),
                severity: "low".to_string(),
                message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_string(),
                repair: Some(RERANK_MODEL_UNAVAILABLE_REPAIR.to_owned()),
            },
        ],
        false,
    );

    let human = report.human_summary();
    let json = report.data_json();
    let repeated_json = report.data_json();

    assert_eq!(json["resultCount"], 0);
    assert_eq!(json["rerank"]["schema"], "ee.rerank_posture.v1");
    assert_eq!(json["rerank"]["mode"], "fusion_only_degraded");
    assert_eq!(json["rerank"]["configured"], "auto");
    assert_eq!(json["rerank"]["topK"], 50);
    assert_eq!(json["rerank"]["available"], false);
    assert_eq!(json["rerank"]["rerankScoreCount"], 0);
    assert_eq!(
        json["rerank"]["scoreKind"],
        ScoreSource::Hybrid.score_kind()
    );
    assert_eq!(json["rerank"]["degradedCode"], "rerank_model_unavailable");
    assert!(json["rerank"].get("permanent").is_none());
    assert_eq!(
        json["rerank"]["advisory"]["code"],
        "rerank_model_unavailable"
    );
    assert_eq!(json["rerank"]["advisory"]["permanent"], true);
    assert_eq!(
        json["rerank"]["advisory"]["repair"],
        RERANK_MODEL_UNAVAILABLE_REPAIR
    );
    assert!(json["rerank"]["advisorySummary"].get("schema").is_none());
    assert_eq!(
        json["rerank"]["advisorySummary"]["scope"],
        SEARCH_ADVISORY_SCOPE_PROCESS
    );
    assert_eq!(json["rerank"]["advisorySummary"]["emittedCount"], 1);
    assert_eq!(json["rerank"]["advisorySummary"]["suppressedCount"], 0);
    assert_eq!(repeated_json["rerank"], json["rerank"]);
    assert_eq!(
        json["degraded"].as_array().map_or(0, |rows| rows.len()),
        0,
        "permanent capability posture must not repeat in per-query degraded output"
    );
    assert!(!human.contains(RERANK_MODEL_UNAVAILABLE_ADVISORY));
    assert!(!human.contains(RERANK_MODEL_UNAVAILABLE_REPAIR));
}

#[test]
fn explicit_long_lived_session_emits_permanent_advisory_once() {
    let first = empty_search_report(
        vec![SearchDegradation {
            code: "rerank_model_unavailable".to_string(),
            severity: "low".to_string(),
            message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_string(),
            repair: Some(RERANK_MODEL_UNAVAILABLE_REPAIR.to_owned()),
        }],
        false,
    );
    let mut second = first.clone();
    second.query = "a distinct second query in the same process".to_string();
    let mut session = SearchAdvisorySession::default();

    let first_json = first.data_json_with_advisory_session(&mut session);
    let second_json = second.data_json_with_advisory_session(&mut session);

    assert_eq!(first_json["rerank"]["advisory"]["permanent"], true);
    assert_eq!(
        first_json["rerank"]["advisorySummary"]["scope"],
        SEARCH_ADVISORY_SCOPE_PROCESS
    );
    assert_eq!(first_json["rerank"]["advisorySummary"]["emittedCount"], 1);
    assert_eq!(
        first_json["rerank"]["advisorySummary"]["suppressedCount"],
        0
    );
    assert!(second_json["rerank"]["advisory"].is_null());
    assert_eq!(second_json["rerank"]["advisorySummary"]["permanent"], true);
    assert_eq!(second_json["rerank"]["advisorySummary"]["emittedCount"], 0);
    assert_eq!(
        second_json["rerank"]["advisorySummary"]["suppressedCount"],
        1
    );
    assert_eq!(
        second_json["rerank"]["advisorySummary"]["sessionOccurrenceCount"],
        2
    );
    assert_eq!(
        second_json["rerank"]["advisorySummary"]["sessionSuppressedCount"],
        1
    );
}

#[test]
fn structural_session_keeps_permanent_advisory_consumed_after_recovery() {
    // This pins the pure renderer state machine only. The daemon UDS suite
    // contains the archive-gated absent -> verified import -> absent runtime
    // proof; this fixture is not evidence that the gated path executed.
    let absent = empty_search_report(
        vec![SearchDegradation {
            code: "rerank_model_unavailable".to_string(),
            severity: "low".to_string(),
            message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_string(),
            repair: Some(RERANK_MODEL_UNAVAILABLE_REPAIR.to_owned()),
        }],
        false,
    );
    let available = empty_search_report(Vec::new(), true);
    let mut session = SearchAdvisorySession::default();

    let first_absent = absent.data_json_with_advisory_session(&mut session);
    let recovered = available.data_json_with_advisory_session(&mut session);
    let second_absent = absent.data_json_with_advisory_session(&mut session);

    assert_eq!(
        first_absent["rerank"]["advisory"]["code"],
        "rerank_model_unavailable"
    );
    assert!(recovered["rerank"]["advisory"].is_null());
    assert_eq!(recovered["resultCount"], 0);
    assert_eq!(recovered["rerank"]["rerankScoreCount"], 0);
    assert_eq!(recovered["rerank"]["available"], true);
    assert_eq!(recovered["rerank"]["mode"], "fusion_only");
    assert!(second_absent["rerank"]["advisory"].is_null());
    assert_eq!(
        second_absent["rerank"]["advisorySummary"]["suppressedCount"],
        1
    );
    assert_eq!(
        second_absent["rerank"]["advisorySummary"]["sessionOccurrenceCount"],
        2
    );
}

#[test]
fn large_gap_advisory_rearms_after_ready_response() {
    let large_gap = empty_search_report(
        vec![
            SearchDegradation {
                code: "search_index_stale".to_string(),
                severity: "medium".to_string(),
                message: "Search index is stale.".to_string(),
                repair: Some("ee index rebuild --workspace .".to_string()),
            },
            SearchDegradation {
                code: "search_index_large_gap".to_string(),
                severity: "medium".to_string(),
                message: "Search index generation gap is large.".to_string(),
                repair: Some("ee index rebuild --workspace .".to_string()),
            },
        ],
        false,
    );
    let ready = empty_search_report(Vec::new(), false);
    let mut session = SearchAdvisorySession::default();

    let first_gap = large_gap.data_json_with_advisory_session(&mut session);
    let repeated_gap = large_gap.data_json_with_advisory_session(&mut session);
    let ready_response = ready.data_json_with_advisory_session(&mut session);
    let new_gap = large_gap.data_json_with_advisory_session(&mut session);

    assert!(first_gap["degraded"].as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry["code"] == "search_index_large_gap")
    }));
    assert!(repeated_gap["degraded"].as_array().is_some_and(|entries| {
        entries
            .iter()
            .all(|entry| entry["code"] != "search_index_large_gap")
    }));
    assert_eq!(ready_response["degraded"], serde_json::json!([]));
    assert!(new_gap["degraded"].as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry["code"] == "search_index_large_gap")
    }));
}

#[test]
fn explicit_session_emits_each_distinct_permanent_advisory_once() {
    let first = empty_search_report(
        vec![SearchDegradation {
            code: "rerank_model_unavailable".to_string(),
            severity: "low".to_string(),
            message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_string(),
            repair: Some(RERANK_MODEL_UNAVAILABLE_REPAIR.to_owned()),
        }],
        false,
    );
    let second = empty_search_report(
        vec![SearchDegradation {
            code: "rerank_model_unavailable".to_string(),
            severity: "warning".to_string(),
            message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_string(),
            repair: Some(RERANK_MODEL_UNAVAILABLE_REPAIR.to_owned()),
        }],
        false,
    );
    let repeated_first = first.clone();
    let mut session = SearchAdvisorySession::default();

    let first_json = first.data_json_with_advisory_session(&mut session);
    let second_json = second.data_json_with_advisory_session(&mut session);
    let repeated_first_json = repeated_first.data_json_with_advisory_session(&mut session);

    assert_eq!(first_json["rerank"]["advisory"]["severity"], "low");
    assert_eq!(second_json["rerank"]["advisory"]["severity"], "warning");
    assert_eq!(second_json["rerank"]["advisorySummary"]["distinctCount"], 2);
    assert!(repeated_first_json["rerank"]["advisory"].is_null());
    assert_eq!(
        repeated_first_json["rerank"]["advisorySummary"]["distinctCount"],
        2
    );
    assert_eq!(
        repeated_first_json["rerank"]["advisorySummary"]["sessionSuppressedCount"],
        1
    );
}

#[test]
fn transient_reranker_load_failure_remains_a_query_degradation() {
    let permanent = empty_search_report(
        vec![SearchDegradation {
            code: "rerank_model_unavailable".to_string(),
            severity: "low".to_string(),
            message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_string(),
            repair: Some(RERANK_MODEL_UNAVAILABLE_REPAIR.to_owned()),
        }],
        false,
    );
    let report = empty_search_report(vec![SearchDegradation {
        code: "rerank_model_unavailable".to_string(),
        severity: "low".to_string(),
        message: "The registered local reranker could not be loaded; retry the query or inspect local model status."
            .to_string(),
        repair: None,
    }], false);

    let mut repeated = report.clone();
    repeated.query = "second transient query".to_string();
    let mut session = SearchAdvisorySession::default();
    let permanent_json = permanent.data_json_with_advisory_session(&mut session);
    let json = report.data_json_with_advisory_session(&mut session);
    let repeated_json = repeated.data_json_with_advisory_session(&mut session);

    assert_eq!(permanent_json["rerank"]["advisory"]["permanent"], true);
    assert!(json["rerank"].get("permanent").is_none());
    assert_eq!(json["rerank"]["advisory"]["permanent"], false);
    assert_eq!(
        json["rerank"]["advisory"]["resolution"],
        "retry_or_inspect_local_registry"
    );
    assert_eq!(json["degraded"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["degraded"][0]["code"], "rerank_model_unavailable");
    assert_eq!(repeated_json["rerank"]["advisory"]["permanent"], false);
    assert_eq!(
        repeated_json["rerank"]["advisorySummary"]["emittedCount"],
        1
    );
    assert_eq!(
        repeated_json["rerank"]["advisorySummary"]["suppressedCount"],
        0
    );
    assert_eq!(
        repeated_json["rerank"]["advisorySummary"]["sessionSuppressedCount"], 0,
        "a prior permanent advisory with the same code must not suppress transient query truth"
    );
    assert_eq!(
        repeated_json["degraded"][0]["code"],
        "rerank_model_unavailable"
    );
}
