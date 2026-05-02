#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

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

fn main() -> ExitCode {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    if should_inject_json_flag(&args) {
        args.insert(1, OsString::from("--json"));
    }
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    ee::cli::run(args, &mut stdout, &mut stderr).into()
}
