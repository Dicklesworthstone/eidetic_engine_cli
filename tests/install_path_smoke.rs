use serde_json::Value;
use std::path::PathBuf;

type TestResult = Result<(), String>;

const README: &str = include_str!("../README.md");
const CARGO_TOML: &str = include_str!("../Cargo.toml");

#[derive(Debug)]
struct InstallMethod {
    section: &'static str,
    command_fragment: &'static str,
    status_row: &'static str,
    planned_marker: &'static str,
}

const INSTALL_METHODS: &[InstallMethod] = &[
    InstallMethod {
        section: "### Release installer (planned)",
        command_fragment: "releases/download/v0.1.0/install.sh",
        status_row: "| GitHub release installer | planned; no release assets published yet | `bd-2gill.3` |",
        planned_marker: "Planned after the first signed GitHub release ships; see `bd-2gill.3`.",
    },
    InstallMethod {
        section: "### Homebrew (macOS / Linux)",
        command_fragment: "brew install Dicklesworthstone/tap/ee",
        status_row: "| Homebrew tap | planned; tap formula not published yet | `bd-2gill.2` |",
        planned_marker: "Planned after `Dicklesworthstone/homebrew-tap` publishes `Formula/ee.rb`; see",
    },
    InstallMethod {
        section: "### Cargo",
        command_fragment: "cargo install eidetic-engine",
        status_row: "| crates.io | planned; package name selected as `eidetic-engine`; binary remains `ee` | `bd-3usjw.10` |",
        planned_marker: "Planned as the `eidetic-engine` package, which installs the `ee` binary.",
    },
];

#[test]
fn readme_install_status_table_covers_every_advertised_install_path() -> TestResult {
    let installation = installation_section()?;
    ensure_contains(
        installation,
        "### Installation status",
        "installation status heading",
    )?;
    ensure_contains(
        installation,
        "| Source build | available now | this README |",
        "source build available status row",
    )?;
    ensure_contains(
        installation,
        "git clone https://github.com/Dicklesworthstone/eidetic_engine_cli",
        "source build clone command",
    )?;
    ensure_contains(
        installation,
        "cargo build --release",
        "source build command",
    )?;

    for method in INSTALL_METHODS {
        ensure_contains(installation, method.status_row, method.status_row)?;
        ensure_contains(installation, method.section, method.section)?;
        ensure_contains(
            installation,
            method.command_fragment,
            method.command_fragment,
        )?;
    }

    Ok(())
}

#[test]
fn advertised_install_paths_are_planned_or_backed_by_audit_posture() -> TestResult {
    let installation = installation_section()?;
    let audit = load_audit()?;

    for method in INSTALL_METHODS {
        let body = subsection(installation, method.section)?;
        let section_marks_planned =
            body.to_ascii_lowercase().contains("planned") && body.contains(method.planned_marker);
        let table_marks_planned = installation.contains(method.status_row);
        if section_marks_planned && table_marks_planned {
            continue;
        }

        let Some(audit) = audit.as_ref() else {
            return Err(format!(
                "{} is advertised as live, but no install audit artifact exists at {}",
                method.section,
                audit_artifact_path().display()
            ));
        };
        assert_live_posture(method, audit)?;
    }

    Ok(())
}

#[test]
fn cargo_install_name_matches_package_name_when_not_planned() -> TestResult {
    let installation = installation_section()?;
    let cargo = subsection(installation, "### Cargo")?;
    if cargo.to_ascii_lowercase().contains("planned") {
        return Ok(());
    }

    let readme_name = cargo
        .lines()
        .find_map(cargo_install_package)
        .ok_or_else(|| {
            "README Cargo section does not advertise a cargo install command".to_owned()
        })?;
    let package_name = cargo_package_name()?;
    if readme_name != package_name {
        return Err(format!(
            "README cargo install package `{readme_name}` does not match Cargo.toml package `{package_name}`"
        ));
    }
    ensure(
        !cargo_package_publish_is_false(),
        "README advertises live cargo install, but Cargo.toml has package.publish=false",
    )
}

fn installation_section() -> Result<&'static str, String> {
    let start = README
        .find("## Installation")
        .ok_or_else(|| "README missing `## Installation` section".to_owned())?;
    let tail = &README[start..];
    let end = tail
        .find("\n## ")
        .filter(|index| *index > 0)
        .unwrap_or(tail.len());
    Ok(&tail[..end])
}

fn subsection<'a>(section: &'a str, heading: &str) -> Result<&'a str, String> {
    let start = section
        .find(heading)
        .ok_or_else(|| format!("README Installation section missing `{heading}`"))?;
    let tail = &section[start..];
    let end = tail
        .find("\n### ")
        .filter(|index| *index > 0)
        .unwrap_or(tail.len());
    Ok(&tail[..end])
}

fn assert_live_posture(method: &InstallMethod, audit: &Value) -> TestResult {
    match method.section {
        "### Release installer (planned)" => {
            ensure_bool_path(
                audit,
                &["decision_inputs", "latest_release_published"],
                "live release installer requires a published GitHub release",
            )?;
            ensure_bool_path(
                audit,
                &["github_release_assets", "asset_matrix_complete"],
                "live release installer requires complete release assets",
            )
        }
        "### Homebrew (macOS / Linux)" => ensure_bool_path(
            audit,
            &["homebrew_tap", "formula_present"],
            "live Homebrew install requires a published tap formula",
        ),
        "### Cargo" => {
            ensure_bool_path(
                audit,
                &["dependency_resolution", "dep_resolution_ready"],
                "live cargo install requires all path dependencies to be publishable",
            )?;
            ensure_bool_path(
                audit,
                &["decision_inputs", "crates_points_at_project"],
                "live cargo install requires crates.io to point at this project",
            )
        }
        other => Err(format!("unhandled install method `{other}`")),
    }
}

fn ensure_bool_path(audit: &Value, path: &[&str], message: &str) -> TestResult {
    let mut value = audit;
    for segment in path {
        value = value
            .get(*segment)
            .ok_or_else(|| format!("audit missing `{}`", path.join(".")))?;
    }
    ensure(value.as_bool() == Some(true), message)
}

fn audit_artifact_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("audit_artifacts")
        .join("latest_install_pipeline.json")
}

fn load_audit() -> Result<Option<Value>, String> {
    let path = audit_artifact_path();
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    Ok(Some(value))
}

fn cargo_install_package(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("cargo install ")
        .and_then(|value| value.split_whitespace().next())
}

fn cargo_package_name() -> Result<&'static str, String> {
    let mut in_package = false;
    for line in CARGO_TOML.lines() {
        let trimmed = line.trim();
        if trimmed == "[package]" {
            in_package = true;
            continue;
        }
        if in_package && trimmed.starts_with('[') {
            break;
        }
        if in_package {
            if let Some(value) = trimmed.strip_prefix("name = ") {
                return Ok(value.trim_matches('"'));
            }
        }
    }
    Err("Cargo.toml missing [package].name".to_owned())
}

fn cargo_package_publish_is_false() -> bool {
    let mut in_package = false;
    for line in CARGO_TOML.lines() {
        let trimmed = line.trim();
        if trimmed == "[package]" {
            in_package = true;
            continue;
        }
        if in_package && trimmed.starts_with('[') {
            break;
        }
        if in_package && trimmed == "publish = false" {
            return true;
        }
    }
    false
}

fn ensure_contains(haystack: &str, needle: &str, label: &str) -> TestResult {
    ensure(
        haystack.contains(needle),
        format!("README Installation section missing {label}: `{needle}`"),
    )
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
