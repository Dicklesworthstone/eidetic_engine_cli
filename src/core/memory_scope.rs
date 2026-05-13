use std::collections::BTreeSet;
use std::path::Path;
use std::str::FromStr;

use crate::config::{ConfigFile, EnvVar, read_env_var};
use crate::db::StoredMemory;
use crate::models::{MemoryScope, MemoryScopeStats, TrustClass};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryScopeContext {
    pub scope: MemoryScope,
    pub strict_scope: bool,
    pub current_agent: Option<String>,
    pub team_members: BTreeSet<String>,
}

impl MemoryScopeContext {
    #[must_use]
    pub fn for_workspace(workspace_path: &Path, scope: MemoryScope, strict_scope: bool) -> Self {
        Self {
            scope,
            strict_scope,
            current_agent: current_agent_name(),
            team_members: load_team_members(workspace_path),
        }
    }

    #[must_use]
    pub fn stats(&self) -> MemoryScopeStats {
        MemoryScopeStats::new(
            self.scope,
            self.strict_scope,
            self.current_agent.clone(),
            self.team_members.len(),
        )
    }

    #[must_use]
    pub fn memory_in_scope(&self, memory: &StoredMemory) -> bool {
        match self.scope {
            MemoryScope::Swarm | MemoryScope::Workspace => true,
            MemoryScope::Verified => is_verified_memory(memory),
            MemoryScope::SelfOnly => self
                .current_agent
                .as_deref()
                .is_some_and(|agent| memory_producer_agent(memory).as_deref() == Some(agent)),
            MemoryScope::Team => memory_producer_agent(memory).is_some_and(|producer| {
                self.current_agent.as_deref() == Some(producer.as_str())
                    || self.team_members.contains(&producer)
            }),
        }
    }
}

#[must_use]
pub fn current_agent_name() -> Option<String> {
    read_env_var(EnvVar::AgentName).and_then(normalized_non_empty)
}

#[must_use]
pub fn remember_trust_subclass(base: &str) -> Option<String> {
    let base = base.trim();
    let Some(agent) = current_agent_name() else {
        return normalized_non_empty(base.to_owned());
    };
    if base.is_empty() {
        Some(format!("agent:{agent}"))
    } else {
        Some(format!("{base}; agent:{agent}"))
    }
}

#[must_use]
pub fn memory_producer_agent(memory: &StoredMemory) -> Option<String> {
    memory
        .trust_subclass
        .as_deref()
        .and_then(agent_from_trust_subclass)
        .or_else(|| {
            memory
                .provenance_uri
                .as_deref()
                .and_then(agent_from_provenance_uri)
        })
}

#[must_use]
pub fn is_verified_memory(memory: &StoredMemory) -> bool {
    matches!(
        TrustClass::from_str(&memory.trust_class),
        Ok(TrustClass::HumanExplicit | TrustClass::AgentValidated)
    )
}

fn load_team_members(workspace_path: &Path) -> BTreeSet<String> {
    let config_path = workspace_path.join(".ee").join("config.toml");
    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return BTreeSet::new();
    };
    let Ok(config) = ConfigFile::parse(&contents) else {
        return BTreeSet::new();
    };
    config
        .trust
        .team_members
        .unwrap_or_default()
        .into_iter()
        .filter_map(normalized_non_empty)
        .collect()
}

fn agent_from_trust_subclass(value: &str) -> Option<String> {
    value
        .split([';', ',', '|'])
        .map(str::trim)
        .find_map(|part| {
            part.strip_prefix("agent:")
                .or_else(|| part.strip_prefix("agent="))
                .map(str::to_owned)
        })
        .and_then(normalized_non_empty)
}

fn agent_from_provenance_uri(value: &str) -> Option<String> {
    let rest = value
        .strip_prefix("agent://")
        .or_else(|| value.strip_prefix("agent-mail://"))
        .or_else(|| value.strip_prefix("agent_mail://"))?;
    let name = rest
        .split(['/', '#', '?'])
        .next()
        .unwrap_or_default()
        .to_owned();
    normalized_non_empty(name)
}

fn normalized_non_empty(value: impl Into<String>) -> Option<String> {
    let value = value.into().trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_with_scope(trust_class: TrustClass, trust_subclass: Option<&str>) -> StoredMemory {
        StoredMemory {
            id: "mem-1".to_owned(),
            workspace_id: "workspace".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: "Run cargo fmt --check before release.".to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: trust_class.as_str().to_owned(),
            trust_subclass: trust_subclass.map(str::to_owned),
            provenance_chain_hash: None,
            provenance_chain_hash_version: "none".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-05-13T00:00:00Z".to_owned(),
            updated_at: "2026-05-13T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    #[test]
    fn verified_scope_accepts_human_and_agent_validated_only() {
        let context = MemoryScopeContext {
            scope: MemoryScope::Verified,
            strict_scope: false,
            current_agent: None,
            team_members: BTreeSet::new(),
        };

        assert!(context.memory_in_scope(&memory_with_scope(TrustClass::HumanExplicit, None)));
        assert!(context.memory_in_scope(&memory_with_scope(TrustClass::AgentValidated, None)));
        assert!(!context.memory_in_scope(&memory_with_scope(TrustClass::AgentAssertion, None)));
    }

    #[test]
    fn self_scope_requires_matching_producer_agent() {
        let context = MemoryScopeContext {
            scope: MemoryScope::SelfOnly,
            strict_scope: false,
            current_agent: Some("BlueLake".to_owned()),
            team_members: BTreeSet::new(),
        };

        assert!(context.memory_in_scope(&memory_with_scope(
            TrustClass::HumanExplicit,
            Some("ee remember; agent:BlueLake")
        )));
        assert!(!context.memory_in_scope(&memory_with_scope(
            TrustClass::HumanExplicit,
            Some("ee remember; agent:GreenField")
        )));
    }

    #[test]
    fn team_scope_accepts_configured_team_members() {
        let context = MemoryScopeContext {
            scope: MemoryScope::Team,
            strict_scope: false,
            current_agent: Some("BlueLake".to_owned()),
            team_members: BTreeSet::from(["GreenField".to_owned()]),
        };

        assert!(context.memory_in_scope(&memory_with_scope(
            TrustClass::HumanExplicit,
            Some("agent=GreenField")
        )));
        assert!(!context.memory_in_scope(&memory_with_scope(
            TrustClass::HumanExplicit,
            Some("agent=RedStone")
        )));
    }

    #[test]
    fn workspace_and_swarm_scopes_include_any_memory() {
        let mut context = MemoryScopeContext {
            scope: MemoryScope::Workspace,
            strict_scope: false,
            current_agent: None,
            team_members: BTreeSet::new(),
        };
        let memory = memory_with_scope(TrustClass::AgentAssertion, Some("agent=RedStone"));

        assert!(context.memory_in_scope(&memory));
        context.scope = MemoryScope::Swarm;
        assert!(context.memory_in_scope(&memory));
    }
}
