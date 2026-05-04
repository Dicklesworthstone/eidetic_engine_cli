//! Migration guide validation tests.
//!
//! Validates that docs/migration-guide.md contains required sections,
//! references valid commands, skills, and degraded codes.

use std::path::Path;

const MIGRATION_GUIDE: &str = include_str!("../docs/migration-guide.md");
const COMMAND_INVENTORY: &str = include_str!("../docs/mechanical-boundary-command-inventory.md");
const SKILLS_README: &str = include_str!("../skills/README.md");

/// Required sections in the migration guide.
const REQUIRED_SECTIONS: &[&str] = &[
    "## Quick Reference",
    "## Core Documents",
    "## Command Family Migration Map",
    "## Side-Effect and Mutation Table",
    "## Skill Handoff Table",
    "## Degraded Code Reference",
    "## Workflow Examples",
    "## Testing and Validation",
    "## No Features Dropped",
];

/// Command families that must be documented.
const REQUIRED_COMMAND_FAMILIES: &[&str] = &[
    "context",
    "search",
    "why",
    "memory",
    "remember",
    "causal",
    "learn",
    "preflight",
    "tripwire",
    "procedure",
    "lab",
    "situation",
    "plan",
    "rehearse",
    "economy",
    "certificate",
];

/// Skills that must be referenced.
const REQUIRED_SKILL_REFERENCES: &[&str] = &[
    "skills/causal-credit-review/SKILL.md",
    "skills/counterfactual-failure-analysis/SKILL.md",
    "skills/preflight-risk-review/SKILL.md",
    "skills/procedure-distillation/SKILL.md",
    "skills/situation-framing/SKILL.md",
];

/// Degraded codes that must be documented.
const REQUIRED_DEGRADED_CODES: &[&str] = &[
    "storage",
    "search_index_unavailable",
    "causal_evidence_unavailable",
    "causal_sample_underpowered",
    "learning_records_unavailable",
    "experiment_registry_unavailable",
    "preflight_evidence_unavailable",
    "procedure_evidence_unavailable",
    "lab_evidence_unavailable",
    "rehearsal_unavailable",
];

#[test]
fn migration_guide_has_required_sections() {
    let mut missing = Vec::new();
    for section in REQUIRED_SECTIONS {
        if !MIGRATION_GUIDE.contains(section) {
            missing.push(*section);
        }
    }
    assert!(
        missing.is_empty(),
        "Migration guide missing sections: {:?}",
        missing
    );
}

#[test]
fn migration_guide_documents_command_families() {
    let mut missing = Vec::new();
    for family in REQUIRED_COMMAND_FAMILIES {
        // Look for the command family in headers or code blocks
        let patterns = [
            format!("`{}`", family),
            format!(
                "### {}",
                family.chars().next().unwrap().to_uppercase().to_string() + &family[1..]
            ),
            format!("`{} ", family),
        ];
        let found = patterns
            .iter()
            .any(|p| MIGRATION_GUIDE.contains(p.as_str()));
        if !found {
            missing.push(*family);
        }
    }
    assert!(
        missing.is_empty(),
        "Migration guide missing command families: {:?}",
        missing
    );
}

#[test]
fn migration_guide_references_skills() {
    let mut missing = Vec::new();
    for skill in REQUIRED_SKILL_REFERENCES {
        if !MIGRATION_GUIDE.contains(skill) {
            missing.push(*skill);
        }
    }
    assert!(
        missing.is_empty(),
        "Migration guide missing skill references: {:?}",
        missing
    );
}

#[test]
fn migration_guide_documents_degraded_codes() {
    let mut missing = Vec::new();
    for code in REQUIRED_DEGRADED_CODES {
        if !MIGRATION_GUIDE.contains(code) {
            missing.push(*code);
        }
    }
    assert!(
        missing.is_empty(),
        "Migration guide missing degraded codes: {:?}",
        missing
    );
}

#[test]
fn migration_guide_references_core_documents() {
    let required_refs = [
        "mechanical-boundary-command-inventory.md",
        "command_classification.md",
        "boundary-migration-e2e-logging.md",
    ];

    let mut missing = Vec::new();
    for doc in required_refs {
        if !MIGRATION_GUIDE.contains(doc) {
            missing.push(doc);
        }
    }
    assert!(
        missing.is_empty(),
        "Migration guide missing document references: {:?}",
        missing
    );
}

#[test]
fn migration_guide_has_before_after_examples() {
    assert!(
        MIGRATION_GUIDE.contains("Before") && MIGRATION_GUIDE.contains("After"),
        "Migration guide must have before/after examples"
    );
    assert!(
        MIGRATION_GUIDE.contains("### Before:") || MIGRATION_GUIDE.contains("**Before:**"),
        "Migration guide must have labeled before examples"
    );
    assert!(
        MIGRATION_GUIDE.contains("### After:") || MIGRATION_GUIDE.contains("**After:**"),
        "Migration guide must have labeled after examples"
    );
}

#[test]
fn migration_guide_has_repair_commands() {
    assert!(
        MIGRATION_GUIDE.contains("repair"),
        "Migration guide must document repair commands"
    );
    assert!(
        MIGRATION_GUIDE.contains("ee index rebuild") || MIGRATION_GUIDE.contains("ee init"),
        "Migration guide must include specific repair commands"
    );
}

#[test]
fn migration_guide_states_no_features_dropped() {
    assert!(
        MIGRATION_GUIDE.contains("No Features Dropped")
            || MIGRATION_GUIDE.contains("no features were dropped"),
        "Migration guide must explicitly state no features were dropped"
    );
}

#[test]
fn migration_guide_explains_split_responsibilities() {
    let required_concepts = ["Mechanical CLI", "project-local skill", "degraded"];

    let mut missing = Vec::new();
    for concept in required_concepts {
        if !MIGRATION_GUIDE
            .to_lowercase()
            .contains(&concept.to_lowercase())
        {
            missing.push(concept);
        }
    }
    assert!(
        missing.is_empty(),
        "Migration guide must explain: {:?}",
        missing
    );
}

#[test]
fn migration_guide_has_mutation_table() {
    assert!(
        MIGRATION_GUIDE.contains("Mutation") && MIGRATION_GUIDE.contains("Idempotency"),
        "Migration guide must have mutation/idempotency table"
    );
    assert!(
        MIGRATION_GUIDE.contains("audited_mutation") || MIGRATION_GUIDE.contains("append_only"),
        "Migration guide must document mutation classes"
    );
}

#[test]
fn referenced_skills_exist() {
    for skill_path in REQUIRED_SKILL_REFERENCES {
        let full_path = Path::new(skill_path);
        // We can't check filesystem in include_str tests, but we can verify
        // the skill is mentioned in skills/README.md
        let skill_name = full_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");

        assert!(
            SKILLS_README.contains(skill_name) || skill_name.is_empty(),
            "Skill {} should be listed in skills/README.md",
            skill_path
        );
    }
}

#[test]
fn command_families_exist_in_inventory() {
    for family in REQUIRED_COMMAND_FAMILIES {
        // Check that each documented family appears in the command inventory
        let pattern = format!("`{}`", family);
        assert!(
            COMMAND_INVENTORY.contains(&pattern) || COMMAND_INVENTORY.contains(family),
            "Command family {} should appear in command inventory",
            family
        );
    }
}

#[test]
fn migration_guide_has_json_examples() {
    assert!(
        MIGRATION_GUIDE.contains("```json"),
        "Migration guide must have JSON examples"
    );
    assert!(
        MIGRATION_GUIDE.contains("\"schema\":"),
        "JSON examples must include schema field"
    );
}

#[test]
fn migration_guide_has_bash_examples() {
    assert!(
        MIGRATION_GUIDE.contains("```bash"),
        "Migration guide must have bash command examples"
    );
    assert!(
        MIGRATION_GUIDE.contains("ee "),
        "Bash examples must include ee commands"
    );
}
