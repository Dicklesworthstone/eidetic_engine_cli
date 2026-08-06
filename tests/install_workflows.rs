use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

const FRANKEN_STACK_LOCK: &str = include_str!("../franken-stack.lock");
const FRANKEN_STACK_BASH: &str = include_str!("../scripts/checkout-franken-stack.sh");
const FRANKEN_STACK_POWERSHELL: &str = include_str!("../scripts/checkout-franken-stack.ps1");
const CI_WORKFLOW: &str = include_str!("../.github/workflows/ci.yml");
const RELEASE_WORKFLOW: &str = include_str!("../.github/workflows/release.yml");
const MACOS_ARTIFACT_WORKFLOW: &str = include_str!("../.github/workflows/macos-ee-artifact.yml");
const NATIVE_RERANKER_E2E: &str = include_str!("../scripts/e2e_native_reranker.sh");

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-install-artifacts")
        .join(format!("{prefix}-{}-{now}", std::process::id())))
}

fn ensure(condition: bool, context: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.to_owned())
    }
}

fn ensure_equal<T: std::fmt::Debug + PartialEq>(
    actual: T,
    expected: T,
    context: &str,
) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn readme_recommends_verified_idempotent_installers_without_claiming_hook_mutation() -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let readme = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;

    ensure(
        readme.matches("| bash -s -- --easy-mode --verify").count() >= 2,
        "README should recommend PATH repair and executable verification in both Unix install examples",
    )?;
    ensure(
        readme.contains(
            "raw.githubusercontent.com/Dicklesworthstone/eidetic_engine_cli/main/install.ps1?cache=",
        ) && readme.contains("& $f -Verify"),
        "README should fetch the current Windows installer and recommend executable verification",
    )?;
    ensure(
        readme.contains("settings remain untouched")
            && readme.contains("without changing agent settings")
            && !readme.contains("auto-configures the Claude Code"),
        "README must describe the informational agent scan without claiming installer-side hook mutation",
    )
}

fn parse_stdout(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).map_err(|error| format!("invalid JSON stdout: {error}\n{stdout}"))
}

fn json_str<'a>(value: &'a serde_json::Value, pointer: &str) -> Result<Option<&'a str>, String> {
    value
        .pointer(pointer)
        .map(|field| {
            field
                .as_str()
                .ok_or_else(|| format!("{pointer} is not a string"))
        })
        .transpose()
}

fn json_bool(value: &serde_json::Value, pointer: &str) -> Result<Option<bool>, String> {
    value
        .pointer(pointer)
        .map(|field| {
            field
                .as_bool()
                .ok_or_else(|| format!("{pointer} is not a bool"))
        })
        .transpose()
}

fn json_array<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
) -> Result<&'a Vec<serde_json::Value>, String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{pointer} is not an array"))
}

fn has_finding(value: &serde_json::Value, code: &str) -> Result<bool, String> {
    Ok(json_array(value, "/data/findings")?
        .iter()
        .any(|finding| json_str(finding, "/code").ok().flatten() == Some(code)))
}

fn has_finding_with(
    value: &serde_json::Value,
    code: &str,
    severity: &str,
    next_action_fragment: &str,
) -> Result<bool, String> {
    for finding in json_array(value, "/data/findings")? {
        let matches = json_str(finding, "/code")? == Some(code)
            && json_str(finding, "/severity")? == Some(severity)
            && json_str(finding, "/nextAction")?
                .is_some_and(|next_action| next_action.contains(next_action_fragment));
        if matches {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_finding_with(
    value: &serde_json::Value,
    code: &str,
    severity: &str,
    next_action_fragment: &str,
) -> TestResult {
    ensure(
        has_finding_with(value, code, severity, next_action_fragment)?,
        &format!("{code} {severity} finding should mention {next_action_fragment:?}"),
    )
}

fn normalize_dynamic_value(value: &mut serde_json::Value, install_root: Option<&Path>) {
    match value {
        serde_json::Value::String(text) => {
            let mut normalized = std::mem::take(text);
            if let Some(root) = install_root {
                let root = root.to_string_lossy().replace('\\', "/");
                normalized = normalized.replace(&root, "<INSTALL_ROOT>");
            }
            let manifest_dir = env!("CARGO_MANIFEST_DIR").replace('\\', "/");
            normalized = normalized.replace(&manifest_dir, "<WORKSPACE>");
            normalized = normalized.replace(env!("CARGO_PKG_VERSION"), "<EE_VERSION>");
            *text = normalized;
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_dynamic_value(item, install_root);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                normalize_dynamic_value(item, install_root);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn normalized_install_json(
    mut value: serde_json::Value,
    install_root: Option<&Path>,
) -> Result<String, String> {
    normalize_dynamic_value(&mut value, install_root);
    if let Some(data) = value
        .get_mut("data")
        .and_then(serde_json::Value::as_object_mut)
        && data.contains_key("idempotencyKey")
    {
        data.insert(
            "idempotencyKey".to_owned(),
            serde_json::Value::String("<IDEMPOTENCY_KEY>".to_owned()),
        );
    }
    serde_json::to_string(&value).map_err(|error| error.to_string())
}

fn assert_install_golden(
    name: &str,
    value: serde_json::Value,
    install_root: Option<&Path>,
) -> TestResult {
    let actual = normalized_install_json(value, install_root)?;
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("install")
        .join(format!("{name}.golden"));
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    ensure_equal(actual.trim(), expected.trim(), name)
}

#[test]
fn installer_archive_binary_selection_refuses_ambiguous_fallbacks() -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let script = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;

    ensure(
        script.contains("select_extracted_binary()"),
        "installer should isolate archive binary candidate selection",
    )?;
    ensure(
        script.contains("Archive contains multiple executable '$BINARY' candidates:"),
        "installer should reject multiple executable candidates",
    )?;
    ensure(
        script.contains(
            "Archive contains multiple matching '$BINARY' candidates without owner-execute mode:",
        ),
        "installer should reject ambiguous chmod fallback candidates",
    )?;
    ensure(
        script.contains("Refusing to choose by filesystem traversal order."),
        "installer should explain why ambiguous candidates are refused",
    )?;
    ensure(
        script.contains("Extracted '$BINARY' lacks owner-execute mode; applying chmod u+x"),
        "installer should log the chmod fallback",
    )?;
    ensure(
        script.contains("if ! chmod u+x \"$BIN\" 2>/dev/null; then"),
        "installer should treat chmod fallback failure as fatal",
    )?;
    ensure(
        !script.contains(
            "find \"$TMP/extract\" -maxdepth 3 -type f -name \"$BINARY\" 2>/dev/null | head -n 1",
        ),
        "installer must not pick a non-executable fallback by traversal order",
    )?;
    ensure(
        !script.contains("chmod u+x \"$BIN\" 2>/dev/null || true"),
        "installer must not silently ignore chmod fallback failure",
    )
}

#[cfg(unix)]
#[test]
fn installer_proxy_forwarding_is_bash_3_2_nounset_safe() -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let installer = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let section_start = installer
        .find("PROXY_ARGS=()")
        .ok_or_else(|| "installer proxy section start is missing".to_owned())?;
    let section_end = installer[section_start..]
        .find("\nusage() {")
        .map(|offset| section_start + offset)
        .ok_or_else(|| "installer proxy section end is missing".to_owned())?;
    let proxy_section = &installer[section_start..section_end];

    let safe_forwarding = r#""${PROXY_ARGS[@]+"${PROXY_ARGS[@]}"}" "$@""#;
    ensure(
        proxy_section.contains(safe_forwarding),
        "installer must use Bash 3.2-safe forwarding for an empty proxy array under set -u",
    )?;

    let harness = format!(
        r#"set -euo pipefail
{proxy_section}
curl() {{
  local empty=0
  local proxy=""
  local last=""
  local arg=""
  for arg in "$@"; do
    [ -n "$arg" ] || empty=1
    last="$arg"
  done
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "--proxy" ]; then
      proxy="${{2:-}}"
      shift 2
    else
      shift
    fi
  done
  printf 'empty=%s proxy=<%s> last=<%s>\n' "$empty" "$proxy" "$last"
}}

unset HTTPS_PROXY HTTP_PROXY
setup_proxy
ee_curl https://example.invalid/direct

HTTPS_PROXY=https://proxy.invalid:8443
HTTP_PROXY=http://ignored.invalid:8080
setup_proxy
ee_curl https://example.invalid/proxied
"#
    );
    let output = Command::new("/bin/bash")
        .arg("-c")
        .arg(harness)
        .env_remove("BASH_ENV")
        .output()
        .map_err(|error| format!("failed to run installer proxy harness: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        &format!(
            "installer proxy harness failed with status {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        ),
    )?;
    ensure_equal(
        stdout.as_ref(),
        concat!(
            "empty=0 proxy=<> last=<https://example.invalid/direct>\n",
            "empty=0 proxy=<https://proxy.invalid:8443> ",
            "last=<https://example.invalid/proxied>\n"
        ),
        "installer proxy forwarding",
    )
}

#[cfg(unix)]
#[test]
fn installer_has_no_bash_3_2_unsafe_empty_array_expansions() -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let installer = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut unsafe_expansions = Vec::new();

    for (line_index, line) in installer.lines().enumerate() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let mut array_names = Vec::new();
        let mut cursor = 0;
        while let Some(relative_start) = line[cursor..].find("${") {
            let start = cursor + relative_start + 2;
            let Some(relative_end) = line[start..].find("[@]}") else {
                cursor = start;
                continue;
            };
            let name = &line[start..start + relative_end];
            if !name.is_empty()
                && name
                    .chars()
                    .all(|character| character == '_' || character.is_ascii_alphanumeric())
            {
                array_names.push(name);
            }
            cursor = start;
        }

        let mut remainder = line.to_owned();
        array_names.sort_unstable();
        array_names.dedup();
        for name in array_names {
            let safe = ["${", name, "[@]+\"${", name, "[@]}\"}"].concat();
            remainder = remainder.replace(&safe, "");
            let unsafe_form = ["${", name, "[@]}"].concat();
            if remainder.contains(&unsafe_form) {
                unsafe_expansions.push(format!("{}: {}", line_index + 1, line.trim()));
            }
        }
    }

    ensure(
        unsafe_expansions.is_empty(),
        &format!(
            "install.sh contains empty-array expansions that fail under `set -u` on Bash 3.2: {}",
            unsafe_expansions.join("; ")
        ),
    )?;

    let function_start = installer
        .find("is_agent_detected() {")
        .ok_or_else(|| "installer is_agent_detected function is missing".to_owned())?;
    let function_end = installer[function_start..]
        .find("\n}\n\n# ─")
        .map(|offset| function_start + offset + 2)
        .ok_or_else(|| "installer is_agent_detected function end is missing".to_owned())?;
    let function = &installer[function_start..function_end];
    let harness = format!(
        r#"set -euo pipefail
DETECTED_AGENTS=()
{function}
if is_agent_detected codex-cli; then
  exit 10
fi
DETECTED_AGENTS=("claude-code" "codex-cli")
is_agent_detected codex-cli
if is_agent_detected missing-agent; then
  exit 11
fi
printf 'empty-and-populated-agent-scan-ok\n'
"#
    );
    let output = Command::new("/bin/bash")
        .arg("-c")
        .arg(harness)
        .env_remove("BASH_ENV")
        .output()
        .map_err(|error| format!("failed to run installer empty-array harness: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        &format!(
            "installer empty-array harness failed with status {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        ),
    )?;
    ensure_equal(
        stdout.as_ref(),
        "empty-and-populated-agent-scan-ok\n",
        "installer empty-array agent scan",
    )
}

#[cfg(unix)]
#[test]
fn installer_retries_compatible_linux_archive_without_crossing_trust_inputs() -> TestResult {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let installer = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    ensure(
        installer.contains("--connect-timeout 3 --max-time 5 --range 0-0 -o /dev/null \"$URL\""),
        "installer network preflight should probe one byte instead of downloading the archive",
    )?;
    ensure(
        installer.contains(
            "explicit artifact/checksum inputs forbid automatic retargeting or source fallback",
        ),
        "installer should fail closed when a caller-pinned artifact cannot be downloaded",
    )?;

    let platform_start = installer
        .find("OS=\"\"\nARCH=\"\"\nTARGET=\"\"")
        .ok_or_else(|| "installer platform section start is missing".to_owned())?;
    let platform_end = installer[platform_start..]
        .find("# Version resolution and artifact URL")
        .map(|offset| platform_start + offset)
        .ok_or_else(|| "installer platform section end is missing".to_owned())?;
    let artifact_start = installer
        .find("TAR=\"\"\nURL=\"\"")
        .ok_or_else(|| "installer artifact section start is missing".to_owned())?;
    let artifact_end = installer[artifact_start..]
        .find("# Preflight checks")
        .map(|offset| artifact_start + offset)
        .ok_or_else(|| "installer artifact section end is missing".to_owned())?;
    let platform_section = &installer[platform_start..platform_end];
    let artifact_section = &installer[artifact_start..artifact_end];

    let harness = format!(
        r#"set -euo pipefail
FROM_SOURCE=0
ARTIFACT_URL=""
CHECKSUM=""
CHECKSUM_URL=""
VERSION="v0.12.0"
OWNER="Dicklesworthstone"
REPO="eidetic_engine_cli"
TMP="/tmp/ee-installer-fallback-harness"
info() {{ :; }}
warn() {{ printf 'warning=%s\n' "$*"; }}
uname() {{
  case "$1" in
    -s) printf 'Linux\n' ;;
    -m) printf 'x86_64\n' ;;
    *) return 1 ;;
  esac
}}
ee_curl() {{
  case "$1" in
    *x86_64-unknown-linux-musl*) printf 'attempt=musl\n'; return 22 ;;
    *x86_64-unknown-linux-gnu*) printf 'attempt=gnu\n'; return 0 ;;
    *) printf 'attempt=unexpected:%s\n' "$1"; return 23 ;;
  esac
}}

{platform_section}
{artifact_section}

detect_platform
set_artifact_url
download_release_artifact
printf 'selected=%s tar=%s\n' "$TARGET" "$TAR"

printf '%s\n' '-- explicit-checksum --'
OS=""
ARCH=""
TARGET=""
FALLBACK_TARGET=""
CHECKSUM="caller-pinned-checksum"
detect_platform
set_artifact_url
if download_release_artifact; then
  printf 'explicit-checksum=unexpected-success\n'
  exit 1
fi
printf 'explicit-checksum=failed-closed selected=%s\n' "$TARGET"
"#
    );
    let output = Command::new("/bin/bash")
        .arg("-c")
        .arg(harness)
        .env_remove("BASH_ENV")
        .output()
        .map_err(|error| format!("failed to run installer target-fallback harness: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        &format!(
            "installer target-fallback harness failed with status {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        ),
    )?;
    ensure(
        stdout.contains("attempt=musl\n")
            && stdout.contains("attempt=gnu\n")
            && stdout.contains(
                "selected=x86_64-unknown-linux-gnu tar=ee-x86_64-unknown-linux-gnu.tar.xz",
            ),
        "installer should retry and select the compatible GNU release archive",
    )?;

    let explicit_checksum = stdout
        .split("-- explicit-checksum --\n")
        .nth(1)
        .ok_or_else(|| "explicit-checksum harness output is missing".to_owned())?;
    ensure(
        explicit_checksum.contains("attempt=musl\n")
            && !explicit_checksum.contains("attempt=gnu\n")
            && explicit_checksum
                .contains("explicit-checksum=failed-closed selected=x86_64-unknown-linux-musl"),
        "installer must not retarget a caller-pinned checksum to the GNU fallback",
    )
}

#[cfg(unix)]
fn write_installer_fixture_script(path: &Path, content: &str) -> TestResult {
    fs::write(path, content).map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn matching_version_installer_command(
    installer: &Path,
    root: &Path,
    fail_version_at: Option<u32>,
) -> Result<Command, String> {
    let home = root.join("home");
    let dest = root.join("bin");
    let mock_bin = root.join("mock-bin");
    let ee_log = root.join("ee-invocations.log");
    let mkdir_log = root.join("mkdir-invocations.log");
    let curl_log = root.join("curl-invocations.log");
    let version_count = root.join("version-count");

    fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    fs::create_dir_all(&dest).map_err(|error| error.to_string())?;
    fs::create_dir_all(&mock_bin).map_err(|error| error.to_string())?;

    write_installer_fixture_script(
        &dest.join("ee"),
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$EE_TEST_EE_LOG"
case "${1:-}" in
  --version)
    count=0
    if [ -f "$EE_TEST_VERSION_COUNT" ]; then
      count=$(sed -n '1p' "$EE_TEST_VERSION_COUNT")
    fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$EE_TEST_VERSION_COUNT"
    printf 'ee 0.13.0\n'
    if [ -n "${EE_TEST_FAIL_VERSION_AT:-}" ] &&
       [ "$count" -ge "$EE_TEST_FAIL_VERSION_AT" ]; then
      exit 23
    fi
    ;;
  completion)
    case "${2:-}" in
      --help) exit 0 ;;
      zsh) printf '#compdef ee\n' ;;
      *) exit 2 ;;
    esac
    ;;
  doctor)
    printf '{"schema":"ee.response.v2","success":true,"degraded":[{"code":"fixture"}]}\n'
    exit 6
    ;;
  *)
    exit 2
    ;;
esac
"#,
    )?;
    write_installer_fixture_script(
        &mock_bin.join("curl"),
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$EE_TEST_CURL_LOG"
exit 97
"#,
    )?;
    write_installer_fixture_script(
        &mock_bin.join("mkdir"),
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$EE_TEST_MKDIR_LOG"
exec /bin/mkdir "$@"
"#,
    )?;

    // Keep the destination active in this process PATH while leaving the
    // fresh HOME without any shell rc files. `--easy-mode` must still create
    // and persist the active shell's startup file rather than returning early.
    let path = format!(
        "{}:{}:/usr/bin:/bin:/usr/sbin:/sbin",
        mock_bin.display(),
        dest.display()
    );
    let mut command = Command::new("/bin/bash");
    command
        .arg(installer)
        .args([
            "--version",
            "v0.13.0",
            "--dest",
            dest.to_str()
                .ok_or_else(|| "installer destination was not UTF-8".to_owned())?,
            "--easy-mode",
            "--verify",
            "--offline",
            "--no-gum",
            "--no-configure",
        ])
        .env("HOME", &home)
        .env("SHELL", "/bin/zsh")
        .env("PATH", path)
        .env("EE_INSTALLER_AGENT_VERSIONS", "0")
        .env("EE_TEST_EE_LOG", ee_log)
        .env("EE_TEST_MKDIR_LOG", mkdir_log)
        .env("EE_TEST_CURL_LOG", curl_log)
        .env("EE_TEST_VERSION_COUNT", version_count)
        .env_remove("BASH_ENV")
        .env_remove("HTTPS_PROXY")
        .env_remove("HTTP_PROXY")
        .env_remove("EE_VERSION")
        .env_remove("VERSION")
        .env_remove("EE_INSTALL_DIR")
        .env_remove("DEST")
        .env_remove("EE_SKIP_VERIFY")
        .env_remove("EE_REQUIRE_PROVENANCE")
        .env_remove("EE_INSTALL_REQUIRE_KEYLESS")
        .env_remove("EE_OFFLINE")
        .env_remove("ARTIFACT_URL")
        .env_remove("CHECKSUM")
        .env_remove("CHECKSUM_URL");
    if let Some(call) = fail_version_at {
        command.env("EE_TEST_FAIL_VERSION_AT", call.to_string());
    } else {
        command.env_remove("EE_TEST_FAIL_VERSION_AT");
    }
    Ok(command)
}

#[cfg(unix)]
#[test]
fn matching_version_installer_rerun_repairs_integration_without_acquisition() -> TestResult {
    let root = unique_artifact_dir("matching-version-rerun")?;
    let installer = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let mut first = matching_version_installer_command(&installer, &root, None)?;
    let first_output = first
        .output()
        .map_err(|error| format!("failed to run matching-version installer: {error}"))?;
    let first_stdout = String::from_utf8_lossy(&first_output.stdout);
    let first_stderr = String::from_utf8_lossy(&first_output.stderr);
    ensure(
        first_output.status.success(),
        &format!(
            "matching-version installer failed with status {:?}\nstdout:\n{first_stdout}\nstderr:\n{first_stderr}",
            first_output.status.code()
        ),
    )?;
    ensure(
        first_stdout.contains("is already installed")
            && first_stdout.contains("Running self-test")
            && first_stderr.contains("ee doctor reported issues"),
        "matching-version rerun should verify the binary and keep doctor degradation advisory",
    )?;
    ensure(
        !first_stdout.contains("Downloading") && !first_stdout.contains("Building from source"),
        "matching-version rerun must skip acquisition",
    )?;

    let zshrc = fs::read_to_string(root.join("home/.zshrc"))
        .map_err(|error| format!("failed to read repaired .zshrc: {error}"))?;
    let expected_path_line = format!("export PATH=\"{}:$PATH\"", root.join("bin").display());
    ensure_equal(
        zshrc
            .lines()
            .filter(|line| *line == expected_path_line.as_str())
            .count(),
        1,
        "matching-version PATH repair count",
    )?;
    ensure(
        !root.join("home/.bashrc").exists(),
        "fresh zsh integration should create only the active shell startup file",
    )?;
    ensure_equal(
        fs::read_to_string(root.join("home/.local/share/zsh/site-functions/_ee"))
            .map_err(|error| format!("failed to read generated zsh completion: {error}"))?,
        "#compdef ee\n".to_owned(),
        "matching-version completion content",
    )?;

    let ee_log = fs::read_to_string(root.join("ee-invocations.log"))
        .map_err(|error| format!("failed to read fake ee log: {error}"))?;
    ensure(
        ee_log.contains("completion --help")
            && ee_log.contains("completion zsh")
            && ee_log.contains("doctor --json"),
        "matching-version rerun should regenerate completions and run requested verification",
    )?;
    ensure(
        !root.join("curl-invocations.log").exists(),
        "matching-version rerun must not invoke curl",
    )?;
    let mkdir_log = fs::read_to_string(root.join("mkdir-invocations.log"))
        .map_err(|error| format!("failed to read mkdir log: {error}"))?;
    ensure(
        !mkdir_log.contains("ee-install.lock.d"),
        "matching-version rerun must not acquire the installer lock",
    )?;

    let mut second = matching_version_installer_command(&installer, &root, None)?;
    let second_output = second
        .output()
        .map_err(|error| format!("failed to rerun matching-version installer: {error}"))?;
    ensure(
        second_output.status.success(),
        "second matching-version rerun should remain idempotent",
    )?;
    let zshrc = fs::read_to_string(root.join("home/.zshrc"))
        .map_err(|error| format!("failed to reread repaired .zshrc: {error}"))?;
    ensure_equal(
        zshrc
            .lines()
            .filter(|line| *line == expected_path_line.as_str())
            .count(),
        1,
        "idempotent matching-version PATH repair count",
    )
}

#[cfg(unix)]
#[test]
fn matching_version_installer_verify_fails_on_nonzero_version_command() -> TestResult {
    let root = unique_artifact_dir("matching-version-broken-binary")?;
    let installer = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let output = matching_version_installer_command(&installer, &root, Some(3))?
        .output()
        .map_err(|error| format!("failed to run broken-binary installer fixture: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        !output.status.success(),
        "matching-version --verify must fail when ee --version exits nonzero",
    )?;
    ensure(
        stderr.contains("ee --version failed with exit code 23"),
        &format!("fatal version failure was not explained\nstdout:\n{stdout}\nstderr:\n{stderr}"),
    )?;
    let ee_log = fs::read_to_string(root.join("ee-invocations.log"))
        .map_err(|error| format!("failed to read broken-binary ee log: {error}"))?;
    ensure(
        !ee_log.contains("doctor --json"),
        "doctor must not mask a fatal ee --version failure",
    )?;
    ensure(
        !root.join("curl-invocations.log").exists(),
        "failed matching-version verification must remain acquisition-free",
    )?;
    let mkdir_log = fs::read_to_string(root.join("mkdir-invocations.log"))
        .map_err(|error| format!("failed to read broken-binary mkdir log: {error}"))?;
    ensure(
        !mkdir_log.contains("ee-install.lock.d"),
        "failed matching-version verification must remain lock-free",
    )
}

#[test]
fn installers_recommend_the_canonical_pack_surface() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for installer in ["install.sh", "install.ps1"] {
        let content = fs::read_to_string(root.join(installer))
            .map_err(|error| format!("failed to read {installer}: {error}"))?;
        ensure(
            content.contains("ee pack"),
            &format!("{installer} should recommend the canonical ee pack surface"),
        )?;
        ensure(
            !content.contains("ee context"),
            &format!(
                "{installer} should not introduce new users to the soft-deprecated ee context alias"
            ),
        )?;
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn source_installers_fail_closed_on_missing_or_non_tagged_requested_versions() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let unix_installer = fs::read_to_string(root.join("install.sh"))
        .map_err(|error| format!("failed to read install.sh: {error}"))?;
    let helper_start = unix_installer
        .find("clone_source_tree() {")
        .ok_or_else(|| "Unix source-clone helper is missing".to_owned())?;
    let helper_end = unix_installer[helper_start..]
        .find("\nensure_rust() {")
        .map(|offset| helper_start + offset)
        .ok_or_else(|| "Unix source-clone helper boundary is missing".to_owned())?;
    let helper = &unix_installer[helper_start..helper_end];

    ensure(
        helper.contains("--branch \"$VERSION\" --single-branch"),
        "Unix source install should clone only the requested release ref",
    )?;
    ensure(
        helper.contains("\"refs/tags/${VERSION}^{commit}\""),
        "Unix source install should require the requested name to resolve as a tag",
    )?;
    ensure(
        helper.contains("[ \"$head_commit\" != \"$requested_commit\" ]"),
        "Unix source install should prove HEAD matches the requested tag commit",
    )?;

    let harness = format!(
        r#"set -euo pipefail
OWNER="Dicklesworthstone"
REPO="eidetic_engine_cli"
VERSION="v999.0.0"
err() {{ printf 'error=%s\n' "$*"; }}
git() {{
  printf 'git=%s\n' "$*" >&2
  case "$GIT_MODE" in
    missing)
      return 42
      ;;
    non-tag)
      [ "$1" = "clone" ] && return 0
      return 1
      ;;
    matching-tag)
      [ "$1" = "clone" ] && return 0
      printf '0123456789abcdef0123456789abcdef01234567\n'
      return 0
      ;;
    *)
      return 99
      ;;
  esac
}}

{helper}

GIT_MODE="missing"
if clone_source_tree /tmp/ee-source-clone-must-not-exist; then
  printf 'missing-tag=unexpected-success\n'
  exit 1
fi
printf 'missing-tag=failed-closed\n'

GIT_MODE="non-tag"
if clone_source_tree /tmp/ee-source-clone-must-not-exist; then
  printf 'non-tag=unexpected-success\n'
  exit 1
fi
printf 'non-tag=failed-closed\n'

GIT_MODE="matching-tag"
clone_source_tree /tmp/ee-source-clone-must-not-exist
printf 'matching-tag=accepted\n'
"#
    );
    let output = Command::new("/bin/bash")
        .arg("-c")
        .arg(harness)
        .env_remove("BASH_ENV")
        .output()
        .map_err(|error| format!("failed to run pinned source-clone harness: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!(
            "pinned source-clone harness failed with status {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code()
        ),
    )?;
    ensure_equal(
        stderr.matches("git=clone ").count(),
        3,
        "each source-clone scenario should make exactly one clone attempt",
    )?;
    ensure(
        stdout.contains("missing-tag=failed-closed")
            && stdout.contains("non-tag=failed-closed")
            && stdout.contains("matching-tag=accepted"),
        "source clone helper should reject missing/non-tag refs and accept an exact matching tag",
    )?;
    ensure(
        stdout.contains("Refusing to build a different revision")
            && stdout.contains("Refusing to build a branch or different revision"),
        "rejected source revisions should explain the fail-closed behavior",
    )?;

    let windows_installer = fs::read_to_string(root.join("install.ps1"))
        .map_err(|error| format!("failed to read install.ps1: {error}"))?;
    ensure(
        windows_installer.contains("clone --depth 1 --branch $VersionTag --single-branch"),
        "Windows source install should clone only the requested release ref",
    )?;
    ensure(
        windows_installer.contains("rev-parse --verify \"refs/tags/${VersionTag}^{commit}\""),
        "Windows source install should require the requested name to resolve as a tag",
    )?;
    ensure(
        windows_installer.contains("$headCommit -ne $tagCommit"),
        "Windows source install should prove HEAD matches the requested tag commit",
    )?;
    ensure(
        !windows_installer.contains("default-branch retry has somewhere to go"),
        "Windows source install must not retain the missing-tag default-branch fallback",
    )
}

#[test]
fn franken_stack_lock_pins_complete_full_sha_closure() -> TestResult {
    const EXPECTED: &[(&str, &str)] = &[
        ("asupersync", "24eb7ec6c62e9ba037d70fed4a69c4e733785926"),
        (
            "franken_agent_detection",
            "f839b370a91e0d403fc4e2e16c1dc9d19b5a5ceb",
        ),
        (
            "franken_networkx",
            "bec1d4b9edfd832bc31c3a56713c918a0bf682b9",
        ),
        ("frankensearch", "b559c92e03242336614b995c562a13dfd1269eed"),
        ("frankensqlite", "6a86c07176830dcab0fd845a71a3dd070694ea28"),
        ("sqlmodel_rust", "84e4f0a9d2f1098afeeae19142442d8208c0f265"),
        ("toon_rust", "e46b71957fad5397d4bae9eef9d428c8ac8b52aa"),
    ];

    ensure(
        FRANKEN_STACK_LOCK.starts_with("# ee.franken-stack.lock.v1\n"),
        "lock should declare the versioned synchronized-stack contract",
    )?;

    let rows = FRANKEN_STACK_LOCK
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (repository, revision) = line
                .split_once('\t')
                .ok_or_else(|| format!("lock row is not tab-delimited: {line:?}"))?;
            ensure(
                !revision.contains('\t'),
                &format!("lock row has extra fields: {line:?}"),
            )?;
            ensure(
                revision.len() == 40
                    && revision
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                &format!("{repository} is not locked to a full lowercase commit ID"),
            )?;
            Ok((repository, revision))
        })
        .collect::<Result<Vec<_>, String>>()?;

    ensure_equal(rows, EXPECTED.to_vec(), "locked Franken-stack revisions")
}

#[test]
fn all_build_and_install_paths_use_the_locked_franken_stack() -> TestResult {
    for (name, workflow) in [
        ("CI", CI_WORKFLOW),
        ("release", RELEASE_WORKFLOW),
        ("macOS artifact", MACOS_ARTIFACT_WORKFLOW),
    ] {
        ensure(
            !workflow.contains(
                "git clone --depth 1 https://github.com/Dicklesworthstone/asupersync.git",
            ),
            &format!("{name} workflow must not clone moving Franken-stack HEADs"),
        )?;
        ensure(
            workflow.contains("./scripts/checkout-franken-stack.sh"),
            &format!("{name} workflow should use the locked Bash checkout helper"),
        )?;
    }
    ensure(
        CI_WORKFLOW.contains("./scripts/checkout-franken-stack.ps1"),
        "CI Windows lanes should use the locked PowerShell checkout helper",
    )?;
    ensure(
        CI_WORKFLOW.matches("bash -s -- --verify").count() >= 2
            && !CI_WORKFLOW.contains(r#"| EE_VERSION="${EE_RELEASE_TAG}" sh"#),
        "CI Unix installer smokes must invoke Bash rather than piping a Bash installer to sh",
    )?;
    ensure(
        RELEASE_WORKFLOW.contains("./scripts/checkout-franken-stack.ps1"),
        "release Windows lanes should use the locked PowerShell checkout helper",
    )?;

    let unix_installer =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
            .map_err(|error| format!("failed to read install.sh: {error}"))?;
    ensure(
        unix_installer.contains(r#""$TMP/src/scripts/checkout-franken-stack.sh" "$TMP""#),
        "install.sh --from-source should provision locked sibling dependencies",
    )?;

    let windows_installer =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.ps1"))
            .map_err(|error| format!("failed to read install.ps1: {error}"))?;
    ensure(
        windows_installer.contains(r#"& $checkoutHelper -DestinationRoot $sourceRoot"#),
        "install.ps1 -FromSource should provision locked sibling dependencies",
    )
}

#[test]
fn ci_proves_five_target_native_reranker_release_gate() -> TestResult {
    let (_, cross_and_rest) = CI_WORKFLOW
        .split_once("\n  cross-platform-determinism:\n")
        .ok_or_else(|| "CI is missing the cross-platform-determinism job".to_owned())?;
    let (cross, aggregate_and_rest) = cross_and_rest
        .split_once("\n  native-reranker-release-proof:\n")
        .ok_or_else(|| "CI is missing the native-reranker aggregate job".to_owned())?;
    let (aggregate, _) = aggregate_and_rest
        .split_once("\n  windows-installer-static-conformance:\n")
        .ok_or_else(|| "CI native-reranker aggregate job has no stable boundary".to_owned())?;
    let (_, strict_and_rest) = cross
        .split_once("      - name: Run fail-closed native-reranker release proof\n")
        .ok_or_else(|| "cross-platform job is missing its strict model lane".to_owned())?;
    let (strict, _) = strict_and_rest
        .split_once("      - name: Upload per-target native-reranker evidence\n")
        .ok_or_else(|| "strict native-reranker lane has no evidence-upload boundary".to_owned())?;
    let (_, matrix_and_steps) = cross
        .split_once("        include:\n")
        .ok_or_else(|| "cross-platform job is missing its include matrix".to_owned())?;
    let (matrix, _) = matrix_and_steps
        .split_once("    steps:\n")
        .ok_or_else(|| "cross-platform matrix has no steps boundary".to_owned())?;
    let rows = matrix
        .split("          - os: ")
        .filter(|row| !row.trim().is_empty())
        .collect::<Vec<_>>();

    let expected = [
        ("aarch64-apple-darwin", "macos-15"),
        ("x86_64-apple-darwin", "macos-15-intel"),
        ("aarch64-unknown-linux-gnu", "ubuntu-24.04-arm"),
        ("x86_64-unknown-linux-gnu", "ubuntu-latest"),
        ("x86_64-pc-windows-msvc", "windows-latest"),
    ];
    ensure_equal(
        matrix.matches("            release_target: true\n").count(),
        expected.len(),
        "exactly five matrix rows should count as release targets",
    )?;
    for (target, runner) in expected {
        let target_marker = format!("target: {target}\n");
        let matching = rows
            .iter()
            .copied()
            .filter(|row| row.contains(&target_marker))
            .collect::<Vec<_>>();
        ensure_equal(matching.len(), 1, &format!("matrix row count for {target}"))?;
        let row = matching[0];
        ensure(
            row.starts_with(runner) && row.contains("release_target: true"),
            &format!("{target} should run natively on {runner} and count toward the proof"),
        )?;
    }
    let musl_row = rows
        .iter()
        .find(|row| row.contains("target: x86_64-unknown-linux-musl\n"))
        .ok_or_else(|| "the extra musl determinism row should remain present".to_owned())?;
    ensure(
        musl_row.contains("release_target: false"),
        "musl is useful extra coverage but must not count among the five release targets",
    )?;

    let (_, dispatch_and_rest) = CI_WORKFLOW
        .split_once("      run_native_reranker_release_matrix:\n")
        .ok_or_else(|| "CI is missing the native-reranker dispatch input".to_owned())?;
    let (dispatch_input, model_url_and_rest) = dispatch_and_rest
        .split_once("      rerank_model_url:\n")
        .ok_or_else(|| "CI is missing the reranker model URL input".to_owned())?;
    let (model_url_input, _) = model_url_and_rest
        .split_once("  schedule:\n")
        .ok_or_else(|| "CI model URL input has no schedule boundary".to_owned())?;
    ensure(
        dispatch_input.contains("default: false")
            && dispatch_input.contains("type: boolean")
            && model_url_input.contains("type: string")
            && cross.contains("inputs.run_native_reranker_release_matrix == true"),
        "the real-model matrix should be an explicit boolean dispatch that requires a model URL",
    )?;
    for contract in [
        "cargo build --locked --workspace --bin ee --target",
        "run_logged full-suite-link",
        "--no-run --target \"$TARGET\"",
        "cargo tree --locked --target \"$TARGET\" -e features --prefix none",
        "timestamps.tsv",
        "if: ${{ always() && matrix.release_target }}",
        "b559c92e03242336614b995c562a13dfd1269eed",
    ] {
        ensure(
            cross.contains(contract),
            &format!("cross-platform release proof is missing {contract:?}"),
        )?;
    }
    for dependency in [
        "tokio",
        "tokio-util",
        "hyper",
        "axum",
        "tower",
        "reqwest",
        "async-std",
        "smol",
        "rusqlite",
        "sqlx",
        "diesel",
        "sea-orm",
        "petgraph",
        "ort",
        "ort-sys",
        "onnxruntime",
        "onnxruntime-sys",
    ] {
        ensure(
            cross.contains(&format!("\"{dependency}\"")),
            &format!("target-resolved audit should reject {dependency}"),
        )?;
    }
    for contract in [
        "82767464",
        "adaada3ccc15ae535e9bea238d2ec05e4f39726bdcad07dd87cba9f85dc10edb",
        "EE_E2E_RERANK_REQUIRE_MODEL=1",
        "EE_E2E_NATIVE_RERANK_REQUIRE_MODEL=1",
        "run_logged full-ee-suite",
        "-p frankensearch-rerank --no-default-features --features native",
        "bash scripts/e2e_native_reranker.sh",
        "rerank-vector.json",
    ] {
        ensure(
            strict.contains(contract),
            &format!("strict native-reranker lane is missing {contract:?}"),
        )?;
    }
    for contract in [
        "RERANK_MODEL_URL: ${{ inputs.rerank_model_url }}",
        "if [[ -z \"$RERANK_MODEL_URL\" ]]",
        "cargo test --locked --workspace --lib --bins --tests --examples",
        "--no-default-features --features native",
        "[native_reranker] SKIP",
        "[native_reranker] ranking(desc)=",
    ] {
        ensure(
            strict.contains(contract),
            &format!("strict fail-closed execution is missing {contract:?}"),
        )?;
    }
    ensure(
        !strict.contains("--no-run"),
        "strict full-suite execution must run tests rather than compile only",
    )?;

    for target in [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
    ] {
        ensure(
            aggregate.contains(&format!("\"{target}\"")),
            &format!("aggregate proof should require {target}"),
        )?;
    }
    for contract in [
        "len(records) != 5",
        "ee.rerank_determinism.vector.v1",
        "fusionOnlyOrder",
        "rerankedOrder",
        "tolerance = 0.01",
        "abs(actual[\"rerankScore\"] - expected[\"rerankScore\"])",
        "if: ${{ always()",
        "needs.cross-platform-determinism.result",
        "strict-completed-at.txt",
        "MATRIX_RESULT",
    ] {
        ensure(
            aggregate.contains(contract),
            &format!("aggregate vector comparison is missing {contract:?}"),
        )?;
    }

    for contract in [
        "EE_E2E_RERANK_VECTOR_OUT",
        "EE_E2E_TARGET_TRIPLE",
        "ee.rerank_determinism.vector.v1",
        "VECTOR_EMITTED=0",
        "USERPROFILE=",
        "APPDATA=",
        "LOCALAPPDATA=",
        "dumpbin.exe",
        "llvm-objdump",
        "objdump",
        "ort\\.dll",
    ] {
        ensure(
            NATIVE_RERANKER_E2E.contains(contract),
            &format!("native reranker E2E portability contract is missing {contract:?}"),
        )?;
    }

    #[cfg(unix)]
    {
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/e2e_native_reranker.sh");
        let status = Command::new("bash")
            .arg("-n")
            .arg(&script)
            .status()
            .map_err(|error| format!("failed to parse {}: {error}", script.display()))?;
        ensure(status.success(), "native reranker E2E should pass bash -n")?;
    }
    Ok(())
}

#[test]
fn franken_stack_helpers_refuse_to_overwrite_existing_work() -> TestResult {
    for (name, helper) in [
        ("Bash", FRANKEN_STACK_BASH),
        ("PowerShell", FRANKEN_STACK_POWERSHELL),
    ] {
        ensure(
            helper.contains("franken-stack.lock"),
            &format!("{name} helper should consume the central lock"),
        )?;
        ensure(
            helper.contains("status") && helper.contains("--porcelain"),
            &format!("{name} helper should verify existing checkout cleanliness"),
        )?;
        ensure(
            helper.contains("rev-parse") && helper.contains("HEAD"),
            &format!("{name} helper should verify the checked-out commit"),
        )?;
        ensure(
            helper.contains("refusing to modify"),
            &format!("{name} helper should fail closed on an existing mismatch"),
        )?;
        ensure(
            !helper.contains("reset --hard"),
            &format!("{name} helper must not rewrite an existing checkout"),
        )?;
        ensure(
            !helper.contains("Remove-Item") && !helper.contains("rm -"),
            &format!("{name} helper must not delete an existing checkout"),
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn write_fake_ee(path: &Path) -> TestResult {
    fs::write(path, "#!/bin/sh\nexit 0\n").map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn write_fake_ee_version(path: &Path, version: &str) -> TestResult {
    fs::write(path, format!("#!/bin/sh\nprintf 'ee {version}\\n'\n"))
        .map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
}

#[cfg(unix)]
#[test]
fn install_check_detects_duplicate_path_binaries_without_stderr() -> TestResult {
    let root = unique_artifact_dir("install-check")?;
    let bin_a = root.join("a");
    let bin_b = root.join("b");
    fs::create_dir_all(&bin_a).map_err(|error| error.to_string())?;
    fs::create_dir_all(&bin_b).map_err(|error| error.to_string())?;
    write_fake_ee(&bin_a.join("ee"))?;
    write_fake_ee(&bin_b.join("ee"))?;
    let path_value = std::env::join_paths([bin_a.as_path(), bin_b.as_path()])
        .map_err(|error| error.to_string())?;
    let path_arg = path_value
        .to_str()
        .ok_or_else(|| "PATH argument was not UTF-8".to_owned())?;
    let install_dir = bin_a
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let current_binary = bin_b.join("ee");
    let current_binary_arg = current_binary
        .to_str()
        .ok_or_else(|| "current binary was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "check",
        "--json",
        "--install-dir",
        install_dir,
        "--current-binary",
        current_binary_arg,
        "--path",
        path_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("install check should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "JSON install check must not write stderr",
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.response.v2"),
        "response schema",
    )?;
    ensure_equal(
        json_str(&value, "/data/schema")?,
        Some("ee.install.check.v1"),
        "install schema",
    )?;
    ensure_equal(
        json_str(&value, "/data/path/status")?,
        Some("duplicate"),
        "PATH duplicate status",
    )?;
    ensure(
        has_finding(&value, "duplicate_path_binary")?,
        "duplicate finding present",
    )?;
    assert_install_golden("duplicate_path_check.json", value, Some(&root))
}

#[cfg(unix)]
#[test]
fn install_check_pins_shadowed_stale_path_contract_bd_3utv2_5() -> TestResult {
    let root = unique_artifact_dir("install-shadow-skew")?;
    let stale_dir = root.join("stale");
    let current_dir = root.join("current");
    fs::create_dir_all(&stale_dir).map_err(|error| error.to_string())?;
    fs::create_dir_all(&current_dir).map_err(|error| error.to_string())?;
    write_fake_ee_version(&stale_dir.join("ee"), "0.1.0")?;
    write_fake_ee_version(&current_dir.join("ee"), env!("CARGO_PKG_VERSION"))?;
    let path_value = std::env::join_paths([stale_dir.as_path(), current_dir.as_path()])
        .map_err(|error| error.to_string())?;
    let path_arg = path_value
        .to_str()
        .ok_or_else(|| "PATH argument was not UTF-8".to_owned())?;
    let install_dir = current_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let current_binary = current_dir.join("ee");
    let current_binary_arg = current_binary
        .to_str()
        .ok_or_else(|| "current binary was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "check",
        "--json",
        "--install-dir",
        install_dir,
        "--current-binary",
        current_binary_arg,
        "--path",
        path_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("install check should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "JSON install check must not write stderr",
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/path/status")?,
        Some("duplicate"),
        "PATH duplicate status",
    )?;
    ensure_equal(
        json_str(&value, "/data/path/binaries/0/version")?,
        Some("0.1.0"),
        "first PATH binary version",
    )?;
    ensure_equal(
        json_str(&value, "/data/path/binaries/0/versionStatus")?,
        Some("reported"),
        "first PATH binary version status",
    )?;
    ensure_equal(
        json_bool(&value, "/data/path/binaries/0/isCurrentBinary")?,
        Some(false),
        "first PATH binary is stale shadow",
    )?;
    ensure_equal(
        json_bool(&value, "/data/path/binaries/1/isCurrentBinary")?,
        Some(true),
        "second PATH binary is running binary",
    )?;
    ensure_equal(
        json_str(&value, "/data/path/binaries/1/version")?,
        Some(env!("CARGO_PKG_VERSION")),
        "running PATH binary version",
    )?;
    ensure_equal(
        json_str(&value, "/data/freshness/verdict")?,
        Some("shadowed_binary"),
        "freshness verdict",
    )?;
    ensure_equal(
        json_bool(&value, "/data/freshness/authoritative")?,
        Some(false),
        "freshness fails closed",
    )?;
    ensure(
        json_array(&value, "/data/freshness/blockingFindings")?
            .iter()
            .any(|finding| finding.as_str() == Some("current_binary_shadowed")),
        "shadowed binary blocks claim-gate authority",
    )?;
    ensure_finding_with(
        &value,
        "duplicate_path_binary",
        "warning",
        "Remove stale duplicates",
    )?;
    ensure_finding_with(
        &value,
        "path_binary_version_mismatch",
        "warning",
        "rebuild/install the current release",
    )?;
    ensure_finding_with(
        &value,
        "current_binary_shadowed",
        "warning",
        "PATH/install-dir ordering",
    )?;
    ensure_finding_with(
        &value,
        "current_binary_shadowed",
        "error",
        "verified artifact adoption",
    )?;
    ensure_finding_with(&value, "offline_no_manifest", "info", "Pass --manifest")
}

#[cfg(unix)]
#[test]
fn install_freshness_claim_gate_e2e_script_bd_3utv2_7() -> TestResult {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = repo.join("scripts").join("e2e_install_freshness.sh");
    let artifacts = unique_artifact_dir("install-freshness-e2e")?;
    fs::create_dir_all(&artifacts).map_err(|error| error.to_string())?;
    let event_log = artifacts.join("events.jsonl");

    let output = Command::new("bash")
        .arg(&script)
        .env("EE_BIN", env!("CARGO_BIN_EXE_ee"))
        .env("EE_BINARY", env!("CARGO_BIN_EXE_ee"))
        .env("EE_E2E_TMPDIR", &artifacts)
        .env("EE_TEST_LOG_PATH", &event_log)
        .env("LOG_DIR", &artifacts)
        .env("TMPDIR", &artifacts)
        .output()
        .map_err(|error| format!("run {}: {error}", script.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!(
            "install freshness e2e script failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            stdout,
            stderr
        ),
    )?;

    let events = fs::read_to_string(&event_log)
        .map_err(|error| format!("read {}: {error}", event_log.display()))?;
    ensure(
        events.contains("\"schema\":\"ee.test_event.v1\""),
        "script should emit structured test events",
    )?;
    ensure(
        events.contains("\"label\":\"stale_claim_gate\"")
            && events.contains("\"freshness_verdict\":\"shadowed_binary\""),
        "stale claim-gate event should pin the shadowed-binary refusal",
    )?;
    ensure(
        events.contains("\"label\":\"fresh_claim_gate\"")
            && events.contains("\"freshness_verdict\":\"fresh\""),
        "fresh claim-gate event should pin the authoritative control path",
    )?;
    ensure(
        events.contains("\"stdout_artifact_path\":\"[REPO]/target/ee-install-artifacts/"),
        "script should log scrubbed stdout artifact paths",
    )
}

#[test]
fn install_plan_selects_manifest_artifact_and_stays_dry_run() -> TestResult {
    let root = unique_artifact_dir("install-plan")?;
    let install_dir = root.join("bin");
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("single_platform_dev.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-musl",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("install plan should succeed; stderr: {stderr}"),
    )?;
    ensure(stderr.is_empty(), "JSON install plan must not write stderr")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/schema")?,
        Some("ee.install.plan.v1"),
        "install plan schema",
    )?;
    ensure_equal(json_bool(&value, "/data/dryRun")?, Some(true), "dry run")?;
    ensure_equal(
        json_str(&value, "/data/artifact/artifactId")?,
        Some("ee-0.1.0-dev-x86_64-unknown-linux-musl"),
        "selected artifact",
    )?;
    ensure(
        json_array(&value, "/data/plannedOperations")?
            .iter()
            .all(|operation| {
                json_bool(operation, "/requiresVerification").ok().flatten() == Some(true)
            }),
        "planned operations require verification",
    )?;
    assert_install_golden("fresh_install_plan.json", value, Some(&root))
}

#[test]
fn update_dry_run_manifest_plan_matches_golden() -> TestResult {
    let root = unique_artifact_dir("update-plan")?;
    let install_dir = root.join("bin");
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("multi_platform.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "update",
        "--dry-run",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("update dry-run should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "JSON update dry-run must not write stderr",
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/schema")?,
        Some("ee.update.plan.v1"),
        "update schema",
    )?;
    ensure_equal(
        json_str(&value, "/data/operation")?,
        Some("update"),
        "operation",
    )?;
    assert_install_golden("update_plan.json", value, Some(&root))
}

#[test]
fn install_plan_checksum_mismatch_refuses_unverified_artifact() -> TestResult {
    let root = unique_artifact_dir("checksum-mismatch")?;
    let install_dir = root.join("bin");
    let artifact_root = root.join("artifacts");
    fs::create_dir_all(&artifact_root).map_err(|error| error.to_string())?;
    fs::write(
        artifact_root.join("ee-x86_64-unknown-linux-gnu.tar.xz"),
        "wrong artifact bytes",
    )
    .map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let artifact_root_arg = artifact_root
        .to_str()
        .ok_or_else(|| "artifact root was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("checksum_mismatch.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--artifact-root",
        artifact_root_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    ensure(
        output.status.success(),
        "checksum mismatch remains a successful dry-run report",
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/status")?,
        Some("blocked"),
        "blocked status",
    )?;
    ensure_equal(
        json_str(&value, "/data/verification/checksumStatus")?,
        Some("failed"),
        "checksum status",
    )?;
    ensure(
        has_finding(&value, "artifact_checksum_mismatch")?,
        "checksum mismatch finding",
    )?;
    assert_install_golden("checksum_mismatch_plan.json", value, Some(&root))
}

#[test]
fn install_plan_unsupported_target_matches_golden() -> TestResult {
    let root = unique_artifact_dir("unsupported-target")?;
    let install_dir = root.join("bin");
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("unsupported_target.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "sparc64-unknown-plan9",
        "--offline",
    ])?;
    ensure(output.status.success(), "unsupported target dry-run report")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/status")?,
        Some("blocked"),
        "blocked status",
    )?;
    ensure(
        has_finding(&value, "unsupported_target")?,
        "unsupported target finding",
    )?;
    assert_install_golden("unsupported_target_plan.json", value, Some(&root))
}

#[test]
fn install_plan_without_manifest_matches_offline_golden() -> TestResult {
    let root = unique_artifact_dir("offline-plan")?;
    let install_dir = root.join("bin");
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    ensure(output.status.success(), "offline dry-run report")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/status")?,
        Some("blocked"),
        "blocked status",
    )?;
    ensure(
        has_finding(&value, "offline_no_manifest")?,
        "offline no manifest finding",
    )?;
    assert_install_golden("offline_no_manifest_plan.json", value, Some(&root))
}

#[cfg(unix)]
#[test]
fn install_check_permission_denied_matches_golden() -> TestResult {
    let output = run_ee(&[
        "install",
        "check",
        "--json",
        "--install-dir",
        "/dev/null/ee",
        "--current-binary",
        "/dev/null/not-ee",
        "--path",
        "/dev/null",
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    ensure(output.status.success(), "permission check report")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/permissions/status")?,
        Some("missing_parent_unknown"),
        "permission status",
    )?;
    ensure(
        has_finding(&value, "install_dir_not_writable")?,
        "install dir not writable finding",
    )?;
    assert_install_golden("permission_denied_check.json", value, None)
}

#[test]
fn update_without_dry_run_is_policy_denied_json() -> TestResult {
    let output = run_ee(&["update", "--json"])?;
    let value = parse_stdout(&output)?;

    ensure(!output.status.success(), "update apply should fail")?;
    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.error.v2"),
        "error schema",
    )?;
    ensure_equal(
        json_str(&value, "/error/code")?,
        Some("policy_denied"),
        "policy denied code",
    )
}

// ============================================================================
// EE-DIST-004: Additional e2e scenarios
// ============================================================================

#[test]
fn install_check_already_current_is_idempotent() -> TestResult {
    let root = unique_artifact_dir("already-current")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("already_current.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": env!("CARGO_PKG_VERSION"),
            "artifacts": [{
                "artifactId": "ee-current-x86_64-unknown-linux-gnu",
                "target": "x86_64-unknown-linux-gnu",
                "url": "file:///dev/null",
                "sha256": "0".repeat(64),
                "bytes": 1000
            }]
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "check",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("already-current check should succeed; stderr: {stderr}"),
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.response.v2"),
        "response schema",
    )?;
    ensure_equal(
        json_str(&value, "/data/schema")?,
        Some("ee.install.check.v1"),
        "install check schema",
    )
}

#[test]
fn install_plan_rejects_path_traversal_in_artifact_name() -> TestResult {
    let root = unique_artifact_dir("path-traversal")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("traversal_manifest.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": "0.1.0",
            "artifacts": [{
                "artifactId": "../../../etc/passwd",
                "target": "x86_64-unknown-linux-gnu",
                "url": "file:///tmp/evil.tar.xz",
                "sha256": "0".repeat(64),
                "bytes": 1000
            }]
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let value = parse_stdout(&output)?;

    let status = json_str(&value, "/data/status")?;
    let has_traversal = has_finding(&value, "path_traversal_detected").unwrap_or(false)
        || has_finding(&value, "invalid_artifact_id").unwrap_or(false)
        || status == Some("blocked");
    ensure(
        has_traversal,
        "path traversal should be detected or blocked",
    )
}

#[test]
fn install_plan_rejects_unicode_control_chars_in_path() -> TestResult {
    let root = unique_artifact_dir("unicode-control")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("unicode_manifest.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": "0.1.0",
            "artifacts": [{
                "artifactId": "ee-\u{202E}gnp.exe",
                "target": "x86_64-unknown-linux-gnu",
                "url": "file:///tmp/rtl-trick.tar.xz",
                "sha256": "0".repeat(64),
                "bytes": 1000
            }]
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let value = parse_stdout(&output)?;

    let status = json_str(&value, "/data/status")?;
    let has_unicode_issue = has_finding(&value, "unicode_control_character").unwrap_or(false)
        || has_finding(&value, "invalid_artifact_id").unwrap_or(false)
        || status == Some("blocked");
    ensure(
        has_unicode_issue,
        "unicode control characters should be detected or blocked",
    )
}

#[test]
fn install_plan_handles_duplicate_target_ids() -> TestResult {
    let root = unique_artifact_dir("duplicate-target")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("duplicate_target_manifest.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": "0.1.0",
            "artifacts": [
                {
                    "artifactId": "ee-v1-x86_64-unknown-linux-gnu",
                    "target": "x86_64-unknown-linux-gnu",
                    "url": "file:///tmp/first.tar.xz",
                    "sha256": "a".repeat(64),
                    "bytes": 1000
                },
                {
                    "artifactId": "ee-v2-x86_64-unknown-linux-gnu",
                    "target": "x86_64-unknown-linux-gnu",
                    "url": "file:///tmp/second.tar.xz",
                    "sha256": "b".repeat(64),
                    "bytes": 1000
                }
            ]
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.response.v2"),
        "response schema",
    )?;
    let has_duplicate_finding = has_finding(&value, "duplicate_target").unwrap_or(false)
        || has_finding(&value, "ambiguous_artifact").unwrap_or(false);
    let selected_artifact = json_str(&value, "/data/artifact/artifactId")?;
    ensure(
        has_duplicate_finding || selected_artifact.is_some(),
        "duplicate targets should be handled (warning or deterministic selection)",
    )
}

#[cfg(unix)]
#[test]
fn install_check_handles_symlinked_install_root() -> TestResult {
    let root = unique_artifact_dir("symlink-root")?;
    let real_bin = root.join("real_bin");
    let symlink_bin = root.join("linked_bin");
    fs::create_dir_all(&real_bin).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&real_bin, &symlink_bin).map_err(|error| error.to_string())?;
    let install_dir_arg = symlink_bin
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("release_manifest")
        .join("single_platform_dev.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let output = run_ee(&[
        "install",
        "check",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-musl",
        "--offline",
    ])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        output.status.success(),
        &format!("symlinked root check should succeed; stderr: {stderr}"),
    )?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.response.v2"),
        "response schema",
    )?;
    ensure_equal(
        json_str(&value, "/data/schema")?,
        Some("ee.install.check.v1"),
        "install check schema",
    )
}

#[test]
fn install_plan_huge_manifest_does_not_hang() -> TestResult {
    let root = unique_artifact_dir("huge-manifest")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("huge_manifest.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;

    let mut artifacts = Vec::new();
    for i in 0..500 {
        artifacts.push(serde_json::json!({
            "artifactId": format!("ee-{i}-fake-target-{i}"),
            "target": format!("fake-target-{i}"),
            "url": format!("file:///tmp/artifact-{i}.tar.xz"),
            "sha256": format!("{:064x}", i),
            "bytes": 1000 + i
        }));
    }
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": "0.1.0",
            "artifacts": artifacts
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    ensure(output.status.success(), "huge manifest should complete")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/schema")?,
        Some("ee.response.v2"),
        "response schema",
    )?;
    let status = json_str(&value, "/data/status")?;
    ensure(
        status == Some("blocked") || status == Some("ready"),
        "huge manifest should report blocked (no target) or ready",
    )
}

#[test]
fn install_plan_empty_manifest_is_blocked() -> TestResult {
    let root = unique_artifact_dir("empty-manifest")?;
    let install_dir = root.join("bin");
    fs::create_dir_all(&install_dir).map_err(|error| error.to_string())?;
    let install_dir_arg = install_dir
        .to_str()
        .ok_or_else(|| "install dir was not UTF-8".to_owned())?;
    let manifest = root.join("empty_manifest.json");
    let manifest_arg = manifest
        .to_str()
        .ok_or_else(|| "manifest path was not UTF-8".to_owned())?;
    fs::write(
        &manifest,
        serde_json::json!({
            "schema": "ee.release_manifest.v1",
            "version": "0.1.0",
            "artifacts": []
        })
        .to_string(),
    )
    .map_err(|error| error.to_string())?;

    let output = run_ee(&[
        "install",
        "plan",
        "--json",
        "--manifest",
        manifest_arg,
        "--install-dir",
        install_dir_arg,
        "--target",
        "x86_64-unknown-linux-gnu",
        "--offline",
    ])?;
    ensure(output.status.success(), "empty manifest report")?;
    let value = parse_stdout(&output)?;

    ensure_equal(
        json_str(&value, "/data/status")?,
        Some("blocked"),
        "empty manifest should block",
    )?;
    ensure(
        has_finding(&value, "unsupported_target")? || has_finding(&value, "no_artifacts")?,
        "empty manifest should have finding",
    )
}
