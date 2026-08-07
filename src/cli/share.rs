use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{ArgAction, Parser, Subcommand};
use serde_json::{Value as JsonValue, json};

use crate::config::MeshLane;
use crate::core::memory_scope::MeshOutboundPolicyDecisionInput;
use crate::db::{DbConnection, StoredMemory};
use crate::mesh::policy::MeshPeerPolicyRegistry;
use crate::models::{DomainError, ProcessExitCode};
use crate::output;
use crate::policy::{
    SHARE_PREVIEW_PEER_UNKNOWN_CODE, SharePreviewCandidate, SharePreviewInput, SharePreviewReport,
    build_share_preview, redact_secret_like_content,
};

use super::{Cli, write_domain_error, write_stdout};

const SHARE_PREVIEW_COMMAND: &str = "share preview";
const EMBEDDING_ESTIMATED_BYTES: u64 = 1536 * 4;
const SHARE_EVENT_EXPORT_NOT_PERFORMED: &str = "export_not_performed";
const SHARE_EVENT_PREVIEW_GENERATED: &str = "preview_generated";

/// Subcommands for `ee share`.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum ShareCommand {
    /// Preview what would be shared with a peer without exporting anything.
    Preview(SharePreviewArgs),
}

/// Arguments for `ee share preview`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct SharePreviewArgs {
    /// Target peer identity or local peer alias.
    #[arg(long = "peer", value_name = "PEER_ID")]
    pub peer_id: String,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Only include memories at this level.
    #[arg(long, value_name = "LEVEL")]
    pub level: Option<String>,

    /// Include memory body lane in the allowed estimate.
    #[arg(long = "include-body", action = ArgAction::SetTrue)]
    pub include_body: bool,

    /// Include embedding lane in the allowed estimate.
    #[arg(long = "include-embeddings", action = ArgAction::SetTrue)]
    pub include_embeddings: bool,

    /// Maximum number of representative redacted examples.
    #[arg(long = "max-examples", default_value_t = 6)]
    pub max_examples: usize,
}

pub fn handle_share<W, E>(
    cli: &Cli,
    command: &ShareCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    match command {
        ShareCommand::Preview(args) => handle_share_preview(cli, args, stdout, stderr),
    }
}

fn handle_share_preview<W, E>(
    cli: &Cli,
    args: &SharePreviewArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let validated_args = match validate_share_preview_args(args) {
        Ok(validated_args) => validated_args,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };

    let workspace_path = cli.resolve_workspace();
    let database_path = args
        .database
        .clone()
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let connection = match DbConnection::open_file(&database_path) {
        Ok(connection) => connection,
        Err(error) => {
            let domain_error = storage_error("Failed to open share-preview database", error);
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
    };
    let workspace_id = match resolve_share_workspace_id(&connection, &workspace_path) {
        Ok(workspace_id) => workspace_id,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let memories = match connection.list_memories(&workspace_id, args.level.as_deref(), false) {
        Ok(memories) => memories,
        Err(error) => {
            let domain_error = storage_error("Failed to list share-preview memories", error);
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
    };
    let (registry, _, _) = match super::mesh::load_mesh_peer_policy_registry(
        cli,
        args.database.as_deref(),
        &connection,
        &workspace_id,
    ) {
        Ok(loaded) => loaded,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let candidate_set = share_preview_candidates(
        &memories,
        args.include_body,
        args.include_embeddings,
        &registry,
        validated_args.peer_id,
        &workspace_id,
    );
    let report = build_share_preview(&SharePreviewInput {
        target_peer_id: validated_args.peer_id,
        candidates: &candidate_set.candidates,
        consent_required: true,
        max_examples: args.max_examples,
    });
    let render_input = SharePreviewRenderInput {
        workspace_path: &workspace_path,
        database_path: &database_path,
        workspace_id: &workspace_id,
        args,
        report: &report,
        peer_unknown: candidate_set.peer_unknown,
    };

    write_share_preview_report(cli, &render_input, stdout)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SharePreviewValidatedArgs<'a> {
    peer_id: &'a str,
}

fn validate_share_preview_args(
    args: &SharePreviewArgs,
) -> Result<SharePreviewValidatedArgs<'_>, DomainError> {
    let peer_id = args.peer_id.trim();
    if peer_id.is_empty() {
        return Err(DomainError::Usage {
            message: "share preview requires --peer to name the target peer".to_owned(),
            repair: Some("Use `ee share preview --peer peer_alpha --json`.".to_owned()),
        });
    }

    Ok(SharePreviewValidatedArgs { peer_id })
}

struct SharePreviewCandidateSet<'a> {
    candidates: Vec<SharePreviewCandidate<'a>>,
    peer_unknown: bool,
}

fn policy_action_label(allowed: bool) -> &'static str {
    if allowed { "allow" } else { "deny" }
}

/// Build the per-lane share-preview candidates for `target_peer_id`, deriving
/// each lane's `policy_action` from the real outbound peer-policy verdict
/// (`decide_outbound`) rather than simulating "allow". Memories are treated as
/// originating in the local workspace, so onward-sharing provenance of
/// imported memories is out of scope here. Peer resolution is lane-independent,
/// so a single probe decides whether the peer is configured at all: when it is
/// not, `select_outbound_policy` errors, every lane fails closed (deny), and
/// `peer_unknown` is set so the caller can surface
/// [`SHARE_PREVIEW_PEER_UNKNOWN_CODE`]. The `--include-body`/`--include-embeddings`
/// flags remain the operator's opt-in for previewing those lanes; an
/// un-requested lane is always denied regardless of policy, and a body whose
/// content is secret-like is never counted as exportable even when the peer
/// policy would otherwise allow the body lane.
fn share_preview_candidates<'a>(
    memories: &'a [StoredMemory],
    include_body: bool,
    include_embeddings: bool,
    registry: &MeshPeerPolicyRegistry,
    target_peer_id: &str,
    workspace_id: &str,
) -> SharePreviewCandidateSet<'a> {
    let outbound_input = |material_lane: MeshLane| MeshOutboundPolicyDecisionInput {
        local_workspace_id: workspace_id,
        target_peer_id,
        origin_workspace_id: workspace_id,
        material_lane,
        payload_is_redacted: false,
    };

    let peer_unknown = registry
        .select_outbound_policy(&outbound_input(MeshLane::Metadata))
        .is_err();

    // The per-lane verdicts do not depend on per-memory content (we evaluate
    // raw-payload export with `payload_is_redacted = false`), so resolve them
    // once and gate body/embedding on the operator's include flags.
    let metadata_allowed = registry
        .decide_outbound(&outbound_input(MeshLane::Metadata))
        .permits_payload_export();
    let body_policy_allows = include_body
        && registry
            .decide_outbound(&outbound_input(MeshLane::Body))
            .permits_payload_export();
    let embedding_allowed = include_embeddings
        && registry
            .decide_outbound(&outbound_input(MeshLane::Embedding))
            .permits_payload_export();

    let mut candidates = Vec::with_capacity(memories.len().saturating_mul(3));
    for memory in memories {
        candidates.push(SharePreviewCandidate {
            memory_id: &memory.id,
            entity_revision: &memory.updated_at,
            level: &memory.level,
            kind: &memory.kind,
            trust_class: &memory.trust_class,
            material_lane: "metadata",
            redaction_class: "metadata_only",
            policy_action: policy_action_label(metadata_allowed),
            content_preview: &memory.content,
            estimated_bytes: if metadata_allowed {
                metadata_estimated_bytes(memory)
            } else {
                0
            },
            body_bytes: 0,
            embedding_bytes: 0,
        });

        let body_redaction = redact_secret_like_content(&memory.content);
        let body_exportable = body_policy_allows && !body_redaction.redacted;
        candidates.push(SharePreviewCandidate {
            memory_id: &memory.id,
            entity_revision: &memory.updated_at,
            level: &memory.level,
            kind: &memory.kind,
            trust_class: &memory.trust_class,
            material_lane: "body",
            redaction_class: if include_body {
                if body_redaction.redacted {
                    "body_redacted"
                } else {
                    "body_allowed"
                }
            } else {
                "body_denied"
            },
            policy_action: policy_action_label(body_exportable),
            content_preview: &memory.content,
            estimated_bytes: if body_exportable {
                memory.content.len() as u64
            } else {
                0
            },
            body_bytes: if body_exportable {
                memory.content.len() as u64
            } else {
                0
            },
            embedding_bytes: 0,
        });

        candidates.push(SharePreviewCandidate {
            memory_id: &memory.id,
            entity_revision: &memory.updated_at,
            level: &memory.level,
            kind: &memory.kind,
            trust_class: &memory.trust_class,
            material_lane: "embedding",
            redaction_class: if include_embeddings {
                "embedding_allowed"
            } else {
                "embedding_denied"
            },
            policy_action: policy_action_label(embedding_allowed),
            content_preview: &memory.content,
            estimated_bytes: if embedding_allowed {
                EMBEDDING_ESTIMATED_BYTES
            } else {
                0
            },
            body_bytes: 0,
            embedding_bytes: if embedding_allowed {
                EMBEDDING_ESTIMATED_BYTES
            } else {
                0
            },
        });
    }

    SharePreviewCandidateSet {
        candidates,
        peer_unknown,
    }
}

fn metadata_estimated_bytes(memory: &StoredMemory) -> u64 {
    [
        memory.id.as_str(),
        memory.workspace_id.as_str(),
        memory.level.as_str(),
        memory.kind.as_str(),
        memory.trust_class.as_str(),
        memory.trust_subclass.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .map(|value| value.len() as u64)
    .sum::<u64>()
    .saturating_add(32)
}

struct SharePreviewRenderInput<'a> {
    workspace_path: &'a Path,
    database_path: &'a Path,
    workspace_id: &'a str,
    args: &'a SharePreviewArgs,
    report: &'a SharePreviewReport,
    peer_unknown: bool,
}

fn write_share_preview_report<W>(
    cli: &Cli,
    input: &SharePreviewRenderInput<'_>,
    stdout: &mut W,
) -> ProcessExitCode
where
    W: Write,
{
    match cli.renderer() {
        output::Renderer::Human | output::Renderer::Markdown => write_stdout(
            stdout,
            &render_share_preview_human(input.report, input.peer_unknown),
        ),
        output::Renderer::Toon => {
            let data = share_preview_data_json(input);
            write_stdout(
                stdout,
                &(output::render_toon_from_json(&data.to_string()) + "\n"),
            )
        }
        output::Renderer::Json
        | output::Renderer::Jsonl
        | output::Renderer::Compact
        | output::Renderer::Hook => {
            let json = json!({
                "schema": crate::models::RESPONSE_SCHEMA_V2,
                "success": true,
                "data": share_preview_data_json(input),
                "degraded": share_preview_degraded(input),
            });
            write_stdout(stdout, &(json.to_string() + "\n"))
        }
    }
}

/// Degraded notices for the share preview. The only entry is the fail-closed
/// [`SHARE_PREVIEW_PEER_UNKNOWN_CODE`], emitted when the target peer has no
/// resolvable outbound policy so nothing would be shared.
fn share_preview_degraded(input: &SharePreviewRenderInput<'_>) -> Vec<JsonValue> {
    let mut degraded = Vec::new();
    if input.peer_unknown {
        degraded.push(json!({
            "code": SHARE_PREVIEW_PEER_UNKNOWN_CODE,
            "severity": "warning",
            "message": format!(
                "No outbound mesh policy resolves for peer '{}'; every lane fails closed and nothing would be shared.",
                input.report.target_peer_id
            ),
            "repair": "Add a [[mesh.peer_policies]] entry for this peer (matching workspace_id, peer_id, and origin_workspace_ids with allowed_lanes) to .ee/config.toml, then re-run the preview.",
        }));
    }
    degraded
}

fn share_preview_data_json(input: &SharePreviewRenderInput<'_>) -> JsonValue {
    json!({
        "schema": input.report.schema,
        "command": SHARE_PREVIEW_COMMAND,
        "workspacePath": input.workspace_path.display().to_string(),
        "workspaceId": input.workspace_id,
        "databasePath": input.database_path.display().to_string(),
        "dryRun": true,
        "exportPerformed": false,
        "includeBody": input.args.include_body,
        "includeEmbeddings": input.args.include_embeddings,
        "preview": input.report,
        "events": share_preview_events(),
    })
}

fn share_preview_events() -> Vec<JsonValue> {
    vec![
        json!({
            "event": SHARE_EVENT_PREVIEW_GENERATED,
        }),
        json!({
            "event": SHARE_EVENT_EXPORT_NOT_PERFORMED,
            "dryRun": true,
        }),
    ]
}

fn render_share_preview_human(report: &SharePreviewReport, peer_unknown: bool) -> String {
    let mut output = format!(
        "Share preview for {peer}\n  Dry run: yes\n  Export performed: no\n  Candidates: {total} total, {allowed} exportable, {denied} denied\n  Estimated exposure: {bytes} bytes ({body} body, {embedding} embedding)\n",
        peer = report.target_peer_id,
        total = report.total_candidates,
        allowed = report.exportable_count,
        denied = report.denied_count,
        bytes = report.estimated_bytes,
        body = report.estimated_body_bytes,
        embedding = report.estimated_embedding_bytes,
    );
    if peer_unknown {
        output.push_str(
            "  WARNING: no outbound mesh policy resolves for this peer; nothing would be shared.\n",
        );
    }
    if !report.denied_classes.is_empty() {
        output.push_str("  Denied classes:\n");
        for denied in &report.denied_classes {
            output.push_str(&format!("    - {denied}\n"));
        }
    }
    output
}

fn resolve_share_workspace_id(
    connection: &DbConnection,
    workspace_path: &std::path::Path,
) -> Result<String, DomainError> {
    let primary = workspace_path.to_string_lossy().into_owned();
    if let Some(workspace) = connection
        .get_workspace_by_path(&primary)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace: {error}"),
            repair: Some("ee doctor".to_string()),
        })?
    {
        return Ok(workspace.id);
    }

    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let canonical_str = canonical.to_string_lossy().into_owned();
    if canonical_str != primary
        && let Some(workspace) =
            connection
                .get_workspace_by_path(&canonical_str)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to query workspace: {error}"),
                    repair: Some("ee doctor".to_string()),
                })?
    {
        return Ok(workspace.id);
    }

    Ok(super::stable_cli_workspace_id(&canonical))
}

fn storage_error(context: &str, error: crate::db::DbError) -> DomainError {
    DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some("Run `ee doctor --json` and verify the workspace database.".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigFile;

    const TEST_WORKSPACE_ID: &str = "wsp_sharepreview0000000000001";

    /// A `[[mesh.peer_policies]]` registry that allows every lane for
    /// `peer_alpha` originating in [`TEST_WORKSPACE_ID`], so the flag-gated
    /// candidate-generation assertions are driven by the include flags and
    /// redaction rather than by a missing policy.
    fn allow_all_registry() -> MeshPeerPolicyRegistry {
        let config = ConfigFile::parse(
            r#"
[[mesh.peer_policies]]
policy_id = "pol_preview"
workspace_id = "wsp_sharepreview0000000000001"
peer_id = "peer_alpha"
origin_workspace_ids = ["wsp_sharepreview0000000000001"]
trust_lane = "peerAgent"
import_trust_class = "agent_validated"
default_action = "deny"

[mesh.peer_policies.allowed_lanes]
metadata = "allow"
body = "allow"
embedding = "allow"
graph_link = "allow"
revision_notice = "allow"
curation_signal = "allow"

[mesh.peer_policies.redaction]
metadata = "share"
preview = "redact"
body = "share"
embedding = "share"

[mesh.peer_policies.body_fetch]
allowed = true
requires_consent = false
max_bytes = 1048576
"#,
        )
        .expect("allow-all preview policy config should parse");
        MeshPeerPolicyRegistry::from_config(&config)
    }

    fn stored_memory(id: &str, content: &str) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
            workspace_id: "wsp_sharepreview0000000000001".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.8,
            utility: 0.7,
            importance: 0.6,
            provenance_uri: None,
            trust_class: "agent_validated".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "ee.memory.provenance_chain.v1".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-05-19T00:00:00Z".to_owned(),
            updated_at: "2026-05-19T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: Some("2026-05-19T00:00:00Z".to_owned()),
            valid_to: None,
        }
    }

    fn share_preview_args() -> SharePreviewArgs {
        SharePreviewArgs {
            peer_id: "peer_alpha".to_owned(),
            database: None,
            level: None,
            include_body: false,
            include_embeddings: false,
            max_examples: 6,
        }
    }

    #[test]
    fn share_preview_validation_rejects_blank_peer() {
        let mut args = share_preview_args();
        args.peer_id = " \t ".to_owned();

        let error = validate_share_preview_args(&args).expect_err("blank peer must fail");
        let DomainError::Usage { message, repair } = error else {
            panic!("expected usage error for blank peer");
        };

        assert!(message.contains("--peer"));
        assert_eq!(
            repair.as_deref(),
            Some("Use `ee share preview --peer peer_alpha --json`.")
        );
    }

    #[test]
    fn share_preview_validation_trims_peer_id() {
        let mut args = share_preview_args();
        args.peer_id = " peer_alpha ".to_owned();

        let validated = validate_share_preview_args(&args).expect("valid peer id");

        assert_eq!(validated.peer_id, "peer_alpha");
    }

    #[test]
    fn metadata_only_preview_denies_body_and_embedding_lanes() {
        let memories = [stored_memory(
            "mem_sharepreview00000000000001",
            "Never send API_KEY=sk-proj-local-secret over mesh.",
        )];
        let candidates = share_preview_candidates(
            &memories,
            false,
            false,
            &allow_all_registry(),
            "peer_alpha",
            TEST_WORKSPACE_ID,
        )
        .candidates;
        let report = build_share_preview(&SharePreviewInput {
            target_peer_id: "peer_alpha",
            candidates: &candidates,
            consent_required: true,
            max_examples: 4,
        });

        assert_eq!(report.total_candidates, 3);
        assert_eq!(report.exportable_count, 1);
        assert_eq!(report.denied_count, 2);
        assert_eq!(report.estimated_body_bytes, 0);
        assert_eq!(report.estimated_embedding_bytes, 0);
        assert!(
            report
                .denied_classes
                .contains(&"redaction_class:body_denied".to_owned())
        );
        assert!(
            report
                .denied_classes
                .contains(&"redaction_class:embedding_denied".to_owned())
        );
        assert!(
            report
                .examples
                .iter()
                .all(|example| !example.redacted_preview.contains("sk-proj"))
        );
    }

    #[test]
    fn body_allowed_preview_still_marks_embeddings_separately() {
        let memories = [stored_memory(
            "mem_sharepreview00000000000002",
            "Public release note can be shared after review.",
        )];
        let candidates = share_preview_candidates(
            &memories,
            true,
            false,
            &allow_all_registry(),
            "peer_alpha",
            TEST_WORKSPACE_ID,
        )
        .candidates;
        let report = build_share_preview(&SharePreviewInput {
            target_peer_id: "peer_alpha",
            candidates: &candidates,
            consent_required: true,
            max_examples: 0,
        });

        assert_eq!(report.exportable_count, 2);
        assert_eq!(report.denied_count, 1);
        assert!(report.estimated_body_bytes > 0);
        assert_eq!(report.estimated_embedding_bytes, 0);
        assert_eq!(report.counts_by_material_lane.get("body"), Some(&1));
        assert!(
            report
                .denied_classes
                .contains(&"redaction_class:embedding_denied".to_owned())
        );
    }

    #[test]
    fn body_redacted_preview_is_not_exportable() {
        let memories = [stored_memory(
            "mem_sharepreview00000000000004",
            "Never send API_KEY=sk-proj-local-secret over mesh.",
        )];
        let candidates = share_preview_candidates(
            &memories,
            true,
            false,
            &allow_all_registry(),
            "peer_alpha",
            TEST_WORKSPACE_ID,
        )
        .candidates;
        let report = build_share_preview(&SharePreviewInput {
            target_peer_id: "peer_alpha",
            candidates: &candidates,
            consent_required: true,
            max_examples: 4,
        });

        assert_eq!(report.total_candidates, 3);
        assert_eq!(report.exportable_count, 1);
        assert_eq!(report.denied_count, 2);
        assert_eq!(report.estimated_body_bytes, 0);
        assert_eq!(report.counts_by_policy_action.get("allow"), Some(&1));
        assert_eq!(report.counts_by_policy_action.get("deny"), Some(&2));
        assert_eq!(
            report.counts_by_redaction_class.get("body_redacted"),
            Some(&1)
        );
        assert!(
            report
                .denied_classes
                .contains(&"material_lane:body".to_owned())
        );
        assert!(
            report
                .denied_classes
                .contains(&"redaction_class:body_redacted".to_owned())
        );
        assert!(
            report
                .examples
                .iter()
                .all(|example| !example.redacted_preview.contains("sk-proj"))
        );
    }

    #[test]
    fn share_preview_events_are_dry_run_only() {
        let events = share_preview_events();
        let event_names = events
            .iter()
            .filter_map(|event| event.get("event").and_then(JsonValue::as_str))
            .collect::<Vec<_>>();

        assert_eq!(
            event_names,
            vec![
                SHARE_EVENT_PREVIEW_GENERATED,
                SHARE_EVENT_EXPORT_NOT_PERFORMED
            ]
        );
    }

    #[test]
    fn share_preview_json_envelope_includes_clean_degraded_array() {
        let cli = Cli::try_parse_from(["ee", "--json"]).expect("parse json cli");
        let args = share_preview_args();
        let memories = [stored_memory(
            "mem_sharepreview00000000000003",
            "Public release note can be shared after review.",
        )];
        let candidates = share_preview_candidates(
            &memories,
            false,
            false,
            &allow_all_registry(),
            "peer_alpha",
            TEST_WORKSPACE_ID,
        )
        .candidates;
        let report = build_share_preview(&SharePreviewInput {
            target_peer_id: "peer_alpha",
            candidates: &candidates,
            consent_required: true,
            max_examples: 0,
        });
        let input = SharePreviewRenderInput {
            workspace_path: Path::new("/tmp/share-preview-workspace"),
            database_path: Path::new("/tmp/share-preview-workspace/.ee/ee.db"),
            workspace_id: "wsp_sharepreview0000000000001",
            args: &args,
            report: &report,
            peer_unknown: false,
        };
        let mut stdout = Vec::new();

        let exit = write_share_preview_report(&cli, &input, &mut stdout);

        assert_eq!(exit, ProcessExitCode::Success);
        let envelope: serde_json::Value =
            serde_json::from_slice(&stdout).expect("share preview json envelope");
        assert_eq!(envelope["schema"], crate::models::RESPONSE_SCHEMA_V2);
        assert_eq!(envelope["success"], true);
        assert_eq!(envelope["degraded"], serde_json::json!([]));
        assert!(envelope["data"].get("previewHash").is_none());
        assert!(envelope["data"]["events"][0].get("previewHash").is_none());
        assert_eq!(
            envelope["data"]["schema"],
            crate::policy::SHARE_PREVIEW_SCHEMA_V2
        );
    }

    #[test]
    fn configured_peer_allows_metadata_and_reports_peer_known() {
        let memories = [stored_memory(
            "mem_sharepreview00000000000006",
            "Public release note can be shared after review.",
        )];
        let set = share_preview_candidates(
            &memories,
            false,
            false,
            &allow_all_registry(),
            "peer_alpha",
            TEST_WORKSPACE_ID,
        );

        assert!(!set.peer_unknown, "configured peer must not be unknown");
        let metadata = set
            .candidates
            .iter()
            .find(|candidate| candidate.material_lane == "metadata")
            .expect("metadata candidate present");
        assert_eq!(metadata.policy_action, "allow");
    }

    #[test]
    fn unknown_peer_fails_closed_and_denies_every_lane() {
        let memories = [stored_memory(
            "mem_sharepreview00000000000007",
            "Public release note can be shared after review.",
        )];
        // Empty registry => no policy resolves for the peer.
        let empty = MeshPeerPolicyRegistry::from_config(&ConfigFile::default());
        let set = share_preview_candidates(
            &memories,
            true,
            true,
            &empty,
            "peer_alpha",
            TEST_WORKSPACE_ID,
        );

        assert!(
            set.peer_unknown,
            "unconfigured peer must be flagged unknown"
        );
        assert!(
            set.candidates
                .iter()
                .all(|candidate| candidate.policy_action == "deny"),
            "every lane must fail closed for an unknown peer"
        );
    }

    #[test]
    fn unknown_peer_envelope_emits_peer_unknown_degraded_code() {
        let cli = Cli::try_parse_from(["ee", "--json"]).expect("parse json cli");
        let args = share_preview_args();
        let memories = [stored_memory(
            "mem_sharepreview00000000000008",
            "Public release note can be shared after review.",
        )];
        let empty = MeshPeerPolicyRegistry::from_config(&ConfigFile::default());
        let set = share_preview_candidates(
            &memories,
            false,
            false,
            &empty,
            "peer_alpha",
            TEST_WORKSPACE_ID,
        );
        let report = build_share_preview(&SharePreviewInput {
            target_peer_id: "peer_alpha",
            candidates: &set.candidates,
            consent_required: true,
            max_examples: 0,
        });
        let input = SharePreviewRenderInput {
            workspace_path: Path::new("/tmp/share-preview-workspace"),
            database_path: Path::new("/tmp/share-preview-workspace/.ee/ee.db"),
            workspace_id: TEST_WORKSPACE_ID,
            args: &args,
            report: &report,
            peer_unknown: set.peer_unknown,
        };
        let mut stdout = Vec::new();

        let exit = write_share_preview_report(&cli, &input, &mut stdout);

        assert_eq!(exit, ProcessExitCode::Success);
        let envelope: serde_json::Value =
            serde_json::from_slice(&stdout).expect("share preview json envelope");
        // A fail-closed preview is still a successful, non-mutating command.
        assert_eq!(envelope["success"], true);
        let degraded = envelope["degraded"]
            .as_array()
            .expect("degraded array present");
        assert_eq!(degraded.len(), 1);
        assert_eq!(
            degraded[0]["code"],
            crate::policy::SHARE_PREVIEW_PEER_UNKNOWN_CODE
        );
        assert_eq!(degraded[0]["severity"], "warning");
        assert!(
            degraded[0]["repair"]
                .as_str()
                .is_some_and(|repair| repair.contains("mesh.peer_policies"))
        );
    }
}
