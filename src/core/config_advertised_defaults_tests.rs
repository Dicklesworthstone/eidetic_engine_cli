//! Every config layer reaches the readers of `pack.default_max_tokens`,
//! `pack.default_profile` and `[feedback]` (bd-advertised-defaults-not-read-nh9xk).
//! Each key and its `EE_*` variable was documented as an override while
//! nothing read it.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::pack::ContextPackProfile;
use std::ffi::OsString;

/// Merge built-ins, an optional project file and the given variables, with an
/// empty home so no real user config can leak in.
fn layered(project: Option<&str>, variables: &[(&str, &str)]) -> crate::config::MergedConfig {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir_all(workspace.join(".ee")).unwrap();
    fs::create_dir_all(&home).unwrap();
    if let Some(contents) = project {
        fs::write(workspace.join(".ee/config.toml"), contents).unwrap();
    }
    let mut environment = BTreeMap::from([(
        if cfg!(windows) { "USERPROFILE" } else { "HOME" }.to_owned(),
        OsString::from(home.as_os_str()),
    )]);
    for (name, value) in variables {
        environment.insert((*name).to_owned(), OsString::from(*value));
    }
    merged_config_with_environment(
        &ConfigSurfaceOptions {
            workspace_root: workspace,
            config_path: None,
        },
        &environment,
    )
    .unwrap()
}

#[test]
fn advertised_default_max_tokens_reads_every_layer() {
    // The built-in must equal the selector's own default, or wiring the key
    // would have moved every pack that never configured it.
    assert_eq!(
        pack_max_tokens_from(&layered(None, &[])).unwrap(),
        Some(crate::pack::DEFAULT_CONTEXT_MAX_TOKENS)
    );
    let project = "[pack]\ndefault_max_tokens = 2500\n";
    assert_eq!(
        pack_max_tokens_from(&layered(Some(project), &[])).unwrap(),
        Some(2500)
    );
    assert_eq!(
        pack_max_tokens_from(&layered(Some(project), &[("EE_MAX_TOKENS", "1800")])).unwrap(),
        Some(1800)
    );
    for raw in ["0", "4294967296"] {
        let error = pack_max_tokens_from(&layered(
            Some(&format!("[pack]\ndefault_max_tokens = {raw}\n")),
            &[],
        ))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(crate::config::PACK_DEFAULT_MAX_TOKENS_KEY),
            "{raw}: {error}"
        );
    }
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve_pack_max_tokens(temp.path(), Some(77)).unwrap(),
        Some(77)
    );
}

#[test]
fn advertised_default_profile_reads_every_layer() {
    assert_eq!(
        pack_profile_from(&layered(None, &[])).unwrap(),
        Some(ContextPackProfile::Balanced)
    );
    let project = "[pack]\ndefault_profile = \"thorough\"\n";
    assert_eq!(
        pack_profile_from(&layered(Some(project), &[])).unwrap(),
        Some(ContextPackProfile::Thorough)
    );
    assert_eq!(
        pack_profile_from(&layered(Some(project), &[("EE_PROFILE", "compact")])).unwrap(),
        Some(ContextPackProfile::Compact)
    );
    let error = pack_profile_from(&layered(
        Some("[pack]\ndefault_profile = \"verbose\"\n"),
        &[],
    ))
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains(crate::config::PACK_DEFAULT_PROFILE_KEY),
        "{error}"
    );
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve_pack_profile(temp.path(), Some(ContextPackProfile::Grounding)).unwrap(),
        Some(ContextPackProfile::Grounding)
    );
}

#[test]
fn advertised_default_harmful_feedback_limits_read_every_layer() {
    assert_eq!(
        harmful_feedback_limits_from(&layered(None, &[]), None, None).unwrap(),
        (
            crate::core::outcome::DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            crate::core::outcome::DEFAULT_HARMFUL_BURST_WINDOW_SECONDS
        )
    );
    let project =
        "[feedback]\nharmful_per_source_per_hour = 9\nharmful_burst_window_seconds = 600\n";
    assert_eq!(
        harmful_feedback_limits_from(&layered(Some(project), &[]), None, None).unwrap(),
        (9, 600)
    );
    assert_eq!(
        harmful_feedback_limits_from(
            &layered(Some(project), &[("EE_HARMFUL_PER_SOURCE_PER_HOUR", "2")]),
            None,
            None
        )
        .unwrap(),
        (2, 600)
    );
    // Each flag overrides only its own limit.
    assert_eq!(
        harmful_feedback_limits_from(&layered(Some(project), &[]), Some(3), None).unwrap(),
        (3, 600)
    );
    let error = harmful_feedback_limits_from(
        &layered(Some("[feedback]\nharmful_burst_window_seconds = 0\n"), &[]),
        None,
        None,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains(crate::config::FEEDBACK_HARMFUL_BURST_WINDOW_SECONDS_KEY),
        "{error}"
    );
}
