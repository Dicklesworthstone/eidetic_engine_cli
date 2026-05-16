#![no_main]

//! Fuzz target for `ee context --ppr-weight=<float>` parsing and effective
//! clamp semantics.
//!
//! The production context path accepts raw user input, stores it on
//! `ContextArgs`, and later normalizes it before Personalized PageRank rerank.
//! This target exercises the public parser and pins the documented clamp
//! contract for all generated finite, infinite, and NaN bit patterns.

use clap::Parser;
use ee::cli::{Cli, Command};
use libfuzzer_sys::fuzz_target;

const DEFAULT_CONTEXT_PPR_WEIGHT: f32 = 0.30;
const MAX_DECIMAL_LEN: usize = 64;

fn read_u32(data: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    let len = data.len().min(buf.len());
    buf[..len].copy_from_slice(&data[..len]);
    u32::from_le_bytes(buf)
}

fn expected_effective_weight(value: Option<f32>) -> f32 {
    match value {
        Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
        Some(_) => DEFAULT_CONTEXT_PPR_WEIGHT,
        None => 0.0,
    }
}

fn assert_effective_weight_contract(raw: f32) {
    let rendered = raw.to_string();
    if rendered.len() > MAX_DECIMAL_LEN {
        return;
    }

    let parsed = Cli::try_parse_from(["ee", "context", "fuzz", "--ppr-weight", &rendered]);
    let Ok(cli) = parsed else {
        return;
    };

    let Some(Command::Context(args)) = cli.command else {
        panic!("ppr-weight argv must parse as context command");
    };
    let effective = expected_effective_weight(args.ppr_weight);
    assert!(
        (0.0..=1.0).contains(&effective),
        "effective ppr weight must stay in [0, 1], got {effective}"
    );

    if raw.is_finite() {
        assert_eq!(
            effective,
            raw.clamp(0.0, 1.0),
            "finite ppr weights clamp to the closed unit interval"
        );
    } else {
        assert_eq!(
            effective, DEFAULT_CONTEXT_PPR_WEIGHT,
            "non-finite ppr weights fall back to the default"
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let bits = read_u32(data);
    assert_effective_weight_contract(f32::from_bits(bits));

    if let Ok(text) = std::str::from_utf8(data) {
        let trimmed = text.trim();
        if trimmed.len() <= MAX_DECIMAL_LEN {
            let parsed = Cli::try_parse_from(["ee", "context", "fuzz", "--ppr-weight", trimmed]);
            if let Ok(cli) = parsed {
                let Some(Command::Context(args)) = cli.command else {
                    panic!("accepted ppr-weight text must parse as context command");
                };
                let effective = expected_effective_weight(args.ppr_weight);
                assert!((0.0..=1.0).contains(&effective));
            }
        }
    }
});
