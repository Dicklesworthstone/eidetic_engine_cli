type TestResult = Result<(), String>;

const README: &str = include_str!("../README.md");
const CARGO_TOML: &str = include_str!("../Cargo.toml");

#[derive(Debug)]
struct InstallMethod {
    section: &'static str,
    command_fragment: &'static str,
    /// The row's leading cell only. The status cell is prose that is
    /// reworded as install support changes; pinning it asserts a spelling.
    /// Coverage is what this field is for: the table has a row for this
    /// method. Liveness is asserted separately by the `planned` check.
    status_row_prefix: &'static str,
}

const INSTALL_METHODS: &[InstallMethod] = &[
    InstallMethod {
        section: "### Release installer",
        command_fragment: "eidetic_engine_cli@main/install.sh",
        status_row_prefix: "| GitHub release installer |",
    },
    InstallMethod {
        section: "### Homebrew (macOS / Linux)",
        command_fragment: "brew install Dicklesworthstone/tap/ee",
        status_row_prefix: "| Homebrew tap |",
    },
    InstallMethod {
        section: "### Cargo",
        command_fragment: "cargo install eidetic-engine",
        status_row_prefix: "| crates.io |",
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
        ensure_contains(installation, method.status_row_prefix, method.status_row)?;
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
fn advertised_install_paths_are_live_and_not_marked_planned() -> TestResult {
    let installation = installation_section()?;

    for method in INSTALL_METHODS {
        let body = subsection(installation, method.section)?;
        ensure(
            !body.to_ascii_lowercase().contains("planned"),
            format!(
                "{} is a live install path but is still marked planned",
                method.section
            ),
        )?;
        ensure_contains(installation, method.status_row_prefix, method.status_row)?;
    }

    Ok(())
}

#[test]
fn cargo_install_name_matches_publishable_package_name() -> TestResult {
    let installation = installation_section()?;
    let cargo = subsection(installation, "### Cargo")?;
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

/// Extract the package name from a `cargo install` line, tolerating a
/// toolchain selector.
///
/// README advertises `cargo +nightly install eidetic-engine ...` because this
/// crate is nightly-only; a bare `cargo install` would fail for a user on
/// stable. The earlier `strip_prefix("cargo install ")` could not see that
/// form, so this guard went red when `3889af4bf` (2026-09-12) made the README
/// command correct. The README is the right side; the parser was too narrow.
fn cargo_install_package(line: &str) -> Option<&str> {
    let rest = line.trim().strip_prefix("cargo ")?;
    let rest = match rest.strip_prefix('+') {
        // `+nightly install pkg` -> drop the toolchain token
        Some(after_plus) => after_plus.split_once(' ').map(|(_, tail)| tail)?,
        None => rest,
    };
    rest.strip_prefix("install ")
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
