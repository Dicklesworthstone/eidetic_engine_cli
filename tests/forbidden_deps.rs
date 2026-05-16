//! EE-012 forbidden-dependency audit.
//!
//! AGENTS.md `Forbidden Dependencies (Hard Rule, Audited By CI)` requires
//! the resolved dependency tree to exclude `tokio`, `tokio-util`,
//! `async-std`, `smol`, `rusqlite`, `sqlx`, `diesel`, `sea-orm`, `petgraph`,
//! `hyper`, `axum`, `tower`, and `reqwest`. This integration test fails if
//! any of those crate names appears in the resolved cargo tree under the
//! default feature set or under `--all-features`.
//!
//! The test shells out to `cargo tree --prefix none --edges normal,build,dev`
//! and matches the first whitespace-separated token of each non-empty line
//! against the forbidden list. The test is deterministic and offline as
//! long as the local cargo cache already has the manifest's resolved
//! dependencies; it does not perform new network resolution.

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
    "articulation_points",
    "betweenness_centrality_directed",
    "dominance_frontiers",
    "ego_graph",
    "find_cycle_directed",
    "gomory_hu_tree",
    "hits_centrality_directed",
    "immediate_dominators",
    "k_core",
    "k_truss",
    "label_propagation_communities",
    "louvain_communities",
    "min_cost_flow",
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

fn manifest_path() -> String {
    format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"))
}

fn run_cargo_tree(extra: &[&str]) -> String {
    let mut args: Vec<&str> = vec![
        "tree",
        "--edges",
        "normal,build,dev",
        "--prefix",
        "none",
        "--manifest-path",
    ];
    let manifest = manifest_path();
    args.push(manifest.as_str());
    args.extend_from_slice(extra);

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
