#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Spec fields, in the order every `## <fm-id>` section must list them
/// (docs/doctor/README.md, "Spec fields").
const SPEC_FIELDS: [&str; 9] = [
    "Label",
    "Severity",
    "Detector",
    "Real trigger",
    "Repair",
    "Undo",
    "Oracle",
    "Negative control",
    "Pinned sha",
];

const LABELS: [&str; 7] = [
    "REPAIR",
    "GUIDANCE-ONLY",
    "NOT-DETECTED",
    "PINNED-DEFECT",
    "UNRESOLVED",
    "UNCLASSIFIED",
    "OUT-OF-SCOPE",
];

/// Labels whose fixture proves doctor handles the failure.
const COVERAGE_LABELS: [&str; 2] = ["REPAIR", "GUIDANCE-ONLY"];

/// Labels whose fixture pins a gap: passing means the gap is still there.
const GAP_LABELS: [&str; 2] = ["NOT-DETECTED", "PINNED-DEFECT"];

/// Labels allowed only on marker-only fixtures (no real trigger, either not
/// built yet or, for OUT-OF-SCOPE, not a doctor failure mode at all).
const MARKER_ONLY_LABELS: [&str; 3] = ["UNRESOLVED", "UNCLASSIFIED", "OUT-OF-SCOPE"];

const FAILURE_CLASSES: [&str; 10] = [
    "missing",
    "empty/truncated",
    "corrupt-bytes",
    "stale/generation-drift",
    "malformed-text",
    "permission",
    "locked/contended",
    "resource-pressure",
    "external-tool-absent",
    "external-tool-contract-mismatch",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_root() -> PathBuf {
    repo_root().join("tests/doctor_fixtures")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", path.display());
    })
}

fn manifest() -> Value {
    let path = fixture_root().join("manifest.json");
    serde_json::from_str(&read(&path)).unwrap_or_else(|error| {
        panic!("failed to parse {}: {error}", path.display());
    })
}

fn manifest_fixtures() -> Vec<Value> {
    manifest()["fixtures"]
        .as_array()
        .expect("fixtures array")
        .clone()
}

fn manifest_ids() -> BTreeSet<String> {
    manifest_fixtures()
        .iter()
        .map(|entry| entry["id"].as_str().expect("fixture id").to_owned())
        .collect()
}

fn str_field<'a>(value: &'a Value, key: &str, context: &str) -> &'a str {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("{context}: `{key}` must be a string"))
}

fn scored_rows() -> Vec<Value> {
    let path = repo_root().join("docs/doctor/failure_mode_scores.jsonl");
    read(&path)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("failure mode score json"))
        .collect()
}

/// Maps each fixture id to the scored row that names it.
fn scored_row_by_fixture() -> BTreeMap<String, Value> {
    let mut by_fixture = BTreeMap::new();
    for row in scored_rows() {
        for fixture in row["fixtures"].as_array().expect("fixtures array") {
            let id = fixture.as_str().expect("fixture id").to_owned();
            let previous = by_fixture.insert(id.clone(), row.clone());
            assert!(
                previous.is_none(),
                "{id} is named by more than one scored row"
            );
        }
    }
    by_fixture
}

/// Returns the lines of the `## <id>` section of a spec file.
fn spec_section(spec: &str, id: &str) -> Vec<String> {
    let heading = format!("## {id}");
    let mut lines = spec.lines().skip_while(|line| *line != heading);
    assert_eq!(lines.next(), Some(heading.as_str()), "spec lacks {heading}");
    lines
        .take_while(|line| !line.starts_with("## "))
        .map(str::to_owned)
        .collect()
}

/// Returns each spec field's value, asserting all nine are present, in order
/// and non-empty.
fn spec_fields(spec: &str, id: &str) -> BTreeMap<&'static str, String> {
    let section = spec_section(spec, id);
    let mut fields = BTreeMap::new();
    let mut last_index = None;
    for field in SPEC_FIELDS {
        let prefix = format!("- **{field}:** ");
        let (index, line) = section
            .iter()
            .enumerate()
            .find(|(_, line)| line.starts_with(&prefix))
            .unwrap_or_else(|| panic!("{id}: spec field `{field}` missing"));
        assert!(
            last_index.is_none_or(|last| index > last),
            "{id}: spec field `{field}` is out of order"
        );
        last_index = Some(index);
        let value = line[prefix.len()..].trim().to_owned();
        assert!(!value.is_empty(), "{id}: spec field `{field}` is empty");
        fields.insert(field, value);
    }
    fields
}

fn backticked(text: &str) -> Vec<&str> {
    text.split('`').skip(1).step_by(2).collect()
}

fn error_codes(text: &str) -> BTreeSet<String> {
    text.match_indices("EE-E")
        .filter_map(|(start, _)| {
            let code = text.get(start..start + 7)?;
            code[4..]
                .chars()
                .all(|c| c.is_ascii_digit())
                .then(|| code.to_owned())
        })
        .collect()
}

fn source_text() -> String {
    let mut text = String::new();
    let mut stack = vec![repo_root().join("src")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read src dir") {
            let path = entry.expect("src entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                text.push_str(&read(&path));
            }
        }
    }
    text
}

/// A marker-only corrupt.sh does nothing but source lib.sh and write the marker.
fn corrupt_is_marker_only(corrupt: &str) -> bool {
    corrupt.lines().map(str::trim).all(|line| {
        line.is_empty()
            || line.starts_with('#')
            || line.starts_with("set -")
            || line.starts_with("SCRIPT_DIR=")
            || line == ". \"$SCRIPT_DIR/../lib.sh\""
            || line.starts_with("doctor_fixture_corrupt ")
    })
}

#[test]
fn doctor_fixtures_scored_population_is_well_formed_and_maps_to_manifest() {
    let ids = manifest_ids();
    let fixtures: BTreeMap<String, Value> = manifest_fixtures()
        .into_iter()
        .map(|fixture| (str_field(&fixture, "id", "manifest").to_owned(), fixture))
        .collect();
    let rows = scored_rows();
    assert!(!rows.is_empty(), "scored population is empty");

    let mut row_ids = BTreeSet::new();
    for row in &rows {
        let fm_id = str_field(row, "fm_id", "scored row");
        assert!(row_ids.insert(fm_id.to_owned()), "duplicate fm_id {fm_id}");
        for key in ["state_object", "rubric_clause"] {
            assert!(
                !str_field(row, key, fm_id).is_empty(),
                "{fm_id}: empty {key}"
            );
        }
        let class = str_field(row, "failure_class", fm_id);
        assert!(
            FAILURE_CLASSES.contains(&class),
            "{fm_id}: unknown class {class}"
        );
        let severity = str_field(row, "severity", fm_id);
        assert!(
            matches!(severity, "P0" | "P1" | "P2"),
            "{fm_id}: bad severity {severity}"
        );
        assert!(
            row["detected_by"].is_null() || row["detected_by"]["check"].is_string(),
            "{fm_id}: detected_by must be null or name a check"
        );
        let mapped = row["fixtures"].as_array().expect("fixtures array");
        let gap_reason = row["gap_reason"].as_str().unwrap_or("");
        if mapped.is_empty() && severity != "P2" {
            assert!(
                !gap_reason.is_empty(),
                "{fm_id}: a {severity} row without fixtures needs a gap_reason"
            );
        }
        for fixture_id in mapped {
            let fixture_id = fixture_id.as_str().expect("fixture id");
            let fixture = fixtures
                .get(fixture_id)
                .unwrap_or_else(|| panic!("{fm_id} names unknown fixture {fixture_id}"));
            let fixture_severity = str_field(fixture, "severity", fixture_id);
            if fixture_severity != severity {
                assert!(
                    row["severity_note"]
                        .as_str()
                        .is_some_and(|note| !note.is_empty()),
                    "{fm_id}: scored {severity} but fixture {fixture_id} is \
                     {fixture_severity}; a severity_note must explain it"
                );
            }
        }
    }

    // Only unbuilt or out-of-scope fixtures may sit outside the scored doctor
    // surface.
    let mapped = scored_row_by_fixture();
    for id in &ids {
        let label = str_field(&fixtures[id], "label", id);
        if !mapped.contains_key(id) {
            assert!(
                matches!(label, "UNCLASSIFIED" | "OUT-OF-SCOPE"),
                "{id} is labelled {label} but no scored row names it"
            );
        }
    }
}

/// An OUT-OF-SCOPE fixture is an absence or category claim from a code
/// survey, so it must carry its evidence: a category, the enumerated search,
/// and citations that resolve to a real line of a real file (bd-2oh15 ruling
/// on c9985).
#[test]
fn doctor_fixtures_out_of_scope_entries_carry_their_evidence() {
    let mut out_of_scope = 0;
    for fixture in manifest_fixtures() {
        let id = str_field(&fixture, "id", "manifest");
        let reason = &fixture["scopeReason"];
        if str_field(&fixture, "label", id) != "OUT-OF-SCOPE" {
            assert!(
                reason.is_null(),
                "{id}: only OUT-OF-SCOPE fixtures carry a scopeReason"
            );
            continue;
        }
        out_of_scope += 1;
        for key in ["category", "search"] {
            assert!(
                reason[key]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty()),
                "{id}: scopeReason.{key} must be a non-empty string"
            );
        }
        let citations = reason["citations"]
            .as_array()
            .filter(|list| !list.is_empty())
            .unwrap_or_else(|| panic!("{id}: scopeReason.citations must be a non-empty array"));
        for citation in citations {
            let citation = citation.as_str().expect("citation string");
            let (path, line) = citation
                .rsplit_once(':')
                .unwrap_or_else(|| panic!("{id}: citation {citation} is not path:line"));
            let line: usize = line
                .parse()
                .unwrap_or_else(|_| panic!("{id}: citation {citation} has no line number"));
            let lines = read(&repo_root().join(path)).lines().count();
            assert!(
                line >= 1 && line <= lines,
                "{id}: citation {citation} points past the end of {path} ({lines} lines)"
            );
        }
    }
    assert!(
        out_of_scope > 0,
        "no OUT-OF-SCOPE fixture found: the scan read nothing"
    );
}

#[test]
fn doctor_fixtures_have_triplet_files_and_metadata() {
    for fixture in manifest_fixtures() {
        let id = str_field(&fixture, "id", "manifest");
        let severity = str_field(&fixture, "severity", id);
        let subsystem = str_field(&fixture, "subsystem", id);
        let spec_rel = format!("docs/doctor/repair-specs/{subsystem}.md");
        assert_eq!(str_field(&fixture, "spec", id), spec_rel, "{id}: spec path");
        let spec = repo_root().join(&spec_rel);
        assert!(spec.is_file(), "missing spec for {id}: {}", spec.display());

        let dir = fixture_root().join(id);
        assert!(dir.is_dir(), "missing fixture dir {}", dir.display());
        for name in ["README.md", "corrupt.sh", "assert.sh"] {
            assert!(dir.join(name).is_file(), "missing {id}/{name}");
        }

        let readme = read(&dir.join("README.md"));
        assert!(readme.contains(id), "README must name {id}");
        assert!(
            readme.contains(severity),
            "README must name severity for {id}"
        );
        assert!(
            readme.contains(subsystem),
            "README must name subsystem for {id}"
        );
        // V1: the README links to this fixture's own spec section.
        let link = format!("(../../../{spec_rel}#{id})");
        assert!(readme.contains(&link), "README for {id} must link {link}");
        // The README states exactly one label, and it is the manifest's: a
        // relabelled fixture must not keep advertising its old bucket.
        let label = str_field(&fixture, "label", id);
        let stated: Vec<&str> = readme
            .match_indices("Label: **")
            .map(|(at, prefix)| {
                let rest = &readme[at + prefix.len()..];
                let end = rest.find("**").unwrap_or(rest.len());
                rest[..end].split_whitespace().next().unwrap_or("")
            })
            .collect();
        assert_eq!(
            stated,
            vec![label],
            "README for {id} must state its manifest label exactly once"
        );
    }
}

#[test]
fn doctor_fixtures_specs_have_one_complete_section_per_fixture() {
    let spec_dir = repo_root().join("docs/doctor/repair-specs");
    let mut headings = Vec::new();
    for entry in fs::read_dir(&spec_dir).expect("read spec dir") {
        let path = entry.expect("spec entry").path();
        for line in read(&path).lines() {
            if let Some(id) = line.strip_prefix("## ") {
                headings.push(id.to_owned());
            }
        }
    }
    let unique: BTreeSet<String> = headings.iter().cloned().collect();
    assert_eq!(unique.len(), headings.len(), "duplicate spec sections");
    assert_eq!(
        unique,
        manifest_ids(),
        "spec sections must match the manifest"
    );

    let mapped = scored_row_by_fixture();
    let source = source_text();
    for fixture in manifest_fixtures() {
        let id = str_field(&fixture, "id", "manifest");
        let label = str_field(&fixture, "label", id);
        let severity = str_field(&fixture, "severity", id);
        assert!(LABELS.contains(&label), "{id}: unknown label {label}");
        let spec = read(&repo_root().join(str_field(&fixture, "spec", id)));
        // V2: nine non-empty fields in order.
        let fields = spec_fields(&spec, id);
        assert!(
            fields["Label"].starts_with(label),
            "{id}: spec Label must start with manifest label {label}"
        );

        // V3: severity matches the manifest and names the scored row.
        let line = &fields["Severity"];
        assert!(
            line.starts_with(&format!("{severity}. ")),
            "{id}: spec Severity must start with manifest severity {severity}"
        );
        match mapped.get(id) {
            Some(row) => {
                let scored = format!(
                    "Scored {} as `{}`",
                    str_field(row, "severity", id),
                    str_field(row, "fm_id", id)
                );
                assert!(line.contains(&scored), "{id}: Severity must say {scored}");
            }
            None => assert!(
                line.contains("Not in the scored population"),
                "{id}: Severity must say it is not in the scored population"
            ),
        }

        // V4: every error code a spec's Detector cites exists in the source.
        for code in error_codes(&fields["Detector"]) {
            assert!(
                source.contains(&format!("\"{code}\"")),
                "{id}: Detector cites {code}, which no source file defines"
            );
        }
    }
}

#[test]
fn doctor_fixtures_labels_match_what_the_scripts_do() {
    let fixers = read(&repo_root().join("src/core/doctor_fixers.rs"));
    let runtime = read(&repo_root().join("src/core/doctor_runtime.rs"));
    for fixture in manifest_fixtures() {
        let id = str_field(&fixture, "id", "manifest");
        let label = str_field(&fixture, "label", id);
        let dir = fixture_root().join(id);
        let corrupt = read(&dir.join("corrupt.sh"));
        let assert = read(&dir.join("assert.sh"));
        let spec = read(&repo_root().join(str_field(&fixture, "spec", id)));
        let fields = spec_fields(&spec, id);

        // V6: marker-only fixtures, and only they, carry a marker-only label.
        let marker_only = corrupt_is_marker_only(&corrupt);
        let runs_ee = assert.contains("EE_DOCTOR_FIXTURE_RUN_EE");
        if MARKER_ONLY_LABELS.contains(&label) {
            assert!(
                marker_only && !runs_ee,
                "{id} is labelled {label} but its scripts build a real trigger"
            );
            continue;
        }
        assert!(
            !marker_only && runs_ee,
            "{id} is labelled {label} but is marker-only or never runs ee"
        );

        match label {
            "REPAIR" => {
                // V5: the named fixer and operation exist, and the fixture
                // asserts the operation was applied.
                let names = backticked(&fields["Repair"]);
                let fixer = names
                    .iter()
                    .find(|name| name.starts_with("fix_"))
                    .unwrap_or_else(|| panic!("{id}: Repair must name a fixer"));
                assert!(
                    fixers.contains(&format!("pub fn {fixer}(")),
                    "{id}: fixer {fixer} is not defined in doctor_fixers.rs"
                );
                assert!(
                    names.iter().any(|op| {
                        let quoted = format!("\"{op}\"");
                        runtime.contains(&quoted) && assert.contains(&quoted)
                    }),
                    "{id}: Repair must name an Op kind that doctor_runtime.rs \
                     defines and assert.sh checks"
                );
                assert!(
                    assert.contains(".outcome == \"applied\""),
                    "{id}: REPAIR must assert an applied outcome"
                );
            }
            // Report-only fixtures record no fixer at all; guidance-only ones
            // record manual guidance and must still leave the damage reported.
            "GUIDANCE-ONLY" => assert!(
                assert.contains("doctor_fixture_assert_report_only ")
                    || assert.contains("doctor_fixture_assert_guidance_only "),
                "{id}: GUIDANCE-ONLY must use doctor_fixture_assert_report_only or \
                 doctor_fixture_assert_guidance_only"
            ),
            "NOT-DETECTED" => assert!(
                assert.contains("doctor_fixture_assert_pinned_gap "),
                "{id}: NOT-DETECTED must use doctor_fixture_assert_pinned_gap"
            ),
            "PINNED-DEFECT" => {
                let bead = fields["Label"]
                    .split_whitespace()
                    .find(|word| word.starts_with("bd-"))
                    .unwrap_or_else(|| panic!("{id}: PINNED-DEFECT must name its bead"));
                assert!(
                    assert.contains(bead),
                    "{id}: assert.sh must pin defect {bead}"
                );
            }
            other => panic!("{id}: unhandled label {other}"),
        }
    }
}

#[test]
fn doctor_fixtures_pinned_gaps_are_never_coverage() {
    let mut coverage = 0;
    let mut gaps = 0;
    for fixture in manifest_fixtures() {
        let id = str_field(&fixture, "id", "manifest");
        let label = str_field(&fixture, "label", id);
        let spec = read(&repo_root().join(str_field(&fixture, "spec", id)));
        let spec_label = &spec_fields(&spec, id)["Label"];
        if GAP_LABELS.contains(&label) {
            gaps += 1;
            assert!(
                spec_label.contains("NOT coverage"),
                "{id}: a {label} spec must say NOT coverage"
            );
        } else {
            assert!(
                !spec_label.contains("coverage"),
                "{id}: only gap labels may mention coverage"
            );
        }
        if COVERAGE_LABELS.contains(&label) {
            coverage += 1;
        }
    }
    assert!(
        gaps > 0,
        "no gap fixtures found: the label scan read nothing"
    );
    assert!(coverage > 0, "no coverage fixtures found");
    assert!(
        read(&fixture_root().join("lib.sh")).contains("-- NOT coverage"),
        "the pinned-gap helper must print that it is not coverage"
    );
}

#[test]
fn doctor_fixtures_scripts_are_non_destructive_and_no_local_cargo() {
    let forbidden = [
        "rm ",
        "rm\t",
        "rm -",
        "git reset",
        "git checkout",
        "git stash",
        "git worktree",
        "cargo ",
        "rustc ",
        "rustdoc ",
    ];
    for entry in walk_fixture_scripts(&fixture_root()) {
        let text = read(&entry);
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "{} contains forbidden token {needle:?}",
                entry.display()
            );
        }
    }
}

#[test]
fn doctor_fixtures_manifest_has_unique_dirs_by_subsystem() {
    let mut by_subsystem: BTreeMap<String, usize> = BTreeMap::new();
    for fixture in manifest_fixtures() {
        let subsystem = fixture["subsystem"].as_str().expect("subsystem");
        *by_subsystem.entry(subsystem.to_owned()).or_default() += 1;
    }
    for subsystem in [
        "agent_coordination",
        "cass_integration",
        "graph_subsystem",
        "policy_safety",
        "schema_migrations",
        "search_indexes",
        "state_files",
        "workspace_config",
    ] {
        assert!(
            by_subsystem.contains_key(subsystem),
            "missing subsystem coverage for {subsystem}"
        );
    }
}

fn walk_fixture_scripts(root: &Path) -> Vec<PathBuf> {
    let mut scripts = Vec::new();
    scripts.push(root.join("lib.sh"));
    scripts.push(root.join("run_all.sh"));
    for id in manifest_ids() {
        scripts.push(root.join(&id).join("corrupt.sh"));
        scripts.push(root.join(&id).join("assert.sh"));
        let condition = root.join(id).join("condition.sh");
        if condition.is_file() {
            scripts.push(condition);
        }
    }
    scripts
}

/// Damage that is not in the store's bytes, an environment carried in
/// .fixture_baseline/env.sh or a process that assert.sh starts in the
/// background, is invisible to a harness that only runs doctor on the target.
/// Such a fixture must ship condition.sh so every harness can apply it, and
/// condition.sh must be able to say it was not applied (bd-2oh15 ruling t2250
/// R2). A pass that did not exercise the condition must not print as a pass.
#[test]
fn doctor_fixtures_out_of_store_conditions_ship_condition_sh() {
    let mut conditioned = 0;
    for id in manifest_ids() {
        let dir = fixture_root().join(&id);
        let corrupt = read(&dir.join("corrupt.sh"));
        let assert = read(&dir.join("assert.sh"));
        let carries_env = corrupt.contains("env.sh") || assert.contains("env.sh");
        let starts_process = assert
            .lines()
            .map(str::trim_end)
            .any(|line| !line.trim_start().starts_with('#') && line.ends_with(" &"));
        let condition = dir.join("condition.sh");
        if carries_env || starts_process {
            conditioned += 1;
            assert!(
                condition.is_file(),
                "{id} carries damage outside the store (env.sh: {carries_env}, \
                 background process: {starts_process}) but ships no condition.sh"
            );
        }
        if condition.is_file() {
            let text = read(&condition);
            assert!(
                text.contains("DOCTOR_FIXTURE_CONDITION_NOT_APPLIED") && text.contains("\"$@\""),
                "{id}/condition.sh must run its command and be able to report \
                 DOCTOR_FIXTURE_CONDITION_NOT_APPLIED"
            );
        }
    }
    assert!(
        conditioned > 0,
        "no out-of-store fixture found: the scan read nothing"
    );
}
