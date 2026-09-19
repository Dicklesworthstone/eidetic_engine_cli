//! Real-binary revision -> search/diagnostics -> read-only context acceptance.
//!
//! The fixture authors and revises through the CLI. Direct database reads only
//! observe the actual revision instant and retained history; they seed nothing.

use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ee::db::DbConnection;
use serde_json::{Value, json};

type TestResult = Result<(), String>;
const QUERY: &str = "quartz release checks manifest workflow";
const PRIOR_BODY: &str = "For quartz release checks use the legacy manifest workflow.";
const HEAD_BODY: &str = "For quartz release checks use the reviewed manifest workflow.";

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    home: PathBuf,
    artifacts: PathBuf,
    ordinal: usize,
}

impl Fixture {
    fn new() -> Result<Self, String> {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let physical = root.path().canonicalize().map_err(|e| e.to_string())?;
        let workspace = physical.join("workspace");
        let home = physical.join("home");
        fs::create_dir(&workspace).map_err(|e| e.to_string())?;
        fs::create_dir(&home).map_err(|e| e.to_string())?;
        let artifacts = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("revision-search-cli")
            .join(uuid::Uuid::now_v7().to_string());
        fs::create_dir_all(&artifacts).map_err(|e| e.to_string())?;
        Ok(Self {
            _root: root,
            workspace,
            home,
            artifacts,
            ordinal: 0,
        })
    }

    fn run(&mut self, args: &[&str]) -> Result<Value, String> {
        self.ordinal += 1;
        let prefix = self.artifacts.join(format!("{:02}", self.ordinal));
        let stdout_path = prefix.with_extension("stdout");
        let stderr_path = prefix.with_extension("stderr");
        let stdout = File::create(&stdout_path).map_err(|e| e.to_string())?;
        let stderr = File::create(&stderr_path).map_err(|e| e.to_string())?;
        let started = Instant::now();
        let mut child = Command::new(env!("CARGO_BIN_EXE_ee"))
            .args(args)
            .args(["--json", "--workspace"])
            .arg(&self.workspace)
            .current_dir(&self.workspace)
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_WORKSPACE_REGISTRY")
            .env_remove("EE_DATABASE_PATH")
            .env_remove("EE_INDEX_DIR")
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.home)
            .env("EE_EMBED_DOWNLOAD", "off")
            .env("EE_NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if started.elapsed() > Duration::from_secs(120) {
                timed_out = true;
                child.kill().map_err(|e| e.to_string())?;
                break child.wait().map_err(|e| e.to_string())?;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let stdout = fs::read(&stdout_path).map_err(|e| e.to_string())?;
        let parsed = serde_json::from_slice::<Value>(&stdout);
        let envelope_valid = parsed
            .as_ref()
            .is_ok_and(|value| value["schema"] == "ee.response.v2" && value["success"] == true);
        let event = json!({
            "test": "revision_search_cli",
            "command": args, "workspace": self.workspace,
            "environment": {"HOME": self.home, "XDG_DATA_HOME": self.home,
                "XDG_CONFIG_HOME": self.home, "EE_EMBED_DOWNLOAD": "off"},
            "elapsedMs": started.elapsed().as_millis(), "exitCode": status.code(),
            "timedOut": timed_out, "stdoutPath": stdout_path, "stderrPath": stderr_path,
            "envelopeValidated": envelope_valid,
        });
        fs::write(prefix.with_extension("json"), event.to_string()).map_err(|e| e.to_string())?;
        if timed_out || !status.success() || !envelope_valid {
            return Err(format!(
                "ee {args:?}: status={status}, timeout={timed_out}, machine envelope={envelope_valid}; artifacts={}\nstdout={}\nstderr={}",
                self.artifacts.display(),
                String::from_utf8_lossy(&stdout),
                fs::read_to_string(&stderr_path).map_err(|e| e.to_string())?,
            ));
        }
        parsed.map_err(|e| e.to_string())
    }

    fn revised(&mut self) -> Result<(String, String, String, PathBuf), String> {
        self.run(&["init", "--skip-boilerplate"])?;
        let remembered = self.run(&[
            "remember",
            PRIOR_BODY,
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--valid-from",
            "2026-01-01T00:00:00Z",
            "--valid-to",
            "2099-01-01T00:00:00Z",
            "--no-auto-link",
            "--no-propose-candidates",
        ])?;
        let prior = string(&remembered, "/data/memoryId")?.to_owned();
        let database = PathBuf::from(string(&remembered, "/data/database_path")?);
        let revised = self.run(&["memory", "revise", &prior, "--content", HEAD_BODY])?;
        assert_eq!(revised["data"]["original_id"], prior);
        assert_eq!(revised["data"]["persisted"], true);
        let head = string(&revised, "/data/new_id")?.to_owned();
        assert_ne!(head, prior);
        let db = DbConnection::open_file_read_only(&database).map_err(|e| e.to_string())?;
        let cutoff = db
            .get_memory_superseded_at(&prior)
            .map_err(|e| e.to_string())?
            .ok_or("prior was not superseded")?;
        let old = db
            .get_memory(&prior)
            .map_err(|e| e.to_string())?
            .ok_or("prior missing")?;
        let new = db
            .get_memory(&head)
            .map_err(|e| e.to_string())?
            .ok_or("head missing")?;
        assert_eq!(old.content, PRIOR_BODY);
        assert_eq!(old.valid_to.as_deref(), Some("2099-01-01T00:00:00Z"));
        assert_eq!(new.content, HEAD_BODY);
        assert_eq!(new.valid_from.as_deref(), Some(cutoff.as_str()));
        assert_eq!(
            db.get_memory_superseded_at(&head)
                .map_err(|e| e.to_string())?,
            None
        );
        assert_eq!(
            db.get_memory_logical_id(&head).map_err(|e| e.to_string())?,
            Some(prior.clone())
        );
        assert_eq!(
            db.count_table_rows("memories").map_err(|e| e.to_string())?,
            2
        );
        db.close().map_err(|e| e.to_string())?;
        Ok((prior, head, cutoff, database))
    }
}

fn string<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing nonempty string at {pointer}"))
}

fn ids<'a>(value: &'a Value, pointer: &str, key: &str) -> Result<Vec<&'a str>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing result array at {pointer}"))?
        .iter()
        .map(|row| {
            row.get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("missing {key} in {pointer}"))
        })
        .collect()
}

#[test]
fn revised_advice_is_current_in_cli_search_diagnostics_and_read_only_packs() -> TestResult {
    let mut fixture = Fixture::new()?;
    let (prior, head, cutoff, database) = fixture.revised()?;
    fixture.run(&["index", "rebuild"])?;
    let search = fixture.run(&[
        "search",
        QUERY,
        "--source-mode",
        "lexical_only",
        "--relevance-floor",
        "0",
        "--include-expired",
        "--include-stale",
    ])?;
    assert_eq!(ids(&search, "/data/results", "id")?, vec![head.as_str()]);
    let diag = fixture.run(&[
        "diag",
        "search",
        QUERY,
        "--all-arms",
        "--relevance-floor",
        "0",
    ])?;
    assert_eq!(
        ids(&diag, "/data/final/results", "id")?,
        vec![head.as_str()]
    );
    assert_eq!(
        ids(&diag, "/data/preFusion/lexical/results", "docId")?,
        vec![head.as_str()]
    );
    for pointer in [
        "/data/preFusion/semanticFast/results",
        "/data/fusion/perDocContribution",
    ] {
        assert!(
            !ids(&diag, pointer, "docId")?.contains(&prior.as_str()),
            "{pointer}"
        );
    }
    let db = DbConnection::open_file_read_only(&database).map_err(|e| e.to_string())?;
    let pack_rows = db
        .count_table_rows("pack_records")
        .map_err(|e| e.to_string())?;
    db.close().map_err(|e| e.to_string())?;
    let args = [
        "pack",
        QUERY,
        "--source-mode",
        "lexical_only",
        "--read-only",
        "--as-of",
        cutoff.as_str(),
        "--max-tokens",
        "1500",
    ];
    let first = fixture.run(&args)?;
    assert_eq!(
        ids(&first, "/data/pack/items", "memoryId")?,
        vec![head.as_str()]
    );
    let second = fixture.run(&args)?;
    assert_eq!(
        ids(&second, "/data/pack/items", "memoryId")?,
        vec![head.as_str()]
    );
    assert_eq!(
        string(&first, "/data/pack/hash")?,
        string(&second, "/data/pack/hash")?
    );
    let db = DbConnection::open_file_read_only(&database).map_err(|e| e.to_string())?;
    assert_eq!(
        db.count_table_rows("pack_records")
            .map_err(|e| e.to_string())?,
        pack_rows
    );
    assert_eq!(
        db.count_table_rows("memories").map_err(|e| e.to_string())?,
        2
    );
    db.close().map_err(|e| e.to_string())?;
    Ok(())
}

#[test]
fn historical_cli_search_selects_the_prior_until_the_exact_revision_boundary() -> TestResult {
    let mut fixture = Fixture::new()?;
    let (prior, head, cutoff, _database) = fixture.revised()?;
    let at = DateTime::parse_from_rfc3339(&cutoff)
        .map_err(|e| e.to_string())?
        .with_timezone(&Utc);
    let before = at
        .checked_sub_signed(chrono::Duration::nanoseconds(1))
        .ok_or("revision timestamp underflow")?
        .to_rfc3339();
    fixture.run(&["index", "rebuild"])?;
    for (reference, expected) in [
        (before.as_str(), prior.as_str()),
        (cutoff.as_str(), head.as_str()),
    ] {
        let search = fixture.run(&[
            "search",
            QUERY,
            "--source-mode",
            "lexical_only",
            "--relevance-floor",
            "0",
            "--as-of",
            reference,
        ])?;
        assert_eq!(
            ids(&search, "/data/results", "id")?,
            vec![expected],
            "{reference}"
        );
    }
    Ok(())
}
