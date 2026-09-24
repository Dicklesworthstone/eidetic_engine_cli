//! Regression coverage for the persisted pack pool in GH #49.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::config::ConfigValueSource;
use std::ffi::OsString;

fn options(root: &Path) -> ConfigSurfaceOptions {
    ConfigSurfaceOptions { workspace_root: root.to_owned(), config_path: None }
}

#[test]
fn candidate_pool_set_round_trips_and_dry_run_does_not_write() {
    let temp = tempfile::tempdir().unwrap();
    let opts = options(temp.path());
    let path = temp.path().join(".ee/config.toml");
    let dry = set_config(&opts, PACK_CANDIDATE_POOL_KEY, "24", true).unwrap();
    assert!(dry.would_write && !dry.applied);
    assert!(!path.exists());
    assert!(!temp.path().join(".ee").exists());
    assert_eq!(ConfigFile::parse(&dry.planned_toml).unwrap().pack.candidate_pool, Some(24));
    let applied = set_config(&opts, PACK_CANDIDATE_POOL_KEY, "24", false).unwrap();
    assert!(applied.applied);
    let report = get_config(&opts, PACK_CANDIDATE_POOL_KEY).unwrap();
    assert_eq!((report.value.as_str(), report.source), ("24", "project"));
    let shown = show_config(&opts, Some(PACK_CANDIDATE_POOL_KEY)).unwrap();
    assert_eq!(shown.entries.len(), 1);
    assert_eq!(shown.entries[0].value, "24");
    let before = fs::read(&path).unwrap();
    let repeated = set_config(&opts, PACK_CANDIDATE_POOL_KEY, "24", false).unwrap();
    assert!(!repeated.would_write && !repeated.applied);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn candidate_pool_updates_preserve_other_settings_and_invalid_writes_are_atomic() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join(".ee")).unwrap();
    let path = temp.path().join(".ee/config.toml");
    let original = "# keep this comment\n[search]\ndefault_speed = 'fast'\n[pack]\ncandidate_pool = 11\ndefault_max_tokens = 2048\n";
    fs::write(&path, original).unwrap();
    let opts = options(temp.path());
    set_config(&opts, PACK_CANDIDATE_POOL_KEY, "13", true).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    set_config(&opts, PACK_CANDIDATE_POOL_KEY, "13", false).unwrap();
    let stored = fs::read_to_string(&path).unwrap();
    assert!(stored.contains("# keep this comment"));
    let parsed = ConfigFile::parse(&stored).unwrap();
    assert_eq!(parsed.pack.candidate_pool, Some(13));
    assert_eq!(parsed.pack.default_max_tokens, Some(2048));
    assert_eq!(get_config(&opts, SEARCH_DEFAULT_SPEED_KEY).unwrap().value, "fast");
    for raw in ["0", "-1", "4294967296", "9223372036854775807", "1.5", "true", "\"24\"", ""] {
        for dry_run in [false, true] {
            let error = set_config(&opts, PACK_CANDIDATE_POOL_KEY, raw, dry_run).unwrap_err();
            assert!(matches!(error, ConfigSurfaceError::InvalidValue { .. }), "{raw}: {error}");
            assert_eq!(fs::read_to_string(&path).unwrap(), stored, "invalid value {raw}");
            assert!(!path.with_extension("tmp").exists());
        }
    }
}

#[test]
fn candidate_pool_toml_and_setter_accept_the_same_positive_u32_range() {
    let temp = tempfile::tempdir().unwrap();
    let opts = options(temp.path());
    for value in [1, 100, u32::MAX] {
        let raw = value.to_string();
        let parsed = ConfigFile::parse(&format!("[pack]\ncandidate_pool = {raw}\n")).unwrap();
        assert_eq!(parsed.pack.candidate_pool, Some(u64::from(value)));
        let report = set_config(&opts, PACK_CANDIDATE_POOL_KEY, &raw, true).unwrap();
        assert_eq!(ConfigFile::parse(&report.planned_toml).unwrap().pack.candidate_pool, parsed.pack.candidate_pool);
    }
    for raw in ["0", "-1", "4294967296", "9223372036854775807", "1.5", "true", "\"24\""] {
        let error = ConfigFile::parse(&format!("[pack]\ncandidate_pool = {raw}\n")).unwrap_err();
        assert!(error.to_string().contains(PACK_CANDIDATE_POOL_KEY), "{error}");
    }
    assert_eq!(ConfigFile::parse("[pack]\n").unwrap().pack.candidate_pool, None);
}

#[test]
fn candidate_pool_default_user_project_and_explicit_precedence() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(workspace.join(".ee")).unwrap();
    fs::create_dir_all(home.join(".config/ee")).unwrap();
    let environment = BTreeMap::from([(
        if cfg!(windows) { "USERPROFILE" } else { "HOME" }.to_owned(),
        OsString::from(home.as_os_str()),
    )]);
    let opts = options(&workspace);
    let merged = merged_config_with_environment(&opts, &environment).unwrap();
    assert_eq!(merged.values.pack.candidate_pool, Some(100));
    assert_eq!(merged.source(PACK_CANDIDATE_POOL_KEY), Some(ConfigValueSource::Default));
    fs::write(home.join(".config/ee/config.toml"), "[pack]\ncandidate_pool = 13\n").unwrap();
    let merged = merged_config_with_environment(&opts, &environment).unwrap();
    assert_eq!(merged.values.pack.candidate_pool, Some(13));
    assert_eq!(merged.source(PACK_CANDIDATE_POOL_KEY), Some(ConfigValueSource::User));
    fs::write(workspace.join(".ee/config.toml"), "[pack]\ncandidate_pool = 7\n").unwrap();
    let merged = merged_config_with_environment(&opts, &environment).unwrap();
    assert_eq!(merged.values.pack.candidate_pool, Some(7));
    assert_eq!(merged.source(PACK_CANDIDATE_POOL_KEY), Some(ConfigValueSource::Project));
    assert_eq!(resolve_pack_candidate_pool(&workspace, None).unwrap(), Some(7));
    // 100 is an explicit override, never a sentinel for an omitted flag.
    assert_eq!(resolve_pack_candidate_pool(&workspace, Some(100)).unwrap(), Some(100));
    assert_eq!(resolve_pack_candidate_pool(&workspace, Some(11)).unwrap(), Some(11));
}
