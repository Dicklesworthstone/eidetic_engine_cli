//! Command-level renderer capability matrix (bd-2ulvu.1).
//!
//! Field-level renderer parity already has coverage in
//! `renderer_parity_matrix.rs`. This test validates the command-level
//! inventory that future no-fallback tests use to decide whether a
//! requested renderer must produce a diagram, a machine contract, human
//! text, or an explicit unsupported response.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

const KNOWN_FORMATS: &[&str] = &[
    "human", "markdown", "mermaid", "json", "toon", "jsonl", "compact", "hook",
];

const KNOWN_CAPABILITIES: &[&str] = &[
    "canonical",
    "machine",
    "human_text",
    "diagram",
    "json_override",
    "unsupported",
];

const REQUIRED_COMMANDS: &[&str] = &[
    "context",
    "search",
    "why",
    "status",
    "doctor",
    "memory show",
    "memory history",
    "graph export",
    "graph neighborhood",
    "curate candidates",
    "curate validate",
    "curate apply",
];

#[derive(Debug)]
struct CommandCapability {
    name: String,
    owner: String,
    notes: String,
    formats: Vec<(String, String)>,
    json_override: bool,
    mermaid_markers: Vec<String>,
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn matrix_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("renderer_command_capabilities.toml")
}

fn parse_matrix() -> Result<Vec<CommandCapability>, String> {
    let path = matrix_path();
    let raw =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let document = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("parse TOML {}: {error}", path.display()))?;
    let commands = document
        .get("command")
        .and_then(|item| item.as_array_of_tables())
        .ok_or_else(|| "missing top-level [[command]] array".to_string())?;

    let mut parsed = Vec::with_capacity(commands.len());
    for (index, table) in commands.iter().enumerate() {
        let string_field = |key: &str| -> Result<String, String> {
            table
                .get(key)
                .and_then(|item| item.as_str())
                .map(str::to_owned)
                .ok_or_else(|| format!("[[command]] #{index}: missing string field `{key}`"))
        };
        let formats_table = table
            .get("formats")
            .and_then(|item| item.as_table_like())
            .ok_or_else(|| format!("[[command]] #{index}: missing `formats` table"))?;
        let mut formats = Vec::with_capacity(KNOWN_FORMATS.len());
        for (format, capability) in formats_table.iter() {
            let capability = capability
                .as_str()
                .ok_or_else(|| {
                    format!("[[command]] #{index}: formats.{format} must be a string capability")
                })?
                .to_owned();
            formats.push((format.to_owned(), capability));
        }
        let json_override = table
            .get("json_override")
            .and_then(|item| item.as_bool())
            .ok_or_else(|| format!("[[command]] #{index}: missing bool field `json_override`"))?;
        let mermaid_markers = table
            .get("mermaid_markers")
            .and_then(|item| item.as_array())
            .map(|array| {
                array
                    .iter()
                    .map(|value| {
                        value.as_str().map(str::to_owned).ok_or_else(|| {
                            format!(
                                "[[command]] #{index}: every mermaid_markers item must be a string"
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();

        parsed.push(CommandCapability {
            name: string_field("name")?,
            owner: string_field("owner")?,
            notes: string_field("notes")?,
            formats,
            json_override,
            mermaid_markers,
        });
    }
    Ok(parsed)
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("renderer-command-capabilities")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    Ok(dir)
}

fn run_ee(args: &[String]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn first_stdout_line(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(160)
        .collect()
}

fn command_format_capability<'a>(entry: &'a CommandCapability, format: &str) -> Option<&'a str> {
    entry
        .formats
        .iter()
        .find(|(candidate, _)| candidate == format)
        .map(|(_, capability)| capability.as_str())
}

fn unsupported_mermaid_args(command: &str, workspace_arg: &str) -> Option<Vec<String>> {
    let mut args = vec![
        "--workspace".to_owned(),
        workspace_arg.to_owned(),
        "--format".to_owned(),
        "mermaid".to_owned(),
    ];
    match command {
        "search" => args.extend(["search", "renderer contract"].map(str::to_owned)),
        "status" => args.push("status".to_owned()),
        "memory show" => {
            args.extend(["memory", "show", "mem_renderer_contract"].map(str::to_owned))
        }
        "curate candidates" => args.extend(["curate", "candidates"].map(str::to_owned)),
        "curate validate" => {
            args.extend(["curate", "validate", "cand_renderer_contract"].map(str::to_owned));
        }
        "curate apply" => {
            args.extend(["curate", "apply", "cand_renderer_contract"].map(str::to_owned));
        }
        _ => return None,
    }
    Some(args)
}

#[test]
fn matrix_parses_and_covers_required_commands() -> TestResult {
    let entries = parse_matrix()?;
    if entries.is_empty() {
        return Err("renderer command capability matrix must not be empty".to_string());
    }

    let present: BTreeSet<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    for required in REQUIRED_COMMANDS {
        if !present.contains(required) {
            return Err(format!(
                "renderer command capability matrix is missing required command `{required}`"
            ));
        }
    }
    Ok(())
}

#[test]
fn command_names_are_unique_and_documented() -> TestResult {
    let entries = parse_matrix()?;
    let mut seen = BTreeSet::new();
    for entry in &entries {
        if !seen.insert(entry.name.as_str()) {
            return Err(format!(
                "renderer command capability matrix contains duplicate command `{}`",
                entry.name
            ));
        }
        if entry.owner.trim().len() < 5 {
            return Err(format!(
                "command `{}` must name a substantive owner path/module; got `{}`",
                entry.name, entry.owner
            ));
        }
        if entry.notes.trim().len() < 40 {
            return Err(format!(
                "command `{}` must include a substantive notes field explaining renderer intent",
                entry.name
            ));
        }
    }
    Ok(())
}

#[test]
fn every_command_declares_every_known_format_once() -> TestResult {
    let entries = parse_matrix()?;
    let required: BTreeSet<&str> = KNOWN_FORMATS.iter().copied().collect();
    for entry in &entries {
        let mut seen = BTreeSet::new();
        for (format, capability) in &entry.formats {
            if !KNOWN_FORMATS.contains(&format.as_str()) {
                return Err(format!(
                    "command `{}` declares unknown format `{}`; allowed: {:?}",
                    entry.name, format, KNOWN_FORMATS
                ));
            }
            if !seen.insert(format.as_str()) {
                return Err(format!(
                    "command `{}` declares format `{}` more than once",
                    entry.name, format
                ));
            }
            if !KNOWN_CAPABILITIES.contains(&capability.as_str()) {
                return Err(format!(
                    "command `{}` format `{}` uses unknown capability `{}`; allowed: {:?}",
                    entry.name, format, capability, KNOWN_CAPABILITIES
                ));
            }
        }
        let missing: Vec<&str> = required.difference(&seen).copied().collect();
        if !missing.is_empty() {
            return Err(format!(
                "command `{}` is missing format declarations: {:?}",
                entry.name, missing
            ));
        }
    }
    Ok(())
}

#[test]
fn json_is_canonical_and_json_override_is_pinned() -> TestResult {
    let entries = parse_matrix()?;
    for entry in &entries {
        let json_capability = entry
            .formats
            .iter()
            .find(|(format, _)| format == "json")
            .map(|(_, capability)| capability.as_str())
            .ok_or_else(|| format!("command `{}` is missing json format", entry.name))?;
        if json_capability != "canonical" {
            return Err(format!(
                "command `{}` must mark json as canonical; got `{json_capability}`",
                entry.name
            ));
        }
        if !entry.json_override {
            return Err(format!(
                "command `{}` must explicitly pin json/robot override behavior",
                entry.name
            ));
        }
    }
    Ok(())
}

#[test]
fn mermaid_support_is_explicitly_diagram_or_unsupported() -> TestResult {
    let entries = parse_matrix()?;
    let mut diagram_count = 0;
    for entry in &entries {
        let mermaid_capability = entry
            .formats
            .iter()
            .find(|(format, _)| format == "mermaid")
            .map(|(_, capability)| capability.as_str())
            .ok_or_else(|| format!("command `{}` is missing mermaid format", entry.name))?;
        match mermaid_capability {
            "diagram" => {
                diagram_count += 1;
                if entry.mermaid_markers.len() < 2 {
                    return Err(format!(
                        "command `{}` marks Mermaid as diagram but has fewer than two mermaid_markers",
                        entry.name
                    ));
                }
            }
            "unsupported" => {
                if !entry.mermaid_markers.is_empty() {
                    return Err(format!(
                        "command `{}` marks Mermaid unsupported but still lists mermaid_markers",
                        entry.name
                    ));
                }
                if !entry.notes.to_ascii_lowercase().contains("mermaid") {
                    return Err(format!(
                        "command `{}` marks Mermaid unsupported but notes do not explain Mermaid intent",
                        entry.name
                    ));
                }
            }
            other => {
                return Err(format!(
                    "command `{}` must mark mermaid as `diagram` or `unsupported`, not `{other}`. \
                     This prevents silent Markdown/human fallback from being mistaken for diagram support.",
                    entry.name
                ));
            }
        }
    }
    if diagram_count == 0 {
        return Err(
            "matrix must include at least one explicit Mermaid diagram command".to_string(),
        );
    }
    Ok(())
}

#[test]
fn unsupported_mermaid_matrix_entries_return_structured_usage_errors() -> TestResult {
    let entries = parse_matrix()?;
    let workspace = unique_workspace("unsupported-mermaid")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let mut checked = 0usize;
    for entry in &entries {
        if command_format_capability(entry, "mermaid") != Some("unsupported") {
            continue;
        }
        let args = unsupported_mermaid_args(&entry.name, &workspace_arg).ok_or_else(|| {
            format!(
                "command `{}` marks Mermaid unsupported but has no representative test invocation",
                entry.name
            )
        })?;
        let output = run_ee(&args)?;
        let first_line = first_stdout_line(&output);
        let command_line = format!("ee {}", args.join(" "));
        checked += 1;

        ensure(
            !output.status.success(),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=unsupported; expected nonzero status; \
                 observedFirstOutputLine={first_line:?}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        ensure(
            output.stderr.is_empty(),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=unsupported; structured error should be on stdout; \
                 observedFirstOutputLine={first_line:?}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;

        let parsed: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=unsupported; stdout must be ee.error.v2 JSON: {error}; \
                 observedFirstOutputLine={first_line:?}"
            )
        })?;
        ensure(
            parsed["schema"].as_str() == Some("ee.error.v2"),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=unsupported; schema drifted; \
                 observedFirstOutputLine={first_line:?}; got {parsed}"
            ),
        )?;
        ensure(
            parsed["error"]["code"].as_str() == Some("usage"),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=unsupported; code must stay generic usage unless a \
                 cataloged renderer-specific code is added; observedFirstOutputLine={first_line:?}; got {}",
                parsed["error"]
            ),
        )?;

        let message = parsed["error"]["message"].as_str().unwrap_or_default();
        for needle in [entry.name.as_str(), "--format mermaid", "unsupported"] {
            ensure(
                message.contains(needle),
                format!(
                    "command=`{command_line}`; requestedFormat=mermaid; \
                     expectedCapability=unsupported; message must contain {needle:?}; \
                     observedFirstOutputLine={first_line:?}; got {message}"
                ),
            )?;
        }

        let details = &parsed["error"]["details"];
        ensure(
            details["command"].as_str() == Some(entry.name.as_str()),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=unsupported; details.command drifted; \
                 observedFirstOutputLine={first_line:?}; got {details}"
            ),
        )?;
        ensure(
            details["requestedFormat"].as_str() == Some("mermaid"),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=unsupported; details.requestedFormat drifted; \
                 observedFirstOutputLine={first_line:?}; got {details}"
            ),
        )?;
        ensure(
            details["expectedCapability"].as_str() == Some("unsupported"),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=unsupported; details.expectedCapability drifted; \
                 observedFirstOutputLine={first_line:?}; got {details}"
            ),
        )?;

        let repair = parsed["error"]["repair"].as_str().unwrap_or_default();
        ensure(
            repair.contains("--json") && repair.contains("renderer_command_capabilities.toml"),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=unsupported; repair must name JSON fallback and matrix; \
                 observedFirstOutputLine={first_line:?}; got {repair}"
            ),
        )?;
        ensure(
            !first_line.starts_with("flowchart")
                && !first_line.starts_with("Memory")
                && !first_line.starts_with("Search")
                && !first_line.starts_with("Status"),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=unsupported; output must not be diagram or human fallback; \
                 observedFirstOutputLine={first_line:?}"
            ),
        )?;
    }

    ensure(
        checked > 0,
        "matrix must include at least one unsupported Mermaid command test case",
    )
}

#[test]
fn machine_modes_override_unsupported_mermaid_status_to_canonical_json() -> TestResult {
    let workspace = unique_workspace("status-mermaid-machine")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    for mode_flag in ["--json", "--robot"] {
        let args = [
            "--workspace",
            workspace_arg.as_str(),
            mode_flag,
            "--format",
            "mermaid",
            "status",
        ]
        .map(str::to_owned);
        let output = run_ee(&args)?;
        let first_line = first_stdout_line(&output);
        let command_line = format!("ee {}", args.join(" "));

        ensure(
            output.status.success(),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=canonical JSON override; status failed; \
                 observedFirstOutputLine={first_line:?}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        ensure(
            output.stderr.is_empty(),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=canonical JSON override; stderr must be empty; \
                 observedFirstOutputLine={first_line:?}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        let parsed: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=canonical JSON override; stdout must be JSON: {error}; \
                 observedFirstOutputLine={first_line:?}"
            )
        })?;
        ensure(
            parsed["schema"].as_str() == Some("ee.response.v2"),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=canonical JSON override; schema drifted; \
                 observedFirstOutputLine={first_line:?}; got {parsed}"
            ),
        )?;
        ensure(
            parsed["data"]["command"].as_str() == Some("status"),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=canonical JSON override; data.command drifted; \
                 observedFirstOutputLine={first_line:?}; got {}",
                parsed["data"]
            ),
        )?;
        ensure(
            !first_line.starts_with("Status") && !first_line.starts_with("flowchart"),
            format!(
                "command=`{command_line}`; requestedFormat=mermaid; \
                 expectedCapability=canonical JSON override; output must not be human or diagram fallback; \
                 observedFirstOutputLine={first_line:?}"
            ),
        )?;
    }

    Ok(())
}

fn documented_mermaid_command(line: &str) -> Option<String> {
    let line = line.trim().trim_matches('`').trim_end_matches('\\').trim();
    if !line.starts_with("ee ") || !line.contains("--format mermaid") {
        return None;
    }

    let tokens = line.split_whitespace().collect::<Vec<_>>();
    let first = *tokens.get(1)?;
    let command = match first {
        "graph" | "memory" | "curate" => {
            let second = *tokens.get(2)?;
            format!("{first} {second}")
        }
        other => other.to_owned(),
    };
    Some(command)
}

#[test]
fn flag_precedence_mermaid_examples_are_matrix_backed() -> TestResult {
    let entries = parse_matrix()?;
    let matrix = entries
        .iter()
        .map(|entry| {
            let mermaid = entry
                .formats
                .iter()
                .find(|(format, _)| format == "mermaid")
                .map(|(_, capability)| capability.as_str())
                .unwrap_or("missing");
            (entry.name.as_str(), mermaid)
        })
        .collect::<BTreeMap<_, _>>();

    let doc_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("cli-reference")
        .join("flag-precedence.md");
    let doc = fs::read_to_string(&doc_path)
        .map_err(|error| format!("read {}: {error}", doc_path.display()))?;
    let mut documented = BTreeSet::new();
    for line in doc.lines() {
        if let Some(command) = documented_mermaid_command(line) {
            documented.insert(command);
        }
    }
    if documented.is_empty() {
        return Err(format!(
            "{} must include at least one Mermaid command example",
            doc_path.display()
        ));
    }

    for command in documented {
        let capability = matrix.get(command.as_str()).ok_or_else(|| {
            format!(
                "{} documents `ee {command} --format mermaid`, but `{command}` is missing from tests/renderer_command_capabilities.toml",
                doc_path.display()
            )
        })?;
        if *capability != "diagram" {
            return Err(format!(
                "{} documents `ee {command} --format mermaid`, but the matrix marks Mermaid as `{capability}` instead of `diagram`",
                doc_path.display()
            ));
        }
    }

    Ok(())
}
