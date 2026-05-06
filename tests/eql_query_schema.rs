//! Contract coverage for the section 13.5 EQL-inspired JSON query shape.

use ee::models::query::{EqlQueryError, EqlSpeedMode, EqlTagsMode, parse_eql_query};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn parse(value: Value) -> Result<ee::models::query::EqlQuery, String> {
    parse_eql_query(&value).map_err(|error| error.to_string())
}

fn parse_error(value: Value, message: &str) -> Result<EqlQueryError, String> {
    match parse_eql_query(&value) {
        Ok(_) => Err(message.to_owned()),
        Err(error) => Ok(error),
    }
}

#[test]
fn eql_parser_accepts_full_section_13_5_shape() -> TestResult {
    let query = parse(json!({
        "q": "release automation failed after branch rename",
        "workspace": ".",
        "levels": ["procedural", "episodic", "semantic"],
        "kinds": ["rule", "anti_pattern", "failure", "fix", "decision"],
        "tags": ["release", "git", "ci"],
        "tags_mode": "any",
        "scope": ["workspace", "repository"],
        "time": {"since": "180d", "until": "2026-05-01T00:00:00Z"},
        "confidence": {"min": 0.4, "max": 0.95},
        "graph": {
            "center": "mem_01HXX",
            "hops": 2,
            "relations": ["supports", "same_error", "derived_from"]
        },
        "limit": 20,
        "speed": "quality",
        "rerank": true,
        "return_subgraph": true,
        "explain": true
    }))?;

    ensure(
        query.q == "release automation failed after branch rename",
        "query text",
    )?;
    ensure(query.workspace.as_deref() == Some("."), "workspace")?;
    ensure(query.levels.len() == 3, "levels")?;
    ensure(query.kinds.len() == 5, "kinds")?;
    ensure(query.tags_mode == EqlTagsMode::Any, "tags mode")?;
    ensure(query.limit == 20, "limit")?;
    ensure(query.speed == EqlSpeedMode::Quality, "quality speed")?;
    ensure(query.rerank, "rerank")?;
    ensure(query.return_subgraph, "return subgraph")?;
    ensure(query.explain, "explain")?;
    ensure(
        query
            .graph
            .as_ref()
            .is_some_and(|graph| graph.hops == Some(2) && graph.relations.len() == 3),
        "graph filter",
    )
}

#[test]
fn eql_executor_applies_metadata_filters_tags_time_and_graph() -> TestResult {
    let query = parse(json!({
        "q": "prepare release",
        "workspace": "eidetic_engine_cli",
        "levels": ["procedural", "episodic"],
        "kinds": ["rule", "failure"],
        "tags": ["release", "ci"],
        "tags_mode": "all",
        "scope": ["workspace"],
        "time": {"since": "30d"},
        "confidence": {"min": 0.7},
        "graph": {"center": "mem_root", "hops": 2, "relations": ["supports"]},
        "limit": 2,
        "speed": "instant",
        "rerank": false,
        "return_subgraph": false,
        "explain": true
    }))?;

    let candidates = vec![
        json!({
            "id": "mem_a",
            "workspace": "eidetic_engine_cli",
            "level": "procedural",
            "kind": "rule",
            "tags": ["release", "ci", "rust"],
            "scope": "workspace",
            "ageDays": 3,
            "confidence": 0.92,
            "graph": {"center": "mem_root", "hops": 1, "relations": ["supports"]}
        }),
        json!({
            "id": "mem_b",
            "workspace": "eidetic_engine_cli",
            "level": "episodic",
            "kind": "failure",
            "tags": ["release", "ci"],
            "scope": "workspace",
            "ageDays": 12,
            "confidence": 0.75,
            "graph": {"center": "mem_root", "hops": 2, "relations": ["supports", "same_error"]}
        }),
        json!({
            "id": "mem_c",
            "workspace": "eidetic_engine_cli",
            "level": "procedural",
            "kind": "rule",
            "tags": ["release"],
            "scope": "workspace",
            "ageDays": 3,
            "confidence": 0.95,
            "graph": {"center": "mem_root", "hops": 1, "relations": ["supports"]}
        }),
        json!({
            "id": "mem_d",
            "workspace": "eidetic_engine_cli",
            "level": "semantic",
            "kind": "decision",
            "tags": ["release", "ci"],
            "scope": "workspace",
            "ageDays": 1,
            "confidence": 0.99,
            "graph": {"center": "mem_root", "hops": 1, "relations": ["supports"]}
        }),
    ];

    let results = query.execute_metadata(&candidates);
    ensure(
        results.len() == 2,
        "limit and filters should retain two candidates",
    )?;
    ensure(
        results.first().and_then(|item| item.get("id")) == Some(&json!("mem_a")),
        "first retained candidate",
    )?;
    ensure(
        results.get(1).and_then(|item| item.get("id")) == Some(&json!("mem_b")),
        "second retained candidate",
    )?;
    ensure(query.speed == EqlSpeedMode::Instant, "instant speed parsed")?;
    ensure(query.explain, "explain flag parsed")
}

#[test]
fn eql_parser_rejects_unknown_or_invalid_fields() -> TestResult {
    let unknown = parse_error(
        json!({"q": "release", "surprise": true}),
        "unknown field should fail",
    )?;
    ensure(unknown.field == "surprise", "unknown field path")?;

    let invalid_speed = parse_error(
        json!({"q": "release", "speed": "slow"}),
        "invalid speed should fail",
    )?;
    ensure(invalid_speed.field == "speed", "invalid speed path")?;

    let invalid_tags_mode = parse_error(
        json!({"q": "release", "tags_mode": "none"}),
        "invalid tags_mode should fail",
    )?;
    ensure(
        invalid_tags_mode.field == "tags_mode",
        "invalid tags_mode path",
    )?;

    let invalid_confidence = parse_error(
        json!({"q": "release", "confidence": {"min": 0.9, "max": 0.1}}),
        "invalid confidence bounds should fail",
    )?;
    ensure(invalid_confidence.field == "confidence", "confidence path")
}
