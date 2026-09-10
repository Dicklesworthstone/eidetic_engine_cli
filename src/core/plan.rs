//! Command recipe catalog and workspace recipe retrieval (EE-PLAN-001).
//!
//! Deterministic, local, schema-validated catalog over known EE commands,
//! capabilities, effect metadata, and degraded branches. Retrieval includes
//! stored workspace recipes with explicit provenance.
//! Recommendations never execute steps or classify arbitrary commands as safe.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};

use crate::core::task_frame::{TaskFrameRecord, TaskFrameShowOptions, show_task_frame};
use crate::db::{DbConnection, StoredPlanRecipe, StoredProceduralRule};
use crate::models::DomainError;

pub const GOAL_PLAN_SCHEMA_V1: &str = "ee.plan.goal.v1";
pub const RECIPE_LIST_SCHEMA_V1: &str = "ee.plan.recipe_list.v1";
pub const RECIPE_SHOW_SCHEMA_V1: &str = "ee.plan.recipe.v1";
pub const PLAN_EXPLAIN_SCHEMA_V1: &str = "ee.plan.explain.v1";
pub const PLAN_RECIPE_CATALOG_SOURCE_V1: &str = "ee.plan.recipe_catalog.v1";

// ============================================================================
// Goal Classification
// ============================================================================

/// Known goal categories that map to recipes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GoalCategory {
    /// First run / workspace initialization.
    Init,
    /// Pre-task briefing and context pack.
    PreTaskBriefing,
    /// In-task retrieval and explanation.
    InTaskRetrieval,
    /// Degraded state repair.
    DegradedRepair,
    /// Remember / outcome capture.
    OutcomeCapture,
    /// Session review and curation proposal.
    SessionReview,
    /// Handoff / resume workflow.
    Handoff,
    /// Support bundle capture.
    SupportBundle,
    /// Backup / export.
    BackupExport,
    /// Rehearsal of risky workflows.
    Rehearsal,
    /// Audit timeline inspection.
    AuditInspection,
    /// Implementation closeout evidence.
    Closeout,
    /// Unknown or ambiguous goal.
    Unknown,
}

impl GoalCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::PreTaskBriefing => "pre_task_briefing",
            Self::InTaskRetrieval => "in_task_retrieval",
            Self::DegradedRepair => "degraded_repair",
            Self::OutcomeCapture => "outcome_capture",
            Self::SessionReview => "session_review",
            Self::Handoff => "handoff",
            Self::SupportBundle => "support_bundle",
            Self::BackupExport => "backup_export",
            Self::Rehearsal => "rehearsal",
            Self::AuditInspection => "audit_inspection",
            Self::Closeout => "closeout",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Init,
            Self::PreTaskBriefing,
            Self::InTaskRetrieval,
            Self::DegradedRepair,
            Self::OutcomeCapture,
            Self::SessionReview,
            Self::Handoff,
            Self::SupportBundle,
            Self::BackupExport,
            Self::Rehearsal,
            Self::AuditInspection,
            Self::Closeout,
        ]
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Init => "First run / workspace initialization",
            Self::PreTaskBriefing => "Pre-task briefing and context pack",
            Self::InTaskRetrieval => "In-task retrieval and explanation",
            Self::DegradedRepair => "Degraded state repair",
            Self::OutcomeCapture => "Remember / outcome capture",
            Self::SessionReview => "Session review and curation proposal",
            Self::Handoff => "Handoff / resume workflow",
            Self::SupportBundle => "Support bundle capture",
            Self::BackupExport => "Backup / export",
            Self::Rehearsal => "Rehearsal of risky workflows",
            Self::AuditInspection => "Audit timeline inspection",
            Self::Closeout => "Implementation closeout evidence",
            Self::Unknown => "Unknown or ambiguous goal",
        }
    }
}

impl std::fmt::Display for GoalCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Classify a goal string into a category.
#[must_use]
pub fn classify_goal(goal: &str) -> GoalClassification {
    let goal_lower = goal.to_lowercase();
    let mut scores: Vec<(GoalCategory, u32)> = Vec::new();

    // Keyword matching with weighted scores
    let keywords: &[(GoalCategory, &[&str], u32)] = &[
        (
            GoalCategory::Init,
            &[
                "init",
                "initialize",
                "setup",
                "first",
                "new workspace",
                "create workspace",
            ],
            10,
        ),
        (
            GoalCategory::PreTaskBriefing,
            &[
                "brief",
                "context",
                "prepare",
                "before task",
                "pre-task",
                "starting work",
                "begin task",
            ],
            10,
        ),
        (
            GoalCategory::InTaskRetrieval,
            &[
                "search",
                "find",
                "retrieve",
                "look up",
                "why",
                "explain",
                "during task",
                "working on",
            ],
            10,
        ),
        (
            GoalCategory::DegradedRepair,
            &[
                "repair", "fix", "degraded", "broken", "error", "doctor", "health", "recover",
            ],
            10,
        ),
        (
            GoalCategory::OutcomeCapture,
            &[
                "remember",
                "outcome",
                "record",
                "capture",
                "lesson",
                "learned",
                "save",
                "completed",
            ],
            10,
        ),
        (
            GoalCategory::SessionReview,
            &[
                "review",
                "session",
                "curation",
                "curate",
                "history",
                "past work",
            ],
            10,
        ),
        (
            GoalCategory::Handoff,
            &[
                "handoff",
                "hand off",
                "resume",
                "continue",
                "pass",
                "transition",
                "switch",
            ],
            10,
        ),
        (
            GoalCategory::SupportBundle,
            &[
                "support",
                "bundle",
                "diagnostic",
                "debug",
                "help",
                "troubleshoot",
            ],
            10,
        ),
        (
            GoalCategory::BackupExport,
            &["backup", "export", "archive", "save state", "snapshot"],
            10,
        ),
        (
            GoalCategory::Rehearsal,
            &[
                "rehearse",
                "rehearsal",
                "dry run",
                "test",
                "simulate",
                "risky",
                "practice",
            ],
            10,
        ),
        (
            GoalCategory::AuditInspection,
            &[
                "audit",
                "inspect",
                "timeline",
                "trace",
                "history",
                "log",
                "what happened",
            ],
            10,
        ),
        (
            GoalCategory::Closeout,
            &[
                "closeout",
                "close out",
                "finish",
                "complete",
                "done",
                "evidence",
                "wrap up",
            ],
            10,
        ),
    ];

    for (category, kws, weight) in keywords {
        let mut score = 0u32;
        for kw in *kws {
            if goal_lower.contains(kw) {
                score += weight;
            }
        }
        if score > 0 {
            scores.push((*category, score));
        }
    }

    scores.sort_by_key(|score| Reverse(score.1));

    if scores.is_empty() {
        return GoalClassification {
            primary: GoalCategory::Unknown,
            confidence: 0.0,
            alternatives: vec![],
            ambiguous: true,
        };
    }

    let (primary, top_score) = scores
        .first()
        .map(|(category, score)| (*category, *score))
        .unwrap_or((GoalCategory::Unknown, 0));

    // Check for ambiguity (multiple high scores)
    let alternatives: Vec<GoalCategory> = scores
        .iter()
        .skip(1)
        .filter(|(_, s)| *s >= top_score / 2)
        .map(|(c, _)| *c)
        .collect();

    let ambiguous = !alternatives.is_empty();
    let confidence = if ambiguous { 0.5 } else { 0.9 };

    GoalClassification {
        primary,
        confidence,
        alternatives,
        ambiguous,
    }
}

/// Result of goal classification.
#[derive(Clone, Debug)]
pub struct GoalClassification {
    pub primary: GoalCategory,
    pub confidence: f64,
    pub alternatives: Vec<GoalCategory>,
    pub ambiguous: bool,
}

// ============================================================================
// Recipes
// ============================================================================

/// Effect posture for a recipe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectPosture {
    /// Stored commands have not been evaluated for effects.
    Unknown,
    /// Read-only, no mutations.
    ReadOnly,
    /// Writes to local workspace only.
    LocalWrite,
    /// May affect external systems.
    External,
}

impl EffectPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::ReadOnly => "read_only",
            Self::LocalWrite => "local_write",
            Self::External => "external",
        }
    }
}

/// A command step in a recipe.
#[derive(Clone, Debug)]
pub struct CommandStep {
    pub order: u32,
    pub command: String,
    pub description: String,
    pub effect_class: EffectPosture,
    pub dry_run_available: bool,
    pub required: bool,
    pub stop_on_failure: bool,
}

impl CommandStep {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "order": self.order,
            "command": self.command,
            "description": self.description,
            "effectClass": self.effect_class.as_str(),
            "dryRunAvailable": self.dry_run_available,
            "required": self.required,
            "stopOnFailure": self.stop_on_failure,
        })
    }
}

/// A built-in recipe definition.
#[derive(Clone, Debug)]
pub struct Recipe {
    pub id: String,
    pub version: u32,
    pub category: GoalCategory,
    pub name: String,
    pub description: String,
    pub effect_posture: EffectPosture,
    pub required_capabilities: Vec<String>,
    pub steps: Vec<CommandStep>,
    pub degraded_branches: Vec<DegradedBranch>,
    pub profiles: Vec<String>,
}

impl Recipe {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "id": self.id,
            "version": self.version,
            "sourceKind": "static_command_catalog",
            "sourceId": recipe_source_id(&self.id),
            "catalogSource": PLAN_RECIPE_CATALOG_SOURCE_V1,
            "decisionBoundary": "mechanical_command_catalog_only",
            "goalPlanning": false,
            "category": self.category.as_str(),
            "name": self.name,
            "description": self.description,
            "effectPosture": self.effect_posture.as_str(),
            "requiredCapabilities": self.required_capabilities,
            "steps": self.steps.iter().map(CommandStep::data_json).collect::<Vec<_>>(),
            "degradedBranches": self.degraded_branches.iter().map(DegradedBranch::data_json).collect::<Vec<_>>(),
            "profiles": self.profiles,
        })
    }

    #[must_use]
    pub fn summary_json(&self) -> JsonValue {
        json!({
            "id": self.id,
            "version": self.version,
            "sourceKind": "static_command_catalog",
            "sourceId": recipe_source_id(&self.id),
            "catalogSource": PLAN_RECIPE_CATALOG_SOURCE_V1,
            "decisionBoundary": "mechanical_command_catalog_only",
            "goalPlanning": false,
            "category": self.category.as_str(),
            "name": self.name,
            "effectPosture": self.effect_posture.as_str(),
            "requiredCapabilities": self.required_capabilities,
            "stepCount": self.steps.len(),
            "profiles": self.profiles,
        })
    }
}

fn recipe_source_id(recipe_id: &str) -> String {
    format!("{PLAN_RECIPE_CATALOG_SOURCE_V1}#{recipe_id}")
}

/// A degraded branch in a recipe.
#[derive(Clone, Debug)]
pub struct DegradedBranch {
    pub condition: String,
    pub alternative_steps: Vec<CommandStep>,
    pub message: String,
}

impl DegradedBranch {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "condition": self.condition,
            "alternativeSteps": self.alternative_steps.iter().map(CommandStep::data_json).collect::<Vec<_>>(),
            "message": self.message,
        })
    }
}

/// Get all built-in recipes.
#[must_use]
pub fn builtin_recipes() -> Vec<Recipe> {
    vec![
        Recipe {
            id: "init-workspace".to_string(),
            version: 1,
            category: GoalCategory::Init,
            name: "Initialize Workspace".to_string(),
            description: "Set up a new EE workspace with database, indexes, and configuration.".to_string(),
            effect_posture: EffectPosture::LocalWrite,
            required_capabilities: vec!["storage".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee init --workspace .".to_string(),
                    description: "Initialize workspace database and configuration".to_string(),
                    effect_class: EffectPosture::LocalWrite,
                    dry_run_available: true,
                    required: true,
                    stop_on_failure: true,
                },
                CommandStep {
                    order: 2,
                    command: "ee status --json".to_string(),
                    description: "Verify workspace state".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![],
            profiles: vec!["compact".to_string(), "full".to_string(), "safe".to_string()],
        },
        Recipe {
            id: "pre-task-context".to_string(),
            version: 1,
            category: GoalCategory::PreTaskBriefing,
            name: "Pre-Task Context Pack".to_string(),
            description: "Gather relevant context before starting a task.".to_string(),
            effect_posture: EffectPosture::ReadOnly,
            required_capabilities: vec!["storage".to_string(), "search".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee status --json".to_string(),
                    description: "Check workspace health".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
                CommandStep {
                    order: 2,
                    command: "ee pack \"<task>\" --workspace . --max-tokens 4000 --json".to_string(),
                    description: "Generate context pack for task".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![
                DegradedBranch {
                    condition: "search_index_stale".to_string(),
                    alternative_steps: vec![
                        CommandStep {
                            order: 1,
                            command: "ee index rebuild --workspace .".to_string(),
                            description: "Rebuild stale search index".to_string(),
                            effect_class: EffectPosture::LocalWrite,
                            dry_run_available: true,
                            required: true,
                            stop_on_failure: true,
                        },
                    ],
                    message: "Search index is stale, rebuild before context generation".to_string(),
                },
            ],
            profiles: vec!["compact".to_string(), "full".to_string(), "safe".to_string()],
        },
        Recipe {
            id: "in-task-search".to_string(),
            version: 1,
            category: GoalCategory::InTaskRetrieval,
            name: "In-Task Search and Explain".to_string(),
            description: "Search for relevant memories and explain selections.".to_string(),
            effect_posture: EffectPosture::ReadOnly,
            required_capabilities: vec!["storage".to_string(), "search".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee search \"<query>\" --workspace . --json".to_string(),
                    description: "Search for relevant memories".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
                CommandStep {
                    order: 2,
                    command: "ee why <memory-id> --json".to_string(),
                    description: "Explain why a memory was selected".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: false,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![],
            profiles: vec!["compact".to_string(), "full".to_string()],
        },
        Recipe {
            id: "degraded-repair".to_string(),
            version: 1,
            category: GoalCategory::DegradedRepair,
            name: "Repair Degraded State".to_string(),
            description: "Diagnose and repair degraded workspace state.".to_string(),
            effect_posture: EffectPosture::LocalWrite,
            required_capabilities: vec!["storage".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee doctor --workspace . --json".to_string(),
                    description: "Run diagnostics".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
                CommandStep {
                    order: 2,
                    command: "ee doctor --workspace . --fix-plan --json".to_string(),
                    description: "Generate fix plan".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
                CommandStep {
                    order: 3,
                    command: "ee check --workspace .".to_string(),
                    description: "Verify repairs".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: false,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![],
            profiles: vec!["compact".to_string(), "full".to_string(), "safe".to_string()],
        },
        Recipe {
            id: "outcome-capture".to_string(),
            version: 1,
            category: GoalCategory::OutcomeCapture,
            name: "Capture Task Outcome".to_string(),
            description: "Record task outcome, lessons learned, and evidence.".to_string(),
            effect_posture: EffectPosture::LocalWrite,
            required_capabilities: vec!["storage".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee remember --workspace . --level procedural --kind lesson \"<lesson>\" --json".to_string(),
                    description: "Record lesson learned".to_string(),
                    effect_class: EffectPosture::LocalWrite,
                    dry_run_available: true,
                    required: true,
                    stop_on_failure: false,
                },
                CommandStep {
                    order: 2,
                    command: "ee outcome --workspace . --json".to_string(),
                    description: "Record task outcome".to_string(),
                    effect_class: EffectPosture::LocalWrite,
                    dry_run_available: true,
                    required: false,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![],
            profiles: vec!["compact".to_string(), "full".to_string()],
        },
        Recipe {
            id: "session-review".to_string(),
            version: 1,
            category: GoalCategory::SessionReview,
            name: "Review Session History".to_string(),
            description: "Review past session and propose curation actions.".to_string(),
            effect_posture: EffectPosture::ReadOnly,
            required_capabilities: vec!["storage".to_string(), "cass".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee review session --workspace . --json".to_string(),
                    description: "Review session history".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![
                DegradedBranch {
                    condition: "cass_unavailable".to_string(),
                    alternative_steps: vec![
                        CommandStep {
                            order: 1,
                            command: "ee memory list --workspace . --json".to_string(),
                            description: "List memories without CASS".to_string(),
                            effect_class: EffectPosture::ReadOnly,
                            dry_run_available: false,
                            required: true,
                            stop_on_failure: false,
                        },
                    ],
                    message: "CASS is unavailable, falling back to memory list".to_string(),
                },
            ],
            profiles: vec!["compact".to_string(), "full".to_string()],
        },
        Recipe {
            id: "handoff-prepare".to_string(),
            version: 1,
            category: GoalCategory::Handoff,
            name: "Prepare Handoff".to_string(),
            description: "Prepare workspace state for handoff to another agent.".to_string(),
            effect_posture: EffectPosture::ReadOnly,
            required_capabilities: vec!["storage".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee status --json".to_string(),
                    description: "Capture current state".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
                CommandStep {
                    order: 2,
                    command: "ee pack \"handoff summary\" --workspace . --json".to_string(),
                    description: "Generate handoff context".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![],
            profiles: vec!["compact".to_string(), "full".to_string()],
        },
        Recipe {
            id: "support-bundle".to_string(),
            version: 1,
            category: GoalCategory::SupportBundle,
            name: "Create Support Bundle".to_string(),
            description: "Create redacted diagnostic bundle for support.".to_string(),
            effect_posture: EffectPosture::LocalWrite,
            required_capabilities: vec!["storage".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee support bundle --workspace . --redacted --dry-run --json".to_string(),
                    description: "Plan support bundle".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: true,
                    required: true,
                    stop_on_failure: false,
                },
                CommandStep {
                    order: 2,
                    command: "ee support bundle --workspace . --redacted --out <dir> --json".to_string(),
                    description: "Create support bundle".to_string(),
                    effect_class: EffectPosture::LocalWrite,
                    dry_run_available: true,
                    required: true,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![],
            profiles: vec!["compact".to_string(), "full".to_string(), "safe".to_string()],
        },
        Recipe {
            id: "backup-export".to_string(),
            version: 1,
            category: GoalCategory::BackupExport,
            name: "Backup and Export".to_string(),
            description: "Export workspace data for backup or migration.".to_string(),
            effect_posture: EffectPosture::LocalWrite,
            required_capabilities: vec!["storage".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee status --json".to_string(),
                    description: "Verify workspace state".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![],
            profiles: vec!["compact".to_string(), "full".to_string()],
        },
        Recipe {
            id: "rehearsal-risky".to_string(),
            version: 1,
            category: GoalCategory::Rehearsal,
            name: "Rehearse Risky Workflow".to_string(),
            description: "Rehearse a risky workflow in sandbox mode.".to_string(),
            effect_posture: EffectPosture::ReadOnly,
            required_capabilities: vec!["storage".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee lab capture --workspace . --json".to_string(),
                    description: "Capture current state for rehearsal".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: true,
                },
            ],
            degraded_branches: vec![],
            profiles: vec!["safe".to_string()],
        },
        Recipe {
            id: "audit-inspect".to_string(),
            version: 1,
            category: GoalCategory::AuditInspection,
            name: "Inspect Audit Timeline".to_string(),
            description: "Review audit log and trace recent operations.".to_string(),
            effect_posture: EffectPosture::ReadOnly,
            required_capabilities: vec!["storage".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee memory history --workspace . --json".to_string(),
                    description: "View memory history".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![],
            profiles: vec!["compact".to_string(), "full".to_string()],
        },
        Recipe {
            id: "closeout-evidence".to_string(),
            version: 1,
            category: GoalCategory::Closeout,
            name: "Implementation Closeout".to_string(),
            description: "Gather evidence for implementation closeout.".to_string(),
            effect_posture: EffectPosture::ReadOnly,
            required_capabilities: vec!["storage".to_string()],
            steps: vec![
                CommandStep {
                    order: 1,
                    command: "ee status --json".to_string(),
                    description: "Final status check".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
                CommandStep {
                    order: 2,
                    command: "ee check --workspace .".to_string(),
                    description: "Verify workspace integrity".to_string(),
                    effect_class: EffectPosture::ReadOnly,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: false,
                },
            ],
            degraded_branches: vec![],
            profiles: vec!["compact".to_string(), "full".to_string()],
        },
    ]
}

/// Get a recipe by ID.
#[must_use]
pub fn get_recipe(id: &str) -> Option<Recipe> {
    builtin_recipes().into_iter().find(|r| r.id == id)
}

/// List recipes by category.
#[must_use]
pub fn recipes_by_category(category: Option<GoalCategory>) -> Vec<Recipe> {
    match category {
        Some(cat) => builtin_recipes()
            .into_iter()
            .filter(|r| r.category == cat)
            .collect(),
        None => builtin_recipes(),
    }
}

// ============================================================================
// Plan Generation
// ============================================================================

/// Profile for plan generation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlanProfile {
    /// Minimal output, fewer fields.
    Compact,
    /// Full output with all details.
    #[default]
    Full,
    /// Prefer dry-run/rehearsal, refuse missing effect metadata.
    Safe,
}

impl PlanProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Full => "full",
            Self::Safe => "safe",
        }
    }

    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        <Self as std::str::FromStr>::from_str(s).ok()
    }
}

impl std::str::FromStr for PlanProfile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "compact" => Ok(Self::Compact),
            "full" => Ok(Self::Full),
            "safe" => Ok(Self::Safe),
            _ => Err(format!("invalid plan profile: {s}")),
        }
    }
}

/// A generated goal plan.
#[derive(Clone, Debug)]
pub struct GoalPlan {
    pub plan_id: String,
    pub goal_input: String,
    pub classification: GoalClassification,
    pub recipe_id: String,
    pub recipe_version: u32,
    pub profile: PlanProfile,
    pub steps: Vec<CommandStep>,
    pub preconditions: Vec<String>,
    pub stop_conditions: Vec<String>,
    pub degraded_branches: Vec<DegradedBranch>,
    pub dry_run_recommended: bool,
    pub next_inspection_commands: Vec<String>,
    pub rejected_alternatives: Vec<RejectedAlternative>,
    pub task_frame_posture: Option<TaskFramePlanPosture>,
}

impl GoalPlan {
    #[must_use]
    pub fn data_json(&self, include_alternatives: bool) -> JsonValue {
        let mut obj = json!({
            "planId": self.plan_id,
            "goalInput": self.goal_input,
            "classification": {
                "primary": self.classification.primary.as_str(),
                "confidence": self.classification.confidence,
                "ambiguous": self.classification.ambiguous,
            },
            "recipeId": self.recipe_id,
            "recipeVersion": self.recipe_version,
            "profile": self.profile.as_str(),
            "steps": self.steps.iter().map(CommandStep::data_json).collect::<Vec<_>>(),
            "preconditions": self.preconditions,
            "stopConditions": self.stop_conditions,
            "degradedBranches": self.degraded_branches.iter().map(DegradedBranch::data_json).collect::<Vec<_>>(),
            "dryRunRecommended": self.dry_run_recommended,
            "nextInspectionCommands": self.next_inspection_commands,
        });

        if let Some(obj_map) = obj.as_object_mut() {
            if let Some(posture) = &self.task_frame_posture {
                obj_map.insert("taskFramePosture".to_string(), posture.data_json());
            }

            if include_alternatives && !self.rejected_alternatives.is_empty() {
                obj_map.insert(
                    "rejectedAlternatives".to_string(),
                    json!(
                        self.rejected_alternatives
                            .iter()
                            .map(RejectedAlternative::data_json)
                            .collect::<Vec<_>>()
                    ),
                );
            }

            if !self.classification.alternatives.is_empty() {
                if let Some(class_map) = obj_map
                    .get_mut("classification")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    class_map.insert(
                        "alternatives".to_string(),
                        json!(
                            self.classification
                                .alternatives
                                .iter()
                                .map(|c| c.as_str())
                                .collect::<Vec<_>>()
                        ),
                    );
                }
            }
        }

        obj
    }
}

/// A rejected alternative recipe.
#[derive(Clone, Debug)]
pub struct RejectedAlternative {
    pub recipe_id: String,
    pub reason: String,
}

impl RejectedAlternative {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "recipeId": self.recipe_id,
            "reason": self.reason,
        })
    }
}

/// Options for plan generation.
#[derive(Clone, Debug, Default)]
pub struct PlanGoalOptions {
    pub goal: String,
    pub workspace: Option<String>,
    pub profile: PlanProfile,
    pub task_frame_id: Option<String>,
}

/// Passive task-frame state used as plan posture input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskFramePlanPosture {
    pub frame_id: String,
    pub status: String,
    pub root_goal: String,
    pub current_focus: Option<String>,
    pub blocker_count: usize,
    pub active_subgoal_count: usize,
    pub redaction_status: String,
    pub non_executing: bool,
    pub source_id: String,
}

impl TaskFramePlanPosture {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "frameId": self.frame_id,
            "status": self.status,
            "rootGoal": self.root_goal,
            "currentFocus": self.current_focus,
            "blockerCount": self.blocker_count,
            "activeSubgoalCount": self.active_subgoal_count,
            "redactionStatus": self.redaction_status,
            "nonExecuting": self.non_executing,
            "sourceId": self.source_id,
        })
    }
}

/// Generate a plan for a goal.
#[must_use]
pub fn generate_plan(options: &PlanGoalOptions) -> GoalPlan {
    let classification = classify_goal(&options.goal);
    let plan_id = format!("plan-{:08x}", rand_id());
    let task_frame_posture = options.workspace.as_ref().and_then(|workspace| {
        task_frame_plan_posture(workspace, options.task_frame_id.as_deref()).ok()
    });

    let recipe = if classification.primary == GoalCategory::Unknown {
        None
    } else {
        recipes_by_category(Some(classification.primary))
            .into_iter()
            .next()
    };

    let (recipe_id, recipe_version, steps, degraded_branches) = match recipe {
        Some(r) => (r.id, r.version, r.steps, r.degraded_branches),
        None => (
            "unknown".to_string(),
            0,
            vec![CommandStep {
                order: 1,
                command: "ee plan recipe list --json".to_string(),
                description: "List available recipes to find appropriate workflow".to_string(),
                effect_class: EffectPosture::ReadOnly,
                dry_run_available: false,
                required: true,
                stop_on_failure: false,
            }],
            vec![],
        ),
    };

    let dry_run_recommended = options.profile == PlanProfile::Safe;

    let mut next_inspection_commands = vec![
        "ee status --json".to_string(),
        plan_next_inspection_command(&recipe_id),
    ];
    if let Some(posture) = &task_frame_posture {
        next_inspection_commands.push(format!("ee task-frame show {} --json", posture.frame_id));
    }

    let rejected_alternatives: Vec<RejectedAlternative> = classification
        .alternatives
        .iter()
        .filter_map(|cat| {
            recipes_by_category(Some(*cat))
                .into_iter()
                .next()
                .map(|r| RejectedAlternative {
                    recipe_id: r.id,
                    reason: format!("Lower confidence match for category '{}'", cat.as_str()),
                })
        })
        .collect();

    GoalPlan {
        plan_id,
        goal_input: options.goal.clone(),
        classification,
        recipe_id,
        recipe_version,
        profile: options.profile,
        steps,
        preconditions: plan_preconditions(task_frame_posture.as_ref()),
        stop_conditions: vec!["any_step_fails".to_string()],
        degraded_branches,
        dry_run_recommended,
        next_inspection_commands,
        rejected_alternatives,
        task_frame_posture,
    }
}

fn plan_next_inspection_command(recipe_id: &str) -> String {
    if recipe_id == "unknown" {
        "ee plan recipe list --json".to_owned()
    } else {
        format!(
            "ee plan explain {} --json",
            shell_quote_command_arg(recipe_id)
        )
    }
}

fn shell_quote_command_arg(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if value.bytes().all(|byte| {
        matches!(
            byte,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'_'
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b'@'
                | b'+'
                | b'='
        )
    }) {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Return passive task-frame posture for commands that may inspect it without execution.
pub fn task_frame_plan_posture(
    workspace: &str,
    task_frame_id: Option<&str>,
) -> Result<TaskFramePlanPosture, String> {
    let report = show_task_frame(&TaskFrameShowOptions {
        workspace_path: std::path::PathBuf::from(workspace),
        frame_id: task_frame_id.map(ToOwned::to_owned),
        active: task_frame_id.is_none(),
    })
    .map_err(|error| error.message())?;
    let frame = report
        .frame
        .ok_or_else(|| "task-frame posture unavailable".to_owned())?;
    Ok(posture_from_frame(&frame))
}

fn posture_from_frame(frame: &TaskFrameRecord) -> TaskFramePlanPosture {
    TaskFramePlanPosture {
        frame_id: frame.id.clone(),
        status: frame.status.as_str().to_owned(),
        root_goal: frame.root_goal.clone(),
        current_focus: frame.current_focus.clone(),
        blocker_count: frame.blockers.len()
            + frame
                .subgoals
                .iter()
                .filter(|subgoal| subgoal.status.as_str() == "blocked")
                .count(),
        active_subgoal_count: frame.active_subgoal_count(),
        redaction_status: frame.redaction_status.clone(),
        non_executing: true,
        source_id: format!("ee.task_frame.store.v1#{}", frame.id),
    }
}

fn plan_preconditions(task_frame_posture: Option<&TaskFramePlanPosture>) -> Vec<String> {
    let mut preconditions = vec!["workspace_initialized".to_owned()];
    if let Some(posture) = task_frame_posture {
        preconditions.push(format!("task_frame_status:{}", posture.status));
        if posture.blocker_count > 0 {
            preconditions.push("task_frame_has_blockers".to_owned());
        }
    }
    preconditions
}

/// Generate a pseudo-random ID (deterministic for testing when seeded).
fn rand_id() -> u32 {
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        return now.subsec_nanos() ^ saturating_unix_seconds_u32(now);
    }
    u32::from_ne_bytes(bytes)
}

fn saturating_unix_seconds_u32(duration: std::time::Duration) -> u32 {
    u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
}

// ============================================================================
// Explain
// ============================================================================

/// Explanation of a plan or recipe selection.
#[derive(Clone, Debug)]
pub struct PlanExplanation {
    pub plan_id: Option<String>,
    pub recipe_id: String,
    pub goal_input: Option<String>,
    pub classification_reasoning: String,
    pub selection_reasoning: String,
    pub rejected_alternatives: Vec<RejectedAlternative>,
    pub posture_inputs: Vec<String>,
    pub next_inspection: Vec<String>,
}

impl PlanExplanation {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "planId": self.plan_id,
            "recipeId": self.recipe_id,
            "goalInput": self.goal_input,
            "classificationReasoning": self.classification_reasoning,
            "selectionReasoning": self.selection_reasoning,
            "rejectedAlternatives": self.rejected_alternatives.iter().map(RejectedAlternative::data_json).collect::<Vec<_>>(),
            "postureInputs": self.posture_inputs,
            "nextInspection": self.next_inspection,
        })
    }
}

/// Explain a recipe selection.
#[must_use]
pub fn explain_recipe_selection(recipe_id: &str) -> Option<PlanExplanation> {
    let recipe = get_recipe(recipe_id)?;

    Some(PlanExplanation {
        plan_id: None,
        recipe_id: recipe.id.clone(),
        goal_input: None,
        classification_reasoning: format!(
            "Recipe '{}' is designed for {} goals.",
            recipe.id,
            recipe.category.description()
        ),
        selection_reasoning: format!(
            "This recipe provides {} steps with {} effect posture.",
            recipe.steps.len(),
            recipe.effect_posture.as_str()
        ),
        rejected_alternatives: vec![],
        posture_inputs: vec!["workspace_status".to_string(), "capabilities".to_string()],
        next_inspection: vec![
            "ee status --json".to_string(),
            format!("ee plan recipe show {} --json", recipe.id),
        ],
    })
}

// ============================================================================
// Plan Recommend (EE-jfd9)
// ============================================================================

pub const PLAN_RECOMMEND_SCHEMA_V1: &str = "ee.plan.recommend.v1";
pub const RECIPE_SEMANTIC_UNAVAILABLE: &str = "semantic_search_unavailable";

/// A catalog recipe and its actual source metadata. Built-ins and stored
/// procedures retain distinct provenance even when their text is identical.
#[derive(Clone)]
pub(crate) struct RecipeCatalogEntry {
    pub recipe: Recipe,
    pub source_kind: &'static str,
    pub source_id: String,
    pub maturity: Option<String>,
    pub evidence_uris: Vec<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
}

impl RecipeCatalogEntry {
    pub fn data_json(&self) -> JsonValue {
        self.decorate(self.recipe.data_json())
    }

    pub fn summary_json(&self) -> JsonValue {
        self.decorate(self.recipe.summary_json())
    }

    fn decorate(&self, mut value: JsonValue) -> JsonValue {
        value["sourceKind"] = json!(self.source_kind);
        value["sourceId"] = json!(self.source_id);
        if let Some(maturity) = &self.maturity {
            value["maturity"] = json!(maturity);
        }
        value["evidenceUris"] = json!(self.evidence_uris);
        if self.source_kind != "static_command_catalog" {
            value["catalogSource"] = JsonValue::Null;
            value["version"] = JsonValue::Null;
            value["decisionBoundary"] = json!("stored_instructions_only");
            if let Some(steps) = value.get_mut("steps").and_then(JsonValue::as_array_mut) {
                for step in steps {
                    for field in ["required", "stopOnFailure", "dryRunAvailable"] {
                        step[field] = JsonValue::Null;
                    }
                }
            }
        }
        value
    }
}

fn recipe_storage_error(error: impl std::fmt::Display) -> DomainError {
    DomainError::Storage {
        message: format!("Recipe storage failed: {}", recipe_text(&error.to_string())),
        repair: Some("ee doctor --workspace . --json".to_owned()),
    }
}

fn recipe_search_error(error: impl std::fmt::Display) -> DomainError {
    DomainError::SearchIndex {
        message: format!(
            "Recipe retrieval failed: {}",
            recipe_text(&error.to_string())
        ),
        repair: Some("ee doctor --workspace . --json".to_owned()),
    }
}

pub(crate) fn recipe_text(text: &str) -> String {
    crate::output::jsonl_export::redact_content(text, crate::models::RedactionLevel::Standard)
}

fn recipe_evidence_text(uri: &str) -> String {
    // Canonical internal references are addressable identifiers, not arbitrary
    // high-entropy prose. Keep the secret-pattern guard and require the whole
    // suffix to validate, so query strings and malformed IDs get normal redaction.
    let no_secret =
        crate::output::jsonl_export::redact_content(uri, crate::models::RedactionLevel::Minimal)
            == uri;
    let internal = uri
        .strip_prefix("ee://memory/")
        .is_some_and(|id| id.parse::<crate::models::MemoryId>().is_ok())
        || uri
            .strip_prefix("ee://curation-candidate/")
            .is_some_and(|id| {
                crate::core::curate::validate_curate_candidate_id(id)
                    .is_ok_and(|canonical| canonical == id)
            });
    if no_secret && internal {
        uri.to_owned()
    } else {
        recipe_text(uri)
    }
}

pub(crate) fn recipe_searchable_text(text: &str) -> String {
    use crate::output::jsonl_export::{
        REDACTED_ID_PLACEHOLDER, REDACTED_PATH_PLACEHOLDER, REDACTED_PLACEHOLDER,
    };
    recipe_text(text)
        .replace(REDACTED_PLACEHOLDER, " ")
        .replace(REDACTED_PATH_PLACEHOLDER, " ")
        .replace(REDACTED_ID_PLACEHOLDER, " ")
}

fn stored_recipe_entry(row: StoredPlanRecipe) -> Result<RecipeCatalogEntry, DomainError> {
    // IDs are used verbatim for addressable provenance. Refuse secret-bearing
    // identifiers instead of returning a secret or inventing a replacement ID.
    for id in [&row.id, &row.workspace_id] {
        if crate::output::jsonl_export::redact_content(id, crate::models::RedactionLevel::Minimal)
            != *id
        {
            return Err(recipe_storage_error(
                "a recipe identifier contains secret material",
            ));
        }
    }
    let steps: Vec<JsonValue> =
        serde_json::from_str(&row.steps_json).map_err(recipe_storage_error)?;
    let evidence: Vec<String> =
        serde_json::from_str(&row.evidence_uris_json).map_err(recipe_storage_error)?;
    let mut evidence_uris = evidence
        .into_iter()
        .map(|uri| recipe_evidence_text(&uri))
        .collect::<Vec<_>>();
    evidence_uris.sort();
    evidence_uris.dedup();
    let created_at = DateTime::parse_from_rfc3339(&row.created_at)
        .map_err(recipe_storage_error)?
        .with_timezone(&Utc);
    let updated_at = DateTime::parse_from_rfc3339(&row.updated_at)
        .map_err(recipe_storage_error)?
        .with_timezone(&Utc);
    Ok(RecipeCatalogEntry {
        source_kind: "stored_plan_recipe",
        source_id: format!("ee://workspace/{}/plan-recipe/{}", row.workspace_id, row.id),
        maturity: Some(row.maturity),
        evidence_uris,
        created_at: Some(created_at),
        updated_at: Some(updated_at),
        recipe: Recipe {
            id: row.id,
            version: 1, // JSON exposes no historical version for stored rows.
            category: GoalCategory::Unknown,
            name: recipe_text(&row.name),
            description: recipe_text(&row.when_to_use),
            effect_posture: EffectPosture::Unknown,
            required_capabilities: Vec::new(),
            steps: steps
                .into_iter()
                .enumerate()
                .map(|(index, step)| CommandStep {
                    order: u32::try_from(index + 1).unwrap_or(u32::MAX),
                    command: recipe_text(
                        step.as_str()
                            .or_else(|| step.get("command").and_then(JsonValue::as_str))
                            .map_or_else(|| step.to_string(), str::to_owned)
                            .as_str(),
                    ),
                    description: "Stored instruction; effects have not been evaluated.".to_owned(),
                    effect_class: EffectPosture::Unknown,
                    dry_run_available: false,
                    required: true,
                    stop_on_failure: true,
                })
                .collect(),
            degraded_branches: Vec::new(),
            profiles: Vec::new(),
        },
    })
}

pub(crate) fn recipe_catalog(
    workspace: &Path,
    database: Option<&Path>,
) -> Result<Vec<RecipeCatalogEntry>, DomainError> {
    let mut catalog = builtin_recipes()
        .into_iter()
        .map(|recipe| RecipeCatalogEntry {
            source_id: recipe_source_id(&recipe.id),
            evidence_uris: vec![format!("ee://plan/recipe/{}", recipe.id)],
            recipe,
            source_kind: "static_command_catalog",
            maturity: None,
            created_at: None,
            updated_at: None,
        })
        .collect::<Vec<_>>();
    let workspace = workspace.canonicalize().map_err(recipe_storage_error)?;
    let path = database
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace.join(".ee/ee.db"));
    if !path.try_exists().map_err(recipe_storage_error)? {
        if database.is_some() {
            return Err(crate::core::storeless_workspace_error(&path));
        }
        return Ok(catalog);
    }
    let db = DbConnection::open_file_read_only(&path).map_err(recipe_storage_error)?;
    db.begin_read_snapshot().map_err(recipe_storage_error)?;
    if db.needs_migration().map_err(recipe_storage_error)? {
        return Err(DomainError::MigrationRequired {
            message: "The recipe store needs migration.".to_owned(),
            repair: Some("ee migrate run --workspace . --json".to_owned()),
        });
    }
    let requested_id = crate::core::workspace::stable_workspace_id(&workspace);
    if let Some(row) =
        crate::core::workspace::select_existing_workspace_row(&db, &requested_id, &[&workspace])?
    {
        for recipe in db
            .list_plan_recipes(&row.id)
            .map_err(recipe_storage_error)?
        {
            catalog.push(stored_recipe_entry(recipe)?);
        }
        for rule in db
            .list_procedural_rules(&row.id, None, None, false)
            .map_err(recipe_storage_error)?
        {
            if rule.superseded_by.is_some()
                || !matches!(rule.maturity.as_str(), "draft" | "candidate" | "validated")
            {
                continue;
            }
            catalog.push(procedural_rule_entry(&db, &workspace, rule)?);
        }
    }
    catalog.sort_by(|left, right| left.recipe.id.cmp(&right.recipe.id));
    db.commit_read_snapshot().map_err(recipe_storage_error)?;
    Ok(catalog)
}

fn procedural_rule_entry(
    db: &DbConnection,
    workspace: &Path,
    rule: StoredProceduralRule,
) -> Result<RecipeCatalogEntry, DomainError> {
    let scope = rule
        .scope
        .parse::<crate::models::RuleScope>()
        .map_err(recipe_storage_error)?;
    let pattern = crate::search::normalize_rule_scope_pattern(
        workspace,
        scope,
        rule.scope_pattern.as_deref(),
    )
    .map_err(recipe_storage_error)?;
    let mut evidence = Vec::new();
    for id in db
        .get_rule_source_memory_ids(&rule.id)
        .map_err(recipe_storage_error)?
    {
        // A foreign source is not permission to reveal its identity or body.
        if db
            .get_memory(&id)
            .map_err(recipe_storage_error)?
            .is_some_and(|memory| memory.workspace_id == rule.workspace_id)
        {
            evidence.push(format!("ee://memory/{id}"));
        }
    }
    let applicability = match pattern {
        Some(pattern) => format!(
            "Scope: {} ({pattern}). Check this scope before using the rule.",
            scope.as_str()
        ),
        None => format!("Scope: {}.", scope.as_str()),
    };
    let mut entry = stored_recipe_entry(StoredPlanRecipe {
        id: rule.id.clone(),
        workspace_id: rule.workspace_id.clone(),
        name: recipe_text(&rule.content)
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect(),
        when_to_use: applicability,
        steps_json: json!([rule.content]).to_string(),
        evidence_uris_json: json!(evidence).to_string(),
        maturity: rule.maturity,
        confidence: f64::from(rule.confidence),
        helpful_count: u64::from(rule.positive_feedback_count),
        harmful_count: u64::from(rule.negative_feedback_count),
        created_at: rule.created_at,
        updated_at: rule.updated_at,
        last_recommended_at: None,
    })?;
    entry.source_kind = "procedural_rule";
    entry.source_id = format!("ee://workspace/{}/rule/{}", rule.workspace_id, rule.id);
    Ok(entry)
}

pub(crate) fn find_recipe(
    workspace: &Path,
    database: Option<&Path>,
    id: &str,
) -> Result<Option<RecipeCatalogEntry>, DomainError> {
    Ok(recipe_catalog(workspace, database)?
        .into_iter()
        .find(|entry| entry.recipe.id == id))
}

pub(crate) struct RecipeSaveOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub name: String,
    pub when_to_use: String,
    pub steps: Vec<String>,
    pub evidence_uris: Vec<String>,
    pub dry_run: bool,
}

/// Save user-supplied instructions as a draft, without evaluating or executing
/// them. Recipe and audit commit together; a preview opens the store read-only.
pub(crate) fn save_recipe(options: &RecipeSaveOptions) -> Result<JsonValue, DomainError> {
    if [&options.name, &options.when_to_use]
        .into_iter()
        .chain(options.steps.iter())
        .any(|text| recipe_searchable_text(text).trim().is_empty())
        || options.steps.is_empty()
    {
        return Err(DomainError::Usage {
            message: "Recipe name, applicability, and at least one step must retain non-empty text after redaction.".to_owned(),
            repair: Some("ee plan recipe save --help".to_owned()),
        });
    }
    let workspace = options
        .workspace_path
        .canonicalize()
        .map_err(recipe_storage_error)?;
    let database = options
        .database_path
        .clone()
        .unwrap_or_else(|| workspace.join(".ee/ee.db"));
    if !database.try_exists().map_err(recipe_storage_error)? {
        return Err(crate::core::storeless_workspace_error(&database));
    }
    let db = if options.dry_run {
        DbConnection::open_file_read_only(&database)
    } else {
        DbConnection::open_file(&database)
    }
    .map_err(recipe_storage_error)?;
    if options.dry_run {
        db.begin_read_snapshot().map_err(recipe_storage_error)?;
    }
    if db.needs_migration().map_err(recipe_storage_error)? {
        return Err(DomainError::MigrationRequired {
            message: "The recipe store needs migration.".to_owned(),
            repair: Some("ee migrate run --workspace . --json".to_owned()),
        });
    }
    let requested_id = crate::core::workspace::stable_workspace_id(&workspace);
    let workspace =
        crate::core::workspace::select_existing_workspace_row(&db, &requested_id, &[&workspace])?
            .ok_or_else(|| DomainError::Usage {
            message: "Initialize the selected workspace before saving a recipe.".to_owned(),
            repair: Some("ee init --workspace . --json".to_owned()),
        })?;
    let timestamp = Utc::now().to_rfc3339();
    let mut evidence = options
        .evidence_uris
        .iter()
        .map(|uri| recipe_evidence_text(uri.trim()))
        .filter(|uri| !uri.is_empty())
        .collect::<Vec<_>>();
    evidence.sort();
    evidence.dedup();
    let row = StoredPlanRecipe {
        id: format!("plrec_{}", uuid::Uuid::now_v7().simple()),
        workspace_id: workspace.id.clone(),
        name: recipe_text(options.name.trim()),
        when_to_use: recipe_text(options.when_to_use.trim()),
        steps_json: json!(
            options
                .steps
                .iter()
                .map(|step| recipe_text(step.trim()))
                .collect::<Vec<_>>()
        )
        .to_string(),
        evidence_uris_json: json!(evidence).to_string(),
        maturity: "draft".to_owned(),
        confidence: 0.0,
        helpful_count: 0,
        harmful_count: 0,
        created_at: timestamp.clone(),
        updated_at: timestamp,
        last_recommended_at: None,
    };
    let mut recipe = stored_recipe_entry(row.clone())?.data_json();
    let audit_id = if options.dry_run {
        db.commit_read_snapshot().map_err(recipe_storage_error)?;
        recipe["id"] = JsonValue::Null;
        recipe["sourceId"] = JsonValue::Null;
        recipe["sourceKind"] = json!("recipe_draft_preview");
        None
    } else {
        let id = crate::db::generate_audit_id();
        db.with_transaction(|| {
            db.insert_plan_recipe(&row)?;
            db.insert_audit(
                &id,
                &crate::db::CreateAuditInput {
                    workspace_id: Some(workspace.id.clone()),
                    actor: Some("ee plan recipe save".to_owned()),
                    action: crate::db::audit_actions::PLAN_RECIPE_SAVE.to_owned(),
                    target_type: Some("plan_recipe".to_owned()),
                    target_id: Some(row.id.clone()),
                    details: Some(recipe.to_string()),
                },
            )
        })
        .map_err(recipe_storage_error)?;
        Some(id)
    };
    Ok(
        json!({ "command":"plan recipe save", "dryRun":options.dry_run, "persisted":!options.dry_run,
        "recipe":recipe, "auditId":audit_id }),
    )
}

/// Options for recommending recipes based on task description.
#[derive(Clone, Debug)]
pub struct PlanRecommendOptions {
    pub task: String,
    pub limit: u32,
    pub min_score: f64,
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeScoreComponents {
    pub text_similarity: f64,
    pub semantic_similarity: f64,
    pub maturity_score: f64,
    pub recency_decay: f64,
    pub evidence_count: f64,
}

/// A ranked recipe recommendation.
#[derive(Clone, Debug)]
pub struct RecipeRecommendation {
    pub recipe_id: String,
    pub recipe_name: String,
    pub category: GoalCategory,
    pub score: f64,
    pub components: RecipeScoreComponents,
    pub rank: usize,
    pub source_kind: &'static str,
    pub source_id: String,
    pub evidence_uris: Vec<String>,
    pub maturity: Option<String>,
    pub match_reasons: Vec<String>,
    pub steps_count: usize,
    pub effect_posture: EffectPosture,
}

/// Report from plan recommend.
#[derive(Clone, Debug)]
pub struct PlanRecommendReport {
    pub schema: String,
    pub task: String,
    pub recommendations: Vec<RecipeRecommendation>,
    pub total_recipes_considered: usize,
    pub matches_found: usize,
    pub recency_anchor: Option<String>,
    pub degraded: Vec<JsonValue>,
}

impl PlanRecommendReport {
    #[must_use]
    pub fn empty(task: &str) -> Self {
        Self {
            schema: PLAN_RECOMMEND_SCHEMA_V1.to_owned(),
            task: task.to_owned(),
            recommendations: Vec::new(),
            total_recipes_considered: 0,
            matches_found: 0,
            recency_anchor: None,
            degraded: Vec::new(),
        }
    }
}

fn validate_recommend_options(options: &PlanRecommendOptions) -> Result<(), DomainError> {
    if options.task.trim().is_empty()
        || recipe_searchable_text(&options.task).trim().is_empty()
        || options.limit == 0
        || !options.min_score.is_finite()
        || !(0.0..=1.0).contains(&options.min_score)
    {
        return Err(DomainError::Usage {
            message: "Recipe recommendation requires searchable task text after redaction, a positive limit, and a finite --min-score between 0 and 1.".to_owned(),
            repair: Some("ee plan recommend --help".to_owned()),
        });
    }
    Ok(())
}

/// Retrieve stored and built-in recipes without modifying the store or indexes.
pub fn recommend_recipes(
    options: &PlanRecommendOptions,
) -> Result<PlanRecommendReport, DomainError> {
    validate_recommend_options(options)?;
    // Finish the DB read before any optional model preparation or download.
    let catalog = recipe_catalog(&options.workspace_path, options.database_path.as_deref())?;
    recommend_with_prepared_embedder(options, catalog)
}

fn recommend_with_prepared_embedder(
    options: &PlanRecommendOptions,
    catalog: Vec<RecipeCatalogEntry>,
) -> Result<PlanRecommendReport, DomainError> {
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        let database = options
            .database_path
            .clone()
            .unwrap_or_else(|| options.workspace_path.join(".ee/ee.db"));
        let preparation = crate::core::index::prepare_search_embedder_for_workspace(
            &cx,
            &options.workspace_path,
            &database,
        )
        .await;
        cx.checkpoint().map_err(recipe_search_error)?;
        recommend_from_catalog(
            &cx,
            options,
            catalog,
            preparation.as_ref().ok().map(|p| p.fast_embedder.as_ref()),
        )
        .await
    })
    .map_err(recipe_search_error)?
}

pub(crate) async fn recommend_from_catalog(
    cx: &asupersync::Cx,
    options: &PlanRecommendOptions,
    catalog: Vec<RecipeCatalogEntry>,
    embedder: Option<&dyn crate::search::Embedder>,
) -> Result<PlanRecommendReport, DomainError> {
    validate_recommend_options(options)?;
    let task = recipe_text(options.task.trim());
    let documents = catalog
        .iter()
        .map(|entry| {
            let recipe = &entry.recipe;
            crate::search::IndexableDocument::new(
                &recipe.id,
                recipe_searchable_text(&format!(
                    "{}\n{}\n{}",
                    recipe.name,
                    recipe.description,
                    recipe
                        .steps
                        .iter()
                        .map(|s| s.command.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                )),
            )
        })
        .collect::<Vec<_>>();
    let query = recipe_searchable_text(&task);
    let scores = crate::search::search_catalog(cx, &documents, &query, embedder)
        .await
        .map_err(recipe_search_error)?;
    let anchor = catalog.iter().filter_map(|entry| entry.updated_at).max();
    let mut report = PlanRecommendReport::empty(&task);
    report.total_recipes_considered = catalog.len();
    report.recency_anchor = anchor.map(|time| time.to_rfc3339());
    if !scores.semantic_available {
        report.degraded.push(json!({
            "code": RECIPE_SEMANTIC_UNAVAILABLE,
            "severity": "warning",
            "message": "Recipe semantic search is unavailable; recommendations use Frankensearch lexical retrieval with redistributed weights."
        }));
    }
    let text_weight = if scores.semantic_available {
        0.30
    } else {
        0.55
    };
    let semantic_weight = if cfg!(feature = "lexical-bm25") {
        0.25
    } else {
        0.55
    };
    let mut ranked = Vec::new();
    for entry in catalog {
        let text = scores.lexical.get(&entry.recipe.id).copied().unwrap_or(0.0);
        let semantic = scores
            .semantic
            .get(&entry.recipe.id)
            .copied()
            .unwrap_or(0.0);
        // Metadata alone must never recommend an unrelated procedure. Semantic
        // only matches need at least 0.5 cosine similarity.
        if text <= 0.0 && semantic < 0.5 {
            continue;
        }
        let maturity = match entry.maturity.as_deref() {
            Some("draft" | "candidate") => 0.3,
            Some("validated") => 0.6,
            Some("promoted") => 1.0,
            _ => 0.0, // Static catalog entries have no learned maturity.
        };
        // Anchor decay to recorded data, never wall time. A 30-day half-life
        // preserves recency preference without changing identical reads.
        let recency = match (anchor, entry.updated_at) {
            (Some(anchor), Some(updated)) => {
                2.0_f64.powf(-((anchor - updated).num_seconds().max(0) as f64) / (30.0 * 86400.0))
            }
            _ => 0.0,
        };
        let evidence_count = entry
            .evidence_uris
            .iter()
            .filter(|uri| {
                entry.source_kind != "static_command_catalog"
                    && !uri.trim().is_empty()
                    && !uri.contains("[REDACTED")
            })
            .count();
        let evidence = evidence_count as f64 / (1.0 + evidence_count as f64);
        let components = RecipeScoreComponents {
            text_similarity: text,
            semantic_similarity: semantic,
            maturity_score: maturity,
            recency_decay: recency,
            evidence_count: evidence,
        };
        let score = text_weight * text
            + semantic_weight * semantic
            + 0.20 * maturity
            + 0.10 * recency
            + 0.15 * evidence;
        if score < options.min_score {
            continue;
        }
        let mut reasons = vec![
            format!("Frankensearch text similarity {text:.6}; semantic similarity {semantic:.6}."),
            format!(
                "Maturity {}; {} supporting evidence links (catalog self-references excluded); recency {recency:.6} against the recorded catalog anchor.",
                entry
                    .maturity
                    .as_deref()
                    .unwrap_or("not learned (built-in catalog)"),
                evidence_count
            ),
        ];
        if entry.source_kind == "procedural_rule" {
            reasons.push(entry.recipe.description.clone());
        }
        ranked.push((
            RecipeRecommendation {
                recipe_id: entry.recipe.id,
                recipe_name: entry.recipe.name,
                category: entry.recipe.category,
                score,
                components,
                rank: 0,
                source_kind: entry.source_kind,
                source_id: entry.source_id,
                evidence_uris: entry.evidence_uris,
                maturity: entry.maturity,
                match_reasons: reasons,
                steps_count: entry.recipe.steps.len(),
                effect_posture: entry.recipe.effect_posture,
            },
            entry.created_at,
        ));
    }
    ranked.sort_by(|a, b| {
        b.0.score
            .total_cmp(&a.0.score)
            .then_with(|| a.0.recipe_id.cmp(&b.0.recipe_id))
    });
    // Epsilon in a pairwise comparator is non-transitive. Form deterministic
    // groups relative to each group's highest score, then apply ADR tie keys.
    let mut start = 0;
    while start < ranked.len() {
        let maximum = ranked[start].0.score;
        let mut end = start + 1;
        while end < ranked.len() && maximum - ranked[end].0.score <= 1e-6 {
            end += 1;
        }
        ranked[start..end].sort_by(|a, b| {
            b.0.components
                .maturity_score
                .total_cmp(&a.0.components.maturity_score)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.0.recipe_id.cmp(&b.0.recipe_id))
        });
        start = end;
    }
    report.matches_found = ranked.len();
    ranked.truncate(options.limit as usize);
    report.recommendations = ranked
        .into_iter()
        .enumerate()
        .map(|(index, (mut recommendation, _))| {
            recommendation.rank = index + 1;
            recommendation
        })
        .collect();
    Ok(report)
}

// ============================================================================
// Plan Explain (EE-jfd9)
// ============================================================================

/// Report from explaining a recipe.
#[derive(Clone, Debug)]
pub struct PlanExplainReport {
    pub schema: String,
    pub recipe_id: String,
    pub found: bool,
    pub recipe_name: Option<String>,
    pub category: Option<String>,
    pub description: Option<String>,
    pub when_to_use: Option<String>,
    pub steps: Vec<String>,
    pub effect_posture: Option<String>,
    pub maturity: Option<String>,
    pub evidence_uris: Vec<String>,
    pub source_kind: Option<String>,
    pub source_id: Option<String>,
    pub task_evaluation: Option<PlanRecommendReport>,
}

impl PlanExplainReport {
    #[must_use]
    pub fn not_found(recipe_id: &str) -> Self {
        Self {
            schema: PLAN_EXPLAIN_SCHEMA_V1.to_owned(),
            recipe_id: recipe_id.to_owned(),
            found: false,
            recipe_name: None,
            category: None,
            description: None,
            when_to_use: None,
            steps: Vec::new(),
            effect_posture: None,
            maturity: None,
            evidence_uris: Vec::new(),
            source_kind: None,
            source_id: None,
            task_evaluation: None,
        }
    }
}

/// Explain why a recipe exists and when to use it.
pub fn explain_recipe(
    workspace: &Path,
    database: Option<&Path>,
    recipe_id: &str,
) -> Result<PlanExplainReport, DomainError> {
    let entry = find_recipe(workspace, database, recipe_id)?;
    Ok(explain_catalog_entry(recipe_id, entry))
}

fn explain_catalog_entry(recipe_id: &str, entry: Option<RecipeCatalogEntry>) -> PlanExplainReport {
    match entry {
        Some(entry) => {
            let r = entry.recipe;
            PlanExplainReport {
                schema: PLAN_EXPLAIN_SCHEMA_V1.to_owned(),
                recipe_id: recipe_id.to_owned(),
                found: true,
                recipe_name: Some(r.name.clone()),
                category: Some(r.category.as_str().to_owned()),
                description: Some(r.description.clone()),
                when_to_use: Some(r.description),
                steps: r.steps.iter().map(|s| s.command.clone()).collect(),
                effect_posture: Some(r.effect_posture.as_str().to_owned()),
                maturity: entry.maturity,
                evidence_uris: entry.evidence_uris,
                source_kind: Some(entry.source_kind.to_owned()),
                source_id: Some(entry.source_id),
                task_evaluation: None,
            }
        }
        None => PlanExplainReport::not_found(recipe_id),
    }
}

/// Explain a recipe against the same complete snapshot and scorer used for
/// recommendations. No recommendation history or wall-clock rank is invented.
pub fn explain_recipe_for_task(
    workspace: &Path,
    database: Option<&Path>,
    recipe_id: &str,
    task: &str,
) -> Result<PlanExplainReport, DomainError> {
    let options = PlanRecommendOptions {
        workspace_path: workspace.to_path_buf(),
        database_path: database.map(Path::to_path_buf),
        task: task.to_owned(),
        limit: u32::MAX,
        min_score: 0.0,
    };
    validate_recommend_options(&options)?;
    let catalog = recipe_catalog(workspace, database)?;
    let mut report = explain_catalog_entry(
        recipe_id,
        catalog.iter().find(|e| e.recipe.id == recipe_id).cloned(),
    );
    if report.found {
        report.task_evaluation = Some(recommend_with_prepared_embedder(&options, catalog)?);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    #[test]
    fn goal_category_roundtrip() {
        for cat in GoalCategory::all() {
            let s = cat.as_str();
            assert!(!s.is_empty());
            assert!(!cat.description().is_empty());
        }
    }

    #[test]
    fn classify_goal_init() {
        let result = classify_goal("initialize workspace");
        assert_eq!(result.primary, GoalCategory::Init);
        assert!(result.confidence > 0.5);
    }

    #[test]
    fn classify_goal_unknown() {
        let result = classify_goal("xyzzy random gibberish");
        assert_eq!(result.primary, GoalCategory::Unknown);
        assert!(result.ambiguous);
    }

    #[test]
    fn classify_goal_context() {
        let result = classify_goal("prepare context for task");
        assert_eq!(result.primary, GoalCategory::PreTaskBriefing);
    }

    #[test]
    fn classify_goal_repair() {
        let result = classify_goal("fix degraded state");
        assert_eq!(result.primary, GoalCategory::DegradedRepair);
    }

    #[test]
    fn builtin_recipes_not_empty() {
        let recipes = builtin_recipes();
        assert!(!recipes.is_empty());
        assert!(recipes.len() >= 10);
    }

    #[test]
    fn builtin_recipes_use_canonical_pack_command_for_context_packs() -> TestResult {
        let mut commands = Vec::new();
        for recipe in builtin_recipes() {
            for step in recipe.steps {
                commands.push(step.command);
            }
            for branch in recipe.degraded_branches {
                for step in branch.alternative_steps {
                    commands.push(step.command);
                }
            }
        }

        let stale_context_prefix = ["ee", "context"].join(" ");
        let stale_context_commands = commands
            .iter()
            .filter(|command| command.starts_with(&format!("{stale_context_prefix} ")))
            .cloned()
            .collect::<Vec<_>>();
        if !stale_context_commands.is_empty() {
            return Err(format!(
                "built-in plan recipes must recommend canonical `ee pack`, not the soft-deprecated context alias: {stale_context_commands:?}"
            ));
        }
        if !commands
            .iter()
            .any(|command| command.starts_with("ee pack "))
        {
            return Err("expected at least one built-in recipe to recommend `ee pack`".to_string());
        }
        Ok(())
    }

    #[test]
    fn builtin_recipes_expose_static_catalog_provenance() -> TestResult {
        let recipe = get_recipe("init-workspace")
            .ok_or_else(|| "init-workspace recipe missing".to_string())?;
        let json = recipe.summary_json();

        assert_eq!(
            json.get("sourceKind").and_then(JsonValue::as_str),
            Some("static_command_catalog")
        );
        assert_eq!(
            json.get("catalogSource").and_then(JsonValue::as_str),
            Some(PLAN_RECIPE_CATALOG_SOURCE_V1)
        );
        assert_eq!(
            json.get("sourceId").and_then(JsonValue::as_str),
            Some("ee.plan.recipe_catalog.v1#init-workspace")
        );
        assert_eq!(
            json.get("goalPlanning").and_then(JsonValue::as_bool),
            Some(false)
        );
        assert_eq!(
            json.get("decisionBoundary").and_then(JsonValue::as_str),
            Some("mechanical_command_catalog_only")
        );
        Ok(())
    }

    #[test]
    fn get_recipe_by_id() -> TestResult {
        let recipe = get_recipe("init-workspace");
        assert!(recipe.is_some());
        let r = recipe.ok_or_else(|| "init-workspace recipe missing".to_string())?;
        assert_eq!(r.category, GoalCategory::Init);
        Ok(())
    }

    #[test]
    fn generate_plan_for_init() {
        let options = PlanGoalOptions {
            goal: "initialize new workspace".to_string(),
            workspace: None,
            profile: PlanProfile::Full,
            task_frame_id: None,
        };
        let plan = generate_plan(&options);
        assert_eq!(plan.recipe_id, "init-workspace");
        assert!(!plan.steps.is_empty());
        assert!(
            plan.next_inspection_commands
                .iter()
                .any(|command| command == "ee plan explain init-workspace --json")
        );
        assert!(
            plan.next_inspection_commands
                .iter()
                .all(|command| !command.contains(&plan.plan_id)),
            "next inspection commands must reference durable recipe IDs, not ephemeral plan IDs"
        );
    }

    #[test]
    fn generate_plan_unknown_goal() {
        let options = PlanGoalOptions {
            goal: "xyzzy".to_string(),
            workspace: None,
            profile: PlanProfile::Full,
            task_frame_id: None,
        };
        let plan = generate_plan(&options);
        assert_eq!(plan.recipe_id, "unknown");
        assert!(plan.classification.ambiguous);
        assert!(
            plan.next_inspection_commands
                .iter()
                .any(|command| command == "ee plan recipe list --json")
        );
    }

    #[test]
    fn rand_id_fallback_seconds_saturate_before_narrowing() {
        let duration = std::time::Duration::from_secs(u64::from(u32::MAX) + 1);
        assert_eq!(saturating_unix_seconds_u32(duration), u32::MAX);
    }

    #[test]
    fn explain_recipe_exists() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|e| e.to_string())?;
        let exp =
            explain_recipe(workspace.path(), None, "init-workspace").map_err(|e| e.message())?;
        assert!(exp.found);
        assert_eq!(exp.recipe_id, "init-workspace");
        assert_eq!(exp.evidence_uris, ["ee://plan/recipe/init-workspace"]);
        assert_eq!(exp.maturity, None);
        let entry = find_recipe(workspace.path(), None, "init-workspace")
            .map_err(|e| e.message())?
            .ok_or("missing catalog recipe")?;
        assert!(entry.data_json().get("maturity").is_none());
        #[cfg(feature = "lexical-bm25")]
        {
            let report = lexical_recommend(&PlanRecommendOptions {
                task: "initialize workspace".to_owned(),
                limit: 5,
                min_score: 0.0,
                workspace_path: workspace.path().to_path_buf(),
                database_path: None,
            })?;
            assert!(!report.recommendations.is_empty());
            for recommendation in &report.recommendations {
                assert!(recommendation.maturity.is_none());
                assert_eq!(recommendation.components.maturity_score, 0.0);
            }
            let rendered = crate::output::render_plan_recommend_json(&report);
            assert!(!rendered.contains("\"maturity\":"));
            assert!(
                crate::core::degraded_honesty::validate_no_unsupported_evidence_claims(
                    "plan recommend",
                    true,
                    false,
                    &rendered
                )
                .passed
            );
        }
        Ok(())
    }

    #[test]
    fn generate_plan_can_reference_task_frame_posture_without_execution() -> TestResult {
        let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
        let created = crate::core::task_frame::create_task_frame(
            &crate::core::task_frame::TaskFrameCreateOptions {
                workspace_path: workspace.path().to_path_buf(),
                goal: "Ship task frame support".to_owned(),
                actor: "cod-pane6".to_owned(),
                status: crate::core::task_frame::TaskFrameStatus::Active,
                current_focus: Some("handoff posture".to_owned()),
                blockers: vec!["blocked on review".to_owned()],
                evidence_links: Vec::new(),
                created_at: Some("2026-05-04T00:00:00Z".to_owned()),
                dry_run: false,
            },
        )
        .map_err(|error| error.message())?;
        let frame_id = created.frame.ok_or_else(|| "missing frame".to_owned())?.id;
        let options = PlanGoalOptions {
            goal: "continue task frame work".to_owned(),
            workspace: Some(workspace.path().display().to_string()),
            profile: PlanProfile::Safe,
            task_frame_id: Some(frame_id.clone()),
        };
        let plan = generate_plan(&options);
        let posture = plan
            .task_frame_posture
            .ok_or_else(|| "missing task-frame posture".to_owned())?;

        assert_eq!(posture.frame_id, frame_id);
        assert_eq!(posture.status, "active");
        assert!(posture.non_executing);
        assert_eq!(posture.blocker_count, 1);
        assert!(
            plan.preconditions
                .iter()
                .any(|precondition| precondition == "task_frame_status:active")
        );
        assert!(
            plan.next_inspection_commands
                .iter()
                .any(|command| command.starts_with("ee task-frame show "))
        );
        Ok(())
    }

    #[test]
    fn effect_posture_as_str() {
        assert_eq!(EffectPosture::Unknown.as_str(), "unknown");
        assert_eq!(EffectPosture::ReadOnly.as_str(), "read_only");
        assert_eq!(EffectPosture::LocalWrite.as_str(), "local_write");
        assert_eq!(EffectPosture::External.as_str(), "external");
    }

    #[test]
    fn plan_profile_roundtrip() {
        assert_eq!(PlanProfile::from_str("compact"), Some(PlanProfile::Compact));
        assert_eq!(PlanProfile::from_str("full"), Some(PlanProfile::Full));
        assert_eq!(PlanProfile::from_str("safe"), Some(PlanProfile::Safe));
        assert_eq!(PlanProfile::from_str("invalid"), None);
        assert_eq!(
            PlanProfile::from_str(" Compact "),
            Some(PlanProfile::Compact)
        );
        assert_eq!(PlanProfile::from_str("SAFE"), Some(PlanProfile::Safe));
    }

    fn stored_recipe_fixture(workspace_id: &str, id: &str) -> StoredPlanRecipe {
        StoredPlanRecipe {
            id: id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            name: "Tangerine compass release".to_owned(),
            when_to_use: "Prepare the tangerine compass release".to_owned(),
            steps_json: json!(["ee status --json", {"command":"api_key=recipe-secret-step"}])
                .to_string(),
            evidence_uris_json: json!([
                "ee://evidence/release",
                "ee://evidence/release",
                "api_key=recipe-secret-uri"
            ])
            .to_string(),
            maturity: "promoted".to_owned(),
            confidence: 0.75,
            helpful_count: 17,
            harmful_count: 2,
            created_at: "2026-09-01T00:00:00Z".to_owned(),
            updated_at: "2026-09-02T00:00:00Z".to_owned(),
            last_recommended_at: Some("2026-09-01T00:00:00Z".to_owned()),
        }
    }

    fn recipe_workspace() -> Result<(tempfile::TempDir, PathBuf, String), String> {
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = directory.path().canonicalize().map_err(|e| e.to_string())?;
        let database = workspace.join("recipes.db");
        let id = crate::core::workspace::stable_workspace_id(&workspace);
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        db.migrate().map_err(|e| e.to_string())?;
        db.insert_workspace(
            &id,
            &crate::db::CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: Some("recipes".to_owned()),
            },
        )
        .map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        Ok((directory, database, id))
    }

    #[cfg(feature = "lexical-bm25")]
    fn lexical_recommend(options: &PlanRecommendOptions) -> Result<PlanRecommendReport, String> {
        let catalog = recipe_catalog(&options.workspace_path, options.database_path.as_deref())
            .map_err(|e| e.message())?;
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let embedder = crate::search::HashEmbedder::default_256();
            recommend_from_catalog(&cx, options, catalog, Some(&embedder)).await
        })
        .map_err(|e| e.to_string())?
        .map_err(|e| e.message())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn stored_recipes_rank_with_scope_provenance_and_no_mutation() -> TestResult {
        let (directory, database, workspace_id) = recipe_workspace()?;
        let workspace = directory.path();
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let other_path = workspace.join("other");
        std::fs::create_dir(&other_path).map_err(|e| e.to_string())?;
        let other_id = crate::core::workspace::stable_workspace_id(&other_path);
        db.insert_workspace(
            &other_id,
            &crate::db::CreateWorkspaceInput {
                path: other_path.display().to_string(),
                name: None,
            },
        )
        .map_err(|e| e.to_string())?;
        let mut draft = stored_recipe_fixture(&workspace_id, "plrec_c_draft");
        draft.maturity = "draft".to_owned();
        let mut unrelated = stored_recipe_fixture(&workspace_id, "plrec_unrelated");
        unrelated.name = "Zoological migration".to_owned();
        unrelated.when_to_use = "Observe wildebeest migration".to_owned();
        let history = crate::db::StoredMaintenanceHistory {
            recipes: vec![
                stored_recipe_fixture(&workspace_id, "plrec_b_release"),
                stored_recipe_fixture(&workspace_id, "plrec_a_release"),
                draft,
                unrelated,
                stored_recipe_fixture(&other_id, "plrec_other_workspace"),
            ],
            ..Default::default()
        };
        db.with_transaction(|| db.insert_maintenance_history_for_recovery(&history))
            .map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        let before = std::fs::read(&database).map_err(|e| e.to_string())?;
        let options = PlanRecommendOptions {
            task: "tangerine compass release".to_owned(),
            limit: 2,
            min_score: 0.0,
            workspace_path: workspace.to_path_buf(),
            database_path: Some(database.clone()),
        };
        let report = lexical_recommend(&options)?;
        assert_eq!(
            report.matches_found, 3,
            "count before limit, excluding unrelated and other workspace rows"
        );
        assert_eq!(
            report
                .recommendations
                .iter()
                .map(|r| r.recipe_id.as_str())
                .collect::<Vec<_>>(),
            ["plrec_a_release", "plrec_b_release"]
        );
        assert_eq!(
            report.recency_anchor.as_deref(),
            Some("2026-09-02T00:00:00+00:00")
        );
        let first = &report.recommendations[0];
        assert_eq!(first.rank, 1);
        assert_eq!(
            first.components.semantic_similarity, 0.0,
            "hash vectors are not semantic evidence"
        );
        assert!(first.components.text_similarity > 0.0);
        assert_eq!(first.components.maturity_score, 1.0);
        assert_eq!(first.components.recency_decay, 1.0);
        assert_eq!(
            first.components.evidence_count, 0.5,
            "duplicates and redacted secrets earn no evidence credit"
        );
        assert!(
            (first.score - (0.55 * first.components.text_similarity + 0.20 + 0.10 + 0.075)).abs()
                < 1e-12
        );
        assert_eq!(first.source_kind, "stored_plan_recipe");
        assert!(first.source_id.contains(&workspace_id));
        assert_eq!(first.effect_posture, EffectPosture::Unknown);
        assert_eq!(report.degraded[0]["code"], RECIPE_SEMANTIC_UNAVAILABLE);
        let rendered = crate::output::render_plan_recommend_json(&report);
        assert_eq!(
            rendered,
            crate::output::render_plan_recommend_json(&lexical_recommend(&options)?)
        );
        assert!(!rendered.contains("recipe-secret"));
        let shown = find_recipe(workspace, Some(&database), "plrec_a_release")
            .map_err(|e| e.message())?
            .ok_or("missing stored recipe")?
            .data_json();
        assert!(shown["version"].is_null());
        assert!(shown["steps"][0]["required"].is_null());
        assert_eq!(shown["effectPosture"], "unknown");
        assert!(!shown.to_string().contains("recipe-secret"));
        let explanation = explain_recipe(workspace, Some(&database), "plrec_a_release")
            .map_err(|e| e.message())?;
        assert_eq!(
            explanation.when_to_use.as_deref(),
            Some("Prepare the tangerine compass release")
        );
        assert_eq!(explanation.maturity.as_deref(), Some("promoted"));
        assert_eq!(
            explanation.source_id.as_deref(),
            Some(first.source_id.as_str())
        );
        assert!(
            !explain_recipe(workspace, Some(&database), "plrec_other_workspace")
                .map_err(|e| e.message())?
                .found
        );
        assert_eq!(
            std::fs::read(&database).map_err(|e| e.to_string())?,
            before,
            "catalog, recommendations and explanations must not mutate the database"
        );
        Ok(())
    }

    #[test]
    fn recipe_reads_refuse_malformed_store_and_missing_explicit_database() -> TestResult {
        let (directory, database, id) = recipe_workspace()?;
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let mut malformed = stored_recipe_fixture(&id, "plrec_malformed");
        malformed.steps_json = json!("api_key=recipe-shape-secret").to_string(); // Legal SQL JSON, wrong recipe shape.
        db.insert_maintenance_history_for_recovery(&crate::db::StoredMaintenanceHistory {
            recipes: vec![malformed],
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        let error = recipe_catalog(directory.path(), Some(&database))
            .err()
            .ok_or("malformed recipe unexpectedly accepted")?;
        assert!(matches!(&error, DomainError::Storage { .. }));
        assert!(
            !error.message().contains("recipe-shape-secret"),
            "typed parse errors must not echo secret-bearing malformed values"
        );
        let missing = directory.path().join("missing.db");
        assert!(recipe_catalog(directory.path(), Some(&missing)).is_err());
        assert!(!missing.exists());
        assert_eq!(
            recipe_catalog(directory.path(), None)
                .map_err(|e| e.message())?
                .len(),
            builtin_recipes().len()
        );
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn native_rules_join_recipes_with_lifecycle_scope_and_provenance() -> TestResult {
        let (directory, database, workspace_id) = recipe_workspace()?;
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let memory_id = "mem_0123456789ABCDEFGHJKMNPQRST".to_owned();
        db.insert_memory(
            &memory_id,
            &crate::db::CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: "Tangerine compass evidence".to_owned(),
                workflow_id: None,
                confidence: 0.5,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: None,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|e| e.to_string())?;
        let mut active_id = String::new();
        let mut redacted_id = String::new();
        for (maturity, contains_secret) in [
            ("draft", false),
            ("candidate", false),
            ("validated", false),
            ("deprecated", false),
            ("superseded", false),
            ("candidate", true),
        ] {
            let id = crate::models::RuleId::now().to_string();
            if contains_secret {
                redacted_id = id.clone();
            } else if maturity == "candidate" {
                active_id = id.clone();
            }
            db.insert_procedural_rule(
                &id,
                &crate::db::CreateProceduralRuleInput {
                    workspace_id: workspace_id.clone(),
                    content: if contains_secret {
                        "Tangerine compass check api_key=native-rule-secret"
                    } else {
                        "Tangerine compass check"
                    }
                    .to_owned(),
                    confidence: 0.7,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "agent_assertion".to_owned(),
                    scope: "directory".to_owned(),
                    scope_pattern: Some("src".to_owned()),
                    maturity: maturity.to_owned(),
                    protected: false,
                    source_memory_ids: vec![memory_id.clone()],
                    tags: Vec::new(),
                },
            )
            .map_err(|e| e.to_string())?;
        }
        for retired in ["tombstone", "supersession"] {
            let id = crate::models::RuleId::now().to_string();
            db.insert_procedural_rule(
                &id,
                &crate::db::CreateProceduralRuleInput {
                    workspace_id: workspace_id.clone(),
                    content: "Tangerine compass retired".to_owned(),
                    confidence: 0.8,
                    utility: 0.5,
                    importance: 0.5,
                    trust_class: "agent_assertion".to_owned(),
                    scope: "workspace".to_owned(),
                    scope_pattern: None,
                    maturity: "validated".to_owned(),
                    protected: false,
                    source_memory_ids: Vec::new(),
                    tags: Vec::new(),
                },
            )
            .map_err(|e| e.to_string())?;
            let update = if retired == "tombstone" {
                "tombstoned_at = '2026-09-01T00:00:00Z'".to_owned()
            } else {
                format!("superseded_by = '{active_id}'")
            };
            db.execute_raw(&format!(
                "UPDATE procedural_rules SET {update} WHERE id = '{id}'"
            ))
            .map_err(|e| e.to_string())?;
        }
        db.insert_plan_recipe(&stored_recipe_fixture(
            &workspace_id,
            "plrec_native_alternative",
        ))
        .map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        let before = std::fs::read(&database).map_err(|e| e.to_string())?;
        let options = PlanRecommendOptions {
            task: "tangerine compass".to_owned(),
            limit: 20,
            min_score: 0.0,
            workspace_path: directory.path().to_path_buf(),
            database_path: Some(database.clone()),
        };
        let ranked = lexical_recommend(&options)?;
        assert_eq!(ranked.matches_found, 4);
        assert_eq!(ranked.total_recipes_considered, builtin_recipes().len() + 5);
        assert!(
            ranked
                .recommendations
                .iter()
                .all(|row| row.recipe_id != redacted_id),
            "the existing whole-content secret redaction must prevent a match on hidden text"
        );
        let native = ranked
            .recommendations
            .iter()
            .find(|r| r.recipe_id == active_id)
            .ok_or("native rule missing")?;
        assert_eq!(native.source_kind, "procedural_rule");
        assert_eq!(native.maturity.as_deref(), Some("candidate"));
        assert_eq!(native.components.maturity_score, 0.3);
        assert!(
            native
                .match_reasons
                .iter()
                .any(|reason| reason.contains("directory (src)")),
            "recommendations must expose the scope restriction without another command"
        );
        assert_eq!(native.evidence_uris, [format!("ee://memory/{memory_id}")]);
        assert_eq!(native.components.evidence_count, 0.5);
        let explanation = explain_recipe(directory.path(), Some(&database), &active_id)
            .map_err(|e| e.message())?;
        assert!(
            explanation
                .when_to_use
                .as_deref()
                .is_some_and(|text| text.contains("directory (src)"))
        );
        assert!(
            !crate::output::render_plan_explain_json(&explanation).contains("native-rule-secret")
        );
        let redacted = explain_recipe(directory.path(), Some(&database), &redacted_id)
            .map_err(|e| e.message())?;
        assert!(redacted.found);
        assert_eq!(redacted.steps, ["[REDACTED]"]);
        assert!(!crate::output::render_plan_explain_json(&redacted).contains("native-rule-secret"));
        assert_eq!(
            crate::output::render_plan_recommend_json(&ranked),
            crate::output::render_plan_recommend_json(&lexical_recommend(&options)?)
        );
        assert_eq!(std::fs::read(&database).map_err(|e| e.to_string())?, before);
        Ok(())
    }

    #[test]
    fn recipe_evidence_preserves_typed_internal_ids_without_exempting_secrets() {
        for uri in [
            "ee://memory/mem_0123456789ABCDEFGHJKMNPQRST",
            "ee://curation-candidate/curate_0123456789abcdefghijklmnop",
        ] {
            assert!(
                recipe_text(uri).contains("[REDACTED]"),
                "exercise the entropy false positive"
            );
            assert_eq!(recipe_evidence_text(uri), uri);
        }
        for uri in [
            "ee://memory/mem_0123456789ABCDEFGHJKMNPQRST?api_key=recipe-link-canary",
            "ee://curation-candidate/curate_password0123456789ABCDEFGH",
            "https://example.test/?api_key=recipe-link-canary",
            "file:///Users/private/recipe-evidence.txt",
        ] {
            assert_eq!(recipe_evidence_text(uri), recipe_text(uri));
            assert_ne!(recipe_evidence_text(uri), uri);
        }
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn recipe_rank_cases_match_golden() -> TestResult {
        let expected: JsonValue =
            serde_json::from_str(include_str!("../../tests/golden/plan-decisioning.snap"))
                .map_err(|e| e.to_string())?;
        let (directory, database, workspace_id) = recipe_workspace()?;
        let mut options = PlanRecommendOptions {
            task: "tangerine compass".to_owned(),
            limit: 10,
            min_score: 0.0,
            workspace_path: directory.path().to_path_buf(),
            database_path: Some(database.clone()),
        };
        for (case, insert) in [
            ("empty", None),
            ("single", Some("plrec_a")),
            ("tie", Some("plrec_b")),
            ("no_match", None),
        ] {
            if let Some(id) = insert {
                let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
                db.insert_plan_recipe(&stored_recipe_fixture(&workspace_id, id))
                    .map_err(|e| e.to_string())?;
                db.close().map_err(|e| e.to_string())?;
            }
            if case == "no_match" {
                options.task = "zygomorphic quokka".to_owned();
            }
            let report = lexical_recommend(&options)?;
            let output = crate::output::render_plan_recommend_json(&report);
            assert_eq!(
                output,
                crate::output::render_plan_recommend_json(&lexical_recommend(&options)?)
            );
            let parsed: JsonValue = serde_json::from_str(&output).map_err(|e| e.to_string())?;
            let ranked = parsed["data"]["recommendations"].as_array().ok_or("missing recommendations")?.iter().map(|r| {
                json!({"recipeId":r["recipeId"], "rank":r["rank"], "sourceKind":r["sourceKind"], "maturity":r["maturity"]})
            }).collect::<Vec<_>>();
            assert_eq!(
                json!({"matchesFound": parsed["data"]["matchesFound"], "ranked":ranked}),
                expected[case],
                "{case}"
            );
        }
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn task_explanation_uses_real_ranking_and_bounded_alternatives() -> TestResult {
        let (directory, database, workspace_id) = recipe_workspace()?;
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        for index in 0..7 {
            db.insert_plan_recipe(&stored_recipe_fixture(
                &workspace_id,
                &format!("plrec_{index}"),
            ))
            .map_err(|e| e.to_string())?;
        }
        db.close().map_err(|e| e.to_string())?;
        let mut options = PlanRecommendOptions {
            task: "tangerine compass".to_owned(),
            limit: u32::MAX,
            min_score: 0.0,
            workspace_path: directory.path().to_path_buf(),
            database_path: Some(database.clone()),
        };
        let mut explanation = explain_recipe(directory.path(), Some(&database), "plrec_0")
            .map_err(|e| e.message())?;
        explanation.task_evaluation = Some(lexical_recommend(&options)?);
        let rendered: JsonValue =
            serde_json::from_str(&crate::output::render_plan_explain_json(&explanation))
                .map_err(|e| e.to_string())?;
        let evaluation = &rendered["data"]["taskEvaluation"];
        assert_eq!(evaluation["matched"], true);
        assert_eq!(evaluation["recommendation"]["rank"], 1);
        assert_eq!(evaluation["matchesFound"], 7);
        assert_eq!(evaluation["alternativesTruncated"], true);
        assert_eq!(
            evaluation["alternativesConsidered"]
                .as_array()
                .ok_or("alternatives array missing")?
                .iter()
                .map(|r| r["recipeId"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            ["plrec_1", "plrec_2", "plrec_3", "plrec_4", "plrec_5"]
        );
        assert_eq!(rendered["degraded"][0]["code"], RECIPE_SEMANTIC_UNAVAILABLE);
        options.task = "zygomorphic quokka".to_owned();
        explanation.task_evaluation = Some(lexical_recommend(&options)?);
        let rendered: JsonValue =
            serde_json::from_str(&crate::output::render_plan_explain_json(&explanation))
                .map_err(|e| e.to_string())?;
        assert_eq!(rendered["data"]["taskEvaluation"]["matched"], false);
        assert!(rendered["data"]["taskEvaluation"]["recommendation"].is_null());
        assert_eq!(
            rendered["data"]["taskEvaluation"]["alternativesConsidered"],
            json!([])
        );
        Ok(())
    }

    #[test]
    fn recipe_recommend_rejects_invalid_requests_before_store_access() -> TestResult {
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut options = PlanRecommendOptions {
            task: "release".to_owned(),
            limit: 5,
            min_score: 0.0,
            workspace_path: directory.path().join("missing-workspace"),
            database_path: None,
        };
        for score in [f64::NAN, f64::INFINITY, -0.01, 1.01] {
            options.min_score = score;
            assert!(matches!(
                recommend_recipes(&options),
                Err(DomainError::Usage { .. })
            ));
        }
        options.min_score = 0.0;
        options.task = "api_key=recipe-secret-query".to_owned();
        assert!(matches!(
            recommend_recipes(&options),
            Err(DomainError::Usage { .. })
        ));
        options.task = " \n\t".to_owned();
        assert!(matches!(
            recommend_recipes(&options),
            Err(DomainError::Usage { .. })
        ));
        options.task = "release".to_owned();
        options.limit = 0;
        assert!(matches!(
            recommend_recipes(&options),
            Err(DomainError::Usage { .. })
        ));
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn recipe_recommend_no_match_does_not_return_metadata_only_candidates() -> TestResult {
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let options = PlanRecommendOptions {
            task: "zygomorphicxylophagia".to_owned(),
            limit: 5,
            min_score: 0.0,
            workspace_path: directory.path().to_path_buf(),
            database_path: None,
        };
        let report = lexical_recommend(&options)?;
        assert_eq!(report.matches_found, 0);
        assert!(report.recommendations.is_empty());
        assert!(report.recency_anchor.is_none());
        assert!(!directory.path().join(".ee").exists());
        Ok(())
    }

    #[cfg(feature = "lexical-bm25")]
    #[test]
    fn recipe_save_is_audited_retrievable_and_atomic() -> TestResult {
        let (directory, database, workspace_id) = recipe_workspace()?;
        let mut options = RecipeSaveOptions {
            workspace_path: directory.path().to_path_buf(),
            database_path: Some(database.clone()),
            name: "Tangerine compass release".to_owned(),
            when_to_use: "Prepare a tangerine compass release".to_owned(),
            steps: vec!["ee status --json".to_owned(), "exit 73".to_owned()],
            evidence_uris: vec![
                "ee://evidence/release".to_owned(),
                "api_key=recipe-save-secret".to_owned(),
            ],
            dry_run: true,
        };
        let before = std::fs::read(&database).map_err(|e| e.to_string())?;
        let preview = save_recipe(&options).map_err(|e| e.message())?;
        assert_eq!(preview["persisted"], false);
        assert!(preview["recipe"]["id"].is_null());
        assert!(preview["auditId"].is_null());
        assert_eq!(
            std::fs::read(&database).map_err(|e| e.to_string())?,
            before,
            "preview must not write"
        );
        options.dry_run = false;
        let saved = save_recipe(&options).map_err(|e| e.message())?;
        let id = saved["recipe"]["id"]
            .as_str()
            .ok_or("missing saved recipe ID")?;
        let audit_id = saved["auditId"].as_str().ok_or("missing recipe audit ID")?;
        assert_eq!(saved["persisted"], true);
        assert_eq!(saved["recipe"]["maturity"], "draft");
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let rows = db
            .list_plan_recipes(&workspace_id)
            .map_err(|e| e.to_string())?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].confidence, 0.0);
        assert_eq!(rows[0].helpful_count, 0);
        assert!(rows[0].last_recommended_at.is_none());
        assert!(!rows[0].evidence_uris_json.contains("recipe-save-secret"));
        let audit = db
            .get_audit(audit_id)
            .map_err(|e| e.to_string())?
            .ok_or("recipe audit missing")?;
        assert_eq!(audit.target_id.as_deref(), Some(id));
        assert_eq!(audit.action, crate::db::audit_actions::PLAN_RECIPE_SAVE);
        assert!(
            !audit
                .details
                .as_deref()
                .unwrap_or_default()
                .contains("recipe-save-secret")
        );
        db.close().map_err(|e| e.to_string())?;
        let recommendations = lexical_recommend(&PlanRecommendOptions {
            task: "tangerine compass release".to_owned(),
            limit: 5,
            min_score: 0.0,
            workspace_path: directory.path().to_path_buf(),
            database_path: Some(database.clone()),
        })?;
        assert!(
            recommendations
                .recommendations
                .iter()
                .any(|recipe| recipe.recipe_id == id)
        );
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        db.execute_raw("CREATE TRIGGER recipe_test_reject_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT, 'recipe audit failure injection'); END").map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        assert!(
            save_recipe(&options).is_err(),
            "audit failure must prevent a successful save"
        );
        let db = DbConnection::open_file_read_only(&database).map_err(|e| e.to_string())?;
        assert_eq!(
            db.list_plan_recipes(&workspace_id)
                .map_err(|e| e.to_string())?,
            rows,
            "failed audit rolls back recipe insertion"
        );
        db.close().map_err(|e| e.to_string())?;
        options.steps.clear();
        options.database_path = Some(directory.path().join("missing.db"));
        assert!(matches!(
            save_recipe(&options),
            Err(DomainError::Usage { .. })
        ));
        assert!(!directory.path().join("missing.db").exists());
        Ok(())
    }
}
