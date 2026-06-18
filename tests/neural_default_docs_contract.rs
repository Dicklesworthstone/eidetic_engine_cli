//! Static contract tests for the neural-local default docs slice (bd-1et0v.23).

const README: &str = include_str!("../README.md");
const AGENTS: &str = include_str!("../AGENTS.md");
const TAXONOMY: &str = include_str!("../docs/degraded_code_taxonomy.md");
const DEGRADED_CODES: &str = include_str!("../docs/degraded_codes.md");
const ENV_VARS: &str = include_str!("../docs/env_vars.md");
const LIFECYCLE: &str = include_str!("../docs/model-lifecycle-readiness.md");
const ADR_0016: &str =
    include_str!("../docs/adr/0016-embedding-model-choice-owned-by-frankensearch.md");
const ADR_0080: &str = include_str!("../docs/adr/0080-bundled-default-embedder.md");
const FEATURE_FLAGS: &str = include_str!("../docs/feature_flag_registry.md");
const DEP_MATRIX: &str = include_str!("../docs/dependency-contract-matrix.md");
const DEP_RESEARCH: &str = include_str!("../docs/dependency-research-notes.md");
const FIXTURE: &str = include_str!("fixtures/failure_modes/embed_model_unavailable.json");

type TestResult = Result<(), String>;

fn ensure(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{context} missing `{needle}`"))
    }
}

#[test]
fn readme_and_agent_docs_present_neural_local_as_default() -> TestResult {
    ensure(
        README,
        "ready (neural_local, potion-multilingual-128M)",
        "README quick example",
    )?;
    ensure(
        README,
        "BM25 + neural-local vector search",
        "README hybrid retrieval row",
    )?;
    ensure(
        README,
        "deterministic hash fallback for offline runs",
        "README local-first row",
    )?;
    ensure(
        README,
        "EE_EMBED_MODEL_PATH` is a diagnostics/fault",
        "README troubleshooting",
    )?;
    ensure(
        AGENTS,
        r#"default = ["fts5", "json", "embed-fast", "lexical-bm25", "graph"]"#,
        "AGENTS default feature list",
    )?;
    ensure(
        AGENTS,
        r#"embed-fast = ["frankensearch/model2vec", "frankensearch/download"]"#,
        "AGENTS embed-fast download feature",
    )?;
    ensure(
        AGENTS,
        "The pinned local Model2Vec embedding model may download automatically once",
        "AGENTS local-first model download wording",
    )?;
    ensure(
        LIFECYCLE,
        "Default builds are expected to report semantic readiness",
        "model lifecycle docs",
    )
}

#[test]
fn degraded_taxonomy_and_catalog_describe_fallback_not_missing_feature() -> TestResult {
    ensure(
        TAXONOMY,
        "the pinned bundled model cannot be loaded/downloaded",
        "degraded taxonomy",
    )?;
    ensure(
        DEGRADED_CODES,
        "default semantic path is neural-local via Frankensearch Model2Vec",
        "degraded code catalog",
    )?;
    ensure(
        FIXTURE,
        "EE_EMBED_MODEL_PATH=/nonexistent/model",
        "failure-mode fixture",
    )?;
    ensure(
        FIXTURE,
        "The default semantic path is neural-local via Frankensearch Model2Vec",
        "failure-mode fixture default semantic path",
    )?;
    ensure(
        FIXTURE,
        "potion-multilingual-128M",
        "failure-mode fixture bundled model",
    )?;
    if FIXTURE.contains("default frankensearch_hash_fallback") {
        return Err(
            "embed_model_unavailable fixture still describes hash fallback as the default"
                .to_owned(),
        );
    }
    if TAXONOMY.contains("Build-time: no dense embedder feature compiled") {
        return Err(
            "embed_model_unavailable taxonomy still names the old no-dense-feature trigger"
                .to_owned(),
        );
    }
    Ok(())
}

#[test]
fn env_and_dependency_docs_match_the_manifest_feature_shape() -> TestResult {
    ensure(
        ENV_VARS,
        "Not a user-facing model loader or model-selection knob",
        "env var docs",
    )?;
    ensure(
        ENV_VARS,
        "Presence adds an `ee doctor` info note",
        "foreign embedding env docs",
    )?;
    ensure(
        FEATURE_FLAGS,
        "[\"frankensearch/model2vec\", \"frankensearch/download\"]",
        "feature flag registry",
    )?;
    ensure(
        DEP_MATRIX,
        "`hash`, `storage`, `model2vec`, `download`, `lexical`, `fts5`, and `rerank`",
        "dependency contract matrix",
    )?;
    ensure(
        DEP_RESEARCH,
        "`embed-fast` → `frankensearch/model2vec` + `frankensearch/download`",
        "dependency research notes",
    )
}

#[test]
fn adrs_reconcile_delegation_with_the_bundled_default() -> TestResult {
    ensure(
        ADR_0080,
        "potion-multilingual-128M",
        "ADR 0080 bundled default",
    )?;
    ensure(
        ADR_0016,
        "ADR 0080 narrows this decision",
        "ADR 0016 update note",
    )?;
    ensure(
        ADR_0016,
        "`embed-fast` includes `model2vec` and `download`",
        "ADR 0016 verification",
    )
}
