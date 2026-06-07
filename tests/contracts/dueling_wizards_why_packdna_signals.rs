//! bd-1n0np.23.5 - additive why / Pack DNA signal contract.
//!
//! This test pins the explanation signal obligations discovered by the
//! dueling-wizards cross-cutting review. It does not claim the runtime fields
//! are implemented; it prevents future implementation slices from omitting the
//! shared signal vocabulary, schema anchors, redaction posture, and proof rule.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const MANIFEST_REL: &str = "tests/fixtures/contracts/dueling_wizards_why_packdna_signals.json";
const DOC_REL: &str = "docs/agent-ux/dueling-wizards-why-packdna-signals.md";
const WHY_SCHEMA_REL: &str = "docs/schemas/ee.why.v1.json";
const PACK_DNA_SCHEMA_REL: &str = "docs/schemas/ee.context.pack_dna.v1.json";
const CAUSAL_WHY_SCHEMA_REL: &str = "docs/schemas/ee.why.causal.v1.json";
const WHY_SOURCE_REL: &str = "src/core/why.rs";
const PACK_DNA_SOURCE_REL: &str = "src/graph/pack_dna.rs";

const REQUIRED_SIGNALS: &[&str] = &[
    "freshness_symbol_drift",
    "contradiction_suppressed",
    "sentinel_state",
    "task_lens",
    "anchor_file_line_provenance",
    "causal_ancestry_path",
];

const SIGNAL_AGENT_CONTRACTS: &[(&str, &str, &str)] = &[
    (
        "freshness_symbol_drift",
        "Did source movement make this memory stale?",
        "rank_down_or_reverify_stale_memory",
    ),
    (
        "contradiction_suppressed",
        "Was this memory suppressed by stronger contradictory evidence?",
        "inspect_winning_evidence_before_reusing_memory",
    ),
    (
        "sentinel_state",
        "Did the latest sentinel check pass for this memory?",
        "trust_verified_memory_or_schedule_check",
    ),
    (
        "task_lens",
        "Which task lens shaped this pack selection?",
        "explain_pack_lens_and_compare_alternatives",
    ),
    (
        "anchor_file_line_provenance",
        "Which source anchor connected this memory to code?",
        "jump_to_redacted_source_anchor",
    ),
    (
        "causal_ancestry_path",
        "Which causal path made this memory relevant?",
        "inspect_causal_path_before_accepting_relevance",
    ),
];

const REQUIRED_SIGNAL_MUST_CLAUSES: u64 = 12;
const MIN_MUST_COVERAGE_MILLI: u64 = 950;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_text(rel: &str) -> Result<String, String> {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).map_err(|error| format!("read {rel}: {error}"))
}

fn read_json(rel: &str) -> Result<Value, String> {
    let text = read_text(rel)?;
    serde_json::from_str(&text).map_err(|error| format!("parse {rel}: {error}"))
}

fn string_field<'a>(value: &'a Value, pointer: &str, context: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context}: missing string field {pointer}"))
}

fn bool_field(value: &Value, pointer: &str, context: &str) -> Result<bool, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{context}: missing bool field {pointer}"))
}

fn u64_field(value: &Value, pointer: &str, context: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{context}: missing integer field {pointer}"))
}

fn array_field<'a>(
    value: &'a Value,
    pointer: &str,
    context: &str,
) -> Result<&'a Vec<Value>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{context}: missing array field {pointer}"))
}

fn string_set(values: &[Value], context: &str) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let text = value
            .as_str()
            .ok_or_else(|| format!("{context}[{index}] must be a string"))?;
        if text.trim().is_empty() {
            return Err(format!("{context}[{index}] must not be empty"));
        }
        out.insert(text.to_owned());
    }
    Ok(out)
}

fn required_signal_set() -> BTreeSet<String> {
    REQUIRED_SIGNALS
        .iter()
        .map(|signal| (*signal).to_owned())
        .collect()
}

#[test]
fn why_packdna_manifest_identity_and_policy_are_stable() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    if string_field(&manifest, "/schema", MANIFEST_REL)?
        != "ee.dueling_wizards.why_packdna_signals.v1"
    {
        return Err(format!(
            "{MANIFEST_REL}: schema must be ee.dueling_wizards.why_packdna_signals.v1"
        ));
    }
    if string_field(&manifest, "/initiativeBead", MANIFEST_REL)? != "bd-1n0np" {
        return Err("manifest must identify initiativeBead bd-1n0np".to_owned());
    }
    if string_field(&manifest, "/gateBead", MANIFEST_REL)? != "bd-1n0np.23.5" {
        return Err("manifest must identify gateBead bd-1n0np.23.5".to_owned());
    }
    for (pointer, expected) in [
        ("/doc", DOC_REL),
        ("/whySchema", WHY_SCHEMA_REL),
        ("/packDnaSchema", PACK_DNA_SCHEMA_REL),
        ("/causalWhySchema", CAUSAL_WHY_SCHEMA_REL),
        ("/whySource", WHY_SOURCE_REL),
        ("/packDnaSource", PACK_DNA_SOURCE_REL),
    ] {
        if string_field(&manifest, pointer, MANIFEST_REL)? != expected {
            return Err(format!("{pointer} must point at {expected}"));
        }
    }
    if string_field(&manifest, "/implementationState", MANIFEST_REL)? != "planned_contract" {
        return Err("implementationState must stay planned_contract".to_owned());
    }
    for (pointer, expected) in [
        ("/policy/whyEnvelopeCompatibility", "stable_additive"),
        ("/policy/packDnaCompatibility", "stable_additive"),
        ("/policy/missingSignalBehavior", "degraded_not_silent"),
        ("/policy/redactionDefault", "redaction_safe_no_raw_bodies"),
        ("/policy/localCargoProof", "invalid"),
    ] {
        if string_field(&manifest, pointer, MANIFEST_REL)? != expected {
            return Err(format!("{pointer} must stay {expected}"));
        }
    }
    for pointer in [
        "/policy/goldensUpdatedCoordinately",
        "/policy/rchProofRequiredForRuntimeTests",
    ] {
        if !bool_field(&manifest, pointer, MANIFEST_REL)? {
            return Err(format!("{pointer} must stay true"));
        }
    }
    Ok(())
}

#[test]
fn required_signal_set_is_complete_and_additive() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let mut actual = BTreeSet::new();
    for (index, signal) in array_field(&manifest, "/requiredSignals", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("requiredSignals[{index}]");
        let id = string_field(signal, "/id", &context)?;
        if !actual.insert(id.to_owned()) {
            return Err(format!("duplicate required signal {id}"));
        }
        for pointer in [
            "/sourceFeature",
            "/reasonSource",
            "/redaction",
            "/degradedHandling",
            "/followupCommand",
            "/agentQuestion",
            "/decisionImpact",
        ] {
            if string_field(signal, pointer, &context)?.trim().is_empty() {
                return Err(format!("{id}: {pointer} must not be empty"));
            }
        }
        let owner_beads = string_set(
            array_field(signal, "/ownerBeads", &context)?,
            &format!("{id}.ownerBeads"),
        )?;
        if !owner_beads.contains("bd-1n0np.23.5") {
            return Err(format!("{id}: ownerBeads must include bd-1n0np.23.5"));
        }
        let why_fields = string_set(
            array_field(signal, "/whyFields", &context)?,
            &format!("{id}.whyFields"),
        )?;
        let pack_dna_fields = string_set(
            array_field(signal, "/packDnaFields", &context)?,
            &format!("{id}.packDnaFields"),
        )?;
        if why_fields.is_empty() || pack_dna_fields.is_empty() {
            return Err(format!(
                "{id}: whyFields and packDnaFields must both be non-empty"
            ));
        }
        let schema_refs = string_set(
            array_field(signal, "/schemaRefs", &context)?,
            &format!("{id}.schemaRefs"),
        )?;
        if !schema_refs.contains("ee.why.v1") || !schema_refs.contains("ee.context.pack_dna.v1") {
            return Err(format!(
                "{id}: schemaRefs must include ee.why.v1 and ee.context.pack_dna.v1"
            ));
        }
        if id == "causal_ancestry_path" && !schema_refs.contains("ee.why.causal.v1") {
            return Err("causal_ancestry_path must reference ee.why.causal.v1".to_owned());
        }
    }

    let expected = required_signal_set();
    if actual != expected {
        return Err(format!(
            "required signal set drifted: missing={:?}, extra={:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn signal_coverage_matrix_accounts_for_each_required_signal() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let expected_ids = required_signal_set();
    let runtime_policy = match (
        string_field(&manifest, "/policy/localCargoProof", MANIFEST_REL)?,
        bool_field(
            &manifest,
            "/policy/rchProofRequiredForRuntimeTests",
            MANIFEST_REL,
        )?,
    ) {
        ("invalid", true) => "rch_required_local_invalid",
        (local_policy, rch_required) => {
            return Err(format!(
                "unsupported runtime proof posture localCargoProof={local_policy} rchProofRequiredForRuntimeTests={rch_required}"
            ));
        }
    };

    let mut signals = BTreeMap::new();
    for (index, signal) in array_field(&manifest, "/requiredSignals", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("requiredSignals[{index}]");
        let id = string_field(signal, "/id", &context)?;
        if signals.insert(id.to_owned(), signal).is_some() {
            return Err(format!("duplicate required signal {id}"));
        }
    }

    let mut matrix_ids = BTreeSet::new();
    for (index, row) in array_field(&manifest, "/signalCoverageMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("signalCoverageMatrix[{index}]");
        let signal_id = string_field(row, "/signal", &context)?;
        if !matrix_ids.insert(signal_id.to_owned()) {
            return Err(format!("duplicate signalCoverageMatrix row {signal_id}"));
        }

        let signal = signals
            .get(signal_id)
            .ok_or_else(|| format!("{context}: missing required signal {signal_id}"))?;
        let signal_context = format!("requiredSignals[{signal_id}]");

        if string_field(row, "/sourceFeature", &context)?
            != string_field(signal, "/sourceFeature", &signal_context)?
        {
            return Err(format!(
                "{signal_id}: sourceFeature must mirror the required signal"
            ));
        }
        if string_field(row, "/compatibility", &context)? != "stable_additive" {
            return Err(format!(
                "{signal_id}: compatibility must be stable_additive"
            ));
        }
        if string_field(row, "/redactionStatus", &context)? != "redaction_safe" {
            return Err(format!(
                "{signal_id}: redactionStatus must be redaction_safe"
            ));
        }
        if string_field(row, "/degradedHandlingStatus", &context)? != "degraded_not_silent" {
            return Err(format!(
                "{signal_id}: degradedHandlingStatus must be degraded_not_silent"
            ));
        }
        if string_field(row, "/agentQuestionStatus", &context)? != "concrete_question" {
            return Err(format!(
                "{signal_id}: agentQuestionStatus must be concrete_question"
            ));
        }
        if string_field(row, "/decisionImpactStatus", &context)? != "concrete_decision" {
            return Err(format!(
                "{signal_id}: decisionImpactStatus must be concrete_decision"
            ));
        }
        if string_field(row, "/runtimeProofPolicy", &context)? != runtime_policy {
            return Err(format!(
                "{signal_id}: runtimeProofPolicy must be {runtime_policy}"
            ));
        }
        if string_field(row, "/complianceStatus", &context)? != "planned_conformant" {
            return Err(format!(
                "{signal_id}: complianceStatus must be planned_conformant"
            ));
        }

        for (pointer, expected) in [
            (
                "/ownerBeadCount",
                array_field(signal, "/ownerBeads", &signal_context)?.len() as u64,
            ),
            (
                "/whyFieldCount",
                array_field(signal, "/whyFields", &signal_context)?.len() as u64,
            ),
            (
                "/packDnaFieldCount",
                array_field(signal, "/packDnaFields", &signal_context)?.len() as u64,
            ),
            (
                "/schemaRefCount",
                array_field(signal, "/schemaRefs", &signal_context)?.len() as u64,
            ),
            ("/staticAssertions", REQUIRED_SIGNAL_MUST_CLAUSES),
            ("/mustClauses", REQUIRED_SIGNAL_MUST_CLAUSES),
            ("/tested", REQUIRED_SIGNAL_MUST_CLAUSES),
            ("/passing", REQUIRED_SIGNAL_MUST_CLAUSES),
            ("/divergent", 0),
        ] {
            let actual = u64_field(row, pointer, &context)?;
            if actual != expected {
                return Err(format!(
                    "{signal_id}: {pointer} must be {expected}, got {actual}"
                ));
            }
        }

        let score_milli = u64_field(row, "/scoreMilli", &context)?;
        let computed_score = u64_field(row, "/passing", &context)?.saturating_mul(1000)
            / u64_field(row, "/mustClauses", &context)?;
        if score_milli != computed_score {
            return Err(format!(
                "{signal_id}: scoreMilli={score_milli} must match computed MUST score={computed_score}"
            ));
        }
        if score_milli < MIN_MUST_COVERAGE_MILLI {
            return Err(format!(
                "{signal_id}: MUST coverage {score_milli} is below {MIN_MUST_COVERAGE_MILLI}"
            ));
        }
    }

    if matrix_ids != expected_ids {
        return Err(format!(
            "signalCoverageMatrix drifted from required signals: missing={:?}, extra={:?}",
            expected_ids.difference(&matrix_ids).collect::<Vec<_>>(),
            matrix_ids.difference(&expected_ids).collect::<Vec<_>>()
        ));
    }

    Ok(())
}

#[test]
fn signals_answer_concrete_agent_questions_and_decision_impacts() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let mut actual = BTreeSet::new();

    for (index, signal) in array_field(&manifest, "/requiredSignals", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("requiredSignals[{index}]");
        let id = string_field(signal, "/id", &context)?;
        actual.insert((
            id.to_owned(),
            string_field(signal, "/agentQuestion", &context)?.to_owned(),
            string_field(signal, "/decisionImpact", &context)?.to_owned(),
        ));
    }

    let expected = SIGNAL_AGENT_CONTRACTS
        .iter()
        .map(|(id, question, impact)| {
            (
                (*id).to_owned(),
                (*question).to_owned(),
                (*impact).to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(format!(
            "signal agent-question contract drifted: missing={:?}, extra={:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn schema_and_runtime_anchors_still_exist() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    for pointer in ["/runtimeAnchors", "/schemaAnchors"] {
        for (index, anchor) in array_field(&manifest, pointer, MANIFEST_REL)?
            .iter()
            .enumerate()
        {
            let context = format!("{pointer}[{index}]");
            let source = string_field(anchor, "/source", &context)?;
            let source_text = read_text(source)?;
            for needle in array_field(anchor, "/needles", &context)? {
                let needle = needle
                    .as_str()
                    .ok_or_else(|| format!("{context}: needle must be a string"))?;
                if !source_text.contains(needle) {
                    return Err(format!("{source}: missing required anchor {needle}"));
                }
            }
        }
    }
    Ok(())
}

#[test]
fn documentation_mentions_all_inputs_and_signals() -> TestResult {
    let doc = read_text(DOC_REL)?;
    for required in [
        MANIFEST_REL,
        WHY_SCHEMA_REL,
        PACK_DNA_SCHEMA_REL,
        CAUSAL_WHY_SCHEMA_REL,
        WHY_SOURCE_REL,
        PACK_DNA_SOURCE_REL,
        "Signal Coverage Matrix",
        "signalCoverageMatrix",
        "rch_required_local_invalid",
        "planned_conformant",
        "Local Cargo fallback is not valid proof",
    ] {
        if !doc.contains(required) {
            return Err(format!("{DOC_REL}: missing required reference {required}"));
        }
    }
    for signal in REQUIRED_SIGNALS {
        if !doc.contains(signal) {
            return Err(format!("{DOC_REL}: missing required signal {signal}"));
        }
    }
    for (_, question, impact) in SIGNAL_AGENT_CONTRACTS {
        for needle in [*question, *impact] {
            if !doc.contains(needle) {
                return Err(format!(
                    "{DOC_REL}: missing agent-facing signal detail {needle}"
                ));
            }
        }
    }
    Ok(())
}
