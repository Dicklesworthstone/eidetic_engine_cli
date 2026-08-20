//! Product-level `ee team` commands over mesh primitives.

use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;

use crate::db::DbConnection;
use crate::mesh::peer_state::MeshDriftThresholds;
use crate::mesh::team::{
    TeamInviteReport, TeamMemberRecord, TeamStatusReport, add_local_team_node, adopt_team_project,
    any_local_team_paused, attest_local_id_token, create_local_team_with_store,
    execute_team_idp_token_poll, execute_team_steward_once, fetch_local_team_body,
    inspect_team_health, inspect_team_port, join_team_with_code_on_store, leave_local_team,
    list_team_activity, list_team_projects, local_team_status, migrate_local_team_port,
    mint_team_invite_with_store, plan_team_idp_device, reconcile_local_team_membership,
    reconcile_local_team_projects, remove_team_member, require_tailnet_attested,
    resume_pending_invite, revalidate_team_identities, revoke_team_invite,
    revoke_team_invites_before_floor, rotate_local_signing_key,
    serve_one_bootstrap_join_with_store, serve_one_invite_first_sync, set_local_team_paused,
    set_team_oidc_provider, share_team_bodies_represented, share_team_history, share_team_project,
    team_idp_status, unshare_team_bodies,
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
    /// Encrypted pair-key and signing-seed backup.
    #[command(subcommand)]
    Credentials(TeamCredentialsCommand),
    /// Inspect or migrate the folded team hello port.
    #[command(subcommand)]
    Port(TeamPortCommand),
}

/// Nested `ee team port` verbs.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TeamPortCommand {
    /// Show the folded hello port without rewriting genesis.
    Show(TeamPortShowArgs),
    /// Append a versioned `teamPortMigrated` event and rewrite enrolled locators.
    Migrate(TeamPortMigrateArgs),
}

/// Nested `ee team credentials` verbs.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum TeamCredentialsCommand {
    /// Write an encrypted credential backup under the workspace keys tree.
    Backup(TeamCredentialsBackupArgs),
    /// Restore pair keys and signing seeds from an encrypted backup.
    Restore(TeamCredentialsRestoreArgs),
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
    /// Replay origin project shares onto local rows.
    Reconcile(TeamProjectsReconcileArgs),
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
    /// When omitted, the local Tailscale IPv4 address is used if present.
    #[arg(long)]
    pub endpoint: Option<String>,

    /// Bind the advertised locator and accept one join before exiting.
    #[arg(long)]
    pub wait: bool,

    /// Resume waiting on an existing pending invite without re-emitting the secret.
    #[arg(long, value_name = "INVITE_ID")]
    pub resume: Option<String>,

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

    /// Restrict to this member display name.
    #[arg(long)]
    pub member: Option<String>,

    /// Restrict to this team project display name.
    #[arg(long)]
    pub project: Option<String>,

    /// Inclusive lower bound. JSON requires RFC 3339. Human mode also
    /// accepts a relative duration such as `2h` or `7d`.
    #[arg(long)]
    pub since: Option<String>,

    /// Resume from an `ee.cursor.v1` token. Invalid/stale tokens yield
    /// an empty page plus `cursorError`.
    #[arg(long)]
    pub cursor: Option<String>,

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

/// Arguments for `ee team projects reconcile`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamProjectsReconcileArgs {
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

/// Arguments for `ee team port show`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamPortShowArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team port migrate`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamPortMigrateArgs {
    /// Next non-privileged hello port. Does not rewrite the genesis event.
    #[arg(long = "to", value_name = "PORT")]
    pub to: u16,

    /// Confirm appending `teamPortMigrated` and rewriting enrolled peer locators.
    #[arg(long)]
    pub confirm: bool,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee team credentials backup`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamCredentialsBackupArgs {
    /// Destination file or directory under the workspace. Defaults to
    /// `<workspace>/.ee/keys/mesh-credential-backup/credentials.backup.v1.json`.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Read the passphrase from stdin. Never accepted on argv.
    #[arg(long)]
    pub passphrase_stdin: bool,

    /// Replace an existing backup envelope at the destination.
    #[arg(long)]
    pub overwrite: bool,
}

/// Arguments for `ee team credentials restore`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct TeamCredentialsRestoreArgs {
    /// Encrypted backup envelope. May live outside the workspace.
    #[arg(long, value_name = "PATH")]
    pub input: PathBuf,

    /// Read the passphrase from stdin. Never accepted on argv.
    #[arg(long)]
    pub passphrase_stdin: bool,

    /// Replace existing pair-key and signing-seed slots.
    #[arg(long)]
    pub overwrite: bool,
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
        TeamCommand::Projects(TeamProjectsCommand::Reconcile(args)) => {
            handle_team_projects_reconcile(cli, args, stdout, stderr)
        }
        TeamCommand::Fetch(TeamFetchCommand::Body(args)) => {
            handle_team_fetch_body(cli, args, stdout, stderr)
        }
        TeamCommand::Steward(TeamStewardCommand::RunOnce(args)) => {
            handle_team_steward_run_once(cli, args, stdout, stderr)
        }
        TeamCommand::Doctor(args) => handle_team_doctor(cli, args, stdout, stderr),
        TeamCommand::Credentials(TeamCredentialsCommand::Backup(args)) => {
            handle_team_credentials_backup(cli, args, stdout, stderr)
        }
        TeamCommand::Credentials(TeamCredentialsCommand::Restore(args)) => {
            handle_team_credentials_restore(cli, args, stdout, stderr)
        }
        TeamCommand::Port(TeamPortCommand::Show(args)) => {
            handle_team_port_show(cli, args, stdout, stderr)
        }
        TeamCommand::Port(TeamPortCommand::Migrate(args)) => {
            handle_team_port_migrate(cli, args, stdout, stderr)
        }
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
            let as_of = chrono::Utc::now();
            let freshness = collect_team_member_freshness(&connection, &report.members, as_of);
            let human = render_team_status_human(&report, &freshness, as_of);
            match inject_team_member_freshness(&report, &freshness) {
                Ok(data) => write_team_report(cli, &data, &human, stdout),
                Err(error) => write_domain_error(
                    &DomainError::Storage {
                        message: format!("Failed to serialize team status: {error}"),
                        repair: Some("ee team status --workspace . --json".to_owned()),
                    },
                    cli.wants_json(),
                    stdout,
                    stderr,
                ),
            }
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

fn wait_for_invite_join<W, E>(
    cli: &Cli,
    connection: &crate::db::DbConnection,
    workspace_id: &str,
    workspace_path: &std::path::Path,
    mut report: TeamInviteReport,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let Some(bind) = crate::mesh::bootstrap_envelope::parse_live_peer_endpoint(
        &report.endpoint,
        report.hello_port,
    ) else {
        return write_domain_error(
            &DomainError::Storage {
                message: "Invite wait needs a live TCP endpoint".to_owned(),
                repair: Some("ee team invite --endpoint <ip-or-ip:port> --wait".to_owned()),
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
                    repair: Some("ee mesh hello-responder run --workspace .".to_owned()),
                },
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
    };
    match serve_one_bootstrap_join_with_store(
        connection,
        workspace_id,
        &listener,
        std::time::Duration::from_secs(300),
        Some(workspace_path),
    ) {
        Ok(granted) => report.granted = Some(granted),
        Err(error) => {
            return write_domain_error(
                &DomainError::Storage {
                    message: format!("Invite wait failed: {error}"),
                    repair: Some(
                        "ee team invite --wait --resume <invite-id> --workspace .".to_owned(),
                    ),
                },
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
    }
    if let Ok(_served) = serve_one_invite_first_sync(
        connection,
        workspace_id,
        &listener,
        std::time::Duration::from_secs(15),
    ) {
        report.first_sync_served = true;
        if !report
            .mesh_primitives
            .iter()
            .any(|item| *item == "mesh_sync")
        {
            report.mesh_primitives.push("mesh_sync");
        }
    }
    let human = match &report.granted {
        Some(granted) => format!(
            "Invite redeemed by join\n  invite_id: {}\n  team_id: {}\n  joiner recorded for {}\n  first_sync: {}\n",
            report.invite_id,
            granted.team_id,
            granted.display_name,
            if report.first_sync_served {
                "served"
            } else {
                "joiner did not fetch — start ee mesh hello-responder run"
            }
        ),
        None => format!(
            "Resumed invite waiter\n  invite_id: {}\n  endpoint: {}:{}\n",
            report.invite_id, report.endpoint, report.hello_port
        ),
    };
    write_team_report(cli, &report, &human, stdout)
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
    if let Some(invite_id) = args
        .resume
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        if !args.wait {
            return write_domain_error(
                &DomainError::Usage {
                    message: "invite --resume requires --wait".to_owned(),
                    repair: Some(
                        "ee team invite --wait --resume <invite-id> --workspace .".to_owned(),
                    ),
                },
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
        return match resume_pending_invite(&connection, invite_id) {
            Ok(report) => wait_for_invite_join(
                cli,
                &connection,
                &workspace_id,
                &workspace_path,
                report,
                stdout,
                stderr,
            ),
            Err(error) => write_domain_error(
                &DomainError::Storage {
                    message: format!("Failed to resume invite: {error}"),
                    repair: Some("ee team status --workspace . --json".to_owned()),
                },
                cli.wants_json(),
                stdout,
                stderr,
            ),
        };
    }
    let endpoint = match args
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(explicit) => explicit.to_owned(),
        None => match probe_local_tailscale_for_team().self_tailscale_ip {
            Some(ip) if !ip.is_empty() => ip,
            _ => {
                return write_domain_error(
                    &DomainError::Usage {
                        message: "invite needs --endpoint or a reachable Tailscale self IP"
                            .to_owned(),
                        repair: Some(
                            "ee team invite --endpoint <tailscale-ip> --workspace .".to_owned(),
                        ),
                    },
                    cli.wants_json(),
                    stdout,
                    stderr,
                );
            }
        },
    };
    match mint_team_invite_with_store(
        &connection,
        &endpoint,
        &produced_at,
        &expires_at,
        Some(&workspace_path),
    ) {
        Ok(report) => {
            if args.wait {
                return wait_for_invite_join(
                    cli,
                    &connection,
                    &workspace_id,
                    &workspace_path,
                    report,
                    stdout,
                    stderr,
                );
            }
            write_team_report(
                cli,
                &report,
                &format!(
                    "Invite minted for {}\n  invite_id: {}\n  endpoint: {}:{}\n  expires: {}\n  code: {}\nNext:\n  ee team join --invite <code> --workspace . --json\n  ee team invite --wait --resume {} --workspace .\n",
                    report.team_id,
                    report.invite_id,
                    report.endpoint,
                    report.hello_port,
                    report.expires_at,
                    report.invite_code,
                    report.invite_id
                ),
                stdout,
            )
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
            time_budget_ms: crate::mesh::foreground_cli::FOREGROUND_SYNC_TIME_BUDGET_MS,
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

fn resolve_team_activity_since(
    raw: Option<&str>,
    json: bool,
) -> Result<Option<String>, DomainError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if let Ok(stamp) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(Some(stamp.with_timezone(&chrono::Utc).to_rfc3339()));
    }
    if json {
        return Err(DomainError::Usage {
            message: "JSON --since must be an RFC 3339 timestamp.".to_owned(),
            repair: Some(
                "Use --since 2026-08-13T00:00:00Z. Relative durations such as 2h are human-only."
                    .to_owned(),
            ),
        });
    }
    let now = chrono::Utc::now();
    let resolved = parse_human_activity_since(raw, now)?;
    Ok(Some(resolved.to_rfc3339()))
}

fn parse_human_activity_since(
    raw: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<chrono::DateTime<chrono::Utc>, DomainError> {
    let trimmed = raw.trim().strip_prefix('+').unwrap_or(raw.trim());
    let usage = || DomainError::Usage {
        message: format!("since must be RFC 3339 or a relative duration such as 2h, not {raw:?}"),
        repair: Some("Use 2026-08-13T00:00:00Z, 2h, 30m, or 7d.".to_owned()),
    };
    let (amount, unit) = trimmed.split_at(trimmed.len().saturating_sub(1));
    let amount: i64 = amount.parse().map_err(|_| usage())?;
    if amount < 0 {
        return Err(usage());
    }
    let duration = match unit {
        "s" => chrono::Duration::seconds(amount),
        "m" => chrono::Duration::minutes(amount),
        "h" => chrono::Duration::hours(amount),
        "d" => chrono::Duration::days(amount),
        _ => return Err(usage()),
    };
    now.checked_sub_signed(duration).ok_or_else(usage)
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
    let since = match resolve_team_activity_since(args.since.as_deref(), cli.wants_json()) {
        Ok(since) => since,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    match list_team_activity(
        &connection,
        &workspace_id,
        &args.as_of,
        args.limit,
        args.member.as_deref(),
        args.project.as_deref(),
        since.as_deref(),
        args.cursor.as_deref(),
    ) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Team activity {}: {} event(s) as-of {}{}\n",
                report.team_id,
                report.event_count,
                report.as_of,
                report
                    .since
                    .as_deref()
                    .map(|since| format!(" since {since}"))
                    .unwrap_or_default()
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

fn handle_team_projects_reconcile<W, E>(
    cli: &Cli,
    args: &TeamProjectsReconcileArgs,
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
    match reconcile_local_team_projects(&connection) {
        Ok(report) => write_team_report(
            cli,
            &report,
            &format!(
                "Reconciled {} project row(s) from origin\n",
                report.applied_additions
            ),
            stdout,
        ),
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to reconcile projects: {error}"),
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
    let mut local =
        match fetch_local_team_body(&connection, &workspace_id, &workspace_path, &args.key) {
            Ok(report) => report,
            Err(error) => {
                return write_domain_error(
                    &DomainError::Storage {
                        message: format!("Failed to fetch team body: {error}"),
                        repair: Some("ee team share bodies --workspace . --json".to_owned()),
                    },
                    cli.wants_json(),
                    stdout,
                    stderr,
                );
            }
        };
    if local.body_hex.is_none() {
        let database = args
            .database
            .clone()
            .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
        let _ = crate::mesh::foreground_cli::fetch_pending_team_bodies_from_paths(
            &workspace_path,
            &database,
        );
        if let Ok(refreshed) =
            fetch_local_team_body(&connection, &workspace_id, &workspace_path, &args.key)
        {
            local = refreshed;
        }
    }
    let human = if local.body_hex.is_some() {
        format!(
            "Fetched {} ({} bytes)\n",
            local.body_cache_key, local.size_bytes
        )
    } else {
        format!("Body {} is {}\n", local.body_cache_key, local.cache_status)
    };
    write_team_report(cli, &local, &human, stdout)
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
    let workspace_path = cli.resolve_workspace();
    let plan = match execute_team_steward_once(&connection, Some(&workspace_path)) {
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
        let database = args
            .database
            .clone()
            .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
        let _ = crate::mesh::foreground_cli::fetch_pending_team_bodies_from_paths(
            &workspace_path,
            &database,
        );
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
            time_budget_ms: crate::mesh::foreground_cli::FOREGROUND_SYNC_TIME_BUDGET_MS,
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

fn handle_team_credentials_backup<W, E>(
    cli: &Cli,
    args: &TeamCredentialsBackupArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    if !args.passphrase_stdin {
        return write_domain_error(
            &DomainError::Usage {
                message: "Passphrase must be read from stdin via --passphrase-stdin.".to_owned(),
                repair: Some(
                    "printf '%s\\n' \"$PASSPHRASE\" | ee team credentials backup --passphrase-stdin --workspace ."
                        .to_owned(),
                ),
            },
            cli.wants_json(),
            stdout,
            stderr,
        );
    }
    let passphrase = match read_invite_code_from_stdin() {
        Ok(value) => value,
        Err(error) => {
            return write_domain_error(
                &DomainError::Usage {
                    message: format!("Failed to read passphrase from stdin: {error}"),
                    repair: Some(
                        "printf '%s\\n' \"$PASSPHRASE\" | ee team credentials backup --passphrase-stdin --workspace ."
                            .to_owned(),
                    ),
                },
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
    };
    let workspace_path = cli.resolve_workspace();
    let (output_dir, file_name) =
        match resolve_credential_backup_output(&workspace_path, args.output.as_deref()) {
            Ok(resolved) => resolved,
            Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
        };
    let created_at = chrono::Utc::now().to_rfc3339();
    match crate::mesh::credential_backup::backup_workspace_credentials(
        &workspace_path,
        &output_dir,
        &file_name,
        &passphrase,
        args.overwrite,
        &created_at,
    ) {
        Ok(report) => {
            let human = format!(
                "Credential backup written\n  path: {}\n  pair_slots: {}\n  signing_slots: {}\n  store_present: {}\n",
                report.path, report.pair_count, report.signing_count, report.store_present
            );
            write_team_report(cli, &report, &human, stdout)
        }
        Err(error) => write_domain_error(
            &credential_backup_domain_error(error),
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_credentials_restore<W, E>(
    cli: &Cli,
    args: &TeamCredentialsRestoreArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    if !args.passphrase_stdin {
        return write_domain_error(
            &DomainError::Usage {
                message: "Passphrase must be read from stdin via --passphrase-stdin.".to_owned(),
                repair: Some(
                    "printf '%s\\n' \"$PASSPHRASE\" | ee team credentials restore --input <path> --passphrase-stdin --workspace ."
                        .to_owned(),
                ),
            },
            cli.wants_json(),
            stdout,
            stderr,
        );
    }
    let passphrase = match read_invite_code_from_stdin() {
        Ok(value) => value,
        Err(error) => {
            return write_domain_error(
                &DomainError::Usage {
                    message: format!("Failed to read passphrase from stdin: {error}"),
                    repair: Some(
                        "printf '%s\\n' \"$PASSPHRASE\" | ee team credentials restore --input <path> --passphrase-stdin --workspace ."
                            .to_owned(),
                    ),
                },
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
    };
    let workspace_path = cli.resolve_workspace();
    let created_at = chrono::Utc::now().to_rfc3339();
    match crate::mesh::credential_backup::restore_workspace_credentials(
        &workspace_path,
        &args.input,
        &passphrase,
        args.overwrite,
        &created_at,
    ) {
        Ok(report) => {
            let human = format!(
                "Credential backup restored\n  path: {}\n  pair_slots: {}\n  signing_slots: {}\n  overwrite: {}\n",
                report.path, report.pair_count, report.signing_count, report.overwrite
            );
            write_team_report(cli, &report, &human, stdout)
        }
        Err(error) => write_domain_error(
            &credential_backup_domain_error(error),
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn resolve_credential_backup_output(
    workspace_path: &std::path::Path,
    output: Option<&std::path::Path>,
) -> Result<(PathBuf, String), DomainError> {
    let default_dir = crate::mesh::credential_backup::mesh_credential_backup_dir(workspace_path);
    let default_name =
        crate::mesh::credential_backup::DEFAULT_CREDENTIAL_BACKUP_FILE_NAME.to_owned();
    let Some(output) = output else {
        return Ok((default_dir, default_name));
    };
    let absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| DomainError::Usage {
                message: format!("Failed to resolve backup output path: {error}"),
                repair: Some("Pass an absolute --output path under the workspace.".to_owned()),
            })?
            .join(output)
    };
    let (dir, name) = if absolute.is_dir()
        || output
            .as_os_str()
            .to_string_lossy()
            .ends_with(std::path::MAIN_SEPARATOR)
    {
        (absolute, default_name)
    } else {
        let name = absolute
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| DomainError::Usage {
                message: "Backup output file name is not valid UTF-8.".to_owned(),
                repair: Some("Use a file name such as credentials.backup.v1.json.".to_owned()),
            })?
            .to_owned();
        let dir = absolute
            .parent()
            .map_or_else(|| workspace_path.to_path_buf(), PathBuf::from);
        (dir, name)
    };
    if dir.strip_prefix(workspace_path).is_err() {
        return Err(DomainError::Usage {
            message: format!(
                "Credential backup output {} is outside the workspace {}",
                dir.display(),
                workspace_path.display()
            ),
            repair: Some(
                "Write the encrypted envelope under the workspace (default: .ee/keys/mesh-credential-backup/), then copy the file if you need it elsewhere.".to_owned(),
            ),
        });
    }
    Ok((dir, name))
}

fn credential_backup_domain_error(
    error: crate::mesh::credential_backup::CredentialBackupError,
) -> DomainError {
    use crate::mesh::credential_backup::CredentialBackupError;
    match error {
        CredentialBackupError::Passphrase { message } => DomainError::Usage {
            message,
            repair: Some(
                "Use a passphrase of at least 12 characters on stdin via --passphrase-stdin."
                    .to_owned(),
            ),
        },
        CredentialBackupError::Conflict { message } => DomainError::Usage {
            message,
            repair: Some(
                "ee team credentials restore --input <path> --passphrase-stdin --overwrite --workspace ."
                    .to_owned(),
            ),
        },
        CredentialBackupError::Crypto { message }
        | CredentialBackupError::Malformed { message } => DomainError::Usage {
            message,
            repair: Some(
                "Confirm the passphrase and that the file is an ee.mesh.credentials.backup.v1 envelope."
                    .to_owned(),
            ),
        },
        CredentialBackupError::Io { path, message } => DomainError::Storage {
            message: format!("Credential backup I/O failed at {path}: {message}"),
            repair: Some("ee team doctor --workspace . --json".to_owned()),
        },
        CredentialBackupError::KeyStore(error) => DomainError::Storage {
            message: error.message(),
            repair: Some(error.repair()),
        },
    }
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

fn handle_team_port_show<W, E>(
    cli: &Cli,
    args: &TeamPortShowArgs,
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
    match inspect_team_port(&connection) {
        Ok(report) => {
            let previous = report
                .previous_hello_port
                .map_or_else(|| "none".to_owned(), |port| port.to_string());
            write_team_report(
                cli,
                &report,
                &format!(
                    "Team hello port\n  team_id: {}\n  current: {}\n  genesis: {}\n  previous: {previous}\n  generation: {}\n  genesis_event_hash: {}\n  configured: {}\n",
                    report.team_id,
                    report.current_hello_port,
                    report.genesis_hello_port,
                    report.port_generation,
                    report.genesis_event_hash,
                    report.configured_hello_port,
                ),
                stdout,
            )
        }
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to inspect team hello port: {error}"),
                repair: Some("ee team create --name \"<team>\" --workspace .".to_owned()),
            },
            cli.wants_json(),
            stdout,
            stderr,
        ),
    }
}

fn handle_team_port_migrate<W, E>(
    cli: &Cli,
    args: &TeamPortMigrateArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    if !args.confirm {
        return write_domain_error(
            &DomainError::Usage {
                message: "Port migrate requires --confirm".to_owned(),
                repair: Some(format!(
                    "ee team port migrate --to {} --confirm --workspace .",
                    args.to
                )),
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
    match migrate_local_team_port(
        &connection,
        &workspace_id,
        args.to,
        &produced_at,
        Some(workspace_path.as_path()),
    ) {
        Ok(report) => {
            let previous = report
                .previous_hello_port
                .map_or_else(|| "none".to_owned(), |port| port.to_string());
            write_team_report(
                cli,
                &report,
                &format!(
                    "Team hello port migrated\n  team_id: {}\n  current: {}\n  previous: {previous}\n  generation: {}\n  genesis_event_hash: {}\n  peer_endpoints_rewritten: {}\n  pair_keys_unchanged: {}\n  grants_unchanged: {}\nNext:\n  ee mesh hello-responder run --workspace .\n  # unset EE_MESH_HELLO_PORT if it still pins the previous port; --port and the env var win over the folded team port\n",
                    report.team_id,
                    report.current_hello_port,
                    report.port_generation,
                    report.genesis_event_hash,
                    report.peer_endpoints_rewritten,
                    report.pair_keys_unchanged,
                    report.grants_unchanged,
                ),
                stdout,
            )
        }
        Err(error) => write_domain_error(
            &DomainError::Storage {
                message: format!("Failed to migrate team hello port: {error}"),
                repair: Some(format!(
                    "ee team port show --workspace . --json; ee team port migrate --to {} --confirm --workspace .",
                    args.to
                )),
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
                "Joined {}: {}\n  team_id: {}\n  origin_node_id: {}\n  first_sync: {}\nNext:\n  ee team status --workspace . --json\n  ee mesh hello-responder run --workspace . --json\n  ee mesh sync --once --workspace . --json\n",
                report.team.display_name,
                if report.joined { "ok" } else { "already local" },
                report.team.team_id,
                report.team.origin_node_id,
                if report.first_sync.complete {
                    format!("{} events", report.first_sync.imported_events)
                } else {
                    "incomplete — run ee mesh sync --once".to_owned()
                }
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
    let workspace_id =
        crate::mesh::foreground_cli::resolve_store_workspace_id(&connection, &workspace_path)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to resolve team workspace: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            })?;
    Ok((connection, workspace_id))
}

const MEMBER_REACHABILITY_SELF: &str = "self";
const MEMBER_REACHABILITY_NEVER_SYNCED: &str = "never_synced";
const MEMBER_REACHABILITY_SYNCED: &str = "synced";
const MEMBER_REACHABILITY_SOFT_STALE: &str = "soft_stale";
const MEMBER_REACHABILITY_HARD_STALE: &str = "hard_stale";

#[derive(Clone, Debug, Eq, PartialEq)]
struct TeamMemberFreshness {
    last_seen_at: Option<String>,
    reachability: &'static str,
}

fn collect_team_member_freshness(
    connection: &DbConnection,
    members: &[TeamMemberRecord],
    as_of: chrono::DateTime<chrono::Utc>,
) -> Vec<TeamMemberFreshness> {
    let thresholds = MeshDriftThresholds::default();
    members
        .iter()
        .map(|member| {
            let last_seen_at = if member.is_self {
                None
            } else {
                latest_peer_last_seen(connection, &member.workspace_id, &member.origin_node_id)
            };
            TeamMemberFreshness {
                reachability: classify_team_member_reachability(
                    member.is_self,
                    last_seen_at.as_deref(),
                    as_of,
                    thresholds,
                ),
                last_seen_at,
            }
        })
        .collect()
}

fn latest_peer_last_seen(
    connection: &DbConnection,
    workspace_id: &str,
    origin_node_id: &str,
) -> Option<String> {
    let peers = connection.list_mesh_peers(workspace_id).ok()?;
    peers
        .into_iter()
        .filter(|peer| {
            peer.origin_node_id == origin_node_id && !peer.last_seen_at.trim().is_empty()
        })
        .max_by(|left, right| last_seen_ord(&left.last_seen_at, &right.last_seen_at))
        .map(|peer| peer.last_seen_at)
}

fn last_seen_ord(left: &str, right: &str) -> std::cmp::Ordering {
    match (parse_rfc3339_utc(left), parse_rfc3339_utc(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => left.cmp(right),
    }
}

fn classify_team_member_reachability(
    is_self: bool,
    last_seen_at: Option<&str>,
    as_of: chrono::DateTime<chrono::Utc>,
    thresholds: MeshDriftThresholds,
) -> &'static str {
    if is_self {
        return MEMBER_REACHABILITY_SELF;
    }
    let Some(seen) = last_seen_at.and_then(parse_rfc3339_utc) else {
        return MEMBER_REACHABILITY_NEVER_SYNCED;
    };
    let elapsed = as_of.signed_duration_since(seen).num_seconds();
    if elapsed < 0 {
        return MEMBER_REACHABILITY_SYNCED;
    }
    let elapsed = u64::try_from(elapsed).unwrap_or(u64::MAX);
    if elapsed >= thresholds.hard_stale_after_seconds {
        MEMBER_REACHABILITY_HARD_STALE
    } else if elapsed >= thresholds.soft_stale_after_seconds {
        MEMBER_REACHABILITY_SOFT_STALE
    } else {
        MEMBER_REACHABILITY_SYNCED
    }
}

fn parse_rfc3339_utc(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|stamp| stamp.with_timezone(&chrono::Utc))
}

fn render_team_status_human(
    report: &TeamStatusReport,
    freshness: &[TeamMemberFreshness],
    as_of: chrono::DateTime<chrono::Utc>,
) -> String {
    if report.teams.is_empty() {
        return "No local team genesis recorded.\nNext:\n  ee team create --name \"<team>\" --workspace . --json\n"
            .to_owned();
    }
    let mut lines = vec![format!("Teams: {}", report.team_count)];
    for team in &report.teams {
        lines.push(format!(
            "  {} ({}) port {} genesis {}",
            team.display_name, team.team_id, team.hello_port, team.genesis_event_id
        ));
    }
    if !report.members.is_empty() {
        lines.push(format!("Members: {}", report.members.len()));
        for (member, fresh) in report.members.iter().zip(freshness.iter()) {
            let role = if member.is_self { "self" } else { "peer" };
            let mut line = format!(
                "  {} ({}) {} {}",
                member.display_name, member.member_id, member.bound_via, role
            );
            if let Some(label) = human_member_freshness_label(
                fresh.reachability,
                fresh.last_seen_at.as_deref(),
                as_of,
            ) {
                line.push_str(" · ");
                line.push_str(&label);
            }
            lines.push(line);
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
}

fn human_member_freshness_label(
    reachability: &str,
    last_seen_at: Option<&str>,
    as_of: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    match reachability {
        MEMBER_REACHABILITY_SELF => None,
        MEMBER_REACHABILITY_NEVER_SYNCED => Some("never synced".to_owned()),
        MEMBER_REACHABILITY_SYNCED => Some(format!(
            "synced {} ago",
            human_age_since(last_seen_at, as_of)
        )),
        MEMBER_REACHABILITY_SOFT_STALE => {
            Some(format!("stale {}", human_age_since(last_seen_at, as_of)))
        }
        MEMBER_REACHABILITY_HARD_STALE => Some(format!(
            "unreachable {}",
            human_age_since(last_seen_at, as_of)
        )),
        _ => None,
    }
}

fn human_age_since(last_seen_at: Option<&str>, as_of: chrono::DateTime<chrono::Utc>) -> String {
    let Some(seen) = last_seen_at.and_then(parse_rfc3339_utc) else {
        return "unknown".to_owned();
    };
    let elapsed = as_of.signed_duration_since(seen).num_seconds().max(0);
    let elapsed = u64::try_from(elapsed).unwrap_or(0);
    if elapsed < 60 {
        format!("{elapsed}s")
    } else if elapsed < 3_600 {
        format!("{}m", elapsed / 60)
    } else if elapsed < 86_400 {
        format!("{}h", elapsed / 3_600)
    } else {
        format!("{}d", elapsed / 86_400)
    }
}

fn inject_team_member_freshness(
    report: &TeamStatusReport,
    freshness: &[TeamMemberFreshness],
) -> Result<serde_json::Value, serde_json::Error> {
    let mut data = serde_json::to_value(report)?;
    let Some(members) = data
        .get_mut("members")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return Ok(data);
    };
    for (member, fresh) in members.iter_mut().zip(freshness.iter()) {
        let Some(object) = member.as_object_mut() else {
            continue;
        };
        if let Some(last_seen_at) = fresh.last_seen_at.as_deref() {
            object.insert("lastSeenAt".to_owned(), json!(last_seen_at));
        }
        object.insert("reachability".to_owned(), json!(fresh.reachability));
    }
    Ok(data)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        CreateWorkspaceInput, DbConnection, InsertTeamMemberInput, UpsertMeshPeerInput,
    };
    use crate::mesh::team::create_local_team;

    fn open_db() -> DbConnection {
        let connection = DbConnection::open_memory().expect("open");
        connection.migrate().expect("migrate");
        connection
    }

    #[test]
    fn classify_self_and_age_windows() {
        let as_of = parse_rfc3339_utc("2026-08-13T01:00:00Z").expect("as_of");
        let thresholds = MeshDriftThresholds::default();
        assert_eq!(
            classify_team_member_reachability(
                true,
                Some("2026-08-13T00:00:00Z"),
                as_of,
                thresholds
            ),
            MEMBER_REACHABILITY_SELF
        );
        assert_eq!(
            classify_team_member_reachability(false, None, as_of, thresholds),
            MEMBER_REACHABILITY_NEVER_SYNCED
        );
        assert_eq!(
            classify_team_member_reachability(
                false,
                Some("2026-08-13T00:59:00Z"),
                as_of,
                thresholds
            ),
            MEMBER_REACHABILITY_SYNCED
        );
        assert_eq!(
            classify_team_member_reachability(
                false,
                Some("2026-08-13T00:50:00Z"),
                as_of,
                thresholds
            ),
            MEMBER_REACHABILITY_SOFT_STALE
        );
        assert_eq!(
            classify_team_member_reachability(
                false,
                Some("2026-08-12T23:00:00Z"),
                as_of,
                thresholds
            ),
            MEMBER_REACHABILITY_HARD_STALE
        );
        assert_eq!(
            human_member_freshness_label(
                MEMBER_REACHABILITY_HARD_STALE,
                Some("2026-08-10T01:00:00Z"),
                as_of
            )
            .as_deref(),
            Some("unreachable 3d")
        );
        assert_eq!(
            human_member_freshness_label(
                MEMBER_REACHABILITY_SYNCED,
                Some("2026-08-13T00:56:00Z"),
                as_of
            )
            .as_deref(),
            Some("synced 4m ago")
        );
    }

    #[test]
    fn team_status_human_names_peer_sync_freshness() {
        let connection = open_db();
        connection
            .insert_workspace(
                "wsp_statusfresh000000000000001",
                &CreateWorkspaceInput {
                    path: "/tmp/ee-team-status-fresh".to_owned(),
                    name: Some("status-fresh".to_owned()),
                },
            )
            .expect("workspace");
        let created = create_local_team(
            &connection,
            "wsp_statusfresh000000000000001",
            "Analysts",
            "2026-08-13T00:00:00Z",
        )
        .expect("create");
        connection
            .insert_team_member(&InsertTeamMemberInput {
                member_id: "mbr_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
                team_id: created.team.team_id.clone(),
                workspace_id: "wsp_statusfresh000000000000001".to_owned(),
                display_name: "Priya".to_owned(),
                state: "active".to_owned(),
                is_self: false,
                origin_node_id: "node_priya00000000000000000001".to_owned(),
                bound_via: "invite_ceremony".to_owned(),
                joined_at: "2026-08-13T00:56:00Z".to_owned(),
            })
            .expect("priya");
        connection
            .upsert_mesh_peer(&UpsertMeshPeerInput {
                workspace_id: "wsp_statusfresh000000000000001".to_owned(),
                peer_id: "peer_priyafresh000000000000001".to_owned(),
                origin_node_id: "node_priya00000000000000000001".to_owned(),
                display_name: Some("Priya".to_owned()),
                policy_summary_json: None,
                enabled: true,
                last_seen_at: Some("2026-08-13T00:56:00Z".to_owned()),
            })
            .expect("priya peer");
        connection
            .insert_team_member(&InsertTeamMemberInput {
                member_id: "mbr_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                team_id: created.team.team_id.clone(),
                workspace_id: "wsp_statusfresh000000000000001".to_owned(),
                display_name: "Marcus".to_owned(),
                state: "active".to_owned(),
                is_self: false,
                origin_node_id: "node_marcus0000000000000000001".to_owned(),
                bound_via: "invite_ceremony".to_owned(),
                joined_at: "2026-08-13T00:00:00Z".to_owned(),
            })
            .expect("marcus");
        connection
            .upsert_mesh_peer(&UpsertMeshPeerInput {
                workspace_id: "wsp_statusfresh000000000000001".to_owned(),
                peer_id: "peer_stalehana0000000000000001".to_owned(),
                origin_node_id: "node_hanaold000000000000000001".to_owned(),
                display_name: Some("Hana-laptop".to_owned()),
                policy_summary_json: None,
                enabled: true,
                last_seen_at: Some("2026-08-10T01:00:00Z".to_owned()),
            })
            .expect("unused peer");

        let report = local_team_status(&connection).expect("status");
        let as_of = parse_rfc3339_utc("2026-08-13T01:00:00Z").expect("as_of");
        let freshness = collect_team_member_freshness(&connection, &report.members, as_of);
        let human = render_team_status_human(&report, &freshness, as_of);
        let data = inject_team_member_freshness(&report, &freshness).expect("json");

        let self_member = report.members.iter().find(|m| m.is_self).expect("self");
        let priya = report
            .members
            .iter()
            .find(|m| m.display_name == "Priya")
            .expect("priya");
        let marcus = report
            .members
            .iter()
            .find(|m| m.display_name == "Marcus")
            .expect("marcus");
        let by_id = |id: &str| {
            freshness
                .iter()
                .zip(report.members.iter())
                .find(|(_, member)| member.member_id == id)
                .map(|(fresh, _)| fresh)
                .expect("fresh")
        };

        assert_eq!(
            by_id(&self_member.member_id).reachability,
            MEMBER_REACHABILITY_SELF
        );
        assert_eq!(
            by_id(&priya.member_id).reachability,
            MEMBER_REACHABILITY_SYNCED
        );
        assert_eq!(
            by_id(&priya.member_id).last_seen_at.as_deref(),
            Some("2026-08-13T00:56:00Z")
        );
        assert_eq!(
            by_id(&marcus.member_id).reachability,
            MEMBER_REACHABILITY_NEVER_SYNCED
        );
        assert!(
            human.contains("Priya") && human.contains("synced 4m ago"),
            "human must name Priya's last sync: {human}"
        );
        assert!(
            human.contains("Marcus") && human.contains("never synced"),
            "human must say Marcus never synced: {human}"
        );
        assert!(
            !human.contains("unreachable"),
            "an unused old peer must not label the local operator unreachable: {human}"
        );

        let members = data["members"].as_array().expect("members");
        let priya_json = members
            .iter()
            .find(|row| row["displayName"] == "Priya")
            .expect("priya json");
        assert_eq!(priya_json["reachability"], "synced");
        assert_eq!(priya_json["lastSeenAt"], "2026-08-13T00:56:00Z");
        let marcus_json = members
            .iter()
            .find(|row| row["displayName"] == "Marcus")
            .expect("marcus json");
        assert_eq!(marcus_json["reachability"], "never_synced");
        assert!(marcus_json.get("lastSeenAt").is_none());
    }
}
