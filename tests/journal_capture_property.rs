//! Property coverage for the capture batch contracts (`bd-1pi9m.6`).
//!
//! The example tests in `src/core/journal.rs` and `src/core/memory.rs` pin the
//! happy paths. These proptests widen the input distribution for the two batch
//! invariants harnesses depend on:
//! - journal JSONL capture preserves per-line isolation across valid and
//!   invalid lines;
//! - remember JSONL idempotency-key replay returns the first memory id.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use ee::core::journal::{JournalAppendOptions, JournalSource, append_journal_entries_stdin};
use ee::core::memory::{
    RememberBatchOptions, RememberMemoryOptions, RememberOutcome, RememberWriteControls,
    remember_memory_batch_stdin, remember_memory_with_controls,
};
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestCaseError};

#[derive(Clone, Debug)]
enum JournalLineCase {
    Valid { body: String, command_failure: bool },
    MissingBody,
    InvalidJson,
    NonObject,
}

impl JournalLineCase {
    fn is_valid(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }

    fn jsonl(&self, index: usize) -> String {
        match self {
            Self::Valid {
                body,
                command_failure,
            } => {
                let mut value = serde_json::json!({
                    "body": format!("journal property {index}: {body}"),
                    "kind": "observation",
                });
                if *command_failure {
                    value["kind"] = serde_json::json!("command_failure");
                    value["cmd"] = serde_json::json!("cargo test --lib journal_capture");
                    value["exitCode"] = serde_json::json!(101);
                    value["stderrTail"] = serde_json::json!("property failure stderr");
                }
                value.to_string()
            }
            Self::MissingBody => serde_json::json!({"kind": "note"}).to_string(),
            Self::InvalidJson => "{not valid json".to_owned(),
            Self::NonObject => serde_json::json!("not an object").to_string(),
        }
    }
}

fn safe_text() -> impl Strategy<Value = String> {
    "[A-Za-z0-9 _.,:/-]{1,64}"
        .prop_map(|raw| raw.trim().to_owned())
        .prop_filter("generated text must not be blank", |text| !text.is_empty())
}

fn safe_key() -> impl Strategy<Value = String> {
    "[A-Za-z0-9_-]{1,48}"
        .prop_map(|raw| raw.trim_matches('-').trim_matches('_').to_owned())
        .prop_filter("generated key must not be blank", |key| !key.is_empty())
}

fn journal_line_case() -> impl Strategy<Value = JournalLineCase> {
    prop_oneof![
        (safe_text(), any::<bool>()).prop_map(|(body, command_failure)| {
            JournalLineCase::Valid {
                body,
                command_failure,
            }
        }),
        Just(JournalLineCase::MissingBody),
        Just(JournalLineCase::InvalidJson),
        Just(JournalLineCase::NonObject),
    ]
}

fn temp_workspace(prefix: &str) -> Result<(tempfile::TempDir, PathBuf), TestCaseError> {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .map_err(|error| TestCaseError::fail(error.to_string()))?;
    let workspace = dir
        .path()
        .canonicalize()
        .map_err(|error| TestCaseError::fail(error.to_string()))?;
    Ok((dir, workspace))
}

fn remember_options<'a>(workspace: &'a Path, content: &'a str) -> RememberMemoryOptions<'a> {
    RememberMemoryOptions {
        workspace_path: workspace,
        database_path: None,
        content,
        workflow_id: None,
        level: "episodic",
        kind: "fact",
        tags: None,
        confidence: 0.8,
        source: None,
        allow_secret_mention: false,
        valid_from: None,
        valid_to: None,
        dry_run: false,
        auto_link: false,
        propose_candidates: false,
    }
}

fn remember_batch_options(workspace: &Path) -> RememberBatchOptions<'_> {
    RememberBatchOptions {
        workspace_path: workspace,
        database_path: None,
        reinforce: false,
        dry_run: false,
        auto_link: false,
        propose_candidates: false,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn journal_jsonl_batch_preserves_per_line_isolation(
        lines in proptest::collection::vec(journal_line_case(), 1..24),
    ) {
        let (_dir, workspace) = temp_workspace("ee-journal-prop")?;
        let input = lines
            .iter()
            .enumerate()
            .map(|(index, line)| line.jsonl(index + 1))
            .collect::<Vec<_>>()
            .join("\n");
        let options = JournalAppendOptions {
            workspace_path: &workspace,
            database_path: None,
            agent_name: Some("OrangePike".to_owned()),
            source: JournalSource::Stdin,
        };

        let report = append_journal_entries_stdin(&options, &input)
            .map_err(|error| TestCaseError::fail(error.message()))?;
        let expected_stored = lines.iter().filter(|line| line.is_valid()).count();

        prop_assert_eq!(report.line_count, lines.len());
        prop_assert_eq!(report.stored_count, expected_stored);
        prop_assert_eq!(report.failed_count, lines.len() - expected_stored);
        prop_assert_eq!(report.results.len(), lines.len());
        prop_assert_eq!(report.all_failed(), expected_stored == 0);

        for (index, line) in lines.iter().enumerate() {
            let result = &report.results[index];
            prop_assert_eq!(result.line, index + 1);
            if line.is_valid() {
                prop_assert_eq!(result.status, "stored");
                prop_assert!(result.entry_id.as_deref().is_some_and(|id| id.starts_with("jrn_")));
                prop_assert_eq!(result.error_code, None);
            } else {
                prop_assert_eq!(result.status, "failed");
                prop_assert!(result.entry_id.is_none());
                prop_assert!(result.error_code.is_some());
            }
        }
    }

    #[test]
    fn remember_batch_idempotency_replay_returns_first_memory_id(
        body in safe_text(),
        key in safe_key(),
    ) {
        let (_dir, workspace) = temp_workspace("ee-remember-idem-prop")?;
        let content = format!("Remember property replay: {body}");
        let line = serde_json::json!({
            "content": content,
            "idempotencyKey": key,
        })
        .to_string();
        let input = format!("{line}\n{line}\n");

        let report = remember_memory_batch_stdin(&remember_batch_options(&workspace), &input)
            .map_err(|error| TestCaseError::fail(error.message()))?;

        prop_assert_eq!(report.line_count, 2);
        prop_assert_eq!(report.stored_count, 1);
        prop_assert_eq!(report.already_recorded_count, 1);
        prop_assert_eq!(report.failed_count, 0);
        prop_assert_eq!(report.results[0].status, "stored");
        prop_assert_eq!(report.results[1].status, "already_recorded");
        prop_assert_eq!(&report.results[0].memory_id, &report.results[1].memory_id);
    }

    #[test]
    fn direct_remember_idempotency_replay_returns_first_memory_id(
        body in safe_text(),
        key in safe_key(),
    ) {
        let (_dir, workspace) = temp_workspace("ee-remember-direct-idem-prop")?;
        let content = format!("Direct remember property replay: {body}");
        let controls = RememberWriteControls {
            reinforce: false,
            idempotency_key: Some(&key),
            defer_index_processing: true,
        };

        let created = remember_memory_with_controls(&remember_options(&workspace, &content), &controls)
            .map_err(|error| TestCaseError::fail(error.message()))?;
        let first_id = match created {
            RememberOutcome::Created(report) => report.memory_id.to_string(),
            other => return Err(TestCaseError::fail(format!("expected created, got {other:?}"))),
        };

        let replay = remember_memory_with_controls(&remember_options(&workspace, &content), &controls)
            .map_err(|error| TestCaseError::fail(error.message()))?;
        let replay_id = match replay {
            RememberOutcome::AlreadyRecorded(report) => report.memory_id,
            other => {
                return Err(TestCaseError::fail(format!(
                    "expected already recorded, got {other:?}"
                )));
            }
        };

        prop_assert_eq!(replay_id, first_id);
    }
}
