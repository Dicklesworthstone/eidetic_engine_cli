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

#[cfg(test)]
mod self_tests {
    use super::{FORBIDDEN_CRATES, forbidden_hits};

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
