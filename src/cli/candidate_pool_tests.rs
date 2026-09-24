//! Preserve omission all the way from clap to the pack configuration resolver.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;

#[test]
fn candidate_pool_cli_distinguishes_omission_from_explicit_hundred() {
    for command in ["context", "pack"] {
        for (flags, expected) in [(vec![], None), (vec!["--candidate-pool", "100"], Some(100)), (vec!["--candidate-pool", "7"], Some(7))] {
            let mut argv = vec!["ee", command, "pool test"];
            argv.extend(flags);
            let cli = Cli::try_parse_from(argv).unwrap();
            let pool = match cli.command.unwrap() {
                Command::Context(args) => args.candidate_pool,
                Command::Pack(args) => args.candidate_pool,
                _ => panic!("unexpected command"),
            };
            assert_eq!(pool, expected, "{command}");
        }
    }
}

#[test]
fn candidate_pool_query_document_preserves_omission_and_explicit_value() {
    let omitted = parse_query_document(r#"{"version":"ee.query.v1","query":{"text":"pool test"}}"#).unwrap();
    assert_eq!(omitted.candidate_pool, None);
    let explicit = parse_query_document(r#"{"version":"ee.query.v1","query":{"text":"pool test"},"budget":{"candidatePool":100}}"#).unwrap();
    assert_eq!(explicit.candidate_pool, Some(100));
}
