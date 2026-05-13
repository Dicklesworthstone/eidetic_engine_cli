//! Producer identity metadata for agent-produced evidence.
//!
//! This module defines a stable JSON contract for "who produced this durable
//! fact" without tying the model to any one harness or coordination system.

use serde::{Deserialize, Serialize};

pub const PRODUCER_METADATA_SCHEMA_V1: &str = "ee.producer.metadata.v1";
pub const PRODUCER_SCHEMA_CATALOG_V1: &str = "ee.producer.schemas.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerIdentityStatus {
    Known,
    Unknown,
    Unobserved,
}

impl ProducerIdentityStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Unknown => "unknown",
            Self::Unobserved => "unobserved",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerSourceSystem {
    Cli,
    Cass,
    Audit,
    AgentMail,
    Recorder,
    Curation,
    Verification,
    Unknown,
}

impl ProducerSourceSystem {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Cass => "cass",
            Self::Audit => "audit",
            Self::AgentMail => "agent_mail",
            Self::Recorder => "recorder",
            Self::Curation => "curation",
            Self::Verification => "verification",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentIdentity {
    pub status: ProducerIdentityStatus,
    pub agent_name: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
}

impl AgentIdentity {
    #[must_use]
    pub fn known(agent_name: Option<&str>, harness: Option<&str>, model: Option<&str>) -> Self {
        Self {
            status: ProducerIdentityStatus::Known,
            agent_name: normalized_non_empty(agent_name),
            harness: normalized_non_empty(harness),
            model: normalized_non_empty(model),
        }
    }

    #[must_use]
    pub fn unknown() -> Self {
        Self {
            status: ProducerIdentityStatus::Unknown,
            agent_name: None,
            harness: None,
            model: None,
        }
    }

    #[must_use]
    pub fn unobserved() -> Self {
        Self {
            status: ProducerIdentityStatus::Unobserved,
            agent_name: None,
            harness: None,
            model: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    pub workspace_fingerprint: Option<String>,
}

impl AgentRun {
    #[must_use]
    pub fn new(
        run_id: Option<&str>,
        session_id: Option<&str>,
        workspace_fingerprint: Option<&str>,
    ) -> Self {
        Self {
            run_id: normalized_non_empty(run_id),
            session_id: normalized_non_empty(session_id),
            workspace_fingerprint: normalized_non_empty(workspace_fingerprint),
        }
    }

    #[must_use]
    pub fn unobserved() -> Self {
        Self {
            run_id: None,
            session_id: None,
            workspace_fingerprint: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerMetadata {
    pub schema: String,
    pub source_system: ProducerSourceSystem,
    pub identity: AgentIdentity,
    pub run: AgentRun,
    pub observed_at: Option<String>,
}

impl ProducerMetadata {
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "producer metadata intentionally mirrors the stable JSON fields"
    )]
    pub fn known_agent(
        source_system: ProducerSourceSystem,
        agent_name: Option<&str>,
        harness: Option<&str>,
        model: Option<&str>,
        run_id: Option<&str>,
        session_id: Option<&str>,
        workspace_fingerprint: Option<&str>,
        observed_at: Option<&str>,
    ) -> Self {
        Self {
            schema: PRODUCER_METADATA_SCHEMA_V1.to_owned(),
            source_system,
            identity: AgentIdentity::known(agent_name, harness, model),
            run: AgentRun::new(run_id, session_id, workspace_fingerprint),
            observed_at: normalized_non_empty(observed_at),
        }
    }

    #[must_use]
    pub fn unknown_agent(
        source_system: ProducerSourceSystem,
        run_id: Option<&str>,
        session_id: Option<&str>,
        workspace_fingerprint: Option<&str>,
        observed_at: Option<&str>,
    ) -> Self {
        Self {
            schema: PRODUCER_METADATA_SCHEMA_V1.to_owned(),
            source_system,
            identity: AgentIdentity::unknown(),
            run: AgentRun::new(run_id, session_id, workspace_fingerprint),
            observed_at: normalized_non_empty(observed_at),
        }
    }

    #[must_use]
    pub fn unobserved(
        source_system: ProducerSourceSystem,
        workspace_fingerprint: Option<&str>,
        observed_at: Option<&str>,
    ) -> Self {
        Self {
            schema: PRODUCER_METADATA_SCHEMA_V1.to_owned(),
            source_system,
            identity: AgentIdentity::unobserved(),
            run: AgentRun::new(None, None, workspace_fingerprint),
            observed_at: normalized_non_empty(observed_at),
        }
    }

    #[must_use]
    pub fn manual_remember(workspace_fingerprint: Option<&str>, observed_at: Option<&str>) -> Self {
        Self::unobserved(
            ProducerSourceSystem::Cli,
            workspace_fingerprint,
            observed_at,
        )
    }

    #[must_use]
    pub fn context_pack(workspace_fingerprint: Option<&str>, observed_at: Option<&str>) -> Self {
        Self::unobserved(
            ProducerSourceSystem::Cli,
            workspace_fingerprint,
            observed_at,
        )
    }

    #[must_use]
    pub fn curation_candidate(
        source_type: &str,
        source_id: Option<&str>,
        workspace_fingerprint: Option<&str>,
        observed_at: Option<&str>,
    ) -> Self {
        if source_type.eq_ignore_ascii_case("cass")
            || source_id
                .is_some_and(|id| id.starts_with("cass:") || id.starts_with("cass-session://"))
        {
            return Self::cass_evidence(source_id, workspace_fingerprint, observed_at);
        }

        Self::unobserved(
            ProducerSourceSystem::Curation,
            workspace_fingerprint,
            observed_at,
        )
    }

    #[must_use]
    pub fn audit_actor(actor: Option<&str>, observed_at: Option<&str>) -> Self {
        if let Some(agent_name) = normalized_non_empty(actor) {
            return Self::known_agent(
                ProducerSourceSystem::Audit,
                Some(&agent_name),
                None,
                None,
                None,
                None,
                None,
                observed_at,
            );
        }

        Self::unobserved(ProducerSourceSystem::Audit, None, observed_at)
    }

    #[must_use]
    pub fn cass_evidence(
        source_id: Option<&str>,
        workspace_fingerprint: Option<&str>,
        observed_at: Option<&str>,
    ) -> Self {
        Self::unknown_agent(
            ProducerSourceSystem::Cass,
            None,
            cass_session_id(source_id).as_deref(),
            workspace_fingerprint,
            observed_at,
        )
    }

    pub fn to_json_string(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    #[must_use]
    pub fn to_json_string_lossy(&self) -> String {
        self.to_json_string()
            .unwrap_or_else(|_| r#"{"schema":"ee.producer.metadata.v1","sourceSystem":"unknown","identity":{"status":"unknown","agentName":null,"harness":null,"model":null},"run":{"runId":null,"sessionId":null,"workspaceFingerprint":null},"observedAt":null}"#.to_owned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerFieldSchema {
    pub name: &'static str,
    pub kind: &'static str,
    pub required: bool,
    pub description: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerObjectSchema {
    pub id: &'static str,
    pub description: &'static str,
    pub fields: Vec<ProducerFieldSchema>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProducerSchemaCatalog {
    schema: &'static str,
    schemas: Vec<ProducerObjectSchema>,
}

#[must_use]
pub fn producer_schemas() -> Vec<ProducerObjectSchema> {
    vec![
        ProducerObjectSchema {
            id: PRODUCER_METADATA_SCHEMA_V1,
            description: "Producer metadata attached to memory-like evidence records.",
            fields: vec![
                ProducerFieldSchema {
                    name: "schema",
                    kind: "string",
                    required: true,
                    description: "Stable schema identifier.",
                },
                ProducerFieldSchema {
                    name: "sourceSystem",
                    kind: "enum",
                    required: true,
                    description: "System that observed or produced the record.",
                },
                ProducerFieldSchema {
                    name: "identity",
                    kind: "object",
                    required: true,
                    description: "Agent identity, explicitly known, unknown, or unobserved.",
                },
                ProducerFieldSchema {
                    name: "run",
                    kind: "object",
                    required: true,
                    description: "Run, session, and workspace identifiers when observed.",
                },
                ProducerFieldSchema {
                    name: "observedAt",
                    kind: "rfc3339|null",
                    required: true,
                    description: "Time the producer identity was observed, or null.",
                },
            ],
        },
        ProducerObjectSchema {
            id: "ee.producer.agent_identity.v1",
            description: "Agent identity fields independent of any one harness.",
            fields: vec![
                ProducerFieldSchema {
                    name: "status",
                    kind: "known|unknown|unobserved",
                    required: true,
                    description: "Whether identity fields were observed.",
                },
                ProducerFieldSchema {
                    name: "agentName",
                    kind: "string|null",
                    required: true,
                    description: "Agent name if explicitly known.",
                },
                ProducerFieldSchema {
                    name: "harness",
                    kind: "string|null",
                    required: true,
                    description: "Harness or program if explicitly known.",
                },
                ProducerFieldSchema {
                    name: "model",
                    kind: "string|null",
                    required: true,
                    description: "Model if explicitly supplied by the source.",
                },
            ],
        },
        ProducerObjectSchema {
            id: "ee.producer.agent_run.v1",
            description: "Observed run/session/workspace identity for a producer.",
            fields: vec![
                ProducerFieldSchema {
                    name: "runId",
                    kind: "string|null",
                    required: true,
                    description: "Run identifier if explicitly known.",
                },
                ProducerFieldSchema {
                    name: "sessionId",
                    kind: "string|null",
                    required: true,
                    description: "Session identifier if explicitly known.",
                },
                ProducerFieldSchema {
                    name: "workspaceFingerprint",
                    kind: "string|null",
                    required: true,
                    description: "Workspace fingerprint if explicitly known.",
                },
            ],
        },
    ]
}

#[must_use]
pub fn producer_schema_catalog_json() -> String {
    let catalog = ProducerSchemaCatalog {
        schema: PRODUCER_SCHEMA_CATALOG_V1,
        schemas: producer_schemas(),
    };
    serde_json::to_string(&catalog)
        .unwrap_or_else(|_| format!(r#"{{"schema":"{PRODUCER_SCHEMA_CATALOG_V1}","schemas":[]}}"#))
}

fn normalized_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_owned)
}

fn cass_session_id(source_id: Option<&str>) -> Option<String> {
    let raw = normalized_non_empty(source_id)?;
    if let Some(rest) = raw.strip_prefix("cass-session://") {
        return rest
            .split(['#', '?'])
            .next()
            .and_then(|value| normalized_non_empty(Some(value)));
    }
    if let Some(rest) = raw.strip_prefix("cass:") {
        return rest
            .split(':')
            .next()
            .and_then(|value| normalized_non_empty(Some(value)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const PRODUCER_METADATA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/producer_metadata.json.golden");

    #[test]
    fn known_agent_metadata_matches_golden_shape() -> TestResult {
        let metadata = ProducerMetadata::known_agent(
            ProducerSourceSystem::AgentMail,
            Some("MagentaSummit"),
            Some("codex-cli"),
            Some("gpt-5"),
            Some("run_20260512"),
            Some("019e1a95-c745-7ea0-9b71-d0c72185aa97"),
            Some("repo:25e38e130474e7f0292de2a3"),
            Some("2026-05-12T12:00:00Z"),
        );

        assert_eq!(
            metadata.to_json_string()?,
            PRODUCER_METADATA_GOLDEN.trim_end_matches('\n')
        );
        Ok(())
    }

    #[test]
    fn unknown_agent_keeps_identity_explicit() {
        let metadata = ProducerMetadata::unknown_agent(
            ProducerSourceSystem::AgentMail,
            None,
            None,
            Some("repo:abc"),
            None,
        );

        assert_eq!(metadata.identity.status, ProducerIdentityStatus::Unknown);
        assert_eq!(metadata.identity.agent_name, None);
        assert_eq!(
            metadata.run.workspace_fingerprint.as_deref(),
            Some("repo:abc")
        );
        assert_eq!(metadata.observed_at, None);
    }

    #[test]
    fn cass_evidence_extracts_session_without_inventing_identity() {
        let metadata = ProducerMetadata::cass_evidence(
            Some("cass-session://session-abc#L20-L30"),
            Some("repo:abc"),
            Some("2026-05-12T13:00:00Z"),
        );

        assert_eq!(metadata.source_system, ProducerSourceSystem::Cass);
        assert_eq!(metadata.identity.status, ProducerIdentityStatus::Unknown);
        assert_eq!(metadata.run.session_id.as_deref(), Some("session-abc"));
        assert_eq!(
            metadata.run.workspace_fingerprint.as_deref(),
            Some("repo:abc")
        );
    }

    #[test]
    fn manual_remembered_notes_are_unobserved_not_inferred() {
        let metadata = ProducerMetadata::manual_remember(None, None);

        assert_eq!(metadata.source_system, ProducerSourceSystem::Cli);
        assert_eq!(metadata.identity.status, ProducerIdentityStatus::Unobserved);
        assert_eq!(metadata.identity.agent_name, None);
        assert_eq!(metadata.run, AgentRun::unobserved());
    }

    #[test]
    fn producer_schema_catalog_is_stable_json() -> TestResult {
        let catalog = producer_schema_catalog_json();
        let value: serde_json::Value = serde_json::from_str(&catalog)?;

        assert_eq!(value["schema"], PRODUCER_SCHEMA_CATALOG_V1);
        assert_eq!(value["schemas"][0]["id"], PRODUCER_METADATA_SCHEMA_V1);
        Ok(())
    }
}
