use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn root_mesh_scripts() -> Result<Vec<PathBuf>, String> {
    let scripts_dir = repo_root().join("scripts");
    let mut scripts = Vec::new();
    for entry in fs::read_dir(&scripts_dir)
        .map_err(|error| format!("read_dir {}: {error}", scripts_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read_dir entry: {error}"))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name.starts_with("e2e_mesh_") && name.ends_with(".sh") {
            scripts.push(path);
        }
    }
    scripts.sort();
    Ok(scripts)
}

#[test]
fn mesh_scripts_with_scheduled_events_emit_scenario_outcomes() -> TestResult {
    let mut scheduled_scripts = Vec::new();

    for script in root_mesh_scripts()? {
        let body = read(&script)?;
        let has_bare_scheduled = body.contains(r#""stage":"scheduled""#)
            || body.contains(r#""stage\": \"scheduled\""#)
            || body.contains("mesh_e2e_emit_scheduled");
        if !has_bare_scheduled {
            continue;
        }
        scheduled_scripts.push(script.display().to_string());

        if !body.contains("scripts/lib/mesh_e2e_outcomes.sh")
            && !body.contains("lib/mesh_e2e_outcomes.sh")
        {
            return Err(format!(
                "{} schedules mesh scenarios without the outcome helper",
                script.display()
            ));
        }
        if !body.contains("mesh_e2e_run_with_outcomes") && !body.contains("mesh_e2e_emit_outcomes")
        {
            return Err(format!(
                "{} schedules mesh scenarios without per-scenario outcome emission",
                script.display()
            ));
        }
    }

    if scheduled_scripts.is_empty() {
        return Err("expected at least one scheduled mesh e2e script".to_owned());
    }

    Ok(())
}

#[test]
fn mesh_outcome_helper_pins_required_outcome_fields() -> TestResult {
    let helper = read(&repo_root().join("scripts/lib/mesh_e2e_outcomes.sh"))?;
    for required in [
        r#""phase": "outcome""#,
        r#""status": status"#,
        r#""ok": status == "pass""#,
        r#""duration_ms": duration"#,
        r#""stderr_tail": stderr_tail"#,
        r#""pass""#,
        r#""fail""#,
        r#""skipped""#,
    ] {
        if !helper.contains(required) {
            return Err(format!(
                "mesh outcome helper missing required contract fragment: {required}"
            ));
        }
    }
    Ok(())
}
