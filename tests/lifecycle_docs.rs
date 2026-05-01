use ee::models::{ERROR_SCHEMA_V1, LifecycleEvent, RESPONSE_SCHEMA_V1, RuleMaturity, TrustClass};

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
