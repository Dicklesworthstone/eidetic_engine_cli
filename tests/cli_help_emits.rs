use std::process::Command;

#[test]
fn root_help_emits_walking_skeleton_prelude() -> Result<(), String> {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--help")
        .output()
        .map_err(|error| format!("run ee --help: {error}"))?;

    assert!(
        output.status.success(),
        "ee --help failed with status {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("help stdout is utf-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("help stderr is utf-8: {error}"))?;

    assert!(!stdout.trim().is_empty(), "ee --help stdout is empty");
    assert!(
        stderr.trim().is_empty(),
        "ee --help should not write stderr: {stderr}"
    );

    for required in [
        "Most-used commands (start here)",
        "  init ",
        "  remember ",
        "  search ",
        "  context ",
        "  why ",
        "Usage:",
    ] {
        assert!(
            stdout.contains(required),
            "ee --help stdout missing {required:?}:\n{stdout}"
        );
    }
    Ok(())
}
