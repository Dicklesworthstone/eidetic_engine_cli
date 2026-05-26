use std::path::PathBuf;
use std::process::Command;

#[test]
fn cargo_package_list_excludes_generated_artifacts() -> Result<(), Box<dyn std::error::Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("bash")
        .arg("scripts/package-artifact-leak-check.sh")
        .current_dir(&repo_root)
        .output()?;

    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "package artifact leak check failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout,
        stderr
    )
    .into())
}
