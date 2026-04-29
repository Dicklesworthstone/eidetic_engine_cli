#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    ee::cli::run_from_env().into()
}
