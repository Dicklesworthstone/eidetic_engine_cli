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
                format!("Failed to create golden directory {}: {}", parent.display(), error)
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

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
        ensure(
            haystack.contains(needle),
            format!("{context}: expected to contain {needle:?}, got {haystack:?}"),
        )
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
}
