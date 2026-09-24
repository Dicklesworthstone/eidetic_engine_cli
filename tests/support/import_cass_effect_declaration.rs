//! bd-kmsd6: the EffectManifest declaration of `import cass` matches what the
//! command writes, both ways. Every table a triggering scenario writes is
//! declared, and every declared table is written by some scenario. Bounded to
//! `import cass` (GraniteKite ruling C); the scenarios are the ones measured in
//! bd-kmsd6 probe v4 (c10175).

use super::*;
use std::cell::Cell;
use std::collections::BTreeMap;

type TableSet = BTreeSet<String>;

/// Written implies declared for every scenario, and declared implies written
/// by some scenario. An empty declaration or an empty union is red, never a
/// vacuous pass.
fn two_way_declaration_check(declared: &TableSet, written: &[(&str, TableSet)]) -> TestResult {
    if declared.is_empty() {
        return Err("empty world: import cass declares no db_tables".to_owned());
    }
    let union: TableSet = written
        .iter()
        .flat_map(|(_, tables)| tables.iter().cloned())
        .collect();
    if union.is_empty() {
        return Err("empty world: no scenario wrote any table".to_owned());
    }
    let mut problems = Vec::new();
    for (scenario, tables) in written {
        let undeclared: Vec<&String> = tables.difference(declared).collect();
        if !undeclared.is_empty() {
            problems.push(format!("{scenario} wrote undeclared tables {undeclared:?}"));
        }
    }
    let never_written: Vec<&String> = declared.difference(&union).collect();
    if !never_written.is_empty() {
        problems.push(format!(
            "declared but written by no scenario: {never_written:?}"
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

#[test]
fn import_cass_declaration_check_is_red_on_an_empty_world_and_on_drift() -> TestResult {
    let set = |tables: &[&str]| -> TableSet { tables.iter().map(|t| (*t).to_owned()).collect() };
    let red = |result: TestResult, cause: &str| result.is_err_and(|error| error.contains(cause));
    ensure(
        red(
            two_way_declaration_check(&set(&[]), &[("a", set(&["sessions"]))]),
            "declares no db_tables",
        ),
        "an empty declaration is red for that reason",
    )?;
    ensure(
        red(
            two_way_declaration_check(&set(&["sessions"]), &[("a", set(&[]))]),
            "no scenario wrote",
        ),
        "an empty union is red for that reason",
    )?;
    ensure(
        red(
            two_way_declaration_check(&set(&["sessions"]), &[("a", set(&["sessions", "phantom"]))]),
            "undeclared tables [\"phantom\"]",
        ),
        "an undeclared write is red and named",
    )?;
    ensure(
        red(
            two_way_declaration_check(
                &set(&["sessions", "memories"]),
                &[("a", set(&["sessions"]))],
            ),
            "written by no scenario: [\"memories\"]",
        ),
        "a declared-but-never-written table is red and named",
    )?;
    two_way_declaration_check(&set(&["sessions"]), &[("a", set(&["sessions"]))])
}

/// One workspace with its own fixture session and CASS stub.
struct CassWorkspace {
    ws: PathBuf,
    base: PathBuf,
    session: PathBuf,
    envs: Vec<(&'static str, OsString)>,
    copies: Cell<u32>,
}

impl CassWorkspace {
    fn new(root: &Path, name: &str, extra_line: Option<&str>) -> Result<Self, String> {
        let base = root.join(name);
        let ws = base.join("workspace");
        let home = base.join("home");
        let codex_home = base.join("codex-home");
        let cass_data = base.join("cass-data");
        for directory in [&ws, &home, &codex_home, &cass_data] {
            fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        }
        let session = write_codex_cass_fixture_session(&codex_home, &ws)?;
        if let Some(text) = extra_line {
            append_assistant_record(&session, text, 10)?;
        }
        let mut workspace = Self {
            ws,
            base,
            session,
            envs: vec![
                ("HOME", home.into_os_string()),
                ("CODEX_HOME", codex_home.into_os_string()),
                ("CASS_DATA_DIR", cass_data.into_os_string()),
                ("CASS_IGNORE_SOURCES_CONFIG", OsString::from("1")),
                ("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT", OsString::from("1")),
                ("EE_EMBED_DOWNLOAD", OsString::from("off")),
                ("NO_COLOR", OsString::from("1")),
            ],
            copies: Cell::new(0),
        };
        workspace.install_stub("stub")?;
        Ok(workspace)
    }

    /// Writes a fresh stub for the session's current bytes into new
    /// directories; the stub writer leaves its binary non-writable.
    fn install_stub(&mut self, name: &str) -> TestResult {
        let stub = write_stub_cass_binary(
            &self.base.join(name),
            &self.base.join(format!("{name}-payloads")),
            &self.session,
            &self.ws,
        )?;
        let path = path_with_binary_parent(&stub.binary)?;
        self.envs.retain(|(key, _)| {
            !matches!(
                *key,
                "EE_CASS_BINARY"
                    | "CASS_STUB_SESSIONS_JSON"
                    | "CASS_STUB_VIEW_JSONL"
                    | "CASS_STUB_INVOCATION_LOG"
                    | "PATH"
            )
        });
        self.envs.extend([
            ("EE_CASS_BINARY", stub.binary.into_os_string()),
            (
                "CASS_STUB_SESSIONS_JSON",
                stub.sessions_json.into_os_string(),
            ),
            ("CASS_STUB_VIEW_JSONL", stub.view_jsonl.into_os_string()),
            (
                "CASS_STUB_INVOCATION_LOG",
                stub.invocation_log.into_os_string(),
            ),
            ("PATH", path),
        ]);
        Ok(())
    }

    fn run(&self, args: &[&str]) -> Result<JsonValue, String> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
        command
            .arg("--workspace")
            .arg(&self.ws)
            .arg("--json")
            .args(args)
            .env_remove("EE_WORKSPACE")
            .env_remove("EE_AGENT_NAME");
        for (key, value) in &self.envs {
            command.env(key, value);
        }
        let output = command.output().map_err(|error| error.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            return Err(format!(
                "ee {args:?} failed with {:?}: stdout {stdout}; stderr {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        serde_json::from_str(&stdout)
            .map_err(|error| format!("ee {args:?} printed non-JSON stdout ({error}): {stdout}"))
    }

    /// Logical digest of every table, read from a copy of the store.
    fn table_digests(&self) -> Result<BTreeMap<String, String>, String> {
        self.copies.set(self.copies.get() + 1);
        let copy = self.base.join(format!("copy-{}", self.copies.get()));
        fs::create_dir_all(&copy).map_err(|error| error.to_string())?;
        for entry in fs::read_dir(self.ws.join(".ee")).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "ee.db" || name == "ee.db-wal" || name.starts_with("ee.db-wal-cert") {
                fs::copy(entry.path(), copy.join(&name)).map_err(|error| error.to_string())?;
            }
        }
        ee::db::DbConnection::open_file(copy.join("ee.db"))
            .map_err(|error| error.to_string())?
            .logical_table_digests()
            .map_err(|error| error.to_string())
    }

    /// The tables whose logical digest one invocation changed, with its report.
    fn measure(&self, args: &[&str]) -> Result<(TableSet, JsonValue), String> {
        let before = self.table_digests()?;
        let report = self.run(args)?;
        let after = self.table_digests()?;
        let written = before
            .keys()
            .chain(after.keys())
            .filter(|table| before.get(*table) != after.get(*table))
            .cloned()
            .collect();
        Ok((written, report))
    }
}

fn append_assistant_record(session: &Path, text: &str, second: u32) -> TestResult {
    let record = json!({
        "timestamp": format!("2026-05-06T03:41:{second:02}Z"),
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": text}]
        }
    });
    let mut bytes = fs::read_to_string(session).map_err(|error| error.to_string())?;
    if !bytes.ends_with('\n') {
        bytes.push('\n');
    }
    bytes.push_str(&record.to_string());
    bytes.push('\n');
    fs::write(session, bytes).map_err(|error| error.to_string())
}

#[test]
fn import_cass_writes_exactly_its_declared_tables() -> TestResult {
    let declared: TableSet = ee::core::effect::EffectManifest::build()
        .get("import cass")
        .ok_or_else(|| "import cass is not in the EffectManifest".to_owned())?
        .write_surfaces
        .db_tables
        .iter()
        .map(|table| (*table).to_owned())
        .collect();
    let root = tempfile::Builder::new()
        .prefix("ee-kmsd6-")
        .tempdir()
        .map_err(|error| error.to_string())?;
    let import = ["import", "cass", "--limit", "5"];

    let mut a = CassWorkspace::new(root.path(), "a", None)?;
    a.run(&["init"])?;
    let (first, report) = a.measure(&import)?;
    ensure_equal(
        &report["data"]["sessionsImported"],
        &json!(1),
        "trigger: the first import imports the fixture session",
    )?;
    let (unchanged, report) = a.measure(&import)?;
    ensure_equal(
        &report["data"]["sessionsSkipped"],
        &json!(1),
        "trigger: the unchanged re-import skips the known session",
    )?;
    append_assistant_record(
        &a.session,
        "x65f the session grew by one assistant message after the first import",
        20,
    )?;
    a.install_stub("stub-grown")?;
    let (grown, report) = a.measure(&import)?;
    ensure(
        report["data"]["spansImported"]
            .as_u64()
            .is_some_and(|spans| spans >= 1),
        format!("trigger: the grown session re-imports its new span; got {report}"),
    )?;

    let b = CassWorkspace::new(
        root.path(),
        "b",
        Some("deploy notes: api_key=kmsd6-secret-canary-4815 was rotated today"),
    )?;
    b.run(&["init"])?;
    let (redacted, report) = b.measure(&import)?;
    ensure_equal(
        &report["data"]["sessionsImported"],
        &json!(1),
        "trigger: the import with a secret imports its session",
    )?;
    ensure(
        redacted.contains("audit_log") && !first.contains("audit_log"),
        "trigger: the api_key span was redacted (an audit row the plain first import does not write)",
    )?;

    two_way_declaration_check(
        &declared,
        &[
            ("first_import", first),
            ("unchanged_reimport", unchanged),
            ("reimport_after_growth", grown),
            ("redacted_span", redacted),
        ],
    )
}
