//! Agent goal planner and command recipe resolver (EE-PLAN-001).
//!
//! Deterministic, local, schema-validated recipe resolver over known EE commands,
//! current workspace posture, capabilities, effect metadata, and degraded state.
//! This is NOT an autonomous executor - it only emits plans for explicit execution.

use std::cmp::Reverse;

use serde_json::{Value as JsonValue, json};

pub const GOAL_PLAN_SCHEMA_V1: &str = "ee.plan.goal.v1";
pub const RECIPE_LIST_SCHEMA_V1: &str = "ee.plan.recipe_list.v1";
pub const RECIPE_SHOW_SCHEMA_V1: &str = "ee.plan.recipe.v1";
pub const PLAN_EXPLAIN_SCHEMA_V1: &str = "ee.plan.explain.v1";

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

    let top_score = scores[0].1;
    let primary = scores[0].0;

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
            "category": self.category.as_str(),
            "name": self.name,
            "effectPosture": self.effect_posture.as_str(),
            "requiredCapabilities": self.required_capabilities,
            "stepCount": self.steps.len(),
            "profiles": self.profiles,
        })
    }
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
                    command: "ee context \"<task>\" --workspace . --max-tokens 4000 --json".to_string(),
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
                    command: "ee context \"handoff summary\" --workspace . --json".to_string(),
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
        match s {
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

        if include_alternatives && !self.rejected_alternatives.is_empty() {
            obj["rejectedAlternatives"] = json!(
                self.rejected_alternatives
                    .iter()
                    .map(RejectedAlternative::data_json)
                    .collect::<Vec<_>>()
            );
        }

        if !self.classification.alternatives.is_empty() {
            obj["classification"]["alternatives"] = json!(
                self.classification
                    .alternatives
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
            );
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
}

/// Generate a plan for a goal.
#[must_use]
pub fn generate_plan(options: &PlanGoalOptions) -> GoalPlan {
    let classification = classify_goal(&options.goal);
    let plan_id = format!("plan-{:08x}", rand_id());

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

    let next_inspection_commands = vec![
        "ee status --json".to_string(),
        format!("ee plan explain {} --json", plan_id),
    ];

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
        preconditions: vec!["workspace_initialized".to_string()],
        stop_conditions: vec!["any_step_fails".to_string()],
        degraded_branches,
        dry_run_recommended,
        next_inspection_commands,
        rejected_alternatives,
    }
}

/// Generate a pseudo-random ID (deterministic for testing when seeded).
fn rand_id() -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let mut hasher = DefaultHasher::new();
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    hasher.finish() as u32
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
pub fn explain_recipe(recipe_id: &str) -> Option<PlanExplanation> {
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
        };
        let plan = generate_plan(&options);
        assert_eq!(plan.recipe_id, "init-workspace");
        assert!(!plan.steps.is_empty());
    }

    #[test]
    fn generate_plan_unknown_goal() {
        let options = PlanGoalOptions {
            goal: "xyzzy".to_string(),
            workspace: None,
            profile: PlanProfile::Full,
        };
        let plan = generate_plan(&options);
        assert_eq!(plan.recipe_id, "unknown");
        assert!(plan.classification.ambiguous);
    }

    #[test]
    fn explain_recipe_exists() -> TestResult {
        let explanation = explain_recipe("init-workspace");
        assert!(explanation.is_some());
        let exp = explanation.ok_or_else(|| "init-workspace explanation missing".to_string())?;
        assert_eq!(exp.recipe_id, "init-workspace");
        Ok(())
    }

    #[test]
    fn effect_posture_as_str() {
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
    }
}
