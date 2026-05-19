//! Host-adaptive operating profile probes.
//!
//! The probe is deliberately side-effect-free: it reads local resource signals
//! and reports redaction-safe facts for later profile recommendation.

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Serialize, Serializer};
use toml_edit::{DocumentMut, Item};

use crate::core::degraded_aggregation::{
    AggregatedDegradation, DegradationAggregationInput, aggregate_degraded_entries,
};
use crate::models::{ArtifactSummary, MetricValueKind};

pub const HOST_PROFILE_PROBE_SCHEMA_V1: &str = "ee.host_profile.v1";
pub const PROFILE_CONFIG_PLAN_SCHEMA_V1: &str = "ee.profile.config.plan.v1";
pub const RUNTIME_PROFILE_SCHEMA_V1: &str = "ee.profile.runtime.v1";
pub const PROFILE_BUDGET_CONFORMANCE_SCHEMA_V1: &str = "ee.profile.budget_conformance.v1";

const TOOL_NAMES: [&str; 7] = ["cargo", "rustfmt", "clippy", "br", "bv", "rch", "gh"];
const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostResourceProbeReport {
    pub schema: &'static str,
    pub side_effect_free: bool,
    pub redaction: &'static str,
    pub complete: bool,
    pub workspace: WorkspaceProbe,
    pub cpu: CpuProbe,
    pub memory: MemoryProbe,
    pub paths: Vec<PathCapacityProbe>,
    pub tools: Vec<ToolProbe>,
    pub environment: EnvironmentProbe,
    pub topology: HostTopologyProbe,
    #[serde(serialize_with = "serialize_host_probe_degradations")]
    pub degraded: Vec<HostProbeDegradation>,
}

impl HostResourceProbeReport {
    #[must_use]
    pub fn gather_for_workspace(workspace_root: &Path) -> Self {
        Self::gather_for_workspace_with_options(workspace_root, &HostProfileProbeOptions::default())
    }

    #[must_use]
    pub fn gather_for_workspace_with_options(
        workspace_root: &Path,
        options: &HostProfileProbeOptions,
    ) -> Self {
        let workspace = WorkspaceProbe::for_path(workspace_root);
        let cpu = CpuProbe::gather();
        let memory = MemoryProbe::gather();
        let paths = gather_path_probes_with_options(workspace_root, options);
        let tools = gather_tool_probes(env::var_os("PATH").as_deref());
        let environment = EnvironmentProbe::gather();
        let topology = HostTopologyProbe::gather(env::var_os("PATH").as_deref());
        let degraded = host_probe_degradations(&cpu, &memory, &paths);

        let complete = degraded.is_empty();

        Self {
            schema: HOST_PROFILE_PROBE_SCHEMA_V1,
            side_effect_free: true,
            redaction: if options.include_paths {
                "operator_requested_paths_presence_only_env"
            } else {
                "label_only_paths_presence_only_env"
            },
            complete,
            workspace,
            cpu,
            memory,
            paths,
            tools,
            environment,
            topology,
            degraded,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostProfileProbeOptions {
    pub include_paths: bool,
}

fn host_probe_degradations(
    cpu: &CpuProbe,
    memory: &MemoryProbe,
    paths: &[PathCapacityProbe],
) -> Vec<HostProbeDegradation> {
    let mut degraded = Vec::new();

    if cpu.logical_cores.is_none() {
        degraded.push(HostProbeDegradation::warning(
            "cpu_probe_unavailable",
            "CPU parallelism could not be inspected.",
            "Run `ee status --json` and check host permissions.",
        ));
    }
    if memory.total_bytes.is_none() {
        degraded.push(HostProbeDegradation::warning(
            "memory_probe_unavailable",
            "Host memory totals could not be inspected.",
            "Run on a platform with /proc/meminfo or provide explicit profile config.",
        ));
    }
    for path in paths {
        if path.available_bytes.is_none() {
            degraded.push(HostProbeDegradation::warning(
                "path_capacity_unavailable",
                format!(
                    "Capacity for path label `{}` could not be inspected.",
                    path.label
                ),
                "Check filesystem permissions or configure profile budgets explicitly.",
            ));
        }
    }
    degraded
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum OperatingProfile {
    Constrained,
    Portable,
    Workstation,
    Swarm,
}

impl OperatingProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Constrained => "constrained",
            Self::Portable => "portable",
            Self::Workstation => "workstation",
            Self::Swarm => "swarm",
        }
    }
}

impl fmt::Display for OperatingProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for OperatingProfile {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl FromStr for OperatingProfile {
    type Err = ParseOperatingProfileError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "constrained" => Ok(Self::Constrained),
            "portable" => Ok(Self::Portable),
            "workstation" => Ok(Self::Workstation),
            "swarm" => Ok(Self::Swarm),
            other => Err(ParseOperatingProfileError {
                value: other.to_string(),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseOperatingProfileError {
    value: String,
}

impl fmt::Display for ParseOperatingProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid operating profile `{}`; expected constrained, portable, workstation, or swarm",
            self.value
        )
    }
}

impl std::error::Error for ParseOperatingProfileError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSelectionReport {
    pub recommended: OperatingProfile,
    pub effective: OperatingProfile,
    pub confidence: &'static str,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBudgets {
    pub search: SearchProfileBudget,
    pub pack: PackProfileBudget,
    pub cache: CacheProfileBudget,
    pub write_spool: WriteSpoolProfileBudget,
    pub steward: StewardProfileBudget,
    pub verification: VerificationProfileBudget,
    pub diagnostics: DiagnosticsProfileBudget,
}

impl ProfileBudgets {
    #[must_use]
    pub const fn for_profile(profile: OperatingProfile) -> Self {
        match profile {
            OperatingProfile::Constrained => Self {
                search: SearchProfileBudget {
                    candidate_limit: 48,
                    concurrent_index_readers: 1,
                    stale_index_tolerance: "strict",
                },
                pack: PackProfileBudget {
                    max_tokens: 3_000,
                    max_candidate_memories: 24,
                    explanation_verbosity: "standard",
                },
                cache: CacheProfileBudget {
                    memory_cap_mb: 128,
                    entry_cap: 512,
                    hotset_prewarm_limit: 0,
                },
                write_spool: WriteSpoolProfileBudget {
                    queue_cap: 512,
                    batch_cap: 32,
                    retry_budget: 3,
                },
                steward: StewardProfileBudget {
                    maintenance_window_ms: 500,
                    graph_refresh_budget: 128,
                    daemon_prewarm: false,
                },
                verification: VerificationProfileBudget {
                    recipe: "quick",
                    target_dir_posture: "shared",
                    timeout_class: "short",
                    heavy_strategy: "manual",
                },
                diagnostics: DiagnosticsProfileBudget {
                    support_bundle_profile: "minimal",
                    redaction: "strict",
                },
            },
            OperatingProfile::Portable => Self {
                search: SearchProfileBudget {
                    candidate_limit: 96,
                    concurrent_index_readers: 2,
                    stale_index_tolerance: "strict",
                },
                pack: PackProfileBudget {
                    max_tokens: 4_500,
                    max_candidate_memories: 48,
                    explanation_verbosity: "standard",
                },
                cache: CacheProfileBudget {
                    memory_cap_mb: 512,
                    entry_cap: 1_024,
                    hotset_prewarm_limit: 64,
                },
                write_spool: WriteSpoolProfileBudget {
                    queue_cap: 1_024,
                    batch_cap: 64,
                    retry_budget: 5,
                },
                steward: StewardProfileBudget {
                    maintenance_window_ms: 1_000,
                    graph_refresh_budget: 256,
                    daemon_prewarm: false,
                },
                verification: VerificationProfileBudget {
                    recipe: "workspace",
                    target_dir_posture: "isolated",
                    timeout_class: "standard",
                    heavy_strategy: "rch_preferred",
                },
                diagnostics: DiagnosticsProfileBudget {
                    support_bundle_profile: "standard",
                    redaction: "strict",
                },
            },
            OperatingProfile::Workstation => Self {
                search: SearchProfileBudget {
                    candidate_limit: 160,
                    concurrent_index_readers: 4,
                    stale_index_tolerance: "repair_hint",
                },
                pack: PackProfileBudget {
                    max_tokens: 6_000,
                    max_candidate_memories: 96,
                    explanation_verbosity: "full",
                },
                cache: CacheProfileBudget {
                    memory_cap_mb: 1_024,
                    entry_cap: 4_096,
                    hotset_prewarm_limit: 256,
                },
                write_spool: WriteSpoolProfileBudget {
                    queue_cap: 4_096,
                    batch_cap: 128,
                    retry_budget: 8,
                },
                steward: StewardProfileBudget {
                    maintenance_window_ms: 2_000,
                    graph_refresh_budget: 1_024,
                    daemon_prewarm: true,
                },
                verification: VerificationProfileBudget {
                    recipe: "workspace",
                    target_dir_posture: "isolated",
                    timeout_class: "extended",
                    heavy_strategy: "rch_preferred",
                },
                diagnostics: DiagnosticsProfileBudget {
                    support_bundle_profile: "standard",
                    redaction: "policy_applied",
                },
            },
            OperatingProfile::Swarm => Self {
                search: SearchProfileBudget {
                    candidate_limit: 240,
                    concurrent_index_readers: 8,
                    stale_index_tolerance: "repair_hint",
                },
                pack: PackProfileBudget {
                    max_tokens: 8_000,
                    max_candidate_memories: 160,
                    explanation_verbosity: "full",
                },
                cache: CacheProfileBudget {
                    memory_cap_mb: 2_048,
                    entry_cap: 8_192,
                    hotset_prewarm_limit: 512,
                },
                write_spool: WriteSpoolProfileBudget {
                    queue_cap: 8_192,
                    batch_cap: 256,
                    retry_budget: 12,
                },
                steward: StewardProfileBudget {
                    maintenance_window_ms: 5_000,
                    graph_refresh_budget: 4_096,
                    daemon_prewarm: true,
                },
                verification: VerificationProfileBudget {
                    recipe: "full",
                    target_dir_posture: "isolated",
                    timeout_class: "extended",
                    heavy_strategy: "rch_default",
                },
                diagnostics: DiagnosticsProfileBudget {
                    support_bundle_profile: "full",
                    redaction: "policy_applied",
                },
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchProfileBudget {
    pub candidate_limit: u64,
    pub concurrent_index_readers: u64,
    pub stale_index_tolerance: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackProfileBudget {
    pub max_tokens: u64,
    pub max_candidate_memories: u64,
    pub explanation_verbosity: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheProfileBudget {
    pub memory_cap_mb: u64,
    pub entry_cap: u64,
    pub hotset_prewarm_limit: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteSpoolProfileBudget {
    pub queue_cap: u64,
    pub batch_cap: u64,
    pub retry_budget: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StewardProfileBudget {
    pub maintenance_window_ms: u64,
    pub graph_refresh_budget: u64,
    pub daemon_prewarm: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationProfileBudget {
    pub recipe: &'static str,
    pub target_dir_posture: &'static str,
    pub timeout_class: &'static str,
    pub heavy_strategy: &'static str,
}

pub const VERIFICATION_RECIPE_SCHEMA_V1: &str = "ee.profile.verification_recipe.v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationRecipe {
    pub schema: &'static str,
    pub profile: OperatingProfile,
    pub recipe_name: &'static str,
    pub gates_included: Vec<VerificationGate>,
    pub gates_skipped: Vec<SkippedGate>,
    pub rch_commands: Vec<RchCommand>,
    pub cargo_commands: Vec<CargoCommand>,
    pub target_dir_strategy: TargetDirStrategy,
    pub timeout_seconds: u64,
    #[serde(serialize_with = "serialize_verification_degradations")]
    pub degraded: Vec<VerificationDegradation>,
}

impl VerificationRecipe {
    #[must_use]
    pub fn for_profile(profile: OperatingProfile) -> Self {
        let budgets = ProfileBudgets::for_profile(profile);
        build_verification_recipe(profile, &budgets.verification)
    }

    #[must_use]
    pub fn is_degraded(&self) -> bool {
        !self.degraded.is_empty()
    }

    #[must_use]
    pub fn skipped_heavy_gates(&self) -> Vec<&SkippedGate> {
        self.gates_skipped
            .iter()
            .filter(|g| g.weight == "heavy")
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationGate {
    CargoCheck,
    CargoClippy,
    CargoFmt,
    CargoTest,
    CargoTestDoc,
    ForbiddenDeps,
    GoldenSnapshots,
    PropertyTests,
    IntegrationTests,
    E2eTests,
}

impl VerificationGate {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CargoCheck => "cargo_check",
            Self::CargoClippy => "cargo_clippy",
            Self::CargoFmt => "cargo_fmt",
            Self::CargoTest => "cargo_test",
            Self::CargoTestDoc => "cargo_test_doc",
            Self::ForbiddenDeps => "forbidden_deps",
            Self::GoldenSnapshots => "golden_snapshots",
            Self::PropertyTests => "property_tests",
            Self::IntegrationTests => "integration_tests",
            Self::E2eTests => "e2e_tests",
        }
    }

    #[must_use]
    pub const fn weight(self) -> &'static str {
        match self {
            Self::CargoCheck | Self::CargoFmt => "light",
            Self::CargoClippy | Self::CargoTestDoc | Self::ForbiddenDeps => "medium",
            Self::CargoTest | Self::GoldenSnapshots => "standard",
            Self::PropertyTests | Self::IntegrationTests | Self::E2eTests => "heavy",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedGate {
    pub gate: VerificationGate,
    pub weight: &'static str,
    pub reason: &'static str,
    pub manual_command: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchCommand {
    pub description: &'static str,
    pub command: String,
    pub timeout_seconds: u64,
    pub requires_rch: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CargoCommand {
    pub description: &'static str,
    pub command: String,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetDirStrategy {
    pub posture: &'static str,
    pub env_var: &'static str,
    pub recommended_path: &'static str,
    pub rationale: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: &'static str,
}

fn build_verification_recipe(
    profile: OperatingProfile,
    budget: &VerificationProfileBudget,
) -> VerificationRecipe {
    let (gates_included, gates_skipped) =
        partition_gates_for_recipe(budget.recipe, budget.heavy_strategy);
    let timeout_seconds = timeout_for_class(budget.timeout_class);
    let target_dir_strategy = target_dir_strategy_for_posture(budget.target_dir_posture);
    let rch_commands = build_rch_commands(budget, &gates_included, timeout_seconds);
    let cargo_commands = build_cargo_commands(&gates_included, timeout_seconds);
    let degraded = collect_verification_degradations(budget, &gates_skipped);

    VerificationRecipe {
        schema: VERIFICATION_RECIPE_SCHEMA_V1,
        profile,
        recipe_name: budget.recipe,
        gates_included,
        gates_skipped,
        rch_commands,
        cargo_commands,
        target_dir_strategy,
        timeout_seconds,
        degraded,
    }
}

fn partition_gates_for_recipe(
    recipe: &str,
    heavy_strategy: &str,
) -> (Vec<VerificationGate>, Vec<SkippedGate>) {
    let mut included = Vec::new();
    let mut skipped = Vec::new();

    // Light gates always included
    included.push(VerificationGate::CargoCheck);
    included.push(VerificationGate::CargoFmt);

    // Medium gates included for workspace+ recipes
    if recipe != "quick" {
        included.push(VerificationGate::CargoClippy);
        included.push(VerificationGate::ForbiddenDeps);
        included.push(VerificationGate::CargoTestDoc);
    } else {
        skipped.push(SkippedGate {
            gate: VerificationGate::CargoClippy,
            weight: "medium",
            reason: "quick recipe excludes lint gates",
            manual_command: "cargo clippy --all-targets -- -D warnings",
        });
        skipped.push(SkippedGate {
            gate: VerificationGate::ForbiddenDeps,
            weight: "medium",
            reason: "quick recipe excludes forbidden deps check",
            manual_command: "cargo test --test forbidden_deps",
        });
    }

    // Standard gates included for workspace+ recipes
    if recipe != "quick" {
        included.push(VerificationGate::CargoTest);
        included.push(VerificationGate::GoldenSnapshots);
    } else {
        skipped.push(SkippedGate {
            gate: VerificationGate::CargoTest,
            weight: "standard",
            reason: "quick recipe excludes unit tests",
            manual_command: "cargo test --lib",
        });
        skipped.push(SkippedGate {
            gate: VerificationGate::GoldenSnapshots,
            weight: "standard",
            reason: "quick recipe excludes golden tests",
            manual_command: "cargo test --test golden",
        });
    }

    // Heavy gates depend on strategy
    let heavy_gates = [
        (
            VerificationGate::PropertyTests,
            "cargo test --test property",
        ),
        (
            VerificationGate::IntegrationTests,
            "cargo test --test integration",
        ),
        (VerificationGate::E2eTests, "cargo test --test '*_e2e'"),
    ];

    match (recipe, heavy_strategy) {
        ("full", "rch_default") => {
            for (gate, _) in heavy_gates {
                included.push(gate);
            }
        }
        (_, "rch_preferred") => {
            // Include property tests, skip others
            included.push(VerificationGate::PropertyTests);
            skipped.push(SkippedGate {
                gate: VerificationGate::IntegrationTests,
                weight: "heavy",
                reason: "rch_preferred defers integration tests to CI",
                manual_command: "rch exec -- cargo test --test integration",
            });
            skipped.push(SkippedGate {
                gate: VerificationGate::E2eTests,
                weight: "heavy",
                reason: "rch_preferred defers e2e tests to CI",
                manual_command: "rch exec -- cargo test --test '*_e2e'",
            });
        }
        _ => {
            // manual or quick: skip all heavy gates
            for (gate, cmd) in heavy_gates {
                skipped.push(SkippedGate {
                    gate,
                    weight: "heavy",
                    reason: "profile budget excludes heavy verification gates",
                    manual_command: cmd,
                });
            }
        }
    }

    (included, skipped)
}

fn timeout_for_class(timeout_class: &str) -> u64 {
    match timeout_class {
        "short" => 120,
        "standard" => 300,
        "extended" => 600,
        _ => 300,
    }
}

fn target_dir_strategy_for_posture(posture: &str) -> TargetDirStrategy {
    match posture {
        "shared" => TargetDirStrategy {
            posture: "shared",
            env_var: "CARGO_TARGET_DIR",
            recommended_path: "target",
            rationale: "Reuses default target directory to minimize disk usage on constrained hosts.",
        },
        _ => TargetDirStrategy {
            posture: "isolated",
            env_var: "CARGO_TARGET_DIR",
            recommended_path: "/Volumes/USBNVME16TB/temp_agent_space/cargo-target/$PANE",
            rationale: "Isolated target directory prevents lock contention in swarm/parallel builds.",
        },
    }
}

fn build_rch_commands(
    budget: &VerificationProfileBudget,
    gates: &[VerificationGate],
    timeout: u64,
) -> Vec<RchCommand> {
    let mut commands = Vec::new();
    let use_rch = budget.heavy_strategy != "manual";

    if gates.contains(&VerificationGate::CargoCheck) {
        commands.push(RchCommand {
            description: "Type check all targets",
            command: format!(
                "{}cargo check --all-targets",
                if use_rch { "rch exec -- " } else { "" }
            ),
            timeout_seconds: timeout / 4,
            requires_rch: use_rch,
        });
    }

    if gates.contains(&VerificationGate::CargoClippy) {
        commands.push(RchCommand {
            description: "Lint with clippy (warnings as errors)",
            command: format!(
                "{}cargo clippy --all-targets -- -D warnings",
                if use_rch { "rch exec -- " } else { "" }
            ),
            timeout_seconds: timeout / 3,
            requires_rch: use_rch,
        });
    }

    if gates.contains(&VerificationGate::CargoTest) {
        commands.push(RchCommand {
            description: "Run unit and integration tests",
            command: format!(
                "{}cargo test --workspace",
                if use_rch { "rch exec -- " } else { "" }
            ),
            timeout_seconds: timeout,
            requires_rch: use_rch,
        });
    }

    if gates.contains(&VerificationGate::E2eTests) && budget.heavy_strategy == "rch_default" {
        commands.push(RchCommand {
            description: "Run E2E tests via RCH",
            command: "rch exec -- cargo test --workspace --test '*_e2e'".to_string(),
            timeout_seconds: timeout,
            requires_rch: true,
        });
    }

    commands
}

fn build_cargo_commands(gates: &[VerificationGate], timeout: u64) -> Vec<CargoCommand> {
    let mut commands = Vec::new();

    if gates.contains(&VerificationGate::CargoFmt) {
        commands.push(CargoCommand {
            description: "Check formatting",
            command: "cargo fmt --check".to_string(),
            timeout_seconds: 30,
        });
    }

    if gates.contains(&VerificationGate::ForbiddenDeps) {
        commands.push(CargoCommand {
            description: "Verify no forbidden dependencies",
            command: "cargo test --test forbidden_deps".to_string(),
            timeout_seconds: timeout / 4,
        });
    }

    if gates.contains(&VerificationGate::GoldenSnapshots) {
        commands.push(CargoCommand {
            description: "Verify golden snapshot tests",
            command: "cargo test --test golden".to_string(),
            timeout_seconds: timeout / 2,
        });
    }

    commands
}

fn collect_verification_degradations(
    budget: &VerificationProfileBudget,
    skipped: &[SkippedGate],
) -> Vec<VerificationDegradation> {
    let mut degraded = Vec::new();

    let heavy_skipped: Vec<_> = skipped.iter().filter(|s| s.weight == "heavy").collect();
    if !heavy_skipped.is_empty() {
        degraded.push(VerificationDegradation {
            code: "heavy_gates_skipped",
            severity: "info",
            message: format!(
                "{} heavy verification gate(s) skipped by profile: {}",
                heavy_skipped.len(),
                heavy_skipped
                    .iter()
                    .map(|s| s.gate.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            repair: "Run skipped gates manually or upgrade to a higher-resource profile.",
        });
    }

    if budget.heavy_strategy == "manual" {
        degraded.push(VerificationDegradation {
            code: "manual_heavy_strategy",
            severity: "warning",
            message: "Heavy tests require manual invocation on this profile.".to_string(),
            repair: "Use `rch exec -- cargo test` for heavy gates, or configure rch_preferred.",
        });
    }

    degraded
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsProfileBudget {
    pub support_bundle_profile: &'static str,
    pub redaction: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileConfigOptions {
    pub workspace_root: PathBuf,
    pub config_path: Option<PathBuf>,
    pub requested_profile: Option<OperatingProfile>,
    pub dry_run: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConfigReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub dry_run: bool,
    pub config_path: String,
    pub path_redaction: &'static str,
    pub config_exists: bool,
    pub profile: ProfileSelectionReport,
    pub budgets: ProfileBudgets,
    pub overrides: Vec<ProfileConfigOverride>,
    pub edits: Vec<ProfileConfigEdit>,
    pub conflicts: Vec<ProfileConfigConflict>,
    pub would_write: bool,
    pub applied: bool,
    pub repair: Option<&'static str>,
    pub planned_toml: String,
    pub probe: HostResourceProbeReport,
}

impl ProfileConfigReport {
    #[must_use]
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConfigOverride {
    pub key: &'static str,
    pub value: String,
    pub source: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConfigEdit {
    pub key: &'static str,
    pub before: Option<String>,
    pub after: String,
    pub status: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConfigConflict {
    pub key: String,
    pub found: String,
    pub expected: &'static str,
    pub repair: &'static str,
}

#[derive(Debug)]
pub enum ProfileConfigError {
    Read { path: PathBuf, source: io::Error },
    Parse { path: PathBuf, message: String },
    Write { path: PathBuf, source: io::Error },
}

impl fmt::Display for ProfileConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(
                    f,
                    "could not read profile config `{}`: {source}",
                    path.display()
                )
            }
            Self::Parse { path, message } => {
                write!(
                    f,
                    "could not parse profile config `{}`: {message}",
                    path.display()
                )
            }
            Self::Write { path, source } => {
                write!(
                    f,
                    "could not write profile config `{}`: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ProfileConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Parse { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeProfileReport {
    pub schema: &'static str,
    pub active_profile: OperatingProfile,
    pub source: String,
    pub budgets: ProfileBudgets,
}

impl RuntimeProfileReport {
    #[must_use]
    pub fn for_profile(profile: OperatingProfile, source: impl Into<String>) -> Self {
        Self {
            schema: RUNTIME_PROFILE_SCHEMA_V1,
            active_profile: profile,
            source: source.into(),
            budgets: ProfileBudgets::for_profile(profile),
        }
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "activeProfile": self.active_profile.as_str(),
            "source": self.source,
            "budgets": self.budgets,
        })
    }

    #[must_use]
    pub fn cap_search_limit(&self, requested: u32) -> (u32, bool) {
        cap_u32(requested, self.budgets.search.candidate_limit)
    }

    #[must_use]
    pub fn cap_pack_max_tokens(&self, requested: u32) -> (u32, bool) {
        cap_u32(requested, self.budgets.pack.max_tokens)
    }

    #[must_use]
    pub fn cap_pack_candidate_pool(&self, requested: u32) -> (u32, bool) {
        cap_u32(requested, self.budgets.pack.max_candidate_memories)
    }

    #[must_use]
    pub fn cap_index_job_limit(&self, requested: Option<u32>) -> (Option<u32>, bool) {
        let cap = self.budgets.write_spool.batch_cap.min(u64::from(u32::MAX)) as u32;
        match requested {
            Some(value) if value > cap => (Some(cap), true),
            Some(value) => (Some(value), false),
            None => (Some(cap), true),
        }
    }
}

#[must_use]
pub fn runtime_profile_for_workspace(workspace_root: &Path) -> RuntimeProfileReport {
    if let Some(profile) = selected_profile_from_config(workspace_root) {
        RuntimeProfileReport::for_profile(profile, "workspace_config")
    } else {
        let probe = HostResourceProbeReport::gather_for_workspace(workspace_root);
        let selection = recommend_operating_profile(&probe);
        RuntimeProfileReport::for_profile(selection.effective, "host_probe")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileBudgetConformanceStatus {
    Passed,
    Degraded,
    Failed,
}

impl ProfileBudgetConformanceStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileBudgetConformanceSeverity {
    Low,
    Medium,
    High,
}

impl ProfileBudgetConformanceSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBudgetConformanceReport {
    pub schema: &'static str,
    pub side_effect_free: bool,
    pub requested_profile: OperatingProfile,
    pub advertised_profile: Option<OperatingProfile>,
    pub effective_profile: OperatingProfile,
    pub explicit_overrides: Vec<String>,
    pub artifact: ProfileBudgetArtifactSummary,
    pub status: ProfileBudgetConformanceStatus,
    pub checks: Vec<ProfileBudgetConformanceCheck>,
    #[serde(serialize_with = "serialize_profile_budget_conformance_degradations")]
    pub degraded: Vec<ProfileBudgetConformanceDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBudgetArtifactSummary {
    pub artifact_id: String,
    pub source_schema: String,
    pub observed_profile: Option<String>,
    pub metric_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBudgetConformanceCheck {
    pub field: String,
    pub owner: String,
    pub expected: Option<String>,
    pub observed: Option<String>,
    pub status: ProfileBudgetConformanceStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBudgetConformanceDegradation {
    pub code: String,
    pub severity: ProfileBudgetConformanceSeverity,
    pub owner: String,
    pub field: String,
    pub message: String,
    pub repair: Option<String>,
}

impl ProfileBudgetConformanceDegradation {
    fn new(
        code: impl Into<String>,
        severity: ProfileBudgetConformanceSeverity,
        owner: impl Into<String>,
        field: impl Into<String>,
        message: impl Into<String>,
        repair: Option<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            owner: owner.into(),
            field: field.into(),
            message: message.into(),
            repair,
        }
    }
}

#[must_use]
pub fn check_profile_budget_artifact_conformance(
    advertised_profile: Option<OperatingProfile>,
    effective_profile: OperatingProfile,
    explicit_overrides: &[String],
    artifact: &ArtifactSummary,
    observed_verification_recipe: Option<&str>,
) -> ProfileBudgetConformanceReport {
    let budgets = ProfileBudgets::for_profile(effective_profile);
    let mut checks = Vec::new();
    let mut degraded = Vec::new();
    let explicit_overrides = stable_overrides(explicit_overrides);

    match advertised_profile {
        Some(advertised) if advertised != effective_profile => {
            degraded.push(ProfileBudgetConformanceDegradation::new(
                "advertised_profile_mismatch",
                ProfileBudgetConformanceSeverity::Medium,
                "profile",
                "profile.advertised",
                format!(
                    "Advertised profile `{}` differs from effective profile `{}`.",
                    advertised.as_str(),
                    effective_profile.as_str()
                ),
                Some(
                    "Use the effective profile in generated artifacts or document an override."
                        .to_owned(),
                ),
            ));
        }
        None => {
            degraded.push(ProfileBudgetConformanceDegradation::new(
                "advertised_profile_missing",
                ProfileBudgetConformanceSeverity::Medium,
                "profile",
                "profile.advertised",
                "No advertised profile was provided for conformance comparison.",
                Some(
                    "Include the advertised profile alongside the effective runtime profile."
                        .to_owned(),
                ),
            ));
        }
        _ => {}
    }

    match artifact
        .profile
        .as_ref()
        .map(|profile| profile.profile_name.as_str())
    {
        Some(observed) if observed != effective_profile.as_str() => {
            degraded.push(ProfileBudgetConformanceDegradation::new(
                "artifact_profile_mismatch",
                ProfileBudgetConformanceSeverity::High,
                "profile",
                "artifact.profile.profileName",
                format!(
                    "Artifact profile `{observed}` differs from effective profile `{}`.",
                    effective_profile.as_str()
                ),
                Some("Regenerate the artifact under the effective profile or compare against its declared profile.".to_owned()),
            ));
        }
        None => {
            degraded.push(ProfileBudgetConformanceDegradation::new(
                "profile_provenance_missing",
                ProfileBudgetConformanceSeverity::Medium,
                "profile",
                "artifact.profile",
                "Artifact does not include profile provenance.",
                Some("Regenerate the artifact with profile evidence enabled.".to_owned()),
            ));
        }
        _ => {}
    }

    push_u64_budget_check(
        &mut checks,
        &mut degraded,
        &explicit_overrides,
        artifact,
        "profile.budgets.search_candidate_limit",
        "search",
        budgets.search.candidate_limit,
    );
    push_u64_budget_check(
        &mut checks,
        &mut degraded,
        &explicit_overrides,
        artifact,
        "profile.budgets.pack_max_tokens",
        "pack",
        budgets.pack.max_tokens,
    );
    push_u64_budget_check(
        &mut checks,
        &mut degraded,
        &explicit_overrides,
        artifact,
        "profile.budgets.cache_memory_cap_mb",
        "cache",
        budgets.cache.memory_cap_mb,
    );
    push_u64_budget_check(
        &mut checks,
        &mut degraded,
        &explicit_overrides,
        artifact,
        "profile.budgets.write_spool_batch_cap",
        "write_spool",
        budgets.write_spool.batch_cap,
    );
    push_string_budget_check(
        &mut checks,
        &mut degraded,
        &explicit_overrides,
        "profile.budgets.verification_recipe",
        "profile",
        budgets.verification.recipe,
        observed_verification_recipe,
    );

    degraded.sort_by(|a, b| {
        severity_rank(b.severity)
            .cmp(&severity_rank(a.severity))
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.field.cmp(&b.field))
    });

    let status = if checks
        .iter()
        .any(|check| check.status == ProfileBudgetConformanceStatus::Failed)
        || degraded
            .iter()
            .any(|d| d.severity == ProfileBudgetConformanceSeverity::High)
    {
        ProfileBudgetConformanceStatus::Failed
    } else if checks
        .iter()
        .any(|check| check.status == ProfileBudgetConformanceStatus::Degraded)
        || !degraded.is_empty()
    {
        ProfileBudgetConformanceStatus::Degraded
    } else {
        ProfileBudgetConformanceStatus::Passed
    };

    ProfileBudgetConformanceReport {
        schema: PROFILE_BUDGET_CONFORMANCE_SCHEMA_V1,
        side_effect_free: true,
        requested_profile: effective_profile,
        advertised_profile,
        effective_profile,
        explicit_overrides,
        artifact: ProfileBudgetArtifactSummary {
            artifact_id: artifact.artifact_id.clone(),
            source_schema: artifact.source_schema.clone(),
            observed_profile: artifact
                .profile
                .as_ref()
                .map(|profile| profile.profile_name.clone()),
            metric_count: artifact.metrics.len(),
        },
        status,
        checks,
        degraded,
    }
}

fn stable_overrides(explicit_overrides: &[String]) -> Vec<String> {
    let mut overrides = explicit_overrides.to_vec();
    overrides.sort();
    overrides.dedup();
    overrides
}

fn push_u64_budget_check(
    checks: &mut Vec<ProfileBudgetConformanceCheck>,
    degraded: &mut Vec<ProfileBudgetConformanceDegradation>,
    explicit_overrides: &[String],
    artifact: &ArtifactSummary,
    field: &'static str,
    owner: &'static str,
    expected: u64,
) {
    let observed = artifact_metric_u64(artifact, field);
    push_budget_check(
        checks,
        degraded,
        explicit_overrides,
        field,
        owner,
        expected.to_string(),
        observed.map(|value| value.to_string()),
    );
}

fn push_string_budget_check(
    checks: &mut Vec<ProfileBudgetConformanceCheck>,
    degraded: &mut Vec<ProfileBudgetConformanceDegradation>,
    explicit_overrides: &[String],
    field: &'static str,
    owner: &'static str,
    expected: &'static str,
    observed: Option<&str>,
) {
    push_budget_check(
        checks,
        degraded,
        explicit_overrides,
        field,
        owner,
        expected.to_owned(),
        observed.map(ToOwned::to_owned),
    );
}

fn push_budget_check(
    checks: &mut Vec<ProfileBudgetConformanceCheck>,
    degraded: &mut Vec<ProfileBudgetConformanceDegradation>,
    explicit_overrides: &[String],
    field: &'static str,
    owner: &'static str,
    expected: String,
    observed: Option<String>,
) {
    let override_present = explicit_overrides.iter().any(|value| value == field);
    let status = match observed.as_deref() {
        Some(value) if value == expected => ProfileBudgetConformanceStatus::Passed,
        Some(_) if override_present => ProfileBudgetConformanceStatus::Degraded,
        Some(_) => ProfileBudgetConformanceStatus::Failed,
        None => ProfileBudgetConformanceStatus::Degraded,
    };

    if status != ProfileBudgetConformanceStatus::Passed {
        degraded.push(profile_budget_degradation(
            field,
            owner,
            &expected,
            observed.as_deref(),
            override_present,
        ));
    }

    checks.push(ProfileBudgetConformanceCheck {
        field: field.to_owned(),
        owner: owner.to_owned(),
        expected: Some(expected),
        observed,
        status,
    });
}

fn profile_budget_degradation(
    field: &'static str,
    owner: &'static str,
    expected: &str,
    observed: Option<&str>,
    override_present: bool,
) -> ProfileBudgetConformanceDegradation {
    match observed {
        None => ProfileBudgetConformanceDegradation::new(
            "observed_budget_missing",
            ProfileBudgetConformanceSeverity::Medium,
            owner,
            field,
            format!("Observed budget field `{field}` is missing from the artifact."),
            Some("Regenerate the artifact with profile budget metrics enabled.".to_owned()),
        ),
        Some(value) if override_present => ProfileBudgetConformanceDegradation::new(
            "explicit_override_observed",
            ProfileBudgetConformanceSeverity::Low,
            owner,
            field,
            format!(
                "Observed budget field `{field}` has explicit override value `{value}` instead of profile value `{expected}`."
            ),
            Some("Keep the override if intentional, otherwise remove it and regenerate the artifact.".to_owned()),
        ),
        Some(value) => {
            let code = numeric_budget_mismatch_code(expected, value);
            ProfileBudgetConformanceDegradation::new(
                code,
                ProfileBudgetConformanceSeverity::High,
                owner,
                field,
                format!(
                    "Observed budget field `{field}` has value `{value}` but profile expects `{expected}`."
                ),
                Some("Regenerate the artifact under the effective profile or correct the profile budget wiring.".to_owned()),
            )
        }
    }
}

fn numeric_budget_mismatch_code(expected: &str, observed: &str) -> &'static str {
    match (expected.parse::<u64>(), observed.parse::<u64>()) {
        (Ok(expected), Ok(observed)) if observed > expected => "observed_budget_above_profile",
        (Ok(expected), Ok(observed)) if observed < expected => "observed_budget_below_profile",
        _ => "observed_budget_mismatch",
    }
}

fn artifact_metric_u64(artifact: &ArtifactSummary, field: &str) -> Option<u64> {
    let metric = artifact.metrics.get(field)?;
    if metric.kind != MetricValueKind::Measured {
        return None;
    }
    let value = metric.value?;
    if value.is_finite() && value >= 0.0 && value.fract() == 0.0 {
        Some(value as u64)
    } else {
        None
    }
}

fn severity_rank(severity: ProfileBudgetConformanceSeverity) -> u8 {
    match severity {
        ProfileBudgetConformanceSeverity::Low => 0,
        ProfileBudgetConformanceSeverity::Medium => 1,
        ProfileBudgetConformanceSeverity::High => 2,
    }
}

fn cap_u32(requested: u32, cap: u64) -> (u32, bool) {
    let cap = cap.min(u64::from(u32::MAX)) as u32;
    if requested > cap {
        (cap, true)
    } else {
        (requested, false)
    }
}

fn selected_profile_from_config(workspace_root: &Path) -> Option<OperatingProfile> {
    let path = workspace_root.join(".ee").join("config.toml");
    let contents = read_profile_config_if_regular(&path, "read").ok()??;
    let document = contents.parse::<DocumentMut>().ok()?;
    document
        .as_table()
        .get("profile")?
        .get("selected")?
        .as_str()?
        .parse()
        .ok()
}

#[must_use]
pub fn recommend_operating_profile(probe: &HostResourceProbeReport) -> ProfileSelectionReport {
    let logical_cores = probe.cpu.logical_cores.unwrap_or(1);
    let available_memory = probe
        .memory
        .available_bytes
        .or(probe.memory.total_bytes)
        .unwrap_or(0);

    let recommended = if logical_cores >= 12 && available_memory >= 32 * GIB {
        OperatingProfile::Swarm
    } else if logical_cores >= 6 && available_memory >= 16 * GIB {
        OperatingProfile::Workstation
    } else if logical_cores >= 2 && available_memory >= 8 * GIB {
        OperatingProfile::Portable
    } else {
        OperatingProfile::Constrained
    };

    let confidence = if probe.cpu.logical_cores.is_some() && available_memory > 0 {
        "high"
    } else {
        "medium"
    };

    let mut reasons = vec![
        format!("logical cores: {logical_cores}"),
        format!("available memory GiB: {}", available_memory / GIB),
    ];
    if !probe.complete {
        reasons.push("probe completed with warnings; conservative thresholds applied".to_string());
    }

    ProfileSelectionReport {
        recommended,
        effective: recommended,
        confidence,
        reasons,
    }
}

/// Build a side-effect-free config mutation report for a profile selection.
///
/// # Errors
///
/// Returns [`ProfileConfigError`] when an existing TOML file cannot be read or
/// parsed. Structural conflicts inside recognized profile sections are returned
/// in the report so JSON callers can inspect stable repair hints.
pub fn plan_profile_config(
    options: &ProfileConfigOptions,
) -> Result<ProfileConfigReport, ProfileConfigError> {
    build_profile_config_report(options, "profile config plan")
}

/// Apply a profile config mutation unless `dry_run` is set.
///
/// # Errors
///
/// Returns [`ProfileConfigError`] when the config file cannot be read, parsed,
/// or written.
pub fn apply_profile_config(
    options: &ProfileConfigOptions,
) -> Result<ProfileConfigReport, ProfileConfigError> {
    let mut report = build_profile_config_report(options, "profile config apply")?;
    if report.has_conflicts() || report.dry_run || !report.would_write {
        return Ok(report);
    }

    let path = effective_config_path(&options.workspace_root, options.config_path.as_deref());
    ensure_no_profile_config_symlink_components(&path, "write").map_err(|source| {
        ProfileConfigError::Write {
            path: path.clone(),
            source,
        }
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ProfileConfigError::Write {
            path: path.clone(),
            source,
        })?;
    }
    ensure_no_profile_config_symlink_components(&path, "write").map_err(|source| {
        ProfileConfigError::Write {
            path: path.clone(),
            source,
        }
    })?;
    ensure_profile_config_write_path_is_regular_or_missing(&path).map_err(|source| {
        ProfileConfigError::Write {
            path: path.clone(),
            source,
        }
    })?;
    let temp_path = path.with_extension("toml.tmp");
    ensure_no_profile_config_symlink_components(&temp_path, "write temp").map_err(|source| {
        ProfileConfigError::Write {
            path: temp_path.clone(),
            source,
        }
    })?;
    ensure_profile_config_temp_path_is_missing(&temp_path).map_err(|source| {
        ProfileConfigError::Write {
            path: temp_path.clone(),
            source,
        }
    })?;
    write_profile_config_temp_file(&temp_path, report.planned_toml.as_bytes()).map_err(
        |source| ProfileConfigError::Write {
            path: temp_path.clone(),
            source,
        },
    )?;
    publish_profile_config_temp_file(&temp_path, &path).map_err(|source| {
        ProfileConfigError::Write {
            path: path.clone(),
            source,
        }
    })?;

    report.applied = true;
    report.repair = None;
    for edit in &mut report.edits {
        if edit.status == "planned" {
            edit.status = "applied";
        }
    }
    Ok(report)
}

fn publish_profile_config_temp_file(temp_path: &Path, path: &Path) -> Result<(), io::Error> {
    ensure_no_profile_config_symlink_components(temp_path, "publish temp")?;
    ensure_profile_config_temp_path_is_regular(temp_path)?;
    ensure_no_profile_config_symlink_components(path, "publish")?;
    ensure_profile_config_write_path_is_regular_or_missing(path)?;
    fs::rename(temp_path, path)
}

fn ensure_profile_config_temp_path_is_regular(path: &Path) -> Result<(), io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to publish profile config temp file `{}` because it is not a regular file",
                path.display()
            ),
        )),
        Err(error) => Err(error),
    }
}

fn build_profile_config_report(
    options: &ProfileConfigOptions,
    command: &'static str,
) -> Result<ProfileConfigReport, ProfileConfigError> {
    let path = effective_config_path(&options.workspace_root, options.config_path.as_deref());
    let (config_exists, input) = read_optional_config(&path)?;
    let mut document =
        input
            .parse::<DocumentMut>()
            .map_err(|source| ProfileConfigError::Parse {
                path: path.clone(),
                message: source.to_string(),
            })?;
    let conflicts = profile_config_conflicts(&document);

    let probe = HostResourceProbeReport::gather_for_workspace(&options.workspace_root);
    let mut profile = recommend_operating_profile(&probe);
    let mut overrides = Vec::new();
    if let Some(requested) = options.requested_profile {
        profile.effective = requested;
        overrides.push(ProfileConfigOverride {
            key: "profile.effective",
            value: requested.as_str().to_string(),
            source: "cli",
        });
    }

    let budgets = ProfileBudgets::for_profile(profile.effective);
    let planned_values = profile_config_values(profile.effective, &budgets);
    let mut edits = planned_values
        .iter()
        .map(|planned| {
            let before = item_for_path(&document, planned.path).map(item_value_for_report);
            let status = if before.as_deref() == Some(planned.report_value.as_str()) {
                "unchanged"
            } else if conflicts.is_empty() {
                "planned"
            } else {
                "blocked"
            };
            ProfileConfigEdit {
                key: planned.key,
                before,
                after: planned.report_value.clone(),
                status,
            }
        })
        .collect::<Vec<_>>();

    if conflicts.is_empty() {
        for planned in planned_values {
            set_toml_value(&mut document, planned.path, planned.toml_value);
        }
    }

    let would_write = edits.iter().any(|edit| edit.status == "planned");
    if !conflicts.is_empty() {
        for edit in &mut edits {
            if edit.status == "planned" {
                edit.status = "blocked";
            }
        }
    }

    Ok(ProfileConfigReport {
        schema: PROFILE_CONFIG_PLAN_SCHEMA_V1,
        command,
        dry_run: options.dry_run,
        config_path: path.display().to_string(),
        path_redaction: "operator_requested_config_path",
        config_exists,
        profile,
        budgets,
        overrides,
        edits,
        conflicts,
        would_write,
        applied: false,
        repair: if would_write {
            Some("Review plannedToml, then run `ee profile config apply` without `--dry-run`.")
        } else {
            None
        },
        planned_toml: document.to_string(),
        probe,
    })
}

fn effective_config_path(workspace_root: &Path, config_path: Option<&Path>) -> PathBuf {
    match config_path {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => workspace_root.join(path),
        None => workspace_root.join(".ee").join("config.toml"),
    }
}

fn read_optional_config(path: &Path) -> Result<(bool, String), ProfileConfigError> {
    match read_profile_config_if_regular(path, "read").map_err(|source| {
        ProfileConfigError::Read {
            path: path.to_path_buf(),
            source,
        }
    })? {
        Some(contents) => Ok((true, contents)),
        None => Ok((false, String::new())),
    }
}

fn read_profile_config_if_regular(
    path: &Path,
    operation: &'static str,
) -> Result<Option<String>, io::Error> {
    ensure_no_profile_config_symlink_components(path, operation)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to {operation} profile config `{}` because it is not a regular file",
                path.display()
            ),
        ));
    }
    read_profile_config_file_no_follow(path).map(Some)
}

fn read_profile_config_file_no_follow(path: &Path) -> Result<String, io::Error> {
    let mut file = open_profile_config_file_for_read(path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

fn open_profile_config_file_for_read(path: &Path) -> Result<fs::File, io::Error> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_profile_config_open_no_follow(&mut options);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_profile_config_open_no_follow(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_profile_config_open_no_follow(_options: &mut fs::OpenOptions) {}

fn ensure_profile_config_write_path_is_regular_or_missing(path: &Path) -> Result<(), io::Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to write profile config `{}` because it is not a regular file",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_profile_config_temp_path_is_missing(path: &Path) -> Result<(), io::Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "refusing to write profile config temp file `{}` because it already exists",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn write_profile_config_temp_file(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn ensure_no_profile_config_symlink_components(
    path: &Path,
    operation: &'static str,
) -> Result<(), io::Error> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing to {operation} profile config `{}` through symlinked path component `{}`",
                        path.display(),
                        current.display()
                    ),
                ));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn profile_config_conflicts(document: &DocumentMut) -> Vec<ProfileConfigConflict> {
    let mut conflicts = Vec::new();
    if let Some(profile) = document.as_table().get("profile") {
        if !profile.is_table_like() {
            conflicts.push(ProfileConfigConflict {
                key: "profile".to_string(),
                found: profile.type_name().to_string(),
                expected: "table",
                repair: "Change `profile` to a TOML table before applying profile config.",
            });
            return conflicts;
        }
        if let Some(selected) = profile.get("selected") {
            if !selected.is_str() {
                conflicts.push(ProfileConfigConflict {
                    key: "profile.selected".to_string(),
                    found: selected.type_name().to_string(),
                    expected: "string",
                    repair: "Set `profile.selected` to constrained, portable, workstation, or swarm.",
                });
            }
        }
        if let Some(budgets) = profile.get("budgets") {
            if !budgets.is_table_like() {
                conflicts.push(ProfileConfigConflict {
                    key: "profile.budgets".to_string(),
                    found: budgets.type_name().to_string(),
                    expected: "table",
                    repair: "Change `profile.budgets` to a TOML table before applying profile config.",
                });
            }
        }
    }
    conflicts
}

#[derive(Clone, Debug)]
struct PlannedTomlValue {
    key: &'static str,
    path: &'static [&'static str],
    report_value: String,
    toml_value: TomlScalar,
}

#[derive(Clone, Copy, Debug)]
enum TomlScalar {
    String(&'static str),
    Integer(i64),
    Boolean(bool),
}

fn profile_config_values(
    profile: OperatingProfile,
    budgets: &ProfileBudgets,
) -> Vec<PlannedTomlValue> {
    vec![
        planned_string(
            "profile.selected",
            &["profile", "selected"],
            profile.as_str(),
        ),
        planned_integer(
            "profile.budgets.search_candidate_limit",
            &["profile", "budgets", "search_candidate_limit"],
            budgets.search.candidate_limit,
        ),
        planned_integer(
            "profile.budgets.search_concurrent_index_readers",
            &["profile", "budgets", "search_concurrent_index_readers"],
            budgets.search.concurrent_index_readers,
        ),
        planned_string(
            "profile.budgets.search_stale_index_tolerance",
            &["profile", "budgets", "search_stale_index_tolerance"],
            budgets.search.stale_index_tolerance,
        ),
        planned_integer(
            "profile.budgets.pack_max_tokens",
            &["profile", "budgets", "pack_max_tokens"],
            budgets.pack.max_tokens,
        ),
        planned_integer(
            "profile.budgets.pack_max_candidate_memories",
            &["profile", "budgets", "pack_max_candidate_memories"],
            budgets.pack.max_candidate_memories,
        ),
        planned_string(
            "profile.budgets.pack_explanation_verbosity",
            &["profile", "budgets", "pack_explanation_verbosity"],
            budgets.pack.explanation_verbosity,
        ),
        planned_integer(
            "profile.budgets.cache_memory_cap_mb",
            &["profile", "budgets", "cache_memory_cap_mb"],
            budgets.cache.memory_cap_mb,
        ),
        planned_integer(
            "profile.budgets.cache_entry_cap",
            &["profile", "budgets", "cache_entry_cap"],
            budgets.cache.entry_cap,
        ),
        planned_integer(
            "profile.budgets.cache_hotset_prewarm_limit",
            &["profile", "budgets", "cache_hotset_prewarm_limit"],
            budgets.cache.hotset_prewarm_limit,
        ),
        planned_integer(
            "profile.budgets.write_spool_queue_cap",
            &["profile", "budgets", "write_spool_queue_cap"],
            budgets.write_spool.queue_cap,
        ),
        planned_integer(
            "profile.budgets.write_spool_batch_cap",
            &["profile", "budgets", "write_spool_batch_cap"],
            budgets.write_spool.batch_cap,
        ),
        planned_integer(
            "profile.budgets.write_spool_retry_budget",
            &["profile", "budgets", "write_spool_retry_budget"],
            budgets.write_spool.retry_budget,
        ),
        planned_integer(
            "profile.budgets.steward_maintenance_window_ms",
            &["profile", "budgets", "steward_maintenance_window_ms"],
            budgets.steward.maintenance_window_ms,
        ),
        planned_integer(
            "profile.budgets.steward_graph_refresh_budget",
            &["profile", "budgets", "steward_graph_refresh_budget"],
            budgets.steward.graph_refresh_budget,
        ),
        planned_boolean(
            "profile.budgets.steward_daemon_prewarm",
            &["profile", "budgets", "steward_daemon_prewarm"],
            budgets.steward.daemon_prewarm,
        ),
        planned_string(
            "profile.budgets.verification_recipe",
            &["profile", "budgets", "verification_recipe"],
            budgets.verification.recipe,
        ),
        planned_string(
            "profile.budgets.verification_target_dir_posture",
            &["profile", "budgets", "verification_target_dir_posture"],
            budgets.verification.target_dir_posture,
        ),
        planned_string(
            "profile.budgets.verification_timeout_class",
            &["profile", "budgets", "verification_timeout_class"],
            budgets.verification.timeout_class,
        ),
        planned_string(
            "profile.budgets.verification_heavy_strategy",
            &["profile", "budgets", "verification_heavy_strategy"],
            budgets.verification.heavy_strategy,
        ),
        planned_string(
            "profile.budgets.diagnostics_support_bundle_profile",
            &["profile", "budgets", "diagnostics_support_bundle_profile"],
            budgets.diagnostics.support_bundle_profile,
        ),
        planned_string(
            "profile.budgets.diagnostics_redaction",
            &["profile", "budgets", "diagnostics_redaction"],
            budgets.diagnostics.redaction,
        ),
    ]
}

fn planned_string(
    key: &'static str,
    path: &'static [&'static str],
    value: &'static str,
) -> PlannedTomlValue {
    PlannedTomlValue {
        key,
        path,
        report_value: value.to_string(),
        toml_value: TomlScalar::String(value),
    }
}

fn planned_integer(
    key: &'static str,
    path: &'static [&'static str],
    value: u64,
) -> PlannedTomlValue {
    PlannedTomlValue {
        key,
        path,
        report_value: value.to_string(),
        toml_value: TomlScalar::Integer((value.min(i64::MAX as u64)) as i64),
    }
}

fn planned_boolean(
    key: &'static str,
    path: &'static [&'static str],
    value: bool,
) -> PlannedTomlValue {
    PlannedTomlValue {
        key,
        path,
        report_value: value.to_string(),
        toml_value: TomlScalar::Boolean(value),
    }
}

fn item_for_path<'a>(document: &'a DocumentMut, path: &[&str]) -> Option<&'a Item> {
    let mut item = document.as_table().get(path.first()?)?;
    for key in &path[1..] {
        item = item.get(*key)?;
    }
    Some(item)
}

fn item_value_for_report(item: &Item) -> String {
    if let Some(value) = item.as_str() {
        value.to_string()
    } else if let Some(value) = item.as_integer() {
        value.to_string()
    } else if let Some(value) = item.as_bool() {
        value.to_string()
    } else {
        item.type_name().to_string()
    }
}

fn set_toml_value(document: &mut DocumentMut, path: &[&str], value: TomlScalar) {
    let item = match path {
        [section, key] => &mut document[*section][*key],
        [section, subsection, key] => &mut document[*section][*subsection][*key],
        _ => return,
    };
    *item = match value {
        TomlScalar::String(value) => toml_edit::value(value),
        TomlScalar::Integer(value) => toml_edit::value(value),
        TomlScalar::Boolean(value) => toml_edit::value(value),
    };
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceProbe {
    pub label: &'static str,
    pub initialized: bool,
    pub redaction: &'static str,
}

impl WorkspaceProbe {
    #[must_use]
    pub fn for_path(workspace_root: &Path) -> Self {
        Self {
            label: "workspace",
            initialized: workspace_root.join(".ee").is_dir(),
            redaction: "path_not_emitted",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuProbe {
    pub logical_cores: Option<u32>,
    pub physical_cores: Option<u32>,
    pub source: &'static str,
}

impl CpuProbe {
    #[must_use]
    pub fn gather() -> Self {
        Self {
            logical_cores: std::thread::available_parallelism()
                .ok()
                .and_then(|cores| u32::try_from(cores.get()).ok()),
            physical_cores: None,
            source: "std_thread_available_parallelism",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryProbe {
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub cgroup_limit_bytes: Option<u64>,
    pub source: &'static str,
}

impl MemoryProbe {
    #[must_use]
    pub fn gather() -> Self {
        let meminfo = fs::read_to_string("/proc/meminfo").ok();
        let (total_bytes, available_bytes) = meminfo
            .as_deref()
            .map(parse_proc_meminfo_bytes)
            .unwrap_or((None, None));
        Self {
            total_bytes,
            available_bytes,
            cgroup_limit_bytes: read_cgroup_memory_limit_bytes(),
            source: if meminfo.is_some() {
                "proc_meminfo"
            } else {
                "unavailable"
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathCapacityProbe {
    pub label: &'static str,
    pub role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub exists: bool,
    pub nearest_existing_ancestor: bool,
    pub same_filesystem_as_workspace: Option<bool>,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub redaction: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FsCapacity {
    total_bytes: u64,
    available_bytes: u64,
    filesystem_id: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PathSpec<'a> {
    label: &'static str,
    role: &'static str,
    path: &'a Path,
}

#[cfg(test)]
fn gather_path_probes(workspace_root: &Path) -> Vec<PathCapacityProbe> {
    gather_path_probes_with_options(workspace_root, &HostProfileProbeOptions::default())
}

fn gather_path_probes_with_options(
    workspace_root: &Path,
    options: &HostProfileProbeOptions,
) -> Vec<PathCapacityProbe> {
    let ee_dir = workspace_root.join(".ee");
    let database_path = ee_dir.join("ee.db");
    let index_dir = ee_dir.join("index");
    let cache_dir = ee_dir.join("cache");
    let temp_dir = env::temp_dir();
    let cargo_target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    let workspace_capacity = capacity_for_path(workspace_root).map(|(_, capacity)| capacity);

    let specs = [
        PathSpec {
            label: "workspace",
            role: "workspace_root",
            path: workspace_root,
        },
        PathSpec {
            label: "ee_state",
            role: "workspace_state_dir",
            path: &ee_dir,
        },
        PathSpec {
            label: "database",
            role: "frankensqlite_database",
            path: &database_path,
        },
        PathSpec {
            label: "index",
            role: "frankensearch_index",
            path: &index_dir,
        },
        PathSpec {
            label: "cache",
            role: "derived_cache",
            path: &cache_dir,
        },
        PathSpec {
            label: "temp",
            role: "temporary_directory",
            path: &temp_dir,
        },
        PathSpec {
            label: "cargo_target",
            role: "build_cache_directory",
            path: &cargo_target_dir,
        },
    ];

    specs
        .iter()
        .map(|spec| {
            let exists = spec.path.exists();
            let capacity = capacity_for_path(spec.path);
            let (nearest_existing_ancestor, capacity) = match capacity {
                Some((nearest_existing_ancestor, capacity)) => {
                    (nearest_existing_ancestor, Some(capacity))
                }
                None => (false, None),
            };
            let same_filesystem_as_workspace =
                capacity
                    .zip(workspace_capacity)
                    .map(|(path_capacity, workspace)| {
                        path_capacity.filesystem_id == workspace.filesystem_id
                    });

            PathCapacityProbe {
                label: spec.label,
                role: spec.role,
                path: options
                    .include_paths
                    .then(|| spec.path.to_string_lossy().into_owned()),
                exists,
                nearest_existing_ancestor,
                same_filesystem_as_workspace,
                total_bytes: capacity.map(|capacity| capacity.total_bytes),
                available_bytes: capacity.map(|capacity| capacity.available_bytes),
                redaction: if options.include_paths {
                    "path_emitted_by_operator_request"
                } else {
                    "path_not_emitted"
                },
            }
        })
        .collect()
}

fn capacity_for_path(path: &Path) -> Option<(bool, FsCapacity)> {
    let existing = nearest_existing_path(path)?;
    let used_ancestor = existing != path;
    statvfs_capacity(&existing).map(|capacity| (used_ancestor, capacity))
}

fn nearest_existing_path(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

#[cfg(unix)]
fn statvfs_capacity(path: &Path) -> Option<FsCapacity> {
    let stat = rustix::fs::statvfs(path).ok()?;
    let block_size = if stat.f_frsize == 0 {
        stat.f_bsize
    } else {
        stat.f_frsize
    };
    Some(FsCapacity {
        total_bytes: stat.f_blocks.saturating_mul(block_size),
        available_bytes: stat.f_bavail.saturating_mul(block_size),
        filesystem_id: stat.f_fsid,
    })
}

#[cfg(not(unix))]
fn statvfs_capacity(path: &Path) -> Option<FsCapacity> {
    let _ = fs::metadata(path).ok()?;
    // Rust std has no portable filesystem-capacity API; callers already
    // surface capacity absence through the existing degraded path.
    None
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolProbe {
    pub name: &'static str,
    pub available: bool,
    pub source: &'static str,
}

fn gather_tool_probes(path_env: Option<&OsStr>) -> Vec<ToolProbe> {
    TOOL_NAMES
        .iter()
        .map(|name| ToolProbe {
            name,
            available: path_env.is_some_and(|paths| path_contains_tool(paths, name)),
            source: "path_lookup_presence_only",
        })
        .collect()
}

fn path_contains_tool(path_env: &OsStr, tool_name: &str) -> bool {
    env::split_paths(path_env).any(|dir| {
        let candidate = dir.join(tool_name);
        candidate.is_file()
            || env::consts::EXE_SUFFIX
                .strip_prefix('.')
                .is_some_and(|suffix| dir.join(format!("{tool_name}.{suffix}")).is_file())
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProbe {
    pub tmpdir_configured: bool,
    pub cargo_target_dir_configured: bool,
    pub rch_hint_configured: bool,
    pub redaction: &'static str,
}

impl EnvironmentProbe {
    #[must_use]
    pub fn gather() -> Self {
        Self {
            tmpdir_configured: env::var_os("TMPDIR").is_some(),
            cargo_target_dir_configured: env::var_os("CARGO_TARGET_DIR").is_some(),
            rch_hint_configured: env::var_os("RCH_QUEUE_WHEN_BUSY").is_some()
                || env::var_os("RCH_VISIBILITY").is_some()
                || env::var_os("RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS").is_some(),
            redaction: "presence_only",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostTopologyProbe {
    pub rch: RchTopologyProbe,
}

impl HostTopologyProbe {
    #[must_use]
    pub fn gather(path_env: Option<&OsStr>) -> Self {
        Self {
            rch: RchTopologyProbe::gather(path_env),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RchTopologyProbe {
    pub available: bool,
    pub status: &'static str,
    pub posture: &'static str,
    pub source: &'static str,
    pub message: String,
    pub repair: Option<&'static str>,
}

impl RchTopologyProbe {
    #[must_use]
    pub fn gather(path_env: Option<&OsStr>) -> Self {
        let available = path_env.is_some_and(|paths| path_contains_tool(paths, "rch"));
        if available {
            Self {
                available,
                status: "available_not_queried",
                posture: "ok",
                source: "path_lookup_presence_only",
                message:
                    "RCH binary is present on PATH; worker health is checked by RCH-specific diagnostics."
                        .to_string(),
                repair: None,
            }
        } else {
            Self {
                available,
                status: "missing",
                posture: "degraded_recoverable",
                source: "path_lookup_presence_only",
                message: "RCH binary is missing from PATH; ordinary ee commands are not blocked."
                    .to_string(),
                repair: Some("Install rch or provide it on PATH before heavy Cargo verification."),
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostProbeDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: &'static str,
}

impl HostProbeDegradation {
    #[must_use]
    pub fn warning(code: &'static str, message: impl Into<String>, repair: &'static str) -> Self {
        Self {
            code,
            severity: "warning",
            message: message.into(),
            repair,
        }
    }
}

fn serialize_host_probe_degradations<S>(
    degraded: &[HostProbeDegradation],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    aggregate_host_probe_degradations(degraded).serialize(serializer)
}

fn serialize_verification_degradations<S>(
    degraded: &[VerificationDegradation],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    aggregate_verification_degradations(degraded).serialize(serializer)
}

fn serialize_profile_budget_conformance_degradations<S>(
    degraded: &[ProfileBudgetConformanceDegradation],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    aggregate_profile_budget_conformance_degradations(degraded).serialize(serializer)
}

fn aggregate_host_probe_degradations(
    degraded: &[HostProbeDegradation],
) -> Vec<AggregatedDegradation> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "profile_host_probe",
            entry.code.to_owned(),
            entry.severity.to_owned(),
            entry.message.clone(),
            entry.repair.to_owned(),
        )
    }))
}

fn aggregate_verification_degradations(
    degraded: &[VerificationDegradation],
) -> Vec<AggregatedDegradation> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "profile_verification_recipe",
            entry.code.to_owned(),
            entry.severity.to_owned(),
            entry.message.clone(),
            entry.repair.to_owned(),
        )
    }))
}

fn aggregate_profile_budget_conformance_degradations(
    degraded: &[ProfileBudgetConformanceDegradation],
) -> Vec<AggregatedDegradation> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "profile_budget_conformance",
            entry.code.clone(),
            entry.severity.as_str().to_owned(),
            entry.message.clone(),
            entry
                .repair
                .clone()
                .unwrap_or_else(|| "Review profile budget conformance details.".to_owned()),
        )
    }))
}

fn parse_proc_meminfo_bytes(input: &str) -> (Option<u64>, Option<u64>) {
    let mut total = None;
    let mut available = None;

    for line in input.lines() {
        if let Some(value) = parse_meminfo_kib(line, "MemTotal:") {
            total = Some(value);
        } else if let Some(value) = parse_meminfo_kib(line, "MemAvailable:") {
            available = Some(value);
        }
    }

    (total, available)
}

fn parse_meminfo_kib(line: &str, key: &str) -> Option<u64> {
    let rest = line.strip_prefix(key)?;
    let mut parts = rest.split_whitespace();
    let amount = parts.next()?.parse::<u64>().ok()?;
    let unit = parts.next().unwrap_or("kB");
    if unit == "kB" {
        Some(amount.saturating_mul(1024))
    } else {
        None
    }
}

fn read_cgroup_memory_limit_bytes() -> Option<u64> {
    let value = fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?;
    let trimmed = value.trim();
    if trimmed == "max" {
        None
    } else {
        trimmed.parse::<u64>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn ensure_true(condition: bool, ctx: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(ctx.to_string())
        }
    }

    fn config_options(
        workspace_root: &Path,
        profile: OperatingProfile,
        dry_run: bool,
    ) -> ProfileConfigOptions {
        ProfileConfigOptions {
            workspace_root: workspace_root.to_path_buf(),
            config_path: None,
            requested_profile: Some(profile),
            dry_run,
        }
    }

    #[test]
    fn proc_meminfo_parser_extracts_total_and_available_bytes() -> TestResult {
        let (total, available) = parse_proc_meminfo_bytes(
            "MemTotal:       32768000 kB\nMemAvailable:   16384000 kB\nSwapTotal:             0 kB\n",
        );

        ensure(total, Some(33_554_432_000), "total bytes")?;
        ensure(available, Some(16_777_216_000), "available bytes")
    }

    #[test]
    fn proc_meminfo_parser_rejects_unknown_units() -> TestResult {
        let (total, available) =
            parse_proc_meminfo_bytes("MemTotal: 1024 blocks\nMemAvailable: 512 kB\n");

        ensure(total, None, "unknown total unit rejected")?;
        ensure(available, Some(524_288), "available kB parsed")
    }

    #[test]
    fn tool_probe_order_is_stable_and_presence_only() -> TestResult {
        let probes = gather_tool_probes(None);

        let names: Vec<&str> = probes.iter().map(|probe| probe.name).collect();
        ensure(names, TOOL_NAMES.to_vec(), "tool order")?;
        ensure_true(
            probes.iter().all(|probe| !probe.available),
            "missing PATH reports all tools unavailable",
        )?;
        ensure_true(
            probes
                .iter()
                .all(|probe| probe.source == "path_lookup_presence_only"),
            "tool source is presence-only",
        )
    }

    #[test]
    fn report_serialization_omits_raw_paths_and_env_values() -> TestResult {
        let workspace = Path::new("/very/secret/project-name");
        let report = HostResourceProbeReport::gather_for_workspace(workspace);
        let json = serde_json::to_string(&report).map_err(|error| error.to_string())?;

        ensure_true(
            !json.contains("/very/secret") && !json.contains("project-name"),
            "serialized report omits raw workspace path",
        )?;
        ensure_true(
            json.contains(HOST_PROFILE_PROBE_SCHEMA_V1),
            "serialized report includes schema",
        )?;
        ensure_true(
            json.contains("label_only_paths_presence_only_env"),
            "serialized report includes redaction posture",
        )
    }

    #[test]
    fn path_probe_labels_are_stable() -> TestResult {
        let probes = gather_path_probes(Path::new("/workspace/example"));
        let labels: Vec<&str> = probes.iter().map(|probe| probe.label).collect();

        ensure(
            labels,
            vec![
                "workspace",
                "ee_state",
                "database",
                "index",
                "cache",
                "temp",
                "cargo_target",
            ],
            "path labels",
        )?;
        ensure_true(
            probes
                .iter()
                .all(|probe| probe.redaction == "path_not_emitted"),
            "path probes never emit raw paths",
        )
    }

    #[test]
    fn full_path_probe_mode_is_explicit() -> TestResult {
        let workspace = Path::new("/workspace/example");
        let redacted = gather_path_probes(workspace);
        ensure_true(
            redacted.iter().all(|probe| probe.path.is_none()),
            "default path probes omit raw paths",
        )?;

        let full = gather_path_probes_with_options(
            workspace,
            &HostProfileProbeOptions {
                include_paths: true,
            },
        );
        ensure_true(
            full.iter().any(|probe| probe.path.is_some()),
            "full path mode emits paths",
        )?;
        ensure_true(
            full.iter()
                .all(|probe| probe.redaction == "path_emitted_by_operator_request"),
            "full path mode reports operator-requested path emission",
        )
    }

    #[test]
    fn missing_rch_is_degraded_recoverable() -> TestResult {
        let topology = HostTopologyProbe::gather(Some(OsStr::new("/definitely/no/rch/here")));
        ensure_true(!topology.rch.available, "rch unavailable")?;
        ensure(
            topology.rch.posture,
            "degraded_recoverable",
            "rch missing posture",
        )?;
        ensure(
            topology.rch.status,
            "missing",
            "missing rch status is stable",
        )
    }

    #[test]
    fn profile_config_plan_reports_exact_toml_without_writing() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join(".ee").join("config.toml");
        let options = config_options(temp.path(), OperatingProfile::Portable, true);

        let report = plan_profile_config(&options).map_err(|error| error.to_string())?;
        let rendered = serde_json::to_value(&report).map_err(|error| error.to_string())?;

        ensure(report.schema, PROFILE_CONFIG_PLAN_SCHEMA_V1, "schema")?;
        ensure(
            report.profile.effective,
            OperatingProfile::Portable,
            "effective profile",
        )?;
        ensure_true(report.dry_run, "plan reports dry-run posture")?;
        ensure_true(report.would_write, "new config has pending edits")?;
        ensure_true(!report.applied, "plan does not apply")?;
        ensure_true(!config_path.exists(), "plan does not write config")?;
        ensure_true(
            report.planned_toml.contains("selected = \"portable\""),
            "planned TOML contains requested profile",
        )?;
        ensure_true(
            report.planned_toml.contains("pack_max_tokens = "),
            "planned TOML contains budget keys",
        )?;
        ensure_true(
            rendered.get("plannedToml").is_some(),
            "JSON uses stable camelCase plannedToml field",
        )?;
        ensure_true(
            rendered.get("wouldWrite").is_some(),
            "JSON uses stable camelCase wouldWrite field",
        )
    }

    #[test]
    fn profile_config_apply_dry_run_does_not_write() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join(".ee").join("config.toml");
        let options = config_options(temp.path(), OperatingProfile::Workstation, true);

        let report = apply_profile_config(&options).map_err(|error| error.to_string())?;

        ensure_true(report.dry_run, "apply reports dry-run posture")?;
        ensure_true(report.would_write, "dry-run apply has pending edits")?;
        ensure_true(!report.applied, "dry-run apply does not write")?;
        ensure_true(
            report
                .edits
                .iter()
                .any(|edit| edit.key == "profile.selected" && edit.status == "planned"),
            "dry-run keeps selected profile edit planned",
        )?;
        ensure_true(!config_path.exists(), "dry-run apply leaves config absent")
    }

    #[test]
    fn profile_config_apply_writes_and_next_plan_is_unchanged() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join(".ee").join("config.toml");
        let options = config_options(temp.path(), OperatingProfile::Swarm, false);

        let applied = apply_profile_config(&options).map_err(|error| error.to_string())?;
        let saved = fs::read_to_string(&config_path).map_err(|error| error.to_string())?;
        let runtime = runtime_profile_for_workspace(temp.path());
        let next_plan = plan_profile_config(&options).map_err(|error| error.to_string())?;

        ensure_true(applied.applied, "apply reports written config")?;
        ensure(applied.repair, None, "successful apply clears repair hint")?;
        ensure(saved, applied.planned_toml, "written TOML matches plan")?;
        ensure(
            runtime.active_profile,
            OperatingProfile::Swarm,
            "runtime profile reads selected config",
        )?;
        ensure_true(
            !next_plan.would_write,
            "planning after apply reports no pending write",
        )?;
        ensure_true(
            next_plan
                .edits
                .iter()
                .all(|edit| edit.status == "unchanged"),
            "planning after apply marks every edit unchanged",
        )
    }

    #[cfg(unix)]
    #[test]
    fn profile_config_apply_rejects_symlinked_metadata_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_metadata = temp.path().join("real-ee");
        fs::create_dir_all(&real_metadata).map_err(|error| error.to_string())?;
        symlink(&real_metadata, temp.path().join(".ee")).map_err(|error| error.to_string())?;

        let options = config_options(temp.path(), OperatingProfile::Swarm, false);
        let error = apply_profile_config(&options)
            .expect_err("symlinked .ee parent should reject profile config apply");
        ensure_true(
            error.to_string().contains("symlinked path component"),
            "symlinked .ee error message",
        )?;
        ensure_true(
            !real_metadata.join("config.toml").exists(),
            "profile config apply must not write through symlinked .ee",
        )
    }

    #[cfg(unix)]
    #[test]
    fn profile_config_plan_rejects_symlinked_config_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_dir = temp.path().join(".ee");
        fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        let outside_config = temp.path().join("outside-config.toml");
        fs::write(
            &outside_config,
            "[profile]\nselected = \"swarm\"\n[profile.budgets]\npack_max_tokens = 4096\n",
        )
        .map_err(|error| error.to_string())?;
        symlink(&outside_config, config_dir.join("config.toml"))
            .map_err(|error| error.to_string())?;

        let options = config_options(temp.path(), OperatingProfile::Portable, true);
        let error = plan_profile_config(&options)
            .expect_err("symlinked config file should reject profile config plan");
        ensure_true(
            error.to_string().contains("symlinked path component"),
            "symlinked config error message",
        )?;
        ensure(
            selected_profile_from_config(temp.path()),
            None,
            "runtime profile must not read symlinked config",
        )
    }

    #[cfg(unix)]
    #[test]
    fn profile_config_final_read_open_rejects_swapped_symlink_file() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_dir = temp.path().join(".ee");
        fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        let original_config = config_dir.join("config.toml.validated");
        let original_text = "[profile]\nselected = \"portable\"\n";
        fs::write(&original_config, original_text).map_err(|error| error.to_string())?;
        let outside_config = temp.path().join("outside-config.toml");
        let outside_text = "[profile]\nselected = \"swarm\"\n";
        fs::write(&outside_config, outside_text).map_err(|error| error.to_string())?;
        let config_path = config_dir.join("config.toml");
        symlink(&outside_config, &config_path).map_err(|error| error.to_string())?;

        let error = open_profile_config_file_for_read(&config_path)
            .expect_err("final profile config read open must reject symlinks");

        ensure_true(
            error.kind() != io::ErrorKind::NotFound,
            "final symlink read should fail because the path is a symlink",
        )?;
        ensure(
            fs::read_to_string(&outside_config).map_err(|error| error.to_string())?,
            outside_text.to_owned(),
            "profile config read helper must not follow the symlink target",
        )?;
        ensure(
            fs::read_to_string(&original_config).map_err(|error| error.to_string())?,
            original_text.to_owned(),
            "validated profile config copy must remain untouched",
        )
    }

    #[test]
    fn profile_config_plan_rejects_config_directory() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        fs::create_dir_all(temp.path().join(".ee").join("config.toml"))
            .map_err(|error| error.to_string())?;

        let options = config_options(temp.path(), OperatingProfile::Portable, true);
        let error = plan_profile_config(&options)
            .expect_err("profile config plan should reject config directory");

        ensure_true(
            error.to_string().contains("not a regular file"),
            "config directory error message",
        )?;
        ensure(
            selected_profile_from_config(temp.path()),
            None,
            "runtime profile must not read config directory",
        )
    }

    #[test]
    fn profile_config_apply_rejects_config_directory_before_write() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join(".ee").join("config.toml");
        fs::create_dir_all(&config_path).map_err(|error| error.to_string())?;

        let options = config_options(temp.path(), OperatingProfile::Swarm, false);
        let error = apply_profile_config(&options)
            .expect_err("profile config apply should reject config directory before write");

        ensure_true(
            error.to_string().contains("not a regular file"),
            "apply error reports non-regular config path",
        )?;
        ensure_true(
            config_path.is_dir(),
            "profile config apply leaves non-regular config path untouched",
        )?;
        ensure(
            selected_profile_from_config(temp.path()),
            None,
            "runtime profile must not read config directory after failed apply",
        )
    }

    #[test]
    fn profile_config_write_preflight_rejects_non_regular_final_path() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join(".ee").join("config.toml");
        fs::create_dir_all(&config_path).map_err(|error| error.to_string())?;

        let error = ensure_profile_config_write_path_is_regular_or_missing(&config_path)
            .expect_err("write preflight should reject a directory config path");

        ensure_true(
            error.to_string().contains("not a regular file"),
            "write preflight error mentions non-regular path",
        )?;
        ensure_true(
            config_path.is_dir(),
            "write preflight leaves non-regular config path untouched",
        )
    }

    #[test]
    fn profile_config_apply_rejects_existing_temp_file_without_truncating() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join(".ee").join("config.toml");
        let temp_path = config_path.with_extension("toml.tmp");
        fs::create_dir_all(temp_path.parent().expect("profile config temp parent"))
            .map_err(|error| error.to_string())?;
        fs::write(&temp_path, "stale profile temp").map_err(|error| error.to_string())?;

        let options = config_options(temp.path(), OperatingProfile::Swarm, false);
        let error =
            apply_profile_config(&options).expect_err("existing temp file should reject apply");

        ensure_true(
            error.to_string().contains("already exists"),
            "existing temp error message",
        )?;
        ensure(
            fs::read_to_string(&temp_path).map_err(|error| error.to_string())?,
            "stale profile temp".to_owned(),
            "existing temp content remains unchanged",
        )?;
        ensure_true(
            !config_path.exists(),
            "profile config apply must not publish final config when temp exists",
        )
    }

    #[cfg(unix)]
    #[test]
    fn profile_config_publish_rechecks_final_symlink_before_rename() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join(".ee").join("config.toml");
        let temp_path = config_path.with_extension("toml.tmp");
        fs::create_dir_all(temp_path.parent().expect("profile config temp parent"))
            .map_err(|error| error.to_string())?;
        write_profile_config_temp_file(&temp_path, b"[profile]\nselected = \"swarm\"\n")
            .map_err(|error| error.to_string())?;

        let outside_config = temp.path().join("outside-config.toml");
        fs::write(&outside_config, "outside sentinel").map_err(|error| error.to_string())?;
        symlink(&outside_config, &config_path).map_err(|error| error.to_string())?;

        let error = publish_profile_config_temp_file(&temp_path, &config_path)
            .expect_err("final symlink must be rejected before profile config publish");
        ensure_true(
            error.to_string().contains("symlinked path component"),
            "final symlink publish error message",
        )?;
        ensure(
            fs::read_to_string(&outside_config).map_err(|error| error.to_string())?,
            "outside sentinel".to_owned(),
            "outside symlink target remains unchanged",
        )?;
        ensure(
            fs::read_to_string(&temp_path).map_err(|error| error.to_string())?,
            "[profile]\nselected = \"swarm\"\n".to_owned(),
            "temp config remains available after rejected publish",
        )
    }

    #[cfg(unix)]
    #[test]
    fn profile_config_publish_rechecks_temp_symlink_before_rename() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = temp.path().join(".ee").join("config.toml");
        let temp_path = config_path.with_extension("toml.tmp");
        let preserved_temp = config_path.with_extension("toml.tmp.preserved");
        fs::create_dir_all(temp_path.parent().expect("profile config temp parent"))
            .map_err(|error| error.to_string())?;
        write_profile_config_temp_file(&temp_path, b"[profile]\nselected = \"swarm\"\n")
            .map_err(|error| error.to_string())?;
        fs::rename(&temp_path, &preserved_temp).map_err(|error| error.to_string())?;

        let outside_config = temp.path().join("outside-config.toml");
        fs::write(&outside_config, "outside sentinel").map_err(|error| error.to_string())?;
        symlink(&outside_config, &temp_path).map_err(|error| error.to_string())?;

        let error = publish_profile_config_temp_file(&temp_path, &config_path)
            .expect_err("temp symlink must be rejected before profile config publish");
        ensure_true(
            error.to_string().contains("symlinked path component")
                || error.to_string().contains("not a regular file"),
            "temp symlink publish error message",
        )?;
        ensure_true(
            !config_path.exists(),
            "profile config must not publish through swapped temp symlink",
        )?;
        ensure(
            fs::read_to_string(&outside_config).map_err(|error| error.to_string())?,
            "outside sentinel".to_owned(),
            "outside symlink target remains unchanged",
        )?;
        ensure_true(
            fs::symlink_metadata(&temp_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            "rejected temp config symlink remains for inspection",
        )?;
        ensure(
            fs::read_to_string(&preserved_temp).map_err(|error| error.to_string())?,
            "[profile]\nselected = \"swarm\"\n".to_owned(),
            "preserved temp config remains available after simulated swap",
        )
    }

    #[test]
    fn profile_config_conflict_blocks_write() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_dir = temp.path().join(".ee");
        let config_path = config_dir.join("config.toml");
        fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        fs::write(&config_path, "profile = 7\n").map_err(|error| error.to_string())?;
        let before = fs::read_to_string(&config_path).map_err(|error| error.to_string())?;
        let options = config_options(temp.path(), OperatingProfile::Constrained, false);

        let report = apply_profile_config(&options).map_err(|error| error.to_string())?;
        let after = fs::read_to_string(&config_path).map_err(|error| error.to_string())?;

        ensure_true(
            report.has_conflicts(),
            "conflicting profile table is reported",
        )?;
        ensure_true(!report.applied, "conflict blocks apply")?;
        ensure(
            report
                .conflicts
                .first()
                .map(|conflict| conflict.key.as_str()),
            Some("profile"),
            "conflict key",
        )?;
        ensure_true(
            report.edits.iter().all(|edit| edit.status == "blocked"),
            "conflict blocks all planned edits",
        )?;
        ensure(after, before, "conflict leaves config unchanged")
    }

    fn probe_with_resources(
        logical_cores: Option<u32>,
        memory_gib: u64,
    ) -> HostResourceProbeReport {
        HostResourceProbeReport {
            schema: HOST_PROFILE_PROBE_SCHEMA_V1,
            side_effect_free: true,
            redaction: "label_only_paths_presence_only_env",
            complete: logical_cores.is_some() && memory_gib > 0,
            workspace: WorkspaceProbe {
                label: "workspace",
                initialized: true,
                redaction: "path_not_emitted",
            },
            cpu: CpuProbe {
                logical_cores,
                physical_cores: logical_cores.map(|c| c / 2).or(Some(1)),
                source: "test_synthetic",
            },
            memory: MemoryProbe {
                total_bytes: Some(memory_gib * GIB),
                available_bytes: Some(memory_gib * GIB),
                cgroup_limit_bytes: None,
                source: "test_synthetic",
            },
            paths: vec![],
            tools: vec![],
            environment: EnvironmentProbe {
                tmpdir_configured: false,
                cargo_target_dir_configured: false,
                rch_hint_configured: false,
                redaction: "presence_only",
            },
            topology: HostTopologyProbe {
                rch: RchTopologyProbe {
                    available: true,
                    status: "available_not_queried",
                    posture: "ok",
                    source: "test_synthetic",
                    message: "RCH available for synthetic test probe.".to_string(),
                    repair: None,
                },
            },
            degraded: vec![],
        }
    }

    #[test]
    fn host_probe_degradation_codes_are_stable() -> TestResult {
        let degraded = host_probe_degradations(
            &CpuProbe {
                logical_cores: None,
                physical_cores: None,
                source: "test_unavailable",
            },
            &MemoryProbe {
                total_bytes: None,
                available_bytes: None,
                cgroup_limit_bytes: None,
                source: "test_unavailable",
            },
            &[PathCapacityProbe {
                label: "workspace",
                role: "workspace_root",
                path: None,
                exists: false,
                nearest_existing_ancestor: false,
                same_filesystem_as_workspace: None,
                total_bytes: None,
                available_bytes: None,
                redaction: "path_not_emitted",
            }],
        );

        ensure(degraded.len(), 3usize, "degradation count")?;
        ensure(
            degraded[0].code,
            "cpu_probe_unavailable",
            "cpu degradation code",
        )?;
        ensure(degraded[0].severity, "warning", "cpu severity")?;
        ensure(
            degraded[0].repair,
            "Run `ee status --json` and check host permissions.",
            "cpu repair hint",
        )?;
        ensure(
            degraded[1].code,
            "memory_probe_unavailable",
            "memory degradation code",
        )?;
        ensure(degraded[1].severity, "warning", "memory severity")?;
        ensure(
            degraded[1].repair,
            "Run on a platform with /proc/meminfo or provide explicit profile config.",
            "memory repair hint",
        )?;
        ensure(
            degraded[2].code,
            "path_capacity_unavailable",
            "path degradation code",
        )?;
        ensure(degraded[2].severity, "warning", "path severity")?;
        ensure_true(
            degraded[2].message.contains("`workspace`"),
            "path degradation includes stable label",
        )?;
        ensure(
            degraded[2].repair,
            "Check filesystem permissions or configure profile budgets explicitly.",
            "path repair hint",
        )
    }

    #[test]
    fn host_probe_serializes_aggregated_degraded_entries() -> TestResult {
        let mut report = probe_with_resources(Some(8), 32);
        report.complete = false;
        report.degraded = vec![
            HostProbeDegradation::warning(
                "profile_fixture_degraded",
                "first duplicate",
                "first repair",
            ),
            HostProbeDegradation::warning(
                "profile_fixture_degraded",
                "second duplicate",
                "second repair",
            ),
        ];

        let value = serde_json::to_value(&report).map_err(|error| error.to_string())?;
        let degraded = value
            .get("degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("degraded array missing: {value}"))?;

        ensure(
            degraded.len(),
            1usize,
            "duplicate host probe degraded entries aggregate",
        )?;
        ensure(
            degraded[0]["code"].as_str(),
            Some("profile_fixture_degraded"),
            "aggregated host probe code",
        )?;
        ensure(
            degraded[0]["sources"].clone(),
            serde_json::json!(["profile_host_probe"]),
            "host probe source label",
        )
    }

    #[test]
    fn recommend_swarm_for_high_resource_host() -> TestResult {
        let probe = probe_with_resources(Some(16), 64);
        let result = recommend_operating_profile(&probe);

        ensure(result.recommended, OperatingProfile::Swarm, "swarm profile")?;
        ensure(
            result.effective,
            OperatingProfile::Swarm,
            "effective profile",
        )?;
        ensure(result.confidence, "high", "high confidence with full probe")
    }

    #[test]
    fn recommend_workstation_for_mid_resource_host() -> TestResult {
        let probe = probe_with_resources(Some(8), 24);
        let result = recommend_operating_profile(&probe);

        ensure(
            result.recommended,
            OperatingProfile::Workstation,
            "workstation profile",
        )?;
        ensure(
            result.effective,
            OperatingProfile::Workstation,
            "effective profile",
        )
    }

    #[test]
    fn recommend_portable_for_laptop_resources() -> TestResult {
        let probe = probe_with_resources(Some(4), 12);
        let result = recommend_operating_profile(&probe);

        ensure(
            result.recommended,
            OperatingProfile::Portable,
            "portable profile",
        )?;
        ensure(
            result.effective,
            OperatingProfile::Portable,
            "effective profile",
        )
    }

    #[test]
    fn recommend_constrained_for_low_resources() -> TestResult {
        let probe = probe_with_resources(Some(2), 4);
        let result = recommend_operating_profile(&probe);

        ensure(
            result.recommended,
            OperatingProfile::Constrained,
            "constrained profile",
        )?;
        ensure(
            result.effective,
            OperatingProfile::Constrained,
            "effective profile",
        )
    }

    #[test]
    fn recommend_constrained_when_probe_incomplete() -> TestResult {
        let probe = probe_with_resources(None, 0);
        let result = recommend_operating_profile(&probe);

        ensure(
            result.recommended,
            OperatingProfile::Constrained,
            "constrained fallback",
        )?;
        ensure(
            result.confidence,
            "medium",
            "medium confidence without probe data",
        )
    }

    #[test]
    fn profile_thresholds_are_deterministic_at_boundaries() -> TestResult {
        // Swarm boundary: 12 cores AND 32 GiB
        let at_swarm = probe_with_resources(Some(12), 32);
        ensure(
            recommend_operating_profile(&at_swarm).recommended,
            OperatingProfile::Swarm,
            "exactly at swarm threshold",
        )?;

        // Just below swarm (cores)
        let below_cores = probe_with_resources(Some(11), 32);
        ensure(
            recommend_operating_profile(&below_cores).recommended,
            OperatingProfile::Workstation,
            "below swarm cores threshold",
        )?;

        // Just below swarm (memory)
        let below_mem = probe_with_resources(Some(12), 31);
        ensure(
            recommend_operating_profile(&below_mem).recommended,
            OperatingProfile::Workstation,
            "below swarm memory threshold",
        )?;

        // Workstation boundary: 6 cores AND 16 GiB
        let at_workstation = probe_with_resources(Some(6), 16);
        ensure(
            recommend_operating_profile(&at_workstation).recommended,
            OperatingProfile::Workstation,
            "exactly at workstation threshold",
        )?;

        // Just below workstation
        let below_workstation = probe_with_resources(Some(5), 16);
        ensure(
            recommend_operating_profile(&below_workstation).recommended,
            OperatingProfile::Portable,
            "below workstation threshold",
        )?;

        // Portable boundary: 2 cores AND 8 GiB
        let at_portable = probe_with_resources(Some(2), 8);
        ensure(
            recommend_operating_profile(&at_portable).recommended,
            OperatingProfile::Portable,
            "exactly at portable threshold",
        )?;

        // Below portable
        let below_portable = probe_with_resources(Some(1), 8);
        ensure(
            recommend_operating_profile(&below_portable).recommended,
            OperatingProfile::Constrained,
            "below portable threshold",
        )
    }

    #[test]
    fn profile_budgets_scale_with_profile() -> TestResult {
        let constrained = ProfileBudgets::for_profile(OperatingProfile::Constrained);
        let portable = ProfileBudgets::for_profile(OperatingProfile::Portable);
        let workstation = ProfileBudgets::for_profile(OperatingProfile::Workstation);
        let swarm = ProfileBudgets::for_profile(OperatingProfile::Swarm);

        ensure_true(
            constrained.search.candidate_limit < portable.search.candidate_limit,
            "portable has higher search limit than constrained",
        )?;
        ensure_true(
            portable.search.candidate_limit < workstation.search.candidate_limit,
            "workstation has higher search limit than portable",
        )?;
        ensure_true(
            workstation.search.candidate_limit < swarm.search.candidate_limit,
            "swarm has highest search limit",
        )?;

        ensure_true(
            constrained.pack.max_tokens < portable.pack.max_tokens,
            "portable has higher token limit than constrained",
        )?;
        ensure_true(
            portable.pack.max_tokens < workstation.pack.max_tokens,
            "workstation has higher token limit than portable",
        )?;
        ensure_true(
            workstation.pack.max_tokens < swarm.pack.max_tokens,
            "swarm has highest token limit",
        )?;

        ensure_true(
            constrained.cache.memory_cap_mb < swarm.cache.memory_cap_mb,
            "swarm has larger cache than constrained",
        )
    }

    #[test]
    fn runtime_profile_caps_context_search_and_pack_budgets() -> TestResult {
        let profile = RuntimeProfileReport::for_profile(OperatingProfile::Constrained, "test");

        ensure(
            profile.cap_search_limit(12),
            (12, false),
            "search under cap",
        )?;
        ensure(profile.cap_search_limit(4_000), (48, true), "search cap")?;
        ensure(
            profile.cap_pack_max_tokens(2_000),
            (2_000, false),
            "pack tokens under cap",
        )?;
        ensure(
            profile.cap_pack_max_tokens(50_000),
            (3_000, true),
            "pack token cap",
        )?;
        ensure(
            profile.cap_pack_candidate_pool(12),
            (12, false),
            "candidate pool under cap",
        )?;
        ensure(
            profile.cap_pack_candidate_pool(1_000),
            (24, true),
            "candidate pool cap",
        )
    }

    #[test]
    fn runtime_profile_caps_index_jobs_from_write_spool_budget() -> TestResult {
        let profile = RuntimeProfileReport::for_profile(OperatingProfile::Constrained, "test");

        ensure(
            profile.cap_index_job_limit(Some(12)),
            (Some(12), false),
            "index job limit under cap",
        )?;
        ensure(
            profile.cap_index_job_limit(Some(4_000)),
            (Some(32), true),
            "index job limit cap",
        )?;
        ensure(
            profile.cap_index_job_limit(None),
            (Some(32), true),
            "missing index job limit defaults to profile cap",
        )
    }

    #[test]
    fn verification_recipe_constrained_skips_heavy_gates() -> TestResult {
        let recipe = VerificationRecipe::for_profile(OperatingProfile::Constrained);

        ensure(recipe.recipe_name, "quick", "constrained uses quick recipe")?;
        ensure_true(
            recipe.gates_skipped.iter().any(|s| s.weight == "heavy"),
            "constrained skips heavy gates",
        )?;
        ensure_true(
            !recipe.skipped_heavy_gates().is_empty(),
            "skipped_heavy_gates returns non-empty for constrained",
        )?;
        ensure_true(
            recipe.is_degraded(),
            "constrained verification is degraded due to skipped gates",
        )?;
        ensure(
            recipe.target_dir_strategy.posture,
            "shared",
            "constrained uses shared target dir",
        )
    }

    #[test]
    fn verification_recipe_swarm_includes_all_gates() -> TestResult {
        let recipe = VerificationRecipe::for_profile(OperatingProfile::Swarm);

        ensure(recipe.recipe_name, "full", "swarm uses full recipe")?;
        ensure_true(
            recipe.gates_included.contains(&VerificationGate::E2eTests),
            "swarm includes e2e tests",
        )?;
        ensure_true(
            recipe
                .gates_included
                .contains(&VerificationGate::PropertyTests),
            "swarm includes property tests",
        )?;
        ensure_true(
            recipe.skipped_heavy_gates().is_empty(),
            "swarm skips no heavy gates",
        )?;
        ensure(
            recipe.target_dir_strategy.posture,
            "isolated",
            "swarm uses isolated target dir",
        )
    }

    #[test]
    fn verification_recipe_portable_prefers_rch() -> TestResult {
        let recipe = VerificationRecipe::for_profile(OperatingProfile::Portable);

        ensure(
            recipe.recipe_name,
            "workspace",
            "portable uses workspace recipe",
        )?;
        ensure_true(
            recipe.rch_commands.iter().any(|c| c.requires_rch),
            "portable includes RCH commands",
        )?;
        ensure_true(
            recipe.skipped_heavy_gates().len() == 2,
            "portable skips integration and e2e tests",
        )?;
        ensure_true(
            recipe
                .gates_included
                .contains(&VerificationGate::PropertyTests),
            "portable includes property tests (rch_preferred)",
        )
    }

    #[test]
    fn verification_recipe_skipped_gates_have_manual_commands() -> TestResult {
        let recipe = VerificationRecipe::for_profile(OperatingProfile::Constrained);

        for skipped in &recipe.gates_skipped {
            ensure_true(
                !skipped.manual_command.is_empty(),
                &format!("skipped gate {} has manual command", skipped.gate.as_str()),
            )?;
        }
        Ok(())
    }

    #[test]
    fn verification_recipe_timeout_scales_with_profile() -> TestResult {
        let constrained = VerificationRecipe::for_profile(OperatingProfile::Constrained);
        let workstation = VerificationRecipe::for_profile(OperatingProfile::Workstation);
        let swarm = VerificationRecipe::for_profile(OperatingProfile::Swarm);

        ensure_true(
            constrained.timeout_seconds < workstation.timeout_seconds,
            "workstation has longer timeout than constrained",
        )?;
        ensure_true(
            workstation.timeout_seconds <= swarm.timeout_seconds,
            "swarm has equal or longer timeout than workstation",
        )
    }

    #[test]
    fn verification_gate_weights_are_stable() -> TestResult {
        ensure(
            VerificationGate::CargoCheck.weight(),
            "light",
            "check is light",
        )?;
        ensure(
            VerificationGate::CargoClippy.weight(),
            "medium",
            "clippy is medium",
        )?;
        ensure(
            VerificationGate::CargoTest.weight(),
            "standard",
            "test is standard",
        )?;
        ensure(VerificationGate::E2eTests.weight(), "heavy", "e2e is heavy")
    }

    #[test]
    fn verification_recipe_serializes_to_json() -> TestResult {
        let recipe = VerificationRecipe::for_profile(OperatingProfile::Portable);
        let json = serde_json::to_string(&recipe).map_err(|e| e.to_string())?;

        ensure_true(
            json.contains(VERIFICATION_RECIPE_SCHEMA_V1),
            "JSON contains schema",
        )?;
        ensure_true(json.contains("gatesIncluded"), "JSON has camelCase fields")?;
        ensure_true(json.contains("gatesSkipped"), "JSON has skipped gates")?;
        ensure_true(json.contains("rchCommands"), "JSON has RCH commands")
    }

    #[test]
    fn verification_recipe_serializes_aggregated_degraded_entries() -> TestResult {
        let mut recipe = VerificationRecipe::for_profile(OperatingProfile::Constrained);
        recipe.degraded.push(VerificationDegradation {
            code: "manual_heavy_strategy",
            severity: "high",
            message: "Escalated duplicate".to_string(),
            repair: "Use RCH for heavy gates.",
        });

        let value = serde_json::to_value(&recipe).map_err(|error| error.to_string())?;
        let degraded = value
            .get("degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("degraded array missing: {value}"))?;
        let manual = degraded
            .iter()
            .find(|entry| entry["code"] == "manual_heavy_strategy")
            .ok_or_else(|| format!("manual_heavy_strategy missing: {degraded:?}"))?;

        ensure(
            degraded
                .iter()
                .filter(|entry| entry["code"] == "manual_heavy_strategy")
                .count(),
            1usize,
            "duplicate verification degraded code aggregates",
        )?;
        ensure(
            manual["severity"].as_str(),
            Some("high"),
            "highest severity wins",
        )?;
        ensure(
            manual["sources"].clone(),
            serde_json::json!(["profile_verification_recipe"]),
            "verification source label",
        )
    }

    #[test]
    fn profile_budget_conformance_serializes_aggregated_degraded_entries() -> TestResult {
        let report = ProfileBudgetConformanceReport {
            schema: PROFILE_BUDGET_CONFORMANCE_SCHEMA_V1,
            side_effect_free: true,
            requested_profile: OperatingProfile::Portable,
            advertised_profile: Some(OperatingProfile::Portable),
            effective_profile: OperatingProfile::Portable,
            explicit_overrides: Vec::new(),
            artifact: ProfileBudgetArtifactSummary {
                artifact_id: "artifact_profile_fixture".to_string(),
                source_schema: "ee.test.profile_fixture.v1".to_string(),
                observed_profile: Some("portable".to_string()),
                metric_count: 0,
            },
            status: ProfileBudgetConformanceStatus::Failed,
            checks: Vec::new(),
            degraded: vec![
                ProfileBudgetConformanceDegradation::new(
                    "profile_fixture_mismatch",
                    ProfileBudgetConformanceSeverity::Low,
                    "profile",
                    "fixture.low",
                    "low duplicate",
                    Some("low repair".to_string()),
                ),
                ProfileBudgetConformanceDegradation::new(
                    "profile_fixture_mismatch",
                    ProfileBudgetConformanceSeverity::High,
                    "profile",
                    "fixture.high",
                    "high duplicate",
                    Some("high repair".to_string()),
                ),
            ],
        };

        let value = serde_json::to_value(&report).map_err(|error| error.to_string())?;
        let degraded = value
            .get("degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("degraded array missing: {value}"))?;

        ensure(
            degraded.len(),
            1usize,
            "duplicate profile budget degraded entries aggregate",
        )?;
        ensure(
            degraded[0]["severity"].as_str(),
            Some("high"),
            "profile budget highest severity wins",
        )?;
        ensure(
            degraded[0]["sources"].clone(),
            serde_json::json!(["profile_budget_conformance"]),
            "profile budget source label",
        )
    }
}
