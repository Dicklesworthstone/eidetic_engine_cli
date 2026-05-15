use std::collections::HashSet;

use toml_edit::{DocumentMut, InlineTable, Table, Value};

const MANIFEST: &str = include_str!("readme_invariants/manifest.toml");
const README: &str = include_str!("../README.md");

#[derive(Debug)]
struct Invariant<'a> {
    id: String,
    readme_section: String,
    readme_line_anchor: usize,
    sentence_hash: String,
    classification: String,
    verify: &'a InlineTable,
}

#[test]
fn readme_invariant_manifest_schema_is_pinned() {
    let document = MANIFEST
        .parse::<DocumentMut>()
        .expect("manifest TOML parses");
    assert_eq!(document["schema"].as_str(), Some("ee.readme_invariants.v1"));
    assert!(
        document["scrubber"]["denylist_regexes"]
            .as_array()
            .is_some_and(|array| !array.is_empty()),
        "scrubber denylist must be explicit"
    );

    let invariants = document["invariant"]
        .as_array_of_tables()
        .expect("manifest has [[invariant]] entries");
    assert!(
        !invariants.is_empty(),
        "manifest must seed at least one invariant"
    );

    let readme_lines: Vec<&str> = README.lines().collect();
    let mut ids = HashSet::new();
    let mut failures = Vec::new();

    for table in invariants {
        if let Some(entry) = parse_invariant(table, &mut failures) {
            validate_id(&entry, &mut ids, &mut failures);
            validate_classification(&entry, &mut failures);
            validate_anchor_hash(&entry, &readme_lines, &mut failures);
            validate_verify(&entry, &mut failures);
        }
    }

    assert!(
        failures.is_empty(),
        "README invariant manifest failures:\n{}",
        failures.join("\n")
    );
}

fn parse_invariant<'a>(table: &'a Table, failures: &mut Vec<String>) -> Option<Invariant<'a>> {
    let id = required_str(table, "id", "invariant", failures)?;
    Some(Invariant {
        readme_section: required_str(table, "readme_section", &id, failures)?,
        readme_line_anchor: required_usize(table, "readme_line_anchor", &id, failures)?,
        sentence_hash: required_str(table, "sentence_hash", &id, failures)?,
        classification: required_str(table, "classification", &id, failures)?,
        verify: table
            .get("verify")
            .and_then(|item| item.as_inline_table())
            .or_else(|| {
                failures.push(format!("{id} must define inline verify table"));
                None
            })?,
        id,
    })
}

fn required_str(
    table: &Table,
    key: &str,
    context: &str,
    failures: &mut Vec<String>,
) -> Option<String> {
    table
        .get(key)
        .and_then(|item| item.as_str())
        .map(str::to_owned)
        .or_else(|| {
            failures.push(format!("{context} missing string field {key}"));
            None
        })
}

fn required_usize(
    table: &Table,
    key: &str,
    context: &str,
    failures: &mut Vec<String>,
) -> Option<usize> {
    let value = table.get(key).and_then(|item| item.as_integer());
    match value.and_then(|raw| usize::try_from(raw).ok()) {
        Some(value) => Some(value),
        None => {
            failures.push(format!(
                "{context} missing non-negative integer field {key}"
            ));
            None
        }
    }
}

fn validate_id(entry: &Invariant<'_>, ids: &mut HashSet<String>, failures: &mut Vec<String>) {
    if entry.id.is_empty()
        || !entry
            .id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !entry.id.as_bytes()[0].is_ascii_lowercase()
    {
        failures.push(format!("{} has an invalid stable slug", entry.id));
    }
    if !ids.insert(entry.id.clone()) {
        failures.push(format!("{} is duplicated", entry.id));
    }
}

fn validate_classification(entry: &Invariant<'_>, failures: &mut Vec<String>) {
    let allowed = ["quantitative", "invariant", "promise", "constraint"];
    if !allowed.contains(&entry.classification.as_str()) {
        failures.push(format!(
            "{} has unsupported classification {}",
            entry.id, entry.classification
        ));
    }
    if entry.readme_section.trim().is_empty() {
        failures.push(format!("{} has an empty README section", entry.id));
    }
}

fn validate_anchor_hash(entry: &Invariant<'_>, readme_lines: &[&str], failures: &mut Vec<String>) {
    let Some(line) = entry
        .readme_line_anchor
        .checked_sub(1)
        .and_then(|index| readme_lines.get(index))
    else {
        failures.push(format!(
            "{} points at missing README line {}",
            entry.id, entry.readme_line_anchor
        ));
        return;
    };

    let canonical = canonical_anchor_text(line);
    if canonical.is_empty() {
        failures.push(format!(
            "{} points at an empty README line {}",
            entry.id, entry.readme_line_anchor
        ));
    }
    let expected = format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex());
    if entry.sentence_hash != expected {
        failures.push(format!(
            "{} hash mismatch: expected {expected} for {:?}, found {}",
            entry.id, canonical, entry.sentence_hash
        ));
    }
}

fn canonical_anchor_text(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn validate_verify(entry: &Invariant<'_>, failures: &mut Vec<String>) {
    match inline_str(entry.verify, "type").as_deref() {
        Some("test") => match inline_str(entry.verify, "path") {
            Some(path) if path.starts_with("tests/") || path.starts_with("scripts/") => {
                if let Some(missing) = missing_verifier_file(&path) {
                    failures.push(format!(
                        "{} test verifier path does not exist on disk: {missing}",
                        entry.id
                    ));
                }
            }
            Some(path) => failures.push(format!(
                "{} test verifier path must live under tests/ or scripts/: {}",
                entry.id, path
            )),
            None => failures.push(format!("{} test verifier must set path", entry.id)),
        },
        Some("defer_bead") => {
            match inline_str(entry.verify, "id") {
                Some(id) if id.starts_with("bd-") => {}
                Some(id) => failures.push(format!(
                    "{} defer_bead verifier has invalid id {}",
                    entry.id, id
                )),
                None => failures.push(format!("{} defer_bead verifier must set id", entry.id)),
            }
            if inline_str(entry.verify, "defer_until").is_none() {
                failures.push(format!(
                    "{} defer_bead verifier must set defer_until",
                    entry.id
                ));
            }
        }
        Some(other) => failures.push(format!(
            "{} has unsupported verifier type {}",
            entry.id, other
        )),
        None => failures.push(format!("{} verifier must set type", entry.id)),
    }
}

fn inline_str(table: &InlineTable, key: &str) -> Option<String> {
    table.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn missing_verifier_file(path: &str) -> Option<String> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let file_only = path.split("::").next().unwrap_or(path);
    let absolute = std::path::Path::new(manifest_dir).join(file_only);
    if absolute.exists() {
        None
    } else {
        Some(absolute.display().to_string())
    }
}
