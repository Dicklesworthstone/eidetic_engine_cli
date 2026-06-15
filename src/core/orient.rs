//! Core helpers for agent orientation.
//!
//! Most `ee orient` assembly is currently performed in the CLI layer because
//! it composes several public commands. This module holds reusable read-only
//! data providers that should not be duplicated by the CLI renderer.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};

use super::decide::{DecideItem, DecideRevisitOptions, decide_revisit};
use crate::models::DomainError;

pub const ORIENT_DECISIONS_SCHEMA_V1: &str = "ee.orient.decisions.v1";

#[derive(Clone, Debug)]
pub struct OrientDecisionOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: Option<&'a Path>,
    pub warning_days: Option<u64>,
    pub limit: usize,
    pub now: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrientDecisionReport {
    pub schema: &'static str,
    pub due_count: usize,
    pub returned_count: usize,
    pub warning_days: u64,
    pub window_end: String,
    pub decisions: Vec<DecideItem>,
}

impl OrientDecisionReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

pub fn orient_decisions(
    options: &OrientDecisionOptions<'_>,
) -> Result<OrientDecisionReport, DomainError> {
    let revisit = decide_revisit(&DecideRevisitOptions {
        workspace_path: options.workspace_path,
        database_path: options.database_path,
        warning_days: options.warning_days,
        limit: options.limit,
        now: options.now,
    })?;
    Ok(OrientDecisionReport {
        schema: ORIENT_DECISIONS_SCHEMA_V1,
        due_count: revisit.due_count,
        returned_count: revisit.returned_count,
        warning_days: revisit.warning_days,
        window_end: revisit.window_end,
        decisions: revisit.decisions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::decide::{DecideRecordOptions, decide_record};

    type TestResult = Result<(), String>;

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn orient_decisions_reuses_due_decision_query() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(temp.path().join(".ee")).map_err(|error| error.to_string())?;
        let now = DateTime::parse_from_rfc3339("2026-06-15T12:00:00Z")
            .map_err(|error| error.to_string())?
            .with_timezone(&Utc);
        decide_record(&DecideRecordOptions {
            workspace_path: temp.path(),
            database_path: None,
            topic: "Prompt format",
            chosen: "markdown",
            alternatives: vec!["json".to_owned()],
            rationale: "Humans can scan it quickly.",
            revisit_by: Some("2026-06-16T12:00:00Z"),
            supersedes: None,
            dry_run: false,
            actor: Some("test"),
            now: Some(now),
        })
        .map_err(|error| error.to_string())?;

        let report = orient_decisions(&OrientDecisionOptions {
            workspace_path: temp.path(),
            database_path: None,
            warning_days: Some(2),
            limit: 10,
            now: Some(now),
        })
        .map_err(|error| error.to_string())?;
        ensure_equal(&report.schema, &ORIENT_DECISIONS_SCHEMA_V1, "schema")?;
        ensure_equal(&report.due_count, &1, "due count")?;
        ensure_equal(
            &report.decisions[0].topic,
            &"Prompt format".to_owned(),
            "decision topic",
        )
    }
}
