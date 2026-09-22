//! EE-012 forbidden-dependency audit.
//!
//! AGENTS.md `Forbidden Dependencies (Hard Rule, Audited By CI)` requires
//! the resolved dependency tree to exclude `tokio`, `tokio-util`,
//! `async-std`, `smol`, `rusqlite`, `sqlx`, `diesel`, `sea-orm`, `petgraph`,
//! `hyper`, `axum`, `tower`, and `reqwest`. This integration test fails if
//! any of those crate names appears in the resolved cargo tree under the
//! default feature set or under `--all-features`.
//!
//! The test shells out to
//! `cargo tree --locked --prefix none --edges normal,build,dev` and matches
//! the first whitespace-separated token of each non-empty line against the
//! forbidden list. `--locked` is mandatory so an audit cannot rewrite
//! `Cargo.lock` while inspecting drifted path dependencies. The test is
//! deterministic and offline as long as the local cargo cache already has the
//! manifest's resolved dependencies; it does not perform new network
//! resolution.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code may unwrap/expect
use std::collections::BTreeSet;
use std::process::Command;

const FORBIDDEN_CRATES: &[&str] = &[
    "tokio",
    "tokio-util",
    "async-std",
    "smol",
    "rusqlite",
    "sqlx",
    "diesel",
    "sea-orm",
    "petgraph",
    "hyper",
    "axum",
    "tower",
    "reqwest",
];

/// fnx_algorithms calls currently permitted in ee graph surfaces.
///
/// This list is intentionally explicit: adding a graph algorithm call should
/// update the audit before it can land, so forbidden transitive dependencies
/// pulled by new algorithm modules are reviewed through the same gate.
const AUDITED_FNX_ALGORITHM_CALLS: &[&str] = &[
    "all_pairs_lowest_common_ancestor",
    "articulation_points",
    "betweenness_centrality_directed",
    "dominance_frontiers",
    "ego_graph",
    "find_cycle_directed",
    "gomory_hu_tree",
    "hits_centrality",
    "hits_centrality_directed",
    "immediate_dominators",
    "k_core",
    "k_truss",
    "label_propagation_communities",
    "louvain_communities",
    "min_cost_flow",
    "number_connected_components",
    "onion_layers",
    "pagerank_directed",
    "pagerank_with_params",
    "shortest_path_unweighted_directed",
    "simrank_similarity",
    "transitive_closure",
    "voronoi_cells",
];

/// GraphAccretion roadmap algorithms from bd-igvt.5, using the Rust fnx
/// function names rather than Python `_rust` adapter names.
const ROADMAP_FNX_ALGORITHM_CALLS: &[&str] = &[
    "pagerank_directed",
    "betweenness_centrality_directed",
    "k_truss",
    "onion_layers",
    "articulation_points",
    "transitive_closure",
    "min_cost_flow",
    "gomory_hu_tree",
    "immediate_dominators",
    "dominance_frontiers",
    "voronoi_cells",
    "ego_graph",
    "hits_centrality_directed",
    "louvain_communities",
    "simrank_similarity",
];

/// AI/LLM client crates that must never appear in the core binary.
/// The mechanical CLI boundary prohibits runtime LLM dependencies.
const FORBIDDEN_AI_CRATES: &[&str] = &[
    "openai",
    "async-openai",
    "openai-api",
    "anthropic",
    "anthropic-rs",
    "google-generative-ai",
    "langchain",
    "llm",
    "ollama-rs",
    "replicate",
    "cohere",
];

#[test]
fn frankensearch_default_features_enable_model2vec_download_without_fastembed() {
    let manifest = std::fs::read_to_string(manifest_path())
        .expect("Cargo.toml should be readable for feature guard");
    let dependency_line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("frankensearch ="))
        .expect("Cargo.toml must declare frankensearch dependency");

    for required in ["\"hash\"", "\"storage\"", "\"model2vec\"", "\"download\""] {
        assert!(
            dependency_line.contains(required),
            "frankensearch dependency must include {required}: {dependency_line}"
        );
    }
    assert!(
        !dependency_line.contains("\"fastembed\""),
        "fastembed remains forbidden-dependency-blocked and must not be enabled by default: {dependency_line}"
    );
}

/// bd-1nl13.9: the reranker is the pure-Rust frankentorch `native` backend, and
/// the default dependency tree (which now includes it) must be free of the ONNX
/// runtime (`ort`/`ort-sys`/`onnxruntime`). The forbidden-crate trees above
/// (`default_feature_tree_excludes_forbidden_crates`, run with the native
/// reranker enabled) cover tokio/hyper/etc.; this guard pins the ONNX removal so
/// a regression that re-introduces the C++ reranker is caught by CI.
#[test]
fn native_reranker_enabled_and_onnx_runtime_absent() {
    let manifest = std::fs::read_to_string(manifest_path())
        .expect("Cargo.toml should be readable for native reranker guard");
    let dependency_line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("frankensearch ="))
        .expect("Cargo.toml must declare frankensearch dependency");
    assert!(
        dependency_line.contains("\"native\""),
        "frankensearch dependency must enable the pure-Rust native reranker: {dependency_line}"
    );

    let tree = run_cargo_tree(&[]);
    let onnx_hits: Vec<&str> = tree
        .lines()
        .map(str::trim)
        .filter(|line| {
            let name = line.split_whitespace().next().unwrap_or("");
            matches!(name, "ort" | "ort-sys" | "onnxruntime" | "onnxruntime-sys")
        })
        .collect();
    assert!(
        onnx_hits.is_empty(),
        "ONNX runtime must be absent from the default (native-reranker) tree, found: {onnx_hits:?}.\n\
         The reranker is pure-Rust (frankentorch); ort/onnxruntime must not return."
    );
}

/// Direct Franken-stack crates must be exact (`=version`) pins.
///
/// Cargo.lock already selected these versions; a caret requirement would
/// still resolve identically under `--locked` and silently drift without it
/// (bd-reality-core-convergence-1azkt.18).
const FRANKEN_STACK_DIRECT_CRATES: &[&str] = &[
    "asupersync",
    "franken-agent-detection",
    "fnx-algorithms",
    "fnx-classes",
    "fnx-runtime",
    "frankensearch",
    "fsqlite",
    "sqlmodel-core",
    "sqlmodel-frankensqlite",
    "toon",
];

#[test]
fn franken_stack_direct_deps_use_exact_version_pins() {
    let manifest = std::fs::read_to_string(manifest_path())
        .expect("Cargo.toml should be readable for franken-stack pin audit");
    let mut missing = Vec::new();
    let mut caret = Vec::new();
    for name in FRANKEN_STACK_DIRECT_CRATES {
        let prefix = format!("{name} =");
        let lines: Vec<&str> = manifest
            .lines()
            .filter(|line| line.trim_start().starts_with(&prefix))
            .collect();
        if lines.is_empty() {
            missing.push(*name);
            continue;
        }
        for line in lines {
            assert!(
                line.contains("version = \""),
                "franken-stack dep {name} must declare a version: {line}"
            );
            if !line.contains("version = \"=") {
                caret.push(line.to_owned());
            }
        }
    }
    assert!(
        missing.is_empty(),
        "missing franken-stack direct deps in Cargo.toml: {missing:?}"
    );
    assert!(
        caret.is_empty(),
        "franken-stack direct deps must be exact (=version) pins so unlocked cargo cannot drift; caret lines: {caret:?}"
    );
}

/// checkout-franken-stack must materialize locked revisions without rewriting
/// captured sibling Cargo.toml files after checkout.
#[test]
fn checkout_franken_stack_does_not_rewrite_captured_sources() {
    let bash = include_str!("../scripts/checkout-franken-stack.sh");
    let powershell = include_str!("../scripts/checkout-franken-stack.ps1");
    let rch_verify = std::fs::read_to_string(format!(
        "{}/scripts/rch_verify.sh",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("scripts/rch_verify.sh should be readable");
    assert!(
        !bash.contains("relax_sqlmodel_asupersync_pin") && !bash.contains("perl -i"),
        "checkout-franken-stack.sh must not rewrite captured SQLModel after checkout"
    );
    assert!(
        !powershell.contains("relaxed sqlmodel asupersync exact pin"),
        "checkout-franken-stack.ps1 must not rewrite captured SQLModel after checkout"
    );
    assert!(
        !rch_verify.contains("perl -i -pe"),
        "rch_verify.sh pinned-bundle materialization must not rewrite captured SQLModel"
    );
}

fn manifest_path() -> String {
    format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"))
}

fn cargo_tree_args<'a>(manifest: &'a str, extra: &'a [&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "tree",
        "--locked",
        "--edges",
        "normal,build,dev",
        "--prefix",
        "none",
        "--manifest-path",
        manifest,
    ];
    args.extend_from_slice(extra);
    args
}

fn run_cargo_tree(extra: &[&str]) -> String {
    let manifest = manifest_path();
    let args = cargo_tree_args(manifest.as_str(), extra);

    let output = match Command::new(env!("CARGO")).args(&args).output() {
        Ok(value) => value,
        Err(error) => panic!("failed to invoke `cargo tree`: {error}"),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        panic!("`cargo tree` returned non-zero exit code\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }

    match String::from_utf8(output.stdout) {
        Ok(text) => text,
        Err(error) => panic!("`cargo tree` produced non-UTF-8 output: {error}"),
    }
}

#[test]
fn every_dependency_audit_is_lockfile_pinned() {
    let args = cargo_tree_args("/tmp/ee-forbidden-deps/Cargo.toml", &[]);
    assert_eq!(
        &args[..2],
        &["tree", "--locked"],
        "Rust dependency-tree audits must fail closed on lockfile drift"
    );

    let shell_audit = include_str!("../scripts/check-forbidden-deps.sh");
    assert!(
        shell_audit.contains("cargo metadata --locked --format-version=1"),
        "shell dependency audit must use lockfile-pinned Cargo metadata"
    );
}

fn forbidden_hits(tree_output: &str) -> BTreeSet<&'static str> {
    let mut hits: BTreeSet<&'static str> = BTreeSet::new();
    for line in tree_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let name = match trimmed.split_whitespace().next() {
            Some(value) => value,
            None => continue,
        };
        for forbidden in FORBIDDEN_CRATES {
            if name == *forbidden {
                hits.insert(*forbidden);
            }
        }
    }
    hits
}

fn fail_with_hits(scenario: &str, hits: BTreeSet<&'static str>) -> ! {
    let names: Vec<&str> = hits.into_iter().collect();
    panic!(
        "Forbidden dependencies present in {scenario} feature tree: {}.\n\n\
         Fix: remove the dependency, or quarantine it behind an explicit feature \
         that is disabled by default. See AGENTS.md \
         `Forbidden Dependencies (Hard Rule, Audited By CI)` for the canonical \
         list and rationale.",
        names.join(", ")
    );
}

fn audited_fnx_algorithm_call_set() -> BTreeSet<&'static str> {
    AUDITED_FNX_ALGORITHM_CALLS.iter().copied().collect()
}

fn direct_fnx_algorithm_calls_in_source() -> BTreeSet<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut calls = BTreeSet::new();
    collect_fnx_algorithm_calls(&root.join("src/graph"), &mut calls);
    collect_fnx_algorithm_calls(&root.join("src/cli/mod.rs"), &mut calls);
    calls
}

fn collect_fnx_algorithm_calls(path: &std::path::Path, calls: &mut BTreeSet<String>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            collect_fnx_algorithm_calls(&entry.path(), calls);
        }
        return;
    }

    if path.extension().and_then(|value| value.to_str()) != Some("rs") {
        return;
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    collect_fnx_algorithm_calls_from_text(&content, calls);
}

fn collect_fnx_algorithm_calls_from_text(text: &str, calls: &mut BTreeSet<String>) {
    let mut import_block = String::new();
    let mut in_import_block = false;

    for line in text.lines() {
        collect_qualified_fnx_calls(line, calls);

        let trimmed = line.trim();
        if in_import_block {
            import_block.push(' ');
            import_block.push_str(trimmed);
            if trimmed.contains("};") {
                collect_fnx_import_symbols(&import_block, calls);
                import_block.clear();
                in_import_block = false;
            }
            continue;
        }

        if trimmed.starts_with("use fnx_algorithms::{") {
            import_block.push_str(trimmed);
            if trimmed.contains("};") {
                collect_fnx_import_symbols(&import_block, calls);
                import_block.clear();
            } else {
                in_import_block = true;
            }
        } else if trimmed.starts_with("use fnx_algorithms::") {
            collect_fnx_import_symbols(trimmed, calls);
        }
    }
}

fn collect_qualified_fnx_calls(line: &str, calls: &mut BTreeSet<String>) {
    let mut rest = line;
    while let Some(index) = rest.find("fnx_algorithms::") {
        let after_prefix = &rest[index + "fnx_algorithms::".len()..];
        let symbol: String = after_prefix
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        if is_fnx_algorithm_function_symbol(&symbol) {
            calls.insert(symbol);
        }
        rest = after_prefix;
    }
}

fn collect_fnx_import_symbols(import: &str, calls: &mut BTreeSet<String>) {
    for symbol in import
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|symbol| is_fnx_algorithm_function_symbol(symbol))
    {
        calls.insert(symbol.to_owned());
    }
}

fn is_fnx_algorithm_function_symbol(symbol: &str) -> bool {
    symbol
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && !matches!(symbol, "as" | "fnx_algorithms" | "use")
}

#[test]
fn default_feature_tree_excludes_forbidden_crates() {
    let tree = run_cargo_tree(&[]);
    let hits = forbidden_hits(&tree);
    if !hits.is_empty() {
        fail_with_hits("default", hits);
    }
}

#[test]
fn all_features_tree_excludes_forbidden_crates() {
    let tree = run_cargo_tree(&["--all-features"]);
    let hits = forbidden_hits(&tree);
    if !hits.is_empty() {
        fail_with_hits("--all-features", hits);
    }
}

#[test]
fn no_default_features_tree_excludes_forbidden_crates() {
    let tree = run_cargo_tree(&["--no-default-features"]);
    let hits = forbidden_hits(&tree);
    if !hits.is_empty() {
        fail_with_hits("--no-default-features", hits);
    }
}

#[test]
fn graph_algorithm_audit_covers_roadmap_calls() {
    let audited = audited_fnx_algorithm_call_set();
    let missing: Vec<_> = ROADMAP_FNX_ALGORITHM_CALLS
        .iter()
        .copied()
        .filter(|function| !audited.contains(function))
        .collect();

    assert!(
        missing.is_empty(),
        "GraphAccretion roadmap fnx algorithms missing from dependency audit: {missing:?}"
    );
}

#[test]
fn graph_algorithm_audit_tracks_direct_call_sites() {
    let audited = audited_fnx_algorithm_call_set();
    let calls = direct_fnx_algorithm_calls_in_source();
    let unaudited: Vec<_> = calls
        .iter()
        .filter(|function| !audited.contains(function.as_str()))
        .cloned()
        .collect();

    assert!(
        unaudited.is_empty(),
        "Direct fnx_algorithms calls must be added to AUDITED_FNX_ALGORITHM_CALLS: {unaudited:?}"
    );
}

#[cfg(test)]
mod self_tests {
    use std::collections::BTreeSet;

    use super::{
        AUDITED_FNX_ALGORITHM_CALLS, FORBIDDEN_CRATES, ROADMAP_FNX_ALGORITHM_CALLS,
        collect_fnx_algorithm_calls_from_text, forbidden_hits,
    };

    #[test]
    fn detects_each_forbidden_crate_when_present() {
        for forbidden in FORBIDDEN_CRATES {
            let synthetic = format!("ee v0.1.0\n{forbidden} v1.0.0\nserde v1.0.0\n");
            let hits = forbidden_hits(&synthetic);
            assert!(
                hits.contains(forbidden),
                "scanner failed to detect `{forbidden}` in synthetic tree"
            );
        }
    }

    #[test]
    fn ignores_unrelated_crates() {
        let synthetic = "ee v0.1.0\nserde v1.0.0\nclap v4.5.0\nthiserror v1.0.0\n";
        let hits = forbidden_hits(synthetic);
        assert!(
            hits.is_empty(),
            "scanner produced false positives: {hits:?}"
        );
    }

    #[test]
    fn ignores_empty_and_whitespace_lines() {
        let synthetic = "\n   \nee v0.1.0\n\n";
        let hits = forbidden_hits(synthetic);
        assert!(hits.is_empty());
    }

    #[test]
    fn matches_exact_crate_name_not_substring() {
        // A crate named `tokio-foo` would share the prefix but is not on the
        // forbidden list; only the exact crate name should match.
        let synthetic = "ee v0.1.0\ntokio-foo v0.1.0\nrusqlite-clone v0.1.0\n";
        let hits = forbidden_hits(synthetic);
        assert!(hits.is_empty(), "false positives: {hits:?}");
    }

    #[test]
    fn ai_crate_list_is_non_empty() {
        assert!(
            !super::FORBIDDEN_AI_CRATES.is_empty(),
            "AI crate list must not be empty"
        );
    }

    #[test]
    fn collects_qualified_and_imported_fnx_algorithm_calls() {
        let source = r#"
            use fnx_algorithms::{
                PageRankResult,
                betweenness_centrality_directed,
                louvain_communities,
            };

            fn run() {
                let _ = fnx_algorithms::pagerank_directed(&graph);
                let _ = fnx_algorithms::hits_centrality_directed(&graph);
            }
        "#;
        let mut calls = BTreeSet::new();
        collect_fnx_algorithm_calls_from_text(source, &mut calls);

        assert!(calls.contains("betweenness_centrality_directed"));
        assert!(calls.contains("hits_centrality_directed"));
        assert!(calls.contains("louvain_communities"));
        assert!(calls.contains("pagerank_directed"));
        assert!(
            !calls.contains("PageRankResult"),
            "type-only imports should not be treated as algorithm call sites"
        );
    }

    #[test]
    fn roadmap_algorithm_list_is_a_subset_of_audited_calls() {
        let audited: BTreeSet<_> = AUDITED_FNX_ALGORITHM_CALLS.iter().copied().collect();
        let missing: Vec<_> = ROADMAP_FNX_ALGORITHM_CALLS
            .iter()
            .copied()
            .filter(|function| !audited.contains(function))
            .collect();

        assert!(missing.is_empty(), "missing roadmap calls: {missing:?}");
    }
}

// ---------------------------------------------------------------------------
// AI crate detection
// ---------------------------------------------------------------------------

fn ai_crate_hits(tree_output: &str) -> BTreeSet<&'static str> {
    let mut hits: BTreeSet<&'static str> = BTreeSet::new();
    for line in tree_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let name = match trimmed.split_whitespace().next() {
            Some(value) => value,
            None => continue,
        };
        for forbidden in FORBIDDEN_AI_CRATES {
            if name == *forbidden {
                hits.insert(*forbidden);
            }
        }
    }
    hits
}

#[test]
fn default_feature_tree_excludes_ai_crates() {
    let tree = run_cargo_tree(&[]);
    let hits = ai_crate_hits(&tree);
    if !hits.is_empty() {
        let names: Vec<&str> = hits.into_iter().collect();
        panic!(
            "AI/LLM client dependencies present in default feature tree: {}.\n\n\
             Fix: the mechanical CLI boundary requires ee to be a pure computation \
             layer with no runtime LLM dependencies. Move AI-dependent logic to \
             project-local skills or an external orchestrator.",
            names.join(", ")
        );
    }
}

#[test]
fn all_features_tree_excludes_ai_crates() {
    let tree = run_cargo_tree(&["--all-features"]);
    let hits = ai_crate_hits(&tree);
    if !hits.is_empty() {
        let names: Vec<&str> = hits.into_iter().collect();
        panic!(
            "AI/LLM client dependencies present in --all-features tree: {}.\n\n\
             Fix: even behind feature flags, AI client crates are forbidden in the \
             core binary. They belong in external skills or adapters.",
            names.join(", ")
        );
    }
}

// ---------------------------------------------------------------------------
// Source scan for AI API patterns in core Rust code
// ---------------------------------------------------------------------------

use std::path::Path;

/// Patterns that indicate direct AI API calls in Rust source.
/// These should not appear in core runtime code.
const AI_API_PATTERNS: &[&str] = &[
    "ChatCompletion",
    "chat.completions",
    "create_completion",
    "complete_chat",
    "anthropic::Client",
    "openai::Client",
    "OpenAIClient",
    "AnthropicClient",
    "model.generate",
    "llm.invoke",
];

/// Directories to exclude from the source scan (docs, skills, tests are allowed).
const SCAN_EXCLUDE_DIRS: &[&str] = &["docs", "skills", "tests", "target", ".git", "benches"];

fn scan_rust_files_for_ai_patterns(root: &Path) -> Vec<(String, usize, String)> {
    let mut findings = Vec::new();
    scan_directory(root, &mut findings);
    findings
}

fn scan_directory(dir: &Path, findings: &mut Vec<(String, usize, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if SCAN_EXCLUDE_DIRS.contains(&file_name) {
            continue;
        }

        if path.is_dir() {
            scan_directory(&path, findings);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            scan_file(&path, findings);
        }
    }
}

fn scan_file(path: &Path, findings: &mut Vec<(String, usize, String)>) {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return,
    };

    let path_str = path.display().to_string();

    for (line_num, line) in content.lines().enumerate() {
        // Skip comments
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("*") {
            continue;
        }

        for pattern in AI_API_PATTERNS {
            if line.contains(pattern) {
                findings.push((path_str.clone(), line_num + 1, pattern.to_string()));
            }
        }
    }
}

#[test]
fn core_source_excludes_ai_api_patterns() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let findings = scan_rust_files_for_ai_patterns(&src_dir);

    if !findings.is_empty() {
        let mut report = String::from(
            "AI API patterns found in core source code:\n\n\
             The mechanical CLI boundary prohibits direct AI/LLM API calls in the \
             core binary. Move this logic to project-local skills or an external \
             orchestrator.\n\n",
        );

        for (path, line, pattern) in &findings {
            report.push_str(&format!("  {path}:{line} — matched: {pattern}\n"));
        }

        panic!("{report}");
    }
}

// ---------------------------------------------------------------------------
// Determinism ambient-randomness lint scaffold (N4.4)
// ---------------------------------------------------------------------------

const DETERMINISM_CLIPPY_METHODS: &[&str] =
    &["rand::thread_rng", "rand::random", "uuid::Uuid::new_v4"];

const DETERMINISM_AMBIENT_RANDOMNESS_PATTERNS: &[&str] =
    &["thread_rng(", "rand::random", "Uuid::new_v4("];

fn clippy_toml_text() -> &'static str {
    include_str!("../clippy.toml")
}

fn source_scan_exclude_dirs(file_name: &str) -> bool {
    matches!(
        file_name,
        "docs" | "target" | ".git" | "benches" | "tests" | "scripts"
    )
}

fn scan_rust_files_for_patterns(
    root: &Path,
    patterns: &[&'static str],
) -> Vec<(String, usize, &'static str)> {
    let mut findings = Vec::new();
    scan_directory_for_patterns(root, patterns, &mut findings);
    findings
}

fn scan_directory_for_patterns(
    dir: &Path,
    patterns: &[&'static str],
    findings: &mut Vec<(String, usize, &'static str)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if source_scan_exclude_dirs(file_name) {
            continue;
        }

        if path.is_dir() {
            scan_directory_for_patterns(&path, patterns, findings);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            scan_file_for_patterns(&path, patterns, findings);
        }
    }
}

fn scan_file_for_patterns(
    path: &Path,
    patterns: &[&'static str],
    findings: &mut Vec<(String, usize, &'static str)>,
) {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return,
    };
    let path_str = path.display().to_string();

    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue;
        }

        for pattern in patterns {
            if line.contains(pattern) {
                findings.push((path_str.clone(), line_num + 1, *pattern));
            }
        }
    }
}

#[test]
fn determinism_clippy_config_disallows_ambient_randomness_methods() {
    let config = clippy_toml_text();
    for method in DETERMINISM_CLIPPY_METHODS {
        assert!(
            config.contains(&format!("path = \"{method}\"")),
            "clippy.toml must disallow `{method}` for bd-17c65.14.4.4"
        );
    }
}

#[test]
fn core_source_excludes_ambient_randomness_patterns() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let findings = scan_rust_files_for_patterns(&src_dir, DETERMINISM_AMBIENT_RANDOMNESS_PATTERNS);

    if !findings.is_empty() {
        let mut report = String::from(
            "Ambient randomness patterns found in core source code:\n\n\
             N4.4 requires deterministic paths to use Deterministic<Seed> or \
             seeded ID helpers instead of ambient RNG/UUID calls.\n\n",
        );

        for (path, line, pattern) in &findings {
            report.push_str(&format!("  {path}:{line} - matched: {pattern}\n"));
        }

        panic!("{report}");
    }
}

/// bd-reality-core-convergence-1azkt.18: FOUR copies of the forbidden-crate
/// list exist and nothing holds them equal.
///
///   `FORBIDDEN_CRATES` in this file          enforced by this test target
///   `scripts/check-forbidden-deps.sh`        a BASELINE FLOOR, not the
///                                            operative list -- see below
///   `AGENTS.md` forbidden-dependency table   documentation
///   `deny.toml` `[bans]`                     THE SOURCE the shell gate reads
///
/// THE LINE ABOVE USED TO SAY `deny.toml` `[bans]` WAS "NOT ENFORCED -- CI
/// runs only `cargo deny check advisories`". That was true when written and
/// is false now, in both halves. Corrected under bd-wstau after measuring it
/// rather than re-reading it:
///
///   - 2a05a90fe (2026-09-22) made `scripts/check-forbidden-deps.sh` load its
///     ban list FROM `deny.toml` `[bans].deny`, keeping its own array only as
///     a floor it refuses to drop below. So `deny.toml` is no longer a copy
///     nothing reads; it is the input to the gate that does run. Measured:
///     adding `{ name = "serde" }` to `[bans].deny` makes that script exit 2
///     naming serde, and a version-qualified entry it cannot express exits 3
///     rather than being silently discarded.
///   - "CI runs only `cargo deny check advisories`" is wrong twice. `ci.yml`
///     has carried `command: check bans sources` since b92333910, and neither
///     it nor `release.yml` executes at all -- both are `disabled_manually`.
///     `ci-static.yml`, the workflow that does run on push, contains ZERO
///     cargo-deny invocations of any kind. No cargo-deny check -- advisories,
///     bans, sources or licenses -- runs on the push path today.
///
/// So the enforcement that exists is the shell gate in `ci-static.yml`, which
/// reads `deny.toml`. The `cargo deny` COMMAND is what nothing runs; the
/// `[bans]` POLICY is enforced. Those are different claims and this comment
/// previously conflated them.
///
/// They agree at the time of writing; that was measured with set diffs rather
/// than length comparisons, which is the check that would have missed a swap
/// of equal size. But agreeing today is a statement about the calendar, which
/// is why this test exists regardless of which copy is authoritative.
///
/// This test does not decide WHICH list is authoritative. Equality does not
/// require primacy: naming the authority decides where a future edit should
/// land, while this decides that no edit can land in one place only. The
/// first question is open; the second is closed by this test.
///
/// On divergence it names the source, the missing entries and the unexpected
/// ones, and reports every diverging source at once rather than the first. A
/// four-way comparison that printed "lists differ" would reproduce, in the
/// commit that fixes this, the defect of an assertion that says something
/// failed without saying what.
#[test]
fn forbidden_crate_list_is_identical_in_every_source() {
    let root = env!("CARGO_MANIFEST_DIR");
    let read = |relative: &str| -> String {
        std::fs::read_to_string(format!("{root}/{relative}"))
            .unwrap_or_else(|error| panic!("{relative} must be readable: {error}"))
    };

    // deny.toml: `[bans]` ... `deny = [ { name = "x" }, ... ]`, up to the next
    // top-level table.
    let deny_toml = read("deny.toml");
    let bans: BTreeSet<String> = deny_toml
        .lines()
        .skip_while(|line| line.trim() != "[bans]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .filter_map(|line| {
            let rest = line.split("name").nth(1)?;
            let mut parts = rest.split('"');
            parts.next()?;
            parts.next().map(str::to_owned)
        })
        .collect();

    // AGENTS.md: the first column of the forbidden-dependency table holds one
    // or more backticked crate names per row.
    let agents = read("AGENTS.md");
    let table: BTreeSet<String> = agents
        .lines()
        .skip_while(|line| !line.contains("Forbidden Dependencies"))
        .take_while(|line| !line.trim_start().starts_with("Run `cargo tree"))
        .filter(|line| line.trim_start().starts_with('|'))
        .filter_map(|line| line.split('|').nth(1))
        .flat_map(|cell| {
            cell.split('`')
                .skip(1)
                .step_by(2)
                .map(str::trim)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|name| !name.is_empty())
        .collect();

    // check-forbidden-deps.sh: `FORBIDDEN=(` one bare crate name per line `)`.
    let script = read("scripts/check-forbidden-deps.sh");
    let shell: BTreeSet<String> = script
        .lines()
        .skip_while(|line| line.trim() != "FORBIDDEN=(")
        .skip(1)
        .take_while(|line| line.trim() != ")")
        .map(|line| line.trim().to_owned())
        .filter(|name| !name.is_empty() && !name.starts_with('#'))
        .collect();

    let canonical: BTreeSet<String> = FORBIDDEN_CRATES.iter().map(|c| (*c).to_owned()).collect();

    let mut divergences = Vec::new();
    for (source, actual) in [
        ("deny.toml [bans]", &bans),
        ("AGENTS.md forbidden-dependency table", &table),
        ("scripts/check-forbidden-deps.sh FORBIDDEN=()", &shell),
    ] {
        // An empty parse means the format moved, which is a different failure
        // from a real divergence and must not be reported as one.
        assert!(
            !actual.is_empty(),
            "{source}: parsed zero crate names. The file's format changed and \
             this test can no longer read it -- fix the parser before trusting \
             any verdict from it."
        );
        let missing: Vec<&str> = canonical.difference(actual).map(String::as_str).collect();
        let unexpected: Vec<&str> = actual.difference(&canonical).map(String::as_str).collect();
        if !missing.is_empty() || !unexpected.is_empty() {
            divergences.push(format!(
                "  {source}\n    missing (present in FORBIDDEN_CRATES, absent here): {missing:?}\n    unexpected (here, absent from FORBIDDEN_CRATES): {unexpected:?}"
            ));
        }
    }

    assert!(
        divergences.is_empty(),
        "the forbidden-crate list has diverged across its copies. \
         FORBIDDEN_CRATES in tests/forbidden_deps.rs is the comparison basis \
         (this is a comparison basis, NOT a ruling on which copy is \
         authoritative -- see bd-reality-core-convergence-1azkt.18). \
         Every diverging source is listed:\n{}",
        divergences.join("\n")
    );
}
