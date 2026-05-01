use std::path::PathBuf;
use std::{env, fs};

type TestResult = Result<(), String>;

pub struct GoldenTest {
    name: String,
    category: String,
}

impl GoldenTest {
    #[must_use]
    pub fn new(category: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            category: category.into(),
        }
    }

    fn golden_path(&self) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("golden")
            .join(&self.category)
            .join(format!("{}.golden", self.name))
    }

    pub fn assert_eq(&self, actual: &str) -> TestResult {
        let update_mode = env::var("UPDATE_GOLDEN").is_ok();

        if update_mode {
            self.update_golden(actual)?;
            return Ok(());
        }

        let expected = self.load_golden()?;
        if actual == expected {
            Ok(())
        } else {
            Err(self.format_diff(&expected, actual))
        }
    }

    fn load_golden(&self) -> Result<String, String> {
        let path = self.golden_path();
        fs::read_to_string(&path).map_err(|error| {
            format!(
                "Golden file not found: {}\nRun with UPDATE_GOLDEN=1 to create it.\nError: {}",
                path.display(),
                error
            )
        })
    }

    fn update_golden(&self, content: &str) -> TestResult {
        let path = self.golden_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Failed to create golden directory {}: {}",
                    parent.display(),
                    error
                )
            })?;
        }
        fs::write(&path, content).map_err(|error| {
            format!("Failed to write golden file {}: {}", path.display(), error)
        })?;
        eprintln!("Updated golden file: {}", path.display());
        Ok(())
    }

    fn format_diff(&self, expected: &str, actual: &str) -> String {
        let expected_lines: Vec<&str> = expected.lines().collect();
        let actual_lines: Vec<&str> = actual.lines().collect();

        let mut diff = String::new();
        diff.push_str(&format!(
            "Golden test '{}::{}' failed.\n",
            self.category, self.name
        ));
        diff.push_str(&format!("Golden file: {}\n", self.golden_path().display()));
        diff.push_str("Run with UPDATE_GOLDEN=1 to update the golden file.\n\n");

        diff.push_str("--- expected\n");
        diff.push_str("+++ actual\n\n");

        let max_lines = expected_lines.len().max(actual_lines.len());
        for i in 0..max_lines {
            let exp = expected_lines.get(i);
            let act = actual_lines.get(i);
            match (exp, act) {
                (Some(e), Some(a)) if e == a => {
                    diff.push_str(&format!("  {}\n", e));
                }
                (Some(e), Some(a)) => {
                    diff.push_str(&format!("- {}\n", e));
                    diff.push_str(&format!("+ {}\n", a));
                }
                (Some(e), None) => {
                    diff.push_str(&format!("- {}\n", e));
                }
                (None, Some(a)) => {
                    diff.push_str(&format!("+ {}\n", a));
                }
                (None, None) => {}
            }
        }

        diff
    }
}

pub fn assert_golden(category: &str, name: &str, actual: &str) -> TestResult {
    GoldenTest::new(category, name).assert_eq(actual)
}

pub fn assert_json_golden(category: &str, name: &str, actual: &str) -> TestResult {
    let normalized = normalize_json_for_comparison(actual);
    GoldenTest::new(category, format!("{}.json", name)).assert_eq(&normalized)
}

fn normalize_json_for_comparison(json: &str) -> String {
    json.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
    use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
    use std::path::Path;
    use std::process::{Command, Output};
    use std::time::{SystemTime, UNIX_EPOCH};

    type TestResult = Result<(), String>;

    fn run_ee(args: &[&str]) -> Result<Output, String> {
        Command::new(env!("CARGO_BIN_EXE_ee"))
            .args(args)
            .output()
            .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
    }

    fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock moved backwards: {error}"))?
            .as_nanos();
        Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("ee-golden-artifacts")
            .join(format!("{prefix}-{}-{now}", std::process::id())))
    }

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

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

    fn ensure_json_number_close(
        actual: &serde_json::Value,
        expected: &serde_json::Value,
        tolerance: f64,
        context: &str,
    ) -> TestResult {
        let actual_number = actual
            .as_f64()
            .ok_or_else(|| format!("{context}: actual value must be numeric, got {actual:?}"))?;
        let expected_number = expected.as_f64().ok_or_else(|| {
            format!("{context}: expected value must be numeric, got {expected:?}")
        })?;

        ensure(
            (actual_number - expected_number).abs() <= tolerance,
            format!(
                "{context}: expected {expected_number:?} within {tolerance}, got {actual_number:?}"
            ),
        )
    }

    fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
        ensure(
            haystack.contains(needle),
            format!("{context}: expected to contain {needle:?}, got {haystack:?}"),
        )
    }

    fn assert_agent_stdout_golden(args: &[&str], name: &str, expect_success: bool) -> TestResult {
        let output = run_ee(args)?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("stdout was not UTF-8 for ee {}: {error}", args.join(" ")))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("stderr was not UTF-8 for ee {}: {error}", args.join(" ")))?;

        ensure(
            output.status.success() == expect_success,
            format!(
                "ee {} exit status mismatch: got {:?}, stderr: {stderr}",
                args.join(" "),
                output.status.code()
            ),
        )?;
        ensure(
            stderr.is_empty(),
            format!("ee {} must keep diagnostics out of stderr", args.join(" ")),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("ee {} stdout must start with JSON data", args.join(" ")),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("ee {} stdout must end with a newline", args.join(" ")),
        )?;

        assert_golden("agent", name, &stdout)
    }

    fn seed_search_workspace(workspace: &Path, database: &Path) -> TestResult {
        if let Some(parent) = database.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create database parent {}: {error}",
                    parent.display()
                )
            })?;
        }

        let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_searchjson0000000000000001",
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("search-json-contract".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000001",
                &CreateMemoryInput {
                    workspace_id: "wsp_searchjson0000000000000001".to_string(),
                    level: "procedural".to_string(),
                    kind: "rule".to_string(),
                    content: "Run cargo fmt --check before release.".to_string(),
                    confidence: 0.92,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("file://AGENTS.md#L164-173".to_string()),
                    trust_class: "human_explicit".to_string(),
                    trust_subclass: Some("project-rule".to_string()),
                    valid_from: None,
                    valid_to: None,
                    tags: vec!["cargo".to_string(), "formatting".to_string()],
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "UPDATE memories SET created_at = '2026-04-29T12:00:00+00:00', updated_at = '2026-04-29T12:00:00+00:00' WHERE id = 'mem_00000000000000000000000001'",
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())
    }

    fn seed_pack_selection(database: &Path) -> TestResult {
        let connection = DbConnection::open_file(database).map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "INSERT INTO pack_records (id, workspace_id, query, profile, max_tokens, used_tokens, item_count, omitted_count, pack_hash, degraded_json, created_at, created_by) VALUES ('pack_00000000000000000000000001', 'wsp_searchjson0000000000000001', 'format before release', 'compact', 4000, 8, 1, 0, 'blake3:test-pack-hash', NULL, '2026-04-29T12:01:00+00:00', 'golden-test')",
            )
            .map_err(|error| error.to_string())?;
        connection
            .execute_raw(
                "INSERT INTO pack_items (pack_id, memory_id, rank, section, estimated_tokens, relevance, utility, why, diversity_key) VALUES ('pack_00000000000000000000000001', 'mem_00000000000000000000000001', 1, 'procedural_rules', 8, 0.91, 0.8, 'Selected because the memory matches release-formatting work.', 'procedural:rule:cargo')",
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())
    }

    fn build_search_index(workspace: &Path, database: &Path, index_dir: &Path) -> TestResult {
        let report = rebuild_index(&IndexRebuildOptions {
            workspace_path: workspace.to_path_buf(),
            database_path: Some(database.to_path_buf()),
            index_dir: Some(index_dir.to_path_buf()),
            dry_run: false,
        })
        .map_err(|error| error.to_string())?;

        ensure_equal(
            &report.status,
            &IndexRebuildStatus::Success,
            "index rebuild status",
        )?;
        ensure_equal(&report.documents_total, &1, "indexed document count")
    }

    #[test]
    fn golden_path_uses_manifest_dir_and_category() -> TestResult {
        let test = GoldenTest::new("status", "json_output");
        let path = test.golden_path();
        let path_str = path.to_string_lossy();
        ensure_contains(&path_str, "tests/fixtures/golden/status", "path structure")?;
        ensure_contains(&path_str, "json_output.golden", "file name")
    }

    #[test]
    fn format_diff_shows_line_differences() -> TestResult {
        let test = GoldenTest::new("test", "diff");
        let expected = "line1\nline2\nline3";
        let actual = "line1\nchanged\nline3";
        let diff = test.format_diff(expected, actual);
        ensure_contains(&diff, "- line2", "removed line")?;
        ensure_contains(&diff, "+ changed", "added line")?;
        ensure_contains(&diff, "  line1", "unchanged line")
    }

    #[test]
    fn agent_status_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(&["status", "--json"], "status.json", true)
    }

    #[test]
    fn agent_doctor_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(&["doctor", "--json"], "doctor.json", true)
    }

    #[test]
    fn agent_docs_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(&["--agent-docs"], "agent_docs.json", true)
    }

    #[test]
    fn agent_health_unavailable_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(&["--json", "health"], "health_unavailable.json", true)
    }

    #[test]
    fn agent_search_unavailable_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(
            &["--json", "search", "format-before-release"],
            "search_unavailable.json",
            false,
        )
    }

    #[test]
    fn agent_search_json_returns_indexed_memory() -> TestResult {
        let artifact_dir = unique_artifact_dir("search-json")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;
        build_search_index(&workspace, &database, &index_dir)?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("--workspace")
            .arg(&workspace)
            .arg("search")
            .arg("format before release")
            .arg("--database")
            .arg(&database)
            .arg("--index-dir")
            .arg(&index_dir)
            .output()
            .map_err(|error| format!("failed to run ee search --json: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("search stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("search stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("search --json should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("search --json stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("search stdout must start with JSON data, got: {stdout:?}"),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("search stdout must end with a newline, got: {stdout:?}"),
        )?;

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "search schema",
        )?;
        ensure_equal(
            &value["success"],
            &serde_json::json!(true),
            "search success",
        )?;
        ensure_equal(
            &value["data"]["command"],
            &serde_json::json!("search"),
            "search command",
        )?;
        ensure_equal(
            &value["data"]["status"],
            &serde_json::json!("success"),
            "search status",
        )?;
        ensure_equal(
            &value["data"]["result_count"],
            &serde_json::json!(1),
            "search result count",
        )?;
        ensure_equal(
            &value["data"]["results"][0]["doc_id"],
            &serde_json::json!("mem_00000000000000000000000001"),
            "search result memory id",
        )?;
        ensure_equal(
            &value["data"]["metrics"]["requested_limit"],
            &serde_json::json!(10),
            "search metrics requested limit",
        )?;
        ensure_equal(
            &value["data"]["metrics"]["returned_count"],
            &serde_json::json!(1),
            "search metrics returned count",
        )?;
        ensure_equal(
            &value["data"]["metrics"]["error_count"],
            &serde_json::json!(0),
            "search metrics error count",
        )?;
        ensure_json_number_close(
            &value["data"]["metrics"]["score_distribution"]["top"],
            &value["data"]["results"][0]["score"],
            0.000_001,
            "search metrics top score",
        )?;
        let source = value["data"]["results"][0]["source"]
            .as_str()
            .ok_or_else(|| "search result source must be a string".to_string())?;
        ensure_equal(
            &value["data"]["metrics"]["source_counts"][source],
            &serde_json::json!(1),
            "search metrics source count",
        )
    }

    #[test]
    fn agent_context_json_returns_indexed_memory() -> TestResult {
        let artifact_dir = unique_artifact_dir("context-json")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;
        build_search_index(&workspace, &database, &index_dir)?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("--workspace")
            .arg(&workspace)
            .arg("context")
            .arg("format before release")
            .arg("--database")
            .arg(&database)
            .arg("--index-dir")
            .arg(&index_dir)
            .arg("--profile")
            .arg("compact")
            .arg("--max-tokens")
            .arg("4000")
            .arg("--candidate-pool")
            .arg("10")
            .output()
            .map_err(|error| format!("failed to run ee context --json: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("context stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("context stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("context --json should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("context --json stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("context stdout must start with JSON data, got: {stdout:?}"),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("context stdout must end with a newline, got: {stdout:?}"),
        )?;

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "context schema",
        )?;
        ensure_equal(
            &value["success"],
            &serde_json::json!(true),
            "context success",
        )?;
        ensure_equal(
            &value["data"]["command"],
            &serde_json::json!("context"),
            "context command",
        )?;
        ensure_equal(
            &value["data"]["request"]["query"],
            &serde_json::json!("format before release"),
            "context request query",
        )?;
        ensure_equal(
            &value["data"]["request"]["profile"],
            &serde_json::json!("compact"),
            "context request profile",
        )?;
        ensure_equal(
            &value["data"]["request"]["candidatePool"],
            &serde_json::json!(10),
            "context request candidate pool",
        )?;
        ensure_equal(
            &value["data"]["pack"]["selectionCertificate"]["objective"],
            &serde_json::json!("mmr_redundancy"),
            "context selection certificate objective",
        )?;
        ensure_equal(
            &value["data"]["pack"]["selectionCertificate"]["steps"][0]["memoryId"],
            &serde_json::json!("mem_00000000000000000000000001"),
            "context selection certificate memory id",
        )?;

        let items = value["data"]["pack"]["items"]
            .as_array()
            .ok_or_else(|| "context pack items must be an array".to_string())?;
        ensure_equal(&items.len(), &1_usize, "context item count")?;
        ensure_equal(
            &items[0]["memoryId"],
            &serde_json::json!("mem_00000000000000000000000001"),
            "context item memory id",
        )?;
        ensure_contains(
            items[0]["content"].as_str().unwrap_or_default(),
            "cargo fmt --check",
            "context item content",
        )?;

        let provenance = items[0]["provenance"]
            .as_array()
            .ok_or_else(|| "context item provenance must be an array".to_string())?;
        ensure(
            !provenance.is_empty(),
            "context item provenance must be present",
        )?;

        let normalized = normalize_context_pack_json(&stdout);
        assert_golden("agent", "context_pack.json", &normalized)
    }

    #[test]
    fn agent_context_markdown_returns_formatted_pack() -> TestResult {
        let artifact_dir = unique_artifact_dir("context-markdown")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        let index_dir = workspace.join(".ee").join("index");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;
        build_search_index(&workspace, &database, &index_dir)?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--format")
            .arg("markdown")
            .arg("--workspace")
            .arg(&workspace)
            .arg("context")
            .arg("format before release")
            .arg("--database")
            .arg(&database)
            .arg("--index-dir")
            .arg(&index_dir)
            .arg("--profile")
            .arg("compact")
            .arg("--max-tokens")
            .arg("4000")
            .output()
            .map_err(|error| format!("failed to run ee context --format markdown: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("context markdown stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("context markdown stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("context --format markdown should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("context --format markdown stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('#'),
            format!("context markdown must start with # header, got: {stdout:?}"),
        )?;
        ensure_contains(&stdout, "Context Pack:", "should have pack header")?;
        ensure_contains(&stdout, "format before release", "should have query")?;
        ensure_contains(&stdout, "cargo fmt --check", "should have memory content")?;

        assert_golden("agent", "context_pack.md", &stdout)
    }

    fn normalize_context_pack_json(json: &str) -> String {
        let mut value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return json.to_string(),
        };

        if let Some(data) = value.get_mut("data") {
            if let Some(pack) = data.get_mut("pack") {
                if pack.get("elapsedMs").is_some() {
                    pack["elapsedMs"] = serde_json::json!(0.0);
                }
            }
        }

        serde_json::to_string_pretty(&value).unwrap_or_else(|_| json.to_string()) + "\n"
    }

    #[test]
    fn agent_why_json_explains_pack_selected_memory() -> TestResult {
        let artifact_dir = unique_artifact_dir("why-json")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;
        seed_pack_selection(&database)?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("--workspace")
            .arg(&workspace)
            .arg("why")
            .arg("mem_00000000000000000000000001")
            .arg("--database")
            .arg(&database)
            .output()
            .map_err(|error| format!("failed to run ee why --json: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("why stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("why stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("why --json should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("why --json stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("why stdout must start with JSON data, got: {stdout:?}"),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("why stdout must end with a newline, got: {stdout:?}"),
        )?;

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "why schema",
        )?;
        ensure_equal(&value["success"], &serde_json::json!(true), "why success")?;
        ensure_equal(
            &value["data"]["command"],
            &serde_json::json!("why"),
            "why command",
        )?;
        ensure_equal(
            &value["data"]["found"],
            &serde_json::json!(true),
            "why found",
        )?;
        ensure_equal(
            &value["data"]["selection"]["latestPackSelection"]["packId"],
            &serde_json::json!("pack_00000000000000000000000001"),
            "why latest pack id",
        )?;
        ensure_equal(
            &value["data"]["selection"]["latestPackSelection"]["rank"],
            &serde_json::json!(1),
            "why latest pack rank",
        )?;

        assert_golden("agent", "why_selected.json", &stdout)
    }

    #[test]
    fn agent_outcome_json_records_feedback_and_audit() -> TestResult {
        let artifact_dir = unique_artifact_dir("outcome-json")?;
        let workspace = artifact_dir.join("workspace");
        let database = workspace.join(".ee").join("ee.db");
        fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "failed to create workspace {}: {error}",
                workspace.display()
            )
        })?;

        seed_search_workspace(&workspace, &database)?;

        let output = Command::new(env!("CARGO_BIN_EXE_ee"))
            .arg("--json")
            .arg("--workspace")
            .arg(&workspace)
            .arg("outcome")
            .arg("mem_00000000000000000000000001")
            .arg("--database")
            .arg(&database)
            .arg("--signal")
            .arg("helpful")
            .arg("--source-type")
            .arg("human_explicit")
            .arg("--source-id")
            .arg("golden-run")
            .arg("--reason")
            .arg("The memory prevented a release workflow mistake.")
            .arg("--evidence-json")
            .arg(r#"{"outcome":"success","redacted":true}"#)
            .arg("--event-id")
            .arg("fb_31234567890123456789012345")
            .arg("--actor")
            .arg("golden-test")
            .output()
            .map_err(|error| format!("failed to run ee outcome --json: {error}"))?;

        let stdout = String::from_utf8(output.stdout)
            .map_err(|error| format!("outcome stdout was not UTF-8: {error}"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|error| format!("outcome stderr was not UTF-8: {error}"))?;

        ensure(
            output.status.success(),
            format!("outcome --json should succeed; stderr: {stderr}"),
        )?;
        ensure(
            stderr.is_empty(),
            format!("outcome --json stderr must be empty, got: {stderr:?}"),
        )?;
        ensure(
            stdout.starts_with('{'),
            format!("outcome stdout must start with JSON data, got: {stdout:?}"),
        )?;
        ensure(
            stdout.ends_with('\n'),
            format!("outcome stdout must end with a newline, got: {stdout:?}"),
        )?;
        ensure(
            !stdout.contains(r#""redacted""#),
            "outcome output must not echo evidence JSON keys",
        )?;

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|error| error.to_string())?;
        ensure_equal(
            &value["schema"],
            &serde_json::json!("ee.response.v1"),
            "outcome schema",
        )?;
        ensure_equal(
            &value["success"],
            &serde_json::json!(true),
            "outcome success",
        )?;
        ensure_equal(
            &value["data"]["command"],
            &serde_json::json!("outcome"),
            "outcome command",
        )?;
        ensure_equal(
            &value["data"]["status"],
            &serde_json::json!("recorded"),
            "outcome status",
        )?;
        ensure_equal(
            &value["data"]["event"]["id"],
            &serde_json::json!("fb_31234567890123456789012345"),
            "outcome event id",
        )?;
        ensure_equal(
            &value["data"]["event"]["evidenceJsonPresent"],
            &serde_json::json!(true),
            "outcome evidence presence",
        )?;
        ensure_equal(
            &value["data"]["feedback"]["totalCount"],
            &serde_json::json!(1),
            "outcome feedback count",
        )?;

        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        let event = connection
            .get_feedback_event("fb_31234567890123456789012345")
            .map_err(|error| error.to_string())?;
        ensure(event.is_some(), "feedback event must be durable")?;
        let audit = connection
            .list_audit_by_target("memory", "mem_00000000000000000000000001", None)
            .map_err(|error| error.to_string())?;
        ensure_equal(&audit.len(), &1_usize, "outcome audit row count")?;

        let normalized = normalize_outcome_json(&stdout);
        assert_golden("agent", "outcome_recorded.json", &normalized)
    }

    fn normalize_outcome_json(json: &str) -> String {
        let mut value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return json.to_string(),
        };

        if let Some(audit_id) = value
            .get_mut("data")
            .and_then(|data| data.get_mut("event"))
            .and_then(|event| event.get_mut("auditId"))
        {
            *audit_id = serde_json::json!("audit_DYNAMIC");
        }

        serde_json::to_string_pretty(&value).unwrap_or_else(|_| json.to_string()) + "\n"
    }

    #[test]
    fn agent_context_unavailable_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(
            &[
                "--json",
                "--workspace",
                "tests/fixtures/missing-ee-workspace",
                "context",
                "prepare-release",
            ],
            "context_unavailable.json",
            false,
        )
    }

    #[test]
    fn agent_api_version_unavailable_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(
            &["--json", "api-version"],
            "api_version_unavailable.json",
            false,
        )
    }

    // =========================================================================
    // Degradation Matrix Contract Tests (EE-311)
    // =========================================================================

    fn degradation_matrix_json() -> String {
        use ee::models::degradation::ALL_DEGRADATION_CODES;

        let codes: Vec<serde_json::Value> = ALL_DEGRADATION_CODES
            .iter()
            .map(|code| {
                serde_json::json!({
                    "id": code.id,
                    "subsystem": code.subsystem.as_str(),
                    "severity": code.severity.as_str(),
                    "description": code.description,
                    "behavior_change": code.behavior_change,
                    "auto_recoverable": code.auto_recoverable,
                    "repair": code.repair,
                })
            })
            .collect();

        let matrix = serde_json::json!({
            "schema": "ee.degradation_matrix.v1",
            "count": codes.len(),
            "codes": codes,
        });

        serde_json::to_string_pretty(&matrix).unwrap_or_default()
    }

    #[test]
    fn degradation_matrix_matches_golden() -> TestResult {
        let json = degradation_matrix_json();
        assert_golden("degradation", "matrix.json", &json)
    }

    #[test]
    fn degradation_matrix_all_codes_have_required_fields() -> TestResult {
        use ee::models::degradation::ALL_DEGRADATION_CODES;

        for code in ALL_DEGRADATION_CODES {
            ensure(!code.id.is_empty(), format!("code {:?} has empty id", code))?;
            ensure(
                code.id.starts_with('D'),
                format!("code {} id must start with 'D'", code.id),
            )?;
            ensure(
                !code.description.is_empty(),
                format!("code {} has empty description", code.id),
            )?;
            ensure(
                !code.behavior_change.is_empty(),
                format!("code {} has empty behavior_change", code.id),
            )?;
        }
        Ok(())
    }

    #[test]
    fn degradation_matrix_ids_are_unique() -> TestResult {
        use ee::models::degradation::ALL_DEGRADATION_CODES;
        use std::collections::HashSet;

        let mut seen = HashSet::new();
        for code in ALL_DEGRADATION_CODES {
            ensure(
                seen.insert(code.id),
                format!("duplicate degradation code id: {}", code.id),
            )?;
        }
        Ok(())
    }

    #[test]
    fn degradation_matrix_ids_are_sorted_by_number() -> TestResult {
        use ee::models::degradation::ALL_DEGRADATION_CODES;

        let numbers: Vec<u16> = ALL_DEGRADATION_CODES.iter().map(|c| c.number()).collect();
        for window in numbers.windows(2) {
            ensure(
                window[0] <= window[1],
                format!(
                    "degradation codes out of order: D{:03} > D{:03}",
                    window[0], window[1]
                ),
            )?;
        }
        Ok(())
    }

    #[test]
    fn degradation_matrix_honesty_checks_pass() -> TestResult {
        use ee::core::degraded_honesty::validate_all_codes;

        let report = validate_all_codes();
        if report.passed {
            Ok(())
        } else {
            let failures: Vec<String> = report
                .checks
                .iter()
                .filter(|c| !c.passed)
                .map(|c| {
                    format!(
                        "{}: {} (code: {:?})",
                        c.check_name,
                        c.issue.as_deref().unwrap_or("no details"),
                        c.code_id
                    )
                })
                .collect();
            Err(format!(
                "Honesty check failed with {} issues:\n{}",
                report.issue_count,
                failures.join("\n")
            ))
        }
    }

    #[test]
    fn degradation_matrix_repair_commands_are_valid() -> TestResult {
        use ee::core::degraded_honesty::validate_repair_command;
        use ee::models::degradation::ALL_DEGRADATION_CODES;

        for code in ALL_DEGRADATION_CODES {
            if let Some(repair) = code.repair {
                let result = validate_repair_command(repair);
                ensure(
                    result.passed,
                    format!(
                        "code {} has invalid repair command '{}': {}",
                        code.id,
                        repair,
                        result.issue.unwrap_or_default()
                    ),
                )?;
            }
        }
        Ok(())
    }

    #[test]
    fn degradation_matrix_subsystem_coverage() -> TestResult {
        use ee::models::degradation::{ALL_DEGRADATION_CODES, DegradedSubsystem};
        use std::collections::HashSet;

        let expected_subsystems = [
            DegradedSubsystem::Search,
            DegradedSubsystem::Storage,
            DegradedSubsystem::Cass,
            DegradedSubsystem::Graph,
            DegradedSubsystem::Pack,
            DegradedSubsystem::Curate,
            DegradedSubsystem::Policy,
            DegradedSubsystem::Network,
        ];

        let covered: HashSet<&str> = ALL_DEGRADATION_CODES
            .iter()
            .map(|c| c.subsystem.as_str())
            .collect();

        for subsystem in &expected_subsystems {
            ensure(
                covered.contains(subsystem.as_str()),
                format!("subsystem {} has no degradation codes", subsystem.as_str()),
            )?;
        }
        Ok(())
    }
}
