use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};

use crate::cass::{CassClient, CassImportOptions, import_cass_sessions};
use crate::core::capabilities::CapabilitiesReport;
use crate::core::check::CheckReport;
use crate::core::doctor::DoctorReport;
use crate::core::status::StatusReport;
use crate::models::{DomainError, ProcessExitCode};
use crate::output;

#[derive(Clone, Debug, Parser, PartialEq)]
#[command(
    name = "ee",
    version,
    about = "Durable, local-first, explainable memory for coding agents.",
    disable_colored_help = true,
    color = clap::ColorChoice::Never,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Emit JSON output for commands that support machine output.
    #[arg(long, short = 'j', global = true, action = ArgAction::SetTrue)]
    pub json: bool,

    /// Workspace root to operate on.
    #[arg(long, global = true, value_name = "PATH")]
    pub workspace: Option<PathBuf>,

    /// Disable colored diagnostics.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub no_color: bool,

    /// Use agent-oriented output defaults; currently implies JSON where supported.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub robot: bool,

    /// Select the output renderer for commands that support multiple formats.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Human)]
    pub format: OutputFormat,

    /// Control the verbosity level of output fields.
    #[arg(long, global = true, value_enum, default_value_t = FieldsLevel::Standard)]
    pub fields: FieldsLevel,

    /// Print the JSON schema for the response envelope and exit.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub schema: bool,

    /// Print JSON-formatted help and exit.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub help_json: bool,

    /// Print agent-oriented documentation and exit.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub agent_docs: bool,

    /// Include additional metadata in the response envelope.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub meta: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    #[must_use]
    pub const fn wants_json(&self) -> bool {
        self.json || self.robot || self.format.is_machine_readable()
    }

    #[must_use]
    pub const fn renderer(&self) -> output::Renderer {
        match self.format {
            OutputFormat::Human if self.json || self.robot => output::Renderer::Json,
            OutputFormat::Human => output::Renderer::Human,
            OutputFormat::Json => output::Renderer::Json,
            OutputFormat::Toon => output::Renderer::Toon,
            OutputFormat::Jsonl => output::Renderer::Jsonl,
            OutputFormat::Compact => output::Renderer::Compact,
            OutputFormat::Hook => output::Renderer::Hook,
        }
    }

    #[must_use]
    pub const fn fields_level(&self) -> FieldsLevel {
        self.fields
    }

    #[must_use]
    pub const fn wants_meta(&self) -> bool {
        self.meta
    }
}

#[derive(Clone, Debug, PartialEq, Subcommand)]
pub enum Command {
    /// Report feature availability, commands, and subsystem status.
    Capabilities,
    /// Quick posture summary: ready, degraded, or needs attention.
    Check,
    /// Run health checks on workspace and subsystems.
    Doctor,
    /// Run evaluation scenarios against fixtures.
    #[command(subcommand)]
    Eval(EvalCommand),
    /// Print command help.
    Help,
    /// Import memories and evidence from external sources.
    #[command(subcommand)]
    Import(ImportCommand),
    /// Store a new memory.
    Remember(RememberArgs),
    /// List or export public response schemas.
    #[command(subcommand)]
    Schema(SchemaCommand),
    /// Report workspace and subsystem readiness.
    Status,
    /// Print the ee version.
    Version,
}

/// Arguments for the remember command.
#[derive(Clone, Debug, Parser, PartialEq)]
pub struct RememberArgs {
    /// Memory content to store.
    #[arg(value_name = "CONTENT")]
    pub content: String,

    /// Memory level (working, episodic, semantic, procedural).
    #[arg(long, short = 'l', default_value = "episodic")]
    pub level: String,

    /// Memory kind (rule, fact, decision, failure, etc.).
    #[arg(long, short = 'k', default_value = "fact")]
    pub kind: String,

    /// Tags to apply (comma-separated).
    #[arg(long, short = 't')]
    pub tags: Option<String>,

    /// Confidence score (0.0 to 1.0).
    #[arg(long, default_value = "0.8")]
    pub confidence: f32,

    /// Source provenance URI (e.g., file://path:line).
    #[arg(long)]
    pub source: Option<String>,

    /// Perform a dry run without storing.
    #[arg(long, action = ArgAction::SetTrue)]
    pub dry_run: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum ImportCommand {
    /// Import sessions and evidence from coding_agent_session_search.
    Cass(CassImportArgs),
}

/// Arguments for `ee import cass`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct CassImportArgs {
    /// Maximum sessions to import from CASS.
    #[arg(long, default_value_t = 10)]
    pub limit: u32,

    /// Database path to write. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Query CASS and render the import plan without writing storage.
    #[arg(long, action = ArgAction::SetTrue)]
    pub dry_run: bool,

    /// Skip first-window evidence span capture through `cass view`.
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_spans: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum EvalCommand {
    /// Run one or more evaluation scenarios.
    Run {
        /// Scenario ID to run (e.g., "usr_pre_task_brief"). Omit for all scenarios.
        #[arg(value_name = "SCENARIO_ID")]
        scenario_id: Option<String>,
        /// Path to fixture directory (defaults to tests/fixtures/eval/).
        #[arg(long, value_name = "PATH")]
        fixture_dir: Option<std::path::PathBuf>,
    },
    /// List available evaluation scenarios.
    List,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum SchemaCommand {
    /// List all available public schemas.
    List,
    /// Export a schema's JSON Schema definition.
    Export {
        /// Schema ID to export (e.g., "ee.response.v1"). Omit for all schemas.
        #[arg(value_name = "SCHEMA_ID")]
        schema_id: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
    Toon,
    Jsonl,
    Compact,
    Hook,
}

impl OutputFormat {
    #[must_use]
    pub const fn is_machine_readable(self) -> bool {
        matches!(self, Self::Json | Self::Jsonl | Self::Compact | Self::Hook)
    }

    #[must_use]
    pub const fn to_renderer(self) -> output::Renderer {
        match self {
            Self::Human => output::Renderer::Human,
            Self::Json => output::Renderer::Json,
            Self::Toon => output::Renderer::Toon,
            Self::Jsonl => output::Renderer::Jsonl,
            Self::Compact => output::Renderer::Compact,
            Self::Hook => output::Renderer::Hook,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum FieldsLevel {
    Minimal,
    Summary,
    #[default]
    Standard,
    Full,
}

impl FieldsLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Summary => "summary",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }
}

pub fn run_from_env() -> ProcessExitCode {
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    run(std::env::args_os(), &mut stdout, &mut stderr)
}

pub fn run<I, W, E>(args: I, stdout: &mut W, stderr: &mut E) -> ProcessExitCode
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    let args: Vec<OsString> = args.into_iter().collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => return write_parse_error(error, &args, stdout, stderr),
    };

    if cli.schema {
        return write_stdout(stdout, &(output::schema_json() + "\n"));
    }
    if cli.help_json {
        return write_stdout(stdout, &(output::help_json() + "\n"));
    }
    if cli.agent_docs {
        return write_stdout(stdout, &(output::agent_docs() + "\n"));
    }

    match cli.command {
        None | Some(Command::Help) => write_help(stdout),
        Some(Command::Capabilities) => {
            let report = CapabilitiesReport::gather();
            match cli.renderer() {
                output::Renderer::Human => {
                    write_stdout(stdout, &output::render_capabilities_human(&report))
                }
                output::Renderer::Toon => {
                    write_stdout(stdout, &(output::render_capabilities_toon(&report) + "\n"))
                }
                output::Renderer::Json
                | output::Renderer::Jsonl
                | output::Renderer::Compact
                | output::Renderer::Hook => {
                    write_stdout(stdout, &(output::render_capabilities_json(&report) + "\n"))
                }
            }
        }
        Some(Command::Check) => {
            let report = CheckReport::gather();
            match cli.renderer() {
                output::Renderer::Human => {
                    write_stdout(stdout, &output::render_check_human(&report))
                }
                output::Renderer::Toon => {
                    write_stdout(stdout, &(output::render_check_toon(&report) + "\n"))
                }
                output::Renderer::Json
                | output::Renderer::Jsonl
                | output::Renderer::Compact
                | output::Renderer::Hook => {
                    write_stdout(stdout, &(output::render_check_json(&report) + "\n"))
                }
            }
        }
        Some(Command::Doctor) => {
            let report = DoctorReport::gather();
            match cli.renderer() {
                output::Renderer::Human => {
                    write_stdout(stdout, &output::render_doctor_human(&report))
                }
                output::Renderer::Toon => {
                    write_stdout(stdout, &(output::render_doctor_toon(&report) + "\n"))
                }
                output::Renderer::Json
                | output::Renderer::Jsonl
                | output::Renderer::Compact
                | output::Renderer::Hook => {
                    write_stdout(stdout, &(output::render_doctor_json(&report) + "\n"))
                }
            }
        }
        Some(Command::Eval(ref eval_cmd)) => match eval_cmd {
            EvalCommand::Run { scenario_id, .. } => match cli.renderer() {
                output::Renderer::Human => write_stdout(
                    stdout,
                    &output::render_eval_run_human(scenario_id.as_deref()),
                ),
                output::Renderer::Toon => write_stdout(
                    stdout,
                    &(output::render_eval_run_toon(scenario_id.as_deref()) + "\n"),
                ),
                output::Renderer::Json
                | output::Renderer::Jsonl
                | output::Renderer::Compact
                | output::Renderer::Hook => write_stdout(
                    stdout,
                    &(output::render_eval_run_json(scenario_id.as_deref()) + "\n"),
                ),
            },
            EvalCommand::List => match cli.renderer() {
                output::Renderer::Human => write_stdout(stdout, &output::render_eval_list_human()),
                output::Renderer::Toon => {
                    write_stdout(stdout, &(output::render_eval_list_toon() + "\n"))
                }
                output::Renderer::Json
                | output::Renderer::Jsonl
                | output::Renderer::Compact
                | output::Renderer::Hook => {
                    write_stdout(stdout, &(output::render_eval_list_json() + "\n"))
                }
            },
        },
        Some(Command::Import(ImportCommand::Cass(ref args))) => {
            handle_import_cass(&cli, args, stdout, stderr)
        }
        Some(Command::Schema(ref schema_cmd)) => match schema_cmd {
            SchemaCommand::List => match cli.renderer() {
                output::Renderer::Human => {
                    write_stdout(stdout, &output::render_schema_list_human())
                }
                output::Renderer::Toon => {
                    write_stdout(stdout, &(output::render_schema_list_toon() + "\n"))
                }
                output::Renderer::Json
                | output::Renderer::Jsonl
                | output::Renderer::Compact
                | output::Renderer::Hook => {
                    write_stdout(stdout, &(output::render_schema_list_json() + "\n"))
                }
            },
            SchemaCommand::Export { schema_id } => match cli.renderer() {
                output::Renderer::Human => write_stdout(
                    stdout,
                    &output::render_schema_export_human(schema_id.as_deref()),
                ),
                output::Renderer::Toon => write_stdout(
                    stdout,
                    &(output::render_schema_export_toon(schema_id.as_deref()) + "\n"),
                ),
                output::Renderer::Json
                | output::Renderer::Jsonl
                | output::Renderer::Compact
                | output::Renderer::Hook => write_stdout(
                    stdout,
                    &(output::render_schema_export_json(schema_id.as_deref()) + "\n"),
                ),
            },
        },
        Some(Command::Remember(ref args)) => {
            let result = handle_remember(args, cli.wants_json());
            match cli.renderer() {
                output::Renderer::Human => write_stdout(stdout, &result.human_output()),
                output::Renderer::Toon => write_stdout(stdout, &(result.toon_output() + "\n")),
                output::Renderer::Json
                | output::Renderer::Jsonl
                | output::Renderer::Compact
                | output::Renderer::Hook => write_stdout(stdout, &(result.json_output() + "\n")),
            }
        }
        Some(Command::Status) => {
            let report = StatusReport::gather();
            match cli.renderer() {
                output::Renderer::Human => {
                    write_stdout(stdout, &output::render_status_human(&report))
                }
                output::Renderer::Toon => {
                    write_stdout(stdout, &(output::render_status_toon(&report) + "\n"))
                }
                output::Renderer::Json
                | output::Renderer::Jsonl
                | output::Renderer::Compact
                | output::Renderer::Hook => {
                    write_stdout(stdout, &(output::render_status_json(&report) + "\n"))
                }
            }
        }
        Some(Command::Version) => {
            write_stdout(stdout, concat!("ee ", env!("CARGO_PKG_VERSION"), "\n"))
        }
    }
}

fn write_stdout<W>(stdout: &mut W, text: &str) -> ProcessExitCode
where
    W: Write,
{
    match stdout.write_all(text.as_bytes()) {
        Ok(()) => ProcessExitCode::Success,
        Err(_) => ProcessExitCode::Usage,
    }
}

fn write_help<W>(stdout: &mut W) -> ProcessExitCode
where
    W: Write,
{
    let mut command = Cli::command();
    match command
        .write_help(stdout)
        .and_then(|()| stdout.write_all(b"\n"))
    {
        Ok(()) => ProcessExitCode::Success,
        Err(_) => ProcessExitCode::Usage,
    }
}

fn args_request_json<I>(args: I) -> bool
where
    I: IntoIterator,
    I::Item: AsRef<std::ffi::OsStr>,
{
    let mut prev_was_format = false;
    for arg in args {
        let s = arg.as_ref().to_string_lossy();
        if prev_was_format {
            if s == "json" {
                return true;
            }
            prev_was_format = false;
            continue;
        }
        if s == "--json" || s == "-j" || s == "--robot" {
            return true;
        }
        if s == "--format" {
            prev_was_format = true;
        } else if s.starts_with("--format=") && s.ends_with("json") {
            return true;
        }
    }
    false
}

fn write_parse_error<I, W, E>(
    error: clap::Error,
    args: I,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    I: IntoIterator,
    I::Item: AsRef<std::ffi::OsStr>,
    W: Write,
    E: Write,
{
    match error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            let _ = stdout.write_all(error.to_string().as_bytes());
            ProcessExitCode::Success
        }
        _ => {
            let domain_error = DomainError::Usage {
                message: clap_error_message(&error),
                repair: Some("ee --help".to_string()),
            };
            if args_request_json(args) {
                let json = output::error_response_json(&domain_error);
                let _ = stdout.write_all(json.as_bytes());
                let _ = stdout.write_all(b"\n");
            } else {
                let _ = stderr.write_all(error.to_string().as_bytes());
            }
            domain_error.exit_code()
        }
    }
}

fn clap_error_message(error: &clap::Error) -> String {
    let full = error.to_string();
    full.lines()
        .find(|line| line.starts_with("error:"))
        .map(|line| line.trim_start_matches("error:").trim().to_string())
        .unwrap_or_else(|| full.lines().next().unwrap_or("Unknown error").to_string())
}

fn handle_import_cass<W, E>(
    cli: &Cli,
    args: &CassImportArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let workspace_path = cli.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
    let options = CassImportOptions {
        workspace_path,
        database_path: args.database.clone(),
        limit: args.limit,
        dry_run: args.dry_run,
        include_spans: !args.no_spans,
    };

    match import_cass_sessions(&CassClient::new_default(), &options) {
        Ok(report) => match cli.renderer() {
            output::Renderer::Human => write_stdout(stdout, &report.human_summary()),
            output::Renderer::Toon => write_stdout(
                stdout,
                &format!(
                    "IMPORT_CASS|{}|{}|{}|{}\n",
                    report.status,
                    report.sessions_discovered,
                    report.sessions_imported,
                    report.spans_imported
                ),
            ),
            output::Renderer::Json
            | output::Renderer::Jsonl
            | output::Renderer::Compact
            | output::Renderer::Hook => {
                let json = serde_json::json!({
                    "schema": crate::models::RESPONSE_SCHEMA_V1,
                    "success": true,
                    "data": report.data_json(),
                });
                write_stdout(stdout, &(json.to_string() + "\n"))
            }
        },
        Err(error) => {
            let domain_error = DomainError::Import {
                message: error.to_string(),
                repair: error.repair_hint().map(str::to_string),
            };
            write_domain_error(&domain_error, cli.wants_json(), stdout, stderr)
        }
    }
}

fn write_domain_error<W, E>(
    error: &DomainError,
    wants_json: bool,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    if wants_json {
        let _ = stdout.write_all(output::error_response_json(error).as_bytes());
        let _ = stdout.write_all(b"\n");
    } else {
        let _ = writeln!(stderr, "error: {}", error.message());
        if let Some(repair) = error.repair() {
            let _ = writeln!(stderr, "\nNext:\n  {repair}");
        }
    }
    error.exit_code()
}

#[derive(Clone, Debug, PartialEq)]
pub struct RememberResult {
    pub memory_id: String,
    pub content: String,
    pub level: String,
    pub kind: String,
    pub confidence: f32,
    pub tags: Vec<String>,
    pub source: Option<String>,
    pub dry_run: bool,
}

impl RememberResult {
    #[must_use]
    pub fn human_output(&self) -> String {
        if self.dry_run {
            format!(
                "DRY RUN: Would store {} memory ({})\n  Content: {}\n  Confidence: {:.2}\n",
                self.level, self.kind, self.content, self.confidence
            )
        } else {
            format!(
                "Stored {} memory ({}): {}\n  ID: {}\n",
                self.level, self.kind, self.content, self.memory_id
            )
        }
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        if self.dry_run {
            format!("DRY_RUN|{}|{}|{}", self.level, self.kind, self.content)
        } else {
            format!("STORED|{}|{}|{}", self.memory_id, self.level, self.kind)
        }
    }

    #[must_use]
    pub fn json_output(&self) -> String {
        let tags_json = self
            .tags
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let source_json = self
            .source
            .as_ref()
            .map_or("null".to_string(), |s| format!("\"{s}\""));

        format!(
            r#"{{"schema":"ee.response.v1","data":{{"memory_id":"{}","content":"{}","level":"{}","kind":"{}","confidence":{},"tags":[{}],"source":{},"dry_run":{}}}}}"#,
            self.memory_id,
            self.content.replace('"', "\\\""),
            self.level,
            self.kind,
            self.confidence,
            tags_json,
            source_json,
            self.dry_run
        )
    }
}

fn handle_remember(args: &RememberArgs, _wants_json: bool) -> RememberResult {
    let tags: Vec<String> = args
        .tags
        .as_ref()
        .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let memory_id = if args.dry_run {
        "mem_00000000000000000000".to_string()
    } else {
        format!(
            "mem_{:020}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                % 100_000_000_000_000_000_000u128
        )
    };

    RememberResult {
        memory_id,
        content: args.content.clone(),
        level: args.level.clone(),
        kind: args.kind.clone(),
        confidence: args.confidence,
        tags,
        source: args.source.clone(),
        dry_run: args.dry_run,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fmt::Debug;

    use clap::Parser;

    use super::{Cli, Command, FieldsLevel, OutputFormat, run};
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

    fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
        ensure(
            haystack.contains(needle),
            format!("{context}: expected output to contain {needle:?}, got {haystack:?}"),
        )
    }

    fn ensure_starts_with(haystack: &str, prefix: &str, context: &str) -> TestResult {
        ensure(
            haystack.starts_with(prefix),
            format!("{context}: expected output to start with {prefix:?}, got {haystack:?}"),
        )
    }

    fn ensure_ends_with(haystack: &str, suffix: char, context: &str) -> TestResult {
        ensure(
            haystack.ends_with(suffix),
            format!("{context}: expected output to end with {suffix:?}, got {haystack:?}"),
        )
    }

    fn invoke(args: &[&str]) -> (ProcessExitCode, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = run(args.iter().map(OsString::from), &mut stdout, &mut stderr);
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        (exit, stdout, stderr)
    }

    #[test]
    fn parser_accepts_global_json_before_or_after_command() -> TestResult {
        let before = Cli::try_parse_from(["ee", "--json", "status"])
            .map(|cli| (cli.json, cli.command))
            .map_err(|error| format!("failed to parse --json before status: {:?}", error.kind()))?;
        let after = Cli::try_parse_from(["ee", "status", "--json"])
            .map(|cli| (cli.json, cli.command))
            .map_err(|error| format!("failed to parse --json after status: {:?}", error.kind()))?;

        ensure_equal(
            &before,
            &(true, Some(Command::Status)),
            "--json before status parse",
        )?;
        ensure_equal(
            &after,
            &(true, Some(Command::Status)),
            "--json after status parse",
        )
    }

    #[test]
    fn parser_accepts_robot_and_format_globals() -> TestResult {
        let parsed = Cli::try_parse_from(["ee", "status", "--robot", "--format", "json"])
            .map(|cli| (cli.robot, cli.format, cli.wants_json()))
            .map_err(|error| format!("failed to parse robot format globals: {:?}", error.kind()))?;

        ensure_equal(
            &parsed,
            &(true, OutputFormat::Json, true),
            "robot and format globals",
        )
    }

    #[test]
    fn parser_accepts_workspace_and_no_color_globals() -> TestResult {
        let parsed = Cli::try_parse_from(["ee", "--workspace", ".", "status", "--no-color"])
            .map(|cli| {
                (
                    cli.workspace.as_deref() == Some(std::path::Path::new(".")),
                    cli.no_color,
                    cli.command,
                )
            })
            .map_err(|error| {
                format!(
                    "failed to parse workspace/no-color globals: {:?}",
                    error.kind()
                )
            })?;

        ensure_equal(
            &parsed,
            &(true, true, Some(Command::Status)),
            "workspace and no-color globals",
        )
    }

    #[test]
    fn status_json_writes_machine_data_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "status", "--json"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "status JSON exit")?;
        ensure_starts_with(
            &stdout,
            "{\"schema\":\"ee.response.v1\"",
            "status JSON schema",
        )?;
        ensure_ends_with(&stdout, '\n', "status JSON trailing newline")?;
        ensure(stderr.is_empty(), "status JSON stderr must be empty")
    }

    #[test]
    fn status_format_json_writes_machine_data_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "status", "--format", "json"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "status format JSON exit")?;
        ensure_starts_with(
            &stdout,
            "{\"schema\":\"ee.response.v1\"",
            "status format JSON schema",
        )?;
        ensure(stderr.is_empty(), "status format JSON stderr must be empty")
    }

    #[test]
    fn status_format_toon_writes_toon_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "status", "--format", "toon"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "status format TOON exit")?;
        ensure_starts_with(&stdout, "schema: ee.response.v1", "status TOON schema")?;
        ensure_contains(&stdout, "command: status", "status TOON command")?;
        ensure_contains(
            &stdout,
            "degraded[2]{code,severity,message,repair}:",
            "status TOON degradation table",
        )?;
        ensure_ends_with(&stdout, '\n', "status TOON trailing newline")?;
        ensure(stderr.is_empty(), "status format TOON stderr must be empty")
    }

    #[test]
    fn bare_invocation_writes_help_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "bare invocation exit")?;
        ensure_contains(&stdout, "Usage:", "bare invocation usage")?;
        ensure_contains(&stdout, "status", "bare invocation status subcommand")?;
        ensure(stderr.is_empty(), "bare invocation stderr must be empty")
    }

    #[test]
    fn help_subcommand_writes_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "help"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "help subcommand exit")?;
        ensure_contains(&stdout, "Usage:", "help subcommand usage")?;
        ensure_contains(&stdout, "status", "help subcommand status")?;
        ensure(stderr.is_empty(), "help subcommand stderr must be empty")
    }

    #[test]
    fn clap_help_writes_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--help"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "help exit")?;
        ensure_contains(&stdout, "Usage:", "help usage line")?;
        ensure_contains(&stdout, "status", "help status subcommand")?;
        ensure(stderr.is_empty(), "help stderr must be empty")
    }

    #[test]
    fn version_subcommand_writes_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "version"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "version subcommand exit")?;
        ensure_equal(
            &stdout.as_str(),
            &concat!("ee ", env!("CARGO_PKG_VERSION"), "\n"),
            "version subcommand stdout",
        )?;
        ensure(stderr.is_empty(), "version subcommand stderr must be empty")
    }

    #[test]
    fn version_flag_writes_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--version"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "version flag exit")?;
        ensure_equal(
            &stdout.as_str(),
            &concat!("ee ", env!("CARGO_PKG_VERSION"), "\n"),
            "version flag stdout",
        )?;
        ensure(stderr.is_empty(), "version flag stderr must be empty")
    }

    #[test]
    fn unknown_command_writes_diagnostics_to_stderr_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "unknown"]);
        ensure_equal(&exit, &ProcessExitCode::Usage, "unknown command exit")?;
        ensure(stdout.is_empty(), "unknown command stdout must be empty")?;
        ensure_contains(
            &stderr,
            "error: unrecognized subcommand",
            "unknown command diagnostic",
        )
    }

    #[test]
    fn unknown_command_with_json_flag_writes_json_error_to_stdout() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--json", "unknown"]);
        ensure_equal(&exit, &ProcessExitCode::Usage, "json unknown exit")?;
        ensure_starts_with(&stdout, "{\"schema\":\"ee.error.v1\"", "json error schema")?;
        ensure_contains(&stdout, "\"code\":\"usage\"", "json error code")?;
        ensure(stderr.is_empty(), "json error stderr must be empty")
    }

    #[test]
    fn unknown_command_with_robot_flag_writes_json_error_to_stdout() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--robot", "unknown"]);
        ensure_equal(&exit, &ProcessExitCode::Usage, "robot unknown exit")?;
        ensure_starts_with(&stdout, "{\"schema\":\"ee.error.v1\"", "robot error schema")?;
        ensure_contains(&stdout, "\"code\":\"usage\"", "robot error code")?;
        ensure(stderr.is_empty(), "robot error stderr must be empty")
    }

    #[test]
    fn unknown_command_with_format_json_writes_json_error_to_stdout() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--format", "json", "unknown"]);
        ensure_equal(&exit, &ProcessExitCode::Usage, "format json unknown exit")?;
        ensure_starts_with(
            &stdout,
            "{\"schema\":\"ee.error.v1\"",
            "format json error schema",
        )?;
        ensure_contains(&stdout, "\"code\":\"usage\"", "format json error code")?;
        ensure(stderr.is_empty(), "format json error stderr must be empty")
    }

    #[test]
    fn json_error_contains_repair_hint() -> TestResult {
        let (_, stdout, _) = invoke(&["ee", "-j", "badcmd"]);
        ensure_contains(
            &stdout,
            "\"repair\":\"ee --help\"",
            "json error repair hint",
        )
    }

    #[test]
    fn schema_flag_outputs_json_schema_info() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--schema"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "schema exit")?;
        ensure_starts_with(&stdout, "{\"schema\":\"ee.response.v1\"", "schema envelope")?;
        ensure_contains(&stdout, "\"command\":\"schema\"", "schema command field")?;
        ensure(stderr.is_empty(), "schema stderr must be empty")
    }

    #[test]
    fn help_json_flag_outputs_json_help() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--help-json"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "help-json exit")?;
        ensure_starts_with(
            &stdout,
            "{\"schema\":\"ee.response.v1\"",
            "help-json envelope",
        )?;
        ensure_contains(&stdout, "\"command\":\"help\"", "help-json command field")?;
        ensure(stderr.is_empty(), "help-json stderr must be empty")
    }

    #[test]
    fn agent_docs_flag_outputs_documentation() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--agent-docs"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "agent-docs exit")?;
        ensure_starts_with(
            &stdout,
            "{\"schema\":\"ee.response.v1\"",
            "agent-docs envelope",
        )?;
        ensure_contains(
            &stdout,
            "\"command\":\"agent-docs\"",
            "agent-docs command field",
        )?;
        ensure(stderr.is_empty(), "agent-docs stderr must be empty")
    }

    #[test]
    fn parser_accepts_fields_flag() -> TestResult {
        let parsed = Cli::try_parse_from(["ee", "--fields", "minimal", "status"])
            .map(|cli| cli.fields)
            .map_err(|error| format!("failed to parse fields flag: {:?}", error.kind()))?;
        ensure_equal(&parsed, &FieldsLevel::Minimal, "fields minimal")
    }

    #[test]
    fn parser_accepts_all_fields_levels() -> TestResult {
        for (arg, expected) in [
            ("minimal", FieldsLevel::Minimal),
            ("summary", FieldsLevel::Summary),
            ("standard", FieldsLevel::Standard),
            ("full", FieldsLevel::Full),
        ] {
            let parsed = Cli::try_parse_from(["ee", "--fields", arg, "status"])
                .map(|cli| cli.fields)
                .map_err(|error| format!("failed to parse --fields {arg}: {:?}", error.kind()))?;
            ensure_equal(&parsed, &expected, &format!("fields {arg}"))?;
        }
        Ok(())
    }

    #[test]
    fn parser_accepts_meta_flag() -> TestResult {
        let parsed = Cli::try_parse_from(["ee", "--meta", "status"])
            .map(|cli| cli.meta)
            .map_err(|error| format!("failed to parse meta flag: {:?}", error.kind()))?;
        ensure_equal(&parsed, &true, "meta flag")
    }

    #[test]
    fn parser_accepts_all_format_values() -> TestResult {
        for (arg, expected) in [
            ("human", OutputFormat::Human),
            ("json", OutputFormat::Json),
            ("toon", OutputFormat::Toon),
            ("jsonl", OutputFormat::Jsonl),
            ("compact", OutputFormat::Compact),
            ("hook", OutputFormat::Hook),
        ] {
            let parsed = Cli::try_parse_from(["ee", "--format", arg, "status"])
                .map(|cli| cli.format)
                .map_err(|error| format!("failed to parse --format {arg}: {:?}", error.kind()))?;
            ensure_equal(&parsed, &expected, &format!("format {arg}"))?;
        }
        Ok(())
    }

    #[test]
    fn format_is_machine_readable_classification() -> TestResult {
        ensure(
            !OutputFormat::Human.is_machine_readable(),
            "human not machine",
        )?;
        ensure(
            !OutputFormat::Toon.is_machine_readable(),
            "toon not machine",
        )?;
        ensure(OutputFormat::Json.is_machine_readable(), "json is machine")?;
        ensure(
            OutputFormat::Jsonl.is_machine_readable(),
            "jsonl is machine",
        )?;
        ensure(
            OutputFormat::Compact.is_machine_readable(),
            "compact is machine",
        )?;
        ensure(OutputFormat::Hook.is_machine_readable(), "hook is machine")
    }

    #[test]
    fn fields_level_as_str_returns_stable_names() -> TestResult {
        ensure_equal(&FieldsLevel::Minimal.as_str(), &"minimal", "minimal")?;
        ensure_equal(&FieldsLevel::Summary.as_str(), &"summary", "summary")?;
        ensure_equal(&FieldsLevel::Standard.as_str(), &"standard", "standard")?;
        ensure_equal(&FieldsLevel::Full.as_str(), &"full", "full")
    }

    // ========================================================================
    // Stream Isolation Tests (EE-019)
    //
    // These tests enforce the CLI output contract:
    // - stdout: machine data only (JSON responses, human output, help text)
    // - stderr: diagnostics only (errors in human mode, progress, debugging)
    //
    // When --json/--robot/--format=json is set, errors go to stdout as JSON.
    // When human mode is active, errors go to stderr as plain text.
    // ========================================================================

    #[test]
    fn stream_isolation_human_status_writes_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "status"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "human status exit")?;
        ensure(!stdout.is_empty(), "human status stdout has content")?;
        ensure(stderr.is_empty(), "human status stderr must be empty")
    }

    #[test]
    fn stream_isolation_human_help_writes_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "help"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "human help exit")?;
        ensure_contains(&stdout, "Usage:", "human help contains usage")?;
        ensure(stderr.is_empty(), "human help stderr must be empty")
    }

    #[test]
    fn stream_isolation_human_version_writes_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "version"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "human version exit")?;
        ensure_contains(&stdout, env!("CARGO_PKG_VERSION"), "version in stdout")?;
        ensure(stderr.is_empty(), "human version stderr must be empty")
    }

    #[test]
    fn stream_isolation_human_error_writes_to_stderr_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "not-a-command"]);
        ensure_equal(&exit, &ProcessExitCode::Usage, "human error exit")?;
        ensure(stdout.is_empty(), "human error stdout must be empty")?;
        ensure(!stderr.is_empty(), "human error stderr has diagnostic")
    }

    #[test]
    fn stream_isolation_json_error_writes_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--json", "not-a-command"]);
        ensure_equal(&exit, &ProcessExitCode::Usage, "json error exit")?;
        ensure_starts_with(
            &stdout,
            "{\"schema\":\"ee.error.v1\"",
            "json error envelope",
        )?;
        ensure(stderr.is_empty(), "json error stderr must be empty")
    }

    #[test]
    fn stream_isolation_robot_error_writes_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--robot", "badcmd"]);
        ensure_equal(&exit, &ProcessExitCode::Usage, "robot error exit")?;
        ensure_starts_with(
            &stdout,
            "{\"schema\":\"ee.error.v1\"",
            "robot error envelope",
        )?;
        ensure(stderr.is_empty(), "robot error stderr must be empty")
    }

    #[test]
    fn stream_isolation_format_json_error_writes_to_stdout_only() -> TestResult {
        let (exit, stdout, stderr) = invoke(&["ee", "--format=json", "badcmd"]);
        ensure_equal(&exit, &ProcessExitCode::Usage, "format json error exit")?;
        ensure_starts_with(
            &stdout,
            "{\"schema\":\"ee.error.v1\"",
            "format json error env",
        )?;
        ensure(stderr.is_empty(), "format json error stderr must be empty")
    }

    #[test]
    fn stream_isolation_meta_flags_with_json_writes_to_stdout_only() -> TestResult {
        for flag in &["--schema", "--help-json", "--agent-docs"] {
            let (exit, stdout, stderr) = invoke(&["ee", flag]);
            #[allow(clippy::needless_borrows_for_generic_args)]
            {
                ensure_equal(&exit, &ProcessExitCode::Success, &format!("{flag} exit"))?;
                ensure_starts_with(
                    &stdout,
                    "{\"schema\":\"ee.response.v1\"",
                    &format!("{flag} envelope"),
                )?;
                ensure(stderr.is_empty(), &format!("{flag} stderr must be empty"))?;
            }
        }
        Ok(())
    }

    #[test]
    fn stream_isolation_contract_documentation() -> TestResult {
        // This test documents the stream isolation contract for agent consumers.
        //
        // stdout receives:
        // - JSON responses (ee.response.v1 envelope) in machine mode
        // - JSON errors (ee.error.v1 envelope) when --json/--robot/--format=json is set
        // - Human-readable output (help, status, version) in human mode
        //
        // stderr receives:
        // - Diagnostic errors in human mode (parse failures, unknown commands)
        // - Progress indicators (when TTY is attached, future feature)
        // - Debug/trace output (when --verbose is set, future feature)
        //
        // Agents should:
        // 1. Always pass --json, --robot, or --format=json to get structured output
        // 2. Parse stdout for data, ignore stderr unless exit code is non-zero
        // 3. Check exit codes to detect error conditions
        //
        // The contract is stable: stdout is data, stderr is diagnostics.

        // Verify success output goes to stdout
        let (exit, stdout, stderr) = invoke(&["ee", "--json", "status"]);
        ensure_equal(&exit, &ProcessExitCode::Success, "doc: success exit")?;
        ensure(!stdout.is_empty(), "doc: success stdout has data")?;
        ensure(stderr.is_empty(), "doc: success stderr empty")?;

        // Verify error output in json mode goes to stdout
        let (exit, stdout, stderr) = invoke(&["ee", "--json", "badcmd"]);
        ensure_equal(&exit, &ProcessExitCode::Usage, "doc: error exit")?;
        ensure(!stdout.is_empty(), "doc: error stdout has json")?;
        ensure(stderr.is_empty(), "doc: error stderr empty in json mode")?;

        // Verify error output in human mode goes to stderr
        let (exit, stdout, stderr) = invoke(&["ee", "badcmd"]);
        ensure_equal(&exit, &ProcessExitCode::Usage, "doc: human error exit")?;
        ensure(stdout.is_empty(), "doc: human error stdout empty")?;
        ensure(!stderr.is_empty(), "doc: human error stderr has diagnostic")
    }
}
