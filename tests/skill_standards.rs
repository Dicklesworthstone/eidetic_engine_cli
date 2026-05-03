use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

const SKILL_LINT_LOG_SCHEMA: &str = "ee.skill_standards.lint_log.v1";
const SKILL_EVIDENCE_BUNDLE_SCHEMA: &str = "ee.skill_evidence_bundle.v1";

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
    "evidenceBundlePath",
    "evidenceBundleHash",
    "evidenceBundleHashes",
    "provenanceIds",
    "redactionClasses",
    "redactionStatus",
    "trustClasses",
    "degradedCodes",
    "degradedStates",
    "promptInjectionQuarantineStatus",
    "directDbScrapingAllowed",
    "durableMutationViaExplicitEeCommand",
    "commandBoundaryMatrixRow",
    "readmeWorkflowRow",
    "outputArtifactPath",
    "firstMissingRequirement",
];

const REQUIRED_EVIDENCE_FIXTURE_CASES: &[&str] = &[
    "normal_bundle",
    "redacted_sensitive_value",
    "prompt_injection_like_evidence",
    "missing_provenance",
    "degraded_cli_output",
    "stale_evidence_bundle",
    "malformed_bundle",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillEvidenceBundle {
    schema: String,
    bundle_id: String,
    created_at: String,
    workspace: String,
    source_command: Vec<String>,
    allowed_input_formats: Vec<String>,
    evidence_items: Vec<SkillEvidenceItem>,
    redaction: SkillRedactionState,
    trust: SkillTrustState,
    degraded: Vec<SkillDegradedState>,
    prompt_injection: SkillPromptInjectionState,
    mutation_rules: SkillMutationRules,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillEvidenceItem {
    id: String,
    kind: String,
    provenance_uri: String,
    content_hash: String,
    redaction_classes: Vec<String>,
    trust_class: String,
    degraded_codes: Vec<String>,
    prompt_injection_quarantined: bool,
    stale: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillRedactionState {
    status: String,
    classes: Vec<String>,
    raw_secrets_included: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillTrustState {
    class: String,
    source_classes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillDegradedState {
    code: String,
    repair: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillPromptInjectionState {
    quarantined: bool,
    signals: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillMutationRules {
    no_direct_db_scraping: bool,
    durable_mutation_requires_explicit_ee_command: bool,
    allowed_mutation_commands: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct ValidatedSkillBundle {
    evidence_ids: Vec<String>,
    provenance_ids: Vec<String>,
    redaction_classes: Vec<String>,
    trust_classes: Vec<String>,
    degraded_codes: Vec<String>,
    prompt_injection_quarantined: bool,
    bundle_hash: String,
}

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
        "ee.skill_evidence_bundle.v1",
        "direct DB",
        "trust class",
        "prompt-injection",
        "durable memory mutation",
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
    let evidence_hash = format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex());
    json!({
        "schema": SKILL_LINT_LOG_SCHEMA,
        "skillPath": relative_display_path(skill_path),
        "requiredFiles": ["SKILL.md"],
        "parsedMetadata": metadata,
        "referencedEeCommands": commands,
        "evidenceBundlePath": "target/e2e/skills/ee-skill-standards.evidence.json",
        "evidenceBundleHash": evidence_hash.clone(),
        "evidenceBundleHashes": [evidence_hash],
        "provenanceIds": ["skill:ee-skill-standards/SKILL.md"],
        "redactionClasses": [],
        "redactionStatus": {
            "status": "verified_no_raw_secret_material",
            "rawSecretsIncluded": false
        },
        "trustClasses": ["project_local_skill_standard"],
        "degradedCodes": [],
        "degradedStates": [],
        "promptInjectionQuarantineStatus": {
            "quarantined": false,
            "signals": []
        },
        "directDbScrapingAllowed": false,
        "durableMutationViaExplicitEeCommand": true,
        "commandBoundaryMatrixRow": "skill-boundary",
        "readmeWorkflowRow": "project-local-skills",
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

fn sorted_strings<I, S>(items: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut strings = items.into_iter().map(Into::into).collect::<Vec<_>>();
    strings.sort();
    strings.dedup();
    strings
}

fn canonical_skill_bundle_hash(bundle: &SkillEvidenceBundle) -> Result<String, String> {
    let mut evidence_items = bundle.evidence_items.clone();
    evidence_items.sort_by(|left, right| left.id.cmp(&right.id));

    let canonical_evidence = evidence_items
        .into_iter()
        .map(|item| {
            json!({
                "contentHash": item.content_hash,
                "degradedCodes": sorted_strings(item.degraded_codes),
                "id": item.id,
                "kind": item.kind,
                "promptInjectionQuarantined": item.prompt_injection_quarantined,
                "provenanceUri": item.provenance_uri,
                "redactionClasses": sorted_strings(item.redaction_classes),
                "stale": item.stale,
                "trustClass": item.trust_class,
            })
        })
        .collect::<Vec<_>>();
    let canonical = json!({
        "allowedInputFormats": sorted_strings(bundle.allowed_input_formats.clone()),
        "bundleId": bundle.bundle_id.clone(),
        "createdAt": bundle.created_at.clone(),
        "degraded": bundle.degraded.iter().map(|state| {
            json!({"code": state.code.clone(), "repair": state.repair.clone()})
        }).collect::<Vec<_>>(),
        "evidenceItems": canonical_evidence,
        "mutationRules": {
            "allowedMutationCommands": sorted_strings(bundle.mutation_rules.allowed_mutation_commands.clone()),
            "durableMutationRequiresExplicitEeCommand": bundle.mutation_rules.durable_mutation_requires_explicit_ee_command,
            "noDirectDbScraping": bundle.mutation_rules.no_direct_db_scraping,
        },
        "promptInjection": {
            "quarantined": bundle.prompt_injection.quarantined,
            "signals": sorted_strings(bundle.prompt_injection.signals.clone()),
        },
        "redaction": {
            "classes": sorted_strings(bundle.redaction.classes.clone()),
            "rawSecretsIncluded": bundle.redaction.raw_secrets_included,
            "status": bundle.redaction.status.clone(),
        },
        "schema": bundle.schema.clone(),
        "sourceCommand": bundle.source_command.clone(),
        "trust": {
            "class": bundle.trust.class.clone(),
            "sourceClasses": sorted_strings(bundle.trust.source_classes.clone()),
        },
        "workspace": bundle.workspace.clone(),
    });
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|error| format!("failed to serialize canonical bundle: {error}"))?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

fn validate_skill_evidence_bundle(value: &Value) -> Result<ValidatedSkillBundle, String> {
    for field in [
        "schema",
        "bundleId",
        "createdAt",
        "workspace",
        "sourceCommand",
        "allowedInputFormats",
        "evidenceItems",
        "redaction",
        "trust",
        "degraded",
        "promptInjection",
        "mutationRules",
    ] {
        if value.get(field).is_none() {
            return Err(format!("missing_required_field:{field}"));
        }
    }
    let bundle = serde_json::from_value::<SkillEvidenceBundle>(value.clone())
        .map_err(|error| format!("bundle_parse_error:{error}"))?;
    if bundle.schema != SKILL_EVIDENCE_BUNDLE_SCHEMA {
        return Err(format!("schema_mismatch:{}", bundle.schema));
    }
    if bundle.bundle_id.trim().is_empty() {
        return Err("missing_required_field:bundleId".to_string());
    }
    if bundle.workspace.trim().is_empty() {
        return Err("missing_required_field:workspace".to_string());
    }
    if !bundle
        .source_command
        .first()
        .is_some_and(|command| command == "ee")
    {
        return Err("invalid_source_command".to_string());
    }
    if !bundle
        .source_command
        .iter()
        .any(|argument| argument == "--json")
    {
        return Err("source_command_must_be_json".to_string());
    }
    let allowed_formats = sorted_strings(bundle.allowed_input_formats.clone());
    if !allowed_formats.contains(&"json".to_string()) {
        return Err("missing_allowed_input_format:json".to_string());
    }
    if bundle.evidence_items.is_empty() {
        return Err("missing_required_field:evidenceItems".to_string());
    }
    if bundle.redaction.status == "unknown" {
        return Err("redaction_status_unknown".to_string());
    }
    if bundle.redaction.raw_secrets_included {
        return Err("redaction_failed:rawSecretsIncluded".to_string());
    }
    if bundle.trust.class.trim().is_empty() || bundle.trust.source_classes.is_empty() {
        return Err("missing_required_field:trust.class".to_string());
    }
    if !bundle.mutation_rules.no_direct_db_scraping {
        return Err("direct_db_scraping_declared".to_string());
    }
    if !bundle
        .mutation_rules
        .durable_mutation_requires_explicit_ee_command
    {
        return Err("durable_mutation_without_explicit_ee_command".to_string());
    }

    let degraded_codes = sorted_strings(bundle.degraded.iter().map(|state| state.code.clone()));
    let mut evidence_ids = Vec::new();
    let mut provenance_ids = Vec::new();
    let mut redaction_classes = bundle.redaction.classes.clone();
    let mut trust_classes = vec![bundle.trust.class.clone()];
    trust_classes.extend(bundle.trust.source_classes.clone());

    for (index, item) in bundle.evidence_items.iter().enumerate() {
        if item.id.trim().is_empty() {
            return Err(format!("missing_required_field:evidenceItems[{index}].id"));
        }
        if item.provenance_uri.trim().is_empty() {
            return Err(format!(
                "missing_required_field:evidenceItems[{index}].provenanceUri"
            ));
        }
        if !item.content_hash.starts_with("blake3:") {
            return Err(format!("invalid_content_hash:{}", item.id));
        }
        if item.trust_class.trim().is_empty() {
            return Err(format!(
                "missing_required_field:evidenceItems[{index}].trustClass"
            ));
        }
        if item.stale && !degraded_codes.iter().any(|code| code == "evidence_stale") {
            return Err(format!("stale_evidence_without_degraded_code:{}", item.id));
        }
        if item
            .degraded_codes
            .iter()
            .any(|code| !degraded_codes.contains(code))
        {
            return Err(format!("degraded_code_not_propagated:{}", item.id));
        }
        evidence_ids.push(item.id.clone());
        provenance_ids.push(item.provenance_uri.clone());
        redaction_classes.extend(item.redaction_classes.clone());
        trust_classes.push(item.trust_class.clone());
    }

    let prompt_injection_quarantined = bundle.prompt_injection.quarantined
        || bundle
            .evidence_items
            .iter()
            .any(|item| item.prompt_injection_quarantined);
    if !bundle.prompt_injection.signals.is_empty() && !prompt_injection_quarantined {
        return Err("prompt_injection_not_quarantined".to_string());
    }

    Ok(ValidatedSkillBundle {
        evidence_ids: sorted_strings(evidence_ids),
        provenance_ids: sorted_strings(provenance_ids),
        redaction_classes: sorted_strings(redaction_classes),
        trust_classes: sorted_strings(trust_classes),
        degraded_codes,
        prompt_injection_quarantined,
        bundle_hash: canonical_skill_bundle_hash(&bundle)?,
    })
}

fn skill_evidence_bundle_fixture(
    evidence_items: Vec<Value>,
    redaction_classes: Vec<&str>,
    degraded: Vec<Value>,
    prompt_injection_quarantined: bool,
    prompt_injection_signals: Vec<&str>,
) -> Value {
    json!({
        "schema": SKILL_EVIDENCE_BUNDLE_SCHEMA,
        "bundleId": "bundle_release_review_001",
        "createdAt": "2026-05-03T00:00:00Z",
        "workspace": "/workspace/example",
        "sourceCommand": ["ee", "context", "prepare release", "--workspace", "/workspace/example", "--json"],
        "allowedInputFormats": ["json", "markdown"],
        "evidenceItems": evidence_items,
        "redaction": {
            "status": "redacted",
            "classes": redaction_classes,
            "rawSecretsIncluded": false
        },
        "trust": {
            "class": "project_local_skill_handoff",
            "sourceClasses": ["mechanical_ee_json", "redacted_side_path_artifact"]
        },
        "degraded": degraded,
        "promptInjection": {
            "quarantined": prompt_injection_quarantined,
            "signals": prompt_injection_signals
        },
        "mutationRules": {
            "noDirectDbScraping": true,
            "durableMutationRequiresExplicitEeCommand": true,
            "allowedMutationCommands": ["ee remember --json", "ee outcome --json", "ee curate apply --json"]
        }
    })
}

fn skill_evidence_item(
    id: &str,
    provenance_uri: &str,
    redaction_classes: Vec<&str>,
    trust_class: &str,
    degraded_codes: Vec<&str>,
    prompt_injection_quarantined: bool,
    stale: bool,
) -> Value {
    json!({
        "id": id,
        "kind": "context_pack_item",
        "provenanceUri": provenance_uri,
        "contentHash": format!("blake3:{}", blake3::hash(id.as_bytes()).to_hex()),
        "redactionClasses": redaction_classes,
        "trustClass": trust_class,
        "degradedCodes": degraded_codes,
        "promptInjectionQuarantined": prompt_injection_quarantined,
        "stale": stale
    })
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
        "ee.skill_evidence_bundle.v1",
        "direct DB scraping",
        "durable memory mutation",
        "prompt-injection quarantine",
        "trust class",
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
            "ee.skill_evidence_bundle.v1",
            "direct DB",
            "trust class",
            "prompt-injection",
            "durable memory mutation",
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
    if required_json_field(&log, "evidenceBundleHash")? != &json!(hash) {
        return Err("skill lint log must mirror the primary evidence bundle hash".to_string());
    }
    if required_json_field(&log, "evidenceBundlePath")?
        != &json!("target/e2e/skills/ee-skill-standards.evidence.json")
    {
        return Err("skill lint log must record evidence bundle path".to_string());
    }
    if required_json_field(&log, "provenanceIds")? != &json!(["skill:ee-skill-standards/SKILL.md"])
    {
        return Err("skill lint log must record provenance ids".to_string());
    }
    if required_json_field(&log, "redactionClasses")? != &json!([]) {
        return Err("skill lint log must record redaction classes".to_string());
    }
    let redaction_status = required_json_field(&log, "redactionStatus")?;
    if required_json_field(redaction_status, "rawSecretsIncluded")? != &json!(false) {
        return Err("skill lint log must record redaction status".to_string());
    }
    if required_json_field(&log, "trustClasses")? != &json!(["project_local_skill_standard"]) {
        return Err("skill lint log must record trust classes".to_string());
    }
    if required_json_field(&log, "degradedCodes")? != &json!([]) {
        return Err("skill lint log must record degraded codes".to_string());
    }
    if !required_json_field(&log, "degradedStates")?
        .as_array()
        .is_some_and(std::vec::Vec::is_empty)
    {
        return Err("skill lint log must record degraded states".to_string());
    }
    let quarantine = required_json_field(&log, "promptInjectionQuarantineStatus")?;
    if required_json_field(quarantine, "quarantined")? != &json!(false) {
        return Err("skill lint log must record prompt-injection quarantine status".to_string());
    }
    if required_json_field(&log, "directDbScrapingAllowed")? != &json!(false) {
        return Err("skill lint log must forbid direct DB scraping".to_string());
    }
    if required_json_field(&log, "durableMutationViaExplicitEeCommand")? != &json!(true) {
        return Err("skill lint log must require explicit ee mutation commands".to_string());
    }
    if required_json_field(&log, "commandBoundaryMatrixRow")? != &json!("skill-boundary") {
        return Err("skill lint log must record the command-boundary row".to_string());
    }
    if required_json_field(&log, "readmeWorkflowRow")? != &json!("project-local-skills") {
        return Err("skill lint log must record the README workflow row".to_string());
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

#[test]
fn project_skill_standards_document_redacted_evidence_handoff_contract() -> TestResult {
    let readme = read_to_string(&repo_root().join("skills/README.md"))?;
    let skill = read_to_string(&repo_root().join("skills/ee-skill-standards/SKILL.md"))?;
    let boundary_log = read_to_string(&repo_root().join("docs/boundary-migration-e2e-logging.md"))?;
    let inventory =
        read_to_string(&repo_root().join("docs/mechanical-boundary-command-inventory.md"))?;

    for (label, content) in [
        ("skills README", readme.as_str()),
        ("skill standards", skill.as_str()),
        ("boundary logging doc", boundary_log.as_str()),
        ("command matrix", inventory.as_str()),
    ] {
        for phrase in [
            "ee.skill_evidence_bundle.v1",
            "direct DB scraping",
            "explicit `ee`",
            "provenance",
            "redaction",
            "trust",
            "degraded",
            "prompt-injection",
        ] {
            if !content.contains(phrase) {
                return Err(format!(
                    "{label} missing evidence handoff phrase `{phrase}`"
                ));
            }
        }
    }

    Ok(())
}

#[test]
fn skill_evidence_bundle_validation_covers_required_fixture_cases() -> TestResult {
    let normal_bundle = skill_evidence_bundle_fixture(
        vec![skill_evidence_item(
            "ev.normal",
            "ee://memory/mem_normal",
            vec![],
            "mechanical_ee_json",
            vec![],
            false,
            false,
        )],
        vec![],
        vec![],
        false,
        vec![],
    );
    let redacted_sensitive_value = skill_evidence_bundle_fixture(
        vec![skill_evidence_item(
            "ev.redacted_sensitive_value",
            "ee://support/redaction-report/sensitive",
            vec!["api_key", "home_path"],
            "mechanical_ee_json",
            vec![],
            false,
            false,
        )],
        vec!["api_key", "home_path"],
        vec![],
        false,
        vec![],
    );
    let prompt_injection_like_evidence = skill_evidence_bundle_fixture(
        vec![skill_evidence_item(
            "ev.prompt_injection",
            "ee://recorder/event/prompt-injection",
            vec!["prompt_injection_like"],
            "untrusted_transcript_excerpt",
            vec![],
            true,
            false,
        )],
        vec!["prompt_injection_like"],
        vec![],
        true,
        vec!["instruction_like_text"],
    );
    let degraded_cli_output = skill_evidence_bundle_fixture(
        vec![skill_evidence_item(
            "ev.degraded",
            "ee://command/context/degraded",
            vec![],
            "degraded_ee_json",
            vec!["context_unavailable"],
            false,
            false,
        )],
        vec![],
        vec![json!({
            "code": "context_unavailable",
            "repair": "ee init --workspace /workspace/example"
        })],
        false,
        vec![],
    );
    let stale_evidence_bundle = skill_evidence_bundle_fixture(
        vec![skill_evidence_item(
            "ev.stale",
            "ee://index/search/stale",
            vec![],
            "degraded_ee_json",
            vec!["evidence_stale"],
            false,
            true,
        )],
        vec![],
        vec![json!({
            "code": "evidence_stale",
            "repair": "ee index rebuild --workspace /workspace/example"
        })],
        false,
        vec![],
    );
    let mut missing_provenance = normal_bundle.clone();
    missing_provenance["evidenceItems"][0]["provenanceUri"] = json!("");

    let cases = [
        ("normal_bundle", Some(normal_bundle), None),
        (
            "redacted_sensitive_value",
            Some(redacted_sensitive_value),
            None,
        ),
        (
            "prompt_injection_like_evidence",
            Some(prompt_injection_like_evidence),
            None,
        ),
        (
            "missing_provenance",
            Some(missing_provenance),
            Some("missing_required_field:evidenceItems[0].provenanceUri"),
        ),
        ("degraded_cli_output", Some(degraded_cli_output), None),
        ("stale_evidence_bundle", Some(stale_evidence_bundle), None),
        ("malformed_bundle", None, Some("bundle_parse_error")),
    ];
    let case_names = cases.iter().map(|(name, _, _)| *name).collect::<Vec<_>>();
    for required_case in REQUIRED_EVIDENCE_FIXTURE_CASES {
        if !case_names.contains(required_case) {
            return Err(format!(
                "missing skill evidence fixture case `{required_case}`"
            ));
        }
    }

    for (case_name, value, expected_error_prefix) in cases {
        let result = match value {
            Some(value) => validate_skill_evidence_bundle(&value),
            None => serde_json::from_str::<Value>("{not-json")
                .map_err(|error| format!("bundle_parse_error:{error}"))
                .and_then(|value| validate_skill_evidence_bundle(&value)),
        };
        match (result, expected_error_prefix) {
            (Ok(_), None) => {}
            (Err(error), Some(prefix)) if error.starts_with(prefix) => {}
            (Ok(_), Some(prefix)) => {
                return Err(format!("{case_name} unexpectedly passed; wanted {prefix}"));
            }
            (Err(error), None) => {
                return Err(format!("{case_name} unexpectedly failed: {error}"));
            }
            (Err(error), Some(prefix)) => {
                return Err(format!(
                    "{case_name} failed with `{error}`, wanted `{prefix}`"
                ));
            }
        }
    }

    Ok(())
}

#[test]
fn skill_evidence_bundle_propagates_security_and_degraded_state() -> TestResult {
    let value = skill_evidence_bundle_fixture(
        vec![
            skill_evidence_item(
                "ev.degraded",
                "ee://command/search/degraded",
                vec!["home_path"],
                "degraded_ee_json",
                vec!["semantic_disabled"],
                false,
                false,
            ),
            skill_evidence_item(
                "ev.quarantined",
                "ee://recorder/event/quarantined",
                vec!["prompt_injection_like"],
                "untrusted_transcript_excerpt",
                vec!["semantic_disabled"],
                true,
                false,
            ),
        ],
        vec!["home_path", "prompt_injection_like"],
        vec![json!({
            "code": "semantic_disabled",
            "repair": "ee index reembed --dry-run --workspace /workspace/example"
        })],
        true,
        vec!["instruction_like_text"],
    );

    let validated = validate_skill_evidence_bundle(&value)?;
    if validated.evidence_ids != vec!["ev.degraded".to_string(), "ev.quarantined".to_string()] {
        return Err("bundle must expose deterministic evidence IDs".to_string());
    }
    if validated.provenance_ids
        != vec![
            "ee://command/search/degraded".to_string(),
            "ee://recorder/event/quarantined".to_string(),
        ]
    {
        return Err("bundle must propagate provenance IDs".to_string());
    }
    if validated.redaction_classes
        != vec!["home_path".to_string(), "prompt_injection_like".to_string()]
    {
        return Err("bundle must propagate redaction classes".to_string());
    }
    if !validated
        .trust_classes
        .contains(&"untrusted_transcript_excerpt".to_string())
    {
        return Err("bundle must propagate item trust classes".to_string());
    }
    if validated.degraded_codes != vec!["semantic_disabled".to_string()] {
        return Err("bundle must propagate degraded codes".to_string());
    }
    if !validated.prompt_injection_quarantined {
        return Err("bundle must preserve prompt-injection quarantine status".to_string());
    }

    Ok(())
}

#[test]
fn skill_evidence_bundle_rejects_unsafe_handoff_states() -> TestResult {
    let mut raw_secret_bundle = skill_evidence_bundle_fixture(
        vec![skill_evidence_item(
            "ev.raw_secret",
            "ee://support/redaction-report/raw",
            vec!["api_key"],
            "mechanical_ee_json",
            vec![],
            false,
            false,
        )],
        vec!["api_key"],
        vec![],
        false,
        vec![],
    );
    raw_secret_bundle["redaction"]["rawSecretsIncluded"] = json!(true);

    let mut direct_db_bundle = raw_secret_bundle.clone();
    direct_db_bundle["redaction"]["rawSecretsIncluded"] = json!(false);
    direct_db_bundle["mutationRules"]["noDirectDbScraping"] = json!(false);

    let unquarantined_prompt_injection = skill_evidence_bundle_fixture(
        vec![skill_evidence_item(
            "ev.unquarantined",
            "ee://recorder/event/unquarantined",
            vec!["prompt_injection_like"],
            "untrusted_transcript_excerpt",
            vec![],
            false,
            false,
        )],
        vec!["prompt_injection_like"],
        vec![],
        false,
        vec!["instruction_like_text"],
    );

    let mut stale_without_degraded = skill_evidence_bundle_fixture(
        vec![skill_evidence_item(
            "ev.stale",
            "ee://index/search/stale",
            vec![],
            "mechanical_ee_json",
            vec![],
            false,
            true,
        )],
        vec![],
        vec![],
        false,
        vec![],
    );
    stale_without_degraded["evidenceItems"][0]["degradedCodes"] = json!([]);

    for (value, expected_error) in [
        (raw_secret_bundle, "redaction_failed:rawSecretsIncluded"),
        (direct_db_bundle, "direct_db_scraping_declared"),
        (
            unquarantined_prompt_injection,
            "prompt_injection_not_quarantined",
        ),
        (
            stale_without_degraded,
            "stale_evidence_without_degraded_code:ev.stale",
        ),
    ] {
        let error = match validate_skill_evidence_bundle(&value) {
            Ok(_) => return Err("unsafe skill evidence bundle unexpectedly passed".to_string()),
            Err(error) => error,
        };
        if error != expected_error {
            return Err(format!(
                "unsafe bundle failed with `{error}`, wanted `{expected_error}`"
            ));
        }
    }

    Ok(())
}

#[test]
fn skill_evidence_bundle_hash_is_stable_and_order_independent() -> TestResult {
    let first = skill_evidence_bundle_fixture(
        vec![
            skill_evidence_item(
                "ev.b",
                "ee://memory/b",
                vec!["home_path", "api_key"],
                "mechanical_ee_json",
                vec![],
                false,
                false,
            ),
            skill_evidence_item(
                "ev.a",
                "ee://memory/a",
                vec!["api_key", "home_path"],
                "mechanical_ee_json",
                vec![],
                false,
                false,
            ),
        ],
        vec!["api_key", "home_path"],
        vec![],
        false,
        vec![],
    );
    let second = skill_evidence_bundle_fixture(
        vec![
            skill_evidence_item(
                "ev.a",
                "ee://memory/a",
                vec!["home_path", "api_key"],
                "mechanical_ee_json",
                vec![],
                false,
                false,
            ),
            skill_evidence_item(
                "ev.b",
                "ee://memory/b",
                vec!["api_key", "home_path"],
                "mechanical_ee_json",
                vec![],
                false,
                false,
            ),
        ],
        vec!["home_path", "api_key"],
        vec![],
        false,
        vec![],
    );

    let first = validate_skill_evidence_bundle(&first)?;
    let second = validate_skill_evidence_bundle(&second)?;
    if first.bundle_hash != second.bundle_hash {
        return Err("bundle hash must be independent of evidence item ordering".to_string());
    }
    if first.evidence_ids != vec!["ev.a".to_string(), "ev.b".to_string()] {
        return Err("evidence IDs must be sorted deterministically".to_string());
    }

    Ok(())
}
