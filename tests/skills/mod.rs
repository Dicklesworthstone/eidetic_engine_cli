//! Shared skill test harness and utilities.
//!
//! This module provides infrastructure for testing project-local skills,
//! validating SKILL.md structure, parsing fixtures, and logging e2e results.

use serde::Deserialize;

pub type TestResult = Result<(), String>;

pub const SCHEMA_SKILL_LINT_LOG: &str = "ee.skill_standards.lint_log.v1";

pub const REQUIRED_SECTIONS: &[&str] = &[
    "## Trigger Conditions",
    "## Stop/Go Gates",
    "## Evidence Gathering",
    "## Output Template",
    "## Uncertainty Handling",
    "## Degraded Behavior",
    "## Testing Requirements",
    "## E2E Logging",
];

pub const REQUIRED_LOG_FIELDS: &[&str] = &[
    "skill_path",
    "fixture_id",
    "fixture_hash",
    "evidence_bundle_path",
    "evidence_bundle_hash",
    "redaction_status",
    "degraded_codes",
    "output_artifact_path",
    "required_section_check",
    "first_failure_diagnosis",
];

#[derive(Debug, Clone)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct ParsedSkill {
    pub frontmatter: SkillFrontmatter,
    pub body: String,
}

pub fn parse_frontmatter(content: &str) -> Result<(&str, &str), String> {
    let after_open = content
        .strip_prefix("---\n")
        .ok_or_else(|| "skill is missing opening YAML frontmatter".to_string())?;
    after_open
        .split_once("\n---\n")
        .ok_or_else(|| "skill frontmatter must close with `---`".to_string())
}

pub fn frontmatter_value<'a>(frontmatter: &'a str, key: &str) -> Option<&'a str> {
    frontmatter.lines().find_map(|line| {
        let (candidate, value) = line.split_once(':')?;
        (candidate.trim() == key).then_some(value.trim())
    })
}

pub fn parse_skill(content: &str) -> Result<ParsedSkill, String> {
    let (frontmatter_raw, body) = parse_frontmatter(content)?;
    let name = frontmatter_value(frontmatter_raw, "name")
        .ok_or_else(|| "skill frontmatter missing name".to_string())?
        .to_string();
    let description = frontmatter_value(frontmatter_raw, "description")
        .ok_or_else(|| "skill frontmatter missing description".to_string())?
        .to_string();

    Ok(ParsedSkill {
        frontmatter: SkillFrontmatter { name, description },
        body: body.to_string(),
    })
}

pub fn assert_contains_all(label: &str, content: &str, required: &[&str]) -> TestResult {
    for item in required {
        if !content.contains(item) {
            return Err(format!("{label} missing required text `{item}`"));
        }
    }
    Ok(())
}

pub fn assert_has_required_sections(skill_body: &str, skill_name: &str) -> TestResult {
    assert_contains_all(
        &format!("{skill_name} skill"),
        skill_body,
        REQUIRED_SECTIONS,
    )
}

pub fn compute_fixture_hash(fixture_id: &str) -> String {
    format!("blake3:{}", blake3::hash(fixture_id.as_bytes()).to_hex())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericSkillFixture {
    pub id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub ee_commands: Vec<String>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub redaction_status: String,
    #[serde(default)]
    pub raw_secrets_included: bool,
    #[serde(default)]
    pub prompt_injection_quarantined: bool,
    #[serde(default)]
    pub degraded_codes: Vec<String>,
    #[serde(default)]
    pub expected_disposition: String,
    #[serde(default)]
    pub first_failure_diagnosis: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericSkillFixtures {
    pub schema: String,
    #[serde(default)]
    pub skill_path: String,
    pub fixtures: Vec<GenericSkillFixture>,
}

pub fn parse_fixtures<T: for<'de> Deserialize<'de>>(
    json: &str,
    skill_name: &str,
) -> Result<T, String> {
    serde_json::from_str(json)
        .map_err(|error| format!("{skill_name} skill fixtures must parse as JSON: {error}"))
}

pub fn validate_fixture_dispositions(
    fixtures: &[GenericSkillFixture],
    skill_name: &str,
) -> TestResult {
    let valid_dispositions = [
        "go",
        "no-go",
        "needs-escalation",
        "unavailable",
        "refuse",
        "draft_only",
        "refuse_promotion",
        "ready_for_manual_review",
        "request_more_evidence",
        "reject_non_durable",
        "reject_duplicate",
        "generate_candidate",
        "request_repair",
        "review_supported",
        "refuse_strengthen",
        "request_evidence",
        "supported_hypothesis",
        "refuse_strong_claim",
        "hypothesis_only",
        "request_data_collection",
        "generate_plan",
        "propose_replication",
        "request_more_samples",
        "ask_user",
    ];
    for fixture in fixtures {
        if !fixture.expected_disposition.is_empty()
            && !valid_dispositions.contains(&fixture.expected_disposition.as_str())
        {
            return Err(format!(
                "{skill_name} fixture {} has invalid disposition `{}`; valid: {valid_dispositions:?}",
                fixture.id, fixture.expected_disposition
            ));
        }
    }
    Ok(())
}

pub fn validate_redaction_states(fixtures: &[GenericSkillFixture], skill_name: &str) -> TestResult {
    let valid_states = ["passed", "failed", "unknown", "redacted", "not_applicable"];
    for fixture in fixtures {
        if !fixture.redaction_status.is_empty()
            && !valid_states.contains(&fixture.redaction_status.as_str())
        {
            return Err(format!(
                "{skill_name} fixture {} has invalid redaction_status `{}`; valid: {valid_states:?}",
                fixture.id, fixture.redaction_status
            ));
        }
    }
    Ok(())
}

pub fn validate_refusal_fixtures(fixtures: &[GenericSkillFixture], skill_name: &str) -> TestResult {
    let refusal_fixtures: Vec<_> = fixtures
        .iter()
        .filter(|f| f.expected_disposition == "refuse" || f.expected_disposition == "unavailable")
        .collect();

    for fixture in refusal_fixtures {
        if fixture.first_failure_diagnosis.is_none() {
            return Err(format!(
                "{skill_name} fixture {} expects refusal but has no first_failure_diagnosis",
                fixture.id
            ));
        }
    }
    Ok(())
}

pub fn validate_no_hidden_cot(skill_body: &str, skill_name: &str) -> TestResult {
    let forbidden_patterns = [
        "hidden chain-of-thought",
        "private reasoning as durable memory",
        "scrape raw DB",
        "direct DB scraping",
    ];
    for pattern in forbidden_patterns {
        if skill_body.to_lowercase().contains(&pattern.to_lowercase()) {
            let context = skill_body
                .lines()
                .find(|line| line.to_lowercase().contains(&pattern.to_lowercase()))
                .unwrap_or("");
            if !context.contains("must not")
                && !context.contains("never")
                && !context.contains("forbidden")
                && !context.contains("not allowed")
                && !context.contains("Do not")
            {
                return Err(format!(
                    "{skill_name} skill may improperly reference `{pattern}` without forbidding it"
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SkillLintLog {
    pub schema: &'static str,
    pub skill_path: String,
    pub fixture_id: Option<String>,
    pub fixture_hash: Option<String>,
    pub evidence_bundle_path: Option<String>,
    pub evidence_bundle_hash: Option<String>,
    pub mutation_posture: String,
    pub redaction_status: Option<String>,
    pub degraded_codes: Vec<String>,
    pub output_artifact_path: Option<String>,
    pub required_section_check: String,
    pub first_failure_diagnosis: Option<String>,
}

impl SkillLintLog {
    pub fn new(skill_path: impl Into<String>) -> Self {
        Self {
            schema: SCHEMA_SKILL_LINT_LOG,
            skill_path: skill_path.into(),
            fixture_id: None,
            fixture_hash: None,
            evidence_bundle_path: None,
            evidence_bundle_hash: None,
            mutation_posture: "read-only".to_string(),
            redaction_status: None,
            degraded_codes: Vec::new(),
            output_artifact_path: None,
            required_section_check: "pending".to_string(),
            first_failure_diagnosis: None,
        }
    }

    pub fn with_fixture(mut self, fixture: &GenericSkillFixture) -> Self {
        self.fixture_id = Some(fixture.id.clone());
        self.fixture_hash = Some(compute_fixture_hash(&fixture.id));
        self.redaction_status = Some(fixture.redaction_status.clone());
        self.degraded_codes = fixture.degraded_codes.clone();
        self.first_failure_diagnosis = fixture.first_failure_diagnosis.clone();
        self
    }

    pub fn with_section_check(mut self, status: &str) -> Self {
        self.required_section_check = status.to_string();
        self
    }

    pub fn to_json(&self) -> String {
        serde_json::json!({
            "schema": self.schema,
            "skill_path": self.skill_path,
            "fixture_id": self.fixture_id,
            "fixture_hash": self.fixture_hash,
            "evidence_bundle_path": self.evidence_bundle_path,
            "evidence_bundle_hash": self.evidence_bundle_hash,
            "mutation_posture": self.mutation_posture,
            "redaction_status": self.redaction_status,
            "degraded_codes": self.degraded_codes,
            "output_artifact_path": self.output_artifact_path,
            "required_section_check": self.required_section_check,
            "first_failure_diagnosis": self.first_failure_diagnosis,
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_extracts_name_and_body() -> TestResult {
        let content =
            "---\nname: test-skill\ndescription: A test skill.\n---\n\n# Test\n\nBody content.";
        let (frontmatter, body) = parse_frontmatter(content)?;
        assert!(frontmatter.contains("name: test-skill"));
        assert!(body.contains("Body content."));
        Ok(())
    }

    #[test]
    fn frontmatter_value_extracts_correct_value() {
        let frontmatter = "name: test-skill\ndescription: A test skill description.";
        assert_eq!(frontmatter_value(frontmatter, "name"), Some("test-skill"));
        assert_eq!(
            frontmatter_value(frontmatter, "description"),
            Some("A test skill description.")
        );
        assert_eq!(frontmatter_value(frontmatter, "missing"), None);
    }

    #[test]
    fn compute_fixture_hash_is_deterministic() {
        let hash1 = compute_fixture_hash("test_fixture_001");
        let hash2 = compute_fixture_hash("test_fixture_001");
        assert_eq!(hash1, hash2);
        assert!(hash1.starts_with("blake3:"));
    }

    #[test]
    fn validate_dispositions_rejects_invalid() {
        let fixtures = vec![GenericSkillFixture {
            id: "test".to_string(),
            description: "test".to_string(),
            ee_commands: vec!["ee status --json".to_string()],
            evidence_ids: vec!["ev-test".to_string()],
            redaction_status: "passed".to_string(),
            raw_secrets_included: false,
            prompt_injection_quarantined: true,
            degraded_codes: vec!["degraded-test".to_string()],
            expected_disposition: "invalid_disposition".to_string(),
            first_failure_diagnosis: None,
        }];
        assert_eq!(fixtures[0].description, "test");
        assert_eq!(fixtures[0].ee_commands, ["ee status --json"]);
        assert_eq!(fixtures[0].evidence_ids, ["ev-test"]);
        assert!(!fixtures[0].raw_secrets_included);
        assert!(fixtures[0].prompt_injection_quarantined);
        assert_eq!(fixtures[0].degraded_codes, ["degraded-test"]);
        assert!(validate_fixture_dispositions(&fixtures, "test").is_err());
    }

    #[test]
    fn skill_lint_log_serializes_to_json() {
        let log = SkillLintLog::new("skills/test/SKILL.md").with_section_check("pass");
        let json = log.to_json();
        assert!(json.contains("\"schema\":\"ee.skill_standards.lint_log.v1\""));
        assert!(json.contains("\"required_section_check\":\"pass\""));
    }

    #[test]
    fn skill_lint_log_uses_declared_required_fields() {
        let fixture = GenericSkillFixture {
            id: "fixture-1".to_string(),
            description: "fixture".to_string(),
            ee_commands: Vec::new(),
            evidence_ids: Vec::new(),
            redaction_status: "passed".to_string(),
            raw_secrets_included: false,
            prompt_injection_quarantined: true,
            degraded_codes: vec!["degraded-test".to_string()],
            expected_disposition: "go".to_string(),
            first_failure_diagnosis: None,
        };
        let json = SkillLintLog::new("skills/test/SKILL.md")
            .with_fixture(&fixture)
            .to_json();
        for field in REQUIRED_LOG_FIELDS {
            assert!(json.contains(&format!("\"{field}\":")));
        }
    }
}
