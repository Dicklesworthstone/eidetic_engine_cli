//! Static policy checks for graph test determinism.
//!
//! Graph tests may use the deterministic hash embedder or checked-in fixtures.
//! They must not instantiate real semantic backends in default CI, because that
//! would make graph output depend on local model availability and model drift.

type TestResult = Result<(), String>;

const GRAPH_TEST_SOURCES: &[(&str, &str)] = &[
    (
        "tests/graph_determinism.rs",
        include_str!("graph_determinism.rs"),
    ),
    (
        "tests/graph_neighborhood_smoke.rs",
        include_str!("graph_neighborhood_smoke.rs"),
    ),
    (
        "tests/contracts/graph_schemas_v1.rs",
        include_str!("contracts/graph_schemas_v1.rs"),
    ),
];

const TESTING_STRATEGY: &str = include_str!("../docs/testing-strategy.md");

fn real_embedder_markers() -> Vec<String> {
    vec![
        format!("{}{}", "Model2Vec", "Embedder"),
        format!("{}{}", "FastEmbed", "Embedder"),
        format!("{}{}", "ModelProvider::", "Model2Vec"),
        format!("{}{}", "ModelProvider::", "FastEmbed"),
        format!("{}{}", "model", "2vec-base"),
        format!("{}{}", "fast", "embed"),
    ]
}

#[test]
fn graph_tests_do_not_instantiate_real_embedding_backends() -> TestResult {
    let markers = real_embedder_markers();
    let mut violations = Vec::new();
    for (path, source) in GRAPH_TEST_SOURCES {
        for marker in &markers {
            if source.contains(marker) {
                violations.push(format!("{path} contains `{marker}`"));
            }
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "graph tests must use HashEmbedder or checked-in fixtures, not real backends:\n{}",
            violations.join("\n")
        ))
    }
}

#[test]
fn testing_strategy_documents_graph_determinism_contract() -> TestResult {
    for needle in [
        "Graph Test Determinism",
        "LabRuntime",
        "HashEmbedder",
        "model2vec",
        "fastembed",
        "non-CI coverage",
    ] {
        if !TESTING_STRATEGY.contains(needle) {
            return Err(format!(
                "docs/testing-strategy.md must document graph determinism detail `{needle}`"
            ));
        }
    }
    Ok(())
}
