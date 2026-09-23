//! bd-rvrj2 guard: no NEW test may spawn the `ee` binary without isolating its
//! data dir.
//!
//! `ee` keeps its model cache, global store and catalog under `XDG_DATA_HOME`
//! (else `HOME/.local/share`). A spawn that inherits the runner's environment
//! reads whatever the worker holds, so its verdict depends on the host.
//!
//! Each function that spawns `ee` is classified by its ENCLOSING fn body,
//! because helpers set environment in separate statements. It is isolated if
//! that body sets `"HOME"` or `"XDG_DATA_HOME"`, calls `.env_clear()`, or goes
//! through `isolated_ee_command` (tests/support/isolated_ee.rs). Un-isolated
//! functions are counted per file and held to a SHRINK-ONLY baseline
//! (tests/fixtures/ee_spawn_isolation_baseline.tsv): a file above its row
//! fails, and a row above reality also fails so it must come down when a spawn
//! is migrated. The census that produced the baseline is on bd-rvrj2.
//!
//! Neural is opt-in (bd-rvrj2 A2): the helper is the only test code that may
//! read the model-fixture variable, so a test cannot quietly score against a
//! model the runner happens to provide.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/isolated_ee.rs"]
mod isolated_ee;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// Built with concat! so this guard does not count itself as a spawn site.
const SPAWN_TOKEN: &str = concat!("CARGO_BIN_", "EXE_ee");
const ISOLATION_MARKERS: [&str; 4] = [
    "\"HOME\"",
    "\"XDG_DATA_HOME\"",
    ".env_clear()",
    "isolated_ee_command(",
];
const BASELINE: &str = "tests/fixtures/ee_spawn_isolation_baseline.tsv";
/// Built with concat! so this guard is not itself a reader of the opt-in.
const MODEL_FIXTURE_TOKEN: &str = concat!("EE_EMBED_MODEL_", "FIXTURE_DIR");
const HELPER: &str = "tests/support/isolated_ee.rs";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `[pub[(..)]] [async] [const] [unsafe] fn <name>`, at the start of a line.
fn is_fn_header(line: &str) -> bool {
    let mut rest = line.trim_start();
    if let Some(after) = rest.strip_prefix("pub") {
        let after = match after.strip_prefix('(') {
            Some(inner) => match inner.find(')') {
                Some(close) => &inner[close + 1..],
                None => return false,
            },
            None => after,
        };
        if !after.starts_with(char::is_whitespace) {
            return false;
        }
        rest = after.trim_start();
    }
    for keyword in ["async", "const", "unsafe"] {
        if let Some(after) = rest.strip_prefix(keyword)
            && after.starts_with(char::is_whitespace)
        {
            rest = after.trim_start();
        }
    }
    let Some(after) = rest.strip_prefix("fn") else {
        return false;
    };
    after.starts_with(char::is_whitespace)
        && after
            .trim_start()
            .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
}

/// The brace-matched body of the fn enclosing line `idx`, keyed by its header
/// line. Falls back to a window around `idx` when no fn encloses it.
fn enclosing_fn(lines: &[&str], idx: usize) -> (usize, String) {
    for start in (0..=idx).rev() {
        if !is_fn_header(lines[start]) {
            continue;
        }
        let mut depth: i64 = 0;
        let mut opened = false;
        let mut body = Vec::new();
        for (j, line) in lines.iter().enumerate().skip(start) {
            body.push(*line);
            depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
            if line.contains('{') {
                opened = true;
            }
            if opened && depth <= 0 {
                if j >= idx {
                    return (start, body.join("\n"));
                }
                break;
            }
        }
    }
    let low = idx.saturating_sub(40);
    let high = (idx + 40).min(lines.len());
    (idx, lines[low..high].join("\n"))
}

/// Number of distinct un-isolated spawning fns in one source text.
fn unisolated_spawn_fns(text: &str) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    let mut seen = BTreeSet::new();
    let mut unisolated = 0;
    for (idx, line) in lines.iter().enumerate() {
        if !line.contains(SPAWN_TOKEN) {
            continue;
        }
        let (start, body) = enclosing_fn(&lines, idx);
        if !seen.insert(start) {
            continue;
        }
        if !ISOLATION_MARKERS.iter().any(|marker| body.contains(marker)) {
            unisolated += 1;
        }
    }
    unisolated
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Un-isolated spawn fns per repo-relative file (files with zero omitted).
fn census(root: &Path) -> BTreeMap<String, usize> {
    let mut files = Vec::new();
    rust_sources(&root.join("tests"), &mut files);
    rust_sources(&root.join("src"), &mut files);
    let mut counts = BTreeMap::new();
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if !text.contains(SPAWN_TOKEN) {
            continue;
        }
        let count = unisolated_spawn_fns(&text);
        if count > 0 {
            let rel = path
                .strip_prefix(root)
                .expect("source under repo root")
                .to_string_lossy()
                .replace('\\', "/");
            counts.insert(rel, count);
        }
    }
    counts
}

fn parse_baseline(text: &str) -> BTreeMap<String, usize> {
    let mut rows = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (count, path) = line
            .split_once('\t')
            .expect("baseline row is <count>\\t<path>");
        rows.insert(
            path.trim().to_owned(),
            count.trim().parse().expect("baseline count is a number"),
        );
    }
    rows
}

/// Shrink-only comparison: above a row fails, below a row fails (lower it).
fn ratchet_findings(
    actual: &BTreeMap<String, usize>,
    baseline: &BTreeMap<String, usize>,
) -> Vec<String> {
    let mut findings = Vec::new();
    let files: BTreeSet<&String> = actual.keys().chain(baseline.keys()).collect();
    for file in files {
        let now = actual.get(file).copied().unwrap_or(0);
        let allowed = baseline.get(file).copied().unwrap_or(0);
        if now > allowed {
            findings.push(format!(
                "{file}: {now} un-isolated ee spawn fn(s), baseline allows {allowed}. Spawn ee through \
                 isolated_ee_command (tests/support/isolated_ee.rs) or set HOME/XDG_DATA_HOME."
            ));
        } else if now < allowed {
            findings.push(format!(
                "{file}: baseline allows {allowed} but only {now} remain; lower the row to {now} (shrink-only)."
            ));
        }
    }
    findings
}

#[test]
fn ee_spawns_do_not_add_unisolated_sites() {
    let root = repo_root();
    let actual = census(&root);
    // Empty-world guard: a census that finds nothing is a broken instrument.
    assert!(
        actual.values().sum::<usize>() > 0,
        "census found no un-isolated ee spawns; the scan is broken or the baseline is obsolete"
    );
    // Positive control: the isolated helper spawns ee and must NOT be counted.
    let helper = fs::read_to_string(root.join("tests/support/isolated_ee.rs"))
        .expect("tests/support/isolated_ee.rs exists");
    assert!(helper.contains(SPAWN_TOKEN), "helper must spawn ee");
    assert_eq!(
        unisolated_spawn_fns(&helper),
        0,
        "the isolated helper must classify isolated"
    );

    let baseline = parse_baseline(
        &fs::read_to_string(root.join(BASELINE)).expect("ee spawn isolation baseline exists"),
    );
    let findings = ratchet_findings(&actual, &baseline);
    assert!(
        findings.is_empty(),
        "ee spawn isolation ratchet (bd-rvrj2): {} finding(s):\n{}\n\ncurrent census ({} un-isolated fns):\n{}",
        findings.len(),
        findings.join("\n"),
        actual.values().sum::<usize>(),
        actual
            .iter()
            .map(|(file, count)| format!("{count}\t{file}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn classifier_controls_both_directions() {
    let spawn = format!("env!(\"{SPAWN_TOKEN}\")");
    let unisolated = format!(
        "fn run(ws: &Path) -> Output {{\n    Command::new({spawn}).arg(\"--workspace\").arg(ws).output().unwrap()\n}}\n"
    );
    assert_eq!(
        unisolated_spawn_fns(&unisolated),
        1,
        "a bare inherited-env spawn is un-isolated"
    );

    let home = format!(
        "pub fn run(home: &Path) -> Output {{\n    Command::new({spawn})\n        .env(\"HOME\", home)\n        .output()\n        .unwrap()\n}}\n"
    );
    assert_eq!(unisolated_spawn_fns(&home), 0, "setting HOME isolates");

    let later_statement = format!(
        "fn command(data: &Path) -> Command {{\n    let mut command = Command::new({spawn});\n    command.env(\"XDG_DATA_HOME\", data);\n    command\n}}\n"
    );
    assert_eq!(
        unisolated_spawn_fns(&later_statement),
        0,
        "XDG_DATA_HOME set in a later statement of the same fn isolates"
    );

    let cleared =
        format!("async fn run() {{\n    let _ = Command::new({spawn}).env_clear().status();\n}}\n");
    assert_eq!(unisolated_spawn_fns(&cleared), 0, "env_clear isolates");

    let helper_routed = "fn run(root: &Path) -> Output {\n    isolated_ee_command(root).unwrap().output().unwrap()\n}\n";
    assert_eq!(
        unisolated_spawn_fns(helper_routed),
        0,
        "a helper-routed call is not a spawn site"
    );

    let mixed = format!("{unisolated}\n{home}\n{unisolated}");
    assert_eq!(
        unisolated_spawn_fns(&mixed),
        2,
        "each un-isolated fn counts once, isolated ones not at all"
    );

    let pinned_only = format!(
        "fn run(ws: &Path) -> Output {{\n    Command::new({spawn}).env(\"EE_EMBED_DOWNLOAD\", \"off\").arg(ws).output().unwrap()\n}}\n"
    );
    assert_eq!(
        unisolated_spawn_fns(&pinned_only),
        1,
        "pinning the model alone does not isolate the global store"
    );
}

/// The environment a `Command` will apply: `Some(value)` set, `None` removed.
fn envs_of(command: &std::process::Command) -> BTreeMap<String, Option<String>> {
    command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect()
}

/// Whether source text names the model opt-in as a string literal anywhere
/// but an `env_remove(..)` line. Scrubbing the variable is not honouring it,
/// and a bare mention in a comment is not a read.
fn honours_model_fixture(text: &str) -> bool {
    let quoted = format!("\"{MODEL_FIXTURE_TOKEN}\"");
    text.lines()
        .any(|line| line.contains(&quoted) && !line.contains("env_remove("))
}

/// Repo-relative `.rs` files under tests/ that honour the model opt-in.
fn model_fixture_readers(root: &Path) -> BTreeSet<String> {
    let mut files = Vec::new();
    rust_sources(&root.join("tests"), &mut files);
    files
        .into_iter()
        .filter(|path| fs::read_to_string(path).is_ok_and(|text| honours_model_fixture(&text)))
        .map(|path| {
            path.strip_prefix(root)
                .expect("source under repo root")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

#[test]
fn isolated_helper_points_every_data_dir_under_its_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("isolated");
    let command = isolated_ee::isolated_ee_command(&root).expect("isolated command");
    let envs = envs_of(&command);
    for key in [
        "HOME",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
    ] {
        let value = envs
            .get(key)
            .and_then(Clone::clone)
            .unwrap_or_else(|| panic!("{key} must be set"));
        assert!(
            Path::new(&value).starts_with(&root) && Path::new(&value).is_dir(),
            "{key}={value} must be an existing dir under {}",
            root.display()
        );
    }
    assert_eq!(envs.get("EE_EMBED_DOWNLOAD"), Some(&Some("off".to_owned())));
    for key in ["EE_WORKSPACE", "EE_WORKSPACE_REGISTRY"]
        .into_iter()
        .chain(isolated_ee::MODEL_SELECTION_ENV)
    {
        assert_eq!(envs.get(key), Some(&None), "{key} must be removed");
    }
    assert!(
        isolated_ee::MODEL_SELECTION_ENV.contains(&"EE_EMBED_MODEL_DIR")
            && isolated_ee::MODEL_SELECTION_ENV.contains(&MODEL_FIXTURE_TOKEN),
        "an inherited model dir or fixture must never reach a non-opted-in spawn"
    );
    // It really spawns the binary.
    let output = isolated_ee::isolated_ee_command(&root)
        .expect("isolated command")
        .arg("--version")
        .output()
        .expect("spawn ee");
    assert!(output.status.success(), "ee --version failed: {output:?}");
}

#[test]
fn ratchet_controls_both_directions() {
    let rows = |pairs: &[(&str, usize)]| {
        pairs
            .iter()
            .map(|(file, count)| ((*file).to_owned(), *count))
            .collect::<BTreeMap<_, _>>()
    };
    let baseline = rows(&[("tests/a.rs", 2), ("tests/b.rs", 1)]);
    assert!(ratchet_findings(&rows(&[("tests/a.rs", 2), ("tests/b.rs", 1)]), &baseline).is_empty());

    let above = ratchet_findings(&rows(&[("tests/a.rs", 3), ("tests/b.rs", 1)]), &baseline);
    assert_eq!(above.len(), 1);
    assert!(above[0].contains("baseline allows 2"), "{above:?}");

    let below = ratchet_findings(&rows(&[("tests/a.rs", 1), ("tests/b.rs", 1)]), &baseline);
    assert_eq!(below.len(), 1);
    assert!(below[0].contains("lower the row to 1"), "{below:?}");

    let new_file = ratchet_findings(
        &rows(&[("tests/a.rs", 2), ("tests/b.rs", 1), ("tests/new.rs", 1)]),
        &baseline,
    );
    assert_eq!(new_file.len(), 1);
    assert!(new_file[0].contains("tests/new.rs") && new_file[0].contains("baseline allows 0"));

    assert_eq!(
        parse_baseline("# comment\n\n2\ttests/a.rs\n1\ttests/b.rs\n"),
        baseline,
        "baseline rows parse as <count>\\t<path>"
    );
}

#[test]
fn model_fixture_is_honoured_only_by_the_helper() {
    assert_eq!(isolated_ee::MODEL_FIXTURE_ENV, MODEL_FIXTURE_TOKEN);
    // Positive control and empty-world guard in one: the helper must be found.
    let expected: BTreeSet<String> = [HELPER.to_owned()].into_iter().collect();
    assert_eq!(
        model_fixture_readers(&repo_root()),
        expected,
        "only {HELPER} may read {MODEL_FIXTURE_TOKEN}; call isolated_ee::model_fixture_root \
         or isolated_ee_command_with_model instead"
    );
}

#[test]
fn model_fixture_reader_controls_both_directions() {
    let read = format!("let root = std::env::var_os(\"{MODEL_FIXTURE_TOKEN}\");\n");
    assert!(
        honours_model_fixture(&read),
        "a direct read honours the opt-in"
    );
    let constant = format!("pub const ENV: &str = \"{MODEL_FIXTURE_TOKEN}\";\n");
    assert!(
        honours_model_fixture(&constant),
        "naming it as a literal counts"
    );
    let scrub = format!("command.env_remove(\"{MODEL_FIXTURE_TOKEN}\");\n");
    assert!(
        !honours_model_fixture(&scrub),
        "scrubbing it is not honouring it"
    );
    let comment = format!("// {MODEL_FIXTURE_TOKEN} is documented elsewhere\n");
    assert!(
        !honours_model_fixture(&comment),
        "a bare mention is not a read"
    );
    assert!(
        !honours_model_fixture("std::env::var_os(\"EE_EMBED_MODEL_DIR\")"),
        "a different variable is not the opt-in"
    );
}

#[test]
fn model_opt_in_controls_both_directions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("isolated");

    let unset = isolated_ee::isolated_ee_command_with_model_from(&root, None)
        .expect_err("no opt-in, no model");
    assert!(unset.contains(MODEL_FIXTURE_TOKEN), "{unset}");
    let empty = isolated_ee::model_fixture_root_from(Some(OsStr::new("")))
        .expect_err("an empty value is not an opt-in");
    assert!(empty.contains(MODEL_FIXTURE_TOKEN), "{empty}");
    let absent = temp.path().join("absent");
    let not_dir = isolated_ee::model_fixture_root_from(Some(absent.as_os_str()))
        .expect_err("a missing fixture dir");
    assert!(not_dir.contains("is not a directory"), "{not_dir}");

    let fixture = temp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("fixture dir");
    let incomplete =
        isolated_ee::isolated_ee_command_with_model_from(&root, Some(fixture.as_os_str()))
            .expect_err("a fixture without the model files");
    assert!(incomplete.contains("model.safetensors"), "{incomplete}");

    for file in isolated_ee::MODEL_FIXTURE_FILES {
        let path = fixture.join(file);
        fs::create_dir_all(path.parent().expect("parent")).expect("model dir");
        fs::write(&path, b"placeholder").expect("placeholder");
    }
    assert_eq!(
        isolated_ee::model_fixture_root_from(Some(fixture.as_os_str())),
        Ok(fixture.clone())
    );
    let command =
        isolated_ee::isolated_ee_command_with_model_from(&root, Some(fixture.as_os_str()))
            .expect("opted-in command");
    let envs = envs_of(&command);
    assert_eq!(
        envs.get("EE_EMBED_MODEL_DIR"),
        Some(&Some(fixture.to_string_lossy().into_owned()))
    );
    assert_eq!(envs.get("EE_EMBED_DOWNLOAD"), Some(&Some("off".to_owned())));
    assert_eq!(
        envs.get(MODEL_FIXTURE_TOKEN),
        Some(&None),
        "the child never sees the opt-in itself"
    );
    let home = envs.get("HOME").and_then(Clone::clone).expect("HOME set");
    assert!(
        Path::new(&home).starts_with(&root),
        "opting in keeps HOME isolated: {home}"
    );
}
