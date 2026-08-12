#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use ee::config::env_registry::{EnvVar, read};
use ee::obs::{LogEnvelope, LogLevel, now_rfc3339_nanos};
use serde_json::Value;
use tracing_subscriber::EnvFilter;

// Windows' default process-entry stack is too small for Clap's large top-level
// command enum. Unix keeps the process main stack so the host's stack policy
// remains authoritative instead of imposing a fixed ceiling as the CLI grows.
#[cfg(windows)]
const WINDOWS_CLI_STACK_SIZE: usize = 8 * 1024 * 1024;

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
    let mut args = args.iter().skip(1).peekable();
    while let Some(arg) = args.next() {
        if arg.as_os_str() == std::ffi::OsStr::new("--") {
            return false;
        }
        let Some(value) = arg.to_str() else {
            continue;
        };
        if value == "--json" || value == "-j" || value == "--robot" {
            return true;
        }
        if let Some(format) = value.strip_prefix("--format=")
            && is_known_output_format(format)
        {
            return true;
        }
        if value == "--format"
            && let Some(next) = args.peek()
            && next.as_os_str() != std::ffi::OsStr::new("--")
            && next
                .to_str()
                .is_some_and(|value| !value.starts_with('-') && is_known_output_format(value))
        {
            return true;
        }
    }
    false
}

fn is_known_output_format(value: &str) -> bool {
    matches!(
        value,
        "human"
            | "json"
            | "toon"
            | "jsonl"
            | "compact"
            | "hook"
            | "markdown"
            | "binary"
            | "mermaid"
    )
}

fn injected_output_flag_for_env(
    args: &[OsString],
    requested: Option<ee::output::Renderer>,
) -> Option<OsString> {
    if has_explicit_machine_output_flag(args) {
        return None;
    }
    // `--json` (rather than `--format=json`) preserves `wants_json` semantics
    // for EE_JSON / EE_AGENT_MODE, matching the historical agent-mode
    // injection. Every other renderer maps onto its canonical --format value.
    requested.map(|renderer| match renderer {
        ee::output::Renderer::Json => OsString::from("--json"),
        other => OsString::from(format!("--format={}", other.as_str())),
    })
}

fn injected_output_flag(args: &[OsString]) -> Option<OsString> {
    injected_output_flag_for_env(args, ee::output::renderer_requested_by_env())
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
                | "--max-output-tokens"
                | "--cards"
                | "--policy"
                | "--shadow"
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
            || value.starts_with("--max-output-tokens=")
            || value.starts_with("--cards=")
            || value.starts_with("--policy=")
            || value.starts_with("--shadow=")
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
            let value = tracing_filter_with_runtime_noise_defaults(value);
            EnvFilter::try_new(value).unwrap_or_else(|_| EnvFilter::new("off"))
        }
        _ => EnvFilter::new("off"),
    }
}

const DEFAULT_TRACE_NOISE_TARGETS: &[&str] = &[
    "fsqlite::runtime",
    "ee::search::embedder_down",
    "ee::output::error",
];

fn tracing_filter_with_runtime_noise_defaults(value: String) -> String {
    let mut normalized = value.trim().to_owned();
    if normalized.eq_ignore_ascii_case("off") {
        return normalized;
    }

    for target in DEFAULT_TRACE_NOISE_TARGETS {
        if !tracing_filter_has_target(&normalized, target) {
            normalized.push(',');
            normalized.push_str(target);
            normalized.push_str("=error");
        }
    }
    normalized
}

fn tracing_filter_has_target(value: &str, target: &str) -> bool {
    value.split(',').any(|directive| {
        let Some(remainder) = directive.trim_start().strip_prefix(target) else {
            return false;
        };
        remainder.is_empty() || remainder.starts_with('=') || remainder.starts_with('[')
    })
}

fn init_tracing_subscriber() {
    let filter = tracing_env_filter_from_env(std::env::var("RUST_LOG").ok());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .json()
        .try_init();
}

fn cli_main() -> ExitCode {
    init_tracing_subscriber();
    let mut args: Vec<OsString> = std::env::args_os().collect();
    if let Some(flag) = injected_output_flag(&args) {
        args.insert(1, flag);
    }
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    write_start_log(&args, &mut stderr);
    ee::cli::run(args, &mut stdout, &mut stderr).into()
}

#[cfg(windows)]
fn main() -> ExitCode {
    match std::thread::Builder::new()
        .name("ee-cli-main".to_owned())
        .stack_size(WINDOWS_CLI_STACK_SIZE)
        .spawn(cli_main)
    {
        Ok(handle) => handle.join().unwrap_or_else(|_| ExitCode::from(101)),
        Err(error) => {
            let mut stderr = io::stderr();
            let _ = writeln!(stderr, "failed to start ee CLI thread: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    cli_main()
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
    fn explicit_machine_output_flag_detection_stops_at_separator() {
        let args = [
            OsString::from("ee"),
            OsString::from("search"),
            OsString::from("--"),
            OsString::from("--format=json"),
            OsString::from("--json"),
        ];

        assert!(!has_explicit_machine_output_flag(&args));
    }

    #[test]
    fn explicit_machine_output_flag_detection_handles_real_flags() {
        let split_format = [
            OsString::from("ee"),
            OsString::from("pack"),
            OsString::from("task"),
            OsString::from("--format"),
            OsString::from("markdown"),
        ];
        assert!(has_explicit_machine_output_flag(&split_format));

        let json_flag = [
            OsString::from("ee"),
            OsString::from("--json"),
            OsString::from("status"),
        ];
        assert!(has_explicit_machine_output_flag(&json_flag));

        let equals_format = [
            OsString::from("ee"),
            OsString::from("status"),
            OsString::from("--format=json"),
        ];
        assert!(has_explicit_machine_output_flag(&equals_format));
    }

    #[test]
    fn env_requested_json_injects_json_when_no_output_flag_is_explicit() {
        let args = [OsString::from("ee"), OsString::from("status")];

        assert_eq!(
            injected_output_flag_for_env(&args, Some(ee::output::Renderer::Json)),
            Some(OsString::from("--json"))
        );
    }

    #[test]
    fn env_requested_hook_injects_hook_renderer_when_no_output_flag_is_explicit() {
        let args = [
            OsString::from("ee"),
            OsString::from("pack"),
            OsString::from("task"),
        ];

        assert_eq!(
            injected_output_flag_for_env(&args, Some(ee::output::Renderer::Hook)),
            Some(OsString::from("--format=hook"))
        );
    }

    #[test]
    fn env_requested_renderers_map_to_canonical_format_flags() {
        let args = [OsString::from("ee"), OsString::from("status")];
        for (renderer, expected) in [
            (ee::output::Renderer::Human, "--format=human"),
            (ee::output::Renderer::Toon, "--format=toon"),
            (ee::output::Renderer::Jsonl, "--format=jsonl"),
            (ee::output::Renderer::Compact, "--format=compact"),
            (ee::output::Renderer::Markdown, "--format=markdown"),
        ] {
            assert_eq!(
                injected_output_flag_for_env(&args, Some(renderer)),
                Some(OsString::from(expected)),
                "renderer {renderer:?} must inject its canonical --format value"
            );
        }
        assert_eq!(
            injected_output_flag_for_env(&args, None),
            None,
            "no env-requested renderer means no injection"
        );
    }

    #[test]
    fn env_output_injection_preserves_explicit_renderer_precedence() {
        let explicit = [
            OsString::from("ee"),
            OsString::from("--format"),
            OsString::from("markdown"),
            OsString::from("pack"),
            OsString::from("task"),
        ];
        assert_eq!(
            injected_output_flag_for_env(&explicit, Some(ee::output::Renderer::Json)),
            None,
            "explicit renderer must not be overridden"
        );

        let implicit = [OsString::from("ee"), OsString::from("status")];
        assert_eq!(
            injected_output_flag_for_env(&implicit, Some(ee::output::Renderer::Json)),
            Some(OsString::from("--json")),
            "env-requested JSON keeps the same precedence as OutputContext"
        );
    }

    #[test]
    fn bare_format_flag_does_not_suppress_env_injection() {
        let missing_format_value = [
            OsString::from("ee"),
            OsString::from("status"),
            OsString::from("--format"),
        ];
        assert_eq!(
            injected_output_flag_for_env(&missing_format_value, Some(ee::output::Renderer::Json)),
            Some(OsString::from("--json")),
            "env JSON should still force JSON parse errors"
        );
        assert_eq!(
            injected_output_flag_for_env(&missing_format_value, Some(ee::output::Renderer::Hook)),
            Some(OsString::from("--format=hook")),
            "env hook should still force hook parse errors"
        );

        let separator_after_format = [
            OsString::from("ee"),
            OsString::from("status"),
            OsString::from("--format"),
            OsString::from("--"),
        ];
        assert_eq!(
            injected_output_flag_for_env(&separator_after_format, Some(ee::output::Renderer::Json)),
            Some(OsString::from("--json")),
            "a separator is not a renderer value"
        );

        let empty_format_value = [
            OsString::from("ee"),
            OsString::from("status"),
            OsString::from("--format="),
        ];
        assert_eq!(
            injected_output_flag_for_env(&empty_format_value, Some(ee::output::Renderer::Json)),
            Some(OsString::from("--json")),
            "an empty --format= value is not a renderer"
        );

        let next_flag_after_format = [
            OsString::from("ee"),
            OsString::from("status"),
            OsString::from("--format"),
            OsString::from("--workspace"),
            OsString::from("."),
        ];
        assert_eq!(
            injected_output_flag_for_env(&next_flag_after_format, Some(ee::output::Renderer::Hook)),
            Some(OsString::from("--format=hook")),
            "another flag is not a renderer value"
        );
    }

    #[test]
    fn malformed_format_value_does_not_suppress_env_injection() {
        let invalid_split_format = [
            OsString::from("ee"),
            OsString::from("--format"),
            OsString::from("status"),
        ];
        assert_eq!(
            injected_output_flag_for_env(&invalid_split_format, Some(ee::output::Renderer::Json)),
            Some(OsString::from("--json")),
            "env JSON should still force JSON for invalid split --format values"
        );
        assert_eq!(
            injected_output_flag_for_env(&invalid_split_format, Some(ee::output::Renderer::Hook)),
            Some(OsString::from("--format=hook")),
            "env hook should still force hook output for invalid split --format values"
        );

        let invalid_equals_format = [
            OsString::from("ee"),
            OsString::from("status"),
            OsString::from("--format=bogus"),
        ];
        assert_eq!(
            injected_output_flag_for_env(&invalid_equals_format, Some(ee::output::Renderer::Json)),
            Some(OsString::from("--json")),
            "env JSON should still force JSON for invalid --format=value values"
        );
        assert_eq!(
            injected_output_flag_for_env(&invalid_equals_format, Some(ee::output::Renderer::Hook)),
            Some(OsString::from("--format=hook")),
            "env hook should still force hook output for invalid --format=value values"
        );
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

    #[test]
    fn tracing_filter_suppresses_default_noise_warnings_unless_explicit() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_env_filter_from_env(Some("warn".to_owned())))
            .with_writer(SharedMakeWriter(Arc::clone(&output)))
            .with_ansi(false)
            .json()
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "fsqlite::runtime", event = "hidden", "runtime_warn");
            tracing::warn!(target: "ee::search::embedder_down", event = "hidden", "embedder_warn");
            tracing::warn!(target: "ee::output::error", event = "hidden", "error_envelope_warn");
            tracing::error!(target: "fsqlite::runtime", event = "visible", "runtime_error");
            tracing::error!(target: "ee::search::embedder_down", event = "visible", "embedder_error");
            tracing::error!(target: "ee::output::error", event = "visible", "error_envelope_error");
        });

        let captured = String::from_utf8(output.lock().expect("writer buffer poisoned").clone())
            .expect("tracing output is utf-8");
        assert!(!captured.contains("runtime_warn"));
        assert!(!captured.contains("embedder_warn"));
        assert!(!captured.contains("error_envelope_warn"));
        assert!(captured.contains("runtime_error"));
        assert!(captured.contains("embedder_error"));
        assert!(captured.contains("error_envelope_error"));

        let explicit = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_env_filter_from_env(Some(
                "warn,fsqlite::runtime=warn,ee::search::embedder_down=warn,ee::output::error=warn"
                    .to_owned(),
            )))
            .with_writer(SharedMakeWriter(Arc::clone(&explicit)))
            .with_ansi(false)
            .json()
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "fsqlite::runtime", event = "visible", "runtime_warn");
            tracing::warn!(target: "ee::search::embedder_down", event = "visible", "embedder_warn");
            tracing::warn!(target: "ee::output::error", event = "visible", "error_envelope_warn");
        });

        let captured = String::from_utf8(explicit.lock().expect("writer buffer poisoned").clone())
            .expect("tracing output is utf-8");
        assert!(captured.contains("runtime_warn"));
        assert!(captured.contains("embedder_warn"));
        assert!(captured.contains("error_envelope_warn"));

        let narrowed =
            tracing_filter_with_runtime_noise_defaults("warn,ee::output::errorish=warn".to_owned());
        assert!(
            narrowed.contains("ee::output::error=error"),
            "prefix-neighbor targets must not suppress the real default: {narrowed}"
        );

        let child_target = tracing_filter_with_runtime_noise_defaults(
            "warn,ee::output::error::child=warn".to_owned(),
        );
        assert!(
            child_target.contains("ee::output::error=error"),
            "child targets must not suppress the parent default: {child_target}"
        );
    }

    #[test]
    fn tracing_filter_trims_outer_whitespace_before_parsing_directives() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_env_filter_from_env(Some(" warn ".to_owned())))
            .with_writer(SharedMakeWriter(Arc::clone(&output)))
            .with_ansi(false)
            .json()
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "ee_trace_test", event = "visible", "trimmed_warn");
        });

        let captured = String::from_utf8(output.lock().expect("writer buffer poisoned").clone())
            .expect("tracing output is utf-8");
        assert!(
            captured.contains("trimmed_warn"),
            "whitespace-padded RUST_LOG directives must keep their intended level: {captured:?}"
        );

        assert_eq!(
            tracing_filter_with_runtime_noise_defaults(" off ".to_owned()),
            "off",
            "the off directive should be normalized before EnvFilter parsing"
        );
    }
}
