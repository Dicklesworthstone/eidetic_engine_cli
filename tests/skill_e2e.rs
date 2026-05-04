//! End-to-end tests for all project-local skills.
//!
//! This file tests that each skill has required SKILL.md structure, valid fixtures,
//! proper degradation handling, redaction awareness, and output contracts.
//!
//! Acceptance criteria from eidetic_engine_cli-s38h:
//! - Every project-local skill has a fixture or e2e contract
//! - Skill tests consume the shared fixture corpus
//! - Harness tests cover SKILL.md section validation, trigger metadata, etc.
//! - Tests verify skills reference ee evidence command outputs
//! - Tests fail if a skill asks for unsupported hidden chain-of-thought

mod skills;

use skills::{
    GenericSkillFixtures, SkillLintLog, TestResult, assert_contains_all,
    assert_has_required_sections, parse_fixtures, parse_skill, validate_fixture_dispositions,
    validate_no_hidden_cot, validate_redaction_states, validate_refusal_fixtures,
};

const REHEARSAL_SKILL: &str = include_str!("../skills/rehearsal-promotion-review/SKILL.md");
const REHEARSAL_FIXTURES: &str =
    include_str!("../skills/rehearsal-promotion-review/fixtures/e2e-fixtures.json");

const PREFLIGHT_SKILL: &str = include_str!("../skills/preflight-risk-review/SKILL.md");
const PREFLIGHT_FIXTURES: &str =
    include_str!("../skills/preflight-risk-review/fixtures/e2e-fixtures.json");

const SITUATION_SKILL: &str = include_str!("../skills/situation-framing/SKILL.md");
const SITUATION_FIXTURES: &str =
    include_str!("../skills/situation-framing/fixtures/e2e-fixtures.json");

const PROCEDURE_DISTILLATION_SKILL: &str =
    include_str!("../skills/procedure-distillation/SKILL.md");
const PROCEDURE_DISTILLATION_FIXTURES: &str =
    include_str!("../skills/procedure-distillation/fixtures/e2e-fixtures.json");

const CAUSAL_CREDIT_SKILL: &str = include_str!("../skills/causal-credit-review/SKILL.md");
const CAUSAL_CREDIT_FIXTURES: &str =
    include_str!("../skills/causal-credit-review/fixtures/e2e-fixtures.json");

const COUNTERFACTUAL_SKILL: &str =
    include_str!("../skills/counterfactual-failure-analysis/SKILL.md");
const COUNTERFACTUAL_FIXTURES: &str =
    include_str!("../skills/counterfactual-failure-analysis/fixtures/e2e-fixtures.json");

const ACTIVE_LEARNING_SKILL: &str =
    include_str!("../skills/active-learning-experiment-planner/SKILL.md");
const ACTIVE_LEARNING_FIXTURES: &str =
    include_str!("../skills/active-learning-experiment-planner/fixtures/e2e-fixtures.json");

const SESSION_REVIEW_SKILL: &str =
    include_str!("../skills/session-review-memory-distillation/SKILL.md");
const SESSION_REVIEW_FIXTURES: &str =
    include_str!("../skills/session-review-memory-distillation/fixtures/e2e-fixtures.json");

const CLAIM_CERT_SKILL: &str = include_str!("../skills/claim-certificate-review/SKILL.md");
const CLAIM_CERT_FIXTURES: &str =
    include_str!("../skills/claim-certificate-review/fixtures/e2e-fixtures.json");

const EE_SKILL_STANDARDS: &str = include_str!("../skills/ee-skill-standards/SKILL.md");

const ALL_SKILLS: &[(&str, &str, Option<&str>)] = &[
    (
        "rehearsal-promotion-review",
        REHEARSAL_SKILL,
        Some(REHEARSAL_FIXTURES),
    ),
    (
        "preflight-risk-review",
        PREFLIGHT_SKILL,
        Some(PREFLIGHT_FIXTURES),
    ),
    (
        "situation-framing",
        SITUATION_SKILL,
        Some(SITUATION_FIXTURES),
    ),
    (
        "procedure-distillation",
        PROCEDURE_DISTILLATION_SKILL,
        Some(PROCEDURE_DISTILLATION_FIXTURES),
    ),
    (
        "causal-credit-review",
        CAUSAL_CREDIT_SKILL,
        Some(CAUSAL_CREDIT_FIXTURES),
    ),
    (
        "counterfactual-failure-analysis",
        COUNTERFACTUAL_SKILL,
        Some(COUNTERFACTUAL_FIXTURES),
    ),
    (
        "active-learning-experiment-planner",
        ACTIVE_LEARNING_SKILL,
        Some(ACTIVE_LEARNING_FIXTURES),
    ),
    (
        "session-review-memory-distillation",
        SESSION_REVIEW_SKILL,
        Some(SESSION_REVIEW_FIXTURES),
    ),
    (
        "claim-certificate-review",
        CLAIM_CERT_SKILL,
        Some(CLAIM_CERT_FIXTURES),
    ),
];

#[test]
fn all_skills_have_valid_frontmatter() -> TestResult {
    for (name, content, _) in ALL_SKILLS {
        let parsed = parse_skill(content).map_err(|err| format!("{name} skill: {err}"))?;
        if parsed.frontmatter.name != *name {
            return Err(format!(
                "{name} skill frontmatter name mismatch: expected {name}, got {}",
                parsed.frontmatter.name
            ));
        }
        if parsed.frontmatter.description.is_empty() {
            return Err(format!("{name} skill has empty description"));
        }
    }
    Ok(())
}

#[test]
fn all_skills_have_required_sections() -> TestResult {
    for (name, content, _) in ALL_SKILLS {
        let parsed = parse_skill(content).map_err(|err| format!("{name} skill: {err}"))?;
        assert_has_required_sections(&parsed.body, name)?;
    }
    Ok(())
}

#[test]
fn all_skills_reference_ee_evidence_commands() -> TestResult {
    let required_patterns = ["ee status", "ee.response.v1", "--json"];
    for (name, content, _) in ALL_SKILLS {
        for pattern in required_patterns {
            if !content.contains(pattern) {
                return Err(format!(
                    "{name} skill must reference `{pattern}` for evidence commands"
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn all_skills_forbid_hidden_cot_and_direct_db() -> TestResult {
    for (name, content, _) in ALL_SKILLS {
        validate_no_hidden_cot(content, name)?;
    }
    Ok(())
}

#[test]
fn all_skill_fixtures_have_valid_dispositions() -> TestResult {
    for (name, _, fixtures_opt) in ALL_SKILLS {
        if let Some(fixtures_json) = fixtures_opt {
            let fixtures: GenericSkillFixtures = parse_fixtures(fixtures_json, name)?;
            validate_fixture_dispositions(&fixtures.fixtures, name)?;
        }
    }
    Ok(())
}

#[test]
fn all_skill_fixtures_have_valid_redaction_states() -> TestResult {
    for (name, _, fixtures_opt) in ALL_SKILLS {
        if let Some(fixtures_json) = fixtures_opt {
            let fixtures: GenericSkillFixtures = parse_fixtures(fixtures_json, name)?;
            validate_redaction_states(&fixtures.fixtures, name)?;
        }
    }
    Ok(())
}

#[test]
fn all_skill_refusal_fixtures_have_diagnosis() -> TestResult {
    for (name, _, fixtures_opt) in ALL_SKILLS {
        if let Some(fixtures_json) = fixtures_opt {
            let fixtures: GenericSkillFixtures = parse_fixtures(fixtures_json, name)?;
            validate_refusal_fixtures(&fixtures.fixtures, name)?;
        }
    }
    Ok(())
}

#[test]
fn rehearsal_skill_has_required_rehearsal_commands() -> TestResult {
    let parsed = parse_skill(REHEARSAL_SKILL)?;
    assert_contains_all(
        "rehearsal skill",
        &parsed.body,
        &[
            "ee rehearse plan",
            "ee rehearse run",
            "ee rehearse inspect",
            "ee rehearse promote",
            "rehearsal_unavailable",
            "dry_run",
            "destructive",
        ],
    )
}

#[test]
fn rehearsal_skill_never_authorizes_destructive_commands() -> TestResult {
    assert_contains_all(
        "rehearsal skill destructive gate",
        REHEARSAL_SKILL,
        &[
            "never authorizes destructive commands",
            "needs-escalation",
            "requires user approval",
            "Destructive Command Escalation",
        ],
    )
}

#[test]
fn rehearsal_fixtures_cover_required_scenarios() -> TestResult {
    let fixtures: GenericSkillFixtures = parse_fixtures(REHEARSAL_FIXTURES, "rehearsal")?;
    let fixture_ids: Vec<&str> = fixtures.fixtures.iter().map(|f| f.id.as_str()).collect();

    let required_scenarios = [
        "successful_dry_run",
        "partial_failure",
        "unavailable",
        "unsupported",
        "redacted",
        "malformed",
        "degraded",
        "destructive",
    ];

    for scenario in required_scenarios {
        let found = fixture_ids.iter().any(|id| id.contains(scenario));
        if !found {
            return Err(format!(
                "rehearsal fixtures missing scenario covering `{scenario}`"
            ));
        }
    }
    Ok(())
}

#[test]
fn preflight_skill_has_tripwire_and_risk_commands() -> TestResult {
    let parsed = parse_skill(PREFLIGHT_SKILL)?;
    assert_contains_all(
        "preflight skill",
        &parsed.body,
        &[
            "ee status",
            "ee preflight",
            "tripwire",
            "risk",
            "go/no-go",
            "ask_user",
        ],
    )
}

#[test]
fn preflight_fixtures_cover_required_scenarios() -> TestResult {
    let fixtures: GenericSkillFixtures = parse_fixtures(PREFLIGHT_FIXTURES, "preflight")?;
    let fixture_ids: Vec<&str> = fixtures.fixtures.iter().map(|f| f.id.as_str()).collect();

    let required_scenarios = [
        "low_risk",
        "destructive",
        "migration",
        "no_evidence",
        "redacted",
        "degraded",
    ];

    for scenario in required_scenarios {
        let found = fixture_ids.iter().any(|id| id.contains(scenario));
        if !found {
            return Err(format!(
                "preflight fixtures missing scenario covering `{scenario}`"
            ));
        }
    }
    Ok(())
}

#[test]
fn situation_skill_has_framing_and_planning_sections() -> TestResult {
    let parsed = parse_skill(SITUATION_SKILL)?;
    assert_contains_all(
        "situation skill",
        &parsed.body,
        &["situation", "task", "context", "ee status", "ee context"],
    )
}

#[test]
fn situation_fixtures_cover_required_scenarios() -> TestResult {
    let fixtures: GenericSkillFixtures = parse_fixtures(SITUATION_FIXTURES, "situation")?;
    let fixture_ids: Vec<&str> = fixtures.fixtures.iter().map(|f| f.id.as_str()).collect();

    let required_scenarios = [
        "bug_fix",
        "feature",
        "refactor",
        "investigate",
        "docs",
        "deploy",
        "ambiguous",
        "missing_evidence",
        "degraded",
    ];

    for scenario in required_scenarios {
        let found = fixture_ids.iter().any(|id| id.contains(scenario));
        if !found {
            return Err(format!(
                "situation fixtures missing scenario covering `{scenario}`"
            ));
        }
    }
    Ok(())
}

#[test]
fn causal_credit_skill_references_causal_evidence() -> TestResult {
    let parsed = parse_skill(CAUSAL_CREDIT_SKILL)?;
    assert_contains_all(
        "causal-credit skill",
        &parsed.body,
        &["causal", "credit", "ee why", "ee search", "evidence"],
    )
}

#[test]
fn counterfactual_skill_references_failure_analysis() -> TestResult {
    let parsed = parse_skill(COUNTERFACTUAL_SKILL)?;
    assert_contains_all(
        "counterfactual skill",
        &parsed.body,
        &["counterfactual", "failure", "alternative", "ee why"],
    )
}

#[test]
fn active_learning_skill_references_experiments() -> TestResult {
    let parsed = parse_skill(ACTIVE_LEARNING_SKILL)?;
    assert_contains_all(
        "active-learning skill",
        &parsed.body,
        &["experiment", "hypothesis", "uncertainty", "ee lab"],
    )
}

#[test]
fn session_review_skill_references_distillation() -> TestResult {
    let parsed = parse_skill(SESSION_REVIEW_SKILL)?;
    assert_contains_all(
        "session-review skill",
        &parsed.body,
        &["session", "memory", "distill", "candidate", "ee search"],
    )
}

#[test]
fn claim_certificate_skill_references_verification() -> TestResult {
    let parsed = parse_skill(CLAIM_CERT_SKILL)?;
    assert_contains_all(
        "claim-certificate skill",
        &parsed.body,
        &[
            "claim",
            "certificate",
            "verify",
            "manifest",
            "hash",
            "ee claim",
        ],
    )
}

#[test]
fn claim_certificate_fixtures_cover_required_scenarios() -> TestResult {
    let fixtures: GenericSkillFixtures = parse_fixtures(CLAIM_CERT_FIXTURES, "claim-certificate")?;
    let fixture_ids: Vec<&str> = fixtures.fixtures.iter().map(|f| f.id.as_str()).collect();

    let required_scenarios = [
        "verified", "stale", "expired", "missing", "failed", "redacted", "degraded",
    ];

    for scenario in required_scenarios {
        let found = fixture_ids.iter().any(|id| id.contains(scenario));
        if !found {
            return Err(format!(
                "claim-certificate fixtures missing scenario covering `{scenario}`"
            ));
        }
    }
    Ok(())
}

#[test]
fn ee_skill_standards_defines_mechanical_boundary() -> TestResult {
    assert_contains_all(
        "ee-skill-standards",
        EE_SKILL_STANDARDS,
        &[
            "Mechanical Command Boundary",
            "ee.skill_evidence_bundle.v1",
            "stdout",
            "stderr",
            "redaction",
            "degraded",
            "Unsupported Claims",
        ],
    )
}

#[test]
fn skill_lint_log_schema_is_consistent() {
    let log = SkillLintLog::new("skills/test/SKILL.md");
    let json = log.to_json();
    assert!(json.contains("\"schema\":\"ee.skill_standards.lint_log.v1\""));
}

#[test]
fn all_skill_fixtures_have_schema_field() -> TestResult {
    for (name, _, fixtures_opt) in ALL_SKILLS {
        if let Some(fixtures_json) = fixtures_opt {
            let fixtures: GenericSkillFixtures = parse_fixtures(fixtures_json, name)?;
            if fixtures.schema.is_empty() {
                return Err(format!("{name} fixtures missing schema field"));
            }
            if !fixtures.schema.starts_with("ee.skill.") {
                return Err(format!(
                    "{name} fixtures schema should start with ee.skill., got {}",
                    fixtures.schema
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn all_skill_fixtures_have_valid_skill_path_if_present() -> TestResult {
    for (name, _, fixtures_opt) in ALL_SKILLS {
        if let Some(fixtures_json) = fixtures_opt {
            let fixtures: GenericSkillFixtures = parse_fixtures(fixtures_json, name)?;
            if !fixtures.skill_path.is_empty() && !fixtures.skill_path.contains("SKILL.md") {
                return Err(format!(
                    "{name} fixtures skill_path should reference SKILL.md, got {}",
                    fixtures.skill_path
                ));
            }
        }
    }
    Ok(())
}
