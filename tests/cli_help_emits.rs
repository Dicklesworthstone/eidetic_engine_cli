use std::process::Command;

#[test]
fn root_help_emits_walking_skeleton_prelude() {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("--help")
        .output()
        .expect("run ee --help");

    assert!(
        output.status.success(),
        "ee --help failed with status {:?}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("help stdout is utf-8");
    let stderr = String::from_utf8(output.stderr).expect("help stderr is utf-8");

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
}
