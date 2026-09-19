//! Repository-backed memory capture for MCP agents, through the ordinary CLI.
//!
//! No shell expansion, alternate writer, secret-policy bypass, or implicit
//! apply: the existing remember command owns collection, screening, storage,
//! idempotency, audit, indexing, and errors. Both write controls are required.

use serde_json::{Value, json};
use std::ffi::OsString;

use super::{
    McpToolEffect, append_optional_number_flag, append_optional_string_flag, gated_write_dry_run,
    optional_bool, optional_string, push_arg, required_string,
};

pub(super) const EFFECT: McpToolEffect = McpToolEffect {
    kind: "durable_write",
    write_surface: &[
        "memories",
        "audit_log",
        "search_index_jobs",
        "memory_tags",
        "memory_links",
        "curation_candidates",
        "remember_idempotency_keys",
    ],
    default_dry_run: true,
    requires_allow_write_when_dry_run_false: true,
    audit: "ee remember --from-* records the same memory and audit identity as the CLI",
    redaction: "Git content and provenance are screened before ordinary remember admission; no bypass is exposed",
    idempotency: "idempotencyKey replays the original memory only when captured content matches",
    destructive: false,
};

pub(super) fn schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["workspace", "mode"],
        "properties": {
            "workspace": {"type": "string", "minLength": 1, "description": "Explicit local Git workspace to inspect"},
            "mode": {"type": "string", "enum": ["commit", "diff", "worktree"]},
            "reference": {"type": "string", "minLength": 1, "description": "Commit ref (default HEAD) or required diff base/range. Forbidden for worktree mode."},
            "level": {"type": "string", "description": "Memory level; defaults to episodic"},
            "kind": {"type": "string", "description": "Override the kind inferred from the captured change"},
            "tags": {"type": "string", "description": "Comma-separated tags merged with capture tags"},
            "confidence": {"type": "number", "minimum": 0, "maximum": 1},
            "workflow": {"type": "string"},
            "validFrom": {"type": "string", "description": "RFC3339 applicability start"},
            "validTo": {"type": "string", "description": "RFC3339 applicability end"},
            "idempotencyKey": {"type": "string", "minLength": 1, "description": "Explicit retry key. Changed evidence with the same key is rejected."},
            "noAutoLink": {"type": "boolean"},
            "noProposeCandidates": {"type": "boolean"},
            "dryRun": {"type": "boolean", "default": true},
            "allowWrite": {"type": "boolean", "default": false}
        },
        "allOf": [
            {"if": {"properties": {"mode": {"const": "diff"}}}, "then": {"required": ["reference"]}},
            {"if": {"properties": {"mode": {"const": "worktree"}}}, "then": {"not": {"required": ["reference"]}}}
        ]
    })
}

pub(super) fn args(args: &mut Vec<OsString>, input: &Value) -> Result<(), String> {
    // The schema documents these constraints, but tools/call must enforce them
    // itself: MCP clients are not required to run a JSON-schema validator.
    let object = input
        .as_object()
        .ok_or("Git capture arguments must be an object")?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "workspace"
                | "mode"
                | "reference"
                | "level"
                | "kind"
                | "tags"
                | "confidence"
                | "workflow"
                | "validFrom"
                | "validTo"
                | "idempotencyKey"
                | "noAutoLink"
                | "noProposeCandidates"
                | "dryRun"
                | "allowWrite"
        )
    }) {
        // Do not echo an unknown key: it may itself contain private source text.
        return Err("Git capture received an unsupported argument".to_owned());
    }
    required_string(input, &["workspace"])?;
    let dry_run = gated_write_dry_run("ee_capture_git", input)?;
    let mode = required_string(input, &["mode"])?;
    let reference = optional_string(input, &["reference"])?;
    if input.get("reference").is_some() && reference.is_none_or(str::is_empty) {
        return Err("Git capture reference must be a non-empty string".to_owned());
    }
    push_arg(args, "remember");
    match mode {
        "commit" => {
            push_arg(args, "--from-commit");
            push_arg(args, reference.unwrap_or("HEAD"));
        }
        "diff" => {
            push_arg(args, "--from-diff");
            push_arg(
                args,
                reference.ok_or("Git diff capture requires reference")?,
            );
        }
        "worktree" if reference.is_none() => push_arg(args, "--from-worktree"),
        "worktree" => return Err("Git worktree capture does not accept reference".to_owned()),
        _ => return Err("Git capture mode must be commit, diff, or worktree".to_owned()),
    }
    for (name, flag) in [
        ("level", "--level"),
        ("kind", "--kind"),
        ("tags", "--tags"),
        ("workflow", "--workflow"),
        ("validFrom", "--valid-from"),
        ("validTo", "--valid-to"),
        ("idempotencyKey", "--idempotency-key"),
    ] {
        append_optional_string_flag(args, input, &[name], flag)?;
    }
    append_optional_number_flag(args, input, &["confidence"], "--confidence")?;
    for (name, flag) in [
        ("noAutoLink", "--no-auto-link"),
        ("noProposeCandidates", "--no-propose-candidates"),
    ] {
        if optional_bool(input, &[name])? {
            push_arg(args, flag);
        }
    }
    // Unlike plain remember, omitting --dry-run does NOT apply a Git capture.
    // The explicit CLI --apply flag is mandatory for an authorized write.
    push_arg(args, if dry_run { "--dry-run" } else { "--apply" });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{build_cli_args_for_tool, handle_tools_list, mcp_tool_entry};
    type TestResult = Result<(), String>;

    fn built(input: Value) -> Result<Vec<String>, String> {
        let tool = mcp_tool_entry("ee_capture_git").ok_or("missing capture tool")?;
        Ok(build_cli_args_for_tool(tool, &input)?
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect())
    }

    #[test]
    fn default_capture_is_an_explicit_preview_even_when_writes_are_allowed() -> TestResult {
        for allow in [false, true] {
            let args = built(json!({"workspace": "/repo", "mode": "commit", "allowWrite": allow}))?;
            assert_eq!(
                args,
                [
                    "ee",
                    "--json",
                    "--workspace",
                    "/repo",
                    "remember",
                    "--from-commit",
                    "HEAD",
                    "--dry-run"
                ]
            );
        }
        Ok(())
    }

    #[test]
    fn durable_capture_requires_both_controls_and_emits_apply() -> TestResult {
        assert!(built(json!({"workspace": "/repo", "mode": "worktree", "dryRun": false})).is_err());
        let args = built(
            json!({"workspace": "/repo", "mode": "worktree", "dryRun": false, "allowWrite": true, "idempotencyKey": "capture-one"}),
        )?;
        assert!(args.contains(&"--apply".to_owned()));
        assert!(!args.contains(&"--dry-run".to_owned()));
        assert!(
            args.windows(2)
                .any(|w| w == ["--idempotency-key", "capture-one"])
        );
        Ok(())
    }

    #[test]
    fn source_selection_and_write_bypasses_fail_closed_without_echoing_input() {
        for input in [
            json!({"mode": "commit"}),
            json!({"workspace": "/repo", "mode": "diff"}),
            json!({"workspace": "/repo", "mode": "commit", "reference": ""}),
            json!({"workspace": "/repo", "mode": "commit", "reference": null}),
            json!({"workspace": "/repo", "mode": "worktree", "reference": "PRIVATE_SENTINEL"}),
            json!({"workspace": "/repo", "mode": "PRIVATE_SENTINEL"}),
            json!({"workspace": "/repo", "mode": "commit", "allowSecretMention": true}),
            json!({"workspace": "/repo", "mode": "commit", "source": "PRIVATE_SENTINEL"}),
            json!({"workspace": "/repo", "mode": "commit", "apply": true}),
            json!({"workspace": "/repo", "mode": "commit", "dryRun": "false"}),
        ] {
            let error = built(input).err();
            assert!(error.is_some());
            assert!(!error.unwrap_or_default().contains("PRIVATE_SENTINEL"));
        }
    }

    #[test]
    fn diff_ranges_and_memory_controls_remain_distinct_os_arguments() -> TestResult {
        let args = built(
            json!({"workspace": "/repo with space", "mode": "diff", "reference": "main...topic", "kind": "decision", "tags": "build,release", "confidence": 0.9, "workflow": "release flow", "noAutoLink": true, "noProposeCandidates": true}),
        )?;
        assert!(
            args.windows(2)
                .any(|w| w == ["--workspace", "/repo with space"])
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--from-diff", "main...topic"])
        );
        assert!(args.windows(2).any(|w| w == ["--workflow", "release flow"]));
        assert!(args.contains(&"--no-auto-link".to_owned()));
        assert!(args.contains(&"--no-propose-candidates".to_owned()));
        Ok(())
    }

    #[test]
    fn discovery_exposes_capture_schema_and_durable_effects() -> TestResult {
        let listed = handle_tools_list(json!(1));
        let tool = listed
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .and_then(|tools| tools.iter().find(|t| t["name"] == "ee_capture_git"))
            .ok_or("capture missing from tools/list")?;
        assert_eq!(tool["annotations"]["readOnlyHint"], false);
        assert_eq!(tool["annotations"]["idempotentHint"], false);
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(
            tool["inputSchema"]["required"],
            json!(["workspace", "mode"])
        );
        assert_eq!(tool["eeEffect"]["defaultDryRun"], true);
        assert_eq!(tool["eeEffect"]["requiresAllowWriteWhenDryRunFalse"], true);
        Ok(())
    }
}
