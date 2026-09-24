//! bd-hin8m: `ee import cass` against a storeless address is refused exactly
//! as `ee remember` refuses one (the 91cf7bcbd contract: no write surface
//! plants a store; `ee init` owns creation), and nothing is created. An
//! initialized workspace still imports.

use super::*;

/// Every path under `root`, relative, directories included.
fn listing(root: &Path) -> Result<BTreeSet<String>, String> {
    fn visit(dir: &Path, root: &Path, out: &mut BTreeSet<String>) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            let relative = path.strip_prefix(root).map_err(|error| error.to_string())?;
            out.insert(relative.display().to_string());
            if path.is_dir() {
                visit(&path, root, out)?;
            }
        }
        Ok(())
    }
    let mut out = BTreeSet::new();
    visit(root, root, &mut out)?;
    Ok(out)
}

#[test]
fn import_cass_refuses_a_missing_store_like_remember_and_creates_nothing() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("ee-hin8m-")
        .tempdir()
        .map_err(|error| error.to_string())?;
    let workspace = root.path().join("workspace");
    let home = root.path().join("home");
    let codex_home = root.path().join("codex-home");
    let cass_data = root.path().join("cass-data");
    for directory in [&workspace, &home, &codex_home, &cass_data] {
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    let session = write_codex_cass_fixture_session(&codex_home, &workspace)?;
    let stub = write_stub_cass_binary(
        &root.path().join("stub"),
        &root.path().join("payloads"),
        &session,
        &workspace,
    )?;
    let envs = [
        ("HOME", home.as_os_str().to_owned()),
        ("CODEX_HOME", codex_home.as_os_str().to_owned()),
        ("CASS_DATA_DIR", cass_data.as_os_str().to_owned()),
        ("CASS_IGNORE_SOURCES_CONFIG", OsString::from("1")),
        ("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT", OsString::from("1")),
        ("EE_EMBED_DOWNLOAD", OsString::from("off")),
        ("NO_COLOR", OsString::from("1")),
        ("EE_CASS_BINARY", stub.binary.as_os_str().to_owned()),
        (
            "CASS_STUB_SESSIONS_JSON",
            stub.sessions_json.as_os_str().to_owned(),
        ),
        (
            "CASS_STUB_VIEW_JSONL",
            stub.view_jsonl.as_os_str().to_owned(),
        ),
        (
            "CASS_STUB_INVOCATION_LOG",
            stub.invocation_log.as_os_str().to_owned(),
        ),
        ("PATH", path_with_binary_parent(&stub.binary)?),
    ];
    let run = |args: &[&str]| -> Result<(Option<i32>, JsonValue), String> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
        command
            .arg("--workspace")
            .arg(&workspace)
            .arg("--json")
            .args(args)
            .env_remove("EE_WORKSPACE");
        for (key, value) in &envs {
            command.env(key, value);
        }
        let output = command.output().map_err(|error| error.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let value = serde_json::from_str(&stdout)
            .map_err(|error| format!("ee {args:?} printed non-JSON stdout ({error}): {stdout}"))?;
        Ok((output.status.code(), value))
    };

    // Refusal arm: nothing exists under the workspace, before or after.
    let before = listing(&workspace)?;
    ensure_equal(&before, &BTreeSet::new(), "the workspace starts empty")?;
    let (import_exit, import) = run(&["import", "cass", "--limit", "5"])?;
    ensure_equal(
        &listing(&workspace)?,
        &before,
        "import cass into a storeless workspace creates nothing",
    )?;
    ensure(
        !workspace.join(".ee").exists(),
        "import cass planted no .ee directory",
    )?;
    ensure(
        !stub.invocation_log.exists()
            || fs::read_to_string(&stub.invocation_log)
                .map_err(|error| error.to_string())?
                .is_empty(),
        "the refusal happens before cass is ever invoked",
    )?;
    let (remember_exit, remember) = run(&[
        "remember",
        "bd-hin8m control: remember refuses a storeless workspace",
        "--level",
        "semantic",
        "--kind",
        "fact",
    ])?;
    ensure_equal(
        &listing(&workspace)?,
        &before,
        "remember (the control) creates nothing either",
    )?;
    ensure_equal(
        &import["error"]["code"],
        &JsonValue::from("workspace_store_missing"),
        "import cass refuses with the storeless-workspace code",
    )?;
    ensure_equal(
        &import["error"]["code"],
        &remember["error"]["code"],
        "import cass and remember refuse with the same code",
    )?;
    ensure_equal(
        &import["error"]["repair"],
        &remember["error"]["repair"],
        "import cass and remember give the same repair",
    )?;
    ensure(
        import["error"]["repair"]
            .as_str()
            .is_some_and(|repair| !repair.is_empty()),
        "the shared repair is present, not two equal nulls",
    )?;
    ensure_equal(
        &import_exit,
        &remember_exit,
        "import cass and remember exit alike",
    )?;

    // Positive arm: once `ee init` has created the store, the import runs.
    let (init_exit, init) = run(&["init"])?;
    ensure_equal(&init_exit, &Some(0), &format!("ee init succeeds: {init}"))?;
    let (imported_exit, imported) = run(&["import", "cass", "--limit", "5"])?;
    ensure_equal(
        &imported_exit,
        &Some(0),
        &format!("import cass into an initialized workspace succeeds: {imported}"),
    )?;
    ensure_equal(
        &imported["data"]["sessionsImported"],
        &JsonValue::from(1),
        "the initialized import imports the fixture session",
    )
}
