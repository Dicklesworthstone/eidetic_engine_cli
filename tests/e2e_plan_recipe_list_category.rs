//! bd-1cz6s: real-binary pin test for `ee plan recipe list --category`
//! filter — known-category narrowing and unknown-category silent fallback.
//!
//! `handle_plan_recipe_list` (src/cli/mod.rs:17942) routes `--category`
//! through `GoalCategory::all().iter().find(|cat| cat.as_str() == c)`. A
//! known category narrows recipes to that category and echoes it in
//! `data.category`. An UNKNOWN category silently returns None and
//! `recipes_by_category(None)` returns ALL recipes — agent-visible
//! behavior that a refactor could easily flip to a Usage error without
//! breaking the existing happy-path list test from bd-326us.
//!
//! tests/e2e_plan_recipe.rs (bd-326us) covers only the unfiltered list
//! and the unknown-recipe-id NotFound branch; the --category filter has
//! no real-binary assertions. This pin-test mirrors the
//! tests/e2e_plan_recipe.rs harness shape.

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

fn run_recipe_list(extra: &[&str]) -> Result<Value, String> {
    let mut args: Vec<&str> = vec!["--json", "plan", "recipe", "list"];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    if !output.status.success() {
        return Err(format!(
            "plan recipe list {extra:?} must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("plan recipe list stdout must be JSON: {error}"))
}

fn first_category_with_recipes(unfiltered: &Value) -> Result<String, String> {
    let recipes = unfiltered["data"]["recipes"]
        .as_array()
        .ok_or_else(|| format!("data.recipes must be an array; got {unfiltered}"))?;
    recipes
        .iter()
        .find_map(|recipe| recipe["category"].as_str().map(str::to_owned))
        .ok_or_else(|| {
            format!(
                "expected at least one recipe with a category in the unfiltered list; got {recipes:?}"
            )
        })
}

#[test]
fn plan_recipe_list_known_category_narrows_to_that_category() -> TestResult {
    let unfiltered = run_recipe_list(&[])?;
    let known_category = first_category_with_recipes(&unfiltered)?;

    let filtered = run_recipe_list(&["--category", known_category.as_str()])?;
    ensure(
        filtered["schema"].as_str() == Some("ee.plan.recipe_list.v1"),
        format!("schema must be ee.plan.recipe_list.v1; got {filtered}"),
    )?;
    let data = &filtered["data"];
    ensure(
        data["category"].as_str() == Some(known_category.as_str()),
        format!("data.category must echo the filter `{known_category}`; got {data}"),
    )?;
    let recipes = data["recipes"]
        .as_array()
        .ok_or_else(|| format!("data.recipes must be an array; got {data}"))?;
    ensure(
        !recipes.is_empty(),
        format!("known-category filter must return non-empty recipes; got {recipes:?}"),
    )?;
    for (index, recipe) in recipes.iter().enumerate() {
        ensure(
            recipe["category"].as_str() == Some(known_category.as_str()),
            format!(
                "recipes[{index}].category must equal the filter `{known_category}`; got {recipe}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn plan_recipe_list_unknown_category_silently_returns_all_recipes() -> TestResult {
    let unfiltered = run_recipe_list(&[])?;
    let unfiltered_total = unfiltered["data"]["totalCount"]
        .as_u64()
        .ok_or_else(|| format!("data.totalCount must be numeric; got {unfiltered}"))?;
    ensure(
        unfiltered_total > 0,
        format!(
            "baseline unfiltered list must contain at least one recipe; got totalCount={unfiltered_total}"
        ),
    )?;

    let fallback = run_recipe_list(&["--category", "nonexistent_garbage_category_xyz"])?;
    ensure(
        fallback["schema"].as_str() == Some("ee.plan.recipe_list.v1"),
        format!("unknown-category fallback must still emit ee.plan.recipe_list.v1; got {fallback}"),
    )?;
    let data = &fallback["data"];
    ensure(
        data["category"].is_null(),
        format!(
            "unknown-category fallback must surface data.category=null (silent fallback to all); got {data}"
        ),
    )?;
    let fallback_total = data["totalCount"]
        .as_u64()
        .ok_or_else(|| format!("fallback totalCount must be numeric; got {data}"))?;
    ensure(
        fallback_total == unfiltered_total,
        format!(
            "unknown-category fallback must return the SAME totalCount as unfiltered (proves fallback is full set, not zero); fallback={fallback_total}, unfiltered={unfiltered_total}"
        ),
    )?;
    Ok(())
}
