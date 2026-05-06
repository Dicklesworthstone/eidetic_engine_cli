#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use ee::obs::{LogEnvelope, LogLevel, now_rfc3339_nanos};
use serde_json::Value;

fn env_flag_truthy(value: Option<String>) -> bool {
    value.is_some_and(|raw| {
        let trimmed = raw.trim();
        !(trimmed.is_empty()
            || trimmed == "0"
            || trimmed.eq_ignore_ascii_case("false")
            || trimmed.eq_ignore_ascii_case("no")
            || trimmed.eq_ignore_ascii_case("off"))
    })
}

fn has_explicit_machine_output_flag(args: &[OsString]) -> bool {
    args.iter().skip(1).any(|arg| {
        arg.to_str().is_some_and(|value| {
            value == "--json"
                || value == "-j"
                || value == "--robot"
                || value == "--format"
                || value.starts_with("--format=")
        })
    })
}

fn should_inject_json_flag(args: &[OsString]) -> bool {
    let hook_mode = env_flag_truthy(std::env::var("EE_HOOK_MODE").ok());
    let agent_mode = env_flag_truthy(std::env::var("EE_AGENT_MODE").ok());
    (hook_mode || agent_mode) && !has_explicit_machine_output_flag(args)
}

fn env_value_is_json(value: Option<String>) -> bool {
    value.is_some_and(|raw| raw.trim().eq_ignore_ascii_case("json"))
}

fn json_log_enabled() -> bool {
    env_flag_truthy(std::env::var("EE_LOG_JSON").ok())
        || env_value_is_json(std::env::var("EE_LOG_FORMAT").ok())
}

fn inferred_command_name(args: &[OsString]) -> String {
    let mut skip_next = false;
    let mut inferred = None;
    for arg in args.iter().skip(1) {
        let Some(value) = arg.to_str() else {
            continue;
        };
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(
            value,
            "--workspace"
                | "--database"
                | "--config"
                | "--format"
                | "--schema-version"
                | "--fields"
        ) {
            skip_next = true;
            continue;
        }
        if value.starts_with("--workspace=")
            || value.starts_with("--database=")
            || value.starts_with("--config=")
            || value.starts_with("--format=")
            || value.starts_with("--schema-version=")
            || value.starts_with("--fields=")
            || value.starts_with('-')
        {
            continue;
        }
        inferred = Some(value);
        break;
    }
    inferred.unwrap_or("help").to_owned()
}

fn write_start_log<W: Write>(args: &[OsString], stderr: &mut W) {
    if !json_log_enabled() {
        return;
    }
    let envelope = LogEnvelope::new(now_rfc3339_nanos(), LogLevel::Info, "ee.cli")
        .with_field("event", Value::String("command_start".to_owned()))
        .with_field("command", Value::String(inferred_command_name(args)));
    let _ = envelope.write_to(stderr);
}

fn main() -> ExitCode {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    if should_inject_json_flag(&args) {
        args.insert(1, OsString::from("--json"));
    }
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    write_start_log(&args, &mut stderr);
    ee::cli::run(args, &mut stdout, &mut stderr).into()
}
