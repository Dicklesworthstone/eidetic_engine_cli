//! Canonical pack streaming, read fences, and path-dependent CLI cache admission.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn pack_read_fence_modes_parse_and_forward_across_build_forms() {
    use crate::core::read_fence::ReadFence;

    for prefix in [
        vec!["ee", "pack", "release"],
        vec!["ee", "pack", "--query-file", "query.json"],
        vec!["ee", "pack", "build", "--query-file", "query.json"],
    ] {
        for (mode, expected) in [
            (None, ReadFence::Eventual),
            (Some("latest"), ReadFence::Latest),
            (Some("snapshot:12"), ReadFence::Snapshot(12)),
        ] {
            let mut argv = prefix.clone();
            if let Some(mode) = mode {
                argv.extend(["--read-fence", mode, "--strict-read-fence"]);
            }
            let cli = Cli::try_parse_from(argv).unwrap();
            let Some(Command::Pack(args)) = cli.command else {
                panic!("pack args");
            };
            if let Some(PackCommand::Build(build)) = &args.command {
                assert_eq!(build.read_fence, expected);
                assert_eq!(build.strict_read_fence, mode.is_some());
            } else {
                assert_eq!(args.read_fence, expected);
                assert_eq!(args.strict_read_fence, mode.is_some());
                if args.query_file.is_some() {
                    let build = args.legacy_build_args().unwrap();
                    assert_eq!(build.read_fence, expected);
                    assert_eq!(build.strict_read_fence, mode.is_some());
                }
            }
        }
        for invalid in [
            "later",
            "snapshot",
            "snapshot:",
            "snapshot:-1",
            "snapshot:18446744073709551616",
        ] {
            let mut argv = prefix.clone();
            argv.extend(["--read-fence", invalid]);
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "invalid fence {invalid}"
            );
        }
    }
}

#[test]
fn eventual_read_fence_never_resolves_or_observes_a_store() {
    let mut output = Vec::new();
    let exit = read_fence_preflight(
        || panic!("the default fence must not resolve store paths or open the store"),
        PACK_COMMAND,
        crate::core::read_fence::ReadFence::Eventual,
        false,
        &mut output,
    );
    assert_eq!(exit, None);
    assert!(output.is_empty());
}

fn execute_fenced_pack(argv: Vec<&str>) -> (ProcessExitCode, serde_json::Value) {
    let cli = Cli::try_parse_from(argv).unwrap();
    let Some(Command::Pack(args)) = &cli.command else {
        panic!("pack args");
    };
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let exit = handle_pack_command(
        &mut CliProcess::default(),
        &cli,
        args,
        &mut output,
        &mut errors,
    );
    assert!(errors.is_empty(), "{}", String::from_utf8_lossy(&errors));
    (exit, serde_json::from_slice(&output).unwrap())
}

fn assert_fence_refusal(exit: ProcessExitCode, output: &serde_json::Value, code: &str) {
    assert_eq!(exit, ProcessExitCode::UnsatisfiedDegradedMode);
    assert_eq!(output["success"], false);
    assert_eq!(output["data"]["command"], PACK_COMMAND);
    assert_eq!(output["data"]["readFence"]["strictFailed"], true);
    assert_eq!(output["degraded"][0]["code"], code);
    assert_eq!(output["degraded"][0]["severity"], "high");
    assert!(output["data"].get("pack").is_none());
}

#[test]
fn strict_pack_refuses_unobservable_store_before_output_or_streaming() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().to_str().unwrap();
    let output_path = temp.path().join("absent-parent/pack.json");
    for suffix in [
        vec!["--output", output_path.to_str().unwrap()],
        vec!["--stream"],
    ] {
        let mut argv = vec![
            "ee",
            "--json",
            "--workspace",
            workspace,
            "pack",
            "release",
            "--read-fence",
            "latest",
            "--strict-read-fence",
        ];
        argv.extend(suffix);
        let (exit, output) = execute_fenced_pack(argv);
        assert_fence_refusal(exit, &output, "read_fence_generation_unavailable");
        assert_eq!(
            output["data"]["readFence"]["generationSource"],
            "unavailable"
        );
        assert_eq!(output["data"]["readFence"]["verdict"], "unavailable");
        assert!(!output_path.exists());
        assert!(!temp.path().join(".ee").exists());
    }
}

fn write_fence_generations(
    options: &IndexStatusOptions,
    database_generation: Option<u64>,
    index_generation: Option<u64>,
) {
    if let Some(generation) = database_generation {
        let path = options
            .database_path
            .clone()
            .unwrap_or_else(|| options.workspace_path.join(".ee/ee.db"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let db = crate::db::DbConnection::open_file(&path).unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            "wsp_00000000000000000000000710",
            &crate::db::CreateWorkspaceInput {
                path: options
                    .workspace_path
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                name: Some("Pack read-fence fixture".to_owned()),
            },
        )
        .unwrap();
        db.execute_raw(&format!(
            "UPDATE workspace_generations SET generation = {generation};"
        ))
        .unwrap();
        assert_eq!(
            db.get_workspace_generation("wsp_00000000000000000000000710")
                .unwrap(),
            Some(generation)
        );
        db.close().unwrap();
    }
    if let Some(generation) = index_generation {
        let path = crate::config::workspace::resolve_store_index_dir(
            &options.workspace_path,
            options.database_path.as_deref(),
            options.index_dir.as_deref(),
        );
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("meta.json"),
            serde_json::json!({"sourceGeneration": generation}).to_string(),
        )
        .unwrap();
    }
}

#[test]
fn strict_read_fence_requires_both_database_and_index_generations() {
    for (database_generation, index_generation) in [(None, None), (Some(7), None), (None, Some(6))]
    {
        let temp = tempfile::tempdir().unwrap();
        let options = IndexStatusOptions {
            workspace_path: temp.path().canonicalize().unwrap(),
            database_path: None,
            index_dir: None,
        };
        write_fence_generations(&options, database_generation, index_generation);
        let (_, _, source) = resolve_store_integrity_generations(&options, None, None);
        assert_eq!(
            source,
            crate::core::store_integrity::GenerationSource::Unavailable
        );
        let mut output = Vec::new();
        let exit = read_fence_preflight(
            || options,
            PACK_COMMAND,
            crate::core::read_fence::ReadFence::Latest,
            true,
            &mut output,
        )
        .unwrap();
        assert_fence_refusal(
            exit,
            &serde_json::from_slice(&output).unwrap(),
            "read_fence_generation_unavailable",
        );
    }
}

#[test]
fn canonical_pack_fence_observes_explicit_database_and_index_paths() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().canonicalize().unwrap();
    let default_options = IndexStatusOptions {
        workspace_path: workspace.clone(),
        database_path: None,
        index_dir: None,
    };
    write_fence_generations(&default_options, Some(9), Some(9));
    let database = workspace.join("selected-store/selected.db");
    let index = workspace.join("selected-index");
    let selected = IndexStatusOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(index.clone()),
    };
    write_fence_generations(&selected, Some(17), Some(4));
    let (exit, output) = execute_fenced_pack(vec![
        "ee",
        "--json",
        "--workspace",
        workspace.to_str().unwrap(),
        "pack",
        "release",
        "--database",
        database.to_str().unwrap(),
        "--index-dir",
        index.to_str().unwrap(),
        "--read-fence",
        "latest",
        "--strict-read-fence",
    ]);
    assert_fence_refusal(exit, &output, "read_fence_stale_derived_asset");
    assert_eq!(output["data"]["readFence"]["workspaceGeneration"], 17);
    assert_eq!(
        output["data"]["readFence"]["assetGenerations"][0]["generation"],
        4
    );
    assert_eq!(output["data"]["readFence"]["generationSource"], "observed");
    let mut output = Vec::new();
    assert_eq!(
        read_fence_preflight(
            || default_options,
            PACK_COMMAND,
            crate::core::read_fence::ReadFence::Latest,
            true,
            &mut output
        ),
        None
    );
    assert!(
        output.is_empty(),
        "a coherent fence admits the read without replacing its output"
    );
}

#[test]
fn pack_query_file_fence_uses_the_document_workspace_unless_cli_overrides_it() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().canonicalize().unwrap();
    write_fence_generations(
        &IndexStatusOptions {
            workspace_path: workspace.clone(),
            database_path: None,
            index_dir: None,
        },
        Some(7),
        Some(5),
    );
    let query_file = workspace.join("task.eeq.json");
    std::fs::write(&query_file, serde_json::json!({"version": "ee.query.v1", "workspace": workspace, "query": {"text": "prepare release"}}).to_string()).unwrap();
    let override_workspace = tempfile::tempdir().unwrap();
    for build in [false, true] {
        let mut argv = vec!["ee", "--json", "pack"];
        if build {
            argv.push("build");
        }
        argv.extend([
            "--query-file",
            query_file.to_str().unwrap(),
            "--read-fence",
            "latest",
            "--strict-read-fence",
        ]);
        let (exit, output) = execute_fenced_pack(argv.clone());
        assert_fence_refusal(exit, &output, "read_fence_stale_derived_asset");
        assert_eq!(output["data"]["readFence"]["workspaceGeneration"], 7);
        assert_eq!(
            output["data"]["readFence"]["assetGenerations"][0]["generation"],
            5
        );
        argv.extend(["--workspace", override_workspace.path().to_str().unwrap()]);
        let (exit, output) = execute_fenced_pack(argv);
        assert_fence_refusal(exit, &output, "read_fence_generation_unavailable");
        assert!(!override_workspace.path().join(".ee").exists());
    }
}

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
