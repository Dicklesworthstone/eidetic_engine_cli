use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Once;

use serde_json::Value;
use tracing_subscriber::fmt::MakeWriter;

static INIT_TRACING: Once = Once::new();

thread_local! {
    static TRACE_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestPhase {
    Setup,
    Exercise,
    Verify,
    Teardown,
}

impl TestPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Exercise => "exercise",
            Self::Verify => "verify",
            Self::Teardown => "teardown",
        }
    }
}

#[derive(Debug)]
pub struct TestTraceGuard {
    bead_id: String,
    request_id: String,
    surface: String,
    test_name: String,
    workspace_id: String,
    #[allow(dead_code)]
    path: PathBuf,
}

impl TestTraceGuard {
    #[must_use]
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn event(
        &self,
        phase: TestPhase,
        fixture_name: &str,
        actual: impl ToString,
        expected: impl ToString,
        message: &str,
    ) {
        tracing::info!(
            target: "ee.test_tracing",
            workspace_id = %self.workspace_id,
            request_id = %self.request_id,
            bead_id = %self.bead_id,
            surface = %self.surface,
            test_name = %self.test_name,
            phase = phase.as_str(),
            elapsed_ms = 0_u64,
            degraded_codes = ?Vec::<String>::new(),
            fixture_name = fixture_name,
            actual = %actual.to_string(),
            expected = %expected.to_string(),
            "{message}"
        );
    }

    pub fn setup(&self, fixture_name: &str, message: &str) {
        self.event(TestPhase::Setup, fixture_name, "", "", message);
    }

    pub fn exercise(&self, fixture_name: &str, actual: impl ToString, message: &str) {
        self.event(TestPhase::Exercise, fixture_name, actual, "", message);
    }

    pub fn verify(
        &self,
        fixture_name: &str,
        actual: impl ToString,
        expected: impl ToString,
        message: &str,
    ) {
        self.event(TestPhase::Verify, fixture_name, actual, expected, message);
    }

    pub fn teardown(&self, fixture_name: &str, message: &str) {
        self.event(TestPhase::Teardown, fixture_name, "", "", message);
    }
}

impl Drop for TestTraceGuard {
    fn drop(&mut self) {
        TRACE_PATH.with(|path| {
            path.borrow_mut().take();
        });
    }
}

#[must_use]
pub fn init_test_tracing(bead_id: &str, test_name: &str) -> TestTraceGuard {
    INIT_TRACING.call_once(|| {
        tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .with_current_span(false)
            .with_span_list(false)
            .with_target(true)
            .with_thread_ids(false)
            .with_thread_names(false)
            .with_file(false)
            .with_line_number(false)
            .with_writer(TestTraceWriter)
            .init();
    });

    let path = test_trace_path(bead_id, test_name);
    if path.exists() {
        let _ = std::fs::write(&path, "");
    }
    TRACE_PATH.with(|slot| {
        *slot.borrow_mut() = Some(path.clone());
    });

    TestTraceGuard {
        bead_id: bead_id.to_owned(),
        request_id: format!("{bead_id}:{test_name}"),
        surface: "e2e_test_logging_convention".to_owned(),
        test_name: test_name.to_owned(),
        workspace_id: "test-workspace".to_owned(),
        path,
    }
}

#[allow(dead_code)]
pub fn normalize_trace_jsonl(path: &Path) -> Result<String, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut normalized = String::new();
    for (line_number, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "trace line {} is not valid JSON: {error}; line={line}",
                line_number + 1
            )
        })?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("trace line {} is not a JSON object", line_number + 1))?;
        if object.get("target").and_then(Value::as_str) != Some("ee.test_tracing") {
            continue;
        }
        let fields = object
            .get("fields")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("trace line {} is missing fields", line_number + 1))?;

        let mut sorted_fields = BTreeMap::new();
        for (key, value) in fields {
            sorted_fields.insert(key.clone(), value.clone());
        }
        let mut normalized_line = BTreeMap::new();
        normalized_line.insert("fields", serde_json::json!(sorted_fields));
        normalized_line.insert("level", object.get("level").cloned().unwrap_or(Value::Null));
        normalized_line.insert(
            "target",
            object.get("target").cloned().unwrap_or(Value::Null),
        );
        normalized_line.insert("timestamp", Value::String("[TIMESTAMP]".to_owned()));
        normalized.push_str(
            &serde_json::to_string(&normalized_line)
                .map_err(|error| format!("failed to render normalized trace line: {error}"))?,
        );
        normalized.push('\n');
    }
    Ok(normalized)
}

fn test_trace_path(bead_id: &str, test_name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-test-tracing")
        .join(sanitize_path_segment(bead_id))
        .join(format!("{}.jsonl", sanitize_path_segment(test_name)))
}

fn sanitize_path_segment(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct TestTraceWriter;

impl<'a> MakeWriter<'a> for TestTraceWriter {
    type Writer = Box<dyn Write + Send + 'a>;

    fn make_writer(&'a self) -> Self::Writer {
        let Some(path) = TRACE_PATH.with(|slot| slot.borrow().clone()) else {
            return Box::new(io::sink());
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => {
                if std::env::var_os("EE_TEST_TRACE").is_some() {
                    Box::new(FanoutWriter {
                        file,
                        stdout: Some(io::stdout()),
                    })
                } else {
                    Box::new(file)
                }
            }
            Err(_) => Box::new(io::sink()),
        }
    }
}

struct FanoutWriter {
    file: File,
    stdout: Option<io::Stdout>,
}

impl Write for FanoutWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write_all(buf)?;
        if let Some(stdout) = &mut self.stdout {
            stdout.write_all(buf)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()?;
        if let Some(stdout) = &mut self.stdout {
            stdout.flush()?;
        }
        Ok(())
    }
}
