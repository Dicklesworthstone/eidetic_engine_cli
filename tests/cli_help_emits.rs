use std::process::Command;

const SENTINEL_KINDS: &[&str] = &[
    "path_exists",
    "file_hash_or_marker",
    "json_schema_contains_field",
    "config_key_exists",
    "env_var_registered",
    "degraded_code_fixture_exists",
    "dependency_capability_present",
    "command_help_contains_flag",
];

const TYPED_FIELD_FILTER_TOKENS: &[&str] = &["NAME=VALUE", "NAME~VALUE", "NAME^VALUE"];

fn command_help(args: &[&str]) -> Result<String, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("run ee {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "ee {} failed with status {:?}; stderr: {}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("ee {} stderr is utf-8: {error}", args.join(" ")))?;
    if !stderr.trim().is_empty() {
        return Err(format!("ee {} wrote stderr: {stderr}", args.join(" ")));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("ee {} stdout is utf-8: {error}", args.join(" ")))
}

fn assert_contains_tokens(surface: &str, rendered: &str, tokens: &[&str]) -> Result<(), String> {
    let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
    let missing = tokens
        .iter()
        .copied()
        .filter(|token| !normalized.contains(token))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{surface} missing documentation tokens {missing:?}:\n{rendered}"
        ))
    }
}

fn agent_docs_command_arg<'a>(
    docs: &'a serde_json::Value,
    command_name: &str,
    arg_name: &str,
) -> Result<&'a serde_json::Value, String> {
    let commands = docs["data"]["commands"]
        .as_array()
        .ok_or_else(|| "agent-docs data.commands must be an array".to_string())?;
    let command = commands
        .iter()
        .find(|command| command["name"].as_str() == Some(command_name))
        .ok_or_else(|| format!("agent-docs command {command_name:?} is missing"))?;
    command["args"]
        .as_array()
        .ok_or_else(|| format!("agent-docs command {command_name:?} args must be an array"))?
        .iter()
        .find(|arg| arg["name"].as_str() == Some(arg_name))
        .ok_or_else(|| {
            format!("agent-docs command {command_name:?} argument {arg_name:?} is missing")
        })
}

fn assert_optional_arg_without_default(
    command_name: &str,
    arg: &serde_json::Value,
) -> Result<(), String> {
    if arg["required"].as_bool() != Some(false) {
        return Err(format!(
            "agent-docs {command_name} {} must be optional: {arg}",
            arg["name"]
        ));
    }
    if arg.get("default").is_some() {
        return Err(format!(
            "agent-docs {command_name} {} must not advertise a default: {arg}",
            arg["name"]
        ));
    }
    Ok(())
}

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

#[test]
fn remember_and_search_help_document_sentinel_and_typed_field_grammars() -> Result<(), String> {
    let remember_help = command_help(&["remember", "--help"])?;
    assert_contains_tokens(
        "ee remember --help sentinel grammar",
        &remember_help,
        &["--sentinel", "KIND:TARGET", "ee sentinel explain"],
    )?;
    assert_contains_tokens(
        "ee remember --help sentinel kinds",
        &remember_help,
        SENTINEL_KINDS,
    )?;
    let normalized_remember_help = remember_help.to_ascii_lowercase();
    assert_contains_tokens(
        "ee remember --help sentinel rejection",
        &normalized_remember_help,
        &["unknown", "rejected"],
    )?;
    assert_contains_tokens(
        "ee remember --help typed-field producer",
        &remember_help,
        &["--field", "NAME=VALUE", "ee.memory.typed_fields.v2"],
    )?;

    let search_help = command_help(&["search", "--help"])?;
    assert_contains_tokens(
        "ee search --help typed-field grammar",
        &search_help.to_ascii_uppercase(),
        TYPED_FIELD_FILTER_TOKENS,
    )?;
    assert_contains_tokens(
        "ee search --help typed-field producer",
        &search_help,
        &["--field", "ee remember"],
    )?;
    assert_contains_tokens(
        "ee search --help typed-field operator meanings",
        &search_help.to_ascii_lowercase(),
        &["exact", "contains", "prefix", "repeat"],
    )
}

#[test]
fn agent_docs_commands_json_matches_help_grammars() -> Result<(), String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["agent-docs", "commands", "--json"])
        .output()
        .map_err(|error| format!("run ee agent-docs commands --json: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ee agent-docs commands --json failed with status {:?}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if !output.stderr.is_empty() {
        return Err(format!(
            "ee agent-docs commands --json wrote stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let docs: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("agent-docs commands stdout is JSON: {error}"))?;

    let remember_sentinel = agent_docs_command_arg(&docs, "remember", "--sentinel")?;
    assert_optional_arg_without_default("remember", remember_sentinel)?;
    let sentinel_description = remember_sentinel["description"]
        .as_str()
        .ok_or_else(|| "agent-docs remember --sentinel description must be a string".to_string())?;
    assert_contains_tokens(
        "agent-docs remember --sentinel kinds",
        sentinel_description,
        SENTINEL_KINDS,
    )?;
    assert_contains_tokens(
        "agent-docs remember --sentinel grammar",
        sentinel_description,
        &[
            "KIND:TARGET",
            "Unknown kinds are rejected",
            "ee sentinel explain",
        ],
    )?;

    let remember_field = agent_docs_command_arg(&docs, "remember", "--field")?;
    assert_optional_arg_without_default("remember", remember_field)?;
    let remember_field_description = remember_field["description"]
        .as_str()
        .ok_or_else(|| "agent-docs remember --field description must be a string".to_string())?;
    assert_contains_tokens(
        "agent-docs remember --field producer",
        remember_field_description,
        &["NAME=VALUE", "Repeat", "ee.memory.typed_fields.v2"],
    )?;

    let search_field = agent_docs_command_arg(&docs, "search", "--field")?;
    assert_optional_arg_without_default("search", search_field)?;
    let search_field_description = search_field["description"]
        .as_str()
        .ok_or_else(|| "agent-docs search --field description must be a string".to_string())?;
    assert_contains_tokens(
        "agent-docs search --field grammar",
        search_field_description,
        TYPED_FIELD_FILTER_TOKENS,
    )?;
    assert_contains_tokens(
        "agent-docs search --field producer",
        search_field_description,
        &[
            "exact",
            "contains",
            "prefix",
            "repeat",
            "ee remember --field NAME=VALUE",
            "ee.memory.typed_fields.v2",
        ],
    )
}
