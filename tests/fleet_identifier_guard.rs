//! Public-repo hygiene guard: no private infrastructure identifiers in the tree.
//!
//! This repository is public. Host names, tailnet addresses and operator
//! account names from the maintainer's private build fleet must never appear
//! in tracked files — not in source, not in docs, not in published JSON
//! schemas, and not in test fixtures or golden files. Fixtures should use
//! neutral placeholders instead: `worker-01` / `worker-a` / `windows-host-1`
//! for hosts, RFC 5737 documentation addresses (`198.51.100.x`) for public
//! addresses, and the CGNAT range (`100.64.0.x`) for tailnet peers.
//!
//! Addresses are checked by range rather than by a fixed prefix list, so a
//! *new* machine's address is caught the first time it is written down.
//!
//! The guard scans every git-tracked file — that is exactly the set that gets
//! published — falling back to a filesystem walk when git is unavailable, and
//! fails with the exact file, line and token whenever a banned identifier
//! reappears, so a future fixture refresh cannot silently republish them.
//!
//! Deliberately **not** covered:
//!
//! * `/data/projects` — this is `rch`'s publicly documented default project
//!   root (see the remote_compilation_helper README), not a private path. It
//!   is load-bearing in production code (`core::verify_ledger` path-topology
//!   normalisation, `core::doctor`'s dependency contract matrix) and in
//!   `Cargo.toml` patch paths, so it stays.
//! * `fuzz/corpus/**` — libFuzzer-generated mutation noise that happens to
//!   contain short byte sequences; it is machine-generated input, not prose.
//! * Untracked `.beads` state (database, locks, `.br_history`, recovery
//!   snapshots). Only the five files git tracks there are published.
//!
//! If a banned token ever becomes legitimate (for example `css` in an HTML
//! output feature), add the specific file to `ALLOWLISTED_FILES` with a
//! comment explaining why, rather than weakening the token list.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Directory names that are never scanned, relative to the repository root or
/// at any depth.
const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    "target",
    "target-local",
    "node_modules",
    ".venv",
    "__pycache__",
];

/// Repository-relative path prefixes that are never scanned.
const SKIPPED_PREFIXES: &[&str] = &["fuzz/corpus/"];

/// Dot-directories the filesystem-walk fallback is allowed to descend into.
/// Every other dot-directory holds local state (beads history and recovery
/// snapshots, editor caches) that git does not publish; on a build worker the
/// synced tree can still carry stale copies of it, so the walk must not look.
const WALKED_DOT_DIRECTORIES: &[&str] = &[".beads", ".cargo", ".github"];

/// The only `.beads` entries git tracks; the rest of that directory is local
/// database, lock and history state (see `.beads/.gitignore`).
const TRACKED_BEADS_FILES: &[&str] = &[
    ".beads/.gitignore",
    ".beads/config.yaml",
    ".beads/deletions.jsonl",
    ".beads/issues.jsonl",
    ".beads/metadata.json",
];

/// Repository-relative files exempt from the scan, each with a reason.
const ALLOWLISTED_FILES: &[(&str, &str)] = &[
    // This file necessarily spells the banned tokens out in order to ban them.
    (
        "tests/fleet_identifier_guard.rs",
        "the guard's own token list",
    ),
];

/// Whole-word host names from the private build fleet, paired with whether the
/// match ignores case. `css` is matched case-sensitively so that prose about
/// stylesheets ("Frontend CSS tweaks") stays legal while the host name does
/// not; every other name has no other meaning here.
const BANNED_HOST_WORDS: &[(&str, bool)] = &[
    ("trj", true),
    ("csd", true),
    ("css", false),
    ("ts1", true),
    ("ts2", true),
    ("hz1", true),
    ("hz2", true),
    ("hz3", true),
    ("hz4", true),
    ("fmd", true),
    ("yto", true),
    ("omarchy", true),
    ("wsurf", true),
    ("wlap", true),
];

/// Substrings that are banned wherever they occur: machine names and the
/// maintainer's Windows workstation.
const BANNED_SUBSTRINGS: &[&str] = &[
    "mac-mini-old",
    "mac-mini-max",
    "SurfaceBookJE",
    "SURFACEBOOKJE",
];

/// Globally routable addresses that are legitimately named in the tree, with a
/// reason. Everything else routable is treated as private infrastructure.
const ALLOWLISTED_PUBLIC_IPS: &[(&str, &str)] = &[("8.8.8.8", "public DNS resolver example")];

/// Whole-word account names, matched ignoring case.
const BANNED_ACCOUNT_WORDS: &[&str] = &["jeffr"];

/// A single guard violation.
#[derive(Debug)]
struct Violation {
    path: String,
    line: usize,
    token: String,
    excerpt: String,
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// True when `needle` occurs in `haystack` delimited by non-word bytes.
/// `needle` must be lowercase ASCII; `ignore_case` folds the haystack.
fn contains_word(haystack: &str, needle: &str, ignore_case: bool) -> bool {
    let folded;
    let haystack = if ignore_case {
        folded = haystack.to_ascii_lowercase();
        folded.as_str()
    } else {
        haystack
    };
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut start = 0usize;
    while let Some(offset) = haystack[start..].find(needle) {
        let begin = start + offset;
        let end = begin + needle_bytes.len();
        let before_ok = begin == 0 || !is_word_byte(bytes[begin - 1]);
        let after_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        start = begin + 1;
        if start >= haystack.len() {
            break;
        }
    }
    false
}

/// True when the line contains `vmi` followed by six or more digits, the shape
/// of the fleet's VPS host names. Case-insensitive.
fn contains_vps_hostname(line: &str) -> bool {
    let folded = line.to_ascii_lowercase();
    let bytes = folded.as_bytes();
    let mut index = 0usize;
    while index + 3 <= bytes.len() {
        if bytes[index..].starts_with(b"vmi") && (index == 0 || !is_word_byte(bytes[index - 1])) {
            let digits = bytes[index + 3..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if digits >= 6 {
                return true;
            }
        }
        index += 1;
    }
    false
}

/// True when `octets` is in a range that is safe to write down: unspecified,
/// private, loopback, link-local, CGNAT (which is where the mesh fixtures'
/// synthetic tailnet addresses live), documentation, benchmarking, multicast
/// or reserved.
fn is_non_routable(octets: [u8; 4]) -> bool {
    match octets {
        [0, ..] | [10, ..] | [127, ..] => true,
        [100, second, ..] if (64..=127).contains(&second) => true,
        [169, 254, ..] | [192, 168, ..] => true,
        [172, second, ..] if (16..=31).contains(&second) => true,
        [192, 0, 0 | 2, _] | [192, 88, 99, _] => true,
        [198, 18 | 19, ..] | [198, 51, 100, _] | [203, 0, 113, _] => true,
        [first, ..] if first >= 224 => true,
        _ => false,
    }
}

/// Parse `text` as a dotted quad, rejecting empty octets, octets longer than
/// three digits and octets above 255, so that version strings such as
/// `1.2.3.4444` do not parse as addresses.
fn parse_dotted_quad(text: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut parts = text.split('.');
    for slot in &mut octets {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse::<u8>().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(octets)
}

/// Find a globally routable IPv4 address in `line`. Candidates must be
/// delimited by non-word, non-dot bytes so that bead ids such as
/// `bd-17c65.10.17.1` and version strings are not misread as addresses.
fn contains_public_ip(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        let preceded_ok =
            index == 0 || !(is_word_byte(bytes[index - 1]) || bytes[index - 1] == b'.');
        if !preceded_ok || !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let span_end = index
            + bytes[index..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit() || **byte == b'.')
                .count();
        // Sentence punctuation: `... on 203.0.113.5.` must still parse.
        let mut end = span_end;
        while end > index && bytes[end - 1] == b'.' {
            end -= 1;
        }
        let followed_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        let candidate = &line[index..end];
        if followed_ok
            && let Some(octets) = parse_dotted_quad(candidate)
            && !is_non_routable(octets)
            && !ALLOWLISTED_PUBLIC_IPS
                .iter()
                .any(|(allowed, _)| *allowed == candidate)
        {
            return Some(candidate.to_owned());
        }
        index = span_end.max(index + 1);
    }
    None
}

fn banned_token_in_line(line: &str) -> Option<String> {
    if contains_vps_hostname(line) {
        return Some("vmi<digits> host name".to_owned());
    }
    if let Some(address) = contains_public_ip(line) {
        return Some(format!("routable address {address}"));
    }
    for needle in BANNED_SUBSTRINGS {
        if line.contains(needle) {
            return Some((*needle).to_owned());
        }
    }
    for (word, ignore_case) in BANNED_HOST_WORDS {
        if contains_word(line, word, *ignore_case) {
            return Some((*word).to_owned());
        }
    }
    for word in BANNED_ACCOUNT_WORDS {
        if contains_word(line, word, true) {
            return Some((*word).to_owned());
        }
    }
    None
}

fn is_skipped_directory(name: &str) -> bool {
    SKIPPED_DIRECTORIES.contains(&name) || name.starts_with(".rch-target")
}

fn is_skipped_path(relative: &str) -> bool {
    if SKIPPED_PREFIXES
        .iter()
        .any(|prefix| relative.starts_with(prefix))
        || ALLOWLISTED_FILES.iter().any(|(path, _)| *path == relative)
    {
        return true;
    }
    // Untracked beads state, which the walk fallback would otherwise reach.
    relative.starts_with(".beads/") && !TRACKED_BEADS_FILES.contains(&relative)
}

fn collect_files(root: &Path, directory: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() {
            if is_skipped_directory(&name)
                || (name.starts_with('.') && !WALKED_DOT_DIRECTORIES.contains(&name.as_str()))
            {
                continue;
            }
            collect_files(root, &path, out);
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map(|rest| rest.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if is_skipped_path(&relative) {
                continue;
            }
            out.push(path);
        }
    }
}

/// Files git actually tracks, which is exactly the set that gets published.
/// Returns `None` when git is unavailable (for example on a build worker that
/// received the sources without `.git`), and the caller falls back to walking
/// the tree.
fn tracked_files(root: &Path) -> Option<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8(output.stdout).ok()?;
    let files: Vec<PathBuf> = listing
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .filter(|entry| !is_skipped_path(entry))
        .filter(|entry| {
            !Path::new(entry)
                .components()
                .any(|component| is_skipped_directory(&component.as_os_str().to_string_lossy()))
        })
        .map(|entry| root.join(entry))
        .filter(|path| path.is_file())
        .collect();
    if files.is_empty() { None } else { Some(files) }
}

fn scan(root: &Path) -> Vec<Violation> {
    let mut files = tracked_files(root).unwrap_or_else(|| {
        let mut walked = Vec::new();
        collect_files(root, root, &mut walked);
        walked
    });
    files.sort();

    let mut violations = Vec::new();
    for path in files {
        // Binary blobs (databases, images, compiled artefacts) are not prose;
        // anything that is not valid UTF-8 is skipped rather than guessed at.
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(root)
            .map(|rest| rest.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        for (index, line) in contents.lines().enumerate() {
            if let Some(token) = banned_token_in_line(line) {
                violations.push(Violation {
                    path: relative.clone(),
                    line: index + 1,
                    token,
                    excerpt: line.chars().take(160).collect(),
                });
                if violations.len() >= 200 {
                    return violations;
                }
            }
        }
    }
    violations
}

#[test]
fn no_private_fleet_identifiers_are_published() {
    let root = repository_root();
    let violations = scan(&root);
    assert!(
        violations.is_empty(),
        "private fleet identifiers found in {} place(s); replace them with \
         neutral placeholders such as worker-01 / worker-a / windows-host-1:\n{}",
        violations.len(),
        violations
            .iter()
            .map(|violation| format!(
                "  {}:{}: [{}] {}",
                violation.path, violation.line, violation.token, violation.excerpt
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn guard_detects_each_banned_identifier_shape() {
    assert!(banned_token_in_line("worker_id = \"vmi1234567\"").is_some());
    assert!(banned_token_in_line("WORKER VMI1234567 REPORTED").is_some());
    assert!(banned_token_in_line("selected worker: csd").is_some());
    assert!(banned_token_in_line("host = 209.145.1.2").is_some());
    assert!(banned_token_in_line("the proof ran on 209.145.1.2.").is_some());
    assert!(banned_token_in_line("C:\\Users\\jeffr\\ee.exe").is_some());
    assert!(banned_token_in_line("mac-mini-old launchd unit").is_some());
    // Host names are caught regardless of case...
    assert!(banned_token_in_line("proof host: HZ1 lib gate").is_some());
    assert!(banned_token_in_line("Isolated CSD pinned tree").is_some());
    // ...but lower-case `css` is always the host, never the language.
    assert!(banned_token_in_line("selected worker css").is_some());
}

#[test]
fn guard_does_not_flag_neutral_placeholders() {
    assert!(banned_token_in_line("worker_id = \"worker-01\"").is_none());
    assert!(banned_token_in_line("selected worker: worker-c").is_none());
    assert!(banned_token_in_line("host = 100.64.0.10").is_none());
    assert!(banned_token_in_line("C:\\Users\\dev\\ee.exe").is_none());
    assert!(banned_token_in_line("mac-worker-2 launchd unit").is_none());
    // Word boundaries: banned host words must not match inside longer words.
    assert!(banned_token_in_line("discuss the ytov handler").is_none());
    assert!(banned_token_in_line("vmi12345 is too short to be a host").is_none());
    // Decimal numbers and identifiers that look address-shaped are not.
    assert!(banned_token_in_line("p99 latency 51.222 ms").is_none());
    assert!(banned_token_in_line("bead bd-17c65.10.17.1 closed").is_none());
    assert!(banned_token_in_line("version 1.2.3.4444 shipped").is_none());
    // Non-routable ranges are how fixtures are supposed to spell addresses.
    assert!(banned_token_in_line("peer 100.64.0.10:41888").is_none());
    assert!(banned_token_in_line("bind 127.0.0.1:8080").is_none());
    assert!(banned_token_in_line("doc example 198.51.100.11").is_none());
    assert!(banned_token_in_line("resolver 8.8.8.8").is_none());
    // ...except upper-case CSS, which is the stylesheet language, not the host.
    assert!(banned_token_in_line("Frontend CSS tweaks are unrelated").is_none());
}
