use std::ffi::OsString;
use std::io::{self, Write};

use crate::models::ProcessExitCode;
use crate::output;

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
    let mut iter = args.into_iter();
    let _program = iter.next();
    let tokens = iter
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let token_refs = tokens.iter().map(String::as_str).collect::<Vec<_>>();

    match token_refs.as_slice() {
        [] | ["--help"] | ["-h"] | ["help"] => write_stdout(stdout, output::help_text()),
        ["--version"] | ["-V"] | ["version"] => {
            write_stdout(stdout, concat!("ee ", env!("CARGO_PKG_VERSION"), "\n"))
        }
        ["status"] => write_stdout(stdout, output::human_status()),
        ["status", "--json"] | ["status", "-j"] | ["--json", "status"] => {
            write_stdout(stdout, &(output::status_response_json() + "\n"))
        }
        [unknown, ..] => {
            let _ = writeln!(stderr, "error: unknown command or flag `{unknown}`");
            let _ = writeln!(stderr);
            let _ = writeln!(stderr, "Next:");
            let _ = writeln!(stderr, "  ee --help");
            ProcessExitCode::Usage
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::run;
    use crate::models::ProcessExitCode;

    fn invoke(args: &[&str]) -> (ProcessExitCode, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = run(args.iter().map(OsString::from), &mut stdout, &mut stderr);
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        (exit, stdout, stderr)
    }

    #[test]
    fn status_json_writes_machine_data_to_stdout_only() {
        let (exit, stdout, stderr) = invoke(&["ee", "status", "--json"]);
        assert_eq!(exit, ProcessExitCode::Success);
        assert!(stdout.starts_with("{\"schema\":\"ee.response.v1\""));
        assert!(stdout.ends_with('\n'));
        assert!(stderr.is_empty());
    }

    #[test]
    fn unknown_command_writes_diagnostics_to_stderr_only() {
        let (exit, stdout, stderr) = invoke(&["ee", "unknown"]);
        assert_eq!(exit, ProcessExitCode::Usage);
        assert!(stdout.is_empty());
        assert!(stderr.contains("error: unknown command"));
    }
}
