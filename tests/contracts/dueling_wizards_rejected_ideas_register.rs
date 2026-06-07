//! bd-1n0np.14 - dueling-wizards rejected ideas register.
//!
//! This contract pins the decision record for ideas the duel rejected or
//! reframed. It prevents killed ideas from losing their score, rationale, or
//! "new evidence required" reopen policy while future planning work continues.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

type TestResult = Result<(), String>;

const MANIFEST_REL: &str = "tests/fixtures/contracts/dueling_wizards_rejected_ideas_register.json";
const DOC_REL: &str = "docs/agent-ux/dueling-wizards-rejected-ideas-register.md";

const REQUIRED_GUARDRAILS: &[&str] = &[
    "reopen_requires_new_evidence",
    "lab_mode_before_live_policy",
    "candidate_not_auto_mutation",
    "determinism_preserved",
];

const REOPEN_EVIDENCE_KINDS: &[&str] = &[
    "measured_workflow_result",
    "regression",
    "benchmark",
    "corpus_finding",
];

const FORBIDDEN_REOPEN_EVIDENCE: &[&str] = &[
    "restated_proposal",
    "aesthetic_preference",
    "unmeasured_opinion",
];

const KILLED_IDEAS: &[(&str, &str, u64)] = &[
    ("stochastic_ab_packing", "Gemini #5", 458),
    ("outcome_aligned_local_projection", "Gemini #1", 521),
    (
        "cross_harness_bayesian_prior_calibration",
        "Gemini #10",
        538,
    ),
    ("concept_drift_radar_auto_demote", "Gemini #3", 600),
];

const REFRAMED_IDEAS: &[(&str, &str, &str)] = &[
    (
        "conformal_pack_coverage",
        "calibration_honesty_report",
        "bd-1n0np.13",
    ),
    ("coverage_radar", "gap_honesty_coverage_gaps", "bd-1n0np.6"),
];

const IDEA_WIZARD_PHASE_GATES: &[(u64, &str, &str, &str)] = &[
    (
        2,
        "generate_and_winnow",
        "preserve_scores_sources_and_rationales",
        "forbidden",
    ),
    (
        3,
        "expand_next_ten",
        "record_reframed_or_killed_disposition",
        "forbidden",
    ),
    (4, "overlap_check", "merge_before_duplicate", "forbidden"),
    (
        5,
        "create_beads",
        "blocked_until_tracker_authoritative",
        "br_only_when_tracker_authoritative",
    ),
    (
        6,
        "refine_plan_space",
        "repeat_four_to_five_passes_with_tests",
        "br_only_when_tracker_authoritative",
    ),
];

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

fn required_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn rejected_register_manifest_identity_and_policy_are_stable() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    if string_field(&manifest, "/schema", MANIFEST_REL)?
        != "ee.dueling_wizards.rejected_ideas_register.v1"
    {
        return Err(format!(
            "{MANIFEST_REL}: schema must be ee.dueling_wizards.rejected_ideas_register.v1"
        ));
    }
    if string_field(&manifest, "/initiativeBead", MANIFEST_REL)? != "bd-1n0np" {
        return Err("manifest must identify initiativeBead bd-1n0np".to_owned());
    }
    if string_field(&manifest, "/gateBead", MANIFEST_REL)? != "bd-1n0np.14" {
        return Err("manifest must identify gateBead bd-1n0np.14".to_owned());
    }
    for (pointer, expected) in [
        (
            "/manifestOwner",
            "tests/contracts/dueling_wizards_rejected_ideas_register.rs",
        ),
        ("/doc", DOC_REL),
    ] {
        if string_field(&manifest, pointer, MANIFEST_REL)? != expected {
            return Err(format!("{pointer} must point at {expected}"));
        }
    }
    if string_field(&manifest, "/implementationState", MANIFEST_REL)? != "decision_record" {
        return Err("implementationState must stay decision_record".to_owned());
    }
    if string_field(&manifest, "/policy/recordType", MANIFEST_REL)? != "rejected_ideas_register" {
        return Err("policy.recordType must stay rejected_ideas_register".to_owned());
    }
    if string_field(&manifest, "/policy/reopenPolicy", MANIFEST_REL)? != "requires_new_evidence" {
        return Err("policy.reopenPolicy must stay requires_new_evidence".to_owned());
    }
    if string_field(&manifest, "/policy/liveMutationDefault", MANIFEST_REL)? != "forbidden" {
        return Err("policy.liveMutationDefault must stay forbidden".to_owned());
    }
    if string_field(&manifest, "/policy/salvagePath", MANIFEST_REL)? != "explicit_lab_or_eval_mode"
    {
        return Err("policy.salvagePath must stay explicit_lab_or_eval_mode".to_owned());
    }
    for pointer in [
        "/policy/candidateBeforeMutation",
        "/policy/determinismRequiredBeforeLiveUse",
    ] {
        if !bool_field(&manifest, pointer, MANIFEST_REL)? {
            return Err(format!("{pointer} must stay true"));
        }
    }

    let guardrails = string_set(
        array_field(&manifest, "/requiredGuardrails", MANIFEST_REL)?,
        "requiredGuardrails",
    )?;
    let expected = required_set(REQUIRED_GUARDRAILS);
    if guardrails != expected {
        return Err(format!(
            "requiredGuardrails drifted: missing={:?}, extra={:?}",
            expected.difference(&guardrails).collect::<Vec<_>>(),
            guardrails.difference(&expected).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[test]
fn idea_wizard_phase_gates_block_duplicate_or_unsafe_bead_creation() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let mut actual = BTreeMap::new();

    for (index, gate) in array_field(&manifest, "/ideaWizardPhaseGates", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("ideaWizardPhaseGates[{index}]");
        let phase = u64_field(gate, "/phase", &context)?;
        let name = string_field(gate, "/name", &context)?;
        let required_action = string_field(gate, "/requiredAction", &context)?;
        let beads_mutation = string_field(gate, "/beadsMutation", &context)?;
        if actual
            .insert(
                phase,
                (
                    name.to_owned(),
                    required_action.to_owned(),
                    beads_mutation.to_owned(),
                ),
            )
            .is_some()
        {
            return Err(format!("duplicate idea-wizard phase gate {phase}"));
        }
        if !matches!(
            beads_mutation,
            "forbidden" | "br_only_when_tracker_authoritative"
        ) {
            return Err(format!(
                "phase {phase}: beadsMutation must be conservative, got {beads_mutation}"
            ));
        }
    }

    let expected = IDEA_WIZARD_PHASE_GATES
        .iter()
        .map(|(phase, name, required_action, beads_mutation)| {
            (
                *phase,
                (
                    (*name).to_owned(),
                    (*required_action).to_owned(),
                    (*beads_mutation).to_owned(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if actual != expected {
        return Err(format!(
            "ideaWizardPhaseGates drifted: expected {expected:?}, got {actual:?}"
        ));
    }
    Ok(())
}

#[test]
fn killed_ideas_preserve_scores_sources_and_reopen_policy() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let mut expected = BTreeMap::new();
    for (id, source, score) in KILLED_IDEAS {
        expected.insert((*id).to_owned(), ((*source).to_owned(), *score));
    }
    let mut actual = BTreeMap::new();

    for (index, idea) in array_field(&manifest, "/killedIdeas", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("killedIdeas[{index}]");
        let id = string_field(idea, "/id", &context)?;
        if actual
            .insert(
                id.to_owned(),
                (
                    string_field(idea, "/source", &context)?.to_owned(),
                    u64_field(idea, "/score", &context)?,
                ),
            )
            .is_some()
        {
            return Err(format!("duplicate killed idea id {id}"));
        }
        if string_field(idea, "/status", &context)? != "killed" {
            return Err(format!("{id}: status must stay killed"));
        }
        if string_field(idea, "/reopenPolicy", &context)? != "requires_new_evidence" {
            return Err(format!("{id}: reopenPolicy must require new evidence"));
        }
        if string_field(idea, "/salvageMode", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!("{id}: salvageMode must not be empty"));
        }
        let tags = string_set(
            array_field(idea, "/rationaleTags", &context)?,
            &format!("{id}.rationaleTags"),
        )?;
        if tags.len() < 3 {
            return Err(format!(
                "{id}: rationaleTags must preserve at least three reasons"
            ));
        }
    }

    if actual != expected {
        return Err(format!(
            "killed idea set drifted: expected {expected:?}, got {actual:?}"
        ));
    }
    Ok(())
}

#[test]
fn reopen_evidence_matrix_covers_killed_ideas_and_blocks_weak_revival() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let accepted_evidence = string_set(
        array_field(&manifest, "/reopenEvidenceKinds", MANIFEST_REL)?,
        "reopenEvidenceKinds",
    )?;
    let expected_accepted = required_set(REOPEN_EVIDENCE_KINDS);
    if accepted_evidence != expected_accepted {
        return Err(format!(
            "reopenEvidenceKinds drifted: missing={:?}, extra={:?}",
            expected_accepted
                .difference(&accepted_evidence)
                .collect::<Vec<_>>(),
            accepted_evidence
                .difference(&expected_accepted)
                .collect::<Vec<_>>()
        ));
    }

    let forbidden_evidence = string_set(
        array_field(&manifest, "/forbiddenReopenEvidence", MANIFEST_REL)?,
        "forbiddenReopenEvidence",
    )?;
    let expected_forbidden = required_set(FORBIDDEN_REOPEN_EVIDENCE);
    if forbidden_evidence != expected_forbidden {
        return Err(format!(
            "forbiddenReopenEvidence drifted: missing={:?}, extra={:?}",
            expected_forbidden
                .difference(&forbidden_evidence)
                .collect::<Vec<_>>(),
            forbidden_evidence
                .difference(&expected_forbidden)
                .collect::<Vec<_>>()
        ));
    }

    let expected = KILLED_IDEAS
        .iter()
        .map(|(id, source, score)| ((*id).to_owned(), ((*source).to_owned(), *score)))
        .collect::<BTreeMap<_, _>>();
    let mut actual = BTreeMap::new();

    for (index, row) in array_field(&manifest, "/reopenEvidenceMatrix", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("reopenEvidenceMatrix[{index}]");
        let id = string_field(row, "/id", &context)?;
        if actual
            .insert(
                id.to_owned(),
                (
                    string_field(row, "/source", &context)?.to_owned(),
                    u64_field(row, "/score", &context)?,
                ),
            )
            .is_some()
        {
            return Err(format!("duplicate reopen evidence row {id}"));
        }
        if string_field(row, "/status", &context)? != "killed" {
            return Err(format!("{id}: reopen matrix status must stay killed"));
        }
        if string_field(row, "/reopenPolicy", &context)? != "requires_new_evidence" {
            return Err(format!("{id}: reopen matrix must require new evidence"));
        }
        if u64_field(row, "/minimumEvidenceCount", &context)? == 0 {
            return Err(format!("{id}: minimumEvidenceCount must be nonzero"));
        }
        let row_accepted = string_set(
            array_field(row, "/acceptedEvidenceKinds", &context)?,
            &format!("{id}.acceptedEvidenceKinds"),
        )?;
        if row_accepted != expected_accepted {
            return Err(format!(
                "{id}: acceptedEvidenceKinds must mirror reopenEvidenceKinds"
            ));
        }
        let row_forbidden = string_set(
            array_field(row, "/forbiddenEvidenceKinds", &context)?,
            &format!("{id}.forbiddenEvidenceKinds"),
        )?;
        if row_forbidden != expected_forbidden {
            return Err(format!(
                "{id}: forbiddenEvidenceKinds must mirror forbiddenReopenEvidence"
            ));
        }
        if string_field(row, "/firstAllowedDisposition", &context)? != "explicit_lab_or_eval_mode" {
            return Err(format!(
                "{id}: firstAllowedDisposition must stay explicit_lab_or_eval_mode"
            ));
        }
        if string_field(row, "/livePolicyMutation", &context)?
            != "forbidden_until_deterministic_proof"
        {
            return Err(format!(
                "{id}: livePolicyMutation must require deterministic proof"
            ));
        }
        if string_field(row, "/beadsMutation", &context)? != "br_only_when_tracker_authoritative" {
            return Err(format!(
                "{id}: beadsMutation must stay br_only_when_tracker_authoritative"
            ));
        }
    }

    if actual != expected {
        return Err(format!(
            "reopen evidence matrix drifted: expected {expected:?}, got {actual:?}"
        ));
    }
    Ok(())
}

#[test]
fn reframed_ideas_keep_their_target_beads_and_not_killed_status() -> TestResult {
    let manifest = read_json(MANIFEST_REL)?;
    let mut expected = BTreeMap::new();
    for (id, reframed_as, target_bead) in REFRAMED_IDEAS {
        expected.insert(
            (*id).to_owned(),
            ((*reframed_as).to_owned(), (*target_bead).to_owned()),
        );
    }
    let mut actual = BTreeMap::new();

    for (index, idea) in array_field(&manifest, "/reframedIdeas", MANIFEST_REL)?
        .iter()
        .enumerate()
    {
        let context = format!("reframedIdeas[{index}]");
        let id = string_field(idea, "/id", &context)?;
        if string_field(idea, "/status", &context)? != "reframed_not_killed" {
            return Err(format!("{id}: status must stay reframed_not_killed"));
        }
        let target_bead = string_field(idea, "/targetBead", &context)?;
        if !target_bead.starts_with("bd-1n0np.") {
            return Err(format!("{id}: targetBead must stay under bd-1n0np"));
        }
        if string_field(idea, "/rejectedFacet", &context)?
            .trim()
            .is_empty()
        {
            return Err(format!("{id}: rejectedFacet must not be empty"));
        }
        if actual
            .insert(
                id.to_owned(),
                (
                    string_field(idea, "/reframedAs", &context)?.to_owned(),
                    target_bead.to_owned(),
                ),
            )
            .is_some()
        {
            return Err(format!("duplicate reframed idea id {id}"));
        }
    }

    if actual != expected {
        return Err(format!(
            "reframed idea set drifted: expected {expected:?}, got {actual:?}"
        ));
    }
    Ok(())
}

#[test]
fn documentation_mentions_every_decision_and_guardrail() -> TestResult {
    let doc = read_text(DOC_REL)?;
    for needle in [
        MANIFEST_REL,
        "bd-1n0np.14",
        "requires_new_evidence",
        "explicit lab",
        "candidate",
        "deterministic",
        "pack hashes",
        "ideaWizardPhaseGates",
        "reopenEvidenceMatrix",
        "measured_workflow_result",
        "restated_proposal",
        "forbidden_until_deterministic_proof",
        "merge_before_duplicate",
        "blocked_until_tracker_authoritative",
        "br_only_when_tracker_authoritative",
    ] {
        if !doc.contains(needle) {
            return Err(format!("{DOC_REL}: missing required reference {needle}"));
        }
    }
    for (id, source, score) in KILLED_IDEAS {
        let score = score.to_string();
        for needle in [*id, *source, score.as_str()] {
            if !doc.contains(needle) {
                return Err(format!("{DOC_REL}: missing killed idea detail {needle}"));
            }
        }
    }
    for (id, reframed_as, target_bead) in REFRAMED_IDEAS {
        for needle in [*id, *reframed_as, *target_bead] {
            if !doc.contains(needle) {
                return Err(format!("{DOC_REL}: missing reframed idea detail {needle}"));
            }
        }
    }
    for guardrail in REQUIRED_GUARDRAILS {
        if !doc.contains(guardrail) {
            return Err(format!("{DOC_REL}: missing guardrail {guardrail}"));
        }
    }
    for (_, name, required_action, beads_mutation) in IDEA_WIZARD_PHASE_GATES {
        for needle in [*name, *required_action, *beads_mutation] {
            if !doc.contains(needle) {
                return Err(format!("{DOC_REL}: missing idea-wizard gate {needle}"));
            }
        }
    }
    Ok(())
}
