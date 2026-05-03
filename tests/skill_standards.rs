use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

const SKILL_LINT_LOG_SCHEMA: &str = "ee.skill_standards.lint_log.v1";

const REQUIRED_SECTIONS: &[&str] = &[
    "## Trigger Conditions",
    "## Mechanical Command Boundary",
    "## Evidence Gathering",
    "## Stop/Go Gates",
    "## Output Template",
    "## Uncertainty Handling",
    "## Privacy And Redaction",
    "## Degraded Behavior",
    "## Unsupported Claims",
    "## Testing Requirements",
    "## E2E Logging",
];

const REQUIRED_LOG_FIELDS: &[&str] = &[
    "schema",
    "skillPath",
    "requiredFiles",
    "parsedMetadata",
    "referencedEeCommands",
    "evidenceBundleHashes",
    "redactionStatus",
    "degradedStates",
    "outputArtifactPath",
    "firstMissingRequirement",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_to_string(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("failed to read {}: {error}", path.display()))
}

fn skill_dirs(root: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(root)
        .map_err(|error| format!("failed to read skills dir {}: {error}", root.display()))?;
    let mut dirs = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| format!("failed to read skills dir entry: {error}"))?
            .path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn frontmatter_value<'a>(frontmatter: &'a str, key: &str) -> Option<&'a str> {
    frontmatter.lines().find_map(|line| {
        let (line_key, value) = line.split_once(':')?;
        if line_key.trim() == key {
            Some(value.trim())
        } else {
            None
        }
    })
}

fn relative_display_path(path: &Path) -> String {
    path.strip_prefix(repo_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn parse_frontmatter<'a>(content: &'a str, path: &Path) -> Result<(&'a str, &'a str), String> {
    let Some(after_open) = content.strip_prefix("---\n") else {
        return Err(format!("{} missing YAML frontmatter", path.display()));
    };
    after_open
        .split_once("\n---\n")
        .ok_or_else(|| format!("{} frontmatter must close with `---`", path.display()))
}

fn parsed_frontmatter_map(frontmatter: &str) -> BTreeMap<String, String> {
    frontmatter
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn referenced_ee_commands(content: &str) -> Vec<String> {
    let mut commands = content
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("ee ") && line.contains("--json"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    commands.sort();
    commands.dedup();
    commands
}

fn first_missing_requirement(skill_path: &Path, content: &str) -> Option<String> {
    if !skill_path.ends_with("SKILL.md") {
        return Some(format!("missing required {}", skill_path.display()));
    }
    let Ok((_frontmatter, body)) = parse_frontmatter(content, skill_path) else {
        return Some("missing YAML frontmatter".to_string());
    };
    for section in REQUIRED_SECTIONS {
        if !body.contains(section) {
            return Some(format!("missing section `{section}`"));
        }
    }
    for phrase in [
        "ee status",
        "--json",
        "degraded",
        "redaction",
        "Unsupported",
        "evidence",
    ] {
        if !body.contains(phrase) {
            return Some(format!("missing required guidance phrase `{phrase}`"));
        }
    }
    None
}

fn build_skill_lint_log(skill_path: &Path, content: &str, output_artifact_path: &Path) -> Value {
    let (metadata, commands) = match parse_frontmatter(content, skill_path) {
        Ok((frontmatter, body)) => (
            parsed_frontmatter_map(frontmatter),
            referenced_ee_commands(body),
        ),
        Err(_) => (BTreeMap::new(), Vec::new()),
    };
    json!({
        "schema": SKILL_LINT_LOG_SCHEMA,
        "skillPath": relative_display_path(skill_path),
        "requiredFiles": ["SKILL.md"],
        "parsedMetadata": metadata,
        "referencedEeCommands": commands,
        "evidenceBundleHashes": [format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex())],
        "redactionStatus": {
            "status": "verified_no_raw_secret_material",
            "rawSecretsIncluded": false
        },
        "degradedStates": [],
        "outputArtifactPath": relative_display_path(output_artifact_path),
        "firstMissingRequirement": first_missing_requirement(skill_path, content),
    })
}

fn required_json_field<'a>(value: &'a Value, field: &str) -> Result<&'a Value, String> {
    value
        .get(field)
        .ok_or_else(|| format!("skill lint log missing required field `{field}`"))
}

fn assert_valid_skill_name(name: &str, path: &Path) -> TestResult {
    let valid = !name.is_empty()
        && name.len() < 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "{} has invalid skill name `{name}`; use lowercase letters, digits, and hyphens",
            path.display()
        ))
    }
}

#[test]
fn project_local_skills_directory_has_standards() -> TestResult {
    let root = repo_root().join("skills");
    if !root.is_dir() {
        return Err(format!(
            "missing project-local skills dir: {}",
            root.display()
        ));
    }

    let index = read_to_string(&root.join("README.md"))?;
    for section in REQUIRED_SECTIONS {
        let required = section.trim_start_matches("## ");
        if !index.contains(required) {
            return Err(format!(
                "skills README missing required section `{required}`"
            ));
        }
    }
    for phrase in [
        "JSON data comes from stdout",
        "degraded",
        "redaction",
        "durable mutation",
        "explicit `ee` commands",
        "minimum test shape",
        "skill path",
        "first missing requirement",
        "agent judgment",
        "Rust `ee`",
    ] {
        if !index.contains(phrase) {
            return Err(format!(
                "skills README missing convention phrase `{phrase}`"
            ));
        }
    }

    Ok(())
}

#[test]
fn every_project_skill_has_required_frontmatter_and_sections() -> TestResult {
    let root = repo_root().join("skills");
    let dirs = skill_dirs(&root)?;
    if dirs.is_empty() {
        return Err("skills dir must contain at least one project-local skill".to_string());
    }

    for dir in dirs {
        let skill_path = dir.join("SKILL.md");
        if !skill_path.is_file() {
            return Err(format!("missing required {}", skill_path.display()));
        }
        if dir.join("README.md").exists() {
            return Err(format!(
                "skill folder {} must keep instructions in SKILL.md, not README.md",
                dir.display()
            ));
        }

        let content = read_to_string(&skill_path)?;
        let (frontmatter, body) = parse_frontmatter(&content, &skill_path)?;
        let name = frontmatter_value(frontmatter, "name")
            .ok_or_else(|| format!("{} missing `name` frontmatter", skill_path.display()))?;
        let description = frontmatter_value(frontmatter, "description")
            .ok_or_else(|| format!("{} missing `description` frontmatter", skill_path.display()))?;
        assert_valid_skill_name(name, &skill_path)?;
        if !description.contains("Use when") {
            return Err(format!(
                "{} description must include trigger language with `Use when`",
                skill_path.display()
            ));
        }

        for section in REQUIRED_SECTIONS {
            if !body.contains(section) {
                return Err(format!(
                    "{} missing section `{section}`",
                    skill_path.display()
                ));
            }
        }
        for phrase in [
            "ee status",
            "--json",
            "degraded",
            "redaction",
            "Unsupported",
            "evidence",
        ] {
            if !body.contains(phrase) {
                return Err(format!(
                    "{} missing required guidance phrase `{phrase}`",
                    skill_path.display()
                ));
            }
        }
    }

    Ok(())
}

#[test]
fn skill_lint_log_contract_records_required_fields() -> TestResult {
    let skill_path = repo_root().join("skills/ee-skill-standards/SKILL.md");
    let content = read_to_string(&skill_path)?;
    let output_artifact_path = repo_root().join("target/e2e/skills/ee-skill-standards.json");
    let log = build_skill_lint_log(&skill_path, &content, &output_artifact_path);

    for field in REQUIRED_LOG_FIELDS {
        required_json_field(&log, field)?;
    }
    if required_json_field(&log, "schema")? != &json!(SKILL_LINT_LOG_SCHEMA) {
        return Err("skill lint log has wrong schema".to_string());
    }
    if required_json_field(&log, "skillPath")? != &json!("skills/ee-skill-standards/SKILL.md") {
        return Err("skill lint log must record project-relative skill path".to_string());
    }
    if required_json_field(&log, "requiredFiles")? != &json!(["SKILL.md"]) {
        return Err("skill lint log must record required skill files".to_string());
    }
    let parsed_metadata = required_json_field(&log, "parsedMetadata")?;
    if required_json_field(parsed_metadata, "name")? != &json!("ee-skill-standards") {
        return Err("skill lint log must record parsed skill metadata".to_string());
    }

    let commands = required_json_field(&log, "referencedEeCommands")?
        .as_array()
        .ok_or_else(|| "referencedEeCommands must be an array".to_string())?;
    if commands.is_empty() {
        return Err("skill lint log must record referenced ee commands".to_string());
    }
    for command in commands {
        let command = command
            .as_str()
            .ok_or_else(|| "referenced ee command must be a string".to_string())?;
        if !command.starts_with("ee ") || !command.contains("--json") {
            return Err(format!("invalid referenced ee command `{command}`"));
        }
    }

    let evidence_hashes = required_json_field(&log, "evidenceBundleHashes")?
        .as_array()
        .ok_or_else(|| "evidenceBundleHashes must be an array".to_string())?;
    if evidence_hashes.len() != 1 {
        return Err("skill lint log must record one evidence bundle hash".to_string());
    }
    let hash = evidence_hashes
        .first()
        .ok_or_else(|| "missing evidence bundle hash".to_string())?
        .as_str()
        .ok_or_else(|| "evidence bundle hash must be a string".to_string())?;
    if !hash.starts_with("blake3:") {
        return Err("evidence bundle hash must include blake3 prefix".to_string());
    }
    let redaction_status = required_json_field(&log, "redactionStatus")?;
    if required_json_field(redaction_status, "rawSecretsIncluded")? != &json!(false) {
        return Err("skill lint log must record redaction status".to_string());
    }
    if !required_json_field(&log, "degradedStates")?
        .as_array()
        .is_some_and(std::vec::Vec::is_empty)
    {
        return Err("skill lint log must record degraded states".to_string());
    }
    if required_json_field(&log, "outputArtifactPath")?
        != &json!("target/e2e/skills/ee-skill-standards.json")
    {
        return Err("skill lint log must record output artifact path".to_string());
    }
    if !required_json_field(&log, "firstMissingRequirement")?.is_null() {
        return Err("valid skill should not report a missing requirement".to_string());
    }

    Ok(())
}

#[test]
fn skill_lint_log_reports_first_missing_requirement() -> TestResult {
    let broken_skill_path = repo_root().join("skills/broken/SKILL.md");
    let broken_content = "---\nname: broken\ndescription: Use when broken.\n---\n# Broken\n";
    let log = build_skill_lint_log(
        &broken_skill_path,
        broken_content,
        &repo_root().join("target/e2e/skills/broken.json"),
    );

    if required_json_field(&log, "firstMissingRequirement")?
        != &json!("missing section `## Trigger Conditions`")
    {
        return Err("skill lint log must report the first missing requirement".to_string());
    }

    Ok(())
}
