use ee::models::trust::{LOCAL_SIGNING_KEY_POLICY_SCHEMA_V1, LocalSigningKeyPosture};
use ee::models::{
    ContextProfileName, ERROR_SCHEMA_V1, LifecycleEvent, RESPONSE_SCHEMA_V1, RuleLifecycleAction,
    RuleLifecycleTrigger, RuleMaturity, TrustClass,
};

type TestResult = Result<(), String>;

const README: &str = include_str!("../README.md");
const TRUST_MODEL: &str = include_str!("../docs/trust-model.md");

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{context}: expected to find `{needle}`"))
    }
}

fn initial_confidence_wire(class: TrustClass) -> &'static str {
    match class {
        TrustClass::HumanExplicit => "0.85",
        TrustClass::AgentValidated => "0.65",
        TrustClass::AgentAssertion => "0.50",
        TrustClass::CassEvidence => "0.45",
        TrustClass::LegacyImport => "0.30",
    }
}

#[test]
fn readme_links_trust_model_guide() -> TestResult {
    ensure_contains(
        README,
        "[`docs/trust-model.md`](docs/trust-model.md)",
        "README documentation index",
    )?;
    ensure_contains(
        README,
        "Lifecycle rules, advisory priority, and prompt-injection handling",
        "README trust section",
    )
}

#[test]
fn readme_context_profiles_match_shipped_cli_profiles() -> TestResult {
    let expected_profiles = [
        (ContextProfileName::Compact, "`compact`"),
        (ContextProfileName::Balanced, "`balanced`"),
        (ContextProfileName::Thorough, "`thorough`"),
        (ContextProfileName::Submodular, "`submodular`"),
    ];
    if expected_profiles.map(|(profile, _documented_name)| profile) != ContextProfileName::all() {
        return Err("README context profile test must enumerate every shipped profile".to_string());
    }
    for (_profile, documented_name) in expected_profiles {
        ensure_contains(README, documented_name, "README context profile coverage")?;
    }
    let stale_examples = [
        "--profile release",
        "--profile refactor",
        "--profile debug",
        "--profile security",
        "--profile performance",
        "default_profile  = \"default\"",
        "~/.config/ee/profiles/",
        "extends = \"release\"",
    ];
    if let Some(stale_example) = stale_examples
        .iter()
        .find(|stale_example| README.contains(**stale_example))
    {
        return Err(format!(
            "README context profile examples mention unsupported profile `{stale_example}`"
        ));
    }
    ensure_contains(
        README,
        "default_profile  = \"balanced\"",
        "README config default profile",
    )
}

#[test]
fn trust_model_mentions_every_trust_class_and_confidence() -> TestResult {
    for class in TrustClass::all() {
        ensure_contains(TRUST_MODEL, class.as_str(), "trust model class coverage")?;
        ensure_contains(
            TRUST_MODEL,
            initial_confidence_wire(class),
            "trust model confidence coverage",
        )?;
    }
    ensure_contains(TRUST_MODEL, "ADR 0009", "canonical trust ADR")
}

#[test]
fn trust_model_mentions_rule_maturity_states() -> TestResult {
    for maturity in RuleMaturity::all() {
        ensure_contains(
            TRUST_MODEL,
            maturity.as_str(),
            "trust model rule maturity coverage",
        )?;
    }
    ensure_contains(
        TRUST_MODEL,
        "promotion, demotion, quarantine",
        "curation lifecycle coverage",
    )
}

#[test]
fn trust_model_mentions_rule_lifecycle_triggers_and_actions() -> TestResult {
    for trigger in RuleLifecycleTrigger::all() {
        ensure_contains(
            TRUST_MODEL,
            trigger.as_str(),
            "trust model rule lifecycle trigger coverage",
        )?;
    }
    for action in RuleLifecycleAction::all() {
        ensure_contains(
            TRUST_MODEL,
            action.as_str(),
            "trust model rule lifecycle action coverage",
        )?;
    }
    for phrase in [
        "candidate-to-validated",
        "explicit review",
        "no silent promotion",
    ] {
        ensure_contains(TRUST_MODEL, phrase, "rule lifecycle safety coverage")?;
    }
    Ok(())
}

#[test]
fn trust_model_mentions_local_signing_key_policy() -> TestResult {
    ensure_contains(
        TRUST_MODEL,
        LOCAL_SIGNING_KEY_POLICY_SCHEMA_V1,
        "local signing policy schema coverage",
    )?;
    for posture in LocalSigningKeyPosture::all() {
        ensure_contains(
            TRUST_MODEL,
            posture.as_str(),
            "local signing policy posture coverage",
        )?;
    }
    for code in [
        "local_signing_key_required",
        "local_signing_key_recommended",
        "local_signing_key_not_required",
        "local_signing_key_satisfied",
    ] {
        ensure_contains(TRUST_MODEL, code, "local signing policy code coverage")?;
    }
    for phrase in [
        "does not generate keys",
        "mutate memories",
        "silently promote",
        "trust; it only reports",
        "stays out of authoritative",
        "procedural sections until signed",
    ] {
        ensure_contains(TRUST_MODEL, phrase, "local signing policy safety coverage")?;
    }
    Ok(())
}

#[test]
fn trust_model_mentions_lifecycle_events_and_machine_schemas() -> TestResult {
    for event in LifecycleEvent::all() {
        ensure_contains(
            TRUST_MODEL,
            event.as_str(),
            "trust model lifecycle event coverage",
        )?;
    }
    ensure_contains(TRUST_MODEL, RESPONSE_SCHEMA_V1, "response schema coverage")?;
    ensure_contains(TRUST_MODEL, ERROR_SCHEMA_V1, "error schema coverage")?;
    ensure_contains(TRUST_MODEL, "ee.memory.v1", "memory schema coverage")
}

#[test]
fn trust_model_mentions_agent_facing_commands_and_safety_contract() -> TestResult {
    for command in [
        "ee context",
        "ee why",
        "ee outcome",
        "ee curate candidates",
        "ee import cass",
        "ee import eidetic-legacy",
    ] {
        ensure_contains(TRUST_MODEL, command, "command integration coverage")?;
    }
    for phrase in [
        "No lifecycle step silently upgrades",
        "Blocked content must not enter a context pack",
        "Imported sessions and legacy artifacts",
    ] {
        ensure_contains(TRUST_MODEL, phrase, "safety contract coverage")?;
    }
    Ok(())
}
