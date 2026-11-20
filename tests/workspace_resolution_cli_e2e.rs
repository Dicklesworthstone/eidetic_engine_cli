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

    fn boundary_workspace(&self, name: &str) -> Result<PathBuf, String> {
        let workspace = self.root.join(name);
        for relative in [
            ".home/data",
            ".home/config",
            ".home/cache",
            ".home/state",
            ".tmp",
        ] {
            fs::create_dir_all(workspace.join(relative)).map_err(|error| error.to_string())?;
        }
        Ok(workspace)
    }

    fn boundary_command(&self, workspace: &Path) -> Command {
        let mut command = self.workspace_command(workspace, false);
        command
            .env("EE_FS_BOUNDARY", workspace)
            .env("HOME", workspace.join(".home"))
            .env("XDG_DATA_HOME", workspace.join(".home/data"))
            .env("XDG_CONFIG_HOME", workspace.join(".home/config"))
            .env("XDG_CACHE_HOME", workspace.join(".home/cache"))
            .env("XDG_STATE_HOME", workspace.join(".home/state"))
            .env("TMPDIR", workspace.join(".tmp"))
            .current_dir(workspace);
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

fn boundary_refusal_response(output: &Output) -> Result<Value, String> {
    let value = response(output)?;
    assert!(
        !output.status.success(),
        "boundary refusal exited successfully: {value}"
    );
    assert!(
        (value["schema"] == "ee.error.v2" && value["error"]["code"].is_string())
            || (value["schema"] == "ee.response.v2" && value["success"] == false),
        "boundary refusal did not use a failure envelope: {value}",
    );
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

#[test]
fn filesystem_boundary_bootstraps_authenticated_store_and_persists_journal() -> TestResult {
    let fixture = Fixture::new()?;
    let workspace = fixture.boundary_workspace("boundary-workspace")?;
    successful_response(
        fixture
            .boundary_command(&workspace)
            .args(["init", "--skip-boilerplate"]),
    )?;
    assert!(workspace.join(".ee/ee.db").is_file());
    assert!(workspace.join(".ee/index").is_dir());
    let key_path = workspace.join(".ee/keys/store_auth_root.json");
    let key_before = fs::read(&key_path).map_err(|error| error.to_string())?;
    assert!(
        !key_before.is_empty(),
        "init must persist its authentication root"
    );

    let body = "Workspace-confined journal capture survives a new ee process.";
    let appended = successful_response(
        fixture
            .boundary_command(&workspace)
            .args(["journal", "append", "--source", "hook", body]),
    )?;
    assert_eq!(appended["data"]["status"], "stored", "{appended}");
    assert_eq!(appended["data"]["entry"]["body"], body, "{appended}");
    let listed = successful_response(
        fixture
            .boundary_command(&workspace)
            .args(["journal", "list"]),
    )?;
    assert_eq!(listed["data"]["entryCount"], 1, "{listed}");
    assert_eq!(listed["data"]["entries"][0]["body"], body, "{listed}");

    // A second initialization must reopen the same hardened key store, not
    // silently replace its root while repairing the partially initialized case.
    successful_response(
        fixture
            .boundary_command(&workspace)
            .args(["init", "--skip-boilerplate"]),
    )?;
    assert!(
        fs::read(&key_path).map_err(|error| error.to_string())? == key_before,
        "reinitialization replaced the authentication root",
    );
    Ok(())
}

#[test]
fn invalid_filesystem_boundaries_refuse_before_any_workspace_write() -> TestResult {
    let fixture = Fixture::new()?;
    let workspace = fixture.boundary_workspace("invalid-boundary-workspace")?;
    let before = tree_snapshot(&fixture.root)?;
    for boundary in [
        PathBuf::new(),
        PathBuf::from("relative-boundary"),
        workspace.join(".."),
        workspace.join("child/.."),
        fixture.root.join("missing-boundary"),
    ] {
        let output = run(fixture
            .boundary_command(&workspace)
            .env("EE_FS_BOUNDARY", &boundary)
            .args(["init", "--allow-symlink", "--skip-boilerplate"]))?;
        assert!(
            !output.status.success(),
            "invalid boundary {boundary:?} was accepted"
        );
        boundary_refusal_response(&output)?;
        assert_eq!(
            tree_snapshot(&fixture.root)?,
            before,
            "invalid boundary {boundary:?} changed the filesystem",
        );
    }
    Ok(())
}

#[test]
fn filesystem_boundary_rejects_outside_and_parent_traversal_workspaces_without_writes() -> TestResult
{
    let fixture = Fixture::new()?;
    let boundary = fixture.boundary_workspace("boundary")?;
    let outside = fixture.boundary_workspace("boundary-neighbor")?;
    let before = tree_snapshot(&fixture.root)?;
    for workspace in [outside.clone(), boundary.join("../boundary-neighbor")] {
        let output = run(fixture
            .boundary_command(&workspace)
            .env("EE_FS_BOUNDARY", &boundary)
            .current_dir(&boundary)
            .args(["init", "--allow-symlink", "--skip-boilerplate"]))?;
        assert!(
            !output.status.success(),
            "outside workspace {workspace:?} was accepted"
        );
        boundary_refusal_response(&output)?;
        assert_eq!(tree_snapshot(&fixture.root)?, before);
    }
    Ok(())
}

#[test]
fn filesystem_boundary_never_authorizes_symlinked_roots_or_store_directories() -> TestResult {
    let fixture = Fixture::new()?;
    let workspace = fixture.boundary_workspace("real-boundary")?;
    let alias = fixture.root.join("symlinked-boundary");
    symlink(&workspace, &alias).map_err(|error| error.to_string())?;
    let before = tree_snapshot(&fixture.root)?;
    let output = run(fixture.boundary_command(&alias).args([
        "init",
        "--allow-symlink",
        "--skip-boilerplate",
    ]))?;
    assert!(!output.status.success(), "symlinked boundary was accepted");
    boundary_refusal_response(&output)?;
    assert_eq!(tree_snapshot(&fixture.root)?, before);

    for outside_boundary in [false, true] {
        let name = if outside_boundary {
            "external-store"
        } else {
            "internal-store"
        };
        let workspace = fixture.boundary_workspace(name)?;
        let target = if outside_boundary {
            fixture.root.join("redirected-outside-store")
        } else {
            workspace.join("redirected-inside-store")
        };
        fs::create_dir(&target).map_err(|error| error.to_string())?;
        fs::write(
            target.join("sentinel"),
            "Do not initialize this redirected store.",
        )
        .map_err(|error| error.to_string())?;
        symlink(&target, workspace.join(".ee")).map_err(|error| error.to_string())?;
        let before = tree_snapshot(&fixture.root)?;
        let output = run(fixture.boundary_command(&workspace).args([
            "init",
            "--allow-symlink",
            "--skip-boilerplate",
        ]))?;
        assert!(!output.status.success(), "symlinked .ee was accepted");
        boundary_refusal_response(&output)?;
        assert_eq!(tree_snapshot(&fixture.root)?, before);
    }
    Ok(())
}

/// This test exercises the OS policy that motivated GH#88. Run explicitly on
/// a native macOS host; sandbox-exec cannot install its profile from some
/// already-sandboxed CI environments. An unavailable/denied sandbox is a test
/// failure when selected, never a passing substitute for confinement.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires native macOS with permission to install a sandbox-exec profile"]
fn filesystem_boundary_works_under_workspace_only_seatbelt() -> TestResult {
    fn quoted_path(path: &Path) -> Result<String, String> {
        serde_json::to_string(&path.to_string_lossy()).map_err(|error| error.to_string())
    }

    fn profile(workspace: &Path, binary: &Path) -> Result<String, String> {
        let mut text = format!(
            "(version 1)\n(deny default)\n\
             (allow process-fork) (allow process-exec) (allow sysctl-read)\n\
             (allow file-read* (literal \"/\") (subpath \"/usr/lib\") (subpath \"/System\")\n\
               (literal \"/dev/null\") (literal \"/dev/urandom\") (literal \"/dev/random\")\n\
               (literal \"/etc\") (literal \"/var\") (literal \"/tmp\")\n\
               (literal \"/private/etc/localtime\") (literal \"/bin/ls\")\n\
               (literal {}))\n\
             (allow file-read* file-write* (literal \"/dev/null\")\n\
               (literal \"/dev/stdout\") (literal \"/dev/stderr\"))\n\
             (allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\"))\n\
             (allow file-read* file-write* (subpath {}))\n",
            quoted_path(binary)?,
            quoted_path(workspace)?,
        );
        for ancestor in workspace.ancestors().skip(1) {
            text.push_str(&format!(
                "(allow file-read-metadata (literal {}))\n",
                quoted_path(ancestor)?,
            ));
        }
        // A dynamically linked Cargo test binary needs its loader directories,
        // not blanket access to the workspace's parent or the user's home.
        for name in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"] {
            if let Some(value) = std::env::var_os(name) {
                for directory in std::env::split_paths(&value) {
                    if directory.is_absolute() && !workspace.starts_with(&directory) {
                        text.push_str(&format!(
                            "(allow file-read* (subpath {}))\n",
                            quoted_path(&directory)?,
                        ));
                    }
                }
            }
        }
        Ok(text)
    }

    fn confined(command: &Command, policy: &str, binary: &Path) -> Command {
        let mut sandbox = Command::new("/usr/bin/sandbox-exec");
        sandbox.env_clear();
        for (name, value) in command.get_envs() {
            if let Some(value) = value {
                sandbox.env(name, value);
            }
        }
        if let Some(directory) = command.get_current_dir() {
            sandbox.current_dir(directory);
        }
        sandbox
            .args(["-p", policy])
            .arg(binary)
            .args(command.get_args());
        sandbox
    }

    let fixture = Fixture::new()?;
    let workspace = fixture.boundary_workspace("seatbelt-workspace")?;
    let binary = Path::new(env!("CARGO_BIN_EXE_ee"))
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let policy = profile(&workspace, &binary)?;

    // Prove the policy prevents listing the workspace's parent. Otherwise a
    // permissive profile would make both the old and new implementations pass.
    let listing = Command::new("/usr/bin/sandbox-exec")
        .args(["-p", &policy, "/bin/ls"])
        .arg(&fixture.root)
        .output()
        .map_err(|error| format!("Seatbelt control could not run: {error}"))?;
    assert!(
        !listing.status.success(),
        "Seatbelt unexpectedly exposed the parent directory"
    );

    let mut default_init = fixture.boundary_command(&workspace);
    default_init.env_remove("EE_FS_BOUNDARY").args([
        "init",
        "--allow-symlink",
        "--skip-boilerplate",
    ]);
    let old_path = run(&mut confined(&default_init, &policy, &binary))?;
    let old_response = response(&old_path)?;
    assert!(
        !old_path.status.success(),
        "unbounded descent unexpectedly succeeded"
    );
    assert!(
        old_response
            .to_string()
            .contains("store authentication root"),
        "default control did not reach the key-store ancestor-open refusal: {old_response}",
    );

    let mut bounded_init = fixture.boundary_command(&workspace);
    bounded_init.args(["init", "--allow-symlink", "--skip-boilerplate"]);
    successful_response(&mut confined(&bounded_init, &policy, &binary))?;
    assert!(workspace.join(".ee/keys/store_auth_root.json").is_file());
    assert!(workspace.join(".ee/index").is_dir());
    let mut append = fixture.boundary_command(&workspace);
    append.args([
        "journal",
        "append",
        "--source",
        "hook",
        "Seatbelt-confined observation.",
    ]);
    let appended = successful_response(&mut confined(&append, &policy, &binary))?;
    assert_eq!(appended["data"]["status"], "stored", "{appended}");

    let before = tree_snapshot(&workspace)?;
    let mut invalid = fixture.boundary_command(&workspace);
    invalid
        .env("EE_FS_BOUNDARY", "relative-boundary")
        .args(["init", "--skip-boilerplate"]);
    let refused = run(&mut confined(&invalid, &policy, &binary))?;
    assert!(!refused.status.success());
    boundary_refusal_response(&refused)?;
    assert!(
        tree_snapshot(&workspace)? == before,
        "invalid boundary changed the initialized workspace"
    );

    let outside = fixture.boundary_workspace("seatbelt-outside")?;
    let outside_before = tree_snapshot(&outside)?;
    let mut outside_init = fixture.boundary_command(&outside);
    outside_init
        .env("EE_FS_BOUNDARY", &workspace)
        .current_dir(&workspace)
        .args(["init", "--allow-symlink", "--skip-boilerplate"]);
    let refused = run(&mut confined(&outside_init, &policy, &binary))?;
    assert!(!refused.status.success());
    boundary_refusal_response(&refused)?;
    assert_eq!(tree_snapshot(&outside)?, outside_before);

    let planted = fixture.boundary_workspace("seatbelt-planted-store")?;
    symlink(&outside, planted.join(".ee")).map_err(|error| error.to_string())?;
    let planted_policy = profile(&planted, &binary)?;
    let planted_before = tree_snapshot(&planted)?;
    let mut planted_init = fixture.boundary_command(&planted);
    planted_init.args(["init", "--allow-symlink", "--skip-boilerplate"]);
    let refused = run(&mut confined(&planted_init, &planted_policy, &binary))?;
    assert!(!refused.status.success());
    boundary_refusal_response(&refused)?;
    assert_eq!(tree_snapshot(&planted)?, planted_before);
    assert_eq!(tree_snapshot(&outside)?, outside_before);
    Ok(())
}
