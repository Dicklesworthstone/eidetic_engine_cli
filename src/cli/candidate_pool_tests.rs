//! Preserve omission all the way from clap to the pack configuration resolver.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;

#[test]
fn candidate_pool_cli_distinguishes_omission_from_explicit_hundred() {
    for command in ["context", "pack"] {
        for (flags, expected) in [
            (vec![], None),
            (vec!["--candidate-pool", "100"], Some(100)),
            (vec!["--candidate-pool", "7"], Some(7)),
        ] {
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
    let omitted =
        parse_query_document(r#"{"version":"ee.query.v1","query":{"text":"pool test"}}"#).unwrap();
    assert_eq!(omitted.candidate_pool, None);
    let explicit = parse_query_document(
        r#"{"version":"ee.query.v1","query":{"text":"pool test"},"budget":{"candidatePool":100}}"#,
    )
    .unwrap();
    assert_eq!(explicit.candidate_pool, Some(100));
}

fn request_candidate_pool(argv: &[&str]) -> Result<u64, String> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = run(argv.iter().map(OsString::from), &mut stdout, &mut stderr);
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    if exit != ProcessExitCode::Success {
        return Err(format!(
            "{argv:?} exited {exit:?}: {stdout} {}",
            String::from_utf8_lossy(&stderr)
        ));
    }
    let value: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| e.to_string())?;
    value["data"]["request"]["candidatePool"]
        .as_u64()
        .ok_or_else(|| format!("{argv:?}: no data.request.candidatePool in {stdout}"))
}

/// GH #49: `ee pack` and the `ee context` alias both follow
/// `pack.candidate_pool` when `--candidate-pool` is omitted, and an explicit
/// flag still wins.
#[test]
fn omitted_candidate_pool_follows_workspace_config_for_pack_and_context() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.to_str().unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = run(
        ["ee", "init", "--workspace", workspace, "--json"].map(OsString::from),
        &mut out,
        &mut err,
    );
    assert_eq!(
        exit,
        ProcessExitCode::Success,
        "{}",
        String::from_utf8_lossy(&out)
    );
    std::fs::write(
        root.join(".ee").join("config.toml"),
        "[pack]\ncandidate_pool = 7\n",
    )
    .unwrap();
    for command in ["pack", "context"] {
        let base = [
            "ee",
            "--json",
            "--workspace",
            workspace,
            command,
            "pool test",
            "--read-only",
        ];
        assert_eq!(
            request_candidate_pool(&base).unwrap(),
            7,
            "{command} follows config"
        );
        let mut explicit = base.to_vec();
        explicit.extend(["--candidate-pool", "13"]);
        assert_eq!(
            request_candidate_pool(&explicit).unwrap(),
            13,
            "{command} explicit flag wins"
        );
    }
    std::fs::write(
        root.join(".ee").join("config.toml"),
        "[pack]\ncandidate_pool = 0\n",
    )
    .unwrap();
    for command in ["pack", "context"] {
        let argv = [
            "ee",
            "--json",
            "--workspace",
            workspace,
            command,
            "pool test",
            "--read-only",
        ];
        assert!(
            request_candidate_pool(&argv).is_err(),
            "{command} rejects a zero pool from config"
        );
    }
}
