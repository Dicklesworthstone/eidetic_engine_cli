#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use ee::config::env_registry::{EnvVar, read};
use ee::obs::{LogEnvelope, LogLevel, now_rfc3339_nanos};
use serde_json::Value;
use tracing_subscriber::EnvFilter;

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
    let hook_mode = env_flag_truthy(read(EnvVar::HookMode));
    let agent_mode = env_flag_truthy(read(EnvVar::AgentMode));
    (hook_mode || agent_mode) && !has_explicit_machine_output_flag(args)
}

fn env_value_is_json(value: Option<String>) -> bool {
    value.is_some_and(|raw| raw.trim().eq_ignore_ascii_case("json"))
}

fn json_log_enabled() -> bool {
    env_flag_truthy(read(EnvVar::LogJson)) || env_value_is_json(read(EnvVar::LogFormat))
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

fn tracing_env_filter_from_env(raw: Option<String>) -> EnvFilter {
    match raw {
        Some(value) if !value.trim().is_empty() => {
            EnvFilter::try_new(value).unwrap_or_else(|_| EnvFilter::new("off"))
        }
        _ => EnvFilter::new("off"),
    }
}

fn init_tracing_subscriber() {
    let filter = tracing_env_filter_from_env(std::env::var("RUST_LOG").ok());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .json()
        .try_init();
}

fn main() -> ExitCode {
    init_tracing_subscriber();
    let mut args: Vec<OsString> = std::env::args_os().collect();
    if should_inject_json_flag(&args) {
        args.insert(1, OsString::from("--json"));
    }
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    write_start_log(&args, &mut stderr);
    ee::cli::run(args, &mut stdout, &mut stderr).into()
}

#[cfg(test)]
// Main tests treat poisoned in-memory log buffers as fatal test failures.
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct SharedMakeWriter(Arc<Mutex<Vec<u8>>>);

    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("writer buffer poisoned").extend(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedMakeWriter {
        type Writer = SharedWriter;

        fn make_writer(&'a self) -> Self::Writer {
            SharedWriter(Arc::clone(&self.0))
        }
    }

    #[test]
    fn tracing_filter_emits_json_events_when_rust_log_matches() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_env_filter_from_env(Some(
                "ee_trace_test=debug".to_owned(),
            )))
            .with_writer(SharedMakeWriter(Arc::clone(&output)))
            .with_ansi(false)
            .json()
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "ee_trace_test", event = "visible", "trace_visible");
        });

        let captured = String::from_utf8(output.lock().expect("writer buffer poisoned").clone())
            .expect("tracing output is utf-8");
        assert!(captured.contains("\"target\":\"ee_trace_test\""));
        assert!(captured.contains("trace_visible"));
    }
}
