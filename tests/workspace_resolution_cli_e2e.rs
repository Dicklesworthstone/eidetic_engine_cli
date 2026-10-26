//! Real-binary workspace addressing must resolve the root once while retaining
//! the no-follow boundary around `.ee` and its database.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection, WorkspaceScopeFields};
use serde_json::Value;

type TestResult = Result<(), String>;

const WORKSPACE_ID: &str = "wsp_00000000000000000000000392";
const MEMORY_ID: &str = "mem_00000000000000000000000392";
const BODY: &str = "Resolve workspace aliases before opening the durable memory store.";

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self, String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temporary
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let home = root.join("isolated-home");
        for path in [
            home.clone(),
            home.join("data"),
            home.join("config"),
            home.join("cache"),
            home.join("state"),
            root.join("unrelated-cwd"),
        ] {
            fs::create_dir_all(path).map_err(|error| error.to_string())?;
        }
        Ok(Self {
            _temporary: temporary,
            root,
            home,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
        command
            .env_clear()
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_CACHE_HOME", self.home.join("cache"))
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("EE_EMBED_DOWNLOAD", "off")
            .env("NO_COLOR", "1")
            .current_dir(self.root.join("unrelated-cwd"))
            .arg("--json");
        // Cargo may need its loader paths to run a dynamically linked test
        // binary. These do not select an ee store, configuration, or model.
        for name in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
    }

    fn workspace_command(&self, workspace: &Path, from_environment: bool) -> Command {
        let mut command = self.command();
        if from_environment {
            command.env("EE_WORKSPACE", workspace);
        } else {
            command.arg("--workspace").arg(workspace);
        }
        command
    }
}

fn run(command: &mut Command) -> Result<Output, String> {
    command.output().map_err(|error| format!("run ee: {error}"))
}

fn response(output: &Output) -> Result<Value, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "ee did not emit JSON: {error}; exit {:?}; stdout {}; stderr {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn successful_response(command: &mut Command) -> Result<Value, String> {
    let output = run(command)?;
    let value = response(&output)?;
    assert!(
        output.status.success(),
        "ee failed: {value}; stderr {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(value["success"], true, "{value}");
    Ok(value)
}

/// Seed real persisted rows without loading a model or creating an index. A
/// separate bound root lets the rejection controls prove that a redirected
/// database would otherwise be a usable store for the selected workspace.
fn seed_store(store: &Path, bound_workspace: &Path) -> TestResult {
    fs::create_dir_all(store.join(".ee")).map_err(|error| error.to_string())?;
    let db = DbConnection::open_file(store.join(".ee/ee.db")).map_err(|error| error.to_string())?;
    db.migrate().map_err(|error| error.to_string())?;
    db.upsert_workspace_with_scope(
        WORKSPACE_ID,
        &CreateWorkspaceInput {
            path: bound_workspace.to_string_lossy().into_owned(),
            name: None,
        },
        &WorkspaceScopeFields::standalone(),
    )
    .map_err(|error| error.to_string())?;
    db.insert_memory(
        MEMORY_ID,
        &CreateMemoryInput {
            workspace_id: WORKSPACE_ID.to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content: BODY.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.7,
            importance: 0.6,
            provenance_uri: Some("manual://workspace-resolution".to_owned()),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        },
    )
    .map_err(|error| error.to_string())?;
    db.close().map_err(|error| error.to_string())
}

fn assert_workspace(value: &Value, workspace: &Path) {
    assert_eq!(value["data"]["workspaceId"], WORKSPACE_ID, "{value}");
    assert_eq!(
        value["data"]["canonicalRoot"].as_str(),
        Some(workspace.to_string_lossy().as_ref()),
        "{value}",
    );
    assert_eq!(value["data"]["markerPresent"], true, "{value}");
}

#[derive(Debug, Eq, PartialEq)]
enum TreeEntry {
    Directory,
    File(Vec<u8>),
    Symlink(PathBuf),
}

fn tree_snapshot(root: &Path) -> Result<BTreeMap<PathBuf, TreeEntry>, String> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|error| error.to_string())?
                .to_path_buf();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            let value = if kind.is_symlink() {
                TreeEntry::Symlink(fs::read_link(&path).map_err(|error| error.to_string())?)
            } else if kind.is_dir() {
                pending.push(path);
                TreeEntry::Directory
            } else {
                TreeEntry::File(fs::read(path).map_err(|error| error.to_string())?)
            };
            result.insert(relative, value);
        }
    }
    Ok(result)
}

#[test]
fn workspace_aliases_reach_the_same_memory_and_expand_user_input_only_once() -> TestResult {
    let fixture = Fixture::new()?;
    for (number, leaf) in ["ordinary-store", "${HOME}"].into_iter().enumerate() {
        let workspace = fixture.root.join("stores").join(leaf);
        seed_store(&workspace, &workspace)?;
        let alias = fixture.root.join(format!("workspace-alias-{number}"));
        symlink(&workspace, &alias).map_err(|error| error.to_string())?;
        let before = tree_snapshot(&workspace)?;

        for from_environment in [false, true] {
            let listed = successful_response(
                fixture
                    .workspace_command(&alias, from_environment)
                    .args(["memory", "list"]),
            )?;
            assert_eq!(listed["data"]["total_count"], 1, "{listed}");
            let memories = listed["data"]["memories"]
                .as_array()
                .ok_or("memory list omitted its rows")?;
            assert_eq!(memories.len(), 1);
            assert_eq!(memories[0]["id"], MEMORY_ID);
            assert_eq!(memories[0]["content"], BODY);

            let resolved = successful_response(
                fixture
                    .workspace_command(&alias, from_environment)
                    .args(["workspace", "resolve"]),
            )?;
            assert_workspace(&resolved, &workspace);
        }

        let positional =
            successful_response(fixture.command().args(["workspace", "resolve"]).arg(&alias))?;
        assert_workspace(&positional, &workspace);

        // Expansion substitutes the variable's value once. The literal HOME
        // token in that value is a filename, not a second environment lookup.
        let expanded = successful_response(
            fixture
                .command()
                .env("EE_TEST_WORKSPACE", &workspace)
                .args(["workspace", "resolve", "$EE_TEST_WORKSPACE/."]),
        )?;
        assert_workspace(&expanded, &workspace);
        assert_eq!(
            tree_snapshot(&workspace)?,
            before,
            "reads changed the store"
        );
    }
    Ok(())
}

#[test]
fn init_still_refuses_a_symlinked_final_workspace_root() -> TestResult {
    let fixture = Fixture::new()?;
    let workspace = fixture.root.join("uninitialized-target");
    fs::create_dir(&workspace).map_err(|error| error.to_string())?;
    fs::write(workspace.join("existing.txt"), "keep this file")
        .map_err(|error| error.to_string())?;
    let alias = fixture.root.join("init-alias");
    symlink(&workspace, &alias).map_err(|error| error.to_string())?;
    let before = tree_snapshot(&workspace)?;

    for from_environment in [false, true] {
        let output = run(fixture
            .workspace_command(&alias, from_environment)
            .args(["init", "--skip-boilerplate"]))?;
        let refused = response(&output)?;
        assert_eq!(output.status.code(), Some(2), "{refused}");
        assert_eq!(refused["success"], false, "{refused}");
        assert!(
            refused["degraded"]
                .as_array()
                .ok_or("init omitted its degradation array")?
                .iter()
                .any(|entry| entry["code"] == "workspace_symlink_refused"),
            "{refused}",
        );
        assert_eq!(tree_snapshot(&workspace)?, before);
    }
    Ok(())
}

#[test]
fn resolving_the_root_does_not_authorize_symlinks_inside_the_store() -> TestResult {
    let fixture = Fixture::new()?;
    for redirect_directory in [true, false] {
        let label = if redirect_directory {
            "directory"
        } else {
            "database"
        };
        let workspace = fixture.root.join(format!("selected-{label}"));
        fs::create_dir(&workspace).map_err(|error| error.to_string())?;
        let outside = fixture.root.join(format!("outside-{label}"));
        seed_store(&outside, &workspace)?;
        let redirected = if redirect_directory {
            workspace.join(".ee")
        } else {
            fs::create_dir(workspace.join(".ee")).map_err(|error| error.to_string())?;
            workspace.join(".ee/ee.db")
        };
        let target = if redirect_directory {
            outside.join(".ee")
        } else {
            outside.join(".ee/ee.db")
        };
        symlink(&target, &redirected).map_err(|error| error.to_string())?;
        let alias = fixture.root.join(format!("root-alias-{label}"));
        symlink(&workspace, &alias).map_err(|error| error.to_string())?;
        let outside_before = tree_snapshot(&outside)?;
        let workspace_before = tree_snapshot(&workspace)?;

        for args in [
            ["memory", "list"],
            [
                "remember",
                "A redirected store must not accept this memory.",
            ],
        ] {
            let output = run(fixture.workspace_command(&alias, false).args(args))?;
            let refused = response(&output)?;
            assert_eq!(output.status.code(), Some(3), "{refused}");
            assert_eq!(refused["schema"], "ee.error.v2", "{refused}");
            let message = refused["error"]["message"]
                .as_str()
                .ok_or("storage refusal omitted its reason")?;
            assert!(message.contains("symbolic link"), "{message}");
            assert_eq!(tree_snapshot(&outside)?, outside_before);
            assert_eq!(tree_snapshot(&workspace)?, workspace_before);
        }
    }
    Ok(())
}
