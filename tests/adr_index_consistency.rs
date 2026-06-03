//! Conformance checks for the ADR index.
//!
//! The index is the discoverability contract for project decisions. These
//! checks intentionally cover only numbered ADR documents; `0000-template.md`
//! remains a template and is not required to appear in the public index.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

type TestResult = Result<(), String>;

const ADR_INDEX: &str = include_str!("../docs/adr/README.md");

#[derive(Debug)]
struct IndexEntry {
    line: usize,
    number: String,
    target: String,
}

fn adr_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/adr")
}

fn is_numbered_adr_filename(name: &str) -> bool {
    let bytes = name.as_bytes();
    name != "0000-template.md"
        && name.ends_with(".md")
        && bytes.len() > 5
        && bytes[4] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
}

fn adr_number_from_filename(name: &str) -> String {
    name[..4].to_owned()
}

fn numbered_adr_files() -> Result<BTreeSet<String>, String> {
    let mut files = BTreeSet::new();
    for entry in fs::read_dir(adr_dir()).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| format!("non-UTF-8 ADR filename: {name:?}"))?;
        if is_numbered_adr_filename(&name) {
            files.insert(name);
        }
    }
    Ok(files)
}

fn parse_index_entries() -> Result<Vec<IndexEntry>, String> {
    let mut entries = Vec::new();
    for (zero_based_line, line) in ADR_INDEX.lines().enumerate() {
        if !line.starts_with("- [ADR ") {
            continue;
        }
        let line_number = zero_based_line + 1;
        let number = line
            .strip_prefix("- [ADR ")
            .and_then(|rest| rest.get(..4))
            .ok_or_else(|| format!("line {line_number}: missing ADR number"))?;
        if !number.as_bytes().iter().all(u8::is_ascii_digit) {
            return Err(format!(
                "line {line_number}: ADR number `{number}` is not four digits"
            ));
        }
        let link_start = line
            .find("](")
            .ok_or_else(|| format!("line {line_number}: missing markdown link target"))?
            + 2;
        let link_end = line[link_start..]
            .find(')')
            .ok_or_else(|| format!("line {line_number}: unterminated markdown link target"))?
            + link_start;
        let target = &line[link_start..link_end];
        if !is_numbered_adr_filename(target) {
            return Err(format!(
                "line {line_number}: indexed ADR target `{target}` is not a numbered ADR filename"
            ));
        }
        entries.push(IndexEntry {
            line: line_number,
            number: number.to_owned(),
            target: target.to_owned(),
        });
    }
    Ok(entries)
}

fn grouped_values<'a, I>(values: I) -> BTreeMap<String, Vec<String>>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for value in values {
        grouped
            .entry(value.to_owned())
            .or_default()
            .push(value.to_owned());
    }
    grouped
}

#[test]
fn adr_index_has_one_entry_for_each_numbered_adr_file() -> TestResult {
    let files = numbered_adr_files()?;
    let entries = parse_index_entries()?;
    let indexed_targets = entries
        .iter()
        .map(|entry| entry.target.as_str())
        .collect::<BTreeSet<_>>();

    let missing_from_index = files
        .iter()
        .filter(|file| !indexed_targets.contains(file.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !missing_from_index.is_empty() {
        return Err(format!(
            "ADR files missing from docs/adr/README.md: {missing_from_index:?}"
        ));
    }

    let stale_index_targets = indexed_targets
        .iter()
        .filter(|target| !files.contains(**target))
        .copied()
        .collect::<Vec<_>>();
    if !stale_index_targets.is_empty() {
        return Err(format!(
            "ADR index targets without matching docs/adr file: {stale_index_targets:?}"
        ));
    }

    let duplicate_targets = grouped_values(entries.iter().map(|entry| entry.target.as_str()))
        .into_iter()
        .filter(|(_, values)| values.len() > 1)
        .map(|(target, values)| format!("{target} appears {} times", values.len()))
        .collect::<Vec<_>>();
    if !duplicate_targets.is_empty() {
        return Err(format!(
            "ADR index contains duplicate targets: {duplicate_targets:?}"
        ));
    }

    Ok(())
}

#[test]
fn adr_numbers_are_unique_and_match_index_targets() -> TestResult {
    let files = numbered_adr_files()?;
    let entries = parse_index_entries()?;

    let duplicate_file_numbers = grouped_values(files.iter().map(|file| &file[..4]))
        .into_iter()
        .filter(|(_, values)| values.len() > 1)
        .map(|(number, values)| format!("ADR {number} has {} files", values.len()))
        .collect::<Vec<_>>();
    if !duplicate_file_numbers.is_empty() {
        return Err(format!(
            "ADR directory contains duplicate numbers: {duplicate_file_numbers:?}"
        ));
    }

    let duplicate_index_numbers = grouped_values(entries.iter().map(|entry| entry.number.as_str()))
        .into_iter()
        .filter(|(_, values)| values.len() > 1)
        .map(|(number, values)| format!("ADR {number} appears {} times", values.len()))
        .collect::<Vec<_>>();
    if !duplicate_index_numbers.is_empty() {
        return Err(format!(
            "ADR index contains duplicate numbers: {duplicate_index_numbers:?}"
        ));
    }

    for entry in entries {
        let target_number = adr_number_from_filename(&entry.target);
        if entry.number != target_number {
            return Err(format!(
                "line {}: displayed ADR {} links to target {}",
                entry.line, entry.number, entry.target
            ));
        }
    }

    Ok(())
}

#[test]
fn adr_index_entries_are_sorted_by_number() -> TestResult {
    let entries = parse_index_entries()?;
    let numbers = entries
        .iter()
        .map(|entry| entry.number.as_str())
        .collect::<Vec<_>>();
    let mut sorted_numbers = numbers.clone();
    sorted_numbers.sort_unstable();

    if numbers == sorted_numbers {
        Ok(())
    } else {
        Err(format!(
            "ADR index entries must stay sorted by ADR number: got {numbers:?}"
        ))
    }
}
