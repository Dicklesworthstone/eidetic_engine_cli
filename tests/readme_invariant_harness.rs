use std::collections::HashSet;

use regex_lite::Regex;
use toml_edit::DocumentMut;

const MANIFEST: &str = include_str!("readme_invariants/manifest.toml");
const README: &str = include_str!("../README.md");

const TRIGGER_PATTERN: &str = r"(?i)\b(always|never|every|must|cannot|deterministic|deterministically|byte-stable|byte-identical|reproducible|surfaces?|enforces?|prevents?|detects?|quarantines?|decays?|audits?|redacts?)\b";

const QUANT_PATTERN: &str =
    r"(?i)\b\d+(\.\d+)?\s*(ms|millisecond|seconds?|minutes?|hours?|kB|MB|GB|TB|tokens?)\b";

#[test]
fn readme_invariant_harness_covers_all_candidates() {
    let trigger = Regex::new(TRIGGER_PATTERN).expect("trigger regex compiles");
    let quant = Regex::new(QUANT_PATTERN).expect("quantitative regex compiles");

    let document = MANIFEST
        .parse::<DocumentMut>()
        .expect("manifest TOML parses");

    let scrubber: Vec<Regex> = document["scrubber"]["denylist_regexes"]
        .as_array()
        .expect("scrubber denylist must be an array")
        .iter()
        .map(|value| {
            let pattern = value
                .as_str()
                .expect("scrubber entry must be a TOML string");
            Regex::new(pattern)
                .unwrap_or_else(|err| panic!("scrubber pattern {pattern:?} did not compile: {err}"))
        })
        .collect();

    let manifest_hashes: HashSet<String> = document["invariant"]
        .as_array_of_tables()
        .expect("manifest has [[invariant]] entries")
        .iter()
        .filter_map(|table| table.get("sentence_hash").and_then(|item| item.as_str()))
        .map(str::to_owned)
        .collect();

    let mut missing: Vec<String> = Vec::new();
    let mut in_code_fence = false;
    for (idx0, raw) in README.lines().enumerate() {
        let stripped = raw.trim();
        if stripped.starts_with("```") || stripped.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }
        if stripped.is_empty()
            || stripped.starts_with('#')
            || stripped == "---"
            || stripped.starts_with("<!--")
            || stripped.ends_with("-->")
            || stripped.starts_with('<')
            || stripped.starts_with("</")
            || stripped.starts_with('|')
        {
            continue;
        }

        let canonical = canonical_anchor_text(raw);
        if scrubber.iter().any(|rx| rx.is_match(&canonical)) {
            continue;
        }
        if !trigger.is_match(&canonical) && !quant.is_match(&canonical) {
            continue;
        }

        let expected_hash = format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex());
        if !manifest_hashes.contains(&expected_hash) {
            let id_hint = suggest_id_slug(idx0 + 1);
            missing.push(format!(
                "L{line} unaccounted README invariant candidate.\n  text: {canonical:?}\n\n  Add an entry to tests/readme_invariants/manifest.toml:\n\n  [[invariant]]\n  id = \"{id_hint}\"\n  readme_section = \"<owning README heading>\"\n  readme_line_anchor = {line}\n  sentence_hash = \"{expected_hash}\"\n  classification = \"<quantitative|invariant|promise|constraint>\"\n  verify = {{ type = \"test\", path = \"tests/<existing-test>.rs\" }}\n\n  (or scrub the line in [scrubber].denylist_regexes if it is not load-bearing).",
                line = idx0 + 1,
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "{} README invariant(s) without manifest coverage:\n\n{}",
        missing.len(),
        missing.join("\n\n")
    );
}

fn canonical_anchor_text(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn suggest_id_slug(line: usize) -> String {
    format!("rfm-new-line-{line:04}")
}
