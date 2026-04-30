use std::env;
use std::fs;
use std::path::PathBuf;

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
                "mem_searchjson0000000000000001",
                &CreateMemoryInput {
                    workspace_id: "wsp_searchjson0000000000000001".to_string(),
                    level: "procedural".to_string(),
                    kind: "rule".to_string(),
                    content: "Run cargo fmt --check before release.".to_string(),
                    confidence: 0.92,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("file://AGENTS.md#compiler-checks".to_string()),
                    trust_class: "human_explicit".to_string(),
                    trust_subclass: Some("project-rule".to_string()),
                    tags: vec!["cargo".to_string(), "formatting".to_string()],
                },
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
            &serde_json::json!("mem_searchjson0000000000000001"),
            "search result memory id",
        )
    }

    #[test]
    fn agent_context_unavailable_json_matches_golden() -> TestResult {
        assert_agent_stdout_golden(
            &["--json", "context", "prepare-release"],
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
}
