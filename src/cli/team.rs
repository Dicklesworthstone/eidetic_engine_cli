//! Product-level `ee team` commands over mesh primitives.

use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;

use crate::db::DbConnection;
use crate::mesh::team::{
    add_local_team_node, adopt_team_project, any_local_team_paused, attest_local_id_token,
    create_local_team_with_store, execute_team_idp_token_poll, execute_team_steward_once,
    fetch_local_team_body, inspect_team_health, join_team_with_code_on_store, leave_local_team,
    list_team_activity, list_team_projects, local_team_status, mint_team_invite_with_store,
    plan_team_idp_device, reconcile_local_team_membership, remove_team_member,
    require_tailnet_attested, revalidate_team_identities, revoke_team_invite,
    revoke_team_invites_before_floor, rotate_local_signing_key,
    serve_one_bootstrap_join_with_store, set_local_team_paused, set_team_oidc_provider,
    share_team_bodies_represented, share_team_history, share_team_project, team_idp_status,
    unshare_team_bodies,
};
use crate::models::{DomainError, ProcessExitCode};
use crate::output;

use super::{Cli, write_domain_error, write_stdout};

/// Subcommands for `ee team`.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TeamCommand {
    /// Create a local team genesis on the origin stream.
    Create(TeamCreateArgs),
    /// Show locally recorded team genesis events.
    Status(TeamStatusArgs),
    /// Mint a single-use invite for the local team.
    Invite(TeamInviteArgs),
    /// Join a team by proving an invite over live TCP.
    Join(TeamJoinArgs),
    /// Revoke a pending invite.
    Revoke(TeamRevokeArgs),
    /// Share origin-owned history as metadata-only origin events.
    #[command(subcommand)]
    Share(TeamShareCommand),
    /// Stop future serving of previously published bodies.
    #[command(subcommand)]
    Unshare(TeamUnshareCommand),
    /// Membership list/remove.
    #[command(subcommand)]
    Members(TeamMembersCommand),
    /// Leave the local team (self removal).
    Leave(TeamLeaveArgs),
    /// Run one mesh sync cycle for the local team.
    Sync(TeamSyncArgs),
    /// Pause team network exchange.
    Pause(TeamPauseArgs),
    /// Resume team network exchange.
    Resume(TeamResumeArgs),
    /// List closed-metadata team activity.
    Activity(TeamActivityArgs),
    /// Mint, adopt, or list team project identities.
    #[command(subcommand)]
    Projects(TeamProjectsCommand),
    /// Fetch a published body from the local hardened cache.
    #[command(subcommand)]
    Fetch(TeamFetchCommand),
    /// Foreground steward pass.
    #[command(subcommand)]
    Steward(TeamStewardCommand),
    /// Read-only team health checks.
    Doctor(TeamDoctorArgs),
    /// Tailnet-attested identity policy.
    #[command(subcommand)]
    Idp(TeamIdpCommand),
}

/// Nested `ee team fetch` verbs.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TeamFetchCommand {
    /// Read one published body cache key.
    Body(TeamFetchBodyArgs),
}

/// Nested `ee team steward` verbs.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TeamStewardCommand {
    /// Plan and, if triggered, run one mesh sync.
    #[command(name = "once", alias = "run-once")]
    RunOnce(TeamStewardRunOnceArgs),
}

/// Nested `ee team projects` verbs.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TeamProjectsCommand {
    /// Mint a team-scoped project id for a local path.
    Share(TeamProjectsShareArgs),
    /// Map an existing project id onto a local path.
    Adopt(TeamProjectsAdoptArgs),
    /// List minted and adopted projects.
    List(TeamProjectsListArgs),
}

/// Nested `ee team members` verbs.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TeamMembersCommand {
    /// List recorded members.
    List(TeamMembersListArgs),
    /// Remove a non-self member.
    Remove(TeamMembersRemoveArgs),
    /// Bind another local node to the self member.
    AddNode(TeamMembersAddNodeArgs),
    /// Rotate the local signing key.
    RotateKey(TeamMembersRotateKeyArgs),
    /// Replay origin membership events onto local rows.
    Reconcile(TeamMembersReconcileArgs),
    /// Recheck tailnet owners against the recorded IdP policy.
    Revalidate(TeamMembersRevalidateArgs),
}

/// Nested `ee team idp` verbs.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TeamIdpCommand {
    /// Require every member node to be owned by a tailnet login.
    Require(TeamIdpRequireArgs),
    /// Show the recorded identity policy.
    Status(TeamIdpStatusArgs),
    /// Pin a secretless-public OIDC issuer from a local discovery document.
    Set(TeamIdpSetArgs),
    /// Plan a local RFC 8628 device ceremony from offline JSON.
    Device(TeamIdpDeviceArgs),
    /// Bind allowlisted ID-token claims to the local self member.
    Attest(TeamIdpAttestArgs),
}

/// Nested `ee team share` verbs.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TeamShareCommand {
    /// Preview or project pre-team local memories.
    History(TeamShareHistoryArgs),
    /// Preview or publish origin-owned bodies into the local cache.
    Bodies(TeamShareBodiesArgs),
}

/// Nested `ee team unshare` verbs.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TeamUnshareCommand {
    /// Stop future body serving from this node.
    Bodies(TeamUnshareBodiesArgs),
}

/// Arguments for `ee team create`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamCreateArgs {
    /// Human display name for the team.
    #[arg(long)]
    pub name: String,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team status`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamStatusArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team invite`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamInviteArgs {
    /// Live TCP locator the joiner should contact (IP or IP:port).
    #[arg(long)]
    pub endpoint: String,

    /// Bind the advertised locator and accept one join before exiting.
    #[arg(long)]
    pub wait: bool,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team join`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamJoinArgs {
    /// `eeteam1-` invite code.
    #[arg(long, required_unless_present = "invite_stdin")]
    pub invite: Option<String>,

    /// Read the invite code from stdin (no-echo TTY, or a pipe for agents).
    #[arg(long)]
    pub invite_stdin: bool,

    /// Display name the inviter should record for this node.
    #[arg(long, default_value = "joiner")]
    pub name: String,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team revoke`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamRevokeArgs {
    /// Invite id from `ee team invite`.
    #[arg(long, required_unless_present = "all_before_floor")]
    pub invite_id: Option<String>,

    /// Revoke every pending invite created before the authorization floor.
    #[arg(long)]
    pub all_before_floor: bool,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team share bodies`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamShareBodiesArgs {
    /// Publish previewed bodies into the hardened local cache.
    #[arg(long)]
    pub confirm: bool,

    /// Mint a sensitive `eeap1_` body-approval token on preview.
    #[arg(long)]
    pub issue_token: bool,

    /// Consume a body-approval token on confirm.
    #[arg(long)]
    pub token: Option<String>,

    /// Read the body-approval token from stdin instead of `--token`.
    #[arg(long)]
    pub token_stdin: bool,

    /// Maximum memories to consider (1–256).
    #[arg(long, default_value_t = 64)]
    pub limit: usize,

    /// Signed body representation. `already_redacted` is allowed; switching an
    /// `exact` publication to `already_redacted` is refused.
    #[arg(long, default_value = "exact")]
    pub representation: String,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team unshare bodies`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamUnshareBodiesArgs {
    /// Confirm the non-erasure unshare.
    #[arg(long)]
    pub confirm: bool,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team share history`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamShareHistoryArgs {
    /// Project the previewed history onto the origin stream.
    #[arg(long)]
    pub confirm: bool,

    /// Maximum memories to consider (1–256).
    #[arg(long, default_value_t = 64)]
    pub limit: usize,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team members list`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamMembersListArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team members remove`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamMembersRemoveArgs {
    /// Member id from `ee team status` / `ee team members list`.
    #[arg(long)]
    pub member_id: String,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team members add-node`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamMembersAddNodeArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team members rotate-key`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamMembersRotateKeyArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team members reconcile`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamMembersReconcileArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team leave`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamLeaveArgs {
    /// Confirm the irreversible local leave.
    #[arg(long)]
    pub confirm: bool,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team sync`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamSyncArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team pause`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamPauseArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team resume`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamResumeArgs {
    /// Confirm resume after a pause.
    #[arg(long)]
    pub confirm: bool,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team activity`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamActivityArgs {
    /// Exclusive member-attested as-of timestamp (RFC 3339).
    #[arg(long)]
    pub as_of: String,

    /// Maximum events to return (1–1000).
    #[arg(long, default_value_t = 100)]
    pub limit: usize,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team projects share`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamProjectsShareArgs {
    /// Human project name.
    #[arg(long)]
    pub name: String,

    /// Local path this node binds to the project.
    #[arg(long)]
    pub path: String,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team projects adopt`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamProjectsAdoptArgs {
    /// `prj_tm_` project id from another member.
    #[arg(long)]
    pub project_id: String,

    /// Human project name.
    #[arg(long)]
    pub name: String,

    /// Local path this node binds to the project.
    #[arg(long)]
    pub path: String,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team projects list`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamProjectsListArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team fetch body`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamFetchBodyArgs {
    /// Body cache key from `ee team share bodies`.
    #[arg(long)]
    pub key: String,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team steward once`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamStewardRunOnceArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team doctor`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamDoctorArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team idp require`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamIdpRequireArgs {
    /// Bind every member node to the tailnet UserProfile owner.
    #[arg(long)]
    pub tailnet_attested: bool,

    /// Optional login domain restriction, e.g. acme.com.
    #[arg(long)]
    pub domain: Option<String>,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team idp status`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamIdpStatusArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team idp set`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamIdpSetArgs {
    /// Issuer URL. Must be https.
    #[arg(long)]
    pub issuer: String,

    /// Public client id. Never a client secret.
    #[arg(long)]
    pub client_id: String,

    /// Local OpenID discovery JSON file. No network is used.
    #[arg(long, value_name = "PATH")]
    pub discovery_json: PathBuf,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team idp device`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamIdpDeviceArgs {
    /// Local OpenID discovery JSON file. No network is used.
    #[arg(long, value_name = "PATH")]
    pub discovery_json: PathBuf,

    /// Local RFC 8628 device-authorization JSON file.
    #[arg(long, value_name = "PATH")]
    pub authorization_json: PathBuf,

    /// Absolute curl binary. Defaults to /usr/bin/curl.
    #[arg(long, value_name = "PATH", default_value = "/usr/bin/curl")]
    pub curl: PathBuf,

    /// Run one constrained HTTPS token poll. Raw tokens are not printed.
    #[arg(long)]
    pub execute: bool,

    /// Absolute CA bundle used to pin TLS for `--execute`. Never `--insecure`.
    #[arg(long, value_name = "PATH")]
    pub ca_bundle: Option<PathBuf>,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team idp attest`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamIdpAttestArgs {
    /// Compact ID token, or `-` to read stdin.
    #[arg(long)]
    pub id_token: String,

    /// Configured group to match. Repeatable. Unlisted groups are dropped.
    #[arg(long = "group")]
    pub groups: Vec<String>,

    /// Local JWKS JSON used to verify the ID token signature. Optional.
    #[arg(long, value_name = "PATH")]
    pub jwks_json: Option<PathBuf>,

    /// HTTPS JWKS URL fetched with constrained curl. Optional.
    #[arg(long, value_name = "URL")]
    pub jwks_url: Option<String>,

    /// Absolute CA bundle used to pin TLS for `--jwks-url`. Never `--insecure`.
    #[arg(long, value_name = "PATH")]
    pub ca_bundle: Option<PathBuf>,

    /// Absolute curl binary. Defaults to /usr/bin/curl.
    #[arg(long, value_name = "PATH", default_value = "/usr/bin/curl")]
    pub curl: PathBuf,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team members revalidate`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamMembersRevalidateArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

pub fn handle_team<W, E>(
    cli: &Cli,
    command: &TeamCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    match command {
        TeamCommand::Create(args) => handle_team_create(cli, args, stdout, stderr),
        TeamCommand::Status(args) => handle_team_status(cli, args, stdout, stderr),
        TeamCommand::Invite(args) => handle_team_invite(cli, args, stdout, stderr),
        TeamCommand::Join(args) => handle_team_join(cli, args, stdout, stderr),
        TeamCommand::Revoke(args) => handle_team_revoke(cli, args, stdout, stderr),
        TeamCommand::Share(TeamShareCommand::History(args)) => {
            handle_team_share_history(cli, args, stdout, stderr)
        }
        TeamCommand::Share(TeamShareCommand::Bodies(args)) => {
            handle_team_share_bodies(cli, args, stdout, stderr)
        }
        TeamCommand::Unshare(TeamUnshareCommand::Bodies(args)) => {
            handle_team_unshare_bodies(cli, args, stdout, stderr)
        }
        TeamCommand::Members(TeamMembersCommand::List(args)) => {
            handle_team_members_list(cli, args, stdout, stderr)
        }
        TeamCommand::Members(TeamMembersCommand::Remove(args)) => {
            handle_team_members_remove(cli, args, stdout, stderr)
        }
        TeamCommand::Members(TeamMembersCommand::AddNode(args)) => {
            handle_team_members_add_node(cli, args, stdout, stderr)
        }
        TeamCommand::Members(TeamMembersCommand::RotateKey(args)) => {
            handle_team_members_rotate_key(cli, args, stdout, stderr)
        }
        TeamCommand::Members(TeamMembersCommand::Reconcile(args)) => {
            handle_team_members_reconcile(cli, args, stdout, stderr)
        }
        TeamCommand::Members(TeamMembersCommand::Revalidate(args)) => {
            handle_team_members_revalidate(cli, args, stdout, stderr)
        }
        TeamCommand::Idp(TeamIdpCommand::Require(args)) => {
            handle_team_idp_require(cli, args, stdout, stderr)
        }
        TeamCommand::Idp(TeamIdpCommand::Status(args)) => {
            handle_team_idp_status(cli, args, stdout, stderr)
        }
        TeamCommand::Idp(TeamIdpCommand::Set(args)) => {
            handle_team_idp_set(cli, args, stdout, stderr)
        }
        TeamCommand::Idp(TeamIdpCommand::Device(args)) => {
            handle_team_idp_device(cli, args, stdout, stderr)
        }
        TeamCommand::Idp(TeamIdpCommand::Attest(args)) => {
            handle_team_idp_attest(cli, args, stdout, stderr)
        }
        TeamCommand::Leave(args) => handle_team_leave(cli, args, stdout, stderr),
        TeamCommand::Sync(args) => handle_team_sync(cli, args, stdout, stderr),
        TeamCommand::Pause(args) => handle_team_pause(cli, args, stdout, stderr),
        TeamCommand::Resume(args) => handle_team_resume(cli, args, stdout, stderr),
        TeamCommand::Activity(args) => handle_team_activity(cli, args, stdout, stderr),
        TeamCommand::Projects(TeamProjectsCommand::Share(args)) => {
            handle_team_projects_share(cli, args, stdout, stderr)
        }
        TeamCommand::Projects(TeamProjectsCommand::Adopt(args)) => {
            handle_team_projects_adopt(cli, args, stdout, stderr)
        }
        TeamCommand::Projects(TeamProjectsCommand::List(args)) => {
            handle_team_projects_list(cli, args, stdout, stderr)
        }
        TeamCommand::Fetch(TeamFetchCommand::Body(args)) => {
            handle_team_fetch_body(cli, args, stdout, stderr)
        }
        TeamCommand::Steward(TeamStewardCommand::RunOnce(args)) => {
            handle_team_steward_run_once(cli, args, stdout, stderr)
        }
        TeamCommand::Doctor(args) => handle_team_doctor(cli, args, stdout, stderr),
    }
}

fn handle_team_create<W, E>(
    cli: &Cli,
    args: &TeamCreateArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let workspace_path = cli.resolve_workspace();
    match create_local_team_with_store(
        &connection,
        &workspace_id,
        &args.name,
        &produced_at,
        Some(&workspace_path),
    ) {
        Ok(report) => {
            let human = format!(
                "Team {}: {}\n  team_id: {}\n  origin_node_id: {}\n  hello_port: {}\n  genesis: {}\nNext:\n  {}\n",
                if report.created {
                    "created"
                } else {
                    "already exists"
                },
                report.team.display_name,
                report.team.team_id,
                report.team.origin_node_id,
                report.team.hello_port,
                report.team.genesis_event_id,
                report.next_commands.join("\n  ")
            );
            write_team_report(cli, &report, &human, stdout)
        }
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to create team: {error}"),
                repair: Some("ee init --workspace . && ee migrate run --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_status<W, E>(
    cli: &Cli,
    args: &TeamStatusArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    match local_team_status(&connection) {
        Ok(report) => {
            let human = if report.teams.is_empty() {
                "No local team genesis recorded.\nNext:\n  ee team create --name \"<team>\" --workspace . --json\n"
                    .to_owned()
            } else {
                let mut lines = vec![format!("Teams: {}", report.team_count)];
                for team in &report.teams {
                    lines.push(format!(
                        "  {} ({}) port {} genesis {}",
                        team.display_name, team.team_id, team.hello_port, team.genesis_event_id
                    ));
                }
                if !report.members.is_empty() {
                    lines.push(format!("Members: {}", report.members.len()));
                    for member in &report.members {
                        lines.push(format!(
                            "  {} ({}) {} {}",
                            member.display_name,
                            member.member_id,
                            member.bound_via,
                            if member.is_self { "self" } else { "peer" }
                        ));
                    }
                }
                if !report.nodes.is_empty() {
                    lines.push(format!("Nodes: {}", report.nodes.len()));
                    for node in &report.nodes {
                        lines.push(format!(
                            "  {} gen {} {}",
                            node.node_id, node.signing_key_generation, node.state
                        ));
                    }
                }
                lines.join("\n") + "\n"
            };
            write_team_report(cli, &report, &human, stdout)
        }
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to read team status: {error}"),
                repair: Some("ee migrate run --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_invite<W, E>(
    cli: &Cli,
    args: &TeamInviteArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let expires_at = (chrono::Utc::now() + chrono::Duration::days(7)).to_rfc3339();
    let workspace_path = cli.resolve_workspace();
    match mint_team_invite_with_store(
        &connection,
        &args.endpoint,
        &produced_at,
        &expires_at,
        Some(&workspace_path),
    ) {
        Ok(mut report) => {
            if args.wait {
                let Some(bind) = crate::mesh::bootstrap_envelope::parse_live_peer_endpoint(
                    &report.endpoint,
                    report.hello_port,
                ) else {
                    return write_domain_error(
                        &DomainError::Storage {
                            message: "Invite wait needs a live TCP endpoint".to_owned(),
                            repair: Some(
                                "ee team invite --endpoint <ip-or-ip:port> --wait".to_owned(),
                            ),
                        },
                        cli.wants_json(),
                        stdout,
                        stderr,
                    );
                };
                let listener = match std::net::TcpListener::bind(bind) {
                    Ok(listener) => listener,
                    Err(error) => {
                        return write_domain_error(
                            &DomainError::Storage {
                                message: format!("Failed to bind invite waiter: {error}"),
                                repair: Some(
                                    "ee mesh hello-responder run --workspace .".to_owned(),
                                ),
                            },
                            cli.wants_json(),
                            stdout,
                            stderr,
                        );
                    }
                };
                match serve_one_bootstrap_join_with_store(
                    &connection,
                    &workspace_id,
                    &listener,
                    std::time::Duration::from_secs(300),
                    Some(&workspace_path),
                ) {
                    Ok(granted) => report.granted = Some(granted),
                    Err(error) => {
                        return write_domain_error(
                            &DomainError::Storage {
                                message: format!("Invite wait failed: {error}"),
                                repair: Some(
                                    "ee team join --invite <code> --workspace .".to_owned(),
                                ),
                            },
                            cli.wants_json(),
                            stdout,
                            stderr,
                        );
                    }
                }
            }
            let human = match &report.granted {
                Some(granted) => format!(
                    "Invite redeemed by join\n  invite_id: {}\n  team_id: {}\n  joiner recorded for {}\n  code: {}\n",
                    report.invite_id, granted.team_id, granted.display_name, report.invite_code
                ),
                None => format!(
                    "Invite minted for {}\n  invite_id: {}\n  endpoint: {}:{}\n  expires: {}\n  code: {}\nNext:\n  ee team join --invite <code> --workspace . --json\n  ee team invite --endpoint {} --wait --workspace .\n",
                    report.team_id,
                    report.invite_id,
                    report.endpoint,
                    report.hello_port,
                    report.expires_at,
                    report.invite_code,
                    report.endpoint
                ),
            };
            write_team_report(cli, &report, &human, stdout)
        }
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to mint team invite: {error}"),
                repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_revoke<W, E>(
    cli: &Cli,
    args: &TeamRevokeArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let revoked_at = chrono::Utc::now().to_rfc3339();
    if args.all_before_floor {
        return match revoke_team_invites_before_floor(&connection, &revoked_at) {
            Ok(revoked) => {
                let report = json!({
                    "schema": "ee.team.revoke.v1",
                    "command": "team revoke",
                    "allBeforeFloor": true,
                    "revokedCount": revoked,
                    "revokedAt": revoked_at,
                    "meshPrimitives": ["team_pending_invites.revoke", "team_invite_auth_floor"],
                });
                write_team_report(
                    cli,
                    &report,
                    &format!("{revoked} pending invite(s) below the authorization floor revoked\n"),
                    stdout,
                )
            }
            Err(error) => write_domain_error(
                &DomainError::Storage {
                    message: format!("Failed to revoke invites before the floor: {error}"),
                    repair: Some("ee team doctor --workspace . --json".to_owned()),
                },
                cli.wants_json(),
                stdout,
                stderr,
            ),
        };
    }
    let Some(invite_id) = args.invite_id.as_deref() else {
        return write_domain_error(
            &DomainError::Usage {
                message: "invite revoke requires --invite-id or --all-before-floor".to_owned(),
                repair: Some("ee team revoke --invite-id <id> --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        );
    };
    match revoke_team_invite(&connection, invite_id, &revoked_at) {
        Ok(true) => {
            let report = json!({
                "schema": "ee.team.revoke.v1",
                "command": "team revoke",
                "inviteId": invite_id,
                "revoked": true,
                "revokedAt": revoked_at,
                "meshPrimitives": ["team_pending_invites.revoke", "team_invite_auth_floor"],
            });
            write_team_report(
                cli,
                &report,
                &format!("Invite {invite_id} revoked\n"),
                stdout,
            )
        }
        Ok(false) => write_domain_error(
            &DomainError::Storage {
                message: format!("Invite {invite_id} is not pending"),
                repair: Some("ee team invite --endpoint <ip> --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to revoke invite: {error}"),
                repair: Some("ee team status --workspace . --json".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_share_history<W, E>(
    cli: &Cli,
    args: &TeamShareHistoryArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let workspace_path = cli.resolve_workspace();
    match share_team_history(
        &connection,
        &workspace_id,
        &produced_at,
        args.confirm,
        args.limit,
        Some(&workspace_path),
    ) {
        Ok(report) => {
            let human = if report.confirmed {
                format!(
                    "History projected: {} new, {} already shared\n  team_id: {}\n  consent: {}\n",
                    report.projected_count,
                    report.skipped_count,
                    report.team_id,
                    report.consent_hash
                )
            } else {
                format!(
                    "History preview: {} candidates ({} already shared)\n  team_id: {}\n  consent: {}\nNext:\n  ee team share history --confirm --workspace .\n",
                    report.candidate_count,
                    report.skipped_count,
                    report.team_id,
                    report.consent_hash
                )
            };
            write_team_report(cli, &report, &human, stdout)
        }
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to share team history: {error}"),
                repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_share_bodies<W, E>(
    cli: &Cli,
    args: &TeamShareBodiesArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let workspace_path = cli.resolve_workspace();
    let stdin_token = if args.token_stdin {
        match read_invite_code_from_stdin() {
            Ok(token) => Some(token),
            Err(error) => {
                return write_domain_error(
                    &DomainError::Usage {
                        message: format!("Failed to read body token from stdin: {error}"),
                        repair: Some(
                            "ee team share bodies --confirm --token-stdin --workspace .".to_owned(),
                        ),
                    },
                    cli.wants_json(),
                    stdout,
                    stderr,
                );
            }
        }
    } else {
        None
    };
    let token = stdin_token.as_deref().or(args.token.as_deref());
    match share_team_bodies_represented(
        &connection,
        &workspace_id,
        &produced_at,
        args.confirm,
        args.limit,
        Some(&workspace_path),
        args.issue_token,
        token,
        &args.representation,
    ) {
        Ok(report) => {
            let human = if report.confirmed {
                format!(
                    "Bodies published: {} new, {} already cached\n  team_id: {}\n  representation: {}\n  consent: {}\n",
                    report.published_count,
                    report.skipped_count,
                    report.team_id,
                    report.representation,
                    report.consent_hash
                )
            } else {
                format!(
                    "Body preview: {} candidates ({} already cached)\n  team_id: {}\n  representation: {}\n  consent: {}\nNext:\n  ee team share bodies --confirm --representation {} --workspace .\n",
                    report.candidate_count,
                    report.skipped_count,
                    report.team_id,
                    report.representation,
                    report.consent_hash,
                    report.representation
                )
            };
            write_team_report(cli, &report, &human, stdout)
        }
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to share team bodies: {error}"),
                repair: Some("ee team share history --workspace . --json".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_unshare_bodies<W, E>(
    cli: &Cli,
    args: &TeamUnshareBodiesArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    if !args.confirm {
        return write_domain_error(
            &DomainError::Storage {
                message: "Unshare bodies requires --confirm".to_owned(),
                repair: Some("ee team unshare bodies --confirm --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        );
    }
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    match unshare_team_bodies(&connection, &workspace_id, &produced_at) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Unshared {} body cache row(s) (bytes not erased)\n  team_id: {}\n",
                report.published_count, report.team_id
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to unshare team bodies: {error}"),
                repair: Some("ee team share bodies --workspace . --json".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_members_list<W, E>(
    cli: &Cli,
    args: &TeamMembersListArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    handle_team_status(
        cli,
        &TeamStatusArgs {
            database: args.database.clone(),
        },
        stdout,
        stderr,
    )
}

fn handle_team_members_remove<W, E>(
    cli: &Cli,
    args: &TeamMembersRemoveArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let workspace_path = cli.resolve_workspace();
    match remove_team_member(
        &connection,
        &workspace_id,
        &args.member_id,
        &produced_at,
        Some(&workspace_path),
    ) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Member {} now {}\n  team_id: {}\n",
                report.member_id, report.state, report.team_id
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to remove member: {error}"),
                repair: Some("ee team members list --workspace . --json".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_members_add_node<W, E>(
    cli: &Cli,
    args: &TeamMembersAddNodeArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let workspace_path = cli.resolve_workspace();
    match add_local_team_node(
        &connection,
        &workspace_id,
        &produced_at,
        Some(&workspace_path),
    ) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Added node {}\n  team_id: {}\n",
                report.origin_node_id, report.team_id
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to add node: {error}"),
                repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_members_rotate_key<W, E>(
    cli: &Cli,
    args: &TeamMembersRotateKeyArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let workspace_path = cli.resolve_workspace();
    match rotate_local_signing_key(&connection, &workspace_id, &produced_at, &workspace_path) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Rotated signing key for {}\n  {}\n",
                report.origin_node_id, report.state
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to rotate signing key: {error}"),
                repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_members_reconcile<W, E>(
    cli: &Cli,
    args: &TeamMembersReconcileArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    match reconcile_local_team_membership(&connection, &workspace_id) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Reconciled {}: {} addition(s), {} removal(s) from {} event(s)\n",
                report.team_id,
                report.applied_additions,
                report.applied_removals,
                report.inspected_events
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to reconcile membership: {error}"),
                repair: Some("ee team status --workspace . --json".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_leave<W, E>(
    cli: &Cli,
    args: &TeamLeaveArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    if !args.confirm {
        return write_domain_error(
            &DomainError::Storage {
                message: "Leave requires --confirm".to_owned(),
                repair: Some("ee team leave --confirm --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        );
    }
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let workspace_path = cli.resolve_workspace();
    match leave_local_team(
        &connection,
        &workspace_id,
        &produced_at,
        Some(&workspace_path),
    ) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!("Left team {}\n", report.team_id),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to leave team: {error}"),
                repair: Some("ee team status --workspace . --json".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_sync<W, E>(
    cli: &Cli,
    args: &TeamSyncArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    match any_local_team_paused(&connection) {
        Ok(true) => {
            return write_domain_error(
                &DomainError::Storage {
                    message: "Team is paused".to_owned(),
                    repair: Some("ee team resume --confirm --workspace .".to_owned()),
                },
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
        Ok(false) => {}
        Err(error) => {
            return write_domain_error(
                &DomainError::Storage {
                    message: format!("Failed to read team posture: {error}"),
                    repair: Some("ee team status --workspace . --json".to_owned()),
                },
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
    }
    super::mesh::handle_mesh_sync(
        cli,
        &super::mesh::MeshSyncArgs {
            database: args.database.clone(),
            once: true,
            cadence_ms: 0,
            peer_concurrency: 1,
            body_fetch_budget_bytes: 65_536,
            stale_read_window_ms: 5_000,
            time_budget_ms: 5_000,
        },
        stdout,
        stderr,
    )
}

fn handle_team_pause<W, E>(
    cli: &Cli,
    args: &TeamPauseArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    handle_team_posture(cli, args.database.as_deref(), true, stdout, stderr)
}

fn handle_team_resume<W, E>(
    cli: &Cli,
    args: &TeamResumeArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    if !args.confirm {
        return write_domain_error(
            &DomainError::Storage {
                message: "Resume requires --confirm".to_owned(),
                repair: Some("ee team resume --confirm --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        );
    }
    handle_team_posture(cli, args.database.as_deref(), false, stdout, stderr)
}

fn handle_team_activity<W, E>(
    cli: &Cli,
    args: &TeamActivityArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    match list_team_activity(&connection, &workspace_id, &args.as_of, args.limit) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Team activity {}: {} event(s) as-of {}\n",
                report.team_id, report.event_count, report.as_of
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to list team activity: {error}"),
                repair: Some("ee team status --workspace . --json".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_projects_share<W, E>(
    cli: &Cli,
    args: &TeamProjectsShareArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let workspace_path = cli.resolve_workspace();
    match share_team_project(
        &connection,
        &workspace_id,
        &args.name,
        &args.path,
        &produced_at,
        Some(&workspace_path),
    ) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "{} project {}\n  team_id: {}\n",
                if report.minted { "Shared" } else { "Existing" },
                report
                    .projects
                    .first()
                    .map(|project| project.project_id.as_str())
                    .unwrap_or("unknown"),
                report.team_id
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to share project: {error}"),
                repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_projects_adopt<W, E>(
    cli: &Cli,
    args: &TeamProjectsAdoptArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    match adopt_team_project(
        &connection,
        &args.project_id,
        &args.name,
        &args.path,
        &produced_at,
    ) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!("Adopted {}\n  path: {}\n", args.project_id, args.path),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to adopt project: {error}"),
                repair: Some("ee team projects list --workspace . --json".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_projects_list<W, E>(
    cli: &Cli,
    args: &TeamProjectsListArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    match list_team_projects(&connection) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Team projects {}: {} project(s)\n",
                report.team_id, report.project_count
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to list projects: {error}"),
                repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_fetch_body<W, E>(
    cli: &Cli,
    args: &TeamFetchBodyArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let workspace_path = cli.resolve_workspace();
    match fetch_local_team_body(&connection, &workspace_id, &workspace_path, &args.key) {
        Ok(report) => {
            let human = if report.body_hex.is_some() {
                format!(
                    "Fetched {} ({} bytes)\n",
                    report.body_cache_key, report.size_bytes
                )
            } else {
                format!(
                    "Body {} is {}\n",
                    report.body_cache_key, report.cache_status
                )
            };
            write_team_report(cli, &report, &human, stdout)
        }
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to fetch team body: {error}"),
                repair: Some("ee team share bodies --workspace . --json".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_steward_run_once<W, E>(
    cli: &Cli,
    args: &TeamStewardRunOnceArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let plan = match execute_team_steward_once(&connection) {
        Ok(plan) => plan,
        Err(error) => {
            return write_domain_error(
                &DomainError::Storage {
                    message: format!("Failed to run team steward: {error}"),
                    repair: Some("ee team status --workspace . --json".to_owned()),
                },
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
    };
    if !plan.ran_sync {
        return write_team_report(
            cli,
            &plan,
            &format!(
                "Steward {}: {} (sync skipped)\n  team_id: {}\n",
                plan.outcome, plan.reason, plan.team_id
            ),
            stdout,
        );
    }
    super::mesh::handle_mesh_sync(
        cli,
        &super::mesh::MeshSyncArgs {
            database: args.database.clone(),
            once: true,
            cadence_ms: 0,
            peer_concurrency: 1,
            body_fetch_budget_bytes: 65_536,
            stale_read_window_ms: 5_000,
            time_budget_ms: 5_000,
        },
        stdout,
        stderr,
    )
}

fn handle_team_idp_require<W, E>(
    cli: &Cli,
    args: &TeamIdpRequireArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    if !args.tailnet_attested {
        return write_domain_error(
            &DomainError::Usage {
                message: "ee team idp require needs --tailnet-attested".to_owned(),
                repair: Some(
                    "ee team idp require --tailnet-attested [--domain acme.com] --workspace ."
                        .to_owned(),
                ),
            },
            cli.wants_json(),
            stdout,
            stderr,
        );
    }
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let workspace_path = cli.resolve_workspace();
    match require_tailnet_attested(
        &connection,
        &workspace_id,
        args.domain.as_deref(),
        &produced_at,
        Some(&workspace_path),
    ) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Team IdP policy {} gen {}\n  team_id: {}\n  domain: {}\n",
                report.kind,
                report.policy_generation,
                report.team_id,
                report.allowed_domain.as_deref().unwrap_or("<any>")
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to require tailnet-attested identity: {error}"),
                repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_idp_status<W, E>(
    cli: &Cli,
    args: &TeamIdpStatusArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    match team_idp_status(&connection) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Team IdP policy {} gen {}\n  team_id: {}\n  domain: {}\n",
                report.kind,
                report.policy_generation,
                report.team_id,
                report.allowed_domain.as_deref().unwrap_or("<any>")
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to read team IdP policy: {error}"),
                repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_idp_set<W, E>(
    cli: &Cli,
    args: &TeamIdpSetArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let bytes = match std::fs::read(&args.discovery_json) {
        Ok(bytes) => bytes,
        Err(error) => {
            return write_domain_error(
                &DomainError::Usage {
                    message: format!("Failed to read discovery JSON: {error}"),
                    repair: Some(
                        "ee team idp set --issuer https://idp.example --client-id <id> --discovery-json <file> --workspace ."
                            .to_owned(),
                    ),
                },
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
    };
    let discovery = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return write_domain_error(
                &DomainError::Usage {
                    message: format!("Discovery JSON is malformed: {error}"),
                    repair: Some(
                        "provide a local OpenID discovery document; ee does not fetch it"
                            .to_owned(),
                    ),
                },
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
    };
    let set_at = chrono::Utc::now().to_rfc3339();
    match set_team_oidc_provider(
        &connection,
        &args.issuer,
        &args.client_id,
        &discovery,
        &set_at,
    ) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Team OIDC provider {} ({})\n  team_id: {}\n",
                report.issuer, report.capability, report.team_id
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to pin OIDC provider: {error}"),
                repair: Some(
                    "ee team idp set --issuer https://idp.example --client-id <id> --discovery-json <file> --workspace ."
                        .to_owned(),
                ),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_idp_device<W, E>(
    cli: &Cli,
    args: &TeamIdpDeviceArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let discovery = match read_json_file(&args.discovery_json) {
        Ok(value) => value,
        Err(error) => {
            return write_domain_error(&error, cli.wants_json(), stdout, stderr);
        }
    };
    let authorization = match read_json_file(&args.authorization_json) {
        Ok(value) => value,
        Err(error) => {
            return write_domain_error(&error, cli.wants_json(), stdout, stderr);
        }
    };
    if args.execute {
        let ca_bundle = args
            .ca_bundle
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        return match execute_team_idp_token_poll(
            &connection,
            &discovery,
            &authorization,
            &args.curl.to_string_lossy(),
            ca_bundle.as_deref(),
        ) {
            Ok(report) => write_team_report(
                cli,
                &report,
                &format!(
                    "Team device poll {}\n  uri: {}\n  exit: {}\n",
                    report.user_code, report.verification_uri, report.curl_exit_code
                ),
                stdout,
            ),
            Err(error) => write_domain_error(
                &DomainError::Storage {
                    message: format!("Failed to execute device poll: {error}"),
                    repair: Some(
                        "ee team idp set then ee team idp device --execute --workspace ."
                            .to_owned(),
                    ),
                },
                cli.wants_json(),
                stdout,
                stderr,
            ),
        };
    }
    match plan_team_idp_device(
        &connection,
        &discovery,
        &authorization,
        &args.curl.to_string_lossy(),
    ) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Team device ceremony {}\n  uri: {}\n  wait: {}s\n",
                report.user_code,
                report
                    .verification_uri_complete
                    .as_deref()
                    .unwrap_or(&report.verification_uri),
                report.first_wait_secs
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to plan device ceremony: {error}"),
                repair: Some(
                    "ee team idp device --discovery-json <file> --authorization-json <file> --workspace ."
                        .to_owned(),
                ),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_idp_attest<W, E>(
    cli: &Cli,
    args: &TeamIdpAttestArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let token = if args.id_token == "-" {
        match read_invite_code_from_stdin() {
            Ok(token) => token,
            Err(error) => {
                return write_domain_error(
                    &DomainError::Usage {
                        message: format!("Failed to read id token from stdin: {error}"),
                        repair: Some("ee team idp attest --id-token - --workspace .".to_owned()),
                    },
                    cli.wants_json(),
                    stdout,
                    stderr,
                );
            }
        }
    } else {
        args.id_token.clone()
    };
    let groups = args.groups.iter().map(String::as_str).collect::<Vec<_>>();
    let checked_at = chrono::Utc::now().to_rfc3339();
    let mut jwks = match args.jwks_json.as_ref() {
        Some(path) => match read_json_file(path) {
            Ok(value) => Some(value),
            Err(error) => {
                return write_domain_error(&error, cli.wants_json(), stdout, stderr);
            }
        },
        None => None,
    };
    if let Some(url) = args.jwks_url.as_deref() {
        let ca = args
            .ca_bundle
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        match crate::mesh::idp::fetch_jwks_document(
            &args.curl.to_string_lossy(),
            url,
            ca.as_deref(),
        ) {
            Ok(value) => jwks = Some(value),
            Err(error) => {
                return write_domain_error(
                    &DomainError::Usage {
                        message: format!("Failed to fetch JWKS: {error}"),
                        repair: Some(
                            "ee team idp attest --jwks-url https://idp.example/jwks --ca-bundle <pem> --workspace ."
                                .to_owned(),
                        ),
                    },
                    cli.wants_json(),
                    stdout,
                    stderr,
                );
            }
        }
    }
    match attest_local_id_token(
        &connection,
        token.trim(),
        &groups,
        &checked_at,
        jwks.as_ref(),
    ) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Team identity attested\n  member: {}\n  subject: {}\n",
                report.member_id, report.subject
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to attest identity: {error}"),
                repair: Some("ee team idp attest --id-token <jwt> --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn read_json_file(path: &std::path::Path) -> Result<serde_json::Value, DomainError> {
    let bytes = std::fs::read(path).map_err(|error| DomainError::Usage {
        message: format!("Failed to read {}: {error}", path.display()),
        repair: Some("pass a local JSON file; ee does not fetch IdP HTTP".to_owned()),
    })?;
    serde_json::from_slice(&bytes).map_err(|error| DomainError::Usage {
        message: format!("JSON is malformed: {error}"),
        repair: Some("pass a local JSON file; ee does not fetch IdP HTTP".to_owned()),
    })
}

fn handle_team_members_revalidate<W, E>(
    cli: &Cli,
    args: &TeamMembersRevalidateArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let report = probe_local_tailscale_for_team();
    let checked_at = chrono::Utc::now().to_rfc3339();
    match revalidate_team_identities(&connection, &report, &checked_at) {
        Ok(result) => write_team_report(
            cli,
            &result,
            &format!(
                "Team identity revalidate: {} checked, {} attested, {} suspended, {} missing\n  team_id: {}\n",
                result.checked, result.attested, result.suspended, result.missing, result.team_id
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to revalidate team identities: {error}"),
                repair: Some("ee team idp require --tailnet-attested --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn probe_local_tailscale_for_team() -> crate::core::tailscale_probe::TailscaleLocalReport {
    use crate::core::tailscale_probe::{
        SystemTailscaleCliProbeRunner, SystemTailscaleSocketProbeRunner, TailscaleCliProbeConfig,
        TailscaleSocketProbeConfig, probe_tailscale_local_with_runners,
    };
    let mut socket_config = TailscaleSocketProbeConfig::mesh_enabled();
    let mut cli_config = TailscaleCliProbeConfig::mesh_enabled();
    socket_config.platform_hint =
        crate::core::tailscale_probe::TailscalePlatform::parse(Some(std::env::consts::OS));
    cli_config.platform_hint = socket_config.platform_hint;
    let mut socket_runner = SystemTailscaleSocketProbeRunner;
    let mut cli_runner = SystemTailscaleCliProbeRunner;
    probe_tailscale_local_with_runners(
        &socket_config,
        &cli_config,
        &mut socket_runner,
        &mut cli_runner,
    )
}

fn handle_team_doctor<W, E>(
    cli: &Cli,
    args: &TeamDoctorArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let workspace_path = cli.resolve_workspace();
    match inspect_team_health(&connection, &workspace_id, Some(&workspace_path)) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Team doctor {}: {} check(s)\n",
                report.posture,
                report.checks.len()
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to inspect team health: {error}"),
                repair: Some("ee team status --workspace . --json".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_posture<W, E>(
    cli: &Cli,
    database: Option<&std::path::Path>,
    paused: bool,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, _) = match open_team_store(cli, database) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let updated_at = chrono::Utc::now().to_rfc3339();
    match set_local_team_paused(&connection, paused, &updated_at) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Team {} (generation {})\n  team_id: {}\n",
                if report.paused { "paused" } else { "resumed" },
                report.pause_generation,
                report.team_id
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to update team posture: {error}"),
                repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_join<W, E>(
    cli: &Cli,
    args: &TeamJoinArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (connection, workspace_id) = match open_team_store(cli, args.database.as_deref()) {
        Ok(opened) => opened,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let produced_at = chrono::Utc::now().to_rfc3339();
    let invite_code = if args.invite_stdin {
        match read_invite_code_from_stdin() {
            Ok(code) => code,
            Err(error) => {
                return write_domain_error(
                    &DomainError::Storage {
                        message: format!("Failed to read invite from stdin: {error}"),
                        repair: Some("ee team join --invite-stdin --workspace .".to_owned()),
                    },
                    cli.wants_json(),
                    stdout,
                    stderr,
                );
            }
        }
    } else {
        args.invite.clone().unwrap_or_default()
    };
    if invite_code.is_empty() {
        return write_domain_error(
            &DomainError::Storage {
                message: "Join needs --invite or --invite-stdin".to_owned(),
                repair: Some("ee team join --invite-stdin --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        );
    }
    let workspace_path = cli.resolve_workspace();
    match join_team_with_code_on_store(
        &connection,
        &workspace_id,
        &invite_code,
        &args.name,
        &produced_at,
        std::time::Duration::from_secs(10),
        Some(&workspace_path),
    ) {
        Ok(report) => {
            let human = format!(
                "Joined {}: {}\n  team_id: {}\n  origin_node_id: {}\nNext:\n  ee team status --workspace . --json\n  ee mesh sync --once --workspace . --json\n",
                report.team.display_name,
                if report.joined { "ok" } else { "already local" },
                report.team.team_id,
                report.team.origin_node_id
            );
            write_team_report(cli, &report, &human, stdout)
        }
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to join team: {error}"),
                repair: Some("ee mesh hello-responder run --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

#[cfg(unix)]
struct StdinEchoGuard;

#[cfg(unix)]
impl Drop for StdinEchoGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("stty").arg("echo").status();
    }
}

fn read_invite_code_from_stdin() -> Result<String, String> {
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
    #[cfg(unix)]
    let _guard = if tty {
        std::process::Command::new("stty")
            .arg("-echo")
            .status()
            .map_err(|error| format!("disable stdin echo: {error}"))?;
        Some(StdinEchoGuard)
    } else {
        None
    };
    let mut raw = String::new();
    std::io::stdin()
        .read_line(&mut raw)
        .map_err(|error| error.to_string())?;
    if tty {
        let _ = writeln!(std::io::stderr());
    }
    Ok(raw.trim().to_owned())
}

fn open_team_store(
    cli: &Cli,
    database: Option<&std::path::Path>,
) -> Result<(DbConnection, String), DomainError> {
    let workspace_path = cli.resolve_workspace();
    let database_path = database
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    if !database_path.exists() {
        return Err(DomainError::Storage {
            message: format!("Workspace store is missing at {}", database_path.display()),
            repair: Some("ee init --workspace .".to_owned()),
        });
    }
    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open team database: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let workspace_id = connection
        .get_workspace_by_path(&workspace_path.to_string_lossy())
        .ok()
        .flatten()
        .map(|workspace| workspace.id)
        .unwrap_or_else(|| super::stable_cli_workspace_id(&workspace_path));
    Ok((connection, workspace_id))
}

fn write_team_report<W, T>(
    cli: &Cli,
    report: &T,
    human_output: &str,
    stdout: &mut W,
) -> ProcessExitCode
where
    W: Write,
    T: serde::Serialize,
{
    match cli.renderer() {
        output::Renderer::Human | output::Renderer::Markdown => write_stdout(stdout, human_output),
        output::Renderer::Toon => {
            let data = match serde_json::to_value(report) {
                Ok(data) => data,
                Err(error) => {
                    return write_stdout(
                        stdout,
                        &format!("error: failed to serialize team report: {error}\n"),
                    );
                }
            };
            write_stdout(
                stdout,
                &(output::render_toon_from_json(&data.to_string()) + "\n"),
            )
        }
        output::Renderer::Json
        | output::Renderer::Jsonl
        | output::Renderer::Compact
        | output::Renderer::Hook => {
            let data = match serde_json::to_value(report) {
                Ok(data) => data,
                Err(error) => {
                    return write_stdout(
                        stdout,
                        &format!("error: failed to serialize team report: {error}\n"),
                    );
                }
            };
            let json = json!({
                "schema": crate::models::RESPONSE_SCHEMA_V2,
                "success": true,
                "data": data,
                "degraded": []
            });
            write_stdout(stdout, &(json.to_string() + "\n"))
        }
    }
}
