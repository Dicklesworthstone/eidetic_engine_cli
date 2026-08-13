//! Product-level `ee team` commands over mesh primitives.

use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;

use crate::db::DbConnection;
use crate::mesh::team::{
    create_local_team, join_team_with_code, local_team_status, mint_team_invite,
    serve_one_bootstrap_join,
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
    #[arg(long)]
    pub invite: String,

    /// Display name the inviter should record for this node.
    #[arg(long, default_value = "joiner")]
    pub name: String,

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
    match create_local_team(&connection, &workspace_id, &args.name, &produced_at) {
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
    match mint_team_invite(&connection, &args.endpoint, &produced_at, &expires_at) {
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
                match serve_one_bootstrap_join(
                    &connection,
                    &workspace_id,
                    &listener,
                    std::time::Duration::from_secs(300),
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
    match join_team_with_code(
        &connection,
        &workspace_id,
        &args.invite,
        &args.name,
        &produced_at,
        std::time::Duration::from_secs(10),
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
