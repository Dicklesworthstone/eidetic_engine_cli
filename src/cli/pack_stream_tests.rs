//! Canonical pack streaming and path-dependent CLI cache admission.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn canonical_pack_stream_parses_with_literal_targets_and_read_only() {
    let cli = Cli::try_parse_from([
        "ee",
        "--json",
        "pack",
        "prepare release",
        "--stream",
        "--read-only",
        "--task-path",
        "src/payments/invoice.rs",
        "--task-path",
        "tests/expiry.rs",
    ])
    .unwrap();
    let Some(Command::Pack(args)) = cli.command else {
        panic!("canonical pack");
    };
    assert!(args.stream && args.read_only);
    assert_eq!(
        args.task_paths,
        ["src/payments/invoice.rs", "tests/expiry.rs"]
    );
    assert_eq!(args.query.as_deref(), Some("prepare release"));
    let cli = Cli::try_parse_from(["ee", "pack", "prepare release"]).unwrap();
    let Some(Command::Pack(args)) = cli.command else {
        panic!("canonical pack");
    };
    assert!(!args.stream, "ordinary packs remain batch requests");
}

#[test]
fn canonical_stream_cannot_silently_ignore_file_or_envelope_options() {
    for suffix in [
        vec!["--output", "pack.json"],
        vec!["--explain-performance"],
        vec!["--explain-gaps"],
        vec!["--cursor", "cursor-token"],
        vec!["--since", "last"],
    ] {
        let mut argv = vec!["ee", "--json", "pack", "release", "--stream"];
        argv.extend(suffix);
        assert!(Cli::try_parse_from(argv).is_err());
    }
    for argv in [
        vec!["ee", "pack", "--stream"],
        vec!["ee", "pack", "--query-file", "query.json", "--stream"],
        vec![
            "ee",
            "pack",
            "replay",
            "pack_00000000000000000000000421",
            "--stream",
        ],
    ] {
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "streaming is for a task query"
        );
    }
}

#[test]
fn target_dependent_and_streamed_requests_bypass_cached_json() {
    let cli = Cli::try_parse_from(["ee", "--json", "context", "prepare release"]).unwrap();
    let Some(Command::Context(args)) = &cli.command else {
        panic!("context args");
    };
    assert!(context_json_cache_enabled(&cli, args, false));
    let mut targeted = args.clone();
    targeted.task_paths = vec!["src/payments/invoice.rs".to_owned()];
    assert!(!context_json_cache_enabled(&cli, &targeted, false));
    let mut streamed = args.clone();
    streamed.stream = true;
    assert!(!context_json_cache_enabled(&cli, &streamed, false));
    assert!(
        context_json_cache_enabled(&cli, args, false),
        "no-target behavior is unchanged"
    );
}

#[test]
fn existing_stream_format_validation_keeps_machine_frame_contract() {
    for format in ["json", "jsonl", "human", "markdown", "binary"] {
        let cli = Cli::try_parse_from(["ee", "--format", format, "context", "release", "--stream"])
            .unwrap();
        let Some(Command::Context(args)) = &cli.command else {
            panic!("context args");
        };
        assert_eq!(
            validate_context_stream_request(&cli, args).is_ok(),
            matches!(format, "json" | "jsonl")
        );
    }
}

#[test]
fn task_target_diff_uses_commitments_not_private_display_wrappers() {
    let hash = format!("blake3:{}", blake3::hash(b"src/payments/a.rs").to_hex());
    let plain = ParsedPackLedger::trusted_for_test(serde_json::json!({
        "request": {"taskPaths": [{"hash": hash, "text": "src/payments/a.rs", "redacted": false}]}
    }));
    let hidden = ParsedPackLedger::trusted_for_test(serde_json::json!({
        "request": {"taskPaths": [{"hash": hash, "redactedText": "[redacted]", "redacted": true}]}
    }));
    assert_eq!(
        pack_record_task_path_hashes(&plain),
        pack_record_task_path_hashes(&hidden)
    );
    let other = ParsedPackLedger::trusted_for_test(serde_json::json!({
        "request": {"taskPaths": [{"hash": format!("blake3:{}", blake3::hash(b"src/payments/b.rs").to_hex())}]}
    }));
    assert_ne!(
        pack_record_task_path_hashes(&plain),
        pack_record_task_path_hashes(&other)
    );
}

#[test]
fn historical_missing_targets_compare_as_empty_and_untrusted_ledgers_stay_unavailable() {
    let historical = ParsedPackLedger::trusted_for_test(serde_json::json!({"request": {}}));
    let empty =
        ParsedPackLedger::trusted_for_test(serde_json::json!({"request": {"taskPaths": []}}));
    assert_eq!(
        pack_record_task_path_hashes(&historical),
        pack_record_task_path_hashes(&empty)
    );
    for status in [
        PackLedgerStatus::Missing,
        PackLedgerStatus::Malformed,
        PackLedgerStatus::HashMismatch,
    ] {
        let untrusted = ParsedPackLedger::unavailable_for_test(status, vec![]);
        assert!(available_pack_ledger(&untrusted).is_none());
        assert!(pack_record_task_path_hashes(&untrusted).is_empty());
    }
}
