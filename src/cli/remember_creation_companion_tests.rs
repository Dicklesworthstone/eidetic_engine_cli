//! Exercise the CLI's atomic creation routing, not just the core helpers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn parse_remember(workspace: &Path, args: &[&str]) -> Cli {
    let mut argv = vec![
        "ee".to_owned(),
        "--workspace".to_owned(),
        workspace.to_string_lossy().into_owned(),
    ];
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    Cli::try_parse_from(argv).unwrap()
}

fn run_remember(cli: &Cli) -> Result<(RememberOutcome, Option<serde_json::Value>), DomainError> {
    let Some(Command::Remember(args)) = cli.command.as_ref() else {
        panic!("fixture must parse as remember");
    };
    handle_remember(cli, args)
}

fn snapshot(db: &DbConnection) -> Vec<String> {
    [
        "memories",
        "audit_log",
        "search_index_jobs",
        "remember_idempotency_keys",
        "memory_sentinel_specs",
        "memory_seals",
    ]
    .into_iter()
    .map(|table| {
        format!(
            "{:?}",
            db.query(&format!("SELECT * FROM {table} ORDER BY 1, 2"), &[])
                .unwrap()
        )
    })
    .collect()
}

#[test]
fn remember_cli_companion_failure_rolls_back_and_retry_reports_committed_evidence() {
    for sealed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().canonicalize().unwrap();
        let initialized = crate::core::init::init_workspace(&crate::core::init::InitOptions {
            workspace_path: workspace.clone(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        });
        assert!(initialized.status.is_success(), "{initialized:?}");
        let db = DbConnection::open_file(&initialized.database_path).unwrap();
        let cli = if sealed {
            parse_remember(
                &workspace,
                &[
                    "remember",
                    "The pre-registered measurement has not been revealed.",
                    "--seal",
                    "--no-auto-link",
                    "--no-propose-candidates",
                ],
            )
        } else {
            parse_remember(
                &workspace,
                &[
                    "remember",
                    "This decision requires the recorded creation guards.",
                    "--sentinel",
                    "path_exists:Cargo.toml",
                    "--revive-when",
                    "path_exists:src/lib.rs",
                    "--idempotency-key",
                    "cli-guard-key",
                    "--no-auto-link",
                    "--no-propose-candidates",
                ],
            )
        };
        let trigger = if sealed {
            "CREATE TRIGGER reject_cli_companion BEFORE INSERT ON memory_seals
             BEGIN SELECT RAISE(ABORT, 'cli_creation_companion_failure'); END"
        } else {
            "CREATE TRIGGER reject_cli_companion BEFORE INSERT ON memory_sentinel_specs
             WHEN (SELECT COUNT(*) FROM memory_sentinel_specs WHERE memory_id = NEW.memory_id) = 1
             BEGIN SELECT RAISE(ABORT, 'cli_creation_companion_failure'); END"
        };
        db.execute_raw(trigger).unwrap();
        let before = snapshot(&db);
        let error = run_remember(&cli).unwrap_err();
        assert!(
            error.message().contains("cli_creation_companion_failure"),
            "{error:?}"
        );
        assert_eq!(
            snapshot(&db),
            before,
            "CLI failure cannot leave an incomplete memory"
        );
        db.execute_raw("DROP TRIGGER reject_cli_companion").unwrap();
        let (outcome, seal_report) = run_remember(&cli).unwrap();
        let RememberOutcome::Created(report) = outcome else {
            panic!("retry after a rolled-back source must create it");
        };
        let id = report.memory_id.to_string();
        assert!(!report.dry_run);
        assert!(db.get_memory(&id).unwrap().is_some());
        if sealed {
            let stored = db.get_memory_seal(&id).unwrap().unwrap();
            let seal_report = seal_report.unwrap();
            assert_eq!(
                seal_report["contentCommitment"].as_str(),
                Some(stored.content_commitment.as_str())
            );
            assert_eq!(
                seal_report["sealedAt"].as_str(),
                Some(stored.sealed_at.as_str())
            );
            assert_eq!(seal_report["revealed"], false);
            assert_eq!(
                db.get_memory(&id).unwrap().unwrap().content,
                crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT
            );
        } else {
            assert!(seal_report.is_none());
            assert_eq!(db.list_memory_sentinel_specs(&id).unwrap().len(), 2);
            let committed = snapshot(&db);
            assert!(matches!(
                run_remember(&cli).unwrap().0,
                RememberOutcome::AlreadyRecorded(_)
            ));
            assert_eq!(snapshot(&db), committed);
        }
    }
}

#[test]
fn remember_cli_seal_preview_returns_prepared_evidence_without_a_store() {
    let root = tempfile::tempdir().unwrap();
    let content = "The sealed preview is prepared before source creation.";
    let cli = parse_remember(root.path(), &["remember", content, "--seal", "--dry-run"]);
    let (outcome, seal_report) = run_remember(&cli).unwrap();
    let RememberOutcome::Created(report) = outcome else {
        panic!("seal preview must return the created report shape");
    };
    let seal = seal_report.unwrap();
    assert!(report.dry_run);
    assert_eq!(
        seal["contentCommitment"],
        crate::models::seal_commitment_for_content(content.as_bytes()).unwrap()
    );
    assert!(chrono::DateTime::parse_from_rfc3339(seal["sealedAt"].as_str().unwrap()).is_ok());
    assert!(!report.database_path.exists());
    assert!(!root.path().join(".ee").exists());
}

#[test]
fn remember_cli_rejects_unstorable_threshold_before_local_or_global_bootstrap() {
    for global in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut args = vec![
            "remember",
            "The threshold must fit the durable sentinel representation.",
            "--sentinel-stale-threshold-seconds",
            "18446744073709551615",
        ];
        if global {
            args.push("--global");
        }
        let error = run_remember(&parse_remember(root.path(), &args)).unwrap_err();
        assert!(matches!(error, DomainError::Usage { .. }), "{error:?}");
        assert!(error.message().contains("signed 64-bit"), "{error:?}");
        assert!(!root.path().join(".ee").exists());
    }
}
