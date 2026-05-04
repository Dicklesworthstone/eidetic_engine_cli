use serde::Deserialize;
use std::collections::BTreeSet;

type TestResult = Result<(), String>;

const SKILL: &str = include_str!("../skills/procedure-distillation/SKILL.md");
const PROCEDURE_DRAFT_TEMPLATE: &str =
    include_str!("../skills/procedure-distillation/references/procedure-draft-template.md");
const VERIFICATION_MATRIX_TEMPLATE: &str =
    include_str!("../skills/procedure-distillation/references/verification-matrix-template.md");
const SKILL_CAPSULE_REVIEW_TEMPLATE: &str =
    include_str!("../skills/procedure-distillation/references/skill-capsule-review-template.md");
const DRIFT_REVIEW_TEMPLATE: &str =
    include_str!("../skills/procedure-distillation/references/drift-review-template.md");
const E2E_FIXTURES: &str =
    include_str!("../skills/procedure-distillation/fixtures/e2e-fixtures.json");
const VALIDATION_SCRIPT: &str = include_str!(
    "../skills/procedure-distillation/scripts/validate_procedure_distillation_skill.py"
);

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

const REQUIRED_TEMPLATE_FIELDS: &[&str] = &[
    "sourceRunIds",
    "evidenceIds",
    "evidenceBundlePath",
    "evidenceBundleHash",
    "redactionStatus",
    "trustClass",
    "extractedFacts",
    "candidateSteps",
    "assumptions",
    "verificationPlan",
    "renderOnlySkillCapsule",
    "unsupportedClaims",
    "degradedState",
    "firstFailureDiagnosis",
];

const REQUIRED_LOG_FIELDS: &[&str] = &[
    "skillPath",
    "fixtureId",
    "fixtureHash",
    "sourceRunIds",
    "evidenceIds",
    "evidenceBundlePath",
    "evidenceBundleHash",
    "verificationStatus",
    "redactionStatus",
    "degradedStatus",
    "degradedCodes",
    "outputArtifactPath",
    "requiredSectionCheck",
    "firstFailureDiagnosis",
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcedureSkillFixtures {
    schema: String,
    skill_path: String,
    fixtures: Vec<ProcedureSkillFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcedureSkillFixture {
    id: String,
    fixture_hash: String,
    description: String,
    ee_commands: Vec<String>,
    source_run_ids: Vec<String>,
    evidence_ids: Vec<String>,
    procedure_ids: Vec<String>,
    evidence_bundle_path: String,
    evidence_bundle_hash: String,
    verification_status: String,
    redaction_status: String,
    raw_secrets_included: bool,
    prompt_injection_quarantined: bool,
    degraded_status: String,
    degraded_codes: Vec<String>,
    output_artifact_path: String,
    required_section_check: String,
    render_only_export_status: String,
    expected_disposition: String,
    first_failure_diagnosis: Option<String>,
}

fn parse_frontmatter(content: &str) -> Result<(&str, &str), String> {
    let after_open = content
        .strip_prefix("---\n")
        .ok_or_else(|| "skill is missing opening YAML frontmatter".to_string())?;
    after_open
        .split_once("\n---\n")
        .ok_or_else(|| "skill frontmatter must close with `---`".to_string())
}

fn frontmatter_value<'a>(frontmatter: &'a str, key: &str) -> Option<&'a str> {
    frontmatter.lines().find_map(|line| {
        let (candidate, value) = line.split_once(':')?;
        (candidate.trim() == key).then_some(value.trim())
    })
}

fn fixtures() -> Result<ProcedureSkillFixtures, String> {
    serde_json::from_str(E2E_FIXTURES)
        .map_err(|error| format!("procedure skill fixtures must parse as JSON: {error}"))
}

fn fixture_hash(fixture: &ProcedureSkillFixture) -> String {
    format!("blake3:{}", blake3::hash(fixture.id.as_bytes()).to_hex())
}

fn assert_contains_all(label: &str, content: &str, required: &[&str]) -> TestResult {
    for item in required {
        if !content.contains(item) {
            return Err(format!("{label} missing required text `{item}`"));
        }
    }
    Ok(())
}

#[test]
fn procedure_distillation_skill_has_required_sections_and_commands() -> TestResult {
    let (frontmatter, body) = parse_frontmatter(SKILL)?;
    if frontmatter_value(frontmatter, "name") != Some("procedure-distillation") {
        return Err("procedure skill frontmatter must name procedure-distillation".to_string());
    }
    let description = frontmatter_value(frontmatter, "description")
        .ok_or_else(|| "procedure skill missing description".to_string())?;
    if !description.contains("Use when") || !description.contains("render-only skill capsules") {
        return Err(
            "description must include trigger language for render-only capsules".to_string(),
        );
    }

    assert_contains_all("procedure skill", body, REQUIRED_SECTIONS)?;
    assert_contains_all(
        "procedure skill",
        body,
        &[
            "ee.status",
            "ee.skill_evidence_bundle.v1",
            "source run ID",
            "evidence ID",
            "ee procedure verify",
            "render-only",
            "direct DB",
            "durable memory mutation",
            "trust class",
            "prompt-injection",
            "Unsupported",
        ],
    )
    .or_else(|_| {
        assert_contains_all(
            "procedure skill",
            body,
            &[
                "ee status",
                "ee.skill_evidence_bundle.v1",
                "source run ID",
                "evidence ID",
                "ee procedure verify",
                "render-only",
                "direct DB",
                "durable memory mutation",
                "trust class",
                "prompt-injection",
                "Unsupported",
            ],
        )
    })?;

    for command in [
        "ee status --workspace <workspace> --json",
        "ee procedure propose --title <title> --source-run <run-id> --evidence <evidence-id> --dry-run --json",
        "ee procedure show <procedure-id> --include-verification --workspace <workspace> --json",
        "ee procedure verify <procedure-id> --source-kind eval_fixture --source <fixture-id> --dry-run --json",
        "ee procedure export <procedure-id> --export-format skill-capsule --workspace <workspace> --json",
        "ee procedure drift <procedure-id> --workspace <workspace> --json",
    ] {
        if !body.contains(command) {
            return Err(format!("procedure skill missing command `{command}`"));
        }
    }

    Ok(())
}

#[test]
fn procedure_distillation_requires_source_evidence_before_drafting() -> TestResult {
    assert_contains_all(
        "procedure skill source gate",
        SKILL,
        &[
            "Require at least one source recorder run ID or evidence ID before drafting",
            "procedure_source_evidence_missing",
            "source run IDs and evidence IDs are both empty",
            "Do not use it for ordinary documentation",
        ],
    )?;

    let fixtures = fixtures()?;
    let insufficient = fixtures
        .fixtures
        .iter()
        .find(|fixture| fixture.id == "pd_insufficient_evidence")
        .ok_or_else(|| "missing insufficient evidence fixture".to_string())?;
    if !insufficient.source_run_ids.is_empty() || !insufficient.evidence_ids.is_empty() {
        return Err("insufficient evidence fixture must omit source IDs".to_string());
    }
    if insufficient.expected_disposition != "refuse" {
        return Err("insufficient evidence fixture must refuse drafting".to_string());
    }
    if insufficient.first_failure_diagnosis.is_none() {
        return Err("insufficient evidence fixture must record first failure".to_string());
    }

    Ok(())
}

#[test]
fn procedure_distillation_output_separates_facts_steps_assumptions_and_capsule() -> TestResult {
    assert_contains_all(
        "procedure skill output template",
        SKILL,
        &[
            "extractedFacts",
            "candidateSteps",
            "assumptions",
            "verificationPlan",
            "renderOnlySkillCapsule",
            "unsupportedClaims",
            "recommendedExplicitCommands",
        ],
    )?;
    assert_contains_all(
        "procedure draft template",
        PROCEDURE_DRAFT_TEMPLATE,
        REQUIRED_TEMPLATE_FIELDS,
    )?;

    let facts_index = SKILL
        .find("extractedFacts")
        .ok_or_else(|| "missing extractedFacts section".to_string())?;
    let steps_index = SKILL
        .find("candidateSteps")
        .ok_or_else(|| "missing candidateSteps section".to_string())?;
    let assumptions_index = SKILL
        .find("assumptions")
        .ok_or_else(|| "missing assumptions section".to_string())?;
    if !(facts_index < steps_index && steps_index < assumptions_index) {
        return Err("output template must order facts before steps before assumptions".to_string());
    }

    Ok(())
}

#[test]
fn procedure_distillation_refuses_promotion_without_verification() -> TestResult {
    assert_contains_all(
        "procedure skill promotion gate",
        SKILL,
        &[
            "Promotion requires",
            "ee procedure verify",
            "Without verification, refuse promotion",
            "procedure_verification_missing",
            "safe to promote",
        ],
    )?;
    assert_contains_all(
        "verification matrix template",
        VERIFICATION_MATRIX_TEMPLATE,
        &[
            "verificationStatus",
            "requiredVerificationCommand",
            "promotionDecision",
            "allowed: false",
            "promotion requires passed ee procedure verify",
        ],
    )?;

    let fixtures = fixtures()?;
    let draft_only = fixtures
        .fixtures
        .iter()
        .find(|fixture| fixture.id == "pd_recorder_derived_draft")
        .ok_or_else(|| "missing recorder-derived draft fixture".to_string())?;
    if draft_only.verification_status != "missing"
        || draft_only.expected_disposition != "draft_only"
    {
        return Err("recorder-derived draft must stay draft-only before verification".to_string());
    }
    let failed = fixtures
        .fixtures
        .iter()
        .find(|fixture| fixture.id == "pd_failed_verification")
        .ok_or_else(|| "missing failed verification fixture".to_string())?;
    if failed.verification_status != "failed" || failed.expected_disposition != "refuse_promotion" {
        return Err("failed verification fixture must refuse promotion".to_string());
    }

    Ok(())
}

#[test]
fn procedure_distillation_templates_cover_required_review_artifacts() -> TestResult {
    let templates = [
        ("procedure draft", PROCEDURE_DRAFT_TEMPLATE),
        ("verification matrix", VERIFICATION_MATRIX_TEMPLATE),
        ("skill capsule review", SKILL_CAPSULE_REVIEW_TEMPLATE),
        ("drift review", DRIFT_REVIEW_TEMPLATE),
    ];
    for (label, template) in templates {
        assert_contains_all(
            label,
            template,
            &["schema:", "source", "evidence", "redaction", "degraded"],
        )?;
    }
    assert_contains_all(
        "skill capsule review template",
        SKILL_CAPSULE_REVIEW_TEMPLATE,
        &[
            "installMode: render_only",
            "noAutomaticInstallationLanguage",
            "ready_for_manual_copy",
            "no automatic installation",
        ],
    )?;
    assert_contains_all(
        "drift review template",
        DRIFT_REVIEW_TEMPLATE,
        &[
            "driftSignals",
            "verificationStatus",
            "ee procedure drift",
            "Do not retire, promote, or mutate",
        ],
    )?;

    Ok(())
}

#[test]
fn procedure_distillation_fixture_matrix_covers_e2e_logs() -> TestResult {
    let fixtures = fixtures()?;
    if fixtures.schema != "ee.skill.procedure_distillation.fixtures.v1" {
        return Err("procedure fixture schema mismatch".to_string());
    }
    if fixtures.skill_path != "skills/procedure-distillation/SKILL.md" {
        return Err("procedure fixture skill path mismatch".to_string());
    }

    let expected_ids = BTreeSet::from([
        "pd_insufficient_evidence",
        "pd_recorder_derived_draft",
        "pd_failed_verification",
        "pd_render_only_export_logging",
        "pd_redacted_source_evidence",
        "pd_degraded_procedure_output",
    ]);
    let actual_ids = fixtures
        .fixtures
        .iter()
        .map(|fixture| fixture.id.as_str())
        .collect::<BTreeSet<_>>();
    if actual_ids != expected_ids {
        return Err(format!(
            "procedure fixture IDs mismatch: expected {expected_ids:?}, got {actual_ids:?}"
        ));
    }

    for fixture in &fixtures.fixtures {
        validate_fixture_log_shape(fixture)?;
    }

    Ok(())
}

#[test]
fn procedure_distillation_has_e2e_validation_script() -> TestResult {
    assert_contains_all(
        "procedure validation script",
        VALIDATION_SCRIPT,
        &[
            "ee.skill.procedure_distillation.e2e_log.v1",
            "ee.skill.procedure_distillation.fixtures.v1",
            "pd_insufficient_evidence",
            "pd_render_only_export_logging",
            "procedure_store_unavailable",
            "renderOnlyExportCheck",
        ],
    )?;
    assert_contains_all(
        "procedure skill validation script reference",
        SKILL,
        &["scripts/validate_procedure_distillation_skill.py"],
    )
}

fn validate_fixture_log_shape(fixture: &ProcedureSkillFixture) -> TestResult {
    if fixture.description.trim().is_empty() {
        return Err(format!("{} missing description", fixture.id));
    }
    if !fixture
        .ee_commands
        .iter()
        .all(|command| command.starts_with("ee ") && command.contains("--json"))
    {
        return Err(format!("{} has non-machine ee command", fixture.id));
    }
    if !fixture
        .evidence_bundle_path
        .starts_with("target/e2e/skills/")
    {
        return Err(format!(
            "{} evidence bundle path outside e2e tree",
            fixture.id
        ));
    }
    if !fixture.evidence_bundle_hash.starts_with("blake3:") {
        return Err(format!(
            "{} evidence bundle hash missing blake3",
            fixture.id
        ));
    }
    if !fixture
        .output_artifact_path
        .starts_with("target/e2e/skills/")
    {
        return Err(format!(
            "{} output artifact path outside e2e tree",
            fixture.id
        ));
    }
    if fixture.id != "pd_insufficient_evidence" && fixture.procedure_ids.is_empty() {
        return Err(format!(
            "{} must log procedure IDs once a procedure surface is involved",
            fixture.id
        ));
    }
    if fixture.required_section_check != "passed" {
        return Err(format!("{} must log required-section check", fixture.id));
    }
    if fixture.raw_secrets_included {
        return Err(format!(
            "{} fixture must not include raw secrets",
            fixture.id
        ));
    }
    if fixture.redaction_status == "passed" && !fixture.prompt_injection_quarantined {
        return Err(format!(
            "{} must log prompt-injection quarantine for accepted redacted evidence",
            fixture.id
        ));
    }
    if matches!(
        fixture.verification_status.as_str(),
        "failed" | "degraded" | "missing"
    ) && fixture.first_failure_diagnosis.is_none()
        && fixture.expected_disposition != "draft_only"
    {
        return Err(format!(
            "{} must record first failure diagnosis for blocked verification",
            fixture.id
        ));
    }
    if fixture.render_only_export_status == "render_only"
        && fixture.expected_disposition != "ready_for_manual_review"
    {
        return Err(format!(
            "{} render-only export should end at manual review",
            fixture.id
        ));
    }
    let log_fields = REQUIRED_LOG_FIELDS.iter().copied().collect::<BTreeSet<_>>();
    let present_fields = BTreeSet::from([
        "skillPath",
        "fixtureId",
        "fixtureHash",
        "sourceRunIds",
        "evidenceIds",
        "evidenceBundlePath",
        "evidenceBundleHash",
        "verificationStatus",
        "redactionStatus",
        "degradedStatus",
        "degradedCodes",
        "outputArtifactPath",
        "requiredSectionCheck",
        "firstFailureDiagnosis",
    ]);
    if present_fields != log_fields {
        return Err(format!("{} missing log field coverage", fixture.id));
    }
    if !fixture_hash(fixture).starts_with("blake3:") {
        return Err(format!(
            "{} fixture hash helper must include blake3",
            fixture.id
        ));
    }
    if !fixture.fixture_hash.starts_with("blake3:") {
        return Err(format!("{} must log fixtureHash with blake3", fixture.id));
    }
    if fixture.degraded_status == "none" && !fixture.degraded_codes.is_empty() {
        return Err(format!(
            "{} cannot have degraded codes when degradedStatus is none",
            fixture.id
        ));
    }
    if fixture.degraded_status != "none" && fixture.degraded_codes.is_empty() {
        return Err(format!(
            "{} must name degraded codes when degradedStatus is not none",
            fixture.id
        ));
    }
    Ok(())
}

#[test]
fn procedure_distillation_render_only_language_blocks_auto_install() -> TestResult {
    assert_contains_all(
        "procedure skill render-only wording",
        SKILL,
        &[
            "render-only",
            "procedure_render_only_export_required",
            "automatic skill installation",
            "automatic installation",
            "no files are installed",
        ],
    )?;
    assert_contains_all(
        "skill capsule review template",
        SKILL_CAPSULE_REVIEW_TEMPLATE,
        &[
            "installMode: render_only",
            "no automatic installation",
            "no file copying",
            "no mutation",
        ],
    )?;

    let fixtures = fixtures()?;
    let export = fixtures
        .fixtures
        .iter()
        .find(|fixture| fixture.id == "pd_render_only_export_logging")
        .ok_or_else(|| "missing render-only export fixture".to_string())?;
    if export.render_only_export_status != "render_only" {
        return Err("render-only export fixture must log render_only status".to_string());
    }
    if export.verification_status != "passed" {
        return Err("render-only export fixture must be backed by passed verification".to_string());
    }

    Ok(())
}
