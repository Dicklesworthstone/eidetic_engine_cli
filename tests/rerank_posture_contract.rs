use ee::core::profile::{OperatingProfile, RuntimeProfileReport};
use ee::core::search::{
    RERANK_MODEL_OFFLINE_IMPORT_COMMAND, RERANK_MODEL_UNAVAILABLE_ADVISORY, ScoreSource,
    SearchDegradation, SearchHit, SearchReport, SearchSourceMode, SearchStatus,
};
use ee::models::{EmbedBackend, MemoryScope, MemoryScopeStats};

fn runtime_profile() -> RuntimeProfileReport {
    RuntimeProfileReport::for_profile(OperatingProfile::Swarm, "rerank-posture-contract")
}

fn scope_stats() -> MemoryScopeStats {
    MemoryScopeStats::new(MemoryScope::Swarm, false, None, 0)
}

fn empty_search_report(degraded: Vec<SearchDegradation>) -> SearchReport {
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
fn duplicate_permanent_rerank_rows_collapse_within_each_structured_response() {
    let report = empty_search_report(vec![
        SearchDegradation {
            code: "rerank_model_unavailable".to_string(),
            severity: "low".to_string(),
            message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_string(),
            repair: Some(RERANK_MODEL_OFFLINE_IMPORT_COMMAND.to_string()),
        },
        SearchDegradation {
            code: "rerank_model_unavailable".to_string(),
            severity: "low".to_string(),
            message: RERANK_MODEL_UNAVAILABLE_ADVISORY.to_string(),
            repair: Some(RERANK_MODEL_OFFLINE_IMPORT_COMMAND.to_string()),
        },
    ]);

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
        RERANK_MODEL_OFFLINE_IMPORT_COMMAND
    );
    assert_eq!(repeated_json["rerank"], json["rerank"]);
    assert_eq!(
        json["degraded"].as_array().map_or(0, |rows| rows.len()),
        0,
        "permanent capability posture must not repeat in per-query degraded output"
    );
    assert!(!human.contains(RERANK_MODEL_UNAVAILABLE_ADVISORY));
    assert!(!human.contains(RERANK_MODEL_OFFLINE_IMPORT_COMMAND));
}

#[test]
fn transient_reranker_load_failure_remains_a_query_degradation() {
    let report = empty_search_report(vec![SearchDegradation {
        code: "rerank_model_unavailable".to_string(),
        severity: "low".to_string(),
        message: "The registered local reranker could not be loaded; retry the query or inspect local model status."
            .to_string(),
        repair: None,
    }]);

    let json = report.data_json();

    assert!(json["rerank"].get("permanent").is_none());
    assert_eq!(json["rerank"]["advisory"]["permanent"], false);
    assert_eq!(
        json["rerank"]["advisory"]["resolution"],
        "retry_or_inspect_local_registry"
    );
    assert_eq!(json["degraded"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["degraded"][0]["code"], "rerank_model_unavailable");
}
