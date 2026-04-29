use std::process::Command;

#[test]
fn status_json_stdout_is_stable_machine_data() {
    let output = match Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["status", "--json"])
        .output()
    {
        Ok(output) => output,
        Err(error) => panic!("failed to run ee status --json: {error}"),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr must be empty for JSON status");
    assert!(stdout.starts_with("{\"schema\":\"ee.response.v1\""));
    assert!(stdout.contains("\"command\":\"status\""));
    assert!(stdout.ends_with('\n'));
}

#[test]
fn unknown_command_keeps_stdout_clean() {
    let output = match Command::new(env!("CARGO_BIN_EXE_ee"))
        .arg("unknown")
        .output()
    {
        Ok(output) => output,
        Err(error) => panic!("failed to run ee unknown: {error}"),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stdout.is_empty(), "stdout must stay clean on usage errors");
    assert!(stderr.contains("error: unknown command"));
}
