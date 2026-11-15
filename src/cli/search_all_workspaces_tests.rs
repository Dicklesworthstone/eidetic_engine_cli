#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{CreateMemoryInput, CreateProceduralRuleInput, CreateWorkspaceInput, DbConnection};
use crate::models::{MemoryId, RuleId};
use sqlmodel_core::Value;

const QUERY: &str = "fanoutquasar";

struct Store {
    root: PathBuf,
    database: PathBuf,
    id: String,
    db: DbConnection,
}

impl Store {
    fn new(root: PathBuf, explicit_database: bool) -> Self {
        fs::create_dir_all(root.join(".ee")).unwrap();
        let root = root.canonicalize().unwrap();
        fs::write(
            root.join(".ee/config.toml"),
            "[memory]\ninclude_global = false\n",
        )
        .unwrap();
        let database = root.join(if explicit_database {
            "selected.db"
        } else {
            ".ee/ee.db"
        });
        let db = DbConnection::open_file(&database).unwrap();
        db.migrate().unwrap();
        let id = crate::core::workspace::stable_workspace_id(&root);
        db.insert_workspace(
            &id,
            &CreateWorkspaceInput {
                path: root.display().to_string(),
                name: None,
            },
        )
        .unwrap();
        Self {
            root,
            database,
            id,
            db,
        }
    }

    fn register(&self, other: &Store) {
        self.db
            .insert_workspace(
                &other.id,
                &CreateWorkspaceInput {
                    path: other.root.display().to_string(),
                    name: None,
                },
            )
            .unwrap();
    }

    fn memory(&self, number: u128, kind: &str, body: &str, condition: Option<&str>) -> String {
        insert_memory(&self.db, &self.id, number, kind, body, condition)
    }

    fn native_rule(&self, number: u128) -> String {
        let id = RuleId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
        self.db
            .insert_procedural_rule(
                &id,
                &CreateProceduralRuleInput {
                    workspace_id: self.id.clone(),
                    content: format!("{QUERY} native release guidance"),
                    confidence: 1.0,
                    utility: 1.0,
                    importance: 0.5,
                    trust_class: "human_explicit".to_owned(),
                    scope: "workspace".to_owned(),
                    scope_pattern: None,
                    maturity: "validated".to_owned(),
                    protected: false,
                    source_memory_ids: Vec::new(),
                    tags: Vec::new(),
                },
            )
            .unwrap();
        id
    }
}

fn insert_memory(
    db: &DbConnection,
    workspace: &str,
    number: u128,
    kind: &str,
    body: &str,
    condition: Option<&str>,
) -> String {
    let id = MemoryId::from_uuid(uuid::Uuid::from_u128(number)).to_string();
    db.insert_memory(
        &id,
        &CreateMemoryInput {
            workspace_id: workspace.to_owned(),
            content: body.to_owned(),
            level: "semantic".to_owned(),
            kind: kind.to_owned(),
            workflow_id: None,
            confidence: 1.0,
            utility: 1.0,
            importance: 0.5,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            provenance_uri: Some("manual://fanout-search".to_owned()),
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        },
    )
    .unwrap();
    if let Some(condition) = condition {
        db.set_memory_typed_fields_json(
            &id,
            Some(&serde_json::json!({"condition": condition}).to_string()),
        )
        .unwrap();
    }
    id
}

fn state(db: &DbConnection) -> Vec<Vec<Vec<(String, Value)>>> {
    [
        "workspaces",
        "memories",
        "memory_tags",
        "memory_anchors",
        "memory_anchor_index",
        "memory_seals",
        "procedural_rules",
        "search_index_jobs",
        "audit_log",
    ]
    .into_iter()
    .map(|table| {
        db.query(&format!("SELECT * FROM {table} ORDER BY 1, 2"), &[])
            .unwrap()
            .into_iter()
            .map(|row| {
                row.iter()
                    .map(|(name, value)| (name.to_owned(), value.clone()))
                    .collect()
            })
            .collect()
    })
    .collect()
}

fn invoke(root: &Path, database: &Path, extra: &[&str]) -> (ProcessExitCode, serde_json::Value) {
    let mut arguments = vec![
        "ee".to_owned(),
        "--json".to_owned(),
        "--workspace".to_owned(),
        root.display().to_string(),
        "search".to_owned(),
        QUERY.to_owned(),
        "--all-workspaces".to_owned(),
        "--database".to_owned(),
        database.display().to_string(),
    ];
    arguments.extend(extra.iter().map(|arg| (*arg).to_owned()));
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let exit = run(
        arguments.into_iter().map(OsString::from),
        &mut stdout,
        &mut stderr,
    );
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    (exit, serde_json::from_slice(&stdout).unwrap())
}

fn row<'a>(response: &'a serde_json::Value, workspace: &str) -> &'a serde_json::Value {
    response["data"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["workspaceId"] == workspace)
        .unwrap()
}

fn ids(row: &serde_json::Value) -> std::collections::BTreeSet<String> {
    row["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["docId"].as_str().unwrap().to_owned())
        .collect()
}

fn index_files(root: &Path) -> std::collections::BTreeMap<PathBuf, blake3::Hash> {
    fn collect(
        root: &Path,
        path: &Path,
        files: &mut std::collections::BTreeMap<PathBuf, blake3::Hash>,
    ) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                collect(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    blake3::hash(&fs::read(path).unwrap()),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    collect(root, root, &mut files);
    files
}

#[test]
fn filtered_fanout_honors_addressed_database_and_keeps_sources_unmodified() {
    let directory = tempfile::tempdir().unwrap();
    let local = Store::new(directory.path().join("a-local"), true);
    let sibling = Store::new(directory.path().join("b-sibling"), false);
    local.register(&sibling);
    let body = format!(
        "{QUERY} release guidance {}",
        "preserves current decisions. ".repeat(20)
    );
    let local_rule = local.memory(1, "rule", &body, Some("before release"));
    let sibling_rule = sibling.memory(2, "rule", &body, Some("before release"));
    local.memory(3, "fact", QUERY, None);
    sibling.memory(4, "fact", QUERY, None);
    let native = local.native_rule(5);
    let local_before = state(&local.db);
    let sibling_before = state(&sibling.db);

    let (exit, response) = invoke(
        &local.root,
        &local.database,
        &[
            "--kind",
            "rule",
            "--field",
            "condition=before release",
            "--limit",
            "1",
            "--source-mode",
            "lexical_only",
            "--strict-source-mode",
            "--relevance-floor",
            "0",
            "--full",
            "--explain",
        ],
    );
    assert_eq!(exit, ProcessExitCode::Success, "{response}");
    assert_eq!(response["success"], true);
    for (store, expected) in [(&local, &local_rule), (&sibling, &sibling_rule)] {
        let row = row(&response, &store.id);
        assert_eq!(
            ids(row),
            std::collections::BTreeSet::from([expected.clone()])
        );
        assert_eq!(row["request"]["sourceMode"], "lexical_only");
        assert_eq!(row["request"]["strictSourceMode"], true);
        assert_eq!(row["metrics"]["sourceModeApplied"], "lexical_only");
        assert_eq!(row["hits"][0]["content"], body);
        assert!(row["hits"][0].get("content_truncated").is_none());
        assert!(row["hits"][0].get("explanation").is_some());
        assert!(
            row["degraded"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["code"] == "search_live_snapshot_lexical")
        );
    }
    assert!(
        response["degraded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["code"] == "search_live_snapshot_lexical")
    );

    let (exit, response) = invoke(
        &local.root,
        &local.database,
        &[
            "--kind",
            "rule",
            "--source-mode",
            "lexical_only",
            "--relevance-floor",
            "0",
        ],
    );
    assert_eq!(exit, ProcessExitCode::Success, "{response}");
    assert_eq!(
        ids(row(&response, &local.id)),
        std::collections::BTreeSet::from([local_rule, native])
    );
    assert_eq!(
        ids(row(&response, &sibling.id)),
        std::collections::BTreeSet::from([sibling_rule])
    );
    assert_eq!(state(&local.db), local_before);
    assert_eq!(state(&sibling.db), sibling_before);
    assert!(
        !local.root.join(".ee/ee.db").exists(),
        "explicit database must remain authoritative"
    );
    assert!(!local.root.join(".ee/index").exists());
    assert!(!sibling.root.join(".ee/index").exists());
}

#[test]
fn malformed_and_unsupported_fanout_controls_fail_before_storage() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("never-created.db");
    for controls in [
        vec!["--kind", "invalid kind"],
        vec!["--field", "no-operator"],
        vec!["--relevance-floor", "NaN"],
        vec!["--relevance-floor", "1.01"],
        vec!["--use-daemon"],
        vec!["--use-daemon", "--daemon-socket", "/never-opened.sock"],
        vec!["--explain-performance"],
        vec!["--recalibrate-now"],
        vec!["--mesh", "cache"],
        vec!["--cursor", "opaque-cursor"],
        vec!["--read-fence", "latest"],
        vec!["--read-fence", "snapshot:1"],
        vec!["--strict-read-fence"],
    ] {
        let (exit, response) = invoke(directory.path(), &database, &controls);
        assert_eq!(exit, ProcessExitCode::Usage, "{controls:?}: {response}");
        assert_eq!(response["schema"], "ee.error.v2");
        assert_eq!(response["error"]["code"], "usage");
        assert!(!database.exists());
        assert!(!directory.path().join(".ee").exists());
    }
}

#[test]
fn published_default_hybrid_fanout_uses_the_workspace_producer_without_repair() {
    const CHILD: &str = "EE_TEST_FANOUT_PUBLISHED_PRODUCER";
    if std::env::var_os(CHILD).is_none() {
        let isolated = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::search_all_workspaces_tests::published_default_hybrid_fanout_uses_the_workspace_producer_without_repair", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("EE_EMBED_BACKEND", "local")
            .env("EE_EMBED_DOWNLOAD", "off")
            .env_remove("EE_EMBED_MODEL_DIR")
            .env_remove("EE_EMBED_MODEL_PATH")
            .env("XDG_CONFIG_HOME", isolated.path().join("config"))
            .env("XDG_CACHE_HOME", isolated.path().join("cache"))
            .env("XDG_DATA_HOME", isolated.path().join("data"))
            .env("XDG_STATE_HOME", isolated.path().join("state"))
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let local = Store::new(directory.path().join("workspace"), true);
    let matching = local.memory(1, "rule", QUERY, Some("before release"));
    local.memory(2, "fact", QUERY, None);
    // A producer different from the ambient 256-dimensional hash default
    // catches inspection paths that forget workspace-specific preparation.
    let _selected = crate::core::index::install_test_workspace_embedder(
        &local.id,
        crate::search::EmbedderStack::from_parts(
            std::sync::Arc::new(crate::search::HashEmbedder::new(
                64,
                frankensearch::HashAlgorithm::FnvModular,
            )),
            None,
        ),
    );
    let index = local.root.join("custom-index");
    let rebuilt = crate::core::index::rebuild_index(&IndexRebuildOptions {
        workspace_path: local.root.clone(),
        database_path: Some(local.database.clone()),
        index_dir: Some(index.clone()),
        dry_run: false,
    })
    .unwrap();
    assert_eq!(rebuilt.status, IndexRebuildStatus::Success, "{rebuilt:?}");
    let before = state(&local.db);
    let files_before = index_files(&index);
    let (exit, response) = invoke(
        &local.root,
        &local.database,
        &[
            "--index-dir",
            index.to_str().unwrap(),
            "--kind",
            "rule",
            "--field",
            "condition=before release",
            "--limit",
            "1",
            "--relevance-floor",
            "0",
        ],
    );
    assert_eq!(exit, ProcessExitCode::Success, "{response}");
    let row = row(&response, &local.id);
    assert_eq!(ids(row), std::collections::BTreeSet::from([matching]));
    assert_eq!(row["request"]["sourceMode"], "hybrid");
    assert_eq!(row["embed_backend"], "hash_fallback");
    assert!(
        !row["degraded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["code"] == "search_live_snapshot_lexical")
    );
    assert_eq!(state(&local.db), before);
    assert_eq!(index_files(&index), files_before);
    assert!(!local.root.join(".ee/index").exists());
}

#[test]
fn partial_failures_keep_successful_workspaces_and_return_a_nonzero_verdict() {
    let directory = tempfile::tempdir().unwrap();
    let local = Store::new(directory.path().join("a-local"), true);
    let broken = Store::new(directory.path().join("b-broken"), false);
    let healthy = Store::new(directory.path().join("c-healthy"), false);
    local.register(&broken);
    local.register(&healthy);
    let local_memory = local.memory(1, "fact", QUERY, None);
    let healthy_memory = healthy.memory(2, "fact", QUERY, None);
    broken.memory(3, "fact", QUERY, None);
    fs::write(
        broken.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = [invalid\n",
    )
    .unwrap();
    let before = [state(&local.db), state(&broken.db), state(&healthy.db)];
    let (exit, response) = invoke(
        &local.root,
        &local.database,
        &["--source-mode", "lexical_only", "--relevance-floor", "0"],
    );
    assert_eq!(exit, ProcessExitCode::Configuration, "{response}");
    assert_eq!(response["success"], false);
    assert_eq!(response["data"]["failedWorkspaces"], 1);
    assert_eq!(
        ids(row(&response, &local.id)),
        std::collections::BTreeSet::from([local_memory])
    );
    assert_eq!(
        ids(row(&response, &healthy.id)),
        std::collections::BTreeSet::from([healthy_memory])
    );
    let failure = row(&response, &broken.id);
    assert_eq!(failure["searched"], false);
    assert_eq!(failure["error"]["code"], "configuration");
    assert_eq!(failure["exitCode"], ProcessExitCode::Configuration as u8);
    assert_eq!(
        [state(&local.db), state(&broken.db), state(&healthy.db)],
        before
    );
}

#[test]
fn explicit_source_and_historical_requests_cannot_be_replaced_by_current_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let local = Store::new(directory.path().join("workspace"), true);
    local.memory(1, "fact", QUERY, None);
    let before = state(&local.db);
    for controls in [
        vec!["--source-mode", "semantic_only", "--strict-source-mode"],
        vec![
            "--source-mode",
            "lexical_only",
            "--as-of",
            "2026-01-01T00:00:00Z",
        ],
        vec!["--source-mode", "lexical_only", "--include-tombstoned"],
    ] {
        let (exit, response) = invoke(&local.root, &local.database, &controls);
        assert_eq!(
            exit,
            ProcessExitCode::SearchIndex,
            "{controls:?}: {response}"
        );
        assert_eq!(response["success"], false);
        assert_eq!(row(&response, &local.id)["searched"], false);
        assert_eq!(row(&response, &local.id)["error"]["code"], "search_index");
        assert_eq!(state(&local.db), before);
        assert!(!local.root.join(".ee/index").exists());
    }
}

#[test]
fn fanout_global_lane_keeps_typed_filters_scope_policy_and_read_only_sources() {
    const CHILD: &str = "EE_TEST_FANOUT_GLOBAL_STORE";
    if std::env::var_os(CHILD).is_none() {
        let data = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::search_all_workspaces_tests::fanout_global_lane_keeps_typed_filters_scope_policy_and_read_only_sources", "--nocapture"])
            .env(CHILD, "1")
            .env("XDG_DATA_HOME", data.path())
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let local = Store::new(directory.path().join("workspace"), true);
    fs::write(
        local.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = true\n",
    )
    .unwrap();
    let local_rule = local.memory(10, "rule", QUERY, Some("before release"));
    let paths = crate::core::global_store::default_global_store_paths_from_env().unwrap();
    let (global, global_workspace) =
        crate::core::global_store::open_or_create_global_store(&paths).unwrap();
    let global_rule = insert_memory(
        &global,
        &global_workspace,
        11,
        "rule",
        QUERY,
        Some("before release"),
    );
    insert_memory(&global, &global_workspace, 12, "fact", QUERY, None);
    let before = state(&local.db);
    let global_before = state(&global);
    let controls = [
        "--source-mode",
        "lexical_only",
        "--kind",
        "rule",
        "--field",
        "condition=before release",
        "--relevance-floor",
        "0",
    ];
    let (exit, response) = invoke(&local.root, &local.database, &controls);
    assert_eq!(exit, ProcessExitCode::Success, "{response}");
    let local_row = row(&response, &local.id);
    assert_eq!(
        ids(local_row),
        std::collections::BTreeSet::from([local_rule.clone(), global_rule.clone()])
    );
    let global_hit = local_row["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["docId"] == global_rule)
        .unwrap();
    assert_eq!(global_hit["storeLane"], "global");

    let mut global_only = controls.to_vec();
    global_only.extend(["--memory-scope", "global"]);
    let (exit, response) = invoke(&local.root, &local.database, &global_only);
    assert_eq!(exit, ProcessExitCode::Success, "{response}");
    assert_eq!(
        ids(row(&response, &local.id)),
        std::collections::BTreeSet::from([global_rule])
    );

    fs::write(
        local.root.join(".ee/config.toml"),
        "[memory]\ninclude_global = true\nparticipate = false\n",
    )
    .unwrap();
    let (exit, response) = invoke(&local.root, &local.database, &controls);
    assert_eq!(exit, ProcessExitCode::Success, "{response}");
    assert_eq!(
        ids(row(&response, &local.id)),
        std::collections::BTreeSet::from([local_rule])
    );
    assert!(
        response["degraded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["code"] == "global_lane_disabled")
    );
    assert_eq!(state(&local.db), before);
    assert_eq!(state(&global), global_before);
    assert!(!local.root.join(".ee/index").exists());
    assert!(!paths.index_dir.exists());
}
