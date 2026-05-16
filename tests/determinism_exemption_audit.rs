//! N4.4 determinism lint exemption audit.
//!
//! `clippy::disallowed_methods` is the first enforcement layer for ambient
//! randomness in deterministic paths. Any exemption must be rare, justified in
//! source, and counted against a versioned baseline.

use std::path::{Path, PathBuf};

const BASELINE: &str =
    include_str!("fixtures/determinism_disallowed_methods_exemption_baseline.txt");
const EXEMPTION_PATTERNS: &[&str] = &[
    "#[allow(clippy::disallowed_methods)]",
    "#[expect(clippy::disallowed_methods)]",
];
const JUSTIFICATION_MARKERS: &[&str] = &["why:", "because", "justification:", "determinism:"];

#[derive(Debug)]
struct Exemption {
    path: PathBuf,
    line: usize,
}

#[test]
fn disallowed_methods_exemptions_are_justified_and_counted() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src_dir = manifest_dir.join("src");
    let exemptions = collect_exemptions(&src_dir);
    let baseline = parse_baseline(Baseline::raw());

    let unjustified = exemptions
        .iter()
        .filter(|exemption| !has_nearby_justification(exemption))
        .collect::<Vec<_>>();
    assert!(
        unjustified.is_empty(),
        "clippy::disallowed_methods exemptions need a nearby justification comment: {unjustified:?}"
    );

    assert!(
        exemptions.len() <= baseline,
        "clippy::disallowed_methods exemption count grew from baseline {baseline} to {}. Review the exemption, add a justification, and update the N4.4 baseline only when accepted.",
        exemptions.len()
    );
}

struct Baseline;

impl Baseline {
    fn raw() -> &'static str {
        BASELINE
    }
}

fn parse_baseline(raw: &str) -> usize {
    raw.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("allowed_exemptions = "))
        .unwrap_or("0")
        .parse()
        .expect("determinism exemption baseline must be a usize")
}

fn collect_exemptions(root: &Path) -> Vec<Exemption> {
    let mut exemptions = Vec::new();
    collect_exemptions_from_dir(root, &mut exemptions);
    exemptions
}

fn collect_exemptions_from_dir(dir: &Path, exemptions: &mut Vec<Exemption>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_exemptions_from_dir(&path, exemptions);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            collect_exemptions_from_file(&path, exemptions);
        }
    }
}

fn collect_exemptions_from_file(path: &Path, exemptions: &mut Vec<Exemption>) {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(_) => return,
    };

    for (index, line) in source.lines().enumerate() {
        if EXEMPTION_PATTERNS
            .iter()
            .any(|pattern| line.contains(pattern))
        {
            exemptions.push(Exemption {
                path: path.to_path_buf(),
                line: index + 1,
            });
        }
    }
}

fn has_nearby_justification(exemption: &Exemption) -> bool {
    let source = match std::fs::read_to_string(&exemption.path) {
        Ok(source) => source,
        Err(_) => return false,
    };
    let lines = source.lines().collect::<Vec<_>>();
    let start = exemption.line.saturating_sub(1);
    let end = (start + 4).min(lines.len());

    lines[start..end].iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.trim_start().starts_with("//")
            && JUSTIFICATION_MARKERS
                .iter()
                .any(|marker| lower.contains(marker))
    })
}

#[cfg(test)]
mod self_tests {
    use super::parse_baseline;

    #[test]
    fn parses_zero_baseline() {
        assert_eq!(parse_baseline("allowed_exemptions = 0\n"), 0);
    }
}
