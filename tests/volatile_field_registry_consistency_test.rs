#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;

use ee::obs::VOLATILE_FIELD_NAMES;

type TestResult<T = ()> = Result<T, String>;

#[cfg(unix)]
#[test]
fn bash_pack_slo_normalization_rejects_false_green_and_preserves_other_statuses() -> TestResult {
    let base = serde_json::json!({
        "status": "outside",
        "data": {"pack": {"hash": "blake3:keep", "slo": {
            "schema": "ee.pack.slo.v1",
            "budgetClass": {"elapsedMsTarget": 200, "elapsedMsWarning": 500, "elapsedMsFailure": 2000},
            "actuals": {"elapsedMs": 24457, "scannedCount": 12},
            "resourceStatus": "within_budget", "elapsedStatus": "failure", "status": "failure",
            "degradations": []
        }}},
        "unrelated": {"elapsedStatus": "preserve", "status": "failure"}
    });
    for (status, valid) in [("failure", true), ("within_budget", false)] {
        let mut input = base.clone();
        input["data"]["pack"]["slo"]["status"] = status.into();
        input["data"]["pack"]["slo"]["elapsedStatus"] = status.into();
        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg("source \"$1\"; printf '%s' \"$2\" | strip_variable_fields")
            .arg("ee-slo-normalization-test")
            .arg(repo_file("scripts/e2e_overhaul/determinism.sh"))
            .arg(input.to_string())
            .output()
            .map_err(|error| error.to_string())?;
        assert_eq!(
            output.status.success(),
            valid,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if valid {
            let normalized: serde_json::Value =
                serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
            let mut expected = base.clone();
            assert!(ee::obs::normalize_pack_slo_measurements(&mut expected)?);
            assert_eq!(
                normalized, expected,
                "Bash and Rust retain all semantic fields"
            );
        } else {
            assert!(
                output.stdout.is_empty(),
                "invalid SLO must not emit a comparable normalized body"
            );
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("measured classification disagrees")
            );
        }
    }
    Ok(())
}

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
        "createdAt",
        "created_at",
        "updatedAt",
        "completedAt",
        "finishedAt",
        "expiresAt",
        "capturedAt",
        "captured_at",
        "computedAt",
        "computed_at",
        "observedAt",
        "recordedAt",
        "refreshedAt",
        "selectedAt",
        "decidedAt",
        "estimatedAt",
        "exposedAt",
        "lastValidatedAt",
        "last_accessed",
        "last_accessed_at",
        "last_seen_at",
        "last_used_at",
        "audit_ts",
        "elapsedMs",
        "elapsed_ms",
        "elapsedMsBucket",
        "durationMs",
        "wallClockMs",
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
        "snapshotRefreshedAt",
        "witnessElapsedMs",
        "witnessRecordedAt",
        "algorithmStartedAt",
        "projectionMs",
        "pagerankMs",
        "betweennessMs",
        "totalMs",
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
