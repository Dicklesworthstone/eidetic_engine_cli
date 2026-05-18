//! Shared `ee.repair_action_graph.v1` schema and pure builder.
//!
//! Two adjacent SRR6.46 surfaces emit a structured graph of repair
//! actions that a downstream consumer (agent harness, human operator,
//! `ee doctor` renderer) can topologically execute to clear all open
//! issues:
//!
//! - **SRR6.46.4** (`ee mesh status`) surfaces a 1–3 action subset
//!   pinned to the current drift state.
//! - **SRR6.46.16** (`ee doctor`) surfaces the full graph across all
//!   15 readiness checks.
//!
//! Both consumers parse the same shape. Keeping the schema here as a
//! single source of truth — rather than duplicated across two surfaces
//! — means the two beads cannot drift apart, the renderer is shared,
//! and the schema-lifecycle drift gate has one anchor to verify.
//!
//! This module is **pure**: it owns the type definitions, the builder
//! that assembles a graph from caller-supplied actions, deterministic
//! topological ordering, and a minimal cycle-detection invariant. It
//! does not consult the database, the Tailscale CLI, or any I/O
//! source. The caller (`ee mesh status` or `ee doctor`) supplies the
//! resolved [`RepairAction`] slice; this module produces the
//! schema-shaped [`RepairActionGraph`] envelope.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

/// JSON schema identifier for the shared repair-action-graph surface.
/// Held here as the source-of-truth constant so the renderer, the
/// schema-lifecycle drift gate, and both consumer surfaces agree on
/// exactly one string.
pub const REPAIR_ACTION_GRAPH_SCHEMA_V1: &str = "ee.repair_action_graph.v1";

/// Error returned when [`build_repair_action_graph`] is handed an
/// invalid action set. Pure: no I/O contributors, so all variants are
/// caller-fixable by editing inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepairActionGraphError {
    /// Two actions share the same `id`. Action ids must be unique
    /// across the graph because callers reference them by id in
    /// `prerequisites` and downstream check `fixActionId` fields.
    DuplicateActionId(String),
    /// Action `from` lists `to` as a prerequisite but `to` is not in
    /// the action set. Either the prerequisite is misspelled or the
    /// caller forgot to include the upstream action.
    UnknownPrerequisite { from: String, missing: String },
    /// The dependency edges form a cycle. The cycle root is returned
    /// for diagnostics.
    DependencyCycle { contains: String },
}

impl std::fmt::Display for RepairActionGraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateActionId(id) => write!(f, "duplicate action id: {id:?}"),
            Self::UnknownPrerequisite { from, missing } => write!(
                f,
                "action {from:?} declares prerequisite {missing:?}, which is not in the action set"
            ),
            Self::DependencyCycle { contains } => {
                write!(
                    f,
                    "dependency cycle detected (contains action {contains:?})"
                )
            }
        }
    }
}

impl std::error::Error for RepairActionGraphError {}

// ============================================================================
// Enum vocabularies (canonical snake_case strings)
// ============================================================================

/// Action category. The renderer uses this to pick an icon / phrasing
/// for human output and the agent harness uses it to decide which
/// executor (shell, subcommand, manual) to invoke.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Run a shell command (e.g. `tailscale up`).
    ShellCommand,
    /// Run an `ee` subcommand (e.g. `ee mesh auto-enroll`).
    EeSubcommand,
    /// Invoke an external tool the user must install themselves
    /// (e.g. opening Tailscale admin console in a browser).
    ExternalTool,
    /// Operator-only manual step (e.g. "open a ticket to security to
    /// rotate the API key"). No command — `command` field is the
    /// human description and `kind=manual_step` signals the renderer
    /// not to suggest auto-execution.
    ManualStep,
}

impl ActionKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShellCommand => "shell_command",
            Self::EeSubcommand => "ee_subcommand",
            Self::ExternalTool => "external_tool",
            Self::ManualStep => "manual_step",
        }
    }
}

/// Operator-facing priority. Drives the order of human-readable
/// rendering and biases the topological ordering: ties between
/// independent actions break with `Critical` first.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Critical,
    High,
    Medium,
    Low,
}

impl Priority {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// Sort key for tiebreaker ordering — `Critical` first.
    #[must_use]
    pub fn sort_key(self) -> u8 {
        match self {
            Self::Critical => 0,
            Self::High => 1,
            Self::Medium => 2,
            Self::Low => 3,
        }
    }
}

/// Where the action executes. The agent harness reads this to route to
/// the right executor; the human renderer uses it to phrase the
/// instruction ("run in your shell" vs "run in ee").
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionContext {
    /// User's interactive shell.
    UserShell,
    /// `ee` subcommand surface — the agent harness can call directly.
    EeSubcommand,
    /// External tool the harness cannot drive (browser, GUI, vendor
    /// portal, etc).
    ExternalTool,
}

impl ExecutionContext {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserShell => "user_shell",
            Self::EeSubcommand => "ee_subcommand",
            Self::ExternalTool => "external_tool",
        }
    }
}

// ============================================================================
// Output shapes (camelCase serde for direct envelope emission)
// ============================================================================

/// The forward-link contract for a single action: which doctor checks
/// it resolves on success, and which downstream actions become
/// runnable once it completes.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExpectedOutcome {
    /// Doctor check names (matching `name` in the doctor check
    /// output) that this action is expected to flip from
    /// `fail`/`warning` to `ok`.
    #[serde(rename = "resolvesChecks", default)]
    pub resolves_checks: Vec<String>,
    /// Action ids that become unblocked once this action completes.
    /// Redundant with the inverse of `prerequisites[]` but emitted
    /// explicitly because downstream consumers find the forward
    /// adjacency easier to render.
    #[serde(rename = "preconditionsForNextActions", default)]
    pub preconditions_for_next_actions: Vec<String>,
}

/// One repair action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepairAction {
    /// Unique within a graph. Used in cross-references (prerequisites,
    /// fixActionId on doctor checks).
    pub id: String,
    pub kind: ActionKind,
    /// The actual command string to execute, or for `ManualStep`, the
    /// instruction text.
    pub command: String,
    /// One-line human-readable description for the renderer.
    #[serde(rename = "humanReadable")]
    pub human_readable: String,
    /// Action ids (in this same graph) that must complete first.
    #[serde(default)]
    pub prerequisites: Vec<String>,
    #[serde(rename = "expectedOutcome", default)]
    pub expected_outcome: ExpectedOutcome,
    pub priority: Priority,
    /// Wall-clock estimate for the action in seconds. Used to render
    /// `estimatedTotalDurationSeconds` and to size a watchdog.
    #[serde(rename = "estimatedDurationSeconds")]
    pub estimated_duration_seconds: u32,
    /// Whether running this action is reversible by a deterministic
    /// reverse command — and if so, the command in `reversalCommand`.
    pub reversible: bool,
    /// Reverse command, present when `reversible == true` and a
    /// deterministic reversal exists; `None` for irreversible actions
    /// (e.g. tailscale logout that requires re-authentication).
    #[serde(rename = "reversalCommand", default)]
    pub reversal_command: Option<String>,
    /// Whether the renderer must surface an interactive confirmation
    /// prompt before running. Always `true` for destructive actions.
    #[serde(rename = "requiresUserConfirmation")]
    pub requires_user_confirmation: bool,
    #[serde(rename = "executionContext")]
    pub execution_context: ExecutionContext,
}

/// Schema-shaped envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepairActionGraph {
    pub schema: String,
    pub actions: Vec<RepairAction>,
    /// Deterministic topological order: each action is preceded by
    /// all of its prerequisites. Ties between independent actions are
    /// broken by priority (Critical first) then by action id
    /// lexicographic order for reproducibility.
    #[serde(rename = "topologicallyOrderedExecution")]
    pub topologically_ordered_execution: Vec<String>,
    /// Coarse-grained parallelizable layers. Layer N contains actions
    /// whose prerequisites are entirely satisfied by layers 0..N-1.
    /// Within a layer, all actions can run concurrently.
    #[serde(rename = "parallelizableGroups")]
    pub parallelizable_groups: Vec<Vec<String>>,
    /// Sum of `estimated_duration_seconds` across the action set.
    /// Coarse upper bound on serial execution; under
    /// parallelization the wall-clock time will be smaller.
    #[serde(rename = "estimatedTotalDurationSeconds")]
    pub estimated_total_duration_seconds: u64,
}

// ============================================================================
// Builder
// ============================================================================

/// Build a [`RepairActionGraph`] from a caller-supplied set of
/// [`RepairAction`]s. The function:
///
/// 1. Validates id uniqueness.
/// 2. Validates that every prerequisite resolves within the action set.
/// 3. Computes a deterministic Kahn topological order (priority +
///    lexicographic tie-breaking) and detects cycles.
/// 4. Computes parallelizable layers from the same Kahn pass.
/// 5. Fills in each action's `expected_outcome.preconditions_for_next_actions`
///    automatically from the inverse prerequisite map IF the caller
///    left that field empty, so the consumer doesn't have to
///    double-author both directions.
/// 6. Sums durations.
///
/// The returned envelope is independent of caller mutation (deep
/// clone semantics via serde-derived `Clone`).
pub fn build_repair_action_graph(
    actions: Vec<RepairAction>,
) -> Result<RepairActionGraph, RepairActionGraphError> {
    let mut by_id: BTreeMap<String, RepairAction> = BTreeMap::new();
    for action in actions {
        let key = action.id.clone();
        if by_id.insert(key.clone(), action).is_some() {
            return Err(RepairActionGraphError::DuplicateActionId(key));
        }
    }

    for action in by_id.values() {
        for prereq in &action.prerequisites {
            if !by_id.contains_key(prereq) {
                return Err(RepairActionGraphError::UnknownPrerequisite {
                    from: action.id.clone(),
                    missing: prereq.clone(),
                });
            }
        }
    }

    let topo_groups = kahn_layers(&by_id)?;
    let topologically_ordered: Vec<String> = topo_groups
        .iter()
        .flat_map(|layer| layer.iter().cloned())
        .collect();

    let mut reverse_adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for action in by_id.values() {
        for prereq in &action.prerequisites {
            reverse_adjacency
                .entry(prereq.clone())
                .or_default()
                .push(action.id.clone());
        }
    }

    let mut actions_out: Vec<RepairAction> = by_id.into_values().collect();
    for action in &mut actions_out {
        if action
            .expected_outcome
            .preconditions_for_next_actions
            .is_empty()
        {
            if let Some(downstream) = reverse_adjacency.get(&action.id) {
                let mut sorted = downstream.clone();
                sorted.sort();
                sorted.dedup();
                action.expected_outcome.preconditions_for_next_actions = sorted;
            }
        }
    }
    actions_out.sort_by(|a, b| a.id.cmp(&b.id));

    let estimated_total_duration_seconds = actions_out
        .iter()
        .map(|action| u64::from(action.estimated_duration_seconds))
        .sum();

    Ok(RepairActionGraph {
        schema: REPAIR_ACTION_GRAPH_SCHEMA_V1.to_owned(),
        actions: actions_out,
        topologically_ordered_execution: topologically_ordered,
        parallelizable_groups: topo_groups,
        estimated_total_duration_seconds,
    })
}

/// Kahn's algorithm in layers. Returns a `Vec<Vec<String>>` where each
/// inner vector is one parallelizable layer (its actions have all
/// prerequisites in earlier layers). Within a layer, actions are
/// sorted by `(priority, id)` for deterministic output.
fn kahn_layers(
    by_id: &BTreeMap<String, RepairAction>,
) -> Result<Vec<Vec<String>>, RepairActionGraphError> {
    let mut in_degree: BTreeMap<String, usize> =
        by_id.keys().map(|id| (id.clone(), 0_usize)).collect();
    for action in by_id.values() {
        for _ in &action.prerequisites {
            *in_degree.entry(action.id.clone()).or_default() += 1;
        }
    }

    let mut adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for action in by_id.values() {
        for prereq in &action.prerequisites {
            adjacency
                .entry(prereq.clone())
                .or_default()
                .push(action.id.clone());
        }
    }

    let mut layers: Vec<Vec<String>> = Vec::new();
    let mut placed: BTreeSet<String> = BTreeSet::new();

    while placed.len() < by_id.len() {
        let mut current_layer: Vec<String> = in_degree
            .iter()
            .filter(|(id, deg)| **deg == 0 && !placed.contains(*id))
            .map(|(id, _)| id.clone())
            .collect();

        if current_layer.is_empty() {
            // Find any node that's still in_degree > 0 — that's the
            // cycle root.
            let cycle_root = in_degree
                .iter()
                .find(|(id, deg)| **deg > 0 && !placed.contains(*id))
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| "<unknown>".to_owned());
            return Err(RepairActionGraphError::DependencyCycle {
                contains: cycle_root,
            });
        }

        // Sort by (priority, id) for deterministic within-layer order.
        current_layer.sort_by(|a, b| {
            let pa = by_id
                .get(a)
                .map(|act| act.priority.sort_key())
                .unwrap_or(u8::MAX);
            let pb = by_id
                .get(b)
                .map(|act| act.priority.sort_key())
                .unwrap_or(u8::MAX);
            pa.cmp(&pb).then_with(|| a.cmp(b))
        });

        let mut next_queue: VecDeque<String> = VecDeque::new();
        for id in &current_layer {
            placed.insert(id.clone());
            if let Some(children) = adjacency.get(id) {
                for child in children {
                    if let Some(deg) = in_degree.get_mut(child) {
                        if *deg > 0 {
                            *deg -= 1;
                            if *deg == 0 {
                                next_queue.push_back(child.clone());
                            }
                        }
                    }
                }
            }
        }

        layers.push(current_layer);
        let _ = next_queue;
    }

    Ok(layers)
}

// ============================================================================
// Inline tests (AGENTS.md L300-302 / bd-3usjw.62 Rule 7)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn action(
        id: &str,
        prerequisites: &[&str],
        priority: Priority,
        duration_seconds: u32,
    ) -> RepairAction {
        RepairAction {
            id: id.to_owned(),
            kind: ActionKind::ShellCommand,
            command: format!("echo {id}"),
            human_readable: format!("run {id}"),
            prerequisites: prerequisites.iter().map(|s| (*s).to_owned()).collect(),
            expected_outcome: ExpectedOutcome::default(),
            priority,
            estimated_duration_seconds: duration_seconds,
            reversible: false,
            reversal_command: None,
            requires_user_confirmation: false,
            execution_context: ExecutionContext::UserShell,
        }
    }

    #[test]
    fn schema_constant_matches_documented_version() {
        assert_eq!(REPAIR_ACTION_GRAPH_SCHEMA_V1, "ee.repair_action_graph.v1");
    }

    #[test]
    fn enum_strings_match_snake_case_serde() {
        for variant in [
            ActionKind::ShellCommand,
            ActionKind::EeSubcommand,
            ActionKind::ExternalTool,
            ActionKind::ManualStep,
        ] {
            let serialized = serde_json::to_string(&variant).expect("serialize");
            assert!(serialized.contains(variant.as_str()), "{serialized}");
        }
        for variant in [
            Priority::Critical,
            Priority::High,
            Priority::Medium,
            Priority::Low,
        ] {
            let serialized = serde_json::to_string(&variant).expect("serialize");
            assert!(serialized.contains(variant.as_str()), "{serialized}");
        }
        for variant in [
            ExecutionContext::UserShell,
            ExecutionContext::EeSubcommand,
            ExecutionContext::ExternalTool,
        ] {
            let serialized = serde_json::to_string(&variant).expect("serialize");
            assert!(serialized.contains(variant.as_str()), "{serialized}");
        }
    }

    #[test]
    fn empty_action_set_builds_empty_graph() {
        let graph = build_repair_action_graph(Vec::new()).expect("empty graph builds");
        assert_eq!(graph.schema, REPAIR_ACTION_GRAPH_SCHEMA_V1);
        assert!(graph.actions.is_empty());
        assert!(graph.topologically_ordered_execution.is_empty());
        assert!(graph.parallelizable_groups.is_empty());
        assert_eq!(graph.estimated_total_duration_seconds, 0);
    }

    #[test]
    fn single_action_with_no_prereqs_is_one_layer() {
        let actions = vec![action("a", &[], Priority::Medium, 5)];
        let graph = build_repair_action_graph(actions).expect("graph builds");
        assert_eq!(graph.parallelizable_groups, vec![vec!["a".to_owned()]]);
        assert_eq!(graph.topologically_ordered_execution, vec!["a".to_owned()]);
        assert_eq!(graph.estimated_total_duration_seconds, 5);
    }

    #[test]
    fn linear_dependency_chain_produces_layers_in_order() {
        let actions = vec![
            action("c", &["b"], Priority::Medium, 1),
            action("a", &[], Priority::Medium, 1),
            action("b", &["a"], Priority::Medium, 1),
        ];
        let graph = build_repair_action_graph(actions).expect("graph builds");
        assert_eq!(
            graph.parallelizable_groups,
            vec![
                vec!["a".to_owned()],
                vec!["b".to_owned()],
                vec!["c".to_owned()],
            ]
        );
        assert_eq!(
            graph.topologically_ordered_execution,
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
    }

    #[test]
    fn independent_actions_collapse_into_one_parallelizable_layer() {
        let actions = vec![
            action("z", &[], Priority::Critical, 2),
            action("a", &[], Priority::Medium, 2),
            action("m", &[], Priority::Medium, 2),
        ];
        let graph = build_repair_action_graph(actions).expect("graph builds");
        assert_eq!(graph.parallelizable_groups.len(), 1);
        // Within the single layer: Critical first, then medium ones lex-ordered.
        assert_eq!(
            graph.parallelizable_groups[0],
            vec!["z".to_owned(), "a".to_owned(), "m".to_owned()]
        );
    }

    #[test]
    fn cycle_is_detected_and_returned_with_root() {
        let actions = vec![
            action("a", &["b"], Priority::Medium, 1),
            action("b", &["a"], Priority::Medium, 1),
        ];
        let err = build_repair_action_graph(actions).expect_err("cycle detected");
        match err {
            RepairActionGraphError::DependencyCycle { contains } => {
                assert!(contains == "a" || contains == "b", "{contains}");
            }
            other => panic!("expected DependencyCycle, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_id_is_rejected_with_the_offending_id() {
        let actions = vec![
            action("dup", &[], Priority::Medium, 1),
            action("dup", &[], Priority::Medium, 1),
        ];
        let err = build_repair_action_graph(actions).expect_err("duplicate rejected");
        match err {
            RepairActionGraphError::DuplicateActionId(id) => assert_eq!(id, "dup"),
            other => panic!("expected DuplicateActionId, got {other:?}"),
        }
    }

    #[test]
    fn unknown_prerequisite_is_rejected_with_both_action_ids() {
        let actions = vec![action("present", &["missing"], Priority::Medium, 1)];
        let err = build_repair_action_graph(actions).expect_err("unknown prereq rejected");
        match err {
            RepairActionGraphError::UnknownPrerequisite { from, missing } => {
                assert_eq!(from, "present");
                assert_eq!(missing, "missing");
            }
            other => panic!("expected UnknownPrerequisite, got {other:?}"),
        }
    }

    #[test]
    fn empty_expected_outcome_is_auto_populated_from_reverse_adjacency() {
        let actions = vec![
            action("root", &[], Priority::Medium, 1),
            action("child_a", &["root"], Priority::Medium, 1),
            action("child_b", &["root"], Priority::Medium, 1),
        ];
        let graph = build_repair_action_graph(actions).expect("graph builds");
        let root = graph
            .actions
            .iter()
            .find(|a| a.id == "root")
            .expect("root present");
        assert_eq!(
            root.expected_outcome.preconditions_for_next_actions,
            vec!["child_a".to_owned(), "child_b".to_owned()]
        );
        let child_a = graph
            .actions
            .iter()
            .find(|a| a.id == "child_a")
            .expect("child_a present");
        // Child has no downstream, auto-population leaves it empty.
        assert!(
            child_a
                .expected_outcome
                .preconditions_for_next_actions
                .is_empty()
        );
    }

    #[test]
    fn caller_provided_expected_outcome_is_preserved() {
        let mut root = action("root", &[], Priority::Medium, 1);
        root.expected_outcome.preconditions_for_next_actions = vec!["manually-supplied".to_owned()];
        let actions = vec![root, action("child", &["root"], Priority::Medium, 1)];
        let graph = build_repair_action_graph(actions).expect("graph builds");
        let root = graph
            .actions
            .iter()
            .find(|a| a.id == "root")
            .expect("root present");
        assert_eq!(
            root.expected_outcome.preconditions_for_next_actions,
            vec!["manually-supplied".to_owned()],
            "caller value must not be overwritten by reverse-adjacency"
        );
    }

    #[test]
    fn estimated_total_duration_sums_all_actions() {
        let actions = vec![
            action("a", &[], Priority::Medium, 10),
            action("b", &["a"], Priority::Medium, 5),
            action("c", &["b"], Priority::Medium, 15),
        ];
        let graph = build_repair_action_graph(actions).expect("graph builds");
        assert_eq!(graph.estimated_total_duration_seconds, 30);
    }

    #[test]
    fn round_trip_serialization_preserves_envelope() {
        let mut act = action("act", &[], Priority::High, 7);
        act.reversible = true;
        act.reversal_command = Some("ee mesh disable".to_owned());
        act.expected_outcome.resolves_checks = vec!["check_1".to_owned()];
        act.execution_context = ExecutionContext::EeSubcommand;
        act.kind = ActionKind::EeSubcommand;
        act.requires_user_confirmation = true;

        let graph = build_repair_action_graph(vec![act]).expect("graph builds");
        let serialized = serde_json::to_string(&graph).expect("serialize");
        let parsed: RepairActionGraph = serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(parsed, graph);
        assert!(serialized.contains(REPAIR_ACTION_GRAPH_SCHEMA_V1));
        assert!(serialized.contains("\"resolvesChecks\""));
        assert!(serialized.contains("\"preconditionsForNextActions\""));
        assert!(serialized.contains("\"reversalCommand\""));
        assert!(serialized.contains("\"requiresUserConfirmation\""));
        assert!(serialized.contains("\"executionContext\""));
    }

    #[test]
    fn within_layer_ordering_is_priority_then_lex() {
        let actions = vec![
            action("zz_low", &[], Priority::Low, 1),
            action("aa_critical", &[], Priority::Critical, 1),
            action("bb_low", &[], Priority::Low, 1),
            action("cc_medium", &[], Priority::Medium, 1),
        ];
        let graph = build_repair_action_graph(actions).expect("graph builds");
        assert_eq!(
            graph.parallelizable_groups[0],
            vec![
                "aa_critical".to_owned(),
                "cc_medium".to_owned(),
                "bb_low".to_owned(),
                "zz_low".to_owned(),
            ]
        );
    }

    #[test]
    fn diamond_dependency_collapses_correctly() {
        // a → b, a → c, b → d, c → d
        let actions = vec![
            action("a", &[], Priority::Medium, 1),
            action("b", &["a"], Priority::Medium, 1),
            action("c", &["a"], Priority::Medium, 1),
            action("d", &["b", "c"], Priority::Medium, 1),
        ];
        let graph = build_repair_action_graph(actions).expect("graph builds");
        assert_eq!(graph.parallelizable_groups.len(), 3);
        assert_eq!(graph.parallelizable_groups[0], vec!["a".to_owned()]);
        assert_eq!(
            graph.parallelizable_groups[1],
            vec!["b".to_owned(), "c".to_owned()]
        );
        assert_eq!(graph.parallelizable_groups[2], vec!["d".to_owned()]);
    }
}
