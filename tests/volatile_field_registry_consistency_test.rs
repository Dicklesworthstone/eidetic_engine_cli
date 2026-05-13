#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

use ee::obs::VOLATILE_FIELD_NAMES;

type TestResult<T = ()> = Result<T, String>;

fn repo_file(path: impl AsRef<Path>) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn parse_bash_volatile_list(script: &str) -> TestResult<Vec<String>> {
    let marker = "VOLATILE_FIELD_NAMES=(";
    let start = script
        .find(marker)
        .ok_or_else(|| "determinism.sh missing VOLATILE_FIELD_NAMES list".to_owned())?
        + marker.len();
    let rest = &script[start..];
    let end = rest
        .find("\n)")
        .ok_or_else(|| "determinism.sh VOLATILE_FIELD_NAMES list is unterminated".to_owned())?;
    let mut fields = Vec::new();
    for line in rest[..end].lines() {
        let value = line.trim().trim_matches('"').trim_matches('\'');
        if value.is_empty() || value.starts_with('#') {
            continue;
        }
        fields.push(value.to_owned());
    }
    Ok(fields)
}

#[test]
fn rust_registry_matches_bash_determinism_list() -> TestResult {
    let script = fs::read_to_string(repo_file("scripts/e2e_overhaul/determinism.sh"))
        .map_err(|error| format!("read determinism.sh: {error}"))?;
    let bash_fields = parse_bash_volatile_list(&script)?;
    let rust_fields = VOLATILE_FIELD_NAMES
        .iter()
        .map(|field| (*field).to_owned())
        .collect::<Vec<_>>();
    if bash_fields != rust_fields {
        return Err(format!(
            "volatile field registry drifted\nrust: {rust_fields:?}\nbash: {bash_fields:?}"
        ));
    }
    if !script.contains("jq \"$(volatile_field_delete_filter)\"") {
        return Err("determinism.sh strip function must use the shared bash list".to_owned());
    }
    if !script.contains(r#"if [ "${BASH_SOURCE[0]}" != "$0" ]; then"#) {
        return Err("determinism.sh must be safely sourceable for registry readers".to_owned());
    }
    Ok(())
}

#[test]
fn docs_mention_every_registered_volatile_field() -> TestResult {
    let docs = fs::read_to_string(repo_file("docs/volatile_field_registry.md"))
        .map_err(|error| format!("read volatile field registry docs: {error}"))?;
    for field in VOLATILE_FIELD_NAMES {
        let needle = format!("`{field}`");
        if !docs.contains(&needle) {
            return Err(format!("docs/volatile_field_registry.md missing {needle}"));
        }
    }
    Ok(())
}

#[test]
fn source_registry_mentions_are_registered() -> TestResult {
    let docs = fs::read_to_string(repo_file("docs/volatile_field_registry.md"))
        .map_err(|error| format!("read volatile field registry docs: {error}"))?;
    let script = fs::read_to_string(repo_file("scripts/e2e_overhaul/determinism.sh"))
        .map_err(|error| format!("read determinism.sh: {error}"))?;
    let registered = VOLATILE_FIELD_NAMES
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();

    for candidate in [
        "generatedAt",
        "generated_at",
        "last_accessed",
        "last_accessed_at",
        "last_seen_at",
        "last_used_at",
        "audit_ts",
        "elapsedMs",
        "elapsed_ms",
        "startedAt",
        "started_at",
        "endedAt",
        "ended_at",
        "ts",
        "timestamp",
        "ee_binary_hash",
        "databasePath",
        "workspacePath",
        "indexDir",
    ] {
        let mentioned = docs.contains(candidate) || script.contains(candidate);
        if mentioned && !registered.contains(candidate) {
            return Err(format!(
                "volatile field {candidate} is mentioned but not registered"
            ));
        }
    }
    Ok(())
}
