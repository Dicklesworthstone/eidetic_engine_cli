use std::process::Command;

#[test]
fn init_help_promises_a_ready_search_index() -> Result<(), String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["init", "--help"])
        .output()
        .map_err(|error| format!("run ee init --help: {error}"))?;

    assert!(
        output.status.success(),
        "ee init --help failed with status {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("init help stdout is utf-8: {error}"))?;
    let normalized = stdout.to_ascii_lowercase();
    assert!(
        normalized.contains("ready zero-document search index"),
        "ee init --help must explain search-index readiness:\n{stdout}"
    );
    assert!(
        normalized.contains("without a separate") && normalized.contains("index rebuild"),
        "ee init --help must say that no separate rebuild is required:\n{stdout}"
    );

    Ok(())
}

#[test]
fn root_help_emits_walking_skeleton_prelude() -> Result<(), String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--help")
        .output()
        .map_err(|error| format!("run ee --help: {error}"))?;

    assert!(
        output.status.success(),
        "ee --help failed with status {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("help stdout is utf-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("help stderr is utf-8: {error}"))?;

    assert!(!stdout.trim().is_empty(), "ee --help stdout is empty");
    assert!(
        stderr.trim().is_empty(),
        "ee --help should not write stderr: {stderr}"
    );

    for required in [
        "Most-used commands (start here)",
        "  orient ",
        "  init ",
        "  remember ",
        "  search ",
        "  ask ",
        "  pack ",
        "  lens ",
        "  why ",
        "  status ",
        "Assert:         claim, certificate, attest, demo, tripwire",
        "Coordinate:     swarm, handoff, preflight, situation, plan, workflow",
        "ee agent-docs <topic>",
        "Usage:",
    ] {
        assert!(
            stdout.contains(required),
            "ee --help stdout missing {required:?}:\n{stdout}"
        );
    }

    let most_used = stdout
        .split_once("Most-used commands (start here):\n")
        .and_then(|(_, tail)| tail.split_once("\nAgent shortcuts:"))
        .map(|(section, _)| {
            section
                .lines()
                .filter_map(|line| line.split_whitespace().next())
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| "ee --help missing a bounded Most-used section".to_string())?;

    let docs_output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["agent-docs", "--json"])
        .output()
        .map_err(|error| format!("run ee agent-docs --json: {error}"))?;
    assert!(
        docs_output.status.success(),
        "ee agent-docs --json failed with status {:?}; stderr: {}",
        docs_output.status.code(),
        String::from_utf8_lossy(&docs_output.stderr)
    );
    let docs: serde_json::Value = serde_json::from_slice(&docs_output.stdout)
        .map_err(|error| format!("agent-docs stdout is JSON: {error}"))?;
    let core_commands = docs["data"]["coreCommands"]
        .as_array()
        .ok_or_else(|| "agent-docs data.coreCommands must be an array".to_string())?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "agent-docs core command must be a string".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        most_used, core_commands,
        "human Most-used commands must equal agent-docs coreCommands"
    );
    assert_eq!(most_used.first(), Some(&"orient"), "orient must be first");

    Ok(())
}
