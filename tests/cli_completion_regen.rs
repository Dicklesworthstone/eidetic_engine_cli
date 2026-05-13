#![forbid(unsafe_code)]

use std::process::Command;

type TestResult = Result<(), String>;

fn run_completion(shell: &str) -> Result<String, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["completion", shell])
        .output()
        .map_err(|error| format!("run ee completion {shell}: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "ee completion {shell} exited {:?}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if !output.stderr.is_empty() {
        return Err(format!(
            "ee completion {shell} wrote stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    String::from_utf8(output.stdout).map_err(|error| format!("completion {shell} utf8: {error}"))
}

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{context}: missing {needle:?}"))
    }
}

#[test]
fn completion_generates_supported_shell_scripts() -> TestResult {
    let cases = [
        ("bash", "_ee()", "complete -F _ee"),
        ("zsh", "#compdef ee", "_arguments"),
        ("fish", "function __fish_ee_needs_command", "complete -c ee"),
        (
            "powershell",
            "Register-ArgumentCompleter -Native -CommandName 'ee'",
            "[CompletionResult]::new",
        ),
        (
            "elvish",
            "set edit:completion:arg-completer[ee]",
            "edit:complex-candidate",
        ),
    ];

    for (shell, marker, binding) in cases {
        let completion = run_completion(shell)?;
        ensure_contains(&completion, marker, shell)?;
        ensure_contains(&completion, binding, shell)?;
        if completion.trim_start().starts_with('{') {
            return Err(format!(
                "completion {shell} must be raw shell text, not JSON"
            ));
        }
    }

    Ok(())
}

#[test]
fn bash_completion_is_deterministic_and_includes_aliases() -> TestResult {
    let first = run_completion("bash")?;
    let second = run_completion("bash")?;
    if first != second {
        return Err("bash completion output is not deterministic".to_owned());
    }

    for alias in ["show", "link", "tag", "history"] {
        ensure_contains(
            &first,
            &format!("ee,{alias})"),
            &format!("bash completion top-level alias {alias}"),
        )?;
    }

    Ok(())
}
