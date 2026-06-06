use crate::config::EnvVar;
use crate::models::{ERROR_SCHEMA_V2, RESPONSE_SCHEMA_V2};

fn normalized_agent_docs_token(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_was_lowercase_or_digit = false;

    for character in value.trim().chars() {
        match character {
            '-' | '_' => {
                if !normalized.ends_with('_') {
                    normalized.push('_');
                }
                previous_was_lowercase_or_digit = false;
            }
            ch if ch.is_ascii_uppercase() => {
                if previous_was_lowercase_or_digit && !normalized.ends_with('_') {
                    normalized.push('_');
                }
                normalized.push(ch.to_ascii_lowercase());
                previous_was_lowercase_or_digit = false;
            }
            ch => {
                normalized.push(ch.to_ascii_lowercase());
                previous_was_lowercase_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
            }
        }
    }

    normalized
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentDocsTopic {
    Guide,
    Commands,
    Contracts,
    Schemas,
    Paths,
    Env,
    ExitCodes,
    Fields,
    Errors,
    Formats,
    Examples,
    Recipes,
}

impl AgentDocsTopic {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Guide => "guide",
            Self::Commands => "commands",
            Self::Contracts => "contracts",
            Self::Schemas => "schemas",
            Self::Paths => "paths",
            Self::Env => "env",
            Self::ExitCodes => "exit-codes",
            Self::Fields => "fields",
            Self::Errors => "errors",
            Self::Formats => "formats",
            Self::Examples => "examples",
            Self::Recipes => "recipes",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Guide => "Getting started guide for agents using ee",
            Self::Commands => "Complete command reference with arguments and flags",
            Self::Contracts => "Stable JSON/TOON output contracts for agent parsing",
            Self::Schemas => "Available response and error schema definitions",
            Self::Paths => "Default paths for database, indexes, and configuration",
            Self::Env => "Environment variables that affect ee behavior",
            Self::ExitCodes => "Exit code meanings for scripting and error handling",
            Self::Fields => "Field profiles and output verbosity levels",
            Self::Errors => "Error codes, categories, and repair suggestions",
            Self::Formats => "Output format options (json, toon, human, etc.)",
            Self::Examples => "Common workflows and command examples for agents",
            Self::Recipes => "Machine-readable workflows with jq selectors and failure branches",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match normalized_agent_docs_token(s).as_str() {
            "guide" => Some(Self::Guide),
            "commands" => Some(Self::Commands),
            "contracts" => Some(Self::Contracts),
            "schemas" => Some(Self::Schemas),
            "paths" => Some(Self::Paths),
            "env" => Some(Self::Env),
            "exit_codes" => Some(Self::ExitCodes),
            "fields" => Some(Self::Fields),
            "errors" => Some(Self::Errors),
            "formats" => Some(Self::Formats),
            "examples" => Some(Self::Examples),
            "recipes" => Some(Self::Recipes),
            _ => None,
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Guide,
            Self::Commands,
            Self::Contracts,
            Self::Schemas,
            Self::Paths,
            Self::Env,
            Self::ExitCodes,
            Self::Fields,
            Self::Errors,
            Self::Formats,
            Self::Examples,
            Self::Recipes,
        ]
    }
}

#[derive(Clone, Debug)]
pub struct AgentDocsReport {
    pub version: &'static str,
    pub topic: Option<AgentDocsTopic>,
}

impl AgentDocsReport {
    #[must_use]
    pub fn new(topic: Option<AgentDocsTopic>) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            topic,
        }
    }

    #[must_use]
    pub fn gather(topic: Option<AgentDocsTopic>) -> Self {
        Self::new(topic)
    }
}

#[derive(Clone, Debug)]
pub struct GuideSection {
    pub title: &'static str,
    pub content: &'static str,
}

pub const GUIDE_SECTIONS: &[GuideSection] = &[
    GuideSection {
        title: "Overview",
        content: "ee is a durable, local-first, explainable memory substrate for coding agents. It captures facts, work history, decisions, procedural rules, failures, and evidence; indexes them with hybrid search; and emits compact, explainable context packs.",
    },
    GuideSection {
        title: "Primary Workflow",
        content: "ee pack \"<task>\" --workspace . --max-tokens 4000 --json",
    },
    GuideSection {
        title: "Machine Output",
        content: "Always use --json, --robot, or --format=json for machine-parseable output. stdout is data, stderr is diagnostics. Check exit codes for error conditions.",
    },
    GuideSection {
        title: "Workspace",
        content: "ee operates on workspaces (directories). Use --workspace to specify the target, or it defaults to the current directory. The .ee/ folder contains the database and indexes.",
    },
    GuideSection {
        title: "Degradation",
        content: "ee degrades gracefully. If semantic search is unavailable, it falls back to lexical. If the database is missing, init creates it. Check ee status --json for capability state.",
    },
];

#[derive(Clone, Debug)]
pub struct PathEntry {
    pub name: &'static str,
    pub default: &'static str,
    pub description: &'static str,
    pub env_override: Option<&'static str>,
}

pub const DEFAULT_PATHS: &[PathEntry] = &[
    PathEntry {
        name: "database",
        default: "<workspace>/.ee/ee.db",
        description: "SQLite database storing memories, sessions, and metadata",
        env_override: Some(EnvVar::DatabasePath.name()),
    },
    PathEntry {
        name: "index_dir",
        default: "<workspace>/.ee/index/",
        description: "Directory containing search indexes",
        env_override: Some(EnvVar::IndexDir.name()),
    },
    PathEntry {
        name: "config",
        default: "<workspace>/.ee/config.toml",
        description: "Workspace-specific configuration file",
        env_override: None,
    },
    PathEntry {
        name: "global_config",
        default: "~/.config/ee/config.toml",
        description: "Global user configuration file",
        env_override: None,
    },
    PathEntry {
        name: "lock",
        default: "<workspace>/.ee/ee.lock",
        description: "Advisory write lock file for concurrent access",
        env_override: None,
    },
];

#[derive(Clone, Debug)]
pub struct EnvVarEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub default: Option<&'static str>,
    pub category: &'static str,
}

impl EnvVarEntry {
    #[must_use]
    pub const fn from_env_var(var: EnvVar) -> Self {
        Self {
            name: var.name(),
            description: var.description(),
            default: var.default_value(),
            category: var.category(),
        }
    }
}

#[must_use]
pub fn env_var_entries() -> Vec<EnvVarEntry> {
    EnvVar::all()
        .iter()
        .copied()
        .map(EnvVarEntry::from_env_var)
        .collect()
}

pub const ENV_VARS: &[EnvVarEntry] = &[
    EnvVarEntry {
        name: EnvVar::AgentName.name(),
        description: EnvVar::AgentName.description(),
        default: EnvVar::AgentName.default_value(),
        category: EnvVar::AgentName.category(),
    },
    EnvVarEntry {
        name: EnvVar::AgentMode.name(),
        description: EnvVar::AgentMode.description(),
        default: EnvVar::AgentMode.default_value(),
        category: EnvVar::AgentMode.category(),
    },
    EnvVarEntry {
        name: EnvVar::AuditLaneBatchMax.name(),
        description: EnvVar::AuditLaneBatchMax.description(),
        default: EnvVar::AuditLaneBatchMax.default_value(),
        category: EnvVar::AuditLaneBatchMax.category(),
    },
    EnvVarEntry {
        name: EnvVar::AuditLaneCapacity.name(),
        description: EnvVar::AuditLaneCapacity.description(),
        default: EnvVar::AuditLaneCapacity.default_value(),
        category: EnvVar::AuditLaneCapacity.category(),
    },
    EnvVarEntry {
        name: EnvVar::AuditLaneFlushMs.name(),
        description: EnvVar::AuditLaneFlushMs.description(),
        default: EnvVar::AuditLaneFlushMs.default_value(),
        category: EnvVar::AuditLaneFlushMs.category(),
    },
    EnvVarEntry {
        name: EnvVar::CassBinary.name(),
        description: EnvVar::CassBinary.description(),
        default: EnvVar::CassBinary.default_value(),
        category: EnvVar::CassBinary.category(),
    },
    EnvVarEntry {
        name: EnvVar::DatabasePath.name(),
        description: EnvVar::DatabasePath.description(),
        default: EnvVar::DatabasePath.default_value(),
        category: EnvVar::DatabasePath.category(),
    },
    EnvVarEntry {
        name: EnvVar::DemoEvidenceRoot.name(),
        description: EnvVar::DemoEvidenceRoot.description(),
        default: EnvVar::DemoEvidenceRoot.default_value(),
        category: EnvVar::DemoEvidenceRoot.category(),
    },
    EnvVarEntry {
        name: EnvVar::DiagForceCapabilityGap.name(),
        description: EnvVar::DiagForceCapabilityGap.description(),
        default: EnvVar::DiagForceCapabilityGap.default_value(),
        category: EnvVar::DiagForceCapabilityGap.category(),
    },
    EnvVarEntry {
        name: EnvVar::DisableToon.name(),
        description: EnvVar::DisableToon.description(),
        default: EnvVar::DisableToon.default_value(),
        category: EnvVar::DisableToon.category(),
    },
    EnvVarEntry {
        name: EnvVar::DisableRememberSearchNeighbors.name(),
        description: EnvVar::DisableRememberSearchNeighbors.description(),
        default: EnvVar::DisableRememberSearchNeighbors.default_value(),
        category: EnvVar::DisableRememberSearchNeighbors.category(),
    },
    EnvVarEntry {
        name: EnvVar::E2eRetentionManifest.name(),
        description: EnvVar::E2eRetentionManifest.description(),
        default: EnvVar::E2eRetentionManifest.default_value(),
        category: EnvVar::E2eRetentionManifest.category(),
    },
    EnvVarEntry {
        name: EnvVar::EmbedDedupCosineFloor.name(),
        description: EnvVar::EmbedDedupCosineFloor.description(),
        default: EnvVar::EmbedDedupCosineFloor.default_value(),
        category: EnvVar::EmbedDedupCosineFloor.category(),
    },
    EnvVarEntry {
        name: EnvVar::EmbedDedupEnabled.name(),
        description: EnvVar::EmbedDedupEnabled.description(),
        default: EnvVar::EmbedDedupEnabled.default_value(),
        category: EnvVar::EmbedDedupEnabled.category(),
    },
    EnvVarEntry {
        name: EnvVar::EmbedDedupHammingK.name(),
        description: EnvVar::EmbedDedupHammingK.description(),
        default: EnvVar::EmbedDedupHammingK.default_value(),
        category: EnvVar::EmbedDedupHammingK.category(),
    },
    EnvVarEntry {
        name: EnvVar::EmbedModelPath.name(),
        description: EnvVar::EmbedModelPath.description(),
        default: EnvVar::EmbedModelPath.default_value(),
        category: EnvVar::EmbedModelPath.category(),
    },
    EnvVarEntry {
        name: EnvVar::ExperimentalTriad.name(),
        description: EnvVar::ExperimentalTriad.description(),
        default: EnvVar::ExperimentalTriad.default_value(),
        category: EnvVar::ExperimentalTriad.category(),
    },
    EnvVarEntry {
        name: EnvVar::FlightRecorder.name(),
        description: EnvVar::FlightRecorder.description(),
        default: EnvVar::FlightRecorder.default_value(),
        category: EnvVar::FlightRecorder.category(),
    },
    EnvVarEntry {
        name: EnvVar::FlightRecorderDir.name(),
        description: EnvVar::FlightRecorderDir.description(),
        default: EnvVar::FlightRecorderDir.default_value(),
        category: EnvVar::FlightRecorderDir.category(),
    },
    EnvVarEntry {
        name: EnvVar::FlightRecorderRetentionDays.name(),
        description: EnvVar::FlightRecorderRetentionDays.description(),
        default: EnvVar::FlightRecorderRetentionDays.default_value(),
        category: EnvVar::FlightRecorderRetentionDays.category(),
    },
    EnvVarEntry {
        name: EnvVar::Format.name(),
        description: EnvVar::Format.description(),
        default: EnvVar::Format.default_value(),
        category: EnvVar::Format.category(),
    },
    EnvVarEntry {
        name: EnvVar::GraphWitnessesRetentionDays.name(),
        description: EnvVar::GraphWitnessesRetentionDays.description(),
        default: EnvVar::GraphWitnessesRetentionDays.default_value(),
        category: EnvVar::GraphWitnessesRetentionDays.category(),
    },
    EnvVarEntry {
        name: EnvVar::HarmfulBurstWindowSeconds.name(),
        description: EnvVar::HarmfulBurstWindowSeconds.description(),
        default: EnvVar::HarmfulBurstWindowSeconds.default_value(),
        category: EnvVar::HarmfulBurstWindowSeconds.category(),
    },
    EnvVarEntry {
        name: EnvVar::HarmfulPerSourcePerHour.name(),
        description: EnvVar::HarmfulPerSourcePerHour.description(),
        default: EnvVar::HarmfulPerSourcePerHour.default_value(),
        category: EnvVar::HarmfulPerSourcePerHour.category(),
    },
    EnvVarEntry {
        name: EnvVar::HookMode.name(),
        description: EnvVar::HookMode.description(),
        default: EnvVar::HookMode.default_value(),
        category: EnvVar::HookMode.category(),
    },
    EnvVarEntry {
        name: EnvVar::IndexDir.name(),
        description: EnvVar::IndexDir.description(),
        default: EnvVar::IndexDir.default_value(),
        category: EnvVar::IndexDir.category(),
    },
    EnvVarEntry {
        name: EnvVar::IndexPublishLockRetryAttempts.name(),
        description: EnvVar::IndexPublishLockRetryAttempts.description(),
        default: EnvVar::IndexPublishLockRetryAttempts.default_value(),
        category: EnvVar::IndexPublishLockRetryAttempts.category(),
    },
    EnvVarEntry {
        name: EnvVar::Json.name(),
        description: EnvVar::Json.description(),
        default: EnvVar::Json.default_value(),
        category: EnvVar::Json.category(),
    },
    EnvVarEntry {
        name: EnvVar::L2PackCacheBytes.name(),
        description: EnvVar::L2PackCacheBytes.description(),
        default: EnvVar::L2PackCacheBytes.default_value(),
        category: EnvVar::L2PackCacheBytes.category(),
    },
    EnvVarEntry {
        name: EnvVar::L2PackCacheDir.name(),
        description: EnvVar::L2PackCacheDir.description(),
        default: EnvVar::L2PackCacheDir.default_value(),
        category: EnvVar::L2PackCacheDir.category(),
    },
    EnvVarEntry {
        name: EnvVar::L2PackCacheDisable.name(),
        description: EnvVar::L2PackCacheDisable.description(),
        default: EnvVar::L2PackCacheDisable.default_value(),
        category: EnvVar::L2PackCacheDisable.category(),
    },
    EnvVarEntry {
        name: EnvVar::LegacySelectionCertificate.name(),
        description: EnvVar::LegacySelectionCertificate.description(),
        default: EnvVar::LegacySelectionCertificate.default_value(),
        category: EnvVar::LegacySelectionCertificate.category(),
    },
    EnvVarEntry {
        name: EnvVar::LexicalIndexHugepages.name(),
        description: EnvVar::LexicalIndexHugepages.description(),
        default: EnvVar::LexicalIndexHugepages.default_value(),
        category: EnvVar::LexicalIndexHugepages.category(),
    },
    EnvVarEntry {
        name: EnvVar::LexicalIndexPinRam.name(),
        description: EnvVar::LexicalIndexPinRam.description(),
        default: EnvVar::LexicalIndexPinRam.default_value(),
        category: EnvVar::LexicalIndexPinRam.category(),
    },
    EnvVarEntry {
        name: EnvVar::LogFormat.name(),
        description: EnvVar::LogFormat.description(),
        default: EnvVar::LogFormat.default_value(),
        category: EnvVar::LogFormat.category(),
    },
    EnvVarEntry {
        name: EnvVar::LogJson.name(),
        description: EnvVar::LogJson.description(),
        default: EnvVar::LogJson.default_value(),
        category: EnvVar::LogJson.category(),
    },
    EnvVarEntry {
        name: EnvVar::MaxTokens.name(),
        description: EnvVar::MaxTokens.description(),
        default: EnvVar::MaxTokens.default_value(),
        category: EnvVar::MaxTokens.category(),
    },
    EnvVarEntry {
        name: EnvVar::MeshEnabled.name(),
        description: EnvVar::MeshEnabled.description(),
        default: EnvVar::MeshEnabled.default_value(),
        category: EnvVar::MeshEnabled.category(),
    },
    EnvVarEntry {
        name: EnvVar::MeshMode.name(),
        description: EnvVar::MeshMode.description(),
        default: EnvVar::MeshMode.default_value(),
        category: EnvVar::MeshMode.category(),
    },
    EnvVarEntry {
        name: EnvVar::NoColor.name(),
        description: EnvVar::NoColor.description(),
        default: EnvVar::NoColor.default_value(),
        category: EnvVar::NoColor.category(),
    },
    EnvVarEntry {
        name: EnvVar::OutputFormat.name(),
        description: EnvVar::OutputFormat.description(),
        default: EnvVar::OutputFormat.default_value(),
        category: EnvVar::OutputFormat.category(),
    },
    EnvVarEntry {
        name: EnvVar::PreflightBypassSecret.name(),
        description: EnvVar::PreflightBypassSecret.description(),
        default: EnvVar::PreflightBypassSecret.default_value(),
        category: EnvVar::PreflightBypassSecret.category(),
    },
    EnvVarEntry {
        name: EnvVar::Profile.name(),
        description: EnvVar::Profile.description(),
        default: EnvVar::Profile.default_value(),
        category: EnvVar::Profile.category(),
    },
    EnvVarEntry {
        name: EnvVar::PprCacheEntries.name(),
        description: EnvVar::PprCacheEntries.description(),
        default: EnvVar::PprCacheEntries.default_value(),
        category: EnvVar::PprCacheEntries.category(),
    },
    EnvVarEntry {
        name: EnvVar::QueryPlanCacheEntries.name(),
        description: EnvVar::QueryPlanCacheEntries.description(),
        default: EnvVar::QueryPlanCacheEntries.default_value(),
        category: EnvVar::QueryPlanCacheEntries.category(),
    },
    EnvVarEntry {
        name: EnvVar::ReadPoolDisablePin.name(),
        description: EnvVar::ReadPoolDisablePin.description(),
        default: EnvVar::ReadPoolDisablePin.default_value(),
        category: EnvVar::ReadPoolDisablePin.category(),
    },
    EnvVarEntry {
        name: EnvVar::ReadPoolAcquireTimeoutMs.name(),
        description: EnvVar::ReadPoolAcquireTimeoutMs.description(),
        default: EnvVar::ReadPoolAcquireTimeoutMs.default_value(),
        category: EnvVar::ReadPoolAcquireTimeoutMs.category(),
    },
    EnvVarEntry {
        name: EnvVar::ReadPoolIdleTimeoutSeconds.name(),
        description: EnvVar::ReadPoolIdleTimeoutSeconds.description(),
        default: EnvVar::ReadPoolIdleTimeoutSeconds.default_value(),
        category: EnvVar::ReadPoolIdleTimeoutSeconds.category(),
    },
    EnvVarEntry {
        name: EnvVar::ReadPoolMaxPinSeconds.name(),
        description: EnvVar::ReadPoolMaxPinSeconds.description(),
        default: EnvVar::ReadPoolMaxPinSeconds.default_value(),
        category: EnvVar::ReadPoolMaxPinSeconds.category(),
    },
    EnvVarEntry {
        name: EnvVar::ReadPoolSize.name(),
        description: EnvVar::ReadPoolSize.description(),
        default: EnvVar::ReadPoolSize.default_value(),
        category: EnvVar::ReadPoolSize.category(),
    },
    EnvVarEntry {
        name: EnvVar::RememberCurationSyncBudgetMs.name(),
        description: EnvVar::RememberCurationSyncBudgetMs.description(),
        default: EnvVar::RememberCurationSyncBudgetMs.default_value(),
        category: EnvVar::RememberCurationSyncBudgetMs.category(),
    },
    EnvVarEntry {
        name: EnvVar::SecurityProfile.name(),
        description: EnvVar::SecurityProfile.description(),
        default: EnvVar::SecurityProfile.default_value(),
        category: EnvVar::SecurityProfile.category(),
    },
    EnvVarEntry {
        name: EnvVar::ScienceBackendPath.name(),
        description: EnvVar::ScienceBackendPath.description(),
        default: EnvVar::ScienceBackendPath.default_value(),
        category: EnvVar::ScienceBackendPath.category(),
    },
    EnvVarEntry {
        name: EnvVar::ShardFanoutEnabled.name(),
        description: EnvVar::ShardFanoutEnabled.description(),
        default: EnvVar::ShardFanoutEnabled.default_value(),
        category: EnvVar::ShardFanoutEnabled.category(),
    },
    EnvVarEntry {
        name: EnvVar::ShardsDir.name(),
        description: EnvVar::ShardsDir.description(),
        default: EnvVar::ShardsDir.default_value(),
        category: EnvVar::ShardsDir.category(),
    },
    EnvVarEntry {
        name: EnvVar::TestLogLevel.name(),
        description: EnvVar::TestLogLevel.description(),
        default: EnvVar::TestLogLevel.default_value(),
        category: EnvVar::TestLogLevel.category(),
    },
    EnvVarEntry {
        name: EnvVar::TestLogPath.name(),
        description: EnvVar::TestLogPath.description(),
        default: EnvVar::TestLogPath.default_value(),
        category: EnvVar::TestLogPath.category(),
    },
    EnvVarEntry {
        name: EnvVar::TestLogTestId.name(),
        description: EnvVar::TestLogTestId.description(),
        default: EnvVar::TestLogTestId.default_value(),
        category: EnvVar::TestLogTestId.category(),
    },
    EnvVarEntry {
        name: EnvVar::TailscaleBinaryOverride.name(),
        description: EnvVar::TailscaleBinaryOverride.description(),
        default: EnvVar::TailscaleBinaryOverride.default_value(),
        category: EnvVar::TailscaleBinaryOverride.category(),
    },
    EnvVarEntry {
        name: EnvVar::TailscaleProbeTimeoutMs.name(),
        description: EnvVar::TailscaleProbeTimeoutMs.description(),
        default: EnvVar::TailscaleProbeTimeoutMs.default_value(),
        category: EnvVar::TailscaleProbeTimeoutMs.category(),
    },
    EnvVarEntry {
        name: EnvVar::TailscaleProbeSocketOverride.name(),
        description: EnvVar::TailscaleProbeSocketOverride.description(),
        default: EnvVar::TailscaleProbeSocketOverride.default_value(),
        category: EnvVar::TailscaleProbeSocketOverride.category(),
    },
    EnvVarEntry {
        name: EnvVar::TailscaleDiscoveryMode.name(),
        description: EnvVar::TailscaleDiscoveryMode.description(),
        default: EnvVar::TailscaleDiscoveryMode.default_value(),
        category: EnvVar::TailscaleDiscoveryMode.category(),
    },
    EnvVarEntry {
        name: EnvVar::TailscalePeerProbeTimeoutMs.name(),
        description: EnvVar::TailscalePeerProbeTimeoutMs.description(),
        default: EnvVar::TailscalePeerProbeTimeoutMs.default_value(),
        category: EnvVar::TailscalePeerProbeTimeoutMs.category(),
    },
    EnvVarEntry {
        name: EnvVar::TailscaleDiscoveryBudgetMs.name(),
        description: EnvVar::TailscaleDiscoveryBudgetMs.description(),
        default: EnvVar::TailscaleDiscoveryBudgetMs.default_value(),
        category: EnvVar::TailscaleDiscoveryBudgetMs.category(),
    },
    EnvVarEntry {
        name: EnvVar::TailscaleRespondMode.name(),
        description: EnvVar::TailscaleRespondMode.description(),
        default: EnvVar::TailscaleRespondMode.default_value(),
        category: EnvVar::TailscaleRespondMode.category(),
    },
    EnvVarEntry {
        name: EnvVar::WorkspaceHygieneAlwaysReviewPatterns.name(),
        description: EnvVar::WorkspaceHygieneAlwaysReviewPatterns.description(),
        default: EnvVar::WorkspaceHygieneAlwaysReviewPatterns.default_value(),
        category: EnvVar::WorkspaceHygieneAlwaysReviewPatterns.category(),
    },
    EnvVarEntry {
        name: EnvVar::WorkspaceHygieneGeneratedPatterns.name(),
        description: EnvVar::WorkspaceHygieneGeneratedPatterns.description(),
        default: EnvVar::WorkspaceHygieneGeneratedPatterns.default_value(),
        category: EnvVar::WorkspaceHygieneGeneratedPatterns.category(),
    },
    EnvVarEntry {
        name: EnvVar::WorkspaceHygieneLocalMachinePatterns.name(),
        description: EnvVar::WorkspaceHygieneLocalMachinePatterns.description(),
        default: EnvVar::WorkspaceHygieneLocalMachinePatterns.default_value(),
        category: EnvVar::WorkspaceHygieneLocalMachinePatterns.category(),
    },
    EnvVarEntry {
        name: EnvVar::WorkspaceHygieneScratchPatterns.name(),
        description: EnvVar::WorkspaceHygieneScratchPatterns.description(),
        default: EnvVar::WorkspaceHygieneScratchPatterns.default_value(),
        category: EnvVar::WorkspaceHygieneScratchPatterns.category(),
    },
    EnvVarEntry {
        name: EnvVar::WalCheckpointBytesThreshold.name(),
        description: EnvVar::WalCheckpointBytesThreshold.description(),
        default: EnvVar::WalCheckpointBytesThreshold.default_value(),
        category: EnvVar::WalCheckpointBytesThreshold.category(),
    },
    EnvVarEntry {
        name: EnvVar::Workspace.name(),
        description: EnvVar::Workspace.description(),
        default: EnvVar::Workspace.default_value(),
        category: EnvVar::Workspace.category(),
    },
    EnvVarEntry {
        name: EnvVar::WorkspaceCloseDrainTimeoutSeconds.name(),
        description: EnvVar::WorkspaceCloseDrainTimeoutSeconds.description(),
        default: EnvVar::WorkspaceCloseDrainTimeoutSeconds.default_value(),
        category: EnvVar::WorkspaceCloseDrainTimeoutSeconds.category(),
    },
    EnvVarEntry {
        name: EnvVar::WorkspaceRegistry.name(),
        description: EnvVar::WorkspaceRegistry.description(),
        default: EnvVar::WorkspaceRegistry.default_value(),
        category: EnvVar::WorkspaceRegistry.category(),
    },
];

#[derive(Clone, Debug)]
pub struct ExitCodeEntry {
    pub code: u8,
    pub name: &'static str,
    pub description: &'static str,
}

pub const EXIT_CODES: &[ExitCodeEntry] = &[
    ExitCodeEntry {
        code: 0,
        name: "success",
        description: "Command completed successfully",
    },
    ExitCodeEntry {
        code: 1,
        name: "usage",
        description: "Invalid arguments or usage error",
    },
    ExitCodeEntry {
        code: 2,
        name: "configuration",
        description: "Configuration file error or invalid settings",
    },
    ExitCodeEntry {
        code: 3,
        name: "storage",
        description: "Database or storage error",
    },
    ExitCodeEntry {
        code: 4,
        name: "search_index",
        description: "Search index error or index not found",
    },
    ExitCodeEntry {
        code: 5,
        name: "import",
        description: "Import operation failed",
    },
    ExitCodeEntry {
        code: 6,
        name: "degraded",
        description: "Operation could not satisfy required mode",
    },
    ExitCodeEntry {
        code: 7,
        name: "policy",
        description: "Policy denied the operation",
    },
    ExitCodeEntry {
        code: 8,
        name: "migration",
        description: "Database migration required",
    },
    ExitCodeEntry {
        code: 9,
        name: "eval_failure",
        description: "Evaluation completed and found regressions",
    },
];

#[derive(Clone, Debug)]
pub struct FieldLevelEntry {
    pub name: &'static str,
    pub flag: &'static str,
    pub includes: &'static str,
    pub use_case: &'static str,
}

pub const FIELD_LEVELS: &[FieldLevelEntry] = &[
    FieldLevelEntry {
        name: "minimal",
        flag: "--fields minimal",
        includes: "command, version, status only",
        use_case: "Bare minimum for scripting status checks",
    },
    FieldLevelEntry {
        name: "summary",
        flag: "--fields summary",
        includes: "+ top-level metrics and summary counts",
        use_case: "Quick overview without array details",
    },
    FieldLevelEntry {
        name: "standard",
        flag: "--fields standard",
        includes: "+ arrays with items (default)",
        use_case: "Normal operation with all relevant data",
    },
    FieldLevelEntry {
        name: "full",
        flag: "--fields full",
        includes: "+ provenance, why, repair hints, debug info",
        use_case: "Debugging and detailed analysis",
    },
];

#[derive(Clone, Debug)]
pub struct FormatEntry {
    pub name: &'static str,
    pub flag: &'static str,
    pub description: &'static str,
    pub machine_readable: bool,
}

pub const OUTPUT_FORMATS: &[FormatEntry] = &[
    FormatEntry {
        name: "human",
        flag: "--format human",
        description: "Human-readable text output (default)",
        machine_readable: false,
    },
    FormatEntry {
        name: "json",
        flag: "--format json or --json or -j",
        description: "Structured JSON with ee.response.v2 envelope",
        machine_readable: true,
    },
    FormatEntry {
        name: "toon",
        flag: "--format toon",
        description: "Token-efficient hierarchical notation for LLM context; 20-40% fewer tokens than JSON; decode-compatible but not for storage/hooks/MCP",
        machine_readable: false,
    },
    FormatEntry {
        name: "markdown",
        flag: "--format markdown",
        description: "Markdown context output for direct agent prompt inclusion",
        machine_readable: false,
    },
    FormatEntry {
        name: "jsonl",
        flag: "--format jsonl",
        description: "Line-delimited JSON for streaming",
        machine_readable: true,
    },
    FormatEntry {
        name: "compact",
        flag: "--format compact",
        description: "Minimal JSON without whitespace",
        machine_readable: true,
    },
    FormatEntry {
        name: "hook",
        flag: "--format hook",
        description: "Format optimized for hook consumption",
        machine_readable: true,
    },
    FormatEntry {
        name: "mermaid",
        flag: "--format mermaid",
        description: "Mermaid graph projection for commands with diagram output",
        machine_readable: false,
    },
];

#[derive(Clone, Debug)]
pub struct ContractEntry {
    pub name: &'static str,
    pub schema: &'static str,
    pub description: &'static str,
    pub stability: &'static str,
}

pub const CONTRACTS: &[ContractEntry] = &[
    ContractEntry {
        name: "response",
        schema: RESPONSE_SCHEMA_V2,
        description: "Standard success response envelope with data payload",
        stability: "stable",
    },
    ContractEntry {
        name: "error",
        schema: ERROR_SCHEMA_V2,
        description: "Standard error response with code, message, and repair hint",
        stability: "stable",
    },
    ContractEntry {
        name: "preflight_guard",
        schema: crate::core::preflight_guard::PREFLIGHT_GUARD_SCHEMA_V1,
        description: "Direct hook-safe guard response for ee preflight check/guard; intentionally not wrapped in ee.response.v2 so command hooks can branch on allowed/exitCode without envelope traversal",
        stability: "stable",
    },
];

#[derive(Clone, Debug)]
pub struct ExampleEntry {
    pub title: &'static str,
    pub description: &'static str,
    pub command: &'static str,
    pub category: &'static str,
}

pub const EXAMPLES: &[ExampleEntry] = &[
    ExampleEntry {
        title: "Pre-task context",
        description: "Get relevant context before starting a task",
        command: "ee pack \"fix failing CI tests\" --workspace . --max-tokens 4000 --json",
        category: "context",
    },
    ExampleEntry {
        title: "Store a procedural rule",
        description: "Remember a learned best practice",
        command: "ee remember --level procedural --kind rule \"Run cargo fmt before commit\" --json",
        category: "memory",
    },
    ExampleEntry {
        title: "Search memories",
        description: "Find relevant past context",
        command: "ee search \"authentication error\" --limit 5 --json",
        category: "search",
    },
    ExampleEntry {
        title: "Check system health",
        description: "Verify ee is ready to use",
        command: "ee health --json",
        category: "diagnostics",
    },
    ExampleEntry {
        title: "Detailed status",
        description: "Get full capability and degradation info",
        command: "ee status --fields full --json",
        category: "diagnostics",
    },
    ExampleEntry {
        title: "Discover schemas",
        description: "List available response schemas",
        command: "ee schema list --json",
        category: "discovery",
    },
    ExampleEntry {
        title: "Self-introspection",
        description: "Get command/schema/error maps for agent tooling",
        command: "ee introspect --json",
        category: "discovery",
    },
    ExampleEntry {
        title: "Import CASS sessions",
        description: "Import evidence from coding agent session search",
        command: "ee import cass --limit 20 --json",
        category: "import",
    },
    ExampleEntry {
        title: "Fix plan",
        description: "Get actionable repair steps for issues",
        command: "ee doctor --fix-plan --json",
        category: "diagnostics",
    },
    ExampleEntry {
        title: "Preflight a shell command",
        description: "Check a command against destructive-action guard rules; this example encodes `git status`, and --cmd-base64 or --stdin keeps intercepted literals off argv",
        command: "ee preflight check --cmd-base64 Z2l0IHN0YXR1cw== --json",
        category: "safety",
    },
    ExampleEntry {
        title: "Token-efficient status",
        description: "Use TOON format for LLM context windows",
        command: "ee status --format toon",
        category: "formats",
    },
    ExampleEntry {
        title: "TOON context pack",
        description: "Get context with 20-40% fewer tokens than JSON",
        command: "ee pack \"task\" --workspace . --format toon",
        category: "formats",
    },
];

#[derive(Clone, Debug)]
pub struct FailureBranchEntry {
    pub condition: &'static str,
    pub jq: &'static str,
    pub next_action: &'static str,
}

#[derive(Clone, Debug)]
pub struct AgentDocsRecipeEntry {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    pub command: &'static str,
    pub jq: &'static str,
    pub success_check: &'static str,
    pub failure_branches: &'static [FailureBranchEntry],
}

pub const CONTEXT_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "context pack command returns an error envelope",
        jq: r#".error | {code, message, repair}"#,
        next_action: "Run the repair command when present, then retry with the same workspace and query.",
    },
    FailureBranchEntry {
        condition: "semantic retrieval is degraded",
        jq: r#".data.degraded[]? | select(.code == "semantic_unavailable")"#,
        next_action: "Continue with lexical results when acceptable, or run `ee index reembed --workspace .`.",
    },
];

pub const STATUS_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "database migration is required",
        jq: r#".. | objects | select(.code? == "migration_required")"#,
        next_action: "Run `ee migrate run --workspace . --json` before mutating memory state.",
    },
    FailureBranchEntry {
        condition: "storage or index capability is unavailable",
        jq: r#".data.degraded[]? | select(.code | test("storage|index"))"#,
        next_action: "Use the reported repair field or run `ee doctor --json` for a full repair plan.",
    },
];

pub const DOCTOR_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "one or more checks failed",
        jq: r#".data.checks[]? | select(.status != "ok") | {name, status, code, repair}"#,
        next_action: "Apply failing check repairs in order and rerun `ee doctor --json`.",
    },
    FailureBranchEntry {
        condition: "doctor command itself returns an error envelope",
        jq: r#".error | {code, message, repair}"#,
        next_action: "Treat the error code as the stable branch key and avoid parsing stderr for automation.",
    },
];

pub const PLAN_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "goal cannot be classified confidently",
        jq: r#".data.degradedBranches[]? | select(.condition | test("unknown|ambiguous"))"#,
        next_action: "Run `ee plan recipe list --json` and select a recipe explicitly.",
    },
    FailureBranchEntry {
        condition: "selected recipe includes degraded branches",
        jq: r#".data.degradedBranches[]? | {condition, command, reason}"#,
        next_action: "Resolve the listed precondition before applying the real command sequence.",
    },
];

pub const CONTRACT_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "expected schema is absent",
        jq: r#".data.contracts[]? | select(.schema == "ee.response.v2")"#,
        next_action: "Pin automation to the published schema list and stop if the expected schema is missing.",
    },
    FailureBranchEntry {
        condition: "agent-docs topic is misspelled",
        jq: r#".error | select(.code == "usage") | {message, repair}"#,
        next_action: "Run `ee agent-docs --json` and select a topic from `.data.topics[].name`.",
    },
];

// ============================================================================
// EE-DIST-005: Install/Update Recipe Failure Branches
// ============================================================================

pub const INSTALL_CHECK_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "install directory cannot be written",
        jq: r#".data.findings[]? | select(.code == "install_dir_not_writable") | {message, nextAction}"#,
        next_action: "Choose a writable --install-dir or create the parent directory with appropriate permissions.",
    },
    FailureBranchEntry {
        condition: "multiple or shadowing ee binaries are found in PATH",
        jq: r#".data.findings[]? | select(.code == "duplicate_path_binary" or .code == "current_binary_shadowed") | {message, nextAction}"#,
        next_action: "Remove stale duplicates or make the intended install directory appear first in PATH.",
    },
    FailureBranchEntry {
        condition: "no deterministic update source is configured",
        jq: r#".data.findings[]? | select(.code == "no_update_source_configured" or .code == "offline_no_manifest") | {message, nextAction}"#,
        next_action: "Pass --manifest for deterministic offline install or update planning.",
    },
];

pub const UPDATE_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "manifest is missing in offline update planning",
        jq: r#".data.findings[]? | select(.code == "manifest_missing" or .code == "offline_no_manifest") | {code, message, nextAction}"#,
        next_action: "Pass --manifest pointing at a local release manifest and rerun `ee update --dry-run --offline --json`.",
    },
    FailureBranchEntry {
        condition: "artifact checksum cannot be verified yet",
        jq: r#".data.findings[]? | select(.code == "checksum_verification_pending") | {message, nextAction}"#,
        next_action: "Pass --artifact-root pointing at downloaded release artifacts before treating the plan as apply-ready.",
    },
    FailureBranchEntry {
        condition: "update would downgrade the installed binary",
        jq: r#".data.findings[]? | select(.code == "would_downgrade") | {message, nextAction}"#,
        next_action: "Rerun the install/update plan with an explicit --pin value and --allow-downgrade only when rollback is intentional.",
    },
    FailureBranchEntry {
        condition: "target artifact is not available for this platform",
        jq: r#".data.findings[]? | select(.code == "target_mismatch" or .code == "unsupported_target") | {code, message, nextAction}"#,
        next_action: "Choose a supported --target from the manifest or publish the missing artifact before planning the update.",
    },
];

pub const PIN_VERSION_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "manifest or target artifact is unavailable",
        jq: r#".data.findings[]? | select(.code == "manifest_missing" or .code == "target_mismatch" or .code == "artifact_missing") | {code, message, nextAction}"#,
        next_action: "Pass --manifest and, when verifying artifacts, --artifact-root that contains the release files.",
    },
    FailureBranchEntry {
        condition: "pinned version would downgrade the installed binary",
        jq: r#".data.findings[]? | select(.code == "would_downgrade") | {message, nextAction}"#,
        next_action: "Add --allow-downgrade only when the rollback is intentional and reviewed.",
    },
];

pub const SUPPORT_BUNDLE_RECIPE_FAILURES: &[FailureBranchEntry] = &[
    FailureBranchEntry {
        condition: "bundle creation failed due to permissions",
        jq: r#".error | select(.code == "storage" or .code == "configuration") | {message, repair}"#,
        next_action: "Ensure write permissions for the output directory or specify an alternate path with `--out`.",
    },
    FailureBranchEntry {
        condition: "dry-run reports no output path",
        jq: r#".data | select(.dryRun == true) | {filesCollected, totalSizeBytes, outputPath}"#,
        next_action: "Rerun without --dry-run and pass --out <dir> when an actual bundle artifact is needed.",
    },
];

pub const AGENT_DOC_RECIPES: &[AgentDocsRecipeEntry] = &[
    AgentDocsRecipeEntry {
        id: "pre-task-context",
        title: "Fetch task context before editing",
        description: "Retrieve a compact, provenance-bearing context pack for the current task.",
        category: "context",
        command: "ee pack \"<task>\" --workspace . --max-tokens 4000 --json",
        jq: r#".data.pack.items[]? | {memoryId, section, why}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: CONTEXT_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "workspace-health",
        title: "Check workspace health",
        description: "Inspect storage, index, and degraded capability state before relying on memory output.",
        category: "diagnostics",
        command: "ee status --workspace . --json",
        jq: r#"{database: .data.database, index: .data.index, degraded: (.data.degraded // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: STATUS_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "repair-plan",
        title: "Collect repair actions",
        description: "Use doctor output as the stable source of repair commands for automation.",
        category: "diagnostics",
        command: "ee doctor --json",
        jq: r#".data.checks[]? | select(.status != "ok") | {name, code, repair}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: DOCTOR_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "goal-to-recipe",
        title: "Resolve an agent goal to commands",
        description: "Map a natural-language goal to a deterministic recipe before running a workflow.",
        category: "planning",
        command: "ee plan goal \"<goal>\" --json",
        jq: r#"{recipeId: .data.recipeId, steps: [.data.steps[]?.command], degraded: (.data.degradedBranches // [])}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: PLAN_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "contract-discovery",
        title: "Discover machine contracts",
        description: "List stable response contracts before writing or updating agent parsers.",
        category: "contracts",
        command: "ee agent-docs contracts --json",
        jq: r#".data.contracts[] | {name, schema, stability}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: CONTRACT_RECIPE_FAILURES,
    },
    // EE-DIST-005: Install/Update/Recovery Recipes
    AgentDocsRecipeEntry {
        id: "install-check",
        title: "Verify ee installation integrity",
        description: "Check binary presence, checksum, version currency, and PATH conflicts before relying on ee.",
        category: "distribution",
        command: "ee install check --json",
        jq: r#"{currentBinary: .data.currentBinary.path, version: .data.version, pathStatus: .data.path.status, findings: [.data.findings[]? | {code, message, nextAction}]}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and (.data.findings | map(select(.severity == "error")) | length == 0)"#,
        failure_branches: INSTALL_CHECK_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "update-dry-run",
        title: "Preview update before applying",
        description: "Show what an update would change without modifying the installed binary.",
        category: "distribution",
        command: "ee update --dry-run --json",
        jq: r#"{currentVersion: .data.currentVersion, targetVersion: .data.targetVersion, status: .data.status, verification: .data.verification, findings: [.data.findings[]? | {code, message, nextAction}]}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: UPDATE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "version-pin",
        title: "Pin ee to a specific version",
        description: "Plan an install from a release manifest pinned to a known version.",
        category: "distribution",
        command: "ee install plan --manifest <manifest> --pin <version> --json",
        jq: r#"{currentVersion: .data.currentVersion, targetVersion: .data.targetVersion, pinnedVersion: .data.pinnedVersion, status: .data.status}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: PIN_VERSION_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "checksum-recovery",
        title: "Recover from checksum mismatch",
        description: "Re-plan a verified install when artifact checksum validation fails.",
        category: "distribution",
        command: "ee install plan --manifest <manifest> --artifact-root <artifacts> --json",
        jq: r#"{status: .data.status, checksumStatus: .data.verification.checksumStatus, findings: [.data.findings[]? | {code, message, nextAction}]}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true and (.data.verification.checksumStatus == "verified" or .data.status == "idempotent")"#,
        failure_branches: PIN_VERSION_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "duplicate-binary-fix",
        title: "Resolve duplicate ee binaries in PATH",
        description: "Identify and remove conflicting ee installations when multiple binaries are found.",
        category: "distribution",
        command: "ee install check --json",
        jq: r#"{firstBinary: .data.path.firstBinary, duplicateCount: .data.path.duplicateCount, findings: [.data.findings[]? | {code, message, nextAction}]}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: INSTALL_CHECK_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "offline-update-posture",
        title: "Check offline update readiness",
        description: "Verify cached update availability when network is unavailable.",
        category: "distribution",
        command: "ee update --dry-run --offline --json",
        jq: r#"{status: .data.status, currentVersion: .data.currentVersion, targetVersion: .data.targetVersion, updateSource: .data.verification.manifestStatus, findings: [.data.findings[]? | {code, message, nextAction}]}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: UPDATE_RECIPE_FAILURES,
    },
    AgentDocsRecipeEntry {
        id: "update-failure-bundle",
        title: "Collect support bundle for failed update",
        description: "Gather diagnostic evidence when an install or update fails for support handoff.",
        category: "distribution",
        command: "ee support bundle --dry-run --json",
        jq: r#"{outputPath: .data.outputPath, totalSizeBytes: .data.totalSizeBytes, filesCollected: .data.filesCollected, redaction: .data.redactionSummary}"#,
        success_check: r#".schema == "ee.response.v2" and .success == true"#,
        failure_branches: SUPPORT_BUNDLE_RECIPE_FAILURES,
    },
];

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use super::{
        AGENT_DOC_RECIPES, AgentDocsTopic, CONTRACTS, DEFAULT_PATHS, EXAMPLES, EXIT_CODES,
        FIELD_LEVELS, GUIDE_SECTIONS, OUTPUT_FORMATS, env_var_entries,
    };
    use crate::config::EnvVar;
    use crate::models::ProcessExitCode;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn topic_all_returns_complete_list() -> TestResult {
        let topics = AgentDocsTopic::all();
        ensure_equal(&topics.len(), &12, "topic count")?;
        ensure_equal(
            &topics.first(),
            &Some(&AgentDocsTopic::Guide),
            "first topic",
        )
    }

    #[test]
    fn topic_round_trip_parse() -> TestResult {
        for topic in AgentDocsTopic::all() {
            let parsed = AgentDocsTopic::parse(topic.as_str());
            ensure_equal(
                &parsed,
                &Some(*topic),
                &format!("round-trip {}", topic.as_str()),
            )?;
        }
        Ok(())
    }

    #[test]
    fn topic_parse_normalizes_cli_values() -> TestResult {
        ensure_equal(
            &AgentDocsTopic::parse(" Exit-Codes "),
            &Some(AgentDocsTopic::ExitCodes),
            "hyphenated topic",
        )?;
        ensure_equal(
            &AgentDocsTopic::parse("exit_codes"),
            &Some(AgentDocsTopic::ExitCodes),
            "underscored topic",
        )?;
        ensure_equal(
            &AgentDocsTopic::parse("ExitCodes"),
            &Some(AgentDocsTopic::ExitCodes),
            "camel-case topic",
        )?;
        ensure_equal(
            &AgentDocsTopic::parse("RECIPES"),
            &Some(AgentDocsTopic::Recipes),
            "uppercase topic",
        )
    }

    #[test]
    fn topic_parse_returns_none_for_unknown() -> TestResult {
        let parsed = AgentDocsTopic::parse("unknown-topic");
        ensure_equal(&parsed, &None, "unknown topic")
    }

    #[test]
    fn guide_sections_are_non_empty() -> TestResult {
        ensure(!GUIDE_SECTIONS.is_empty(), "guide sections exist")?;
        for section in GUIDE_SECTIONS {
            ensure(!section.title.is_empty(), "guide section title non-empty")?;
            ensure(
                !section.content.is_empty(),
                "guide section content non-empty",
            )?;
        }
        Ok(())
    }

    #[test]
    fn default_paths_are_non_empty() -> TestResult {
        ensure(!DEFAULT_PATHS.is_empty(), "default paths exist")?;
        for path in DEFAULT_PATHS {
            ensure(!path.name.is_empty(), "path name non-empty")?;
            ensure(!path.default.is_empty(), "path default non-empty")?;
        }
        Ok(())
    }

    #[test]
    fn env_vars_are_non_empty() -> TestResult {
        let env_vars = env_var_entries();
        ensure(!env_vars.is_empty(), "env vars exist")?;
        for var in &env_vars {
            ensure(!var.name.is_empty(), "env var name non-empty")?;
            ensure(!var.description.is_empty(), "env var description non-empty")?;
        }
        Ok(())
    }

    #[test]
    fn env_docs_match_registry_order() -> TestResult {
        let env_vars = env_var_entries();
        for (entry, var) in env_vars.iter().zip(EnvVar::all()) {
            ensure_equal(&entry.name, &var.name(), "env docs name")?;
            ensure_equal(
                &entry.description,
                &var.description(),
                &format!("env docs description {}", var.name()),
            )?;
            ensure_equal(
                &entry.default,
                &var.default_value(),
                &format!("env docs default {}", var.name()),
            )?;
            ensure_equal(
                &entry.category,
                &var.category(),
                &format!("env docs category {}", var.name()),
            )?;
        }
        ensure_equal(
            &env_vars.len(),
            &EnvVar::all().len(),
            "env docs registry count",
        )
    }

    #[test]
    fn exit_codes_are_sequential() -> TestResult {
        for (i, code) in EXIT_CODES.iter().enumerate() {
            ensure_equal(
                &(code.code as usize),
                &i,
                &format!("exit code {} sequential", i),
            )?;
        }
        Ok(())
    }

    #[test]
    fn exit_codes_match_process_exit_code_contract() -> TestResult {
        let expected = [
            ("success", ProcessExitCode::Success),
            ("usage", ProcessExitCode::Usage),
            ("configuration", ProcessExitCode::Configuration),
            ("storage", ProcessExitCode::Storage),
            ("search_index", ProcessExitCode::SearchIndex),
            ("import", ProcessExitCode::Import),
            ("degraded", ProcessExitCode::UnsatisfiedDegradedMode),
            ("policy", ProcessExitCode::PolicyDenied),
            ("migration", ProcessExitCode::MigrationRequired),
            ("eval_failure", ProcessExitCode::EvalFailure),
        ];
        ensure_equal(&EXIT_CODES.len(), &expected.len(), "exit code count")?;
        for (entry, (name, code)) in EXIT_CODES.iter().zip(expected) {
            ensure_equal(&entry.name, &name, "exit code name")?;
            ensure_equal(&entry.code, &(code as u8), "exit code value")?;
        }
        Ok(())
    }

    #[test]
    fn field_levels_are_non_empty() -> TestResult {
        ensure_equal(&FIELD_LEVELS.len(), &4, "field level count")?;
        for level in FIELD_LEVELS {
            ensure(!level.name.is_empty(), "field level name non-empty")?;
        }
        Ok(())
    }

    #[test]
    fn output_formats_are_non_empty() -> TestResult {
        ensure(!OUTPUT_FORMATS.is_empty(), "output formats exist")?;
        for fmt in OUTPUT_FORMATS {
            ensure(!fmt.name.is_empty(), "format name non-empty")?;
        }
        Ok(())
    }

    #[test]
    fn json_output_format_documents_current_response_envelope() -> TestResult {
        let json_format = OUTPUT_FORMATS
            .iter()
            .find(|format| format.name == "json")
            .ok_or_else(|| "json output format is documented".to_string())?;
        let legacy_schema = ["ee", "response", "v1"].join(".");

        ensure(
            json_format.description.contains("ee.response.v2"),
            "json output format documents ee.response.v2",
        )?;
        ensure(
            !json_format.description.contains(&legacy_schema),
            "json output format does not document legacy response schema",
        )
    }

    #[test]
    fn output_formats_cover_global_format_enum() -> TestResult {
        let names = OUTPUT_FORMATS
            .iter()
            .map(|format| format.name)
            .collect::<Vec<_>>();
        ensure_equal(
            &names,
            &vec![
                "human", "json", "toon", "markdown", "jsonl", "compact", "hook", "mermaid",
            ],
            "documented output formats",
        )
    }

    #[test]
    fn contracts_have_stable_schemas() -> TestResult {
        ensure(!CONTRACTS.is_empty(), "contracts exist")?;
        for contract in CONTRACTS {
            ensure_equal(&contract.stability, &"stable", "contract stability")?;
        }
        Ok(())
    }

    #[test]
    fn contracts_catalog_lists_current_response_envelope() -> TestResult {
        let response_contract = CONTRACTS
            .iter()
            .find(|contract| contract.name == "response")
            .ok_or_else(|| "response contract is documented".to_string())?;
        let legacy_schema = ["ee", "response", "v1"].join(".");

        ensure_equal(
            &response_contract.schema,
            &crate::models::RESPONSE_SCHEMA_V2,
            "response contract schema",
        )?;
        ensure(
            response_contract.schema != legacy_schema,
            "response contract must not publish legacy schema",
        )
    }

    #[test]
    fn contracts_catalog_lists_direct_preflight_guard_schema() -> TestResult {
        let preflight_contract = CONTRACTS
            .iter()
            .find(|contract| contract.name == "preflight_guard")
            .ok_or_else(|| "preflight guard contract is documented".to_string())?;

        ensure_equal(
            &preflight_contract.schema,
            &crate::core::preflight_guard::PREFLIGHT_GUARD_SCHEMA_V1,
            "preflight guard direct schema",
        )?;
        ensure(
            preflight_contract.description.contains("not wrapped"),
            "preflight guard docs explain the direct schema exception",
        )
    }

    #[test]
    fn examples_are_non_empty() -> TestResult {
        ensure(!EXAMPLES.is_empty(), "examples exist")?;
        for example in EXAMPLES {
            ensure(!example.command.is_empty(), "example command non-empty")?;
            ensure(
                example.command.starts_with("ee "),
                "example command starts with ee",
            )?;
        }
        Ok(())
    }

    #[test]
    fn examples_include_preflight_base64_and_stdin_escape_hatches() -> TestResult {
        let preflight_example = EXAMPLES
            .iter()
            .find(|example| example.title == "Preflight a shell command")
            .ok_or_else(|| "preflight command-transport example is documented".to_string())?;

        ensure(
            preflight_example.command.contains("--cmd-base64"),
            "preflight example uses base64 transport",
        )?;
        ensure(
            !preflight_example.command.contains('<'),
            "preflight example avoids shell-redirection-shaped placeholders",
        )?;
        ensure(
            preflight_example.description.contains("--stdin"),
            "preflight example mentions stdin transport",
        )?;
        ensure(
            preflight_example.description.contains("git status"),
            "preflight example names the encoded command",
        )
    }

    #[test]
    fn recipes_include_jq_and_failure_branches() -> TestResult {
        ensure(!AGENT_DOC_RECIPES.is_empty(), "agent recipes exist")?;
        for recipe in AGENT_DOC_RECIPES {
            ensure(!recipe.id.is_empty(), "recipe id non-empty")?;
            ensure(
                recipe.command.starts_with("ee "),
                "recipe command starts with ee",
            )?;
            ensure(!recipe.jq.is_empty(), "recipe jq non-empty")?;
            ensure(
                !recipe.success_check.is_empty(),
                "recipe success check non-empty",
            )?;
            ensure(
                !recipe.failure_branches.is_empty(),
                "recipe failure branches exist",
            )?;
            for branch in recipe.failure_branches {
                ensure(!branch.condition.is_empty(), "failure condition non-empty")?;
                ensure(!branch.jq.is_empty(), "failure jq non-empty")?;
                ensure(
                    !branch.next_action.is_empty(),
                    "failure next action non-empty",
                )?;
            }
        }
        Ok(())
    }

    #[test]
    fn distribution_recipes_document_current_cli_surfaces() -> TestResult {
        let find_recipe = |id: &str| {
            AGENT_DOC_RECIPES
                .iter()
                .find(|recipe| recipe.id == id)
                .ok_or_else(|| format!("recipe {id} exists"))
        };

        ensure_equal(
            &find_recipe("install-check")?.command,
            &"ee install check --json",
            "install check recipe command",
        )?;
        ensure_equal(
            &find_recipe("update-dry-run")?.command,
            &"ee update --dry-run --json",
            "update dry-run recipe command",
        )?;
        ensure_equal(
            &find_recipe("duplicate-binary-fix")?.command,
            &"ee install check --json",
            "duplicate binary recipe command",
        )?;
        ensure_equal(
            &find_recipe("offline-update-posture")?.command,
            &"ee update --dry-run --offline --json",
            "offline update recipe command",
        )?;
        ensure_equal(
            &find_recipe("update-failure-bundle")?.command,
            &"ee support bundle --dry-run --json",
            "support bundle recipe command",
        )?;

        let mut rendered_parts = Vec::new();
        for recipe in AGENT_DOC_RECIPES {
            rendered_parts.push(format!(
                "{}\n{}\n{}\n{}",
                recipe.command, recipe.jq, recipe.success_check, recipe.description
            ));
            for branch in recipe.failure_branches {
                rendered_parts.push(format!(
                    "{}\n{}\n{}",
                    branch.condition, branch.jq, branch.next_action
                ));
            }
        }
        let rendered = rendered_parts.join("\n");
        for obsolete in [
            "install diagnose",
            "support-bundle",
            "update --offline --check",
            "config unset version-pin",
            "checksumValid",
            ".data.current,",
            ".data.target,",
            ".data.postUpdate",
            ".data.duplicates",
        ] {
            ensure(
                !rendered.contains(obsolete),
                format!("agent docs recipes must not advertise obsolete surface `{obsolete}`"),
            )?;
        }
        Ok(())
    }
}
