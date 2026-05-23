//! bd-37gnq: real-binary pin test for `ee memory link` list-mode
//! and `--dry-run` behaviors.
//!
//! Companion to `bd-1p09v` (syntactic usage) and `bd-6trv0` (semantic
//! validation); together they pin the three main behavior clusters of
//! the graph-foundation command. `memory_link_e2e.rs` covers a single
//! create+list+duplicate happy path but does NOT pin:
//!
//! * the empty-list envelope shape (status=`listed`, persisted=false,
//!   changed=false, idempotency=`read_only`, links=[])
//! * `--dry-run` create preview envelope (status=`would_create`,
//!   dry_run=true, persisted=false, changed=true, idempotency=
//!   `would_change`) AND the no-mutation contract that the link does
//!   NOT appear in a follow-up list call
//! * `--relation` filter narrowing the list to matching relations
//!   only
//! * list-mode bidirectional contract: a memory appears in the list
//!   regardless of whether it is the src or the dst of the incident
//!   edge

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

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

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-memory-link-list-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn init_workspace(workspace_arg: &str) -> TestResult {
    let init = run_ee(&["--workspace", workspace_arg, "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )
}

fn remember(workspace_arg: &str, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        content,
    ])?;
    if !output.status.success() {
        return Err(format!(
            "remember failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    parsed["data"]["public_id"]
        .as_str()
        .or_else(|| parsed["data"]["memory_id"].as_str())
        .or_else(|| parsed["data"]["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "remember response missing memory id: {}",
                serde_json::to_string(&parsed).unwrap_or_default()
            )
        })
}

fn run_memory_link(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "memory", "link"];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("memory link stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn data_field<'a>(parsed: &'a Value) -> Result<&'a Value, String> {
    // Some renderers wrap the report in an envelope, others emit the
    // report at the top level. Accept either by preferring `data` when
    // present and falling back to the parsed root.
    if parsed.get("data").map(Value::is_object).unwrap_or(false) {
        Ok(&parsed["data"])
    } else {
        Ok(parsed)
    }
}

#[test]
fn memory_link_list_on_isolated_memory_returns_empty_links_envelope() -> TestResult {
    let workspace = unique_workspace("empty-list")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let isolated = remember(&workspace_arg, "Pin-test isolated memory (no links).")?;

    let (output, parsed) = run_memory_link(&workspace_arg, &[&isolated])?;
    ensure(
        output.status.success(),
        format!(
            "memory link list on isolated memory must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = data_field(&parsed)?;
    ensure(
        data["status"].as_str() == Some("listed"),
        format!("status must be `listed`; got {data}"),
    )?;
    ensure(
        data["dry_run"] == Value::Bool(false),
        format!("dry_run must be false for list mode; got {data}"),
    )?;
    ensure(
        data["persisted"] == Value::Bool(false),
        format!("persisted must be false for list mode; got {data}"),
    )?;
    ensure(
        data["changed"] == Value::Bool(false),
        format!("changed must be false for list mode; got {data}"),
    )?;
    ensure(
        data["idempotency"].as_str() == Some("read_only"),
        format!("idempotency must be `read_only` for list mode; got {data}"),
    )?;
    let links = data["links"]
        .as_array()
        .ok_or_else(|| format!("links must be an array; got {data}"))?;
    ensure(
        links.is_empty(),
        format!("links must be empty for an isolated memory; got {links:?}"),
    )?;
    ensure(
        data["link"].is_null(),
        format!("link (singular) must be null for list mode; got {data}"),
    )?;
    Ok(())
}

#[test]
fn memory_link_dry_run_returns_would_create_preview_without_persisting() -> TestResult {
    let workspace = unique_workspace("dry-run")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let src = remember(&workspace_arg, "Pin-test dry-run source.")?;
    let dst = remember(&workspace_arg, "Pin-test dry-run target.")?;

    // Dry-run create: emits preview but does not persist.
    let (dry_output, dry_parsed) = run_memory_link(
        &workspace_arg,
        &[&src, &dst, "--relation", "supports", "--dry-run"],
    )?;
    ensure(
        dry_output.status.success(),
        format!(
            "memory link --dry-run must succeed; stderr: {}",
            String::from_utf8_lossy(&dry_output.stderr)
        ),
    )?;
    let dry_data = data_field(&dry_parsed)?;
    ensure(
        dry_data["status"].as_str() == Some("would_create"),
        format!("dry-run status must be `would_create`; got {dry_data}"),
    )?;
    ensure(
        dry_data["dry_run"] == Value::Bool(true),
        format!("dry-run dry_run must be true; got {dry_data}"),
    )?;
    ensure(
        dry_data["persisted"] == Value::Bool(false),
        format!("dry-run persisted must be false; got {dry_data}"),
    )?;
    ensure(
        dry_data["changed"] == Value::Bool(true),
        format!("dry-run changed must be true (the preview is non-trivial); got {dry_data}"),
    )?;
    ensure(
        dry_data["idempotency"].as_str() == Some("would_change"),
        format!("dry-run idempotency must be `would_change`; got {dry_data}"),
    )?;
    ensure(
        dry_data["link"].is_object(),
        format!("dry-run preview must include a `link` object; got {dry_data}"),
    )?;
    let dry_links = dry_data["links"]
        .as_array()
        .ok_or_else(|| format!("dry-run links must be an array; got {dry_data}"))?;
    ensure(
        dry_links.len() == 1,
        format!("dry-run links must contain the planned link only; got {dry_links:?}"),
    )?;
    // The planned link must not carry a link_id (since nothing was
    // persisted) — this distinguishes preview shape from create shape.
    ensure(
        dry_links[0]["link_id"].is_null(),
        format!("dry-run planned link must NOT carry a link_id; got {dry_links:?}"),
    )?;

    // No-mutation contract: a follow-up list call must show no links.
    let (list_output, list_parsed) = run_memory_link(&workspace_arg, &[&src])?;
    ensure(
        list_output.status.success(),
        format!(
            "follow-up list must succeed; stderr: {}",
            String::from_utf8_lossy(&list_output.stderr)
        ),
    )?;
    let list_data = data_field(&list_parsed)?;
    let post_links = list_data["links"]
        .as_array()
        .ok_or_else(|| format!("list links must be an array; got {list_data}"))?;
    ensure(
        post_links.is_empty(),
        format!(
            "dry-run must not persist a link; list mode shows {} link(s): {post_links:?}",
            post_links.len()
        ),
    )?;
    Ok(())
}

#[test]
fn memory_link_list_filters_by_relation_and_includes_both_directions() -> TestResult {
    let workspace = unique_workspace("filter-bidirectional")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let center = remember(&workspace_arg, "Pin-test list-filter center.")?;
    let downstream = remember(&workspace_arg, "Pin-test list-filter downstream.")?;
    let upstream = remember(&workspace_arg, "Pin-test list-filter upstream.")?;
    let related_peer = remember(&workspace_arg, "Pin-test list-filter related peer.")?;

    // center -> downstream (supports), upstream -> center (supports),
    // center -- related_peer (related). The center memory is incident
    // to three links: one as src (supports/downstream), one as dst
    // (supports/upstream), and one with related_peer (related).
    for (src, dst, rel, link_label) in [
        (&center, &downstream, "supports", "downstream"),
        (&upstream, &center, "supports", "upstream"),
        (&center, &related_peer, "related", "related"),
    ] {
        let (output, _parsed) = run_memory_link(&workspace_arg, &[src, dst, "--relation", rel])?;
        ensure(
            output.status.success(),
            format!(
                "create link {link_label} must succeed; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
    }

    // Unfiltered list on the center: must see all three incident edges
    // (proving bidirectional incident-edge contract).
    let (output, parsed) = run_memory_link(&workspace_arg, &[&center])?;
    ensure(
        output.status.success(),
        format!(
            "unfiltered list must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = data_field(&parsed)?;
    let all_links = data["links"]
        .as_array()
        .ok_or_else(|| format!("links must be an array; got {data}"))?;
    ensure(
        all_links.len() == 3,
        format!(
            "unfiltered list on center must include all three incident links (downstream/upstream/related); got {} link(s): {all_links:?}",
            all_links.len()
        ),
    )?;
    // Confirm center appears as src in some links and dst in others.
    let center_as_src = all_links
        .iter()
        .filter(|link| link["source_memory_id"].as_str() == Some(center.as_str()))
        .count();
    let center_as_dst = all_links
        .iter()
        .filter(|link| link["target_memory_id"].as_str() == Some(center.as_str()))
        .count();
    ensure(
        center_as_src >= 1,
        format!(
            "center must appear as source in at least one link; got src_count={center_as_src}, links={all_links:?}"
        ),
    )?;
    ensure(
        center_as_dst >= 1,
        format!(
            "center must appear as target in at least one link; got dst_count={center_as_dst}, links={all_links:?}"
        ),
    )?;

    // --relation supports filter: must narrow to exactly the two
    // supports links and exclude the related link.
    let (filt_output, filt_parsed) =
        run_memory_link(&workspace_arg, &[&center, "--relation", "supports"])?;
    ensure(
        filt_output.status.success(),
        format!(
            "filtered list must succeed; stderr: {}",
            String::from_utf8_lossy(&filt_output.stderr)
        ),
    )?;
    let filt_data = data_field(&filt_parsed)?;
    let filtered = filt_data["links"]
        .as_array()
        .ok_or_else(|| format!("filtered links must be an array; got {filt_data}"))?;
    ensure(
        filtered.len() == 2,
        format!(
            "--relation supports must narrow to exactly two links; got {} link(s): {filtered:?}",
            filtered.len()
        ),
    )?;
    for (index, link) in filtered.iter().enumerate() {
        ensure(
            link["relation"].as_str() == Some("supports"),
            format!("filtered link[{index}] must carry relation=supports; got {link}"),
        )?;
    }
    Ok(())
}
