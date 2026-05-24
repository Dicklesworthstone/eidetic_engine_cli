//! bd-326us: real-binary pin test for `ee plan recipe show` NotFound and
//! `ee plan recipe list` success envelope.
//!
//! `handle_plan_recipe_show` (src/cli/mod.rs:17964) emits a NotFound
//! DomainError when the requested recipe id doesn't resolve via
//! `core::plan::get_recipe`. The error envelope carries:
//!
//!   { "schema": "ee.error.v2",
//!     "error": {
//!       "code": "not_found",
//!       "details": { "resource": "recipe", "id": "<requested>" },
//!       "repair": "ee plan recipe list --json",
//!       ...
//!     }
//!   }
//!
//! `handle_plan_recipe_list` (src/cli/mod.rs:17896) emits a success
//! envelope:
//!
//!   { "schema": "ee.plan.recipe_list.v1",
//!     "success": true,
//!     "data": {
//!       "command": "plan recipe list",
//!       "totalCount": N,
//!       "category": null,
//!       "recipes": [{ "id": "...", ... }, ...]
//!     }
//!   }
//!
//! No test file under tests/ runs `ee plan recipe ...` against the real
//! binary today; both branches are unpinned. This pin-test mirrors the
//! tests/e2e_schema_export_unknown.rs harness shape.

#![cfg(unix)]

use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[test]
fn plan_recipe_show_unknown_id_returns_not_found_with_list_repair() -> TestResult {
    let phantom = "bogus_recipe_id_not_in_registry";
    let output = run_ee(&["--json", "plan", "recipe", "show", phantom])?;
    ensure(
        !output.status.success(),
        format!(
            "plan recipe show <unknown> must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("plan recipe show stdout must be JSON: {error}"))?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    // The error envelope carries resource/id either in error.details or
    // directly on error depending on the renderer. Look across both.
    let resource = error["details"]["resource"]
        .as_str()
        .or_else(|| error["resource"].as_str())
        .unwrap_or_default();
    ensure(
        resource == "recipe",
        format!("error must identify resource=recipe; got {error}"),
    )?;
    let id = error["details"]["id"]
        .as_str()
        .or_else(|| error["id"].as_str())
        .unwrap_or_default();
    ensure(
        id == phantom,
        format!("error must echo the requested phantom id; got {error}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee plan recipe list"),
        format!("NotFound repair must reference `ee plan recipe list`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn plan_recipe_list_emits_success_envelope_with_recipes_array() -> TestResult {
    let output = run_ee(&["--json", "plan", "recipe", "list"])?;
    ensure(
        output.status.success(),
        format!(
            "plan recipe list must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("plan recipe list stdout must be JSON: {error}"))?;
    ensure(
        parsed["schema"].as_str() == Some("ee.plan.recipe_list.v1"),
        format!("schema must be ee.plan.recipe_list.v1; got {parsed}"),
    )?;
    ensure(
        parsed["success"].as_bool() == Some(true),
        format!("success must be true; got {parsed}"),
    )?;
    let data = &parsed["data"];
    ensure(
        data["command"].as_str() == Some("plan recipe list"),
        format!("data.command must be 'plan recipe list'; got {data}"),
    )?;
    let recipes = data["recipes"]
        .as_array()
        .ok_or_else(|| format!("data.recipes must be an array; got {data}"))?;
    ensure(
        !recipes.is_empty(),
        format!("data.recipes must be non-empty (at least one registered recipe); got {recipes:?}"),
    )?;
    let total = data["totalCount"].as_u64().unwrap_or(0);
    ensure(
        total as usize == recipes.len(),
        format!(
            "data.totalCount must equal recipes.len(); totalCount={total}, recipes.len()={}",
            recipes.len()
        ),
    )?;
    // Every recipe summary should at minimum carry an id.
    for (index, recipe) in recipes.iter().enumerate() {
        ensure(
            recipe["id"].is_string(),
            format!("recipes[{index}].id must be a string; got {recipe}"),
        )?;
    }
    Ok(())
}
