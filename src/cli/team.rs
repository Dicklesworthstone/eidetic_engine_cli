//! Product-level `ee team` commands over mesh primitives.

use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::json;

use crate::db::DbConnection;
use crate::mesh::team::{create_local_team, local_team_status};
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
