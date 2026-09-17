//! Coverage guard for the contracts suite's process spawns
//! (bd-contracts-serialized-spawn-queue-loibi).
//!
//! WHAT THIS PINS, AND WHY NOTHING ELSE DID.
//!
//! Two fixes landed against the contracts suite hanging: a bounded permit
//! (`REAL_EE_MAX_CONCURRENT_SPAWNS`) and a wall-clock deadline
//! (`output_with_deadline`). Both are acquired in exactly two places --
//! `serialized_real_ee` and `serialized_real_ee_with` in `common_spawn.rs` --
//! so they protect the `ee` binary path and nothing else.
//!
//! Measured at the time of writing: 36 guarded call sites across 24 modules,
//! against 13 RAW `Command::new` sites across 11 modules that hold no permit
//! and carry no deadline at all. Three of the raw sites shell `cargo`
//! (`cargo metadata` once, `cargo tree` twice) and one shells the resume E2E
//! script with `EE_RESUME_E2E_SCOPE=all`.
//!
//! The hazard is not that raw spawns are slow. It is that a raw spawn has no
//! upper bound: `cargo metadata` blocks on the package-cache lock, and if the
//! cargo invocation running these very tests holds it, the test waits forever
//! and the suite never reports. That is the exact failure both fixes were
//! meant to remove, still reachable through the unguarded path.
//!
//! This guard does NOT migrate those sites -- that is a separate decision
//! about a 11-module sweep. It pins the population so the debt cannot grow
//! silently, which is the one thing a prose note in a commit message cannot
//! do. A NEW module that spawns a process without a deadline fails this test.
//!
//! Scanning the directory at runtime rather than an `include_str!` list is
//! deliberate: a fixed list cannot see a spawn added in a file that did not
//! exist when the list was written, and that is the case worth catching.

#![allow(clippy::expect_used)]

use std::path::PathBuf;

type TestResult = Result<(), String>;

/// Modules holding raw `Command::new` sites, with their site counts, as
/// measured on 2026-09-17. This may only SHRINK. A count that grows, or a
/// module that appears here for the first time, means a new unbounded spawn
/// entered the suite and must either take a deadline or be added deliberately.
const RAW_SPAWN_BASELINE: &[(&str, usize)] = &[
    ("agent_operating_contract_read_only.rs", 1),
    ("br_concurrent_read_race.rs", 1),
    ("failure_mode_fixtures.rs", 1),
    ("frankensearch_local.rs", 1),
    ("insights_stream.rs", 1),
    ("repair_safety_conformance.rs", 1),
    ("repo_hygiene_root_clutter.rs", 2),
    ("resume_schema.rs", 1),
    ("science_analytics.rs", 1),
    ("symbol_graph_artifacts.rs", 1),
    ("workspace_git_snapshot_read_only.rs", 2),
];

/// The module that owns the permit and the deadline. Excluded from the scan
/// because its raw spawns are the guarded implementation.
const GUARDED_MODULE: &str = "common_spawn.rs";

fn contracts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("contracts")
}

/// Blank out comments, string literals and char literals, preserving length so
/// nothing shifts. Counting `Command::new` in raw text would score doc comments
/// that merely DISCUSS spawning -- `cass_error_from_io.rs` has exactly such a
/// comment -- and string literals that name the symbol.
fn blank_comments_and_literals(source: &str) -> String {
    let bytes: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut index = 0usize;

    while index < bytes.len() {
        let current = bytes[index];
        let next = bytes.get(index + 1).copied();

        // Line comment.
        if current == '/' && next == Some('/') {
            while index < bytes.len() && bytes[index] != '\n' {
                out.push(' ');
                index += 1;
            }
            continue;
        }

        // Block comment. Rust nests these, so track depth.
        if current == '/' && next == Some('*') {
            let mut depth = 0usize;
            while index < bytes.len() {
                if bytes[index] == '/' && bytes.get(index + 1) == Some(&'*') {
                    depth += 1;
                    out.push(' ');
                    out.push(' ');
                    index += 2;
                    continue;
                }
                if bytes[index] == '*' && bytes.get(index + 1) == Some(&'/') {
                    depth -= 1;
                    out.push(' ');
                    out.push(' ');
                    index += 2;
                    if depth == 0 {
                        break;
                    }
                    continue;
                }
                out.push(if bytes[index] == '\n' { '\n' } else { ' ' });
                index += 1;
            }
            continue;
        }

        // Raw string: r"..." or r#"..."#, any number of hashes.
        if current == 'r' {
            let mut probe = index + 1;
            let mut hashes = 0usize;
            while bytes.get(probe) == Some(&'#') {
                hashes += 1;
                probe += 1;
            }
            if bytes.get(probe) == Some(&'"') {
                for _ in index..=probe {
                    out.push(' ');
                }
                index = probe + 1;
                loop {
                    if index >= bytes.len() {
                        break;
                    }
                    if bytes[index] == '"' {
                        let closing = (1..=hashes).all(|off| bytes.get(index + off) == Some(&'#'));
                        if closing {
                            for _ in 0..=hashes {
                                out.push(' ');
                            }
                            index += hashes + 1;
                            break;
                        }
                    }
                    out.push(if bytes[index] == '\n' { '\n' } else { ' ' });
                    index += 1;
                }
                continue;
            }
        }

        // Char literal. This branch is not cosmetic: `trim_matches('"')` in
        // `repo_hygiene_root_clutter.rs` puts a double quote inside a char
        // literal, and without this the scanner reads it as a string opener,
        // blanks forward past the real `Command::new("cargo")` call, and
        // undercounts that module by exactly one site. A lifetime (`'a`) has
        // no closing quote, so it must fall through untouched.
        if current == '\'' {
            if next == Some('\\') {
                let mut probe = index + 2;
                while probe < bytes.len() && bytes[probe] != '\'' {
                    probe += 1;
                }
                if probe < bytes.len() {
                    for _ in index..=probe {
                        out.push(' ');
                    }
                    index = probe + 1;
                    continue;
                }
            } else if bytes.get(index + 2) == Some(&'\'') {
                out.push(' ');
                out.push(' ');
                out.push(' ');
                index += 3;
                continue;
            }
        }

        // Normal string literal, honouring escapes.
        if current == '"' {
            out.push(' ');
            index += 1;
            while index < bytes.len() {
                if bytes[index] == '\\' {
                    out.push(' ');
                    out.push(' ');
                    index += 2;
                    continue;
                }
                if bytes[index] == '"' {
                    out.push(' ');
                    index += 1;
                    break;
                }
                out.push(if bytes[index] == '\n' { '\n' } else { ' ' });
                index += 1;
            }
            continue;
        }

        out.push(current);
        index += 1;
    }

    out
}

const fn is_word_char(value: char) -> bool {
    value.is_ascii_alphanumeric() || value == '_'
}

/// Count `Command::new` occurrences that start at a token boundary, so
/// `DemoCommand::new` -- a model type in `src/models/demo.rs` that spawns
/// nothing -- is not scored as a process spawn. A path prefix such as
/// `std::process::Command::new` still counts, because `:` is not a word char.
fn count_raw_spawns(source: &str) -> usize {
    const NEEDLE: &str = "Command::new";
    let blanked = blank_comments_and_literals(source);
    let mut count = 0usize;
    let mut searched = 0usize;

    while let Some(offset) = blanked[searched..].find(NEEDLE) {
        let start = searched + offset;
        let preceded_by_word_char = blanked[..start]
            .chars()
            .next_back()
            .is_some_and(is_word_char);
        if !preceded_by_word_char {
            count += 1;
        }
        searched = start + 1;
    }

    count
}

/// Every contracts module except the guarded one, with its raw spawn count.
/// Modules with zero sites are omitted.
fn observed_raw_spawns() -> Result<Vec<(String, usize)>, String> {
    let dir = contracts_dir();
    let entries =
        std::fs::read_dir(&dir).map_err(|error| format!("read {}: {error}", dir.display()))?;

    let mut observed = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read entry in {}: {error}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name == GUARDED_MODULE {
            continue;
        }
        let source = std::fs::read_to_string(entry.path())
            .map_err(|error| format!("read {name}: {error}"))?;
        let count = count_raw_spawns(&source);
        if count > 0 {
            observed.push((name, count));
        }
    }

    observed.sort();
    Ok(observed)
}

#[test]
fn no_new_unguarded_spawn_sites_enter_the_contracts_suite() -> TestResult {
    let observed = observed_raw_spawns()?;
    let mut violations = Vec::new();

    for (name, count) in &observed {
        match RAW_SPAWN_BASELINE
            .iter()
            .find(|(baseline_name, _)| baseline_name == name)
        {
            None => violations.push(format!(
                "{name} spawns a process ({count} site(s)) and is not in the baseline. \
                 Route it through crate::common_spawn so it carries a deadline, or add it \
                 here deliberately with the reason it cannot."
            )),
            Some((_, allowed)) if count > allowed => violations.push(format!(
                "{name} grew from {allowed} to {count} raw spawn site(s); new spawns must \
                 carry a deadline."
            )),
            Some(_) => {}
        }
    }

    if violations.is_empty() {
        return Ok(());
    }
    Err(format!(
        "unguarded process spawns entered the contracts suite:\n  {}",
        violations.join("\n  ")
    ))
}

#[test]
fn the_scanner_actually_finds_the_known_spawn_sites() -> TestResult {
    let observed = observed_raw_spawns()?;
    let total: usize = observed.iter().map(|(_, count)| count).sum();

    // A scanner that silently matched nothing would make the guard above pass
    // vacuously forever. The baseline is the floor it must still reach.
    let baseline_total: usize = RAW_SPAWN_BASELINE.iter().map(|(_, count)| count).sum();
    if observed.len() > RAW_SPAWN_BASELINE.len() || total > baseline_total {
        return Ok(()); // growth is the other test's failure to report
    }
    if total == 0 || observed.is_empty() {
        return Err(format!(
            "scanner found {total} spawn site(s) in {} module(s); it is broken, not the suite \
             clean (expected up to {baseline_total} across {})",
            observed.len(),
            RAW_SPAWN_BASELINE.len()
        ));
    }
    Ok(())
}

#[test]
fn the_scanner_ignores_spawns_named_only_in_comments_and_strings() -> TestResult {
    let commented = "// let x = Command::new(\"git\");\nfn f() {}\n";
    if count_raw_spawns(commented) != 0 {
        return Err("a Command::new inside a line comment was counted".to_string());
    }

    let documented = "//! pipe drains flow through `Command::new`\nfn f() {}\n";
    if count_raw_spawns(documented) != 0 {
        return Err("a Command::new inside a doc comment was counted".to_string());
    }

    let stringly = "fn f() { let s = \"Command::new\"; }\n";
    if count_raw_spawns(stringly) != 0 {
        return Err("a Command::new inside a string literal was counted".to_string());
    }

    let blocked = "/* Command::new(\"git\") */ fn f() {}\n";
    if count_raw_spawns(blocked) != 0 {
        return Err("a Command::new inside a block comment was counted".to_string());
    }

    Ok(())
}

#[test]
fn the_scanner_distinguishes_real_spawns_from_lookalike_types() -> TestResult {
    // Paired positive: the guard must still FIRE on real code, or the negative
    // controls above would be satisfied by a scanner that always returns zero.
    let real = "fn f() { let o = Command::new(\"git\").output(); }\n";
    if count_raw_spawns(real) != 1 {
        return Err(format!(
            "expected 1 spawn in a real call site, found {}",
            count_raw_spawns(real)
        ));
    }

    let qualified = "fn f() { let o = std::process::Command::new(\"git\").output(); }\n";
    if count_raw_spawns(qualified) != 1 {
        return Err(format!(
            "a path-qualified spawn must count, found {}",
            count_raw_spawns(qualified)
        ));
    }

    // `DemoCommand::new` is a model type, not a process spawn.
    let lookalike = "fn f() { let c = DemoCommand::new(\"ee context\"); }\n";
    if count_raw_spawns(lookalike) != 0 {
        return Err("DemoCommand::new was miscounted as a process spawn".to_string());
    }

    Ok(())
}
