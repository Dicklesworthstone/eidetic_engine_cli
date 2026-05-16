#![no_main]

//! Fuzz target for `ee proximity <memory-a> <memory-b>` argument parsing.
//!
//! This stays at the clap layer rather than opening a database. The goal is to
//! prove arbitrary bounded byte streams cannot panic the pair-argument parser,
//! and that accepted parses preserve the two positional memory IDs exactly.

use std::ffi::OsString;

use clap::Parser;
use ee::cli::{Cli, Command};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 4096;

fn split_pair(data: &[u8]) -> (&[u8], &[u8]) {
    let split_at = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(data.len() / 2);
    let (left, right_with_separator) = data.split_at(split_at);
    let right = right_with_separator
        .get(1..)
        .unwrap_or(right_with_separator);
    (left, right)
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let (left, right) = split_pair(data);
    let memory_a = String::from_utf8_lossy(left).into_owned();
    let memory_b = String::from_utf8_lossy(right).into_owned();
    let argv = vec![
        OsString::from("ee"),
        OsString::from("--json"),
        OsString::from("proximity"),
        OsString::from(memory_a.clone()),
        OsString::from(memory_b.clone()),
    ];

    match Cli::try_parse_from(argv) {
        Ok(cli) => match cli.command {
            Some(Command::Proximity(args)) => {
                assert_eq!(args.memory_a, memory_a);
                assert_eq!(args.memory_b, memory_b);
                assert!(args.database.is_none());
                assert!(args.min_weight.is_none());
                assert!(args.min_confidence.is_none());
                assert!(args.link_limit.is_none());
                assert!(!args.include_tombstoned);
            }
            other => panic!("accepted argv must parse as proximity, got {other:?}"),
        },
        Err(error) => {
            let rendered = error.to_string();
            assert!(
                !rendered.is_empty(),
                "parse errors must remain renderable diagnostics"
            );
        }
    }
});
